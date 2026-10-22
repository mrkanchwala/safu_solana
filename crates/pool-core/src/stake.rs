//! Stake bounds (multichain `stake.rs`): bps of the pool cap, so the same rule fits any pool size.

use crate::settings::{SettingKey, Settings};
use crate::{add, apply_bps, sub, CoreError, Result};

/// `(min, max)` stake in USDC base units under the live settings.
pub fn bounds(s: &Settings) -> Result<(u64, u64)> {
    let cap = s.pool_cap();
    Ok((
        apply_bps(cap, s.amount(SettingKey::MinStakeBps))?,
        apply_bps(cap, s.amount(SettingKey::MaxStakeBps))?,
    ))
}

/// Splits a deposit of `amount` into `(staked, overflow)`: up to the max is staked, the rest becomes
/// backing (multichain `stake`). Below the min is refused. A pool full for stakes refuses the whole
/// deposit: the overflow never turns a refused stake into backing.
pub fn split_new_stake(amount: u64, total_staked: u64, s: &Settings) -> Result<(u64, u64)> {
    let (min, max) = bounds(s)?;
    if amount == 0 || amount < min {
        return Err(CoreError::StakeOutOfRange);
    }
    let staked = amount.min(max);
    if add(total_staked, staked)? > s.pool_cap() {
        return Err(CoreError::PoolCapExceeded);
    }
    Ok((staked, amount - staked))
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
