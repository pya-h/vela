// Vela support in CodeMirror states, with the real analysis module. These
// run without a browser: commands get a minimal stand-in for the view.

import { CompletionContext, type CompletionResult, type CompletionSource, snippet } from "@codemirror/autocomplete";
import { insertNewlineAndIndent, toggleComment } from "@codemirror/commands";
import { foldable, getIndentation, indentOnInput } from "@codemirror/language";
import { EditorSelection, EditorState, Transaction, type TransactionSpec } from "@codemirror/state";
import type { EditorView } from "@codemirror/view";
import { beforeAll, describe, expect, it } from "vitest";

import { loadAnalysis } from "../analysis/test-module";
import { Analyzer, completionResult, goToDefinition, vela } from "./vela";

let analyzer: Analyzer;

beforeAll(async () => {
  const analysis = await loadAnalysis();
  analyzer = new Analyzer(analysis, analysis.language());
});

/** A state for `source`, with the cursor at `|` (removed). */
function stateFor(source: string): EditorState {
  const at = source.indexOf("|");
  const doc = at < 0 ? source : source.slice(0, at) + source.slice(at + 1);
  return EditorState.create({
    doc,
    selection: EditorSelection.cursor(at < 0 ? doc.length : at),
    extensions: [vela(analyzer), indentOnInput()],
  });
}

/** Just enough of an EditorView for commands and completions. */
function fakeView(initial: EditorState): EditorView & { state: EditorState } {
  const view = {
    state: initial,
    dispatch(...specs: (Transaction | TransactionSpec)[]) {
      const first = specs[0];
      const transaction = first instanceof Transaction ? first : view.state.update(...(specs as TransactionSpec[]));
      view.state = transaction.state;
    },
  };
  return view as unknown as EditorView & { state: EditorState };
}

function type(view: EditorView & { state: EditorState }, text: string): void {
  for (const char of text) {
    const head = view.state.selection.main.head;
    view.dispatch({ changes: { from: head, insert: char }, selection: { anchor: head + 1 }, userEvent: "input.type" });
  }
}

const PROGRAM = `shape Point
    x: int
    y: int
fi

task main()
    region r
    bind p = new Point(Point { x: 1, y: 2 }) at r
    |
fi
`;

function complete(source: string, explicit = false): CompletionResult | null {
  const state = stateFor(source);
  const [source_] = state.languageDataAt<CompletionSource>("autocomplete", state.selection.main.head);
  if (!source_) throw new Error("no completion source");
  return source_(new CompletionContext(state, state.selection.main.head, explicit)) as CompletionResult | null;
}

describe("editing structure", () => {
  it("indents after Enter and dedents fi as it is typed", () => {
    const view = fakeView(stateFor("task main()|"));
    insertNewlineAndIndent(view);
    expect(view.state.doc.toString()).toBe("task main()\n    ");
    type(view, "when true");
    insertNewlineAndIndent(view);
    type(view, "show(1)");
    insertNewlineAndIndent(view);
    expect(view.state.doc.line(4).text).toBe("        ");
    type(view, "fi");
    expect(view.state.doc.line(4).text).toBe("    fi");
    insertNewlineAndIndent(view);
    type(view, "fi");
    expect(view.state.doc.toString()).toBe("task main()\n    when true\n        show(1)\n    fi\nfi");
  });

  it("undoes the dedent when fi turns out to start a longer word", () => {
    const view = fakeView(stateFor("task main()\n    when true\n        |\n    fi\nfi"));
    type(view, "fi");
    expect(view.state.doc.line(3).text).toBe("    fi");
    type(view, "b(1)");
    expect(view.state.doc.line(3).text).toBe("        fib(1)");
  });

  it("gives case lines the match's indentation", () => {
    const state = stateFor("task t(k: K)\n    match k\n    case A\n        show(1)\n        case B\nfi");
    expect(getIndentation(state, state.doc.line(5).from)).toBe(4);
    expect(getIndentation(state, state.doc.line(4).from)).toBe(8);
  });

  it("folds blocks from their header to the line before fi", () => {
    const state = stateFor("task main()\n    when true\n        show(1)\n    fi\nfi\n");
    const line = (n: number) => state.doc.line(n);
    expect(foldable(state, line(1).from, line(1).to)).toEqual({ from: line(1).to, to: line(4).to });
    expect(foldable(state, line(2).from, line(2).to)).toEqual({ from: line(2).to, to: line(3).to });
    expect(foldable(state, line(3).from, line(3).to)).toBeNull();
  });

  it("toggles ; comments", () => {
    const view = fakeView(stateFor("task main()\n    show(1)|\nfi"));
    toggleComment(view);
    expect(view.state.doc.line(2).text).toBe("    ; show(1)");
    toggleComment(view);
    expect(view.state.doc.line(2).text).toBe("    show(1)");
  });

  it("closes brackets and quotes as the language table lists", () => {
    const state = stateFor("|");
    const [brackets] = state.languageDataAt<{ brackets: string[] }>("closeBrackets", 0);
    expect(brackets?.brackets).toEqual(["(", "{", "[", '"', "'"]);
  });
});

describe("analysis", () => {
  it("analyzes each document version once", () => {
    const state = stateFor("task main()\n    show(1)\nfi\n");
    const first = analyzer.file(state.doc);
    expect(analyzer.file(state.doc)).toBe(first);
    expect(first.tokens.some((t) => t.class === "builtin-fn")).toBe(true);
    const changed = state.update({ changes: { from: 0, insert: "; hi\n" } }).state;
    expect(analyzer.file(changed.doc)).not.toBe(first);
  });

  it("goes to definitions", () => {
    const source = PROGRAM.replace("|", "show(p^.x)");
    const view = fakeView(stateFor(source));
    const use = source.indexOf("p^") - 0;
    expect(goToDefinition(analyzer, view, use)).toBe(true);
    const selected = view.state.selection.main;
    expect(view.state.sliceDoc(selected.from, selected.to)).toBe("p");
    expect(selected.from).toBe(source.indexOf("p ="));
    expect(goToDefinition(analyzer, view, source.indexOf("show("))).toBe(false);
  });
});

describe("completion", () => {
  it("keeps the same completion source across edits", () => {
    // CodeMirror drops results from a source it doesn't recognize as the same.
    const state = stateFor(PROGRAM);
    const next = state.update({ changes: { from: 0, insert: "; x\n" } }).state;
    expect(next.languageDataAt("autocomplete", 5)[0]).toBe(state.languageDataAt("autocomplete", 5)[0]);
  });

  it("opens while typing a word, after a dot, or on request", () => {
    const words = complete(PROGRAM.replace("|", "sh|"));
    expect(words?.options.map((o) => o.label)).toContain("show");
    expect(words?.from).toBe(PROGRAM.indexOf("|"));
    expect(complete(PROGRAM)).toBeNull();
    expect(complete(PROGRAM, true)?.options.map((o) => o.label)).toContain("bind");
    // A pointer's fields, its address as text, its copy, and its type.
    expect(complete(PROGRAM.replace("|", "show(p.|)"))?.options.map((o) => o.label)).toEqual(["x", "y", "str", "copy", "type"]);
  });

  it("inserts snippets with their fields and indentation", () => {
    const result = complete(PROGRAM.replace("|", "wh|"), true);
    const option = result?.options.find((o) => o.label === "when" && o.type === "snippet");
    expect(option).toBeDefined();
    const view = fakeView(stateFor(PROGRAM.replace("|", "wh|")));
    const head = view.state.selection.main.head;
    if (typeof option?.apply !== "function") throw new Error("snippet without apply");
    option.apply(view, option, result!.from, head);
    expect(view.state.doc.toString()).toContain("    when condition\n        \n    fi\n");
    // The first field, `condition`, is selected.
    const selected = view.state.selection.main;
    expect(view.state.sliceDoc(selected.from, selected.to)).toBe("condition");
  });

  it("replaces the dot for fields reached through a pointer", () => {
    const source = PROGRAM.replace("|", "show(p.y|)");
    const result = complete(source);
    const x = result?.options.find((o) => o.label === "x");
    expect(result?.from).toBe(source.indexOf("y|"));
    const view = fakeView(stateFor(source));
    if (typeof x?.apply !== "function") throw new Error("no apply for a pointer field");
    x.apply(view, x, result!.from, view.state.selection.main.head);
    expect(view.state.doc.toString()).toContain("show(p^.x)");
    expect(view.state.selection.main.head).toBe(source.indexOf("p.") + "p^.x".length);
  });

  it("re-indents a line dedented for fi when a completion makes it another word", () => {
    const source = "task fib(n: int) -> int\n    <- n\nfi\ntask main()\n    when true\n    fi|\n    fi\nfi\n";
    const result = complete(source);
    const fib = result?.options.find((o) => o.label === "fib");
    const view = fakeView(stateFor(source));
    if (typeof fib?.apply !== "function") throw new Error("no apply for fib");
    fib.apply(view, fib, result!.from, view.state.selection.main.head);
    expect(view.state.doc.line(6).text).toBe("        fib(n)");
    // The snippet's field is still selected after re-indenting.
    const selected = view.state.selection.main;
    expect(view.state.sliceDoc(selected.from, selected.to)).toBe("n");
  });

  it("maps analysis items without extra ranges to plain labels", () => {
    const doc = EditorState.create({ doc: "ab" }).doc;
    const result = completionResult([{ label: "abc", kind: "variable", detail: "int", boost: 3 }], 0, 2, doc);
    expect(result?.options).toEqual([{ label: "abc", type: "variable", boost: 3, detail: "int" }]);
    expect(completionResult([], 0, 2, doc)).toBeNull();
    expect(typeof snippet).toBe("function");
  });
});
