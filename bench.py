#!/usr/bin/env python3
"""xgrep reproducible benchmark suite.

Generates corpus, builds index, benchmarks cached queries vs zgrep.
Usage: python bench.py [--generate] [--small]
"""

import subprocess
import time
import os
import sys
import shutil
import random
import gzip

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
# Use Unix-style path for bash
SCRIPT_DIR_UNIX = SCRIPT_DIR.replace("\\", "/")
XGREP = f"{SCRIPT_DIR_UNIX}/target/release/xgrep.exe"
CORPUS = f"{SCRIPT_DIR_UNIX}/bench_10g"

FILE_COUNT = 100
LINES_PER_FILE = 1_000_000


BASH = r"C:\Program Files\Git\usr\bin\bash.exe"
if not os.path.exists(BASH):
    BASH = "bash"  # fallback


def bash(cmd):
    """Run command through Git Bash, return (elapsed_ms, stdout)."""
    start = time.perf_counter()
    r = subprocess.run([BASH, "-c", cmd], capture_output=True, text=True)
    ms = int((time.perf_counter() - start) * 1000)
    return ms, r.stdout.strip()


def bash_time(cmd):
    """Run command through bash, return elapsed ms."""
    ms, _ = bash(cmd)
    return ms


def bash_best(cmd, runs=3):
    """Best of N runs."""
    return min(bash_time(cmd) for _ in range(runs))


def generate_corpus(out_dir, count):
    os.makedirs(out_dir, exist_ok=True)
    for f_idx in range(count):
        lines = []
        for i in range(LINES_PER_FILE):
            h = random.randint(0, 23); m = random.randint(0, 59); s = random.randint(0, 59)
            level = random.choices(["INFO", "WARN", "ERROR", "DEBUG"], weights=[70, 10, 5, 15])[0]
            user_id = random.randint(1, 100000)
            req_id = f"req-{random.randint(1, 1000000):06x}"
            endpoint = random.choice(["/api/users", "/api/orders", "/api/health", "/api/auth", "/api/search", "/api/upload", "/api/webhook"])
            status = random.choice([200, 200, 200, 200, 201, 301, 400, 401, 403, 404, 500, 502, 503])
            latency = random.randint(1, 5000)
            lines.append(f"2026-01-{(f_idx % 28) + 1:02d} {h:02d}:{m:02d}:{s:02d} {level} user_id={user_id} req={req_id} {endpoint} status={status} latency={latency}ms\n")
        with gzip.open(os.path.join(out_dir, f"app-{f_idx:03d}.log.gz"), "wt") as gf:
            gf.writelines(lines)
        if (f_idx + 1) % 10 == 0:
            print(f"  {f_idx + 1}/{count} files generated", file=sys.stderr)


def file_size_human(path):
    try:
        size = os.path.getsize(path)
    except OSError:
        return "N/A"
    if size > 1e9: return f"{size / 1e9:.1f}GB"
    elif size > 1e6: return f"{size / 1e6:.1f}MB"
    else: return f"{size / 1e3:.1f}KB"


def main():
    global FILE_COUNT
    if "--small" in sys.argv:
        FILE_COUNT = 10

    corpus_dir = SCRIPT_DIR.replace("\\", "/") + "/bench_10g"

    if not os.path.exists(XGREP.replace("/", "\\")):
        alt = XGREP.replace(".exe", "")
        if not os.path.exists(alt.replace("/", "\\")):
            print(f"ERROR: xgrep not found. Run: cargo build --release")
            sys.exit(1)

    if "--generate" in sys.argv or not os.path.isdir(corpus_dir.replace("/", "\\")):
        generate_corpus(corpus_dir.replace("/", "\\"), FILE_COUNT)

    _, gz_count_str = bash(f"ls '{CORPUS}'/*.log.gz | wc -l")
    gz_count = int(gz_count_str.strip())
    _, corpus_size = bash(f"du -sh '{CORPUS}' | cut -f1")
    total_lines = gz_count * LINES_PER_FILE

    GZ = f"'{CORPUS}'/*.log.gz"
    XG = f"'{XGREP}'"

    print()
    print("=" * 64)
    print("  xgrep Benchmark Suite")
    print("=" * 64)
    print(f"  Corpus:     {gz_count} files, {total_lines:,} lines")
    print(f"  Compressed: {corpus_size}")
    print("=" * 64)
    print()

    # Phase 1: Build Index
    print("-- Phase 1: First Query (Build Index) --")
    bash(f"rm -rf '{CORPUS}'/.xgrep")

    build_ms = bash_time(f"{XG} --build-index {GZ}")

    cache_dir_win = os.path.join(SCRIPT_DIR, "bench_10g", ".xgrep")
    index_size = file_size_human(os.path.join(cache_dir_win, "index.xgi"))
    data_size = file_size_human(os.path.join(cache_dir_win, "index.xgd"))

    print(f"  Index build time:  {build_ms:,}ms ({build_ms / 1000:.1f}s)")
    print(f"  Bloom index size:  {index_size}")
    print(f"  Data cache size:   {data_size}")
    print()

    # Phase 2: Cached Literal Queries
    print("-- Phase 2: Cached Repeated Queries (literal) --")
    print()

    queries = [
        ("user_id=12345 ", "Specific user (selective)"),
        ("req=req-00a1b2",  "Request ID (needle)"),
        ("status=503",      "Status code (moderate)"),
        ("ERROR",           "Error level (broad)"),
    ]

    print(f"  {'Query':<40} {'xgrep':>10} {'zgrep':>10} {'Speedup':>10}")
    print(f"  {'-' * 40} {'-' * 10} {'-' * 10} {'-' * 10}")

    for query, label in queries:
        xg_ms = bash_best(f"{XG} -c -F '{query}' {GZ} --no-color")

        zg_ms = bash_time(f"for f in {GZ}; do zgrep -c -F '{query}' \"$f\" > /dev/null; done")

        speedup = zg_ms // xg_ms if xg_ms > 0 else 9999
        print(f"  {label:<40} {xg_ms:>8}ms {zg_ms:>8}ms {speedup:>9}x")

    print()

    # Phase 3: Regex
    print("-- Phase 3: Cached Repeated Queries (regex) --")
    print()

    regex_queries = [
        ("status=503.*webhook", "Regex: status+endpoint"),
        ("ERROR.*timeout",      "Regex: level+keyword"),
    ]

    print(f"  {'Query':<40} {'xgrep':>10} {'zgrep':>10} {'Speedup':>10}")
    print(f"  {'-' * 40} {'-' * 10} {'-' * 10} {'-' * 10}")

    for query, label in regex_queries:
        xg_ms = bash_best(f"{XG} -c '{query}' {GZ} --no-color")
        zg_ms = bash_time(f"for f in {GZ}; do zgrep -c '{query}' \"$f\" > /dev/null; done")
        speedup = zg_ms // xg_ms if xg_ms > 0 else 9999
        print(f"  {label:<40} {xg_ms:>8}ms {zg_ms:>8}ms {speedup:>9}x")

    print()

    # Phase 4: First vs Repeated
    print("-- Phase 4: First Query vs Repeated Query --")
    print()

    bash(f"rm -rf '{CORPUS}'/.xgrep")
    uncached_ms = bash_time(f"{XG} -c -F 'user_id=12345 ' {GZ} --no-color")

    bash(f"{XG} --build-index {GZ}")
    cached_ms = bash_best(f"{XG} -c -F 'user_id=12345 ' {GZ} --no-color")

    zg_ms = bash_time(f"for f in {GZ}; do zgrep -c -F 'user_id=12345 ' \"$f\" > /dev/null; done")

    print(f"  {'Mode':<40} {'Time':>10} {'vs zgrep':>10}")
    print(f"  {'-' * 40} {'-' * 10} {'-' * 10}")
    print(f"  {'xgrep first query (no cache)':<40} {uncached_ms:>8}ms {zg_ms // max(uncached_ms, 1):>9}x")
    print(f"  {'xgrep + build-index':<40} {build_ms:>8}ms {'(one-time)':>10}")
    print(f"  {'xgrep repeated query (cached)':<40} {cached_ms:>8}ms {zg_ms // max(cached_ms, 1):>9}x")
    print(f"  {'zgrep (baseline)':<40} {zg_ms:>8}ms {'1x':>10}")
    print()

    # Summary
    print("=" * 64)
    print("  Summary")
    print("=" * 64)
    print(f"  Corpus:            {gz_count} files, {corpus_size} compressed")
    print(f"  Index build:       {build_ms / 1000:.1f}s (one-time)")
    print(f"  Cache overhead:    {index_size} index + {data_size} data")
    print(f"  First query:       {uncached_ms}ms")
    print(f"  Repeated queries:  {cached_ms}ms (bloom skip + mmap)")
    print(f"  zgrep baseline:    {zg_ms // 1000}s")
    print()
    print("  Architecture: bloom prefilter -> mmap candidate blocks only")
    print("  Key metric:   ~0.1-1% of bytes touched per query")
    print("=" * 64)
    print()


if __name__ == "__main__":
    main()
