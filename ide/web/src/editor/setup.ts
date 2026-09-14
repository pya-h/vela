// The editor: CodeMirror's standard editing features, Vela Studio's theme,
// and, for `.vela` files, Vela language support.

import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap } from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { bracketMatching, foldGutter, foldKeymap, indentOnInput } from "@codemirror/language";
import { lintGutter, lintKeymap } from "@codemirror/lint";
import { highlightSelectionMatches, openSearchPanel, search, searchKeymap } from "@codemirror/search";
import { EditorState, type Extension } from "@codemirror/state";
import {
  EditorView,
  crosshairCursor,
  drawSelection,
  dropCursor,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
  rectangularSelection,
} from "@codemirror/view";

import { type Analyzer, vela } from "./vela";

/** Colors come from CSS variables (see styles/editor.css), so one theme serves light and dark. */
const theme = EditorView.theme({
  "&": { color: "var(--editor-fg)", backgroundColor: "var(--editor-bg)", height: "100%" },
  ".cm-scroller": { fontFamily: "var(--font-code)", lineHeight: "1.6" },
  ".cm-content": { caretColor: "var(--accent)", padding: "8px 0" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--accent)", borderLeftWidth: "2px" },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection":
    { backgroundColor: "var(--editor-selection)" },
  ".cm-activeLine": { backgroundColor: "var(--editor-active-line)" },
  ".cm-gutters": { backgroundColor: "var(--editor-bg)", color: "var(--editor-gutter)", border: "none" },
  ".cm-activeLineGutter": { backgroundColor: "transparent", color: "var(--editor-fg)" },
  ".cm-lineNumbers .cm-gutterElement": { padding: "0 12px 0 16px", minWidth: "40px" },
  ".cm-foldGutter .cm-gutterElement": { padding: "0 4px", color: "var(--muted)" },
  ".cm-selectionMatch": { backgroundColor: "var(--editor-match)" },
  "&.cm-focused .cm-matchingBracket": { backgroundColor: "var(--editor-match)", outline: "1px solid var(--border-strong)" },
  ".cm-searchMatch": { backgroundColor: "var(--editor-search)", outline: "1px solid var(--accent-muted)" },
  ".cm-searchMatch.cm-searchMatch-selected": { backgroundColor: "var(--accent-muted)" },
  ".cm-foldPlaceholder": { backgroundColor: "var(--surface-2)", border: "1px solid var(--border)", color: "var(--muted)" },
  ".cm-tooltip": {
    backgroundColor: "var(--surface-1)",
    color: "var(--fg)",
    border: "1px solid var(--border)",
    borderRadius: "8px",
    boxShadow: "var(--shadow)",
    overflow: "hidden",
  },
  ".cm-tooltip-autocomplete > ul": { fontFamily: "var(--font-code)", maxHeight: "18em" },
  ".cm-tooltip-autocomplete > ul > li": { padding: "2px 8px" },
  ".cm-tooltip-autocomplete > ul > li[aria-selected]": { backgroundColor: "var(--accent-muted)", color: "var(--fg)" },
  ".cm-completionDetail": { color: "var(--muted)", fontStyle: "normal", marginLeft: "12px" },
  ".cm-completionMatchedText": { textDecoration: "none", color: "var(--accent)", fontWeight: "600" },
  ".cm-panels": { backgroundColor: "var(--surface-1)", color: "var(--fg)" },
  ".cm-panels.cm-panels-top": { borderBottom: "1px solid var(--border)" },
  ".cm-diagnostic-error": { borderLeftColor: "var(--danger)" },
  ".cm-lintRange-error": { backgroundImage: "none", textDecoration: "underline wavy var(--danger)", textUnderlineOffset: "3px" },
});

export interface EditorOptions {
  /** The nonce allowing CodeMirror's style elements under the page's policy. */
  nonce: string;
  /** Vela language support, for `.vela` files. */
  analyzer: Analyzer | null;
  /** Called after every change to the editor. */
  onUpdate: Extension;
  /** Extra key bindings, which take precedence. */
  keys: Extension;
}

export function editorExtensions(options: EditorOptions): Extension {
  return [
    options.keys,
    EditorView.cspNonce.of(options.nonce),
    lineNumbers(),
    highlightActiveLineGutter(),
    highlightSpecialChars(),
    history(),
    foldGutter({ openText: "⌄", closedText: "›" }),
    drawSelection(),
    dropCursor(),
    EditorState.allowMultipleSelections.of(true),
    indentOnInput(),
    bracketMatching(),
    closeBrackets(),
    autocompletion({ icons: true, activateOnTypingDelay: 60 }),
    rectangularSelection(),
    crosshairCursor(),
    highlightActiveLine(),
    highlightSelectionMatches(),
    search({ top: true }),
    lintGutter(),
    keymap.of([
      ...closeBracketsKeymap,
      ...defaultKeymap,
      ...searchKeymap,
      ...historyKeymap,
      ...foldKeymap,
      ...completionKeymap,
      ...lintKeymap,
      indentWithTab,
      // Find and replace share one panel (Mod-f opens it too).
      { key: "Mod-h", run: openSearchPanel, preventDefault: true },
    ]),
    theme,
    options.analyzer ? vela(options.analyzer) : [],
    options.onUpdate,
  ];
}
