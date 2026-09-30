//! Liquid SOL vs the Marinade leg (multichain `vault.rs` push / pull).

use crate::params::{AUTO_PUSH_MIN_BPS, BPS_DENOMINATOR, DEPLOY_BPS};
use crate::{apply_bps, sub, Result};

/// Liquid SOL minus yield set aside for stakers and backers. Every payment other than a yield
/// payout, and every push into Marinade, is bounded by this.
pub fn free_liquid(liquid: u64, yield_reserved: u64) -> u64 {
    liquid.saturating_sub(yield_reserved)
}

/// SOL the pool keeps liquid: `capacity x (1 - DEPLOY_BPS)`.
pub fn buffer(capacity: u64) -> Result<u64> {
    apply_bps(capacity, sub(BPS_DENOMINATOR, DEPLOY_BPS)?)
}

/// Idle SOL to stake now: free liquid above open claims, up to the `DEPLOY_BPS` line, and only
/// once it is at least `AUTO_PUSH_MIN_BPS` of capacity (so small stakes don't each pay for a CPI).
pub fn push_amount(
    free_liquid: u64,
    total_allocated: u64,
    capacity: u64,
    deployed_book: u64,
) -> Result<u64> {
    let idle = free_liquid.saturating_sub(total_allocated);
    let room = apply_bps(capacity, DEPLOY_BPS)?.saturating_sub(deployed_book);
    let amount = idle.min(room);
    if amount == 0 || amount < apply_bps(capacity, AUTO_PUSH_MIN_BPS)? {
        return Ok(0);
    }
    Ok(amount)
}

/// SOL a rebalance unstakes (multichain `ensure_liquidity`): the larger of open claims not covered
/// by free liquid SOL, and deployment above the `DEPLOY_BPS` line (which drifts up when stakes leave).
/// The same line `push_amount` deploys to, so the two never undo each other.
pub fn rebalance_shortfall(
    free_liquid: u64,
    total_allocated: u64,
    capacity: u64,
    deployed_book: u64,
) -> Result<u64> {
    let claims = total_allocated.saturating_sub(free_liquid);
    let over_line = deployed_book.saturating_sub(apply_bps(capacity, DEPLOY_BPS)?);
    Ok(claims.max(over_line))
}
