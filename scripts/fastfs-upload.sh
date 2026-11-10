#!/usr/bin/env bash
# Uploads a file to FastFS one chunk at a time, waiting for each transaction to
# finalise before sending the next. (Fire-and-forget chunk uploads can drop a
# chunk, and a file with a missing chunk is never served.) Prints the URL.
# Usage: scripts/fastfs-upload.sh <file> <signer-account> [receiver] [network]
set -euo pipefail
FILE="$1"; SIGNER="$2"; NETWORK="${4:-${NETWORK:-testnet}}"
if [ "$NETWORK" = mainnet ]; then RECEIVER="${3:-outlayer.near}"; HOST=main.fastfs.io; else RECEIVER="${3:-outlayer.testnet}"; HOST=test.fastfs.io; fi
NEAR="${NEAR:-$(command -v near || echo "$HOME/.cargo/bin/near")}"
HASH=$(shasum -a 256 "$FILE" | cut -d' ' -f1)
EXT="${FILE##*.}"
WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT

# Borsh: enum FastfsData::Partial { relative_path, offset, full_size, mime_type, content_chunk, nonce }
python3 - "$FILE" "$HASH.$EXT" "$WORK" <<'PY'
import struct, sys, time
path, rel, out = sys.argv[1], sys.argv[2], sys.argv[3]
data = open(path, "rb").read()
mime = {"wasm": "application/wasm", "json": "application/json"}.get(rel.rsplit(".", 1)[-1], "application/octet-stream")
s = lambda b: struct.pack("<I", len(b)) + b
nonce = int(time.time()) & 0xFFFFFFFF
chunk = 1 << 20
for i, off in enumerate(range(0, len(data), chunk)):
    part = data[off:off + chunk]
    payload = b"\x01" + s(rel.encode()) + struct.pack("<II", off, len(data)) + s(mime.encode()) + s(part) + struct.pack("<I", nonce)
    open(f"{out}/{i:04d}.bin", "wb").write(payload)
PY

total=$(find "$WORK" -name '*.bin' | wc -l | tr -d ' ')
i=0
for payload in "$WORK"/*.bin; do
  i=$((i + 1))
  for attempt in 1 2 3; do
    out=$("$NEAR" contract call-function as-transaction "$RECEIVER" __fastdata_fastfs file-args "$payload" \
      prepaid-gas '1 Tgas' attached-deposit '0 NEAR' sign-as "$SIGNER" network-config "$NETWORK" \
      sign-with-keychain send 2>&1 || true)
    # FastFS records the call's arguments; the call itself is meant to fail
    # with MethodNotFound once included, which is the success signal here.
    if grep -q 'MethodNotFound' <<<"$out"; then echo "chunk $i/$total: recorded" >&2; break; fi
    echo "chunk $i/$total: attempt $attempt failed, retrying" >&2
    [ "$attempt" = 3 ] && { echo "$out" >&2; exit 1; }
  done
done
URL="https://$HOST/$SIGNER/$RECEIVER/$HASH.$EXT"
# A recorded chunk is not yet a served file: wait until FastFS serves the
# exact bytes. Running OutLayer on a URL that is not served yet fails, and
# the failure can stick to that hash.
for _ in $(seq 1 40); do
  if [ "$(curl -sfL "$URL" | shasum -a 256 | cut -d' ' -f1)" = "$HASH" ]; then echo "$URL"; exit 0; fi
  sleep 15
done
echo "uploaded, but FastFS does not serve $URL yet; upload again" >&2
exit 1
