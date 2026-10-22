#!/usr/bin/env bash
# Local validator for the backend e2e: this program (demo build, upgrade authority = a throwaway
# admin key) + a 6-decimal test USDC mint whose mint authority is that same admin key, so the e2e
# can mint test USDC. The mint's address is written to config/pool.localnet.json `usdcMint`.
# Nothing reaches devnet.
#   ./scripts/localnet.sh            start (foreground), fresh ledger every time
# Build first: NO_DNA=1 anchor build -- --features demo
set -euo pipefail
cd "$(dirname "$0")/.."

STATE=.localnet
mkdir -p "$STATE"
[ -f "$STATE/admin.json" ] || solana-keygen new --no-bip39-passphrase --silent -o "$STATE/admin.json"
[ -f "$STATE/usdc_mint.json" ] || solana-keygen new --no-bip39-passphrase --silent -o "$STATE/usdc_mint.json"
ADMIN=$(solana-keygen pubkey "$STATE/admin.json")
MINT=$(solana-keygen pubkey "$STATE/usdc_mint.json")
PROGRAM=$(solana-keygen pubkey target/deploy/safu_pool-keypair.json)
# Decimals from the program's one source (the IDL constant), never retyped here.
RENT=$(solana rent 82 --lamports | awk 'NF{print $(NF-1)}')
DECIMALS=$(python3 -c "import json;print(next(c['value'] for c in json.load(open('target/idl/safu_pool.json'))['constants'] if c['name']=='USDC_DECIMALS'))")

# SPL Token Mint account (82 bytes): mint authority = admin, supply 0, initialized, no freeze authority.
python3 - "$ADMIN" "$MINT" "$DECIMALS" "$RENT" "$STATE/usdc_mint_account.json" <<'PY'
import base64, json, sys
B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
def b58(s):
    n = 0
    for ch in s: n = n * 58 + B58.index(ch)
    return n.to_bytes(32, "big")
admin, mint, decimals, rent, out = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4]), sys.argv[5]
d = bytearray(82)
d[0:4] = (1).to_bytes(4, "little")      # COption::Some(mint authority)
d[4:36] = b58(admin)
d[44] = decimals
d[45] = 1                               # initialized
json.dump({"pubkey": mint, "account": {"lamports": rent, "data": [base64.b64encode(bytes(d)).decode(), "base64"],
           "owner": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA", "executable": False, "rentEpoch": 0, "space": 82}},
          open(out, "w"))
PY
python3 - "$MINT" <<'PY'
import json, sys
p = "config/pool.localnet.json"; c = json.load(open(p)); c["usdcMint"] = sys.argv[1]
json.dump(c, open(p, "w"), indent=2); open(p, "a").write("\n")
PY

exec solana-test-validator --reset --quiet --ledger "$STATE/ledger" \
  --upgradeable-program "$PROGRAM" target/deploy/safu_pool.so "$ADMIN" \
  --account "$MINT" "$STATE/usdc_mint_account.json"
