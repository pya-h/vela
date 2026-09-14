// The WebSocket connecting a terminal view to a session on the server (see
// ide/server/src/ws.rs): the token goes first, then binary messages carry
// keystrokes and output, and text messages carry resizes and the exit.

import type { ExitInfo, TerminalSize } from "./types";

/** Close codes the server uses. */
export const CLOSE_UNAUTHORIZED = 4401;
export const CLOSE_NO_SESSION = 4404;
export const CLOSE_REPLACED = 4409;

export interface SessionHandlers {
  /** Terminal output, starting with what the session printed before connecting. */
  output(data: Uint8Array): void;
  /** The process ended; the connection closes right after. */
  exit(exit: ExitInfo): void;
  /** The connection ended without an exit, with the WebSocket close code. */
  lost(code: number): void;
}

/** The parts of the browser's WebSocket used here, so tests can stand in. */
export interface SocketLike {
  binaryType: string;
  readonly readyState: number;
  onopen: ((event: Event) => void) | null;
  onmessage: ((event: MessageEvent) => void) | null;
  onclose: ((event: CloseEvent) => void) | null;
  send(data: string | Uint8Array<ArrayBuffer>): void;
  close(code?: number, reason?: string): void;
}

const OPEN = 1;

/** The WebSocket address of a session, on the page's own server. */
export function sessionUrl(place: { protocol: string; host: string }, id: string): string {
  const scheme = place.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${place.host}/api/session/${encodeURIComponent(id)}/ws`;
}

function isExit(value: unknown): value is ExitInfo & { type: "exit" } {
  if (typeof value !== "object" || value === null) return false;
  const message = value as Record<string, unknown>;
  return (
    message["type"] === "exit" &&
    typeof message["code"] === "number" &&
    (message["signal"] === null || typeof message["signal"] === "string") &&
    typeof message["durationMs"] === "number"
  );
}

const encoder = new TextEncoder();

export class SessionSocket {
  private readonly socket: SocketLike;
  private authenticated = false;
  private ended = false;
  /** The latest size, sent once connected. */
  private size: TerminalSize | null = null;
  /** Keystrokes typed while connecting. */
  private readonly queued: Uint8Array<ArrayBuffer>[] = [];

  constructor(url: string, token: string, private readonly handlers: SessionHandlers, open: (url: string) => SocketLike = (u) => new WebSocket(u)) {
    this.socket = open(url);
    this.socket.binaryType = "arraybuffer";
    this.socket.onopen = () => {
      this.socket.send(JSON.stringify({ type: "auth", token }));
      this.authenticated = true;
      if (this.size) this.sendResize(this.size);
      for (const bytes of this.queued.splice(0)) this.socket.send(bytes);
    };
    this.socket.onmessage = (event) => this.receive(event.data);
    this.socket.onclose = (event) => {
      if (this.ended) return;
      this.ended = true;
      this.handlers.lost(event.code);
    };
  }

  /** Whether the process has exited or the connection is gone. */
  get isEnded(): boolean {
    return this.ended;
  }

  /** Sends keystrokes (text is sent as UTF-8). */
  send(data: string | Uint8Array<ArrayBuffer>): void {
    if (this.ended) return;
    const bytes = typeof data === "string" ? encoder.encode(data) : data;
    if (this.authenticated && this.socket.readyState === OPEN) this.socket.send(bytes);
    else this.queued.push(bytes);
  }

  resize(size: TerminalSize): void {
    if (this.size && this.size.cols === size.cols && this.size.rows === size.rows) return;
    this.size = size;
    if (this.authenticated && this.socket.readyState === OPEN) this.sendResize(size);
  }

  /** Disconnects without reporting anything (the session keeps running until stopped or idle). */
  dispose(): void {
    this.ended = true;
    this.socket.close(1000, "closed by the page");
  }

  private sendResize(size: TerminalSize): void {
    this.socket.send(JSON.stringify({ type: "resize", cols: size.cols, rows: size.rows }));
  }

  private receive(data: unknown): void {
    if (this.ended) return;
    if (data instanceof ArrayBuffer) {
      this.handlers.output(new Uint8Array(data));
      return;
    }
    if (typeof data !== "string") return;
    let message: unknown;
    try {
      message = JSON.parse(data);
    } catch {
      return;
    }
    if (isExit(message)) {
      this.ended = true;
      this.handlers.exit({ code: message.code, signal: message.signal, durationMs: message.durationMs });
    }
  }
}
