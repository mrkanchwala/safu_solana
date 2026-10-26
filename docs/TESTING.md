# Testing

Every suite for the Solana USDC pool: what it proves, how to run it, and where it runs. Counts are
from 2026-10-23 on branch `solana-usdc-pool`.

| Suite | Tests | Where it runs |
|---|---|---|
| Program (LiteSVM) | 230 per build | Local and CI, normal and demo builds |
| Rule library (`pool-core`) | 93 per build | Local and CI, normal and demo builds |
| Stateful fuzz | 1 harness, 50 step kinds | Build server, in a capped Docker container |
| Mutation testing (cargo-mutants) | 1,273 mutants | GitHub Actions, started by tag |
| Site (vitest) | 33 | Local and CI |
| Backend (pytest, private repository) | 842 | Local |
| Devnet end to end | planned | Solana devnet, Sepolia, Stellar testnet |

Every program and `pool-core` test runs twice: on the normal build and on the `demo` build, whose
waiting periods are minutes instead of days.

## 1. Build and run

```bash
cargo fmt --check
./scripts/check_no_hardcodes.sh                        # one source for addresses and rule numbers
NO_DNA=1 anchor build && cargo test                    # normal build: program + pool-core
NO_DNA=1 anchor build -- --features demo && cargo test -p safu_pool -p pool-core --features demo
cargo clippy --all-targets --locked -- -D warnings
cargo deny check                                       # licences and advisories
```

The program tests load the built `target/deploy/safu_pool.so`, so build before testing. CI
(`.github/workflows/ci.yml`) runs all of the above on both builds, checks the binary is not empty, loads it under the devnet and
mainnet-beta feature sets (`scripts/check_cluster_features.sh`; LiteSVM turns every feature on),
builds the IDL with the Anchor CLI pinned by `Cargo.lock`, and passes the IDL to the site job.

## 2. Program tests on LiteSVM (230)

LiteSVM runs the real program binary. The Kamino Lend tests load klend's mainnet program and real
reserve, market and price accounts. The CCTP tests load Circle's CCTP v2 programs.

| File | Tests | What it checks |
|---|---|---|
| `claims.rs` | 62 | Oracle signature and the Ed25519 instruction (position, offsets, signer, cluster, revocation), tier ceilings, the claim line in order, daily caps that queue instead of failing, time gate, approval, cooldown and vesting stream, the daily payout share, cash-short days, cancel and penalty lock, suspension and its 30-day lapse, the two-key override |
| `stake_back.rs` | 46 | Initialize checks (upgrade authority, distinct roles, USDC mint and decimals, classic SPL Token, a demo build refuses mainnet), pause, stake bounds and overflow into backing, beneficiary rules, withdraw, emergency exit, backing from day one, per-deposit clocks, the daily withdrawal drip, covered-wallet registry, the rounding reserve, donations |
| `settings_and_exits.rs` | 32 | The settings timelock, ranges and order between settings, live settings driving the rules, snapshots on stakes and claims, pause length and gap, paused time added back to every window, partial withdrawal, the free-capital rule for stakers and backers |
| `cctp.rs` | 21 | Stake and back from Ethereum and Stellar over CCTP v2 with Circle's programs: receive, deploy, owner-signed actions (Ethereum `personal_sign`, Stellar SEP-53), nonce, expiry and cluster, no re-split of a signature into other accounts, send home to Ethereum and through Circle's forwarder to Stellar |
| `governance.rs` | 21 | Two of three roles plus 7 days, a role has no say in its own replacement, cancelling takes two, new keys sign their own installation, 90-day recovery and how it is stopped, storage version and `migrate` |
| `kamino.rs` | 29 | Deposit and redeem by CPI into klend, the venue slot refusing a program with no adapter (governance, load, views), a reserve at its deposit limit, harvest takes only growth, stale prices, pulls for payouts and withdrawals, a cash-short day, every venue account checked before any call, Scope- and Pyth-priced reserves, compute and transaction size with full upkeep |
| `upkeep.rs` | 10 | A user transaction without the upkeep accounts is refused, one-click approval past the gate, expiry and stale marking on their own, the watch list walk, the largest transaction fits one packet |
| `points_yield.rs` | 4 | Points per dollar per day, yield paid while paused, yield owed through a full exit |
| `approval_vector.rs` | 1 | The approval message matches the backend's byte for byte |
| `src/` unit tests | 4 | Constants against the program: allowed call names, watch-entry offsets, klend's null key |

`tests/common/` holds shared setup, the klend fixture (`klend.rs`) and the CCTP kit (`cctp.rs`).

## 3. Rule library tests (93)

`crates/pool-core` holds every pool rule as a pure function. Property tests (proptest) check that a
rule holds across random inputs as well as chosen cases.

| File | Tests | What it checks |
|---|---|---|
| `tests/settings.rs` | 22 | Defaults, hard ranges, order between settings, the pause clock, partial withdrawals, money leaves only if open claims still fit |
| `tests/claim_rules.rs` | 15 | Tier ceilings, entitlement and window checks, admission bands, the oracle's daily limit, vesting (monotone, never above the entitlement), admitted claims never exceed capacity |
| `tests/kamino_leg.rs` | 16 | Reading a klend reserve, collateral conversion rounding down, deposit and redeem pre-checks, harvest never redeems principal, how a redeem lands on the books |
| `tests/rules.rs` | 14 | Stake bounds, yield split and conservation, the buffer and push lines |
| `tests/drip.rs` | 5 | The backer withdrawal drip and its daily shares |
| `tests/points.rs` | 5 | Points index, spendable cash, rounding-reserve draws, the upkeep cursor |
| `tests/pinned_values.rs` | 3 | Every clock and every setting range pinned to its rule value, per build |
| `src/` unit tests | 13 | CCTP message parsing and the owner action digest, demo clocks |

## 4. Stateful fuzz

`tests/fuzz_stateful.rs` plays long random episodes against the real program: 250 steps each, drawn
from 50 kinds of step (stakes, backing, claims at every stage, governance votes, settings, pauses,
suspensions, Kamino deposits, redeems and harvests, a reserve that runs out of cash, CCTP receive,
deploy and send home, migrate, time jumps). After every step, accepted or refused, it checks:

- holdings cover the books: only paid claims, and the disclosed Kamino rounding dust in a pool with
  no stakes (`docs/AUDITOR_DISCLOSURES.md` §6), may leave them short;
- stake, backing and open-withdrawal records sum exactly to the pool's totals;
- the claim line and watch list are intact, and open claims match the allocated total;
- the pool's kUSDC account holds exactly what the books say is deployed;
- settings stay in range and in order, and nothing but the allowed exits moves money while paused.

Odd seeds run with the Kamino venue; seeds 2 mod 4 run with Circle's CCTP programs and two home-chain
owners.

```bash
# on a Linux server, inside a capped container (scripts/fuzz_vps.sh runs both builds)
FUZZ_SECS=420 cargo test --release -p safu_pool --test fuzz_stateful -- --ignored --nocapture
FUZZ_SEED=<seed> FUZZ_EPISODES=1 ...    # replay one episode
```

| Run | Build | Steps | Result | Code |
|---|---|---|---|---|
| 2026-10-23 | normal | 295,750 (1,183 episodes, 7 min) | 0 failures | `1dcf178` |
| 2026-10-23 | demo | 297,500 (1,190 episodes, 7 min) | 0 failures | `1dcf178` |
| 2026-10-26 | normal | 640,250 (2,561 episodes, 15 min) | 0 failures | `8816860` |
| 2026-10-26 | demo | 640,250 (2,561 episodes, 15 min) | 0 failures | `8816860` |
| 2026-10-26 | normal | 169,750 (679 episodes, 4 min) | 0 failures | `9f475ac` |
| 2026-10-26 | demo | 171,000 (684 episodes, 4 min) | 0 failures | `9f475ac` |

## 5. Mutation testing

`cargo-mutants` changes one operator or line at a time and checks that some test fails. The program
tests load the built binary, so `scripts/mutants_cargo.sh` rebuilds the program from each mutated tree
before its tests run. `.github/workflows/mutants.yml` runs it on GitHub Actions, started by pushing a
tag:

| Tag | What runs |
|---|---|
| `mutants-smoke-<n>` | One shard of 1/50 of the mutants, to prove the setup gives real results |
| `mutants-full-<n>` | Every mutant in the program and `pool-core`, 20 shards |
| `mutants-check-<n>` | Only the functions listed in `.github/mutants-check.args` |
| `gh workflow run mutants.yml --ref <branch> -f base=<commit>` | Only the mutants on lines changed since `base`, on CI's build cache, no baseline; start once CI is green on that commit |

| Run | Code | Caught | Missed | Notes |
|---|---|---|---|---|
| `mutants-smoke-1` | `691d1e3` | 26 | 0 | Setup check |
| `mutants-full-1` | `691d1e3` | 1,103 | 65 | 1,273 mutants; 66 timeouts (the automatic 20s limit, now fixed at 900s), 39 unviable |
| `mutants-check-1` | `494f077` | 42 | 4 | The 66 timeouts rerun; the 4 misses are equivalents |
| `mutants-check-demo-1` | `494f077` | 23 | 0 | Demo-build values; 2 unviable |
| `mutants-check-2` | `9a54c82` | 12 | 0 | Kamino reserve boundaries |
| `mutants-check-3` | `0b0d02c` | 32 | 2 | First misses after new tests; the 2 misses are equivalents |
| `mutants-check-4` | `bd44c7b` | 26 | 4 | Remaining misses; the 4 misses are equivalents |
| `mutants-check-5` | `6ca83ab` | 19 | 7 | `leg.rs` misses; the 7 misses are equivalents |
| `mutants-check-6` | `e24a496` | 49 | 3 | Venue slot's changed lines; 5 unviable. 2 misses are equivalents, 1 got a test |
| `mutants-check-7` | `da8b1d2` | 8 | 2 | The 3 misses rerun: `deposit_ok` now caught; the 2 misses are equivalents |
| diff run `40309102230` | `c8cc4a1` | 29 | 1 | Compute guard fix, the 30 mutants on lines changed since `43d50ac` (diff mode, 4 min 55 s); the 1 miss is an equivalent; the two old compute equivalents are now caught |

Every missed mutant is now caught by a test or listed in
[MUTATION_EQUIVALENTS.md](MUTATION_EQUIVALENTS.md) (19 entries) with the reason it cannot change
behaviour. A shard that draws only equivalent or unviable mutants shows red: the workflow treats a
shard with nothing caught as a setup fault, so those shards were checked by hand.

## 6. Site (33)

```bash
cd app
npm ci
npm run test     # vitest
npm run build    # type check and production build
npm run lint     # oxlint
```

`friendly-error.test.ts` checks that every program and wallet error turns into a plain-English
message and that every error name it maps exists in the IDL. `shared-cases.test.ts` runs the site's
own maths against the same test vectors as the program (stake bounds, prices, transaction size).

## 7. Backend (842, private repository)

The claim API, CCTP relayer and role signers live in a private repository with the scanner, so their
tests are listed here without the code:

- Claims in USDC for covered wallets on Solana, Ethereum and Stellar, with the upkeep accounts and one resend
- The CCTP relayer: deposit queue, receive and deploy, owner-signed actions, size limits and lookup tables
- Co-signer and guardian signing, checked call by call against the pool's role addresses
- The approval message and the CCTP action digest, matching the program byte for byte (shared vectors)

## 8. Devnet end to end (planned)

One script will run the whole pool on Solana devnet against real Kamino Lend, Pyth and Circle CCTP:
stake and back from Solana, Ethereum and Stellar, covered wallets, claims signed by the production
oracle key, the fast-clock path through queue, approval, cooldown and stream, governance with the real
role keys, Squads, and `migrate`. Results will be added here.
