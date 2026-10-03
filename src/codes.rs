/// DEFLATE-style length and distance code tables.

pub const NUM_LITLEN: usize = 288; // 256 literals + END_BLOCK + 31 length codes
pub const NUM_DIST: usize = 50;    // 0-45 = distance codes (up to 8MB), 46-49 = REP0-REP3
pub const NUM_DIST_CODES: usize = 46; // regular distance codes (not including REP)
pub const REP0_SYM: usize = 46;
pub const REP1_SYM: usize = 47;
pub const REP2_SYM: usize = 48;
pub const REP3_SYM: usize = 49;
pub const END_BLOCK: u16 = 256;

pub const MIN_MATCH: usize = 3;
pub const MAX_MATCH: usize = 1026;

// Length code tables (extended beyond DEFLATE's 258)
pub const LEN_CODE_BASE: [u16; 31] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31,
    35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258,
    259, 515,
];
pub const LEN_EXTRA_BITS: [u8; 31] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2,
    3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    8, 9,
];

// Distance code tables — standard DEFLATE (original, proven optimal)
pub const DIST_CODE_BASE: [u32; 46] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193,
    257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    32769, 49153, 65537, 98305, 131073, 196609, 262145, 393217, 524289, 786433,
    1048577, 1572865, 2097153, 3145729, 4194305, 6291457,
];
pub const DIST_EXTRA_BITS: [u8; 46] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6,
    7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13,
    14, 14, 15, 15, 16, 16, 17, 17, 18, 18,
    19, 19, 20, 20, 21, 21,
];

/// Precomputed LUT for len_to_code: index by (length - MIN_MATCH).
pub static LEN_LUT: [(u8, u16, u8); 1024] = {
    let mut lut = [(0u8, 0u16, 0u8); 1024];
    let mut l = 3u16;
    while l <= 1026 {
        let idx = (l - 3) as usize;
        let mut code = 0u8;
        let mut i = 30u8;
        loop {
            if l >= LEN_CODE_BASE[i as usize] {
                code = i;
                break;
            }
            if i == 0 { break; }
            i -= 1;
        }
        lut[idx] = (code, l - LEN_CODE_BASE[code as usize], LEN_EXTRA_BITS[code as usize]);
        l += 1;
    }
    lut
};

#[inline(always)]
pub fn len_to_code(length: usize) -> (usize, u32, u32) {
    let e = LEN_LUT[length - MIN_MATCH];
    (e.0 as usize, e.1 as u32, e.2 as u32)
}

/// O(1) distance-to-code via leading_zeros (replaces linear scan of 36 entries).
/// DEFLATE distance codes follow a log2 pattern: code = 2*msb + bit_below_msb - 2.
#[inline(always)]
pub fn dist_to_code(distance: usize) -> (usize, u32, u32) {
    let d = distance as u32;
    if d <= 4 {
        let code = (d - 1) as usize;
        return (code, 0, 0);
    }
    // For d >= 5: code based on position of highest set bit
    let msb = 31 - (d - 1).leading_zeros(); // floor(log2(d-1))
    let code = ((msb as usize) * 2) + (((d - 1) >> (msb - 1)) & 1) as usize;
    // Clamp to valid range for extended codes
    let code = code.min(NUM_DIST_CODES - 1);
    (code, d - DIST_CODE_BASE[code], DIST_EXTRA_BITS[code] as u32)
}
