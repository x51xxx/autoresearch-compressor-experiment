/// LZ77 + Huffman compressor (DEFLATE-style combined lit/len alphabet).
///
/// Combined symbol alphabet:
///   0-255     = literal byte
///   256       = end of block
///   257-285   = match length (3-258, using DEFLATE-style length codes)
/// Distance is encoded separately as 5-bit base + extra bits (DEFLATE-style).
///
/// Format: orig_len(8) + num_symbols(4) + litlen_lengths(286*4=1144) + dist_lengths(36) + bitstream

pub mod huffman;
pub mod checksum;
mod rle;
pub mod codes;
mod context;
pub mod rans;
pub mod range_coder;
pub mod suffix_array;
pub mod lzma_coder;

// Checksum function — swap between crc32, adler32, xxhash32
use checksum::xxhash32 as integrity_hash;
use rle::{rle_encode_lengths, rle_decode_lengths};
use codes::*;
use context::*;

const MAX_WINDOW: usize = 8_388_608; // 8MB max window
const MAX_CHAIN: usize = 8192;
#[inline] fn len_bucket(l: usize) -> usize { if l < 4 { 0 } else if l < 8 { 1 } else if l < 16 { 2 } else { 3 } }
const NIL: u32 = u32::MAX;

/// Compare bytes at positions a and b, return match length.
/// Uses 2x u64 parallel lanes (16 bytes/iteration) for ILP.
#[inline(always)]
unsafe fn match_length(dp: *const u8, a: usize, b: usize, max_len: usize) -> usize {
    let mut n = 0usize;
    // 16 bytes per iteration — 2 independent load+XOR chains for ILP
    while n + 16 <= max_len {
        let x0 = std::ptr::read_unaligned(dp.add(a + n) as *const u64)
            ^ std::ptr::read_unaligned(dp.add(b + n) as *const u64);
        let x1 = std::ptr::read_unaligned(dp.add(a + n + 8) as *const u64)
            ^ std::ptr::read_unaligned(dp.add(b + n + 8) as *const u64);
        if x0 != 0 { return n + (x0.trailing_zeros() as usize >> 3); }
        if x1 != 0 { return n + 8 + (x1.trailing_zeros() as usize >> 3); }
        n += 16;
    }
    // Tail: 8 bytes
    if n + 8 <= max_len {
        let x = std::ptr::read_unaligned(dp.add(a + n) as *const u64)
            ^ std::ptr::read_unaligned(dp.add(b + n) as *const u64);
        if x != 0 { return n + (x.trailing_zeros() as usize >> 3); }
        n += 8;
    }
    // Remaining bytes
    while n < max_len && *dp.add(a + n) == *dp.add(b + n) { n += 1; }
    n
}

#[inline(always)]
fn hash4(data: &[u8], pos: usize, hash_bits: usize) -> usize {
    let v = unsafe { std::ptr::read_unaligned(data.as_ptr().add(pos) as *const u32) };
    (v.wrapping_mul(0x1E35A7BD) >> (32 - hash_bits)) as usize
}

#[inline(always)]
fn hash5(data: &[u8], pos: usize, hash_bits: usize) -> usize {
    let v = unsafe { std::ptr::read_unaligned(data.as_ptr().add(pos) as *const u32) } as u64;
    let b5 = unsafe { *data.get_unchecked(pos + 4) } as u64;
    ((v | (b5 << 32)).wrapping_mul(0x9E3779B97F4A7C15) >> (64 - hash_bits)) as usize
}

#[inline(always)]
fn compute_hash(input: &[u8], pos: usize, hash_bits: usize, use_hash5: bool) -> usize {
    if use_hash5 && pos + 5 <= input.len() {
        hash5(input, pos, hash_bits)
    } else {
        hash4(input, pos, hash_bits)
    }
}

#[inline(always)]
fn insert_hash(input: &[u8], pos: usize, head: &mut [u32], prev: &mut [u32], hash_bits: usize, window_size: usize, use_hash5: bool) {
    if pos + 4 <= input.len() {
        let h = compute_hash(input, pos, hash_bits, use_hash5);
        unsafe {
            *prev.get_unchecked_mut(pos & (window_size - 1)) = *head.get_unchecked(h);
            *head.get_unchecked_mut(h) = pos as u32;
        }
    }
}

struct Tok {
    sym: u16,
    len_extra: u16,
    len_ebits: u8,
    dist_code: u8,
    dist_extra: u32,
    dist_ebits: u8,
    /// The byte immediately before this token in the decoded output stream.
    prev_byte: u8,
}

fn make_lit_tok(b: u8, prev: u8) -> Tok {
    Tok { sym: b as u16, len_extra: 0, len_ebits: 0, dist_code: 0, dist_extra: 0, dist_ebits: 0, prev_byte: prev }
}

fn make_match_tok(ml: usize, md: usize, prev: u8) -> Tok {
    let (li, le, lb) = len_to_code(ml);
    let (di, de, db) = dist_to_code(md);
    Tok { sym: (257 + li) as u16, len_extra: le as u16, len_ebits: lb as u8, dist_code: di as u8, dist_extra: de as u32, dist_ebits: db as u8, prev_byte: prev }
}

/// Cost of a literal in fixed-point bits (8.8), context-aware.
#[inline]
fn lit_price(b: u8, ctx: usize, ll_prices: &[[u16; NUM_LITLEN]; NUM_CTX]) -> u64 {
    ll_prices[ctx][b as usize] as u64
}

/// Cost of a match in fixed-point bits (8.8), context-aware.
#[inline]
fn match_price(ml: usize, md: usize, ctx: usize, ll_prices: &[[u16; NUM_LITLEN]; NUM_CTX], dist_prices: &[u16]) -> u64 {
    let (li, _, lb) = len_to_code(ml);
    let (di, _, db) = dist_to_code(md);
    ll_prices[ctx][257 + li] as u64 + (lb as u64) * 256 + dist_prices[di] as u64 + (db as u64) * 256
}

fn choose_window(input_len: usize) -> usize {
    if input_len < 1024 {
        1024
    } else if input_len < MAX_WINDOW {
        input_len.next_power_of_two()
    } else {
        MAX_WINDOW
    }
}

pub fn compress(input: &[u8]) -> Vec<u8> {
    // Try both forward and reverse, pick smaller
    let ws = choose_window(input.len());
    let fwd = compress_inner(input, ws);

    // Quick pre-check: test reverse on a small sample (skip for very large files)
    if input.len() > 4096 && input.len() < 786432 {
        // Sample first 4KB: compress forward and reverse, compare
        let sample_len = 16384.min(input.len());
        let sample = &input[..sample_len];
        let rev_sample: Vec<u8> = input[input.len() - sample_len..].iter().rev().copied().collect();
        let fwd_sample_sz = compress_inner(sample, choose_window(sample_len)).len();
        let rev_sample_sz = compress_inner(&rev_sample, choose_window(sample_len)).len();

        // Only try full reverse if sample suggests it might be better
        if rev_sample_sz < fwd_sample_sz {
            let rev_input: Vec<u8> = input.iter().rev().copied().collect();
            let rev_ws = choose_window(rev_input.len());
            let mut rev = compress_inner(&rev_input, rev_ws);
            if rev.len() < fwd.len() {
                rev[12] |= 0x80;
                return rev;
            }
        }
    } else if input.len() > 256 && input.len() <= 4096 {
        // Small files: always try reverse (cheap)
        let rev_input: Vec<u8> = input.iter().rev().copied().collect();
        let rev_ws = choose_window(rev_input.len());
        let mut rev = compress_inner(&rev_input, rev_ws);
        if rev.len() < fwd.len() {
            rev[12] |= 0x80;
            return rev;
        }
    }

    fwd
}

pub fn compress_inner(input: &[u8], window_size: usize) -> Vec<u8> {
    let hash_bits = (window_size.trailing_zeros() + 2).min(20) as usize; // cap at 20 (1M entries)
    let hash_size = 1 << hash_bits;
    let long_hash_size = std::cmp::min(65536, hash_size);
    let len = input.len();
    let use_hash5 = len > 32768; // 5-byte hash for large files, 4-byte for small

    // Iterative optimal parsing with context-aware prices.
    // ll_prices[ctx][sym] = cost in 8.8 fixed-point bits (256 = 1 bit).
    // Bootstrap with byte frequency-based prices (fractional bit precision).
    let mut ll_prices = [[0u16; NUM_LITLEN]; NUM_CTX];
    {
        let mut byte_freq = [0u32; 256];
        for &b in input { byte_freq[b as usize] += 1; }
        let total = len.max(1) as f64;
        for ctx in 0..NUM_CTX {
            for i in 0..256 {
                if byte_freq[i] > 0 {
                    // -log2(freq/total) * 256, clamped to [256, 3072]
                    let bits256 = (-(byte_freq[i] as f64 / total).log2() * 256.0) as u16;
                    ll_prices[ctx][i] = bits256.clamp(256, 3072);
                } else {
                    ll_prices[ctx][i] = 3072; // 12 bits
                }
            }
            for i in 257..NUM_LITLEN { ll_prices[ctx][i] = 6 * 256; }
            ll_prices[ctx][END_BLOCK as usize] = 8 * 256;
        }
    }
    let mut dist_prices = [5u16 * 256; NUM_DIST];
    dist_prices[REP0_SYM] = 2 * 256; // REP0 typically gets a very short code
    dist_prices[REP1_SYM] = 3 * 256; // REP1 slightly longer
    dist_prices[REP2_SYM] = 4 * 256; // REP2 slightly longer still
    dist_prices[REP3_SYM] = 5 * 256; // REP3

    let mut tokens: Vec<Tok> = Vec::with_capacity(len / 2);

    let mut lzma_tokens: Vec<Tok> = Vec::new();
    if len >= MIN_MATCH {
        // Step 1: Match finding — SA for small files, hash chain for large
        let mut match_ml = vec![0u16; len];
        let mut match_md = vec![0u32; len];
        let mut match2_ml = vec![0u16; len];
        let mut match2_md = vec![0u32; len];
        {
            let dp = input.as_ptr();
            let use_sa = len < 1_000_000; // SA for files < 1MB, hash chain for larger
            let chain_len: u32 = if use_sa { 16 } else if len > 10_000_000 { 32 } else { 64 };

            // Optional: build suffix array for small files
            let sa = if use_sa { suffix_array::build_suffix_array(input) } else { vec![] };
            let isa = if use_sa { suffix_array::build_inverse_sa(&sa) } else { vec![] };

            let mut head = vec![NIL; hash_size];
            let mut prev_chain = vec![NIL; window_size];
            let mut miss_streak = 0u32;

            for i in 0..len {
                if i + 4 > len { break; }

                // Skip incompressible regions
                if miss_streak >= 256 {
                    if i & 1 == 0 {
                        let h = compute_hash(input, i, hash_bits, use_hash5);
                        let cand = unsafe { *head.get_unchecked(h) };
                        if cand != NIL && (cand as usize) < i && i - (cand as usize) <= window_size {
                            miss_streak = 0;
                        } else {
                            insert_hash(input, i, &mut head, &mut prev_chain, hash_bits, window_size, use_hash5);
                            continue;
                        }
                    } else { continue; }
                }

                // SA match (small files only)
                if use_sa && i + MIN_MATCH <= len {
                    let (bl, bd, sa_nl, sa_nd) = suffix_array::find_match_sa(
                        input, &sa, &isa, i, window_size, 32, MIN_MATCH);
                    if bl >= MIN_MATCH {
                        match_ml[i] = bl.min(MAX_MATCH) as u16;
                        match_md[i] = bd as u32;
                    }
                    if sa_nl >= MIN_MATCH && sa_nd > 0 && sa_nd != bd {
                        match2_ml[i] = sa_nl.min(MAX_MATCH) as u16;
                        match2_md[i] = sa_nd as u32;
                    }
                }

                // Hash chain match
                let h = compute_hash(input, i, hash_bits, use_hash5);
                let mut cp = unsafe { *head.get_unchecked(h) };
                let min_pos = i.saturating_sub(window_size);
                let max_ml_here = std::cmp::min(MAX_MATCH, len - i);
                let mut best_len = match_ml[i] as usize;
                let mut near_len = match2_ml[i] as usize;
                let mut near_dist = if match2_md[i] > 0 { match2_md[i] as usize } else { usize::MAX };
                let mut cc = 0u32;

                while cp != NIL && (cp as usize) >= min_pos && cc < chain_len {
                    let c = cp as usize;
                    if c < i {
                        let d = i - c;
                        if unsafe { *dp.add(c + best_len) == *dp.add(i + best_len) } || d < near_dist {
                            let ml = unsafe { match_length(dp, c, i, max_ml_here) };
                            if ml >= MIN_MATCH {
                                if ml > best_len {
                                    best_len = ml;
                                    match_ml[i] = ml.min(MAX_MATCH) as u16;
                                    match_md[i] = d as u32;
                                    if best_len >= 128 { break; } // early abort: long enough
                                }
                                if d < near_dist {
                                    near_len = ml;
                                    near_dist = d;
                                }
                            }
                        }
                    }
                    cp = unsafe { *prev_chain.get_unchecked(c & (window_size - 1)) };
                    cc += 1;
                }

                if near_len >= MIN_MATCH && near_dist < usize::MAX && near_dist != match_md[i] as usize {
                    match2_ml[i] = near_len.min(MAX_MATCH) as u16;
                    match2_md[i] = near_dist as u32;
                }

                if best_len >= MIN_MATCH { miss_streak = 0; } else { miss_streak += 1; }
                insert_hash(input, i, &mut head, &mut prev_chain, hash_bits, window_size, use_hash5);
            }
        }

        // Step 2: Block-based DP — process in 256KB chunks for cache efficiency.
        // Match arrays (match_ml etc.) are full-file; DP arrays are block-scoped.
        // Matches still reference the full window (8MB); only DP state is local.
        let mm = MatchArrays { ml: match_ml, md: match_md, ml2: match2_ml, md2: match2_md };
        let num_dp_passes = 2;

        for iteration in 0..num_dp_passes {
            let pr = Prices::from_huffman(&ll_prices, &dist_prices);
            tokens = dp_parse(input, &mm, &pr, false);

            // After each non-final iteration: rebuild prices from Huffman code lengths
            if iteration < num_dp_passes - 1 {
                let mut llf = [[0u32; NUM_LITLEN]; NUM_CTX];
                let mut df = [0u32; NUM_DIST];
                let mut rep = [0u32; 4];
                for t in &tokens {
                    let ctx = context_class(t.prev_byte);
                    llf[ctx][t.sym as usize] += 1;
                    if t.sym >= 257 {
                        let md = DIST_CODE_BASE[t.dist_code as usize] + t.dist_extra;
                        if md == rep[0] && rep[0] > 0 { df[REP0_SYM] += 1; continue; }
                        let sym = if md == rep[1] && rep[1] > 0 { REP1_SYM }
                            else if md == rep[2] && rep[2] > 0 { REP2_SYM }
                            else if md == rep[3] && rep[3] > 0 { REP3_SYM }
                            else { t.dist_code as usize };
                        df[sym] += 1;
                        rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = md;
                    }
                }
                llf[0][END_BLOCK as usize] += 1;
                for ctx in 0..NUM_CTX {
                    let ac = llf[ctx].iter().filter(|&&f| f > 0).count();
                    if ac < 2 { for f in llf[ctx].iter_mut().take(2) { if *f == 0 { *f = 1; } } }
                    if iteration < num_dp_passes - 2 {
                        // Early passes: use entropy-based prices for exploration
                        let total_ll: u32 = llf[ctx].iter().sum();
                        if total_ll > 0 {
                            let t = total_ll as f64;
                            for i in 0..NUM_LITLEN {
                                if llf[ctx][i] > 0 {
                                    let bits256 = (-(llf[ctx][i] as f64 / t).log2() * 256.0) as u16;
                                    ll_prices[ctx][i] = bits256.clamp(256, 3072);
                                } else {
                                    ll_prices[ctx][i] = 3072;
                                }
                            }
                        }
                    } else {
                        // Last pass: use Huffman code lengths for exact costs
                        let (real_ll, _) = huffman::build_huffman(&llf[ctx], NUM_LITLEN);
                        for i in 0..NUM_LITLEN {
                            ll_prices[ctx][i] = if real_ll[i] > 0 { (real_ll[i] as u16) * 256 } else { 12 * 256 };
                        }
                    }
                }
                let ac_d = df.iter().filter(|&&f| f > 0).count();
                if ac_d < 2 { for f in df.iter_mut().take(2) { if *f == 0 { *f = 1; } } }
                if iteration < num_dp_passes - 2 {
                    let total_d: u32 = df.iter().sum();
                    if total_d > 0 {
                        let t = total_d as f64;
                        for i in 0..NUM_DIST {
                            if df[i] > 0 {
                                let bits256 = (-(df[i] as f64 / t).log2() * 256.0) as u16;
                                dist_prices[i] = bits256.clamp(256, 3072);
                            } else {
                                dist_prices[i] = 3072;
                            }
                        }
                    }
                } else {
                    let (real_dist, _) = huffman::build_huffman(&df, NUM_DIST);
                    for i in 0..NUM_DIST {
                        dist_prices[i] = if real_dist[i] > 0 { (real_dist[i] as u16) * 256 } else { 12 * 256 };
                    }
                }
            }
        }
        // Extra DP pass priced for the adaptive LZMA backend (stats from the Huffman parse).
        let lz_pr = Prices::from_lzma_stats(&tokens, input);
        lzma_tokens = dp_parse(input, &mm, &lz_pr, true);
        let lz_pr = Prices::from_lzma_stats(&lzma_tokens, input);
        lzma_tokens = dp_parse(input, &mm, &lz_pr, true);
        let lz_pr = Prices::from_lzma_stats(&lzma_tokens, input);
        lzma_tokens = dp_parse(input, &mm, &lz_pr, true);
        let lz_pr = Prices::from_lzma_stats(&lzma_tokens, input);
        lzma_tokens = dp_parse(input, &mm, &lz_pr, true);
    } else {
        let mut prev_byte: u8 = 0;
        for &b in input {
            tokens.push(make_lit_tok(b, prev_byte));
            prev_byte = b;
        }
    }

    // Phase 4: Adaptive context encoding.
    // Estimate whether 8 context tables save more bits than the header overhead.
    let use_ctx = if tokens.len() > 2000 {
        // Count frequencies for 1-table and 8-table modes
        let mut freq1 = [0u32; NUM_LITLEN];
        let mut freq8 = [[0u32; NUM_LITLEN]; NUM_CTX];
        for t in &tokens {
            let ctx = context_class(t.prev_byte);
            freq1[t.sym as usize] += 1;
            freq8[ctx][t.sym as usize] += 1;
        }
        freq1[END_BLOCK as usize] += 1;
        freq8[0][END_BLOCK as usize] += 1;
        // Estimate bits: sum(freq * (-log2(freq/total)))
        fn entropy_bits(freq: &[u32]) -> f64 {
            let total: u32 = freq.iter().sum();
            if total == 0 { return 0.0; }
            let t = total as f64;
            freq.iter().filter(|&&f| f > 0).map(|&f| {
                let p = f as f64 / t;
                -(p.log2()) * f as f64
            }).sum()
        }
        let bits_1 = entropy_bits(&freq1);
        let bits_8: f64 = freq8.iter().map(|f| entropy_bits(f)).sum();
        // Header overhead: ~(7 * NUM_LITLEN * 0.5) bytes for 7 extra RLE tables ≈ 700 bits
        let header_overhead_bits = (NUM_CTX as f64 - 1.0) * 50.0 * 8.0; // extra tables * ~50 nibble-packed bytes each
        bits_8 + header_overhead_bits < bits_1
    } else {
        false
    };
    let num_ctx_used: usize = if use_ctx { NUM_CTX } else { 1 };

    let end_prev_byte: u8 = compute_end_prev_byte(input, &tokens);
    let end_ctx = if use_ctx { context_class(end_prev_byte) } else { 0 };

    // Count frequencies, using REP0 for repeated distances
    let mut litlen_freq = [[0u32; NUM_LITLEN]; NUM_CTX];
    let mut dist_freq = [0u32; NUM_DIST];
    {
        let mut rep = [0u32; 4];
        for t in &tokens {
            let ctx = if use_ctx { context_class(t.prev_byte) } else { 0 };
            litlen_freq[ctx][t.sym as usize] += 1;
            if t.sym >= 257 {
                let md = DIST_CODE_BASE[t.dist_code as usize] + t.dist_extra as u32;
                if md == rep[0] && rep[0] > 0 {
                    dist_freq[REP0_SYM] += 1;
                } else if md == rep[1] && rep[1] > 0 {
                    dist_freq[REP1_SYM] += 1;
                    rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = md;
                } else if md == rep[2] && rep[2] > 0 {
                    dist_freq[REP2_SYM] += 1;
                    rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = md;
                } else if md == rep[3] && rep[3] > 0 {
                    dist_freq[REP3_SYM] += 1;
                    rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = md;
                } else {
                    dist_freq[t.dist_code as usize] += 1;
                    rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = md;
                }
            }
        }
    }
    litlen_freq[end_ctx][END_BLOCK as usize] += 1;

    for ctx in 0..num_ctx_used {
        let active_ll = litlen_freq[ctx].iter().filter(|&&f| f > 0).count();
        if active_ll < 2 { for f in litlen_freq[ctx].iter_mut().take(2) { if *f == 0 { *f = 1; } } }
    }
    let active_d = dist_freq.iter().filter(|&&f| f > 0).count();
    if active_d < 2 { for f in dist_freq.iter_mut().take(2) { if *f == 0 { *f = 1; } } }

    let mut ll_lengths_ctx = [[0u8; NUM_LITLEN]; NUM_CTX];
    let mut ll_codes_ctx = [const { Vec::new() }; NUM_CTX];
    for ctx in 0..num_ctx_used {
        let (lengths, codes) = huffman::build_huffman(&litlen_freq[ctx], NUM_LITLEN);
        ll_lengths_ctx[ctx][..NUM_LITLEN].copy_from_slice(&lengths[..NUM_LITLEN]);
        ll_codes_ctx[ctx] = codes;
    }
    let (dist_lengths, dist_codes) = huffman::build_huffman(&dist_freq, NUM_DIST);

    // Header: orig_len(8) + num_tokens(4) + num_ctx_used(1) + integrity_hash(4) + window_log2(1) + RLE-encoded code lengths
    let window_log2 = window_size.trailing_zeros() as u8;
    let mut output = Vec::with_capacity(len / 2 + 1200);
    output.extend_from_slice(&(len as u64).to_le_bytes());
    output.extend_from_slice(&(tokens.len() as u32).to_le_bytes());
    output.push(num_ctx_used as u8);
    output.extend_from_slice(&integrity_hash(input).to_le_bytes()); // CRC32 for integrity
    output.push(window_log2); // window_log2 at position 17

    // RLE-encode all code lengths: litlen tables + dist table
    let mut all_lengths: Vec<u8> = Vec::with_capacity(num_ctx_used * NUM_LITLEN + NUM_DIST);
    for ctx in 0..num_ctx_used {
        all_lengths.extend_from_slice(&ll_lengths_ctx[ctx][..NUM_LITLEN]);
    }
    all_lengths.extend_from_slice(&dist_lengths[..NUM_DIST]);

    let rle = rle_encode_lengths(&all_lengths);
    output.extend_from_slice(&(rle.len() as u16).to_le_bytes());
    output.extend_from_slice(&rle);

    // Encode bitstream with REP0/REP1/REP2 for repeated distances
    let mut bw = huffman::BitWriter::new(len / 2);
    let mut rep = [0u32; 4];
    for t in &tokens {
        let sym = t.sym as usize;
        let ctx = if use_ctx { context_class(t.prev_byte) } else { 0 };
        let (code, nbits) = ll_codes_ctx[ctx][sym];
        bw.write(code as u32, nbits as u32);
        if sym >= 257 {
            if t.len_ebits > 0 { bw.write(t.len_extra as u32, t.len_ebits as u32); }
            let md = DIST_CODE_BASE[t.dist_code as usize] + t.dist_extra as u32;
            if md == rep[0] && rep[0] > 0 {
                let (dc, dnb) = dist_codes[REP0_SYM];
                bw.write(dc as u32, dnb as u32);
            } else if md == rep[1] && rep[1] > 0 {
                let (dc, dnb) = dist_codes[REP1_SYM];
                bw.write(dc as u32, dnb as u32);
                rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = md;
            } else if md == rep[2] && rep[2] > 0 {
                let (dc, dnb) = dist_codes[REP2_SYM];
                bw.write(dc as u32, dnb as u32);
                rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = md;
            } else if md == rep[3] && rep[3] > 0 {
                let (dc, dnb) = dist_codes[REP3_SYM];
                bw.write(dc as u32, dnb as u32);
                rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = md;
            } else {
                let (dc, dnb) = dist_codes[t.dist_code as usize];
                bw.write(dc as u32, dnb as u32);
                if t.dist_ebits > 0 { bw.write(t.dist_extra as u32, t.dist_ebits as u32); }
                rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = md;
            }
        }
    }
    let (ec, en) = ll_codes_ctx[end_ctx][END_BLOCK as usize];
    bw.write(ec as u32, en as u32);

    output.extend_from_slice(&bw.finish());

    // Try rANS encoding — only for medium files with structured/binary data
    // rANS header ~2KB; never wins on large text files, only on structured binary (kennedy-like)
    if len > 100_000 && len < 2_000_000 {
        let rans_output = encode_rans(&tokens, &litlen_freq, &dist_freq, num_ctx_used, use_ctx,
                                       end_ctx, len, window_log2, input);
        if rans_output.len() < output.len() {
            output = rans_output;
        }
    }

    // Try the adaptive LZMA-style backend (no table header), several lc/lp/pb settings.
    // The LZMA-priced parse always beats the Huffman parse under the LZMA backend; fall back
    // to the Huffman parse only when no LZMA parse exists (tiny inputs).
    for toks in [if lzma_tokens.is_empty() { &tokens } else { &lzma_tokens }] {
    let ops: Vec<lzma_coder::Op> = toks.iter().map(|t| {
        if t.sym < 256 { lzma_coder::Op::Lit(t.sym as u8) } else {
            let li = (t.sym - 257) as usize;
            let ml = LEN_CODE_BASE[li] as u32 + t.len_extra as u32;
            let md = DIST_CODE_BASE[t.dist_code as usize] + t.dist_extra;
            lzma_coder::Op::Match(ml, md)
        }
    }).collect();
    for &(lc, lp, pb) in LZMA_PARAM_SETS {
        let params = lzma_coder::Params { lc, lp, pb };
        let body = lzma_coder::encode(&ops, input, &params);
        if 19 + body.len() < output.len() {
            let mut o = Vec::with_capacity(19 + body.len());
            o.extend_from_slice(&(len as u64).to_le_bytes());
            o.extend_from_slice(&(tokens.len() as u32).to_le_bytes());
            o.push(0x20); // bit 5 = LZMA backend
            o.extend_from_slice(&integrity_hash(input).to_le_bytes());
            o.push(window_log2);
            o.push((lc | (lp << 4) | (pb << 6)) as u8);
            o.extend_from_slice(&body);
            output = o;
        }
    }
    }

    output
}

const LZMA_PARAM_SETS: &[(u32, u32, u32)] = &[(0, 0, 0), (2, 0, 0)];

fn encode_rans(tokens: &[Tok], litlen_freq: &[[u32; NUM_LITLEN]; NUM_CTX],
               dist_freq: &[u32; NUM_DIST], num_ctx_used: usize, use_ctx: bool,
               end_ctx: usize, orig_len: usize, window_log2: u8, input: &[u8]) -> Vec<u8> {
    // Build rANS frequency tables from actual token frequencies
    let mut ll_cum_ctx: [(Vec<u16>, Vec<u16>); NUM_CTX] = [const { (Vec::new(), Vec::new()) }; NUM_CTX];
    for ctx in 0..num_ctx_used {
        ll_cum_ctx[ctx] = rans::build_cum_freqs(&litlen_freq[ctx], NUM_LITLEN);
    }
    let (dist_cum, dist_f) = rans::build_cum_freqs(dist_freq, NUM_DIST);

    // Header (same as Huffman but with rANS flag in bit 6)
    let mut output = Vec::with_capacity(orig_len / 2 + 2000);
    output.extend_from_slice(&(orig_len as u64).to_le_bytes());
    output.extend_from_slice(&(tokens.len() as u32).to_le_bytes());
    output.push(num_ctx_used as u8 | 0x40); // bit 6 = rANS flag
    output.extend_from_slice(&integrity_hash(input).to_le_bytes());
    output.push(window_log2);

    // Store freq tables compactly: only non-zero frequencies with indices
    // Format per table: num_active(u16) + [index(u16) + freq(u16)]...
    let mut freq_data = Vec::new();
    for ctx in 0..num_ctx_used {
        let (_, ref freqs) = ll_cum_ctx[ctx];
        let active: Vec<(u16, u16)> = freqs.iter().enumerate()
            .filter(|(_, &f)| f > 0)
            .map(|(i, &f)| (i as u16, f))
            .collect();
        freq_data.extend_from_slice(&(active.len() as u16).to_le_bytes());
        for (idx, f) in &active {
            freq_data.extend_from_slice(&idx.to_le_bytes());
            freq_data.extend_from_slice(&f.to_le_bytes());
        }
    }
    // Distance freq table
    {
        let active: Vec<(u16, u16)> = dist_f.iter().enumerate()
            .filter(|(_, &f)| f > 0)
            .map(|(i, &f)| (i as u16, f))
            .collect();
        freq_data.extend_from_slice(&(active.len() as u16).to_le_bytes());
        for (idx, f) in &active {
            freq_data.extend_from_slice(&idx.to_le_bytes());
            freq_data.extend_from_slice(&f.to_le_bytes());
        }
    }
    output.extend_from_slice(&(freq_data.len() as u16).to_le_bytes());
    output.extend_from_slice(&freq_data);

    // Collect encoding operations forward, encode rANS in reverse
    struct RansOp { cum: u16, freq: u16, extra: u32, ebits: u8 }
    let mut ops: Vec<RansOp> = Vec::with_capacity(tokens.len() * 2);

    let mut rep = [0u32; 4];
    for t in tokens {
        let sym = t.sym as usize;
        let ctx = if use_ctx { context_class(t.prev_byte) } else { 0 };
        let (ref cum, ref freq) = ll_cum_ctx[ctx];
        ops.push(RansOp { cum: cum[sym], freq: freq[sym], extra: 0, ebits: 0 });

        if sym >= 257 {
            if t.len_ebits > 0 {
                ops.push(RansOp { cum: 0, freq: 0, extra: t.len_extra as u32, ebits: t.len_ebits });
            }
            let md = DIST_CODE_BASE[t.dist_code as usize] + t.dist_extra;
            let (dist_sym, dextra, debits) = if md == rep[0] && rep[0] > 0 {
                (REP0_SYM, 0u32, 0u8)
            } else if md == rep[1] && rep[1] > 0 {
                rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = md;
                (REP1_SYM, 0, 0)
            } else if md == rep[2] && rep[2] > 0 {
                rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = md;
                (REP2_SYM, 0, 0)
            } else if md == rep[3] && rep[3] > 0 {
                rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = md;
                (REP3_SYM, 0, 0)
            } else {
                rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = md;
                (t.dist_code as usize, t.dist_extra, t.dist_ebits)
            };
            ops.push(RansOp { cum: dist_cum[dist_sym], freq: dist_f[dist_sym], extra: 0, ebits: 0 });
            if debits > 0 {
                ops.push(RansOp { cum: 0, freq: 0, extra: dextra, ebits: debits });
            }
        }
    }
    // END_BLOCK
    let (ref cum, ref freq) = ll_cum_ctx[end_ctx];
    ops.push(RansOp { cum: cum[END_BLOCK as usize], freq: freq[END_BLOCK as usize], extra: 0, ebits: 0 });

    let mut enc = rans::RansEncoder::new();
    for op in ops.iter().rev() {
        if op.ebits > 0 {
            enc.encode_bits(op.extra, op.ebits as u32);
        } else {
            enc.encode(op.cum, op.freq);
        }
    }
    output.extend_from_slice(&enc.finish());
    output
}

/// Per-pass price tables for the DP (8.8 fixed-point bits).
struct Prices {
    lit: [[u32; 256]; NUM_CTX],
    /// Optional per-position literal cost (adaptive model) + per-ctx literal flag cost.
    lit_pos: Option<Vec<u32>>,
    mlen: Vec<[u32; MAX_MATCH + 1]>, // per ctx: length part of a normal match (incl. flags)
    rlen: Vec<[u32; MAX_MATCH + 1]>, // per ctx: length part of a rep match (incl. flags)
    rep: [u32; 3],
    dist: [u32; NUM_DIST],
    /// State-dependent flag costs, [s][ctx], s = 1 if the previous token was a match.
    st_lit: [[u32; 256]; 5],
    st_match: [[u32; 256]; 5],
    /// [s][lctx][k]: rep-k (k<3) / normal (k=3) selection cost.
    st_rep: [[[u32; 4]; 4]; 2],
}

impl Prices {
    fn from_huffman(ll: &[[u16; NUM_LITLEN]; NUM_CTX], dist: &[u16; NUM_DIST]) -> Self {
        let mut pr = Prices { lit: [[0; 256]; NUM_CTX], lit_pos: None, mlen: vec![[0; MAX_MATCH + 1]; NUM_CTX],
                              rlen: vec![[0; MAX_MATCH + 1]; NUM_CTX], rep: [0; 3], dist: [0; NUM_DIST],
                              st_lit: [[0; 256]; 5], st_match: [[0; 256]; 5], st_rep: [[[0; 4]; 4]; 2] };
        for c in 0..NUM_CTX {
            for b in 0..256 { pr.lit[c][b] = ll[c][b] as u32; }
            for l in MIN_MATCH..=MAX_MATCH {
                let (li, _, lb) = len_to_code(l);
                pr.mlen[c][l] = ll[c][257 + li] as u32 + lb * 256;
            }
            pr.rlen[c] = pr.mlen[c];
        }
        pr.rep = [dist[REP0_SYM] as u32, dist[REP1_SYM] as u32, dist[REP2_SYM] as u32];
        for s in 0..2 {
            for lc in 0..4 {
                pr.st_rep[s][lc] = [pr.rep[0], pr.rep[1], pr.rep[2], 0];
            }
        }
        for d in 0..NUM_DIST { pr.dist[d] = dist[d] as u32; }
        pr
    }

    /// Static approximation of the adaptive LZMA backend's costs, estimated from a parse.
    fn from_lzma_stats(tokens: &[Tok], input: &[u8]) -> Self {
        fn bits(c: f64, tot: f64) -> u32 { ((-(c / tot).log2()) * 256.0).clamp(8.0, 8192.0) as u32 }
        let mut lit_cnt = [[0u32; 256]; 8];
        let mut n_lit_8 = [[0u32; 8]; 2];
        let mut n_lit_lctx = [[0u32; 256]; 5];
        let mut n_match_lctx = [[0u32; 256]; 5];
        let mut rep_cnt = [[[0u32; 5]; 4]; 2]; // [state][lctx][rep0..rep3, normal]
        let mut st = 0usize;
        let mut lctx = 0usize;
        let mut mlen_cnt = vec![vec![0u32; MAX_MATCH + 1]; 4];
        let mut rlen_cnt = vec![vec![0u32; MAX_MATCH + 1]; 4];
        let mut mlen_all = vec![0u32; MAX_MATCH + 1];
        let mut rlen_all = vec![0u32; MAX_MATCH + 1];
        let mut slot_cnt = [0u32; NUM_DIST_CODES];
        let mut rep = [0u32; 4];
        for t in tokens {
            let c8 = (t.prev_byte >> 5) as usize;
            let cb = t.prev_byte as usize;
            let s_idx = if st == 0 { 0 } else { 1 + lctx };
            if t.sym < 256 {
                lit_cnt[c8][t.sym as usize] += 1;
                n_lit_8[st][c8] += 1;
                n_lit_lctx[s_idx][cb] += 1;
                st = 0;
                continue;
            }
            n_match_lctx[s_idx][cb] += 1;
            let li = (t.sym - 257) as usize;
            let ml = LEN_CODE_BASE[li] as usize + t.len_extra as usize;
            let md = DIST_CODE_BASE[t.dist_code as usize] + t.dist_extra;
            if let Some(k) = rep.iter().position(|&r| r == md && r > 0) {
                rep_cnt[st][lctx][k] += 1;
                rlen_cnt[lctx][ml] += 1;
                rlen_all[ml] += 1;
                let d = rep[k]; for q in (1..=k).rev() { rep[q] = rep[q - 1]; } rep[0] = d;
            } else {
                rep_cnt[st][lctx][4] += 1;
                mlen_cnt[lctx][ml] += 1;
                mlen_all[ml] += 1;
                slot_cnt[t.dist_code as usize] += 1;
                rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = md;
            }
            lctx = len_bucket(ml);
            st = 1;
        }
        fn len_prices(cnt: &[u32], all: &[u32]) -> Vec<u32> {
            let tot: f64 = cnt.iter().sum::<u32>() as f64;
            let tot_all: f64 = all.iter().sum::<u32>() as f64;
            let bucket = |l: usize| if l < MIN_MATCH + 8 { 0 } else if l < MIN_MATCH + 16 { 1 } else { 2 };
            let mut bc = [0f64; 3];
            let mut bc_all = [0f64; 3];
            for l in MIN_MATCH..=MAX_MATCH {
                bc[bucket(l)] += cnt[l] as f64;
                bc_all[bucket(l)] += all[l] as f64;
            }
            let sizes = [8.0, 8.0, (MAX_MATCH + 1 - MIN_MATCH - 16) as f64];
            let mut out = vec![0u32; MAX_MATCH + 1];
            for l in MIN_MATCH..=MAX_MATCH {
                let b = bucket(l);
                let prior_b = (bc_all[b] + 0.5) / (tot_all + 1.5);
                let pb = (bc[b] + 2.0 * prior_b) / (tot + 2.0);
                let alpha = if b == 2 { 0.05 } else { 0.5 };
                let prior_v = (all[l] as f64 + alpha) / (bc_all[b] + alpha * sizes[b]);
                let pv = (cnt[l] as f64 + 2.0 * prior_v) / (bc[b] + 2.0);
                out[l] = bits(pb * pv, 1.0);
            }
            out
        }
        let mut pr = Prices { lit: [[0; 256]; NUM_CTX], lit_pos: None, mlen: vec![[0; MAX_MATCH + 1]; 4],
                              rlen: vec![[0; MAX_MATCH + 1]; 4], rep: [0; 3], dist: [12 * 256; NUM_DIST],
                              st_lit: [[0; 256]; 5], st_match: [[0; 256]; 5], st_rep: [[[0; 4]; 4]; 2] };
        for s in 0..2 {
            let n_m_all: f64 = (0..4).map(|lc| rep_cnt[s][lc].iter().sum::<u32>()).sum::<u32>() as f64 + 2.5;
            for lc in 0..4 {
                let n_m: f64 = rep_cnt[s][lc].iter().sum::<u32>() as f64;
                for k in 0..3 {
                    let prior_k = (0..4).map(|c| rep_cnt[s][c][k]).sum::<u32>() as f64 + 0.5;
                    pr.st_rep[s][lc][k] = bits(rep_cnt[s][lc][k] as f64 + 2.0 * (prior_k / n_m_all), n_m + 2.0);
                }
                let prior_norm = (0..4).map(|c| rep_cnt[s][c][4]).sum::<u32>() as f64 + 0.5;
                pr.st_rep[s][lc][3] = bits(rep_cnt[s][lc][4] as f64 + 2.0 * (prior_norm / n_m_all), n_m + 2.0);
            }
        }
        let m_tot_all = (1..5).map(|si| (0..256).map(|c| n_lit_lctx[si][c] + n_match_lctx[si][c]).sum::<u32>()).sum::<u32>() as f64 + 1.0;
        let m_lit_all = (1..5).map(|si| (0..256).map(|c| n_lit_lctx[si][c]).sum::<u32>()).sum::<u32>() as f64 + 0.5;
        let m_prior_lit = m_lit_all / m_tot_all;

        for si in 0..5 {
            let tot_s = (0..256).map(|c| n_lit_lctx[si][c] + n_match_lctx[si][c]).sum::<u32>() as f64 + 1.0;
            let lit_s = (0..256).map(|c| n_lit_lctx[si][c]).sum::<u32>() as f64 + 0.5;
            let prior_lit = if si == 0 { lit_s / tot_s } else { (lit_s + 2.0 * m_prior_lit) / (tot_s + 2.0) };
            for c in 0..256 {
                let cnt = (n_lit_lctx[si][c] + n_match_lctx[si][c]) as f64;
                let plit = (n_lit_lctx[si][c] as f64 + 2.0 * prior_lit) / (cnt + 2.0);
                pr.st_lit[si][c] = bits(plit, 1.0);
                pr.st_match[si][c] = bits(1.0 - plit, 1.0);
            }
        }
        let n_lit_c: Vec<u32> = (0..8).map(|c| n_lit_8[0][c] + n_lit_8[1][c]).collect();
        let n_s: f64 = slot_cnt.iter().sum::<u32>() as f64;
        for d in 0..NUM_DIST_CODES { pr.dist[d] = bits(slot_cnt[d] as f64 + 0.5, n_s + 0.5 * NUM_DIST_CODES as f64); }
        for c in 0..8 {
            for b in 0..256 {
                pr.lit[c][b] = bits(lit_cnt[c][b] as f64 + 0.5, n_lit_c[c] as f64 + 128.0);
            }
        }
        for lc in 0..4 {
            let mlp = len_prices(&mlen_cnt[lc], &mlen_all);
            let rlp = len_prices(&rlen_cnt[lc], &rlen_all);
            for l in MIN_MATCH..=MAX_MATCH {
                pr.mlen[lc][l] = mlp[l];
                pr.rlen[lc][l] = rlp[l];
            }
        }
        let mut is_lit = vec![false; input.len()];
        let mut match_dist = vec![0u32; input.len()];
        let mut pos = 0usize;
        for t in tokens {
            if t.sym < 256 { is_lit[pos] = true; pos += 1; }
            else {
                pos += LEN_CODE_BASE[(t.sym - 257) as usize] as usize + t.len_extra as usize;
                if pos < input.len() { match_dist[pos] = DIST_CODE_BASE[t.dist_code as usize] + t.dist_extra; }
            }
        }
        pr.lit_pos = Some(lzma_coder::literal_costs(input, &is_lit, &match_dist, 0));
        pr
    }
}

struct MatchArrays { ml: Vec<u16>, md: Vec<u32>, ml2: Vec<u16>, md2: Vec<u32> }

/// Block-based forward DP optimal parse under the given price tables.
/// `lzma`: literal context = prev_byte >> 5 (LZMA lc=3) instead of CTX_TABLE classes.
fn dp_parse(input: &[u8], mm: &MatchArrays, pr: &Prices, lzma: bool) -> Vec<Tok> {
    let len = input.len();
    let (match_ml, match_md, match2_ml, match2_md) = (&mm.ml, &mm.md, &mm.ml2, &mm.md2);
    const DP_BLOCK: usize = 262144; // 256KB — fits in L2 cache
    let bsz = DP_BLOCK.min(len);
    let mut cost = vec![u64::MAX; bsz + MAX_MATCH + 1];
    let mut prev_info = vec![0u64; bsz + MAX_MATCH + 1];
    let mut dp_rep_arr = vec![0u32; bsz + MAX_MATCH + 1];
    let mut dp_rep1_arr = vec![0u32; bsz + MAX_MATCH + 1];
    let mut dp_rep2_arr = vec![0u32; bsz + MAX_MATCH + 1];
    let mut dp_lctx_arr = vec![0u8; bsz + MAX_MATCH + 1];
    let mut tokens: Vec<Tok> = Vec::with_capacity(len / 2);
    let mut carry_rep = [0u32; 3];
    let mut carry_lctx = 0u8;
    let mut carry_prev_byte: u8 = 0;
    let mut block_start = 0usize;

    while block_start < len {
        let block_end = (block_start + bsz).min(len);
        let blen = block_end - block_start;
        // Allow DP transitions to extend past block_end by up to MAX_MATCH
        let dp_end = (blen + MAX_MATCH).min(len - block_start);

        // Reset DP arrays for this block
        for j in 0..=dp_end { cost[j] = u64::MAX; prev_info[j] = 0;
            dp_rep_arr[j] = 0; dp_rep1_arr[j] = 0; dp_rep2_arr[j] = 0; dp_lctx_arr[j] = 0; }
        cost[0] = 0;
        dp_rep_arr[0] = carry_rep[0];
        dp_rep1_arr[0] = carry_rep[1];
        dp_rep2_arr[0] = carry_rep[2];
        dp_lctx_arr[0] = carry_lctx;

        // DP forward pass within block (local index j = absolute i - block_start)
        for j in 0..blen {
            let i = block_start + j; // absolute position in input
            let ci = cost[j];
            if ci == u64::MAX { continue; }

            let ctx = if i > 0 { if lzma { (input[i - 1] >> 5) as usize } else { CTX_TABLE[input[i - 1] as usize] as usize } } else { 0 };
            let rep_d = dp_rep_arr[j] as usize;
            let rep_d1 = dp_rep1_arr[j] as usize;
            let rep_d2 = dp_rep2_arr[j] as usize;
            let lctx = dp_lctx_arr[j] as usize;

            // Literal
            let st = (prev_info[j] >> 63) as usize;
            let st_idx = if st == 0 { 0 } else { 1 + lctx };
            let m_ctx = if lzma { if i > 0 { input[i - 1] as usize } else { 0 } } else { ctx };
            let lc = ci + pr.st_lit[st_idx][m_ctx] as u64 + match &pr.lit_pos { Some(lp) => lp[i] as u64, None => pr.lit[ctx][input[i] as usize] as u64 };
            if lc < cost[j + 1] {
                cost[j + 1] = lc;
                prev_info[j + 1] = 0;
                dp_rep_arr[j + 1] = rep_d as u32;
                dp_rep1_arr[j + 1] = rep_d1 as u32;
                dp_rep2_arr[j + 1] = rep_d2 as u32;
                dp_lctx_arr[j + 1] = lctx as u8;
            }

            // Rep-distance match
            let rep_dists = [rep_d, rep_d1, rep_d2];
            let mflag = pr.st_match[st_idx][m_ctx] as u64;
            let rep_prices_arr = [
                pr.st_rep[st][lctx][0] as u64 + mflag,
                pr.st_rep[st][lctx][1] as u64 + mflag,
                pr.st_rep[st][lctx][2] as u64 + mflag,
            ];
            for rep_slot in 0..3 {
                let rd = rep_dists[rep_slot];
                if rd == 0 || rd > i || i + MIN_MATCH > len { continue; }
                let src = i - rd;
                let dp_ptr = input.as_ptr();
                let max_rl = std::cmp::min(MAX_MATCH, len - i);
                let mut rlen = 0usize;
                let safe_rl = if max_rl >= 8 { max_rl - 7 } else { 0 };
                while rlen < safe_rl {
                    let a = unsafe { std::ptr::read_unaligned(dp_ptr.add(src + rlen) as *const u64) };
                    let b = unsafe { std::ptr::read_unaligned(dp_ptr.add(i + rlen) as *const u64) };
                    if a != b { rlen += (a ^ b).trailing_zeros() as usize / 8; break; }
                    rlen += 8;
                }
                while rlen < max_rl && unsafe { *dp_ptr.add(src + rlen) == *dp_ptr.add(i + rlen) } {
                    rlen += 1;
                }
                if rlen >= MIN_MATCH {
                    let try_lens: &[usize] = if len > 1_000_000 {
                        &[3,4,6,10,19,67,258,rlen,0]
                    } else {
                        &[3,4,5,6,7,8,10,13,19,67,258,rlen,0]
                    };
                    for &tl in try_lens {
                        if tl == 0 || tl > rlen || tl < MIN_MATCH { continue; }
                        let target = j + tl;
                        if target > dp_end { continue; }
                        let len_cost = if lzma { pr.rlen[lctx][tl] as u64 } else { pr.rlen[ctx][tl] as u64 };
                        let mc = ci + len_cost + rep_prices_arr[rep_slot];
                        if mc < cost[target] {
                            cost[target] = mc;
                            prev_info[target] = 0x8000_0000_0000_0000u64 | ((tl as u64) << 32) | (rd as u64);
                            dp_rep_arr[target] = rd as u32;
                            dp_rep1_arr[target] = if rep_slot == 0 { rep_d1 as u32 } else { rep_d as u32 };
                            dp_rep2_arr[target] = if rep_slot <= 1 { rep_d2 as u32 } else { rep_d1 as u32 };
                            dp_lctx_arr[target] = len_bucket(tl) as u8;
                        }
                    }
                }
            }

            // Normal match
            for slot in 0..2 {
                let ml = if slot == 0 { match_ml[i] as usize } else { match2_ml[i] as usize };
                if ml < MIN_MATCH { continue; }
                let md = if slot == 0 { match_md[i] as usize } else { match2_md[i] as usize };
                let (di, _, db) = dist_to_code(md);
                let dist_cost = pr.dist[di] as u64 + (db as u64) * 256 + pr.st_rep[st][lctx][3] as u64 + mflag;

                let try_lens: &[usize] = if len > 1_000_000 {
                    // Sparse for large files (speed)
                    &[3,4,5,6,7,8,10,13,19,31,67,131,258,ml,0]
                } else {
                    // Dense for small files (ratio)
                    &[3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,
                      21,22,23,24,25,26,27,28,29,30,31,32,
                      43,67,131,258,515,ml,0]
                };
                for &tl in try_lens {
                    if tl == 0 || tl > ml || tl < MIN_MATCH { continue; }
                    let target = j + tl;
                    if target > dp_end { continue; }
                    let len_cost = if lzma { pr.mlen[lctx][tl] as u64 } else { pr.mlen[ctx][tl] as u64 };
                    let mc = ci + len_cost + dist_cost;
                    if mc < cost[target] {
                        cost[target] = mc;
                        prev_info[target] = 0x8000_0000_0000_0000u64 | ((tl as u64) << 32) | (md as u64);
                        dp_rep2_arr[target] = rep_d1 as u32;
                        dp_rep1_arr[target] = rep_d as u32;
                        dp_rep_arr[target] = md as u32;
                        dp_lctx_arr[target] = len_bucket(tl) as u8;
                    }
                }
            }
        }

        // Backtrack within this block
        let mut path: Vec<u64> = Vec::new();
        let mut cur = blen; // backtrack from block end
        while cur > 0 {
            let info = prev_info[cur];
            path.push(info);
            if info & 0x8000_0000_0000_0000 != 0 {
                cur -= ((info >> 32) & 0x7FFF_FFFF) as usize;
            } else { cur -= 1; }
        }
        path.reverse();

        let mut pos = block_start;
        let mut prev_byte = carry_prev_byte;
        for &info in &path {
            if info & 0x8000_0000_0000_0000 != 0 {
                let ml = ((info >> 32) & 0x7FFF_FFFF) as usize;
                let md = (info & 0xFFFF_FFFF) as usize;
                tokens.push(make_match_tok(ml, md, prev_byte));
                let match_source = pos - md;
                prev_byte = input[match_source + ml - 1];
                pos += ml;
            } else {
                tokens.push(make_lit_tok(input[pos], prev_byte));
                prev_byte = input[pos];
                pos += 1;
            }
        }

        // Carry over state for next block
        carry_rep[0] = dp_rep_arr[blen];
        carry_rep[1] = dp_rep1_arr[blen];
        carry_rep[2] = dp_rep2_arr[blen];
        carry_lctx = dp_lctx_arr[blen];
        carry_prev_byte = prev_byte;
        block_start = pos; // advance past whatever the backtrack consumed
    }
    tokens
}

fn compute_end_prev_byte(input: &[u8], tokens: &[Tok]) -> u8 {
    let mut pos = 0usize;
    let mut last: u8 = 0;
    for t in tokens {
        if t.sym < 256 {
            last = t.sym as u8;
            pos += 1;
        } else if t.sym >= 257 {
            // Match
            let li = (t.sym - 257) as usize;
            let ml = LEN_CODE_BASE[li] as usize + t.len_extra as usize;
            let md = DIST_CODE_BASE[t.dist_code as usize] as usize + t.dist_extra as usize;
            let match_source = pos - md;
            last = input[match_source + ml - 1];
            pos += ml;
        }
    }
    last
}

// Huffman functions moved to huffman.rs

pub fn decompress(compressed: &[u8]) -> Result<Vec<u8>, String> {
    if compressed.len() < 13 {
        return Err("data too short".into());
    }
    let orig_len = u64::from_le_bytes(compressed[0..8].try_into().unwrap()) as usize;
    let _num_tokens = u32::from_le_bytes(compressed[8..12].try_into().unwrap()) as usize;
    let raw_ctx_byte = compressed[12];
    let is_reversed = raw_ctx_byte & 0x80 != 0;
    let use_rans = raw_ctx_byte & 0x40 != 0;
    let num_ctx_used = (raw_ctx_byte & 0x1F) as usize;
    if raw_ctx_byte & 0x20 != 0 {
        if compressed.len() < 19 { return Err("truncated lzma header".into()); }
        let expected = u32::from_le_bytes(compressed[13..17].try_into().unwrap());
        let pbyte = compressed[18] as u32;
        let params = lzma_coder::Params { lc: pbyte & 0xF, lp: (pbyte >> 4) & 3, pb: pbyte >> 6 };
        let mut out = lzma_coder::decode(&compressed[19..], orig_len, &params)?;
        if integrity_hash(&out) != expected { return Err("lzma: checksum mismatch".into()); }
        if is_reversed { out.reverse(); }
        return Ok(out);
    }
    if num_ctx_used != 1 && num_ctx_used != NUM_CTX {
        return Err(format!("invalid num_ctx_used: {}", num_ctx_used));
    }

    let use_ctx = num_ctx_used == NUM_CTX;

    // Read CRC32 from header
    let expected_crc = u32::from_le_bytes(compressed[13..17].try_into().unwrap());

    // Read window_log2 from header
    if compressed.len() < 18 { return Err("data too short for window_log2".into()); }
    let window_log2 = compressed[17];
    let _window_size = 1usize << window_log2;

    let mut offset = 18; // after orig_len(8) + num_tokens(4) + ctx_byte(1) + integrity_hash(4) + window_log2(1)

    // Read RLE-encoded code lengths (only for Huffman format)
    let mut ll_lengths_ctx = [[0u8; NUM_LITLEN]; NUM_CTX];
    let mut dist_lengths = vec![0u8; NUM_DIST];
    if !use_rans {
        if offset + 2 > compressed.len() { return Err("truncated rle header".into()); }
        let rle_len = u16::from_le_bytes(compressed[offset..offset+2].try_into().unwrap()) as usize;
        offset += 2;
        if offset + rle_len > compressed.len() { return Err("truncated rle data".into()); }
        let expected_total = num_ctx_used * NUM_LITLEN + NUM_DIST;
        let all_lengths = rle_decode_lengths(&compressed[offset..offset + rle_len], expected_total);
        offset += rle_len;

        for ctx in 0..num_ctx_used {
            let start = ctx * NUM_LITLEN;
            ll_lengths_ctx[ctx].copy_from_slice(&all_lengths[start..start + NUM_LITLEN]);
        }
        let dist_start = num_ctx_used * NUM_LITLEN;
        dist_lengths.copy_from_slice(&all_lengths[dist_start..dist_start + NUM_DIST]);
    }

    let mut output = Vec::with_capacity(orig_len);
    let mut prev_byte: u8 = 0;
    let mut rep = [1usize; 4];

    if use_rans {
        // Read rANS frequency tables (sparse format)
        if offset + 2 > compressed.len() { return Err("truncated rans freq len".into()); }
        let freq_data_len = u16::from_le_bytes(compressed[offset..offset+2].try_into().unwrap()) as usize;
        offset += 2;
        if offset + freq_data_len > compressed.len() { return Err("truncated rans freq data".into()); }

        let mut ll_cum_ctx: [(Vec<u16>, Vec<u16>); NUM_CTX] = [const { (Vec::new(), Vec::new()) }; NUM_CTX];
        let mut foff = offset;
        for ctx in 0..num_ctx_used {
            let n_active = u16::from_le_bytes(compressed[foff..foff+2].try_into().unwrap()) as usize;
            foff += 2;
            let mut freqs = vec![0u16; NUM_LITLEN];
            for _ in 0..n_active {
                let idx = u16::from_le_bytes(compressed[foff..foff+2].try_into().unwrap()) as usize;
                foff += 2;
                let f = u16::from_le_bytes(compressed[foff..foff+2].try_into().unwrap());
                foff += 2;
                if idx >= NUM_LITLEN { return Err(format!("rans litlen idx {} >= {}", idx, NUM_LITLEN)); }
                freqs[idx] = f;
            }
            let mut cum = vec![0u16; NUM_LITLEN + 1];
            for i in 0..NUM_LITLEN { cum[i+1] = cum[i] + freqs[i]; }
            ll_cum_ctx[ctx] = (cum, freqs);
        }
        // Distance freqs
        let n_active = u16::from_le_bytes(compressed[foff..foff+2].try_into().unwrap()) as usize;
        foff += 2;
        let mut dist_f = vec![0u16; NUM_DIST];
        for _ in 0..n_active {
            let idx = u16::from_le_bytes(compressed[foff..foff+2].try_into().unwrap()) as usize;
            foff += 2;
            let f = u16::from_le_bytes(compressed[foff..foff+2].try_into().unwrap());
            foff += 2;
            dist_f[idx] = f;
        }
        let mut dist_cum = vec![0u16; NUM_DIST + 1];
        for i in 0..NUM_DIST { dist_cum[i+1] = dist_cum[i] + dist_f[i]; }
        offset += freq_data_len;

        let mut dec = rans::RansDecoder::new(&compressed[offset..]);

        loop {
            let ctx = if use_ctx { context_class(prev_byte) } else { 0 };
            let (ref cum, ref freq) = ll_cum_ctx[ctx];
            let sym = dec.decode_symbol(cum, freq) as u16;

            if sym < 256 {
                prev_byte = sym as u8;
                output.push(sym as u8);
            } else if sym == END_BLOCK {
                break;
            } else {
                let li = (sym - 257) as usize;
                let base_len = LEN_CODE_BASE[li] as usize;
                let extra_bits = LEN_EXTRA_BITS[li] as u32;
                let length = base_len + if extra_bits > 0 { dec.decode_bits(extra_bits) as usize } else { 0 };

                let di = dec.decode_symbol(&dist_cum, &dist_f);
                let distance = if di == REP0_SYM {
                    rep[0]
                } else if di == REP1_SYM {
                    let d = rep[1]; rep[3]=rep[2]; rep[2]=rep[1]; rep[1]=rep[0]; rep[0]=d; d
                } else if di == REP2_SYM {
                    let d = rep[2]; rep[3]=rep[2]; rep[2]=rep[1]; rep[1]=rep[0]; rep[0]=d; d
                } else if di == REP3_SYM {
                    let d = rep[3]; rep[3]=rep[2]; rep[2]=rep[1]; rep[1]=rep[0]; rep[0]=d; d
                } else {
                    let base_dist = DIST_CODE_BASE[di] as usize;
                    let dextra = DIST_EXTRA_BITS[di] as u32;
                    let d = base_dist + if dextra > 0 { dec.decode_bits(dextra) as usize } else { 0 };
                    rep[3]=rep[2]; rep[2]=rep[1]; rep[1]=rep[0]; rep[0]=d; d
                };

                if distance == 0 || distance > output.len() {
                    return Err(format!("invalid distance {} at pos {}", distance, output.len()));
                }
                let start = output.len() - distance;
                output.reserve(length);
                unsafe {
                    let ptr = output.as_mut_ptr();
                    let op = output.len();
                    if distance >= length {
                        std::ptr::copy_nonoverlapping(ptr.add(start), ptr.add(op), length);
                    } else {
                        for j in 0..length { *ptr.add(op + j) = *ptr.add(start + j); }
                    }
                    output.set_len(op + length);
                }
                prev_byte = output[output.len() - 1];
            }
        }
    } else {
        // Huffman decode path (original)
        let mut ll_tables: [Vec<(u16, u8)>; NUM_CTX] = [const { Vec::new() }; NUM_CTX];
        for ctx in 0..num_ctx_used {
            ll_tables[ctx] = huffman::build_decode_table(&ll_lengths_ctx[ctx]);
        }
        let dist_table = huffman::build_decode_table(&dist_lengths);
        let mask = (1u32 << huffman::MAX_BITS) - 1;

        let mut br = huffman::BitReader::new(&compressed[offset..]);

        loop {
            let ctx = if use_ctx { context_class(prev_byte) } else { 0 };
            let ll_table = &ll_tables[ctx];

            let bits = br.peek(huffman::MAX_BITS as u32);
            let (sym, nbits) = ll_table[(bits & mask) as usize];
            br.consume(nbits as u32);

        if sym < 256 {
            prev_byte = sym as u8;
            output.push(sym as u8);
        } else if sym == END_BLOCK {
            break;
        } else {
            let li = (sym - 257) as usize;
            let base_len = LEN_CODE_BASE[li] as usize;
            let extra_bits = LEN_EXTRA_BITS[li] as u32;
            let length = base_len + if extra_bits > 0 { br.read(extra_bits) as usize } else { 0 };

            let dbits = br.peek(huffman::MAX_BITS as u32);
            let (dsym, dnbits) = dist_table[(dbits & mask) as usize];
            br.consume(dnbits as u32);
            let di = dsym as usize;
            let distance = if di == REP0_SYM {
                rep[0]
            } else if di == REP1_SYM {
                let d = rep[1];
                rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = d;
                d
            } else if di == REP2_SYM {
                let d = rep[2];
                rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = d;
                d
            } else if di == REP3_SYM {
                let d = rep[3];
                rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = d;
                d
            } else {
                let base_dist = DIST_CODE_BASE[di] as usize;
                let dextra = DIST_EXTRA_BITS[di] as u32;
                let d = base_dist + if dextra > 0 { br.read(dextra) as usize } else { 0 };
                rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = d;
                d
            };

            if distance == 0 || distance > output.len() {
                return Err(format!("invalid distance {} at pos {}", distance, output.len()));
            }
            let start = output.len() - distance;
            output.reserve(length);
            unsafe {
                let ptr = output.as_mut_ptr();
                let op = output.len();
                if distance >= length {
                    std::ptr::copy_nonoverlapping(ptr.add(start), ptr.add(op), length);
                } else {
                    for j in 0..length { *ptr.add(op + j) = *ptr.add(start + j); }
                }
                output.set_len(op + length);
            }
            // Update prev_byte to last byte of match output
            prev_byte = output[output.len() - 1];
        }
    }
    } // end else (Huffman path)

    if output.len() != orig_len {
        return Err(format!("length mismatch: expected {}, got {}", orig_len, output.len()));
    }

    // Verify CRC32 integrity (CRC was computed on the input to compress_inner,
    // which is reversed if is_reversed=true — so check BEFORE reversing)
    let computed_crc = integrity_hash(&output);
    if expected_crc != computed_crc {
        return Err(format!(
            "CRC32 mismatch: stored 0x{:08X}, computed 0x{:08X}",
            expected_crc, computed_crc
        ));
    }

    if is_reversed {
        output.reverse();
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_empty() {
        let c = compress(b"");
        assert_eq!(b"".as_slice(), decompress(&c).unwrap().as_slice());
    }

    #[test]
    fn roundtrip_short() {
        let data = b"ab";
        let c = compress(data);
        assert_eq!(data.as_slice(), decompress(&c).unwrap().as_slice());
    }

    #[test]
    fn roundtrip_repeated() {
        let data = b"abcabcabcabcabcabcabcabc";
        let c = compress(data);
        assert_eq!(data.as_slice(), decompress(&c).unwrap().as_slice());
    }

    macro_rules! roundtrip_test {
        ($name:ident, $path:expr) => {
            #[test]
            fn $name() {
                let data = include_bytes!($path);
                let c = compress(data);
                assert_eq!(data.as_slice(), decompress(&c).unwrap().as_slice());
            }
        };
    }

    roundtrip_test!(roundtrip_alice,     "../data/alice29.txt");
    roundtrip_test!(roundtrip_asyoulik,  "../data/asyoulik.txt");
    roundtrip_test!(roundtrip_fields,    "../data/fields.c");
    roundtrip_test!(roundtrip_cp_html,   "../data/cp.html");
    roundtrip_test!(roundtrip_kennedy,   "../data/kennedy.xls");
    roundtrip_test!(roundtrip_plrabn,    "../data/plrabn12.txt");
    roundtrip_test!(roundtrip_urls,      "../data/urls.10K");
    roundtrip_test!(roundtrip_geo,       "../data/geo.protodata");
    roundtrip_test!(roundtrip_jpeg,      "../data/fireworks.jpeg");
    roundtrip_test!(roundtrip_random,    "../data/random_org_10k.bin");
    roundtrip_test!(roundtrip_franko_lys,    "../data/franko-ivan-iakovych-farbovanyy-lys652.rtf");
    roundtrip_test!(roundtrip_franko_berkut, "../data/franko-ivan-iakovych-zakhar-berkut645.html");

    #[test]
    fn compression_ratio_alice() {
        let data = include_bytes!("../data/alice29.txt");
        let c = compress(data);
        let ratio = c.len() as f64 / data.len() as f64;
        assert!(ratio < 0.7, "ratio {} too high", ratio);
    }
}
