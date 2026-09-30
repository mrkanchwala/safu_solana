//! Yield split and additive yield indexes (multichain `vault.rs` r3).
//!
//! Each unit of yield is split by share of capacity: the staker part and the backer part move their
//! index by what the index can represent exactly; everything else (the non-staker/backer bps and all
//! rounding dust) is protocol revenue. So the set-asides always equal what the indexes owe, to within
//! per-record floor rounding, which favours the pool.

use crate::params::{BPS_DENOMINATOR, HARVEST_MAX_GROWTH_BPS_PER_DAY, SECONDS_PER_DAY, YIELD_INDEX_PRECISION};
use crate::{sub, to_u64, CoreError, Result};

/// Yield owed on `amount` of principal since the index read `index_at`. Rounded down.
pub fn owed(amount: u64, index_now: u128, index_at: u128) -> Result<u64> {
    let delta = index_now.saturating_sub(index_at);
    let v = (amount as u128).checked_mul(delta).ok_or(CoreError::Overflow)?;
    to_u64(v / YIELD_INDEX_PRECISION)
}

/// How one credit of yield moves the books.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Credit {
    pub staker_index_bump: u128,
    pub staker_share: u64,
    pub backer_index_bump: u128,
    pub backer_share: u64,
    pub protocol_share: u64,
}

/// One side's part: `amount x side / capacity x side_bps`, then what the index can carry exactly.
fn side(amount: u64, side_total: u64, capacity: u64, side_bps: u64) -> Result<(u128, u64)> {
    if side_total == 0 {
        return Ok((0, 0));
    }
    let part = amount as u128 * side_total as u128 / capacity as u128 * side_bps as u128
        / BPS_DENOMINATOR as u128;
    let bump = part
        .checked_mul(YIELD_INDEX_PRECISION)
        .ok_or(CoreError::Overflow)?
        / side_total as u128;
    let share = to_u64(bump * side_total as u128 / YIELD_INDEX_PRECISION)?;
    Ok((bump, share))
}

/// Splits `amount` of new yield between stakers, backers and the protocol, at the live split
/// settings (`staker_bps` / `backer_bps`).
pub fn credit(amount: u64, total_staked: u64, total_backed: u64, staker_bps: u64, backer_bps: u64) -> Result<Credit> {
    let capacity = crate::capacity(total_staked, total_backed)?;
    if amount == 0 {
        return Ok(Credit::default());
    }
    if capacity == 0 {
        return Ok(Credit { protocol_share: amount, ..Credit::default() });
    }
    let (staker_index_bump, staker_share) = side(amount, total_staked, capacity, staker_bps)?;
    let (backer_index_bump, backer_share) = side(amount, total_backed, capacity, backer_bps)?;
    let protocol_share = sub(sub(amount, staker_share)?, backer_share)?;
    Ok(Credit { staker_index_bump, staker_share, backer_index_bump, backer_share, protocol_share })
}

/// Most growth one harvest may book: `book x HARVEST_MAX_GROWTH_BPS_PER_DAY x elapsed / day`.
pub fn harvest_limit(book: u64, elapsed_secs: i64) -> Result<u64> {
    let elapsed = u128::try_from(elapsed_secs.max(0)).map_err(|_| CoreError::Overflow)?;
    let v = (book as u128)
        .checked_mul(HARVEST_MAX_GROWTH_BPS_PER_DAY as u128)
        .and_then(|v| v.checked_mul(elapsed))
        .ok_or(CoreError::Overflow)?;
    to_u64(v / (BPS_DENOMINATOR as u128 * SECONDS_PER_DAY as u128))
}

/// What the pool holds above everything it owes: the most the treasury may take.
pub fn protocol_surplus(held: u64, owed: u64) -> u64 {
    held.saturating_sub(owed)
}
