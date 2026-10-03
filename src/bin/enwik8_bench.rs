/// Hutter Prize benchmark: enwik8 (100MB Wikipedia dump)
/// Fair library-vs-library comparison on a large file that exceeds L3 cache.

use std::io::{Read, Write};
use std::time::Instant;

const ITERS: usize = 3;

fn main() {
    let path = "data/enwik8";
    let data = std::fs::read(path).expect("Failed to read data/enwik8. Download from https://mattmahoney.net/dc/enwik8.zip");
    let size = data.len();
    let size_mb = size as f64 / 1e6;

    println!("╔══════════════════════════════════════════════════════════════════╗");
    println!("║  ENWIK8 BENCHMARK — {:.0} MB Wikipedia dump, {} iterations      ║", size_mb, ITERS);
    println!("╚══════════════════════════════════════════════════════════════════╝");
    println!();

    println!("{:<14} {:>10} {:>10} {:>10} {:>10} {:>10}",
             "Tool", "Comp ms", "Decomp ms", "Comp MB/s", "Dec MB/s", "Ratio");
    println!("{}", "-".repeat(75));

    // --- OURS ---
    {
        let t0 = Instant::now();
        let compressed = lz77_bench::compress(&data);
        let comp_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let mut dec_total = 0.0;
        for _ in 0..ITERS {
            let t1 = Instant::now();
            let dec = lz77_bench::decompress(&compressed).unwrap();
            dec_total += t1.elapsed().as_secs_f64();
            assert_eq!(dec.len(), size);
        }
        let dec_ms = dec_total / ITERS as f64 * 1000.0;
        let ratio = compressed.len() as f64 / size as f64;

        println!("{:<14} {:>9.0}ms {:>9.1}ms {:>9.1} {:>9.1} {:>9.4}",
                 "★ OURS", comp_ms, dec_ms, size_mb / (comp_ms / 1000.0),
                 size_mb / (dec_ms / 1000.0), ratio);
        println!("  compressed: {} bytes ({:.1} MB)", compressed.len(), compressed.len() as f64 / 1e6);
    }

    // --- gzip-9 (flate2) ---
    {
        let t0 = Instant::now();
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&data).unwrap();
        let compressed = enc.finish().unwrap();
        let comp_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let mut dec_total = 0.0;
        for _ in 0..ITERS {
            let t1 = Instant::now();
            let mut dec = flate2::read::GzDecoder::new(&compressed[..]);
            let mut out = Vec::new();
            dec.read_to_end(&mut out).unwrap();
            dec_total += t1.elapsed().as_secs_f64();
            assert_eq!(out.len(), size);
        }
        let dec_ms = dec_total / ITERS as f64 * 1000.0;
        let ratio = compressed.len() as f64 / size as f64;

        println!("{:<14} {:>9.0}ms {:>9.1}ms {:>9.1} {:>9.1} {:>9.4}",
                 "gzip-9", comp_ms, dec_ms, size_mb / (comp_ms / 1000.0),
                 size_mb / (dec_ms / 1000.0), ratio);
    }

    // --- lz4 ---
    {
        let t0 = Instant::now();
        let compressed = lz4_flex::compress_prepend_size(&data);
        let comp_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let mut dec_total = 0.0;
        for _ in 0..ITERS {
            let t1 = Instant::now();
            let dec = lz4_flex::decompress_size_prepended(&compressed).unwrap();
            dec_total += t1.elapsed().as_secs_f64();
            assert_eq!(dec.len(), size);
        }
        let dec_ms = dec_total / ITERS as f64 * 1000.0;
        let ratio = compressed.len() as f64 / size as f64;

        println!("{:<14} {:>9.0}ms {:>9.1}ms {:>9.1} {:>9.1} {:>9.4}",
                 "lz4", comp_ms, dec_ms, size_mb / (comp_ms / 1000.0),
                 size_mb / (dec_ms / 1000.0), ratio);
    }

    // --- zstd-1 ---
    {
        let t0 = Instant::now();
        let compressed = zstd::encode_all(&data[..], 1).unwrap();
        let comp_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let mut dec_total = 0.0;
        for _ in 0..ITERS {
            let t1 = Instant::now();
            let dec = zstd::decode_all(&compressed[..]).unwrap();
            dec_total += t1.elapsed().as_secs_f64();
            assert_eq!(dec.len(), size);
        }
        let dec_ms = dec_total / ITERS as f64 * 1000.0;
        let ratio = compressed.len() as f64 / size as f64;

        println!("{:<14} {:>9.0}ms {:>9.1}ms {:>9.1} {:>9.1} {:>9.4}",
                 "zstd-1", comp_ms, dec_ms, size_mb / (comp_ms / 1000.0),
                 size_mb / (dec_ms / 1000.0), ratio);
    }

    // --- zstd-19 ---
    {
        let t0 = Instant::now();
        let compressed = zstd::encode_all(&data[..], 19).unwrap();
        let comp_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let mut dec_total = 0.0;
        for _ in 0..ITERS {
            let t1 = Instant::now();
            let dec = zstd::decode_all(&compressed[..]).unwrap();
            dec_total += t1.elapsed().as_secs_f64();
            assert_eq!(dec.len(), size);
        }
        let dec_ms = dec_total / ITERS as f64 * 1000.0;
        let ratio = compressed.len() as f64 / size as f64;

        println!("{:<14} {:>9.0}ms {:>9.1}ms {:>9.1} {:>9.1} {:>9.4}",
                 "zstd-19", comp_ms, dec_ms, size_mb / (comp_ms / 1000.0),
                 size_mb / (dec_ms / 1000.0), ratio);
    }

    // --- brotli-6 ---
    {
        let t0 = Instant::now();
        let mut brotli_out = Vec::new();
        {
            let mut enc = brotli::CompressorWriter::new(&mut brotli_out, 4096, 6, 22);
            enc.write_all(&data).unwrap();
        }
        let compressed = brotli_out;
        let comp_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let mut dec_total = 0.0;
        for _ in 0..ITERS {
            let t1 = Instant::now();
            let mut dec = brotli::Decompressor::new(&compressed[..], 4096);
            let mut out = Vec::new();
            dec.read_to_end(&mut out).unwrap();
            dec_total += t1.elapsed().as_secs_f64();
            assert_eq!(out.len(), size);
        }
        let dec_ms = dec_total / ITERS as f64 * 1000.0;
        let ratio = compressed.len() as f64 / size as f64;

        println!("{:<14} {:>9.0}ms {:>9.1}ms {:>9.1} {:>9.1} {:>9.4}",
                 "brotli-6", comp_ms, dec_ms, size_mb / (comp_ms / 1000.0),
                 size_mb / (dec_ms / 1000.0), ratio);
    }

    println!();
    println!("Note: enwik8 (100MB) exceeds most L3 caches — this tests real memory bandwidth.");
}
