#!/bin/sh
# Stateful program fuzz (tests/fuzz_stateful.rs) inside the capped VPS container, both builds.
# The program binary is compiled into the test (include_bytes), so each build compiles once.
# Expects logs/normal.so and logs/demo.so (built on the Mac: `anchor build`, then
# `anchor build -- --features demo`). Mode: `check` (compile only) or `run` (FUZZ_SECS each).
set -eu
cd /work
mkdir -p target/deploy
mode=${1:-check}
for build in normal demo; do
    features=""
    [ "$build" = demo ] && features="--features demo"
    cp "logs/$build.so" target/deploy/safu_pool.so
    if [ "$mode" = check ]; then
        cargo test --release -p safu_pool $features --test fuzz_stateful --no-run
    else
        FUZZ_SECS=${FUZZ_SECS:-900} cargo test --release -p safu_pool $features --test fuzz_stateful \
            -- --ignored --nocapture > "logs/fuzz_$build.log" 2>&1 || echo "FUZZ FAILED ($build)" >> "logs/fuzz_$build.log"
    fi
done
