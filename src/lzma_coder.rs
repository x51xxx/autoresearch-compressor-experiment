/// LZMA-style backend: adaptive binary range coder over the LZ77 token stream.
///
/// No code-length / frequency header — all models adapt as they go. Uses the LZMA
/// 12-state machine, order-lc literal contexts, matched-literal coding after a match
/// (byte at rep0 as side information), REP0-REP3 and LZMA distance slots.

const PROB_BITS: u32 = 12;
const PROB_INIT: u16 = 1 << (PROB_BITS - 1);
const MOVE_BITS: u32 = 5;
const TOP: u32 = 1 << 24;

const NUM_STATES: usize = 12;
const LEN_LOW_BITS: usize = 3;
const LEN_MID_BITS: usize = 3;
const LEN_HIGH_BITS: usize = 10;
const LEN_LOW: usize = 1 << LEN_LOW_BITS;
const LEN_MID: usize = 1 << LEN_MID_BITS;
const MIN_LEN: usize = 3;
const NUM_LEN_STATES: usize = 4;
const NUM_SLOTS: usize = 64;
const END_POS_MODEL: usize = 14;
const ALIGN_BITS: usize = 4;

pub enum Op {
    Lit(u8),
    Match(u32, u32), // (len, dist)
}

pub struct Params { pub lc: u32, pub lp: u32, pub pb: u32 }

// ---------------------------------------------------------------- range coder

struct Enc { low: u64, range: u32, cache: u8, cache_size: u64, out: Vec<u8> }

impl Enc {
    fn new() -> Self { Enc { low: 0, range: 0xFFFF_FFFF, cache: 0, cache_size: 1, out: Vec::new() } }

    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
            let carry = (self.low >> 32) as u8;
            let mut temp = self.cache;
            loop {
                self.out.push(temp.wrapping_add(carry));
                temp = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 { break; }
            }
            self.cache = ((self.low >> 24) & 0xFF) as u8;
        }
        self.cache_size += 1;
        self.low = (self.low & 0x00FF_FFFF) << 8;
    }

    #[inline]
    fn bit(&mut self, p: &mut u16, bit: u32) {
        let bound = (self.range >> PROB_BITS) * (*p as u32);
        if bit == 0 {
            self.range = bound;
            *p += ((1 << PROB_BITS) - *p) >> MOVE_BITS;
        } else {
            self.low += bound as u64;
            self.range -= bound;
            *p -= *p >> MOVE_BITS;
        }
        while self.range < TOP { self.range <<= 8; self.shift_low(); }
    }

    /// Code `bit` with explicit P(bit==1) = p1 / 4096 (p1 in 1..4095).
    #[inline]
    fn bit_p(&mut self, p1: u32, bit: u32) {
        let bound = (self.range >> 12) * (4096 - p1);
        if bit == 0 { self.range = bound; } else { self.low += bound as u64; self.range -= bound; }
        while self.range < TOP { self.range <<= 8; self.shift_low(); }
    }

    fn direct(&mut self, v: u32, n: usize) {
        for i in (0..n).rev() {
            self.range >>= 1;
            if (v >> i) & 1 != 0 { self.low += self.range as u64; }
            while self.range < TOP { self.range <<= 8; self.shift_low(); }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 { self.shift_low(); }
        self.out
    }
}

struct Dec<'a> { range: u32, code: u32, data: &'a [u8], pos: usize }

impl<'a> Dec<'a> {
    fn new(data: &'a [u8]) -> Self {
        let mut d = Dec { range: 0xFFFF_FFFF, code: 0, data, pos: 0 };
        for _ in 0..5 { d.code = (d.code << 8) | d.next() as u32; }
        d
    }

    #[inline]
    fn next(&mut self) -> u8 {
        let b = if self.pos < self.data.len() { self.data[self.pos] } else { 0 };
        self.pos += 1;
        b
    }

    #[inline]
    fn bit(&mut self, p: &mut u16) -> u32 {
        let bound = (self.range >> PROB_BITS) * (*p as u32);
        let b = if self.code < bound {
            self.range = bound;
            *p += ((1 << PROB_BITS) - *p) >> MOVE_BITS;
            0
        } else {
            self.code -= bound;
            self.range -= bound;
            *p -= *p >> MOVE_BITS;
            1
        };
        while self.range < TOP { self.range <<= 8; self.code = (self.code << 8) | self.next() as u32; }
        b
    }

    #[inline]
    fn bit_p(&mut self, p1: u32) -> u32 {
        let bound = (self.range >> 12) * (4096 - p1);
        let b = if self.code < bound { self.range = bound; 0 } else { self.code -= bound; self.range -= bound; 1 };
        while self.range < TOP { self.range <<= 8; self.code = (self.code << 8) | self.next() as u32; }
        b
    }

    fn direct(&mut self, n: usize) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            self.range >>= 1;
            let b = if self.code >= self.range { self.code -= self.range; 1 } else { 0 };
            v = (v << 1) | b;
            while self.range < TOP { self.range <<= 8; self.code = (self.code << 8) | self.next() as u32; }
        }
        v
    }
}

// ---------------------------------------------------------------- bit trees

fn tree_enc(e: &mut Enc, probs: &mut [u16], nbits: usize, v: u32) {
    let mut m = 1usize;
    for i in (0..nbits).rev() {
        let b = (v >> i) & 1;
        e.bit(&mut probs[m], b);
        m = (m << 1) | b as usize;
    }
}

fn tree_dec(d: &mut Dec, probs: &mut [u16], nbits: usize) -> u32 {
    let mut m = 1usize;
    for _ in 0..nbits { m = (m << 1) | d.bit(&mut probs[m]) as usize; }
    (m - (1 << nbits)) as u32
}

fn rtree_enc(e: &mut Enc, probs: &mut [u16], nbits: usize, mut v: u32) {
    let mut m = 1usize;
    for _ in 0..nbits {
        let b = v & 1;
        v >>= 1;
        e.bit(&mut probs[m], b);
        m = (m << 1) | b as usize;
    }
}

fn rtree_dec(d: &mut Dec, probs: &mut [u16], nbits: usize) -> u32 {
    let mut m = 1usize;
    let mut v = 0u32;
    for i in 0..nbits {
        let b = d.bit(&mut probs[m]);
        m = (m << 1) | b as usize;
        v |= b << i;
    }
    v
}

// ---------------------------------------------------------------- models

struct LenModel { choice: u16, choice2: u16, low: Vec<[u16; LEN_LOW]>, mid: Vec<[u16; LEN_MID]>, high: Vec<u16> }

impl LenModel {
    fn new(pos_states: usize) -> Self {
        LenModel { choice: PROB_INIT, choice2: PROB_INIT,
                   low: vec![[PROB_INIT; LEN_LOW]; pos_states], mid: vec![[PROB_INIT; LEN_MID]; pos_states],
                   high: vec![PROB_INIT; 1 << LEN_HIGH_BITS] }
    }
    fn enc(&mut self, e: &mut Enc, len: usize, ps: usize) {
        let v = (len - MIN_LEN) as u32;
        if v < LEN_LOW as u32 {
            e.bit(&mut self.choice, 0);
            tree_enc(e, &mut self.low[ps], LEN_LOW_BITS, v);
        } else if v < (LEN_LOW + LEN_MID) as u32 {
            e.bit(&mut self.choice, 1);
            e.bit(&mut self.choice2, 0);
            tree_enc(e, &mut self.mid[ps], LEN_MID_BITS, v - LEN_LOW as u32);
        } else {
            e.bit(&mut self.choice, 1);
            e.bit(&mut self.choice2, 1);
            tree_enc(e, &mut self.high, LEN_HIGH_BITS, v - (LEN_LOW + LEN_MID) as u32);
        }
    }
    fn dec(&mut self, d: &mut Dec, ps: usize) -> usize {
        let v = if d.bit(&mut self.choice) == 0 {
            tree_dec(d, &mut self.low[ps], LEN_LOW_BITS)
        } else if d.bit(&mut self.choice2) == 0 {
            LEN_LOW as u32 + tree_dec(d, &mut self.mid[ps], LEN_MID_BITS)
        } else {
            (LEN_LOW + LEN_MID) as u32 + tree_dec(d, &mut self.high, LEN_HIGH_BITS)
        };
        v as usize + MIN_LEN
    }
}

struct Model {
    lc: u32, lp_mask: usize, pb_mask: usize,
    is_match: Vec<u16>, is_rep: [u16; NUM_STATES], is_rep_g0: [u16; NUM_STATES],
    is_rep_g1: [u16; NUM_STATES], is_rep_g2: [u16; NUM_STATES],
    lit: Vec<u16>,
    slot: [[u16; NUM_SLOTS]; NUM_LEN_STATES],
    spec: Vec<Vec<u16>>, // per slot < END_POS_MODEL
    align: [u16; 1 << ALIGN_BITS],
    len: LenModel, rep_len: LenModel,
    mix: LitMix,
}

impl Model {
    fn new(p: &Params, input_len: usize) -> Self {
        let pos_states = 1usize << p.pb;
        let mut spec = Vec::with_capacity(END_POS_MODEL);
        for s in 0..END_POS_MODEL {
            let fb = if s >= 4 { (s >> 1) - 1 } else { 0 };
            spec.push(vec![PROB_INIT; 1 << fb]);
        }
        Model {
            lc: p.lc, lp_mask: (1 << p.lp) - 1, pb_mask: pos_states - 1,
            is_match: vec![PROB_INIT; NUM_STATES << p.pb],
            is_rep: [PROB_INIT; NUM_STATES], is_rep_g0: [PROB_INIT; NUM_STATES],
            is_rep_g1: [PROB_INIT; NUM_STATES], is_rep_g2: [PROB_INIT; NUM_STATES],
            lit: vec![PROB_INIT; 0x300 << (p.lc + p.lp)],
            slot: [[PROB_INIT; NUM_SLOTS]; NUM_LEN_STATES],
            spec, align: [PROB_INIT; 1 << ALIGN_BITS],
            len: LenModel::new(pos_states), rep_len: LenModel::new(pos_states),
            mix: LitMix::new(input_len),
        }
    }

    #[inline]
    fn lit_base(&self, pos: usize, prev: u8) -> usize {
        let ctx = ((pos & self.lp_mask) << self.lc) + ((prev as usize) >> (8 - self.lc));
        ctx * 0x300
    }
}

#[inline] fn st_lit(s: usize) -> usize { if s < 4 { 0 } else if s < 10 { s - 3 } else { s - 6 } }
#[inline] fn st_match(s: usize) -> usize { if s < 7 { 7 } else { 10 } }
#[inline] fn st_rep(s: usize) -> usize { if s < 7 { 8 } else { 11 } }

#[inline]
fn dist_slot(d0: u32) -> usize {
    if d0 < 4 { return d0 as usize; }
    let n = 31 - d0.leading_zeros();
    ((n << 1) | ((d0 >> (n - 1)) & 1)) as usize
}


// ---------------------------------------------------------------- literal mixing

trait BitIO { fn code(&mut self, p1: u32, bit: u32) -> u32; }
impl BitIO for Enc { #[inline] fn code(&mut self, p1: u32, bit: u32) -> u32 { self.bit_p(p1, bit); bit } }
impl<'a> BitIO for Dec<'a> { #[inline] fn code(&mut self, p1: u32, _bit: u32) -> u32 { self.bit_p(p1) } }

#[inline]
fn upd(p: &mut u16, bit: u32) {
    if bit == 0 { *p += ((1 << PROB_BITS) - *p) >> MOVE_BITS; } else { *p -= *p >> MOVE_BITS; }
}

fn squash_i(d: i32) -> i32 {
    const T: [i32; 33] = [1, 2, 3, 6, 10, 16, 27, 45, 73, 120, 194, 310, 488, 747, 1101, 1546, 2047, 2549,
                          2994, 3348, 3607, 3785, 3901, 3975, 4022, 4050, 4068, 4079, 4085, 4089, 4092, 4093, 4094];
    if d > 2047 { return 4095; }
    if d < -2047 { return 1; }
    let w = d & 127;
    let i = ((d >> 7) + 16) as usize;
    (T[i] * (128 - w) + T[i + 1] * w + 64) >> 7
}

/// Adaptive bit counter: P(1) in 16 bits, adaptation rate 1/(n+1.5) until n hits the limit.
#[derive(Clone, Copy)]
struct Ctr { p: u16, n: u16 }
const CTR_INIT: Ctr = Ctr { p: 32768, n: 0 };
const CTR_LIMIT: u16 = 1020;
/// Hashed literal contexts: order-2, order-3, order-4, order-6, current word.
const NH: usize = 5;
const MIX_N: usize = NH + 3; // LZMA lit prob, order-1, hashed..., bias
const MIX_LR: i32 = 6;
const MIX_SHIFT: u32 = 14;
const APM_RATE: u32 = 7;

#[inline]
fn hmix(x: u64, seed: u64) -> u32 {
    let h = (x ^ seed).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    ((h ^ (h >> 29)).wrapping_mul(0xBF58_476D_1CE4_E5B9) >> 32) as u32
}

/// Context hashes for the literal at `pos` (all bytes before `pos` are known to both sides).
#[inline]
fn lit_ctx(buf: &[u8], pos: usize) -> [u32; NH] {
    let mut h8 = 0u64;
    for k in 1..=8usize.min(pos) { h8 |= (buf[pos - k] as u64) << (8 * (k - 1)); }
    let mut w = 0u64;
    let mut k = pos;
    while k > 0 && pos - k < 32 {
        let b = buf[k - 1];
        if !(b.is_ascii_alphabetic() || b >= 0x80) { break; }
        w = (w ^ b as u64).wrapping_mul(0x100_0000_01B3);
        k -= 1;
    }
    ctx_from(h8, w)
}

#[inline]
fn ctx_from(h8: u64, w: u64) -> [u32; NH] {
    let w = if w == 0 { 0x5555 + (h8 & 0xFF) } else { w };
    [hmix(h8 & 0xFFFF, 2), hmix(h8 & 0xFF_FFFF, 3), hmix(h8 & 0xFFFF_FFFF, 4),
     hmix(h8 & 0xFFFF_FFFF_FFFF, 6), hmix(w, 7)]
}

struct LitMix {
    squash: Vec<i32>,   // index d+2048
    stretch: Vec<i32>,  // index p (12-bit)
    recip: [i32; 1024],
    o1: Vec<Ctr>,
    ht: Vec<Vec<Ctr>>,
    h_mask: usize,
    w: Vec<[i32; MIX_N]>,
    st: [i32; MIX_N],
    pr: i32,
    set: usize,
    i1: usize,
    hi: [usize; NH],
    apm: Vec<u16>, // [prev byte][node][33 bins], P(1) 16-bit
    apm_idx: usize,
}

impl LitMix {
    fn new(input_len: usize) -> Self {
        let squash: Vec<i32> = (-2048..2048).map(squash_i).collect();
        let mut stretch = vec![0i32; 4096];
        let mut pi = 0usize;
        for x in -2047..=2047 {
            let v = squash_i(x) as usize;
            for j in pi..=v { stretch[j] = x; }
            pi = v + 1;
        }
        for j in pi..4096 { stretch[j] = 2047; }
        let mut recip = [0i32; 1024];
        for n in 0..1024 { recip[n] = (65536.0 / (n as f64 + 1.5)) as i32; }
        let h_bits = ((input_len * 8).max(1 << 16).next_power_of_two().trailing_zeros()).min(22);
        let mut w0 = [65536 / 4; MIX_N];
        w0[0] = 65536 / 2;
        w0[MIX_N - 1] = 0;
        LitMix {
            squash, stretch, recip,
            o1: vec![CTR_INIT; 1 << 16],
            ht: (0..NH).map(|_| vec![CTR_INIT; 1 << h_bits]).collect(),
            h_mask: (1 << h_bits) - 1,
            w: vec![w0; 16 * 9],
            st: [0; MIX_N], pr: 2048, set: 0, i1: 0, hi: [0; NH],
            apm: {
                let row: Vec<u16> = (0..33).map(|j| (squash_i((j - 16) * 128) * 16) as u16).collect();
                let mut v = Vec::with_capacity(65536 * 33);
                for _ in 0..65536 { v.extend_from_slice(&row); }
                v
            },
            apm_idx: 0,
        }
    }

    #[inline]
    fn sq(&self, d: i32) -> i32 { self.squash[(d.clamp(-2047, 2047) + 2048) as usize] }

    #[inline]
    fn predict(&mut self, lz_p1: u32, i1: usize, hs: &[u32; NH], node: u32, set: usize) -> u32 {
        self.i1 = i1;
        self.st[0] = self.stretch[lz_p1.clamp(1, 4095) as usize];
        self.st[1] = self.stretch[(self.o1[i1].p >> 4) as usize];
        // Nibble-slotted hashing: each context hash (re-mixed with the high nibble for the
        // second half of the byte) picks a 16-counter slot = one cache line.
        let (salt, sub) = if node < 16 { (0u32, node as usize) } else {
            let b = 31 - node.leading_zeros(); // bits decoded so far (4..7)
            let hi = (node >> (b - 4)) & 15;
            (hi.wrapping_add(1).wrapping_mul(0x9E37_79B1), ((1 << (b - 4)) | (node & ((1 << (b - 4)) - 1))) as usize)
        };
        for k in 0..NH {
            let h = hs[k] ^ salt;
            let h = h ^ (h >> 15);
            self.hi[k] = ((h.wrapping_mul(0x2C1B_3C6D) as usize) & self.h_mask & !0xF) | sub;
            self.st[2 + k] = self.stretch[(self.ht[k][self.hi[k]].p >> 4) as usize];
        }
        self.st[MIX_N - 1] = 256;
        let n2 = self.ht[0][self.hi[0]].n; let n3 = self.ht[1][self.hi[1]].n;
        let conf = if n2 == 0 { 0 } else if n3 == 0 { 1 } else if n3 < 4 { 2 } else { 3 + (n3 >= 16) as usize + (n3 >= 64) as usize };
        self.set = set * 9 + conf;
        let w = &self.w[self.set];
        let mut dot: i64 = 0;
        for k in 0..MIX_N { dot += self.st[k] as i64 * w[k] as i64; }
        self.pr = self.sq((dot >> 16) as i32).clamp(1, 4095);
        // APM / SSE refinement in context (prev byte, partial literal)
        let sv = self.stretch[self.pr as usize] + 2048;
        let lo = (sv >> 7) as usize;
        let w = sv & 127;
        let base = i1 * 33 + lo;
        self.apm_idx = base + (w >> 6) as usize;
        let pa = ((self.apm[base] as i32 * (128 - w) + self.apm[base + 1] as i32 * w) >> 11).clamp(1, 4095);
        ((self.pr + 3 * pa) >> 2).clamp(1, 4095) as u32
    }

    #[inline]
    fn update(&mut self, bit: u32) {
        let err = (((bit as i32) << 12) - self.pr) * MIX_LR;
        let w = &mut self.w[self.set];
        for k in 0..MIX_N { w[k] += (self.st[k] * err) >> MIX_SHIFT; }
        let target = if bit != 0 { 65535 } else { 0 };
        let a = &mut self.apm[self.apm_idx];
        *a = (*a as i32 + ((target - *a as i32) >> APM_RATE)) as u16;
        let recip = &self.recip;
        let upd_ctr = |c: &mut Ctr| {
            let r = recip[c.n as usize];
            c.p = (c.p as i32 + (((target - c.p as i32) * r) >> 16)) as u16;
            if c.n < CTR_LIMIT { c.n += 1; }
        };
        upd_ctr(&mut self.o1[self.i1]);
        for k in 0..NH { upd_ctr(&mut self.ht[k][self.hi[k]]); }
    }
}

/// Code one literal (encode `byte`, or decode and return it). `matched`: byte at rep0 after a match.
fn code_lit<IO: BitIO>(io: &mut IO, m: &mut Model, byte: u32, base: usize, prev: u8, hs: &[u32; NH], matched: Option<u32>, update: bool) -> u8 {
    let h1 = (prev as usize) << 8;
    let mut node = 1u32;
    let mut offs = 0x100u32;
    let mut mb = matched.unwrap_or(0);
    let mset = if matched.is_some() { 8 } else { 0 };
    for k in (0..8).rev() {
        let lz_idx = if matched.is_some() { mb <<= 1; offs + (mb & offs) + node } else { node } as usize;
        let lz_p1 = (1u32 << PROB_BITS) - m.lit[base + lz_idx] as u32;
        let p = m.mix.predict(lz_p1, h1 + node as usize, hs, node, mset + 7 - k);
        let bit = io.code(p, (byte >> k) & 1);
        if update {
            upd(&mut m.lit[base + lz_idx], bit);
            m.mix.update(bit);
        }
        if matched.is_some() { let mbit = mb & offs; if bit == 0 { offs &= !mbit; } else { offs &= mbit; } }
        node = (node << 1) | bit;
    }
    node as u8
}

/// Accumulates the cost (8.8 fixed-point bits) of coding bits instead of coding them.
struct CostIO { cost: u32, table: Vec<u32> }
impl BitIO for CostIO {
    #[inline]
    fn code(&mut self, p1: u32, bit: u32) -> u32 {
        self.cost += self.table[if bit != 0 { p1 } else { 4096 - p1 } as usize];
        bit
    }
}

/// Per-position literal cost under the adaptive literal model (no matched-literal side info).
/// The model is trained only on positions flagged in `is_lit` (the literals of a previous parse),
/// but every position is priced, so the DP sees context-specific literal costs.
/// `match_dist[i]` > 0 marks a literal right after a match in the previous parse (rep0 = that
/// distance): it is priced with matched-literal side info, as the real coder would.
pub fn literal_costs(input: &[u8], is_lit: &[bool], match_dist: &[u32], lc: u32) -> Vec<u32> {
    let p = Params { lc, lp: 0, pb: 0 };
    let mut m = Model::new(&p, input.len());
    let table: Vec<u32> = (0..=4096u32).map(|q| if q == 0 { 4096 * 4 } else { (-(q as f64 / 4096.0).log2() * 256.0) as u32 }).collect();
    let mut io = CostIO { cost: 0, table };
    let mut out = vec![0u32; input.len()];
    let mut hist = 0u64;
    let mut word = 0u64;
    let mut wlen = 0usize;
    for i in 0..input.len() {
        let prev = (hist & 0xFF) as u8;
        let base = m.lit_base(i, prev);
        io.cost = 0;
        let md = match_dist[i] as usize;
        let matched = if md > 0 && md <= i { Some(input[i - md] as u32) } else { None };
        // incremental context; words longer than 32 letters fall back to the exact scan
        let hs = if wlen < 32 { ctx_from(hist, word) } else { lit_ctx(input, i) };
        code_lit(&mut io, &mut m, input[i] as u32, base, prev, &hs, matched, is_lit[i]);
        out[i] = io.cost;
        let b = input[i];
        hist = (hist << 8) | b as u64;
        if b.is_ascii_alphabetic() || b >= 0x80 { word = (word ^ b as u64).wrapping_mul(0x100_0000_01B3); wlen += 1; } else { word = 0; wlen = 0; }
    }
    out
}

// ---------------------------------------------------------------- encode / decode

pub fn encode(ops: &[Op], input: &[u8], p: &Params) -> Vec<u8> {
    let mut m = Model::new(p, input.len());
    let mut e = Enc::new();
    let mut state = 0usize;
    let mut rep = [0u32; 4];
    let mut pos = 0usize;

    for op in ops {
        let ps = pos & m.pb_mask;
        match *op {
            Op::Lit(b) => {
                e.bit(&mut m.is_match[(state << p.pb) + ps], 0);
                let prev = if pos > 0 { input[pos - 1] } else { 0 };
                let base = m.lit_base(pos, prev);
                let matched = if state >= 7 { Some(input[pos - rep[0] as usize] as u32) } else { None };
                code_lit(&mut e, &mut m, b as u32, base, prev, &lit_ctx(input, pos), matched, true);
                state = st_lit(state);
                pos += 1;
            }
            Op::Match(len, dist) => {
                let len = len as usize;
                e.bit(&mut m.is_match[(state << p.pb) + ps], 1);
                let ri = rep.iter().position(|&r| r == dist && r > 0);
                if let Some(ri) = ri {
                    e.bit(&mut m.is_rep[state], 1);
                    if ri == 0 {
                        e.bit(&mut m.is_rep_g0[state], 0);
                    } else {
                        e.bit(&mut m.is_rep_g0[state], 1);
                        if ri == 1 {
                            e.bit(&mut m.is_rep_g1[state], 0);
                        } else {
                            e.bit(&mut m.is_rep_g1[state], 1);
                            e.bit(&mut m.is_rep_g2[state], (ri == 3) as u32);
                        }
                        let d = rep[ri];
                        for k in (1..=ri).rev() { rep[k] = rep[k - 1]; }
                        rep[0] = d;
                    }
                    m.rep_len.enc(&mut e, len, ps);
                    state = st_rep(state);
                } else {
                    e.bit(&mut m.is_rep[state], 0);
                    m.len.enc(&mut e, len, ps);
                    let ls = (len - MIN_LEN).min(NUM_LEN_STATES - 1);
                    let d0 = dist - 1;
                    let slot = dist_slot(d0);
                    tree_enc(&mut e, &mut m.slot[ls], 6, slot as u32);
                    if slot >= 4 {
                        let fb = (slot >> 1) - 1;
                        let base = (2 | (slot as u32 & 1)) << fb;
                        let red = d0 - base;
                        if slot < END_POS_MODEL {
                            rtree_enc(&mut e, &mut m.spec[slot], fb, red);
                        } else {
                            e.direct(red >> ALIGN_BITS, fb - ALIGN_BITS);
                            rtree_enc(&mut e, &mut m.align, ALIGN_BITS, red & ((1 << ALIGN_BITS) - 1));
                        }
                    }
                    rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = dist;
                    state = st_match(state);
                }
                pos += len;
            }
        }
    }
    e.finish()
}

pub fn decode(data: &[u8], orig_len: usize, p: &Params) -> Result<Vec<u8>, String> {
    let mut m = Model::new(p, orig_len);
    let mut d = Dec::new(data);
    let mut out: Vec<u8> = Vec::with_capacity(orig_len);
    let mut state = 0usize;
    let mut rep = [0u32; 4];

    while out.len() < orig_len {
        let pos = out.len();
        let ps = pos & m.pb_mask;
        if d.bit(&mut m.is_match[(state << p.pb) + ps]) == 0 {
            let prev = if pos > 0 { out[pos - 1] } else { 0 };
            let base = m.lit_base(pos, prev);
            let matched = if state >= 7 { Some(out[pos - rep[0] as usize] as u32) } else { None };
            let sym = code_lit(&mut d, &mut m, 0, base, prev, &lit_ctx(&out, pos), matched, true);
            out.push(sym as u8);
            state = st_lit(state);
            continue;
        }
        let len;
        if d.bit(&mut m.is_rep[state]) == 1 {
            let ri = if d.bit(&mut m.is_rep_g0[state]) == 0 { 0 }
                else if d.bit(&mut m.is_rep_g1[state]) == 0 { 1 }
                else if d.bit(&mut m.is_rep_g2[state]) == 0 { 2 } else { 3 };
            if ri > 0 {
                let dd = rep[ri];
                for k in (1..=ri).rev() { rep[k] = rep[k - 1]; }
                rep[0] = dd;
            }
            len = m.rep_len.dec(&mut d, ps);
            state = st_rep(state);
        } else {
            len = m.len.dec(&mut d, ps);
            let ls = (len - MIN_LEN).min(NUM_LEN_STATES - 1);
            let slot = tree_dec(&mut d, &mut m.slot[ls], 6) as usize;
            let d0 = if slot < 4 { slot as u32 } else {
                let fb = (slot >> 1) - 1;
                let base = (2 | (slot as u32 & 1)) << fb;
                if slot < END_POS_MODEL {
                    base + rtree_dec(&mut d, &mut m.spec[slot], fb)
                } else {
                    let hi = d.direct(fb - ALIGN_BITS) << ALIGN_BITS;
                    base + hi + rtree_dec(&mut d, &mut m.align, ALIGN_BITS)
                }
            };
            rep[3] = rep[2]; rep[2] = rep[1]; rep[1] = rep[0]; rep[0] = d0 + 1;
            state = st_match(state);
        }
        let dist = rep[0] as usize;
        if dist == 0 || dist > out.len() || out.len() + len > orig_len {
            return Err(format!("lzma: invalid match dist {} len {} at {}", dist, len, out.len()));
        }
        let start = out.len() - dist;
        for j in 0..len { let b = out[start + j]; out.push(b); }
    }
    Ok(out)
}
