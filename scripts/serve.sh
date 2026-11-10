#!/usr/bin/env bash
# Runs a Sphinx service against the network in .env:
#   scripts/serve.sh api       job API (gasless relay when a relayer key is available)
#   scripts/serve.sh relayer   grades jobs in the enclave and posts the signed results
#   scripts/serve.sh house     the newcomer box: jobs drafted from plain words, paid by the buyer
# The grader build comes from deployments/<network>.json unless SPHINX_GRADER_URL
# and SPHINX_GRADER_SHA256 are set. SPHINX_RELAYER_KEY is taken from the
# environment, or else read from the OS keychain for SPHINX_RELAYER_ACCOUNT.
# The API relays gas-free calls as SPHINX_GAS_ACCOUNT (default gas.$SPHINX_PARENT).
# shellcheck source=scripts/lib.sh
source "$(dirname "$0")/lib.sh"
load_env
require_tools jq cargo
: "${SPHINX_CONTRACT:?set SPHINX_CONTRACT}"
export SPHINX_CONTRACT
DEPLOYMENT="$SPHINX_ROOT/deployments/$NETWORK.json"
SPHINX_RELAYER_ACCOUNT="${SPHINX_RELAYER_ACCOUNT:-$(jq -r .grader.caller "$DEPLOYMENT")}"
export SPHINX_RELAYER_ACCOUNT

relayer_key() {
  [ -n "${SPHINX_RELAYER_KEY:-}" ] && return 0
  # Without the near CLI there is no keychain to read; the caller decides
  # whether running keyless is allowed.
  [ -x "$NEAR" ] || command -v "$NEAR" >/dev/null 2>&1 || return 0
  SPHINX_RELAYER_KEY=$("$NEAR" account export-account "$SPHINX_RELAYER_ACCOUNT" using-private-key \
    network-config "$NETWORK" 2>&1 | grep -oE 'ed25519:[1-9A-HJ-NP-Za-km-z]{80,}' | head -1 || true)
  export SPHINX_RELAYER_KEY
}

# Runs a service and starts it again whenever it exits with 75 (it asked to
# be restarted after losing the chain). Any other exit ends supervision.
supervise() {
  while true; do
    # `set -e` would end the script on the first non-zero exit, before the
    # status can be checked.
    status=0
    "$@" || status=$?
    [ "$status" -eq 75 ] || exit "$status"
    echo "restarting after exit 75" >&2
    sleep 2
  done
}

cd "$SPHINX_ROOT" || exit 1
case "${1:-}" in
  api)
    # Gasless relaying signs with its own account, never the grader's caller,
    # so the two services never race for nonces and draining one cannot stop
    # grading.
    SPHINX_RELAYER_ACCOUNT="${SPHINX_GAS_ACCOUNT:-gas.${SPHINX_PARENT:-sphinx999.testnet}}"
    unset SPHINX_RELAYER_KEY
    relayer_key
    [ -n "${SPHINX_RELAYER_KEY:-}" ] || { unset SPHINX_RELAYER_ACCOUNT SPHINX_RELAYER_KEY; echo "no key for the gas account: running read-only (relaying disabled)" >&2; }
    code_reader_token
    supervise cargo run -q --release -p sphinx-api
    ;;
  relayer)
    relayer_key
    [ -n "${SPHINX_RELAYER_KEY:-}" ] || { echo "no key for $SPHINX_RELAYER_ACCOUNT" >&2; exit 78; }
    SPHINX_GRADER_URL="${SPHINX_GRADER_URL:-$(jq -r .grader.url "$DEPLOYMENT")}"
    SPHINX_GRADER_SHA256="${SPHINX_GRADER_SHA256:-$(jq -r .grader.wasm_sha256 "$DEPLOYMENT")}"
    export SPHINX_GRADER_URL SPHINX_GRADER_SHA256
    code_reader_token
    shift
    if [ "${1:-}" = --once ]; then exec cargo run -q --release -p sphinx-relayer -- "$@"; fi
    supervise cargo run -q --release -p sphinx-relayer -- "$@"
    ;;
  house)
    # The newcomer box: drafts jobs from plain words; buyers pay and post
    # from their own wallets. Holds the model key only.
    SPHINX_MODEL_KEY="${SPHINX_MODEL_KEY:-${NEAR_AI_API_KEY:-}}"
    if [ -z "$SPHINX_MODEL_KEY" ] && command -v security >/dev/null 2>&1; then
      SPHINX_MODEL_KEY=$(security find-generic-password -s sphinx-near-ai -a api-key -w 2>/dev/null || true)
    fi
    [ -n "$SPHINX_MODEL_KEY" ] || { echo "set SPHINX_MODEL_KEY for the concierge" >&2; exit 78; }
    export SPHINX_MODEL_KEY
    supervise cargo run -q --release -p sphinx-house
    ;;
  *)
    echo "usage: scripts/serve.sh api|relayer [--once]|house" >&2
    exit 64
    ;;
esac
