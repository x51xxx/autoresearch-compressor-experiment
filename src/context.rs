/// Order-1 context modeling: 9 context classes for Huffman table selection.

pub const NUM_CTX: usize = 9;

/// Context class lookup table — one array access instead of branches.
/// 9 classes: split space from other whitespace for better text prediction.
pub static CTX_TABLE: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0u16;
    while i < 256 {
        let b = i as u8;
        t[i as usize] = match b {
            b' '                => 1,  // space (most common, predicts lowercase after)
            b'\t' | b'\n' | b'\r' => 2,  // other whitespace (predicts indent/newline patterns)
            b'0'..=b'9'        => 3,
            b'!'..=b'/' | b':'..=b'@' | b'['..=b'`' | b'{'..=b'~' => 4,
            b'A'..=b'Z'        => 5,
            b'a' | b'e' | b'i' | b'o' | b'u' => 6,
            b'b'..=b'd' | b'f'..=b'h' | b'j'..=b'n' | b'p'..=b't' | b'v'..=b'z' => 7,
            0x80..=0xFF         => 8,
            _ => 0,
        };
        i += 1;
    }
    t
};

#[inline(always)]
pub fn context_class(prev_byte: u8) -> usize {
    CTX_TABLE[prev_byte as usize] as usize
}
