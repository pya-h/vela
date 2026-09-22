import { describe, expect, it } from "vitest";

import { type BlockWords, closingLine, firstWord, indentWidth, indentation, methodLines } from "./blocks";

const WORDS: BlockWords = {
  blockOpeners: ["task", "shape", "kind", "when", "loop", "each", "match"],
  blockModifiers: ["priv"],
  branchKeywords: ["else", "case"],
  blockCloser: "fi",
  methodMarker: "alters",
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
    // A modifier before an opener is skipped; alone, or before anything else, it is the word.
    expect([firstWord("priv task f()", ["priv"]), firstWord("    priv balance: int", ["priv"]), firstWord("priv", ["priv"])]).toEqual(["task", "balance", "priv"]);
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
    // Nor does one in a trailing comment or a string.
    expect(indentOfLast("task main()\n    show(1)  # see {\n")).toBe(4);
    expect(indentOfLast('task main()\n    show("use { # like this")\n')).toBe(4);
    expect(indentOfLast('task main()\n    show("\\" {")\n')).toBe(4);
    expect(indentOfLast('task main()\n    bind n = m {  # "quoted" {\n')).toBe(8);
    expect(indentOfLast("task main()\n    bind n = m {\n        show(1)  # uses {\n        0\n        }")).toBe(4);
    // A line closing one plan B and opening another.
    expect(indentOfLast("task main()\n    bind x = a {\n        0\n    } + b {\n")).toBe(8);
    expect(indentOfLast("task main()\n    bind x = a {\n        0\n    } + b {\n        1\n        }")).toBe(4);
  });

  it("reads a { naming the plan B's error as opening its block", () => {
    expect(indentOfLast('task main()\n    bind n = parse("x") { fail e\n')).toBe(8);
    expect(indentOfLast("task main()\n    bind n = parse(t) { fail _  # ignored\n")).toBe(8);
    expect(indentOfLast("task main()\n    when ok\n        bind n = parse(t) { fail e\n            show(e)\n            }")).toBe(8);
    const lines = ["task main()", "    save(3) { fail problem", "        show(problem)", "    }", "fi"];
    expect(closingLine(lines, lines.length, 1, WORDS)).toBe(3);
    // `fail` with its value on one line is a one-line plan B's value, not a name, and a
    // line ending in `fail e` without `{` opens nothing.
    expect(indentOfLast("task main()\n    bind n = m { fail e }\n")).toBe(4);
    expect(indentOfLast("task main()\n    <- fail e\n")).toBe(4);
    expect(indentOfLast('task main()\n    bind n = m { fail "no" \n')).toBe(4);
    expect(closingLine(["task main()", "    x { fail e }", "fi"], 3, 1, WORDS)).toBeNull();
  });

  it("folds a plan B up to its }", () => {
    const lines = ["task main()", "    bind n = m {", "        bind k = j {", "            0", "        }", "        k", "    }", "fi"];
    expect(closingLine(lines, lines.length, 1, WORDS)).toBe(6);
    expect(closingLine(lines, lines.length, 2, WORDS)).toBe(4);
    expect(closingLine(lines, lines.length, 0, WORDS)).toBe(7);
    const chained = ["task main()", "    bind n = m {", "        bind x = a {", "            0", "        } + b {  # see {", "            1", "        }", "        x", "    }", "fi"];
    expect(closingLine(chained, chained.length, 1, WORDS)).toBe(8);
    expect(closingLine(chained, chained.length, 2, WORDS)).toBe(4);
    expect(closingLine(chained, chained.length, 4, WORDS)).toBe(6);
  });
});

describe("closingLine", () => {
  const source = ["task main()", "    when x", "        show(1)", "    else", "        show(2)", "    fi", "fi", "task other()"];

  it("reads a private item as its block", () => {
    expect(indentOfLast("priv task f()\n")).toBe(4);
    expect(indentOfLast("priv task f()\n    when x\n    fi\nfi")).toBe(0);
    expect(indentOfLast("shape S\n    priv x: int\n")).toBe(4);
    expect(indentOfLast("priv shape S\n    priv x: int\nfi")).toBe(0);
    expect(closingLine(["priv kind K", "    A", "fi", "task t()", "fi"], 5, 0, WORDS)).toBe(2);
  });

  it("finds the fi of a block", () => {
    expect(closingLine(source, source.length, 0, WORDS)).toBe(6);
    expect(closingLine(source, source.length, 1, WORDS)).toBe(5);
  });

  it("is null for lines that open nothing and for unclosed blocks", () => {
    expect(closingLine(source, source.length, 2, WORDS)).toBeNull();
    expect(closingLine(source, source.length, 7, WORDS)).toBeNull();
  });
});

describe("methods", () => {
  const counter = [
    "shape Counter",
    "    count: int",
    "",
    "    total() -> int",
    "        <- .count",
    "    fi",
    "",
    "    priv alters step(by: int)",
    "        when by > 0",
    "            .count = .count + by",
    "        fi",
    "    fi",
    "fi",
    "task main()",
    "    show(1)",
    "fi",
  ];

  it("finds a method's line among a shape's members, and never a call in a body", () => {
    expect([...methodLines(counter, WORDS)]).toEqual([3, 7]);
    expect([...methodLines(["task main()", "    show(1)", "    step(2)", "fi"], WORDS)]).toEqual([]);
  });

  it("reads a kind's member as a variant or a method as the parser does", () => {
    const light = [
      "kind Light",
      "    Red",
      "    Amber(seconds: int)",
      "    shown() -> text",
      "        <- \"\"",
      "    fi",
      "    wait(seconds: int)",
      "    fi",
      "fi",
    ];
    // A variant carries named values; a method after another is one whatever it takes.
    expect([...methodLines(light, WORDS)]).toEqual([3, 6]);
    const first = ["kind K", "    A", "    tell(prefix: text) -> unit", "    fi", "    alters set(to: int)", "    fi", "fi"];
    expect([...methodLines(first, WORDS)]).toEqual([2, 4]);
    // Without anything above it saying otherwise, a kind's first member with
    // named values and nothing after them is a variant, as the parser reads it.
    expect([...methodLines(["kind K", "    tell(prefix: text)", "fi"], WORDS)]).toEqual([]);
  });

  it("indents a method's body and lines its fi up with its header", () => {
    expect(indentOfLast(counter.slice(0, 4).join("\n") + "\n")).toBe(8);
    expect(indentOfLast(counter.slice(0, 8).join("\n") + "\n")).toBe(8);
    expect(indentOfLast(counter.slice(0, 5).join("\n") + "\n        fi")).toBe(4);
    expect(indentOfLast(counter.slice(0, 11).join("\n") + "\n            fi")).toBe(4);
    // The shape's own fi, below its methods, lines up with the shape.
    expect(indentOfLast(counter.slice(0, 12).join("\n") + "\n    fi")).toBe(0);
    // After a method, the next member is at the shape's level.
    expect(indentOfLast(counter.slice(0, 6).join("\n") + "\n")).toBe(4);
  });

  it("folds a method up to its fi, and a shape up to its own", () => {
    expect(closingLine(counter, counter.length, 0, WORDS)).toBe(12);
    expect(closingLine(counter, counter.length, 3, WORDS)).toBe(5);
    expect(closingLine(counter, counter.length, 7, WORDS)).toBe(11);
    expect(closingLine(counter, counter.length, 8, WORDS)).toBe(10);
    expect(closingLine(counter, counter.length, 14, WORDS)).toBeNull();
  });
});
