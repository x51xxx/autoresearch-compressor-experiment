# Autoresearch Compressor Experiment

**Can an AI agent, left alone in an edit → benchmark → keep/revert loop, turn a textbook LZ77 into a competitive compressor?**

This repository is the result: a pure-Rust LZ77 compressor evolved by an AI agent over ~190 autonomous experiments.
It started at ratio **0.754** (greedy LZ77, 32 KB window) and now reaches **0.209** on the test corpus, beating
gzip-9, bzip2, zstd-19, xz and brotli-11.

📖 **Read the full story:** [How We Built a Compressor That Beats gzip-9: An Autoresearch Experiment](https://trishchuk.com/blog/autoresearch-compressor/)
· in this repo: [EXPERIMENT.md](EXPERIMENT.md) (English) · [EXPERIMENT.uk.md](EXPERIMENT.uk.md) (Українською)

---

## The experiment

[autoresearch](https://github.com/karpathy/autoresearch) (Andrej Karpathy, March 2026) is a loop in which an AI agent
edits code, runs a benchmark, keeps improvements, reverts failures, and repeats. The original demo optimized LLM
training. This experiment checks whether the same loop works on a classic engineering problem: data compression.

The setup:

- **Starting point:** a bare LZ77 in Rust, with hash-chain matching, greedy parsing and a naive byte format.
- **Agent:** Claude Code running autoresearch as an MCP server. GPT and Gemini were attached as additional MCP
  servers for brainstorming and code review.
- **Rules:** no compression crates in the compressor itself, the format may change freely, no benchmark tricks, and
  every kept change must pass all roundtrip tests.
- **Corpus:** 12 files, 3.46 MB. Canterbury/LibDeflate files (English prose, C source, HTML, Excel, protobuf, URLs, a
  JPEG, random bytes) plus two works by Ivan Franko in Ukrainian (Cyrillic RTF and HTML).
- **Metric:** `ratio = compressed / original` over the whole corpus (lower is better). Compression and decompression
  times are recorded alongside it.

```
         ┌──────────────────────────────────────────────────┐
         ▼                                                  │
  hypothesis ──► edit src/ ──► ./autoresearch.sh ──► ./autoresearch.checks.sh
                                 (ratio, µs)          (cargo test roundtrips)
                                                            │
                     better ──► git commit (keep) ──────────┤
                     worse  ──► git checkout -- src (discard)┘
                     every run ──► autoresearch.jsonl
```

## Results

| | Ratio (12-file corpus) | |
|---|---:|---|
| Baseline: greedy LZ77, 32 KB window | 0.754 | |
| End of the sessions covered in [the article](https://trishchuk.com/blog/autoresearch-compressor/) | 0.221 | −27% vs gzip-9 |
| **Current best: wave 4** | **0.2088** | beats every reference below |
| gzip -9 | 0.303 | |
| brotli -11 | 0.2167 | |
| xz / LZMA (raw, lc=3) | 0.2156 | |

**Wave 4** happened after the article was published. Its main addition is an LZMA-style adaptive binary range coder
with a logistic-mixing literal model, plus DP passes priced by that backend. Those changes took the ratio from 0.2209
to 0.2088. Compressing the full corpus takes about 5.5 s and decompressing it about 108 ms. The agent's running
notes and the current architecture are in [autoresearch.md](autoresearch.md).

### How the ratio got there

| Stage | Ratio | Technique |
|---|---:|---|
| Baseline | 0.754 | Greedy LZ77, hash chains |
| Compact format | 0.452 | Literal runs, variable-length match encoding |
| Huffman | 0.337 | DEFLATE-style literal/length/distance alphabet |
| Optimal parsing | 0.306 | Forward DP with iterative Huffman pricing |
| Context modeling | 0.288 | Order-1 Huffman tables, adaptive selection |
| Rep-offsets | 0.274 | REP0–REP3 distances |
| Larger window | 0.253 | Dynamic window up to 256 KB |
| REP tracking in DP | 0.231 | DP tracks recent distances |
| 8 MB window, rANS, suffix array, block DP | 0.221 | Fractional-bit coding; enwik8 went from 40 min to 76 s |
| LZMA-style range coder | 0.2185 | Adaptive binary coder, 12-state machine |
| Literal mixing | 0.2137 | lpaq-style logistic mixer over order-0…3 models |
| LZMA-priced DP | **0.2088** | Per-position literal prices, state-dependent match prices |

Most experiments were discarded. Eight changes produced about 95% of the improvement, and window size mattered more
than algorithmic cleverness. The article covers what worked, what didn't, and what this says about autoresearch
itself.

## What's inside

```
src/
  lib.rs            match finding, optimal-parse DP, container format, try-all backends
  lzma_coder.rs     LZMA-style range-coder backend + literal mixer (wave 4)
  huffman.rs        canonical Huffman (12-bit lookup decode)
  rans.rs           rANS backend
  range_coder.rs    binary range coder
  suffix_array.rs   suffix-array match finder (files < 1 MB)
  context.rs        byte-class contexts
  codes.rs          length/distance code tables
  rle.rs            nibble-packed RLE for table headers
  checksum.rs       xxhash32 integrity check
  main.rs           lz77-bench: corpus benchmark that prints METRIC lines
  bin/              fair_bench, enwik8_bench, entropy_compare, bench_window
data/               test corpus (enwik8 is downloaded separately)
autoresearch.sh         benchmark step of the loop
autoresearch.checks.sh  correctness gate (cargo test)
autoresearch.md         agent's living notes: objective, architecture, dead ends
autoresearch.ideas.md   idea backlog that survives context resets
benchmark.sh            comparison against gzip / bzip2 / lz4 / brotli / zstd CLIs
```

The compressor has no compression-crate dependencies. `flate2`, `brotli`, `zstd` and `lz4_flex` in `Cargo.toml`
are used only by the comparison benchmarks.

## Quick start

```bash
cargo build --release

./autoresearch.sh          # benchmark the 12-file corpus → METRIC ratio=…
./autoresearch.checks.sh   # roundtrip tests (cargo test --release)
./benchmark.sh             # compare with system gzip, bzip2, lz4, brotli, zstd
```

`benchmark.sh` needs Python 3 and the `gzip`, `bzip2`, `lz4`, `brotli` and `zstd` CLIs on `PATH`.

### enwik8

enwik8 is the first 100 MB of English Wikipedia (Hutter Prize dataset). It is not committed. To run the large-file
benchmark:

```bash
curl -O https://mattmahoney.net/dc/enwik8.zip
unzip enwik8.zip -d data/ && rm enwik8.zip
cargo run --release --bin enwik8_bench
```

## Run your own loop

1. Write `autoresearch.sh` so it prints `METRIC <name>=<value>`. Write `autoresearch.checks.sh` so it fails on any
   regression in correctness.
2. Give the agent an objective and a keep rule. For wave 4 the rule was: ratio must improve, and compression time may
   grow by at most 25%.
3. Have the agent keep `autoresearch.md` and `autoresearch.ideas.md` up to date. These files let a fresh context resume
   the work after the previous one fills up.

See [the article](https://trishchuk.com/blog/autoresearch-compressor/) for lessons learned: context resets, getting
ideas from several models, and bugs that tests can't catch, such as infinite loops and running out of memory.
