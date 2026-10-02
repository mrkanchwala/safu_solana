# Port map: multichain protection pool → Solana pool

Source: `safu_multichain/contracts/protection-pool/src/` (Soroban, live on Stellar mainnet since
2026-09-29). Rules port as they are; Stellar framework code (storage, auth, token calls) is rewritten
for Solana. Every number lives in `crates/pool-core/src/params.rs`.

**Legend:** **Rules** = same rule, rewritten in Anchor + `pool-core`. **Changed** = same intent, Solana
form differs (reason given). **Left out** = not in this build.

## Entry points

| Multichain | Solana | Kind | Note |
|---|---|---|---|
| `__constructor` | `initialize` | Changed | Also takes Marinade program/state/mint (checked against the live state) and the cluster tag. Pool cap from `config/pool.devnet.json`; every other setting starts at its `params.rs` default. |
| settings (`propose_setting` … timers, cap, outflow) | `propose_setting` / `approve_setting` / `execute_setting` / `cancel_setting` | Changed | 19 settings, each by slot number (`SETTING_*` in the IDL): stake min/max, yield split, cooldown, vesting, admit + payout bands, backer notice, maturity, pause max + gap, approve + inactivity windows, pool cap. Admin proposes, co-signer approves the same value, anyone executes after `SETTINGS_TIMELOCK_SECS` (7 days, demo 5 min). Hard ranges (`pool_core::settings`) and the order between settings (min ≤ max, low ≥ mid ≥ high, pause max ≤ pause gap) are checked at proposal and again at execution. 32 slots, 13 spare. Stored inside `Pool`, so no extra account in any instruction. Every read goes through `Pool::settings()`, the one hook for a future self-managing mode. Multichain only lets timers, cap and outflow change. |
| `propose_change` / `approve_change` / `propose_recovery` / `cancel_change` / `execute_change` / `get_pending_change` | none | Left out | Governance 2-of-3 + 90-day recovery. Admin + co-signer override kept. Stretch item. |
| `get_admin` / `get_co_signer` / `get_oracle` / `get_guardian` / `get_paused_until` | `Pool` account fields | Changed | Reads are account fetches on Solana. No guardian (came with governance). |
| `set_pool_cap` | none (`PoolCap` setting) | Changed | Through the settings timelock, up or down. Lowering it never touches money already in; it only blocks new stake above it. |
| `propose_setting` / `approve_setting` / `execute_setting` / `cancel_setting` / `get_setting` / `get_pending_setting` | none | Left out | Settings timelock. Values are constants in `params.rs`; returns with governance. |
| `pause` / `unpause` | `pause` / `unpause` | Changed | Pause expires after the `PauseMaxSecs` setting. **No pause while paused, and a gap** (more than `PauseGapSecs` after the last pause ended; 30 days on both builds, never below the 30-day filing window and never shorter than `PauseMaxSecs`) after the last pause ends (stops back-to-back pauses). **Paused time does not count** against a claim's approve, inactivity or hack-time window (`paused_before` + `pause_mark` per claim); expiry sweeps are refused while paused. Multichain has neither (flagged). |
| `suspend_stake` / `unsuspend_stake` | same | Rules | Unsuspend restarts the approve / collection clock. |
| `stake` | `stake` | Rules | SOL. Bounds = bps of pool cap (0.05–0.5 SOL at 50 SOL). `StakeRecord` PDA `[stake, pool, staker]`. |
| `withdraw` | `withdraw(amount)` | Changed | **Whole or partial.** What stays must be 0 or at least the min stake. Pays that principal + all unpaid yield to the stored beneficiary. Principal leaves only if open claims still fit in capacity afterwards (one rule for staker withdraw, emergency exit and backer withdrawal; re-checked after the payout). Whole withdraw **closes** the `StakeRecord` (rent back to the staker). |
| `set_beneficiary` | `set_beneficiary` | Changed | Also blocked while a claim is open or queued. |
| `emergency_exit` | `emergency_exit(amount)` | Changed | **Only while paused**, whole or partial (same rules and free-capital check as `withdraw`), pays the staker, closes the record on a whole exit, and **honours the penalty lock** (multichain has neither check, so there a cancelled false positive could leave early; flagged to the founder). |
| `back` / `mature_backing` / `request_backer_withdrawal` / `cancel_backer_withdrawal` / `complete_backer_withdrawal` | same | Changed | Four withdrawal-safety rules unchanged. `BackerRecord` PDA `[backer, pool, backer]`. Two additions: `request_backer_withdrawal` harvests first (it can mature pending money, like `mature_backing`), and `complete_backer_withdrawal` re-checks free capital **after** the payout, since its Marinade unstake can mark a loss off `total_staked`. |
| `get_backer` / `get_total_backed` / `get_total_backed_pending` / `get_capacity` | account fields | Changed | Capacity = total staked + total backed (matured). |
| `submit_claim` | `submit_claim` | Changed | Oracle signs the tx **and** an Ed25519 precompile instruction right before it (backstop `verdict.rs` pattern). Claim PDA `[claim, pool, staker, tx_hash]`: the address is the id; a record whose status is not `Unused` means the claim already exists (so a cancelled or expired claim can never be resubmitted). |
| `revoke_approval` | `revoke_approval` | Changed | `RevokedApproval` PDA per payload hash, permanent (no TTL problem). `submit_claim` treats an approval as revoked only when that PDA is owned by this program: SOL sent to the address does not block it. |
| `unlock_pending_claim` | same | Rules | |
| `try_release_queued_claim` / `expire_queued_claim` | same | Rules | Permissionless. |
| `approve_claim` | same | Rules | Staker signs. No points burn (points left out). |
| `expire_pending_approval` / `expire_stale_claim` | same | Rules | Permissionless. |
| `claim_stream` | `claim_stream` | Changed | Pays the stored beneficiary. If liquid SOL is short, Marinade `liquid_unstake` of the shortfall; **the unstake fee on that part comes off that payment** (devnet default; roadmap: user chooses instant or delayed). Same for every payout: withdraw, emergency exit, backer withdrawal, yield claims, treasury. |
| `cancel_claim` | same | Changed | Penalty lock only when the stake was forfeited. What the claim already paid out stays paid and comes off the restored stake; a claim that paid the whole stake leaves it forfeited and earmarked for no claim. Multichain restores the full stake. |
| `approve_override` / `cancel_pending_override` | same | Changed | 2-of-2 admin + co-signer. `Override` PDA `[override, claim]`. On a forfeited stake, only the claim that forfeited it (`StakeRecord.forfeited_by`) may be re-executed; multichain lets any claim count that forfeited principal as capacity again. |
| `get_stake` / `get_claim` / `is_eligible` / `is_claim_eligible` | account fetch + client helpers | Changed | Eligibility maths lives in `pool-core`, read by clients through the IDL constants. |
| `get_points_balance` | none | Left out | Points (V8 legacy). Not shown anywhere. |
| `deploy_to_vault` / `auto_deploy_liquidity` / `provide_liquidity` / `ensure_liquidity` | `rebalance` (permissionless) + in-path push (after `stake` / `back`) and pull (every payout) | Changed | Marinade `deposit` / `liquid_unstake`, built by hand. Push = multichain `push_idle` (idle above open claims, up to `DEPLOY_BPS` = 80%, once ≥ `AUTO_PUSH_MIN_BPS`). `rebalance` pull = multichain `ensure_liquidity` target (open claims short of free cash, or deployment above the line); the pool pays that fee. No admin deploy / provide calls. |
| `harvest` | `harvest` | Changed | Yield = mSOL value growth above book (Marinade `State.msol_price`), unstaked and credited as the SOL that arrives. Growth limit 10 bp/day kept. Also runs first inside `stake`, `back`, `mature_backing`, `withdraw`, `cancel_claim` (approved claims), `claim_yield`, `claim_backer_yield`, `complete_backer_withdrawal` (multichain call sites), so growth goes to the money already in. |
| `claim_yield` / `claim_backer_yield` | same | Rules | Any time (not while paused), principal untouched. Staker yield goes to the stored beneficiary. |
| `withdraw_yield` | `withdraw_yield` | Changed | Treasury takes protocol revenue only, capped at the protocol surplus. **Refused while paused** (multichain allows it). |
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
| `covered-registry` contract (separate) | `CoveredWallet` `[covered, pool, wallet_hash]` (forever) + `StakerWallets` `[staker_wallets, pool, staker]` (max `MAX_COVERED_WALLETS`, 2, Solana or Ethereum in any mix; the 2nd slot sits after `bump`, taken from the old spare bytes). Writer = backend key. |
| Daily counters keyed by day | Current day only, on `Pool`, reset when the day changes. |

## Other changes

- **One clock:** Unix seconds (`Clock`). Multichain used ledger sequence for durations and timestamps for windows.
- **Beneficiary:** stored as a plain `Pubkey`, since it is public at payout anyway. Never the pool or vault (`BeneficiaryIsPoolAccount`).
- **Room to upgrade:** the program is upgradeable (same pool address, same records). `StakeRecord`, `BackerRecord`, `Claim` and `StakerWallets` carry `version` (1) + 64 spare bytes; `Pool` carries 256 spare bytes, 13 spare setting slots and `asset_mint` (native SOL today) so a USDC or CCTP version can migrate in place instead of launching a new pool.
- **Snapshots:** a claim keeps the rules it started with. Approve window at admission, inactivity window at activation, cooldown and vesting at approval. A later setting change applies to new claims only.
- **Stake record reuse:** closed on withdraw / emergency exit and re-created by the next stake. A forfeited record is never closed, so that address can never stake again (as multichain), enforced by `init` failing.
- **Positions are program records.** No position token is minted, and mSOL stays in the pool.
- **No price feed.** The oracle converts the USD loss to lamports off-chain and signs the lamport amount; the program checks it against the tier ceiling in SOL.

## Marinade leg

- **Solana cannot catch a failed call.** Multichain's best-effort `try_deposit` / `try_withdraw` become
  pre-checks: push and harvest skip unless Marinade is unpaused, the amount clears Marinade's minimum
  and (harvest) its liquidity pool can pay. A payout that needs an unstake gets it or fails whole with
  `InsufficientLiquidity` / `UnstakeFeeTooHigh`, and nothing changes.
- **Who pays Marinade's fee.** The payee pays the fee on the part of an unstake their payment needed
  (rounded up). If the whole mSOL position was not worth their shortfall, they take what it returned
  (in practice one lamport of Marinade rounding). A `rebalance` unstake is the pool's: growth that
  comes back pays the fee first, any rest is a loss marked off `total_staked` (multichain
  `DeploymentShortfall`).
- **Fee limit.** Payee-paid unstakes accept Marinade's fee up to Marinade's own `lp_max_fee` (decided
  2026-09-30: the payee pays whatever Marinade charges; the site shows it before signing, via
  `pool_core::marinade::unstake_fee_bps`). Pool-paid `rebalance` unstakes are held to
  `MAX_REBALANCE_SLIPPAGE_BPS` (5%), else `UnstakeFeeTooHigh`. Marinade's fee is linear from `lp_min_fee`
  (at or above its liquidity target) to `lp_max_fee` (empty). Devnet 2026-09-30: 15,264 SOL against a
  160,000 SOL target, fee ~8.2%: payouts go through at that cost, rebalances wait.
- **Compute units** (LiteSVM, Marinade at target liquidity): stake + deposit 77k, harvest 60k,
  claim_yield + harvest 68k, withdraw + harvest + unstake 117k. Client limit `computeUnitLimit` in
  `config/pool.devnet.json` (200k); the test suite fails if any path exceeds it.
