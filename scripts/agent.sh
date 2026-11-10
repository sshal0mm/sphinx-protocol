#!/usr/bin/env bash
# Runs the earning agent against the network in .env.
#   scripts/agent.sh [--once]
# Keys come from the environment, so it runs anywhere:
#   SPHINX_ACCOUNT, SPHINX_ACCOUNT_KEY   the account that signs
#   SPHINX_MODEL_KEY (or NEAR_AI_API_KEY), and SPHINX_MODEL_BASE, SPHINX_MODEL for
#                                        a provider other than NEAR AI Cloud
# Only when one is missing does it read it from the macOS keychain: the account
# SPHINX_AGENT_ACCOUNT (default solver.$SPHINX_PARENT) through near CLI, and the model
# key from the entry sphinx-near-ai / api-key.
# SPHINX_API defaults to the local job API (make api).
# shellcheck source=scripts/lib.sh
source "$(dirname "$0")/lib.sh"
load_env
require_tools cargo
: "${SPHINX_CONTRACT:?set SPHINX_CONTRACT}"
SPHINX_ACCOUNT="${SPHINX_ACCOUNT:-${SPHINX_AGENT_ACCOUNT:-solver.${SPHINX_PARENT:?set SPHINX_ACCOUNT or SPHINX_PARENT}}}"

SPHINX_MODEL_KEY="${SPHINX_MODEL_KEY:-${NEAR_AI_API_KEY:-}}"
if [ -z "$SPHINX_MODEL_KEY" ] && command -v security >/dev/null 2>&1; then
  SPHINX_MODEL_KEY=$(security find-generic-password -s sphinx-near-ai -a api-key -w 2>/dev/null || true)
fi
[ -n "$SPHINX_MODEL_KEY" ] || { echo "set SPHINX_MODEL_KEY (a NEAR AI Cloud key, or another provider's with SPHINX_MODEL_BASE and SPHINX_MODEL)" >&2; exit 78; }
if [ -z "${SPHINX_ACCOUNT_KEY:-}" ] && command -v "$NEAR" >/dev/null 2>&1; then
  SPHINX_ACCOUNT_KEY=$("$NEAR" account export-account "$SPHINX_ACCOUNT" using-private-key network-config "$NETWORK" 2>&1 \
    | grep -oE 'ed25519:[1-9A-HJ-NP-Za-km-z]{80,}' | head -1 || true)
fi
[ -n "${SPHINX_ACCOUNT_KEY:-}" ] || { echo "set SPHINX_ACCOUNT_KEY for $SPHINX_ACCOUNT (ed25519:...)" >&2; exit 78; }

export SPHINX_MODEL_KEY SPHINX_ACCOUNT_KEY SPHINX_CONTRACT SPHINX_ACCOUNT
export SPHINX_API="${SPHINX_API:-http://127.0.0.1:8787}"
cd "$SPHINX_ROOT" || exit 1
cargo build -q --release -p sphinx-agent -p sphinx-mcp
exec "$SPHINX_ROOT/target/release/sphinx-agent" "$@"
