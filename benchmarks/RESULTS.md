# Vela Benchmark Results

Comparison between **Vela**, **C (GCC -O3)**, **Rust (opt-level=3, LTO)**, **Go**, and **Zig (ReleaseFast)**.

* Runs: 10 (Warmup: 3) | Timing uses direct process execution; Peak RSS measured separately via GNU `time -v`. All binaries stripped for fair size comparison.

### Mandelbrot (CPU / Tight Loops)

*2000x2000 Mandelbrot escape sum (50 max iterations) - floating-point and loop codegen*

| Language | Time (Mean ± Stddev) | Min - Max | Peak RSS | Binary Size | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `161.7 ms ± 4.9 ms` | 157.5 ms .. 171.8 ms | 1.8 MB | 14.1 KB | 1.00x (fastest) |
| **RUST** | `172.6 ms ± 2.2 ms` | 171.1 ms .. 178.5 ms | 2.1 MB | 331.9 KB | 1.07x |
| **GO** | `181.7 ms ± 2.7 ms` | 178.6 ms .. 186.5 ms | 2.1 MB | 1.52 MB | 1.12x |
| **VELA** | `172.9 ms ± 4.3 ms` | 169.6 ms .. 184.4 ms | 1.8 MB | 14.1 KB | 1.07x |

### Binary Trees (Memory / Regions vs GC vs RAII)

*Allocating and traversing millions of nodes across short-lived tree passes (depth 16)*

| Language | Time (Mean ± Stddev) | Min - Max | Peak RSS | Binary Size | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `230.5 ms ± 2.9 ms` | 227.7 ms .. 236.2 ms | 9.7 MB | 26.1 KB | 2.44x |
| **RUST** | `257.5 ms ± 1.5 ms` | 255.6 ms .. 260.8 ms | 10.1 MB | 332.0 KB | 2.72x |
| **GO** | `312.0 ms ± 5.9 ms` | 301.1 ms .. 322.5 ms | 18.5 MB | 1.52 MB | 3.30x |
| **VELA** | `94.6 ms ± 4.1 ms` | 89.8 ms .. 100.2 ms | 7.6 MB | 22.1 KB | 1.00x (fastest) |

### Token Ring (Channels / Task Switching)

*4 concurrent workers in a ring passing a token 100,000 times*

| Language | Time (Mean ± Stddev) | Min - Max | Peak RSS | Binary Size | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `227.5 ms ± 29.3 ms` | 196.4 ms .. 300.6 ms | 1.6 MB | 14.1 KB | 15.62x |
| **RUST** | `251.7 ms ± 22.7 ms` | 206.4 ms .. 284.8 ms | 2.1 MB | 394.8 KB | 17.28x |
| **GO** | `14.6 ms ± 633.3 µs` | 13.4 ms .. 15.5 ms | 2.3 MB | 1.53 MB | 1.00x (fastest) |
| **VELA** | `250.6 ms ± 38.1 ms` | 184.0 ms .. 325.7 ms | 1.9 MB | 30.2 KB | 17.21x |

