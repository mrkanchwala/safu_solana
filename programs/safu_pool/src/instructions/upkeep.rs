//! Protocol revenue: `withdraw_yield` (multichain `vault.rs` `withdraw_yield`).

use anchor_lang::prelude::*;
use anchor_spl::token::TokenAccount;
use pool_core::{add, sub, yields};

use crate::constants::{SEED_POOL, SEED_VAULT};
use crate::errors::{CoreResultExt, PoolError};
use crate::events::YieldWithdrawn;
// Glob: `#[derive(Accounts)]` on a struct holding `Cash` needs its generated client modules.
use crate::vault::{self, *};
use crate::state::Pool;

#[derive(Accounts)]
pub struct WithdrawYield<'info> {
    pub admin: Signer<'info>,
    #[account(
        mut,
        seeds = [SEED_POOL],
        bump = pool.bump,
        has_one = admin @ PoolError::NotAdmin,
    )]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    /// The treasury's USDC account (any token account `pool.treasury` owns).
    #[account(
        mut,
        constraint = treasury_usdc.mint == pool.usdc_mint @ PoolError::WrongMint,
        constraint = treasury_usdc.owner == pool.treasury @ PoolError::WrongTokenAccount,
    )]
    pub treasury_usdc: Box<Account<'info, TokenAccount>>,
    pub cash: Cash<'info>,
}

/// Protocol revenue to the treasury. Within the protocol's balance AND the pool's surplus over
/// everything it owes (stakes, backing, set-aside yield, open claims), because claims may have spent
/// protocol revenue that the balance still counts (as multichain). Not while paused
/// for the same reason: stakers and backers cannot claim yield then.
pub fn withdraw_yield(ctx: Context<WithdrawYield>, amount: u64) -> Result<()> {
    let pool_info = ctx.accounts.pool.to_account_info();
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
    let to = ctx.accounts.treasury_usdc.to_account_info();
    vault::pay_out(pool, &pool_info, &ctx.accounts.cash, &vault, &to, amount)
}
