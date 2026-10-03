// Runs Vela's language analysis (the compiler's lexer, parser, and checker,
// compiled to WebAssembly) and exchanges JSON with it. See ide/wasm/src/lib.rs
// for the memory protocol.

import type { Completions, Diagnostic, Hover, Language, Location, OutlineItem, ProgramContext, Query, QueryResults, Token } from "./protocol";

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

/** Whether two sets of module texts are the same, path for path. */
function sameModules(a: Record<string, string>, b: Record<string, string>): boolean {
  const paths = Object.keys(a);
  return paths.length === Object.keys(b).length && paths.every((path) => Object.hasOwn(b, path) && a[path] === b[path]);
}

export class VelaAnalysis {
  private exports: Exports;
  /**
   * The modules the module was last sent, which it keeps: a request with the
   * same ones says so instead of sending their text again. Null when it keeps
   * none, as after a restart.
   */
  private sentModules: Record<string, string> | null = null;

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
   * it only once. `context` gives the file's path and the modules it may include.
   * Throws `AnalysisError` if the module rejects the request or crashes; after
   * a crash the module is restarted, so later calls work.
   */
  request<const Q extends readonly Query[]>(source: string, queries: Q, context?: ProgramContext): Results<Q> {
    let response = this.call(JSON.stringify(this.body(source, queries, context))) as { results?: unknown[]; error?: string; resend?: boolean };
    // The module kept no modules after all: send them.
    if (response.resend === true && context) {
      this.sentModules = null;
      response = this.call(JSON.stringify(this.body(source, queries, context))) as typeof response;
    }
    if (typeof response.error === "string") {
      throw new AnalysisError(response.error);
    }
    if (!Array.isArray(response.results) || response.results.length !== queries.length) {
      throw new AnalysisError("malformed response from the analysis module");
    }
    return response.results as Results<Q>;
  }

  /**
   * Everything that changes when the text changes, in one call, with the
   * modules the program includes that `context` didn't give (`missing`).
   */
  file(source: string, context?: ProgramContext): { tokens: Token[]; diagnostics: Diagnostic[]; outline: OutlineItem[]; missing: string[] } {
    const [tokens, diagnostics, outline, missing] = this.request(
      source,
      [{ kind: "tokens" }, { kind: "diagnostics" }, { kind: "outline" }, { kind: "missing-modules" }] as const,
      context,
    );
    return { tokens, diagnostics, outline, missing };
  }

  complete(source: string, offset: number, context?: ProgramContext): Completions {
    return this.request(source, [{ kind: "complete", offset }] as const, context)[0];
  }

  hover(source: string, offset: number, context?: ProgramContext): Hover | null {
    return this.request(source, [{ kind: "hover", offset }] as const, context)[0];
  }

  definition(source: string, offset: number, context?: ProgramContext): Location | null {
    return this.request(source, [{ kind: "definition", offset }] as const, context)[0];
  }

  language(): Language {
    return this.request("", [{ kind: "language" }] as const)[0];
  }

  /** The request's JSON body, without the modules when the module keeps the same ones. */
  private body(source: string, queries: readonly Query[], context?: ProgramContext): object {
    if (!context) return { source: wellFormed(source), queries };
    if (this.sentModules !== null && sameModules(this.sentModules, context.modules)) {
      return { source: wellFormed(source), path: context.path, "same-modules": true, queries };
    }
    const modules = Object.fromEntries(Object.entries(context.modules).map(([path, text]) => [path, wellFormed(text)]));
    this.sentModules = { ...context.modules };
    return { source: wellFormed(source), path: context.path, modules, queries };
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
        // A trap can leave the module's memory inconsistent: start a fresh
        // instance, which keeps no modules.
        this.exports = instantiate(this.module);
        this.sentModules = null;
        throw new AnalysisError(`the analysis module crashed: ${error.message}`);
      }
      throw error;
    }
    return JSON.parse(json);
  }
}
