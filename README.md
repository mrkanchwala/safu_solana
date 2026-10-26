# SAFU Staking on Solana

One USDC pool on Solana that pays you back when a wallet you registered is drained. Covered wallets
can sit on Solana, Ethereum or Stellar. Payouts follow rules set in advance, and nobody votes on a
claim.

This branch, `solana-usdc-pool`, is the USDC pool going to audit: the same rules, roles and settings
as the Stellar pool in [safu_multichain](https://github.com/mrkanchwala/safu_multichain), rebuilt in
Anchor. `main` still holds the earlier SOL demo pool (Marinade staking). The USDC pool is not
deployed yet; a devnet run comes before the audit.

## How it works

1. **Stake USDC** from a Solana wallet, or from an Ethereum or Stellar wallet over Circle CCTP v2. A
   stake from another chain lives in an identity account that only your own key at home controls,
   and its payouts and withdrawals go back home. Anything above the maximum stake goes into backing:
   it earns yield and is never forfeited on a claim.
2. **Register up to three covered wallets** on any mix of Solana, Ethereum and Stellar. You prove
   each one is yours; a covered wallet is bound to your stake for good.
3. **File a claim** within 30 days of a drain, citing up to 20 drain transactions. The scanner checks
   each one on its own chain, the oracle signs the result with a key held in AWS KMS, and the program
   checks that signature against its own limits before any money moves.
4. **Collect the payout.** A claim filed before your stake is 90 days old is recorded at full value
   and held until day 90. You approve it in one click, a 7-day cooldown runs, and the payout streams
   to you over 45 days. If the pool is short of room on a given day, claims wait in a line and are
   released strictly in the order they joined. A paid claim forfeits the stake, and that address can
   never stake again.

**How much.** Each covered wallet has a tier (A, B or C) based on its on-chain history. The program
caps a payout at 15x your stake for Tier A, 10x for Tier B and 5x for Tier C (adjustable within
bounds; a cut applies only to new stakes), and the amount paid is the real loss up to that ceiling.

**Yield.** Idle USDC is lent on Kamino Lend, up to 80% of the pool. By default the yield on your own
stake goes to you. Backers add USDC that helps the pool pay large claims on time; backing counts from
day one and leaves through a daily drip.

**Points.** One point per dollar per day, for stakers and backers at separately set rates. Points are
burned on a paid claim.

**Changing the rules.** Each adjustable number has hard bounds and changes only through a proposal,
a second key's approval and 7 days in public. Roles, the oracle and the yield venue change only with
two of three roles and 7 days. A code upgrade goes through a Squads multisig of the same three roles,
2 of 3, with a 7-day time lock. Details: [docs/AUDITOR_DISCLOSURES.md](docs/AUDITOR_DISCLOSURES.md).

## Repository layout

```
programs/safu_pool/     The Anchor program: stake, back, claims and the claim line, payout streams,
                        settings, governance, covered wallets, the Kamino Lend leg, CCTP v2
crates/pool-core/       Every pool rule as a pure function, with unit and property tests
app/                    The site (React, Vite, @solana/kit)
config/                 Deploy values per cluster
scripts/                Fuzz runner, mutation-test runner, Squads multisig, local chain, hardcode check
docs/                   Audit scope, auditor disclosures, testing, port map from the Stellar pool
```

The claim backend (scanner, tier engine, loss pricing, oracle signing, CCTP relayer) is kept in a
private repository. Scoring rules are not published, so a claim cannot be shaped to pass them.

## Build and test

```bash
cargo fmt --check && ./scripts/check_no_hardcodes.sh
NO_DNA=1 anchor build && cargo test
NO_DNA=1 anchor build -- --features demo && cargo test -p safu_pool -p pool-core --features demo
```

| Suite | Tests | Where it runs |
|---|---|---|
| Program (LiteSVM, with Kamino Lend and Circle CCTP loaded) | 228 per build | Local and CI |
| Rule library (`pool-core`) | 93 per build | Local and CI |
| Stateful fuzz | 50 step kinds | Build server, capped container |
| Mutation testing (cargo-mutants) | 1,273 mutants, every miss caught or proven equivalent | GitHub Actions |
| Site (vitest) | 33 | Local and CI |

What each suite covers and how to run it: [docs/TESTING.md](docs/TESTING.md). What is in and out of
audit scope: [docs/AUDIT_SCOPE.md](docs/AUDIT_SCOPE.md). How each piece maps to the Stellar pool:
[docs/PORT_MAP.md](docs/PORT_MAP.md).

## Status

Not audited yet, and not deployed. Known, deliberate design choices are written up for the auditor
in [docs/AUDITOR_DISCLOSURES.md](docs/AUDITOR_DISCLOSURES.md), and the threat model is in
[docs/STRIDE_THREAT_MODEL.pdf](docs/STRIDE_THREAT_MODEL.pdf). Prior work this code builds on:
[DISCLOSURE.md](DISCLOSURE.md).

## License

Licensed under Apache 2.0, with the full text in [LICENSE](LICENSE).
