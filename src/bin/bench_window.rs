use std::time::Instant;

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

    let windows = [4096, 16384, 32768, 65536, 131072, 262144];

    println!("╔══════════════════════════════════════════════════════════════════════════════════════════╗");
    println!("║              Dynamic Window Size Benchmark — ratio per window size                       ║");
    println!("╚══════════════════════════════════════════════════════════════════════════════════════════╝");
    println!();

    // Header
    print!("{:<22} {:>8}", "File", "Size");
    for &w in &windows {
        print!("  {:>6}", format!("{}KB", w / 1024));
    }
    print!("    auto");
    println!();
    println!("{}", "─".repeat(100));

    let mut total_orig = 0usize;
    let mut total_per_window = [0usize; 6];
    let mut total_auto = 0usize;

    for (name, data) in &files {
        let orig = data.len();
        total_orig += orig;
        print!("{:<22} {:>8}", name, orig);

        let mut best_ratio = f64::MAX;
        let mut best_window = 0;

        for (wi, &w) in windows.iter().enumerate() {
            let compressed = lz77_bench::compress_inner(data, w);
            let ratio = compressed.len() as f64 / orig as f64;
            total_per_window[wi] += compressed.len();

            if ratio < best_ratio {
                best_ratio = ratio;
                best_window = w;
            }

            // Verify roundtrip
            let decompressed = lz77_bench::decompress(&compressed).unwrap();
            assert_eq!(data.as_ref(), decompressed.as_slice(), "Roundtrip failed for {} window {}KB", name, w / 1024);

            print!("  {:>6.4}", ratio);
        }

        // Auto choice (current implementation)
        let auto = lz77_bench::compress(data);
        let auto_ratio = auto.len() as f64 / orig as f64;
        total_auto += auto.len();

        print!("  {:>6.4}", auto_ratio);

        // Mark best
        if best_window / 1024 <= 32 {
            print!("  (32KB best)");
        } else {
            print!("  ({}KB best)", best_window / 1024);
        }
        println!();
    }

    // Totals
    println!("{}", "─".repeat(100));
    print!("{:<22} {:>8}", "AVERAGE", total_orig);
    for wi in 0..windows.len() {
        let ratio = total_per_window[wi] as f64 / total_orig as f64;
        print!("  {:>6.4}", ratio);
    }
    let auto_ratio = total_auto as f64 / total_orig as f64;
    print!("  {:>6.4}", auto_ratio);
    println!();

    // Speed test for auto
    println!();
    println!("Speed test (auto window, 3 iterations):");
    let t0 = Instant::now();
    for (_, data) in &files {
        for _ in 0..3 {
            let _ = lz77_bench::compress(data);
        }
    }
    let compress_ms = t0.elapsed().as_millis() as f64 / 3.0;

    let t1 = Instant::now();
    for (_, data) in &files {
        let compressed = lz77_bench::compress(data);
        for _ in 0..3 {
            let _ = lz77_bench::decompress(&compressed).unwrap();
        }
    }
    let decompress_ms = t1.elapsed().as_millis() as f64 / 3.0 - compress_ms / 3.0;

    println!("  Compress:   {:.0}ms", compress_ms);
    println!("  Decompress: {:.0}ms", decompress_ms);
    println!("  Auto ratio: {:.6}", auto_ratio);
}
