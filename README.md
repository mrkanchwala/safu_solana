# SAFU Staking on Solana

Automatic payouts when a wallet gets drained. SAFU is a shared protection pool on Solana: you stake
SOL and pick up to two wallets to cover, on Solana or Ethereum. If one is drained through a malicious
link or token approval, a stolen key, a sandwich attack or a wrongful liquidation, you file a claim and
our scanner checks the drain transactions. Once it confirms the loss, the pool pays you back, up to 15,
10 or 5 times your stake depending on the covered wallet's history, and nobody votes on it.

Status: live on Solana devnet at https://solana.safustaking.com, with Ethereum cover read on Sepolia.
This is a test-network build with no real users.

## How it works

- **Stakers** stake SOL and register up to two covered wallets, proving they own each one.
- **Backers** add SOL without cover to make the pool bigger, and share the staking yield with stakers.
- The pool stakes its SOL through Marinade (mSOL) while it waits for claims.
- A claim names up to five drain transactions within 30 days of the drain. The scanner checks each one on
  its own chain, the oracle signs the verdict with a key held in a cloud key vault, and the program
  checks that Ed25519 signature and the tier ceiling before it pays.
- A claim can be approved once the stake is 90 days old. The payout then streams over 45 days after a
  7-day cooldown, and a daily payout cap tightens as more of the pool is in use, so a wave of claims
  is paid in order.
- Program upgrades need two keys to sign together (Squads).

## Tech

Anchor and Rust for the program, with every pool rule written as a pure function in `crates/pool-core`
and covered by unit and property tests. LiteSVM for program tests, plus fuzzing. Marinade for staking,
Squads for the upgrade authority, Ed25519 checks on oracle verdicts. The site uses React, Vite and
@solana/kit. The loss scanner runs in Python on a private backend and reads chain data through Helius
and Alchemy.

## Roadmap

- Let stakers borrow against what they staked while their cover stays on.
- Earn more on the money waiting in the pool.
- Accept USDC from any chain through Circle's CCTP.
- Partner pools: wallets, apps and protocols run their own SAFU pool so their users are covered.

## Repo layout

- `crates/pool-core`: every pool rule as a pure function, with unit and property tests.
- `programs/safu_pool`: the Anchor program.
- `app/`: the site.
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

Local chain for the claim backend: `./scripts/localnet.sh` (demo build, Marinade loaded from the
test fixtures, fresh ledger each start). `config/pool.localnet.json` points clients at it.

Licensed under Apache-2.0.
