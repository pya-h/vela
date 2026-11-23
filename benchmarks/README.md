# Vela Performance & Comparison Benchmarks

This directory contains lightweight, canonical benchmark tasks comparing **Vela** against **C**, **Rust**, and **Go**.

## Benchmark Tasks

1. **`01_fibonacci` (CPU & Recursion)**
   * Recursive `fib(40)` calculating `102334155`.
   * Evaluates function call overhead, stack frames, register allocation, and raw integer arithmetic.
2. **`02_binary_trees` (Memory Allocation & Deallocation)**
   * Builds and traverses trees up to depth 16, allocating and deallocating millions of nodes.
   * Compares memory management paradigms:
     * **Vela**: Region-based allocation (`new Tree(...) at r` + bulk teardown per pass).
     * **Go**: Concurrent GC mark-sweep & heap expansion.
     * **Rust**: Recursive RAII heap drop (`Box<Tree>`).
     * **C**: Direct `malloc` and `free`.
3. **`03_concurrency` (Channels & Message Passing)**
   * 4 concurrent tasks connected in a ring passing a token 100,000 times until exhaustion.
   * Compares task spawning, thread scheduling, and channel synchronization latency.

## Requirements

* **Vela**: Auto-built in release mode via `cargo build --release`
* **GCC / Clang**: C11 compiler with `-O3`
* **Rust**: `rustc` with `-C opt-level=3`
* **Go**: `go` toolchain (Go 1.20+)
* **GNU Time**: `/usr/bin/time` (used for measuring Maximum Resident Set Size / Peak RSS)
* **Python 3**: Runner script

## How to Run

Run all benchmarks with 5 iterations and 2 warmups:
```bash
./benchmarks/run.sh
```

Run a specific benchmark (e.g. Binary Trees):
```bash
./benchmarks/run.sh --filter tree
```

Adjust iteration count:
```bash
./benchmarks/run.sh --runs 10 --warmup 3
```

Clean built binaries before running:
```bash
./benchmarks/run.sh --clean
```

Generated results will be printed to stdout and saved in Markdown format to `benchmarks/RESULTS.md`.
