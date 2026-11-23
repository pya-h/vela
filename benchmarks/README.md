# Vela Performance & Comparison Benchmarks

This directory contains canonical benchmark tasks comparing **Vela** against **C**, **Rust**, **Go**, and **Zig**.

## Benchmark Tasks

1. **`01_mandelbrot` (CPU & Tight Loops)**
   * Computes a 2000x2000 Mandelbrot escape sum (`97631088`) up to 50 iterations per pixel.
   * Evaluates floating-point math, loop optimization, and register allocation without compiler recursion-unrolling artifacts.
2. **`02_binary_trees` (Memory Allocation & Deallocation)**
   * Builds and traverses binary trees up to depth 16, allocating and deallocating millions of nodes.
   * Compares memory management paradigms:
     * **Vela**: Region-based allocation (`new Tree(...) at r` + bulk teardown per pass).
     * **Zig**: `std.heap.ArenaAllocator` (explicit arena reset per pass).
     * **Go**: Concurrent GC mark-sweep & heap expansion.
     * **Rust**: Recursive RAII heap drop (`Box<Tree>`).
     * **C**: Direct `malloc` and `free`.
3. **`03_concurrency` (Channels & Message Passing)**
   * 4 concurrent workers connected in a ring passing a token 100,000 times until exhaustion.
   * Compares task spawning, thread scheduling, and channel synchronization latency.

## Methodology

* **No timing contamination**: Timed runs invoke compiled binaries directly via `time.perf_counter()` after warmups.
* **Accurate memory footprint**: Peak RSS (`Maximum resident set size`) is measured in a separate pass via `/usr/bin/time -v` so `time`'s startup overhead does not skew execution times.
* **Exact output verification**: Every language must produce byte-for-byte identical stdout output.
* **Fair binary sizes**: All production binaries are stripped (`-s` for C/Vela, `-C strip=symbols` for Rust, `-ldflags="-s -w"` for Go, `-fstrip` for Zig).
* **Missing toolchains**: Any compiler not found in `PATH` (for example, `zig` if not installed) is gracefully skipped.

## How to Run

Run all benchmarks (default: 5 timed runs, 2 warmups):
```bash
./benchmarks/run.sh
```

Run a specific benchmark:
```bash
./benchmarks/run.sh --filter tree
```

Adjust iteration count or clean build cache:
```bash
./benchmarks/run.sh --clean --runs 10 --warmup 3
```

Generated results are printed to stdout and saved in Markdown format to `benchmarks/RESULTS.md`.
