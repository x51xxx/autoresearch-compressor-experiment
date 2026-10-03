/// Range coder — encodes symbols with fractional bit precision.
/// Based on Dmitry Subbotin's carryless rangecoder (public domain).
/// Implementation by GPT 5.4.

const TOP_VALUE: u32 = 1 << 24;

pub struct RangeEncoder {
    low: u64,
    range: u32,
    ff_num: usize,
    cache: u8,
    pub output: Vec<u8>,
}

impl RangeEncoder {
    pub fn new() -> Self {
        Self {
            low: 0,
            range: u32::MAX,
            ff_num: 0,
            cache: 0,
            output: Vec::new(),
        }
    }

    fn shift_low(&mut self) {
        if (self.low >> 24) != 0xFF {
            let carry = (self.low >> 32) as u8;
            self.output.push(self.cache.wrapping_add(carry));
            let ff_byte = 0xFFu8.wrapping_add(carry);
            while self.ff_num != 0 {
                self.output.push(ff_byte);
                self.ff_num -= 1;
            }
            self.cache = (self.low >> 24) as u8;
        } else {
            self.ff_num += 1;
        }
        self.low = ((self.low as u32) << 8) as u64;
    }

    fn normalize(&mut self) {
        while self.range < TOP_VALUE {
            self.shift_low();
            self.range <<= 8;
        }
    }

    #[inline]
    pub fn encode(&mut self, cum_freq: u32, freq: u32, total_freq: u32) {
        self.range /= total_freq;
        self.low += u64::from(cum_freq) * u64::from(self.range);
        self.range *= freq;
        self.normalize();
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.low += 1;
        for _ in 0..5 {
            self.shift_low();
        }
        self.output
    }
}

pub struct RangeDecoder<'a> {
    input: &'a [u8],
    pos: usize,
    code: u32,
    range: u32,
}

impl<'a> RangeDecoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let mut decoder = Self {
            input: data,
            pos: 0,
            code: 0,
            range: u32::MAX,
        };
        for _ in 0..5 {
            decoder.code = (decoder.code << 8) | u32::from(decoder.read_byte());
        }
        decoder
    }

    fn read_byte(&mut self) -> u8 {
        if let Some(&byte) = self.input.get(self.pos) {
            self.pos += 1;
            byte
        } else {
            0
        }
    }

    fn normalize(&mut self) {
        while self.range < TOP_VALUE {
            self.code = (self.code << 8) | u32::from(self.read_byte());
            self.range <<= 8;
        }
    }

    #[inline]
    pub fn get_freq(&mut self, total_freq: u32) -> u32 {
        self.range /= total_freq;
        self.code / self.range
    }

    #[inline]
    pub fn update(&mut self, cum_freq: u32, freq: u32) {
        self.code -= cum_freq * self.range;
        self.range *= freq;
        self.normalize();
    }

    #[inline]
    pub fn decode(&mut self, cum_freqs: &[u32]) -> usize {
        let total_freq = *cum_freqs.last().unwrap();
        let value = self.get_freq(total_freq);

        // Binary search for symbol
        let mut lo = 0usize;
        let mut hi = cum_freqs.len() - 1;
        while lo + 1 < hi {
            let mid = lo + (hi - lo) / 2;
            if cum_freqs[mid] <= value {
                lo = mid;
            } else {
                hi = mid;
            }
        }

        let sym = lo;
        let freq = cum_freqs[sym + 1] - cum_freqs[sym];
        self.update(cum_freqs[sym], freq);
        sym
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_cum_freqs(freqs: &[u32]) -> Vec<u32> {
        let mut cum = Vec::with_capacity(freqs.len() + 1);
        cum.push(0);
        for &f in freqs { cum.push(cum.last().unwrap() + f); }
        cum
    }

    fn roundtrip(cum_freqs: &[u32], symbols: &[usize]) {
        let total = *cum_freqs.last().unwrap();
        let mut enc = RangeEncoder::new();
        for &s in symbols {
            enc.encode(cum_freqs[s], cum_freqs[s + 1] - cum_freqs[s], total);
        }
        let data = enc.finish();
        let mut dec = RangeDecoder::new(&data);
        for &expected in symbols {
            assert_eq!(dec.decode(cum_freqs), expected);
        }
    }

    #[test]
    fn basic_four_symbol() {
        roundtrip(&[0, 1, 3, 6, 10], &[0, 1, 2, 3, 3, 2, 1, 0, 2, 2, 3, 1, 0]);
    }

    #[test]
    fn uniform_256() {
        let cum: Vec<u32> = (0..=256).collect();
        let mut syms: Vec<usize> = (0..256).collect();
        syms.extend((0..256).rev());
        roundtrip(&cum, &syms);
    }

    #[test]
    fn skewed() {
        let mut freqs = vec![1u32; 256];
        freqs[0] = 1000;
        freqs[32] = 500;
        let cum = build_cum_freqs(&freqs);
        let mut syms = vec![0usize; 200];
        syms.extend([1, 2, 3, 32, 100, 255]);
        syms.extend(std::iter::repeat(0).take(200));
        syms.extend(0..256);
        roundtrip(&cum, &syms);
    }
}
