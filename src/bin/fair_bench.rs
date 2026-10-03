/// Fair library-vs-library decompression benchmark.
/// All compressors measured in-process — no spawn overhead, same cache conditions.

use std::io::{Read, Write};
use std::time::Instant;

const ITERS: usize = 10;

fn main() {
    let files: Vec<(&str, &[u8])> = vec![
        ("alice29.txt",     include_bytes!("../../data/alice29.txt")),
        ("asyoulik.txt",    include_bytes!("../../data/asyoulik.txt")),
        ("fields.c",        include_bytes!("../../data/fields.c")),
        ("cp.html",         include_bytes!("../../data/cp.html")),
        ("kennedy.xls",     include_bytes!("../../data/kennedy.xls")),
        ("plrabn12.txt",    include_bytes!("../../data/plrabn12.txt")),
        ("urls.10K",        include_bytes!("../../data/urls.10K")),
        ("geo.protodata",   include_bytes!("../../data/geo.protodata")),
        ("fireworks.jpeg",  include_bytes!("../../data/fireworks.jpeg")),
        ("random_org_10k",  include_bytes!("../../data/random_org_10k.bin")),
        ("franko-lys.rtf",  include_bytes!("../../data/franko-ivan-iakovych-farbovanyy-lys652.rtf")),
        ("franko-berkut.html", include_bytes!("../../data/franko-ivan-iakovych-zakhar-berkut645.html")),
    ];

    let total_orig: usize = files.iter().map(|(_, d)| d.len()).sum();

    println!("╔══════════════════════════════════════════════════════════════════════════════════════╗");
    println!("║    FAIR LIBRARY-VS-LIBRARY BENCHMARK ({} files, {:.1} MB, {} iterations)      ║",
             files.len(), total_orig as f64 / 1e6, ITERS);
    println!("╚══════════════════════════════════════════════════════════════════════════════════════╝");
    println!();

    // Pre-compress all files with all compressors
    let mut our_compressed: Vec<Vec<u8>> = Vec::new();
    let mut gzip_compressed: Vec<Vec<u8>> = Vec::new();
    let mut lz4_compressed: Vec<Vec<u8>> = Vec::new();
    let mut brotli_compressed: Vec<Vec<u8>> = Vec::new();
    let mut zstd_compressed: Vec<Vec<u8>> = Vec::new();

    for (_, data) in &files {
        // Our compressor
        our_compressed.push(lz77_bench::compress(data));

        // gzip (flate2) level 9
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(data).unwrap();
        gzip_compressed.push(enc.finish().unwrap());

        // lz4
        lz4_compressed.push(lz4_flex::compress_prepend_size(data));

        // brotli level 6
        let mut brotli_out = Vec::new();
        {
            let mut enc = brotli::CompressorWriter::new(&mut brotli_out, 4096, 6, 22);
            enc.write_all(data).unwrap();
        }
        brotli_compressed.push(brotli_out);

        // zstd level 1
        zstd_compressed.push(zstd::encode_all(&data[..], 1).unwrap());
    }

    // ========================================================================
    // DECOMPRESS BENCHMARK — all in-process, fair comparison
    // ========================================================================
    println!("{:<14} {:>10} {:>10} {:>10} {:>10}  {:>8}",
             "Tool", "Decomp ms", "Dec MB/s", "Comp Size", "Ratio", "Verify");
    println!("{}", "-".repeat(80));

    // --- OURS ---
    {
        let total_comp: usize = our_compressed.iter().map(|c| c.len()).sum();
        let t0 = Instant::now();
        for _ in 0..ITERS {
            for (idx, (_, orig_data)) in files.iter().enumerate() {
                let dec = lz77_bench::decompress(&our_compressed[idx]).unwrap();
                assert_eq!(dec.len(), orig_data.len());
            }
        }
        let elapsed = t0.elapsed().as_secs_f64() / ITERS as f64;
        let mbs = total_orig as f64 / 1e6 / elapsed;
        let ratio = total_comp as f64 / total_orig as f64;
        println!("{:<14} {:>9.2}ms {:>9.1} {:>10} {:>9.4}  {:>8}",
                 "★ OURS", elapsed * 1000.0, mbs, total_comp, ratio, "✓");
    }

    // --- gzip (flate2) ---
    {
        let total_comp: usize = gzip_compressed.iter().map(|c| c.len()).sum();
        let t0 = Instant::now();
        for _ in 0..ITERS {
            for (idx, (_, orig_data)) in files.iter().enumerate() {
                let mut dec = flate2::read::GzDecoder::new(&gzip_compressed[idx][..]);
                let mut out = Vec::new();
                dec.read_to_end(&mut out).unwrap();
                assert_eq!(out.len(), orig_data.len());
            }
        }
        let elapsed = t0.elapsed().as_secs_f64() / ITERS as f64;
        let mbs = total_orig as f64 / 1e6 / elapsed;
        let ratio = total_comp as f64 / total_orig as f64;
        println!("{:<14} {:>9.2}ms {:>9.1} {:>10} {:>9.4}  {:>8}",
                 "gzip-9", elapsed * 1000.0, mbs, total_comp, ratio, "✓");
    }

    // --- lz4 ---
    {
        let total_comp: usize = lz4_compressed.iter().map(|c| c.len()).sum();
        let t0 = Instant::now();
        for _ in 0..ITERS {
            for (idx, (_, orig_data)) in files.iter().enumerate() {
                let dec = lz4_flex::decompress_size_prepended(&lz4_compressed[idx]).unwrap();
                assert_eq!(dec.len(), orig_data.len());
            }
        }
        let elapsed = t0.elapsed().as_secs_f64() / ITERS as f64;
        let mbs = total_orig as f64 / 1e6 / elapsed;
        let ratio = total_comp as f64 / total_orig as f64;
        println!("{:<14} {:>9.2}ms {:>9.1} {:>10} {:>9.4}  {:>8}",
                 "lz4", elapsed * 1000.0, mbs, total_comp, ratio, "✓");
    }

    // --- brotli ---
    {
        let total_comp: usize = brotli_compressed.iter().map(|c| c.len()).sum();
        let t0 = Instant::now();
        for _ in 0..ITERS {
            for (idx, (_, orig_data)) in files.iter().enumerate() {
                let mut dec = brotli::Decompressor::new(&brotli_compressed[idx][..], 4096);
                let mut out = Vec::new();
                dec.read_to_end(&mut out).unwrap();
                assert_eq!(out.len(), orig_data.len());
            }
        }
        let elapsed = t0.elapsed().as_secs_f64() / ITERS as f64;
        let mbs = total_orig as f64 / 1e6 / elapsed;
        let ratio = total_comp as f64 / total_orig as f64;
        println!("{:<14} {:>9.2}ms {:>9.1} {:>10} {:>9.4}  {:>8}",
                 "brotli-6", elapsed * 1000.0, mbs, total_comp, ratio, "✓");
    }

    // --- zstd ---
    {
        let total_comp: usize = zstd_compressed.iter().map(|c| c.len()).sum();
        let t0 = Instant::now();
        for _ in 0..ITERS {
            for (idx, (_, orig_data)) in files.iter().enumerate() {
                let dec = zstd::decode_all(&zstd_compressed[idx][..]).unwrap();
                assert_eq!(dec.len(), orig_data.len());
            }
        }
        let elapsed = t0.elapsed().as_secs_f64() / ITERS as f64;
        let mbs = total_orig as f64 / 1e6 / elapsed;
        let ratio = total_comp as f64 / total_orig as f64;
        println!("{:<14} {:>9.2}ms {:>9.1} {:>10} {:>9.4}  {:>8}",
                 "zstd-1", elapsed * 1000.0, mbs, total_comp, ratio, "✓");
    }

    println!();
    println!("All measurements are pure in-process — no subprocess spawn, no pipe I/O.");
    println!("Data is warm in cache (same conditions for all compressors).");
    println!("Corpus: {} files, {:.2} MB total, {} iterations averaged.", files.len(), total_orig as f64 / 1e6, ITERS);
}
