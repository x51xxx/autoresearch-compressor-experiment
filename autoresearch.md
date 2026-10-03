# Autoresearch: Optimize LZ77 compression ratio

## Objective
Optimize a pure Rust LZ77+Huffman compressor for compression ratio (primary). 12-file corpus, 3.46 MB.

## Metrics
- **Primary**: `ratio` (compressed/original, lower is better)
- **Secondary**: `compress_µs`, `decompress_µs`, `total_µs`

## How to Run
`./autoresearch.sh`

## Current Best
**Ratio: 0.199983** (wave 4, run 28) - compress 7.7 s, decompress 363 ms for the 12-file corpus
(A/B vs wave-4 baseline under equal load: compress 3.78 s -> 7.73 s, decompress 14.5 ms -> 363 ms).
Beats brotli-11 (0.2167) by 7.7%, xz/LZMA raw lc=3 (0.2156) by 7.2%, gzip-9 (0.303). Wave 4 started at 0.220861 (-9.5%).
enwik8 (run 28 build): ratio 0.2550 (was 0.285), compress 136 s (was 76 s), decompress 13 s (was ~0.9 s), peak RSS ~4 GB, roundtrip ok.
Commit/ git: every wave-4 commit is on `main`; commit with explicit paths (`git commit -- src/...`), the user also works in this repo.

## Wave 4 protocol (segment 3 in autoresearch.jsonl)
edit → `./autoresearch.sh` → `./autoresearch.checks.sh` → keep = git commit / discard = `git checkout -- src` → append jsonl line → update this file.
Keep rule: ratio must improve; compress_µs must not grow by more than 25%. Decompress speed is reported, not gated.

## Architecture
- **Match finder**: suffix array (files < 1 MB, scan range 32) + hash chain (4/5-byte hash; chain 16/64/32 by size, early abort at 128). Two match slots (longest + nearest) feed the DP.
- **Optimal parse**: `dp_parse(input, MatchArrays, Prices, lzma)` forward DP in 256 KB blocks; 8.8 fixed-point prices from per-pass `Prices` tables (lit[ctx][b] or per-position lit costs, mlen/rlen[ctx][len], dist slot, state-dependent flag costs st_lit/st_match/st_rep[after_match]). Dense try_lens 3-32 (+43,67,131,258,515) for <1 MB.
- **Rep-offsets**: REP0-REP3; DP tracks REP0-REP2 with learned prices; between-pass freq rebuild replays the encoder's rep state.
- **Context**: 9 byte classes (prev byte), adaptive 1 vs 9 tables by entropy estimate.
- **Window**: 8 MB max (power-of-two of file size), 46 distance codes + 4 REP symbols, MAX_MATCH 1026.
- **Entropy coding**: try-all, keep smallest: Huffman (nibble-packed RLE header), rANS (sparse 14-bit freq header, 100KB-2MB), and the **LZMA-style backend** (`src/lzma_coder.rs`, flag 0x20 in header byte 12, params byte at offset 18): adaptive binary range coder, 12-bit probs + 4-bit count in u16 (adaptive shift 2..5), LZMA 12-state machine; is_match ctx = state x prev>>5; rep flags & len coders ctx = prev-match-length bucket, REP0-3, LZMA dist slots (== our DEFLATE dist codes), len coder low/mid/high(10 bits). Literals: lpaq-style integer logistic mixer over {LZMA lit/matched-lit prob, order-1 ctr, hashed o2/o3/o4/o6/word ctrs (nibble-slotted 16-ctr slots), bias}; weight set = bitpos x matched x ctx-confidence(6); APM (prev byte, node) final p=(mix+3apm)/4. Ctr rate 1/(n+1.5), limit 1020. lc in {0,2}, lp=pb=0. LZMA backend now wins on every compressible file.
- **LZMA parse**: after 2 Huffman DP passes (seed only), 2 extra DP passes priced by `Prices::from_lzma_stats` (static estimates from the previous parse for match side; literals priced PER POSITION by `lzma_coder::literal_costs`, a shadow run of the adaptive literal mixer trained on the previous parse literals). Only the LZMA parse is encoded with the LZMA backend (lc 0 and 2).
- **Reverse**: 16 KB sample pre-check, try both directions. xxhash32 integrity.

## What Would Break Through (post wave 4)
See `autoresearch.ideas.md`. Stream breakdown (run 24): text files ~45-60% literal bits, 20-60% distance bits
(franko-berkut: distances 59%), kennedy was 49% length bits before len ctx.

## Wave 4 dead ends
- Distance-slot tree ctx x (prev byte>>5) or x (after-match state): worse (dilution)
- APM ctx = node only: worse than (prev byte, node) at same speed
- Per-bit scattered hash indexing: best ratio (0.2044 vs nibble 0.2046) but +40% time; 256-block slots: much worse (0.2058)
- LZMA shortrep (rep0long bit + DP option): worse at all priors
- MIX_LR 2/4/10/16 all worse than 6
- Entropy price floor 1/8 bit (no effect); two-rate LZMA counters 4/7,5/6,4/5 (worse); Huffman DP passes 4->3 (worse, no speedup)

## Dead Ends (comprehensive)
Chain tuning (saturated at 8192), 16 ctx classes, MIN_MATCH=2, DP↔encoder rep sync, 3-byte hash, exact context tracking, multi-block (header overhead > cross-block match loss), MAX_BITS>12 (OOM), blended pricing, RLE match slot, 8 DP passes
