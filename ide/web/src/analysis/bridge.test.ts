import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import { AnalysisError, VelaAnalysis } from "./bridge";

const web = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const examples = join(web, "..", "..", "examples");

let analysis: VelaAnalysis;

function wasmBytes(): Uint8Array<ArrayBuffer> {
  try {
    return new Uint8Array(readFileSync(join(web, "wasm", "vela_analysis.wasm")));
  } catch {
    throw new Error("wasm/vela_analysis.wasm is missing: run `npm run build:wasm` first");
  }
}

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

  it("covers every non-space character of every example with a token", () => {
    for (const name of readdirSync(examples).filter((f) => f.endsWith(".vela"))) {
      const source = readFileSync(join(examples, name), "utf8");
      const { tokens, diagnostics, outline } = analysis.file(source);
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
