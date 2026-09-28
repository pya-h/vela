// The Problems list: the errors the analysis reports in open Vela files as
// you type, and the failures of the last build of each file (such as a missing
// `main` task or a C compiler that couldn't start). No DOM here.

import type { Diagnostic } from "../analysis/protocol";
import type { BuildFailure } from "../api/types";

export interface Problem {
  path: string;
  message: string;
  /** 1-based, when the problem has a place in the file. */
  line: number | null;
  col: number | null;
  /** The range in the open document, for live diagnostics. */
  from: number | null;
  to: number | null;
  source: "vela" | "build";
  /** A warning is something the program may not mean; it builds all the same. */
  severity: "error" | "warning";
}

export interface FileProblems {
  path: string;
  problems: Problem[];
}

/** Every open Vela file's live diagnostics and every file's last build failure. */
export function collectProblems(live: { path: string; diagnostics: Diagnostic[] }[], builds: ReadonlyMap<string, BuildFailure>): FileProblems[] {
  const byPath = new Map<string, Problem[]>();
  const add = (problem: Problem) => {
    const list = byPath.get(problem.path) ?? [];
    list.push(problem);
    byPath.set(problem.path, list);
  };
  for (const { path, diagnostics } of live) {
    for (const d of diagnostics) {
      add({ path, message: d.message, line: d.line, col: d.col, from: d.from, to: d.to, source: "vela", severity: d.severity });
    }
  }
  const analyzed = new Set(live.map((file) => file.path));
  for (const [built, failure] of builds) {
    // An error in a module the program includes is a problem of that module's file.
    const path = failure.file ?? built;
    // In an open file, the analysis shows the current Vela errors (with the
    // compiler the build used), so the build's may be out of date. (An open
    // file also shows an error in a module it includes, at its `include`.)
    if (failure.kind === "vela" && (analyzed.has(path) || analyzed.has(built))) continue;
    add({ path, message: failure.message, line: failure.line, col: failure.col, from: null, to: null, source: "build", severity: "error" });
  }
  return [...byPath.entries()]
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([path, problems]) => ({
      path,
      problems: problems.sort((a, b) => (a.line ?? 0) - (b.line ?? 0) || (a.col ?? 0) - (b.col ?? 0)),
    }));
}

export function problemCount(files: readonly FileProblems[]): number {
  return files.reduce((count, file) => count + file.problems.length, 0);
}
