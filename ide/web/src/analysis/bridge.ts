import type { Completions, Diagnostic, Hover, Language, Location, OutlineItem, ProgramContext, Query, QueryResults, Token } from "./protocol";

interface Exports {
  memory: WebAssembly.Memory;
  vela_alloc(len: number): number;
  vela_handle(ptr: number, len: number): number;
  vela_free_response(ptr: number): void;
}

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

/** Replaces lone UTF-16 surrogates 1-for-1 with U+FFFD so JSON serialization succeeds without shifting JS offsets. */
export function wellFormed(text: string): string {
  return text.replace(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/g, "\uFFFD");
}

function sameModules(a: Record<string, string>, b: Record<string, string>): boolean {
  const paths = Object.keys(a);
  return paths.length === Object.keys(b).length && paths.every((path) => Object.hasOwn(b, path) && a[path] === b[path]);
}

export class VelaAnalysis {
  private exports: Exports;
  private sentModules: Record<string, string> | null = null;

  private constructor(private readonly module: WebAssembly.Module) {
    this.exports = instantiate(module);
  }

  static async load(wasm: BufferSource | Response | PromiseLike<Response>): Promise<VelaAnalysis> {
    const source = await wasm;
    const module =
      source instanceof Response ? await WebAssembly.compileStreaming(source) : await WebAssembly.compile(source);
    return new VelaAnalysis(module);
  }

  request<const Q extends readonly Query[]>(source: string, queries: Q, context?: ProgramContext): Results<Q> {
    let response = this.send(source, queries, context);
    if (response.resend === true && context) {
      this.sentModules = null;
      response = this.send(source, queries, context);
    }
    if (typeof response.error === "string") {
      throw new AnalysisError(response.error);
    }
    if (!Array.isArray(response.results) || response.results.length !== queries.length) {
      throw new AnalysisError("malformed response from the analysis module");
    }
    return response.results as Results<Q>;
  }

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

  private send(source: string, queries: readonly Query[], context?: ProgramContext): { results?: unknown[]; error?: string; resend?: boolean } {
    type Response = { results?: unknown[]; error?: string; resend?: boolean };
    if (!context) return this.call(JSON.stringify({ source: wellFormed(source), queries })) as Response;
    if (this.sentModules !== null && sameModules(this.sentModules, context.modules)) {
      return this.call(JSON.stringify({ source: wellFormed(source), path: context.path, "same-modules": true, queries })) as Response;
    }
    const sent = { ...context.modules };
    const modules = Object.fromEntries(Object.entries(sent).map(([path, text]) => [path, wellFormed(text)]));
    // Clear first: a rejected call may still have altered kept modules inside WASM.
    this.sentModules = null;
    const response = this.call(JSON.stringify({ source: wellFormed(source), path: context.path, modules, queries })) as Response;
    if (typeof response.error !== "string") this.sentModules = sent;
    return response;
  }

  private call(body: string): unknown {
    const bytes = encoder.encode(body);
    let json: string;
    try {
      const { memory, vela_alloc, vela_handle, vela_free_response } = this.exports;
      const request = vela_alloc(bytes.length) >>> 0;
      new Uint8Array(memory.buffer, request, bytes.length).set(bytes);
      const response = vela_handle(request, bytes.length) >>> 0;
      // Recreate views because `vela_handle` may grow WebAssembly memory.
      const length = new DataView(memory.buffer).getUint32(response, true);
      json = decoder.decode(new Uint8Array(memory.buffer, response + 4, length));
      vela_free_response(response);
    } catch (error) {
      if (error instanceof WebAssembly.RuntimeError) {
        // Re-instantiate after a WASM trap since linear memory may be corrupted.
        this.exports = instantiate(this.module);
        this.sentModules = null;
        throw new AnalysisError(`the analysis module crashed: ${error.message}`);
      }
      throw error;
    }
    return JSON.parse(json);
  }
}
