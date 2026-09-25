// Vela's block structure, read line by line for indentation and folding.
// Blocks open with a keyword at the start of a line (`task`, `when`, …),
// may have branches (`else`, `case`), and close with `fi`. A method opens a
// block with no keyword: among a shape's or a kind's members, a line that is a
// name followed by `(` (see `methodLines`). A plan B's block opens with a `{`
// ending a line (or followed only by `failure e`, naming the failure) and closes
// with a line starting with `}`. The block words come from the compiler (see
// `language()` in ide/analysis), so nothing here is a grammar.

export interface BlockWords {
  blockOpeners: readonly string[];
  /** Words that may come before an opener, as in `priv task`, which the line's keyword skips. */
  blockModifiers: readonly string[];
  branchKeywords: readonly string[];
  blockCloser: string;
  /** The word before a method that changes its value, `alters`. */
  methodMarker: string;
}

/**
 * The keyword a line starts with, if it starts with a word at all, after any of
 * `modifiers` (so `priv task` starts with `task`).
 */
export function firstWord(line: string, modifiers: readonly string[] = []): string | null {
  const words = /^\s*([A-Za-z_][A-Za-z0-9_]*)(?:\s+([A-Za-z_][A-Za-z0-9_]*))?/.exec(line);
  const first = words?.[1] ?? null;
  const second = words?.[2];
  return first !== null && second !== undefined && modifiers.includes(first) ? second : first;
}

/** The width of a line's leading whitespace, with tabs to the next stop. */
export function indentWidth(line: string, tabSize: number): number {
  let width = 0;
  for (const char of line) {
    if (char === " ") width++;
    else if (char === "\t") width += tabSize - (width % tabSize);
    else break;
  }
  return width;
}

const isBlank = (line: string) => line.trim() === "";

/** A line's code: its comment removed and each string emptied to `""`. */
function code(line: string): string {
  let out = "";
  for (let i = 0; i < line.length; i++) {
    const char = line[i];
    if (char === "#") break;
    if (char === '"') {
      i = afterText(line, i) - 1;
      out += '""';
      continue;
    }
    out += char;
  }
  return out;
}

/**
 * The index just after the string whose opening quote is at `start`, skipping
 * the values in it (`"n = {n}"`), which may hold strings of their own.
 */
function afterText(line: string, start: number): number {
  let i = start + 1;
  while (i < line.length && line[i] !== '"') {
    if (line[i] === "\\") i += 2;
    else if (line[i] === "{") i = afterValue(line, i + 1);
    else i++;
  }
  return i + 1;
}

/** The index just after the `}` closing a value in a string, from just after its `{`. */
function afterValue(line: string, start: number): number {
  let depth = 0;
  let i = start;
  while (i < line.length) {
    const char = line[i];
    if (char === '"') i = afterText(line, i);
    else if (char === "}" && depth === 0) return i + 1;
    else {
      if (char === "{") depth++;
      if (char === "}") depth--;
      i++;
    }
  }
  return i;
}

/**
 * Whether a line's code ends with `{`, or with `{ failure e` naming the failure,
 * opening a plan B's block.
 */
export function opensBrace(line: string): boolean {
  return /\{\s*(?:failure\s+[A-Za-z_][A-Za-z0-9_]*\s*)?$/.test(code(line));
}

/** Whether a line starts with `}`, closing a plan B's block. A line such as `} + b {` also opens one. */
export function closesBrace(line: string): boolean {
  return /^\s*\}/.test(line);
}

/** The words that open an item: a task, a shape, or a kind. */
const ITEMS = ["task", "shape", "kind"];

/**
 * The block-opening word a line has, if any: its first word, when that opens a
 * block, or else a `match` or a `when` used as a value further along it
 * (`bind area = match s`, `show(when ok`), which opens a block just the same.
 * A `when` right after an `else` continues that `when` and opens nothing, since
 * one `fi` closes the whole chain.
 */
export function opener(line: string, words: BlockWords): string | null {
  const first = firstWord(line, words.blockModifiers);
  if (first !== null && words.blockOpeners.includes(first)) return first;
  const found = code(line).match(/[A-Za-z_][A-Za-z0-9_]*/g) ?? [];
  for (let i = 1; i < found.length; i++) {
    const word = found[i];
    if (word !== "match" && word !== "when") continue;
    if (word === "when" && found[i - 1] === "else") continue;
    return word;
  }
  return null;
}

/**
 * The indices of the `lines` (top to bottom) that open a method: inside a shape
 * or a kind, and not inside one of its methods, a line whose code is a name
 * followed by `(` (or by `<`, for one naming types), after `priv` and the
 * method marker. Inside a kind a variant's values are written as a method's
 * parameters are, so, as the parser reads them, a line with no method above
 * it, without the marker, that ends after parentheses holding named values
 * (`Circle(radius: float)`) is a variant.
 */
export function methodLines(lines: readonly string[], words: BlockWords): Set<number> {
  const methods = new Set<number>();
  const blocks: string[] = [];
  let methodsAbove = false;
  const header = new RegExp(`^(?:priv\\s+)?(${words.methodMarker}\\s+)?[A-Za-z_][A-Za-z0-9_]*\\s*([(<])`);
  lines.forEach((line, index) => {
    const word = firstWord(line, words.blockModifiers);
    if (word !== null && ITEMS.includes(word) && indentWidth(line, 1) === 0) {
      blocks.length = 0;
      blocks.push(word);
      methodsAbove = false;
      return;
    }
    if (word === words.blockCloser) {
      blocks.pop();
      return;
    }
    const opens = opener(line, words);
    if (opens !== null) {
      blocks.push(opens);
      return;
    }
    const owner = blocks.length === 1 ? blocks[0] : undefined;
    if (owner !== "shape" && owner !== "kind") return;
    const text = code(line).trim();
    const found = header.exec(text);
    if (found === null) return;
    if (owner === "kind" && !methodsAbove && found[1] === undefined && found[2] === "(" && isPayload(text, found[0].length - 1)) return;
    methods.add(index);
    blocks.push("method");
    methodsAbove = true;
  });
  return methods;
}

/**
 * Whether the parentheses opening at `open` in a kind's member line `text`
 * hold a variant's values: named ones (`name: type`), with nothing after them.
 */
function isPayload(text: string, open: number): boolean {
  let depth = 0;
  let named = false;
  for (let i = open; i < text.length; i++) {
    const char = text[i];
    if (char === "(") depth++;
    else if (char === ")") {
      depth--;
      if (depth === 0) return named && text.slice(i + 1).trim() === "";
    } else if (char === ":" && depth === 1) named = true;
  }
  return false;
}

/**
 * The indentation for a line starting with `current`, given the lines above
 * it, nearest first:
 *
 * - `fi`, `else`, and `case` line up with the keyword that opened their block
 *   (`case` with its `match`, as cases are written at the match's level);
 * - `}` lines up with the line whose `{` it closes;
 * - after a block opener, a branch, or a line ending with `{`, one level
 *   deeper, except after `match`, whose cases come next at its own level;
 * - otherwise, the same as the line above.
 */
export function indentation(current: string, above: Iterable<string>, words: BlockWords, unit: number, tabSize: number): number {
  const lines = [...above];
  const methods = methodLines([...lines].reverse(), words);
  // Whether the line `index` places above opens a block with a word (its first,
  // or a `match` or `when` used as a value), or is a method's.
  const opens = (index: number, line: string) => opener(line, words) !== null || methods.has(lines.length - 1 - index);
  if (closesBrace(current)) {
    let depth = 0;
    // Going up, a line's `{` comes before its own `}`.
    for (const line of lines) {
      if (opensBrace(line)) {
        if (depth === 0) return indentWidth(line, tabSize);
        depth--;
      }
      if (closesBrace(line)) depth++;
    }
    return 0;
  }

  const word = firstWord(current, words.blockModifiers);
  const closesOrBranches = word !== null && (word === words.blockCloser || words.branchKeywords.includes(word));

  if (closesOrBranches) {
    let depth = 0;
    for (const [index, line] of lines.entries()) {
      const w = firstWord(line, words.blockModifiers);
      if (w === null) continue;
      if (w === words.blockCloser) {
        depth++;
      } else if (opens(index, line)) {
        if (depth === 0) return indentWidth(line, tabSize);
        depth--;
      } else if (depth === 0 && w === word && word !== words.blockCloser) {
        // Another branch of the same block, such as the case before.
        return indentWidth(line, tabSize);
      }
    }
    return 0;
  }

  for (const [index, line] of lines.entries()) {
    if (isBlank(line)) continue;
    const base = indentWidth(line, tabSize);
    if (opensBrace(line)) return base + unit;
    const w = firstWord(line, words.blockModifiers);
    // A match's cases come next at its own level, wherever the `match` is
    // written: on its own, or as the value of a `bind` or an argument.
    if (opener(line, words) === "match") return base;
    if (opens(index, line) || (w !== null && words.branchKeywords.includes(w))) return base + unit;
    return base;
  }
  return 0;
}

/**
 * The line of the `fi` closing the block opened on line `open` (0-based), or
 * of the `}` closing a plan B opened there; null if that line opens no block
 * or the block is unclosed.
 */
export function closingLine(lines: readonly string[] | ((index: number) => string), count: number, open: number, words: BlockWords): number | null {
  const line = (i: number) => (typeof lines === "function" ? lines(i) : (lines[i] ?? ""));
  if (opensBrace(line(open))) {
    let depth = 0;
    for (let i = open + 1; i < count; i++) {
      if (closesBrace(line(i))) {
        if (depth === 0) return i;
        depth--;
      }
      if (opensBrace(line(i))) depth++;
    }
    return null;
  }
  const opens = opener(line(open), words);
  // Methods are only found reading from the top, so the whole file is read
  // once some line could be one.
  const methods = opens !== null && !["shape", "kind"].includes(opens) ? null : methodLines(Array.from({ length: count }, (_, i) => line(i)), words);
  if (opens === null && !methods?.has(open)) return null;
  let depth = 0;
  for (let i = open + 1; i < count; i++) {
    const w = firstWord(line(i), words.blockModifiers);
    if (w === null) continue;
    if ((methods?.has(i) ?? false) || opener(line(i), words) !== null) {
      depth++;
    } else if (w === words.blockCloser) {
      if (depth === 0) return i;
      depth--;
    }
  }
  return null;
}
