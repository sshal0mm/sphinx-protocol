#!/usr/bin/env bash
# Runs the published grader's `identity` op inside OutLayer's TDX enclave and
# prints its JSON response: the enclave signing key and the box key
# submissions are encrypted to. Register both with `set_grader_key` after
# checking the run's attestation (printed as `tx <hash>` on stderr).
#
# The enclave grader takes no plaintext inputs: grading happens only through
# sphinx-relayer, for submissions pending on chain, with the code encrypted to
# the box key and the suite read from the buyer's stored secret.
# Usage: scripts/outlayer-grade.sh
# Env: SPHINX_GRADER_URL, SPHINX_GRADER_SHA256 (from deployments/testnet.json),
#      SPHINX_RELAYER (OutLayer caller; the keys belong to this account)
set -euo pipefail
NEAR="${NEAR:-$(command -v near || echo "$HOME/.cargo/bin/near")}"
NETWORK="${NETWORK:-testnet}"
if [ "$NETWORK" = mainnet ]; then OUTLAYER="${OUTLAYER_CONTRACT:-outlayer.near}"; else OUTLAYER="${OUTLAYER_CONTRACT:-outlayer.testnet}"; fi
: "${SPHINX_GRADER_URL:?set SPHINX_GRADER_URL}"
: "${SPHINX_GRADER_SHA256:?set SPHINX_GRADER_SHA256}"
: "${SPHINX_RELAYER:?set SPHINX_RELAYER}"
WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT
# Never pay for a run on a file that is not served with the expected hash.
SERVED=$(curl -sfL "$SPHINX_GRADER_URL" | shasum -a 256 | cut -d' ' -f1)
[ "$SERVED" = "$SPHINX_GRADER_SHA256" ] || { echo "$SPHINX_GRADER_URL is not served with hash $SPHINX_GRADER_SHA256 (got $SERVED)" >&2; exit 1; }
LIMITS='{"max_instructions":2000000000,"max_memory_mb":512,"max_execution_seconds":60}'

jq -n --arg url "$SPHINX_GRADER_URL" --arg sha "$SPHINX_GRADER_SHA256" --argjson limits "$LIMITS" '{
  source: {WasmUrl: {url: $url, hash: $sha, build_target: "wasm32-wasip2"}},
  resource_limits: $limits,
  input_data: ({op: "identity", schema: "sphinx.grader/1"} | tojson),
  secrets_ref: null, response_format: "Json", payer_account_id: null, params: null}' > "$WORK/args.json"

COST=$("$NEAR" contract call-function as-read-only "$OUTLAYER" estimate_execution_cost \
  json-args "{\"resource_limits\":$LIMITS,\"input_len\":64}" \
  network-config "$NETWORK" now 2>/dev/null | tr -d '"\n')
# Attach the estimate exactly, in yoctoNEAR: rounding to NEAR decimals can
# land just below it and the run is refused.
DEPOSIT="$(python3 -c "print(int('$COST'))") yoctoNEAR"

"$NEAR" contract call-function as-transaction "$OUTLAYER" request_execution file-args "$WORK/args.json" \
  prepaid-gas '300 Tgas' attached-deposit "$DEPOSIT" sign-as "$SPHINX_RELAYER" \
  network-config "$NETWORK" sign-with-keychain send >"$WORK/out" 2>"$WORK/err" || { cat "$WORK/err" >&2; exit 1; }
grep -oE "Transaction ID: [A-Za-z0-9]+" "$WORK/err" "$WORK/out" | head -1 | sed "s/.*: /tx /" >&2 || true
# The first run of a new build compiles it, and that can outlast the
# contract's wait: the run then reports nothing here. Run it again; the build
# is cached by then and the keys are the same.
jq -e 'select(.ok == true and .op == "identity")' "$WORK/out" ||
  { echo "no identity in the output; on a new build's first run, run again" >&2; exit 4; }
