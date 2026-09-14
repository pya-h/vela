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
  });

  it("writes the closing line on its own line, styled", () => {
    expect(exitLine({ code: 0, signal: null, durationMs: 3 }, false)).toBe("\r\n\x1b[2m── exited with code 0 · 3 ms ──\x1b[22m\r\n");
    expect(exitLine({ code: 1, signal: null, durationMs: 3 }, false)).toContain("\x1b[31m");
    expect(terminalLines("a\nb\r\nc")).toBe("a\r\nb\r\nc");
  });
});

describe("problems", () => {
  const diagnostic = (line: number, message: string): Diagnostic => ({ from: line * 10, to: line * 10 + 3, line, col: 5, message, severity: "error" });
  const failure = (kind: BuildFailure["kind"], message: string, line: number | null = null): BuildFailure => ({
    kind,
    message,
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
});
