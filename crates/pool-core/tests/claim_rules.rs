//! Claim rules: tier ceilings, windows, admission, vesting, payout caps.

use pool_core::claim::*;
use pool_core::params::*;
use pool_core::settings::Rates;
use pool_core::CoreError;
use proptest::prelude::*;

/// The default daily rates (a new pool's settings).
const ADMIT: Rates = Rates {
    low: ADMIT_LOW_BPS,
    mid: ADMIT_MID_BPS,
    high: ADMIT_HIGH_BPS,
};
const PAYOUT: Rates = Rates {
    low: PAYOUT_LOW_BPS,
    mid: PAYOUT_MID_BPS,
    high: PAYOUT_HIGH_BPS,
};

const USDC: u64 = 1_000_000;
const NOW: i64 = 1_800_000_000;

#[test]
fn tier_ceilings_are_15_10_5_times_the_stake() {
    assert_eq!(tier_cap(USDC, TIER_A), Ok(15 * USDC));
    assert_eq!(tier_cap(USDC, TIER_B), Ok(10 * USDC));
    assert_eq!(tier_cap(USDC, TIER_C), Ok(5 * USDC));
    for bad in [0, TIER_C + 1] {
        assert_eq!(tier_cap(USDC, bad), Err(CoreError::InvalidTier));
    }
}

#[test]
fn entitlement_checks() {
    assert_eq!(
        check_entitlement(0, USDC, TIER_A),
        Err(CoreError::EntitlementNotPositive)
    );
    assert_eq!(check_entitlement(5 * USDC, USDC, TIER_C), Ok(()));
    assert_eq!(
        check_entitlement(5 * USDC + 1, USDC, TIER_C),
        Err(CoreError::EntitlementExceedsTierCap)
    );
}

#[test]
fn hack_time_window() {
    let staked = NOW - 10;
    assert_eq!(
        check_hack_time(NOW + 1, staked, NOW, NOW),
        Err(CoreError::HackTimestampInFuture)
    );
    assert_eq!(
        check_hack_time(staked - 1, staked, NOW, NOW),
        Err(CoreError::HackPredatesStake)
    );
    assert_eq!(
        check_hack_time(
            staked,
            staked,
            staked + CLAIM_WINDOW_SECS,
            staked + CLAIM_WINDOW_SECS
        ),
        Ok(())
    );
    assert_eq!(
        check_hack_time(
            staked,
            staked,
            staked + CLAIM_WINDOW_SECS + 1,
            staked + CLAIM_WINDOW_SECS + 1
        ),
        Err(CoreError::ClaimWindowExpired)
    );
}

#[test]
fn approval_deadline_window() {
    assert_eq!(
        check_deadline(NOW - 1, NOW),
        Err(CoreError::SignatureExpired)
    );
    assert_eq!(check_deadline(NOW, NOW), Ok(()));
    assert_eq!(check_deadline(NOW + MAX_APPROVAL_WINDOW_SECS, NOW), Ok(()));
    assert_eq!(
        check_deadline(NOW + MAX_APPROVAL_WINDOW_SECS + 1, NOW),
        Err(CoreError::SignatureDeadlineTooFar)
    );
}

#[test]
fn stress_cap_bands() {
    let cap = 100 * USDC;
    assert_eq!(stress_cap(cap, 0, ADMIT), Ok(25 * USDC));
    assert_eq!(stress_cap(cap, 20 * USDC, ADMIT), Ok(10 * USDC));
    assert_eq!(stress_cap(cap, 50 * USDC, ADMIT), Ok(3 * USDC));
    assert_eq!(stress_cap(0, 0, ADMIT), Ok(0));
}

#[test]
fn admission_needs_solvency_and_room_under_the_stress_cap() {
    let cap = 100 * USDC;
    assert_eq!(admits(25 * USDC, cap, 0, 0, ADMIT), Ok(true));
    assert_eq!(admits(25 * USDC + 1, cap, 0, 0, ADMIT), Ok(false));
    assert_eq!(admits(USDC, cap, 0, 25 * USDC, ADMIT), Ok(false));
    // Insolvent even though the day is empty.
    assert_eq!(admits(3 * USDC, cap, 98 * USDC, 0, ADMIT), Ok(false));
}

#[test]
fn oracle_limit_is_a_tenth_of_stakers_at_least_one() {
    assert_eq!(oracle_daily_limit(0), 1);
    assert_eq!(oracle_daily_limit(19), 1);
    assert_eq!(oracle_daily_limit(20), 2);
}

#[test]
fn time_gate() {
    assert!(!time_gate_met(NOW, NOW + TIME_GATE_SECS - 1));
    assert!(time_gate_met(NOW, NOW + TIME_GATE_SECS));
}

#[test]
fn vesting_is_linear_after_cooldown() {
    let (c, v) = (NOW, NOW + VESTING_SECS);
    assert_eq!(vested(1_000, c, v, c - 1), Ok(0));
    assert_eq!(vested(1_000, c, v, c), Ok(0));
    assert_eq!(vested(1_000, c, v, c + VESTING_SECS / 2), Ok(500));
    assert_eq!(vested(1_000, c, v, v + 1), Ok(1_000));
    assert_eq!(vested(1_000, c, c, c), Ok(1_000));
}

#[test]
fn payout_cap_bands() {
    let base = 100 * USDC;
    assert_eq!(payout_cap(base, 0, PAYOUT), Ok(5 * USDC));
    assert_eq!(payout_cap(base, 20 * USDC, PAYOUT), Ok(3 * USDC));
    assert_eq!(payout_cap(base, 50 * USDC, PAYOUT), Ok(USDC));
    assert_eq!(payout_cap(0, 0, PAYOUT), Ok(0));
}

#[test]
fn day_boundaries() {
    assert_eq!(day_of(0), 0);
    assert_eq!(day_of(SECONDS_PER_DAY - 1), 0);
    assert_eq!(day_of(SECONDS_PER_DAY), 1);
}

proptest! {
    #[test]
    fn vesting_is_monotone_and_never_above_entitlement(e in 0u64..=1_000 * USDC, a in 0i64..=VESTING_SECS * 2, b in 0i64..=VESTING_SECS * 2) {
        let (c, v) = (NOW, NOW + VESTING_SECS);
        let (lo, hi) = (a.min(b), a.max(b));
        let x = vested(e, c, v, c + lo).unwrap();
        let y = vested(e, c, v, c + hi).unwrap();
        prop_assert!(x <= y && y <= e);
    }

    #[test]
    fn admitted_claims_never_exceed_capacity(e in 1u64..=100 * USDC, cap in 0u64..=100 * USDC, alloc in 0u64..=100 * USDC, day in 0u64..=100 * USDC) {
        if admits(e, cap, alloc, day, ADMIT).unwrap() {
            prop_assert!(alloc + e <= cap);
            prop_assert!(day + e <= stress_cap(cap, alloc, ADMIT).unwrap());
        }
    }

    #[test]
    fn tier_cap_is_monotone_in_stake(s1 in 0u64..=1_000 * USDC, s2 in 0u64..=1_000 * USDC) {
        for t in [TIER_A, TIER_B, TIER_C] {
            let (lo, hi) = (s1.min(s2), s1.max(s2));
            prop_assert!(tier_cap(lo, t).unwrap() <= tier_cap(hi, t).unwrap());
        }
    }
}
