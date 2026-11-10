#!/usr/bin/env bash
# Runs the release grader wasm under the instruction allowance the relayer
# asks OutLayer for (2e9, `RunLimits` in crates/sphinx-near/src/outlayer.rs)
# and checks each case's declared outcome:
#   "N/M"      the result must be exactly N passed of M
#   "result"  any result (the run must not trap)
#   "trap"     the run exhausts the allowance (documented limit; if this
#              starts producing signed results, update docs/LIMITS.md)
set -euo pipefail
cd "$(dirname "$0")/.."

FUEL="${FUEL:-2000000000}"
WASMTIME="${WASMTIME:-$(command -v wasmtime || echo "$HOME/.cargo/bin/wasmtime")}"
WASM=target/wasm32-wasip2/release/sphinx-grader.wasm
CASES=fixtures/wasm-corpus/cases.json

for tool in "$WASMTIME" jq python3; do
  command -v "$tool" >/dev/null 2>&1 || { echo "missing tool: $tool" >&2; exit 69; }
done
[ -f "$WASM" ] || cargo build -q -p sphinx-grader --release --target wasm32-wasip2

request() { # code tests
  python3 - "$1" "$2" <<'PY'
import hashlib, json, sys
code, n = sys.argv[1], int(sys.argv[2])
suite = json.dumps({"version": 1, "runtime": "js-boa/1", "entry": "solve",
                    "tests": [{"args": [i], "expected": i} for i in range(n)]})
h = lambda s: hashlib.sha256(s.encode()).hexdigest()
print(json.dumps({"op": "grade", "schema": "sphinx.grader/1", "contract": "sphinx.example.testnet",
                  "job_id": "1", "sub_id": 0, "nonce": "0" * 64, "suite": suite,
                  "expected_suite_hash": h(suite), "submission": code, "expected_code_hash": h(code)}))
PY
}

failures=0
count=$(jq length "$CASES")
for i in $(seq 0 $((count - 1))); do
  name=$(jq -r ".[$i].name" "$CASES")
  code=$(jq -r ".[$i].code" "$CASES")
  tests=$(jq -r ".[$i].tests" "$CASES")
  expect=$(jq -r ".[$i].expect" "$CASES")
  if out=$(request "$code" "$tests" | "$WASMTIME" run -W fuel="$FUEL" "$WASM" 2>/dev/null); then
    got=$(jq -r '"\(.result.passed)/\(.result.total)"' <<<"$out")
  else
    got="trap"
  fi
  if [ "$expect" = "$got" ] || { [ "$expect" = result ] && [ "$got" != trap ]; }; then
    printf 'ok    %-24s %s\n' "$name" "$got"
  else
    printf 'FAIL  %-24s expected %s, got %s\n' "$name" "$expect" "$got"
    failures=$((failures + 1))
  fi
done
exit "$failures"
