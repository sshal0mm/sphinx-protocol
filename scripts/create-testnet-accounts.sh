#!/usr/bin/env bash
# Creates the Sphinx demo sub-accounts under your testnet account.
# Run it yourself: keys are generated and stored in your macOS keychain.
# Usage: scripts/create-testnet-accounts.sh            (default parent: sphinx999.testnet)
set -euo pipefail
PARENT="${1:-sphinx999.testnet}"
NEAR="${NEAR:-$HOME/.cargo/bin/near}"

create() {  # name amount
  local acct="$1.$PARENT"
  if "$NEAR" account view-account-summary "$acct" network-config testnet now >/dev/null 2>&1; then
    echo "exists: $acct"; return
  fi
  "$NEAR" account create-account fund-myself "$acct" "$2" \
    autogenerate-new-keypair --signature-scheme ed25519 save-to-keychain \
    sign-as "$PARENT" network-config testnet sign-with-keychain send >/dev/null
  echo "created: $acct ($2)"
}

create sphinx    '3 NEAR'    # the escrow contract
create relayer   '0.5 NEAR'  # relays signed grader results (function-call key later)
create verifier  '0.1 NEAR'  # receives the verifier fee
create buyer     '1.5 NEAR'  # posts the demo job
create agent-a   '0.5 NEAR'  # the overfitting agent
create agent-b   '0.5 NEAR'  # the honest agent
echo "done. Remaining parent balance:"
"$NEAR" account view-account-summary "$PARENT" network-config testnet now 2>&1 | grep -i "native account balance"
