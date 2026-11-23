# Vela Performance & Comparison Benchmarks

This directory contains 5 canonical benchmark tasks comparing **Vela** against **C**, **Rust**, **Go**, and **Zig**.

## Benchmark Tasks

1. **`01_mandelbrot` (CPU & Tight Loops)**
   * Computes a 2000x2000 Mandelbrot escape sum (`97631088`) up to 50 iterations per pixel.
   * Evaluates floating-point math, loop optimization, and register allocation.
2. **`02_binary_trees` (Memory Lifecycles)**
   * Builds and traverses binary trees up to depth 16, allocating and deallocating millions of nodes.
   * Compares memory paradigms: Vela Regions vs Zig Arena vs Go GC vs Rust RAII vs C malloc/free.
3. **`03_concurrency` (Channels & Message Passing)**
   * 4 concurrent workers connected in a ring passing a token 100,000 times until exhaustion.
   * Compares task spawning, thread scheduling, and channel synchronization latency.
4. **`04_quicksort` (Contiguous Slices & Array Swapping)**
   * In-place quicksort of 1,000,000 64-bit integers with deterministic PRNG.
   * Tests memory bandwidth, cache performance, slice access, and branch prediction.
5. **`05_prime_sieve` (Flat Memory Access & Striding)**
   * Sieve of Eratosthenes counting primes up to 10,000,000 (`664579`).
   * Tests linear memory traversal, cache line strides, and density.

## Methodology

* **No timing contamination**: Timed runs invoke compiled binaries directly via high-resolution monotonic timers (`time.perf_counter()`) after warmups.
* **Accurate memory footprint**: Peak RSS (`Maximum resident set size`) is measured in a separate pass via `/usr/bin/time -v`.
* **Exact output verification**: Every language must produce byte-for-byte identical stdout output.
* **Fair binary sizes**: All production binaries are stripped (`-s` for C/Vela, `-C strip=symbols` for Rust, `-ldflags="-s -w"` for Go, `-fstrip` for Zig).

## How to Run

Run all benchmarks (default: 5 timed runs, 2 warmups):
```bash
./benchmarks/run.sh
```

Run a specific benchmark:
```bash
./benchmarks/run.sh --filter qsort
```

Adjust iteration count or clean build cache:
```bash
./benchmarks/run.sh --clean --runs 10 --warmup 3
```

## Generated Reports

- **Markdown Summary**: [`benchmarks/RESULTS.md`](./RESULTS.md)
- **Interactive Web App Dashboard**: Open [`benchmarks/RESULTS.html`](./RESULTS.html) directly in any browser for interactive bar charts, metric toggles, and comparative breakdowns.
