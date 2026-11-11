#!/usr/bin/env bash
# Calls one Sphinx MCP tool as a NEAR account whose key is in the macOS
# keychain (the near CLI's store), without printing the key.
#   NETWORK=mainnet scripts/as-account.sh <account> <tool> ['<json>']
#   NETWORK=mainnet scripts/as-account.sh <account> post <job folder>
# A job folder holds spec.md, suite.json, reference.js and terms.json.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
ACCOUNT="${1:?account}"; TOOL="${2:?tool}"; shift 2
NETWORK="${NETWORK:-testnet}"
if [ "$NETWORK" = mainnet ]; then
  export SPHINX_CONTRACT="${SPHINX_CONTRACT:-escrow.sphinxescrow.near}" SPHINX_API="${SPHINX_API:-https://sphinx.sshal0mm.dev/mainnet}"
else
  export SPHINX_CONTRACT="${SPHINX_CONTRACT:-escrow3.sphinx999.testnet}" SPHINX_API="${SPHINX_API:-https://sphinx.sshal0mm.dev/testnet}"
fi
KEY=$(near account export-account "$ACCOUNT" using-private-key network-config "$NETWORK" 2>&1 \
  | grep -oE 'ed25519:[1-9A-HJ-NP-Za-km-z]{80,}' | head -1 || true)
[ -n "$KEY" ] || { echo "no key for $ACCOUNT in the keychain" >&2; exit 2; }
export SPHINX_ACCOUNT="$ACCOUNT" SPHINX_ACCOUNT_KEY="$KEY" SPHINX_MAX_SPEND_NEAR="${SPHINX_MAX_SPEND_NEAR:-0.5}"
export SPHINX_MCP_BIN="${SPHINX_MCP_BIN:-target/release/sphinx-mcp}"
CALL=target/release/sphinx-call
if [ "$TOOL" = post ]; then
  D="${1:?job folder}"
  ARGS=$(jq -c --rawfile spec "$D/spec.md" --rawfile suite "$D/suite.json" --rawfile ref "$D/reference.js" \
    '. + {spec: $spec, suite: $suite, reference: $ref}' "$D/terms.json")
  exec "$CALL" sphinx_post_job "$ARGS"
fi
ARGS="${1:-}"
[ -n "$ARGS" ] || ARGS="{}"
exec "$CALL" "$TOOL" "$ARGS"
