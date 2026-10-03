/// Checksum implementations for integrity verification.

// ============================================================================
// Adler-32 (same as zlib) — simpler and potentially faster than CRC32
// ============================================================================
pub fn adler32(data: &[u8]) -> u32 {
    const MOD_ADLER: u32 = 65521;
    let mut a: u32 = 1;
    let mut b: u32 = 0;

    // Process in chunks to defer modulo operations (Nmax = 5552 for u32 safety)
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += byte as u32;
            b += a;
        }
        a %= MOD_ADLER;
        b %= MOD_ADLER;
    }

    (b << 16) | a
}

// ============================================================================
// CRC32 with 256-entry lookup table (same polynomial as gzip: 0xEDB88320)
// ============================================================================
const fn build_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0u32;
    while i < 256 {
        let mut crc = i;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i as usize] = crc;
        i += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = build_crc_table();

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        let idx = ((crc ^ b as u32) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC_TABLE[idx];
    }
    !crc
}

// ============================================================================
// xxHash32 — very fast non-cryptographic hash (used by zstd, lz4)
// ============================================================================
pub fn xxhash32(data: &[u8]) -> u32 {
    const PRIME1: u32 = 0x9E3779B1;
    const PRIME2: u32 = 0x85EBCA77;
    const PRIME3: u32 = 0xC2B2AE3D;
    const PRIME4: u32 = 0x27D4EB2F;
    const PRIME5: u32 = 0x165667B1;

    let len = data.len() as u32;
    let mut h: u32;
    let mut i = 0usize;

    if data.len() >= 16 {
        let mut v1 = 0u32.wrapping_add(PRIME1).wrapping_add(PRIME2);
        let mut v2 = 0u32.wrapping_add(PRIME2);
        let mut v3 = 0u32;
        let mut v4 = 0u32.wrapping_sub(PRIME1);

        let limit = data.len() - 16;
        while i <= limit {
            let k1 = u32::from_le_bytes([data[i], data[i+1], data[i+2], data[i+3]]);
            v1 = v1.wrapping_add(k1.wrapping_mul(PRIME2)).rotate_left(13).wrapping_mul(PRIME1);
            i += 4;
            let k2 = u32::from_le_bytes([data[i], data[i+1], data[i+2], data[i+3]]);
            v2 = v2.wrapping_add(k2.wrapping_mul(PRIME2)).rotate_left(13).wrapping_mul(PRIME1);
            i += 4;
            let k3 = u32::from_le_bytes([data[i], data[i+1], data[i+2], data[i+3]]);
            v3 = v3.wrapping_add(k3.wrapping_mul(PRIME2)).rotate_left(13).wrapping_mul(PRIME1);
            i += 4;
            let k4 = u32::from_le_bytes([data[i], data[i+1], data[i+2], data[i+3]]);
            v4 = v4.wrapping_add(k4.wrapping_mul(PRIME2)).rotate_left(13).wrapping_mul(PRIME1);
            i += 4;
        }

        h = v1.rotate_left(1)
            .wrapping_add(v2.rotate_left(7))
            .wrapping_add(v3.rotate_left(12))
            .wrapping_add(v4.rotate_left(18));
    } else {
        h = 0u32.wrapping_add(PRIME5);
    }

    h = h.wrapping_add(len);

    // Process remaining bytes in 4-byte chunks
    while i + 4 <= data.len() {
        let k = u32::from_le_bytes([data[i], data[i+1], data[i+2], data[i+3]]);
        h = h.wrapping_add(k.wrapping_mul(PRIME3)).rotate_left(17).wrapping_mul(PRIME4);
        i += 4;
    }

    // Process remaining bytes
    while i < data.len() {
        h = h.wrapping_add((data[i] as u32).wrapping_mul(PRIME5)).rotate_left(11).wrapping_mul(PRIME1);
        i += 1;
    }

    // Avalanche
    h ^= h >> 15;
    h = h.wrapping_mul(PRIME2);
    h ^= h >> 13;
    h = h.wrapping_mul(PRIME3);
    h ^= h >> 16;
    h
}
