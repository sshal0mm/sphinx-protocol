#!/usr/bin/env bash
# Builds and deploys the escrow contract, then records the deployment in
# deployments/$NETWORK.json.
#   SPHINX_CONTRACT=<account> scripts/deploy.sh            first deploy + init
#   SPHINX_CONTRACT=<account> SPHINX_UPGRADE=1 scripts/deploy.sh
#                                   replace code, keep state (layout must match)
# The owner (SPHINX_OWNER, default $SPHINX_PARENT) registers grader keys and
# must differ from the contract account, so the contract's own keys can be
# deleted later. Dev grader keys are refused unless SPHINX_ALLOW_DEV_KEYS=true.
# SPHINX_REPRODUCIBLE=1 builds with `cargo near build reproducible-wasm`
# (needs Docker and a clean, pushed commit) so the code hash can be rebuilt.
# SPHINX_WASM=<file> deploys a contract already built that way instead.
# shellcheck source=scripts/lib.sh
source "$(dirname "$0")/lib.sh"
load_env
require_tools jq cargo "$NEAR"
: "${SPHINX_CONTRACT:?set SPHINX_CONTRACT (account to deploy to)}"
: "${SPHINX_PARENT:?set SPHINX_PARENT (owner of the verifier account)}"
VERIFIER="${SPHINX_VERIFIER:-verifier.$SPHINX_PARENT}"
FEE_BPS="${SPHINX_FEE_BPS:-500}"
OWNER="${SPHINX_OWNER:-$SPHINX_PARENT}"
ALLOW_DEV_KEYS="${SPHINX_ALLOW_DEV_KEYS:-false}"
# Each grading run's enclave proof is checked on chain (SPHINX_ENCLAVE=0
# turns it off): NEAR's DCAP verifier, OutLayer's image registry, and the
# account that starts runs.
GRADER_CALLER="${SPHINX_GRADER_CALLER:-relayer.$SPHINX_PARENT}"
if [ "$NETWORK" = mainnet ]; then
  QUOTE_VERIFIER="${SPHINX_QUOTE_VERIFIER:-tee-verifier-2026-08-04.near}"; REGISTRY=worker.outlayer.near
else
  QUOTE_VERIFIER="${SPHINX_QUOTE_VERIFIER:-tee-verifier-2026-11-02.testnet}"; REGISTRY=worker.outlayer.testnet
fi
if [ "${SPHINX_ENCLAVE:-1}" = 1 ]; then
  ENCLAVE=$(jq -nc --arg v "$QUOTE_VERIFIER" --arg r "$REGISTRY" --arg c "$GRADER_CALLER" \
    '{quote_verifier: $v, measurements_from: $r, grader_caller: $c}')
else
  ENCLAVE=null
fi
# From this date the owner may close the contract once it owes nothing, and
# its whole balance returns to the owner.
RETIRE_AFTER="${SPHINX_RETIRE_AFTER:-2026-12-01T00:00:00Z}"
RETIRE_NS="$(date -j -u -f '%Y-%m-%dT%H:%M:%SZ' "$RETIRE_AFTER" +%s 2>/dev/null || date -u -d "$RETIRE_AFTER" +%s)000000000"
[ "$OWNER" != "$SPHINX_CONTRACT" ] || { echo "SPHINX_OWNER must differ from SPHINX_CONTRACT" >&2; exit 1; }

cd "$SPHINX_ROOT/crates/sphinx-contract" || exit 1
if [ -n "${SPHINX_WASM:-}" ]; then
  # A prebuilt contract, such as the reproducible build made elsewhere: it is
  # deployed exactly as given, and its hash is what the record shows.
  [ -f "$SPHINX_WASM" ] || { echo "SPHINX_WASM=$SPHINX_WASM is not a file" >&2; exit 1; }
elif [ "${SPHINX_REPRODUCIBLE:-0}" = 1 ]; then
  cargo near build reproducible-wasm >/dev/null
else
  cargo near build non-reproducible-wasm >/dev/null
fi
WASM="${SPHINX_WASM:-$SPHINX_ROOT/target/near/sphinx_contract/sphinx_contract.wasm}"
CODE_HASH=$(sha256_file "$WASM")

if [ "${SPHINX_UPGRADE:-0}" = 1 ]; then
  OUT=$("$NEAR" contract deploy "$SPHINX_CONTRACT" use-file "$WASM" without-init-call \
    network-config "$NETWORK" sign-with-keychain send 2>&1)
else
  OUT=$("$NEAR" contract deploy "$SPHINX_CONTRACT" use-file "$WASM" with-init-call new \
    json-args "{\"owner\":\"$OWNER\",\"verifier\":\"$VERIFIER\",\"fee_bps\":$FEE_BPS,\"allow_dev_keys\":$ALLOW_DEV_KEYS,\"enclave\":$ENCLAVE,\"retire_after_ns\":\"$RETIRE_NS\"}" prepaid-gas '100 Tgas' attached-deposit '0 NEAR' \
    network-config "$NETWORK" sign-with-keychain send 2>&1)
fi
TX=$(grep -oE 'Transaction ID: [A-Za-z0-9]+' <<<"$OUT" | head -1 | cut -d' ' -f3)
[ -n "$TX" ] || { echo "$OUT" >&2; exit 1; }

# Updates only the contract fields; other sections (such as the grader) are kept.
mkdir -p "$SPHINX_ROOT/deployments"
RECORD="$SPHINX_ROOT/deployments/$NETWORK.json"
[ -s "$RECORD" ] || echo '{}' > "$RECORD"
if [ "${SPHINX_UPGRADE:-0}" = 1 ]; then
  FIELDS=$(jq -n --arg h "$CODE_HASH" --arg tx "$TX" '{code_sha256:$h, upgrade_tx:$tx}')
else
  FIELDS=$(jq -n --arg c "$SPHINX_CONTRACT" --arg h "$CODE_HASH" --arg tx "$TX" --arg v "$VERIFIER" \
    --argjson fee "$FEE_BPS" --arg n "$NETWORK" --arg o "$OWNER" --argjson dev "$ALLOW_DEV_KEYS" \
    '{network:$n, contract:$c, owner:$o, code_sha256:$h, deploy_tx:$tx, verifier:$v, fee_bps:$fee, allow_dev_keys:$dev}')
fi
jq --argjson f "$FIELDS" '. + $f' "$RECORD" > "$RECORD.tmp" && mv "$RECORD.tmp" "$RECORD"
echo "deployed $SPHINX_CONTRACT (code $CODE_HASH, tx $TX)"
