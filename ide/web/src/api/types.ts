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
