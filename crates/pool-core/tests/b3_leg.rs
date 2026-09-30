//! B3: Marinade leg maths.

use pool_core::leg::*;
use pool_core::marinade::PRICE_DENOMINATOR;
use pool_core::params::*;
use pool_core::{liquidity, yields};
use proptest::prelude::*;

const SOL: u64 = 1_000_000_000;
/// Price of exactly 1 SOL per mSOL.
const PAR: u64 = PRICE_DENOMINATOR as u64;

#[test]
fn value_at_par_and_above() {
    assert_eq!(msol_value(SOL, PAR).unwrap(), SOL);
    assert_eq!(msol_value(SOL, PAR + PAR / 4).unwrap(), SOL + SOL / 4);
}

#[test]
fn payment_sizing_is_on_book_and_capped() {
    // 10 mSOL bought for 10 SOL.
    assert_eq!(msol_for(SOL, 10 * SOL, 10 * SOL).unwrap(), SOL);
    assert_eq!(msol_for(20 * SOL, 10 * SOL, 10 * SOL).unwrap(), 10 * SOL);
}

#[test]
fn harvest_takes_growth_up_to_the_daily_limit() {
    let book = 10 * SOL;
    let price = PAR + PAR / 100; // +1%
    assert_eq!(harvest_msol(book, book, price, 0).unwrap(), 0);
    let day = harvest_msol(book, book, price, SECONDS_PER_DAY).unwrap();
    let limit = yields::harvest_limit(book, SECONDS_PER_DAY).unwrap();
    assert!(msol_value(day, price).unwrap() <= limit);
    assert!(day > 0);
    // Long enough: all growth, never more.
    let all = harvest_msol(book, book, price, 1_000 * SECONDS_PER_DAY).unwrap();
    assert!(msol_value(all, price).unwrap() <= book / 100);
    // No growth, no harvest.
    assert_eq!(harvest_msol(book, book, PAR, SECONDS_PER_DAY).unwrap(), 0);
}

#[test]
fn fee_limit() {
    let over = SOL - SOL * MAX_REBALANCE_SLIPPAGE_BPS / BPS_DENOMINATOR - 1;
    assert!(fee_within_limit(SOL, SOL - SOL * MAX_REBALANCE_SLIPPAGE_BPS / BPS_DENOMINATOR).unwrap());
    assert!(!fee_within_limit(SOL, over).unwrap());
}

#[test]
fn payee_pays_the_fee_on_their_part_only() {
    // 2 SOL unstaked at book 2 SOL, 1% fee; the payment needed 1 SOL of it.
    let r = settle_redeem(2 * SOL, 2 * SOL - 2 * SOL / 100, 2 * SOL, SOL).unwrap();
    assert_eq!(r.payee_fee, SOL / 100);
    assert_eq!((r.gain, r.loss), (0, SOL / 100));
    // Rebalance: the pool pays all of it.
    let r = settle_redeem(2 * SOL, 2 * SOL - 2 * SOL / 100, 2 * SOL, 0).unwrap();
    assert_eq!((r.payee_fee, r.loss), (0, 2 * SOL / 100));
    // Growth covers the pool's fee: the rest is yield.
    let r = settle_redeem(2 * SOL, 2 * SOL - 2 * SOL / 100, SOL, SOL).unwrap();
    assert_eq!((r.payee_fee, r.gain, r.loss), (SOL / 100, SOL - SOL / 100, 0));
}

#[test]
fn whole_position_short_of_the_payment_costs_the_payee_the_gap() {
    // Everything unstaked is worth 1 lamport less than needed (Marinade rounds down), no fee.
    let r = settle_redeem(SOL - 1, SOL - 1, SOL, SOL).unwrap();
    assert_eq!((r.payee_fee, r.gain, r.loss), (1, 0, 0));
}

#[test]
fn rebalance_covers_claims_or_the_line_whichever_is_larger() {
    let cap = 10 * SOL;
    let line = pool_core::apply_bps(cap, DEPLOY_BPS).unwrap();
    assert_eq!(liquidity::rebalance_shortfall(SOL, 3 * SOL, cap, line).unwrap(), 2 * SOL);
    assert_eq!(liquidity::rebalance_shortfall(5 * SOL, 3 * SOL, cap, line + SOL).unwrap(), SOL);
    assert_eq!(liquidity::rebalance_shortfall(5 * SOL, 3 * SOL, cap, line).unwrap(), 0);
    // After a push nothing is short, and after that pull nothing is left to push.
    let pushed = liquidity::push_amount(5 * SOL, 3 * SOL, cap, 0).unwrap();
    assert_eq!(liquidity::rebalance_shortfall(5 * SOL - pushed, 3 * SOL, cap, pushed).unwrap(), 0);
}

proptest! {
    #[test]
    fn redeem_conserves(expected in 0u64..1_000 * SOL, fee_bps in 0u64..1_000, principal in 0u64..1_000 * SOL, part in 0u64..1_000 * SOL) {
        let received = expected - expected * fee_bps / BPS_DENOMINATOR;
        let r = settle_redeem(expected, received, principal, part).unwrap();
        prop_assert!(r.payee_fee <= expected - received + part.saturating_sub(expected));
        // The payee never gets more than the unstake returned for their part.
        prop_assert!(part.min(expected) <= received + r.payee_fee || part > expected);
        prop_assert!(r.gain == 0 || r.loss == 0);
        prop_assert_eq!(received as i128 + r.payee_fee as i128 - principal as i128, r.gain as i128 - r.loss as i128);
    }

    #[test]
    fn sized_unstake_covers_the_payment(lamports in 1u64..100 * SOL, book in SOL..1_000 * SOL, extra_bps in 0u64..2_000) {
        let deployed = book - book * extra_bps / (4 * BPS_DENOMINATOR);
        let msol = msol_for(lamports, book, deployed).unwrap();
        if msol < deployed {
            prop_assert!(book_part(book, msol, deployed).unwrap() + 1 >= lamports);
        }
    }

    #[test]
    fn harvest_never_exceeds_growth(deployed in SOL..1_000 * SOL, growth_bps in 0u64..500, elapsed in 0i64..400 * SECONDS_PER_DAY) {
        let price = PAR + PAR * growth_bps / BPS_DENOMINATOR;
        let book = deployed;
        let msol = harvest_msol(deployed, book, price, elapsed).unwrap();
        let taken = msol_value(msol, price).unwrap();
        prop_assert!(taken <= msol_value(deployed, price).unwrap() - book);
        prop_assert!(taken <= yields::harvest_limit(book, elapsed).unwrap());
    }
}
