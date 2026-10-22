//! Read-only views (multichain `get_pending_yield`, `get_staker_yield_owed`, `get_points_balance`,
//! `get_backer_points`, `get_backer_yield_owed`, `collectable_view`). Run them by simulation: each
//! returns its answer as the instruction's return data and writes nothing.

use anchor_lang::prelude::*;

use super::{backer, now, stake};
use crate::constants::SEED_POOL;
use crate::state::{BackerRecord, Pool, StakeRecord};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StakerView {
    /// Yield `claim_yield` would pay now if the set-aside covers it (kept and live).
    pub yield_owed: u64,
    /// Points to date, including the part not yet banked.
    pub points: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackerView {
    pub yield_owed: u64,
    pub points: u64,
    /// What `complete_backer_withdrawal` would pay now (principal part).
    pub collectable: u64,
}

#[derive(Accounts)]
pub struct ViewStaker<'info> {
    #[account(seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    pub stake_record: Box<Account<'info, StakeRecord>>,
}

pub fn view_staker(ctx: Context<ViewStaker>) -> Result<StakerView> {
    let pool = &ctx.accounts.pool;
    let mut record = (*ctx.accounts.stake_record).clone();
    stake::settle_points(pool, &mut record, now()?)?;
    Ok(StakerView {
        yield_owed: stake::staker_owed(pool, &record)?,
        points: record.points,
    })
}

#[derive(Accounts)]
pub struct ViewBacker<'info> {
    #[account(seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    pub backer_record: Box<Account<'info, BackerRecord>>,
}

/// Counted balance only: a deposit still maturing earns from when it counts.
pub fn view_backer(ctx: Context<ViewBacker>) -> Result<BackerView> {
    let now = now()?;
    let pool = &ctx.accounts.pool;
    let record = &ctx.accounts.backer_record;
    let (yield_owed, points, _) = backer::accrued(pool, record, now)?;
    Ok(BackerView {
        yield_owed,
        points,
        collectable: backer::collectable_view(pool, record, now)?,
    })
}
