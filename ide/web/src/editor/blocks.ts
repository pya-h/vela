export interface BlockWords {
  blockOpeners: readonly string[];
  /** Prefixes allowed before an opener, e.g. `priv` in `priv task`. */
  blockModifiers: readonly string[];
  branchKeywords: readonly string[];
  blockCloser: string;
  methodMarker: string;
}

export function firstWord(line: string, modifiers: readonly string[] = []): string | null {
  const words = /^\s*([A-Za-z_][A-Za-z0-9_]*)(?:\s+([A-Za-z_][A-Za-z0-9_]*))?/.exec(line);
  const first = words?.[1] ?? null;
  const second = words?.[2];
  return first !== null && second !== undefined && modifiers.includes(first) ? second : first;
}

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

/// Strips comments and empties string/char literals so braces and keywords inside them don't match.
function code(line: string): string {
  let out = "";
  for (let i = 0; i < line.length; i++) {
    const char = line[i];
    if (char === ";") break;
    if (char === '"') {
      i = afterText(line, i) - 1;
      out += '""';
      continue;
    }
    if (char === "'") {
      i = afterChar(line, i) - 1;
      out += "''";
      continue;
    }
    out += char;
  }
  return out;
}

function afterChar(line: string, start: number): number {
  let i = start + 1;
  while (i < line.length && line[i] !== "'") {
    i += line[i] === "\\" ? 2 : 1;
  }
  return i + 1;
}

function afterText(line: string, start: number): number {
  let i = start + 1;
  while (i < line.length && line[i] !== '"') {
    if (line[i] === "\\") i += 2;
    else if (line[i] === "{") i = afterValue(line, i + 1);
    else i++;
  }
  return i + 1;
}

function afterValue(line: string, start: number): number {
  let depth = 0;
  let i = start;
  while (i < line.length) {
    const char = line[i];
    if (char === '"') i = afterText(line, i);
    else if (char === "'") i = afterChar(line, i);
    else if (char === "}" && depth === 0) return i + 1;
    else {
      if (char === "{") depth++;
      if (char === "}") depth--;
      i++;
    }
  }
  return i;
}

export function opensBrace(line: string): boolean {
  return /\{\s*(?:failure\s+[A-Za-z_][A-Za-z0-9_]*\s*)?$/.test(code(line));
}

export function closesBrace(line: string): boolean {
  return /^\s*\}/.test(line);
}

const ITEMS = ["task", "shape", "kind"];

const VALUE_OPENERS = ["match", "when", "loop", "each", "endless"];

/**
 * Block keyword opened and left unclosed on `line`, or null.
 * `else when` continues an existing chain so only one `fi` closes it.
 */
export function opener(line: string, words: BlockWords): string | null {
  const found: string[] = code(line).match(/[A-Za-z_][A-Za-z0-9_]*/g) ?? [];
  const word = (i: number) => found[i] ?? "";
  const opens = (i: number) => words.blockOpeners.includes(word(i)) && !(word(i) === "when" && word(i - 1) === "else");
  const first = firstWord(line, words.blockModifiers);
  let at = -1;
  if (first !== null && words.blockOpeners.includes(first)) {
    at = found.indexOf(first);
  } else if (/^\s*[A-Za-z_][A-Za-z0-9_]*\s*=>\s*[A-Za-z_]/.test(code(line)) && opens(1)) {
    at = 1;
  } else {
    for (let i = 1; i < found.length; i++) {
      if (VALUE_OPENERS.includes(word(i)) && opens(i)) {
        at = i;
        break;
      }
    }
  }
  if (at < 0) return null;
  let open = 0;
  let closed = 0;
  for (let i = at; i < found.length; i++) {
    if (word(i) === words.blockCloser) closed++;
    else if (opens(i)) open++;
  }
  return closed >= open ? null : word(at);
}

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
    // Before any method in a `kind`, a signature ending right after `(name: type, …)` is a variant payload.
    if (owner === "kind" && !methodsAbove && found[1] === undefined && found[2] === "(" && isPayload(text, found[0].length - 1)) return;
    methods.add(index);
    blocks.push("method");
    methodsAbove = true;
  });
  return methods;
}

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

export function indentation(current: string, above: Iterable<string>, words: BlockWords, unit: number, tabSize: number): number {
  const lines = [...above];
  const methods = methodLines([...lines].reverse(), words);
  const opens = (index: number, line: string) => opener(line, words) !== null || methods.has(lines.length - 1 - index);
  if (closesBrace(current)) {
    let depth = 0;
    // Scanning upward, closing `}` precedes opening `{`.
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
    // `case` sits at the same indent as `match`.
    if (opener(line, words) === "match") return base;
    if (opens(index, line) || (w !== null && words.branchKeywords.includes(w))) return base + unit;
    return base;
  }
  return 0;
}

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
  // Methods can only be told from variants by scanning from the top of the file.
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
