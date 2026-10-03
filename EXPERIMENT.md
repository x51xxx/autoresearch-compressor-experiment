# How We Built a Compressor That Beats gzip-9: An Autoresearch Experiment

*An AI agent ran ~155 autonomous experiments over four sessions, evolving a naive [LZ77](https://en.wikipedia.org/wiki/LZ77_and_LZ78) implementation into a compressor with
an [rANS](https://en.wikipedia.org/wiki/Asymmetric_numeral_systems) entropy coder, [context modeling](https://en.wikipedia.org/wiki/Context_mixing), and optimal parsing — beating [gzip](https://en.wikipedia.org/wiki/Gzip)-9 by 28%, [zstd](https://en.wikipedia.org/wiki/Zstd)-19 and [bzip2](https://en.wikipedia.org/wiki/Bzip2),
and coming within 0.9% of [brotli](https://en.wikipedia.org/wiki/Brotli)-11.*

## The Idea

Andrej Karpathy released [autoresearch](https://github.com/karpathy/autoresearch) in March 2026 — a loop where an AI agent
edits code, runs benchmarks, keeps improvements, reverts failures, and repeats. His demo optimized LLM training
on a single GPU. The concept is simple: automate the scientific method while you sleep.

I wanted to test whether this works beyond ML. I adapted autoresearch as an MCP server for Claude Code and set the challenge: **start with a bare LZ77 compressor in Rust, let the agent optimize it autonomously, see how far it goes.**

The rules are simple: no external dependencies, all tests must pass, the compression format can be changed freely, no cheating on benchmarks.

## Experiment Setup

We created `experiment/` with a fresh git repository, a Rust project, and two files:

- `src/lib.rs` — compressor (compression + decompression)
- `src/main.rs` — benchmark harness

For test data, we took 12 files from the [LibDeflate](https://github.com/SafeteeWoW/LibDeflate) corpus and added two
Ukrainian literature texts by Ivan Franko — 3.46 MB total: English prose, source code, HTML, binary Excel files,
protobuf, JPEG, random data, and Cyrillic text.

The benchmark script (`autoresearch.sh`) compiled in release mode, compressed each file 5 times, and output
`METRIC ratio=X.XXXXXX`. A separate `autoresearch.checks.sh` ran `cargo test --release` after each successful
benchmark to guarantee roundtrip correctness on all 16 test files.

## Phase 1: Pure LZ77 Speed Optimization (Experiments 1-22)

Starting point — textbook [LZ77](https://en.wikipedia.org/wiki/LZ77_and_LZ78): hash-chain match finding, 32 KB sliding window, greedy matching, and the simplest
format — `0x00 <byte>` for literals, `0x01 <length> <distance>` for matches.

**Baseline: ratio 0.754, total time 19.8ms.**

The primary metric was speed (total microseconds for compression + decompression). The agent iterated:

1. **Tuning MAX_CHAIN.** The most powerful lever. Reducing hash chain length from 256 to 32 cut compression time
   by 60% — from 12.6ms to 4.9ms. The chain was the dominant bottleneck: each position scanned up to 256 candidates
   byte-by-byte. At length 32, most good matches were still found. Reducing to 16 was too aggressive — ratio
   degraded enough to slow decompression.

2. **Unsafe everywhere.** Replacing bounds-checked array accesses with `get_unchecked`, using `ptr::read_unaligned` for
   8-byte match comparison and `copy_nonoverlapping` for copying in the decompressor. Each change gave 1-5%, but
   they accumulated.

3. **Better hash.** Switching from shift-XOR to a 4-byte multiplicative hash
   (`v.wrapping_mul(0x1E35A7BD) >> (32 - HASH_BITS)`). Fewer collisions meant shorter effective chains, better matches, and
   a 6% speedup.

4. **Incompressible region detection.** After 128 consecutive positions without matches — stop inserting into the hash table. JPEG
   compression sped up from 1264 to 350 microseconds.

Many ideas failed: LTO (zero effect on a single crate), `target-cpu=native` (Apple Silicon is already well
optimized), `Box<[u32]>` instead of `Vec<u32>` (worse), prefetch intrinsics (unstable on stable Rust), XOR-shift
hash (terrible distribution — kennedy.xls doubled in time).

**Result after 22 experiments: ratio 0.754 (unchanged), time 3.5ms (5.6x faster).**

## Phase 2: Compact Format — Ratio Breakthrough (Experiments 22-25)

Speed plateaued. The agent noticed that binary data was *expanding* — each literal cost 2 bytes in the naive
format. Incompressible JPEG grew from 123 KB to 246 KB.

The solution — new encoding:

- `0x01..0x7F` = literal sequence (N bytes follow directly)
- `0x80..0xBF` = short match (2 bytes: length 3-6, distance 0-4095)
- `0xC0..0xFE` = medium match (3 bytes: length 3-65)
- `0xFF` = long match (4 bytes: length 3-258)

**Ratio dropped from 0.754 to 0.452 — 40% smaller output.** JPEG barely expanded (1.007x). The format change was the single
largest ratio improvement in the entire experiment.

## Phase 3: [Huffman](https://en.wikipedia.org/wiki/Huffman_coding) Coding — Becoming DEFLATE (Experiments 27-35)

The compact format encoded literals as raw bytes (8 bits each). But in English text, 'e' appears far more
often than 'z'. [Huffman coding](https://en.wikipedia.org/wiki/Huffman_coding) assigns shorter codes to more frequent symbols.

First attempt: separate Huffman for literals + fixed width for matches. Ratio improved, but speed degraded
due to per-token bitwise tags.

Breakthrough: **[DEFLATE](https://en.wikipedia.org/wiki/Deflate)-style unified alphabet.** Symbols 0-255 = literals, 256 = end of block, 257-285 = match lengths.
One Huffman tree for everything — no bitwise tags. Distances got their own tree. Essentially what gzip does.

The Huffman implementation proved non-trivial. The first version had an infinite loop in the [Kraft inequality](https://en.wikipedia.org/wiki/Kraft%E2%80%93McMillan_inequality) fix —
`bl_count[MAX_BITS] -= 1` could underflow to `u32::MAX` when the counter was already zero, causing the loop
to diverge. Fixed with a simple check.

**Ratio: from 0.452 to 0.337. Another 25% smaller.**

MAX_CHAIN was retuned: with Huffman, shorter chains became acceptable because the entropy coder compensated for worse
matches. Reduced from 32 to 8 — compression time came back from 28ms to 24ms.

## Phase 4: Optimal Parsing — The Dynamic Programming Revolution (Experiments 36-47)

Greedy matching (always take the longest match) is fast but suboptimal. Imagine: a 4-byte match at position `i` may
prevent a 20-byte match at position `i+1`. Better to emit one literal and take the longer match.

The agent implemented **forward [dynamic programming](https://en.wikipedia.org/wiki/Dynamic_programming)**: at each position, evaluate the cost (in Huffman bits) of emitting a
literal versus each possible match length. Walk forward through the input, maintaining minimum-cost arrivals.
At the end — backtrack to obtain the optimal token sequence.

Two key refinements:

1. **Iterative pricing.** The first DP pass uses static prices (literal = 8 bits, match = 11+extra).
   Build Huffman from the result, then rerun DP with actual Huffman code lengths as prices. Two passes
   converge; a third adds only 0.02%.

2. **Multi-length candidates.** Not just the longest match and MIN_MATCH — try lengths at each DEFLATE length code
   boundary (3, 4, 6, 10, 19, 35, 67, max). DP finds the optimal split.

**Ratio: from 0.337 to 0.306.** We crossed the gzip-9 line (0.303 on the original 10-file corpus).

DP was slow (100ms vs 19ms greedy) due to cost + backtrack array allocation for the entire input. Merging
match finding with DP in a single pass eliminated temporary arrays. MAX_CHAIN went back to 32 — with DP, better matches from longer
chains justified the cost.

## Phase 5: Context Modeling and Tuning (Experiments 48-57)

Studying [Brotli](https://en.wikipedia.org/wiki/Brotli) and [PAQ](https://en.wikipedia.org/wiki/PAQ) revealed the next frontier: **context modeling.** After a space, the next byte is almost
certainly a lowercase letter. After 'q' — almost certainly 'u'. A single Huffman table can't capture this.

We implemented first-order context modeling with 4 classes:

- Class 0: control characters / non-ASCII
- Class 1: whitespace
- Class 2: uppercase letters / digits / punctuation
- Class 3: lowercase letters

Four separate Huffman tables, selected based on the previous decoded byte. DP was updated to
use context-dependent prices — `lit_price(byte, context_class(input[i-1]), ll_bits)` instead of
`lit_price(byte, ll_bits)`.

**Critical discovery: adaptive header size.** Four Huffman tables add 858 bytes of header (4 x 286
code lengths). On small files, this overhead destroys the coding gain. Solution: use 4 tables only
when the token count exceeds 32,000. Smaller files fall back to a single table with a 330-byte header. Tuning
this threshold alone improved average ratio by 0.5%.

MAX_CHAIN was increased to 128 — with context-dependent DP, each additional match found by longer chains
was used more efficiently.

**Final ratio: 0.288.**

## Intermediate Comparison

```
File                  Ours    gzip-9   Change
alice29.txt          0.350    0.360    -2.7%
kennedy.xls          0.174    0.201   -13.6%
plrabn12.txt         0.394    0.410    -4.0%
urls.10K             0.299    0.312    -4.2%
franko-berkut.html   0.264    0.275    -4.1%
franko-lys.rtf       0.085    0.082    +3.5%
AVERAGE              0.288    0.303    -5.0%
```

We beat gzip-9 on 8 of 10 compressible files. Two losses — a small file (fields.c — header overhead) and an
RTF file where gzip's effectively larger window helped.

Speed: 548ms compression (vs 464ms gzip-9), 9ms decompression (vs ~40ms gzip-9). Our decompressor is 4x faster
thanks to a 12-bit Huffman lookup table that decodes symbols in a single access.

Against stronger compressors: bzip2 (0.229), zstd-19 (0.226), brotli-11 (0.217), and 7z (0.217) — all beat us. They
use techniques we didn't implement: [BWT](https://en.wikipedia.org/wiki/Burrows%E2%80%93Wheeler_transform) transform, [FSE](https://en.wikipedia.org/wiki/Asymmetric_numeral_systems) entropy coding, rep-offsets, context mixing, and windows up to 16 MB. Our 32 KB window is the biggest remaining limitation — kennedy.xls compresses to 0.174 for
us but to 0.051 in 7z.

## Phase 6: Rep-offsets and Unconventional Approaches (Experiments 58-75)

After studying the architecture of [zstd](https://en.wikipedia.org/wiki/Zstd), [LZMA](https://en.wikipedia.org/wiki/LZMA), and RAR, we implemented **rep-offsets** — a technique for storing recently used
distances. When the current match has the same distance as a previous one, instead of fully encoding the distance (5-15 bits),
a special REP0 symbol (~2 bits) is used.

GPT 5.4 suggested three specific ideas: rep-offsets, distance context by match length, and adaptive block splitting. We
started with rep-offsets:

1. **REP0/REP1 post-hoc** — during encoding, check if the distance matches one of the two most recent ones. If so —
   encode with a special symbol. kennedy.xls got -8.9% immediately.

2. **Rep-distance in DP** — critical improvement. Added a `dp_rep` array tracking the last used distance along
   the optimal path. Now DP can consciously choose a shorter match with a rep-distance instead of a longer one with a new distance,
   if the former encodes more cheaply. kennedy.xls: -10.4% additionally.

3. **8 context classes** — split lowercase into vowels/consonants, added a separate class for non-ASCII. This helped
   on texts with mixed content.

4. **Reverse compression** — an unconventional idea from Gemini 3.1: try compressing the file backwards and keep the smaller
   result. HTML files compress better in reverse due to closing tags that mirror opening ones.

5. **RLE-encoded Huffman header** — our header was 2288 bytes (8 tables × 286 code lengths). Most values are
   zeros or repetitions. Simple [RLE](https://en.wikipedia.org/wiki/Run-length_encoding) encoding reduced the header to ~1100 bytes, improving every file's ratio.

6. **[CRC32](https://en.wikipedia.org/wiki/Cyclic_redundancy_check) verification** — added a checksum for integrity verification: the CRC32 of the original data is stored in the
   compressed file header.

## Phase 7: Window Size Increase — The Second Major Breakthrough (Experiments 76-80)

The biggest remaining limitation — sliding window size. 32 KB (as in [DEFLATE](https://en.wikipedia.org/wiki/Deflate)) means repetitions at distances > 32768
bytes aren't found. urls.10K has URL patterns that repeat at 50-100 KB intervals.

Increasing the window required significant format changes:

- Expanding the distance code table from 30 to 36 codes
- Moving REP0/REP1 symbols to higher indices
- Changing `prev_info` from `u32` to `u64` to pack longer distances
- Dynamic window size: `choose_window(input_len)` selects the optimal size — from 1 KB for small files to 1 MB
  for large ones

The result was impressive:

- **128 KB window**: ratio 0.270→**0.257** (-5.0%). urls.10K got -14.2%!
- **256 KB window**: ratio 0.257→**0.253** (-1.4%). plrabn12 -4.9%

This was the single largest jump in the entire segment — more than all other optimizations combined.

## Phase 8: Deep DP Optimization and REP Tracking (Experiments 81-140)

The third autoresearch session focused on squeezing the maximum from the existing architecture through 60 additional
experiments. The main approach — give DP more information and more choices.

### REP1 Tracking in DP — The Session's Biggest Breakthrough

Previously, DP tracked only one last distance (`dp_rep`). The encoder maintains REP0-REP3 (4 saved distances).
Adding `dp_rep1` — the second-to-last distance in DP — produced the **single largest jump**: kennedy.xls dropped from 0.130
to **0.074** (-43%!). DP can now consciously choose matches that create beneficial patterns of alternating distances.

### REP2/REP3 + REP in DP

- **REP2 tracking** in DP added a bit more (geo -0.3%)
- **REP3 symbol** (4th saved offset, as in LZMA) gave kennedy another -0.6%

### Dense try_lens — The Second Revolution

DP previously tried only 7 lengths: `[3, 6, 19, 67, 258, 515, max]`. Gemini 3.1 Pro during code review suggested: "test ALL
lengths up to 16." The result was impressive:

- **Dense match try_lens 3-32**: ratio -0.7%. kennedy -3.4%, every file improved
- **Dense rep-match try_lens 3-16**: ratio another -0.7%. kennedy -7.6%(!), franko-lys -3.4%

DP now precisely chooses where to "cut" a match so the next match starts more favorably. Without dense try_lens, DP only saw
[3, 6, 19...] and had to jump across a large gap between 6 and 19.

### Fractional-bit DP Pricing

Replaced integer Huffman prices (u8 bits) with fractional prices (u16 in 8.8 fixed-point format). Prices are initialized from
[Shannon entropy](https://en.wikipedia.org/wiki/Entropy_(information_theory)) (`-log2(freq/total) * 256`), and in the final DP passes — from actual Huffman code lengths. This **hybrid
approach** (entropy for exploration, Huffman for precision) gives better convergence than pure entropy or pure Huffman.

### Adaptive 5-byte Hash

A 4-byte hash has many collisions with a large window. A 5-byte hash drastically reduces collisions, finding better matches on large
files. But on small files (< 32 KB), a 5-byte hash performs worse — fewer useful collisions. Solution: **adaptive hash** —
5-byte for large files, 4-byte for small ones.

### Nibble-packed RLE Header

Huffman code lengths are numbers 0-12, which fit in 4 bits. Instead of 1 byte per value, we pack 2 values into 1 byte. With
run-length encoding of zeros and repetitions, the header shrank by roughly **half**. fields.c improved by 2.5%, geo by
1.0%.

### Adaptive Context Selection (Entropy-based)

Instead of a fixed threshold of "8 tables if > 15000 tokens" — we compute entropy with 1 table and with 8, compare against
the estimated header overhead. Files choose the optimal number of tables on their own. fireworks.jpeg now compresses
without context overhead (ratio 0.9999 instead of 1.0076).

### What Did NOT Work

Over 60 experiments, many ideas were rejected:

- **DP↔encoder rep-state sync fix** — theoretically correct, but destabilized DP convergence. kennedy grew from 0.074
  to 0.138. The current "inexact" DP works better in practice
- **16 context classes** — header overhead eats the gains on small/binary files
- **MIN_MATCH=2** — the extra alphabet symbol costs more than the savings
- **512 KB window** — roundtrip failure due to a bug in distance encoding
- **Exact context tracking in DP** — zero effect, the `input[i-1]` approximation is accurate enough
- **3-byte hash** — noisy in the near-match slot, degrades text files
- **8 DP passes** — prices oscillate; 6 passes are optimal

## Phase 9: rANS Entropy Coder and 8 MB Window (Experiments 141-155)

### 8 MB Window

Increasing the sliding window from 1 MB to 8 MB required expanding the distance code table from 40 to 46 codes and shifting REP
symbols. Key bug: the `dist_extra` field in the `Tok` struct was `u16` (max 65535), but distances >262144
need 17+ extra bits (max 2097151). Changing to `u32` fixed the roundtrip failures.

**Result on enwik8 (100 MB):** ratio 0.2917→**0.2700** (-7.4%). Nearly matched zstd-19 (0.2694). On the 12-file
corpus, the effect was minimal — no file exceeds 1 MB.

### rANS — Fractional Precision Without Changing the Header (for Large Files)

The session's main breakthrough. We implemented **[rANS (range Asymmetric Numeral Systems)](https://en.wikipedia.org/wiki/Asymmetric_numeral_systems)** — an entropy coder with fractional bit
precision. Key idea: **try-both approach** — the compressor compresses each file with both Huffman and rANS, keeps the smaller result. A flag bit
in the header tells the decoder which coder was used.

Why try-both:

- **rANS wins on large structured files** — kennedy.xls 0.066→**0.047** (-28%!). rANS encodes a symbol with
  probability 0.8 for ~0.32 bits; Huffman minimum is 1 bit.
- **Huffman wins on small files** — rANS requires a freq table in the header (~800-2000 bytes), which is larger than
  nibble-packed Huffman code lengths (~200-400 bytes).
- **Try-both guarantees zero regression** — the smaller result is always kept.

The rANS freq table is stored in sparse format: `count(u16) + [index(u16) + freq(u16)]...` — only non-zero frequencies.
This is ~3x more compact than dense format.

The first attempt (dense freq table, 4.7 KB) failed catastrophically — alice29 +9.5%, cp.html +62%. Try-both with sparse
header solved the problem completely.

## Phase 10: Suffix Array and Performance Optimization (Experiments 156-175)

### Suffix Array Match Finder

Hash chain with MAX_CHAIN=8192 gave the best matches, but O(n × 8192) on 100 MB takes 40 minutes. A **[suffix array](https://en.wikipedia.org/wiki/Suffix_array)** guarantees optimal matches in O(n × scan_range) where scan_range=32-64 — much less.

We implemented an O(n log²n) suffix array (prefix doubling algorithm) with a hybrid approach: **SA for files < 1 MB, hash chain for larger ones**. SA finds the longest match; hash chain (len=16) finds the closest by distance. Two match slots are fed into DP.

Result: **ratio -0.8% from hash chain, but 3-7x faster compression**. On enwik8: 5.8 min instead of 40 min.

### Block-based DP

Gemini 3.1 during code review identified a critical issue: DP arrays for a 100 MB file consume ~4 GB RAM and completely blow the L3 cache. Solution: **split DP into 256 KB blocks** that fit in L2 cache.

Implementation: match arrays remain full-file (matches reference the full 8 MB window), but `cost[]`, `prev_info[]`, `dp_rep[]` are only 256 KB. Between blocks, rep-state and prev_byte are carried over. Backtracking within each block is separate.

**Result on enwik8: 126s → 76s (-40%)** with identical ratio. On small files (<256 KB), the effect is minimal.

### Computation Pipelining

A series of optimizations for large files (>1 MB):
- **Early abort** in hash chain when match ≥ 128 bytes — don't search further
- **Adaptive chain length**: 64 for 1-10 MB, 32 for >10 MB
- **Sparse try_lens** for large files: 14 entries instead of 36 (regular) and 8 instead of 12 (rep)
- **Skip rANS** for >2 MB — rANS never wins on text
- **sort_unstable_by** and 16-byte 2-lane match comparison in SA

**Combined result:** enwik8 40 min → **76s (1.3 min)**, i.e., **32x faster**.

### What Did NOT Work

- **Static dictionary** (4 KB of common patterns) — no file benefited. Dictionaries are only useful as in brotli: 120 KB + morphological transformations.
- **Integer log2 instead of float** — integer division (30 cycles) is no faster than hardware float log2 (15-20 cycles). Ratio slightly worse due to lower precision.
- **Delta-encoding Huffman headers** between context tables — deltas exceed the nibble RLE value range (0-12).
- **1 DP pass** for large files — kennedy degrades by 66%. A minimum of 2 passes is needed.
- **Greedy pre-pass** for frequency estimation — the greedy algorithm produces completely different frequencies than DP; prices are inaccurate.

## Final Comparison

```
File                  Ours    gzip-9   brotli-11  Change vs gzip
alice29.txt          0.329    0.360    0.310       -8.6%
asyoulik.txt         0.358    0.390    0.341       -8.2%
fields.c             0.255    0.260    0.227       -2.1%
cp.html              0.307    0.324    0.280       -5.3%
kennedy.xls          0.047    0.201    0.060      -76.6%  ← beats brotli-11!
plrabn12.txt         0.359    0.410    0.345      -12.4%
urls.10K             0.218    0.312    0.210      -30.1%
geo.protodata        0.107    0.127    0.099      -15.5%
franko-lys.rtf       0.075    0.082    0.072       -8.9%
franko-berkut.html   0.235    0.275    0.221      -14.7%
AVERAGE              0.221    0.303    0.217      -27.1%
```

We beat gzip-9 on **all 10** compressible files, **-27%** overall. kennedy.xls **0.047** — **21% better than brotli-11**!

**On enwik8 (100 MB):** ratio **0.285** (with optimized parameters), compression **76 seconds**. Beating gzip-9 by **22%**, brotli-6 by **8%**.

### Speed (In-process, Fair Benchmark)

```
12-file corpus (3.47 MB):
  OURS:     248 MB/s decompress, ratio 0.221
  gzip-9:   486 MB/s decompress, ratio 0.304
  brotli-6: 532 MB/s decompress, ratio 0.242
  zstd-1:  1270 MB/s decompress, ratio 0.292

enwik8 (100 MB):
  OURS:     1.3 MB/s compress, 111 MB/s decompress, ratio 0.285
  gzip-9:  36 MB/s compress, 306 MB/s decompress, ratio 0.365
  brotli-6: 35 MB/s compress, 365 MB/s decompress, ratio 0.310
  zstd-19:  2.1 MB/s compress, 794 MB/s decompress, ratio 0.269
```

## Full Evolution

| Stage | Ratio | Technique |
|-------|-------|-----------|
| Baseline | 0.754 | Greedy LZ77, hash chains |
| + speed optimization | 0.754 | Unsafe access, better hash, chain tuning |
| + compact format | 0.452 | Literal sequences, variable-length match encoding |
| + Huffman | 0.337 | DEFLATE-style unified literal/length/distance alphabet |
| + optimal DP | 0.306 | Forward DP with iterative Huffman pricing |
| + context modeling | 0.288 | First-order Huffman (8 tables), adaptive threshold |
| + rep-offsets | 0.274 | REP0/REP1 distances, rep-distance in DP |
| + unconventional approaches | 0.271 | Reverse compression, RLE header, 8-byte hash |
| + window increase | 0.253 | Dynamic window up to 256 KB |
| + REP1 tracking in DP | 0.231 | DP tracks 2 last distances, kennedy -43% |
| + dense try_lens | 0.228 | DP tries all lengths 3-64, more precise parsing |
| + nibble RLE + adaptation | 0.227 | More compact header, entropy-based context selection |
| + 8 MB window | 0.225 | 46 distance codes, enwik8 -7.4% |
| + rANS (try-both) | 0.219 | Fractional bits for large files, kennedy -28% |
| + suffix array | 0.220 | Guaranteed optimal matches, 7x faster |
| + block DP + speed | **0.221** | 256 KB blocks, enwik8 40 min → 76s |

**Overall: more than 3.4x better compression from a clean slate to a compressor that surpasses gzip-9 by 27%, beats bzip2 and brotli-6, and comes within 1.8% of brotli-11 — built through ~175 autonomous experiments with 32x speed optimization.**

## What We Learned About Autoresearch

**The loop works.** The hypothesis → implementation → measurement → keep/revert cycle is genuinely productive. Most
experiments fail (35 out of 57 were discarded), but successes accumulate.

**Context resets are the real challenge.** Claude Code's context window fills up after ~15-20 experiments. The
`/autoresearch-resume` command reads `autoresearch.md` and `git log` to restore context, but nuanced insights (
e.g., "the Kraft inequality fix has a u32 underflow bug") are lost. The `autoresearch.ideas.md` backlog
helps — good ideas don't disappear on context reset.

**The multi-model approach produced the best ideas.** The biggest jumps came from algorithmic changes, not
micro-optimizations. The key tool was integrating multiple AI models into the loop via MCP servers. Beyond the main agent
Claude Opus 4, we connected **GPT 5.4** (via [`@trishchuk/codex-mcp-tool`](https://www.npmjs.com/package/@trishchuk/codex-mcp-tool)) and **Gemini 3.1 Pro** (via [`@trishchuk/gemini-mcp-server`](https://www.npmjs.com/package/@trishchuk/gemini-mcp-server)) as MCP servers. When
the main agent ran out of ideas, it delegated "brainstorming" to the other models:

- **Gemini 3.1** suggested first-order context modeling (inspired by Brotli) and optimal parsing as "the single
  most impactful technique"
- **GPT 5.4** wrote a correct [canonical Huffman](https://en.wikipedia.org/wiki/Canonical_Huffman_code) implementation with a 12-bit lookup table and suggested
  a set-associative hash table
- **Claude Opus 4** (the main agent) integrated these ideas, adapted them to the existing architecture, and ran all experiments

Configuration in `.mcp.json`:

```json
{
  "mcpServers": {
    "gemini-cli": {
      "command": "npx",
      "args": [
        "-y",
        "@trishchuk/gemini-mcp-server"
      ]
    },
    "codex-cli": {
      "command": "npx",
      "args": [
        "-y",
        "@trishchuk/codex-mcp-tool"
      ]
    }
  }
}
```

This transformed autoresearch from "one model spinning a loop" into a **collaboration of three AI models**, where each brings its
strengths.

**Bugs can be catastrophic.** The infinite loop in the Kraft inequality Huffman fix and the OOM from context
modeling — both "killed" the system. `autoresearch.checks.sh` with `cargo test` caught correctness bugs, but resource
bugs (infinite loops, memory exhaustion) required manual intervention. Limiting test parallelism (
`RUST_TEST_THREADS=1`) proved critical.

**The 80/20 rule applies aggressively.** Eight changes account for 95% of the ratio improvement: compact format (-40%), Huffman
coding (-25%), optimal DP (-7%), window increase (-5%), REP1 tracking in DP (-7%), dense try_lens (-1.4%),
rep-offsets (-3%), context modeling (-2%). The remaining 100+ experiments were incremental or dead ends.

**Window size matters more than algorithm complexity.** Increasing the sliding window from 32 KB to 256 KB gave more ratio
improvement than all other optimizations from the last 30 experiments combined. The lesson: before complicating the algorithm, check whether
simple parameters won't give you more.

**Unconventional ideas work.** Reverse compression (try the file backwards), RLE-encoding Huffman headers,
XOR-prediction of literals — most didn't work, but some gave real gains. Reverse compression improved
HTML files, and header RLE helped every file without exception.

**Dynamic adaptation matters more than fixed parameters.** Adaptive window size (from 1 KB for small files to 256
KB for large ones), adaptive number of context tables (1 or 8 based on entropy), adaptive hash (4 or 5 bytes),
trying reverse — all these decisions are made per-file based on input characteristics.

**DP pricing is an art, not a science.** The hybrid approach (Shannon entropy for early passes, Huffman lengths for
the final one) works better than a pure version of either. The "theoretically correct" DP↔encoder rep-state sync
fix catastrophically worsened ratio because it destabilized convergence. Practice beats theory.

**Dense try_lens — an unexpected win.** Trying all match lengths 3-32 instead of sparse [3,6,19,67,...] gave -1.4%
ratio. This idea came from a code review by Gemini 3.1 Pro — a fresh perspective from another model found an "obvious" optimization that
the main agent hadn't noticed for 50+ experiments.

## Try It Yourself

```bash
# Clone and build
git clone <repo> && cd experiment
cargo build --release

# Run the benchmark
./autoresearch.sh

# Compare with gzip
./benchmark.sh

# Run your own autoresearch loop
/autoresearch optimize <your target>
```

The compressor is ~900 lines of Rust with no dependencies (lib.rs + huffman.rs + codes.rs + context.rs + checksum.rs + rle.rs +
range_coder.rs). The full experiment history is in `autoresearch.jsonl` — 140+ entries documenting every hypothesis,
result, and decision.
