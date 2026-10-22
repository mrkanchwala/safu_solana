//! Backer exit drip and the fair daily shares (multichain `backer.rs` drip, `claim.rs`
//! `daily_share`). Pure arithmetic; the program keeps the day snapshots.

use crate::params::{BPS_DENOMINATOR, DRIP_FULL_UTIL_BPS, SECONDS_PER_DAY};
use crate::{mul_div_ceil, mul_div_floor, Result};

/// Utilisation, bps: `allocated / capacity`. Zero on an empty pool.
pub fn utilisation_bps(allocated: u64, capacity: u64) -> Result<u64> {
    if capacity == 0 {
        return Ok(0);
    }
    mul_div_floor(allocated, BPS_DENOMINATOR, capacity)
}

/// Drip length in whole days (at least 1): `min_secs` on an idle pool, rising in a straight line to
/// `max_secs` at `DRIP_FULL_UTIL_BPS` utilisation and above. Rounded up to a whole day.
pub fn drip_days(util_bps: u64, min_secs: i64, max_secs: i64) -> Result<u64> {
    let u = util_bps.min(DRIP_FULL_UTIL_BPS);
    let span = max_secs.saturating_sub(min_secs).max(0) as u64;
    let secs = (min_secs.max(0) as u64)
        .checked_add(mul_div_floor(span, u, DRIP_FULL_UTIL_BPS)?)
        .ok_or(crate::CoreError::Overflow)?;
    let day = SECONDS_PER_DAY as u64;
    Ok(secs.div_ceil(day).max(1))
}

/// What the drip has unlocked by day `elapsed_days` of a request for `amount` (day 0 already unlocks
/// one day's part): `amount × (elapsed + 1) / days`, never above `amount`.
pub fn unlocked(amount: u64, elapsed_days: u64, days: u64) -> Result<u64> {
    let parts = elapsed_days.saturating_add(1);
    if parts >= days {
        return Ok(amount);
    }
    mul_div_floor(amount, parts, days)
}

/// One party's share of a day's pot, pro rata to what it is still owed against everything owed at
/// the day's snapshot. Anyone who joined after the snapshot is not in `owed_all`, so their own
/// remainder counts as the whole. Rounded up so nobody owed money gets a zero share.
pub fn share(pot: u64, remaining: u64, owed_all: u64) -> Result<u64> {
    let owed_all = owed_all.max(remaining);
    if owed_all == 0 {
        return Ok(0);
    }
    mul_div_ceil(pot, remaining, owed_all)
}
