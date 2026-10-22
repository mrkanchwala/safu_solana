//! The yield venue's own calls (multichain `harvest`, `auto_deploy_liquidity`, `ensure_liquidity`,
//! `provide_liquidity`), plus the one-time collateral account. Every user call that moves money
//! already harvests, pushes and pulls on its own (`leg::Money`); these are for keepers and the admin.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use super::now;
use crate::constants::{SEED_LEG, SEED_POOL, SEED_VAULT};
use crate::errors::PoolError;
use crate::kamino::check_venue;
use crate::leg::Money;
use crate::state::Pool;
// Glob: `#[derive(Accounts)]` on a struct holding `Cash` needs its generated client modules.
use crate::vault::*;

#[derive(Accounts)]
pub struct InitYieldAccount<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    /// CHECK: must be `pool.yield_venue`, a klend USDC reserve (`check_venue`).
    #[account(address = pool.yield_venue @ PoolError::VenueNotSet)]
    pub reserve: UncheckedAccount<'info>,
    pub collateral_mint: Box<Account<'info, Mint>>,
    #[account(
        init,
        payer = payer,
        seeds = [SEED_LEG, pool.key().as_ref(), collateral_mint.key().as_ref()],
        bump,
        token::mint = collateral_mint,
        token::authority = pool,
        token::token_program = token_program,
    )]
    pub pool_collateral: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

/// Permissionless: opens the pool's collateral account for the venue's collateral mint. Until it
/// exists, the pool deploys nothing.
pub fn init_yield_account(ctx: Context<InitYieldAccount>) -> Result<()> {
    let reserve = check_venue(&ctx.accounts.pool, &ctx.accounts.reserve.to_account_info())?;
    require_keys_eq!(
        ctx.accounts.collateral_mint.key(),
        Pubkey::new_from_array(reserve.collateral_mint),
        PoolError::YieldAccountsMissing
    );
    Ok(())
}

#[derive(Accounts)]
pub struct LegCall<'info> {
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    pub cash: Cash<'info>,
}

#[derive(Accounts)]
pub struct ProvideLiquidity<'info> {
    pub admin: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, has_one = admin @ PoolError::NotAdmin)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    pub cash: Cash<'info>,
}

fn money<'a, 'info>(
    pool: &Account<'info, Pool>,
    vault: &Account<'info, TokenAccount>,
    cash: &'a Cash<'info>,
) -> Result<Money<'a, 'info>> {
    Money::new(pool, pool.to_account_info(), vault.to_account_info(), cash)
}

/// Permissionless, best effort: takes the venue's growth above book value as yield.
pub fn harvest(ctx: Context<LegCall>) -> Result<()> {
    let now = now()?;
    let mut m = money(&ctx.accounts.pool, &ctx.accounts.vault, &ctx.accounts.cash)?;
    m.harvest(&mut ctx.accounts.pool, now)?;
    Ok(())
}

/// Permissionless: deposits idle cash up to the `deploy_bps` line.
pub fn auto_deploy_liquidity(ctx: Context<LegCall>) -> Result<()> {
    let now = now()?;
    let mut m = money(&ctx.accounts.pool, &ctx.accounts.vault, &ctx.accounts.cash)?;
    m.keeper_deploy(&mut ctx.accounts.pool, now)?;
    Ok(())
}

/// Permissionless: brings cash back for open claims, or deployment back under the line.
pub fn ensure_liquidity(ctx: Context<LegCall>) -> Result<()> {
    let now = now()?;
    let mut m = money(&ctx.accounts.pool, &ctx.accounts.vault, &ctx.accounts.cash)?;
    m.keeper_rebalance(&mut ctx.accounts.pool, now)?;
    Ok(())
}

/// Admin: redeems `collateral`, at least `min_out` USDC back. Works while paused.
pub fn provide_liquidity(
    ctx: Context<ProvideLiquidity>,
    collateral: u64,
    min_out: u64,
) -> Result<()> {
    let now = now()?;
    let mut m = money(&ctx.accounts.pool, &ctx.accounts.vault, &ctx.accounts.cash)?;
    m.admin_provide(&mut ctx.accounts.pool, collateral, min_out, now)?;
    Ok(())
}
