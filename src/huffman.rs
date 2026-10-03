/// Canonical Huffman encoder/decoder with 12-bit max code length.
/// Single implementation for variable-size alphabets (up to 512 symbols).

use std::cmp::Reverse;
use std::collections::BinaryHeap;

pub const MAX_BITS: usize = 12;
const TABLE_SIZE: usize = 1 << MAX_BITS;

#[inline(always)]
fn bit_mask(nbits: u32) -> u64 {
    if nbits == 0 { 0 } else { (1u64 << nbits) - 1 }
}

/// Build Huffman code lengths + canonical codes for N symbols.
/// Returns (lengths[0..n], codes[0..n]) where codes[i] = (reversed_code, nbits).
pub fn build_huffman(freq: &[u32], n: usize) -> (Vec<u8>, Vec<(u16, u8)>) {
    let mut lengths = vec![0u8; n];

    let mut leaves: Vec<(u32, usize)> = freq.iter().take(n).enumerate()
        .filter(|(_, &f)| f > 0)
        .map(|(i, &f)| (f, i))
        .collect();

    if leaves.len() <= 1 {
        if let Some(&(_, sym)) = leaves.first() { lengths[sym] = 1; }
        if leaves.is_empty() { lengths[0] = 1; lengths[1] = 1; }
        else {
            let other = if leaves[0].1 == 0 { 1 } else { 0 };
            lengths[other] = 1;
        }
        return (lengths.clone(), build_canonical_codes(&lengths));
    }

    leaves.sort_unstable_by_key(|&(f, s)| (f, s));

    // Build Huffman tree via priority queue
    #[derive(Clone, Copy)]
    struct Node { left: u32, right: u32, sym: u32 }
    const INV: u32 = u32::MAX;

    let mut nodes: Vec<Node> = Vec::with_capacity(leaves.len() * 2);
    let mut heap: BinaryHeap<Reverse<(u64, u32, u32)>> = BinaryHeap::with_capacity(leaves.len() * 2);

    for &(f, sym) in &leaves {
        let idx = nodes.len() as u32;
        nodes.push(Node { left: INV, right: INV, sym: sym as u32 });
        heap.push(Reverse((f as u64, sym as u32, idx)));
    }
    while heap.len() > 1 {
        let Reverse((f1, _, i1)) = heap.pop().unwrap();
        let Reverse((f2, _, i2)) = heap.pop().unwrap();
        let idx = nodes.len() as u32;
        let ms = nodes[i1 as usize].sym.min(nodes[i2 as usize].sym);
        nodes.push(Node { left: i1, right: i2, sym: INV });
        heap.push(Reverse((f1 + f2, ms, idx)));
    }
    let Reverse((_, _, root)) = heap.pop().unwrap();

    // Extract depths
    let mut stack = vec![(root, 0u8)];
    while let Some((idx, d)) = stack.pop() {
        let nd = nodes[idx as usize];
        if nd.sym != INV && (nd.sym as usize) < n {
            lengths[nd.sym as usize] = d;
        }
        if nd.left != INV { stack.push((nd.left, d + 1)); }
        if nd.right != INV { stack.push((nd.right, d + 1)); }
    }

    // Cap at MAX_BITS using simple truncation + redistribution
    let has_overflow = lengths.iter().any(|&l| l > 0 && (l as usize) > MAX_BITS);
    if has_overflow {
        let mut syms: Vec<(u8, usize)> = lengths.iter().enumerate()
            .filter(|(_, &l)| l > 0)
            .map(|(s, &l)| (l.min(MAX_BITS as u8), s))
            .collect();
        syms.sort();

        let num_syms = syms.len();
        let mut assigned: Vec<u8> = syms.iter().map(|&(l, _)| l).collect();

        // Fix Kraft inequality
        loop {
            let kraft: u64 = assigned.iter()
                .map(|&l| 1u64 << (MAX_BITS - l as usize))
                .sum();
            if kraft <= (1u64 << MAX_BITS) { break; }
            let mut fixed = false;
            for i in (0..num_syms).rev() {
                if (assigned[i] as usize) < MAX_BITS {
                    assigned[i] += 1;
                    fixed = true;
                    break;
                }
            }
            if !fixed { break; }
        }

        lengths = vec![0u8; n];
        for (i, &(_, sym)) in syms.iter().enumerate() {
            lengths[sym] = assigned[i];
        }
    }

    (lengths.clone(), build_canonical_codes(&lengths))
}

/// Build canonical codes from code lengths. Returns (reversed_code, nbits) per symbol.
pub fn build_canonical_codes(lengths: &[u8]) -> Vec<(u16, u8)> {
    let n = lengths.len();
    let mut bl_count = [0u16; MAX_BITS + 1];
    for &l in lengths {
        if l > 0 && (l as usize) <= MAX_BITS { bl_count[l as usize] += 1; }
    }
    let mut next_code = [0u16; MAX_BITS + 1];
    let mut code = 0u16;
    for bits in 1..=MAX_BITS {
        code = (code + bl_count[bits - 1]) << 1;
        next_code[bits] = code;
    }
    let mut codes = vec![(0u16, 0u8); n];
    for sym in 0..n {
        let l = lengths[sym] as usize;
        if l > 0 && l <= MAX_BITS {
            let c = next_code[l];
            next_code[l] = c + 1;
            codes[sym] = (c.reverse_bits() >> (16 - l), l as u8);
        }
    }
    codes
}

/// Build a decode lookup table from code lengths.
/// Returns TABLE_SIZE entries of (symbol, nbits).
pub fn build_decode_table(lengths: &[u8]) -> Vec<(u16, u8)> {
    let codes = build_canonical_codes(lengths);
    let mut table = vec![(0u16, 0u8); TABLE_SIZE];
    for (sym, &(code, len)) in codes.iter().enumerate() {
        if len == 0 { continue; }
        let reps = 1usize << (MAX_BITS - len as usize);
        let base = code as usize;
        for i in 0..reps {
            table[base | (i << len as usize)] = (sym as u16, len);
        }
    }
    table
}

// ---- Bit I/O ----

pub struct BitWriter {
    pub buf: Vec<u8>,
    current: u64,
    bits: u32,
}

impl BitWriter {
    pub fn new(cap: usize) -> Self {
        Self { buf: Vec::with_capacity(cap), current: 0, bits: 0 }
    }
    #[inline(always)]
    pub fn write(&mut self, val: u32, nbits: u32) {
        self.current |= ((val as u64) & bit_mask(nbits)) << self.bits;
        self.bits += nbits;
        if self.bits >= 32 {
            self.buf.extend_from_slice(&(self.current as u32).to_le_bytes());
            self.current >>= 32;
            self.bits -= 32;
        }
    }
    pub fn finish(mut self) -> Vec<u8> {
        while self.bits > 0 {
            self.buf.push(self.current as u8);
            self.current >>= 8;
            self.bits = self.bits.saturating_sub(8);
        }
        self.buf
    }
}

pub struct BitReader<'a> {
    src: &'a [u8],
    pos: usize,
    buf: u64,
    bits: u32,
}

impl<'a> BitReader<'a> {
    pub fn new(src: &'a [u8]) -> Self {
        Self { src, pos: 0, buf: 0, bits: 0 }
    }
    #[inline(always)]
    fn ensure(&mut self) {
        if self.bits < 24 {
            while self.bits <= 56 && self.pos < self.src.len() {
                self.buf |= (unsafe { *self.src.get_unchecked(self.pos) } as u64) << self.bits;
                self.pos += 1;
                self.bits += 8;
            }
        }
    }
    #[inline(always)]
    pub fn peek(&mut self, nbits: u32) -> u32 {
        self.ensure();
        (self.buf & bit_mask(nbits)) as u32
    }
    #[inline(always)]
    pub fn consume(&mut self, nbits: u32) {
        self.buf >>= nbits;
        self.bits -= nbits;
    }
    #[inline(always)]
    pub fn read(&mut self, nbits: u32) -> u32 {
        self.ensure();
        let v = (self.buf & bit_mask(nbits)) as u32;
        self.buf >>= nbits;
        self.bits -= nbits;
        v
    }
}
