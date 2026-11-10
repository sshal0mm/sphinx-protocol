#!/usr/bin/env bash
# Posts the jobs in fixtures/jobs/ as the buyer test account, so the network
# always has open work beyond coding puzzles:
#   scripts/post-catalog.sh [job ...]        (default: every job)
# Jobs come from CATALOG_DIR (default data/catalog, outside git). For each job: the task text is stored by hash in the job API (SPHINX_API)
# and named by its public address (SPHINX_API_PUBLIC_BASE), the job is posted,
# its suite hash committed, the hidden suite stored encrypted for the
# registered grader build, and the relayer proves the suite passable in the
# enclave. Reward, bond and deadline come from CATALOG_REWARD (NEAR, default
# 0.2), CATALOG_BOND (default 0.01) and CATALOG_DAYS (default 18); attempts per
# agent and in total from CATALOG_ATTEMPTS (3) and CATALOG_MAX_SUBS (20). The
# contract needs bond x total submissions to be at most the reward. With
# CATALOG_RELAY=0 it waits for a running relayer service instead of relaying.
# shellcheck source=scripts/lib.sh
source "$(dirname "$0")/lib.sh"
load_env
require_tools jq cargo curl "$NEAR"
: "${SPHINX_PARENT:?set SPHINX_PARENT}"
: "${SPHINX_CONTRACT:?set SPHINX_CONTRACT}"
API="${SPHINX_API:-http://127.0.0.1:8787}"
PUBLIC="${SPHINX_API_PUBLIC_BASE:-$API}"
BUYER="buyer.$SPHINX_PARENT"
DEPLOYMENT="$SPHINX_ROOT/deployments/$NETWORK.json"
RELAYER=$(jq -r .grader.caller "$DEPLOYMENT")
export SPHINX_GRADER_URL SPHINX_GRADER_SHA256
SPHINX_GRADER_URL=$(jq -r .grader.url "$DEPLOYMENT")
SPHINX_GRADER_SHA256=$(jq -r .grader.wasm_sha256 "$DEPLOYMENT")
cd "$SPHINX_ROOT" || exit 1
code_reader_token
cargo build -q --release -p sphinx-cli -p sphinx-relayer
SPHINX="$SPHINX_ROOT/target/release/sphinx"

curl -sf "$API/v1" >/dev/null || { echo "the job API at $API is not answering; start it with make api" >&2; exit 1; }
yocto() { python3 -c "from decimal import Decimal as D; print(int(D('$1') * 10**24))"; }
REWARD=$(yocto "${CATALOG_REWARD:-0.2}")
BOND=$(yocto "${CATALOG_BOND:-0.01}")
DEADLINE=$(( ($(date +%s) + ${CATALOG_DAYS:-18} * 86400) * 1000000000 ))
JOB_DEPOSIT=$(near_view get_config | jq -r .job_storage_deposit)
GRADER_KEY=$(near_view get_config | jq -r .grader.public_key)
key_of() {
  "$NEAR" account export-account "$1" using-private-key network-config "$NETWORK" 2>&1 \
    | grep -oE 'ed25519:[1-9A-HJ-NP-Za-km-z]{80,}' | head -1
}

# Live jobs live outside git, in CATALOG_DIR (default data/catalog, which git
# ignores): one folder per job with spec.md, suite.json and reference.js.
# Anyone with the repo could read a tracked suite or submit a tracked
# reference, so any suite or reference whose bytes git has ever stored is
# refused. fixtures/jobs holds only ended jobs, published as examples.
CATALOG="${CATALOG_DIR:-data/catalog}"
JOBS=("$@")
POSTED=()
[ ${#JOBS[@]} -gt 0 ] || while IFS= read -r j; do JOBS+=("$j"); done < <(ls "$CATALOG" 2>/dev/null)
[ ${#JOBS[@]} -gt 0 ] || { echo "no jobs in $CATALOG" >&2; exit 1; }
for name in "${JOBS[@]}"; do
  dir="$CATALOG/$name"
  for f in spec.md suite.json reference.js; do
    [ -f "$dir/$f" ] || { echo "$name: $dir/$f is missing" >&2; exit 1; }
  done
  for f in "$dir/suite.json" "$dir/reference.js"; do
    if git cat-file -e "$(git hash-object "$f")" 2>/dev/null; then
      echo "$name: $f is (or was) in git, so it is public; write new tests and a new reference" >&2
      exit 1
    fi
  done
  "$SPHINX" suite check "$dir/suite.json" --reference "$dir/reference.js" >/dev/null ||
    { echo "$name: the reference does not pass its suite" >&2; exit 1; }
  hash=$(curl -sf -X POST "$API/v1/code" --data-binary @"$dir/spec.md" | jq -r .code_hash)
  [ -n "$hash" ] && [ "$hash" != null ] || { echo "$name: storing the task text failed" >&2; exit 1; }
  title=$(job_title "$dir/spec.md")
  job=$(near_call "$BUYER" post_job "{\"title\":$title,\"spec_uri\":\"$PUBLIC/v1/code/$hash\",\"runtime\":\"js-boa/1\",\"reward\":\"$REWARD\",\"bond\":\"$BOND\",\"max_subs_per_agent\":${CATALOG_ATTEMPTS:-3},\"max_subs_total\":${CATALOG_MAX_SUBS:-20},\"deadline_ns\":\"$DEADLINE\"}" \
    "$(python3 -c "print($REWARD + $JOB_DEPOSIT)") yoctoNEAR" | tr -d '"')
  near_call "$BUYER" commit_suite "{\"job_id\":\"$job\",\"suite_hash\":\"$("$SPHINX" suite hash "$dir/suite.json")\",\"ref_solution_hash\":\"$(sha256_file "$dir/reference.js")\",\"grader_public_key\":\"$GRADER_KEY\"}" >/dev/null
  SPHINX_SIGNER_KEY="$(key_of "$BUYER")" "$SPHINX" suite store --job-id "$job" --suite "$dir/suite.json" \
    --reference "$dir/reference.js" --buyer "$BUYER" >/dev/null
  POSTED+=("$job")
  echo "job $job: $name (task $hash)"
done

if [ "${CATALOG_RELAY:-1}" = 0 ]; then
  # A relayer service is running and signs as the same account: let it open
  # the jobs, so the two never race for nonces.
  echo "== waiting for the relayer service to prove each suite passable"
  for job in "${POSTED[@]}"; do
    for _ in $(seq 1 60); do
      [ "$(near_view get_job "{\"job_id\":\"$job\"}" | jq -r .status)" = Open ] && { echo "   job $job is open"; break; }
      sleep 5
    done
  done
else
  echo "== the relayer proves each suite passable in the enclave"
  SPHINX_RELAYER_ACCOUNT="$RELAYER" SPHINX_RELAYER_KEY="$(key_of "$RELAYER")" SPHINX_API="$API" \
    target/release/sphinx-relayer --once 2>&1 | grep -E '^(posted|failed|waiting)' | sed -E 's/  tx .*//; s/^/   /'
fi
