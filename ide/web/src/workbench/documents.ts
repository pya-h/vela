// The files open in tabs: their editor state, what was last saved, and how
// they relate to the file on disk. No DOM here, so it can be tested alone.

import type { EditorState, Text } from "@codemirror/state";

export type DiskState =
  /** The file matches what the editor last read or saved. */
  | "in-sync"
  /** Another program changed the file while the editor had unsaved edits. */
  | "changed"
  /** The file was deleted or moved away. */
  | "deleted";

export interface OpenDocument {
  path: string;
  state: EditorState;
  /** The text as last read from or saved to disk. */
  saved: Text;
  /** The file's hash for `saved`, sent with the next save. */
  hash: string;
  disk: DiskState;
  /** Where the editor was scrolled when the tab was left. */
  scrollTop: number;
}

export function isDirty(doc: OpenDocument): boolean {
  return doc.disk === "deleted" || !doc.state.doc.eq(doc.saved);
}

export function fileName(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

export function parentFolder(path: string): string {
  const slash = path.lastIndexOf("/");
  return slash < 0 ? "" : path.slice(0, slash);
}

/** Whether `path` is `folder` itself or inside it. */
export function isWithin(path: string, folder: string): boolean {
  return path === folder || path.startsWith(`${folder}/`);
}

/** Why a name can't be used for a new or renamed entry, or null if it can. */
export function nameProblem(name: string): string | null {
  const segments = name.split("/");
  if (segments.some((s) => s === "" || s === "." || s === "..")) return "Use plain names, separated by / for folders.";
  if (/[\\:\u0000-\u001f]/.test(name)) return "Names can't contain \\, :, or control characters.";
  if (segments.some((s) => s.startsWith("."))) return "Names starting with . are hidden in the explorer.";
  return null;
}

/**
 * The smallest single change turning `before` into `after` (the differing
 * middle, between their common start and end), or null if they are equal.
 */
export function textChange(before: string, after: string): { from: number; to: number; insert: string } | null {
  if (before === after) return null;
  const shorter = Math.min(before.length, after.length);
  let start = 0;
  while (start < shorter && before.charCodeAt(start) === after.charCodeAt(start)) start++;
  let end = 0;
  while (end < shorter - start && before.charCodeAt(before.length - 1 - end) === after.charCodeAt(after.length - 1 - end)) end++;
  return { from: start, to: before.length - end, insert: after.slice(start, after.length - end) };
}

export class Documents {
  private readonly open = new Map<string, OpenDocument>();
  /** Tab order. */
  private order: string[] = [];
  active: string | null = null;
  private readonly listeners = new Set<() => void>();

  /** Called after any change to the set of documents, their order, or state. */
  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  changed(): void {
    for (const listener of this.listeners) listener();
  }

  get(path: string): OpenDocument | undefined {
    return this.open.get(path);
  }

  get all(): OpenDocument[] {
    return this.order.flatMap((path) => this.open.get(path) ?? []);
  }

  get current(): OpenDocument | null {
    return (this.active !== null && this.open.get(this.active)) || null;
  }

  add(doc: OpenDocument): void {
    if (!this.open.has(doc.path)) this.order.push(doc.path);
    this.open.set(doc.path, doc);
    this.changed();
  }

  /** Removes a document; returns the path that should become active. */
  remove(path: string): string | null {
    const index = this.order.indexOf(path);
    if (index < 0) return this.active;
    this.open.delete(path);
    this.order.splice(index, 1);
    if (this.active === path) {
      this.active = this.order[Math.min(index, this.order.length - 1)] ?? null;
    }
    this.changed();
    return this.active;
  }

  /** Records a save or reload: `text` is now what the file on disk holds. */
  markSaved(path: string, text: Text, hash: string): void {
    const doc = this.open.get(path);
    if (!doc) return;
    doc.saved = text;
    doc.hash = hash;
    doc.disk = "in-sync";
    this.changed();
  }

  /** Updates paths after a file or folder was renamed from `from` to `to`. */
  renamed(from: string, to: string): void {
    const moves = this.order.filter((path) => isWithin(path, from)).map((path) => [path, to + path.slice(from.length)] as const);
    if (moves.length === 0) return;
    for (const [oldPath, newPath] of moves) {
      const doc = this.open.get(oldPath);
      if (!doc) continue;
      this.open.delete(oldPath);
      doc.path = newPath;
      this.open.set(newPath, doc);
      this.order = this.order.map((path) => (path === oldPath ? newPath : path));
      if (this.active === oldPath) this.active = newPath;
    }
    this.changed();
  }

  /** The open documents inside `folder` (or that file). */
  within(folder: string): OpenDocument[] {
    return this.all.filter((doc) => isWithin(doc.path, folder));
  }
}
