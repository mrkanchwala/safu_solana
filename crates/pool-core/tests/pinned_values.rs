//! Every clock and every setting's hard range, pinned to its number. The other tests use these
//! constants by name, so a changed constant moves the test's expectation with it; these do not.
//! Each value is checked on the build under test: the normal one, or the fast-clock `demo` build.
//! The normal-build numbers match the Stellar pool's (`safu_multichain` `protection-pool`), which
//! sets the same rules.

use pool_core::params::*;
use pool_core::settings::SettingKey as K;

const MIN: i64 = 60;
const HOUR: i64 = 3_600;
const DAY: i64 = 86_400;

/// The normal or the demo value, whichever this build uses.
fn clock(normal: i64, demo: i64) -> i64 {
    if cfg!(feature = "demo") {
        demo
    } else {
        normal
    }
}

#[test]
fn time_units_are_seconds() {
    assert_eq!(
        (SECONDS_PER_MINUTE, SECONDS_PER_HOUR, SECONDS_PER_DAY),
        (60, 3_600, 86_400)
    );
}

#[test]
fn every_clock_is_its_rule_value() {
    let cases = [
        ("time gate", TIME_GATE_SECS, clock(90 * DAY, 2 * MIN)),
        ("cooldown", COOLDOWN_SECS, clock(7 * DAY, MIN)),
        ("vesting", VESTING_SECS, clock(45 * DAY, 5 * MIN)),
        (
            "approve window",
            APPROVE_WINDOW_SECS,
            clock(100 * DAY, HOUR),
        ),
        (
            "collection inactivity",
            COLLECTION_INACTIVITY_SECS,
            clock(100 * DAY, HOUR),
        ),
        (
            "penalty lock",
            PENALTY_LOCK_SECS,
            clock(365 * DAY, 10 * MIN),
        ),
        ("suspend max", SUSPEND_MAX_SECS, clock(30 * DAY, 10 * MIN)),
        ("claim window", CLAIM_WINDOW_SECS, 30 * DAY),
        ("approval signature life", MAX_APPROVAL_WINDOW_SECS, DAY),
        ("pause max", PAUSE_MAX_SECS, 30 * DAY),
        ("pause gap", PAUSE_GAP_SECS, 30 * DAY),
        (
            "settings timelock",
            SETTINGS_TIMELOCK_SECS,
            clock(7 * DAY, 5 * MIN),
        ),
        ("governance delay", GOV_DELAY_SECS, clock(7 * DAY, 5 * MIN)),
        (
            "recovery delay",
            RECOVERY_DELAY_SECS,
            clock(90 * DAY, 15 * MIN),
        ),
        ("backer maturity", BACKER_MATURITY_SECS, 0),
        ("drip min", DRIP_MIN_SECS, clock(7 * DAY, 2 * MIN)),
        ("drip max", DRIP_MAX_SECS, clock(90 * DAY, 10 * MIN)),
    ];
    for (name, got, want) in cases {
        assert_eq!(got, want, "{name}");
    }
}

#[test]
fn every_setting_has_its_hard_range() {
    let ranges = [
        (K::MinStakeBps, 1, 125),
        (K::MaxStakeBps, 1, 125),
        (K::StakerYieldBps, 0, 10_000),
        (K::CooldownSecs, clock(7 * DAY, MIN), clock(30 * DAY, HOUR)),
        (K::VestingSecs, clock(30 * DAY, MIN), clock(90 * DAY, HOUR)),
        (K::AdmitLowBps, 1, 2_500),
        (K::AdmitMidBps, 1, 2_500),
        (K::AdmitHighBps, 1, 2_500),
        (K::PayoutLowBps, 1, 600),
        (K::PayoutMidBps, 1, 600),
        (K::PayoutHighBps, 1, 600),
        (K::Retired11, 0, 0),
        (K::PoolCap, 1, i64::MAX),
        (K::BackerYieldBps, 0, 10_000),
        (K::PauseMaxSecs, clock(DAY, MIN), 30 * DAY),
        (K::PauseGapSecs, 30 * DAY, 90 * DAY),
        (K::TierARatio, 1, 20),
        (K::TierBRatio, 1, 20),
        (K::TierCRatio, 1, 20),
        (K::TimeGateSecs, clock(90 * DAY, MIN), clock(365 * DAY, DAY)),
        (K::ClaimWindowSecs, 14 * DAY, 90 * DAY),
        (K::ApproveWindowSecs, clock(30 * DAY, MIN), 365 * DAY),
        (K::InactivitySecs, clock(30 * DAY, MIN), 365 * DAY),
        (K::BackerMaturitySecs, 0, clock(7 * DAY, HOUR)),
        (K::DripMinSecs, clock(DAY, MIN), clock(90 * DAY, HOUR)),
        (K::DripMaxSecs, clock(7 * DAY, MIN), clock(180 * DAY, DAY)),
        (K::PointsRateStaker, 0, 100_000),
        (K::PointsRateBacker, 0, 100_000),
    ];
    // One row per setting, in slot order.
    assert_eq!(ranges.map(|r| r.0), K::ALL);
    for (key, min, max) in ranges {
        assert_eq!(key.bounds(), (min, max), "{key:?}");
    }
}
