#!/usr/bin/env bash
# The grading story with no network and no keys: suite check, a cheating
# agent, an honest agent, and a decoded on-chain error.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -q --release -p sphinx-cli
S=target/release/sphinx
F=fixtures/fib
N=$(printf 'ab%.0s' {1..32})
show() { jq -r '"   \(.result.passed)/\(.result.total) tests passed  (suite \(.result.suite_hash[0:12])..., code \(.result.code_hash[0:12])...)"'; }

echo "== The buyer checks the hidden suite against a reference solution"
$S suite check $F/suite.json --reference $F/reference.js | sed 's/^/   /'
echo
echo "== agent-a memorised the visible examples"
$S grade --op grade --contract sphinx.local.testnet --job-id 0 --sub-id 0 --nonce "$N" --suite $F/suite.json --code $F/agent-a.js | show
echo "== agent-b solved the problem"
$S grade --op grade --contract sphinx.local.testnet --job-id 0 --sub-id 1 --nonce "$N" --suite $F/suite.json --code $F/agent-b.js | show
echo
echo "== What an agent sees when it exceeds its attempts on-chain"
$S explain 'Smart contract panicked: SPHINX_E_SUBMIT_AGENT_CAP: agent-a.testnet has used 2/2 attempts on job 0 {"v":1,"retry":"never","bond_effect":"none","hint":"","ctx":{"job_id":"0","used":"2","cap":"2"}}' | sed 's/^/   /'
