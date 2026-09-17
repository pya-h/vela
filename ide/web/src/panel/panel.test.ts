import { describe, expect, it } from "vitest";

import type { Diagnostic } from "../analysis/protocol";
import type { BuildFailure } from "../api/types";
import { exitLine, exitSummary, formatDuration, terminalLines } from "./format";
import { collectProblems, problemCount } from "./problems";

describe("run output", () => {
  it("formats durations", () => {
    expect([0, 3, 999, 1000, 1250, 59_999, 60_000, 125_400].map(formatDuration)).toEqual([
      "0 ms",
      "3 ms",
      "999 ms",
      "1.0 s",
      "1.2 s",
      "59.9 s",
      "1 min",
      "2 min 5 s",
    ]);
  });

  it("describes how a program ended", () => {
    expect(exitSummary({ code: 0, signal: null, durationMs: 1 }, false)).toEqual({ text: "exited with code 0", ok: true });
    expect(exitSummary({ code: 2, signal: null, durationMs: 1 }, false)).toEqual({ text: "exited with code 2", ok: false });
    expect(exitSummary({ code: 1, signal: "Hangup", durationMs: 1 }, true)).toEqual({ text: "stopped", ok: false });
    expect(exitSummary({ code: 1, signal: "Killed", durationMs: 1 }, false).text).toBe("ended by signal Killed");
    expect(exitSummary({ code: 1, signal: null, durationMs: 1 }, true)).toEqual({ text: "stopped", ok: false });
  });

  it("writes the closing line on its own line, styled", () => {
    expect(exitLine({ code: 0, signal: null, durationMs: 3 }, false)).toBe("\r\n\x1b[2m── exited with code 0 · 3 ms ──\x1b[22m\r\n");
    expect(exitLine({ code: 1, signal: null, durationMs: 3 }, false)).toContain("\x1b[31m");
    expect(terminalLines("a\nb\r\nc")).toBe("a\r\nb\r\nc");
  });
});

describe("problems", () => {
  const diagnostic = (line: number, message: string): Diagnostic => ({ from: line * 10, to: line * 10 + 3, line, col: 5, message, severity: "error" });
  const failure = (kind: BuildFailure["kind"], message: string, line: number | null = null, file: string | null = null): BuildFailure => ({
    kind,
    message,
    file,
    line,
    col: line === null ? null : 5,
    rendered: message,
  });

  it("lists live errors and build failures by file, in order", () => {
    const files = collectProblems(
      [
        { path: "z.vela", diagnostics: [diagnostic(9, "late"), diagnostic(2, "early")] },
        { path: "clean.vela", diagnostics: [] },
      ],
      new Map([["a.vela", failure("no-main", "program has no 'main' task")]]),
    );
    expect(files.map((f) => [f.path, f.problems.map((p) => [p.message, p.source, p.line])])).toEqual([
      ["a.vela", [["program has no 'main' task", "build", null]]],
      ["z.vela", [["early", "vela", 2], ["late", "vela", 9]]],
    ]);
    expect(problemCount(files)).toBe(3);
    expect(files[1]?.problems[0]).toMatchObject({ from: 20, to: 23 });
  });

  it("shows a build's Vela errors only for files the analysis doesn't cover", () => {
    const live = [{ path: "a.vela", diagnostics: [diagnostic(3, "unknown variable 'x'")] }];
    const same = collectProblems(live, new Map([["a.vela", failure("vela", "unknown variable 'x'", 3)]]));
    expect(problemCount(same)).toBe(1);
    // Fixed since the build: the build's error is out of date.
    const fixed = collectProblems([{ path: "a.vela", diagnostics: [] }], new Map([["a.vela", failure("vela", "unknown variable 'x'", 3)]]));
    expect(problemCount(fixed)).toBe(0);
    const closed = collectProblems([], new Map([["a.vela", failure("vela", "unknown variable 'x'", 3)]]));
    expect(closed[0]?.problems[0]).toMatchObject({ source: "build", line: 3, col: 5 });
    const other = collectProblems(live, new Map([["a.vela", failure("c-failed", "C compilation failed")]]));
    expect(problemCount(other)).toBe(2);
  });

  it("puts a build error in a module under that module's file", () => {
    const inModule = failure("vela", "unknown variable 'x'", 4, "lib/geometry.vela");
    const closed = collectProblems([], new Map([["main.vela", inModule]]));
    expect(closed.map((f) => f.path)).toEqual(["lib/geometry.vela"]);
    expect(closed[0]?.problems[0]).toMatchObject({ line: 4, source: "build" });
    // With the program's file open, its analysis shows the error at the `include`.
    const open = collectProblems([{ path: "main.vela", diagnostics: [diagnostic(1, "lib/geometry.vela:4:5: unknown variable 'x'")] }], new Map([["main.vela", inModule]]));
    expect(problemCount(open)).toBe(1);
  });
});

describe("terminal keys and links", async () => {
  const { isWorkbenchShortcut, openLink } = await import("./terminal");
  const key = (init: Partial<KeyboardEvent>) => ({ ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, ...init }) as KeyboardEvent;

  it("leaves Ctrl to the shell on a Mac, where ⌘ is the shortcut modifier", () => {
    expect(isWorkbenchShortcut(key({ key: "p", ctrlKey: true }), false)).toBe(true);
    expect(isWorkbenchShortcut(key({ key: "p", metaKey: true }), false)).toBe(false);
    expect(isWorkbenchShortcut(key({ key: "p", ctrlKey: true }), true)).toBe(false);
    expect(isWorkbenchShortcut(key({ key: "p", metaKey: true }), true)).toBe(true);
    expect(isWorkbenchShortcut(key({ key: "b", ctrlKey: true }), false)).toBe(false);
  });

  it("opens only web links, and only with Ctrl or ⌘", () => {
    const opened: string[] = [];
    const open = (uri: string) => void opened.push(uri);
    const click = (init: Partial<MouseEvent>) => ({ ctrlKey: false, metaKey: false, ...init }) as MouseEvent;
    expect(openLink(click({}), "https://example.com", open)).toBe(false);
    expect(openLink(click({ ctrlKey: true }), "javascript:alert(1)", open)).toBe(false);
    expect(openLink(click({ metaKey: true }), "file:///etc/passwd", open)).toBe(false);
    expect(openLink(click({ ctrlKey: true }), "https://example.com", open)).toBe(true);
    expect(opened).toEqual(["https://example.com"]);
  });
});
