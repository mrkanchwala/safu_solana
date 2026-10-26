#!/usr/bin/env bash
# Deploys a built program on a local validator that runs a real cluster's feature set, so a syscall
# or feature the cluster has not switched on fails here instead of on the cluster. LiteSVM, and so
# every test, turns all features on: 26 Oct a build calling `sol_remaining_compute_units` passed
# every test and was refused by devnet's loader.
#
# Usage: scripts/check_cluster_features.sh <program.so> [cluster ...]   (default: devnet mainnet-beta)
set -euo pipefail

so=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
shift
clusters=("$@")
[ ${#clusters[@]} -gt 0 ] || clusters=(devnet mainnet-beta)
rpc_port=18899
work=$(mktemp -d)
pid=
cleanup() {
  local rc=$?
  if [ -n "$pid" ]; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
  rm -rf "$work"
  exit "$rc"
}
trap cleanup EXIT

solana-keygen new --no-bip39-passphrase --silent -o "$work/payer.json" >/dev/null
for cluster in "${clusters[@]}"; do
  rm -rf "$work/ledger"
  solana-test-validator --ledger "$work/ledger" --mint "$(solana-keygen pubkey "$work/payer.json")" \
    --clone-feature-set --url "$cluster" --rpc-port "$rpc_port" --quiet --reset &
  pid=$!
  local_url="http://127.0.0.1:$rpc_port"
  for _ in $(seq 1 60); do
    solana cluster-version --url "$local_url" >/dev/null 2>&1 && break
    sleep 1
  done
  if ! solana cluster-version --url "$local_url" >/dev/null 2>&1; then
    echo "$cluster: local validator did not start (not a program result)"
    exit 2
  fi
  solana-keygen new --no-bip39-passphrase --silent --force -o "$work/program.json" >/dev/null
  if solana program deploy "$so" --program-id "$work/program.json" --keypair "$work/payer.json" \
    --url "$local_url" >"$work/deploy.log" 2>&1; then
    echo "$cluster: loads"
  else
    echo "$cluster: REFUSED"
    cat "$work/deploy.log"
    exit 1
  fi
  kill "$pid" && wait "$pid" 2>/dev/null || true
  pid=
done
