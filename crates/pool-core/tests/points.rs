//! Points (dollar-days) and the rounding reserve draw (multichain `points.rs`, `vault.rs`).

use pool_core::liquidity::{free_liquid, rounding_draw};
use pool_core::params::{MAX_ROUNDING_DRAW, POINTS_RATE_ONE, SECONDS_PER_DAY, USDC_UNIT};
use pool_core::points::{earned, index};

const DAY: i64 = SECONDS_PER_DAY;

#[test]
fn one_point_per_usdc_per_day_at_the_default_rate() {
    let start = index(0, 1, POINTS_RATE_ONE, 1).unwrap();
    let later = index(0, 1, POINTS_RATE_ONE, 1 + 30 * DAY).unwrap();
    assert_eq!(earned(100 * USDC_UNIT, start, later), Ok(3_000));
    // Double rate, double points; half a USDC for one day rounds down to nothing.
    let fast = index(0, 1, 2 * POINTS_RATE_ONE, 1 + DAY).unwrap();
    assert_eq!(earned(USDC_UNIT, 0, fast), Ok(2));
    assert_eq!(earned(USDC_UNIT / 2, 0, later / 30), Ok(0));
}

#[test]
fn the_index_waits_for_its_first_checkpoint_and_never_goes_back() {
    assert_eq!(index(5, 0, POINTS_RATE_ONE, 1_000), Ok(5));
    assert_eq!(index(5, 100, POINTS_RATE_ONE, 100), Ok(5));
    assert_eq!(index(5, 100, POINTS_RATE_ONE, 50), Ok(5));
    assert_eq!(earned(USDC_UNIT, 10, 5), Ok(0));
    assert_eq!(earned(0, 0, u128::MAX), Ok(0));
}

#[test]
fn spendable_cash_excludes_only_the_rounding_reserve() {
    assert_eq!(free_liquid(100, 30), 70);
    assert_eq!(free_liquid(10, 30), 0);
}

#[test]
fn the_reserve_covers_only_small_gaps_it_can_afford() {
    assert_eq!(rounding_draw(100, 100, 50), 0);
    assert_eq!(rounding_draw(100, 99, 50), 1);
    assert_eq!(
        rounding_draw(100 + MAX_ROUNDING_DRAW, 100, MAX_ROUNDING_DRAW),
        MAX_ROUNDING_DRAW
    );
    assert_eq!(rounding_draw(101 + MAX_ROUNDING_DRAW, 100, u64::MAX), 0);
    assert_eq!(rounding_draw(105, 100, 4), 0);
}

#[test]
fn upkeep_checks_from_the_cursor_and_wraps_to_the_head() {
    use pool_core::upkeep::watch_places;
    assert_eq!(watch_places(3, 3, 0), [None, None]);
    assert_eq!(watch_places(3, 4, 9), [Some(3), None]);
    assert_eq!(watch_places(3, 6, 4), [Some(4), Some(5)]);
    assert_eq!(watch_places(3, 6, 5), [Some(5), Some(3)]);
    // A cursor left behind the head or past the tail starts again at the head.
    assert_eq!(watch_places(3, 6, 1), [Some(3), Some(4)]);
    assert_eq!(watch_places(3, 6, 6), [Some(3), Some(4)]);
}
