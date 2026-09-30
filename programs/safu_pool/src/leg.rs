//! The Marinade leg (multichain `vault.rs`): push idle SOL in, harvest growth, pull SOL out for a
//! payment, credit yield. Maths in `pool_core::leg`.
//!
//! Solana cannot catch a failed call, so the best-effort paths (push, harvest) check everything a
//! Marinade call needs first and skip if any is not met. A payment that needs an unstake either gets
//! it or fails with a typed error, and then nothing changes.

use anchor_lang::prelude::*;
use pool_core::params::MAX_REBALANCE_SLIPPAGE_BPS;
use pool_core::settings::SettingKey;
use pool_core::{add, leg, liquidity, sub, yields};

use crate::errors::{CoreResultExt, PoolError};
use crate::events::{DeployBelowFloor, Deployed, DeploymentLoss, Harvested, Unstaked, YieldCredited};
use crate::marinade::MarinadeLeg;
use crate::state::Pool;
use crate::vault;

/// The one place yield is split (multichain `credit_yield`).
pub fn credit_yield(pool: &mut Pool, amount: u64) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    let st = pool.settings();
    let c = yields::credit(
        amount,
        pool.total_staked,
        pool.total_backed,
        st.amount(SettingKey::StakerYieldBps),
        st.amount(SettingKey::BackerYieldBps),
    )
    .core()?;
    pool.staker_yield_index =
        pool.staker_yield_index.checked_add(c.staker_index_bump).ok_or(PoolError::MathOverflow)?;
    pool.backer_yield_index =
        pool.backer_yield_index.checked_add(c.backer_index_bump).ok_or(PoolError::MathOverflow)?;
    pool.staker_yield_reserved = add(pool.staker_yield_reserved, c.staker_share).core()?;
    pool.backer_yield_reserved = add(pool.backer_yield_reserved, c.backer_share).core()?;
    pool.protocol_yield_balance = add(pool.protocol_yield_balance, c.protocol_share).core()?;
    pool.total_extracted_yield = add(pool.total_extracted_yield, amount).core()?;
    emit!(YieldCredited {
        amount,
        staker_share: c.staker_share,
        backer_share: c.backer_share,
        protocol_share: c.protocol_share,
    });
    Ok(())
}

/// After SOL comes in: stake idle SOL up to the `DEPLOY_BPS` line. Best effort, returns what was
/// deposited (0 if paused here or at Marinade, or below either minimum).
pub fn push_idle<'info>(
    pool: &mut Pool,
    pool_key: &Pubkey,
    leg: &MarinadeLeg<'info>,
    vault: &AccountInfo<'info>,
    now: i64,
) -> Result<u64> {
    if pool.is_paused(now) {
        return Ok(0);
    }
    let st = leg.state(pool)?;
    if st.paused {
        return Ok(0);
    }
    let capacity = pool_core::capacity(pool.total_staked, pool.total_backed).core()?;
    let free = vault::free_liquid(pool, vault)?;
    let amount = liquidity::push_amount(free, pool.total_allocated, capacity, pool.deployed_book).core()?;
    if amount == 0 || amount < st.min_deposit {
        return Ok(0);
    }
    let before = leg.pool_msol_amount()?;
    leg.deposit(pool, pool_key, vault, amount)?;
    let msol = sub(leg.pool_msol_amount()?, before).core()?;
    // CSO M2: the harvest growth limit's clock starts with the first money deployed.
    if pool.last_harvest_at == 0 {
        pool.last_harvest_at = now;
    }
    pool.deployed_msol = add(pool.deployed_msol, msol).core()?;
    pool.deployed_book = add(pool.deployed_book, amount).core()?;
    let min_msol = leg::min_msol_for_deposit(amount, st.msol_price).core()?;
    if msol < min_msol {
        emit!(DeployBelowFloor { lamports: amount, msol, min_msol });
    }
    emit!(Deployed { lamports: amount, msol });
    Ok(amount)
}

/// Unstakes growth above book, up to the daily growth limit, and credits what arrives. Best effort,
/// skipped while paused; returns the SOL credited. Book value never changes here.
pub fn harvest<'info>(
    pool: &mut Pool,
    pool_key: &Pubkey,
    leg: &MarinadeLeg<'info>,
    vault: &AccountInfo<'info>,
    now: i64,
) -> Result<u64> {
    if pool.is_paused(now) || pool.deployed_msol == 0 || pool.deployed_book == 0 {
        return Ok(0);
    }
    if pool.last_harvest_at == 0 {
        pool.last_harvest_at = now;
        return Ok(0);
    }
    let st = leg.state(pool)?;
    if st.paused {
        return Ok(0);
    }
    let elapsed = now.saturating_sub(pool.last_harvest_at);
    let msol = leg::harvest_msol(pool.deployed_msol, pool.deployed_book, st.msol_price, elapsed).core()?;
    if msol == 0 {
        return Ok(0);
    }
    let expected = leg::msol_value(msol, st.msol_price).core()?;
    if expected < st.min_withdraw || expected > leg.sol_leg_available()? {
        return Ok(0);
    }
    let before = vault.lamports();
    leg.liquid_unstake(pool, pool_key, vault, msol)?;
    let received = sub(vault.lamports(), before).core()?;
    pool.deployed_msol = sub(pool.deployed_msol, msol).core()?;
    pool.last_harvest_at = now;
    credit_yield(pool, received)?;
    emit!(Harvested { msol, received });
    Ok(received)
}

/// Makes `amount` free liquid, unstaking the shortfall if needed. Returns the part of Marinade's fee
/// the payee pays: their share of it only (devnet default: instant unstake, payee pays). With
/// `payee_pays` false (a rebalance) the pool pays all of it. Growth that comes back is yield; the
/// pool's fee above that growth is a loss, marked off `total_staked` as multichain does.
pub fn pull<'info>(
    pool: &mut Pool,
    pool_key: &Pubkey,
    leg: &MarinadeLeg<'info>,
    vault: &AccountInfo<'info>,
    amount: u64,
    payee_pays: bool,
) -> Result<u64> {
    let free = vault::free_liquid(pool, vault)?;
    if free >= amount {
        return Ok(0);
    }
    require!(pool.deployed_msol > 0 && pool.deployed_book > 0, PoolError::InsufficientLiquidity);
    let st = leg.state(pool)?;
    require!(!st.paused, PoolError::InsufficientLiquidity);
    let shortfall = amount - free;
    let msol = leg::msol_for(shortfall, pool.deployed_book, pool.deployed_msol).core()?;
    let principal = leg::book_part(pool.deployed_book, msol, pool.deployed_msol).core()?;
    let expected = leg::msol_value(msol, st.msol_price).core()?;
    require!(
        expected >= st.min_withdraw && expected <= leg.sol_leg_available()?,
        PoolError::InsufficientLiquidity
    );

    let before = vault.lamports();
    leg.liquid_unstake(pool, pool_key, vault, msol)?;
    let received = sub(vault.lamports(), before).core()?;
    // The payee pays Marinade's fee, whatever it is up to Marinade's own maximum; the pool's own
    // unstakes (rebalance) are held to `MAX_REBALANCE_SLIPPAGE_BPS`.
    let limit_bps = if payee_pays { u64::from(st.lp_max_fee_bps) } else { MAX_REBALANCE_SLIPPAGE_BPS };
    require!(leg::fee_within_limit(expected, received, limit_bps).core()?, PoolError::UnstakeFeeTooHigh);

    pool.deployed_msol = sub(pool.deployed_msol, msol).core()?;
    pool.deployed_book = sub(pool.deployed_book, principal).core()?;
    let r = leg::settle_redeem(expected, received, principal, if payee_pays { shortfall } else { 0 }).core()?;
    credit_yield(pool, r.gain)?;
    if r.loss > 0 {
        pool.total_staked = pool.total_staked.saturating_sub(r.loss);
        emit!(DeploymentLoss { loss: r.loss });
    }
    emit!(Unstaked { msol, expected, received, payee_fee: r.payee_fee });
    Ok(r.payee_fee)
}

/// Pays `amount` to `to`, unstaking if cash is short; the payee's part of Marinade's fee comes off
/// the payment. Returns what was sent. Call after the books are updated: on any failure the whole
/// transaction reverts.
pub fn pay_out<'info>(
    pool: &mut Pool,
    pool_key: &Pubkey,
    leg: &MarinadeLeg<'info>,
    vault: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    amount: u64,
) -> Result<u64> {
    let fee = pull(pool, pool_key, leg, vault, amount, true)?;
    let sent = sub(amount, fee).core()?;
    vault::require_free_liquid(pool, vault, sent)?;
    vault::pay(pool_key, pool.vault_bump, vault, to, sent)?;
    Ok(sent)
}
