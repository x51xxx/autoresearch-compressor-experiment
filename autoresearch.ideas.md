# Ideas Backlog

## Priority (from Gemini/Codex review)
1. **Early abort in hash chain** — stop when match >= 128 bytes. Trivial, big speed win on large files.
2. **Block-based DP** — split into 128-256KB chunks. DP arrays fit in L1/L2 cache → 5-10x speedup. LZ77 window still 8MB.
3. **Greedy pre-pass + 1 DP pass** — replace 2 DP passes with 1 greedy freq estimation + 1 accurate DP.

## Already done
- SA match finder (for files < 1MB) + hash chain (for larger)
- rANS try-both (Huffman vs rANS, keep smaller)
- 8MB window, 46 distance codes
- 9 context classes, adaptive selection
- Dense try_lens 3-32, rep try_lens 3-16
- Nibble-packed RLE header
- 14-bit rANS precision

## Structural (not yet tried)
- SA-IS (linear time SA build)
- tANS (table-based ANS for faster decode)
- Separate lit/match alphabets
- Hardware CRC32 hash, SIMD match length
