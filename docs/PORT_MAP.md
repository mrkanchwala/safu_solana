# Port map: multichain protection pool → Solana pool

Source: `safu_multichain/contracts/protection-pool/src/` (Soroban, live on Stellar mainnet since
2026-09-29). Rules port as they are; Stellar framework code (storage, auth, token calls) is rewritten
for Solana. Every number lives in `crates/pool-core/src/params.rs`.

**Legend:** **Rules** = same rule, rewritten in Anchor + `pool-core`. **Changed** = same intent, Solana
form differs (reason given). **Left out** = not in this build.

## Entry points

| Multichain | Solana | Kind | Note |
|---|---|---|---|
| `__constructor` | `initialize` | Changed | Also takes Marinade program/state/mint (checked against the live state) and the cluster tag. Pool cap from `config/pool.devnet.json`. |
| `propose_change` / `approve_change` / `propose_recovery` / `cancel_change` / `execute_change` / `get_pending_change` | — | Left out | Governance 2-of-3 + 90-day recovery. Admin + co-signer override kept. Stretch item. |
| `get_admin` / `get_co_signer` / `get_oracle` / `get_guardian` / `get_paused_until` | `Pool` account fields | Changed | Reads are account fetches on Solana. No guardian (came with governance). |
| `set_pool_cap` | `set_pool_cap` | Rules | Increase only. |
| `propose_setting` / `approve_setting` / `execute_setting` / `cancel_setting` / `get_setting` / `get_pending_setting` | — | Left out | Settings timelock. Values are constants in `params.rs`; returns with governance. |
| `pause` / `unpause` | `pause` / `unpause` | Rules | Pause expires after `PAUSE_MAX_SECS`. |
| `suspend_stake` / `unsuspend_stake` | same | Rules | Unsuspend restarts the approve / collection clock. |
| `stake` | `stake` | Rules | SOL. Bounds = bps of pool cap (0.05–0.5 SOL at 50 SOL). `StakeRecord` PDA `[stake, pool, staker]`. |
| `withdraw` | `withdraw` | Changed | Pays principal + unpaid yield to the stored beneficiary and **closes** the `StakeRecord` (rent back to the staker). |
| `set_beneficiary` | `set_beneficiary` | Changed | Also blocked while a claim is open or queued (eng review D1). |
| `emergency_exit` | `emergency_exit` | Changed | **Only while paused**, pays the staker, closes the record, and **honours the penalty lock** (multichain has neither check, so there a cancelled false positive could leave early; flagged to the founder). |
| `back` / `mature_backing` / `request_backer_withdrawal` / `cancel_backer_withdrawal` / `complete_backer_withdrawal` | same | Rules | Four withdrawal-safety rules unchanged. `BackerRecord` PDA `[backer, pool, backer]`. |
| `get_backer` / `get_total_backed` / `get_total_backed_pending` / `get_capacity` | account fields | Changed | Capacity = total staked + total backed (matured). |
| `submit_claim` | `submit_claim` | Changed | Oracle signs the tx **and** an Ed25519 precompile instruction right before it (backstop `verdict.rs` pattern). Claim PDA `[claim, pool, staker, tx_hash]`: the address is the id; a record whose status is not `Unused` means the claim already exists (so a cancelled or expired claim can never be resubmitted). |
| `revoke_approval` | `revoke_approval` | Changed | `RevokedApproval` PDA per payload hash, permanent (no TTL problem). |
| `unlock_pending_claim` | same | Rules | |
| `try_release_queued_claim` / `expire_queued_claim` | same | Rules | Permissionless. |
| `approve_claim` | same | Rules | Staker signs. No points burn (points left out). |
| `expire_pending_approval` / `expire_stale_claim` | same | Rules | Permissionless. |
| `claim_stream` | `claim_stream` | Changed | Pays the stored beneficiary. If liquid SOL is short, Marinade `liquid_unstake` of the shortfall; **the unstake fee on that part comes off that payment** (devnet default; roadmap: user chooses instant or delayed). Same for every payout: withdraw, emergency exit, backer withdrawal, yield claims, treasury. |
| `cancel_claim` | same | Rules | Penalty lock only when the stake was forfeited. |
| `approve_override` / `cancel_pending_override` | same | Rules | 2-of-2 admin + co-signer. `Override` PDA `[override, claim]`. |
| `get_stake` / `get_claim` / `is_eligible` / `is_claim_eligible` | account fetch + client helpers | Changed | Eligibility maths lives in `pool-core`, read by clients through the IDL constants. |
| `get_points_balance` | — | Left out | Points (V8 legacy). Not shown anywhere. |
| `deploy_to_vault` / `auto_deploy_liquidity` / `provide_liquidity` / `ensure_liquidity` | `rebalance` (permissionless) + in-path push (after `stake` / `back`) and pull (every payout) | Changed | Marinade `deposit` / `liquid_unstake`, built by hand. Push = multichain `push_idle` (idle above open claims, up to `DEPLOY_BPS` = 80%, once ≥ `AUTO_PUSH_MIN_BPS`). `rebalance` pull = multichain `ensure_liquidity` target (open claims short of free cash, or deployment above the line); the pool pays that fee. No admin deploy / provide calls. |
| `harvest` | `harvest` | Changed | Yield = mSOL value growth above book (Marinade `State.msol_price`), unstaked and credited as the SOL that arrives. Growth limit 10 bp/day kept. Also runs first inside `stake`, `back`, `mature_backing`, `withdraw`, `cancel_claim` (approved claims), `claim_yield`, `claim_backer_yield`, `complete_backer_withdrawal` (multichain call sites), so growth goes to the money already in. |
| `claim_yield` / `claim_backer_yield` | same | Rules | Any time (not while paused), principal untouched. Staker yield goes to the stored beneficiary. |
| `withdraw_yield` | `withdraw_yield` | Rules | Treasury takes protocol revenue only, capped at the protocol surplus. |
| `get_liquid_balance` … `get_total_stakers` / `is_paused` (views) | account fields | Changed | Read the `Pool` account. |

## Accounts

| Multichain storage | Solana account |
|---|---|
| Instance storage (admin, oracle, totals, indexes, day counters) | `Pool` PDA `[pool]` |
| Contract balance (XLM) + DeFindex shares | `vault` PDA `[vault, pool]`: system-owned, no data. Holds the SOL and owns the mSOL token account. |
| `Stake(wallet)` | `StakeRecord` `[stake, pool, staker]` |
| `Backer(address)` | `BackerRecord` `[backer, pool, backer]` |
| `Claim(claim_id)` | `Claim` `[claim, pool, staker, tx_hash]` |
| `Override(claim_id)` | `Override` `[override, claim]` |
| Revoked approvals (temporary, TTL) | `RevokedApproval` `[revoked, pool, payload_hash]` (permanent) |
| `covered-registry` contract (separate) | `CoveredWallet` `[covered, pool, wallet_hash]` (forever) + `StakerWallets` `[staker_wallets, pool, staker]` (max 3). Writer = backend key. |
| Daily counters keyed by day | Current day only, on `Pool`, reset when the day changes. |

## Other changes

- **One clock:** Unix seconds (`Clock`). Multichain used ledger sequence for durations and timestamps for windows.
- **Beneficiary:** stored as a plain `Pubkey` (it is public at payout anyway), not a hash.
- **Stake record reuse:** closed on withdraw / emergency exit and re-created by the next stake. A forfeited record is never closed, so that address can never stake again (multichain H2), enforced by `init` failing.
- **Positions are records, not tokens.** mSOL never leaves the pool.
- **No price feed.** The oracle converts the USD loss to lamports off-chain and signs the lamport amount; the program checks it against the tier ceiling in SOL.

## Marinade leg (B3)

- **Solana cannot catch a failed call.** Multichain's best-effort `try_deposit` / `try_withdraw` become
  pre-checks: push and harvest skip unless Marinade is unpaused, the amount clears Marinade's minimum
  and (harvest) its liquidity pool can pay. A payout that needs an unstake gets it or fails whole with
  `InsufficientLiquidity` / `UnstakeFeeTooHigh`, and nothing changes.
- **Who pays Marinade's fee.** The payee pays the fee on the part of an unstake their payment needed
  (rounded up). If the whole mSOL position was not worth their shortfall, they take what it returned
  (in practice one lamport of Marinade rounding). A `rebalance` unstake is the pool's: growth that
  comes back pays the fee first, any rest is a loss marked off `total_staked` (multichain
  `DeploymentShortfall`).
- **Fee limit.** `MAX_REBALANCE_SLIPPAGE_BPS` (5%) caps Marinade's fee on any unstake, else
  `UnstakeFeeTooHigh`. Marinade's fee is linear from `lp_min_fee` (at or above its liquidity target)
  to `lp_max_fee` (empty); `pool_core::marinade::unstake_fee_bps` computes it for clients.
  **Devnet 2026-09-30:** 15,264 SOL in Marinade's liquidity pool against a 160,000 SOL target: fee
  ~8.2%, so payouts that need an unstake are refused on devnet today. Open decision for the founder.
- **Compute units** (LiteSVM, Marinade at target liquidity): stake + deposit 77k, harvest 60k,
  claim_yield + harvest 68k, withdraw + harvest + unstake 117k. Client limit `computeUnitLimit` in
  `config/pool.devnet.json` (200k); the test suite fails if any path exceeds it.
