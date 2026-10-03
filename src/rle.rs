/// RLE-encode code lengths for Huffman header compression.
///
/// Nibble-packed format (2 values per byte):
///   0-12: literal code length value
///   13 + next_nibble: repeat previous value (count+2) times, max 17
///   14 + next_nibble: repeat zero (count+2) times, max 17
///   15 + next_byte: long repeat zero (count+18) times, max 273
///
/// Output: packed nibble stream with byte length prefix.
pub fn rle_encode_lengths(lengths: &[u8]) -> Vec<u8> {
    // First pass: generate nibble stream
    let mut nibbles: Vec<u8> = Vec::with_capacity(lengths.len());
    let mut i = 0;
    while i < lengths.len() {
        let v = lengths[i];
        let mut run = 1;
        while i + run < lengths.len() && lengths[i + run] == v && run < 273 { run += 1; }

        if v == 0 && run >= 2 {
            if run <= 17 {
                nibbles.push(14);
                nibbles.push((run - 2) as u8);
            } else {
                // Long zero run: nibble 15 + full byte
                nibbles.push(15);
                nibbles.push(((run - 18) >> 4) as u8 & 0xF);
                // Need to encode the low nibble too — use extended format
                // Actually simpler: emit 15, then pack count into next 2 nibbles (8 bits)
                nibbles.push(((run - 18) & 0xF) as u8);
            }
            i += run;
        } else if run >= 3 && v > 0 {
            nibbles.push(v);
            nibbles.push(13);
            let repeat = (run - 1).min(17); // repeat prev (run-1) more times
            nibbles.push((repeat - 2) as u8);
            i += 1 + repeat;
        } else {
            for _ in 0..run {
                nibbles.push(v);
                i += 1;
            }
        }
    }

    // Pack nibbles into bytes
    let mut out = Vec::with_capacity(nibbles.len() / 2 + 1);
    let mut j = 0;
    while j + 1 < nibbles.len() {
        out.push((nibbles[j] & 0xF) | ((nibbles[j + 1] & 0xF) << 4));
        j += 2;
    }
    if j < nibbles.len() {
        out.push(nibbles[j] & 0xF);
    }
    out
}

/// Decode RLE-encoded code lengths from nibble-packed format.
pub fn rle_decode_lengths(rle: &[u8], expected_len: usize) -> Vec<u8> {
    // Unpack nibbles
    let mut nibbles: Vec<u8> = Vec::with_capacity(rle.len() * 2);
    for &b in rle {
        nibbles.push(b & 0xF);
        nibbles.push((b >> 4) & 0xF);
    }

    let mut out = Vec::with_capacity(expected_len);
    let mut i = 0;
    while i < nibbles.len() && out.len() < expected_len {
        let v = nibbles[i];
        if v == 14 {
            // Repeat zero
            let count = nibbles.get(i + 1).copied().unwrap_or(0) as usize + 2;
            for _ in 0..count { if out.len() < expected_len { out.push(0); } }
            i += 2;
        } else if v == 15 {
            // Long repeat zero
            let hi = nibbles.get(i + 1).copied().unwrap_or(0) as usize;
            let lo = nibbles.get(i + 2).copied().unwrap_or(0) as usize;
            let count = (hi << 4 | lo) + 18;
            for _ in 0..count { if out.len() < expected_len { out.push(0); } }
            i += 3;
        } else if v == 13 {
            // Repeat previous
            let count = nibbles.get(i + 1).copied().unwrap_or(0) as usize + 2;
            let prev = out.last().copied().unwrap_or(0);
            for _ in 0..count { if out.len() < expected_len { out.push(prev); } }
            i += 2;
        } else {
            out.push(v);
            i += 1;
        }
    }
    while out.len() < expected_len { out.push(0); }
    out
}
