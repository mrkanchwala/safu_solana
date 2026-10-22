//! Points are dollar-days (multichain `points.rs`): 1 point per USDC per day at the default rate,
//! with separate staker and backer rate settings. Each side keeps an index (rate-seconds so far), so
//! a rate change applies from that moment on and never rescales points already earned.

use crate::params::{POINTS_RATE_ONE, SECONDS_PER_DAY, USDC_UNIT};
use crate::{to_u64, CoreError, Result};

pub const STAKER: usize = 0;
pub const BACKER: usize = 1;
pub const SIDES: usize = 2;

/// A side's index at `now`: the stored value plus the live `rate` for each second since it was
/// stored. Does not move until the side's first checkpoint (`at` = 0).
pub fn index(stored: u128, at: i64, rate: u64, now: i64) -> Result<u128> {
    if at == 0 {
        return Ok(stored);
    }
    let elapsed = now.saturating_sub(at).max(0) as u128;
    (rate as u128)
        .checked_mul(elapsed)
        .and_then(|g| stored.checked_add(g))
        .ok_or(CoreError::Overflow)
}

/// Whole points earned by `amount` (USDC base units) between two index values. Rounded down.
pub fn earned(amount: u64, from: u128, to: u128) -> Result<u64> {
    if amount == 0 || to <= from {
        return Ok(0);
    }
    let per_point = USDC_UNIT as u128 * POINTS_RATE_ONE as u128 * SECONDS_PER_DAY as u128;
    let v = (amount as u128)
        .checked_mul(to - from)
        .ok_or(CoreError::Overflow)?;
    to_u64(v / per_point)
}
