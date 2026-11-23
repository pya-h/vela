# Vela Benchmark Results

Comparison between **Vela**, **C (GCC -O3)**, **Rust (opt-level=3, LTO)**, **Go (1.26)**, and **Zig (ReleaseFast)** across 9 comprehensive workloads.

* Runs: 5 (Warmup: 2) | Direct timing via `time.perf_counter()`; Peak RSS via GNU `time -v` / `/proc/pid/status` `VmHWM`. All binaries stripped.

### Mandelbrot (CPU & Tight Loops)

*2000x2000 Mandelbrot escape sum (50 max iters) - floating-point & loop optimization*

| Language | Median Time | Mean ± Stddev | CPU (User + Sys) | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `190.4 ms` | `191.9 ms ± 17.0 ms` | 0.160s | 1.8 MB | 14.1 KB | 68.9 ms | 1.09x |
| **RUST** | `197.8 ms` | `202.2 ms ± 22.6 ms` | 0.180s | 2.2 MB | 331.9 KB | 1.762 s | 1.13x |
| **GO** | `191.0 ms` | `221.8 ms ± 47.8 ms` | 0.170s | 2.1 MB | 1.52 MB | 76.1 ms | 1.09x |
| **ZIG** | `201.1 ms` | `194.8 ms ± 23.7 ms` | 0.200s | 2.1 MB | 5.6 KB | 191.2 ms | 1.15x |
| **VELA** | `175.2 ms` | `179.7 ms ± 12.6 ms` | 0.200s | 1.8 MB | 14.1 KB | 137.0 ms | 1.00x (fastest) |

### Binary Trees (Memory Lifecycles)

*Millions of short-lived nodes (depth 16) - Region bulk free vs GC vs RAII vs Arena*

| Language | Median Time | Mean ± Stddev | CPU (User + Sys) | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `265.7 ms` | `255.6 ms ± 19.9 ms` | 0.230s | 9.6 MB | 26.1 KB | 171.7 ms | 2.84x |
| **RUST** | `279.3 ms` | `280.6 ms ± 15.4 ms` | 0.280s | 10.0 MB | 332.0 KB | 1.674 s | 2.98x |
| **GO** | `803.7 ms` | `837.8 ms ± 84.1 ms` | 1.930s | 17.8 MB | 1.52 MB | 126.3 ms | 8.59x |
| **ZIG** | `240.6 ms` | `242.7 ms ± 4.4 ms` | 0.290s | 8.1 MB | 185.2 KB | 2.126 s | 2.57x |
| **VELA** | `93.6 ms` | `118.1 ms ± 41.8 ms` | 0.110s | 7.7 MB | 22.1 KB | 190.8 ms | 1.00x (fastest) |

### Token Ring (Concurrency & Tasks)

*4 concurrent workers passing a token 100,000 times - channel latency & context switching*

| Language | Median Time | Mean ± Stddev | CPU (User + Sys) | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `573.1 ms` | `568.9 ms ± 30.2 ms` | 0.470s | 1.6 MB | 14.1 KB | 125.9 ms | 19.61x |
| **RUST** | `427.0 ms` | `429.0 ms ± 45.4 ms` | 0.400s | 2.4 MB | 394.8 KB | 1.975 s | 14.61x |
| **GO** | `29.2 ms` | `29.6 ms ± 893.1 µs` | 0.020s | 2.3 MB | 1.53 MB | 171.3 ms | 1.00x (fastest) |
| **ZIG** | `437.4 ms` | `430.8 ms ± 32.4 ms` | 0.440s | 2.9 MB | 184.6 KB | 1.832 s | 14.96x |
| **VELA** | `482.4 ms` | `496.7 ms ± 25.5 ms` | 0.320s | 1.8 MB | 30.2 KB | 415.1 ms | 16.50x |

### Quicksort (Contiguous Slices)

*In-place quicksort of 2,500,000 64-bit integers - array indexing, swapping & cache lines*

| Language | Median Time | Mean ± Stddev | CPU (User + Sys) | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `157.4 ms` | `169.8 ms ± 19.5 ms` | 0.170s | 20.7 MB | 14.1 KB | 203.4 ms | 1.13x |
| **RUST** | `139.8 ms` | `147.4 ms ± 16.9 ms` | 0.140s | 21.1 MB | 332.0 KB | 1.649 s | 1.00x (fastest) |
| **GO** | `170.9 ms` | `175.3 ms ± 8.5 ms` | 0.160s | 21.7 MB | 1.52 MB | 127.7 ms | 1.22x |
| **ZIG** | `175.3 ms` | `162.9 ms ± 22.1 ms` | 0.150s | 20.9 MB | 182.6 KB | 1.785 s | 1.25x |
| **VELA** | `200.1 ms` | `197.1 ms ± 15.5 ms` | 0.160s | 20.7 MB | 14.1 KB | 140.6 ms | 1.43x |

### Prime Sieve (Flat Memory Access)

*Sieve of Eratosthenes up to 30,000,000 - flat boolean buffer stride traversal*

| Language | Median Time | Mean ± Stddev | CPU (User + Sys) | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `129.0 ms` | `126.4 ms ± 8.0 ms` | 0.120s | 30.0 MB | 14.1 KB | 57.9 ms | 1.14x |
| **RUST** | `128.4 ms` | `123.7 ms ± 24.6 ms` | 0.100s | 30.6 MB | 331.1 KB | 1.680 s | 1.13x |
| **GO** | `124.1 ms` | `122.3 ms ± 11.6 ms` | 0.120s | 31.1 MB | 1.52 MB | 164.9 ms | 1.09x |
| **ZIG** | `113.4 ms` | `109.8 ms ± 15.6 ms` | 0.090s | 30.4 MB | 182.5 KB | 1.834 s | 1.00x (fastest) |
| **VELA** | `120.6 ms` | `119.9 ms ± 7.1 ms` | 0.140s | 30.0 MB | 14.1 KB | 156.6 ms | 1.06x |

### Hash Map (Hashing & Lookups)

*5,000,000 random inserts/lookups over 200,000 keys - collision probing & map resizing*

| Language | Median Time | Mean ± Stddev | CPU (User + Sys) | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `62.4 ms` | `72.0 ms ± 17.3 ms` | 0.070s | 19.5 MB | 14.1 KB | 85.3 ms | 1.00x (fastest) |
| **RUST** | `101.1 ms` | `99.8 ms ± 26.2 ms` | 0.090s | 8.6 MB | 342.8 KB | 1.700 s | 1.62x |
| **GO** | `94.0 ms` | `102.7 ms ± 23.0 ms` | 0.080s | 8.7 MB | 1.52 MB | 123.2 ms | 1.51x |
| **ZIG** | `100.2 ms` | `95.8 ms ± 17.5 ms` | 0.100s | 8.3 MB | 184.2 KB | 1.888 s | 1.61x |
| **VELA** | `82.3 ms` | `96.3 ms ± 22.5 ms` | 0.090s | 16.1 MB | 18.1 KB | 200.8 ms | 1.32x |

### HTTP Web Server (Networking & Web I/O)

*Minimal concurrent HTTP/1.1 JSON & health endpoints across 4 worker threads (2,000 reqs/run)*

| Language | Throughput (RPS) | P95 Latency | 2000 Reqs Time | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | **43,058 req/s** | 0.12 ms | 46.4 ms | 1.7 MB | 14.1 KB | 90.5 ms | 1.02x |
| **RUST** | **43,484 req/s** | 0.12 ms | 46.0 ms | 2.4 MB | 363.1 KB | 1.873 s | 1.01x |
| **GO** | **29,017 req/s** | 0.26 ms | 68.9 ms | 14.6 MB | 5.55 MB | 364.3 ms | 1.51x |
| **ZIG** | **43,923 req/s** | 0.13 ms | 45.5 ms | 3.1 MB | 183.6 KB | 1.884 s | 1.00x (fastest) |
| **VELA** | **37,450 req/s** | 0.13 ms | 53.4 ms | 2.2 MB | 158.3 KB | 1.958 s | 1.17x |

### N-Body Simulation (Vector Math & Physics)

*Symplectic orbital simulation of 5 celestial bodies over 5,000,000 steps - pure double-precision vector math*

| Language | Median Time | Mean ± Stddev | CPU (User + Sys) | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `161.4 ms` | `167.9 ms ± 23.7 ms` | 0.180s | 1.6 MB | 14.1 KB | 162.9 ms | 1.14x |
| **RUST** | `141.5 ms` | `150.2 ms ± 20.8 ms` | 0.160s | 2.1 MB | 334.5 KB | 1.671 s | 1.00x (fastest) |
| **GO** | `285.6 ms` | `285.6 ms ± 9.7 ms` | 0.300s | 2.1 MB | 1.52 MB | 109.8 ms | 2.02x |
| **ZIG** | `159.5 ms` | `167.8 ms ± 26.5 ms` | 0.190s | 1.9 MB | 9.1 KB | 215.7 ms | 1.13x |
| **VELA** | `677.9 ms` | `693.3 ms ± 46.1 ms` | 0.670s | 2.2 MB | 14.1 KB | 174.9 ms | 4.79x |

### Matrix Multiplication (Memory & Cache Locality)

*Dense double matrix multiply N=1000 (1 billion inner ops) - contiguous stride cache locality & loop vectorization*

| Language | Median Time | Mean ± Stddev | CPU (User + Sys) | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `148.1 ms` | `151.5 ms ± 11.3 ms` | 0.140s | 24.2 MB | 14.1 KB | 61.6 ms | 1.00x (fastest) |
| **RUST** | `169.0 ms` | `174.9 ms ± 26.4 ms` | 0.180s | 24.8 MB | 331.7 KB | 1.756 s | 1.14x |
| **GO** | `479.8 ms` | `486.9 ms ± 15.1 ms` | 0.460s | 25.4 MB | 1.52 MB | 155.1 ms | 3.24x |
| **ZIG** | `291.5 ms` | `283.5 ms ± 32.0 ms` | 0.280s | 24.5 MB | 6.4 KB | 182.2 ms | 1.97x |
| **VELA** | `720.1 ms` | `713.4 ms ± 19.2 ms` | 0.670s | 24.4 MB | 18.1 KB | 209.8 ms | 4.86x |

