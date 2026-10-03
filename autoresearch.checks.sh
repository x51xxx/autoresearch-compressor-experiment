#!/bin/bash
set -euo pipefail

# Correctness: all roundtrip tests must pass
cargo test --release 2>&1 | tail -10
