#!/usr/bin/env bash
# Releases a new enclave grader build on the network in .env:
#   1. builds it reproducibly (scripts/build-grader.sh);
#   2. uploads it to FastFS and waits until it is served;
#   3. runs its `identity` op in OutLayer's TDX enclave (again if the first
#      run of a new build only compiled it);
#   4. checks the attestation: the output hash, the build and the caller;
#   5. registers the signing and box keys with `set_grader_key` from the
#      owner account, 1 yoctoNEAR attached;
#   6. records the release in deployments/<network>.json (the previous one
#      moves to previous_graders) and tags the source grader-<8 hex>.
# Usage: scripts/release-grader.sh   (OWNER defaults to the contract's owner)
# shellcheck source=scripts/lib.sh
source "$(dirname "$0")/lib.sh"
load_env
require_tools jq curl cargo "$NEAR"
: "${SPHINX_CONTRACT:?set SPHINX_CONTRACT}"
DEPLOYMENT="$SPHINX_ROOT/deployments/$NETWORK.json"
RPC_URL="${SPHINX_RPC:-$DEFAULT_RPC}"
# The grader's caller: recorded by the last release, else (a network's first
# release) SPHINX_RELAYER_ACCOUNT or relayer.$SPHINX_PARENT.
CALLER=$(jq -r '.grader.caller // empty' "$DEPLOYMENT" 2>/dev/null || true)
CALLER="${CALLER:-${SPHINX_RELAYER_ACCOUNT:-relayer.${SPHINX_PARENT:?set SPHINX_PARENT}}}"
OWNER="${OWNER:-$(near_view get_config | jq -r .owner)}"
cd "$SPHINX_ROOT" || exit 1

[ -z "$(git status --porcelain -- crates Cargo.lock Cargo.toml rust-toolchain.toml)" ] ||
  { echo "commit the grader's source first: the release is tagged at HEAD" >&2; exit 1; }

echo "== build"
OUT=$(scripts/build-grader.sh)
WASM=$(sed -n 1p <<<"$OUT"); SHA=$(sed -n 2p <<<"$OUT")
echo "   $SHA"
[ "$SHA" != "$(jq -r .grader.wasm_sha256 "$DEPLOYMENT")" ] || { echo "   already the registered build; nothing to do"; exit 0; }

echo "== upload"
URL=""
for _ in 1 2; do URL=$(scripts/fastfs-upload.sh "$WASM" "$CALLER" "$OUTLAYER_CONTRACT" "$NETWORK" 2>/dev/null) && break; URL=""; done
[ -n "$URL" ] || { echo "FastFS did not serve the upload" >&2; exit 1; }
echo "   $URL"

echo "== identity run in the enclave"
WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT
for attempt in 1 2 3; do
  if SPHINX_GRADER_URL=$URL SPHINX_GRADER_SHA256=$SHA SPHINX_RELAYER=$CALLER \
      scripts/outlayer-grade.sh >"$WORK/identity.json" 2>"$WORK/identity.err"; then break; fi
  [ "$attempt" = 3 ] && { cat "$WORK/identity.err" >&2; exit 1; }
  echo "   run $attempt gave no identity (a new build compiles first); running again"
done
TX=$(grep -oE '^tx [A-Za-z0-9]+' "$WORK/identity.err" | tail -1 | cut -d' ' -f2)
PK=$(jq -r .signing_public_key "$WORK/identity.json")
BOX=$(jq -r .box_public_key "$WORK/identity.json")
echo "   tx $TX, signing key ${PK:0:12}…, box key ${BOX:0:12}…"

echo "== attestation"
ATT_URL="$OUTLAYER_API/attestations/by-tx/$TX"
ATT=""
for _ in $(seq 1 10); do ATT=$(curl -s "$ATT_URL"); jq -e .output_hash <<<"$ATT" >/dev/null 2>&1 && break; sleep 5; done
OUTPUT=$(curl -s "$RPC_URL" -H 'content-type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"EXPERIMENTAL_tx_status\",\"params\":{\"tx_hash\":\"$TX\",\"sender_account_id\":\"$CALLER\",\"wait_until\":\"FINAL\"}}" \
  | jq -r .result.status.SuccessValue | base64 -d | shasum -a 256 | cut -d' ' -f1)
[ "$OUTPUT" = "$(jq -r .output_hash <<<"$ATT")" ] || { echo "the attested output hash is not this run's output" >&2; exit 1; }
[ "$SHA" = "$(jq -r .wasm_hash <<<"$ATT")" ] || { echo "the attested build is not $SHA" >&2; exit 1; }
[ "$(jq -r '.executed_wasm_sha256 // .wasm_hash' <<<"$ATT")" = "$SHA" ] || { echo "the attestation reports two different builds" >&2; exit 1; }
[ "$CALLER" = "$(jq -r .caller_account_id <<<"$ATT")" ] || { echo "the attested caller is not $CALLER" >&2; exit 1; }
echo "   output, build and caller match; the quote is checked offline by sphinx verify"

echo "== register as $OWNER"
REG=$("$NEAR" contract call-function as-transaction "$SPHINX_CONTRACT" set_grader_key json-args \
  "{\"public_key_hex\":\"$PK\",\"box_public_key_hex\":\"$BOX\",\"mode\":\"outlayer_tdx\",\"wasm_sha256\":\"$SHA\",\"attestation_ref\":\"$ATT_URL\"}" \
  prepaid-gas '30 Tgas' attached-deposit '1 yoctoNEAR' sign-as "$OWNER" network-config "$NETWORK" \
  sign-with-keychain send 2>&1 | grep -oE 'Transaction ID: [A-Za-z0-9]+' | cut -d' ' -f3)
[ -n "$REG" ] || { echo "set_grader_key failed" >&2; exit 1; }
echo "   tx $REG"

TAG="grader-${SHA:0:8}"
jq --arg url "$URL" --arg sha "$SHA" --arg pk "$PK" --arg box "$BOX" --arg att "$ATT_URL" \
  --arg itx "$TX" --arg reg "$REG" --arg tag "$TAG" --arg caller "$CALLER" '
  .previous_graders = (if .grader.wasm_sha256 then [.grader] else [] end) + (.previous_graders // [])
  | .grader += {caller: $caller, url: $url, wasm_sha256: $sha, public_key: $pk, box_public_key: $box, attestation: $att,
                identity_tx: $itx, registration_tx: $reg, source_tag: $tag}' "$DEPLOYMENT" > "$WORK/d.json"
mv "$WORK/d.json" "$DEPLOYMENT"
# The same build released on another network already has its tag.
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null || git tag "$TAG"
echo "released $TAG; commit deployments/$NETWORK.json and push the tag"
