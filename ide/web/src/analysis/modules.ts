export interface ModuleReader {
  read(path: string): Promise<string | null>;
  openText(path: string): string | undefined;
}

export class ModuleSources {
  private reader: ModuleReader = { read: async () => null, openText: () => undefined };
  private readonly fetched = new Map<string, string | null>();
  private readonly pending = new Set<string>();
  private readonly listeners = new Set<() => void>();
  private currentVersion = 0;
  private generation = 0;

  get version(): number {
    return this.currentVersion;
  }

  connect(reader: ModuleReader): void {
    this.reader = reader;
    this.forget();
  }

  sources(): Record<string, string> {
    const sources: Record<string, string> = {};
    for (const [path, text] of this.fetched) {
      const current = this.reader.openText(path) ?? text;
      if (current !== null) sources[path] = current;
    }
    return sources;
  }

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
          // Drop stale replies if `forget()` or `connect()` ran while in flight.
          if (generation !== this.generation) return;
          this.pending.delete(path);
          this.fetched.set(path, text);
          this.bump();
        });
    }
  }

  uses(path: string): boolean {
    return this.fetched.has(path) || this.pending.has(path);
  }

  changed(path: string): void {
    if (!this.uses(path)) return;
    // Allow retry if a previously missing module was created.
    if (this.fetched.get(path) === null) this.fetched.delete(path);
    this.bump();
  }

  forget(): void {
    const had = this.fetched.size > 0 || this.pending.size > 0;
    this.generation++;
    this.fetched.clear();
    this.pending.clear();
    if (had) this.bump();
  }

  onChange(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  private bump(): void {
    this.currentVersion++;
    for (const listener of this.listeners) listener();
  }
}
