//! Setup and admin: initialize, pause, unpause, raise the pool cap.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use super::now;
use crate::constants::{CLUSTER_MAINNET, SEED_MSOL, SEED_POOL, SEED_VAULT};
use crate::errors::{CoreResultExt, PoolError};
use crate::events::{PausedUntil, PoolCapRaised, PoolInitialized, Unpaused};
use crate::marinade;
use crate::state::Pool;
use crate::vault;
use pool_core::params::PAUSE_MAX_SECS;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct InitArgs {
    pub co_signer: Pubkey,
    pub oracle: Pubkey,
    pub registry_writer: Pubkey,
    pub treasury: Pubkey,
    pub cluster: u8,
    pub pool_cap: u64,
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    /// This program. Proves `program_data` belongs to it, so only its upgrade authority can initialize.
    #[account(constraint = program.programdata_address()? == Some(program_data.key()) @ PoolError::NotUpgradeAuthority)]
    pub program: Program<'info, crate::program::SafuPool>,
    #[account(
        constraint = program_data.upgrade_authority_address == Some(admin.key()) @ PoolError::NotUpgradeAuthority,
    )]
    pub program_data: Account<'info, ProgramData>,
    #[account(init, payer = admin, space = 8 + Pool::INIT_SPACE, seeds = [SEED_POOL], bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump)]
    pub vault: SystemAccount<'info>,
    /// CHECK: must be executable and the owner of `marinade_state` (checked in the handler).
    #[account(executable)]
    pub marinade_program: UncheckedAccount<'info>,
    /// CHECK: owner, discriminator and length checked by `marinade::read_state`.
    pub marinade_state: UncheckedAccount<'info>,
    pub msol_mint: Box<Account<'info, Mint>>,
    #[account(
        init,
        payer = admin,
        seeds = [SEED_MSOL, pool.key().as_ref()],
        bump,
        token::mint = msol_mint,
        token::authority = vault,
        token::token_program = token_program,
    )]
    pub pool_msol: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn initialize(ctx: Context<Initialize>, args: InitArgs) -> Result<()> {
    let admin = ctx.accounts.admin.key();
    require!(
        admin != args.oracle && admin != args.co_signer && args.oracle != args.co_signer,
        PoolError::RoleCollision
    );
    require!(args.cluster <= CLUSTER_MAINNET, PoolError::InvalidCluster);
    let (min_stake, _) = pool_core::stake::bounds(args.pool_cap).core()?;
    require!(min_stake > 0, PoolError::InvalidPoolCap);

    let state = marinade::read_state(&ctx.accounts.marinade_state, &ctx.accounts.marinade_program.key())?;
    require_keys_eq!(state.msol_mint, ctx.accounts.msol_mint.key(), PoolError::WrongMarinadeAccount);

    // The vault must exist and stay rent-exempt: fund its floor once, here.
    let floor = vault::rent_floor()?;
    let top_up = floor.saturating_sub(ctx.accounts.vault.lamports());
    if top_up > 0 {
        vault::receive(&ctx.accounts.admin.to_account_info(), &ctx.accounts.vault.to_account_info(), top_up)?;
    }

    let pool = &mut ctx.accounts.pool;
    pool.set_inner(Pool {
        admin,
        co_signer: args.co_signer,
        oracle: args.oracle,
        registry_writer: args.registry_writer,
        treasury: args.treasury,
        marinade_program: ctx.accounts.marinade_program.key(),
        marinade_state: ctx.accounts.marinade_state.key(),
        msol_mint: ctx.accounts.msol_mint.key(),
        pool_msol: ctx.accounts.pool_msol.key(),
        cluster: args.cluster,
        bump: ctx.bumps.pool,
        vault_bump: ctx.bumps.vault,
        pool_cap: args.pool_cap,
        paused_until: 0,
        total_staked: 0,
        total_stakers: 0,
        total_backed: 0,
        total_backed_pending: 0,
        total_allocated: 0,
        staker_yield_index: 0,
        backer_yield_index: 0,
        staker_yield_reserved: 0,
        backer_yield_reserved: 0,
        protocol_yield_balance: 0,
        total_extracted_yield: 0,
        deployed_msol: 0,
        deployed_book: 0,
        last_harvest_at: 0,
        day: 0,
        day_admitted: 0,
        day_oracle_count: 0,
        day_outflow: 0,
    });
    emit!(PoolInitialized { admin, pool_cap: args.pool_cap, cluster: args.cluster });
    Ok(())
}

#[derive(Accounts)]
pub struct AdminOnly<'info> {
    pub admin: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, has_one = admin @ PoolError::NotAdmin)]
    pub pool: Box<Account<'info, Pool>>,
}

/// Pauses for at most `PAUSE_MAX_SECS`.
pub fn pause(ctx: Context<AdminOnly>) -> Result<()> {
    let until = now()?.checked_add(PAUSE_MAX_SECS).ok_or(PoolError::MathOverflow)?;
    ctx.accounts.pool.paused_until = until;
    emit!(PausedUntil { until });
    Ok(())
}

pub fn unpause(ctx: Context<AdminOnly>) -> Result<()> {
    require!(ctx.accounts.pool.is_paused(now()?), PoolError::NotPaused);
    ctx.accounts.pool.paused_until = 0;
    emit!(Unpaused {});
    Ok(())
}

/// Increase only (multichain `set_pool_cap`).
pub fn set_pool_cap(ctx: Context<AdminOnly>, pool_cap: u64) -> Result<()> {
    require!(pool_cap > ctx.accounts.pool.pool_cap, PoolError::PoolCapNotIncreased);
    ctx.accounts.pool.pool_cap = pool_cap;
    emit!(PoolCapRaised { pool_cap });
    Ok(())
}
