# SAFU Staking on Solana

A SOL protection pool on Solana. Stakers put in SOL and get coverage for wallet drains on Solana;
backers add capacity. The pool stakes through Marinade (mSOL) and passes the yield to stakers and
backers.

Status: in development, devnet only.

- `crates/pool-core`: every pool rule as a pure function, with unit and property tests.
- `programs/safu_pool`: the Anchor program.
- `config/`: deploy values per cluster.
- `docs/PORT_MAP.md`: how this maps to the multichain pool.
- `DISCLOSURE.md`: prior work.

## Build and test

```bash
cargo test -p pool-core && cargo test -p pool-core --features demo
NO_DNA=1 anchor build && cargo test -p safu_pool
NO_DNA=1 anchor build -- --features demo && cargo test -p safu_pool --features demo
./scripts/check_no_hardcodes.sh
```

Apache-2.0.
