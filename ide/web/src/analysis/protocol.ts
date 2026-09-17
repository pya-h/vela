// The JSON the analysis WebAssembly module returns, mirroring the Rust types
// in ide/analysis (serde's output). All offsets are UTF-16 offsets, the same
// as JavaScript string indices.

export type TokenClass =
  | "keyword"
  | "type-builtin"
  | "type"
  | "variant"
  | "task"
  | "builtin-fn"
  | "parameter"
  | "variable"
  | "region"
  | "field"
  | "number"
  | "string"
  | "escape"
  | "comment"
  | "operator"
  | "punctuation"
  | "invalid"
  | "module";

export interface Token {
  from: number;
  to: number;
  class: TokenClass;
}

export interface Diagnostic {
  from: number;
  to: number;
  /** The compiler's 1-based line and column; null for an internal compiler error. */
  line: number | null;
  col: number | null;
  message: string;
  severity: "error";
}

export type SymbolKind = "task" | "shape" | "kind" | "variant" | "field" | "parameter" | "variable" | "region" | "module";

export interface OutlineItem {
  name: string;
  kind: SymbolKind;
  detail: string;
  from: number;
  to: number;
  nameFrom: number;
  nameTo: number;
  children: OutlineItem[];
}

export interface Location {
  from: number;
  to: number;
  /** The file it is in, when that isn't the analyzed file. */
  path?: string;
}

export type CompletionKind =
  | "keyword"
  | "snippet"
  | "task"
  | "builtin-fn"
  | "variable"
  | "parameter"
  | "field"
  | "variant"
  | "type"
  | "region"
  | "module";

export interface CompletionItem {
  label: string;
  kind: CompletionKind;
  detail: string;
  /** Text to insert instead of the label. */
  apply?: string;
  /** A CodeMirror snippet template to insert instead of the label. */
  snippet?: string;
  /** How much to prefer this item among equally good matches. */
  boost: number;
}

export interface Completions {
  /** The items replace the text from `from` to `to`. */
  from: number;
  to: number;
  items: CompletionItem[];
}

export interface Hover {
  from: number;
  to: number;
  /** Vela source describing the name, such as `task fib(n: int) -> int`. */
  code: string;
  doc: string;
}

export interface Language {
  keywords: string[];
  builtinTypes: string[];
  builtinTasks: string[];
  blockOpeners: string[];
  /** Words that may come before a block opener on its line, as in `priv task`. */
  blockModifiers: string[];
  branchKeywords: string[];
  blockCloser: string;
  lineComment: string;
  brackets: [string, string][];
}

/**
 * Where the analyzed file is, and the text of the other files of its program
 * (the modules it includes) by path. Paths are `/`-separated workspace paths.
 */
export interface ProgramContext {
  path: string;
  modules: Record<string, string>;
}

export type Query =
  | { kind: "tokens" }
  | { kind: "diagnostics" }
  | { kind: "outline" }
  | { kind: "definition"; offset: number }
  | { kind: "complete"; offset: number }
  | { kind: "hover"; offset: number }
  | { kind: "missing-modules" }
  | { kind: "language" };

/** The result type of each query kind. */
export interface QueryResults {
  tokens: Token[];
  diagnostics: Diagnostic[];
  outline: OutlineItem[];
  definition: Location | null;
  complete: Completions;
  hover: Hover | null;
  /** The paths of the files the program includes that the context didn't give. */
  "missing-modules": string[];
  language: Language;
}
