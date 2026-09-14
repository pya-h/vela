import { describe, expect, it } from "vitest";

import type { ExitInfo } from "./types";
import { SessionSocket, type SocketLike, sessionUrl } from "./socket";

class FakeSocket implements SocketLike {
  binaryType = "blob";
  readyState = 0;
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onclose: ((event: CloseEvent) => void) | null = null;
  sent: (string | Uint8Array)[] = [];
  closed: number | null = null;

  constructor(readonly url: string) {}

  send(data: string | Uint8Array<ArrayBuffer>): void {
    this.sent.push(data);
  }
  close(code?: number): void {
    this.closed = code ?? 1000;
  }

  open(): void {
    this.readyState = 1;
    this.onopen?.(new Event("open"));
  }
  message(data: string | ArrayBuffer): void {
    this.onmessage?.({ data } as MessageEvent);
  }
  serverClose(code: number): void {
    this.readyState = 3;
    this.onclose?.({ code } as CloseEvent);
  }
}

function connect() {
  let fake: FakeSocket | null = null;
  const events: { output: string[]; exit: ExitInfo[]; lost: number[] } = { output: [], exit: [], lost: [] };
  const socket = new SessionSocket(
    "ws://127.0.0.1:4000/api/session/s1/ws",
    "secret",
    {
      output: (data) => events.output.push(new TextDecoder().decode(data)),
      exit: (exit) => events.exit.push(exit),
      lost: (code) => events.lost.push(code),
    },
    (url) => (fake = new FakeSocket(url)),
  );
  return { socket, fake: fake! as FakeSocket, events };
}

describe("SessionSocket", () => {
  it("sends the token first, then the size and anything typed while connecting", () => {
    const { socket, fake } = connect();
    expect(fake.binaryType).toBe("arraybuffer");
    socket.resize({ cols: 80, rows: 24 });
    socket.send("ls\r");
    expect(fake.sent).toEqual([]);
    fake.open();
    expect(fake.sent[0]).toBe(JSON.stringify({ type: "auth", token: "secret" }));
    expect(fake.sent[1]).toBe(JSON.stringify({ type: "resize", cols: 80, rows: 24 }));
    expect(new TextDecoder().decode(fake.sent[2] as Uint8Array)).toBe("ls\r");

    // Once connected, messages go straight out; an unchanged size isn't resent.
    socket.send(new Uint8Array([3]));
    socket.resize({ cols: 80, rows: 24 });
    socket.resize({ cols: 100, rows: 24 });
    expect(fake.sent.slice(3)).toEqual([new Uint8Array([3]), JSON.stringify({ type: "resize", cols: 100, rows: 24 })]);
  });

  it("delivers output, then the exit, and doesn't report the closing that follows", () => {
    const { socket, fake, events } = connect();
    fake.open();
    fake.message(new TextEncoder().encode("Hello\r\n").buffer as ArrayBuffer);
    fake.message("not json");
    fake.message(JSON.stringify({ type: "exit", code: 3, signal: null, durationMs: 12 }));
    fake.serverClose(1000);
    expect(events).toEqual({ output: ["Hello\r\n"], exit: [{ code: 3, signal: null, durationMs: 12 }], lost: [] });
    expect(socket.isEnded).toBe(true);
    socket.send("ignored");
    expect(fake.sent).toHaveLength(1);
  });

  it("reports a connection that ends without an exit", () => {
    const { fake, events } = connect();
    fake.open();
    fake.serverClose(4404);
    expect(events.lost).toEqual([4404]);
  });

  it("closes quietly when disposed", () => {
    const { socket, fake, events } = connect();
    fake.open();
    socket.dispose();
    fake.serverClose(1000);
    fake.message(new Uint8Array([65]).buffer);
    expect([fake.closed, events.lost, events.output]).toEqual([1000, [], []]);
  });

  it("addresses the page's own server", () => {
    expect(sessionUrl({ protocol: "http:", host: "127.0.0.1:4000" }, "a b")).toBe("ws://127.0.0.1:4000/api/session/a%20b/ws");
    expect(sessionUrl({ protocol: "https:", host: "localhost:1" }, "x")).toBe("wss://localhost:1/api/session/x/ws");
  });
});
