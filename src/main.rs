use std::time::Instant;

fn main() {
    let files: Vec<(&str, &[u8])> = vec![
        ("alice29.txt",     include_bytes!("../data/alice29.txt")),
        ("asyoulik.txt",    include_bytes!("../data/asyoulik.txt")),
        ("fields.c",        include_bytes!("../data/fields.c")),
        ("cp.html",         include_bytes!("../data/cp.html")),
        ("kennedy.xls",     include_bytes!("../data/kennedy.xls")),
        ("plrabn12.txt",    include_bytes!("../data/plrabn12.txt")),
        ("urls.10K",        include_bytes!("../data/urls.10K")),
        ("geo.protodata",   include_bytes!("../data/geo.protodata")),
        ("fireworks.jpeg",  include_bytes!("../data/fireworks.jpeg")),
        ("random_org_10k",  include_bytes!("../data/random_org_10k.bin")),
        ("franko-lys.rtf",  include_bytes!("../data/franko-ivan-iakovych-farbovanyy-lys652.rtf")),
        ("franko-berkut.html", include_bytes!("../data/franko-ivan-iakovych-zakhar-berkut645.html")),
    ];

    let mut total_compress_ns: u128 = 0;
    let mut total_decompress_ns: u128 = 0;
    let mut total_orig_bytes: usize = 0;
    let mut total_compressed_bytes: usize = 0;
    let iterations = 5;

    for (name, data) in &files {
        let mut compress_ns: u128 = 0;
        let mut decompress_ns: u128 = 0;
        let mut compressed_size = 0usize;

        for _ in 0..iterations {
            let t0 = Instant::now();
            let compressed = lz77_bench::compress(data);
            compress_ns += t0.elapsed().as_nanos();
            compressed_size = compressed.len();

            let t1 = Instant::now();
            let _decompressed = lz77_bench::decompress(&compressed).unwrap();
            decompress_ns += t1.elapsed().as_nanos();
        }

        let avg_compress_us = compress_ns / iterations as u128 / 1000;
        let avg_decompress_us = decompress_ns / iterations as u128 / 1000;
        let ratio = compressed_size as f64 / data.len() as f64;

        eprintln!(
            "{}: {}B -> {}B (ratio {:.4}), compress {:.0}µs, decompress {:.0}µs",
            name,
            data.len(),
            compressed_size,
            ratio,
            avg_compress_us,
            avg_decompress_us,
        );

        total_compress_ns += compress_ns;
        total_decompress_ns += decompress_ns;
        total_orig_bytes += data.len();
        total_compressed_bytes += compressed_size;
    }

    let total_compress_us = total_compress_ns / iterations as u128 / 1000;
    let total_decompress_us = total_decompress_ns / iterations as u128 / 1000;
    let total_ratio = total_compressed_bytes as f64 / total_orig_bytes as f64;

    // Output METRIC lines for autoresearch
    println!("METRIC compress_µs={}", total_compress_us);
    println!("METRIC decompress_µs={}", total_decompress_us);
    println!("METRIC ratio={:.6}", total_ratio);
    println!(
        "METRIC total_µs={}",
        total_compress_us + total_decompress_us
    );
}
