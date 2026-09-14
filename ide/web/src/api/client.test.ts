import { describe, expect, it } from "vitest";

import { ApiError, StudioApi } from "./client";
import { takeToken } from "./session";

interface Call {
  url: string;
  init: RequestInit;
}

function fakeServer(status: number, body: string): { api: StudioApi; calls: Call[] } {
  const calls: Call[] = [];
  const api = new StudioApi("secret", async (url, init) => {
    calls.push({ url, init });
    return new Response(status === 204 ? null : body, { status });
  });
  return { api, calls };
}

describe("StudioApi", () => {
  it("sends the token and JSON bodies", async () => {
    const { api, calls } = fakeServer(200, JSON.stringify({ hash: "abc" }));
    expect(await api.writeFile("a b/x.vela", "text", "old")).toEqual({ hash: "abc" });
    await api.readFile("a b/x.vela");
    const [write, read] = calls;
    expect(write?.url).toBe("/api/file");
    expect(write?.init.method).toBe("PUT");
    expect(write?.init.headers).toMatchObject({ authorization: "Bearer secret", "content-type": "application/json" });
    expect(JSON.parse(String(write?.init.body))).toEqual({ path: "a b/x.vela", content: "text", baseHash: "old" });
    expect(read?.url).toBe("/api/file?path=a%20b%2Fx.vela");
  });

  it("leaves out a missing base hash", async () => {
    const { api, calls } = fakeServer(200, "{}");
    await api.writeFile("x.vela", "text");
    expect(JSON.parse(String(calls[0]?.init.body))).toEqual({ path: "x.vela", content: "text" });
  });

  it("reports the server's error and body", async () => {
    const { api } = fakeServer(409, JSON.stringify({ error: "the file changed on disk", hash: "new" }));
    const error = await api.writeFile("x.vela", "", "old").catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ApiError);
    const apiError = error as ApiError;
    expect([apiError.status, apiError.message, apiError.conflict, apiError.body["hash"]]).toEqual([409, "the file changed on disk", true, "new"]);
  });

  it("handles empty and non-JSON replies", async () => {
    expect(await fakeServer(204, "").api.remove("x.vela")).toBeUndefined();
    const error = (await fakeServer(422, "Failed to deserialize").api.files().catch((e: unknown) => e)) as ApiError;
    expect([error.status, error.message]).toEqual([422, "Failed to deserialize"]);
    const offline = new StudioApi("t", async () => {
      throw new TypeError("Failed to fetch");
    });
    const unreachable = (await offline.info().catch((e: unknown) => e)) as ApiError;
    expect(unreachable.status).toBe(0);
    expect(unreachable.message).toContain("Failed to fetch");
  });
});

describe("takeToken", () => {
  function storage(initial: Record<string, string> = {}) {
    const data = new Map(Object.entries(initial));
    return { data, getItem: (k: string) => data.get(k) ?? null, setItem: (k: string, v: string) => void data.set(k, v) };
  }

  it("moves the token from the address to session storage", () => {
    const store = storage();
    const replaced: string[] = [];
    const history = { replaceState: (_: unknown, __: string, url: string) => void replaced.push(url) };
    expect(takeToken({ hash: "#token=abc123", pathname: "/", search: "?x=1" }, history, store)).toBe("abc123");
    expect(replaced).toEqual(["/?x=1"]);
    expect(store.data.get("vela-studio:token")).toBe("abc123");
  });

  it("falls back to the stored token, or none", () => {
    const history = { replaceState: () => {} };
    expect(takeToken({ hash: "", pathname: "/", search: "" }, history, storage({ "vela-studio:token": "kept" }))).toBe("kept");
    expect(takeToken({ hash: "#other=1", pathname: "/", search: "" }, history, storage())).toBeNull();
    expect(takeToken({ hash: "", pathname: "/", search: "" }, history, null)).toBeNull();
  });
});
