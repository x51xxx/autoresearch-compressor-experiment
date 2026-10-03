/// Compare Huffman vs Range coder entropy efficiency on literal bytes.
/// This measures the theoretical gain from switching entropy coders.

fn main() {
    let files: Vec<(&str, &[u8])> = vec![
        ("alice29.txt",     include_bytes!("../../data/alice29.txt")),
        ("kennedy.xls",     include_bytes!("../../data/kennedy.xls")),
        ("urls.10K",        include_bytes!("../../data/urls.10K")),
        ("plrabn12.txt",    include_bytes!("../../data/plrabn12.txt")),
    ];

    println!("{:<18} {:>8} {:>10} {:>10} {:>8}",
             "File", "Size", "Huffman", "Range", "Savings");
    println!("{}", "-".repeat(60));

    for (name, data) in &files {
        // Count byte frequencies
        let mut freq = [0u32; 256];
        for &b in *data { freq[b as usize] += 1; }
        let total: u32 = freq.iter().sum();

        // Build cumulative frequencies for range coder
        let mut cum = vec![0u32; 257];
        for i in 0..256 { cum[i + 1] = cum[i] + freq[i].max(1); } // ensure no zero freqs
        let rc_total = cum[256];

        // Huffman: build tree, compute total bits
        let (lengths, _) = lz77_bench::huffman::build_huffman(&freq, 256);
        let mut huffman_bits: u64 = 0;
        for &b in *data {
            huffman_bits += lengths[b as usize] as u64;
        }
        let huffman_bytes = (huffman_bits + 7) / 8;

        // Range coder: encode all bytes
        let mut enc = lz77_bench::range_coder::RangeEncoder::new();
        for &b in *data {
            let s = b as usize;
            enc.encode(cum[s], cum[s + 1] - cum[s], rc_total);
        }
        let range_bytes = enc.finish().len() as u64;

        // Theoretical entropy (Shannon)
        let entropy_bits: f64 = freq.iter().filter(|&&f| f > 0).map(|&f| {
            let p = f as f64 / total as f64;
            -(p * p.log2()) * f as f64
        }).sum();
        let _entropy_bytes = (entropy_bits / 8.0) as u64;

        let savings = (huffman_bytes as f64 - range_bytes as f64) / huffman_bytes as f64 * 100.0;

        println!("{:<18} {:>8} {:>9}B {:>9}B {:>7.2}%",
                 name, data.len(), huffman_bytes, range_bytes, savings);
    }
}
