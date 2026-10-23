//! The yield leg in numbers (multichain `vault.rs` redeem / harvest): how much collateral a payment
//! needs, how much growth a harvest takes, and how a redeem lands on the books.
//!
//! `book` is the USDC paid into the venue (never marked to market), `deployed` the collateral held on
//! the books, `value` what that collateral redeems for now.

use crate::params::{BPS_DENOMINATOR, MAX_REBALANCE_SLIPPAGE_BPS};
use crate::{apply_bps, mul_div_ceil, mul_div_floor, to_u64, yields, Result};

/// Book value carried by `collateral` of the `deployed` position, rounded down.
pub fn book_part(book: u64, collateral: u64, deployed: u64) -> Result<u64> {
    mul_div_floor(book, collateral, deployed)
}

/// Collateral to redeem so its book value covers `target` (rounded up, capped at the whole position).
/// Sized on book, not value, so unharvested growth in it is new money, never needed for the payment.
pub fn collateral_for(target: u64, book: u64, deployed: u64) -> Result<u64> {
    // A target at or above the whole book takes the whole position (and never overflows).
    if target >= book {
        return Ok(deployed);
    }
    Ok(mul_div_ceil(target, deployed, book)?.min(deployed))
}

/// Growth a harvest takes: value above book, capped by the daily growth limit.
pub fn harvest_growth(book: u64, value: u64, elapsed_secs: i64) -> Result<u64> {
    Ok(value
        .saturating_sub(book)
        .min(yields::harvest_limit(book, elapsed_secs)?))
}

/// Collateral a harvest redeems: [`harvest_growth`] in collateral, rounded down (never redeems more
/// than the growth is worth).
pub fn harvest_collateral(deployed: u64, book: u64, value: u64, elapsed_secs: i64) -> Result<u64> {
    let growth = harvest_growth(book, value, elapsed_secs)?;
    if growth == 0 || value == 0 {
        return Ok(0);
    }
    mul_div_floor(growth, deployed, value)
}

/// How one redeem lands on the books: growth that came back is yield, a shortfall against the book
/// value that left is a loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Redeem {
    pub gain: u64,
    pub loss: u64,
}

pub fn settle_redeem(principal: u64, received: u64) -> Result<Redeem> {
    let net = received as i128 - principal as i128;
    Ok(Redeem {
        gain: to_u64(net.max(0) as u128)?,
        loss: to_u64((-net).max(0) as u128)?,
    })
}

/// Least collateral a deposit should mint, within `MAX_REBALANCE_SLIPPAGE_BPS` of `expected`.
pub fn min_collateral(expected: u64) -> Result<u64> {
    apply_bps(expected, BPS_DENOMINATOR - MAX_REBALANCE_SLIPPAGE_BPS)
}
