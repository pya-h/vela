import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { VelaAnalysis } from "./bridge";

export const webFolder = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
export const examplesFolder = join(webFolder, "..", "..", "examples");

export function wasmBytes(): Uint8Array<ArrayBuffer> {
  try {
    return new Uint8Array(readFileSync(join(webFolder, "wasm", "vela_analysis.wasm")));
  } catch {
    throw new Error("wasm/vela_analysis.wasm is missing: run `npm run build:wasm` first");
  }
}

export function loadAnalysis(): Promise<VelaAnalysis> {
  return VelaAnalysis.load(wasmBytes());
}
