#!/usr/bin/env bash
# Creates the Sphinx mainnet sub-accounts under your funded mainnet account,
# each with a new key saved to your macOS keychain, and funds them from it.
# This moves real NEAR: run it yourself, after reading the amounts below.
#   PARENT=<you>.near scripts/mainnet-accounts.sh [--dry-run]
# Accounts that already exist are left alone. Nothing here can spend more
# than the sum of the amounts listed (about 6.8 NEAR) plus gas.
set -euo pipefail
: "${PARENT:?set PARENT to your funded mainnet account, e.g. PARENT=alice.near}"
case "$PARENT" in *.near) ;; *) echo "PARENT must be a mainnet .near account" >&2; exit 1 ;; esac
NEAR="${NEAR:-$(command -v near || echo "$HOME/.cargo/bin/near")}"
DRY=0; [ "${1:-}" = --dry-run ] && DRY=1

# name           NEAR   what it is for
ACCOUNTS=(
  "escrow        3.80   the contract: 361 KB of code is 3.61 NEAR of storage, plus state"
  "relayer       0.45   grader caller: about 0.007 NEAR per enclave run (about 45 runs), plus gas"
  "gas           0.20   pays the gas of relayed (gas-free) agent calls"
  "verifier      0.01   receives the 5% fee"
  "buyer         1.70   posts 13 jobs: 0.1 NEAR reward and 0.02 storage each, plus gas"
  "amaka-claude  0.10   agent: 0.02 profile deposit and three 0.0225 submissions"
  "jonas-web     0.10   agent"
  "priya-grok    0.10   agent"
  "marco-opus    0.10   agent"
  "lena-sonnet   0.10   agent"
  "theo-build    0.10   agent"
)

total=0
for row in "${ACCOUNTS[@]}"; do
  read -r name amount _ <<<"$row"
  total=$(python3 -c "print(round($total + $amount, 2))")
done
echo "parent $PARENT funds ${#ACCOUNTS[@]} accounts with $total NEAR in total:"
for row in "${ACCOUNTS[@]}"; do
  read -r name amount why <<<"$row"
  printf '  %-30s %5s NEAR  %s\n' "$name.$PARENT" "$amount" "$why"
done
[ "$DRY" = 1 ] && exit 0
read -r -p "create and fund them now? type yes: " ok
[ "$ok" = yes ] || { echo "nothing was done"; exit 1; }

for row in "${ACCOUNTS[@]}"; do
  read -r name amount _ <<<"$row"
  acct="$name.$PARENT"
  if "$NEAR" account view-account-summary "$acct" network-config mainnet now >/dev/null 2>&1; then
    echo "exists: $acct"; continue
  fi
  "$NEAR" account create-account fund-myself "$acct" "$amount NEAR" \
    autogenerate-new-keypair --signature-scheme ed25519 save-to-keychain \
    sign-as "$PARENT" network-config mainnet sign-with-keychain send >/dev/null
  echo "created: $acct ($amount NEAR)"
done
echo "done; remaining balance of $PARENT:"
"$NEAR" account view-account-summary "$PARENT" network-config mainnet now 2>&1 | grep -i "native account balance"
