//! Setup and admin: initialize, pause, unpause, lower the pool cap. Every other adjustable number
//! changes through the settings timelock (`settings.rs`); roles and where money goes, through
//! governance (`governance.rs`).

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use super::now;
use crate::constants::{CLUSTER_MAINNET, SEED_GOVERNANCE, SEED_POOL, SEED_VAULT};
use crate::errors::{CoreResultExt, PoolError};
use crate::events::{PausedUntil, PoolCapLowered, PoolInitialized, Unpaused};
use crate::state::{Governance, Pool, ACCOUNT_VERSION};
use pool_core::params::USDC_DECIMALS;
use pool_core::settings::{self, SettingKey, Settings};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct InitArgs {
    pub co_signer: Pubkey,
    pub guardian: Pubkey,
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
    #[account(
        init,
        payer = admin,
        space = 8 + std::mem::size_of::<Governance>(),
        seeds = [SEED_GOVERNANCE, pool.key().as_ref()],
        bump,
    )]
    pub governance: AccountLoader<'info, Governance>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn initialize(ctx: Context<Initialize>, args: InitArgs) -> Result<()> {
    let admin = ctx.accounts.admin.key();
    super::governance::check_roles_distinct(&admin, &args.co_signer, &args.guardian, &args.oracle)
        .map_err(|_| error!(PoolError::RoleCollision))?;
    require!(args.cluster <= CLUSTER_MAINNET, PoolError::InvalidCluster);
    require!(
        ctx.accounts.usdc_mint.decimals == USDC_DECIMALS,
        PoolError::WrongMint
    );
    // Default settings at this cap; a cap too small for a non-zero min stake is refused.
    let settings =
        Settings::defaults(args.pool_cap).map_err(|_| error!(PoolError::InvalidPoolCap))?;

    // `init` hands over a zeroed account: set only what is not zero. (Building the whole `Pool` in
    // one value would not fit the 4 KB stack frame.)
    let pool = &mut ctx.accounts.pool;
    pool.version = ACCOUNT_VERSION;
    pool.usdc_mint = ctx.accounts.usdc_mint.key();
    pool.vault = ctx.accounts.vault.key();
    pool.admin = admin;
    pool.co_signer = args.co_signer;
    pool.guardian = args.guardian;
    pool.oracle = args.oracle;
    pool.registry_writer = args.registry_writer;
    pool.treasury = args.treasury;
    pool.cluster = args.cluster;
    pool.bump = ctx.bumps.pool;
    pool.vault_bump = ctx.bumps.vault;
    pool.settings = settings.values;
    // Zeroed: no change pending.
    ctx.accounts.governance.load_init()?;
    emit!(PoolInitialized {
        admin,
        pool_cap: args.pool_cap,
        cluster: args.cluster
    });
    Ok(())
}

#[derive(Accounts)]
pub struct Migrate<'info> {
    /// The one account without the version check: this is how it gets current.
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
}

/// After an upgrade, brings the pool's data up to this code's layout (multichain `migrate`). Anyone
/// can call it; it runs only the fixed steps from the stored version, oldest first, and does nothing
/// the second time. Until it runs, every other instruction refuses (`NotMigrated`). Returns the
/// version.
pub fn migrate(ctx: Context<Migrate>) -> Result<u8> {
    let pool = &mut ctx.accounts.pool;
    let from = pool.version;
    require!(from <= ACCOUNT_VERSION, PoolError::NotMigrated);
    if from < ACCOUNT_VERSION {
        // Version 1 is the first layout: no steps yet. Later versions add theirs here.
        pool.version = ACCOUNT_VERSION;
        emit!(crate::events::PoolMigrated {
            from,
            to: ACCOUNT_VERSION
        });
    }
    Ok(ACCOUNT_VERSION)
}

#[derive(Accounts)]
pub struct AdminOnly<'info> {
    pub admin: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated, has_one = admin @ PoolError::NotAdmin)]
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

/// Lowers the pool cap at once: a safety brake, so no timelock (raising it goes through the
/// settings timelock). The min stake must stay above zero. Stakes already in are untouched.
pub fn lower_pool_cap(ctx: Context<AdminOnly>, value: i64) -> Result<()> {
    let pool = &mut ctx.accounts.pool;
    let s = pool.settings();
    s.check_lower_cap(value).core()?;
    let old_value = s.get(SettingKey::PoolCap);
    pool.settings[SettingKey::PoolCap.index()] = value;
    emit!(PoolCapLowered {
        old_value,
        new_value: value
    });
    Ok(())
}
