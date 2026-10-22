//! Setup and admin: initialize, pause, unpause. The pool cap and every other adjustable number
//! change through the settings timelock (`settings.rs`).

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use super::now;
use crate::constants::{CLUSTER_MAINNET, SEED_POOL, SEED_VAULT};
use crate::errors::PoolError;
use crate::events::{PausedUntil, PoolInitialized, Unpaused};
use crate::state::{PendingSetting, Pool, ACCOUNT_VERSION};
use pool_core::params::USDC_DECIMALS;
use pool_core::settings::{self, SettingKey, Settings, SETTING_SLOTS};

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
    /// USDC. `Account<Mint>` with `Program<Token>` means a classic SPL Token mint (no Token-2022
    /// transfer fees or hooks); decimals checked in the handler.
    pub usdc_mint: Box<Account<'info, Mint>>,
    #[account(
        init,
        payer = admin,
        seeds = [SEED_VAULT, pool.key().as_ref()],
        bump,
        token::mint = usdc_mint,
        token::authority = pool,
        token::token_program = token_program,
    )]
    pub vault: Box<Account<'info, TokenAccount>>,
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
    require!(
        ctx.accounts.usdc_mint.decimals == USDC_DECIMALS,
        PoolError::WrongMint
    );
    // Default settings at this cap; a cap too small for a non-zero min stake is refused.
    let settings =
        Settings::defaults(args.pool_cap).map_err(|_| error!(PoolError::InvalidPoolCap))?;

    let pool = &mut ctx.accounts.pool;
    pool.set_inner(Pool {
        version: ACCOUNT_VERSION,
        usdc_mint: ctx.accounts.usdc_mint.key(),
        vault: ctx.accounts.vault.key(),
        admin,
        co_signer: args.co_signer,
        oracle: args.oracle,
        registry_writer: args.registry_writer,
        treasury: args.treasury,
        cluster: args.cluster,
        bump: ctx.bumps.pool,
        vault_bump: ctx.bumps.vault,
        settings: settings.values,
        pending_settings: [PendingSetting::default(); SETTING_SLOTS],
        paused_until: 0,
        pause_started_at: 0,
        paused_before: 0,
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
        deployed_book: 0,
        last_harvest_at: 0,
        day: 0,
        day_admitted: 0,
        day_oracle_count: 0,
        day_outflow: 0,
        reserved: [0; 256], // hardcode-ok: spare layout bytes, not a rule
    });
    emit!(PoolInitialized {
        admin,
        pool_cap: args.pool_cap,
        cluster: args.cluster
    });
    Ok(())
}

#[derive(Accounts)]
pub struct AdminOnly<'info> {
    pub admin: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, has_one = admin @ PoolError::NotAdmin)]
    pub pool: Box<Account<'info, Pool>>,
}

/// Pauses for at most the `PauseMaxSecs` setting. Never while already paused, and after an earlier
/// pause only once the `PauseGapSecs` setting has passed since it ended (back-to-back
/// pauses would otherwise freeze claims without limit).
pub fn pause(ctx: Context<AdminOnly>) -> Result<()> {
    let now = now()?;
    let pool = &mut ctx.accounts.pool;
    require!(!pool.is_paused(now), PoolError::Paused);
    let s = pool.settings();
    require!(
        settings::pause_gap_passed(
            now,
            pool.pause_started_at,
            pool.paused_until,
            s.get(SettingKey::PauseGapSecs)
        ),
        PoolError::PauseGapNotPassed
    );
    // The last pause is over: fold it into the running total before starting a new one.
    if pool.pause_started_at != 0 {
        let last = pool.paused_until.saturating_sub(pool.pause_started_at);
        pool.paused_before = pool
            .paused_before
            .checked_add(last)
            .ok_or(PoolError::MathOverflow)?;
    }
    let until = now
        .checked_add(s.get(SettingKey::PauseMaxSecs))
        .ok_or(PoolError::MathOverflow)?;
    pool.pause_started_at = now;
    pool.paused_until = until;
    emit!(PausedUntil { until });
    Ok(())
}

/// Ends the pause now. `paused_until` keeps the end time, which the pause clock and gap read.
pub fn unpause(ctx: Context<AdminOnly>) -> Result<()> {
    let now = now()?;
    let pool = &mut ctx.accounts.pool;
    require!(pool.is_paused(now), PoolError::NotPaused);
    pool.paused_until = now;
    emit!(Unpaused {});
    Ok(())
}
