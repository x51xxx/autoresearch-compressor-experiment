#!/bin/bash
set -euo pipefail

# Quick syntax check — fail fast on compile errors
cargo check --release 2>&1 | tail -5

# Build and run benchmark
cargo build --release 2>&1 | tail -3
cargo run --release --bin lz77-bench 2>&1
