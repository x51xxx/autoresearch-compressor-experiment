/// LZMA-style backend: adaptive binary range coder over the LZ77 token stream.
///
/// No code-length / frequency header — all models adapt as they go. Uses the LZMA
/// 12-state machine, order-lc literal contexts, matched-literal coding after a match
/// (byte at rep0 as side information), REP0-REP3 and LZMA distance slots.

const PROB_BITS: u32 = 11;
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
        if self.range < TOP { self.range <<= 8; self.code = (self.code << 8) | self.next() as u32; }
        b
    }

    fn direct(&mut self, n: usize) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            self.range >>= 1;
            let b = if self.code >= self.range { self.code -= self.range; 1 } else { 0 };
            v = (v << 1) | b;
            if self.range < TOP { self.range <<= 8; self.code = (self.code << 8) | self.next() as u32; }
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
}

impl Model {
    fn new(p: &Params) -> Self {
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

// ---------------------------------------------------------------- encode / decode

pub fn encode(ops: &[Op], input: &[u8], p: &Params) -> Vec<u8> {
    let mut m = Model::new(p);
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
                let probs = &mut m.lit[base..base + 0x300];
                let mut sym = b as u32 | 0x100;
                if state >= 7 {
                    let mut mb = input[pos - rep[0] as usize] as u32;
                    let mut offs = 0x100u32;
                    loop {
                        mb <<= 1;
                        e.bit(&mut probs[(offs + (mb & offs) + (sym >> 8)) as usize], (sym >> 7) & 1);
                        sym <<= 1;
                        offs &= !(mb ^ sym);
                        if sym >= 0x10000 { break; }
                    }
                } else {
                    loop {
                        e.bit(&mut probs[(sym >> 8) as usize], (sym >> 7) & 1);
                        sym <<= 1;
                        if sym >= 0x10000 { break; }
                    }
                }
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
    let mut m = Model::new(p);
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
            let probs = &mut m.lit[base..base + 0x300];
            let mut sym = 1u32;
            if state >= 7 {
                let mut mb = out[pos - rep[0] as usize] as u32;
                let mut offs = 0x100u32;
                loop {
                    mb <<= 1;
                    let bit = mb & offs;
                    let b = d.bit(&mut probs[(offs + bit + sym) as usize]);
                    sym = (sym << 1) | b;
                    if b == 0 { offs &= !bit; } else { offs &= bit; }
                    if sym >= 0x100 { break; }
                }
            } else {
                loop {
                    sym = (sym << 1) | d.bit(&mut probs[sym as usize]);
                    if sym >= 0x100 { break; }
                }
            }
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
