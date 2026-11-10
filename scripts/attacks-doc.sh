#!/usr/bin/env bash
# Writes docs/ATTACKS.md (to stdout) from a record made by scripts/attacks.sh.
#   scripts/attacks-doc.sh deployments/attacks/testnet.json > docs/ATTACKS.md
set -euo pipefail
IN="${1:?usage: scripts/attacks-doc.sh <record.json>}"
EXPLORER="https://testnet.nearblocks.io/txns"
jq -r --arg ex "$EXPLORER" '
  def link: if .tx_hash then "[`\(.tx_hash[0:10])…`](\($ex)/\(.tx_hash))" else "–" end;
  "# Attack gallery",
  "",
  "Every row below was attempted for real against the deployed escrow (`\(.contract)`) and its job API on \(.network), on job \(.job_id), graded by enclave build `\(.grader[0:8])`. On-chain attempts are real transactions that the contract refused; their hashes link to the explorer. Recorded \(.recorded_at) by `make attacks`.",
  "",
  "Re-check it without keys: `make attacks-check` reads each transaction back from the network and confirms it failed with the code shown.",
  "",
  "| Attack | Who tries it | What they try | Result | Transaction |",
  "|---|---|---|---|---|",
  (.attacks[] | "| \(.name) | `\(.attacker)` | \(.attempt) | `\(.got)` | \(link) |"),
  "",
  "## Reading the results",
  "",
  "- `SPHINX_E_*` codes are the contract refusing the call; nothing moved. The full list is in [ERRORS.md](ERRORS.md).",
  "- `HTTP 404` is the job API refusing to serve code. It answers the same whether the code exists or not, so a refusal does not confirm anything. `HTTP 200` rows are the reads that are allowed: an agent reading its own code, and the buyer reading the winning code of a job it paid for.",
  "- `no plaintext`: the relayer sends submissions to the enclave encrypted to its box key, so the agents'\'' code never appears in public transaction data. The linked transaction is one of those enclave requests."
' "$IN"
