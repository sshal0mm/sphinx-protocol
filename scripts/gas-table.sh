#!/usr/bin/env bash
# Measures what each escrow method really costs, from the deployed contract's
# own transactions on the network (no keys), and writes docs/GAS.md.
#   scripts/gas-table.sh > docs/GAS.md
# Gas is summed over the transaction and every receipt it caused (payouts and
# their callbacks included); cost is the NEAR actually burnt for that gas.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NETWORK="${NETWORK:-testnet}"
CONTRACT="${SPHINX_CONTRACT:-$(jq -r .contract "$ROOT/deployments/$NETWORK.json")}"
TXAPI="${SPHINX_TX_API:-https://tx.test.fastnear.com}"
WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT

post() { # PATH JSON, retried while rate limited
  local out
  for _ in 1 2 3 4 5 6; do
    out=$(curl -s -X POST "$TXAPI$1" -H 'content-type: application/json' -d "$2")
    jq -e 'type == "object"' <<<"$out" >/dev/null 2>&1 && { echo "$out"; return; }
    sleep 5
  done
  echo "the transactions API did not answer" >&2; exit 1
}
post /v0/account "{\"account_id\":\"$CONTRACT\"}" \
  | jq -c '[.account_txs[] | select(.is_function_call and .is_receiver) | .transaction_hash]' > "$WORK/hashes.json"
count=$(jq length "$WORK/hashes.json")
echo '[]' > "$WORK/txs.json"
for ((i = 0; i < count; i += 20)); do
  post /v0/transactions "{\"tx_hashes\":$(jq -c ".[$i:$((i + 20))]" "$WORK/hashes.json")}" \
    | jq -c --arg c "$CONTRACT" '[.transactions[] | select(.transaction.receiver_id == $c)
        | { method: ([.transaction.actions[]?.FunctionCall?.method_name | select(. != null)][0]),
            ok: ((.execution_outcome.outcome.status | tostring | contains("Failure") | not)
                and ([.receipts[]?.execution_outcome.outcome.status | tostring | select(contains("Failure"))] | length == 0)),
            gas: (.execution_outcome.outcome.gas_burnt + ([.receipts[]?.execution_outcome.outcome.gas_burnt] | add // 0)),
            burnt: ((.execution_outcome.outcome.tokens_burnt | tonumber) + ([.receipts[]?.execution_outcome.outcome.tokens_burnt | tonumber] | add // 0)) }
        | select(.method != null)]' > "$WORK/page.json"
  jq -s 'add' "$WORK/txs.json" "$WORK/page.json" > "$WORK/next.json" && mv "$WORK/next.json" "$WORK/txs.json"
done

jq -r --arg c "$CONTRACT" --arg net "$NETWORK" --arg at "$(date -u +%Y-%m-%d)" '
  def tgas: (. / 1e12 * 100 | round / 100);
  def near: (. / 1e24 * 100000 | round / 100000);
  def median: sort | .[(length / 2 | floor)];
  "# Gas: what each call costs",
  "",
  "Measured on \($net) from the deployed contract `\($c)`'\''s own successful transactions (\(map(select(.ok)) | length) of them), \($at), with `scripts/gas-table.sh`. Gas counts the transaction and every receipt it caused, payouts and their callbacks included. NEAR is what was actually burnt for that gas; deposits (rewards, bonds, storage) are not costs and are not included.",
  "",
  "| Method | Calls | Median Tgas | Max Tgas | Median NEAR burnt |",
  "|---|---|---|---|---|",
  (map(select(.ok)) | group_by(.method) | sort_by(-length)[]
    | "| `\(.[0].method)` | \(length) | \([.[].gas] | median | tgas) | \([.[].gas] | max | tgas) | \([.[].burnt] | median | near) |"),
  "",
  "No call loops over more than one job'\''s submissions, and a job has at most 64 (see [ADR 0007](adr/0007-fund-flow.md)). A payout (`record_score` or `settle`) and a refund walk that job'\''s submissions once; every other call does a fixed amount of work, so nothing grows with the number of jobs or agents. When one call returns many bonds, it transfers the first four and credits the rest to each agent'\''s `unclaimed` balance for `withdraw`, so its gas stays well inside what a caller attaches. The worst cases, measured on a local sandbox node at 64 submissions, are below."
' "$WORK/txs.json"
SANDBOX="$ROOT/deployments/sandbox-gas.json"
if [ -f "$SANDBOX" ]; then
  jq -r '"",
    "| At the cap (\(.submissions) submissions, local sandbox node, `make sandbox`) | Tgas | Share of the relayer'\''s 100 Tgas |",
    "|---|---|---|",
    "| `record_score`, the most a failure costs | \(.record_score_failure_max_tgas) | \(.record_score_failure_max_tgas)% |",
    "| `record_score` that pays, after \(.submissions - 1) failures | \(.record_score_payout_tgas) | \(.record_score_payout_tgas)% |",
    "| `refund_on_timeout`, crediting \(.submissions) held bonds | \(.refund_tgas) | \(.refund_tgas)% |",
    (if .record_score_payout_later_passes_tgas then "| `record_score` that pays the earliest of \(.submissions) passes and returns the others'\'' bonds | \(.record_score_payout_later_passes_tgas) | \(.record_score_payout_later_passes_tgas)% |" else empty end),
    (if .settle_tgas then "| `settle`, expiring \(.submissions - 1) waiting submissions and paying | \(.settle_tgas) | \(.settle_tgas)% |" else empty end)' "$SANDBOX"
fi
