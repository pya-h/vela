#!/usr/bin/env python3
"""
Vela Performance & Resource Benchmark Suite
Compares Vela against C, Rust, Go, and Zig across 7 distinct runtime & web workloads.
Generates both RESULTS.md and an interactive RESULTS.html web dashboard.
"""

import argparse
import json
import os
import re
import shutil
import socket
import statistics
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

ROOT_DIR = Path(__file__).resolve().parent.parent
BENCH_DIR = ROOT_DIR / "benchmarks"
BUILD_DIR = BENCH_DIR / "build"
HTTP_LOAD_SRC = BENCH_DIR / "tools" / "http_load.c"

LANGUAGES = ["c", "rust", "go", "zig", "vela"]

BENCHMARKS = [
    {
        "id": "01_mandelbrot",
        "name": "Mandelbrot",
        "category": "CPU & Tight Loops",
        "kind": "batch",
        "desc": "2000x2000 Mandelbrot escape sum (50 max iters) - floating-point & loop optimization",
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
        "name": "Binary Trees",
        "category": "Memory Lifecycles",
        "kind": "batch",
        "desc": "Millions of short-lived nodes (depth 16) - Region bulk free vs GC vs RAII vs Arena",
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
        "name": "Token Ring",
        "category": "Concurrency & Tasks",
        "kind": "batch",
        "desc": "4 concurrent workers passing a token 100,000 times - channel latency & context switching",
        "files": {
            "c": "ring.c",
            "rust": "ring.rs",
            "go": "ring.go",
            "zig": "ring.zig",
            "vela": "ring.vela",
        },
    },
    {
        "id": "04_quicksort",
        "name": "Quicksort",
        "category": "Contiguous Slices",
        "kind": "batch",
        "desc": "In-place quicksort of 2,500,000 64-bit integers - array indexing, swapping & cache lines",
        "files": {
            "c": "qsort.c",
            "rust": "qsort.rs",
            "go": "qsort.go",
            "zig": "qsort.zig",
            "vela": "qsort.vela",
        },
    },
    {
        "id": "05_prime_sieve",
        "name": "Prime Sieve",
        "category": "Flat Memory Access",
        "kind": "batch",
        "desc": "Sieve of Eratosthenes up to 30,000,000 - flat boolean buffer stride traversal",
        "files": {
            "c": "sieve.c",
            "rust": "sieve.rs",
            "go": "sieve.go",
            "zig": "sieve.zig",
            "vela": "sieve.vela",
        },
    },
    {
        "id": "06_hashmap",
        "name": "Hash Map",
        "category": "Hashing & Lookups",
        "kind": "batch",
        "desc": "5,000,000 random inserts/lookups over 200,000 keys - collision probing & map resizing",
        "files": {
            "c": "hashmap.c",
            "rust": "hashmap.rs",
            "go": "hashmap.go",
            "zig": "hashmap.zig",
            "vela": "hashmap.vela",
        },
    },
    {
        "id": "07_web_server",
        "name": "HTTP Web Server",
        "category": "Networking & Web I/O",
        "kind": "http",
        "port": 18260,
        "threads": 4,
        "requests": 2000,
        "desc": "Minimal concurrent HTTP/1.1 JSON & health endpoints across 4 worker threads (2,000 reqs/run)",
        "files": {
            "c": "web.c",
            "rust": "web.rs",
            "go": "web.go",
            "zig": "web.zig",
            "vela": "web.vela",
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
    zig = find_binary(
        "ZIG",
        [
            str(ROOT_DIR / ".bin" / "zig"),
            "~/.local/bin/zig",
            "/snap/bin/zig",
            "zig",
        ],
    )
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


def compile_binary(
    lang: str, src_path: Path, out_path: Path, tools: dict[str, str]
) -> tuple[str, float]:
    if not src_path.exists():
        return ("skip", 0.0)

    out_path.parent.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()

    if lang == "vela":
        if not tools["vela"] or not tools["cc"]:
            return ("skip", 0.0)
        env["CC"] = f"{tools['cc']} -O3 -march=native -s"
        cmd = [tools["vela"], str(src_path), "-o", str(out_path)]
    elif lang == "c":
        if not tools["cc"]:
            return ("skip", 0.0)
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
            return ("skip", 0.0)
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
            return ("skip", 0.0)
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
            return ("skip", 0.0)
        cmd = [
            tools["zig"],
            "build-exe",
            "-lc",
            "-lpthread",
            "-O",
            "ReleaseFast",
            "-fstrip",
            f"-femit-bin={out_path}",
            str(src_path),
        ]
    else:
        return ("skip", 0.0)

    t0 = time.perf_counter()
    res = subprocess.run(cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    compile_s = time.perf_counter() - t0
    if res.returncode != 0:
        print(f" fail\n    [!] {src_path.name} ({lang}) compile error:")
        print(res.stderr.decode("utf-8", errors="replace"))
        return ("fail", compile_s)
    return ("ok", compile_s)


def parse_gnu_time(stderr_text: str) -> dict:
    info = {"rss_kb": 0, "user_s": 0.0, "sys_s": 0.0, "vol_ctx": 0, "invol_ctx": 0}
    for line in stderr_text.splitlines():
        if "Maximum resident set size" in line:
            m = re.search(r"(\d+)", line)
            if m:
                info["rss_kb"] = int(m.group(1))
        elif "User time (seconds)" in line:
            m = re.search(r"([\d.]+)", line)
            if m:
                info["user_s"] = float(m.group(1))
        elif "System time (seconds)" in line:
            m = re.search(r"([\d.]+)", line)
            if m:
                info["sys_s"] = float(m.group(1))
        elif "Voluntary context switches" in line:
            m = re.search(r"(\d+)", line)
            if m:
                info["vol_ctx"] = int(m.group(1))
        elif "Involuntary context switches" in line:
            m = re.search(r"(\d+)", line)
            if m:
                info["invol_ctx"] = int(m.group(1))
    return info


def verify_and_measure_batch(
    bin_path: Path, runs: int, warmup: int, time_bin: str, compile_s: float
):
    sanity = subprocess.run(
        [str(bin_path)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=True,
    )
    stdout_output = sanity.stdout.strip()

    for _ in range(max(0, warmup - 1)):
        subprocess.run(
            [str(bin_path)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=True,
        )

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

    rss_list, user_list, sys_list, ctx_list = [], [], [], []
    if time_bin and os.path.exists(time_bin):
        for _ in range(min(3, runs)):
            res = subprocess.run(
                [time_bin, "-v", str(bin_path)],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
                check=True,
            )
            parsed = parse_gnu_time(res.stderr)
            if parsed["rss_kb"] > 0:
                rss_list.append(parsed["rss_kb"])
            user_list.append(parsed["user_s"])
            sys_list.append(parsed["sys_s"])
            ctx_list.append(parsed["vol_ctx"] + parsed["invol_ctx"])

    return {
        "stdout": stdout_output,
        "mean": statistics.mean(times),
        "median": statistics.median(times),
        "stddev": statistics.stdev(times) if len(times) > 1 else 0.0,
        "min": min(times),
        "max": max(times),
        "rss_kb": max(rss_list) if rss_list else 0,
        "user_s": statistics.median(user_list) if user_list else 0.0,
        "sys_s": statistics.median(sys_list) if sys_list else 0.0,
        "ctx_switches": int(statistics.median(ctx_list)) if ctx_list else 0,
        "compile_s": compile_s,
        "bin_size_kb": os.path.getsize(bin_path) / 1024.0,
    }


def read_proc_vmhwm_kb(pid: int) -> int:
    try:
        with open(f"/proc/{pid}/status", "r", encoding="utf-8") as f:
            for line in f:
                if line.startswith("VmHWM:"):
                    m = re.search(r"(\d+)", line)
                    if m:
                        return int(m.group(1))
    except OSError:
        pass
    return 0


def wait_port_ready(port: int, timeout_s: float = 5.0) -> bool:
    deadline = time.perf_counter() + timeout_s
    while time.perf_counter() < deadline:
        try:
            with urllib.request.urlopen(
                f"http://127.0.0.1:{port}/health", timeout=0.3
            ) as resp:
                if resp.status == 200 and resp.read().decode().strip() == "ok":
                    return True
        except Exception:
            time.sleep(0.02)
    return False


def wait_port_closed(port: int, timeout_s: float = 3.0):
    deadline = time.perf_counter() + timeout_s
    while time.perf_counter() < deadline:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            s.settimeout(0.1)
            s.connect(("127.0.0.1", port))
            s.close()
            time.sleep(0.02)
        except OSError:
            s.close()
            return


def verify_and_measure_http(
    bin_path: Path,
    http_load_bin: Path,
    port: int,
    threads: int,
    requests_per_run: int,
    runs: int,
    compile_s: float,
):
    wait_port_closed(port)
    proc = subprocess.Popen(
        [str(bin_path)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    try:
        if not wait_port_ready(port):
            err_msg = ""
            if proc.poll() is not None and proc.stderr:
                err_msg = proc.stderr.read().decode("utf-8", errors="replace")
            raise RuntimeError(f"Server {bin_path.name} failed to listen on {port}: {err_msg}")

        # Verify /json parity
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/json", timeout=1.0) as resp:
            json_body = resp.read().decode().strip()
        parsed_json = json.loads(json_body)
        if parsed_json.get("status") != "ok" or parsed_json.get("value") != 42:
            raise RuntimeError(f"Unexpected JSON payload: {json_body}")

        # Warmup burst
        subprocess.run(
            [str(http_load_bin), str(port), str(threads), "200"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            check=True,
        )

        # Timed load runs (cap at 3 runs so ephemeral sockets stay well under 28k)
        load_runs = max(2, min(3, runs))
        elapsed_list, rps_list, p95_list = [], [], []
        for _ in range(load_runs):
            res = subprocess.run(
                [str(http_load_bin), str(port), str(threads), str(requests_per_run)],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                check=True,
            )
            stats = json.loads(res.stdout.strip())
            if stats["fail"] > 0:
                raise RuntimeError(f"HTTP load errors: {stats}")
            elapsed_list.append(stats["elapsed"])
            rps_list.append(stats["rps"])
            p95_list.append(stats["p95_us"] / 1000.0)  # ms

        rss_kb = read_proc_vmhwm_kb(proc.pid)

        return {
            "stdout": json.dumps(parsed_json, sort_keys=True),
            "mean": statistics.mean(elapsed_list),
            "median": statistics.median(elapsed_list),
            "stddev": statistics.stdev(elapsed_list) if len(elapsed_list) > 1 else 0.0,
            "min": min(elapsed_list),
            "max": max(elapsed_list),
            "rps": statistics.median(rps_list),
            "p95_ms": statistics.median(p95_list),
            "rss_kb": rss_kb,
            "user_s": 0.0,
            "sys_s": 0.0,
            "ctx_switches": 0,
            "compile_s": compile_s,
            "bin_size_kb": os.path.getsize(bin_path) / 1024.0,
        }
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=2.0)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        wait_port_closed(port)


def format_duration(seconds: float) -> str:
    if seconds < 0.001:
        return f"{seconds * 1000000:.1f} µs"
    elif seconds < 1.0:
        return f"{seconds * 1000:.1f} ms"
    else:
        return f"{seconds:.3f} s"


def generate_html_report(all_results: dict, runs: int, warmup: int, report_path: Path):
    json_data = json.dumps(all_results, indent=2)

    html_content = f"""<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Vela Benchmark Dashboard | Language Comparison</title>
  <style>
    :root {{
      --bg: #0b0f19;
      --card-bg: #111827;
      --card-border: #1f2937;
      --text: #f3f4f6;
      --text-muted: #9ca3af;
      --accent: #10b981;
      --accent-glow: rgba(16, 185, 129, 0.2);
      --c-color: #3b82f6;
      --rust-color: #f97316;
      --go-color: #06b6d4;
      --zig-color: #eab308;
      --vela-color: #10b981;
    }}
    * {{ box-sizing: border-box; margin: 0; padding: 0; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif; }}
    body {{ background: var(--bg); color: var(--text); padding: 2rem 1.5rem; line-height: 1.5; }}
    .container {{ max-width: 1320px; margin: 0 auto; }}
    
    header {{ display: flex; justify-content: space-between; align-items: center; border-bottom: 1px solid var(--card-border); padding-bottom: 1.5rem; margin-bottom: 2rem; flex-wrap: wrap; gap: 1rem; }}
    h1 {{ font-size: 2rem; font-weight: 800; display: flex; align-items: center; gap: 0.75rem; letter-spacing: -0.025em; }}
    .badge {{ background: var(--accent-glow); color: var(--accent); border: 1px solid var(--accent); font-size: 0.75rem; font-weight: 700; padding: 0.25rem 0.6rem; border-radius: 9999px; text-transform: uppercase; }}
    .subtitle {{ color: var(--text-muted); font-size: 0.95rem; margin-top: 0.35rem; }}
    
    .stats-grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(230px, 1fr)); gap: 1.25rem; margin-bottom: 2rem; }}
    .stat-card {{ background: var(--card-bg); border: 1px solid var(--card-border); border-radius: 1rem; padding: 1.25rem; position: relative; overflow: hidden; }}
    .stat-card::after {{ content: ''; position: absolute; top: 0; left: 0; width: 4px; height: 100%; background: var(--card-color, var(--accent)); }}
    .stat-label {{ font-size: 0.75rem; font-weight: 700; text-transform: uppercase; color: var(--text-muted); letter-spacing: 0.05em; }}
    .stat-val {{ font-size: 1.45rem; font-weight: 800; margin-top: 0.35rem; }}
    .stat-sub {{ font-size: 0.8rem; color: var(--text-muted); margin-top: 0.25rem; }}

    .radar-section {{ background: var(--card-bg); border: 1px solid var(--card-border); border-radius: 1rem; padding: 1.5rem; margin-bottom: 2rem; display: grid; grid-template-columns: 380px 1fr; gap: 2rem; align-items: center; }}
    @media (max-width: 900px) {{ .radar-section {{ grid-template-columns: 1fr; }} }}
    .radar-wrap {{ display: flex; justify-content: center; align-items: center; }}
    .radar-legend {{ display: flex; flex-wrap: wrap; gap: 1rem; margin-top: 1rem; }}
    .legend-item {{ display: flex; align-items: center; gap: 0.5rem; font-size: 0.875rem; font-weight: 700; cursor: pointer; padding: 0.35rem 0.75rem; border-radius: 6px; background: rgba(255,255,255,0.03); border: 1px solid var(--card-border); }}

    .toolbar {{ display: flex; justify-content: space-between; align-items: center; background: var(--card-bg); border: 1px solid var(--card-border); border-radius: 0.75rem; padding: 0.5rem 0.75rem; margin-bottom: 2rem; flex-wrap: wrap; gap: 0.75rem; }}
    .tab-group {{ display: flex; gap: 0.5rem; flex-wrap: wrap; }}
    .btn {{ background: transparent; border: 1px solid transparent; color: var(--text-muted); padding: 0.5rem 0.9rem; border-radius: 0.5rem; font-size: 0.85rem; font-weight: 600; cursor: pointer; transition: all 0.2s ease; }}
    .btn:hover {{ color: var(--text); background: rgba(255,255,255,0.05); }}
    .btn.active {{ color: var(--text); background: #1f2937; border-color: #374151; }}

    .bench-section {{ background: var(--card-bg); border: 1px solid var(--card-border); border-radius: 1rem; padding: 1.5rem; margin-bottom: 2rem; }}
    .bench-header {{ display: flex; justify-content: space-between; align-items: baseline; margin-bottom: 1.5rem; border-bottom: 1px solid rgba(255,255,255,0.06); padding-bottom: 1rem; flex-wrap: wrap; gap: 0.5rem; }}
    .bench-title {{ font-size: 1.35rem; font-weight: 700; }}
    .bench-desc {{ font-size: 0.875rem; color: var(--text-muted); }}

    .bars-container {{ display: flex; flex-direction: column; gap: 0.85rem; margin-bottom: 1.5rem; }}
    .bar-row {{ display: grid; grid-template-columns: 95px 1fr 180px; align-items: center; gap: 1rem; }}
    .bar-label {{ font-size: 0.9rem; font-weight: 700; display: flex; align-items: center; gap: 0.5rem; }}
    .lang-dot {{ width: 10px; height: 10px; border-radius: 50%; display: inline-block; }}
    .bar-track {{ background: rgba(255,255,255,0.04); height: 32px; border-radius: 6px; overflow: hidden; position: relative; border: 1px solid rgba(255,255,255,0.05); }}
    .bar-fill {{ height: 100%; border-radius: 5px; transition: width 0.5s cubic-bezier(0.16, 1, 0.3, 1); display: flex; align-items: center; padding: 0 0.75rem; font-size: 0.8rem; font-weight: 700; color: #fff; min-width: 56px; text-shadow: 0 1px 2px rgba(0,0,0,0.5); white-space: nowrap; }}
    .bar-meta {{ font-size: 0.85rem; font-weight: 600; text-align: right; color: var(--text-muted); font-variant-numeric: tabular-nums; }}

    .table-wrap {{ overflow-x: auto; }}
    table {{ width: 100%; border-collapse: collapse; font-size: 0.84rem; margin-top: 1rem; }}
    th, td {{ padding: 0.7rem 0.85rem; text-align: left; border-bottom: 1px solid var(--card-border); white-space: nowrap; }}
    th {{ color: var(--text-muted); font-weight: 600; text-transform: uppercase; font-size: 0.72rem; letter-spacing: 0.05em; }}
    tr:last-child td {{ border-bottom: none; }}
    .winner-tag {{ background: rgba(16, 185, 129, 0.15); color: #34d399; padding: 0.15rem 0.45rem; border-radius: 4px; font-weight: 700; font-size: 0.7rem; margin-left: 0.4rem; }}

    footer {{ text-align: center; color: var(--text-muted); font-size: 0.85rem; padding-top: 2rem; border-top: 1px solid var(--card-border); margin-top: 3rem; }}
  </style>
</head>
<body>
  <div class="container">
    <header>
      <div>
        <h1>Vela Performance Dashboard <span class="badge">Vela vs C, Rust, Go, Zig</span></h1>
        <div class="subtitle">Cross-language runtime, memory, concurrency, compilation, and HTTP throughput metrics ({runs} timed runs, {warmup} warmups).</div>
      </div>
      <div>
        <span class="badge" style="background: rgba(59, 130, 246, 0.15); color: #60a5fa; border-color: #3b82f6;">Automated Output Parity Checked</span>
      </div>
    </header>

    <div class="stats-grid" id="kpi-cards"></div>

    <div class="radar-section">
      <div class="radar-wrap" id="radar-container"></div>
      <div>
        <h2 style="font-size: 1.25rem; font-weight: 700; margin-bottom: 0.5rem;">Multi-Dimensional Language Profile</h2>
        <p style="color: var(--text-muted); font-size: 0.9rem; margin-bottom: 1rem;">
          Normalized radar shape across all benchmark dimensions (outer edge = 100% fastest/smallest in category). Highlights architectural tradeoffs between region allocation, tracing GC, green threads, and ahead-of-time systems codegen.
        </p>
        <div class="radar-legend" id="radar-legend"></div>
      </div>
    </div>

    <div class="toolbar">
      <div class="tab-group" id="metric-tabs">
        <button class="btn active" onclick="setMetric('median')">⏱️ Median Speed</button>
        <button class="btn" onclick="setMetric('mean')">📊 Mean Speed</button>
        <button class="btn" onclick="setMetric('rss')">🧠 Peak RSS Memory</button>
        <button class="btn" onclick="setMetric('size')">📦 Binary Size</button>
        <button class="btn" onclick="setMetric('compile')">⚡ Compile Time</button>
      </div>
      <div class="tab-group">
        <button class="btn active" id="btn-ratio" onclick="toggleRatio(true)">Relative Ratio (x)</button>
        <button class="btn" id="btn-raw" onclick="toggleRatio(false)">Absolute Values</button>
      </div>
    </div>

    <div id="benchmarks-container"></div>

    <footer>
      Generated dynamically by <code>benchmarks/run.py</code>. Direct execution timing via monotonic clock; Peak RSS via GNU Time / Linux <code>/proc/pid/status</code> VmHWM.
    </footer>
  </div>

  <script>
    const DATA = {json_data};
    const COLORS = {{
      c: "#3b82f6",
      rust: "#f97316",
      go: "#06b6d4",
      zig: "#eab308",
      vela: "#10b981"
    }};
    const LABELS = {{
      c: "C (GCC)",
      rust: "Rust",
      go: "Go",
      zig: "Zig",
      vela: "Vela"
    }};
    let currentMetric = 'median';
    let showRatio = true;

    function formatTime(s) {{
      if (s < 0.001) return (s * 1000000).toFixed(1) + " µs";
      if (s < 1.0) return (s * 1000).toFixed(1) + " ms";
      return s.toFixed(2) + " s";
    }}

    function formatRss(kb) {{
      if (kb < 1024) return kb + " KB";
      return (kb / 1024).toFixed(1) + " MB";
    }}

    function formatSize(kb) {{
      if (kb < 1024) return kb.toFixed(1) + " KB";
      return (kb / 1024).toFixed(2) + " MB";
    }}

    function buildKpiCards() {{
      const grid = document.getElementById('kpi-cards');
      grid.innerHTML = '';
      const cards = [];

      // 1. Binary Trees winner
      if (DATA['02_binary_trees']) {{
        const res = DATA['02_binary_trees'].results;
        const sorted = Object.entries(res).sort((a, b) => a[1].median - b[1].median);
        if (sorted.length > 0) {{
          const [wLang, wStats] = sorted[0];
          const second = sorted[1];
          const goStats = res['go'];
          const sub = goStats
            ? `${{(goStats.median / wStats.median).toFixed(1)}}x faster than Go GC (${{formatRss(wStats.rss_kb)}} RSS)`
            : `${{(second[1].median / wStats.median).toFixed(2)}}x faster than ${{second[0].toUpperCase()}}`;
          cards.push({{
            label: "Memory Lifecycle Winner",
            val: `${{LABELS[wLang]}} (${{formatTime(wStats.median)}})`,
            sub,
            color: COLORS[wLang]
          }});
        }}
      }}

      // 2. Web Server winner
      if (DATA['07_web_server']) {{
        const res = DATA['07_web_server'].results;
        const sorted = Object.entries(res).sort((a, b) => (b[1].rps || 0) - (a[1].rps || 0));
        if (sorted.length > 0) {{
          const [wLang, wStats] = sorted[0];
          const velaStats = res['vela'];
          const sub = velaStats
            ? `Vela std/web: ${{Math.round(velaStats.rps).toLocaleString()}} req/s (${{formatSize(velaStats.bin_size_kb)}} bin)`
            : `Peak throughput: ${{Math.round(wStats.rps).toLocaleString()}} req/s`;
          cards.push({{
            label: "Web Server Throughput Winner",
            val: `${{LABELS[wLang]}} (${{Math.round(wStats.rps).toLocaleString()}} req/s)`,
            sub,
            color: COLORS[wLang]
          }});
        }}
      }}

      // 3. Concurrency winner
      if (DATA['03_concurrency']) {{
        const res = DATA['03_concurrency'].results;
        const sorted = Object.entries(res).sort((a, b) => a[1].median - b[1].median);
        if (sorted.length > 0) {{
          const [wLang, wStats] = sorted[0];
          const second = sorted[1];
          const sub = second ? `${{(second[1].median / wStats.median).toFixed(1)}}x faster than ${{second[0].toUpperCase()}}` : "";
          cards.push({{
            label: "Channel Switching Winner",
            val: `${{LABELS[wLang]}} (${{formatTime(wStats.median)}})`,
            sub,
            color: COLORS[wLang]
          }});
        }}
      }}

      // 4. Average binary size leader
      const langSizes = {{}};
      for (const bData of Object.values(DATA)) {{
        for (const [l, r] of Object.entries(bData.results)) {{
          langSizes[l] = (langSizes[l] || []);
          langSizes[l].push(r.bin_size_kb);
        }}
      }}
      const avgSizes = Object.entries(langSizes).map(([l, arr]) => [l, arr.reduce((a, b) => a + b, 0) / arr.length]);
      avgSizes.sort((a, b) => a[1] - b[1]);
      if (avgSizes.length > 0) {{
        const [sLang, sKb] = avgSizes[0];
        const velaAvg = avgSizes.find(x => x[0] === 'vela');
        cards.push({{
          label: "Smallest Avg Binary",
          val: `${{LABELS[sLang]}} (${{formatSize(sKb)}})`,
          sub: velaAvg && sLang !== 'vela' ? `Vela avg: ${{formatSize(velaAvg[1])}} (stripped ELF)` : `Stripped zero-runtime footprint`,
          color: COLORS[sLang]
        }});
      }}

      cards.forEach(c => {{
        const div = document.createElement('div');
        div.className = 'stat-card';
        div.style.setProperty('--card-color', c.color);
        div.innerHTML = `
          <div class="stat-label">${{c.label}}</div>
          <div class="stat-val" style="color:${{c.color}}">${{c.val}}</div>
          <div class="stat-sub">${{c.sub}}</div>
        `;
        grid.appendChild(div);
      }});
    }}

    function buildRadar() {{
      const axes = [
        {{ key: '01_mandelbrot', label: 'CPU Loops', extract: r => 1 / r.median }},
        {{ key: '02_binary_trees', label: 'Memory Mgmt', extract: r => 1 / r.median }},
        {{ key: '04_quicksort', label: 'Slice Sort', extract: r => 1 / r.median }},
        {{ key: '06_hashmap', label: 'Hash Map', extract: r => 1 / r.median }},
        {{ key: '07_web_server', label: 'Web RPS', extract: r => r.rps || (1 / r.median) }},
        {{ key: '01_mandelbrot', label: 'Small Binary', extract: r => 1 / Math.log2(r.bin_size_kb + 2) }},
      ].filter(a => DATA[a.key]);

      const langs = ['vela', 'c', 'rust', 'go', 'zig'].filter(l =>
        axes.every(a => DATA[a.key].results[l])
      );

      const maxPerAxis = axes.map(a =>
        Math.max(...langs.map(l => a.extract(DATA[a.key].results[l])))
      );

      const size = 340, cx = size / 2, cy = size / 2, radius = 115;
      const n = axes.length;
      let svg = `<svg width="${{size}}" height="${{size}}" viewBox="0 0 ${{size}} ${{size}}">`;

      // Concentric rings
      [0.25, 0.5, 0.75, 1.0].forEach(level => {{
        let pts = [];
        for (let i = 0; i < n; i++) {{
          const ang = (Math.PI * 2 * i / n) - Math.PI / 2;
          pts.push(`${{cx + radius * level * Math.cos(ang)}},${{cy + radius * level * Math.sin(ang)}}`);
        }}
        svg += `<polygon points="${{pts.join(' ')}}" fill="none" stroke="rgba(255,255,255,0.09)" stroke-width="1"/>`;
      }});

      // Axis lines & labels
      for (let i = 0; i < n; i++) {{
        const ang = (Math.PI * 2 * i / n) - Math.PI / 2;
        const x2 = cx + radius * Math.cos(ang);
        const y2 = cy + radius * Math.sin(ang);
        svg += `<line x1="${{cx}}" y1="${{cy}}" x2="${{x2}}" y2="${{y2}}" stroke="rgba(255,255,255,0.1)" stroke-width="1"/>`;
        const lx = cx + (radius + 26) * Math.cos(ang);
        const ly = cy + (radius + 22) * Math.sin(ang);
        svg += `<text x="${{lx}}" y="${{ly}}" fill="#9ca3af" font-size="11" font-weight="600" text-anchor="middle" dominant-baseline="middle">${{axes[i].label}}</text>`;
      }}

      // Language polygons
      langs.slice().reverse().forEach(l => {{
        let pts = [];
        for (let i = 0; i < n; i++) {{
          const ang = (Math.PI * 2 * i / n) - Math.PI / 2;
          const score = Math.max(0.12, axes[i].extract(DATA[axes[i].key].results[l]) / maxPerAxis[i]);
          pts.push(`${{cx + radius * score * Math.cos(ang)}},${{cy + radius * score * Math.sin(ang)}}`);
        }}
        const strokeWidth = l === 'vela' ? 2.5 : 1.75;
        const fillOp = l === 'vela' ? 0.22 : 0.08;
        svg += `<polygon points="${{pts.join(' ')}}" fill="${{COLORS[l]}}" fill-opacity="${{fillOp}}" stroke="${{COLORS[l]}}" stroke-width="${{strokeWidth}}"/>`;
      }});
      svg += `</svg>`;
      document.getElementById('radar-container').innerHTML = svg;

      const legend = document.getElementById('radar-legend');
      legend.innerHTML = '';
      langs.forEach(l => {{
        const div = document.createElement('div');
        div.className = 'legend-item';
        div.innerHTML = `<span class="lang-dot" style="background:${{COLORS[l]}}"></span><span>${{LABELS[l]}}</span>`;
        legend.appendChild(div);
      }});
    }}

    function setMetric(m) {{
      currentMetric = m;
      const map = ['median', 'mean', 'rss', 'size', 'compile'];
      document.querySelectorAll('#metric-tabs .btn').forEach((b, i) => {{
        b.classList.toggle('active', map[i] === m);
      }});
      render();
    }}

    function toggleRatio(r) {{
      showRatio = r;
      document.getElementById('btn-ratio').classList.toggle('active', r);
      document.getElementById('btn-raw').classList.toggle('active', !r);
      render();
    }}

    function getMetricValue(r, isHttp) {{
      if (currentMetric === 'median') return isHttp && r.rps ? r.rps : r.median;
      if (currentMetric === 'mean') return isHttp && r.rps ? r.rps : r.mean;
      if (currentMetric === 'rss') return r.rss_kb;
      if (currentMetric === 'size') return r.bin_size_kb;
      return r.compile_s;
    }}

    function render() {{
      const container = document.getElementById('benchmarks-container');
      container.innerHTML = '';

      for (const [bId, bData] of Object.entries(DATA)) {{
        const meta = bData.meta;
        const res = bData.results;
        const isHttp = meta.kind === 'http';
        const higherIsBetter = isHttp && (currentMetric === 'median' || currentMetric === 'mean');

        const section = document.createElement('div');
        section.className = 'bench-section';

        let hint = higherIsBetter
          ? 'Higher req/s is better'
          : (currentMetric === 'rss' ? 'Lower memory is better' : currentMetric === 'size' ? 'Smaller binary is better' : currentMetric === 'compile' ? 'Faster compile is better' : 'Lower time is faster');

        section.innerHTML = `
          <div class="bench-header">
            <div>
              <div class="bench-title">${{meta.name}} <span style="font-size:0.85rem; font-weight:normal; color:var(--text-muted);">&bull; ${{meta.category}}</span></div>
              <div class="bench-desc">${{meta.desc}}</div>
            </div>
            <div style="font-size:0.8rem; font-weight:700; color:var(--text-muted);">${{hint}}</div>
          </div>
          <div class="bars-container" id="bars-${{bId}}"></div>
          <div class="table-wrap">
            <table>
              <thead>
                <tr>
                  <th>Language</th>
                  <th>${{isHttp ? 'Throughput (RPS)' : 'Median Time'}}</th>
                  <th>${{isHttp ? 'P95 Latency' : 'Mean ± Stddev'}}</th>
                  <th>${{isHttp ? '2000 Reqs Time' : 'CPU (User + Sys)'}}</th>
                  <th>Peak RSS</th>
                  <th>Binary Size</th>
                  <th>Compile Time</th>
                  <th>Relative</th>
                </tr>
              </thead>
              <tbody id="table-${{bId}}"></tbody>
            </table>
          </div>
        `;
        container.appendChild(section);

        const barsContainer = section.querySelector(`#bars-${{bId}}`);
        const tableBody = section.querySelector(`#table-${{bId}}`);

        const vals = Object.entries(res).map(([lang, r]) => ({{
          lang,
          val: getMetricValue(r, isHttp),
          raw: r
        }}));

        const bestVal = higherIsBetter
          ? Math.max(...vals.map(v => v.val))
          : Math.min(...vals.map(v => v.val));

        vals.forEach(item => {{
          const isWinner = item.val === bestVal;
          const ratio = higherIsBetter ? (bestVal / item.val) : (item.val / bestVal);
          const pct = higherIsBetter
            ? Math.max(8, (item.val / bestVal) * 100)
            : Math.max(8, (bestVal / item.val) * 100);

          let displayVal = "";
          if (higherIsBetter) displayVal = `${{Math.round(item.raw.rps).toLocaleString()}} req/s`;
          else if (currentMetric === 'median') displayVal = formatTime(item.raw.median);
          else if (currentMetric === 'mean') displayVal = formatTime(item.raw.mean);
          else if (currentMetric === 'rss') displayVal = formatRss(item.raw.rss_kb);
          else if (currentMetric === 'size') displayVal = formatSize(item.raw.bin_size_kb);
          else displayVal = formatTime(item.raw.compile_s);

          const barRow = document.createElement('div');
          barRow.className = 'bar-row';
          barRow.innerHTML = `
            <div class="bar-label">
              <span class="lang-dot" style="background:${{COLORS[item.lang] || '#fff'}}"></span>
              ${{item.lang.toUpperCase()}}
            </div>
            <div class="bar-track">
              <div class="bar-fill" style="width:${{pct}}%; background:${{COLORS[item.lang] || '#fff'}};">
                ${{showRatio ? (ratio <= 1.005 ? '1.00x (best)' : ratio.toFixed(2) + 'x') : displayVal}}
              </div>
            </div>
            <div class="bar-meta">${{displayVal}}</div>
          `;
          barsContainer.appendChild(barRow);

          const tr = document.createElement('tr');
          const col1 = isHttp ? `<strong>${{Math.round(item.raw.rps).toLocaleString()}} req/s</strong>` : `<code>${{formatTime(item.raw.median)}}</code>`;
          const col2 = isHttp ? `${{item.raw.p95_ms.toFixed(2)}} ms` : `<code>${{formatTime(item.raw.mean)}} &plusmn; ${{formatTime(item.raw.stddev)}}</code>`;
          const col3 = isHttp ? formatTime(item.raw.median) : `${{(item.raw.user_s + item.raw.sys_s).toFixed(3)}}s (${{item.raw.user_s.toFixed(2)}}u + ${{item.raw.sys_s.toFixed(2)}}s)`;

          tr.innerHTML = `
            <td style="font-weight:700; color:${{COLORS[item.lang]}};">
              ${{item.lang.toUpperCase()}}${{isWinner ? '<span class="winner-tag">BEST</span>' : ''}}
            </td>
            <td>${{col1}}</td>
            <td style="color:var(--text-muted);">${{col2}}</td>
            <td style="color:var(--text-muted);">${{col3}}</td>
            <td>${{formatRss(item.raw.rss_kb)}}</td>
            <td>${{formatSize(item.raw.bin_size_kb)}}</td>
            <td style="color:var(--text-muted);">${{formatTime(item.raw.compile_s)}}</td>
            <td><strong>${{ratio <= 1.005 ? '1.00x' : ratio.toFixed(2) + 'x'}}</strong></td>
          `;
          tableBody.appendChild(tr);
        }});
      }}
    }}

    buildKpiCards();
    buildRadar();
    render();
  </script>
</body>
</html>
"""
    with open(report_path, "w", encoding="utf-8") as f:
        f.write(html_content)


def main():
    parser = argparse.ArgumentParser(
        description="Run language benchmarks: Vela vs C, Rust, Go, Zig"
    )
    parser.add_argument(
        "--runs", type=int, default=7, help="Number of timed runs (default: 7)"
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

    http_load_bin = BUILD_DIR / "http_load"
    if HTTP_LOAD_SRC.exists() and (not http_load_bin.exists() or args.clean):
        subprocess.run(
            [tools["cc"], "-O3", "-s", str(HTTP_LOAD_SRC), "-o", str(http_load_bin), "-lpthread"],
            check=True,
        )

    print("=" * 84)
    print(" Vela Comparative Benchmark Suite (7 Workloads x 5 Languages)")
    print("=" * 84)
    print(f"Vela:    {tools['vela'] or 'not found'}")
    print(f"C (GCC): {tools['cc'] or 'not found'}")
    print(f"Rust:    {tools['rustc'] or 'not found'}")
    print(f"Go:      {tools['go'] or 'not found'}")
    print(f"Zig:     {tools['zig'] or 'not found'}")
    print(f"Runs:    {args.runs} (Warmup: {args.warmup})")
    print("=" * 84)

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
        print(f"\n[+] Benchmark: {bench['name']} ({bench['category']})")
        print(f"    {bench['desc']}")
        b_dir = BENCH_DIR / bench["id"]
        out_b_dir = BUILD_DIR / bench["id"]

        bench_res = {}
        expected_stdout = None

        for lang in LANGUAGES:
            src_file = b_dir / bench["files"][lang]
            out_bin = out_b_dir / f"{bench['id']}_{lang}"

            print(f"  • Compiling {lang:5} ({src_file.name})...", end="", flush=True)
            status, compile_s = compile_binary(lang, src_file, out_bin, tools)
            if status == "skip":
                print(" skipped (toolchain unavailable)")
                continue
            if status == "fail":
                has_failure = True
                continue
            print(f" ok ({format_duration(compile_s)})")

            print(f"    Benchmarking {lang}...", end="", flush=True)
            try:
                if bench["kind"] == "http":
                    stats = verify_and_measure_http(
                        out_bin,
                        http_load_bin,
                        bench["port"],
                        bench["threads"],
                        bench["requests"],
                        args.runs,
                        compile_s,
                    )
                else:
                    stats = verify_and_measure_batch(
                        out_bin, args.runs, args.warmup, tools["time"], compile_s
                    )

                if expected_stdout is None:
                    expected_stdout = stats["stdout"]
                elif stats["stdout"] != expected_stdout:
                    print(" OUTPUT MISMATCH!")
                    print(f"      Expected:\n{expected_stdout}")
                    print(f"      Actual ({lang}):\n{stats['stdout']}")
                    has_failure = True
                    continue

                bench_res[lang] = stats
                if bench["kind"] == "http":
                    print(
                        f" {stats['rps']:.0f} req/s (p95: {stats['p95_ms']:.2f} ms), RSS: {stats['rss_kb']} KB"
                    )
                else:
                    print(
                        f" median: {format_duration(stats['median'])} (mean: {format_duration(stats['mean'])} ± {format_duration(stats['stddev'])}), RSS: {stats['rss_kb']} KB"
                    )
            except Exception as e:
                print(f" error: {e}")
                has_failure = True

        all_results[bench["id"]] = {
            "meta": bench,
            "results": bench_res,
        }

        if args.hyperfine and tools["hyperfine"] and bench["kind"] == "batch":
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

    # Output Markdown Report
    report_path_md = BENCH_DIR / "RESULTS.md"
    with open(report_path_md, "w", encoding="utf-8") as f:
        f.write("# Vela Benchmark Results\n\n")
        f.write(
            "Comparison between **Vela**, **C (GCC -O3)**, **Rust (opt-level=3, LTO)**, **Go (1.26)**, and **Zig (ReleaseFast)**.\n\n"
        )
        f.write(
            f"* Runs: {args.runs} (Warmup: {args.warmup}) | Direct timing via `time.perf_counter()`; Peak RSS via GNU `time -v` / `/proc/pid/status` `VmHWM`. All binaries stripped.\n\n"
        )

        for b_id, data in all_results.items():
            meta = data["meta"]
            results = data["results"]
            is_http = meta["kind"] == "http"
            f.write(f"### {meta['name']} ({meta['category']})\n\n")
            f.write(f"*{meta['desc']}*\n\n")

            if is_http:
                f.write(
                    "| Language | Throughput (RPS) | P95 Latency | 2000 Reqs Time | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |\n"
                )
                f.write("| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |\n")
                best_rps = max((r.get("rps", 0.0) for r in results.values()), default=1.0)
                for lang in LANGUAGES:
                    if lang not in results:
                        continue
                    r = results[lang]
                    ratio = best_rps / r["rps"]
                    ratio_str = "1.00x (fastest)" if ratio <= 1.005 else f"{ratio:.2f}x"
                    rss_str = f"{r['rss_kb']} KB" if r["rss_kb"] < 1024 else f"{r['rss_kb']/1024.0:.1f} MB"
                    size_str = f"{r['bin_size_kb']:.1f} KB" if r["bin_size_kb"] < 1024 else f"{r['bin_size_kb']/1024.0:.2f} MB"
                    f.write(
                        f"| **{lang.upper()}** | **{r['rps']:,.0f} req/s** | {r['p95_ms']:.2f} ms | {format_duration(r['median'])} | {rss_str} | {size_str} | {format_duration(r['compile_s'])} | {ratio_str} |\n"
                    )
            else:
                f.write(
                    "| Language | Median Time | Mean ± Stddev | CPU (User + Sys) | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |\n"
                )
                f.write("| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |\n")
                fastest_med = min((r["median"] for r in results.values()), default=1.0)
                for lang in LANGUAGES:
                    if lang not in results:
                        continue
                    r = results[lang]
                    ratio = r["median"] / fastest_med
                    ratio_str = "1.00x (fastest)" if ratio <= 1.005 else f"{ratio:.2f}x"
                    mean_str = f"{format_duration(r['mean'])} ± {format_duration(r['stddev'])}"
                    cpu_str = f"{r['user_s'] + r['sys_s']:.3f}s"
                    rss_str = f"{r['rss_kb']} KB" if r["rss_kb"] < 1024 else f"{r['rss_kb']/1024.0:.1f} MB"
                    size_str = f"{r['bin_size_kb']:.1f} KB" if r["bin_size_kb"] < 1024 else f"{r['bin_size_kb']/1024.0:.2f} MB"
                    f.write(
                        f"| **{lang.upper()}** | `{format_duration(r['median'])}` | `{mean_str}` | {cpu_str} | {rss_str} | {size_str} | {format_duration(r['compile_s'])} | {ratio_str} |\n"
                    )
            f.write("\n")

    report_path_html = BENCH_DIR / "RESULTS.html"
    generate_html_report(all_results, args.runs, args.warmup, report_path_html)

    print("\n" + "=" * 84)
    print(f"Markdown Results saved to: {report_path_md}")
    print(f"HTML Web App saved to:     {report_path_html}")
    print("=" * 84)

    if has_failure:
        sys.exit(1)


if __name__ == "__main__":
    main()
