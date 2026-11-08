/** Matches `ide/analysis` serde JSON; all offsets are UTF-16 code units. */

export type TokenClass =
  | "keyword"
  | "type-builtin"
  | "type"
  | "variant"
  | "task"
  | "method"
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
  line: number | null;
  col: number | null;
  message: string;
  severity: "error" | "warning";
}

export type SymbolKind =
  | "task"
  | "shape"
  | "kind"
  | "alias"
  | "variant"
  | "method"
  | "field"
  | "parameter"
  | "variable"
  | "region"
  | "module"
  | "type-param"
  | "macro"
  | "pattern";

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
  | "method"
  | "variant"
  | "type"
  | "region"
  | "module";

export interface CompletionItem {
  label: string;
  kind: CompletionKind;
  detail: string;
  apply?: string;
  snippet?: string;
  boost: number;
}

export interface Completions {
  from: number;
  to: number;
  items: CompletionItem[];
}

export interface Hover {
  from: number;
  to: number;
  code: string;
  doc: string;
}

export interface Language {
  keywords: string[];
  builtinTypes: string[];
  builtinTasks: string[];
  blockOpeners: string[];
  blockModifiers: string[];
  branchKeywords: string[];
  blockCloser: string;
  methodMarker: string;
  lineComment: string;
  brackets: [string, string][];
}

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

export interface QueryResults {
  tokens: Token[];
  diagnostics: Diagnostic[];
  outline: OutlineItem[];
  definition: Location | null;
  complete: Completions;
  hover: Hover | null;
  "missing-modules": string[];
  language: Language;
}
