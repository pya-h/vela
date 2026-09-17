// Vela language support for CodeMirror, powered by the compiler running in
// WebAssembly (see ../analysis): semantic highlighting, diagnostics,
// completion with snippets, hover, go to definition, indentation, folding,
// comments, and bracket closing. A file's modules come from `ModuleSources`,
// and each document says its path with the `documentPath` facet.

import { type Completion, type CompletionContext, type CompletionResult, insertCompletionText, snippet } from "@codemirror/autocomplete";
import { foldService, getIndentation, indentService, indentString, indentUnit } from "@codemirror/language";
import { type Diagnostic, forceLinting, linter } from "@codemirror/lint";
import { EditorState, type Extension, Facet, RangeSetBuilder, StateEffect, type Text } from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  type Tooltip,
  ViewPlugin,
  type ViewUpdate,
  hoverTooltip,
  keymap,
} from "@codemirror/view";

import type { VelaAnalysis } from "../analysis/bridge";
import { ModuleSources } from "../analysis/modules";
import type { CompletionItem, ProgramContext, Diagnostic as VelaDiagnostic, Language, OutlineItem, Token, TokenClass } from "../analysis/protocol";
import { closingLine, indentation } from "./blocks";

export interface FileAnalysis {
  tokens: Token[];
  diagnostics: VelaDiagnostic[];
  outline: OutlineItem[];
  /** The modules the file's program includes that weren't known yet. */
  missing: string[];
}

const EMPTY: FileAnalysis = { tokens: [], diagnostics: [], outline: [], missing: [] };

/** The workspace path of the file an editor state shows; empty for text without a file. */
export const documentPath = Facet.define<string, string>({ combine: (paths) => paths[0] ?? "" });

/** Redoes an editor's analysis, when the modules its file uses changed. */
const reanalyze = StateEffect.define<null>();

/** Whether `update` asks for the analysis to be redone. */
function reanalyzes(update: ViewUpdate): boolean {
  return update.transactions.some((tr) => tr.effects.some((effect) => effect.is(reanalyze)));
}

/** Redoes the analysis shown in `view`: highlighting and diagnostics. */
export function refreshAnalysis(view: EditorView): void {
  view.dispatch({ effects: reanalyze.of(null) });
  forceLinting(view);
}

/**
 * Analyzes documents, once per version and per version of the modules they
 * use: CodeMirror documents are immutable.
 */
export class Analyzer {
  private readonly cache = new WeakMap<Text, { path: string; modules: number; result: FileAnalysis }>();
  /** The text of the files Vela files include. */
  readonly modules = new ModuleSources();
  /** Shows a definition in another file; the workbench sets it. */
  openDefinition: ((path: string, from: number, to: number) => void) | null = null;

  constructor(
    readonly analysis: VelaAnalysis,
    readonly language: Language,
  ) {}

  /** The analysis of `doc`, the text of the file at `path`. */
  file(doc: Text, path = ""): FileAnalysis {
    const cached = this.cache.get(doc);
    if (cached && cached.path === path && cached.modules === this.modules.version) return cached.result;
    const modules = this.modules.version;
    let result: FileAnalysis;
    try {
      result = this.analysis.file(doc.toString(), this.context(path));
    } catch (error) {
      // A crash restarts the module (see the bridge); show nothing for this version.
      console.error(error);
      result = EMPTY;
    }
    this.cache.set(doc, { path, modules, result });
    // Fetched modules change the version, which redoes this analysis.
    if (result.missing.length > 0) this.modules.request(result.missing);
    return result;
  }

  /** The program around the file at `path`, for a query. */
  context(path: string): ProgramContext {
    return { path, modules: this.modules.sources() };
  }

  /** Runs a query that may crash the module, treating a crash as no answer. */
  safely<T>(query: () => T, fallback: T): T {
    try {
      return query();
    } catch (error) {
      console.error(error);
      return fallback;
    }
  }
}

// Highlighting ------------------------------------------------------------------

const marks = new Map<TokenClass, Decoration>();
function mark(tokenClass: TokenClass): Decoration {
  let decoration = marks.get(tokenClass);
  if (!decoration) {
    decoration = Decoration.mark({ class: `tok-${tokenClass}` });
    marks.set(tokenClass, decoration);
  }
  return decoration;
}

function highlighting(analyzer: Analyzer): Extension {
  const build = (view: EditorView): DecorationSet => {
    const builder = new RangeSetBuilder<Decoration>();
    const tokens = analyzer.file(view.state.doc, view.state.facet(documentPath)).tokens;
    for (const { from, to } of view.visibleRanges) {
      // Tokens are sorted and don't overlap: skip to the first visible one.
      let low = 0;
      let high = tokens.length;
      while (low < high) {
        const middle = (low + high) >> 1;
        if ((tokens[middle]?.to ?? 0) <= from) low = middle + 1;
        else high = middle;
      }
      for (let i = low; i < tokens.length; i++) {
        const token = tokens[i];
        if (!token || token.from >= to) break;
        builder.add(token.from, token.to, mark(token.class));
      }
    }
    return builder.finish();
  };
  return ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(view: EditorView) {
        this.decorations = build(view);
      }
      update(update: ViewUpdate) {
        if (update.docChanged || update.viewportChanged || reanalyzes(update)) {
          this.decorations = build(update.view);
        }
      }
    },
    { decorations: (plugin) => plugin.decorations },
  );
}

/** `code` as elements highlighted like the editor, for hovers and lists. */
export function highlightedCode(analyzer: Analyzer, code: string): DocumentFragment {
  const fragment = document.createDocumentFragment();
  const tokens = analyzer.safely(() => analyzer.analysis.file(code).tokens, []);
  let at = 0;
  for (const token of tokens) {
    if (token.from > at) fragment.append(code.slice(at, token.from));
    const span = document.createElement("span");
    span.className = `tok-${token.class}`;
    span.textContent = code.slice(token.from, token.to);
    fragment.append(span);
    at = token.to;
  }
  if (at < code.length) fragment.append(code.slice(at));
  return fragment;
}

// Diagnostics -------------------------------------------------------------------

function diagnostics(analyzer: Analyzer): Extension {
  return linter(
    (view): Diagnostic[] => {
      const length = view.state.doc.length;
      return analyzer.file(view.state.doc, view.state.facet(documentPath)).diagnostics.map((d) => ({
        from: Math.min(d.from, length),
        to: Math.min(Math.max(d.to, d.from), length),
        severity: "error",
        source: "vela",
        message: d.message,
      }));
    },
    // A lint that ran before the modules arrived is redone when they do.
    { delay: 150, needsRefresh: reanalyzes },
  );
}

// Completion --------------------------------------------------------------------

const WORD_BEFORE = /[A-Za-z0-9_]*$/;

/** Re-indents the line at `pos` as the indentation rules say. */
function reindentLine(view: EditorView, pos: number): void {
  const line = view.state.doc.lineAt(pos);
  const wanted = getIndentation(view.state, line.from);
  if (wanted === null) return;
  const current = /^\s*/.exec(line.text)?.[0] ?? "";
  const indent = indentString(view.state, wanted);
  if (indent !== current) {
    view.dispatch({ changes: { from: line.from, to: line.from + current.length, insert: indent }, userEvent: "input.complete" });
  }
}

/**
 * Converts the analysis's completions to CodeMirror's. `dedented` says the
 * line was re-indented for a `fi`, `else`, or `case` being typed; a completion
 * that turns it into another word re-indents it back.
 */
export function completionResult(items: CompletionItem[], from: number, pos: number, doc: Text, dedented = false): CompletionResult | null {
  if (items.length === 0) return null;
  // CodeMirror filters by the text from `from` to the cursor, which must be
  // the word being typed. Some items replace more (a pointer field replaces
  // the `.` too, to insert `^.x`), so their `apply` covers the analysis range.
  const line = doc.lineAt(pos);
  const wordFrom = pos - (WORD_BEFORE.exec(line.text.slice(0, pos - line.from))?.[0].length ?? 0);
  const start = Math.min(from, wordFrom);
  const options: Completion[] = items.map((item) => {
    const option: Completion = { label: item.label, type: item.kind, boost: item.boost };
    if (item.detail) option.detail = item.detail;
    let insert: ((view: EditorView, completion: Completion, to: number) => void) | null = null;
    if (item.snippet !== undefined) {
      const expand = snippet(item.snippet);
      insert = (view, completion, to) => expand(view, completion, start, to);
    } else if (item.apply !== undefined || from < wordFrom || dedented) {
      const text = item.apply ?? item.label;
      insert = (view, _completion, to) => view.dispatch({ ...insertCompletionText(view.state, text, start, to), userEvent: "input.complete" });
    }
    if (insert) {
      const run = insert;
      option.apply = (view, completion, _from, to) => {
        run(view, completion, to);
        if (dedented) reindentLine(view, view.state.selection.main.head);
      };
    }
    return option;
  });
  return { from: wordFrom, options, validFor: /^[A-Za-z0-9_]*$/ };
}

function completion(analyzer: Analyzer) {
  const { blockCloser, branchKeywords } = analyzer.language;
  const dedentWord = new RegExp(`^\\s*(?:${[blockCloser, ...branchKeywords].join("|")})$`);
  return (context: CompletionContext): CompletionResult | null => {
    const before = context.state.sliceDoc(Math.max(0, context.pos - 1), context.pos);
    // While typing, open only on a word or right after `.`; Ctrl+Space opens anywhere.
    if (!context.explicit && !context.matchBefore(/[A-Za-z_][A-Za-z0-9_]*$/) && before !== ".") return null;
    const source = context.state.doc.toString();
    const program = analyzer.context(context.state.facet(documentPath));
    const result = analyzer.safely(() => analyzer.analysis.complete(source, context.pos, program), null);
    const line = context.state.doc.lineAt(context.pos);
    const dedented = dedentWord.test(line.text.slice(0, context.pos - line.from));
    return result && completionResult(result.items, result.from, context.pos, context.state.doc, dedented);
  };
}

// Hover and definitions -----------------------------------------------------------

function hover(analyzer: Analyzer): Extension {
  return hoverTooltip(
    (view, pos): Tooltip | null => {
      const program = analyzer.context(view.state.facet(documentPath));
      const found = analyzer.safely(() => analyzer.analysis.hover(view.state.doc.toString(), pos, program), null);
      if (!found) return null;
      return {
        pos: found.from,
        end: found.to,
        above: true,
        create: () => {
          const dom = document.createElement("div");
          dom.className = "vs-hover";
          const code = document.createElement("pre");
          code.className = "vs-hover-code";
          code.append(highlightedCode(analyzer, found.code));
          dom.append(code);
          if (found.doc) {
            const doc = document.createElement("p");
            doc.className = "vs-hover-doc";
            doc.textContent = found.doc;
            dom.append(doc);
          }
          return { dom };
        },
      };
    },
    { hoverTime: 350 },
  );
}

/**
 * Selects the declaration of the name at `pos` (or the cursor), opening its
 * file when it is in another one.
 */
export function goToDefinition(analyzer: Analyzer, view: EditorView, pos = view.state.selection.main.head): boolean {
  const program = analyzer.context(view.state.facet(documentPath));
  const target = analyzer.safely(() => analyzer.analysis.definition(view.state.doc.toString(), pos, program), null);
  if (!target) return false;
  if (target.path !== undefined) {
    if (!analyzer.openDefinition) return false;
    analyzer.openDefinition(target.path, target.from, target.to);
    return true;
  }
  view.dispatch({
    selection: { anchor: target.from, head: target.to },
    effects: EditorView.scrollIntoView(target.from, { y: "center" }),
    userEvent: "select.definition",
  });
  return true;
}

function definitions(analyzer: Analyzer): Extension {
  return [
    keymap.of([{ key: "F12", run: (view) => goToDefinition(analyzer, view) }]),
    EditorView.domEventHandlers({
      mousedown(event, view) {
        if (event.button !== 0 || !(event.ctrlKey || event.metaKey)) return false;
        const pos = view.posAtCoords({ x: event.clientX, y: event.clientY });
        if (pos === null || !goToDefinition(analyzer, view, pos)) return false;
        event.preventDefault();
        return true;
      },
    }),
  ];
}

// Structure: indentation, folding, comments, brackets ---------------------------------

function structure(language: Language): Extension {
  const unitText = "    ";
  const closeOrBranch = [language.blockCloser, ...language.branchKeywords].join("|");
  // Created once: CodeMirror compares language data by identity.
  const data = [
    {
      commentTokens: { line: language.lineComment },
      closeBrackets: { brackets: language.brackets.map(([open]) => open) },
      // Re-indent a line when it becomes just `fi`, `else`, `case`, or `}`, and
      // again when a word continues past a keyword (`fib`), undoing the dedent.
      indentOnInput: new RegExp(`^\\s*(?:(?:${closeOrBranch})[A-Za-z0-9_]?|\\})$`),
    },
  ];
  return [
    indentUnit.of(unitText),
    EditorState.tabSize.of(4),
    indentService.of((context, pos) => {
      const current = context.lineAt(pos, 1);
      const above = function* () {
        let from = current.from;
        while (from > 0) {
          const line = context.lineAt(from - 1, -1);
          yield line.text;
          from = line.from;
        }
      };
      return indentation(context.textAfterPos(pos), above(), language, unitText.length, context.state.tabSize);
    }),
    foldService.of((state, lineStart) => {
      const doc = state.doc;
      const open = doc.lineAt(lineStart);
      const close = closingLine((i) => doc.line(i + 1).text, doc.lines, open.number - 1, language);
      if (close === null || close <= open.number) return null;
      // Keep the header and the `fi` visible.
      const lastInside = doc.line(close);
      return lastInside.number > open.number ? { from: open.to, to: lastInside.to } : null;
    }),
    EditorState.languageData.of(() => data),
  ];
}

/** Everything the editor needs for a `.vela` file. */
export function vela(analyzer: Analyzer): Extension {
  const autocompleteData = [{ autocomplete: completion(analyzer) }];
  return [
    structure(analyzer.language),
    highlighting(analyzer),
    diagnostics(analyzer),
    hover(analyzer),
    definitions(analyzer),
    // The same source each time: CodeMirror matches running queries to their
    // source by identity, and would drop every result of a new one.
    EditorState.languageData.of(() => autocompleteData),
  ];
}
