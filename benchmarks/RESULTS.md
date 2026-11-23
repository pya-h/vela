# Vela Benchmark Results

Comparison between **Vela**, **C (GCC -O3)**, **Rust (opt-level=3, LTO)**, and **Go**.

* Runs: 5 (Warmup: 2) | Measured with high-resolution timers and GNU `time` for Peak RSS.

### Fibonacci (CPU / Recursion)

*Recursive fib(40) - pure call stack overhead and integer arithmetic*

| Language | Time (Mean ± Stddev) | Min - Max | Peak RSS | Binary Size | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `111.4 ms ± 4.6 ms` | 106.1 ms .. 118.1 ms | 1.8 MB | 15.6 KB | 35.05x |
| **RUST** | `211.1 ms ± 5.8 ms` | 206.3 ms .. 221.1 ms | 2.2 MB | 2.08 MB | 66.38x |
| **GO** | `377.7 ms ± 18.9 ms` | 362.4 ms .. 409.3 ms | 2.1 MB | 1.52 MB | 118.77x |
| **VELA** | `3.2 ms ± 1.1 ms` | 2.0 ms .. 4.7 ms | 1.8 MB | 16.3 KB | 1.00x (fastest) |

### Binary Trees (Memory / Regions vs GC)

*Allocating and traversing millions of nodes across short-lived tree passes*

| Language | Time (Mean ± Stddev) | Min - Max | Peak RSS | Binary Size | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `244.0 ms ± 2.9 ms` | 239.7 ms .. 247.8 ms | 9.6 MB | 27.9 KB | 2.18x |
| **RUST** | `270.4 ms ± 6.4 ms` | 262.2 ms .. 279.9 ms | 10.2 MB | 2.09 MB | 2.42x |
| **GO** | `429.6 ms ± 59.6 ms` | 364.0 ms .. 506.7 ms | 18.9 MB | 1.52 MB | 3.84x |
| **VELA** | `111.9 ms ± 6.0 ms` | 104.2 ms .. 118.4 ms | 13.8 MB | 24.6 KB | 1.00x (fastest) |

### Token Ring (Channels / Task Switching)

*4 concurrent workers in a ring passing a token 100,000 times*

| Language | Time (Mean ± Stddev) | Min - Max | Peak RSS | Binary Size | Rel. to Fastest |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **C** | `312.9 ms ± 92.0 ms` | 268.1 ms .. 477.4 ms | 1.6 MB | 16.1 KB | 18.23x |
| **RUST** | `253.5 ms ± 16.7 ms` | 231.6 ms .. 275.2 ms | 2.3 MB | 2.27 MB | 14.77x |
| **GO** | `17.2 ms ± 864.4 µs` | 16.0 ms .. 18.1 ms | 2.3 MB | 1.52 MB | 1.00x (fastest) |
| **VELA** | `263.2 ms ± 6.3 ms` | 255.2 ms .. 270.9 ms | 2.0 MB | 33.6 KB | 15.33x |

