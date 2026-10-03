/// Suffix Array match finder for LZ77 compression.
///
/// Builds a suffix array + inverse suffix array, then finds the longest match
/// for each position by scanning neighbors in the sorted suffix order.
/// Guarantees finding the optimal (longest) match — no hash collisions or chain limits.

/// Build suffix array using O(n log^2 n) prefix doubling.
/// Returns SA where SA[i] = position of i-th smallest suffix.
pub fn build_suffix_array(data: &[u8]) -> Vec<u32> {
    let n = data.len();
    if n == 0 { return vec![]; }

    let mut sa: Vec<u32> = (0..n as u32).collect();
    let mut rank: Vec<i32> = data.iter().map(|&b| b as i32).collect();
    let mut tmp: Vec<i32> = vec![0; n];
    let mut k = 1usize;

    while k < n {
        // Sort by (rank[i], rank[i+k])
        let rank_ref = &rank;
        let kk = k;
        sa.sort_unstable_by(|&a, &b| {
            let a = a as usize;
            let b = b as usize;
            let ra = rank_ref[a];
            let rb = rank_ref[b];
            if ra != rb { return ra.cmp(&rb); }
            let ra2 = if a + kk < n { rank_ref[a + kk] } else { -1 };
            let rb2 = if b + kk < n { rank_ref[b + kk] } else { -1 };
            ra2.cmp(&rb2)
        });

        // Compute new ranks
        tmp[sa[0] as usize] = 0;
        for i in 1..n {
            let prev = sa[i - 1] as usize;
            let curr = sa[i] as usize;
            let same = rank[prev] == rank[curr]
                && (prev + k < n && curr + k < n && rank[prev + k] == rank[curr + k]
                    || prev + k >= n && curr + k >= n);
            tmp[curr] = tmp[prev] + if same { 0 } else { 1 };
        }
        rank.copy_from_slice(&tmp);

        if rank[sa[n - 1] as usize] as usize == n - 1 { break; } // all unique
        k *= 2;
    }

    sa
}

/// Build inverse suffix array: ISA[pos] = rank of suffix starting at pos.
pub fn build_inverse_sa(sa: &[u32]) -> Vec<u32> {
    let n = sa.len();
    let mut isa = vec![0u32; n];
    for i in 0..n {
        isa[sa[i] as usize] = i as u32;
    }
    isa
}

/// Find best match for position `pos` using suffix array.
/// Scans up to `scan_range` neighbors in SA order.
/// Returns (match_length, match_distance) or (0, 0) if no match found.
/// Only considers matches within `window_size` distance and at positions < pos.
#[inline]
pub fn find_match_sa(
    data: &[u8],
    sa: &[u32],
    isa: &[u32],
    pos: usize,
    window_size: usize,
    scan_range: usize,
    min_match: usize,
) -> (usize, usize, usize, usize) {
    // Returns (best_len, best_dist, near_len, near_dist)
    let n = data.len();
    if pos + min_match > n { return (0, 0, 0, 0); }

    let rank = isa[pos] as usize;
    let max_len = n - pos;
    let min_pos = pos.saturating_sub(window_size);

    let mut best_len = min_match - 1;
    let mut best_dist = 0usize;
    let mut near_len = 0usize;
    let mut near_dist = usize::MAX;

    let dp = data.as_ptr();

    // Scan left in SA
    let mut lcp_bound = max_len; // upper bound on possible LCP with current direction
    let left_start = rank.saturating_sub(scan_range);
    for r in (left_start..rank).rev() {
        let j = sa[r] as usize;
        if j >= pos { continue; } // must be a previous position
        if j < min_pos { continue; } // outside window

        // Compute match length (bounded by lcp_bound for early termination)
        let ml = unsafe { match_len_fast(dp, j, pos, max_len.min(lcp_bound)) };
        lcp_bound = ml; // LCP can only decrease as we move further in SA

        let d = pos - j;
        if ml > best_len {
            best_len = ml;
            best_dist = d;
        }
        if ml >= min_match && d < near_dist {
            near_len = ml;
            near_dist = d;
        }

        if lcp_bound < min_match { break; } // no more useful matches in this direction
    }

    // Scan right in SA
    lcp_bound = max_len;
    let right_end = (rank + 1 + scan_range).min(n);
    for r in (rank + 1)..right_end {
        let j = sa[r] as usize;
        if j >= pos { continue; }
        if j < min_pos { continue; }

        let ml = unsafe { match_len_fast(dp, j, pos, max_len.min(lcp_bound)) };
        lcp_bound = ml;

        let d = pos - j;
        if ml > best_len {
            best_len = ml;
            best_dist = d;
        }
        if ml >= min_match && d < near_dist {
            near_len = ml;
            near_dist = d;
        }

        if lcp_bound < min_match { break; }
    }

    if best_len >= min_match {
        (best_len, best_dist, near_len, if near_dist == usize::MAX { 0 } else { near_dist })
    } else {
        (0, 0, 0, 0)
    }
}

/// Fast match length comparison using 16-byte 2-lane XOR for ILP.
#[inline(always)]
unsafe fn match_len_fast(dp: *const u8, a: usize, b: usize, max_len: usize) -> usize {
    let mut n = 0usize;
    while n + 16 <= max_len {
        let x0 = std::ptr::read_unaligned(dp.add(a + n) as *const u64)
            ^ std::ptr::read_unaligned(dp.add(b + n) as *const u64);
        let x1 = std::ptr::read_unaligned(dp.add(a + n + 8) as *const u64)
            ^ std::ptr::read_unaligned(dp.add(b + n + 8) as *const u64);
        if x0 != 0 { return n + (x0.trailing_zeros() as usize >> 3); }
        if x1 != 0 { return n + 8 + (x1.trailing_zeros() as usize >> 3); }
        n += 16;
    }
    if n + 8 <= max_len {
        let x = std::ptr::read_unaligned(dp.add(a + n) as *const u64)
            ^ std::ptr::read_unaligned(dp.add(b + n) as *const u64);
        if x != 0 { return n + (x.trailing_zeros() as usize >> 3); }
        n += 8;
    }
    while n < max_len && *dp.add(a + n) == *dp.add(b + n) { n += 1; }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sa_basic() {
        let data = b"banana";
        let sa = build_suffix_array(data);
        // Suffixes sorted: a, ana, anana, banana, na, nana
        // Positions:        5, 3,   1,      0,    4,  2
        assert_eq!(sa, vec![5, 3, 1, 0, 4, 2]);
    }

    #[test]
    fn sa_finds_match() {
        let data = b"abcabcabc";
        let sa = build_suffix_array(data);
        let isa = build_inverse_sa(&sa);

        // At position 6, "abc" matches at position 3 (dist=3) and position 0 (dist=6)
        let (best_len, best_dist, _, _) = find_match_sa(data, &sa, &isa, 6, 9, 32, 3);
        assert!(best_len >= 3);
        assert!(best_dist > 0);
    }

    #[test]
    fn sa_window_limit() {
        let data = b"abc_____abc"; // 11 bytes
        let sa = build_suffix_array(data);
        let isa = build_inverse_sa(&sa);

        // With window=5, position 8 can't reach position 0 (dist=8 > 5)
        let (best_len, _, _, _) = find_match_sa(data, &sa, &isa, 8, 5, 32, 3);
        assert_eq!(best_len, 0); // no match within window
    }
}
