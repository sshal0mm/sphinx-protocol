#!/usr/bin/env bash
# Re-checks a recorded attack gallery with no keys: every on-chain attack's
# transaction is read back from the network, and must have failed with the
# recorded error code.
#   scripts/attacks-check.sh [deployments/attacks/testnet.json]
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IN="${1:-$ROOT/deployments/attacks/testnet.json}"
TXAPI="${SPHINX_TX_API:-https://tx.test.fastnear.com}"
hashes=$(jq -c '[.attacks[] | select(.kind == "chain" and .tx_hash != null) | .tx_hash]' "$IN")
[ "$(jq length <<<"$hashes")" -gt 0 ] || { echo "no recorded transactions to check" >&2; exit 1; }
txs=$(curl -s -X POST "$TXAPI/v0/transactions" -H 'content-type: application/json' -d "{\"tx_hashes\":$hashes}")
failed=0
while IFS=$'\t' read -r name hash code; do
  outcome=$(jq -c --arg h "$hash" '[.transactions[] | select(.transaction.hash == $h)][0]' <<<"$txs")
  if [ "$outcome" = null ]; then echo "?     $name: $hash not found"; failed=$((failed + 1)); continue; fi
  if jq -e --arg c "$code" '(.execution_outcome.outcome.status | has("Failure")) and (tostring | contains($c))' <<<"$outcome" >/dev/null 2>&1 \
    || jq -e --arg c "$code" '([.receipts[]?.execution_outcome.outcome.status | select(has("Failure"))] | length > 0) and (tostring | contains($c))' <<<"$outcome" >/dev/null 2>&1; then
    echo "ok    $name: $hash failed with $code"
  else
    echo "FAIL  $name: $hash did not fail with $code"; failed=$((failed + 1))
  fi
done < <(jq -r '.attacks[] | select(.kind == "chain" and .tx_hash != null) | [.name, .tx_hash, .got] | @tsv' "$IN")
if [ "$failed" != 0 ]; then echo "$failed not confirmed"; exit 1; fi
echo "every recorded attack was refused on chain"
