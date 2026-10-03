# Ideas Backlog (after wave 5, ratio 0.182259)

## Wave 5 follow-ups (first)
0. **Port wave5-codex onto main** (est. ~-0.5%): REP0-63 + 8 Bayesian high distance bits + align ctx; merge st_rep as [s][lctx][k<REP_COUNT+1] with Codex's tree pricing per (s, lctx).
0. **Speed** is now the main debt (compress x3.6, decompress x4.7 vs wave 4): fewer/cheaper DP passes once converged, hash tables x16 instead of x64 with check tags (-0.00002 ratio), SIMD mixer dot products, single literal-model pass for lc.
0. More LZMA-priced DP passes beyond 4 (Gemini found big gains from 3rd/4th); stop on convergence.

# Ideas Backlog (after wave 4, ratio 0.199983)

## Next, ranked by expected value / cost
1. **Distance modeling** — distances are 35% (alice) to 59% (franko-berkut) of the LZMA stream. Try: slot ctx = previous slot bucket; model the top 1-2 direct bits of large distances with probs (ctx = slot); bigger rep cache (REP0-REP7) for HTML/RTF tag patterns.
2. **is_match / literal flag mixing** — is_match ctx is just state x prev>>5. A small mixer over {is_match[state,prev>>5], hashed order-2, order-4 ctx, match-length bucket} like the literal mixer.
3. **Literal model capacity** — 2nd APM with order-2 ctx; two mixers with different weight-set selectors averaged; sparse/skip contexts (pos-2 only, column for urls); bit-history states instead of simple counters.
4. **DP fidelity** — align the word hash in `literal_costs` (incremental, forward FNV) with `lit_ctx` (backward scan): the shadow model prices a word context the real coder does not use; third LZMA DP pass (+~10% time); price len/rep flags with the coder's new contexts (prev-length bucket); per-position match prices from a shadow run like literal_costs.
5. **Speed recovery** — compress is 2x and decompress 25x slower than at wave start. Pick lc from the 16 KB reverse-check sample instead of trying {0,2}; skip literal mixer hash tables > 2^20 for small files; SIMD mixer dot product; gate the LZMA path (or use 1 shadow pass) above ~10 MB.
6. **Large-file path** — enwik8 uses ~4 GB RSS (match arrays + literal_costs + tokens). Stream literal_costs per DP block, u16 costs.

## Dead in wave 4
See autoresearch.md "Wave 4 dead ends": shortrep, two-rate counters, slot ctx by prev byte/state, MIX_LR != 6, entropy price floor.

## Already done (all waves)
SA match finder (<1 MB) + hash chain; Huffman / rANS / LZMA-style backends (try-all); 8 MB window, 46 dist codes;
9-ctx Huffman; dense try_lens; nibble RLE header; block DP; LZMA-priced DP passes with per-position literal costs;
literal mixer o1/o2/o3/o4/o6/word + APM; count-adaptive binary probs; len/rep/is_match contexts.
