# Autoresearch: Optimize LZ77 compression ratio

## Objective
Optimize a pure Rust LZ77+Huffman compressor for compression ratio (primary). 12-file corpus, 3.46 MB.

## Metrics
- **Primary**: `ratio` (compressed/original, lower is better)
- **Secondary**: `compress_µs`, `decompress_µs`, `total_µs`

## How to Run
`./autoresearch.sh`

## Current Best
**Ratio: 0.2245** — beats gzip-9 (0.303) by 25.9%, beats zstd-19 (0.226) by 0.8%, beats brotli-6 (0.242) by 7.2%. Loses to brotli-11 (0.217).

## Architecture
- **Match finder**: 4/5-byte adaptive hash chain + 8-byte secondary hash. MAX_CHAIN=8192.
- **Optimal parse**: Forward DP, 6 passes, hybrid pricing (entropy→Huffman). Dense try_lens 3-64. Fractional 8.8 fixed-point prices.
- **Rep-offsets**: REP0-REP3 in distance alphabet. DP tracks REP0+REP1+REP2. Dense rep try_lens 3-16.
- **Context**: 8 byte classes. Adaptive selection (entropy-based). Per-context Huffman.
- **Window**: Dynamic 1KB-1MB. Distance codes extended to 40 (up from 36).
- **Header**: Nibble-packed RLE code lengths. xxhash32 integrity.
- **Reverse**: 16KB sample pre-check, try both directions.

## What Would Break Through
To beat brotli-11 (0.217) requires structural changes:
1. **Range/ANS coder** — fractional bits instead of Huffman integers (+2-5%)
2. **Separate lit/match alphabets** — enables full order-1 context (256 tables for literals)
3. **Binary tree / suffix array match finder** — guaranteed optimal matches

## Dead Ends (comprehensive)
Chain tuning (saturated at 8192), 16 ctx classes, MIN_MATCH=2, DP↔encoder rep sync, 3-byte hash, exact context tracking, multi-block (header overhead > cross-block match loss), MAX_BITS>12 (OOM), blended pricing, RLE match slot, 8 DP passes
