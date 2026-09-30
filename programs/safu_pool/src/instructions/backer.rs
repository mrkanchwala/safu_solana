//! Backers (multichain `backer.rs`). The four withdrawal-safety rules:
//!   1. Money returns only to the depositing address, with its signature. No admin path.
//!   2. During the notice the money still counts toward capacity.
//!   3. Only free capital leaves: after the withdrawal, `total_allocated <= capacity`.
//!   4. New money counts only after `BACKER_MATURITY_SECS`.
//! Pause blocks new deposits only; request, cancel and complete still work.

use anchor_lang::prelude::*;

use super::now;
use crate::constants::{SEED_BACKER, SEED_POOL, SEED_VAULT};
use crate::errors::{CoreResultExt, PoolError};
use crate::events::{
    Backed, BackerWithdrawalCancelled, BackerWithdrawalRequested, BackerWithdrawn, BackingMatured, YieldClaimed,
};
use crate::leg;
// Glob: `#[derive(Accounts)]` on a struct holding `MarinadeLeg` needs its generated client modules.
use crate::marinade::*;
use crate::state::{BackerRecord, Pool};
use pool_core::params::{BACKER_MATURITY_SECS, BACKER_NOTICE_SECS};
use pool_core::{add, sub, yields};

/// Settles yield on the counted balance, then moves matured pending money into it.
/// Returns the amount moved. Yield first, so newly counted money earns only from now.
fn settle(pool: &mut Pool, record: &mut BackerRecord, now: i64) -> Result<u64> {
    let earned = yields::owed(record.amount, pool.backer_yield_index, record.yield_index_at).core()?;
    record.yield_owed = add(record.yield_owed, earned).core()?;
    record.yield_index_at = pool.backer_yield_index;
    if record.pending_amount == 0 || now < record.pending_matures_at {
        return Ok(0);
    }
    let moved = record.pending_amount;
    record.amount = add(record.amount, moved).core()?;
    record.pending_amount = 0;
    record.pending_matures_at = 0;
    pool.total_backed_pending = sub(pool.total_backed_pending, moved).core()?;
    pool.total_backed = add(pool.total_backed, moved).core()?;
    emit!(BackingMatured { backer: record.backer, amount: moved });
    Ok(moved)
}

#[derive(Accounts)]
pub struct Back<'info> {
    #[account(mut)]
    pub backer: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: SystemAccount<'info>,
    #[account(
        init_if_needed,
        payer = backer,
        space = 8 + BackerRecord::INIT_SPACE,
        seeds = [SEED_BACKER, pool.key().as_ref(), backer.key().as_ref()],
        bump,
    )]
    pub backer_record: Box<Account<'info, BackerRecord>>,
    pub system_program: Program<'info, System>,
    pub leg: MarinadeLeg<'info>,
}

/// Deposit. A top-up while earlier money is maturing restarts the wait for the whole pending amount.
pub fn back(ctx: Context<Back>, amount: u64) -> Result<()> {
    let now = now()?;
    let pool_key = ctx.accounts.pool.key();
    let vault = ctx.accounts.vault.to_account_info();
    let pool = &mut ctx.accounts.pool;
    require!(!pool.is_paused(now), PoolError::Paused);
    require!(amount > 0, PoolError::AmountNotPositive);
    leg::harvest(pool, &pool_key, &ctx.accounts.leg, &vault, now)?;
    let record = &mut ctx.accounts.backer_record;
    if record.backer == Pubkey::default() {
        record.backer = ctx.accounts.backer.key();
        record.yield_index_at = pool.backer_yield_index;
        record.bump = ctx.bumps.backer_record;
    }
    settle(pool, record, now)?;
    let matures_at = now.checked_add(BACKER_MATURITY_SECS).ok_or(PoolError::MathOverflow)?;
    record.pending_amount = add(record.pending_amount, amount).core()?;
    record.pending_matures_at = matures_at;
    pool.total_backed_pending = add(pool.total_backed_pending, amount).core()?;
    emit!(Backed { backer: record.backer, amount, matures_at });

    crate::vault::receive(&ctx.accounts.backer.to_account_info(), &vault, amount)?;
    leg::push_idle(pool, &pool_key, &ctx.accounts.leg, &vault, now)?;
    Ok(())
}

#[derive(Accounts)]
pub struct MatureBacking<'info> {
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: SystemAccount<'info>,
    #[account(mut, seeds = [SEED_BACKER, pool.key().as_ref(), backer_record.backer.as_ref()], bump = backer_record.bump)]
    pub backer_record: Box<Account<'info, BackerRecord>>,
    pub leg: MarinadeLeg<'info>,
}

/// Permissionless. Until someone calls it, capacity is undercounted (the safe direction).
pub fn mature_backing(ctx: Context<MatureBacking>) -> Result<()> {
    let now = now()?;
    let pool_key = ctx.accounts.pool.key();
    let vault = ctx.accounts.vault.to_account_info();
    let record = &mut ctx.accounts.backer_record;
    require!(record.pending_amount > 0, PoolError::NoPendingBacking);
    // Growth so far belongs to the capacity before this money counts.
    leg::harvest(&mut ctx.accounts.pool, &pool_key, &ctx.accounts.leg, &vault, now)?;
    let moved = settle(&mut ctx.accounts.pool, record, now)?;
    require!(moved > 0, PoolError::BackingNotMature);
    Ok(())
}

#[derive(Accounts)]
pub struct BackerOnly<'info> {
    pub backer: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(
        mut,
        seeds = [SEED_BACKER, pool.key().as_ref(), backer.key().as_ref()],
        bump = backer_record.bump,
        has_one = backer,
    )]
    pub backer_record: Box<Account<'info, BackerRecord>>,
}

/// Starts the notice on matured money. Works while paused.
pub fn request_backer_withdrawal(ctx: Context<BackerOnly>, amount: u64) -> Result<()> {
    let now = now()?;
    require!(amount > 0, PoolError::AmountNotPositive);
    let record = &mut ctx.accounts.backer_record;
    require!(record.withdraw_amount == 0, PoolError::BackerWithdrawalPending);
    settle(&mut ctx.accounts.pool, record, now)?;
    require!(amount <= record.amount, PoolError::BackerAmountExceedsBalance);
    let ready_at = now.checked_add(BACKER_NOTICE_SECS).ok_or(PoolError::MathOverflow)?;
    record.withdraw_amount = amount;
    record.withdraw_ready_at = ready_at;
    emit!(BackerWithdrawalRequested { backer: record.backer, amount, ready_at });
    Ok(())
}

/// Works while paused.
pub fn cancel_backer_withdrawal(ctx: Context<BackerOnly>) -> Result<()> {
    let record = &mut ctx.accounts.backer_record;
    require!(record.withdraw_amount > 0, PoolError::NoBackerWithdrawal);
    let amount = record.withdraw_amount;
    record.withdraw_amount = 0;
    record.withdraw_ready_at = 0;
    emit!(BackerWithdrawalCancelled { backer: record.backer, amount });
    Ok(())
}

#[derive(Accounts)]
pub struct CompleteBackerWithdrawal<'info> {
    #[account(mut)]
    pub backer: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: SystemAccount<'info>,
    #[account(
        mut,
        seeds = [SEED_BACKER, pool.key().as_ref(), backer.key().as_ref()],
        bump = backer_record.bump,
        has_one = backer,
    )]
    pub backer_record: Box<Account<'info, BackerRecord>>,
    pub system_program: Program<'info, System>,
    pub leg: MarinadeLeg<'info>,
}

/// Pays a requested withdrawal once the notice has passed, if the capital is free and liquid.
/// On failure nothing changes. Unpaid yield goes out with it. Works while paused.
pub fn complete_backer_withdrawal(ctx: Context<CompleteBackerWithdrawal>) -> Result<()> {
    let now = now()?;
    let pool_key = ctx.accounts.pool.key();
    let vault = ctx.accounts.vault.to_account_info();
    let pool = &mut ctx.accounts.pool;
    let record = &mut ctx.accounts.backer_record;
    let amount = record.withdraw_amount;
    require!(amount > 0, PoolError::NoBackerWithdrawal);
    require!(now >= record.withdraw_ready_at, PoolError::BackerNoticeNotPassed);
    let capacity = pool_core::capacity(pool.total_staked, pool.total_backed).core()?;
    pool_core::check_backer_capital_free(pool.total_allocated, capacity, amount).core()?;
    leg::harvest(pool, &pool_key, &ctx.accounts.leg, &vault, now)?;

    settle(pool, record, now)?;
    let yield_paid = record.yield_owed.min(pool.backer_yield_reserved);
    record.yield_owed -= yield_paid;
    pool.backer_yield_reserved -= yield_paid;
    record.amount = sub(record.amount, amount).core()?;
    record.withdraw_amount = 0;
    record.withdraw_ready_at = 0;
    pool.total_backed = sub(pool.total_backed, amount).core()?;
    emit!(BackerWithdrawn { backer: record.backer, amount, yield_paid });

    let total = add(amount, yield_paid).core()?;
    let to = ctx.accounts.backer.to_account_info();
    leg::pay_out(pool, &pool_key, &ctx.accounts.leg, &vault, &to, total)?;
    Ok(())
}

#[derive(Accounts)]
pub struct ClaimBackerYield<'info> {
    #[account(mut)]
    pub backer: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: SystemAccount<'info>,
    #[account(
        mut,
        seeds = [SEED_BACKER, pool.key().as_ref(), backer.key().as_ref()],
        bump = backer_record.bump,
        has_one = backer,
    )]
    pub backer_record: Box<Account<'info, BackerRecord>>,
    pub leg: MarinadeLeg<'info>,
}

/// Backer yield, any time; principal untouched (multichain `claim_backer_yield`).
pub fn claim_backer_yield(ctx: Context<ClaimBackerYield>) -> Result<()> {
    let now = now()?;
    let pool_key = ctx.accounts.pool.key();
    let vault = ctx.accounts.vault.to_account_info();
    let pool = &mut ctx.accounts.pool;
    require!(!pool.is_paused(now), PoolError::Paused);
    leg::harvest(pool, &pool_key, &ctx.accounts.leg, &vault, now)?;
    let record = &mut ctx.accounts.backer_record;
    settle(pool, record, now)?;
    let paid = record.yield_owed.min(pool.backer_yield_reserved);
    require!(paid > 0, PoolError::NothingToClaim);
    record.yield_owed -= paid;
    pool.backer_yield_reserved -= paid;
    emit!(YieldClaimed { owner: record.backer, amount: paid, backer: true });

    let to = ctx.accounts.backer.to_account_info();
    leg::pay_out(pool, &pool_key, &ctx.accounts.leg, &vault, &to, paid)?;
    Ok(())
}
