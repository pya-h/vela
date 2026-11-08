import type { Diagnostic } from "../analysis/protocol";
import type { BuildFailure } from "../api/types";

export interface Problem {
  path: string;
  message: string;
  line: number | null;
  col: number | null;
  from: number | null;
  to: number | null;
  source: "vela" | "build";
  severity: "error" | "warning";
}

export interface FileProblems {
  path: string;
  problems: Problem[];
}

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
    const path = failure.file ?? built;
    // Live analysis already holds up-to-date Vela errors for open files (and module errors on their include line).
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

export function severityCounts(files: readonly FileProblems[]): { errors: number; warnings: number } {
  const warnings = files.reduce((count, file) => count + file.problems.filter((problem) => problem.severity === "warning").length, 0);
  return { errors: problemCount(files) - warnings, warnings };
}
