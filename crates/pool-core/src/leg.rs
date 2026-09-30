//! The Marinade leg in numbers: mSOL value, how much mSOL a payment needs, harvest size, and how an
//! unstake settles into yield or loss (multichain `vault.rs` redeem / harvest, SOL instead of shares).
//!
//! `book` is the SOL paid for the pool's mSOL (never marked to market), `deployed` the mSOL held on
//! the books, `price` Marinade's `msol_price` (SOL per mSOL, scaled by `PRICE_DENOMINATOR`).

use crate::marinade::PRICE_DENOMINATOR;
use crate::params::{BPS_DENOMINATOR, MAX_REBALANCE_SLIPPAGE_BPS};
use crate::{mul_div_ceil, mul_div_floor, to_u64, yields, Result};

/// `PRICE_DENOMINATOR` as the `u64` the helpers take (2^32 fits).
const DENOM: u64 = PRICE_DENOMINATOR as u64;

/// SOL value of `msol` at `price`, rounded down.
pub fn msol_value(msol: u64, price: u64) -> Result<u64> {
    mul_div_floor(msol, price, DENOM)
}

/// Book value carried by `msol` of the `deployed` position, rounded down.
pub fn book_part(book: u64, msol: u64, deployed: u64) -> Result<u64> {
    mul_div_floor(book, msol, deployed)
}

/// mSOL to unstake so its book value covers `lamports` (rounded up, capped at the whole position).
/// Sized on book, not price, so unharvested growth in it is new money, never needed for the payment.
pub fn msol_for(lamports: u64, book: u64, deployed: u64) -> Result<u64> {
    Ok(mul_div_ceil(lamports, deployed, book)?.min(deployed))
}

/// mSOL a harvest unstakes: growth above book, capped by the daily growth limit, rounded down.
pub fn harvest_msol(deployed: u64, book: u64, price: u64, elapsed_secs: i64) -> Result<u64> {
    let value = msol_value(deployed, price)?;
    let growth = value.saturating_sub(book).min(yields::harvest_limit(book, elapsed_secs)?);
    if growth == 0 {
        return Ok(0);
    }
    mul_div_floor(growth, deployed, value)
}

/// Marinade's fee on an unstake is within `limit_bps` of the expected value, plus one lamport for
/// Marinade's rounding. Payee-paid unstakes use Marinade's own `lp_max_fee` (founder decision
/// 2026-09-30: the payee pays whatever Marinade charges); pool-paid rebalances use
/// `MAX_REBALANCE_SLIPPAGE_BPS`.
pub fn fee_within_limit(expected: u64, received: u64, limit_bps: u64) -> Result<bool> {
    let fee = expected.saturating_sub(received).saturating_sub(1);
    Ok(fee as u128 * BPS_DENOMINATOR as u128 <= expected as u128 * limit_bps as u128)
}

/// How one unstake lands on the books.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Redeem {
    /// Taken off the payee's payment: their part of Marinade's fee, plus any part of their shortfall
    /// the unstaked mSOL was not worth (only when the whole position went and still fell short).
    pub payee_fee: u64,
    /// Growth that came back, after the pool's part of the fee: yield to credit.
    pub gain: u64,
    /// Pool's part of the fee above that growth: a loss.
    pub loss: u64,
}

/// `expected` = value of the unstaked mSOL, `received` = SOL that arrived, `principal` = its book
/// value, `payee_part` = the shortfall a payment needed (0 for a pool rebalance). The payee pays the
/// fee on the part the unstake covers (rounded up) and takes what the unstake returns; the rest of the
/// fee is the pool's. So a payment never exceeds the SOL that exists for it.
pub fn settle_redeem(expected: u64, received: u64, principal: u64, payee_part: u64) -> Result<Redeem> {
    let fee = expected.saturating_sub(received);
    let covered = payee_part.min(expected);
    let fee_share = if expected == 0 { 0 } else { mul_div_ceil(fee, covered, expected)?.min(fee) };
    let payee_fee = crate::add(fee_share, payee_part - covered)?;
    // Pool's result: SOL in (plus what the payee covered) against the book value that left.
    let net = received as i128 + payee_fee as i128 - principal as i128;
    Ok(Redeem {
        payee_fee,
        gain: to_u64(net.max(0) as u128)?,
        loss: to_u64((-net).max(0) as u128)?,
    })
}

/// Least mSOL a deposit of `lamports` should mint at `price`, within `MAX_REBALANCE_SLIPPAGE_BPS`.
pub fn min_msol_for_deposit(lamports: u64, price: u64) -> Result<u64> {
    let fair = mul_div_floor(lamports, DENOM, price)?;
    crate::apply_bps(fair, BPS_DENOMINATOR - MAX_REBALANCE_SLIPPAGE_BPS)
}
