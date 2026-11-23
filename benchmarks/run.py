#!/usr/bin/env python3
"""
Vela Performance & Resource Benchmark Suite
Compares Vela against C, Rust, Go, and Zig across CPU compute, memory allocation, and concurrency.
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

LANGUAGES = ["c", "rust", "go", "zig", "vela"]

BENCHMARKS = [
    {
        "id": "01_mandelbrot",
        "name": "Mandelbrot (CPU / Tight Loops)",
        "desc": "2000x2000 Mandelbrot escape sum (50 max iterations) - floating-point and loop codegen",
        "files": {
            "c": "mandelbrot.c",
            "rust": "mandelbrot.rs",
            "go": "mandelbrot.go",
            "zig": "mandelbrot.zig",
            "vela": "mandelbrot.vela",
        },
    },
    {
        "id": "02_binary_trees",
        "name": "Binary Trees (Memory / Regions vs GC vs RAII)",
        "desc": "Allocating and traversing millions of nodes across short-lived tree passes (depth 16)",
        "files": {
            "c": "tree.c",
            "rust": "tree.rs",
            "go": "tree.go",
            "zig": "tree.zig",
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
            "zig": "ring.zig",
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


def resolve_toolchains() -> dict[str, str]:
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
    zig = find_binary("ZIG", ["zig", "/usr/local/zig/zig"])
    cc = find_binary("CC", ["gcc", "clang", "cc"])
    gnu_time = find_binary("TIME", ["/usr/bin/time", "time"])
    hyperfine = shutil.which("hyperfine") or ""

    return {
        "vela": vela,
        "rustc": rustc,
        "go": go,
        "zig": zig,
        "cc": cc,
        "time": gnu_time,
        "hyperfine": hyperfine,
    }


def compile_binary(lang: str, src_path: Path, out_path: Path, tools: dict[str, str]) -> str:
    """Compiles a benchmark source file. Returns 'ok', 'skip', or 'fail'."""
    if not src_path.exists():
        return "skip"

    out_path.parent.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()

    if lang == "vela":
        if not tools["vela"] or not tools["cc"]:
            return "skip"
        env["CC"] = f"{tools['cc']} -O3 -march=native -s"
        cmd = [tools["vela"], str(src_path), "-o", str(out_path)]
    elif lang == "c":
        if not tools["cc"]:
            return "skip"
        cmd = [
            tools["cc"],
            "-O3",
            "-march=native",
            "-s",
            "-o",
            str(out_path),
            str(src_path),
            "-lpthread",
            "-lm",
        ]
    elif lang == "rust":
        if not tools["rustc"]:
            return "skip"
        cmd = [
            tools["rustc"],
            "-C",
            "opt-level=3",
            "-C",
            "lto=thin",
            "-C",
            "target-cpu=native",
            "-C",
            "strip=symbols",
            "-o",
            str(out_path),
            str(src_path),
        ]
    elif lang == "go":
        if not tools["go"]:
            return "skip"
        cmd = [
            tools["go"],
            "build",
            "-ldflags=-s -w",
            "-o",
            str(out_path),
            str(src_path),
        ]
    elif lang == "zig":
        if not tools["zig"]:
            return "skip"
        cmd = [
            tools["zig"],
            "build-exe",
            "-O",
            "ReleaseFast",
            "-fstrip",
            f"-femit-bin={out_path}",
            str(src_path),
        ]
    else:
        return "skip"

    res = subprocess.run(cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if res.returncode != 0:
        print(f" fail\n    [!] {src_path.name} ({lang}) compile error:")
        print(res.stderr.decode("utf-8", errors="replace"))
        return "fail"
    return "ok"


def verify_and_measure(bin_path: Path, runs: int, warmup: int, time_bin: str):
    # 1. Sanity run + capture canonical stdout
    sanity = subprocess.run(
        [str(bin_path)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=True,
    )
    stdout_output = sanity.stdout.strip()

    # 2. Warmup runs (no timing)
    for _ in range(max(0, warmup - 1)):
        subprocess.run(
            [str(bin_path)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=True,
        )

    # 3. Clean timed runs (direct binary invocation with zero wrapper overhead)
    times = []
    for _ in range(runs):
        t_start = time.perf_counter()
        subprocess.run(
            [str(bin_path)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=True,
        )
        times.append(time.perf_counter() - t_start)

    # 4. Separate Peak RSS runs via GNU time (isolated from wall-clock timing)
    rss_list = []
    if time_bin and os.path.exists(time_bin):
        for _ in range(min(3, runs)):
            res = subprocess.run(
                [time_bin, "-v", str(bin_path)],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
                check=True,
            )
            for line in res.stderr.splitlines():
                if "Maximum resident set size" in line:
                    match = re.search(r"(\d+)", line)
                    if match:
                        rss_list.append(int(match.group(1)))
                        break

    return {
        "stdout": stdout_output,
        "mean": statistics.mean(times),
        "stddev": statistics.stdev(times) if len(times) > 1 else 0.0,
        "min": min(times),
        "max": max(times),
        "rss_kb": max(rss_list) if rss_list else 0,
        "bin_size_kb": os.path.getsize(bin_path) / 1024.0,
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
        description="Run language benchmarks: Vela vs C, Rust, Go, Zig"
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
    print(f"Vela:    {tools['vela'] or 'not found'}")
    print(f"C (GCC): {tools['cc'] or 'not found'}")
    print(f"Rust:    {tools['rustc'] or 'not found'}")
    print(f"Go:      {tools['go'] or 'not found'}")
    print(f"Zig:     {tools['zig'] or 'not found (will skip)'}")
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
    has_failure = False

    for bench in selected_benches:
        print(f"\n[+] Benchmark: {bench['name']}")
        print(f"    {bench['desc']}")
        b_dir = BENCH_DIR / bench["id"]
        out_b_dir = BUILD_DIR / bench["id"]

        bench_res = {}
        expected_stdout = None

        for lang in LANGUAGES:
            src_file = b_dir / bench["files"][lang]
            out_bin = out_b_dir / f"{bench['id']}_{lang}"

            print(f"  • Compiling {lang:5} ({src_file.name})...", end="", flush=True)
            status = compile_binary(lang, src_file, out_bin, tools)
            if status == "skip":
                print(" skipped (toolchain unavailable)")
                continue
            if status == "fail":
                has_failure = True
                continue
            print(" ok")

            print(f"    Benchmarking {lang}...", end="", flush=True)
            try:
                stats = verify_and_measure(
                    out_bin, args.runs, args.warmup, tools["time"]
                )
                if expected_stdout is None:
                    expected_stdout = stats["stdout"]
                elif stats["stdout"] != expected_stdout:
                    print(f" OUTPUT MISMATCH!")
                    print(f"      Expected:\n{expected_stdout}")
                    print(f"      Actual ({lang}):\n{stats['stdout']}")
                    has_failure = True
                    continue

                bench_res[lang] = stats
                print(
                    f" mean: {format_duration(stats['mean'])} ± {format_duration(stats['stddev'])}, RSS: {stats['rss_kb']} KB"
                )
            except Exception as e:
                print(f" error: {e}")
                has_failure = True

        all_results[bench["id"]] = {
            "meta": bench,
            "results": bench_res,
        }

        if args.hyperfine and tools["hyperfine"]:
            print(f"\n    Running hyperfine comparison for {bench['id']}...")
            bins = [
                str(out_b_dir / f"{bench['id']}_{lang}")
                for lang in LANGUAGES
                if lang in bench_res
            ]
            if bins:
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

    report_path = BENCH_DIR / "RESULTS.md"
    with open(report_path, "w", encoding="utf-8") as f:
        f.write("# Vela Benchmark Results\n\n")
        f.write(
            "Comparison between **Vela**, **C (GCC -O3)**, **Rust (opt-level=3, LTO)**, **Go**, and **Zig (ReleaseFast)**.\n\n"
        )
        f.write(
            f"* Runs: {args.runs} (Warmup: {args.warmup}) | Timing uses direct process execution; Peak RSS measured separately via GNU `time -v`. All binaries stripped for fair size comparison.\n\n"
        )

        for b_id, data in all_results.items():
            meta = data["meta"]
            results = data["results"]
            f.write(f"### {meta['name']}\n\n")
            f.write(f"*{meta['desc']}*\n\n")

            f.write(
                "| Language | Time (Mean ± Stddev) | Min - Max | Peak RSS | Binary Size | Rel. to Fastest |\n"
            )
            f.write("| :--- | :--- | :--- | :--- | :--- | :--- |\n")

            fastest_time = min((r["mean"] for r in results.values()), default=1.0)

            for lang in LANGUAGES:
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

    print("\nSummary Results:")
    for b_id, data in all_results.items():
        print(f"\n--- {data['meta']['name']} ---")
        fastest_time = min((r["mean"] for r in data["results"].values()), default=1.0)
        for lang in LANGUAGES:
            if lang in data["results"]:
                r = data["results"][lang]
                ratio = r["mean"] / fastest_time
                print(
                    f"  {lang.upper():5} | {format_duration(r['mean']):>10} ± {format_duration(r['stddev']):<8} | RSS: {r['rss_kb']:>6} KB | Bin: {r['bin_size_kb']:>7.1f} KB | {ratio:.2f}x"
                )

    if has_failure:
        sys.exit(1)


if __name__ == "__main__":
    main()
