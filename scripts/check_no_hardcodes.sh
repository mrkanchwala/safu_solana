#!/usr/bin/env bash
# Fails if an address or a rule number is typed outside its one source.
#   Rule numbers  -> crates/pool-core/src/params.rs, setting ranges -> settings.rs (clients read
#                    them from the IDL or the pool's settings)
#   Marinade facts-> crates/pool-core/src/marinade.rs
#   Deploy values -> config/*.json
#   Program id    -> declare_id! (clients read the IDL `address`)
# A line can opt out with the marker `hardcode-ok: <reason>`.
set -euo pipefail
cd "$(dirname "$0")/.."

SOURCES=(programs/safu_pool/src crates/pool-core/src)
[ -d app/src ] && SOURCES+=(app/src)
ALLOWED='crates/pool-core/src/(params|marinade|settings)\.rs'
fail=0

# 1. Base58 addresses (32-44 chars).
hits=$(grep -rnE '["'"'"'][1-9A-HJ-NP-Za-km-z]{32,44}["'"'"']' "${SOURCES[@]}" \
  --include='*.rs' --include='*.ts' --include='*.tsx' \
  | grep -vE "$ALLOWED" | grep -v 'declare_id!' | grep -v 'hardcode-ok:' || true)
if [ -n "$hits" ]; then echo "Address typed outside config/IDL:"; echo "$hits"; fail=1; fi

# 2. Numeric literals of 3+ digits in program/core logic (rule numbers belong in params.rs).
hits=$(grep -rnE '(^|[^A-Za-z0-9_."])[0-9][0-9_]{2,}([^0-9_]|$)' programs/safu_pool/src crates/pool-core/src \
  --include='*.rs' | grep -vE "$ALLOWED" | grep -vE '^\S+:\s*//' | grep -v 'hardcode-ok:' \
  | grep -vE '#\[cfg\(test\)\]' || true)
# Unit tests inside src/ may use literal fixtures.
hits=$(echo "$hits" | grep -v '/tests\.rs:' || true)
if [ -n "$hits" ]; then echo "Number typed outside params.rs:"; echo "$hits"; fail=1; fi

[ $fail -eq 0 ] && echo "check_no_hardcodes: clean"
exit $fail
