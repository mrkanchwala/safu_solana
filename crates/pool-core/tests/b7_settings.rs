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
fn pause_gap_default_is_the_filing_window_on_both_builds() {
    assert_eq!(PAUSE_GAP_SECS, CLAIM_WINDOW_SECS);
    assert_eq!(PAUSE_GAP_SECS, 30 * SECONDS_PER_DAY);
    if DEMO_BUILD {
        assert_eq!(SETTINGS_TIMELOCK_SECS, 5 * SECONDS_PER_MINUTE);
    } else {
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
fn a_pause_is_never_longer_than_the_gap_after_it() {
    // Audit L1/L2: the gap is never below the filing window, and the ranges keep every pause
    // no longer than the gap.
    let (_, pause_max) = SettingKey::PauseMaxSecs.bounds();
    let (gap_min, _) = SettingKey::PauseGapSecs.bounds();
    assert_eq!(gap_min, CLAIM_WINDOW_SECS);
    assert!(pause_max <= gap_min);
    let s = Settings::defaults(100 * SOL).unwrap();
    assert_eq!(
        s.check_value(SettingKey::PauseGapSecs, CLAIM_WINDOW_SECS - 1),
        Err(CoreError::SettingOutOfBounds)
    );
    assert_eq!(
        s.check_value(SettingKey::PauseGapSecs, CLAIM_WINDOW_SECS),
        Ok(())
    );
    // The order rule itself, should a later upgrade widen either range.
    let mut loose = s;
    loose.values[SettingKey::PauseGapSecs.index()] = 2 * SECONDS_PER_DAY;
    assert_eq!(
        loose.check_value(SettingKey::PauseMaxSecs, 3 * SECONDS_PER_DAY),
        Err(CoreError::SettingOrderInvalid)
    );
    assert_eq!(
        loose.check_value(SettingKey::PauseMaxSecs, 2 * SECONDS_PER_DAY),
        Ok(())
    );
    loose.values[SettingKey::PauseMaxSecs.index()] = 30 * SECONDS_PER_DAY;
    assert_eq!(
        loose.check_value(SettingKey::PauseGapSecs, 30 * SECONDS_PER_DAY),
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
    assert!(!pause_gap_passed(4_600, 500, 1_000, 3_600));
    assert!(pause_gap_passed(4_601, 500, 1_000, 3_600));
}

/// Paused seconds in `[0, t)`, from the full list of pauses (what the pool does not store).
fn true_paused_at(pauses: &[(i64, i64)], t: i64) -> i64 {
    pauses.iter().map(|&(s, u)| (t.min(u) - s).max(0)).sum()
}

proptest! {
    // Audit L2: with the gap at its minimum (the filing window) and pauses spaced only as the gap
    // rule allows, the stored pause record decides every filing exactly as the full history would.
    #[test]
    fn the_stored_pause_record_files_claims_exactly(
        first in 0i64..10 * SECONDS_PER_DAY,
        lens in proptest::collection::vec(0i64..=PAUSE_MAX_SECS, 1..4),
        // Zero half the time: the next pause starts at the first second the gap rule allows.
        extra in proptest::collection::vec(prop_oneof![Just(0i64), 0i64..3 * SECONDS_PER_DAY], 4),
        // The hack lands near one of the pauses, before, inside or after it.
        hack_near in 0usize..4,
        hack_off in -CLAIM_WINDOW_SECS..2 * CLAIM_WINDOW_SECS,
        // Zero half the time: filed the second the last pause ends.
        after in prop_oneof![Just(0i64), 0i64..40 * SECONDS_PER_DAY],
    ) {
        let gap = CLAIM_WINDOW_SECS;
        let mut pauses: Vec<(i64, i64)> = Vec::new();
        let (mut before, mut started, mut until) = (0i64, 0i64, 0i64);
        let mut next = first;
        for (i, len) in lens.iter().enumerate() {
            prop_assert!(pause_gap_passed(next, started, until, gap));
            if started != 0 {
                prop_assert!(!pause_gap_passed(until + gap, started, until, gap));
                before += until - started;
            }
            started = next.max(1);
            until = started + len;
            pauses.push((started, until));
            next = until + gap + 1 + extra[i];
        }
        // Filed while not paused (submit refuses during a pause), any time after the last pause.
        let now = until + after;
        let hack = (pauses[hack_near % pauses.len()].0 + hack_off).clamp(0, now);
        let paused_now = true_paused_at(&pauses, now);
        prop_assert_eq!(paused_secs_at(before, started, until, now), paused_now);
        let exact = claim_clock(now, paused_now, true_paused_at(&pauses, hack));
        let stored = claim_clock(now, paused_now, paused_secs_at(before, started, until, hack));
        prop_assert_eq!(
            exact > hack + CLAIM_WINDOW_SECS,
            stored > hack + CLAIM_WINDOW_SECS
        );
    }

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
