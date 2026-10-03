#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release 2>/dev/null

python3 << 'PYEOF'
import subprocess, time, os, sys, re, tempfile, shutil

DATA_DIR = "data"
OUR_BIN = "./target/release/lz77-bench"
ITERS = 3
TOTAL_SIZE = 3466496  # approximate

FILES = [
    "alice29.txt", "asyoulik.txt", "fields.c", "cp.html",
    "kennedy.xls", "plrabn12.txt", "urls.10K", "geo.protodata",
    "fireworks.jpeg", "random_org_10k.bin",
    "franko-ivan-iakovych-farbovanyy-lys652.rtf",
    "franko-ivan-iakovych-zakhar-berkut645.html",
]

TOOLS = {
    "gzip -1":    (lambda f: ["gzip", "-1", "-c", f],    lambda: ["gzip", "-d", "-c"]),
    "gzip -9":    (lambda f: ["gzip", "-9", "-c", f],    lambda: ["gzip", "-d", "-c"]),
    "bzip2 -9":   (lambda f: ["bzip2", "-9", "-c", f],   lambda: ["bzip2", "-d", "-c"]),
    "lz4 -1":     (lambda f: ["lz4", "-1", "-c", f],     lambda: ["lz4", "-d", "-c"]),
    "lz4 -9":     (lambda f: ["lz4", "-9", "-c", f],     lambda: ["lz4", "-d", "-c"]),
    "brotli -6":  (lambda f: ["brotli", "-q", "6", "-c", f],  lambda: ["brotli", "-d", "-c"]),
    "brotli -11": (lambda f: ["brotli", "-q", "11", "-c", f], lambda: ["brotli", "-d", "-c"]),
    "zstd -1":    (lambda f: ["zstd", "-1", "-c", "-q", f],   lambda: ["zstd", "-d", "-c", "-q"]),
    "zstd -19":   (lambda f: ["zstd", "-19", "-c", "-q", f],  lambda: ["zstd", "-d", "-c", "-q"]),
}

def compress_and_time(tool, filepath):
    """Returns (compressed_data, compressed_size, compress_time_ms)"""
    comp_fn, _ = TOOLS[tool]
    r = subprocess.run(comp_fn(filepath), capture_output=True)
    comp_data = r.stdout
    comp_sz = len(comp_data)
    t0 = time.perf_counter()
    for _ in range(ITERS):
        subprocess.run(comp_fn(filepath), capture_output=True)
    elapsed = (time.perf_counter() - t0) / ITERS
    return comp_data, comp_sz, elapsed * 1000

def decompress_and_time(tool, comp_data):
    """Returns decompress_time_ms"""
    _, dec_fn = TOOLS[tool]
    t0 = time.perf_counter()
    for _ in range(ITERS):
        subprocess.run(dec_fn(), input=comp_data, capture_output=True)
    elapsed = (time.perf_counter() - t0) / ITERS
    return elapsed * 1000

# ========================================================================
# Run our compressor
# ========================================================================
our_output = subprocess.run([OUR_BIN], capture_output=True, text=True).stderr + \
             subprocess.run([OUR_BIN], capture_output=True, text=True).stdout
our_lines = our_output.strip().split("\n")

our_results = {}
for line in our_lines:
    m = re.search(r'^(.+?):\s+\d+B\s+->\s+\d+B\s+\(ratio\s+(\d+\.\d+)\)', line)
    if m:
        name = m.group(1).strip()
        ratio = float(m.group(2))
        for fn in FILES:
            if name in fn or fn in name or fn.startswith(name) or name.replace('.', '') in fn.replace('.', ''):
                our_results[fn] = ratio
                break
        else:
            first_word = name.split('.')[0].split('-')[0]
            for fn in FILES:
                if first_word in fn:
                    if fn not in our_results:
                        our_results[fn] = ratio
                        break

our_metrics = {}
for line in our_lines:
    if line.startswith("METRIC "):
        k, v = line[7:].split("=")
        our_metrics[k] = v

# ========================================================================
# TABLE 1: Compression Ratio
# ========================================================================
tool_names = list(TOOLS.keys())
short_names = ["gzip-1", "gzip-9", "bzip2", "lz4-1", "lz4-9", "brotli6", "brotl11", "zstd-1", "zstd19"]

print()
print("=" * 130)
print("  COMPRESSION RATIO (lower = better)")
print("=" * 130)

hdr = f"{'File':<26} {'Size':>8} | {'OURS':>6}"
for sn in short_names:
    hdr += f" {sn:>8}"
print(hdr)
print("-" * 130)

total_orig = 0
tool_totals = {t: 0 for t in tool_names}
tool_comp_data = {}  # store compressed data for decompress test

for fn in FILES:
    filepath = os.path.join(DATA_DIR, fn)
    orig = os.path.getsize(filepath)
    total_orig += orig

    display = fn if len(fn) <= 26 else fn[:23] + "..."
    our_r = our_results.get(fn, 0)

    row = f"{display:<26} {orig:>8} | {our_r:>6.4f}" if our_r else f"{display:<26} {orig:>8} | {'N/A':>6}"

    for tool in tool_names:
        comp_data, comp_sz, _ = compress_and_time(tool, filepath)
        tool_totals[tool] += comp_sz
        if fn not in tool_comp_data:
            tool_comp_data[fn] = {}
        tool_comp_data[fn][tool] = comp_data
        ratio = comp_sz / orig
        row += f" {ratio:>8.4f}"

    print(row)
    sys.stdout.flush()

print("-" * 130)
our_avg = float(our_metrics.get("ratio", "0"))
row = f"{'AVERAGE'::<26} {total_orig:>8} | {our_avg:>6.4f}"
for tool in tool_names:
    ratio = tool_totals[tool] / total_orig
    row += f" {ratio:>8.4f}"
print(row)

# ========================================================================
# TABLE 2: Compress + Decompress Speed
# ========================================================================
print()
print("=" * 100)
print(f"  SPEED COMPARISON (all {len(FILES)} files, avg of {ITERS} runs, {total_orig/1e6:.1f} MB corpus)")
print("=" * 100)
print(f"{'Tool':<14} {'Compress':>10} {'Decomp':>10} {'Total':>10} {'Ratio':>8} {'Comp MB/s':>10} {'Dec MB/s':>10}")
print("-" * 100)

our_comp_ms = float(our_metrics.get("compress_µs", "0")) / 1000
our_decomp_ms = float(our_metrics.get("decompress_µs", "0")) / 1000
our_total_ms = our_comp_ms + our_decomp_ms
our_comp_mbs = total_orig / 1e6 / (our_comp_ms / 1000) if our_comp_ms > 0 else 0
our_dec_mbs = total_orig / 1e6 / (our_decomp_ms / 1000) if our_decomp_ms > 0 else 0
print(f"{'★ OURS':<14} {our_comp_ms:>9.1f}ms {our_decomp_ms:>9.1f}ms {our_total_ms:>9.1f}ms {our_avg:>8.4f} {our_comp_mbs:>9.1f} {our_dec_mbs:>9.1f}")

speed_tools = ["gzip -1", "gzip -9", "bzip2 -9", "lz4 -1", "brotli -6", "brotli -11", "zstd -1", "zstd -19"]
for tool in speed_tools:
    total_comp_ms = 0
    total_dec_ms = 0
    for fn in FILES:
        filepath = os.path.join(DATA_DIR, fn)
        _, _, comp_ms = compress_and_time(tool, filepath)
        total_comp_ms += comp_ms
        comp_data = tool_comp_data[fn][tool]
        dec_ms = decompress_and_time(tool, comp_data)
        total_dec_ms += dec_ms

    ratio = tool_totals[tool] / total_orig
    total_ms = total_comp_ms + total_dec_ms
    comp_mbs = total_orig / 1e6 / (total_comp_ms / 1000) if total_comp_ms > 0 else 0
    dec_mbs = total_orig / 1e6 / (total_dec_ms / 1000) if total_dec_ms > 0 else 0
    print(f"{tool:<14} {total_comp_ms:>9.1f}ms {total_dec_ms:>9.1f}ms {total_ms:>9.1f}ms {ratio:>8.4f} {comp_mbs:>9.1f} {dec_mbs:>9.1f}")

print()
print("Note: External tool times include process spawn overhead (~3ms per invocation).")
print(f"      Our times are pure in-process measurements (no spawn overhead).")

# ========================================================================
# TABLE 3: Winner per file (ratio)
# ========================================================================
print()
print("=" * 90)
print("  WINNER PER FILE — best ratio (excluding incompressible)")
print("=" * 90)
print(f"{'File':<26} {'Best tool':<14} {'Best ratio':>10} {'OURS':>8} {'vs best':>8}")
print("-" * 90)

our_wins = 0
total_compressible = 0
for fn in FILES:
    filepath = os.path.join(DATA_DIR, fn)
    orig = os.path.getsize(filepath)
    our_r = our_results.get(fn, 99)
    if our_r > 0.95:
        continue
    total_compressible += 1

    best_tool = "OURS"
    best_ratio = our_r
    for tool in tool_names:
        ratio = tool_totals.get(tool, 0)  # already counted
        comp_sz = len(tool_comp_data.get(fn, {}).get(tool, b''))
        if comp_sz == 0:
            comp_data, comp_sz, _ = compress_and_time(tool, filepath)
        r = comp_sz / orig
        if r < best_ratio:
            best_ratio = r
            best_tool = tool

    diff = (our_r - best_ratio) / best_ratio * 100 if best_ratio > 0 else 0
    marker = "✓ WIN" if best_tool == "OURS" else f"+{diff:.1f}%"
    if best_tool == "OURS":
        our_wins += 1
    display = fn if len(fn) <= 26 else fn[:23] + "..."
    print(f"{display:<26} {best_tool:<14} {best_ratio:>10.4f} {our_r:>8.4f} {marker:>8}")

print("-" * 90)
print(f"Our wins: {our_wins}/{total_compressible} compressible files")

# ========================================================================
# TABLE 4: vs gzip-9 head-to-head
# ========================================================================
print()
print("=" * 70)
print("  HEAD-TO-HEAD: OURS vs gzip-9")
print("=" * 70)
print(f"{'File':<26} {'OURS':>8} {'gzip-9':>8} {'Delta':>8} {'Winner':>8}")
print("-" * 70)

wins = 0
for fn in FILES:
    filepath = os.path.join(DATA_DIR, fn)
    orig = os.path.getsize(filepath)
    our_r = our_results.get(fn, 99)
    if our_r > 0.95:
        continue
    gz_sz = len(tool_comp_data.get(fn, {}).get("gzip -9", b''))
    gz_r = gz_sz / orig if gz_sz > 0 else 99
    delta = (our_r - gz_r) / gz_r * 100
    winner = "OURS" if our_r < gz_r else "gzip-9"
    if our_r < gz_r:
        wins += 1
    display = fn if len(fn) <= 26 else fn[:23] + "..."
    print(f"{display:<26} {our_r:>8.4f} {gz_r:>8.4f} {delta:>+7.1f}% {'  ←' if our_r < gz_r else ''}")

print("-" * 70)
gz_total = tool_totals.get("gzip -9", 1)
our_total_r = our_avg
gz_total_r = gz_total / total_orig
print(f"{'TOTAL':<26} {our_total_r:>8.4f} {gz_total_r:>8.4f} {(our_total_r-gz_total_r)/gz_total_r*100:>+7.1f}%   OURS wins {wins}/{total_compressible}")

PYEOF
