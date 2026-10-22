//! Backers (multichain `backer.rs`). A backer's money adds to the pool's capacity; anyone can back,
//! and a staker's deposit above the max stake lands here too. Backers earn yield on counted money
//! and always get back exactly what they put in (no loss accounting).
//!
//! The withdrawal-safety rules:
//!   1. Money returns only to the depositing address, with its signature. No admin path.
//!   2. Money requested out keeps counting toward capacity until it is paid.
//!   3. Only free capital leaves: after a payment, `total_allocated <= capacity` (the same rule
//!      every staker exit follows, `stake.rs`).
//!   4. New money counts after `BackerMaturitySecs` (default 0: from day 1). Each deposit keeps
//!      its own clock.
//!   5. A withdrawal drips out daily: over `DripMinSecs` on an idle pool up to `DripMaxSecs` at
//!      `DRIP_FULL_UTIL_BPS` utilisation, recomputed each day. Each day every open request gets a
//!      pro-rata share (by what it is still owed) of that day's free capital, so collecting first
//!      takes nothing from the others. A day on which open claims need every dollar pays nothing.
//!
//! Pause blocks new deposits only; request, cancel and collect still work (rule 3 still applies),
//! so a pause can never hold backer money.

use anchor_lang::prelude::*;

use super::now;
use crate::constants::{SEED_BACKER, SEED_POOL, SEED_VAULT};
use crate::errors::{CoreResultExt, PoolError};
use crate::events::{
    Backed, BackerWithdrawalCancelled, BackerWithdrawalRequested, BackerWithdrawn, BackingMatured,
    YieldClaimed,
};
use anchor_spl::token::TokenAccount;

// Glob: `#[derive(Accounts)]` on a struct holding `Cash` needs its generated client modules.
use crate::leg::Money;
use crate::state::{BackerRecord, PendingDeposit, Pool, ACCOUNT_VERSION};
use crate::vault::{self, *};
use pool_core::params::{MAX_PENDING_DEPOSITS, SECONDS_PER_DAY};
use pool_core::points::BACKER;
use pool_core::settings::SettingKey;
use pool_core::{add, drip, sub, yields};

/// A record the `init_if_needed` constraint just created gets its owner and yield start.
pub(crate) fn open_record(pool: &Pool, record: &mut BackerRecord, backer: Pubkey, bump: u8) {
    if record.backer == Pubkey::default() {
        record.version = ACCOUNT_VERSION;
        record.backer = backer;
        record.yield_index_at = pool.backer_yield_index;
        record.points_index_at = pool.points_index[BACKER];
        record.bump = bump;
    }
}

/// Yield owed and points earned on the counted balance up to now (not yet written to the record),
/// and the points index they run to.
pub(crate) fn accrued(pool: &Pool, record: &BackerRecord, now: i64) -> Result<(u64, u64, u128)> {
    let earned = yields::owed(
        record.amount,
        pool.backer_yield_index,
        record.yield_index_at,
    )
    .core()?;
    let index = pool.points_index(BACKER, now).core()?;
    let points = pool_core::points::earned(record.amount, record.points_index_at, index).core()?;
    Ok((
        add(record.yield_owed, earned).core()?,
        add(record.points, points).core()?,
        index,
    ))
}

/// Settles yield on the counted balance, then moves every matured deposit into it. Returns the
/// amount moved. Yield first, so newly counted money earns only from now.
fn settle(pool: &mut Pool, record: &mut BackerRecord, now: i64) -> Result<u64> {
    let (yield_owed, points, points_index) = accrued(pool, record, now)?;
    record.yield_owed = yield_owed;
    record.yield_index_at = pool.backer_yield_index;
    record.points = points;
    record.points_index_at = points_index;
    let mut moved = 0;
    let mut waiting = [PendingDeposit::default(); MAX_PENDING_DEPOSITS];
    let mut kept = 0;
    for d in &record.pending[..record.pending_count as usize] {
        if now >= d.matures_at {
            moved = add(moved, d.amount).core()?;
        } else {
            waiting[kept] = *d;
            kept += 1;
        }
    }
    if moved > 0 {
        record.pending = waiting;
        record.pending_count = kept as u8;
        record.amount = add(record.amount, moved).core()?;
        pool.total_backed_pending = sub(pool.total_backed_pending, moved).core()?;
        pool.total_backed = add(pool.total_backed, moved).core()?;
        emit!(BackingMatured {
            backer: record.backer,
            amount: moved
        });
    }
    Ok(moved)
}

/// Books `amount` of backing (multichain `add_backing`). No transfer: the caller (`back`, or `stake`
/// for a deposit above the max stake) moves the money in after. Shared so an overflow and a direct
/// backing follow exactly the same rules.
pub(crate) fn add_backing(
    pool: &mut Pool,
    record: &mut BackerRecord,
    amount: u64,
    now: i64,
) -> Result<()> {
    pool.points_checkpoint(BACKER, now).core()?;
    settle(pool, record, now)?;
    let maturity = pool.settings().get(SettingKey::BackerMaturitySecs);
    let matures_at = now.checked_add(maturity).ok_or(PoolError::MathOverflow)?;
    if matures_at <= now {
        // Default: counts (and earns) from now.
        record.amount = add(record.amount, amount).core()?;
        pool.total_backed = add(pool.total_backed, amount).core()?;
    } else {
        let n = record.pending_count as usize;
        require!(n < MAX_PENDING_DEPOSITS, PoolError::TooManyPendingDeposits);
        record.pending[n] = PendingDeposit { amount, matures_at };
        record.pending_count += 1;
        pool.total_backed_pending = add(pool.total_backed_pending, amount).core()?;
    }
    crate::vault::mark_funded(pool)?;
    emit!(Backed {
        backer: record.backer,
        amount,
        matures_at
    });
    Ok(())
}

// ------------------------------------------------------------------ the daily drip

/// The day's drip snapshot (free capital, open requests, utilisation): the stored one if it is
/// today's, else what it would be now. Writes nothing.
fn drip_values(pool: &Pool, day: i64) -> Result<(u64, u64, u64)> {
    if pool.drip_snap_day == day {
        return Ok((
            pool.drip_snap_free,
            pool.drip_snap_open,
            pool.drip_snap_util,
        ));
    }
    let capacity = pool_core::capacity(pool.total_staked, pool.total_backed).core()?;
    Ok((
        capacity.saturating_sub(pool.total_allocated),
        pool.total_backer_open,
        drip::utilisation_bps(pool.total_allocated, capacity).core()?,
    ))
}

/// Takes the day's snapshot at the day's first backer exit action.
fn drip_snap(pool: &mut Pool, day: i64) -> Result<(u64, u64, u64)> {
    let v = drip_values(pool, day)?;
    if pool.drip_snap_day != day {
        pool.drip_snap_day = day;
        (
            pool.drip_snap_free,
            pool.drip_snap_open,
            pool.drip_snap_util,
        ) = v;
    }
    Ok(v)
}

fn drip_days(pool: &Pool, util: u64) -> Result<u64> {
    let s = pool.settings();
    drip::drip_days(
        util,
        s.get(SettingKey::DripMinSecs),
        s.get(SettingKey::DripMaxSecs),
    )
    .core()
}

/// The limits on a collection now: what the drip has unlocked and not yet paid, this request's
/// share of the day's free capital, live free capital (rule 3), and the snapshot's free capital.
struct Limits {
    due: u64,
    share: u64,
    free_now: u64,
    snap_free: u64,
}

/// Writes nothing.
fn limits(pool: &Pool, record: &BackerRecord, now: i64, snap: (u64, u64, u64)) -> Result<Limits> {
    let day = pool_core::claim::day_of(now);
    let (snap_free, snap_open, util) = snap;
    let elapsed = (now.saturating_sub(record.withdraw_started_at) / SECONDS_PER_DAY).max(0) as u64;
    let unlocked =
        drip::unlocked(record.withdraw_amount, elapsed, drip_days(pool, util)?).core()?;
    let share = if record.share_day == day {
        record.share_left
    } else {
        // A request made after the snapshot is not in `snap_open`: `share` counts it as the whole.
        let remaining = record.withdraw_amount - record.withdraw_paid;
        drip::share(snap_free, remaining, snap_open).core()?
    };
    let capacity = pool_core::capacity(pool.total_staked, pool.total_backed).core()?;
    Ok(Limits {
        due: unlocked.saturating_sub(record.withdraw_paid),
        share,
        free_now: capacity.saturating_sub(pool.total_allocated),
        snap_free,
    })
}

/// What can leave on this call: the smallest of the limits. Errors say which limit is hit. Takes the
/// day's snapshot and fixes the request's day share when the day has turned.
fn collectable(pool: &mut Pool, record: &mut BackerRecord, now: i64) -> Result<u64> {
    let day = pool_core::claim::day_of(now);
    let snap = drip_snap(pool, day)?;
    let l = limits(pool, record, now, snap)?;
    record.share_left = l.share;
    record.share_day = day;
    require!(l.due > 0, PoolError::NothingToCollectYet);
    require!(
        l.snap_free > 0 && l.free_now > 0,
        PoolError::BackerCapitalNotFree
    );
    let pay = l.due.min(l.share).min(l.free_now);
    require!(pay > 0, PoolError::NothingToCollectYet);
    Ok(pay)
}

/// What `complete_backer_withdrawal` would pay now, before any venue pull (0 if nothing). Writes
/// nothing.
pub(crate) fn collectable_view(pool: &Pool, record: &BackerRecord, now: i64) -> Result<u64> {
    if record.withdraw_amount == 0 {
        return Ok(0);
    }
    let day = pool_core::claim::day_of(now);
    let l = limits(pool, record, now, drip_values(pool, day)?)?;
    if l.snap_free == 0 {
        return Ok(0);
    }
    Ok(l.due.min(l.share).min(l.free_now))
}

fn clear_request(record: &mut BackerRecord) {
    record.withdraw_amount = 0;
    record.withdraw_paid = 0;
    record.withdraw_started_at = 0;
    record.share_day = 0;
    record.share_left = 0;
}

#[derive(Accounts)]
pub struct Back<'info> {
    /// Pays rent for a new record (see `Stake::payer`).
    #[account(mut)]
    pub payer: Signer<'info>,
    pub backer: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + BackerRecord::INIT_SPACE,
        seeds = [SEED_BACKER, pool.key().as_ref(), backer.key().as_ref()],
        bump,
    )]
    pub backer_record: Box<Account<'info, BackerRecord>>,
    /// The backer's USDC: deposits come from it and withdrawals and yield go back to it (rule 1:
    /// only to the depositing address).
    #[account(
        mut,
        constraint = backer_usdc.mint == pool.usdc_mint @ PoolError::WrongMint,
        constraint = backer_usdc.owner == backer.key() @ PoolError::WrongTokenAccount,
    )]
    pub backer_usdc: Box<Account<'info, TokenAccount>>,
    pub system_program: Program<'info, System>,
    pub cash: Cash<'info>,
}

/// Deposit. Counts from now under the default maturity; otherwise each deposit waits on its own clock.
pub fn back(ctx: &mut Context<Back>, amount: u64) -> Result<()> {
    let now = now()?;
    let vault = ctx.accounts.vault.to_account_info();
    let mut money = Money::new(
        &ctx.accounts.pool,
        ctx.accounts.pool.to_account_info(),
        vault.clone(),
        &ctx.accounts.cash,
    )?;
    let pool = &mut ctx.accounts.pool;
    require!(!pool.is_paused(now), PoolError::Paused);
    require!(amount > 0, PoolError::AmountNotPositive);
    money.harvest(pool, now)?;
    let record = &mut ctx.accounts.backer_record;
    open_record(
        pool,
        record,
        ctx.accounts.backer.key(),
        ctx.bumps.backer_record,
    );
    add_backing(pool, record, amount, now)?;

    vault::receive(
        pool,
        &ctx.accounts.cash,
        &ctx.accounts.backer_usdc.to_account_info(),
        &ctx.accounts.backer.to_account_info(),
        &vault,
        amount,
    )?;
    money.push_idle(pool, now)?;
    Ok(())
}

#[derive(Accounts)]
pub struct MatureBacking<'info> {
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_BACKER, pool.key().as_ref(), backer_record.backer.as_ref()], bump = backer_record.bump)]
    pub backer_record: Box<Account<'info, BackerRecord>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    pub cash: Cash<'info>,
}

/// Permissionless. Until someone calls it, capacity is undercounted (the safe direction). Backing
/// that matured is new capacity, so idle cash above the line goes to the venue (multichain).
pub fn mature_backing(ctx: &mut Context<MatureBacking>) -> Result<()> {
    let now = now()?;
    let mut money = Money::new(
        &ctx.accounts.pool,
        ctx.accounts.pool.to_account_info(),
        ctx.accounts.vault.to_account_info(),
        &ctx.accounts.cash,
    )?;
    let pool = &mut ctx.accounts.pool;
    let record = &mut ctx.accounts.backer_record;
    require!(record.pending_count > 0, PoolError::NoPendingBacking);
    money.harvest(pool, now)?;
    let moved = settle(pool, record, now)?;
    require!(moved > 0, PoolError::BackingNotMature);
    money.push_idle(pool, now)?;
    Ok(())
}

#[derive(Accounts)]
pub struct RequestBackerWithdrawal<'info> {
    pub backer: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(
        mut,
        seeds = [SEED_BACKER, pool.key().as_ref(), backer.key().as_ref()],
        bump = backer_record.bump,
        has_one = backer,
    )]
    pub backer_record: Box<Account<'info, BackerRecord>>,
}

#[derive(Accounts)]
pub struct BackerOnly<'info> {
    pub backer: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(
        mut,
        seeds = [SEED_BACKER, pool.key().as_ref(), backer.key().as_ref()],
        bump = backer_record.bump,
        has_one = backer,
    )]
    pub backer_record: Box<Account<'info, BackerRecord>>,
}

/// Opens a withdrawal of counted money; it keeps counting until paid (rule 2). The event carries an
/// estimate of the last drip day at today's utilisation. Works while paused.
pub fn request_backer_withdrawal(
    ctx: &mut Context<RequestBackerWithdrawal>,
    amount: u64,
) -> Result<()> {
    let now = now()?;
    require!(amount > 0, PoolError::AmountNotPositive);
    let pool = &mut ctx.accounts.pool;
    let record = &mut ctx.accounts.backer_record;
    require!(
        record.withdraw_amount == 0,
        PoolError::BackerWithdrawalPending
    );
    settle(pool, record, now)?;
    require!(
        amount <= record.amount,
        PoolError::BackerAmountExceedsBalance
    );
    let (_, _, util) = drip_snap(pool, pool_core::claim::day_of(now))?;
    let last_day = drip_days(pool, util)? - 1;
    let ready_at = (last_day as i64)
        .checked_mul(SECONDS_PER_DAY)
        .and_then(|d| now.checked_add(d))
        .ok_or(PoolError::MathOverflow)?;
    clear_request(record);
    record.withdraw_amount = amount;
    record.withdraw_started_at = now;
    pool.total_backer_open = add(pool.total_backer_open, amount).core()?;
    emit!(BackerWithdrawalRequested {
        backer: record.backer,
        amount,
        ready_at
    });
    Ok(())
}

/// Stops the drip; the unpaid part stays in the pool. Works while paused.
pub fn cancel_backer_withdrawal(ctx: &mut Context<BackerOnly>) -> Result<()> {
    let pool = &mut ctx.accounts.pool;
    let record = &mut ctx.accounts.backer_record;
    require!(record.withdraw_amount > 0, PoolError::NoBackerWithdrawal);
    let unpaid = record.withdraw_amount - record.withdraw_paid;
    clear_request(record);
    pool.total_backer_open = pool.total_backer_open.saturating_sub(unpaid);
    emit!(BackerWithdrawalCancelled {
        backer: record.backer,
        amount: unpaid
    });
    Ok(())
}

#[derive(Accounts)]
pub struct CompleteBackerWithdrawal<'info> {
    #[account(mut)]
    pub backer: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(
        mut,
        seeds = [SEED_BACKER, pool.key().as_ref(), backer.key().as_ref()],
        bump = backer_record.bump,
        has_one = backer,
    )]
    pub backer_record: Box<Account<'info, BackerRecord>>,
    /// The backer's USDC: deposits come from it and withdrawals and yield go back to it (rule 1:
    /// only to the depositing address).
    #[account(
        mut,
        constraint = backer_usdc.mint == pool.usdc_mint @ PoolError::WrongMint,
        constraint = backer_usdc.owner == backer.key() @ PoolError::WrongTokenAccount,
    )]
    pub backer_usdc: Box<Account<'info, TokenAccount>>,
    pub cash: Cash<'info>,
}

/// Collects what the drip allows today (rule 5), plus any unpaid yield. On failure nothing changes;
/// the backer retries later. The request closes once all of it is paid. Works while paused.
pub fn complete_backer_withdrawal(ctx: &mut Context<CompleteBackerWithdrawal>) -> Result<()> {
    let now = now()?;
    let mut money = Money::new(
        &ctx.accounts.pool,
        ctx.accounts.pool.to_account_info(),
        ctx.accounts.vault.to_account_info(),
        &ctx.accounts.cash,
    )?;
    let pool = &mut ctx.accounts.pool;
    let record = &mut ctx.accounts.backer_record;
    require!(record.withdraw_amount > 0, PoolError::NoBackerWithdrawal);
    money.harvest(pool, now)?;
    let pay = collectable(pool, record, now)?;

    settle(pool, record, now)?;
    let yield_paid = record.yield_owed.min(pool.backer_yield_reserved);
    record.yield_owed -= yield_paid;
    pool.backer_yield_reserved -= yield_paid;
    record.amount = sub(record.amount, pay).core()?;
    record.withdraw_paid += pay;
    record.share_left -= pay;
    if record.withdraw_paid >= record.withdraw_amount {
        clear_request(record);
    }
    pool.total_backed = sub(pool.total_backed, pay).core()?;
    pool.total_backer_open = pool.total_backer_open.saturating_sub(pay);
    emit!(BackerWithdrawn {
        backer: record.backer,
        amount: pay,
        yield_paid
    });

    let total = add(pay, yield_paid).core()?;
    let to = ctx.accounts.backer_usdc.to_account_info();
    money.pay(pool, &to, total, now)?;
    // Re-check that the capital that left was still free afterwards, or the whole payment reverts
    // (a venue withdrawal that books a loss must never let it pass).
    let capacity = pool_core::capacity(pool.total_staked, pool.total_backed).core()?;
    pool_core::check_capital_free(pool.total_allocated, capacity, 0).core()?;
    Ok(())
}

#[derive(Accounts)]
pub struct ClaimBackerYield<'info> {
    #[account(mut)]
    pub backer: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(
        mut,
        seeds = [SEED_BACKER, pool.key().as_ref(), backer.key().as_ref()],
        bump = backer_record.bump,
        has_one = backer,
    )]
    pub backer_record: Box<Account<'info, BackerRecord>>,
    /// The backer's USDC: deposits come from it and withdrawals and yield go back to it (rule 1:
    /// only to the depositing address).
    #[account(
        mut,
        constraint = backer_usdc.mint == pool.usdc_mint @ PoolError::WrongMint,
        constraint = backer_usdc.owner == backer.key() @ PoolError::WrongTokenAccount,
    )]
    pub backer_usdc: Box<Account<'info, TokenAccount>>,
    pub cash: Cash<'info>,
}

/// Backer yield, any time, paused or not; principal untouched (multichain `claim_backer_yield`).
pub fn claim_backer_yield(ctx: &mut Context<ClaimBackerYield>) -> Result<()> {
    let now = now()?;
    let mut money = Money::new(
        &ctx.accounts.pool,
        ctx.accounts.pool.to_account_info(),
        ctx.accounts.vault.to_account_info(),
        &ctx.accounts.cash,
    )?;
    let pool = &mut ctx.accounts.pool;
    let record = &mut ctx.accounts.backer_record;
    money.harvest(pool, now)?;
    settle(pool, record, now)?;
    let paid = record.yield_owed.min(pool.backer_yield_reserved);
    require!(paid > 0, PoolError::NothingToClaim);
    record.yield_owed -= paid;
    pool.backer_yield_reserved -= paid;
    emit!(YieldClaimed {
        owner: record.backer,
        amount: paid,
        backer: true
    });

    let to = ctx.accounts.backer_usdc.to_account_info();
    money.pay(pool, &to, paid, now)
}
