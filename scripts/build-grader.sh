#!/usr/bin/env bash
# Builds the enclave grader with machine-specific paths remapped, so the wasm
# (and therefore the enclave key bound to its SHA-256) is the same on any
# machine with the pinned toolchain. Prints the path and SHA-256.
# Once published, scripts/outlayer-grade.sh runs its `identity` op to read the
# signing and box keys to register with `set_grader_key`.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
SEP=$'\x1f'
# CARGO_ENCODED_RUSTFLAGS separates flags with 0x1f, so paths with spaces work.
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME/.cargo=/cargo${SEP}--remap-path-prefix=$HOME/.rustup=/rustup${SEP}--remap-path-prefix=$ROOT=/sphinx"
export CARGO_TARGET_DIR="$ROOT/target/enclave"
cargo build --locked --release -p sphinx-grader --target wasm32-wasip2 --features outlayer
WASM="$CARGO_TARGET_DIR/wasm32-wasip2/release/sphinx-grader.wasm"
if grep -q "$HOME" "$WASM"; then
  echo "error: local paths are still embedded in $WASM" >&2
  exit 1
fi
echo "$WASM"
shasum -a 256 "$WASM" | cut -d' ' -f1
