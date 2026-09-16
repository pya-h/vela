// The JSON the vela-studio server exchanges, mirroring ide/server/src/api.rs.

export interface Info {
  name: string;
  version: string;
  workspace: { name: string; root: string };
  cc: { command: string; available: boolean };
  platform: string;
}

export type EntryKind = "file" | "folder";

export interface Entry {
  name: string;
  /** Relative to the workspace, `/`-separated. */
  path: string;
  kind: EntryKind;
  /** A folder's entries: folders first, then files. */
  children?: Entry[];
}

export interface Tree {
  entries: Entry[];
  /** Whether entries were left out because the workspace is too large. */
  truncated: boolean;
}

export interface FileContent {
  path: string;
  content: string;
  /** Identifies this version of the file, for noticing changes on disk. */
  hash: string;
}

/** Why a build failed (`Failure` in ide/server/src/compile.rs). */
export interface BuildFailure {
  kind: "vela" | "no-main" | "compiler-missing" | "c-failed" | "io";
  /** The error on its own, such as `unknown variable 'x'`. */
  message: string;
  /** 1-based, for `vela` errors. */
  /** The file the error is in: the file built, or a module it uses. */
  file: string | null;
  line: number | null;
  col: number | null;
  /** The error as the `vela` command prints it. */
  rendered: string;
}

export type BuildResult = { ok: true } | { ok: false; error: BuildFailure };

export type RunResult = { ok: true; session: string } | { ok: false; error: BuildFailure };

/** How a session's process ended (the WebSocket's `exit` message). */
export interface ExitInfo {
  /** The exit code; 1 when ended by a signal. */
  code: number;
  /** The signal that ended the process, such as `Hangup`, if any. */
  signal: string | null;
  durationMs: number;
}

export interface TerminalSize {
  cols: number;
  rows: number;
}
