//! Stake bounds (multichain `stake.rs`): bps of the pool cap, so the same rule fits any pool size.

use crate::params::{MAX_STAKE_BPS, MIN_STAKE_BPS};
use crate::{add, apply_bps, CoreError, Result};

/// `(min, max)` stake in lamports for a pool cap.
pub fn bounds(pool_cap: u64) -> Result<(u64, u64)> {
    Ok((apply_bps(pool_cap, MIN_STAKE_BPS)?, apply_bps(pool_cap, MAX_STAKE_BPS)?))
}

/// A new stake of `amount` is in bounds and keeps total staked within the cap.
pub fn check_new_stake(amount: u64, pool_cap: u64, total_staked: u64) -> Result<()> {
    let (min, max) = bounds(pool_cap)?;
    if amount == 0 || amount < min || amount > max {
        return Err(CoreError::StakeOutOfRange);
    }
    if add(total_staked, amount)? > pool_cap {
        return Err(CoreError::PoolCapExceeded);
    }
    Ok(())
}
