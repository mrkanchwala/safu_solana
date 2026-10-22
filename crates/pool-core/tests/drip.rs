//! Backer exit drip and fair daily shares (multichain `backer.rs`, `claim.rs::daily_share`).

use pool_core::drip::{drip_days, share, unlocked, utilisation_bps};
use pool_core::params::{
    BPS_DENOMINATOR, DRIP_FULL_UTIL_BPS, DRIP_MAX_SECS, DRIP_MIN_SECS, SECONDS_PER_DAY,
};

fn days(secs: i64) -> u64 {
    (secs as u64).div_ceil(SECONDS_PER_DAY as u64).max(1)
}

#[test]
fn utilisation_is_allocated_over_capacity() {
    assert_eq!(utilisation_bps(0, 0), Ok(0));
    assert_eq!(utilisation_bps(5, 0), Ok(0));
    assert_eq!(utilisation_bps(1, 4), Ok(BPS_DENOMINATOR / 4));
}

#[test]
fn drip_runs_from_the_min_on_an_idle_pool_to_the_max_at_full_utilisation() {
    let (lo, hi) = (DRIP_MIN_SECS, DRIP_MAX_SECS);
    assert_eq!(drip_days(0, lo, hi), Ok(days(lo)));
    assert_eq!(drip_days(DRIP_FULL_UTIL_BPS, lo, hi), Ok(days(hi)));
    // Capped at full utilisation.
    assert_eq!(drip_days(BPS_DENOMINATOR, lo, hi), Ok(days(hi)));
    // Straight line between, rounded up to a whole day.
    let half = drip_days(DRIP_FULL_UTIL_BPS / 2, lo, hi).unwrap();
    assert_eq!(half, days(lo + (hi - lo) / 2));
    // Never zero days, even with zero-length settings.
    assert_eq!(drip_days(0, 0, 0), Ok(1));
}

#[test]
fn drip_with_real_day_lengths() {
    let day = SECONDS_PER_DAY;
    assert_eq!(drip_days(0, 7 * day, 90 * day), Ok(7));
    assert_eq!(drip_days(DRIP_FULL_UTIL_BPS, 7 * day, 90 * day), Ok(90));
    assert_eq!(drip_days(DRIP_FULL_UTIL_BPS / 2, 7 * day, 90 * day), Ok(49));
}

#[test]
fn unlocking_adds_one_part_a_day_and_never_passes_the_amount() {
    assert_eq!(unlocked(700, 0, 7), Ok(100));
    assert_eq!(unlocked(700, 3, 7), Ok(400));
    assert_eq!(unlocked(700, 6, 7), Ok(700));
    assert_eq!(unlocked(700, 60, 7), Ok(700));
    assert_eq!(unlocked(700, 0, 1), Ok(700));
    // Rounded down until the last day pays the remainder.
    assert_eq!(unlocked(10, 0, 3), Ok(3));
    assert_eq!(unlocked(10, 2, 3), Ok(10));
}

#[test]
fn shares_are_pro_rata_and_round_up() {
    assert_eq!(share(100, 30, 100), Ok(30));
    assert_eq!(share(100, 1, 3), Ok(34));
    // Collection order does not matter: each share depends only on the snapshot.
    let (a, b) = (share(90, 2, 3).unwrap(), share(90, 1, 3).unwrap());
    assert_eq!((a, b), (60, 30));
    // Joined after the snapshot: its own remainder is the whole.
    assert_eq!(share(100, 50, 0), Ok(100));
    assert_eq!(share(100, 50, 20), Ok(100));
    // Nothing owed, nothing shared.
    assert_eq!(share(100, 0, 0), Ok(0));
    assert_eq!(share(0, 10, 10), Ok(0));
}
