# Vela Benchmark Results

Comparison between **Vela**, **C (GCC -O3)**, **Rust (opt-level=3, LTO)**, **Go (1.26)**, and **Zig (ReleaseFast)**.

* Runs: 5 (Warmup: 2) | Direct timing via `time.perf_counter()`; Peak RSS via GNU `time -v` / `/proc/pid/status` `VmHWM`. All binaries stripped.

### Mandelbrot (CPU & Tight Loops)

*2000x2000 Mandelbrot escape sum (50 max iters) - floating-point & loop optimization*

| Language | Median Time | Mean ± Stddev | CPU (User + Sys) | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `184.2 ms` | `182.9 ms ± 16.2 ms` | 0.160s | 1.8 MB | 14.1 KB | 72.7 ms | 1.00x (fastest) |
| **RUST** | `204.2 ms` | `209.8 ms ± 33.0 ms` | 0.230s | 2.2 MB | 331.9 KB | 1.648 s | 1.11x |
| **GO** | `185.9 ms` | `200.2 ms ± 22.9 ms` | 0.180s | 2.3 MB | 1.52 MB | 104.5 ms | 1.01x |
| **ZIG** | `188.5 ms` | `191.9 ms ± 21.2 ms` | 0.180s | 2.1 MB | 5.6 KB | 181.2 ms | 1.02x |
| **VELA** | `191.4 ms` | `187.0 ms ± 16.6 ms` | 0.200s | 1.8 MB | 14.1 KB | 129.0 ms | 1.04x |

### Binary Trees (Memory Lifecycles)

*Millions of short-lived nodes (depth 16) - Region bulk free vs GC vs RAII vs Arena*

| Language | Median Time | Mean ± Stddev | CPU (User + Sys) | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `251.8 ms` | `252.0 ms ± 18.4 ms` | 0.230s | 9.7 MB | 26.1 KB | 193.1 ms | 2.27x |
| **RUST** | `278.2 ms` | `285.3 ms ± 14.7 ms` | 0.280s | 10.0 MB | 332.0 KB | 1.829 s | 2.51x |
| **GO** | `657.6 ms` | `712.9 ms ± 211.2 ms` | 1.500s | 17.6 MB | 1.52 MB | 101.3 ms | 5.92x |
| **ZIG** | `249.4 ms` | `264.3 ms ± 26.6 ms` | 0.250s | 8.0 MB | 185.2 KB | 2.116 s | 2.25x |
| **VELA** | `111.1 ms` | `117.3 ms ± 14.9 ms` | 0.110s | 7.7 MB | 22.1 KB | 266.0 ms | 1.00x (fastest) |

### Token Ring (Concurrency & Tasks)

*4 concurrent workers passing a token 100,000 times - channel latency & context switching*

| Language | Median Time | Mean ± Stddev | CPU (User + Sys) | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `724.7 ms` | `720.4 ms ± 36.2 ms` | 0.630s | 1.6 MB | 14.1 KB | 121.4 ms | 48.33x |
| **RUST** | `520.6 ms` | `496.2 ms ± 131.2 ms` | 0.340s | 2.3 MB | 394.8 KB | 2.154 s | 34.72x |
| **GO** | `15.0 ms` | `17.5 ms ± 4.6 ms` | 0.030s | 2.3 MB | 1.53 MB | 149.9 ms | 1.00x (fastest) |
| **ZIG** | `291.4 ms` | `295.3 ms ± 31.7 ms` | 0.230s | 2.9 MB | 184.6 KB | 2.303 s | 19.43x |
| **VELA** | `323.2 ms` | `360.9 ms ± 90.0 ms` | 0.260s | 1.8 MB | 30.2 KB | 457.6 ms | 21.55x |

### Quicksort (Contiguous Slices)

*In-place quicksort of 2,500,000 64-bit integers - array indexing, swapping & cache lines*

| Language | Median Time | Mean ± Stddev | CPU (User + Sys) | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `167.6 ms` | `169.6 ms ± 4.8 ms` | 0.150s | 20.7 MB | 14.1 KB | 77.9 ms | 1.11x |
| **RUST** | `150.4 ms` | `160.5 ms ± 16.7 ms` | 0.140s | 21.1 MB | 332.0 KB | 1.800 s | 1.00x (fastest) |
| **GO** | `174.6 ms` | `177.2 ms ± 9.1 ms` | 0.160s | 21.7 MB | 1.52 MB | 125.6 ms | 1.16x |
| **ZIG** | `173.3 ms` | `169.1 ms ± 25.8 ms` | 0.160s | 21.0 MB | 182.6 KB | 1.796 s | 1.15x |
| **VELA** | `171.6 ms` | `170.8 ms ± 3.6 ms` | 0.190s | 20.7 MB | 14.1 KB | 144.5 ms | 1.14x |

### Prime Sieve (Flat Memory Access)

*Sieve of Eratosthenes up to 30,000,000 - flat boolean buffer stride traversal*

| Language | Median Time | Mean ± Stddev | CPU (User + Sys) | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `130.9 ms` | `126.8 ms ± 22.1 ms` | 0.100s | 30.0 MB | 14.1 KB | 83.8 ms | 1.20x |
| **RUST** | `108.9 ms` | `107.8 ms ± 17.1 ms` | 0.130s | 30.7 MB | 331.1 KB | 1.748 s | 1.00x (fastest) |
| **GO** | `143.9 ms` | `135.8 ms ± 14.3 ms` | 0.120s | 31.1 MB | 1.52 MB | 124.1 ms | 1.32x |
| **ZIG** | `116.8 ms` | `113.5 ms ± 12.7 ms` | 0.100s | 30.4 MB | 182.5 KB | 1.739 s | 1.07x |
| **VELA** | `116.6 ms` | `124.6 ms ± 17.6 ms` | 0.120s | 30.0 MB | 14.1 KB | 130.3 ms | 1.07x |

### Hash Map (Hashing & Lookups)

*5,000,000 random inserts/lookups over 200,000 keys - collision probing & map resizing*

| Language | Median Time | Mean ± Stddev | CPU (User + Sys) | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `83.9 ms` | `78.9 ms ± 15.0 ms` | 0.100s | 19.5 MB | 14.1 KB | 58.0 ms | 1.08x |
| **RUST** | `85.1 ms` | `87.0 ms ± 24.4 ms` | 0.090s | 8.6 MB | 342.8 KB | 1.783 s | 1.10x |
| **GO** | `110.9 ms` | `110.7 ms ± 24.2 ms` | 0.080s | 8.7 MB | 1.52 MB | 104.0 ms | 1.43x |
| **ZIG** | `87.7 ms` | `86.2 ms ± 20.1 ms` | 0.060s | 8.3 MB | 184.2 KB | 1.793 s | 1.13x |
| **VELA** | `77.3 ms` | `84.6 ms ± 13.1 ms` | 0.100s | 16.3 MB | 18.1 KB | 138.4 ms | 1.00x (fastest) |

### HTTP Web Server (Networking & Web I/O)

*Minimal concurrent HTTP/1.1 JSON & health endpoints across 4 worker threads (2,000 reqs/run)*

| Language | Throughput (RPS) | P95 Latency | 2000 Reqs Time | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | **34,537 req/s** | 0.15 ms | 57.9 ms | 1.7 MB | 14.1 KB | 172.5 ms | 1.24x |
| **RUST** | **42,836 req/s** | 0.12 ms | 46.7 ms | 2.3 MB | 363.1 KB | 1.895 s | 1.00x (fastest) |
| **GO** | **20,788 req/s** | 0.28 ms | 96.2 ms | 14.9 MB | 5.55 MB | 376.1 ms | 2.06x |
| **ZIG** | **42,204 req/s** | 0.12 ms | 47.4 ms | 3.1 MB | 183.6 KB | 1.847 s | 1.01x |
| **VELA** | **42,462 req/s** | 0.12 ms | 47.1 ms | 2.2 MB | 158.3 KB | 2.014 s | 1.01x |

