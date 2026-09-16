// Runs Vela's language analysis (the compiler's lexer, parser, and checker,
// compiled to WebAssembly) and exchanges JSON with it. See ide/wasm/src/lib.rs
// for the memory protocol.

import type { Completions, Diagnostic, Hover, Language, Location, OutlineItem, Query, QueryResults, Token } from "./protocol";

interface Exports {
  memory: WebAssembly.Memory;
  vela_alloc(len: number): number;
  vela_handle(ptr: number, len: number): number;
  vela_free_response(ptr: number): void;
}

/** A request the module rejected, or a crash inside the module. */
export class AnalysisError extends Error {
  override name = "AnalysisError";
}

const encoder = new TextEncoder();
const decoder = new TextDecoder();

function instantiate(module: WebAssembly.Module): Exports {
  const exports = new WebAssembly.Instance(module, {}).exports as Partial<Exports>;
  const { memory, vela_alloc, vela_handle, vela_free_response } = exports;
  if (!(memory instanceof WebAssembly.Memory) || !vela_alloc || !vela_handle || !vela_free_response) {
    throw new AnalysisError("not a Vela analysis module");
  }
  return { memory, vela_alloc, vela_handle, vela_free_response };
}

type Results<Q extends readonly Query[]> = { [I in keyof Q]: Q[I] extends Query ? QueryResults[Q[I]["kind"]] : never };

/**
 * `text` with each lone surrogate (half of a character, as a paste or a
 * half-deleted emoji can leave) replaced by U+FFFD. JSON can't carry a lone
 * surrogate to the module, and one code unit replaces one, so offsets stay
 * the same.
 */
export function wellFormed(text: string): string {
  return text.replace(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/g, "\uFFFD");
}

export class VelaAnalysis {
  private exports: Exports;

  private constructor(private readonly module: WebAssembly.Module) {
    this.exports = instantiate(module);
  }

  /** Compiles the module from its bytes or from a fetch response. */
  static async load(wasm: BufferSource | Response | PromiseLike<Response>): Promise<VelaAnalysis> {
    const source = await wasm;
    const module =
      source instanceof Response ? await WebAssembly.compileStreaming(source) : await WebAssembly.compile(source);
    return new VelaAnalysis(module);
  }

  /**
   * Answers several queries about one version of `source` at once, analyzing
   * it only once. Throws `AnalysisError` if the module rejects the request or
   * crashes; after a crash the module is restarted, so later calls work.
   */
  request<const Q extends readonly Query[]>(source: string, queries: Q): Results<Q> {
    const response = this.call(JSON.stringify({ source: wellFormed(source), queries })) as { results?: unknown[]; error?: string };
    if (typeof response.error === "string") {
      throw new AnalysisError(response.error);
    }
    if (!Array.isArray(response.results) || response.results.length !== queries.length) {
      throw new AnalysisError("malformed response from the analysis module");
    }
    return response.results as Results<Q>;
  }

  /** Everything that changes when the text changes, in one call. */
  file(source: string): { tokens: Token[]; diagnostics: Diagnostic[]; outline: OutlineItem[] } {
    const [tokens, diagnostics, outline] = this.request(source, [
      { kind: "tokens" },
      { kind: "diagnostics" },
      { kind: "outline" },
    ] as const);
    return { tokens, diagnostics, outline };
  }

  complete(source: string, offset: number): Completions {
    return this.request(source, [{ kind: "complete", offset }] as const)[0];
  }

  hover(source: string, offset: number): Hover | null {
    return this.request(source, [{ kind: "hover", offset }] as const)[0];
  }

  definition(source: string, offset: number): Location | null {
    return this.request(source, [{ kind: "definition", offset }] as const)[0];
  }

  language(): Language {
    return this.request("", [{ kind: "language" }] as const)[0];
  }

  private call(body: string): unknown {
    const bytes = encoder.encode(body);
    let json: string;
    try {
      const { memory, vela_alloc, vela_handle, vela_free_response } = this.exports;
      const request = vela_alloc(bytes.length) >>> 0;
      new Uint8Array(memory.buffer, request, bytes.length).set(bytes);
      const response = vela_handle(request, bytes.length) >>> 0;
      // Memory may have grown during the call, so views are made afterwards.
      const length = new DataView(memory.buffer).getUint32(response, true);
      json = decoder.decode(new Uint8Array(memory.buffer, response + 4, length));
      vela_free_response(response);
    } catch (error) {
      if (error instanceof WebAssembly.RuntimeError) {
        // A trap can leave the module's memory inconsistent: start a fresh instance.
        this.exports = instantiate(this.module);
        throw new AnalysisError(`the analysis module crashed: ${error.message}`);
      }
      throw error;
    }
    return JSON.parse(json);
  }
}
