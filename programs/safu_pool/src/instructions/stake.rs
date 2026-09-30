//! Stake, withdraw, set beneficiary, emergency exit (multichain `stake.rs`). Withdraw and emergency
//! exit take an amount: all of the stake, or a part that leaves at least the min stake.

use anchor_lang::prelude::*;

use super::now;
use crate::constants::{SEED_POOL, SEED_STAKE, SEED_VAULT};
use crate::errors::{CoreResultExt, PoolError};
use crate::events::{BeneficiarySet, EmergencyExited, Staked, Withdrawn, YieldClaimed};
use crate::leg;
// Glob: `#[derive(Accounts)]` on a struct holding `MarinadeLeg` needs its generated client modules.
use crate::marinade::*;
use crate::state::{Pool, StakeRecord, ACCOUNT_VERSION};
use pool_core::yields;

/// Payouts can never go to a role key (multichain `stake` identity checks), nor to the pool or its
/// vault (the SOL would only be stranded).
fn check_beneficiary(pool: &Pool, pool_key: &Pubkey, beneficiary: &Pubkey) -> Result<()> {
    require_keys_neq!(*beneficiary, pool.oracle, PoolError::BeneficiaryIsOracle);
    require_keys_neq!(*beneficiary, pool.admin, PoolError::BeneficiaryIsAdmin);
    require_keys_neq!(*beneficiary, pool.co_signer, PoolError::BeneficiaryIsCoSigner);
    let vault = Pubkey::create_program_address(&[SEED_VAULT, pool_key.as_ref(), &[pool.vault_bump]], &crate::ID)
        .map_err(|_| error!(PoolError::MathOverflow))?;
    require!(*beneficiary != *pool_key && *beneficiary != vault, PoolError::BeneficiaryIsPoolAccount);
    Ok(())
}

/// No claim open or queued on this stake, and not forfeited.
fn require_no_claim(record: &StakeRecord) -> Result<()> {
    require!(!record.forfeited, PoolError::StakeForfeited);
    require!(record.active_claim.is_none(), PoolError::ClaimActive);
    require!(record.reserved_claim.is_none(), PoolError::ClaimQueuedForStake);
    Ok(())
}

/// The one rule for every staker exit, withdraw or emergency exit, all or part (audit X3, the same
/// rule backers follow): no claim open, queued or forfeited on the stake, the penalty lock passed,
/// what stays is zero or at least the min stake, and only free capital leaves (open claims still
/// fit in capacity afterwards). Takes `amount` of principal plus ALL unpaid yield off the books;
/// yield is never capital, so the capital check counts principal only.
/// Returns `(yield_paid, remaining)`. The payment follows through `leg::pay_out`; the caller then
/// re-checks free capital, because an unstake inside it can mark a Marinade loss off `total_staked`.
fn settle_exit(pool: &mut Pool, record: &mut StakeRecord, amount: u64, now: i64) -> Result<(u64, u64)> {
    require_no_claim(record)?;
    require!(now >= record.penalty_locked_until, PoolError::PenaltyLockActive);
    let remaining = pool_core::stake::check_withdraw(amount, record.amount, &pool.settings()).core()?;
    let capacity = pool_core::capacity(pool.total_staked, pool.total_backed).core()?;
    pool_core::check_capital_free(pool.total_allocated, capacity, amount).core()?;

    // Yield is capped by the set-aside (which a Marinade loss can leave short of the records' sum).
    let owed = yields::owed(record.amount, pool.staker_yield_index, record.yield_index_at).core()?;
    let yield_paid = owed.min(pool.staker_yield_reserved);
    pool.staker_yield_reserved -= yield_paid;
    record.yield_index_at = pool.staker_yield_index;
    record.amount = remaining;
    // Saturating: a Marinade loss marks total_staked down, which can leave it below the last stake.
    pool.total_staked = pool.total_staked.saturating_sub(amount);
    if remaining == 0 {
        pool.total_stakers = pool.total_stakers.saturating_sub(1);
    }
    Ok((yield_paid, remaining))
}

/// After the payment: the capital that left must still have been free (see `settle_exit`).
fn recheck_capital(pool: &Pool) -> Result<()> {
    let capacity = pool_core::capacity(pool.total_staked, pool.total_backed).core()?;
    pool_core::check_capital_free(pool.total_allocated, capacity, 0).core()
}

#[derive(Accounts)]
pub struct Stake<'info> {
    #[account(mut)]
    pub staker: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: SystemAccount<'info>,
    /// Created on the first stake and after a full withdraw (the record is closed then). An existing
    /// record is a live or forfeited stake: both refuse a new one in the handler.
    #[account(
        init_if_needed,
        payer = staker,
        space = 8 + StakeRecord::INIT_SPACE,
        seeds = [SEED_STAKE, pool.key().as_ref(), staker.key().as_ref()],
        bump,
    )]
    pub stake_record: Box<Account<'info, StakeRecord>>,
    pub system_program: Program<'info, System>,
    pub leg: MarinadeLeg<'info>,
}

pub fn stake(ctx: Context<Stake>, amount: u64, beneficiary: Pubkey) -> Result<()> {
    let now = now()?;
    let pool_key = ctx.accounts.pool.key();
    let vault = ctx.accounts.vault.to_account_info();
    let pool = &mut ctx.accounts.pool;
    require!(!pool.is_paused(now), PoolError::Paused);
    // Growth so far belongs to the stakes already in (multichain call site).
    leg::harvest(pool, &pool_key, &ctx.accounts.leg, &vault, now)?;
    let record = &mut ctx.accounts.stake_record;
    if record.staker != Pubkey::default() {
        require!(!record.forfeited, PoolError::AddressHasApprovedClaim);
        return err!(PoolError::AlreadyStaked);
    }
    pool_core::stake::check_new_stake(amount, pool.total_staked, &pool.settings()).core()?;
    check_beneficiary(pool, &pool_key, &beneficiary)?;

    let staker = ctx.accounts.staker.key();
    record.set_inner(StakeRecord {
        version: ACCOUNT_VERSION,
        staker,
        beneficiary,
        amount,
        yield_index_at: pool.staker_yield_index,
        staked_at: now,
        penalty_locked_until: 0,
        forfeited: false,
        forfeited_by: None,
        suspended: false,
        active_claim: None,
        reserved_claim: None,
        bump: ctx.bumps.stake_record,
        reserved: [0; 64],
    });
    pool.total_staked = pool_core::add(pool.total_staked, amount).core()?;
    pool.total_stakers = pool_core::add(pool.total_stakers, 1).core()?;
    emit!(Staked { staker, amount });

    crate::vault::receive(&ctx.accounts.staker.to_account_info(), &vault, amount)?;
    leg::push_idle(pool, &pool_key, &ctx.accounts.leg, &vault, now)?;
    Ok(())
}

#[derive(Accounts)]
pub struct SetBeneficiary<'info> {
    pub staker: Signer<'info>,
    #[account(seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(
        mut,
        seeds = [SEED_STAKE, pool.key().as_ref(), staker.key().as_ref()],
        bump = stake_record.bump,
        has_one = staker,
    )]
    pub stake_record: Box<Account<'info, StakeRecord>>,
}

/// Blocked while a claim is open or queued (eng review D1): a drained wallet may still hold the stake
/// key, and the payout must keep going to the beneficiary set before the claim.
pub fn set_beneficiary(ctx: Context<SetBeneficiary>, beneficiary: Pubkey) -> Result<()> {
    require!(!ctx.accounts.pool.is_paused(now()?), PoolError::Paused);
    check_beneficiary(&ctx.accounts.pool, &ctx.accounts.pool.key(), &beneficiary)?;
    let record = &mut ctx.accounts.stake_record;
    require_no_claim(record)?;
    record.beneficiary = beneficiary;
    emit!(BeneficiarySet { staker: record.staker, beneficiary });
    Ok(())
}

#[derive(Accounts)]
pub struct Withdraw<'info> {
    #[account(mut)]
    pub staker: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: SystemAccount<'info>,
    /// Closed by the handler when the whole stake leaves (rent back to the staker).
    #[account(
        mut,
        seeds = [SEED_STAKE, pool.key().as_ref(), staker.key().as_ref()],
        bump = stake_record.bump,
        has_one = staker,
        has_one = beneficiary @ PoolError::WrongBeneficiary,
    )]
    pub stake_record: Box<Account<'info, StakeRecord>>,
    /// CHECK: must be the stake's beneficiary (`has_one` above); only receives SOL.
    #[account(mut)]
    pub beneficiary: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
    pub leg: MarinadeLeg<'info>,
}

/// `amount` of principal + all unpaid yield to the beneficiary. Taking all of it closes the record
/// (rent back to the staker).
pub fn withdraw(ctx: Context<Withdraw>, amount: u64) -> Result<()> {
    let now = now()?;
    let pool_key = ctx.accounts.pool.key();
    let vault = ctx.accounts.vault.to_account_info();
    let pool = &mut ctx.accounts.pool;
    require!(!pool.is_paused(now), PoolError::Paused);
    leg::harvest(pool, &pool_key, &ctx.accounts.leg, &vault, now)?;

    let record = &mut ctx.accounts.stake_record;
    let (yield_paid, remaining) = settle_exit(pool, record, amount, now)?;
    emit!(Withdrawn { staker: record.staker, principal: amount, yield_paid, remaining });

    let total = pool_core::add(amount, yield_paid).core()?;
    let to = ctx.accounts.beneficiary.to_account_info();
    leg::pay_out(pool, &pool_key, &ctx.accounts.leg, &vault, &to, total)?;
    recheck_capital(pool)?;
    if remaining == 0 {
        ctx.accounts.stake_record.close(ctx.accounts.staker.to_account_info())?;
    }
    Ok(())
}

#[derive(Accounts)]
pub struct EmergencyExit<'info> {
    #[account(mut)]
    pub staker: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: SystemAccount<'info>,
    /// Closed by the handler when the whole stake leaves (rent back to the staker).
    #[account(
        mut,
        seeds = [SEED_STAKE, pool.key().as_ref(), staker.key().as_ref()],
        bump = stake_record.bump,
        has_one = staker,
    )]
    pub stake_record: Box<Account<'info, StakeRecord>>,
    pub system_program: Program<'info, System>,
    pub leg: MarinadeLeg<'info>,
}

/// The pause-time escape hatch: only while paused, pays the staker directly. Same rule as
/// `withdraw` (all or part, free capital only), so a pause can neither lock out a staker whose
/// capital is free nor let one take capital that open claims need. Unlike multichain it honours the
/// penalty lock, so a pause cannot be used to leave a false-positive penalty early.
pub fn emergency_exit(ctx: Context<EmergencyExit>, amount: u64) -> Result<()> {
    let now = now()?;
    let pool_key = ctx.accounts.pool.key();
    let vault = ctx.accounts.vault.to_account_info();
    let pool = &mut ctx.accounts.pool;
    require!(pool.is_paused(now), PoolError::NotPaused);

    let record = &mut ctx.accounts.stake_record;
    let (yield_paid, remaining) = settle_exit(pool, record, amount, now)?;
    emit!(EmergencyExited { staker: record.staker, principal: amount, yield_paid, remaining });

    // No harvest while paused; the unstake a payment needs still runs.
    let total = pool_core::add(amount, yield_paid).core()?;
    let to = ctx.accounts.staker.to_account_info();
    leg::pay_out(pool, &pool_key, &ctx.accounts.leg, &vault, &to, total)?;
    recheck_capital(pool)?;
    if remaining == 0 {
        ctx.accounts.stake_record.close(ctx.accounts.staker.to_account_info())?;
    }
    Ok(())
}

#[derive(Accounts)]
pub struct ClaimYield<'info> {
    pub staker: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: SystemAccount<'info>,
    #[account(
        mut,
        seeds = [SEED_STAKE, pool.key().as_ref(), staker.key().as_ref()],
        bump = stake_record.bump,
        has_one = staker,
        has_one = beneficiary @ PoolError::WrongBeneficiary,
    )]
    pub stake_record: Box<Account<'info, StakeRecord>>,
    /// CHECK: must be the stake's beneficiary (`has_one` above); only receives SOL.
    #[account(mut)]
    pub beneficiary: UncheckedAccount<'info>,
    pub leg: MarinadeLeg<'info>,
}

/// Staker yield to the beneficiary, any time; principal untouched (multichain `claim_yield`).
pub fn claim_yield(ctx: Context<ClaimYield>) -> Result<()> {
    let now = now()?;
    let pool_key = ctx.accounts.pool.key();
    let vault = ctx.accounts.vault.to_account_info();
    let pool = &mut ctx.accounts.pool;
    require!(!pool.is_paused(now), PoolError::Paused);
    let record = &mut ctx.accounts.stake_record;
    require!(!record.forfeited, PoolError::StakeForfeited);
    leg::harvest(pool, &pool_key, &ctx.accounts.leg, &vault, now)?;

    let owed = yields::owed(record.amount, pool.staker_yield_index, record.yield_index_at).core()?;
    let paid = owed.min(pool.staker_yield_reserved);
    require!(paid > 0, PoolError::NothingToClaim);
    record.yield_index_at = pool.staker_yield_index;
    pool.staker_yield_reserved -= paid;
    emit!(YieldClaimed { owner: record.staker, amount: paid, backer: false });

    let to = ctx.accounts.beneficiary.to_account_info();
    leg::pay_out(pool, &pool_key, &ctx.accounts.leg, &vault, &to, paid)?;
    Ok(())
}
