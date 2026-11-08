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
  path: string;
  kind: EntryKind;
  children?: Entry[];
}

export interface Tree {
  entries: Entry[];
  truncated: boolean;
}

export interface FileContent {
  path: string;
  content: string;
  hash: string;
}

export interface BuildFailure {
  kind: "vela" | "no-main" | "compiler-missing" | "c-failed" | "io";
  message: string;
  file: string | null;
  line: number | null;
  col: number | null;
  rendered: string;
}

export type BuildResult = { ok: true } | { ok: false; error: BuildFailure };

export type RunResult = { ok: true; session: string } | { ok: false; error: BuildFailure };

export interface ExitInfo {
  code: number;
  signal: string | null;
  durationMs: number;
}

export interface TerminalSize {
  cols: number;
  rows: number;
}
