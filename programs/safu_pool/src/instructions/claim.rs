//! Claims (multichain `claim.rs`): submit → (queue) → time gate → staker approval (forfeits the
//! stake) → cooldown → vesting stream under a daily payout cap → completed. Plus cancel (false
//! positive), the 2-of-2 override, approval revocation, and the permissionless expiry sweeps.
//!
//! The line: a claim that cannot be admitted now queues at the back (`QueueEntry` at
//! `Pool.queue_tail`), and while anyone waits a new claim joins the back even if there is room for
//! it. Only the head of the line is released; calling fast buys nothing. A queued claim never
//! expires on time (days in the line are days the pool could not pay); it leaves the line only once
//! it can never be released.
//!
//! Payouts: each claim gets a pro-rata share (by what it is still owed) of the day's payout cap,
//! set at its first collection that day from the day's snapshot, so the order of calls does not
//! matter. What a claim does not take stays vested for later days.
//!
//! Blocked time and the claim windows (rule 18): the approve and collection windows run on the claim
//! clock, `Pool::claim_clock(now, claim.blocked_mark)`, which does not count time the pool could not
//! pay: paused, or a whole day on which a payout found it short of cash. The hack → submit window
//! does not count pause time. The expiry sweeps are also refused while paused. So the pool's own
//! trouble never uses up a claimant's window.
//!
//! The claim account address `[SEED_CLAIM, pool, staker, tx_hash]` is the claim id: one claim per
//! (staker, drain transaction), forever.

use anchor_lang::prelude::*;
use pool_core::claim as rules;
use pool_core::params::{PENALTY_LOCK_SECS, SUSPEND_MAX_SECS};
use pool_core::settings::SettingKey;
use pool_core::{add, capacity, drip};

use super::now;
use crate::approval::{self, ClaimApproval};
use crate::constants::{
    SEED_CLAIM, SEED_OVERRIDE, SEED_POOL, SEED_QUEUE, SEED_STAKE, SEED_VAULT, SEED_WATCH,
};
use crate::errors::{CoreResultExt, PoolError};
use crate::events::*;
use crate::leg::Money;
use crate::state::{
    Claim, ClaimStatus, OverrideRequest, Pool, QueueEntry, RevokedApproval, StakeRecord,
    WatchEntry, ACCOUNT_VERSION,
};
// Glob: `#[derive(Accounts)]` on a struct holding `Cash` needs its generated client modules.
use crate::vault::{self, *};
use anchor_spl::token::TokenAccount;

// ------------------------------------------------------------------ shared steps

fn secs_after(t: i64, secs: i64) -> Result<i64> {
    t.checked_add(secs)
        .ok_or_else(|| error!(PoolError::MathOverflow))
}

/// Daily counters hold today only; a new day starts them at zero.
pub(crate) fn roll_day(pool: &mut Pool, now: i64) {
    let today = rules::day_of(now);
    if pool.day != today {
        pool.day = today;
        pool.day_admitted = 0;
        pool.day_oracle_count = 0;
        pool.day_outflow = 0;
    }
}

fn pool_capacity(pool: &Pool) -> Result<u64> {
    capacity(pool.total_staked, pool.total_backed).core()
}

/// Admission writes, shared by `submit_claim` and `try_release_queued_claim` so they cannot drift.
/// The caller has already checked that the claim fits.
fn admit(
    pool: &mut Pool,
    stake: &mut StakeRecord,
    claim: &mut Claim,
    claim_key: Pubkey,
    now: i64,
    count_toward_oracle_limit: bool,
) -> Result<()> {
    pool.day_admitted = add(pool.day_admitted, claim.entitlement).core()?;
    if count_toward_oracle_limit {
        pool.day_oracle_count = add(pool.day_oracle_count, 1).core()?;
    }
    pool.total_allocated = add(pool.total_allocated, claim.entitlement).core()?;
    stake.active_claim = Some(claim_key);
    stake.reserved_claim = None;
    claim.stake = stake.amount;
    // The claim keeps the approve window it was admitted with.
    claim.approve_window = pool.settings().get(SettingKey::ApproveWindowSecs);
    if rules::time_gate_met(stake.staked_at, now, stake.time_gate_secs) {
        open_approve_window(pool, claim, now)?;
    } else {
        // Pause time from here on is added back to the approve window that opens at the gate.
        claim.status = ClaimStatus::PendingTime;
        claim.blocked_mark = pool.blocked_secs(now);
    }
    Ok(())
}

/// Past the time gate a pending claim awaits approval from the gate moment on, whether or not anyone
/// called anything (multichain `promote_if_due`): one-click approve, and the approve window runs
/// from the gate (pause time since admission added back). In memory; the caller persists. True if
/// promoted.
pub(crate) fn promote_if_due(claim: &mut Claim, stake: &StakeRecord, now: i64) -> Result<bool> {
    if claim.status != ClaimStatus::PendingTime
        || !rules::time_gate_met(stake.staked_at, now, stake.time_gate_secs)
    {
        return Ok(false);
    }
    let gate_at = secs_after(stake.staked_at, stake.time_gate_secs)?;
    claim.status = ClaimStatus::AwaitingApproval;
    claim.approve_deadline = secs_after(gate_at, claim.approve_window)?;
    Ok(true)
}

/// The claimant keeps the yield earned so far: settled into `yield_owed` (still set aside, paid on
/// request), before the stake leaves `total_staked` (multichain `settle_staker_yield`).
fn settle_staker_yield(pool: &Pool, stake: &mut StakeRecord) -> Result<()> {
    stake.yield_owed = super::stake::staker_owed(pool, stake)?;
    stake.yield_index_at = pool.staker_yield_index;
    Ok(())
}

/// The staker's approve window starts now, on the claim clock.
fn open_approve_window(pool: &Pool, claim: &mut Claim, now: i64) -> Result<()> {
    claim.status = ClaimStatus::AwaitingApproval;
    claim.approve_deadline = secs_after(now, claim.approve_window)?;
    claim.blocked_mark = pool.blocked_secs(now);
    Ok(())
}

/// The approve window has run out on the claim clock. It never starts before the stake's last
/// suspension ended: a suspension that lapsed on its own gave the staker no fresh window otherwise.
fn approve_window_over(pool: &Pool, claim: &Claim, stake: &StakeRecord, now: i64) -> bool {
    let deadline = claim
        .approve_deadline
        .max(stake.suspended_until.saturating_add(claim.approve_window));
    pool.claim_clock(now, claim.blocked_mark) > deadline
}

/// No collection for longer than the inactivity window, on the claim clock, counted from the later
/// of the last collection and the end of the stake's last suspension.
fn collection_window_over(pool: &Pool, claim: &Claim, stake: &StakeRecord, now: i64) -> bool {
    let from = claim.last_collected.max(stake.suspended_until);
    pool.claim_clock(now, claim.blocked_mark)
        .saturating_sub(from)
        > claim.inactivity_window
}

/// The collection (inactivity) window restarts now, on the claim clock.
fn restart_collection_window(pool: &Pool, claim: &mut Claim, now: i64) {
    claim.last_collected = now;
    claim.blocked_mark = pool.blocked_secs(now);
}

/// Starts the payout clocks from now, with the capacity snapshot taken first. Cooldown, vesting and
/// the inactivity window are the settings at this moment; the claim keeps them.
fn start_clocks(pool: &Pool, claim: &mut Claim, now: i64) -> Result<()> {
    let s = pool.settings();
    claim.capacity_snapshot = pool_capacity(pool)?;
    claim.cooldown_ends = secs_after(now, s.get(SettingKey::CooldownSecs))?;
    claim.vesting_ends = secs_after(claim.cooldown_ends, s.get(SettingKey::VestingSecs))?;
    claim.inactivity_window = s.get(SettingKey::InactivitySecs);
    // The inactivity clock starts at cooldown end: the cooldown is never the staker's inactivity.
    claim.last_collected = claim.cooldown_ends;
    claim.blocked_mark = pool.blocked_secs(now);
    claim.status = ClaimStatus::Active;
    Ok(())
}

/// Approval: forfeits the stake (and its unpaid yield) and starts cooldown + vesting.
fn activate(
    pool: &mut Pool,
    stake: &mut StakeRecord,
    claim: &mut Claim,
    claim_key: Pubkey,
    now: i64,
) -> Result<u64> {
    super::stake::settle_points(pool, stake, now)?;
    let points_burned = core::mem::take(&mut stake.points);
    settle_staker_yield(pool, stake)?;
    stake.forfeited = true;
    stake.claim_approved = true;
    stake.forfeited_by = Some(claim_key);
    start_clocks(pool, claim, now)?;
    claim.stake = stake.amount;
    pool.total_staked = pool.total_staked.saturating_sub(stake.amount);
    pool.total_stakers = pool.total_stakers.saturating_sub(1);
    Ok(points_burned)
}

fn release_allocation(pool: &mut Pool, amount: u64) {
    pool.total_allocated = pool.total_allocated.saturating_sub(amount);
}

/// A queued claim that can never be released: no longer queued (an override took it over), or its
/// stake is forfeited, empty, or now points at another queued claim. A different live claim on the
/// same stake alone is NOT enough: if that one is cancelled, this one must still be able to go.
pub(crate) fn queued_claim_dead(
    claim: &Claim,
    claim_key: &Pubkey,
    stake: Option<&StakeRecord>,
) -> bool {
    if claim.status != ClaimStatus::Reserved {
        return true;
    }
    match stake {
        None => true,
        Some(r) => r.reserved_claim.as_ref() != Some(claim_key) || r.forfeited || r.amount == 0,
    }
}

/// Marks a dead queued claim Expired and frees its stake's queue slot (only if it is this claim's).
fn expire_queued(claim: &mut Claim, claim_key: Pubkey, stake: Option<&mut StakeRecord>) {
    if let Some(r) = stake {
        if r.reserved_claim == Some(claim_key) {
            r.reserved_claim = None;
        }
    }
    claim.status = ClaimStatus::Expired;
    emit!(ClaimQueueExpired { claim: claim_key });
}

/// Creates the place PDA `[seed, pool, seq]`, owned by this program, holding `value(bump)`, rent from
/// `payer`. By hand because a place exists only when needed. Like Anchor's `init`, it works even if
/// someone sent lamports to the address first.
#[allow(clippy::too_many_arguments)]
fn create_place<'info, T: AccountSerialize + Space>(
    seed: &[u8],
    pool_key: &Pubkey,
    seq: u64,
    entry: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    wrong: PoolError,
    value: impl FnOnce(u8) -> T,
) -> Result<()> {
    use anchor_lang::system_program::{allocate, assign, create_account, transfer};
    use anchor_lang::system_program::{Allocate, Assign, CreateAccount, Transfer};
    let seq_bytes = seq.to_le_bytes();
    let (address, bump) =
        Pubkey::find_program_address(&[seed, pool_key.as_ref(), &seq_bytes], &crate::ID);
    require_keys_eq!(entry.key(), address, wrong);
    let space = 8 + T::INIT_SPACE;
    let rent = Rent::get()?.minimum_balance(space);
    let signer: &[&[&[u8]]] = &[&[seed, pool_key.as_ref(), &seq_bytes, &[bump]]];
    let sys = system_program.key();
    if entry.lamports() == 0 {
        create_account(
            CpiContext::new_with_signer(
                sys,
                CreateAccount {
                    from: payer.clone(),
                    to: entry.clone(),
                },
                signer,
            ),
            rent,
            space as u64,
            &crate::ID,
        )?;
    } else {
        let short = rent.saturating_sub(entry.lamports());
        if short > 0 {
            transfer(
                CpiContext::new(
                    sys,
                    Transfer {
                        from: payer.clone(),
                        to: entry.clone(),
                    },
                ),
                short,
            )?;
        }
        allocate(
            CpiContext::new_with_signer(
                sys,
                Allocate {
                    account_to_allocate: entry.clone(),
                },
                signer,
            ),
            space as u64,
        )?;
        assign(
            CpiContext::new_with_signer(
                sys,
                Assign {
                    account_to_assign: entry.clone(),
                },
                signer,
            ),
            &crate::ID,
        )?;
    }
    let mut data = entry.try_borrow_mut_data()?;
    let mut out: &mut [u8] = &mut data;
    value(bump).try_serialize(&mut out)
}

/// Puts the claim at the back of the line (`QueueEntry` at `queue_tail`). Returns its place.
fn join_queue<'info>(
    pool: &mut Pool,
    pool_key: &Pubkey,
    entry: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    claim_key: Pubkey,
) -> Result<u64> {
    let seq = pool.queue_tail;
    let payer_key = payer.key();
    create_place(
        SEED_QUEUE,
        pool_key,
        seq,
        entry,
        payer,
        system_program,
        PoolError::WrongQueueEntry,
        |bump| QueueEntry {
            claim: claim_key,
            payer: payer_key,
            bump,
        },
    )?;
    pool.queue_tail = add(pool.queue_tail, 1).core()?;
    Ok(seq)
}

/// Puts the claim on the watch list (`WatchEntry` at `watch_tail`), so upkeep expires it when due.
fn watch_claim<'info>(
    pool: &mut Pool,
    pool_key: &Pubkey,
    entry: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    claim_key: Pubkey,
) -> Result<()> {
    let payer_key = payer.key();
    create_place(
        SEED_WATCH,
        pool_key,
        pool.watch_tail,
        entry,
        payer,
        system_program,
        PoolError::WrongWatchEntry,
        |bump| WatchEntry {
            claim: claim_key,
            payer: payer_key,
            done: false,
            bump,
        },
    )?;
    pool.watch_tail = add(pool.watch_tail, 1).core()?;
    Ok(())
}

/// The day's payout snapshot (capacity, what open claims are owed), taken at the day's first
/// collection.
fn payout_snap(pool: &mut Pool, day: i64) -> Result<(u64, u64)> {
    if pool.payout_snap_day != day {
        pool.payout_snap_day = day;
        pool.payout_snap_capacity = pool_capacity(pool)?;
        pool.payout_snap_allocated = pool.total_allocated;
    }
    Ok((pool.payout_snap_capacity, pool.payout_snap_allocated))
}

/// This claim's share of today's payout cap: pro rata to what it is still owed against everything
/// owed at the snapshot. Its own approval-time capacity is still the floor of the cap base (so
/// shrinking the pool after approval cannot slow it). Rounded up: no claim gets a zero share.
fn daily_share(pool: &mut Pool, claim: &Claim, day: i64) -> Result<u64> {
    let (snap_capacity, snap_allocated) = payout_snap(pool, day)?;
    let base = snap_capacity.max(claim.capacity_snapshot);
    let cap =
        rules::payout_cap(base, pool.total_allocated, pool.settings().payout_rates()).core()?;
    drip::share(cap, claim.entitlement - claim.streamed, snap_allocated).core()
}

// ------------------------------------------------------------------ submit

#[derive(Accounts)]
#[instruction(approval: ClaimApproval)]
pub struct SubmitClaim<'info> {
    #[account(mut)]
    pub oracle: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated, has_one = oracle @ PoolError::NotOracle)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_STAKE, pool.key().as_ref(), approval.staker.as_ref()], bump = stake_record.bump)]
    pub stake_record: Box<Account<'info, StakeRecord>>,
    /// Fresh claims start `Unused`; anything else is a duplicate (checked in the handler).
    #[account(
        init_if_needed,
        payer = oracle,
        space = 8 + Claim::INIT_SPACE,
        seeds = [SEED_CLAIM, pool.key().as_ref(), approval.staker.as_ref(), approval.tx_hash.as_ref()],
        bump,
    )]
    pub claim: Box<Account<'info, Claim>>,
    /// CHECK: the revocation record of this exact approval; must be its address and not be a record
    /// owned by this program.
    pub revoked: UncheckedAccount<'info>,
    /// CHECK: the instructions sysvar (address constraint).
    #[account(address = solana_sdk_ids::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
    /// CHECK: the back of the line, `[SEED_QUEUE, pool, queue_tail]`; created only if the claim
    /// queues (address checked then).
    #[account(mut)]
    pub queue_entry: UncheckedAccount<'info>,
    /// CHECK: the back of the watch list, `[SEED_WATCH, pool, watch_tail]`; created here (address
    /// checked then).
    #[account(mut)]
    pub watch_entry: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

/// Oracle only, with an Ed25519-signed approval right before this instruction. Check order follows
/// multichain: role, arguments, signature, stake, ceiling, hack window, duplicate, then admit or queue.
pub fn submit_claim(ctx: &mut Context<SubmitClaim>, a: ClaimApproval) -> Result<()> {
    let now = now()?;
    let pool = &mut ctx.accounts.pool;
    require!(!pool.is_paused(now), PoolError::Paused);
    require!(a.entitlement > 0, PoolError::EntitlementNotPositive);
    rules::check_tier(a.tier).core()?;

    rules::check_deadline(a.deadline, now).core()?;
    let message = approval::encode_message(&crate::ID, pool.cluster, &a);
    let hash = approval::approval_hash(&message);
    require_keys_eq!(
        ctx.accounts.revoked.key(),
        approval::revoked_address(&pool.key(), &hash),
        PoolError::WrongRevocationAccount
    );
    // Revoked = the revocation record exists (owned by this program). Lamports alone mean nothing:
    // anyone can send SOL to that address, which must not block the approval.
    require!(
        ctx.accounts.revoked.owner != &crate::ID,
        PoolError::ApprovalRevoked
    );
    approval::verify_preceding_ed25519(&ctx.accounts.instructions, &pool.oracle, &message)?;

    let stake = &mut ctx.accounts.stake_record;
    require!(!stake.forfeited, PoolError::StakeForfeited);
    require!(!stake.is_suspended(now), PoolError::StakeSuspended);
    require!(
        stake.active_claim.is_none(),
        PoolError::ClaimAlreadyActiveForStake
    );
    require!(
        stake.reserved_claim.is_none(),
        PoolError::ClaimAlreadyQueued
    );
    let ratios = stake.tier_ratios(pool.settings().tier_ratios());
    rules::check_entitlement(a.entitlement, stake.amount, a.tier, ratios).core()?;
    // The hack window counts pause time only (multichain `paused_secs_since`): the blocked total
    // has no record of when its cash-short days fell, so it cannot be read at the hack time.
    let hack_clock = pool_core::settings::claim_clock(
        now,
        pool.paused_secs_at(now),
        pool.paused_secs_at(a.hack_timestamp),
    );
    rules::check_hack_time(
        a.hack_timestamp,
        stake.staked_at,
        now,
        hack_clock,
        stake.claim_window_secs,
    )
    .core()?;

    let claim_key = ctx.accounts.claim.key();
    let claim = &mut ctx.accounts.claim;
    require!(
        claim.status == ClaimStatus::Unused,
        PoolError::ClaimAlreadyExists
    );
    claim.set_inner(Claim {
        version: ACCOUNT_VERSION,
        staker: a.staker,
        tx_hash: a.tx_hash,
        hack_timestamp: a.hack_timestamp,
        entitlement: a.entitlement,
        streamed: 0,
        stake: stake.amount,
        cooldown_ends: 0,
        vesting_ends: 0,
        capacity_snapshot: 0,
        tier: a.tier,
        status: ClaimStatus::Reserved,
        approve_deadline: 0,
        last_collected: 0,
        bump: ctx.bumps.claim,
        approve_window: 0,
        inactivity_window: 0,
        blocked_mark: pool.blocked_secs(now),
        queue_seq: 0,
        share_day: 0,
        share_left: 0,
        reserved: [0; 64],
    });

    let pool_key = pool.key();
    watch_claim(
        pool,
        &pool_key,
        &ctx.accounts.watch_entry.to_account_info(),
        &ctx.accounts.oracle.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        claim_key,
    )?;
    roll_day(pool, now);
    let fits = rules::admits(
        a.entitlement,
        pool_capacity(pool)?,
        pool.total_allocated,
        pool.day_admitted,
        pool.settings().admit_rates(),
    )
    .core()?;
    let oracle_limited = pool.day_oracle_count >= rules::oracle_daily_limit(pool.total_stakers);
    let anyone_waiting = pool.queue_head < pool.queue_tail;
    // Genuine but not admittable now, or others are already in line: join the back, never lose it.
    if !fits || oracle_limited || anyone_waiting {
        let seq = join_queue(
            pool,
            &pool_key,
            &ctx.accounts.queue_entry.to_account_info(),
            &ctx.accounts.oracle.to_account_info(),
            &ctx.accounts.system_program.to_account_info(),
            claim_key,
        )?;
        claim.queue_seq = seq;
        stake.reserved_claim = Some(claim_key);
        emit!(ClaimQueued {
            staker: a.staker,
            claim: claim_key,
            entitlement: a.entitlement,
            seq
        });
        return Ok(());
    }
    admit(pool, stake, claim, claim_key, now, true)?;
    emit!(ClaimSubmitted {
        staker: a.staker,
        claim: claim_key,
        entitlement: a.entitlement
    });
    Ok(())
}

// ------------------------------------------------------------------ permissionless transitions

#[derive(Accounts)]
pub struct ClaimTransition<'info> {
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_CLAIM, pool.key().as_ref(), claim.staker.as_ref(), claim.tx_hash.as_ref()], bump = claim.bump)]
    pub claim: Box<Account<'info, Claim>>,
    #[account(mut, seeds = [SEED_STAKE, pool.key().as_ref(), claim.staker.as_ref()], bump = stake_record.bump)]
    pub stake_record: Box<Account<'info, StakeRecord>>,
}

#[derive(Accounts)]
pub struct ReleaseQueuedClaim<'info> {
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_CLAIM, pool.key().as_ref(), claim.staker.as_ref(), claim.tx_hash.as_ref()], bump = claim.bump)]
    pub claim: Box<Account<'info, Claim>>,
    #[account(
        mut,
        close = rent_to,
        seeds = [SEED_QUEUE, pool.key().as_ref(), &claim.queue_seq.to_le_bytes()],
        bump = queue_entry.bump,
        has_one = claim @ PoolError::WrongQueueEntry,
    )]
    pub queue_entry: Box<Account<'info, QueueEntry>>,
    #[account(mut, seeds = [SEED_STAKE, pool.key().as_ref(), claim.staker.as_ref()], bump = stake_record.bump)]
    pub stake_record: Box<Account<'info, StakeRecord>>,
    /// CHECK: whoever paid the place's rent gets it back (address constraint).
    #[account(mut, address = queue_entry.payer @ PoolError::WrongQueueEntry)]
    pub rent_to: UncheckedAccount<'info>,
}

/// Admits the first claim in line if it fits now (solvency and today's stress cap, re-checked
/// against the pool now); the next claim moves up. Shared by the instruction and upkeep.
pub(crate) fn release_head(
    pool: &mut Pool,
    claim: &mut Claim,
    claim_key: Pubkey,
    stake: &mut StakeRecord,
    now: i64,
) -> Result<()> {
    require!(
        claim.status == ClaimStatus::Reserved,
        PoolError::NoSuchQueuedClaim
    );
    require!(
        claim.queue_seq == pool.queue_head,
        PoolError::NotFirstInQueue
    );
    // One claim per stake, re-asserted at release: an override may have opened another meanwhile.
    if let Some(active) = stake.active_claim {
        require_keys_eq!(active, claim_key, PoolError::WalletHasDifferentActiveClaim);
    }
    require!(
        !queued_claim_dead(claim, &claim_key, Some(stake)),
        PoolError::QueuedClaimStakeChanged
    );
    require!(!stake.is_suspended(now), PoolError::StakeSuspended);

    roll_day(pool, now);
    let fits = rules::admits(
        claim.entitlement,
        pool_capacity(pool)?,
        pool.total_allocated,
        pool.day_admitted,
        pool.settings().admit_rates(),
    )
    .core()?;
    require!(fits, PoolError::QueueReleaseNotYetEligible);
    // Never counts toward the oracle's own limit: anyone may call this.
    admit(pool, stake, claim, claim_key, now, false)?;
    pool.queue_head = add(pool.queue_head, 1).core()?;
    emit!(ClaimQueueReleased { claim: claim_key });
    Ok(())
}

/// Permissionless. Releases the first claim in line (`release_head`).
pub fn try_release_queued_claim(ctx: &mut Context<ReleaseQueuedClaim>) -> Result<()> {
    let now = now()?;
    require!(!ctx.accounts.pool.is_paused(now), PoolError::Paused);
    let claim_key = ctx.accounts.claim.key();
    let a = &mut *ctx.accounts;
    release_head(
        &mut a.pool,
        &mut a.claim,
        claim_key,
        &mut a.stake_record,
        now,
    )
}

#[derive(Accounts)]
pub struct AdvanceQueue<'info> {
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_CLAIM, pool.key().as_ref(), claim.staker.as_ref(), claim.tx_hash.as_ref()], bump = claim.bump)]
    pub claim: Box<Account<'info, Claim>>,
    #[account(
        mut,
        close = rent_to,
        seeds = [SEED_QUEUE, pool.key().as_ref(), &claim.queue_seq.to_le_bytes()],
        bump = queue_entry.bump,
        has_one = claim @ PoolError::WrongQueueEntry,
    )]
    pub queue_entry: Box<Account<'info, QueueEntry>>,
    /// The claim's stake. Needed only while the claim is still marked queued; a stake that has
    /// since been closed is left out.
    #[account(mut, seeds = [SEED_STAKE, pool.key().as_ref(), claim.staker.as_ref()], bump = stake_record.bump)]
    pub stake_record: Option<Box<Account<'info, StakeRecord>>>,
    /// CHECK: whoever paid the place's rent gets it back (address constraint).
    #[account(mut, address = queue_entry.payer @ PoolError::WrongQueueEntry)]
    pub rent_to: UncheckedAccount<'info>,
}

/// Moves the head of the line past a place whose claim no longer waits (taken over, expired, or dead
/// now, which this marks Expired), so the line never stalls behind it. Shared with upkeep.
pub(crate) fn skip_head(
    pool: &mut Pool,
    claim: &mut Claim,
    claim_key: Pubkey,
    stake: Option<&mut StakeRecord>,
) -> Result<()> {
    require!(
        claim.queue_seq == pool.queue_head,
        PoolError::NotFirstInQueue
    );
    if claim.status == ClaimStatus::Reserved {
        require!(
            queued_claim_dead(claim, &claim_key, stake.as_deref()),
            PoolError::QueueHeadLive
        );
        expire_queued(claim, claim_key, stake);
    }
    emit!(QueueAdvanced {
        seq: pool.queue_head,
        claim: claim_key
    });
    pool.queue_head = add(pool.queue_head, 1).core()?;
    Ok(())
}

/// Permissionless (`skip_head`).
pub fn advance_queue(ctx: &mut Context<AdvanceQueue>) -> Result<()> {
    require!(!ctx.accounts.pool.is_paused(now()?), PoolError::Paused);
    let claim_key = ctx.accounts.claim.key();
    let a = &mut *ctx.accounts;
    let stake = a.stake_record.as_deref_mut().map(|r| &mut **r);
    skip_head(&mut a.pool, &mut a.claim, claim_key, stake)
}

/// Permissionless, any place in line. A queued claim never expires on time; this clears one only
/// once it can never be released, freeing its stake. Its place is dropped when the head reaches it.
pub fn expire_queued_claim(ctx: &mut Context<ClaimTransition>) -> Result<()> {
    let now = now()?;
    require!(!ctx.accounts.pool.is_paused(now), PoolError::Paused);
    let claim_key = ctx.accounts.claim.key();
    let claim = &mut ctx.accounts.claim;
    require!(
        claim.status == ClaimStatus::Reserved,
        PoolError::NoSuchQueuedClaim
    );
    let stake = &mut ctx.accounts.stake_record;
    require!(
        queued_claim_dead(claim, &claim_key, Some(stake)),
        PoolError::QueueNotYetExpired
    );
    expire_queued(claim, claim_key, Some(stake));
    Ok(())
}

/// Time gate met: the claim waits for the staker's approval, its window counted from the gate.
pub fn unlock_pending_claim(ctx: &mut Context<ClaimTransition>) -> Result<()> {
    let now = now()?;
    require!(!ctx.accounts.pool.is_paused(now), PoolError::Paused);
    let claim = &mut ctx.accounts.claim;
    require!(
        claim.status == ClaimStatus::PendingTime,
        PoolError::ClaimNotPending
    );
    require!(
        promote_if_due(claim, &ctx.accounts.stake_record, now)?,
        PoolError::TimeGateNotMet
    );
    emit!(ClaimUnlocked { claim: claim.key() });
    Ok(())
}

/// The staker did not approve in time (on the claim clock): the reservation returns to the pool. A
/// pending claim past its gate counts as awaiting approval. Never while suspended (the staker cannot
/// approve then). Shared with upkeep.
pub(crate) fn expire_unapproved(
    pool: &mut Pool,
    claim: &mut Claim,
    claim_key: Pubkey,
    stake: &mut StakeRecord,
    now: i64,
) -> Result<()> {
    promote_if_due(claim, stake, now)?;
    require!(
        claim.status == ClaimStatus::AwaitingApproval,
        PoolError::ClaimNotAwaitingApproval
    );
    require!(!stake.is_suspended(now), PoolError::StakeSuspended);
    require!(
        approve_window_over(pool, claim, stake, now),
        PoolError::ApprovalWindowNotExpired
    );
    release_allocation(pool, claim.entitlement);
    stake.active_claim = None;
    claim.status = ClaimStatus::Expired;
    emit!(ClaimExpired {
        claim: claim_key,
        released: claim.entitlement
    });
    Ok(())
}

/// Permissionless (`expire_unapproved`). Never while paused.
pub fn expire_pending_approval(ctx: &mut Context<ClaimTransition>) -> Result<()> {
    let now = now()?;
    require!(!ctx.accounts.pool.is_paused(now), PoolError::Paused);
    let claim_key = ctx.accounts.claim.key();
    let a = &mut *ctx.accounts;
    expire_unapproved(
        &mut a.pool,
        &mut a.claim,
        claim_key,
        &mut a.stake_record,
        now,
    )
}

/// An approved claim left uncollected too long (on the claim clock): the unpaid rest returns to the
/// pool. Never while suspended (the staker cannot collect then). Shared with upkeep.
pub(crate) fn expire_stale(
    pool: &mut Pool,
    claim: &mut Claim,
    claim_key: Pubkey,
    stake: &mut StakeRecord,
    now: i64,
) -> Result<()> {
    require!(
        claim.status == ClaimStatus::Active,
        PoolError::ClaimNotActive
    );
    require!(
        claim.streamed < claim.entitlement,
        PoolError::ClaimFullyStreamed
    );
    require!(!stake.is_suspended(now), PoolError::StakeSuspended);
    require!(
        collection_window_over(pool, claim, stake, now),
        PoolError::ClaimNotStale
    );
    let remaining = claim.entitlement - claim.streamed;
    release_allocation(pool, remaining);
    stake.active_claim = None;
    claim.status = ClaimStatus::Expired;
    emit!(ClaimExpired {
        claim: claim_key,
        released: remaining
    });
    Ok(())
}

/// Permissionless (`expire_stale`). Never while paused.
pub fn expire_stale_claim(ctx: &mut Context<ClaimTransition>) -> Result<()> {
    let now = now()?;
    require!(!ctx.accounts.pool.is_paused(now), PoolError::Paused);
    let claim_key = ctx.accounts.claim.key();
    let a = &mut *ctx.accounts;
    expire_stale(
        &mut a.pool,
        &mut a.claim,
        claim_key,
        &mut a.stake_record,
        now,
    )
}

// ------------------------------------------------------------------ staker actions

#[derive(Accounts)]
pub struct ApproveClaim<'info> {
    pub staker: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_CLAIM, pool.key().as_ref(), staker.key().as_ref(), claim.tx_hash.as_ref()], bump = claim.bump, has_one = staker)]
    pub claim: Box<Account<'info, Claim>>,
    #[account(mut, seeds = [SEED_STAKE, pool.key().as_ref(), staker.key().as_ref()], bump = stake_record.bump)]
    pub stake_record: Box<Account<'info, StakeRecord>>,
}

/// The staker's decision: forfeit the stake, start cooldown and vesting.
pub fn approve_claim(ctx: &mut Context<ApproveClaim>) -> Result<()> {
    let now = now()?;
    let pool = &mut ctx.accounts.pool;
    require!(!pool.is_paused(now), PoolError::Paused);
    let claim_key = ctx.accounts.claim.key();
    let claim = &mut ctx.accounts.claim;
    let stake = &mut ctx.accounts.stake_record;
    promote_if_due(claim, stake, now)?;
    require!(
        claim.status == ClaimStatus::AwaitingApproval,
        PoolError::ClaimNotAwaitingApproval
    );
    require!(
        !approve_window_over(pool, claim, stake, now),
        PoolError::ApprovalWindowExpired
    );
    require!(!stake.is_suspended(now), PoolError::StakeSuspended);
    require!(
        !stake.forfeited && stake.active_claim == Some(claim_key),
        PoolError::ClaimStakeMismatch
    );
    let points_burned = activate(pool, stake, claim, claim_key, now)?;
    emit!(ClaimApproved {
        claim: claim_key,
        points_burned
    });
    Ok(())
}

#[derive(Accounts)]
pub struct ClaimStream<'info> {
    pub staker: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [SEED_CLAIM, pool.key().as_ref(), staker.key().as_ref(), claim.tx_hash.as_ref()], bump = claim.bump, has_one = staker)]
    pub claim: Box<Account<'info, Claim>>,
    #[account(
        seeds = [SEED_STAKE, pool.key().as_ref(), staker.key().as_ref()],
        bump = stake_record.bump,
    )]
    pub stake_record: Box<Account<'info, StakeRecord>>,
    /// The stake's beneficiary's USDC account (any token account it owns).
    #[account(
        mut,
        constraint = beneficiary_usdc.mint == pool.usdc_mint @ PoolError::WrongMint,
        constraint = beneficiary_usdc.owner == stake_record.beneficiary @ PoolError::WrongBeneficiary,
    )]
    pub beneficiary_usdc: Box<Account<'info, TokenAccount>>,
    pub cash: Cash<'info>,
}

/// Pays what has vested, up to today's payout cap, to the beneficiary. Pulls from the venue first; if
/// the pool still cannot pay, nothing is paid and today counts as a cash-short day (rule 18): it
/// is blocked time, so it extends the claim's windows, and the collection window restarts
/// (multichain `claim_stream`). The staker tries again later.
pub fn claim_stream(ctx: &mut Context<ClaimStream>) -> Result<()> {
    let now = now()?;
    let mut money = Money::new(&ctx.accounts.pool, &ctx.accounts.vault, &ctx.accounts.cash)?;
    let pool = &mut ctx.accounts.pool;
    require!(!pool.is_paused(now), PoolError::Paused);
    let claim_key = ctx.accounts.claim.key();
    let claim = &mut ctx.accounts.claim;
    require!(
        claim.status == ClaimStatus::Active,
        PoolError::ClaimNotActive
    );
    require!(now >= claim.cooldown_ends, PoolError::CooldownNotPassed);
    require!(
        claim.streamed < claim.entitlement,
        PoolError::ClaimFullyStreamed
    );
    require!(
        !ctx.accounts.stake_record.is_suspended(now),
        PoolError::StakeSuspended
    );

    let vested = rules::vested(
        claim.entitlement,
        claim.cooldown_ends,
        claim.vesting_ends,
        now,
    )
    .core()?;
    let claimable = vested.saturating_sub(claim.streamed);
    require!(claimable > 0, PoolError::NothingVested);

    roll_day(pool, now);
    let day = rules::day_of(now);
    if claim.share_day != day {
        claim.share_left = daily_share(pool, claim, day)?;
        claim.share_day = day;
    }
    let amount = claimable.min(claim.share_left);
    require!(amount > 0, PoolError::DailyOutflowCapReached);

    money.pull_cash(pool, amount, now)?;
    if !vault::can_pay(pool, &money.vault, amount)? {
        let days_total = pool.mark_cash_short_day(now);
        restart_collection_window(pool, claim, now);
        emit!(CashShortDay {
            claim: claim_key,
            day,
            days_total
        });
        return Ok(());
    }

    claim.streamed += amount;
    claim.share_left -= amount;
    restart_collection_window(pool, claim, now);
    if claim.streamed >= claim.entitlement {
        claim.status = ClaimStatus::Completed;
    }
    release_allocation(pool, amount);
    pool.day_outflow = add(pool.day_outflow, amount).core()?;
    emit!(ClaimStreamed {
        claim: claim_key,
        amount
    });

    let to = ctx.accounts.beneficiary_usdc.to_account_info();
    vault::pay_out(
        pool,
        &money.pool_info,
        money.cash,
        &money.vault,
        &to,
        amount,
    )
}

// ------------------------------------------------------------------ admin: cancel, suspend, revoke

#[derive(Accounts)]
pub struct CancelClaim<'info> {
    pub admin: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated, has_one = admin @ PoolError::NotAdmin)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_CLAIM, pool.key().as_ref(), claim.staker.as_ref(), claim.tx_hash.as_ref()], bump = claim.bump)]
    pub claim: Box<Account<'info, Claim>>,
    #[account(mut, seeds = [SEED_STAKE, pool.key().as_ref(), claim.staker.as_ref()], bump = stake_record.bump)]
    pub stake_record: Box<Account<'info, StakeRecord>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    pub cash: Cash<'info>,
}

/// False-positive reversal. An approved (forfeited) stake is restored under the penalty lock, less
/// what the claim already paid out; before approval nothing was forfeited, so there is no penalty.
pub fn cancel_claim(ctx: Context<CancelClaim>) -> Result<()> {
    let now = now()?;
    let mut money = Money::new(&ctx.accounts.pool, &ctx.accounts.vault, &ctx.accounts.cash)?;
    let pool = &mut ctx.accounts.pool;
    // Growth so far belongs to the stakers counted until now, before the restored stake joins them.
    money.harvest(pool, now)?;
    let claim_key = ctx.accounts.claim.key();
    let claim = &mut ctx.accounts.claim;
    require!(
        matches!(
            claim.status,
            ClaimStatus::Active | ClaimStatus::PendingTime | ClaimStatus::AwaitingApproval
        ),
        PoolError::ClaimNotCancellable
    );
    release_allocation(pool, claim.entitlement - claim.streamed);
    let stake = &mut ctx.accounts.stake_record;
    if claim.status == ClaimStatus::Active {
        // What the claim already paid out stays paid and comes off the stake. The
        // rest may sit below the min stake; it can still leave in full.
        let restored = claim.stake.saturating_sub(claim.streamed);
        stake.forfeited_by = None;
        if restored > 0 {
            stake.forfeited = false;
            stake.amount = restored;
            stake.penalty_locked_until = secs_after(now, PENALTY_LOCK_SECS)?;
            // Out of total_staked while forfeited, so it earns again only from now.
            stake.yield_index_at = pool.staker_yield_index;
            pool.total_staked = add(pool.total_staked, restored).core()?;
            pool.total_stakers = add(pool.total_stakers, 1).core()?;
        }
        // Paid in full: the stake stays forfeited, as on a completed claim, and is earmarked for
        // no claim, so an override cannot count it as capacity again.
    }
    stake.active_claim = None;
    claim.status = ClaimStatus::Cancelled;
    emit!(ClaimCancelled { claim: claim_key });
    Ok(())
}

#[derive(Accounts)]
#[instruction(staker: Pubkey)]
pub struct AdminStake<'info> {
    pub admin: Signer<'info>,
    #[account(seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated, has_one = admin @ PoolError::NotAdmin)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_STAKE, pool.key().as_ref(), staker.as_ref()], bump = stake_record.bump)]
    pub stake_record: Box<Account<'info, StakeRecord>>,
    /// Optional: a claim of this staker whose approve / collection clock restarts on unsuspend.
    #[account(mut, seeds = [SEED_CLAIM, pool.key().as_ref(), staker.as_ref(), claim.tx_hash.as_ref()], bump = claim.bump)]
    pub claim: Option<Box<Account<'info, Claim>>>,
}

/// A stake that can still be suspended: not forfeited, unless its claim is still open.
fn suspendable(stake: &StakeRecord) -> Result<()> {
    require!(
        !stake.forfeited || stake.active_claim.is_some(),
        PoolError::StakeForfeited
    );
    Ok(())
}

/// The admin alone holds payouts on a stake for `SUSPEND_MAX_SECS`, then the hold lapses. Again
/// while one is in force, or within the same length after it ends, needs a second role
/// (`renew_suspension`). Does not block principal withdrawal.
pub fn suspend_stake(ctx: Context<AdminStake>, _staker: Pubkey) -> Result<()> {
    let now = now()?;
    let stake = &mut ctx.accounts.stake_record;
    suspendable(stake)?;
    require!(
        stake.suspended_until == 0 || now >= stake.suspended_until.saturating_add(SUSPEND_MAX_SECS),
        PoolError::SuspendNeedsSecondRole
    );
    stake.suspended = true;
    stake.suspended_until = secs_after(now, SUSPEND_MAX_SECS)?;
    emit!(StakeSuspended {
        staker: stake.staker,
        until: stake.suspended_until
    });
    Ok(())
}

#[derive(Accounts)]
#[instruction(staker: Pubkey)]
pub struct RenewSuspension<'info> {
    pub admin: Signer<'info>,
    /// The co-signer or the guardian.
    pub approver: Signer<'info>,
    #[account(seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated, has_one = admin @ PoolError::NotAdmin)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_STAKE, pool.key().as_ref(), staker.as_ref()], bump = stake_record.bump)]
    pub stake_record: Box<Account<'info, StakeRecord>>,
}

/// The admin plus the co-signer or guardian start or extend a suspension: it runs
/// `SUSPEND_MAX_SECS` from now.
pub fn renew_suspension(ctx: Context<RenewSuspension>, _staker: Pubkey) -> Result<()> {
    let now = now()?;
    let pool = &ctx.accounts.pool;
    let approver = ctx.accounts.approver.key();
    require!(
        approver == pool.co_signer || approver == pool.guardian,
        PoolError::ApproverNotSecondRole
    );
    let stake = &mut ctx.accounts.stake_record;
    suspendable(stake)?;
    stake.suspended = true;
    stake.suspended_until = secs_after(now, SUSPEND_MAX_SECS)?;
    emit!(SuspensionRenewed {
        staker: stake.staker,
        approver,
        until: stake.suspended_until
    });
    Ok(())
}

/// Lifts the hold and gives the staker a fresh approve / collection window, so a suspension can
/// never cost them their claim.
pub fn unsuspend_stake(ctx: Context<AdminStake>, _staker: Pubkey) -> Result<()> {
    let now = now()?;
    let stake = &mut ctx.accounts.stake_record;
    if stake.is_suspended(now) {
        // Ends now; claim windows never start before it.
        stake.suspended_until = now;
    }
    stake.suspended = false;
    emit!(StakeUnsuspended {
        staker: stake.staker
    });
    let pool = &ctx.accounts.pool;
    if let Some(claim) = ctx.accounts.claim.as_mut() {
        promote_if_due(claim, stake, now)?;
        match claim.status {
            ClaimStatus::AwaitingApproval => open_approve_window(pool, claim, now)?,
            ClaimStatus::Active => restart_collection_window(pool, claim, now),
            _ => {}
        }
    }
    Ok(())
}

#[derive(Accounts)]
#[instruction(approval: ClaimApproval, hash: [u8; 32])]
pub struct RevokeApproval<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated, has_one = admin @ PoolError::NotAdmin)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(
        init,
        payer = admin,
        space = 8 + RevokedApproval::INIT_SPACE,
        seeds = [crate::constants::SEED_REVOKED, pool.key().as_ref(), hash.as_ref()],
        bump,
    )]
    pub revoked: Box<Account<'info, RevokedApproval>>,
    pub system_program: Program<'info, System>,
}

/// Cancels a signed, not yet submitted approval. `hash` must be the approval's own hash (rebuilt
/// here), so a garbage hash can never be revoked. Expired approvals are refused: they are dead already.
pub fn revoke_approval(
    ctx: Context<RevokeApproval>,
    a: ClaimApproval,
    hash: [u8; 32],
) -> Result<()> {
    rules::check_deadline(a.deadline, now()?).core()?;
    let message = approval::encode_message(&crate::ID, ctx.accounts.pool.cluster, &a);
    require!(
        approval::approval_hash(&message) == hash,
        PoolError::WrongRevocationAccount
    );
    ctx.accounts.revoked.set_inner(RevokedApproval {
        hash,
        bump: ctx.bumps.revoked,
    });
    emit!(ApprovalRevoked {
        staker: a.staker,
        hash
    });
    Ok(())
}

// ------------------------------------------------------------------ 2-of-2 override

#[derive(Accounts)]
#[instruction(staker: Pubkey, tx_hash: [u8; 32])]
pub struct ApproveOverride<'info> {
    #[account(mut)]
    pub signer: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_STAKE, pool.key().as_ref(), staker.as_ref()], bump = stake_record.bump)]
    pub stake_record: Box<Account<'info, StakeRecord>>,
    #[account(
        init_if_needed,
        payer = signer,
        space = 8 + Claim::INIT_SPACE,
        seeds = [SEED_CLAIM, pool.key().as_ref(), staker.as_ref(), tx_hash.as_ref()],
        bump,
    )]
    pub claim: Box<Account<'info, Claim>>,
    #[account(
        init_if_needed,
        payer = signer,
        space = 8 + OverrideRequest::INIT_SPACE,
        seeds = [SEED_OVERRIDE, claim.key().as_ref()],
        bump,
    )]
    pub override_request: Box<Account<'info, OverrideRequest>>,
    /// CHECK: the back of the watch list, `[SEED_WATCH, pool, watch_tail]`; created when the override
    /// executes (address checked then).
    #[account(mut)]
    pub watch_entry: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

/// Admin and co-signer each approve identical terms; the second approval executes. Bypasses the
/// oracle, the time gate and the staker's approval: the two signatures are the gate.
pub fn approve_override(
    ctx: Context<ApproveOverride>,
    staker: Pubkey,
    tx_hash: [u8; 32],
    entitlement: u64,
    tier: u8,
) -> Result<()> {
    let now = now()?;
    let signer = ctx.accounts.signer.key();
    let (admin, co_signer) = (ctx.accounts.pool.admin, ctx.accounts.pool.co_signer);
    require!(
        signer == admin || signer == co_signer,
        PoolError::NotAdminOrCoSigner
    );
    require!(!ctx.accounts.pool.is_paused(now), PoolError::Paused);
    require!(entitlement > 0, PoolError::EntitlementNotPositive);
    rules::check_tier(tier).core()?;

    let claim_key = ctx.accounts.claim.key();
    let req = &mut ctx.accounts.override_request;
    if req.claim == Pubkey::default() {
        req.set_inner(OverrideRequest {
            claim: claim_key,
            entitlement,
            tier,
            admin_approver: None,
            co_signer_approver: None,
            bump: ctx.bumps.override_request,
        });
    }
    require!(
        req.entitlement == entitlement && req.tier == tier,
        PoolError::OverrideParamsMismatch
    );
    if signer == admin {
        req.admin_approver = Some(admin);
    }
    if signer == co_signer {
        req.co_signer_approver = Some(co_signer);
    }
    emit!(OverrideApproved {
        claim: claim_key,
        approver: signer,
        entitlement,
        tier
    });
    // Ready only while both approvals match the current roles.
    if req.admin_approver != Some(admin) || req.co_signer_approver != Some(co_signer) {
        return Ok(());
    }

    execute_override(
        &mut ctx.accounts.pool,
        &mut ctx.accounts.stake_record,
        &mut ctx.accounts.claim,
        claim_key,
        staker,
        tx_hash,
        entitlement,
        tier,
        ctx.bumps.claim,
        now,
    )?;
    let pool_key = ctx.accounts.pool.key();
    watch_claim(
        &mut ctx.accounts.pool,
        &pool_key,
        &ctx.accounts.watch_entry.to_account_info(),
        &ctx.accounts.signer.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        claim_key,
    )?;
    emit!(OverrideExecuted { claim: claim_key });
    // Deleted on execution, so cancel_pending_override finds nothing afterwards.
    ctx.accounts
        .override_request
        .close(ctx.accounts.signer.to_account_info())
}

#[allow(clippy::too_many_arguments)]
fn execute_override(
    pool: &mut Pool,
    stake: &mut StakeRecord,
    claim: &mut Claim,
    claim_key: Pubkey,
    staker: Pubkey,
    tx_hash: [u8; 32],
    entitlement: u64,
    tier: u8,
    bump: u8,
    now: i64,
) -> Result<()> {
    // A correction of a live claim releases its reservation first and carries what it already paid.
    let mut carried_streamed = 0;
    // A queued claim taken over keeps its place number, so its place can still be found and dropped.
    let queue_seq = claim.queue_seq;
    match claim.status {
        ClaimStatus::Completed => return err!(PoolError::ClaimAlreadyCompleted),
        ClaimStatus::Active | ClaimStatus::PendingTime | ClaimStatus::AwaitingApproval => {
            release_allocation(pool, claim.entitlement - claim.streamed);
            if claim.status == ClaimStatus::Active {
                carried_streamed = claim.streamed;
            }
        }
        _ => {}
    }
    if let Some(active) = stake.active_claim {
        require_keys_eq!(active, claim_key, PoolError::WalletHasDifferentActiveClaim);
    }
    // A forfeited stake is earmarked for the claim that forfeited it: only that claim may be re-executed
    // on it. Any other claim would count the same principal as capacity a second time.
    if stake.forfeited {
        require!(
            stake.forfeited_by == Some(claim_key),
            PoolError::StakeForfeited
        );
    }
    let ratios = stake.tier_ratios(pool.settings().tier_ratios());
    rules::check_entitlement(entitlement, stake.amount, tier, ratios).core()?;

    // An already-forfeited stake (re-execution of this same claim) is earmarked for it, not new capacity.
    let mut effective = pool_capacity(pool)?;
    if stake.forfeited {
        effective = add(effective, stake.amount).core()?;
    }
    require!(
        add(pool.total_allocated, entitlement).core()? <= effective,
        PoolError::Insolvent
    );
    pool.total_allocated = add(pool.total_allocated, entitlement).core()?;

    *claim = Claim {
        version: ACCOUNT_VERSION,
        staker,
        tx_hash,
        hack_timestamp: now,
        entitlement,
        streamed: carried_streamed,
        stake: stake.amount,
        cooldown_ends: 0,
        vesting_ends: 0,
        capacity_snapshot: 0,
        tier,
        status: ClaimStatus::Active,
        approve_deadline: 0,
        last_collected: 0,
        bump,
        approve_window: pool.settings().get(SettingKey::ApproveWindowSecs),
        inactivity_window: 0,
        blocked_mark: 0,
        queue_seq,
        share_day: 0,
        share_left: 0,
        reserved: [0; 64],
    };
    if stake.forfeited {
        start_clocks(pool, claim, now)?;
    } else {
        activate(pool, stake, claim, claim_key, now)?;
    }
    stake.active_claim = Some(claim_key);
    if stake.reserved_claim == Some(claim_key) {
        stake.reserved_claim = None;
    }
    Ok(())
}

#[derive(Accounts)]
pub struct CancelPendingOverride<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated, has_one = admin @ PoolError::NotAdmin)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, close = admin, seeds = [SEED_OVERRIDE, override_request.claim.as_ref()], bump = override_request.bump)]
    pub override_request: Box<Account<'info, OverrideRequest>>,
}

/// Admin only (as V8). Deletes the pending request.
pub fn cancel_pending_override(ctx: Context<CancelPendingOverride>) -> Result<()> {
    emit!(OverrideCancelled {
        claim: ctx.accounts.override_request.claim
    });
    Ok(())
}
