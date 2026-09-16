// Vela's block structure, read line by line for indentation and folding.
// Blocks open with a keyword at the start of a line (`task`, `when`, …),
// may have branches (`else`, `case`), and close with `fi`. A plan B's block
// opens with a `{` ending a line and closes with a line starting with `}`. The words come from
// the compiler (see `language()` in ide/analysis), so nothing here is a grammar.

export interface BlockWords {
  blockOpeners: readonly string[];
  /** Words that may come before an opener, as in `priv task`, which the line's keyword skips. */
  blockModifiers: readonly string[];
  branchKeywords: readonly string[];
  blockCloser: string;
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
      for (i++; i < line.length && line[i] !== '"'; i++) {
        if (line[i] === "\\") i++;
      }
      out += '""';
      continue;
    }
    out += char;
  }
  return out;
}

/** Whether a line's code ends with `{`, opening a plan B's block. */
export function opensBrace(line: string): boolean {
  return /\{\s*$/.test(code(line));
}

/** Whether a line starts with `}`, closing a plan B's block. A line such as `} + b {` also opens one. */
export function closesBrace(line: string): boolean {
  return /^\s*\}/.test(line);
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
  if (closesBrace(current)) {
    let depth = 0;
    // Going up, a line's `{` comes before its own `}`.
    for (const line of above) {
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
    for (const line of above) {
      const w = firstWord(line, words.blockModifiers);
      if (w === null) continue;
      if (w === words.blockCloser) {
        depth++;
      } else if (words.blockOpeners.includes(w)) {
        if (depth === 0) return indentWidth(line, tabSize);
        depth--;
      } else if (depth === 0 && w === word && word !== words.blockCloser) {
        // Another branch of the same block, such as the case before.
        return indentWidth(line, tabSize);
      }
    }
    return 0;
  }

  for (const line of above) {
    if (isBlank(line)) continue;
    const base = indentWidth(line, tabSize);
    if (opensBrace(line)) return base + unit;
    const w = firstWord(line, words.blockModifiers);
    if (w === "match") return base;
    if (w !== null && (words.blockOpeners.includes(w) || words.branchKeywords.includes(w))) return base + unit;
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
  const opener = firstWord(line(open), words.blockModifiers);
  if (opener === null || !words.blockOpeners.includes(opener)) return null;
  let depth = 0;
  for (let i = open + 1; i < count; i++) {
    const w = firstWord(line(i), words.blockModifiers);
    if (w === null) continue;
    if (words.blockOpeners.includes(w)) {
      depth++;
    } else if (w === words.blockCloser) {
      if (depth === 0) return i;
      depth--;
    }
  }
  return null;
}
