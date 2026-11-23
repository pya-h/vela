#!/usr/bin/env python3
"""
Vela Performance & Resource Benchmark Suite
Compares Vela against C, Rust, and Go across CPU compute, memory allocation, and concurrency.
"""

import argparse
import os
import re
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT_DIR = Path(__file__).resolve().parent.parent
BENCH_DIR = ROOT_DIR / "benchmarks"
BUILD_DIR = BENCH_DIR / "build"

BENCHMARKS = [
    {
        "id": "01_fibonacci",
        "name": "Fibonacci (CPU / Recursion)",
        "desc": "Recursive fib(40) - pure call stack overhead and integer arithmetic",
        "files": {
            "c": "fib.c",
            "rust": "fib.rs",
            "go": "fib.go",
            "vela": "fib.vela",
        },
    },
    {
        "id": "02_binary_trees",
        "name": "Binary Trees (Memory / Regions vs GC)",
        "desc": "Allocating and traversing millions of nodes across short-lived tree passes",
        "files": {
            "c": "tree.c",
            "rust": "tree.rs",
            "go": "tree.go",
            "vela": "tree.vela",
        },
    },
    {
        "id": "03_concurrency",
        "name": "Token Ring (Channels / Task Switching)",
        "desc": "4 concurrent workers in a ring passing a token 100,000 times",
        "files": {
            "c": "ring.c",
            "rust": "ring.rs",
            "go": "ring.go",
            "vela": "ring.vela",
        },
    },
]


def find_binary(name: str, candidates: list[str]) -> str:
    env_val = os.environ.get(name.upper())
    if env_val and os.path.exists(env_val):
        return env_val
    for cand in candidates:
        expanded = os.path.expanduser(cand)
        if os.path.exists(expanded) and os.access(expanded, os.X_OK):
            return expanded
        which_path = shutil.which(cand)
        if which_path:
            return which_path
    return ""


def resolve_toolchains():
    vela = find_binary(
        "VELA",
        [
            str(ROOT_DIR / "target" / "release" / "vela"),
            str(ROOT_DIR / "target" / "debug" / "vela"),
            "vela",
        ],
    )
    if not vela or not os.path.exists(vela):
        print("Building vela compiler in release mode...")
        subprocess.run(
            ["cargo", "build", "--release"],
            cwd=str(ROOT_DIR),
            check=True,
            stdout=subprocess.DEVNULL,
        )
        vela = str(ROOT_DIR / "target" / "release" / "vela")

    rustc = find_binary(
        "RUSTC",
        [
            "~/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/rustc",
            "~/.cargo/bin/rustc",
            "rustc",
        ],
    )
    go = find_binary("GO", ["/usr/local/go/bin/go", "go"])
    cc = find_binary("CC", ["gcc", "clang", "cc"])
    gnu_time = find_binary("TIME", ["/usr/bin/time", "time"])
    hyperfine = shutil.which("hyperfine")

    return {
        "vela": vela,
        "rustc": rustc,
        "go": go,
        "cc": cc,
        "time": gnu_time,
        "hyperfine": hyperfine,
    }


def compile_binary(lang: str, src_path: Path, out_path: Path, tools: dict) -> bool:
    out_path.parent.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()

    if lang == "vela":
        if not tools["vela"]:
            print("  [x] vela compiler not found")
            return False
        env["CC"] = f"{tools['cc']} -O3 -march=native"
        cmd = [tools["vela"], str(src_path), "-o", str(out_path)]
    elif lang == "c":
        if not tools["cc"]:
            print("  [x] C compiler not found")
            return False
        cmd = [
            tools["cc"],
            "-O3",
            "-march=native",
            "-o",
            str(out_path),
            str(src_path),
            "-lpthread",
            "-lm",
        ]
    elif lang == "rust":
        if not tools["rustc"]:
            print("  [x] rustc not found")
            return False
        cmd = [
            tools["rustc"],
            "-C",
            "opt-level=3",
            "-C",
            "lto=thin",
            "-C",
            "target-cpu=native",
            "-o",
            str(out_path),
            str(src_path),
        ]
    elif lang == "go":
        if not tools["go"]:
            print("  [x] go toolchain not found")
            return False
        cmd = [
            tools["go"],
            "build",
            "-ldflags=-s -w",
            "-o",
            str(out_path),
            str(src_path),
        ]
    else:
        return False

    res = subprocess.run(cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if res.returncode != 0:
        print(f"  [x] Failed to compile {src_path.name} ({lang}):")
        print(res.stderr.decode("utf-8", errors="replace"))
        return False
    return True


def run_benchmark_target(bin_path: Path, runs: int, warmup: int, time_bin: str):
    # Warmup
    for _ in range(warmup):
        subprocess.run(
            [str(bin_path)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=True,
        )

    times = []
    rss_list = []

    for _ in range(runs):
        if time_bin and os.path.exists(time_bin):
            t_start = time.perf_counter()
            res = subprocess.run(
                [time_bin, "-v", str(bin_path)],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            t_elapsed = time.perf_counter() - t_start

            if res.returncode != 0:
                raise RuntimeError(f"Execution error on {bin_path}: {res.stderr}")

            # Parse Maximum resident set size (kbytes)
            rss = 0
            for line in res.stderr.splitlines():
                if "Maximum resident set size" in line:
                    match = re.search(r"(\d+)", line)
                    if match:
                        rss = int(match.group(1))
                        break
            times.append(t_elapsed)
            if rss > 0:
                rss_list.append(rss)
        else:
            t_start = time.perf_counter()
            subprocess.run(
                [str(bin_path)],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=True,
            )
            t_elapsed = time.perf_counter() - t_start
            times.append(t_elapsed)

    mean_time = statistics.mean(times)
    stddev_time = statistics.stdev(times) if len(times) > 1 else 0.0
    min_time = min(times)
    max_time = max(times)
    max_rss = max(rss_list) if rss_list else 0
    bin_size_kb = os.path.getsize(bin_path) / 1024.0

    return {
        "mean": mean_time,
        "stddev": stddev_time,
        "min": min_time,
        "max": max_time,
        "rss_kb": max_rss,
        "bin_size_kb": bin_size_kb,
    }


def format_duration(seconds: float) -> str:
    if seconds < 0.001:
        return f"{seconds * 1000000:.1f} µs"
    elif seconds < 1.0:
        return f"{seconds * 1000:.1f} ms"
    else:
        return f"{seconds:.3f} s"


def main():
    parser = argparse.ArgumentParser(
        description="Run language benchmarks: Vela vs C, Rust, Go"
    )
    parser.add_argument(
        "--runs", type=int, default=5, help="Number of timed runs (default: 5)"
    )
    parser.add_argument(
        "--warmup", type=int, default=2, help="Number of warmup runs (default: 2)"
    )
    parser.add_argument(
        "--filter", type=str, default="", help="Filter benchmarks by id or name substring"
    )
    parser.add_argument(
        "--clean", action="store_true", help="Clean build directory before running"
    )
    parser.add_argument(
        "--hyperfine",
        action="store_true",
        help="Also run hyperfine if available on system",
    )
    args = parser.parse_args()

    if args.clean and BUILD_DIR.exists():
        shutil.rmtree(BUILD_DIR)

    BUILD_DIR.mkdir(parents=True, exist_ok=True)
    tools = resolve_toolchains()

    print("=" * 80)
    print(" Vela Comparative Benchmark Suite")
    print("=" * 80)
    print(f"Vela:    {tools['vela']}")
    print(f"C (GCC): {tools['cc']}")
    print(f"Rust:    {tools['rustc']}")
    print(f"Go:      {tools['go']}")
    print(f"Runs:    {args.runs} (Warmup: {args.warmup})")
    print("=" * 80)

    selected_benches = [
        b
        for b in BENCHMARKS
        if not args.filter
        or args.filter.lower() in b["id"].lower()
        or args.filter.lower() in b["name"].lower()
    ]

    all_results = {}

    for bench in selected_benches:
        print(f"\n[+] Benchmark: {bench['name']}")
        print(f"    {bench['desc']}")
        b_dir = BENCH_DIR / bench["id"]
        out_b_dir = BUILD_DIR / bench["id"]

        bench_res = {}
        for lang in ["c", "rust", "go", "vela"]:
            src_file = b_dir / bench["files"][lang]
            out_bin = out_b_dir / f"{bench['id']}_{lang}"

            print(f"  • Compiling {lang:5} ({src_file.name})...", end="", flush=True)
            if not compile_binary(lang, src_file, out_bin, tools):
                continue
            print(" ok")

            print(f"    Benchmarking {lang}...", end="", flush=True)
            try:
                stats = run_benchmark_target(
                    out_bin, args.runs, args.warmup, tools["time"]
                )
                bench_res[lang] = stats
                print(
                    f" mean: {format_duration(stats['mean'])} ± {format_duration(stats['stddev'])}, RSS: {stats['rss_kb']} KB"
                )
            except Exception as e:
                print(f" error: {e}")

        all_results[bench["id"]] = {
            "meta": bench,
            "results": bench_res,
        }

        if args.hyperfine and tools["hyperfine"]:
            print(f"\n    Running hyperfine comparison for {bench['id']}...")
            bins = [
                str(out_b_dir / f"{bench['id']}_{lang}")
                for lang in ["c", "rust", "go", "vela"]
                if (out_b_dir / f"{bench['id']}_{lang}").exists()
            ]
            subprocess.run(
                [
                    tools["hyperfine"],
                    "--warmup",
                    str(args.warmup),
                    "--runs",
                    str(args.runs),
                ]
                + bins
            )

    # Output Markdown Report
    report_path = BENCH_DIR / "RESULTS.md"
    with open(report_path, "w", encoding="utf-8") as f:
        f.write("# Vela Benchmark Results\n\n")
        f.write(
            f"Comparison between **Vela**, **C (GCC -O3)**, **Rust (opt-level=3, LTO)**, and **Go**.\n\n"
        )
        f.write(
            f"* Runs: {args.runs} (Warmup: {args.warmup}) | Measured with high-resolution timers and GNU `time` for Peak RSS.\n\n"
        )

        for b_id, data in all_results.items():
            meta = data["meta"]
            results = data["results"]
            f.write(f"### {meta['name']}\n\n")
            f.write(f"*{meta['desc']}*\n\n")

            f.write(
                "| Language | Time (Mean ± Stddev) | Min - Max | Peak RSS | Binary Size | Rel. to Fastest |\n"
            )
            f.write(
                "| :--- | :--- | :--- | :--- | :--- | :--- |\n"
            )

            fastest_time = min((r["mean"] for r in results.values()), default=1.0)

            for lang in ["c", "rust", "go", "vela"]:
                if lang not in results:
                    continue
                r = results[lang]
                ratio = r["mean"] / fastest_time
                ratio_str = "1.00x (fastest)" if ratio <= 1.005 else f"{ratio:.2f}x"
                mean_str = f"{format_duration(r['mean'])} ± {format_duration(r['stddev'])}"
                range_str = (
                    f"{format_duration(r['min'])} .. {format_duration(r['max'])}"
                )
                rss_str = (
                    f"{r['rss_kb']} KB"
                    if r["rss_kb"] < 1024
                    else f"{r['rss_kb']/1024.0:.1f} MB"
                )
                size_str = (
                    f"{r['bin_size_kb']:.1f} KB"
                    if r["bin_size_kb"] < 1024
                    else f"{r['bin_size_kb']/1024.0:.2f} MB"
                )

                f.write(
                    f"| **{lang.upper()}** | `{mean_str}` | {range_str} | {rss_str} | {size_str} | {ratio_str} |\n"
                )
            f.write("\n")

    print("\n" + "=" * 80)
    print(f"Results successfully saved to: {report_path}")
    print("=" * 80)

    # Print summary table to stdout
    print("\nSummary Results:")
    for b_id, data in all_results.items():
        print(f"\n--- {data['meta']['name']} ---")
        fastest_time = min((r["mean"] for r in data["results"].values()), default=1.0)
        for lang in ["c", "rust", "go", "vela"]:
            if lang in data["results"]:
                r = data["results"][lang]
                ratio = r["mean"] / fastest_time
                print(
                    f"  {lang.upper():5} | {format_duration(r['mean']):>10} ± {format_duration(r['stddev']):<8} | RSS: {r['rss_kb']:>6} KB | Bin: {r['bin_size_kb']:>7.1f} KB | {ratio:.2f}x"
                )


if __name__ == "__main__":
    main()
