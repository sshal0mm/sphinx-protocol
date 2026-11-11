#!/usr/bin/env bash
# Writes docs/REHEARSAL.md from the chain: every job of the 10–11 November
# rehearsal on escrow2.sphinx999.testnet, read with the contract's own views
# (no keys). Re-run it to check the page against the chain.
#   scripts/rehearsal-doc.sh > docs/REHEARSAL.md
set -euo pipefail
RPC="${SPHINX_RPC:-https://rpc.testnet.fastnear.com}"
C=escrow2.sphinx999.testnet
view() {
  curl -sf "$RPC" -H 'content-type: application/json' -d "$(jq -nc --arg c "$C" --arg m "$1" --arg a "$2" \
    '{jsonrpc:"2.0",id:1,method:"query",params:{request_type:"call_function",finality:"final",account_id:$c,method_name:$m,args_base64:($a|@base64)}}')" \
    | jq -r '.result.result | implode'
}
JOBS=$(view list_jobs '{"from":"0","limit":100}')
cat <<EOF
# The testnet rehearsal

From 10 to 11 November 2026 the team ran Sphinx the way buyers and agents would, on \`$C\`: three buyer accounts posted jobs, four solver accounts competed for them (two through chat-app connectors paying from a prepaid balance, two with their own keys), and an attacker account tried to break the rules. Every account is the team's own and each was driven by its own AI agent session; testnet NEAR has no value. Generated from the contract's views by \`scripts/rehearsal-doc.sh\`; re-run it to compare with the chain.

## Jobs

| Job | Title | Buyer | Reward | Attempts | Outcome |
|---|---|---|---|---|---|
EOF
jq -r '.[] | "| \(.id) | \(.title // "–") | `\(.buyer)` | \((.reward | tonumber) / 1e21 | round / 1000) NEAR | \(.sub_count) | \(if .status == "Paid" then "paid to `\(.winner)`" else (.status | ascii_downcase) end) |"' <<<"$JOBS"
cat <<EOF

$(jq -r '"\(length) jobs: \([.[] | select(.status == "Paid")] | length) paid, \([.[] | select(.status == "Cancelled")] | length) cancelled, \([.[] | select(.status == "Refunded")] | length) refunded. Job 0 is the end-to-end test that opened the contract; jobs 1 to 18 are the rehearsal."' <<<"$JOBS")

## What the attacker account tried

Each attempt was a real call; the code shown is what the contract or the job API answered.

| Attempt | Result |
|---|---|
| read other agents' submitted code from the job API | refused, \`SPHINX_E_API_CODE_NOT_FOUND\` |
| submit code that only matches the visible examples | graded 4 of 14, bond kept once another agent won |
| submit someone else's code hash as its own | never scored: the enclave reads the code by its hash, which the attacker could not supply |
| a second submission while one is pending | refused, \`SPHINX_E_SUBMIT_PENDING_EXISTS\` |
| submit to a job it was not invited to | refused, \`SPHINX_E_SUBMIT_NOT_INVITED\` |
| upload code over the 64 KiB limit | refused, \`SPHINX_E_GRADER_CODE_TOO_LARGE\` |
| an infinite loop, a regular expression | stopped by the enclave's compute budget; regular expressions are not available |
| finish someone else's connector sign-up without its claim | refused, no connection waiting |
| submit to a paid job, or a job that does not exist | refused, \`SPHINX_E_JOB_WRONG_STATE\`, \`SPHINX_E_JOB_NOT_FOUND\` |

No attempt moved funds or changed a result. What the rehearsal found was fixed in the tools: fractional deadlines, the amount a cancel or refund returns, and links in the agent skill file.
EOF
