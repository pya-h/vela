# Vela Performance & Comparison Benchmarks

This directory contains 9 canonical & real-world benchmark tasks comparing **Vela** against **C**, **Rust**, **Go**, and **Zig**.

## Benchmark Tasks

1. **`01_mandelbrot` (CPU & Tight Loops)**
   * Computes a 2000×2000 Mandelbrot escape sum (`97631088`) up to 50 iterations per pixel.
   * Evaluates floating-point math, loop optimization, and register allocation.
2. **`02_binary_trees` (Memory Lifecycles)**
   * Builds and traverses binary trees up to depth 16, allocating and deallocating millions of nodes.
   * Compares memory paradigms: Vela Regions vs Zig Arena vs Go GC vs Rust RAII vs C `malloc`/`free`.
3. **`03_concurrency` (Channels & Message Passing)**
   * 4 concurrent workers connected in a ring passing a token 100,000 times until exhaustion.
   * Compares task spawning, thread scheduling, and channel synchronization latency.
4. **`04_quicksort` (Contiguous Slices & Array Swapping)**
   * In-place quicksort of 2,500,000 64-bit integers with deterministic PRNG.
   * Tests memory bandwidth, cache performance, slice access, and branch prediction.
5. **`05_prime_sieve` (Flat Memory Access & Striding)**
   * Sieve of Eratosthenes counting primes up to 30,000,000 (`1857859`).
   * Tests linear memory traversal, cache line strides, and memory density.
6. **`06_hashmap` (Hashing & Key Lookups)**
   * 5,000,000 deterministic inserts and lookups across 200,000 keys (`unique: 200000 total: 5000000`).
   * Compares standard hash table implementations (`[int: int]@r`, `std::collections::HashMap`, Go `map`, Zig `AutoHashMap`, and C open addressing).
7. **`07_web_server` (Concurrent HTTP Web Server)**
   * Minimal concurrent HTTP/1.1 servers (`/health` and `/json`) backed by 4 worker threads, benchmarked by a native C multi-threaded load generator (`benchmarks/tools/http_load.c`).
   * Compares Vela's `std/web` concurrent server stack against Go's `net/http` and multi-threaded Rust, C, and Zig TCP HTTP servers across throughput (req/s), P95 latency, and Peak RSS under load.
8. **`08_nbody` (Vector Math & Symplectic Integration)**
   * Symplectic numerical integration of 5 celestial bodies (Sun, Jupiter, Saturn, Uranus, Neptune) over 5,000,000 time steps.
   * Tests pure double-precision floating-point throughput, vector arithmetic, AoS memory layout, and compiler math optimizations without heap allocation.
9. **`09_matmul` (Memory Bandwidth & Cache Locality)**
   * Dense double-precision matrix multiplication $N=1000$ (1,000,000,000 inner operations) with cache-friendly $i, k, j$ traversal.
   * Tests L1/L2/L3 cache misses, contiguous stride memory bandwidth, and auto-vectorization across 24 MB of matrix buffers.

## Methodology

* **No timing contamination**: Batch benchmarks invoke compiled binaries directly via `time.perf_counter()` after warmups and report **Median** alongside **Mean ± Stddev** so OS spikes don't distort results.
* **Accurate memory & CPU profiling**: Peak RSS (`Maximum resident set size`), User CPU time, System CPU time, and context switches are captured in a separate GNU `/usr/bin/time -v` pass (or Linux `/proc/<pid>/status` `VmHWM` for the HTTP servers).
* **Exact output verification**: Every language must produce byte-for-byte identical stdout / JSON output.
* **Fair binary sizes & compile times**: All production binaries are stripped (`-s` for C/Vela, `-C strip=symbols` for Rust, `-ldflags="-s -w"` for Go, `-fstrip` for Zig) and compile times are recorded cleanly.

## How to Run

```bash
# Run all 9 benchmarks across all 5 languages (default: 7 timed runs, 2 warmups)
./benchmarks/run.sh

# Run a specific benchmark
./benchmarks/run.sh --filter nbody
./benchmarks/run.sh --filter matmul
./benchmarks/run.sh --filter web

# Clean build cache and run with custom iterations
./benchmarks/run.sh --clean --runs 7 --warmup 2
```

## Generated Reports

- **Markdown Summary**: [`benchmarks/RESULTS.md`](./RESULTS.md)
- **Interactive Web App Dashboard**: Open [`benchmarks/RESULTS.html`](./RESULTS.html) in your browser for interactive charts, 8-axis radar profile comparisons, metric switchers, full metric tables, and live core algorithm code previews with language tabs (`[GO]`, `[VELA]`, `[RUST]`, `[ZIG]`, `[C]`).
