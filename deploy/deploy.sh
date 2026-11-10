#!/usr/bin/env bash
# Ships this repository to the server and (re)starts the Sphinx stack there.
#   DEPLOY_HOST=<ssh alias> deploy/deploy.sh [--dry-run]
# The server keeps its own deploy/.env (secrets) and deploy/data (code store,
# house records); neither is ever sent from here or put in git. The stack is
# its own compose project on its own network: it publishes no ports and
# touches no other containers.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${DEPLOY_HOST:?set DEPLOY_HOST to the ssh alias of the server}"
DIR="${DEPLOY_DIR:-/root/sphinx}"
DRY=""
[ "${1:-}" = --dry-run ] && DRY="--dry-run"
rsync -az --delete $DRY \
  --exclude .git --exclude target \
  --exclude 'data/' --exclude '.env' --exclude 'deploy/.env' --exclude 'deploy/data/' \
  ./ "$DEPLOY_HOST:$DIR/"
[ -n "$DRY" ] && exit 0
# On the server: mainnet services and their public MCP route (/mcp/mainnet)
# run only once deploy/.env names a mainnet contract.
REMOTE=$(cat <<'SH'
set -e
cd "$DIR"
test -f deploy/.env
mkdir -p deploy/data/code deploy/data/house deploy/data/mainnet/code deploy/data/mainnet/house deploy/data/mcp-routes
chown -R 10001 deploy/data
F="-f deploy/compose.yml"
[ ! -f deploy/data/agents.compose.yml ] || F="$F -f deploy/data/agents.compose.yml"
P=""
if grep -qE '^SPHINX_MAINNET_CONTRACT=.+' deploy/.env; then
  P="--profile mainnet"
  printf 'location = /mcp/mainnet {\n    set $up http://mcp-public-mainnet:8790;\n    rewrite ^ /mcp break;\n    proxy_pass $up;\n}\n' > deploy/data/mcp-routes/mainnet.conf
else
  rm -f deploy/data/mcp-routes/mainnet.conf
fi
docker compose $F $P --env-file deploy/.env up -d --build --remove-orphans
docker restart sphinx-mcp-1 >/dev/null
docker compose $F $P --env-file deploy/.env ps
SH
)
ssh "$DEPLOY_HOST" "DIR=$DIR bash -s" <<<"$REMOTE"
