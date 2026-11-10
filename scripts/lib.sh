#!/usr/bin/env bash
# Shared helpers for Sphinx scripts. Source it; do not run it.
# Requires: near (near-cli-rs), jq. Keys come from the OS keychain via near CLI.

set -euo pipefail

SPHINX_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NEAR="${NEAR:-$(command -v near || echo "$HOME/.cargo/bin/near")}"
NETWORK="${NETWORK:-testnet}"
# Where OutLayer, FastFS and a default RPC live on $NETWORK (used by the
# scripts that source this file).
# shellcheck disable=SC2034
if [ "$NETWORK" = mainnet ]; then
  OUTLAYER_CONTRACT="${OUTLAYER_CONTRACT:-outlayer.near}"
  OUTLAYER_API="${OUTLAYER_API:-https://api.outlayer.ai}"
  FASTFS_HOST="${FASTFS_HOST:-main.fastfs.io}"
  DEFAULT_RPC="https://rpc.mainnet.fastnear.com"
  EXPLORER="https://nearblocks.io"
else
  OUTLAYER_CONTRACT="${OUTLAYER_CONTRACT:-outlayer.testnet}"
  OUTLAYER_API="${OUTLAYER_API:-https://testnet-api.outlayer.ai}"
  FASTFS_HOST="${FASTFS_HOST:-test.fastfs.io}"
  DEFAULT_RPC="https://rpc.testnet.fastnear.com"
  EXPLORER="https://testnet.nearblocks.io"
fi

# Loads only the variables this project uses from .env (never sources it).
# Account keys are never read from .env.
load_env() {
  # Mainnet runs read .env.mainnet, never the testnet .env, so no testnet
  # account, URL or RPC can leak into a mainnet command.
  local file="$SPHINX_ROOT/.env"
  [ "$NETWORK" != mainnet ] || file="$SPHINX_ROOT/.env.mainnet"
  load_env_file "$file"
  if [ "$NETWORK" = mainnet ] && [ -n "${SPHINX_CONTRACT:-}" ] && [ "${SPHINX_CONTRACT##*.}" != near ]; then
    echo "NETWORK=mainnet but SPHINX_CONTRACT is $SPHINX_CONTRACT, not a .near account" >&2
    exit 1
  fi
  return 0
}

load_env_file() {
  local file="$1"
  [ -f "$file" ] || return 0
  local key line
  for key in SPHINX_PARENT SPHINX_CONTRACT SPHINX_DEV_GRADER_SEED SPHINX_MODE \
    SPHINX_RELAYER_ACCOUNT SPHINX_GRADER_URL SPHINX_GRADER_SHA256 SPHINX_POLL_SECS \
    SPHINX_API_ADDR SPHINX_API_PUBLIC_BASE SPHINX_API_ALLOW_LOOPBACK SPHINX_CODE_DIR SPHINX_RELAY_BURST \
    SPHINX_RELAY_PER_MINUTE SPHINX_RELAY_GLOBAL_PER_MINUTE SPHINX_RELAY_MAX_FAILURES \
    SPHINX_RELAY_COOLDOWN_SECS SPHINX_RELAY_DAILY_MAX SPHINX_GAS_ACCOUNT \
    SPHINX_CODE_READER_TOKEN SPHINX_RPC_KEY SPHINX_RPC_URLS; do
    [ -z "${!key:-}" ] || continue
    line=$(grep -E "^${key}=" "$file" | tail -1 || true)
    if [ -n "$line" ]; then
      export "$key=${line#*=}"
    fi
  done
  rpc_key
  return 0
}

# A FastNEAR API key lifts the public RPC's rate limit. It comes from
# SPHINX_RPC_KEY, .env, or the keychain entry sphinx-fastnear/rpc-key, and is
# only ever sent to FastNEAR hosts, as a header.
rpc_key() {
  if [ -z "${SPHINX_RPC_KEY:-}" ] && command -v security >/dev/null 2>&1; then
    SPHINX_RPC_KEY=$(security find-generic-password -s sphinx-fastnear -a rpc-key -w 2>/dev/null || true)
  fi
  [ -n "${SPHINX_RPC_KEY:-}" ] && export SPHINX_RPC_KEY
  return 0
}

require_tools() {
  local t
  for t in "$@"; do
    command -v "$t" >/dev/null 2>&1 || { echo "missing tool: $t" >&2; exit 69; }
  done
}

# near_call SIGNER METHOD JSON [DEPOSIT] -> prints the return value; on
# failure prints the Sphinx error code (if any) to stderr and returns 1.
near_call() {
  local err; err=$(mktemp)
  if "$NEAR" contract call-function as-transaction "$SPHINX_CONTRACT" "$2" json-args "$3" \
      prepaid-gas '100 Tgas' attached-deposit "${4:-0 NEAR}" sign-as "$1" \
      network-config "$NETWORK" sign-with-keychain send 2>"$err"; then
    rm -f "$err"
  else
    LAST_ERROR=$(grep -oE 'SPHINX_E_[A-Z0-9_]+' "$err" | head -1 || true)
    echo "   refused: ${LAST_ERROR:-unknown error (see near output)}" >&2
    [ -z "$LAST_ERROR" ] && cat "$err" >&2
    rm -f "$err"
    return 1
  fi
}

# near_view METHOD [JSON]
near_view() {
  "$NEAR" contract call-function as-read-only "$SPHINX_CONTRACT" "$1" json-args "${2:-"{}"}" \
    network-config "$NETWORK" now 2>/dev/null
}

# expect_fail CODE SIGNER METHOD JSON [DEPOSIT]: the call must fail with CODE.
expect_fail() {
  local code="$1"; shift
  LAST_ERROR=""
  if near_call "$@" >/dev/null 2>/dev/null; then
    echo "FAIL: expected $code but the call succeeded" >&2; exit 1
  fi
  [ "$LAST_ERROR" = "$code" ] || { echo "FAIL: expected $code, got ${LAST_ERROR:-none}" >&2; exit 1; }
  echo "   refused as expected: $code"
}

sha256_file() { shasum -a 256 "$1" | cut -d' ' -f1; }

# The token that lets the relayer read every stored submission (anyone else
# reads only their own code). Taken from the environment or .env, else the
# macOS keychain, else a file under data/ that is created once (mode 600).
code_reader_token() {
  [ -n "${SPHINX_CODE_READER_TOKEN:-}" ] && { export SPHINX_CODE_READER_TOKEN; return 0; }
  require_tools openssl
  if command -v security >/dev/null 2>&1; then
    SPHINX_CODE_READER_TOKEN=$(security find-generic-password -s sphinx-code-reader -a token -w 2>/dev/null || true)
    if [ -z "$SPHINX_CODE_READER_TOKEN" ]; then
      SPHINX_CODE_READER_TOKEN=$(openssl rand -hex 32)
      security add-generic-password -U -s sphinx-code-reader -a token -w "$SPHINX_CODE_READER_TOKEN"
    fi
  else
    local file="$SPHINX_ROOT/data/code-reader-token"
    if [ ! -s "$file" ]; then
      mkdir -p "$SPHINX_ROOT/data"
      (umask 077; openssl rand -hex 32 > "$file")
    fi
    SPHINX_CODE_READER_TOKEN=$(cat "$file")
  fi
  export SPHINX_CODE_READER_TOKEN
}
