//! The yield leg's arithmetic: reading a klend reserve, the checks made before every call, how much
//! collateral a payment or a harvest takes, and rule 18's cash-short days. The same reserve against
//! the real klend program: `programs/safu_pool/tests/kamino.rs`.

use pool_core::kamino::*;
use pool_core::leg::*;
use pool_core::params::{HARVEST_MAX_GROWTH_BPS_PER_DAY, SECONDS_PER_DAY};
use pool_core::settings::{blocked_secs, mark_cash_short_day};
use pool_core::CoreError;
use proptest::prelude::*;

const USDC: u64 = 1_000_000;

fn put(d: &mut [u8], at: usize, v: &[u8]) {
    d[at..at + v.len()].copy_from_slice(v);
}

/// A reserve as klend lays it out: `available` cash, `borrowed` (whole units), collateral supply.
fn reserve_bytes(available: u64, borrowed: u64, supply: u64) -> Vec<u8> {
    let mut d = vec![0u8; RESERVE_LEN];
    put(&mut d, 0, &ACCOUNT_RESERVE);
    put(&mut d, R_LIQ_AVAILABLE, &available.to_le_bytes());
    put(
        &mut d,
        R_LIQ_BORROWED_SF,
        &((borrowed as u128) << SF_SHIFT).to_le_bytes(),
    );
    put(&mut d, R_COLL_SUPPLY, &supply.to_le_bytes());
    put(&mut d, R_DEPOSIT_LIMIT, &u64::MAX.to_le_bytes());
    d
}

fn reserve(available: u64, borrowed: u64, supply: u64) -> Reserve {
    parse_reserve(&reserve_bytes(available, borrowed, supply)).unwrap()
}

#[test]
fn a_reserve_is_read_only_with_klends_discriminator_and_length() {
    let d = reserve_bytes(10, 90, 80);
    assert_eq!(
        parse_reserve(&d[..RESERVE_LEN - 1]),
        Err(CoreError::BadAccount)
    );
    let mut other = d.clone();
    other[0] ^= 1;
    assert_eq!(parse_reserve(&other), Err(CoreError::BadAccount));
    let r = parse_reserve(&d).unwrap();
    assert_eq!((r.available, r.total, r.collateral_supply), (10, 100, 80));
}

#[test]
fn fees_owed_to_klend_are_not_the_depositors() {
    let mut d = reserve_bytes(10 * USDC, 90 * USDC, 80 * USDC);
    put(
        &mut d,
        R_LIQ_PROTOCOL_FEES_SF,
        &((2 * USDC as u128) << SF_SHIFT).to_le_bytes(),
    );
    put(
        &mut d,
        R_LIQ_REFERRER_FEES_SF,
        &((USDC as u128) << SF_SHIFT).to_le_bytes(),
    );
    assert_eq!(parse_reserve(&d).unwrap().total, 97 * USDC);
}

#[test]
fn collateral_and_liquidity_convert_at_the_reserve_rate_rounding_down() {
    // 120 liquidity per 100 collateral.
    let r = reserve(20 * USDC, 100 * USDC, 100 * USDC);
    assert_eq!(r.collateral_for(12 * USDC).unwrap(), 10 * USDC);
    assert_eq!(r.liquidity_for(10 * USDC).unwrap(), 12 * USDC);
    assert_eq!(r.collateral_for(1).unwrap(), 0);
    assert_eq!(r.liquidity_for(1).unwrap(), 1);
    // An empty reserve mints 1:1.
    assert_eq!(reserve(0, 0, 0).collateral_for(5).unwrap(), 5);
}

#[test]
fn a_deposit_is_checked_against_status_emergency_and_limit() {
    let r = reserve(20 * USDC, 100 * USDC, 100 * USDC);
    assert!(r.deposit_ok(USDC, false).unwrap());
    assert!(!r.deposit_ok(0, false).unwrap());
    assert!(!r.deposit_ok(1, false).unwrap(), "mints no collateral");
    assert!(!r.deposit_ok(USDC, true).unwrap(), "market emergency");
    assert!(!Reserve {
        emergency: true,
        ..r
    }
    .deposit_ok(USDC, false)
    .unwrap());
    assert!(!Reserve { status: 1, ..r }.deposit_ok(USDC, false).unwrap());
    let limit = Reserve {
        deposit_limit: r.total + USDC,
        ..r
    };
    assert!(limit.deposit_ok(USDC, false).unwrap());
    assert!(!limit.deposit_ok(USDC + 1, false).unwrap());
}

#[test]
fn a_redeem_needs_cash_in_the_reserve_after_queued_tickets_and_room_under_the_cap() {
    let r = reserve(20 * USDC, 100 * USDC, 100 * USDC);
    let now = 1_000;
    // 20 cash, less one unit of klend rounding.
    assert_eq!(r.redeemable().unwrap(), 20 * USDC - 1);
    let all = r.collateral_for(20 * USDC - 1).unwrap();
    assert!(r.redeem_ok(all, false, now).unwrap());
    assert!(!r
        .redeem_ok(r.collateral_for(21 * USDC).unwrap(), false, now)
        .unwrap());
    // Queued withdraw tickets are paid first.
    let queued = Reserve {
        queued_collateral: 10 * USDC,
        ..r
    };
    assert_eq!(queued.redeemable().unwrap(), 8 * USDC - 1);
    assert!(!r.redeem_ok(all, true, now).unwrap());
    assert!(!r.redeem_ok(0, false, now).unwrap());
    // The withdrawal cap: 5 left this interval, reset once the interval runs out.
    let capped = Reserve {
        cap: WithdrawCap {
            capacity: 10 * USDC as i64,
            current: 5 * USDC as i64,
            start: 900,
            interval: 200,
        },
        ..r
    };
    assert_eq!(capped.cap.room(now), 5 * USDC);
    assert!(!capped
        .redeem_ok(capped.collateral_for(6 * USDC).unwrap(), false, now)
        .unwrap());
    assert_eq!(capped.cap.room(1_100), 10 * USDC);
    assert_eq!(WithdrawCap::default().room(now), u64::MAX);
}

#[test]
fn the_market_emergency_flag_is_read_from_a_lending_market_only() {
    let mut m = vec![0u8; MARKET_MIN_LEN];
    put(&mut m, 0, &ACCOUNT_LENDING_MARKET);
    assert_eq!(market_emergency(&m), Ok(false));
    m[M_EMERGENCY] = 1;
    assert_eq!(market_emergency(&m), Ok(true));
    m[0] ^= 1;
    assert_eq!(market_emergency(&m), Err(CoreError::BadAccount));
}

#[test]
fn a_payment_redeems_by_book_value_so_growth_is_never_spent_on_it() {
    // 100 paid in for 80 collateral; worth 110 now.
    assert_eq!(collateral_for(50, 100, 80).unwrap(), 40);
    // Rounded up, capped at the position.
    assert_eq!(collateral_for(1, 100, 80).unwrap(), 1);
    assert_eq!(collateral_for(500, 100, 80).unwrap(), 80);
    assert_eq!(book_part(100, 40, 80).unwrap(), 50);
}

#[test]
fn a_harvest_takes_growth_up_to_the_daily_limit() {
    let book = 1_000_000 * USDC;
    let deployed = 800_000 * USDC;
    let limit_per_day = book * HARVEST_MAX_GROWTH_BPS_PER_DAY / 10_000;
    // Small growth: all of it, as collateral at the current value, rounded down.
    let value = book + 100 * USDC;
    let c = harvest_collateral(deployed, book, value, SECONDS_PER_DAY).unwrap();
    assert_eq!(
        c,
        ((100 * USDC) as u128 * deployed as u128 / value as u128) as u64
    );
    // Large growth: one day's limit.
    let value = book * 2;
    let c = harvest_collateral(deployed, book, value, SECONDS_PER_DAY).unwrap();
    assert_eq!(
        c,
        (limit_per_day as u128 * deployed as u128 / value as u128) as u64
    );
    // No growth, or a loss: nothing.
    assert_eq!(
        harvest_collateral(deployed, book, book, SECONDS_PER_DAY).unwrap(),
        0
    );
    assert_eq!(
        harvest_collateral(deployed, book, book - 1, SECONDS_PER_DAY).unwrap(),
        0
    );
    assert_eq!(harvest_collateral(deployed, book, value, 0).unwrap(), 0);
}

#[test]
fn a_redeem_books_a_gain_as_yield_and_a_shortfall_as_a_loss() {
    assert_eq!(
        settle_redeem(100, 103).unwrap(),
        Redeem { gain: 3, loss: 0 }
    );
    assert_eq!(settle_redeem(100, 97).unwrap(), Redeem { gain: 0, loss: 3 });
    assert_eq!(settle_redeem(100, 100).unwrap(), Redeem::default());
    // The keeper's floor: 5% under the expected amount.
    assert_eq!(min_collateral(10_000).unwrap(), 9_500);
}

#[test]
fn a_cash_short_day_counts_once_and_adds_a_day_of_blocked_time() {
    assert_eq!(mark_cash_short_day(0, 0, 0), (0, 1), "day 0 counts too");
    assert_eq!(mark_cash_short_day(5, 1, 5), (5, 1));
    assert_eq!(mark_cash_short_day(5, 1, 6), (6, 2));
    assert_eq!(mark_cash_short_day(6, 2, 9), (9, 3));
    assert_eq!(blocked_secs(100, 0), 100);
    assert_eq!(blocked_secs(100, 2), 100 + 2 * SECONDS_PER_DAY);
    assert_eq!(blocked_secs(i64::MAX, 1), i64::MAX);
}

proptest! {
    /// Never redeems more collateral than the payment's share of book needs, and what it redeems
    /// always carries at least the payment in book value (or is the whole position).
    #[test]
    fn collateral_for_covers_the_target_and_no_more(
        book in 1u64..u64::MAX / 4,
        deployed in 1u64..u64::MAX / 4,
        target in 0u64..u64::MAX / 4,
    ) {
        let c = collateral_for(target, book, deployed).unwrap();
        prop_assert!(c <= deployed);
        if c < deployed {
            prop_assert!(book_part(book, c, deployed).unwrap() as u128 + 1 >= target as u128);
        }
    }

    /// A harvest never takes collateral worth more than the growth.
    #[test]
    fn a_harvest_never_redeems_principal(
        book in 1u64..1u64 << 50,
        growth in 0u64..1u64 << 40,
        deployed in 1u64..1u64 << 50,
        elapsed in 0i64..30 * SECONDS_PER_DAY,
    ) {
        let value = book + growth;
        let c = harvest_collateral(deployed, book, value, elapsed).unwrap();
        let worth = c as u128 * value as u128 / deployed as u128;
        prop_assert!(worth <= growth as u128);
    }
}
