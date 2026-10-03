/// rANS (range Asymmetric Numeral Systems) entropy coder.
///
/// Key insight: we derive symbol probabilities from Huffman code lengths,
/// so the header format stays unchanged (RLE-encoded code lengths).
/// But the bitstream achieves fractional-bit precision like arithmetic coding.
///
/// rANS state is a single u32. Encoding works backwards (last symbol first),
/// decoding works forward. Output is a byte stream.
///
/// Probability model: for a symbol with Huffman code length L,
/// its probability is 2^(MAX_BITS - L) / 2^MAX_BITS = 2^(-L).
/// This makes the ANS encoding exactly match Huffman entropy for
/// power-of-2 probabilities, and BEAT Huffman for non-power-of-2 cases
/// when we use actual frequencies instead.

const RANS_PROB_BITS: u32 = 14; // probability precision (higher = better entropy, same header size)
const RANS_PROB_SCALE: u32 = 1 << RANS_PROB_BITS;
const RANS_BYTE_L: u32 = 1 << 23; // lower bound of state (must be >= PROB_SCALE * 256)

/// Build cumulative frequency table from raw frequencies.
/// Normalizes to sum = RANS_PROB_SCALE (4096).
/// Returns (cum_freq[n+1], freq[n]) where cum_freq[0]=0, cum_freq[n]=total.
pub fn build_cum_freqs(raw_freq: &[u32], n: usize) -> (Vec<u16>, Vec<u16>) {
    let total: u64 = raw_freq.iter().take(n).map(|&f| f as u64).sum();
    if total == 0 {
        return (vec![0u16; n + 1], vec![0u16; n]);
    }

    let mut freq = vec![0u16; n];
    let mut remaining = RANS_PROB_SCALE as i32;

    // First pass: assign proportional frequencies, minimum 1 for active symbols
    let mut active_count = 0usize;
    for i in 0..n {
        if raw_freq[i] > 0 {
            let f = ((raw_freq[i] as u64 * RANS_PROB_SCALE as u64) / total).max(1) as u16;
            freq[i] = f;
            remaining -= f as i32;
            active_count += 1;
        }
    }

    // Fix up: distribute remaining probability to most frequent symbols
    if remaining > 0 {
        // Add to the most frequent symbol
        let mut best = 0;
        for i in 1..n {
            if raw_freq[i] > raw_freq[best] { best = i; }
        }
        freq[best] = (freq[best] as i32 + remaining) as u16;
    } else if remaining < 0 {
        // Steal from least probable active symbols (but keep >= 1)
        let mut deficit = -remaining;
        // Sort symbols by frequency ascending
        let mut order: Vec<usize> = (0..n).filter(|&i| freq[i] > 1).collect();
        order.sort_by_key(|&i| freq[i]);
        for &i in &order {
            if deficit <= 0 { break; }
            let steal = ((freq[i] - 1) as i32).min(deficit);
            freq[i] -= steal as u16;
            deficit -= steal;
        }
    }

    // Build cumulative
    let mut cum = vec![0u16; n + 1];
    for i in 0..n {
        cum[i + 1] = cum[i] + freq[i];
    }
    // Sanity: should sum to RANS_PROB_SCALE
    debug_assert_eq!(cum[n], RANS_PROB_SCALE as u16,
        "cum_freq total {} != {}", cum[n], RANS_PROB_SCALE);

    (cum, freq)
}

/// rANS encoder — encodes symbols in REVERSE order, outputs bytes.
pub struct RansEncoder {
    state: u32,
    /// Output buffer (filled in reverse during encoding, reversed at finish)
    buf: Vec<u8>,
}

impl RansEncoder {
    pub fn new() -> Self {
        Self {
            state: RANS_BYTE_L,
            buf: Vec::new(),
        }
    }

    /// Encode one symbol. Must be called in REVERSE order (last symbol first).
    #[inline]
    pub fn encode(&mut self, cum_freq: u16, freq: u16) {
        // Renormalize: bring state into range [RANS_BYTE_L, RANS_BYTE_L * 256)
        let max_state = ((RANS_BYTE_L >> RANS_PROB_BITS) << 8) * freq as u32;
        while self.state >= max_state {
            self.buf.push((self.state & 0xFF) as u8);
            self.state >>= 8;
        }
        // Encode: state = (state / freq) * RANS_PROB_SCALE + (state % freq) + cum_freq
        let q = self.state / freq as u32;
        let r = self.state % freq as u32;
        self.state = q * RANS_PROB_SCALE + r + cum_freq as u32;
    }

    /// Encode raw bits (for extra bits in length/distance codes).
    #[inline]
    pub fn encode_bits(&mut self, val: u32, nbits: u32) {
        if nbits == 0 { return; }
        // Use rANS with uniform distribution: each bit value equally likely
        // This is equivalent to just shifting bits into the state
        // But we do it through the renormalization path for correctness
        let scale = 1u32 << nbits;
        let max_state = ((RANS_BYTE_L >> nbits) << 8) * 1; // freq=1 for each value
        while self.state >= (RANS_BYTE_L >> nbits) << 8 {
            self.buf.push((self.state & 0xFF) as u8);
            self.state >>= 8;
        }
        self.state = (self.state << nbits) | val;
    }

    /// Finish encoding, return the byte stream.
    pub fn finish(mut self) -> Vec<u8> {
        // Flush final state (4 bytes, big-endian for forward reading)
        self.buf.push((self.state >> 0) as u8);
        self.buf.push((self.state >> 8) as u8);
        self.buf.push((self.state >> 16) as u8);
        self.buf.push((self.state >> 24) as u8);
        self.buf.reverse();
        self.buf
    }
}

/// rANS decoder — decodes symbols in forward order.
pub struct RansDecoder<'a> {
    state: u32,
    data: &'a [u8],
    pos: usize,
}

impl<'a> RansDecoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let state = u32::from_le_bytes([
            data.get(3).copied().unwrap_or(0),
            data.get(2).copied().unwrap_or(0),
            data.get(1).copied().unwrap_or(0),
            data.get(0).copied().unwrap_or(0),
        ]);
        Self { state, data, pos: 4 }
    }

    /// Get the current cumulative frequency value (for symbol lookup).
    #[inline]
    pub fn get_cum_freq(&self) -> u16 {
        (self.state & (RANS_PROB_SCALE - 1)) as u16
    }

    /// Advance state after decoding a symbol with given cum_freq and freq.
    #[inline]
    pub fn advance(&mut self, cum_freq: u16, freq: u16) {
        // Decode: state = freq * (state / RANS_PROB_SCALE) + (state % RANS_PROB_SCALE) - cum_freq
        let q = self.state >> RANS_PROB_BITS;
        let r = self.state & (RANS_PROB_SCALE - 1);
        self.state = q * freq as u32 + r - cum_freq as u32;
        // Renormalize
        while self.state < RANS_BYTE_L {
            self.state = (self.state << 8) | self.read_byte() as u32;
        }
    }

    /// Decode raw bits.
    #[inline]
    pub fn decode_bits(&mut self, nbits: u32) -> u32 {
        if nbits == 0 { return 0; }
        let val = self.state & ((1 << nbits) - 1);
        self.state >>= nbits;
        while self.state < RANS_BYTE_L {
            self.state = (self.state << 8) | self.read_byte() as u32;
        }
        val
    }

    /// Find symbol given cumulative frequency value.
    /// Uses binary search on cum_freqs table.
    #[inline]
    pub fn decode_symbol(&mut self, cum_freqs: &[u16], freqs: &[u16]) -> usize {
        let cf = self.get_cum_freq();
        // Binary search: find largest i where cum_freqs[i] <= cf
        let mut lo = 0usize;
        let mut hi = freqs.len();
        while lo + 1 < hi {
            let mid = lo + (hi - lo) / 2;
            if cum_freqs[mid] <= cf { lo = mid; } else { hi = mid; }
        }
        let sym = lo;
        self.advance(cum_freqs[sym], freqs[sym]);
        sym
    }

    fn read_byte(&mut self) -> u8 {
        if self.pos < self.data.len() {
            let b = self.data[self.pos];
            self.pos += 1;
            b
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_basic() {
        let freqs: [u32; 4] = [100, 50, 30, 20];
        let (cum, freq) = build_cum_freqs(&freqs, 4);

        let symbols = [0, 1, 2, 3, 0, 0, 1, 2, 3, 0, 1, 0, 0, 0, 2, 3, 1, 0];

        // Encode in reverse
        let mut enc = RansEncoder::new();
        for &s in symbols.iter().rev() {
            enc.encode(cum[s], freq[s]);
        }
        let data = enc.finish();

        // Decode forward
        let mut dec = RansDecoder::new(&data);
        for &expected in &symbols {
            let sym = dec.decode_symbol(&cum, &freq);
            assert_eq!(sym, expected);
        }
    }

    #[test]
    fn roundtrip_256_uniform() {
        let freqs = [10u32; 256];
        let (cum, freq) = build_cum_freqs(&freqs, 256);

        let symbols: Vec<usize> = (0..256).chain((0..256).rev()).collect();

        let mut enc = RansEncoder::new();
        for &s in symbols.iter().rev() {
            enc.encode(cum[s], freq[s]);
        }
        let data = enc.finish();

        let mut dec = RansDecoder::new(&data);
        for &expected in &symbols {
            assert_eq!(dec.decode_symbol(&cum, &freq), expected);
        }
    }

    #[test]
    fn roundtrip_skewed() {
        let mut freqs = [1u32; 256];
        freqs[0] = 5000;
        freqs[32] = 2000;
        freqs[101] = 500;
        let (cum, freq) = build_cum_freqs(&freqs, 256);

        let symbols: Vec<usize> = (0..1000).map(|i| match i % 10 {
            0..=4 => 0, 5..=7 => 32, 8 => 101, _ => (i % 200) + 1
        }).collect();

        let mut enc = RansEncoder::new();
        for &s in symbols.iter().rev() {
            enc.encode(cum[s], freq[s]);
        }
        let data = enc.finish();

        let mut dec = RansDecoder::new(&data);
        for &expected in &symbols {
            assert_eq!(dec.decode_symbol(&cum, &freq), expected);
        }
    }

    #[test]
    fn better_than_huffman_on_skewed() {
        // Verify rANS uses fewer bytes than Huffman on highly skewed data
        let mut freqs = [0u32; 256];
        freqs[0] = 10000;
        freqs[1] = 100;
        freqs[2] = 50;
        freqs[3] = 10;
        let (cum, freq) = build_cum_freqs(&freqs, 256);

        // Generate test data matching the distribution
        let symbols: Vec<usize> = (0..10000).map(|i| {
            if i < 9850 { 0 } else if i < 9950 { 1 } else if i < 9990 { 2 } else { 3 }
        }).collect();

        let mut enc = RansEncoder::new();
        for &s in symbols.iter().rev() {
            enc.encode(cum[s], freq[s]);
        }
        let rans_bytes = enc.finish().len();

        // Huffman: symbol 0 gets 1 bit, others 2-3 bits
        // ~10000 * 1 bit = 1250 bytes minimum (Huffman can't do less than 1 bit/symbol)
        // rANS: symbol 0 has prob ~0.985, needs ~0.022 bits each = ~220 bits = ~28 bytes
        // Reality is between these extremes due to other symbols
        let huffman_bits = 9850 * 1 + 100 * 3 + 50 * 4 + 10 * 4; // ~10390 bits = 1299 bytes
        let huffman_bytes = (huffman_bits + 7) / 8;

        assert!(rans_bytes < huffman_bytes as usize,
            "rANS {} bytes should be < Huffman {} bytes", rans_bytes, huffman_bytes);
    }
}
