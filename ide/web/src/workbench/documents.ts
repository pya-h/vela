import type { EditorState, Text } from "@codemirror/state";

export type DiskState = "in-sync" | "changed" | "deleted";

export interface OpenDocument {
  path: string;
  state: EditorState;
  saved: Text;
  hash: string;
  disk: DiskState;
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

export function isWithin(path: string, folder: string): boolean {
  return path === folder || path.startsWith(`${folder}/`);
}

export function nameProblem(name: string): string | null {
  const segments = name.split("/");
  if (segments.some((s) => s === "" || s === "." || s === "..")) return "Use plain names, separated by / for folders.";
  if (/[\\:\u0000-\u001f]/.test(name)) return "Names can't contain \\, :, or control characters.";
  if (segments.some((s) => s.startsWith("."))) return "Names starting with . are hidden in the explorer.";
  return null;
}

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
  private order: string[] = [];
  active: string | null = null;
  private readonly listeners = new Set<() => void>();

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

  markSaved(path: string, text: Text, hash: string): void {
    const doc = this.open.get(path);
    if (!doc) return;
    doc.saved = text;
    doc.hash = hash;
    doc.disk = "in-sync";
    this.changed();
  }

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

  within(folder: string): OpenDocument[] {
    return this.all.filter((doc) => isWithin(doc.path, folder));
  }
}
