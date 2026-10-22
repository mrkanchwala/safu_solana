//! Liquid USDC vs the yield leg (multichain `vault.rs` push / pull).

use crate::params::{AUTO_PUSH_MIN_BPS, BPS_DENOMINATOR};
use crate::{apply_bps, sub, Result};

/// Cash the pool can spend: liquid USDC minus the rounding reserve. Unpaid yield is pool cash (it may
/// be deployed and may pay claims; the set-aside counters track what is still owed), so it is not
/// taken off here (multichain `free_liquid`).
pub fn free_liquid(liquid: u64, rounding_reserve: u64) -> u64 {
    liquid.saturating_sub(rounding_reserve)
}

/// What a payment of `amount` takes from the rounding reserve: the gap above free cash, only if it
/// is at most `MAX_ROUNDING_DRAW` and the reserve covers it. Otherwise nothing (the payment then
/// refuses as short of cash).
pub fn rounding_draw(amount: u64, free: u64, reserve: u64) -> u64 {
    let gap = amount.saturating_sub(free);
    if gap == 0 || gap > crate::params::MAX_ROUNDING_DRAW || gap > reserve {
        return 0;
    }
    gap
}

/// USDC the pool keeps liquid: `capacity x (1 - deploy_bps)`. `deploy_bps` is the live, governed
/// share (`Pool.deploy_bps`, at most `MAX_DEPLOY_BPS`).
pub fn buffer(capacity: u64, deploy_bps: u64) -> Result<u64> {
    apply_bps(capacity, sub(BPS_DENOMINATOR, deploy_bps)?)
}

/// Idle USDC to deploy now: free liquid above open claims, up to the `deploy_bps` line, and only
/// once it is at least `AUTO_PUSH_MIN_BPS` of capacity (so small stakes don't each pay for a CPI).
pub fn push_amount(
    free_liquid: u64,
    total_allocated: u64,
    capacity: u64,
    deployed_book: u64,
    deploy_bps: u64,
) -> Result<u64> {
    let idle = free_liquid.saturating_sub(total_allocated);
    let room = apply_bps(capacity, deploy_bps)?.saturating_sub(deployed_book);
    let amount = idle.min(room);
    if amount == 0 || amount < apply_bps(capacity, AUTO_PUSH_MIN_BPS)? {
        return Ok(0);
    }
    Ok(amount)
}

/// USDC a rebalance withdraws (multichain `ensure_liquidity`): the larger of open claims not covered
/// by free liquid USDC, and deployment above the `deploy_bps` line (which drifts up when stakes leave).
/// The same line `push_amount` deploys to, so the two never undo each other.
pub fn rebalance_shortfall(
    free_liquid: u64,
    total_allocated: u64,
    capacity: u64,
    deployed_book: u64,
    deploy_bps: u64,
) -> Result<u64> {
    let claims = total_allocated.saturating_sub(free_liquid);
    let over_line = deployed_book.saturating_sub(apply_bps(capacity, deploy_bps)?);
    Ok(claims.max(over_line))
}
