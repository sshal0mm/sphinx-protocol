#!/usr/bin/env bash
# The full Sphinx loop on NEAR testnet, with assertions. Exits non-zero on any
# unexpected outcome.
#
# Nothing is hand-fed to the grader: the buyer stores the hidden suite, encrypted, for the
# enclave grader build, and the relayer service notices work on-chain, runs
# the grader inside OutLayer's TDX enclave and posts the signed results.
# Test keys are read from the OS keychain into this process only.
# shellcheck source=scripts/lib.sh
source "$(dirname "$0")/lib.sh"
load_env
require_tools jq cargo "$NEAR"
: "${SPHINX_PARENT:?set SPHINX_PARENT}"
: "${SPHINX_CONTRACT:?set SPHINX_CONTRACT}"
DEPLOYMENT="$SPHINX_ROOT/deployments/$NETWORK.json"
GRADER_SHA=$(jq -r .grader.wasm_sha256 "$DEPLOYMENT")
RELAYER=$(jq -r .grader.caller "$DEPLOYMENT")
SPHINX_GRADER_URL=$(jq -r .grader.url "$DEPLOYMENT")
SPHINX_GRADER_SHA256="$GRADER_SHA"
export SPHINX_GRADER_URL SPHINX_GRADER_SHA256

cd "$SPHINX_ROOT" || exit 1
cargo build -q --release -p sphinx-cli -p sphinx-relayer
SPHINX="$SPHINX_ROOT/target/release/sphinx"
F=fixtures/fib
P="$SPHINX_PARENT"
NEAR_YOCTO=1000000000000000000000000
REWARD=300000000000000000000000            # 0.3 NEAR
BOND=10000000000000000000000               # 0.01 NEAR
CONFIG=$(near_view get_config)
JOB_DEPOSIT=$(jq -r .job_storage_deposit <<<"$CONFIG")
SUB_DEPOSIT=$(jq -r .submission_storage_deposit <<<"$CONFIG")
yocto() { python3 -c "print(f'{($1)/$NEAR_YOCTO:.6f}')"; }

# Each demo account needs enough test NEAR for its deposits plus gas.
require_balance() { # ACCOUNT MIN_NEAR
  local have
  have=$("$NEAR" account view-account-summary "$1" network-config "$NETWORK" now 2>/dev/null \
    | grep -oE 'Native account balance +[0-9.]+' | awk '{print $4}')
  python3 -c "import sys; sys.exit(0 if float('${have:-0}') >= $2 else 1)" || {
    echo "account $1 has ${have:-0} NEAR; the demo needs at least $2. Fund it, then retry." >&2; exit 75; }
}
require_balance "buyer.$P" 0.5
require_balance "agent-a.$P" 0.1
require_balance "agent-b.$P" 0.1
require_balance "$RELAYER" 0.2

# A test key, read from the keychain into a variable; never printed.
key_of() {
  "$NEAR" account export-account "$1" using-private-key network-config "$NETWORK" 2>&1 \
    | grep -oE 'ed25519:[1-9A-HJ-NP-Za-km-z]{80,}' | head -1
}
# One relayer pass: grade everything pending and post the signed results.
relay_pass() {
  SPHINX_RELAYER_ACCOUNT="$RELAYER" SPHINX_RELAYER_KEY="$(key_of "$RELAYER")" \
    SPHINX_RELAYER_REPO_ROOT="$SPHINX_ROOT" "$SPHINX_ROOT/target/release/sphinx-relayer" --once 2>&1 \
    | grep -E '^(posted|failed|skipped|waiting)' | sed -E 's/  tx .*//; s/^/   relayer: /'
}
job_view() { near_view get_job "{\"job_id\":\"$JOB\"}"; }
# check DESCRIPTION JQ_FILTER COMMAND...: runs the view COMMAND and stops the
# demo unless the filter is true. Retries for a few seconds, since an RPC
# node can lag the transaction that just landed.
check() {
  local what="$1" filter="$2" out=""; shift 2
  for _ in 1 2 3 4 5 6; do
    out=$("$@")
    jq -e "$filter" <<<"$out" >/dev/null 2>&1 && { echo "   ok: $what"; return; }
    sleep 3
  done
  echo "FAIL: $what" >&2; echo "$out" | jq . >&2 2>/dev/null || echo "$out" >&2; exit 1
}
sub_view() { near_view get_submission "{\"job_id\":\"$JOB\",\"sub_id\":$1}"; }
stats_of() { near_view get_agent_stats "{\"account\":\"$1\"}"; }
deposit() { echo "$(yocto "$1") NEAR"; }

echo "grader: enclave build ${GRADER_SHA:0:12}... run by $RELAYER"

echo "== 1. buyer posts a job: 0.3 NEAR reward, 0.01 NEAR bond, 2 attempts per agent, 5 per job"
DEADLINE=$(( ($(date +%s) + 3600) * 1000000000 ))
JOB=$(near_call "buyer.$P" post_job "{\"spec_uri\":\"repo:$F/spec.md\",\"runtime\":\"js-boa/1\",\"reward\":\"$REWARD\",\"bond\":\"$BOND\",\"max_subs_per_agent\":2,\"max_subs_total\":5,\"deadline_ns\":\"$DEADLINE\"}" \
  "$(deposit "$REWARD+$JOB_DEPOSIT")" | tr -d '"')
echo "   job $JOB"

echo "== 2. buyer commits the suite's hash, then stores the hidden suite for the enclave grader"
GRADER_KEY=$(jq -r .grader.public_key <<<"$CONFIG")
COMMIT=$(near_call "buyer.$P" commit_suite "{\"job_id\":\"$JOB\",\"suite_hash\":\"$("$SPHINX" suite hash $F/suite.json)\",\"ref_solution_hash\":\"$(sha256_file $F/reference.js)\",\"grader_public_key\":\"$GRADER_KEY\"}")
echo "   grader key frozen into the job: $(jq -r '.grader_mode + " " + .grader_public_key[0:12]' <<<"$COMMIT")..."
SPHINX_SIGNER_KEY="$(key_of "buyer.$P")" "$SPHINX" suite store --job-id "$JOB" --suite $F/suite.json --reference $F/reference.js \
  --buyer "buyer.$P" | sed -E 's/; tx .*//; s/^/   /'

echo "== 3. the relayer runs the enclave grader on the hidden suite and posts the suite check"
relay_pass
check "the enclave proved the suite passable; the job is Open" '.status == "Open"' job_view

submit() { # AGENT CODE
  near_call "$1.$P" submit "{\"job_id\":\"$JOB\",\"code_uri\":\"repo:$2\",\"code_hash\":\"$(sha256_file "$2")\"}" \
    "$(deposit "$BOND+$SUB_DEPOSIT")" >/dev/null
}

echo "== 4. agent-a submits code that memorised the visible examples; agent-b submits an honest solution"
submit agent-a $F/agent-a.js
submit agent-b $F/agent-b.js
echo "   2 submissions pending"

echo "== 5. agent-a tries a second attempt while its first is still pending"
expect_fail SPHINX_E_SUBMIT_PENDING_EXISTS "agent-a.$P" submit \
  "{\"job_id\":\"$JOB\",\"code_uri\":\"x\",\"code_hash\":\"$(sha256_file $F/agent-a-v2.js)\"}" \
  "$(deposit "$BOND+$SUB_DEPOSIT")"

echo "== 6. someone posts a forged 12/12 score for agent-a"
SUB0=$(near_view get_submission "{\"job_id\":\"$JOB\",\"sub_id\":0}")
FORGED=$(jq -c --arg c "$SPHINX_CONTRACT" --arg s "$(printf '0%.0s' {1..128})" --arg sh "$(jq -r .commit.suite_hash <<<"$(job_view)")" \
  '{score: {contract: $c, job_id: .job_id, sub_id: .sub_id, runtime: "js-boa/1", passed: 12, total: 12, suite_hash: $sh, code_hash: .code_hash, nonce: .nonce}, signature_hex: $s}' <<<"$SUB0")
expect_fail SPHINX_E_SIG_INVALID "agent-a.$P" record_score "$FORGED"

LOST_BEFORE=$(stats_of "agent-a.$P" | jq -r '.bonds_lost')
echo "== 7. the relayer grades both submissions in the enclave and posts the signed scores"
relay_pass

echo "== checks"
check "agent-a's memorised code failed the hidden tests (5/12)" \
  '.status == "Failed" and .passed == 5 and .total == 12' sub_view 0
check "agent-b's honest solution passed every hidden test (12/12)" \
  '.status == "Passed" and .passed == 12 and .total == 12' sub_view 1
check "the job is paid to agent-b and nothing is left pending" \
  ".status == \"Paid\" and .winner == \"agent-b.$P\" and .pending_count == 0 and .held_bonds == \"0\"" job_view
bond_moved() {
  local after; after=$(stats_of "agent-a.$P" | jq -r '.bonds_lost')
  python3 -c "import sys; sys.exit(0 if int('$after') - int('$LOST_BEFORE') == $BOND else 1)"
}
for _ in 1 2 3 4 5 6; do bond_moved && break; sleep 3; done
bond_moved || { echo "FAIL: agent-a's bond was not counted as lost" >&2; exit 1; }
echo "   ok: agent-a's held bond was burned once the job was paid"
check "the contract holds at least what it owes" '.solvent' near_view get_stats
echo "== 8. the buyer deletes the hidden suite now the job has ended"
SPHINX_SIGNER_KEY="$(key_of "buyer.$P")" "$SPHINX" suite delete --job-id "$JOB" --buyer "buyer.$P" | sed -E 's/; tx .*//; s/^/   /'
echo "PASS"
