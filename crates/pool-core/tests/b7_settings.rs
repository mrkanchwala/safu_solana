//! B7 fix pass: adjustable settings, partial stake withdrawal, one free-capital rule, pause clock.

use pool_core::params::*;
use pool_core::settings::*;
use pool_core::{check_capital_free, stake, CoreError};
use proptest::prelude::*;

const SOL: u64 = 1_000_000_000;
const CAP: u64 = 50 * SOL;

fn defaults() -> Settings {
    Settings::defaults(CAP).unwrap()
}

// ------------------------------------------------------------------ settings

#[test]
fn defaults_are_the_params_values_and_all_in_bounds() {
    let s = defaults();
    assert_eq!(s.get(SettingKey::CooldownSecs), COOLDOWN_SECS);
    assert_eq!(s.get(SettingKey::VestingSecs), VESTING_SECS);
    assert_eq!(s.get(SettingKey::PauseMaxSecs), PAUSE_MAX_SECS);
    assert_eq!(s.get(SettingKey::PauseGapSecs), PAUSE_GAP_SECS);
    assert_eq!(s.pool_cap(), CAP);
    for key in SettingKey::ALL {
        assert_eq!(s.check_value(key, s.get(key)), Ok(()), "{key:?}");
    }
    // Unused slots stay zero: room for settings an upgrade adds.
    assert!(s.values[SETTING_COUNT..].iter().all(|v| *v == 0));
}

#[test]
fn pause_gap_default_is_30_days_or_1_hour_on_the_fast_build() {
    if DEMO_BUILD {
        assert_eq!(PAUSE_GAP_SECS, SECONDS_PER_HOUR);
        assert_eq!(SETTINGS_TIMELOCK_SECS, 5 * SECONDS_PER_MINUTE);
    } else {
        assert_eq!(PAUSE_GAP_SECS, 30 * SECONDS_PER_DAY);
        assert_eq!(SETTINGS_TIMELOCK_SECS, 7 * SECONDS_PER_DAY);
    }
}

#[test]
fn slot_numbers_are_stable_and_unused_slots_are_refused() {
    for (i, key) in SettingKey::ALL.iter().enumerate() {
        assert_eq!(key.index(), i);
        assert_eq!(SettingKey::from_index(i as u8), Ok(*key));
    }
    assert_eq!(
        SettingKey::from_index(SETTING_COUNT as u8),
        Err(CoreError::UnknownSetting)
    );
    assert_eq!(SettingKey::from_index(255), Err(CoreError::UnknownSetting));
    const { assert!(SETTING_COUNT < SETTING_SLOTS) };
}

#[test]
fn a_value_outside_the_hard_range_is_refused() {
    let s = defaults();
    for key in SettingKey::ALL {
        let (min, max) = key.bounds();
        assert_eq!(
            s.check_value(key, min - 1),
            Err(CoreError::SettingOutOfBounds),
            "{key:?}"
        );
        if max < i64::MAX {
            assert_eq!(
                s.check_value(key, max + 1),
                Err(CoreError::SettingOutOfBounds),
                "{key:?}"
            );
        }
    }
}

#[test]
fn settings_keep_their_order() {
    let mut s = defaults();
    // Min stake never above max stake.
    assert_eq!(
        s.check_value(SettingKey::MinStakeBps, MAX_STAKE_BPS as i64 + 1),
        Err(CoreError::SettingOrderInvalid)
    );
    assert_eq!(
        s.check_value(SettingKey::MaxStakeBps, MIN_STAKE_BPS as i64 - 1),
        Err(CoreError::SettingOrderInvalid)
    );
    // Bands: low >= mid >= high, for both rate families.
    s.values[SettingKey::AdmitLowBps.index()] = 2_000;
    assert_eq!(
        s.check_value(SettingKey::AdmitMidBps, 2_001),
        Err(CoreError::SettingOrderInvalid)
    );
    assert_eq!(
        s.check_value(SettingKey::AdmitMidBps, ADMIT_HIGH_BPS as i64 - 1),
        Err(CoreError::SettingOrderInvalid)
    );
    assert_eq!(
        s.check_value(SettingKey::AdmitHighBps, ADMIT_MID_BPS as i64 + 1),
        Err(CoreError::SettingOrderInvalid)
    );
    assert_eq!(
        s.check_value(SettingKey::PayoutLowBps, PAYOUT_MID_BPS as i64 - 1),
        Err(CoreError::SettingOrderInvalid)
    );
    assert_eq!(
        s.check_value(SettingKey::PayoutHighBps, PAYOUT_MID_BPS as i64 + 1),
        Err(CoreError::SettingOrderInvalid)
    );
    // A cap so small the min stake rounds to zero.
    assert_eq!(
        s.check_value(SettingKey::PoolCap, 999),
        Err(CoreError::SettingOrderInvalid)
    );
    assert_eq!(s.check_value(SettingKey::PoolCap, 1_000), Ok(()));
    // Allowed moves.
    assert_eq!(
        s.check_value(SettingKey::PoolCap, (100 * SOL) as i64),
        Ok(())
    );
    assert_eq!(
        s.check_value(SettingKey::PoolCap, (10 * SOL) as i64),
        Ok(())
    );
    assert_eq!(
        s.check_value(SettingKey::PauseGapSecs, PAUSE_GAP_SECS * 2),
        Ok(())
    );
}

#[test]
fn a_pool_cap_too_small_for_a_min_stake_cannot_start_a_pool() {
    assert_eq!(Settings::defaults(999), Err(CoreError::SettingOrderInvalid));
    assert!(Settings::defaults(1_000).is_ok());
}

#[test]
fn live_settings_drive_the_rules() {
    let mut s = defaults();
    s.values[SettingKey::MinStakeBps.index()] = 20;
    s.values[SettingKey::PoolCap.index()] = (100 * SOL) as i64;
    assert_eq!(stake::bounds(&s), Ok((SOL / 5, SOL)));
    assert_eq!(
        stake::check_new_stake(SOL / 10, 0, &s),
        Err(CoreError::StakeOutOfRange)
    );
    assert_eq!(
        s.admit_rates(),
        Rates {
            low: ADMIT_LOW_BPS,
            mid: ADMIT_MID_BPS,
            high: ADMIT_HIGH_BPS
        }
    );
}

// ------------------------------------------------------------------ partial withdrawal

#[test]
fn a_stake_can_be_taken_out_in_full_or_in_part() {
    let s = defaults();
    let (min, _) = stake::bounds(&s).unwrap();
    let stake_amt = SOL / 2;
    assert_eq!(stake::check_withdraw(stake_amt, stake_amt, &s), Ok(0));
    assert_eq!(
        stake::check_withdraw(stake_amt - min, stake_amt, &s),
        Ok(min)
    );
    assert_eq!(
        stake::check_withdraw(stake_amt - min + 1, stake_amt, &s),
        Err(CoreError::StakeBelowMinimum)
    );
    assert_eq!(
        stake::check_withdraw(stake_amt + 1, stake_amt, &s),
        Err(CoreError::AmountExceedsStake)
    );
    assert_eq!(
        stake::check_withdraw(0, stake_amt, &s),
        Err(CoreError::InvalidParameter)
    );
}

#[test]
fn a_stake_below_a_raised_min_can_still_leave_in_full() {
    let mut s = defaults();
    s.values[SettingKey::MinStakeBps.index()] = 100; // min = max = 0.5 SOL
    let small = SOL / 10;
    assert_eq!(stake::check_withdraw(small, small, &s), Ok(0));
    assert_eq!(
        stake::check_withdraw(small / 2, small, &s),
        Err(CoreError::StakeBelowMinimum)
    );
}

// ------------------------------------------------------------------ one capital rule

#[test]
fn money_leaves_only_if_open_claims_still_fit() {
    // 100 capacity, 90 needed by claims: 10 is free.
    assert_eq!(check_capital_free(90, 100, 10), Ok(()));
    assert_eq!(
        check_capital_free(90, 100, 11),
        Err(CoreError::CapitalNotFree)
    );
    assert_eq!(check_capital_free(0, 100, 100), Ok(()));
}

// ------------------------------------------------------------------ pause clock

#[test]
fn the_pause_clock_counts_only_paused_time() {
    // Never paused.
    assert_eq!(paused_secs_at(0, 0, 0, 1_000), 0);
    // Paused 100..160, then 40 more before that.
    assert_eq!(paused_secs_at(40, 100, 160, 50), 40);
    assert_eq!(paused_secs_at(40, 100, 160, 130), 70);
    assert_eq!(paused_secs_at(40, 100, 160, 500), 100);
}

#[test]
fn a_window_does_not_run_while_paused() {
    // Window opened at t=0 (mark 0); pool paused 100..160; now 200: 60 s paused.
    let paused_now = paused_secs_at(0, 100, 160, 200);
    assert_eq!(claim_clock(200, paused_now, 0), 140);
    // A window opened after the pause (mark 60) is unaffected.
    assert_eq!(claim_clock(200, paused_now, 60), 200);
}

#[test]
fn a_new_pause_waits_for_the_gap() {
    // Never paused: allowed.
    assert!(pause_gap_passed(10, 0, 0, 3_600));
    // Last pause ended at 1_000; gap 3_600.
    assert!(!pause_gap_passed(4_599, 500, 1_000, 3_600));
    assert!(pause_gap_passed(4_600, 500, 1_000, 3_600));
}

proptest! {
    #[test]
    fn no_value_outside_the_bounds_is_ever_accepted(idx in 0u8..SETTING_COUNT as u8, value in any::<i64>()) {
        let s = defaults();
        let key = SettingKey::from_index(idx).unwrap();
        let (min, max) = key.bounds();
        if s.check_value(key, value).is_ok() {
            prop_assert!(value >= min && value <= max);
        }
    }

    #[test]
    fn a_partial_withdrawal_never_leaves_a_dust_stake(stake_amt in 1u64..10 * SOL, amount in 1u64..10 * SOL) {
        let s = defaults();
        let (min, _) = stake::bounds(&s).unwrap();
        if let Ok(left) = stake::check_withdraw(amount, stake_amt, &s) {
            prop_assert!(left == 0 || left >= min);
            prop_assert_eq!(left + amount, stake_amt);
        }
    }

    #[test]
    fn the_claim_clock_never_runs_ahead_of_real_time(
        before in 0i64..1_000_000, start in 1i64..1_000_000, len in 0i64..1_000_000, t in 0i64..3_000_000, mark_at in 0i64..3_000_000,
    ) {
        let until = start + len;
        let mark = paused_secs_at(before, start, until, mark_at.min(t));
        let clock = claim_clock(t, paused_secs_at(before, start, until, t), mark);
        prop_assert!(clock <= t);
        // It loses at most the paused time since the mark.
        prop_assert!(t - clock <= len + before);
    }
}
