# Vela Benchmark Results

Comparison between **Vela**, **C (GCC -O3)**, **Rust (opt-level=3, LTO)**, **Go (1.26)**, and **Zig (ReleaseFast)**.

* Runs: 5 (Warmup: 2) | Direct timing via `time.perf_counter()`; Peak RSS via GNU `time -v`. All production binaries stripped.

### Mandelbrot (CPU & Tight Loops)

*2000x2000 Mandelbrot escape sum (50 max iters) - floating-point & loop optimization*

| Language | Time (Mean ± Stddev) | Min - Max | Peak RSS | Binary Size | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `170.9 ms ± 21.2 ms` | 158.5 ms .. 207.8 ms | 1.8 MB | 14.1 KB | 1.00x (fastest) |
| **RUST** | `180.5 ms ± 12.1 ms` | 171.2 ms .. 200.1 ms | 2.2 MB | 331.9 KB | 1.06x |
| **GO** | `196.4 ms ± 22.1 ms` | 177.7 ms .. 223.8 ms | 2.1 MB | 1.52 MB | 1.15x |
| **ZIG** | `190.3 ms ± 18.5 ms` | 173.9 ms .. 215.9 ms | 2.1 MB | 5.6 KB | 1.11x |
| **VELA** | `194.8 ms ± 24.6 ms` | 173.5 ms .. 235.7 ms | 1.8 MB | 14.1 KB | 1.14x |

### Binary Trees (Memory Lifecycles)

*Millions of short-lived nodes (depth 16) - Region bulk free vs GC vs RAII vs Arena*

| Language | Time (Mean ± Stddev) | Min - Max | Peak RSS | Binary Size | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `254.9 ms ± 15.5 ms` | 230.9 ms .. 273.4 ms | 9.6 MB | 26.1 KB | 2.30x |
| **RUST** | `290.1 ms ± 25.4 ms` | 265.1 ms .. 318.1 ms | 9.9 MB | 332.0 KB | 2.61x |
| **GO** | `871.6 ms ± 84.0 ms` | 730.2 ms .. 952.2 ms | 17.4 MB | 1.52 MB | 7.85x |
| **ZIG** | `251.5 ms ± 9.7 ms` | 242.2 ms .. 267.2 ms | 7.9 MB | 185.2 KB | 2.27x |
| **VELA** | `111.0 ms ± 17.5 ms` | 91.5 ms .. 134.8 ms | 7.7 MB | 22.1 KB | 1.00x (fastest) |

### Token Ring (Concurrency & Tasks)

*4 concurrent workers passing a token 100,000 times - channel latency & context switching*

| Language | Time (Mean ± Stddev) | Min - Max | Peak RSS | Binary Size | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `656.2 ms ± 62.7 ms` | 566.2 ms .. 717.4 ms | 1.6 MB | 14.1 KB | 20.37x |
| **RUST** | `577.5 ms ± 38.7 ms` | 509.7 ms .. 606.1 ms | 2.3 MB | 394.8 KB | 17.93x |
| **GO** | `32.2 ms ± 7.2 ms` | 19.7 ms .. 36.6 ms | 2.3 MB | 1.53 MB | 1.00x (fastest) |
| **ZIG** | `703.3 ms ± 81.8 ms` | 588.8 ms .. 813.7 ms | 2.9 MB | 184.6 KB | 21.83x |
| **VELA** | `681.3 ms ± 142.2 ms` | 473.1 ms .. 816.4 ms | 1.9 MB | 30.2 KB | 21.15x |

### Quicksort (Contiguous Slices)

*In-place quicksort of 1,000,000 64-bit integers - array indexing, swapping & cache lines*

| Language | Time (Mean ± Stddev) | Min - Max | Peak RSS | Binary Size | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `101.4 ms ± 24.1 ms` | 60.9 ms .. 124.4 ms | 9.3 MB | 14.1 KB | 1.77x |
| **RUST** | `95.0 ms ± 44.1 ms` | 56.5 ms .. 155.8 ms | 9.7 MB | 331.9 KB | 1.66x |
| **GO** | `116.9 ms ± 19.7 ms` | 85.7 ms .. 133.2 ms | 10.1 MB | 1.52 MB | 2.05x |
| **ZIG** | `57.2 ms ± 5.0 ms` | 54.6 ms .. 66.2 ms | 9.6 MB | 182.6 KB | 1.00x (fastest) |
| **VELA** | `88.2 ms ± 21.8 ms` | 64.3 ms .. 112.8 ms | 9.2 MB | 14.1 KB | 1.54x |

### Prime Sieve (Flat Memory Access)

*Sieve of Eratosthenes up to 10,000,000 - flat boolean buffer stride traversal*

| Language | Time (Mean ± Stddev) | Min - Max | Peak RSS | Binary Size | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `63.0 ms ± 23.1 ms` | 32.1 ms .. 87.9 ms | 11.0 MB | 14.1 KB | 1.00x (fastest) |
| **RUST** | `83.1 ms ± 31.8 ms` | 27.4 ms .. 103.3 ms | 11.6 MB | 331.1 KB | 1.32x |
| **GO** | `72.0 ms ± 15.1 ms` | 54.5 ms .. 90.1 ms | 12.1 MB | 1.52 MB | 1.14x |
| **ZIG** | `76.3 ms ± 31.4 ms` | 23.9 ms .. 102.5 ms | 11.3 MB | 182.5 KB | 1.21x |
| **VELA** | `85.0 ms ± 34.8 ms` | 32.3 ms .. 126.2 ms | 11.0 MB | 14.1 KB | 1.35x |

