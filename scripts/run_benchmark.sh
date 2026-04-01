#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

CONFIG_PATH="${1:-benchmark.toml}"

echo "[bench] building release binaries..."
/home/makoto/.cargo/bin/cargo build --release -p gateway

echo "[bench] running benchmark with config: $CONFIG_PATH"
/home/makoto/.cargo/bin/cargo run --release -p gateway --bin bench_runner -- "$CONFIG_PATH"

echo "[bench] done."
