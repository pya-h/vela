import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { beforeAll, describe, expect, it } from "vitest";

import { AnalysisError, VelaAnalysis, wellFormed } from "./bridge";
import { examplesFolder as examples, wasmBytes } from "./test-module";

let analysis: VelaAnalysis;

beforeAll(async () => {
  analysis = await VelaAnalysis.load(wasmBytes());
});

describe("VelaAnalysis", () => {
  it("highlights with offsets that are JavaScript string indices", () => {
    // 'é' is one UTF-16 unit and '𝄞' two; both are several UTF-8 bytes.
    const source = 'task main()\n    show("é𝄞 ok")\n    bind total = 1\n    show(total)\nfi\n';
    const { tokens, diagnostics } = analysis.file(source);
    expect(diagnostics).toEqual([]);
    const variables = tokens.filter((t) => t.class === "variable").map((t) => source.slice(t.from, t.to));
    expect(variables).toEqual(["total", "total"]);
    expect(tokens.find((t) => t.class === "string")).toMatchObject({ from: source.indexOf('"é') });
  });

  it("analyzes text holding a lone surrogate, with offsets unchanged", () => {
    const source = 'task main()\n    show("a\ud800")\n    show(missing)\nfi\n';
    const { tokens, diagnostics } = analysis.file(source);
    expect(diagnostics).toMatchObject([{ from: source.indexOf("missing"), message: "unknown variable 'missing'" }]);
    expect(tokens.find((t) => t.class === "string")).toMatchObject({ from: source.indexOf('"a'), to: source.indexOf('")') + 1 });
    expect(wellFormed("x\udc00y\ud83d\ude00\ud83d")).toBe("x\ufffdy\ud83d\ude00\ufffd");
  });

  it("covers every non-space character of every example with a token", () => {
    for (const name of readdirSync(examples).filter((f) => f.endsWith(".vela"))) {
      const source = readFileSync(join(examples, name), "utf8");
      // The modules an example includes are next to it, fetched as the editor does.
      const context = { path: name, modules: {} as Record<string, string> };
      for (let missing = analysis.file(source, context).missing; missing.length > 0; missing = analysis.file(source, context).missing) {
        for (const path of missing) context.modules[path] = readFileSync(join(examples, path), "utf8");
      }
      const { tokens, diagnostics, outline } = analysis.file(source, context);
      expect(diagnostics, name).toEqual([]);
      expect(outline.length, name).toBeGreaterThan(0);
      const covered = new Array<boolean>(source.length).fill(false);
      for (const token of tokens) {
        for (let i = token.from; i < token.to; i++) covered[i] = true;
      }
      const uncovered = [...source].flatMap((_, i) => (!covered[i] && !/\s/.test(source[i] ?? "") ? [i] : []));
      expect(uncovered, name).toEqual([]);
    }
  });

  it("analyzes a file with the modules it includes", () => {
    const source = "include shapes/geometry\n\ntask main()\n    show(geometry.area(2))\nfi\n";
    const geometry = "task area(side: int) -> int\n    <- side * side\nfi\n";
    const alone = analysis.file(source, { path: "app/main.vela", modules: {} });
    expect(alone.missing).toEqual(["app/shapes/geometry.vela"]);
    expect(alone.diagnostics[0]?.message).toBe("can't find module 'shapes/geometry': there is no file 'shapes/geometry.vela'");

    const context = { path: "app/main.vela", modules: { "app/shapes/geometry.vela": geometry } };
    const { tokens, diagnostics, missing } = analysis.file(source, context);
    expect([diagnostics, missing]).toEqual([[], []]);
    const area = source.indexOf("area(2)");
    expect(tokens.find((t) => t.from === source.indexOf("geometry.area"))?.class).toBe("module");
    expect(tokens.find((t) => t.from === area)?.class).toBe("task");
    expect(analysis.hover(source, area, context)?.code).toBe("task geometry.area(side: int) -> int");
    expect(analysis.definition(source, area, context)).toEqual({ from: 5, to: 9, path: "app/shapes/geometry.vela" });
    const labels = analysis.complete(source.replace("area(2)", "(2)"), source.indexOf("area"), context).items.map((i) => i.label);
    expect(labels).toEqual(["area"]);
  });

  it("follows aliases, braces, and variants named through their kind", () => {
    const source = "include shapes/geometry => geo {Figure => Shape}\n";
    const bad = analysis.file(source, { path: "app/main.vela", modules: {} });
    expect(bad.diagnostics[0]?.message).toBe("a module included with only its parts ('{…}') has no name in this file, so it can't be given one with '=>'");

    const main = "include shapes/geometry {Figure => Shape, area}\n\ntask main()\n    show(area(Shape.Square(2)))\n    bind n: int = Shape.Square(1)\nfi\n";
    const geometry = "kind Figure\n    Square(side: int)\nfi\n\ntask area(figure: Figure) -> int\n    <- 1\nfi\n";
    const context = { path: "app/main.vela", modules: { "app/shapes/geometry.vela": geometry } };
    const { tokens, diagnostics } = analysis.file(main, context);
    expect(diagnostics.map((d) => d.message)).toEqual(["expected type int, got Shape"]);
    expect(tokens.find((t) => t.from === main.indexOf("=>"))?.class).toBe("operator");
    expect(tokens.find((t) => t.from === main.indexOf("Square(2)"))?.class).toBe("variant");
    expect(analysis.hover(main, main.indexOf("area(Shape"), context)?.code).toBe("task area(figure: Shape) -> int");
    const variants = analysis.complete(main.replace("Square(2)", "(2)"), main.indexOf("Square(2)"), context).items.map((i) => i.label);
    expect(variants).toEqual(["Square"]);
  });

  it("completes an include line with names alone, and cases with the matched kind's variants", () => {
    const geometry = "kind Figure\n    Square(side: int)\n    Dot\nfi\n\ntask area(figure: Figure) -> int\n    <- 1\nfi\n";
    const context = { path: "app/main.vela", modules: { "app/shapes/geometry.vela": geometry } };
    const listed = analysis.complete("include shapes/geometry (\n", "include shapes/geometry (".length, context).items;
    expect(listed.map((i) => [i.label, i.snippet, i.apply])).toEqual([
      ["Figure", undefined, undefined],
      ["Square", undefined, undefined],
      ["Dot", undefined, undefined],
      ["area", undefined, undefined],
    ]);

    const main = "include shapes/geometry\n\nkind Mark\n    Square(label: text)\nfi\n\ntask main()\n    match geometry.Figure.Dot\n    case geometry.Figure.Square side\n        show(side)\n    case Dot\n        show(0)\n    fi\nfi\n";
    expect(analysis.file(main, context).diagnostics).toEqual([]);
    const dot = main.indexOf("Dot\n        show(0)");
    expect(analysis.hover(main, dot, context)?.code).toBe("geometry.Dot");
    expect(analysis.definition(main, dot, context)?.path).toBe("app/shapes/geometry.vela");
  });

  it("reports diagnostics, completions, hovers, and definitions", () => {
    const source = "task main()\n    bind count = 1\n    show(count)\n    show(missing)\nfi\n";
    const [diagnostic] = analysis.file(source).diagnostics;
    expect(diagnostic?.message).toBe("unknown variable 'missing'");
    expect(source.slice(diagnostic?.from, diagnostic?.to)).toBe("missing");

    const use = source.indexOf("count)");
    expect(analysis.hover(source, use)?.code).toBe("bind count: int");
    const definition = analysis.definition(source, use);
    expect(definition && source.slice(definition.from, definition.to)).toBe("count");
    expect(definition?.from).toBe(source.indexOf("count"));

    const line = source.indexOf("    show(missing)") + 4;
    const labels = analysis.complete(source, line).items.map((item) => item.label);
    expect(labels).toEqual(expect.arrayContaining(["bind", "count", "show", "match"]));
  });

  it("completes and describes the changes a slice has", () => {
    const source = "task main()\n    region r\n    hold names = new [text]() at r\n    names.add(\"a\")\n    bind last = names.del()\n    show(last)\n    names.\nfi\n";
    const members = analysis.complete(source, source.indexOf("names.\nfi") + "names.".length).items;
    expect(members.map((item) => [item.label, item.snippet])).toEqual([["len", undefined], ["add", "add(${value})"], ["del", "del(${index})"]]);
    expect(analysis.hover(source, source.indexOf("last ="))?.code).toBe("bind last: text@r");
    expect(analysis.hover(source, source.indexOf("add("))?.code).toBe("add(value) or add(index, value)");
  });

  it("describes scan and the line it reads", () => {
    const source = "task main()\n    region lines\n    bind name = scan(\"Name: \") at lines { \"nobody\" }\n    show(name)\nfi\n";
    expect(analysis.file(source).diagnostics).toEqual([]);
    expect(analysis.hover(source, source.indexOf("name ="))?.code).toBe("bind name: text@lines");
    expect(analysis.hover(source, source.indexOf("scan("))?.code).toBe("task scan(prompt: text) at r -> maybe text@r");
  });

  it("analyzes deeply nested arrays, slices, and parentheses without crashing", () => {
    // The compiler stops at its nesting limit, well inside the module's stack.
    const deep = 20_000;
    const sources = [
      `task main()\n    bind a = ${"[".repeat(deep)}1${"]".repeat(deep)}\n    show(a)\nfi\n`,
      `task main()\n    show(${"(".repeat(deep)}1${")".repeat(deep)})\nfi\n`,
      `task f(a: ${"[".repeat(deep)}int${"; 2]".repeat(deep)})\nfi\n`,
      `task main()\n    bind a: ${"[".repeat(deep)}int${"]".repeat(deep)}@r = 1\nfi\n`,
    ];
    for (const source of sources) {
      expect(analysis.file(source).diagnostics[0]?.message).toContain("nested too deeply");
      expect(analysis.hover(source, source.length - 8)).toBeDefined();
      expect(analysis.complete(source, source.length - 8).items).toBeDefined();
    }
  });

  it("answers several queries in one request", () => {
    const [tokens, hover, language] = analysis.request("task main()\nfi\n", [
      { kind: "tokens" },
      { kind: "hover", offset: 0 },
      { kind: "language" },
    ] as const);
    expect(tokens[0]).toEqual({ from: 0, to: 4, class: "keyword" });
    expect(hover).toBeNull();
    expect(language.blockCloser).toBe("fi");
  });

  it("describes the language from the compiler's tables", () => {
    const language = analysis.language();
    expect(language.keywords).toEqual(expect.arrayContaining(["task", "fi", "hold", "none"]));
    expect(language.blockOpeners).toEqual(["task", "shape", "kind", "when", "loop", "each", "match"]);
    expect(language.blockModifiers).toEqual(["priv"]);
    expect(language.branchKeywords).toEqual(["else", "case"]);
    expect(language.builtinTypes).toEqual(["int", "float", "bool", "text", "unit"]);
    expect(language.lineComment).toBe("#");
  });

  it("rejects malformed requests", () => {
    const bogus = [{ kind: "explode" }] as unknown as [{ kind: "tokens" }];
    expect(() => analysis.request("", bogus)).toThrow(AnalysisError);
    expect(() => analysis.request("", bogus)).toThrow(/invalid request/);
  });

  it("frees its memory after every request", () => {
    const source = readFileSync(join(examples, "todo.vela"), "utf8");
    const memory = (analysis as unknown as { exports: { memory: WebAssembly.Memory } }).exports.memory;
    for (let i = 0; i < 50; i++) analysis.file(source);
    const settled = memory.buffer.byteLength;
    for (let i = 0; i < 500; i++) {
      analysis.file(source);
      analysis.complete(source, source.length - 3);
    }
    expect(memory.buffer.byteLength).toBe(settled);
  });

  it("recovers from a crash inside the module", async () => {
    const crashing = await VelaAnalysis.load(wasmBytes());
    const internals = crashing as unknown as { exports: Record<string, unknown> };
    internals.exports = {
      ...internals.exports,
      vela_handle: () => {
        throw new WebAssembly.RuntimeError("unreachable");
      },
    };
    expect(() => crashing.file("task main()\nfi\n")).toThrow(/crashed: unreachable/);
    // A fresh instance answers the next request.
    expect(crashing.file("task main()\nfi\n").tokens.length).toBe(5);
  });
});
