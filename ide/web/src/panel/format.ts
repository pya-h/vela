// Text for the Output and Terminal views: durations, exit lines, and the ANSI
// styling that xterm.js draws. No DOM here, so it can be tested alone.

import type { ExitInfo } from "../api/types";

/** `3 ms`, `1.2 s`, `2 min 5 s`. */
export function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.max(0, Math.round(ms))} ms`;
  if (ms < 60_000) return `${(Math.floor(ms / 100) / 10).toFixed(1)} s`;
  const minutes = Math.floor(ms / 60_000);
  const seconds = Math.floor((ms % 60_000) / 1000);
  return seconds === 0 ? `${minutes} min` : `${minutes} min ${seconds} s`;
}

export interface ExitSummary {
  /** Such as `exited with code 0` or `stopped`. */
  text: string;
  /** Whether the program ended normally with code 0. */
  ok: boolean;
}

/** How a process ended, in words. `stopped` says the user asked it to stop. */
export function exitSummary(exit: ExitInfo, stopped: boolean): ExitSummary {
  // A stopped process may still exit with a code of its own (zsh exits with 1 on a hangup).
  if (stopped) {
    return { text: "stopped", ok: false };
  }
  if (exit.signal !== null) {
    return { text: `ended by signal ${exit.signal}`, ok: false };
  }
  return { text: `exited with code ${exit.code}`, ok: exit.code === 0 };
}

const ESC = "\x1b[";
export const ansi = {
  dim: (text: string) => `${ESC}2m${text}${ESC}22m`,
  bold: (text: string) => `${ESC}1m${text}${ESC}22m`,
  red: (text: string) => `${ESC}31m${text}${ESC}39m`,
  green: (text: string) => `${ESC}32m${text}${ESC}39m`,
  cyan: (text: string) => `${ESC}36m${text}${ESC}39m`,
};

/** Text for a terminal: every line break as CR LF, which a terminal needs. */
export function terminalLines(text: string): string {
  return text.replace(/\r?\n/g, "\r\n");
}

/**
 * The line that closes a run in Output: `── exited with code 0 · 3 ms ──`,
 * starting on a fresh line, dim when the program succeeded and red otherwise.
 */
export function exitLine(exit: ExitInfo, stopped: boolean): string {
  const { text, ok } = exitSummary(exit, stopped);
  const line = `── ${text} · ${formatDuration(exit.durationMs)} ──`;
  return `\r\n${ok ? ansi.dim(line) : ansi.red(line)}\r\n`;
}
