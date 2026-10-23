#!/bin/sh
# cargo-mutants runs cargo as `$CARGO`. The LiteSVM tests load the built program (include_bytes), not
# its source, so before each test run this rebuilds the SBF program from the mutated tree; without it
# every program mutant would be tested against the unmutated binary and reported missed.
# `REAL_CARGO` is the cargo to hand the original command to. Start cargo-mutants as `cargo-mutants
# mutants`, never `cargo mutants`: cargo sets CARGO to itself for a subcommand, bypassing this script.
# A `--features` given to cargo-mutants (the demo check) is passed on to the program build, so the
# tests and the binary they load are the same build.
set -eu
features=""
prev=""
for a in "$@"; do
    case "$a" in
    --features=*) features="${a#--features=}" ;;
    esac
    [ "$prev" = "--features" ] && features="$a"
    prev="$a"
done
case "${1:-}" in
test | check | build)
    [ -n "${MUTANTS_CARGO_LOG:-}" ] && echo "$(pwd): $*" >> "$MUTANTS_CARGO_LOG"
    if [ -n "$features" ]; then
        env -u CARGO cargo build-sbf --manifest-path programs/safu_pool/Cargo.toml -- --locked -q \
            --features "$features" > /dev/null
    else
        env -u CARGO cargo build-sbf --manifest-path programs/safu_pool/Cargo.toml -- --locked -q > /dev/null
    fi
    ;;
esac
exec "$REAL_CARGO" "$@"
