import type { ExitInfo, TerminalSize } from "./types";

export const CLOSE_UNAUTHORIZED = 4401;
export const CLOSE_NO_SESSION = 4404;
export const CLOSE_REPLACED = 4409;

export interface SessionHandlers {
  output(data: Uint8Array): void;
  exit(exit: ExitInfo): void;
  lost(code: number): void;
}

export interface SocketLike {
  binaryType: string;
  readonly readyState: number;
  onopen: ((event: Event) => void) | null;
  onmessage: ((event: MessageEvent) => void) | null;
  onclose: ((event: CloseEvent) => void) | null;
  send(data: string | Uint8Array<ArrayBuffer>): void;
  close(code?: number, reason?: string): void;
}

const CONNECTING = 0;
const OPEN = 1;

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
  private size: TerminalSize | null = null;
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

  get isEnded(): boolean {
    return this.ended;
  }

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

  dispose(): void {
    this.ended = true;
    // Closing a CONNECTING WebSocket logs a console error in browsers; defer until open.
    if (this.socket.readyState === CONNECTING) {
      this.socket.onopen = () => this.socket.close(1000, "closed by the page");
    } else {
      this.socket.close(1000, "closed by the page");
    }
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
