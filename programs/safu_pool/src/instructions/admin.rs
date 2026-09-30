//! Setup and admin: initialize, pause, unpause. The pool cap and every other adjustable number
//! change through the settings timelock (`settings.rs`).

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use super::now;
use crate::constants::{CLUSTER_MAINNET, NATIVE_SOL_MINT, SEED_MSOL, SEED_POOL, SEED_VAULT};
use crate::errors::PoolError;
use crate::events::{PausedUntil, PoolInitialized, Unpaused};
use crate::marinade;
use crate::state::{PendingSetting, Pool, ACCOUNT_VERSION};
use crate::vault;
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
    // Default settings at this cap; a cap too small for a non-zero min stake is refused.
    let settings =
        Settings::defaults(args.pool_cap).map_err(|_| error!(PoolError::InvalidPoolCap))?;

    let state = marinade::read_state(
        &ctx.accounts.marinade_state,
        &ctx.accounts.marinade_program.key(),
    )?;
    require_keys_eq!(
        state.msol_mint,
        ctx.accounts.msol_mint.key(),
        PoolError::WrongMarinadeAccount
    );

    // The vault must exist and stay rent-exempt: fund its floor once, here.
    let floor = vault::rent_floor()?;
    let top_up = floor.saturating_sub(ctx.accounts.vault.lamports());
    if top_up > 0 {
        vault::receive(
            &ctx.accounts.admin.to_account_info(),
            &ctx.accounts.vault.to_account_info(),
            top_up,
        )?;
    }

    let pool = &mut ctx.accounts.pool;
    pool.set_inner(Pool {
        version: ACCOUNT_VERSION,
        asset_mint: NATIVE_SOL_MINT,
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
        deployed_msol: 0,
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
/// pause only once the `PauseGapSecs` setting has passed since it ended (audit X2: back-to-back
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
