// Calls the vela-studio server's JSON API with the session token.

import type { BuildResult, EntryKind, FileContent, Info, RunResult, TerminalSize, Tree } from "./types";

/** A request the server refused, with its status and `{error, …}` body. */
export class ApiError extends Error {
  override name = "ApiError";

  constructor(
    readonly status: number,
    message: string,
    readonly body: Record<string, unknown> = {},
  ) {
    super(message);
  }

  get unauthorized(): boolean {
    return this.status === 401;
  }
  get notFound(): boolean {
    return this.status === 404;
  }
  get conflict(): boolean {
    return this.status === 409;
  }
}

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

export class StudioApi {
  private readonly fetch: Fetch;

  constructor(
    readonly token: string,
    fetch: Fetch = (input, init) => globalThis.fetch(input, init),
  ) {
    this.fetch = fetch;
  }

  info(): Promise<Info> {
    return this.call("GET", "/api/info");
  }

  files(): Promise<Tree> {
    return this.call("GET", "/api/files");
  }

  readFile(path: string): Promise<FileContent> {
    return this.call("GET", `/api/file?path=${encodeURIComponent(path)}`);
  }

  /**
   * Replaces a file's contents. With `baseHash`, the server refuses (409,
   * `body.hash` = the current hash) if the file changed since that version.
   */
  writeFile(path: string, content: string, baseHash?: string): Promise<{ hash: string }> {
    return this.call("PUT", "/api/file", baseHash === undefined ? { path, content } : { path, content, baseHash });
  }

  create(path: string, kind: EntryKind): Promise<{ path: string }> {
    return this.call("POST", "/api/file/new", { path, kind });
  }

  rename(from: string, to: string): Promise<{ path: string }> {
    return this.call("POST", "/api/file/rename", { from, to });
  }

  remove(path: string): Promise<void> {
    return this.call("POST", "/api/file/delete", { path });
  }

  /** Builds a file without running it; a failed build is a normal answer. */
  build(path: string): Promise<BuildResult> {
    return this.call("POST", "/api/build", { path });
  }

  /** Builds a file and runs it in a new session, stopping the previous run. */
  run(path: string, size: TerminalSize): Promise<RunResult> {
    return this.call("POST", "/api/run", { path, ...size });
  }

  /** Opens a shell in the workspace folder. */
  terminal(size: TerminalSize): Promise<{ session: string }> {
    return this.call("POST", "/api/terminal", size);
  }

  stop(session: string): Promise<void> {
    return this.call("POST", `/api/session/${encodeURIComponent(session)}/stop`);
  }

  private async call<T>(method: string, url: string, body?: unknown): Promise<T> {
    const headers: Record<string, string> = { authorization: `Bearer ${this.token}` };
    const init: RequestInit = { method, headers, cache: "no-store" };
    if (body !== undefined) {
      headers["content-type"] = "application/json";
      init.body = JSON.stringify(body);
    }
    let response: Response;
    try {
      response = await this.fetch(url, init);
    } catch (error) {
      throw new ApiError(0, `can't reach Vela Studio's server (${error instanceof Error ? error.message : String(error)})`);
    }
    if (response.status === 204) {
      return undefined as T;
    }
    const text = await response.text();
    let json: unknown = undefined;
    try {
      json = text ? JSON.parse(text) : undefined;
    } catch {
      // Not JSON, such as a plain-text rejection; reported below.
    }
    if (!response.ok) {
      const errorBody = json && typeof json === "object" ? (json as Record<string, unknown>) : {};
      const message = typeof errorBody["error"] === "string" ? errorBody["error"] : text || response.statusText;
      throw new ApiError(response.status, message, errorBody);
    }
    return json as T;
  }
}
