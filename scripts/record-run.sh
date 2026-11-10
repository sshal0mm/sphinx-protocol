#!/usr/bin/env bash
# Saves everything needed to check one job later without the network:
# contract config, the job and its submissions, the grader's identity run
# and attestation, and every signed result posted for the job (the
# record_suite_check and record_score arguments, with transaction hashes).
# Public RPC nodes drop transaction data after a few days, so the evidence
# lives in the repo: `sphinx verify --bundle` reads only this file.
#   scripts/record-run.sh <job_id>      (read-only; no keys)
# Writes deployments/runs/$NETWORK-job<id>.json.
# shellcheck source=scripts/lib.sh
source "$(dirname "$0")/lib.sh"
load_env
require_tools jq curl python3 cargo
JOB="${1:?usage: scripts/record-run.sh <job_id>}"
DEPLOYMENT="$SPHINX_ROOT/deployments/$NETWORK.json"
CONTRACT="${SPHINX_CONTRACT:-$(jq -r .contract "$DEPLOYMENT")}"
RPC="${SPHINX_RPC:-$DEFAULT_RPC}"
TXAPI="${SPHINX_TX_API:-$TX_API}"
WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT

rpc() { # METHOD PARAMS_JSON, retried on rate limits
  local out
  for _ in 1 2 3 4 5; do
    out=$(curl -s "$RPC" -H 'content-type: application/json' \
      -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":$2}")
    jq -e '.result' <<<"$out" >/dev/null 2>&1 && { jq '.result' <<<"$out"; return; }
    sleep 2
  done
  echo "rpc $1 failed: $out" >&2; exit 1
}
view() { # METHOD ARGS_JSON
  local args; args=$(printf '%s' "$2" | base64 | tr -d '\n')
  rpc query "{\"request_type\":\"call_function\",\"finality\":\"final\",\"account_id\":\"$CONTRACT\",\"method_name\":\"$1\",\"args_base64\":\"$args\"}" \
    | jq -r '.result | implode' | jq .
}

CONFIG=$(view get_config '{}')
HISTORY=$(view get_grader_history '{}')
JOBV=$(view get_job "{\"job_id\":\"$JOB\"}")
[ "$JOBV" != null ] || { echo "job $JOB does not exist on $CONTRACT" >&2; exit 1; }
SUBS=$(view list_submissions "{\"job_id\":\"$JOB\",\"limit\":50}")

# The grader registration: the identity run's exact output and its attestation.
# The job keeps the grader key it was committed with, so pick that grader
# release, current or previous.
JOB_KEY=$(jq -r '.commit.grader.public_key // empty' <<<"$JOBV")
GRADER=$(jq -c --arg k "$JOB_KEY" '[.grader, (.previous_graders // [])[]] | map(select(.public_key == $k)) | .[0] // empty' "$DEPLOYMENT")
[ -n "$GRADER" ] || { echo "job $JOB was committed with grader key $JOB_KEY, which no release in ${DEPLOYMENT#"$SPHINX_ROOT/"} lists" >&2; exit 1; }
IDENTITY_TX=$(jq -r .identity_tx <<<"$GRADER")
CALLER=$(jq -r .caller <<<"$GRADER")
IDENTITY_OUT=$(rpc EXPERIMENTAL_tx_status "{\"tx_hash\":\"$IDENTITY_TX\",\"sender_account_id\":\"$CALLER\",\"wait_until\":\"FINAL\"}" \
  | jq -r '.status.SuccessValue')
ATTESTATION=$(curl -s "$(jq -r .attestation <<<"$GRADER")")
jq -e '.output_hash' <<<"$ATTESTATION" >/dev/null || { echo "attestation not readable" >&2; exit 1; }

# Every signed result posted for this job, from the escrow's transaction list.
curl -s -X POST "$TXAPI/v0/account" -H 'content-type: application/json' \
  -d "{\"account_id\":\"$CONTRACT\"}" | jq '[.account_txs[] | select(.is_success and .is_function_call) | .transaction_hash]' > "$WORK/hashes.json"
python3 - "$WORK/hashes.json" "$TXAPI" "$JOB" > "$WORK/results.json" <<'PY'
import base64, json, sys, urllib.request
hashes, api, job = json.load(open(sys.argv[1])), sys.argv[2], sys.argv[3]
results = []
for i in range(0, len(hashes), 20):
    req = urllib.request.Request(api + "/v0/transactions", method="POST",
        data=json.dumps({"tx_hashes": hashes[i:i + 20]}).encode(),
        headers={"content-type": "application/json"})
    for t in json.load(urllib.request.urlopen(req, timeout=30))["transactions"]:
        tx = t["transaction"]
        for a in tx["actions"]:
            fc = a.get("FunctionCall") if isinstance(a, dict) else None
            if not fc or fc["method_name"] not in ("record_score", "record_suite_check"):
                continue
            args = json.loads(base64.b64decode(fc["args"]))
            body = args.get("score") or args.get("check")
            if str(body.get("job_id")) != job:
                continue
            results.append({"kind": "score" if "score" in args else "suite_check",
                            "tx_hash": tx["hash"], "signer": tx["signer_id"], "args": args})
results.sort(key=lambda r: (r["kind"] != "suite_check", r["args"].get("score", {}).get("sub_id", -1)))
print(json.dumps(results))
PY

mkdir -p "$SPHINX_ROOT/deployments/runs"
OUT="$SPHINX_ROOT/deployments/runs/$NETWORK-job$JOB.json"
CODE=$(rpc query "{\"request_type\":\"view_account\",\"finality\":\"final\",\"account_id\":\"$CONTRACT\"}" | jq -r .code_hash \
  | python3 -c "import sys;A='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';n=0
for ch in sys.stdin.read().strip(): n=n*58+A.index(ch)
print(n.to_bytes(32,'big').hex())")
jq -n --arg code "$CODE" --arg net "$NETWORK" --arg explorer "$EXPLORER" --arg c "$CONTRACT" --arg job "$JOB" --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --argjson config "$CONFIG" --argjson history "$HISTORY" --argjson jobv "$JOBV" --argjson subs "$SUBS" \
  --arg itx "$IDENTITY_TX" --arg caller "$CALLER" --arg iout "$IDENTITY_OUT" --argjson att "$ATTESTATION" \
  --arg reg "$(jq -r .registration_tx <<<"$GRADER")" --slurpfile results "$WORK/results.json" '{
    schema: "sphinx.run/1", network: $net, contract: $c, job_id: $job, recorded_at: $at, code_sha256: $code,
    config: $config, grader_history: $history, job: $jobv, submissions: $subs,
    grader: {identity_tx: $itx, caller: $caller, identity_output_base64: $iout, attestation: $att, registration_tx: $reg},
    results: $results[0],
    explorer: ($explorer + "/address/" + $c)
  }' > "$OUT"
# Intel's collateral for the TDX quote, so the quote can be checked offline.
cargo run -q -p sphinx-cli -- verify --fetch-collateral "$OUT" >/dev/null
echo "recorded job $JOB on $CONTRACT: $(jq '.results | length' "$OUT") signed results -> ${OUT#"$SPHINX_ROOT/"}"
