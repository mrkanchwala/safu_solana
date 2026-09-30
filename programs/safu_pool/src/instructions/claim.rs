//! Claims (multichain `claim.rs`): submit → (queue) → time gate → staker approval (forfeits the
//! stake) → cooldown → vesting stream under a daily payout cap → completed. Plus cancel (false
//! positive), the 2-of-2 override, approval revocation, and the permissionless expiry sweeps.
//!
//! The claim account address `[SEED_CLAIM, pool, staker, tx_hash]` is the claim id: one claim per
//! (staker, drain transaction), forever.

use anchor_lang::prelude::*;
use pool_core::claim as rules;
use pool_core::params::{APPROVE_WINDOW_SECS, COLLECTION_INACTIVITY_SECS, COOLDOWN_SECS, PENALTY_LOCK_SECS, VESTING_SECS};
use pool_core::{add, capacity, yields};

use super::now;
use crate::approval::{self, ClaimApproval};
use crate::constants::{SEED_CLAIM, SEED_OVERRIDE, SEED_POOL, SEED_STAKE, SEED_VAULT};
use crate::errors::{CoreResultExt, PoolError};
use crate::events::*;
use crate::state::{Claim, ClaimStatus, OverrideRequest, Pool, RevokedApproval, StakeRecord};
use crate::leg;
// Glob: `#[derive(Accounts)]` on a struct holding `MarinadeLeg` needs its generated client modules.
use crate::marinade::*;

// ------------------------------------------------------------------ shared steps

fn secs_after(t: i64, secs: i64) -> Result<i64> {
    t.checked_add(secs).ok_or_else(|| error!(PoolError::MathOverflow))
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
fn admit(pool: &mut Pool, stake: &mut StakeRecord, claim: &mut Claim, claim_key: Pubkey, now: i64, count_toward_oracle_limit: bool) -> Result<()> {
    pool.day_admitted = add(pool.day_admitted, claim.entitlement).core()?;
    if count_toward_oracle_limit {
        pool.day_oracle_count = add(pool.day_oracle_count, 1).core()?;
    }
    pool.total_allocated = add(pool.total_allocated, claim.entitlement).core()?;
    stake.active_claim = Some(claim_key);
    stake.reserved_claim = None;
    claim.stake = stake.amount;
    if rules::time_gate_met(stake.staked_at, now) {
        claim.status = ClaimStatus::AwaitingApproval;
        claim.approve_deadline = secs_after(now, APPROVE_WINDOW_SECS)?;
    } else {
        claim.status = ClaimStatus::PendingTime;
    }
    Ok(())
}

/// Moves a stake's unpaid yield to the protocol share (a forfeited stake and its yield go to the pool).
fn forfeit_staker_yield(pool: &mut Pool, stake: &mut StakeRecord) -> Result<()> {
    if !stake.forfeited {
        let owed = yields::owed(stake.amount, pool.staker_yield_index, stake.yield_index_at).core()?;
        let moved = owed.min(pool.staker_yield_reserved);
        pool.staker_yield_reserved -= moved;
        pool.protocol_yield_balance = add(pool.protocol_yield_balance, moved).core()?;
    }
    stake.yield_index_at = pool.staker_yield_index;
    Ok(())
}

/// Starts the payout clocks from now, with the capacity snapshot taken first.
fn start_clocks(pool: &Pool, claim: &mut Claim, now: i64) -> Result<()> {
    claim.capacity_snapshot = pool_capacity(pool)?;
    claim.cooldown_ends = secs_after(now, COOLDOWN_SECS)?;
    claim.vesting_ends = secs_after(claim.cooldown_ends, VESTING_SECS)?;
    // The inactivity clock starts at cooldown end: the cooldown is never the staker's inactivity.
    claim.last_collected = claim.cooldown_ends;
    claim.status = ClaimStatus::Active;
    Ok(())
}

/// Approval: forfeits the stake (and its unpaid yield) and starts cooldown + vesting.
fn activate(pool: &mut Pool, stake: &mut StakeRecord, claim: &mut Claim, now: i64) -> Result<()> {
    forfeit_staker_yield(pool, stake)?;
    stake.forfeited = true;
    start_clocks(pool, claim, now)?;
    claim.stake = stake.amount;
    pool.total_staked = pool.total_staked.saturating_sub(stake.amount);
    pool.total_stakers = pool.total_stakers.saturating_sub(1);
    Ok(())
}

fn release_allocation(pool: &mut Pool, amount: u64) {
    pool.total_allocated = pool.total_allocated.saturating_sub(amount);
}

// ------------------------------------------------------------------ submit

#[derive(Accounts)]
#[instruction(approval: ClaimApproval)]
pub struct SubmitClaim<'info> {
    #[account(mut)]
    pub oracle: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, has_one = oracle @ PoolError::NotOracle)]
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
    /// CHECK: the revocation record of this exact approval; must be its address and must not exist.
    pub revoked: UncheckedAccount<'info>,
    /// CHECK: the instructions sysvar (address constraint).
    #[account(address = solana_sdk_ids::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

/// Oracle only, with an Ed25519-signed approval right before this instruction. Check order follows
/// multichain: role, arguments, signature, stake, ceiling, hack window, duplicate, then admit or queue.
pub fn submit_claim(ctx: Context<SubmitClaim>, a: ClaimApproval) -> Result<()> {
    let now = now()?;
    let pool = &mut ctx.accounts.pool;
    require!(!pool.is_paused(now), PoolError::Paused);
    require!(a.entitlement > 0, PoolError::EntitlementNotPositive);
    rules::tier_ratio(a.tier).core()?;

    rules::check_deadline(a.deadline, now).core()?;
    let message = approval::encode_message(&crate::ID, pool.cluster, &a);
    let hash = approval::approval_hash(&message);
    require_keys_eq!(ctx.accounts.revoked.key(), approval::revoked_address(&pool.key(), &hash), PoolError::WrongRevocationAccount);
    require!(ctx.accounts.revoked.lamports() == 0, PoolError::ApprovalRevoked);
    approval::verify_preceding_ed25519(&ctx.accounts.instructions, &pool.oracle, &message)?;

    let stake = &mut ctx.accounts.stake_record;
    require!(!stake.forfeited, PoolError::StakeForfeited);
    require!(!stake.suspended, PoolError::StakeSuspended);
    require!(stake.active_claim.is_none(), PoolError::ClaimAlreadyActiveForStake);
    require!(stake.reserved_claim.is_none(), PoolError::ClaimAlreadyQueued);
    rules::check_entitlement(a.entitlement, stake.amount, a.tier).core()?;
    rules::check_hack_time(a.hack_timestamp, stake.staked_at, now).core()?;

    let claim_key = ctx.accounts.claim.key();
    let claim = &mut ctx.accounts.claim;
    require!(claim.status == ClaimStatus::Unused, PoolError::ClaimAlreadyExists);
    claim.set_inner(Claim {
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
    });

    roll_day(pool, now);
    let fits = rules::admits(a.entitlement, pool_capacity(pool)?, pool.total_allocated, pool.day_admitted).core()?;
    let oracle_limited = pool.day_oracle_count >= rules::oracle_daily_limit(pool.total_stakers);
    // Genuine but not admittable now: queue, never lose it. Released later by anyone.
    if !fits || oracle_limited {
        stake.reserved_claim = Some(claim_key);
        emit!(ClaimQueued { staker: a.staker, claim: claim_key, entitlement: a.entitlement });
        return Ok(());
    }
    admit(pool, stake, claim, claim_key, now, true)?;
    emit!(ClaimSubmitted { staker: a.staker, claim: claim_key, entitlement: a.entitlement });
    Ok(())
}

// ------------------------------------------------------------------ permissionless transitions

#[derive(Accounts)]
pub struct ClaimTransition<'info> {
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_CLAIM, pool.key().as_ref(), claim.staker.as_ref(), claim.tx_hash.as_ref()], bump = claim.bump)]
    pub claim: Box<Account<'info, Claim>>,
    #[account(mut, seeds = [SEED_STAKE, pool.key().as_ref(), claim.staker.as_ref()], bump = stake_record.bump)]
    pub stake_record: Box<Account<'info, StakeRecord>>,
}

/// Re-checks a queued claim against the pool now; admits it if it fits.
pub fn try_release_queued_claim(ctx: Context<ClaimTransition>) -> Result<()> {
    let now = now()?;
    let pool = &mut ctx.accounts.pool;
    require!(!pool.is_paused(now), PoolError::Paused);
    let claim_key = ctx.accounts.claim.key();
    let claim = &mut ctx.accounts.claim;
    require!(claim.status == ClaimStatus::Reserved, PoolError::NoSuchQueuedClaim);
    let stake = &mut ctx.accounts.stake_record;
    // One claim per stake, re-asserted at release: an override may have opened another meanwhile.
    if let Some(active) = stake.active_claim {
        require_keys_eq!(active, claim_key, PoolError::WalletHasDifferentActiveClaim);
    }
    require!(stake.reserved_claim == Some(claim_key) && !stake.forfeited, PoolError::QueuedClaimStakeChanged);
    require!(!stake.suspended, PoolError::StakeSuspended);

    roll_day(pool, now);
    let fits = rules::admits(claim.entitlement, pool_capacity(pool)?, pool.total_allocated, pool.day_admitted).core()?;
    require!(fits, PoolError::QueueReleaseNotYetEligible);
    // Never counts toward the oracle's own limit: anyone may call this.
    admit(pool, stake, claim, claim_key, now, false)?;
    emit!(ClaimQueueReleased { claim: claim_key });
    Ok(())
}

/// A queued claim whose claim window ran out. Frees the stake's queue slot; nothing was allocated.
pub fn expire_queued_claim(ctx: Context<ClaimTransition>) -> Result<()> {
    let now = now()?;
    require!(!ctx.accounts.pool.is_paused(now), PoolError::Paused);
    let claim_key = ctx.accounts.claim.key();
    let claim = &mut ctx.accounts.claim;
    require!(claim.status == ClaimStatus::Reserved, PoolError::NoSuchQueuedClaim);
    require!(now > secs_after(claim.hack_timestamp, pool_core::params::CLAIM_WINDOW_SECS)?, PoolError::QueueNotYetExpired);
    let stake = &mut ctx.accounts.stake_record;
    if stake.reserved_claim == Some(claim_key) {
        stake.reserved_claim = None;
    }
    claim.status = ClaimStatus::Expired;
    emit!(ClaimQueueExpired { claim: claim_key });
    Ok(())
}

/// Time gate met: the claim waits for the staker's approval.
pub fn unlock_pending_claim(ctx: Context<ClaimTransition>) -> Result<()> {
    let now = now()?;
    require!(!ctx.accounts.pool.is_paused(now), PoolError::Paused);
    let claim = &mut ctx.accounts.claim;
    require!(claim.status == ClaimStatus::PendingTime, PoolError::ClaimNotPending);
    require!(rules::time_gate_met(ctx.accounts.stake_record.staked_at, now), PoolError::TimeGateNotMet);
    claim.status = ClaimStatus::AwaitingApproval;
    claim.approve_deadline = secs_after(now, APPROVE_WINDOW_SECS)?;
    emit!(ClaimUnlocked { claim: claim.key() });
    Ok(())
}

/// The staker did not approve in time: the reservation returns to the pool. Never while suspended.
pub fn expire_pending_approval(ctx: Context<ClaimTransition>) -> Result<()> {
    let now = now()?;
    let claim_key = ctx.accounts.claim.key();
    let claim = &mut ctx.accounts.claim;
    require!(claim.status == ClaimStatus::AwaitingApproval, PoolError::ClaimNotAwaitingApproval);
    let stake = &mut ctx.accounts.stake_record;
    require!(!stake.suspended, PoolError::StakeSuspended);
    require!(now > claim.approve_deadline, PoolError::ApprovalWindowNotExpired);
    release_allocation(&mut ctx.accounts.pool, claim.entitlement);
    stake.active_claim = None;
    claim.status = ClaimStatus::Expired;
    emit!(ClaimExpired { claim: claim_key, released: claim.entitlement });
    Ok(())
}

/// An approved claim left uncollected too long: the unpaid rest returns to the pool.
pub fn expire_stale_claim(ctx: Context<ClaimTransition>) -> Result<()> {
    let now = now()?;
    let claim_key = ctx.accounts.claim.key();
    let claim = &mut ctx.accounts.claim;
    require!(claim.status == ClaimStatus::Active, PoolError::ClaimNotActive);
    require!(claim.streamed < claim.entitlement, PoolError::ClaimFullyStreamed);
    let stake = &mut ctx.accounts.stake_record;
    require!(!stake.suspended, PoolError::StakeSuspended);
    require!(now.saturating_sub(claim.last_collected) > COLLECTION_INACTIVITY_SECS, PoolError::ClaimNotStale);
    let remaining = claim.entitlement - claim.streamed;
    release_allocation(&mut ctx.accounts.pool, remaining);
    stake.active_claim = None;
    claim.status = ClaimStatus::Expired;
    emit!(ClaimExpired { claim: claim_key, released: remaining });
    Ok(())
}

// ------------------------------------------------------------------ staker actions

#[derive(Accounts)]
pub struct ApproveClaim<'info> {
    pub staker: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_CLAIM, pool.key().as_ref(), staker.key().as_ref(), claim.tx_hash.as_ref()], bump = claim.bump, has_one = staker)]
    pub claim: Box<Account<'info, Claim>>,
    #[account(mut, seeds = [SEED_STAKE, pool.key().as_ref(), staker.key().as_ref()], bump = stake_record.bump)]
    pub stake_record: Box<Account<'info, StakeRecord>>,
}

/// The staker's decision: forfeit the stake, start cooldown and vesting.
pub fn approve_claim(ctx: Context<ApproveClaim>) -> Result<()> {
    let now = now()?;
    let pool = &mut ctx.accounts.pool;
    require!(!pool.is_paused(now), PoolError::Paused);
    let claim_key = ctx.accounts.claim.key();
    let claim = &mut ctx.accounts.claim;
    require!(claim.status == ClaimStatus::AwaitingApproval, PoolError::ClaimNotAwaitingApproval);
    require!(now <= claim.approve_deadline, PoolError::ApprovalWindowExpired);
    let stake = &mut ctx.accounts.stake_record;
    require!(!stake.suspended, PoolError::StakeSuspended);
    require!(!stake.forfeited && stake.active_claim == Some(claim_key), PoolError::ClaimStakeMismatch);
    activate(pool, stake, claim, now)?;
    emit!(ClaimApproved { claim: claim_key });
    Ok(())
}

#[derive(Accounts)]
pub struct ClaimStream<'info> {
    pub staker: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: SystemAccount<'info>,
    #[account(mut, seeds = [SEED_CLAIM, pool.key().as_ref(), staker.key().as_ref(), claim.tx_hash.as_ref()], bump = claim.bump, has_one = staker)]
    pub claim: Box<Account<'info, Claim>>,
    #[account(
        seeds = [SEED_STAKE, pool.key().as_ref(), staker.key().as_ref()],
        bump = stake_record.bump,
        has_one = beneficiary @ PoolError::WrongBeneficiary,
    )]
    pub stake_record: Box<Account<'info, StakeRecord>>,
    /// CHECK: must be the stake's beneficiary (`has_one` above); only receives SOL.
    #[account(mut)]
    pub beneficiary: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
    pub leg: MarinadeLeg<'info>,
}

/// Pays what has vested, up to today's payout cap, to the beneficiary.
pub fn claim_stream(ctx: Context<ClaimStream>) -> Result<()> {
    let now = now()?;
    let pool_key = ctx.accounts.pool.key();
    let vault = ctx.accounts.vault.to_account_info();
    let pool = &mut ctx.accounts.pool;
    require!(!pool.is_paused(now), PoolError::Paused);
    let claim_key = ctx.accounts.claim.key();
    let claim = &mut ctx.accounts.claim;
    require!(claim.status == ClaimStatus::Active, PoolError::ClaimNotActive);
    require!(now >= claim.cooldown_ends, PoolError::CooldownNotPassed);
    require!(claim.streamed < claim.entitlement, PoolError::ClaimFullyStreamed);
    require!(!ctx.accounts.stake_record.suspended, PoolError::StakeSuspended);

    let vested = rules::vested(claim.entitlement, claim.cooldown_ends, claim.vesting_ends, now).core()?;
    let claimable = vested.saturating_sub(claim.streamed);
    require!(claimable > 0, PoolError::NothingVested);

    roll_day(pool, now);
    let base = pool_capacity(pool)?.max(claim.capacity_snapshot);
    let cap = rules::payout_cap(base, pool.total_allocated).core()?;
    let amount = claimable.min(cap.saturating_sub(pool.day_outflow));
    require!(amount > 0, PoolError::DailyOutflowCapReached);

    claim.streamed += amount;
    claim.last_collected = now;
    if claim.streamed >= claim.entitlement {
        claim.status = ClaimStatus::Completed;
    }
    release_allocation(pool, amount);
    pool.day_outflow = add(pool.day_outflow, amount).core()?;
    emit!(ClaimStreamed { claim: claim_key, amount });

    // If cash is short, Marinade unstakes the rest; the beneficiary pays that unstake's fee.
    let to = ctx.accounts.beneficiary.to_account_info();
    leg::pay_out(pool, &pool_key, &ctx.accounts.leg, &vault, &to, amount)?;
    Ok(())
}

// ------------------------------------------------------------------ admin: cancel, suspend, revoke

#[derive(Accounts)]
pub struct CancelClaim<'info> {
    pub admin: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, has_one = admin @ PoolError::NotAdmin)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_CLAIM, pool.key().as_ref(), claim.staker.as_ref(), claim.tx_hash.as_ref()], bump = claim.bump)]
    pub claim: Box<Account<'info, Claim>>,
    #[account(mut, seeds = [SEED_STAKE, pool.key().as_ref(), claim.staker.as_ref()], bump = stake_record.bump)]
    pub stake_record: Box<Account<'info, StakeRecord>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: SystemAccount<'info>,
    pub leg: MarinadeLeg<'info>,
}

/// False-positive reversal. An approved (forfeited) stake is restored under the penalty lock;
/// before approval nothing was forfeited, so there is no penalty.
pub fn cancel_claim(ctx: Context<CancelClaim>) -> Result<()> {
    let now = now()?;
    let pool_key = ctx.accounts.pool.key();
    let vault = ctx.accounts.vault.to_account_info();
    let pool = &mut ctx.accounts.pool;
    let claim_key = ctx.accounts.claim.key();
    let claim = &mut ctx.accounts.claim;
    require!(
        matches!(claim.status, ClaimStatus::Active | ClaimStatus::PendingTime | ClaimStatus::AwaitingApproval),
        PoolError::ClaimNotCancellable
    );
    release_allocation(pool, claim.entitlement - claim.streamed);
    let stake = &mut ctx.accounts.stake_record;
    if claim.status == ClaimStatus::Active {
        // Growth so far belongs to the stakes in before this one returns (multichain call site).
        leg::harvest(pool, &pool_key, &ctx.accounts.leg, &vault, now)?;
        stake.forfeited = false;
        stake.penalty_locked_until = secs_after(now, PENALTY_LOCK_SECS)?;
        // Out of total_staked while forfeited, so it earns again only from now.
        stake.yield_index_at = pool.staker_yield_index;
        pool.total_staked = add(pool.total_staked, claim.stake).core()?;
        pool.total_stakers = add(pool.total_stakers, 1).core()?;
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
    #[account(seeds = [SEED_POOL], bump = pool.bump, has_one = admin @ PoolError::NotAdmin)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_STAKE, pool.key().as_ref(), staker.as_ref()], bump = stake_record.bump)]
    pub stake_record: Box<Account<'info, StakeRecord>>,
    /// Optional: a claim of this staker whose approve / collection clock restarts on unsuspend.
    #[account(mut, seeds = [SEED_CLAIM, pool.key().as_ref(), staker.as_ref(), claim.tx_hash.as_ref()], bump = claim.bump)]
    pub claim: Option<Box<Account<'info, Claim>>>,
}

/// Holds payouts on a stake. Does not block principal withdrawal.
pub fn suspend_stake(ctx: Context<AdminStake>, _staker: Pubkey) -> Result<()> {
    let stake = &mut ctx.accounts.stake_record;
    require!(!stake.forfeited || stake.active_claim.is_some(), PoolError::StakeForfeited);
    stake.suspended = true;
    emit!(StakeSuspended { staker: stake.staker });
    Ok(())
}

/// Lifts the hold and gives the staker a fresh approve / collection window, so a suspension can
/// never cost them their claim.
pub fn unsuspend_stake(ctx: Context<AdminStake>, _staker: Pubkey) -> Result<()> {
    let now = now()?;
    let stake = &mut ctx.accounts.stake_record;
    stake.suspended = false;
    emit!(StakeUnsuspended { staker: stake.staker });
    if let Some(claim) = ctx.accounts.claim.as_mut() {
        match claim.status {
            ClaimStatus::AwaitingApproval => claim.approve_deadline = secs_after(now, APPROVE_WINDOW_SECS)?,
            ClaimStatus::Active => claim.last_collected = now,
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
    #[account(seeds = [SEED_POOL], bump = pool.bump, has_one = admin @ PoolError::NotAdmin)]
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
pub fn revoke_approval(ctx: Context<RevokeApproval>, a: ClaimApproval, hash: [u8; 32]) -> Result<()> {
    rules::check_deadline(a.deadline, now()?).core()?;
    let message = approval::encode_message(&crate::ID, ctx.accounts.pool.cluster, &a);
    require!(approval::approval_hash(&message) == hash, PoolError::WrongRevocationAccount);
    ctx.accounts.revoked.set_inner(RevokedApproval { hash, bump: ctx.bumps.revoked });
    emit!(ApprovalRevoked { staker: a.staker, hash });
    Ok(())
}

// ------------------------------------------------------------------ 2-of-2 override

#[derive(Accounts)]
#[instruction(staker: Pubkey, tx_hash: [u8; 32])]
pub struct ApproveOverride<'info> {
    #[account(mut)]
    pub signer: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
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
    pub system_program: Program<'info, System>,
}

/// Admin and co-signer each approve identical terms; the second approval executes. Bypasses the
/// oracle, the time gate and the staker's approval: the two signatures are the gate.
pub fn approve_override(ctx: Context<ApproveOverride>, staker: Pubkey, tx_hash: [u8; 32], entitlement: u64, tier: u8) -> Result<()> {
    let now = now()?;
    let signer = ctx.accounts.signer.key();
    let (admin, co_signer) = (ctx.accounts.pool.admin, ctx.accounts.pool.co_signer);
    require!(signer == admin || signer == co_signer, PoolError::NotAdminOrCoSigner);
    require!(!ctx.accounts.pool.is_paused(now), PoolError::Paused);
    require!(entitlement > 0, PoolError::EntitlementNotPositive);
    rules::tier_ratio(tier).core()?;

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
    require!(req.entitlement == entitlement && req.tier == tier, PoolError::OverrideParamsMismatch);
    if signer == admin {
        req.admin_approver = Some(admin);
    }
    if signer == co_signer {
        req.co_signer_approver = Some(co_signer);
    }
    emit!(OverrideApproved { claim: claim_key, approver: signer, entitlement, tier });
    // Ready only while both approvals match the current roles.
    if req.admin_approver != Some(admin) || req.co_signer_approver != Some(co_signer) {
        return Ok(());
    }

    execute_override(&mut ctx.accounts.pool, &mut ctx.accounts.stake_record, &mut ctx.accounts.claim, claim_key, staker, tx_hash, entitlement, tier, ctx.bumps.claim, now)?;
    emit!(OverrideExecuted { claim: claim_key });
    // Deleted on execution, so cancel_pending_override finds nothing afterwards.
    ctx.accounts.override_request.close(ctx.accounts.signer.to_account_info())
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
    rules::check_entitlement(entitlement, stake.amount, tier).core()?;

    // An already-forfeited stake (re-execution of this same claim) is earmarked for it, not new capacity.
    let mut effective = pool_capacity(pool)?;
    if stake.forfeited {
        effective = add(effective, stake.amount).core()?;
    }
    require!(add(pool.total_allocated, entitlement).core()? <= effective, PoolError::Insolvent);
    pool.total_allocated = add(pool.total_allocated, entitlement).core()?;

    *claim = Claim {
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
    };
    if stake.forfeited {
        start_clocks(pool, claim, now)?;
    } else {
        activate(pool, stake, claim, now)?;
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
    #[account(seeds = [SEED_POOL], bump = pool.bump, has_one = admin @ PoolError::NotAdmin)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, close = admin, seeds = [SEED_OVERRIDE, override_request.claim.as_ref()], bump = override_request.bump)]
    pub override_request: Box<Account<'info, OverrideRequest>>,
}

/// Admin only (as V8). Deletes the pending request.
pub fn cancel_pending_override(ctx: Context<CancelPendingOverride>) -> Result<()> {
    emit!(OverrideCancelled { claim: ctx.accounts.override_request.claim });
    Ok(())
}
