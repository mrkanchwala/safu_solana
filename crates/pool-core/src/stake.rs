//! Stake bounds (multichain `stake.rs`): bps of the pool cap, so the same rule fits any pool size.

use crate::settings::{SettingKey, Settings};
use crate::{add, apply_bps, sub, CoreError, Result};

/// `(min, max)` stake in lamports under the live settings.
pub fn bounds(s: &Settings) -> Result<(u64, u64)> {
    let cap = s.pool_cap();
    Ok((
        apply_bps(cap, s.amount(SettingKey::MinStakeBps))?,
        apply_bps(cap, s.amount(SettingKey::MaxStakeBps))?,
    ))
}

/// A new stake of `amount` is in bounds and keeps total staked within the cap.
pub fn check_new_stake(amount: u64, total_staked: u64, s: &Settings) -> Result<()> {
    let (min, max) = bounds(s)?;
    if amount == 0 || amount < min || amount > max {
        return Err(CoreError::StakeOutOfRange);
    }
    if add(total_staked, amount)? > s.pool_cap() {
        return Err(CoreError::PoolCapExceeded);
    }
    Ok(())
}

/// Taking `amount` out of a stake of `stake`: returns what stays. All of it may go; a part only if
/// what stays is at least today's min stake (a stake below it could not have been opened).
pub fn check_withdraw(amount: u64, stake: u64, s: &Settings) -> Result<u64> {
    if amount == 0 {
        return Err(CoreError::InvalidParameter);
    }
    let left = sub(stake, amount).map_err(|_| CoreError::AmountExceedsStake)?;
    if left > 0 && left < bounds(s)?.0 {
        return Err(CoreError::StakeBelowMinimum);
    }
    Ok(left)
}
