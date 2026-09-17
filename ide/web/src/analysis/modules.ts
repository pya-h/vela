// The text of the files a Vela file includes (its modules), for the analysis,
// which reads nothing itself. An open file's editor text is used, so unsaved
// changes count; any other file is fetched once, when an analysis asks for it,
// and kept until it is forgotten (when files may have changed on disk). No DOM
// here, so it can be tested alone.

export interface ModuleReader {
  /** A file's text from the workspace, or null if it can't be read. */
  read(path: string): Promise<string | null>;
  /** An open file's current text in the editor, if it is open. */
  openText(path: string): string | undefined;
}

export class ModuleSources {
  private reader: ModuleReader = { read: async () => null, openText: () => undefined };
  /** Fetched text, or null for a file that couldn't be read. */
  private readonly fetched = new Map<string, string | null>();
  private readonly pending = new Set<string>();
  private readonly listeners = new Set<() => void>();
  private currentVersion = 0;
  /** Changes when fetched files are forgotten, so answers to older requests are dropped. */
  private generation = 0;

  /** Changes whenever the modules' text may have changed, so analyses can be redone. */
  get version(): number {
    return this.currentVersion;
  }

  /** Reads files through `reader` from now on. */
  connect(reader: ModuleReader): void {
    this.reader = reader;
    this.forget();
  }

  /** Every module's text known now, by path: open files first. */
  sources(): Record<string, string> {
    const sources: Record<string, string> = {};
    for (const [path, text] of this.fetched) {
      const current = this.reader.openText(path) ?? text;
      if (current !== null) sources[path] = current;
    }
    return sources;
  }

  /**
   * Fetches the files among `paths` not asked for before. Each file that
   * arrives (or turns out not to exist) changes the version and notifies the
   * listeners, so that analyses using it are redone.
   */
  request(paths: readonly string[]): void {
    for (const path of paths) {
      if (this.fetched.has(path) || this.pending.has(path)) continue;
      const open = this.reader.openText(path);
      if (open !== undefined) {
        this.fetched.set(path, open);
        this.bump();
        continue;
      }
      this.pending.add(path);
      const generation = this.generation;
      void this.reader
        .read(path)
        .catch(() => null)
        .then((text) => {
          // A forget or reconnect since the request makes this answer stale.
          if (generation !== this.generation) return;
          this.pending.delete(path);
          this.fetched.set(path, text);
          this.bump();
        });
    }
  }

  /** Whether an analysis has asked for `path`, so a change to it matters. */
  uses(path: string): boolean {
    return this.fetched.has(path) || this.pending.has(path);
  }

  /**
   * Records that the text of `path` changed (in the editor or on disk), or
   * that the file was created.
   */
  changed(path: string): void {
    if (!this.uses(path)) return;
    // A file that couldn't be read may exist now: it is read when next asked for.
    if (this.fetched.get(path) === null) this.fetched.delete(path);
    this.bump();
  }

  /**
   * Forgets every fetched file, to be fetched again when asked for. Requests
   * still in flight are dropped too, so the analyses that made them are told
   * to run again and ask anew.
   */
  forget(): void {
    const had = this.fetched.size > 0 || this.pending.size > 0;
    this.generation++;
    this.fetched.clear();
    this.pending.clear();
    if (had) this.bump();
  }

  /** Calls `listener` whenever the version changes; returns a function that stops it. */
  onChange(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  private bump(): void {
    this.currentVersion++;
    for (const listener of this.listeners) listener();
  }
}
