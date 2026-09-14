// Running and building the active file. Output shows the program in its own
// terminal (keystrokes go to it), closed by a line with how it ended; a failed
// build shows the compiler's message there and is kept for the Problems list
// until that file is built again.

import type { StudioApi } from "../api/client";
import { SessionSocket, sessionUrl } from "../api/socket";
import type { BuildFailure, ExitInfo } from "../api/types";
import { ansi, exitLine, exitSummary, formatDuration, terminalLines } from "./format";
import { TerminalView } from "./terminal";

export type RunStatus =
  | { kind: "idle" }
  | { kind: "building"; path: string; run: boolean }
  | { kind: "running"; path: string }
  | { kind: "exited"; path: string; text: string; ok: boolean; duration: string }
  | { kind: "built"; path: string }
  | { kind: "failed"; path: string; failure: BuildFailure };

export interface RunnerEvents {
  /** The status changed. */
  status(status: RunStatus): void;
  /** The build failures changed (see `failures`). */
  failures(): void;
}

export class Runner {
  readonly output: TerminalView;
  private current: RunStatus = { kind: "idle" };
  private socket: SessionSocket | null = null;
  private session: string | null = null;
  private stopped = false;
  private readonly failed = new Map<string, BuildFailure>();

  constructor(
    private readonly api: StudioApi,
    private readonly events: RunnerEvents,
  ) {
    this.output = new TerminalView({
      label: "Output",
      onData: (data) => this.socket?.send(data),
      onResize: (size) => this.socket?.resize(size),
    });
  }

  get status(): RunStatus {
    return this.current;
  }

  /** The last failed build of each file that hasn't been built again since. */
  get failures(): ReadonlyMap<string, BuildFailure> {
    return this.failed;
  }

  get busy(): boolean {
    return this.current.kind === "building";
  }

  /**
   * Builds and runs `path` (saved already), replacing a running program. The
   * Output view should be shown, so the program gets its size.
   */
  async run(path: string): Promise<void> {
    if (this.busy) return;
    this.socket?.dispose();
    this.socket = null;
    this.session = null;
    this.stopped = false;
    this.setStatus({ kind: "building", path, run: true });

    await this.output.open();
    const size = this.output.fit();
    this.output.terminal.reset();
    this.output.write(`${ansi.cyan("▶")} ${ansi.bold(path)}\r\n`);
    try {
      const result = await this.api.run(path, size);
      if (!result.ok) {
        this.fail(path, result.error);
        return;
      }
      this.succeed(path);
      this.session = result.session;
      this.setStatus({ kind: "running", path });
      const socket = new SessionSocket(sessionUrl(window.location, result.session), this.api.token, {
        output: (data) => this.output.write(data),
        exit: (exit) => this.exited(socket, path, exit),
        lost: () => {
          if (this.socket !== socket) return;
          this.socket = null;
          this.output.write(`\r\n${ansi.red("── the connection to the program was lost ──")}\r\n`);
          this.setStatus({ kind: "idle" });
        },
      });
      socket.resize(this.output.fit());
      this.socket = socket;
    } catch (error) {
      this.output.write(`${ansi.red(`Couldn't run ${path}: ${error instanceof Error ? error.message : String(error)}`)}\r\n`);
      this.setStatus({ kind: "idle" });
    }
  }

  /**
   * Builds `path` without running it. Output shows the result unless a
   * program is running there. Resolves to whether the build succeeded, or
   * null if it couldn't be tried.
   */
  async build(path: string): Promise<boolean | null> {
    if (this.busy) return null;
    const runningPath = this.current.kind === "running" ? this.current.path : null;
    const running = runningPath !== null;
    this.setStatus({ kind: "building", path, run: false });
    // A program still running keeps its status; otherwise the build's shows.
    const restore = (): RunStatus => (runningPath !== null && this.socket ? { kind: "running", path: runningPath } : { kind: "idle" });
    try {
      const result = await this.api.build(path);
      if (!running) {
        this.output.terminal.reset();
        this.output.write(`${ansi.cyan("⚒")} ${ansi.bold(path)}\r\n`);
      }
      if (result.ok) {
        this.succeed(path);
        if (!running) this.output.write(`${ansi.green("── built ──")}\r\n`);
        this.setStatus(running ? restore() : { kind: "built", path });
        return true;
      }
      if (running) {
        this.failed.set(path, result.error);
        this.events.failures();
        this.setStatus(restore());
      } else {
        this.fail(path, result.error);
      }
      return false;
    } catch (error) {
      if (!running) this.output.write(`${ansi.red(`Couldn't build ${path}: ${error instanceof Error ? error.message : String(error)}`)}\r\n`);
      this.setStatus(restore());
      return null;
    }
  }

  /** Stops the running program; Output then shows that it stopped. */
  stop(): void {
    if (this.current.kind !== "running" || this.session === null) return;
    this.stopped = true;
    void this.api.stop(this.session).catch(() => undefined);
  }

  clear(): void {
    this.output.terminal.clear();
  }

  /** Follows a renamed file or folder in the kept failures. */
  renamed(from: string, to: string): void {
    let changed = false;
    for (const [path, failure] of [...this.failed]) {
      if (path === from || path.startsWith(`${from}/`)) {
        this.failed.delete(path);
        this.failed.set(to + path.slice(from.length), failure);
        changed = true;
      }
    }
    if (changed) this.events.failures();
  }

  /** Forgets the failures of a deleted file or folder. */
  deleted(path: string): void {
    let changed = false;
    for (const failed of [...this.failed.keys()]) {
      if (failed === path || failed.startsWith(`${path}/`)) changed = this.failed.delete(failed) || changed;
    }
    if (changed) this.events.failures();
  }

  private exited(socket: SessionSocket, path: string, exit: ExitInfo): void {
    if (this.socket !== socket) return;
    this.socket = null;
    this.session = null;
    this.output.write(exitLine(exit, this.stopped));
    const { text, ok } = exitSummary(exit, this.stopped);
    this.setStatus({ kind: "exited", path, text, ok, duration: formatDuration(exit.durationMs) });
  }

  private succeed(path: string): void {
    if (this.failed.delete(path)) this.events.failures();
  }

  private fail(path: string, failure: BuildFailure): void {
    const [first = "", ...rest] = failure.rendered.replace(/\n$/, "").split("\n");
    this.output.write(`${ansi.red(first)}\r\n`);
    if (rest.length) this.output.write(`${terminalLines(rest.join("\n"))}\r\n`);
    this.output.write(`\r\n${ansi.red("── build failed ──")}\r\n`);
    this.failed.set(path, failure);
    this.events.failures();
    this.setStatus({ kind: "failed", path, failure });
  }

  private setStatus(status: RunStatus): void {
    this.current = status;
    this.events.status(status);
  }
}
