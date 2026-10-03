# LZ77 Compressor Experiment

An autonomous experiment loop optimizing a custom LZ77 compressor, using [autoresearch](https://github.com/anthropics/autoresearch).

## Build & Run

```bash
cargo build --release
cargo run --release
```

## Benchmark

```bash
./benchmark.sh
```

## Data

Test corpus includes Canterbury corpus files, enwik8, and Ukrainian literature texts. Small corpus files are included in the repo. Large files must be downloaded separately:

```bash
# enwik8 — first 100MB of English Wikipedia (Hutter Prize dataset)
curl -O https://mattmahoney.net/dc/enwik8.zip
unzip enwik8.zip -d data/
rm enwik8.zip
```
