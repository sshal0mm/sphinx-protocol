#!/usr/bin/env bash
# Registers the local DEV grader key on a development contract. Refuses unless
# SPHINX_MODE=dev, so a production deployment can never get a dev key by
# accident. Jobs committed afterwards are marked mode "dev" on-chain.
# shellcheck source=scripts/lib.sh
source "$(dirname "$0")/lib.sh"
load_env
require_tools jq cargo "$NEAR"
[ "${SPHINX_MODE:-}" = dev ] || { echo "refusing: set SPHINX_MODE=dev to register a dev key" >&2; exit 77; }
: "${SPHINX_CONTRACT:?set SPHINX_CONTRACT}"
: "${SPHINX_DEV_GRADER_SEED:?set SPHINX_DEV_GRADER_SEED (see .env.example)}"

cd "$SPHINX_ROOT" || exit 1
cargo build -q --release -p sphinx-cli --features dev-signer
cargo build -q --release -p sphinx-grader --target wasm32-wasip2
PK=$(target/release/sphinx dev pubkey)
BOX_PK=$(target/release/sphinx dev box-pubkey)
WASM_HASH=$(sha256_file target/wasm32-wasip2/release/sphinx-grader.wasm)
near_call "$SPHINX_CONTRACT" set_grader_key \
  "{\"public_key_hex\":\"$PK\",\"box_public_key_hex\":\"$BOX_PK\",\"mode\":\"dev\",\"wasm_sha256\":\"$WASM_HASH\",\"attestation_ref\":\"dev-local\"}" >/dev/null
echo "registered DEV grader key $PK (box key $BOX_PK) on $SPHINX_CONTRACT"
