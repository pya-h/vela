// The Terminal tab: a shell in the workspace folder. It starts when the tab is
// first shown; when the shell exits (or can't start), any key starts a new one.

import { ApiError, type StudioApi } from "../api/client";
import { SessionSocket, sessionUrl } from "../api/socket";
import { ansi, exitLine } from "./format";
import { TerminalView } from "./terminal";

export class Shell {
  readonly view: TerminalView;
  private socket: SessionSocket | null = null;
  private session: string | null = null;
  private starting = false;
  /** The shell has ended; the next key starts another. */
  private ended = false;
  private stopped = false;
  /** Typed while the shell starts; sent once it has. */
  private typed: string[] = [];

  constructor(private readonly api: StudioApi) {
    this.view = new TerminalView({
      label: "Terminal",
      onData: (data) => {
        if (this.ended) {
          void this.restart();
        } else if (this.socket) {
          this.socket.send(data);
        } else if (this.starting) {
          this.typed.push(data);
        }
      },
      onResize: (size) => this.socket?.resize(size),
    });
  }

  /** Opens the view and starts a shell if none is running. */
  async show(): Promise<void> {
    await this.view.open();
    if (!this.socket && !this.starting && !this.ended) await this.start();
  }

  /** Ends the running shell, if any, and starts a new one in a clear terminal. */
  async restart(): Promise<void> {
    if (this.starting) return;
    this.socket?.dispose();
    if (this.session !== null && !this.ended) void this.api.stop(this.session).catch(() => undefined);
    this.socket = null;
    this.session = null;
    this.ended = false;
    this.view.terminal.reset();
    await this.start();
  }

  /** Stops the shell; its exit is shown, and a key starts a new one. */
  kill(): void {
    if (this.session === null || this.ended) return;
    this.stopped = true;
    void this.api.stop(this.session).catch(() => undefined);
  }

  clear(): void {
    this.view.terminal.clear();
  }

  private async start(): Promise<void> {
    this.starting = true;
    this.stopped = false;
    try {
      const size = this.view.fit();
      const { session } = await this.api.terminal(size);
      this.session = session;
      this.socket = new SessionSocket(sessionUrl(window.location, session), this.api.token, {
        output: (data) => this.view.write(data),
        exit: (exit) => {
          this.end(`${exitLine(exit, this.stopped)}${ansi.dim("Press any key to start a new terminal.")}\r\n`);
        },
        lost: () => {
          this.end(`\r\n${ansi.red("The connection to the terminal was lost.")} ${ansi.dim("Press any key to start a new one.")}\r\n`);
        },
      });
      this.socket.resize(size);
      for (const data of this.typed.splice(0)) this.socket.send(data);
    } catch (error) {
      const reason = error instanceof ApiError || error instanceof Error ? error.message : String(error);
      this.end(`${ansi.red(`Couldn't start a terminal: ${reason}.`)} ${ansi.dim("Press any key to try again.")}\r\n`);
    } finally {
      this.starting = false;
    }
  }

  private end(message: string): void {
    this.ended = true;
    this.typed = [];
    this.socket = null;
    this.session = null;
    this.view.write(message);
  }
}
