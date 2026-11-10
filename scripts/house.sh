#!/usr/bin/env bash
# Keeps the catalog open: for every job in CATALOG_DIR (default data/catalog,
# outside git) with no Open job on
# the contract, posts it again (scripts/post-catalog.sh). Meant to run on a
# timer next to the API and the relayer, so visitors always find work.
#   scripts/house.sh            (needs make api running and the buyer key)
# Stops when the buyer account would fall below HOUSE_FLOOR_NEAR (default 2),
# so a run of quick wins cannot drain it. It expects the relayer service to
# be running (CATALOG_RELAY=1 to relay itself). Jobs are posted by
# buyer.$SPHINX_PARENT; agents run by the same operator are labelled as the
# house in their profiles.
# shellcheck source=scripts/lib.sh
source "$(dirname "$0")/lib.sh"
load_env
require_tools jq curl python3 "$NEAR"
: "${SPHINX_PARENT:?set SPHINX_PARENT}"
API="${SPHINX_API:-http://127.0.0.1:8787}"
BUYER="buyer.$SPHINX_PARENT"
FLOOR="${HOUSE_FLOOR_NEAR:-2}"
cd "$SPHINX_ROOT" || exit 1

# Open jobs' task hashes (the last path segment of spec_uri).
open_specs() {
  local from=0 page
  while :; do
    page=$(near_view list_jobs "{\"from\":\"$from\",\"limit\":50}")
    jq -r '.[] | select(.status == "Open" or .status == "Committed" or .status == "Draft") | .spec_uri | split("/") | last' <<<"$page"
    [ "$(jq length <<<"$page")" -lt 50 ] && break
    from=$((from + 50))
  done
}
balance() {
  curl -s "${SPHINX_RPC:-$DEFAULT_RPC}" -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"query\",\"params\":{\"request_type\":\"view_account\",\"finality\":\"final\",\"account_id\":\"$BUYER\"}}" \
    | jq -r .result.amount | python3 -c "import sys; print(int(sys.stdin.read()) / 1e24)"
}

OPEN=$(open_specs)
missing=()
for dir in "${CATALOG_DIR:-data/catalog}"/*/; do
  name=$(basename "$dir")
  hash=$(sha256_file "$dir/spec.md")
  grep -qx "$hash" <<<"$OPEN" && continue
  # A suite that was revealed is public: posting it again would pay for
  # answers anyone can look up, so it needs new tests first.
  suite_hash=$(sha256_file "$dir/suite.json")
  if [ "$(curl -s -o /dev/null -w '%{http_code}' "$API/v1/code/$suite_hash")" = 200 ]; then
    echo "not reposting $name: its tests were revealed (write a new suite.json)" >&2
    continue
  fi
  missing+=("$name")
done
if [ ${#missing[@]} -eq 0 ]; then echo "every catalog job is open"; exit 0; fi

need=$(python3 -c "print(${#missing[@]} * (float('${CATALOG_REWARD:-0.2}') + 0.03))")
have=$(balance)
if python3 -c "import sys; sys.exit(0 if $have - $need >= $FLOOR else 1)"; then
  echo "reposting: ${missing[*]}"
  SPHINX_API="$API" CATALOG_RELAY="${CATALOG_RELAY:-0}" scripts/post-catalog.sh "${missing[@]}"
else
  echo "not reposting ${missing[*]}: $BUYER has $have NEAR and would fall below $FLOOR NEAR" >&2
  exit 75
fi
