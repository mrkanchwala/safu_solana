//! Adjustable settings (multichain `settings.rs`): admin proposes → co-signer approves the same
//! value, starting `SETTINGS_TIMELOCK_SECS` → anyone executes. Either signer can cancel before
//! execution. Every step emits an event. Which numbers, their bounds and defaults:
//! `pool_core::settings`.

use anchor_lang::prelude::*;
use pool_core::params::SETTINGS_TIMELOCK_SECS;
use pool_core::settings::SettingKey;

use super::admin::AdminOnly;
use super::now;
use crate::constants::SEED_POOL;
use crate::errors::{CoreResultExt, PoolError};
use crate::events::{SettingApproved, SettingCancelled, SettingExecuted, SettingProposed};
use crate::state::{PendingSetting, Pool, PENDING_APPROVED, PENDING_NONE, PENDING_PROPOSED};

#[derive(Accounts)]
pub struct ApproveSetting<'info> {
    pub co_signer: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump, has_one = co_signer @ PoolError::NotAdminOrCoSigner)]
    pub pool: Box<Account<'info, Pool>>,
}

#[derive(Accounts)]
pub struct ExecuteSetting<'info> {
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
}

#[derive(Accounts)]
pub struct CancelSetting<'info> {
    pub signer: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
}

/// Step 1 (admin): the value must fit now. Replaces an earlier pending change for the same setting,
/// which also drops its approval and clock.
pub fn propose_setting(ctx: Context<AdminOnly>, key: u8, value: i64) -> Result<()> {
    let k = SettingKey::from_index(key).core()?;
    let pool = &mut ctx.accounts.pool;
    pool.settings().check_value(k, value).core()?;
    pool.pending_settings[k.index()] = PendingSetting { value, eta: 0, state: PENDING_PROPOSED };
    emit!(SettingProposed { key, value });
    Ok(())
}

/// Step 2 (co-signer): approves the exact proposed value and starts the timelock.
pub fn approve_setting(ctx: Context<ApproveSetting>, key: u8, value: i64) -> Result<()> {
    let k = SettingKey::from_index(key).core()?;
    let eta = now()?.checked_add(SETTINGS_TIMELOCK_SECS).ok_or(PoolError::MathOverflow)?;
    let pending = &mut ctx.accounts.pool.pending_settings[k.index()];
    require!(pending.state != PENDING_NONE, PoolError::NoPendingSetting);
    require!(pending.state == PENDING_PROPOSED, PoolError::SettingAlreadyApproved);
    require!(pending.value == value, PoolError::SettingValueMismatch);
    pending.state = PENDING_APPROVED;
    pending.eta = eta;
    emit!(SettingApproved { key, value, eta });
    Ok(())
}

/// Step 3 (anyone): once approved and the timelock has passed. Bounds and the order between
/// settings are checked again against the live values.
pub fn execute_setting(ctx: Context<ExecuteSetting>, key: u8) -> Result<()> {
    let k = SettingKey::from_index(key).core()?;
    let now = now()?;
    let pool = &mut ctx.accounts.pool;
    let pending = pool.pending_settings[k.index()];
    require!(pending.state != PENDING_NONE, PoolError::NoPendingSetting);
    require!(pending.state == PENDING_APPROVED && now >= pending.eta, PoolError::SettingNotReady);
    pool.settings().check_value(k, pending.value).core()?;
    let old_value = pool.settings[k.index()];
    pool.settings[k.index()] = pending.value;
    pool.pending_settings[k.index()] = PendingSetting::default();
    emit!(SettingExecuted { key, old_value, new_value: pending.value });
    Ok(())
}

/// Admin or co-signer drops a pending change.
pub fn cancel_setting(ctx: Context<CancelSetting>, key: u8) -> Result<()> {
    let k = SettingKey::from_index(key).core()?;
    let by = ctx.accounts.signer.key();
    let pool = &mut ctx.accounts.pool;
    require!(by == pool.admin || by == pool.co_signer, PoolError::NotAdminOrCoSigner);
    require!(pool.pending_settings[k.index()].state != PENDING_NONE, PoolError::NoPendingSetting);
    pool.pending_settings[k.index()] = PendingSetting::default();
    emit!(SettingCancelled { key, by });
    Ok(())
}
