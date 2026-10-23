# Audit scope: SAFU protection pool (Solana)

Prepared 2026-10-23 for the audit quote. The target is the USDC pool on branch `solana-usdc-pool` of
this repository. `main` still holds the earlier SOL demo pool (Marinade staking), which is not the
audit target. A frozen commit and tag follow once the devnet end-to-end run passes; until then the
branch tip is the code to read.

This is the Solana counterpart of the Stellar pool in `mrkanchwala/safu_multichain`: the same pool
rules, the same three roles and the same settings, rebuilt in Anchor for Solana. Where Solana needs
a different shape, the reason is written in [PORT_MAP.md](PORT_MAP.md).

## 1. In scope: Solana (Anchor 1.1.2, Rust 1.96, classic SPL Token)

| Part | Path | Lines (source, no tests/comments/blank) | What it does |
|---|---|---|---|
| Pool program | `programs/safu_pool/src/` | 6,211 | Stake, back, claims and the claim line, payout streams, settings, governance, the covered-wallet registry, the Kamino Lend USDC leg, yield and points, upkeep inside every user call, and stake or back from Ethereum and Stellar over Circle CCTP v2 |
| Rule library | `crates/pool-core/src/` | 1,264 | Every pool rule as a pure function (bounds, tiers, admission, drip, yield and points maths, settings ranges, Kamino reserve reading, CCTP message and signature layout), called by the program and read by clients through the IDL |
| **Total** | | **7,475** | |

Line counts exclude `tests/`, `#[cfg(test)]` blocks, comments and blank lines. Measured the same way,
the Stellar pool's five contracts come to 5,972 lines; its published scope says 5,612 because it was
counted with a different tool. The program binary is 1,454,536 bytes.

The covered-wallet registry and the cross-chain stake path are inside the pool program here. On
Stellar they are separate contracts (`covered-registry`, `cctp-adapter`, `safu-account`).

### Largest files

| File | Lines (raw) | What it holds |
|---|---|---|
| `instructions/claim.rs` | 1,397 | Submit, the claim line, time gate, approval, payout stream, cancel, override, expiry sweeps |
| `instructions/cctp.rs` | 781 | CCTP receive, deploy, owner-signed actions, send home |
| `state.rs` | 600 | Every account and its layout |
| `instructions/backer.rs` | 530 | Backing, per-deposit clocks, the daily withdrawal drip |
| `pool-core/src/cctp.rs` | 482 | Circle message parsing, the owner action digest, decimals check |
| `leg.rs` | 458 | Push, pull and harvest against Kamino; how a redeem lands on the books |
| `instructions/stake.rs` | 435 | Stake, withdraw, beneficiary, emergency exit |
| `instructions/governance.rs` | 405 | Two of three roles, 7-day delay, 90-day recovery |
| `instructions/upkeep.rs` | 400 | Upkeep accounts, the watch list, protocol revenue, the rounding reserve |

## 2. Out of scope

- The site (`app/`), deploy and ops scripts (`scripts/`), all tests and the fuzz harness.
- The off-chain scanner, oracle signer, registry writer, CCTP relayer and claim API (Python, private
  repository). The program trusts one oracle Ed25519 signature per claim and checks it against its
  own limits; the registry writer's key is the only one that can bind a covered wallet.
- Third-party programs the pool calls: Kamino Lend (klend), Circle CCTP v2 (MessageTransmitter,
  TokenMessenger), SPL Token, the Ed25519 and secp256k1 native programs, and Squads v4 (holds the
  upgrade authority).
- The earlier SOL pool on `main`.

## 3. Read first

| Document | What it covers |
|---|---|
| `README.md` | What the pool does, in user terms |
| `docs/PORT_MAP.md` | Every Stellar entry point and account, and its Solana form: ported as is, changed (and why), or left out |
| `docs/AUDITOR_DISCLOSURES.md` | Known-by-design behaviour on Solana, including the trust limit (2 of 3 roles = full control after 7 days) |
| `docs/TESTING.md` | How to build and run every test |
| STRIDE threat model | `docs/STRIDE_THREAT_MODEL.pdf` (2026-10-23, this pool) |
| `DISCLOSURE.md` | Prior work this code builds on |

## 4. Whitelabel deployments and other yield venues

SAFU is sold to wallets, apps and protocols that run their own pool under their own name. We would
like the audit to cover that use, so a partner deployment does not need a new audit each time.

The lending protocol's own contracts are out of scope: SAFU only uses audited venues and relies on
their audits. The audit covers SAFU's code, its settings and the connector that calls the venue.

**Partner pools with their own settings.** A partner pool is a new deployment of the same audited
binary (verifiable build), with its own roles and its own values for the adjustable settings. Every
setting has hard bounds and ordering rules in the code (`pool-core/src/settings.rs`); values are
picked inside them, and changes made before the first deposit take effect without the 7-day wait.
The code stays exactly as audited.
*Question for the auditor:* can a partner deployment of the audited commit, with any settings
inside the coded bounds, rely on this audit without a new one? If not, would a short deployment
review (build hash, settings, roles and addresses checked against the audited commit) be enough,
and what would it cost per pool?

**A different lending protocol.** The pool lends its idle USDC on Kamino Lend. The program calls
klend directly and governance only accepts a Kamino USDC reserve as the venue, so a partner who wants
the money in their own lending market needs a new connector in the program (deposit, redeem, read
the position's value, plus the account checks).
*Questions for the auditor:*
- What would a delta audit of one new lending connector cost, on top of this audit?
- Would you rather review a general venue interface now (Kamino as the first connector), so each
  later connector is a smaller review?

## 5. Testing done so far

- 318 tests on each build (normal and the fast-clock `demo` build): 225 program tests on LiteSVM, with
  Kamino Lend's mainnet program and reserve accounts and Circle's CCTP programs loaded, plus 93
  `pool-core` unit and property tests (`docs/TESTING.md`).
- Stateful fuzz: 593,250 steps across both builds, 0 failures.
- Mutation testing: 1,273 mutants over the program and `pool-core`. Every mutant the first full run
  missed is now caught by a test, or proven unable to change behaviour
  (`docs/MUTATION_EQUIVALENTS.md`, 17 entries).
- Internal reviews on 2026-10-23: code review, security review with STRIDE, infrastructure review and
  a Solana checklist audit. No critical or high findings; the 7 medium findings and every low finding
  with a fix are fixed and tested.
