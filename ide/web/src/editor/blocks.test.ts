import { describe, expect, it } from "vitest";

import { type BlockWords, closingLine, firstWord, indentWidth, indentation } from "./blocks";

const WORDS: BlockWords = {
  blockOpeners: ["task", "shape", "kind", "when", "loop", "each", "match"],
  branchKeywords: ["else", "case"],
  blockCloser: "fi",
};

/** The indentation for the last line of `source`, as columns. */
function indentOfLast(source: string): number {
  const lines = source.split("\n");
  const current = lines.pop() ?? "";
  return indentation(current, lines.reverse(), WORDS, 4, 4);
}

describe("reading lines", () => {
  it("finds the first word and the indentation", () => {
    expect([firstWord("    when x"), firstWord("# when"), firstWord("  "), firstWord("fi")]).toEqual(["when", null, null, "fi"]);
    expect([indentWidth("    x", 4), indentWidth("\tx", 4), indentWidth("  \tx", 4), indentWidth("", 4)]).toEqual([4, 4, 4, 0]);
  });
});

describe("indentation", () => {
  it("goes one level deeper after a block opener", () => {
    expect(indentOfLast("task main()\n")).toBe(4);
    expect(indentOfLast("task main()\n    when x > 1\n")).toBe(8);
    expect(indentOfLast("shape Point\n")).toBe(4);
    expect(indentOfLast("task main()\n    each i in 0..3\n")).toBe(8);
  });

  it("keeps the level of an ordinary line, skipping blank lines", () => {
    expect(indentOfLast("task main()\n    show(1)\n")).toBe(4);
    expect(indentOfLast("task main()\n    show(1)\n\n   \n")).toBe(4);
    expect(indentOfLast("task main()\n    # a comment\n")).toBe(4);
    expect(indentOfLast("")).toBe(0);
  });

  it("lines up cases with their match, and indents case bodies", () => {
    expect(indentOfLast("task t(k: K)\n    match k\n")).toBe(4);
    expect(indentOfLast("task t(k: K)\n    match k\n    case A\n")).toBe(8);
    expect(indentOfLast("task t(k: K)\n    match k\n    case A\n        show(1)\n    case")).toBe(4);
    expect(indentOfLast("task t(k: K)\n    match k\n    case")).toBe(4);
  });

  it("lines up fi and else with the keyword that opened the block", () => {
    expect(indentOfLast("task main()\n    when x\n        show(1)\n        fi")).toBe(4);
    expect(indentOfLast("task main()\n    when x\n        show(1)\n        else")).toBe(4);
    expect(indentOfLast("task main()\n    when x\n        show(1)\n    fi\n    fi")).toBe(0);
    expect(indentOfLast("task t(k: K)\n    match k\n    case A\n        show(1)\n        fi")).toBe(4);
    // Nested blocks are skipped over.
    expect(indentOfLast("task main()\n    loop x\n        when y\n            show(1)\n        fi\n        fi")).toBe(4);
    expect(indentOfLast("fi")).toBe(0);
  });
});

describe("plan B braces", () => {
  it("indents after a line ending with { and lines } up with it", () => {
    expect(indentOfLast("task main()\n    bind n = m {\n")).toBe(8);
    expect(indentOfLast("task main()\n    bind n = m {  # no value\n")).toBe(8);
    expect(indentOfLast("task main()\n    bind n = m {\n        show(1)\n        }")).toBe(4);
    expect(indentOfLast("task main()\n    bind n = m {\n        bind k = j {\n            0\n        }\n        k\n        }")).toBe(4);
    // After the closing line, the block's level again.
    expect(indentOfLast("task main()\n    bind n = m {\n        0\n    }\n")).toBe(4);
    // A brace in a comment or a one-line plan B opens nothing.
    expect(indentOfLast("task main()\n    # a {\n")).toBe(4);
    expect(indentOfLast("task main()\n    bind n = m { 0 }\n")).toBe(4);
  });

  it("folds a plan B up to its }", () => {
    const lines = ["task main()", "    bind n = m {", "        bind k = j {", "            0", "        }", "        k", "    }", "fi"];
    expect(closingLine(lines, lines.length, 1, WORDS)).toBe(6);
    expect(closingLine(lines, lines.length, 2, WORDS)).toBe(4);
    expect(closingLine(lines, lines.length, 0, WORDS)).toBe(7);
  });
});

describe("closingLine", () => {
  const source = ["task main()", "    when x", "        show(1)", "    else", "        show(2)", "    fi", "fi", "task other()"];

  it("finds the fi of a block", () => {
    expect(closingLine(source, source.length, 0, WORDS)).toBe(6);
    expect(closingLine(source, source.length, 1, WORDS)).toBe(5);
  });

  it("is null for lines that open nothing and for unclosed blocks", () => {
    expect(closingLine(source, source.length, 2, WORDS)).toBeNull();
    expect(closingLine(source, source.length, 7, WORDS)).toBeNull();
  });
});
