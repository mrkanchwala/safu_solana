//! Claim rules (multichain `claim.rs`): tier ceiling, hack-time window, approval deadline,
//! admission (solvency + daily stress cap), oracle daily limit, vesting, daily payout cap.

use crate::params::*;
use crate::{add, apply_bps, mul_div_floor, CoreError, Result};

/// Coverage ratio of a tier code (1 = A, 2 = B, 3 = C).
pub fn tier_ratio(tier: u8) -> Result<u64> {
    match tier {
        TIER_A => Ok(TIER_A_RATIO),
        TIER_B => Ok(TIER_B_RATIO),
        TIER_C => Ok(TIER_C_RATIO),
        _ => Err(CoreError::InvalidTier),
    }
}

/// Most a claim on `stake` can pay: `stake x ratio x TIER_COVERAGE_BPS / BPS`.
pub fn tier_cap(stake: u64, tier: u8) -> Result<u64> {
    let raw = stake.checked_mul(tier_ratio(tier)?).ok_or(CoreError::Overflow)?;
    apply_bps(raw, TIER_COVERAGE_BPS)
}

/// Entitlement is positive and within the tier ceiling.
pub fn check_entitlement(entitlement: u64, stake: u64, tier: u8) -> Result<()> {
    if entitlement == 0 {
        return Err(CoreError::EntitlementNotPositive);
    }
    if entitlement > tier_cap(stake, tier)? {
        return Err(CoreError::EntitlementExceedsTierCap);
    }
    Ok(())
}

/// The hack is not in the future, not before the stake, and inside the claim window.
pub fn check_hack_time(hack_ts: i64, staked_at: i64, now: i64) -> Result<()> {
    if hack_ts > now {
        return Err(CoreError::HackTimestampInFuture);
    }
    if hack_ts < staked_at {
        return Err(CoreError::HackPredatesStake);
    }
    if now > hack_ts.checked_add(CLAIM_WINDOW_SECS).ok_or(CoreError::Overflow)? {
        return Err(CoreError::ClaimWindowExpired);
    }
    Ok(())
}

/// An oracle approval is still open and no longer-lived than `MAX_APPROVAL_WINDOW_SECS`.
pub fn check_deadline(deadline: i64, now: i64) -> Result<()> {
    if now > deadline {
        return Err(CoreError::SignatureExpired);
    }
    if deadline > now.checked_add(MAX_APPROVAL_WINDOW_SECS).ok_or(CoreError::Overflow)? {
        return Err(CoreError::SignatureDeadlineTooFar);
    }
    Ok(())
}

/// Utilisation band of `allocated` against `base`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    Low,
    Mid,
    High,
}

pub fn band(allocated: u64, base: u64) -> Result<Band> {
    let util = mul_div_floor(allocated, BPS_DENOMINATOR, base)?;
    Ok(if util < UTIL_LOW_BPS {
        Band::Low
    } else if util < UTIL_MID_BPS {
        Band::Mid
    } else {
        Band::High
    })
}

/// New entitlement admitted per day: a band of capacity. Zero on an empty pool.
pub fn stress_cap(capacity: u64, allocated: u64) -> Result<u64> {
    if capacity == 0 {
        return Ok(0);
    }
    let bps = match band(allocated, capacity)? {
        Band::Low => ADMIT_LOW_BPS,
        Band::Mid => ADMIT_MID_BPS,
        Band::High => ADMIT_HIGH_BPS,
    };
    apply_bps(capacity, bps)
}

/// A claim fits now: the pool stays solvent and today's admissions stay under the stress cap.
/// `false` means queue, not reject.
pub fn admits(entitlement: u64, capacity: u64, allocated: u64, day_admitted: u64) -> Result<bool> {
    let solvent = add(allocated, entitlement)? <= capacity;
    let under_cap = add(day_admitted, entitlement)? <= stress_cap(capacity, allocated)?;
    Ok(solvent && under_cap)
}

/// Oracle submissions per day: `total_stakers / ORACLE_DAILY_LIMIT_DIVISOR`, at least one.
pub fn oracle_daily_limit(total_stakers: u64) -> u64 {
    (total_stakers / ORACLE_DAILY_LIMIT_DIVISOR).max(1)
}

/// The stake has been in long enough for its claim to be approved.
pub fn time_gate_met(staked_at: i64, now: i64) -> bool {
    now.saturating_sub(staked_at) >= TIME_GATE_SECS
}

/// Amount vested by `now`: linear from `cooldown_ends` to `vesting_ends`, rounded down.
pub fn vested(entitlement: u64, cooldown_ends: i64, vesting_ends: i64, now: i64) -> Result<u64> {
    let len = vesting_ends.saturating_sub(cooldown_ends);
    if len <= 0 {
        return Ok(entitlement);
    }
    let elapsed = now.min(vesting_ends).saturating_sub(cooldown_ends).max(0);
    mul_div_floor(entitlement, elapsed as u64, len as u64)
}

/// Claim payouts allowed today: a band of `base` (the caller passes max(capacity now, capacity
/// when the claim was approved), which is what keeps a shrinking pool from throttling old claims).
pub fn payout_cap(base: u64, allocated: u64) -> Result<u64> {
    if base == 0 {
        // As multichain (`dynamic_outflow_bps` returns 100 bps here): a rate of nothing is nothing.
        // Unreachable from `claim_stream`, whose base includes the claim's non-zero capacity snapshot.
        return apply_bps(base, PAYOUT_EMPTY_POOL_BPS);
    }
    let bps = match band(allocated, base)? {
        Band::Low => PAYOUT_LOW_BPS,
        Band::Mid => PAYOUT_MID_BPS,
        Band::High => PAYOUT_HIGH_BPS,
    };
    apply_bps(base, bps)
}

/// Day number of a unix time; daily counters reset when it changes.
pub fn day_of(now: i64) -> i64 {
    now.div_euclid(SECONDS_PER_DAY)
}
