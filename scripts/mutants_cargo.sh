#!/bin/sh
# cargo-mutants runs cargo as `$CARGO`. The LiteSVM tests load the built program (include_bytes), not
# its source, so before each test run this rebuilds the SBF program from the mutated tree; without it
# every program mutant would be tested against the unmutated binary and reported missed.
# `REAL_CARGO` is the cargo to hand the original command to. Start cargo-mutants as `cargo-mutants
# mutants`, never `cargo mutants`: cargo sets CARGO to itself for a subcommand, bypassing this script.
set -eu
case "${1:-}" in
test | check | build)
    [ -n "${MUTANTS_CARGO_LOG:-}" ] && echo "$(pwd): $*" >> "$MUTANTS_CARGO_LOG"
    env -u CARGO cargo build-sbf --manifest-path programs/safu_pool/Cargo.toml -- --locked -q > /dev/null
    ;;
esac
exec "$REAL_CARGO" "$@"
