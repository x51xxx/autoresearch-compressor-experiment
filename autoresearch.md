# Autoresearch: Optimize LZ77 compression ratio

## Objective
Optimize a pure Rust LZ77+Huffman compressor for compression ratio (primary). 12-file corpus, 3.46 MB.

## Metrics
- **Primary**: `ratio` (compressed/original, lower is better)
- **Secondary**: `compress_µs`, `decompress_µs`, `total_µs`

## How to Run
`./autoresearch.sh`

## Current Best
**Ratio: 0.220861** (wave 4 baseline, commit ba72f50) — compress 3.94 s, decompress 14.8 ms for the 12-file corpus.
Beats gzip-9 (0.303) by 27%, zstd-19 (0.226). Loses to brotli-11 (~0.217).

## Wave 4 protocol (segment 3 in autoresearch.jsonl)
edit → `./autoresearch.sh` → `./autoresearch.checks.sh` → keep = git commit / discard = `git checkout -- src` → append jsonl line → update this file.
Keep rule: ratio must improve; compress_µs must not grow by more than 25%. Decompress speed is reported, not gated.

## Architecture
- **Match finder**: suffix array (files < 1 MB, scan range 32) + hash chain (4/5-byte hash; chain 16/64/32 by size, early abort at 128). Two match slots (longest + nearest) feed the DP.
- **Optimal parse**: forward DP in 256 KB blocks; 4 passes (<500 KB) or 2 passes; entropy prices early, Huffman lengths in the last pass; 8.8 fixed-point prices. Dense try_lens 3-32 (+43,67,131,258,515) for <1 MB.
- **Rep-offsets**: REP0-REP3 in the distance alphabet; DP tracks REP0-REP2 with hardcoded 2/3/4-bit prices.
- **Context**: 9 byte classes (prev byte), adaptive 1 vs 9 tables by entropy estimate.
- **Window**: 8 MB max (power-of-two of file size), 46 distance codes + 4 REP symbols, MAX_MATCH 1026.
- **Entropy coding**: Huffman (nibble-packed RLE header) and rANS (sparse 14-bit freq header, 100 KB–2 MB files) — try both, keep smaller.
- **Reverse**: 16 KB sample pre-check, try both directions. xxhash32 integrity.

## What Would Break Through
To beat brotli-11 (0.217) requires structural changes:
1. **Range/ANS coder** — fractional bits instead of Huffman integers (+2-5%)
2. **Separate lit/match alphabets** — enables full order-1 context (256 tables for literals)
3. **Binary tree / suffix array match finder** — guaranteed optimal matches

## Dead Ends (comprehensive)
Chain tuning (saturated at 8192), 16 ctx classes, MIN_MATCH=2, DP↔encoder rep sync, 3-byte hash, exact context tracking, multi-block (header overhead > cross-block match loss), MAX_BITS>12 (OOM), blended pricing, RLE match slot, 8 DP passes
