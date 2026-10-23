# Auditor disclosures: known-by-design behaviour (Solana USDC pool)

Written for the security auditor. Items here are deliberate design choices with known
consequences, disclosed up front so they are not reported as undiscovered findings. Each one points
to the code and the tests that pin it. Paths are under `programs/safu_pool/` unless they start with
`crates/`.

## 1. Pool liquidity: claims are paid from pooled money over time

**Behaviour.** Stakers always get their full principal back, plus their yield share. A claim larger
than the claimant's own forfeited stake is paid from pooled liquidity; the program does not assign
that shortfall to any staker. New stake, the protocol's yield share and backer capital cover it over
time. Solvency is enforced at admission (`total_allocated + entitlement <= total_staked +
total_backed`, plus the daily admission cap and the oracle's daily limit), not by per-staker loss
accounting. Every exit, for stakers and backers alike, takes only free capital: after the payment,
open claims must still fit.

**Consequence.** If new money stops, the last withdrawer can get `InsufficientLiquidity` and wait.

**Tests.** `tests/settings_and_exits.rs` (`stakers_take_only_free_capital_and_wait_for_the_rest`,
`stakers_and_backers_follow_the_same_free_capital_line`), `crates/pool-core/tests/claim_rules.rs`
(`admitted_claims_never_exceed_capacity`), and the stateful fuzz (`tests/fuzz_stateful.rs`), which
checks holdings against the books after every step.

## 2. Upgrades: Squads 2 of 3, 7-day time lock, then `migrate`

**Behaviour.** The program is upgradeable. Its upgrade authority is a Squads v4 multisig whose
members are the pool's three roles (admin, co-signer, guardian), threshold 2, with a 7-day time lock
between approval and execution (`scripts/squads/multisig.mjs`). `initialize` needs the upgrade
authority's signature, so the deployer runs it before handing the authority to Squads.

Every account carries a version and spare bytes. Solana runs upgraded code from the next slot, so
`migrate` is the transaction after an upgrade. Until it runs, every other instruction refuses with
`NotMigrated`, so new code never reads old data.

**Trust limit, stated plainly: 2 of the 3 roles = full control after 7 days.** Stakers can see a
pending upgrade on-chain for those 7 days and leave. `withdraw` works unless the pool is paused, and
`emergency_exit` works while it is.

**Tests.** `tests/governance.rs` (`an_older_pool_refuses_every_call_until_migrate_runs`),
`tests/stake_back.rs` (`initialize_only_by_upgrade_authority`).

## 3. Governance inside the program: 2 of 3 roles, 7-day delay, 90-day recovery

**Behaviour.** Roles, the oracle, the yield venue, the treasury, the venue deploy share and the
registry writer change only through `instructions/governance.rs`: one role proposes, a second role
approves the same change, it waits 7 days in public (`GOV_DELAY_SECS`), then anyone executes.
Cancelling also takes two roles. The role a change replaces never counts toward it. A new role,
oracle or registry writer key must sign its own installation. The three roles and the oracle must be
four different keys. Changes made before any money arrives (treasury, venue, deploy share) take no
wait; a role change always waits.

**Last-resort recovery.** If two roles are lost, the one left can replace a role alone after 90 days
(`RECOVERY_DELAY_SECS`). Any other working role, including the one being replaced, can cancel it, and
the recovery lapses if its proposer loses the role.

**Accepted trade-off.** One role can take over after 90 days if nobody objects. Not recoverable: all
three roles lost, or one honest role against one thief (each cancels the other). In both cases
stakers can still withdraw and collect claims, and a pause ends on its own (section 4).

**Tests.** `tests/governance.rs` (17 tests).

## 4. The admin alone can pause and suspend, within limits

**Behaviour.** `pause` and `suspend_stake` need only the admin.
- A pause lasts at most `PauseMaxSecs` (default 30 days) and ends on its own. There is no pause on top
  of a pause, and the next one waits `PauseGapSecs` after the last one ended.
- A suspension by the admin alone lapses after 30 days (`SUSPEND_MAX_SECS`). Suspending the same stake
  again within that length needs the co-signer or the guardian as well (`renew_suspension`).
- Paused time, and whole days on which a payout found the pool short of cash, do not count against a
  claim's approval or collection window. Paused time does not count against the hack-to-filing
  window either. The expiry sweeps refuse while paused.

**What a pause blocks on Solana.** New stakes, claims and `withdraw` stop. `emergency_exit` works
**only** while paused and still honours the 365-day lock after a cancelled claim. Backer requests and
collections keep working, and so do staker and backer yield claims. `withdraw_yield` (protocol
revenue) is refused while paused.

**Consequence.** A stolen admin key can delay stakes, claims and payouts for one pause. It cannot move
money, change who gets paid or raise a payout, and the other two roles replace it through governance.

**Tests.** `tests/settings_and_exits.rs` (pause rules), `tests/claims.rs` (suspension rules),
`tests/stake_back.rs` (`emergency_exit_only_while_paused_and_honours_the_penalty_lock`).

## 5. Kamino Lend, called directly, with every call pre-checked

**Behaviour.** Idle USDC goes to one Kamino Lend USDC reserve (`Pool.yield_venue`), up to the deploy
share (at most 80%). Money in (`stake`, `back`, `mature_backing`) pushes idle cash above the buffer.
Money out (`withdraw`, `claim_stream`, `emergency_exit`, backer withdrawals, yield claims, treasury)
pulls the shortfall plus a refill of the buffer, or the shortfall alone (`leg.rs`, `kamino.rs`).

Solana cannot catch a failed cross-program call. So every best-effort step checks in advance
everything klend would check (reserve status, the market's emergency flag, deposit limit, cash in the
reserve after queued withdrawal tickets), and skips if any check fails. A venue that cannot pay never
fails a staker's call. The payment then refuses as `InsufficientLiquidity`, or, for a claim payout,
pays nothing and counts a cash-short day.

**Checks on klend.** klend is upgradeable and outside our control. Before any byte is read, owner,
discriminator and length are checked. Every account the pool passes (market, market authority, supply
vault, collateral mint, the pool's kUSDC account, the one price account) is checked against what the
reserve itself records. Governance installs only a klend reserve of the pool's USDC, never while
anything is deployed. A reserve priced by both Scope and Pyth, or by Switchboard, is not accepted.

**Accounting.** Book value is never marked to market; growth is booked only when it comes back, as
yield, capped at 10 bp a day of book value. A shortfall on a redeem is a loss taken off `total_staked`
pool-wide, never off one staker's payout and never off backers. During a venue loss, early exits are
paid in full and the loss stays with those who remain. Keeper rebalances accept at most 5% below book
value; only the admin can redeem lower (`provide_liquidity` with its own `min_out`, works while
paused).

**Compute.** A harvest needs about 240k compute units and a push about 175k
(`HARVEST_MIN_CU`, `PUSH_MIN_CU`). Each is skipped when less is left, so a user call never fails for
lack of compute; the next call does it.

**Tests.** `tests/kamino.rs` (15 tests against the real klend binary and mainnet reserve accounts),
`crates/pool-core/tests/kamino_leg.rs`.

## 6. Rounding dust when a pool has backers but no stakes

**Behaviour.** klend rounds each redeem down, so a redeem can return 1 to 4 base units less than its
book value. The program takes that shortfall off `total_staked` (section 5), saturating at zero. In a
pool with backers and no stakes, there is nothing to take it off, so the dust stays unbooked: the
books can show up to a few base units (0.000001 USDC each) more than the pool holds.

**Why accepted.** Backers are never marked down by design, the amount is negligible, and the
rounding reserve (section 9) covers a payment short by up to 0.001 USDC. Booking it elsewhere would
add a new money path for no practical gain. The stateful fuzz allows exactly this gap, only while
stakes are below the rounding amount, and fails on any other.

**Tests.** `tests/fuzz_stateful.rs` (the books check), `tests/stake_back.rs`
(`the_rounding_reserve_covers_only_a_small_gap`).

## 7. The claim line: order, never speed

**Behaviour.** A claim the pool cannot admit today (capacity, the daily admission cap or the oracle's
daily limit) joins a line of `QueueEntry` accounts (`instructions/claim.rs`). Claims are released
strictly in the order they joined, and a new claim joins the back while anyone waits, even if it
would fit alone. Each paying claim gets a pro-rata share of the day's payout cap by what it is still
owed, set from the day's snapshot, so the order of calls doesn't matter. A queued claim never expires
on time; it leaves the line only once it can never be released.

**Consequence.** A small claim can wait behind a large one.

**Tests.** `tests/claims.rs` (`the_line_is_first_come_first_served`,
`a_new_claim_joins_the_back_while_anyone_waits`, `claims_share_the_days_cap_pro_rata_whatever_the_order`).

## 8. Upkeep runs inside user transactions, so they carry extra accounts

**Behaviour.** There is no keeper. Every user instruction ends with upkeep: release the head of the
line if there is room, and check two places on the watch list (approve a claim past its gate, expire
an unapproved one, mark an uncollected one stale). On Solana an instruction can only touch accounts the
transaction lists, so every user transaction carries the line head and the two watch places as
remaining accounts. If they are missing or stale, the call refuses with `UpkeepAccountsMissing`, and
the site reads the pool again and resends once (a second wallet prompt). Upkeep is skipped while
paused.

**Consequence.** On a quiet day bookkeeping waits for the next transaction. What a user is owed never
depends on it: the claim instructions apply the same checks themselves.

**Tests.** `tests/upkeep.rs` (8 tests, including `the_largest_transaction_fits_one_packet`).

## 9. Rounding reserve

**Behaviour.** Anyone can fund the rounding reserve (`fund_rounding_reserve`) and nothing can take it
out. A payment still short after the venue pays may draw up to 1,000 base units (0.001 USDC) from it.
It is not pool cash, capacity or treasury surplus.

**Tests.** `tests/stake_back.rs` (`the_rounding_reserve_covers_only_a_small_gap`),
`crates/pool-core/tests/points.rs`.

## 10. After a paid claim, that address never stakes again

**Behaviour.** An approved claim forfeits the whole stake, and the forfeited record refuses every new
stake (`AlreadyStaked`). A covered wallet is bound to its staker for good, and coverage only checks for
a live stake, so a same-address re-stake would cover the drained wallet again. The owner can use a
fresh address with new covered wallets.

**Tests.** `tests/stake_back.rs` and `tests/claims.rs` (`forfeited_address_can_never_stake_again`).

## 11. Ethereum and Stellar owners act through an identity the pool controls

**Behaviour.** An Ethereum or Stellar user burns USDC over Circle CCTP v2, naming this pool's inbox
as the recipient and its CCTP authority as the only allowed caller (`instructions/cctp.rs`). Anyone
(SAFU's relayer, which pays rent) completes it: `cctp_receive` moves the mint into the owner's
`held` account, and `cctp_deploy` stakes or backs it as the owner's identity (a PDA). If the pool
refuses, the USDC stays in `held` and can be deployed later or sent home.

Every other action is signed by the owner at home: an Ethereum `personal_sign`, checked by secp256k1
recovery, or a Stellar SEP-53 signed message, checked by the Ed25519 instruction right before. The
signed digest covers the program, the cluster, the account, a nonce, an expiry of at most 1 hour, and
a length-prefixed hash of the call data and every account it uses. Only 11 pool calls are allowed;
`set_beneficiary` is not one of them, so payouts stay with the identity. Money leaves an identity only
into the pool, or home by burn to the owner's own address.

**Known limits.**
- No low-s check on Ethereum signatures. A malleated signature also recovers, but the nonce makes
  each action work once.
- Stellar owners must burn from a classic `G...` account: a contract account cannot sign the
  messages above.
- The relayer can delay or drop an action, but cannot change one; the owner can use another relayer.

**Tests.** `tests/cctp.rs` (20 tests against Circle's CCTP programs), including
`a_signature_cannot_be_resplit_into_other_data_and_accounts`.

## 12. The program trusts one oracle signature per claim, and prices nothing itself

**Behaviour.** The oracle prices the loss in USD off-chain and signs it in USDC base units, with the
staker, the drain transaction hash, the tier, the hack time and a deadline at most 24 hours ahead,
under a domain string, the program id and the cluster. The transaction carries one Ed25519 precompile
instruction **immediately before** `submit_claim`, with every offset inside that same instruction,
and the oracle also signs the transaction. The program checks the entitlement against the stake's
tier ceiling, then solvency and the daily limits. The claim account address
`[claim, pool, staker, tx_hash]` is the claim id, so a drain transaction can be claimed once, ever.
Covered wallets are checked off-chain before the oracle signs. The admin can revoke a signed approval
before it is used; the revocation record is permanent.

**Tests.** `tests/claims.rs` (signature, offset, revocation and ceiling tests),
`tests/approval_vector.rs` (the message matches the backend's byte for byte).

## 13. The fast-clock `demo` build

**Behaviour.** The `demo` feature shortens every waiting period to minutes for testing
(`crates/pool-core/src/params.rs`). A demo build refuses to initialize a mainnet pool, so it can never
run as the real thing.

**Tests.** `tests/stake_back.rs` (`a_demo_build_refuses_mainnet`).

## 14. Not yet checked: duplicate mutable accounts

Our internal Solana checklist could not confirm, for every instruction, that two mutable account
slots can never be passed the same account. Anchor's typed accounts and seeds rule out most cases,
but we have not proven it instruction by instruction. We flag it for the audit.
