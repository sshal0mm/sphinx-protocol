#!/usr/bin/env bash
# Runs real attacks against the deployed escrow and the job API on testnet,
# records every refusal with its transaction, and writes docs/ATTACKS.md.
#   scripts/attacks.sh          (needs `make api` running, testnet keys in the keychain)
# Each on-chain attack is a real transaction that the contract refuses with a
# stable error code; `make attacks-check` later re-reads those transactions
# with no keys. The HTTP attacks try to read code the caller may not see.
# shellcheck source=scripts/lib.sh
source "$(dirname "$0")/lib.sh"
load_env
require_tools jq cargo curl python3 "$NEAR"
: "${SPHINX_PARENT:?set SPHINX_PARENT}"
: "${SPHINX_CONTRACT:?set SPHINX_CONTRACT}"
P="$SPHINX_PARENT"
API="${SPHINX_API:-http://127.0.0.1:8787}"
RPC_URL="${SPHINX_RPC:-https://rpc.testnet.fastnear.com}"
DEPLOYMENT="$SPHINX_ROOT/deployments/$NETWORK.json"
RELAYER=$(jq -r .grader.caller "$DEPLOYMENT")
export SPHINX_GRADER_URL SPHINX_GRADER_SHA256
SPHINX_GRADER_URL=$(jq -r .grader.url "$DEPLOYMENT")
SPHINX_GRADER_SHA256=$(jq -r .grader.wasm_sha256 "$DEPLOYMENT")
cd "$SPHINX_ROOT" || exit 1
code_reader_token
cargo build -q --release -p sphinx-cli -p sphinx-relayer
SPHINX="$SPHINX_ROOT/target/release/sphinx"
F=fixtures/fib
OUT="$SPHINX_ROOT/deployments/attacks/$NETWORK.json"
WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT
echo '[]' > "$WORK/results.json"
curl -sf "$API/v1" >/dev/null || { echo "the job API at $API is not answering; start it with make api" >&2; exit 1; }

key_of() {
  "$NEAR" account export-account "$1" using-private-key network-config "$NETWORK" 2>&1 \
    | grep -oE 'ed25519:[1-9A-HJ-NP-Za-km-z]{80,}' | head -1
}
relay_pass() {
  SPHINX_RELAYER_ACCOUNT="$RELAYER" SPHINX_RELAYER_KEY="$(key_of "$RELAYER")" SPHINX_API="$API" \
    target/release/sphinx-relayer --once 2>&1 | tee -a "$WORK/relay.log" | grep -E '^(posted|failed|waiting)' | sed -E 's/  tx .*//; s/^/   relayer: /'
}
# POST to the FastNear transactions API, retried while it is rate limited.
txapi() { # PATH JSON
  local out
  for _ in 1 2 3 4 5 6; do
    out=$(curl -s -X POST "${SPHINX_TX_API:-https://tx.test.fastnear.com}$1" -H 'content-type: application/json' -d "$2")
    jq -e 'type == "object" and (has("account_txs") or has("transactions"))' <<<"$out" >/dev/null 2>&1 && { echo "$out"; return; }
    sleep 5
  done
  echo "the transactions API did not answer: $out" >&2; exit 1
}
record() { # NAME ATTACKER WHAT EXPECTED GOT TX KIND
  jq --arg n "$1" --arg a "$2" --arg w "$3" --arg e "$4" --arg g "$5" --arg t "$6" --arg k "$7" \
    '. + [{name: $n, attacker: $a, attempt: $w, expected: $e, got: $g, tx_hash: (if $t == "" then null else $t end), kind: $k}]' \
    "$WORK/results.json" > "$WORK/r" && mv "$WORK/r" "$WORK/results.json"
  if [ "$4" = "$5" ]; then echo "   result: $5"; else echo "FAIL: $1 expected $4, got ${5:-success}" >&2; exit 1; fi
}
# attack NAME SIGNER WHAT EXPECTED METHOD JSON [DEPOSIT]: a real transaction the contract must refuse.
attack() {
  local name="$1" signer="$2" what="$3" expected="$4" method="$5" args="$6" deposit="${7:-0 NEAR}" signed out code tx
  echo "== $name: $what"
  # Signed by the near CLI, sent over RPC: a refused call still returns its
  # transaction hash, which the CLI does not print.
  signed=$("$NEAR" contract call-function as-transaction "$SPHINX_CONTRACT" "$method" json-args "$args" \
    prepaid-gas '100 Tgas' attached-deposit "$deposit" sign-as "$signer" network-config "$NETWORK" \
    sign-with-keychain display 2>&1 | grep -A1 'Signed transaction (serialized as base64)' | tail -1 | tr -d ' ')
  out=$(curl -s "$RPC_URL" -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"send_tx\",\"params\":{\"signed_tx_base64\":\"$signed\",\"wait_until\":\"FINAL\"}}")
  tx=$(jq -r '.result.transaction.hash // empty' <<<"$out")
  code=$(grep -oE 'SPHINX_E_[A-Z0-9_]+' <<<"$out" | head -1 || true)
  record "$name" "$signer" "$what" "$expected" "$code" "$tx" chain
}
# read_code NAME WHO WHAT HASH EXPECTED_STATUS [ACCOUNT]: an HTTP read of stored code.
read_code() {
  local name="$1" who="$2" what="$3" hash="$4" expected="$5" account="${6:-}" status
  echo "== $name: $what"
  # The API caches chain reads for up to 45 s, and a stale answer can only
  # refuse; an allowed read is retried until the cache has caught up.
  local tries=1; [ "$expected" = 200 ] && tries=12
  for _ in $(seq 1 "$tries"); do
    if [ -n "$account" ]; then
      SPHINX_SIGNER_KEY="$(key_of "$account")" "$SPHINX" code get "$hash" --api "$API" --as "$who" >/dev/null 2>&1 && status=200 || status=404
    else
      "$SPHINX" code get "$hash" --api "$API" >/dev/null 2>&1 && status=200 || status=404
    fi
    [ "$status" = "$expected" ] && break
    sleep 5
  done
  record "$name" "$who" "$what" "HTTP $expected" "HTTP $status" "" http
}
store() { curl -sf -X POST "$API/v1/code" --data-binary @"$1" | jq -r .code_hash; }

CONFIG=$(near_view get_config)
JOB_DEPOSIT=$(jq -r .job_storage_deposit <<<"$CONFIG")
SUB_DEPOSIT=$(jq -r .submission_storage_deposit <<<"$CONFIG")
REWARD=300000000000000000000000
BOND=10000000000000000000000
yocto() { python3 -c "print(int($1))"; }

echo "== setup: buyer posts a job and the enclave opens it"
DEADLINE=$(( ($(date +%s) + 3600) * 1000000000 ))
SPEC=$(store $F/spec.md)
JOB=$(near_call "buyer.$P" post_job "{\"title\":\"Attack gallery target\",\"spec_uri\":\"$API/v1/code/$SPEC\",\"runtime\":\"js-boa/1\",\"reward\":\"$REWARD\",\"bond\":\"$BOND\",\"max_subs_per_agent\":2,\"max_subs_total\":5,\"deadline_ns\":\"$DEADLINE\"}" \
  "$(yocto "$REWARD+$JOB_DEPOSIT") yoctoNEAR" | tr -d '"')
near_call "buyer.$P" commit_suite "{\"job_id\":\"$JOB\",\"suite_hash\":\"$("$SPHINX" suite hash $F/suite.json)\",\"ref_solution_hash\":\"$(sha256_file $F/reference.js)\",\"grader_public_key\":\"$(jq -r .grader.public_key <<<"$CONFIG")\"}" >/dev/null
SPHINX_SIGNER_KEY="$(key_of "buyer.$P")" "$SPHINX" suite store --job-id "$JOB" --suite $F/suite.json --reference $F/reference.js --buyer "buyer.$P" >/dev/null
relay_pass
echo "   job $JOB is $(near_view get_job "{\"job_id\":\"$JOB\"}" | jq -r .status)"

# Each agent's code carries a marker, to look for it later in public transaction data.
MARK="sphinx-attack-marker-$JOB"
printf '%s\n// %s-a\n' "$(cat $F/agent-a.js)" "$MARK" > "$WORK/a.js"
printf '%s\n// %s-b\n' "$(cat $F/agent-b.js)" "$MARK" > "$WORK/b.js"
HA=$(store "$WORK/a.js"); HB=$(store "$WORK/b.js")
SUBDEP="$(yocto "$BOND+$SUB_DEPOSIT") yoctoNEAR"

attack "underpaid bond" "agent-a.$P" "submit with one yoctoNEAR less than the bond and storage deposit" \
  SPHINX_E_SUBMIT_WRONG_DEPOSIT submit "{\"job_id\":\"$JOB\",\"code_uri\":\"$API/v1/code/$HA\",\"code_hash\":\"$HA\"}" \
  "$(yocto "$BOND+$SUB_DEPOSIT-1") yoctoNEAR"
attack "buyer grades itself" "buyer.$P" "the buyer submits to its own job" \
  SPHINX_E_SUBMIT_BUYER_SELF submit "{\"job_id\":\"$JOB\",\"code_uri\":\"$API/v1/code/$HB\",\"code_hash\":\"$HB\"}" "$SUBDEP"
near_call "agent-a.$P" submit "{\"job_id\":\"$JOB\",\"code_uri\":\"$API/v1/code/$HA\",\"code_hash\":\"$HA\"}" "$SUBDEP" >/dev/null
echo "   agent-a submitted (sub 0)"
attack "copied code" "agent-b.$P" "agent-b submits the exact code agent-a already submitted" \
  SPHINX_E_SUBMIT_DUPLICATE_CODE submit "{\"job_id\":\"$JOB\",\"code_uri\":\"$API/v1/code/$HA\",\"code_hash\":\"$HA\"}" "$SUBDEP"
attack "attempt flooding" "agent-a.$P" "agent-a submits again while its first attempt is still pending" \
  SPHINX_E_SUBMIT_PENDING_EXISTS submit "{\"job_id\":\"$JOB\",\"code_uri\":\"$API/v1/code/$HB\",\"code_hash\":\"$HB\"}" "$SUBDEP"
SUB0=$(near_view get_submission "{\"job_id\":\"$JOB\",\"sub_id\":0}")
SUITE_HASH=$(near_view get_job "{\"job_id\":\"$JOB\"}" | jq -r .commit.suite_hash)
FORGED=$(jq -c --arg c "$SPHINX_CONTRACT" --arg s "$(printf '0%.0s' {1..128})" --arg sh "$SUITE_HASH" \
  '{score: {contract: $c, job_id: .job_id, sub_id: .sub_id, runtime: "js-boa/1", passed: 12, total: 12, suite_hash: $sh, code_hash: .code_hash, nonce: .nonce}, signature_hex: $s}' <<<"$SUB0")
attack "forged score" "agent-a.$P" "agent-a posts a 12/12 score for itself with a made-up signature" \
  SPHINX_E_SIG_INVALID record_score "$FORGED"
# A genuine enclave signature from an earlier job, pointed at this submission.
REPLAY=$(jq -c --arg j "$JOB" '[.results[] | select(.kind == "score" and .args.score.passed == .args.score.total)][0].args | .score.job_id = $j | .score.sub_id = 0' \
  deployments/runs/testnet-job2.json)
attack "replayed score" "agent-a.$P" "agent-a reuses the enclave's real 12/12 signature from job 2 for its own submission" \
  SPHINX_E_SCORE_FIELD_MISMATCH record_score "$REPLAY"
attack "grader takeover" "buyer.$P" "the buyer registers its own key as the grader" \
  SPHINX_E_AUTH_NOT_OWNER set_grader_key "{\"public_key_hex\":\"$(printf '1%.0s' {1..64})\",\"box_public_key_hex\":\"$(printf '2%.0s' {1..64})\",\"mode\":\"outlayer_tdx\",\"wasm_sha256\":\"$(printf '3%.0s' {1..64})\",\"attestation_ref\":\"x\"}" \
  "1 yoctoNEAR"
attack "bond grab" "buyer.$P" "the buyer reclaims agent-a's pending bond before its grading window ends" \
  SPHINX_E_RECLAIM_TOO_EARLY reclaim_bond "{\"job_id\":\"$JOB\",\"sub_id\":0}"
attack "early refund" "buyer.$P" "the buyer takes the reward back before the deadline" \
  SPHINX_E_REFUND_TOO_EARLY refund_on_timeout "{\"job_id\":\"$JOB\"}"
attack "cancel after work" "buyer.$P" "the buyer cancels the job once an agent has submitted" \
  SPHINX_E_JOB_HAS_SUBMISSIONS cancel_job "{\"job_id\":\"$JOB\"}"

read_code "anonymous read" "anyone" "read agent-a's pending code with no signature" "$HA" 404
read_code "competitor read" "agent-b.$P" "agent-b signs a request for agent-a's pending code" "$HA" 404 "agent-b.$P"
read_code "early buyer read" "buyer.$P" "the buyer reads agent-a's code before paying" "$HA" 404 "buyer.$P"
read_code "borrowed name" "agent-a.$P" "agent-b signs with its own key but claims to be agent-a" "$HA" 404 "agent-b.$P"
read_code "own code" "agent-a.$P" "agent-a reads its own code (allowed)" "$HA" 200 "agent-a.$P"

near_call "agent-b.$P" submit "{\"job_id\":\"$JOB\",\"code_uri\":\"$API/v1/code/$HB\",\"code_hash\":\"$HB\"}" "$SUBDEP" >/dev/null
echo "   agent-b submitted (sub 1); the relayer grades both in the enclave"
relay_pass
for _ in 1 2 3 4 5 6; do [ "$(near_view get_job "{\"job_id\":\"$JOB\"}" | jq -r .status)" = Paid ] && break; sleep 3; done

SCORE_TX=$(grep -E "^posted +job $JOB sub 1 " "$WORK/relay.log" | grep -oE 'tx [A-Za-z0-9]+' | tail -1 | cut -d' ' -f2)
GENUINE_ARGS=$(curl -s "${SPHINX_RPC:-https://rpc.testnet.fastnear.com}" -H 'content-type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"EXPERIMENTAL_tx_status\",\"params\":{\"tx_hash\":\"$SCORE_TX\",\"sender_account_id\":\"$RELAYER\",\"wait_until\":\"FINAL\"}}" \
  | jq -c '.result.transaction.actions[0].FunctionCall.args | @base64d | fromjson')
[ "$GENUINE_ARGS" != null ] || { echo "FAIL: could not find the real score for job $JOB" >&2; exit 1; }
attack "replayed real score" "agent-b.$P" "agent-b posts the enclave's real 12/12 score for its submission a second time" \
  SPHINX_E_JOB_WRONG_STATE record_score "$GENUINE_ARGS"

read_code "paid buyer, winner" "buyer.$P" "the buyer reads the winning code of the job it paid (allowed)" "$HB" 200 "buyer.$P"
read_code "paid buyer, loser" "buyer.$P" "the buyer reads agent-a's failed code" "$HA" 404 "buyer.$P"

echo "== in transit: is any submitted code readable in public transaction data?"
RTX=$(txapi /v0/account "{\"account_id\":\"$RELAYER\"}" | jq -c '[.account_txs[].transaction_hash][:20]')
txapi /v0/transactions "{\"tx_hashes\":$RTX}" | jq -c '[.transactions[] | {hash: .transaction.hash, args: ([.transaction.actions[]?.FunctionCall? | select(. != null and .method_name == "request_execution") | .args | @base64d] | join(""))} | select(.args != "")]' > "$WORK/runs.json"
LEAKS=$(jq --arg m "$MARK" '[.[] | select(.args | contains($m))] | length' "$WORK/runs.json")
BOXED=$(jq --arg j "\\\"job_id\\\":\\\"$JOB\\\"" '[.[] | select((.args | contains("submission_box")) and (.args | contains($j)))] | .[0].hash // ""' -r "$WORK/runs.json")
echo "   grading runs carrying an encrypted submission for job $JOB: ${BOXED:-none found}; plaintext copies of the code: $LEAKS"
[ "$LEAKS" = 0 ] || { echo "FAIL: submitted code appears in plain text on chain" >&2; exit 1; }
record "code in transit" "anyone reading the chain" "look for the agents' code in the relayer's public enclave requests" \
  "no plaintext" "no plaintext" "$BOXED" chain-read

jq -n --arg net "$NETWORK" --arg c "$SPHINX_CONTRACT" --arg job "$JOB" --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg grader "$SPHINX_GRADER_SHA256" --slurpfile r "$WORK/results.json" \
  '{schema: "sphinx.attacks/1", network: $net, contract: $c, job_id: $job, grader: $grader, recorded_at: $at, attacks: $r[0]}' > "$OUT"
scripts/attacks-doc.sh "$OUT" > docs/ATTACKS.md
echo "PASS: $(jq '[.attacks[] | select(.got != "HTTP 200")] | length' "$OUT") attacks refused, $(jq '[.attacks[] | select(.got == "HTTP 200")] | length' "$OUT") allowed reads served -> ${OUT#"$SPHINX_ROOT/"}, docs/ATTACKS.md"
