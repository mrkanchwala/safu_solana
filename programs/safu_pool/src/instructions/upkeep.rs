//! Marinade leg upkeep and protocol revenue: harvest, rebalance, withdraw_yield (multichain
//! `vault.rs` `harvest` / `ensure_liquidity` / `withdraw_yield`).

use anchor_lang::prelude::*;
use pool_core::{add, liquidity, sub, yields};

use super::now;
use crate::constants::{SEED_POOL, SEED_VAULT};
use crate::errors::{CoreResultExt, PoolError};
use crate::events::YieldWithdrawn;
use crate::leg;
// Glob: `#[derive(Accounts)]` on a struct holding `MarinadeLeg` needs its generated client modules.
use crate::marinade::*;
use crate::state::Pool;
use crate::vault;

#[derive(Accounts)]
pub struct Upkeep<'info> {
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: SystemAccount<'info>,
    pub leg: MarinadeLeg<'info>,
}

/// Permissionless. Books growth above book value as yield, up to the daily limit. A no-op when there
/// is nothing to take, as multichain.
pub fn harvest(ctx: Context<Upkeep>) -> Result<()> {
    let pool_key = ctx.accounts.pool.key();
    let vault = ctx.accounts.vault.to_account_info();
    leg::harvest(
        &mut ctx.accounts.pool,
        &pool_key,
        &ctx.accounts.leg,
        &vault,
        now()?,
    )?;
    Ok(())
}

/// Permissionless (multichain `ensure_liquidity` + `auto_deploy_liquidity`). Unstakes what open
/// claims need beyond free liquid SOL, or what sits above the `DEPLOY_BPS` line; the pool pays that
/// fee, booked as a loss if growth does not cover it. Otherwise stakes idle SOL.
pub fn rebalance(ctx: Context<Upkeep>) -> Result<()> {
    let now = now()?;
    let pool_key = ctx.accounts.pool.key();
    let vault = ctx.accounts.vault.to_account_info();
    let pool = &mut ctx.accounts.pool;
    require!(!pool.is_paused(now), PoolError::Paused);
    let capacity = pool_core::capacity(pool.total_staked, pool.total_backed).core()?;
    let free = vault::free_liquid(pool, &vault)?;
    let shortfall =
        liquidity::rebalance_shortfall(free, pool.total_allocated, capacity, pool.deployed_book)
            .core()?;
    if shortfall > 0 && pool.deployed_msol > 0 {
        leg::pull(
            pool,
            &pool_key,
            &ctx.accounts.leg,
            &vault,
            add(free, shortfall).core()?,
            false,
        )?;
        return Ok(());
    }
    let pushed = leg::push_idle(pool, &pool_key, &ctx.accounts.leg, &vault, now)?;
    require!(pushed > 0, PoolError::NothingToRebalance);
    Ok(())
}

#[derive(Accounts)]
pub struct WithdrawYield<'info> {
    pub admin: Signer<'info>,
    #[account(
        mut,
        seeds = [SEED_POOL],
        bump = pool.bump,
        has_one = admin @ PoolError::NotAdmin,
        has_one = treasury,
    )]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: SystemAccount<'info>,
    /// CHECK: must be `pool.treasury` (`has_one` above); only receives SOL.
    #[account(mut)]
    pub treasury: UncheckedAccount<'info>,
    pub leg: MarinadeLeg<'info>,
}

/// Protocol revenue to the treasury. Within the protocol's balance AND the pool's surplus over
/// everything it owes (stakes, backing, set-aside yield, open claims), because claims may have spent
/// protocol revenue that the balance still counts (as multichain). Not while paused
/// for the same reason: stakers and backers cannot claim yield then.
pub fn withdraw_yield(ctx: Context<WithdrawYield>, amount: u64) -> Result<()> {
    let pool_key = ctx.accounts.pool.key();
    let vault = ctx.accounts.vault.to_account_info();
    let pool = &mut ctx.accounts.pool;
    require!(!pool.is_paused(super::now()?), PoolError::Paused);
    require!(amount > 0, PoolError::AmountNotPositive);
    require!(
        amount <= pool.protocol_yield_balance,
        PoolError::ExceedsYieldBalance
    );
    let held = add(vault::liquid_balance(&vault)?, pool.deployed_book).core()?;
    let owed = [
        pool.total_backed,
        pool.total_backed_pending,
        pool.staker_yield_reserved,
        pool.backer_yield_reserved,
        pool.total_allocated,
    ]
    .into_iter()
    .try_fold(pool.total_staked, add)
    .core()?;
    require!(
        amount <= yields::protocol_surplus(held, owed),
        PoolError::ExceedsYieldBalance
    );

    pool.protocol_yield_balance = sub(pool.protocol_yield_balance, amount).core()?;
    emit!(YieldWithdrawn {
        treasury: pool.treasury,
        amount
    });
    let to = ctx.accounts.treasury.to_account_info();
    leg::pay_out(pool, &pool_key, &ctx.accounts.leg, &vault, &to, amount)?;
    Ok(())
}
