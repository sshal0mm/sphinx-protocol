#!/usr/bin/env bash
# Says what this machine can run and what is missing, without changing
# anything.   scripts/doctor.sh
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 1
ok() { printf 'ok    %s\n' "$1"; }
no() { printf 'no    %s: %s\n' "$1" "$2"; }

# A tool on PATH, or installed where its installer puts it but not on PATH.
find_tool() { # NAME -> path, or nothing
  command -v "$1" 2>/dev/null && return
  local d
  for d in "$HOME/.cargo/bin" "$HOME/.wasmtime/bin" /opt/homebrew/bin /usr/local/bin; do
    [ -x "$d/$1" ] && { echo "$d/$1"; return; }
  done
}
tool() { # NAME WHAT HOW
  local path; path=$(find_tool "$1")
  if [ -z "$path" ]; then no "$1" "$3"; return 1; fi
  if command -v "$1" >/dev/null 2>&1; then ok "$1 ($2)"; return 0; fi
  no "$1" "installed at $path but not on PATH: export PATH=\"$(dirname "$path"):\$PATH\""
  return 1
}
rust_wanted=$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)
if tool cargo "Rust; rustup installs $rust_wanted on first build" "install from https://rustup.rs (needed for everything)"; then
  v=$(rustc --version 2>/dev/null | awk '{print $2}')
  if [ "$v" != "$rust_wanted" ]; then printf '      rustc %s found; this repo pins %s\n' "$v" "$rust_wanted"; fi
fi
tool jq "the scripts" "brew install jq (needed by the scripts)"
tool curl "the scripts" "needed by the scripts"
tool python3 "some scripts" "needed by some scripts"
tool wasmtime "make check, make test-wasm" "only for make check: https://wasmtime.dev"
tool cargo-near "make contract-wasm" "only to build the contract: cargo install cargo-near"
tool near "testnet scripts" "only for testnet scripts: cargo install near-cli-rs"

if [ -f .env ]; then ok ".env"; else no ".env" "only for testnet scripts: cp .env.example .env"; fi
if curl -sf -m 10 https://rpc.testnet.fastnear.com -H 'content-type: application/json' \
    -d '{"jsonrpc":"2.0","id":1,"method":"status","params":[]}' >/dev/null; then
  ok "NEAR testnet RPC reachable"
else
  no "NEAR testnet RPC" "not reachable; the offline commands still work"
fi
API="${SPHINX_API:-http://127.0.0.1:8787}"
if curl -sf -m 3 "$API/v1" >/dev/null; then ok "job API at $API"; else no "job API" "not running at $API (make api); needed by the agent, attacks and catalog scripts"; fi

echo
echo "Without keys or accounts:  make quick   (grading offline, every recorded job verified, tampering caught)"
echo "With Rust and wasmtime:    make check   (every test and lint)"
