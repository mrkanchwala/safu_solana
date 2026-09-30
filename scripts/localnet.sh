#!/usr/bin/env bash
# Local validator for the backend e2e: this program (demo build, upgrade authority = a throwaway
# admin key) + Marinade and its accounts from the test fixtures (devnet dump). Nothing reaches devnet.
#   ./scripts/localnet.sh            start (foreground), fresh ledger every time
# Build first: NO_DNA=1 anchor build -- --features demo
set -euo pipefail
cd "$(dirname "$0")/.."

STATE=.localnet
FIX=programs/safu_pool/tests/fixtures
mkdir -p "$STATE"
[ -f "$STATE/admin.json" ] || solana-keygen new --no-bip39-passphrase --silent -o "$STATE/admin.json"
ADMIN=$(solana-keygen pubkey "$STATE/admin.json")
PROGRAM=$(solana-keygen pubkey target/deploy/safu_pool-keypair.json)
MARINADE=$(python3 -c "import json;print(json.load(open('config/pool.localnet.json'))['marinade']['program'])")

ACCOUNTS=()
for f in "$FIX"/marinade_*.json; do
  ACCOUNTS+=(--account "$(python3 -c "import json,sys;print(json.load(open(sys.argv[1]))['pubkey'])" "$f")" "$f")
done

exec solana-test-validator --reset --quiet --ledger "$STATE/ledger" \
  --upgradeable-program "$PROGRAM" target/deploy/safu_pool.so "$ADMIN" \
  --bpf-program "$MARINADE" "$FIX/marinade.so" \
  "${ACCOUNTS[@]}"
