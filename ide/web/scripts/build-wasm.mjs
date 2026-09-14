// Builds the analysis WebAssembly module from the Rust workspace and copies it
// to wasm/vela_analysis.wasm, where the bridge and its tests load it from.
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const web = dirname(dirname(fileURLToPath(import.meta.url)));
const root = join(web, "..", "..");

execFileSync("cargo", ["build", "-p", "vela-analysis-wasm", "--target", "wasm32-unknown-unknown", "--release"], {
  cwd: root,
  stdio: "inherit",
});

// Cargo's target directory may be moved (CARGO_TARGET_DIR, .cargo/config.toml).
const metadata = JSON.parse(
  execFileSync("cargo", ["metadata", "--format-version", "1", "--no-deps"], { cwd: root, encoding: "utf8" }),
);
const built = join(metadata.target_directory, "wasm32-unknown-unknown", "release", "vela_analysis_wasm.wasm");
const target = join(web, "wasm", "vela_analysis.wasm");
mkdirSync(dirname(target), { recursive: true });
copyFileSync(built, target);
console.log(`wasm/vela_analysis.wasm (${Math.round(statSync(target).size / 1024)} KiB)`);
