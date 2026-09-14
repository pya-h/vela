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
  | "field"
  | "number"
  | "string"
  | "escape"
  | "comment"
  | "operator"
  | "punctuation"
  | "invalid";

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

export type SymbolKind = "task" | "shape" | "kind" | "variant" | "field" | "parameter" | "variable";

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
  | "type";

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
  branchKeywords: string[];
  blockCloser: string;
  lineComment: string;
  brackets: [string, string][];
}

export type Query =
  | { kind: "tokens" }
  | { kind: "diagnostics" }
  | { kind: "outline" }
  | { kind: "definition"; offset: number }
  | { kind: "complete"; offset: number }
  | { kind: "hover"; offset: number }
  | { kind: "language" };

/** The result type of each query kind. */
export interface QueryResults {
  tokens: Token[];
  diagnostics: Diagnostic[];
  outline: OutlineItem[];
  definition: Location | null;
  complete: Completions;
  hover: Hover | null;
  language: Language;
}
