//! Adjustable pool settings: which numbers can change, their defaults and their hard bounds.
//!
//! Every setting sits in one slot of the pool's `settings` array and is read through [`Settings`],
//! the one read path. That keeps the pool open to later program upgrades:
//! - a new setting takes the next free slot (`SETTING_SLOTS` leaves room), no account change;
//! - a setting can become self-managing (computed from pool state instead of stored) by changing
//!   [`Settings::get`] alone;
//! - a range is changed by changing [`SettingKey::bounds`] in an upgrade.
//!
//! A change goes admin proposes → co-signer approves the same value → `SETTINGS_TIMELOCK_SECS` →
//! anyone executes, with bounds and cross-setting order re-checked at execution (multichain
//! `settings.rs`). Stakers see every change coming and can leave first.
//!
//! Never a setting (compile-time constants in `params.rs`): the penalty lock, the oracle approval
//! life, the covered-wallet and per-claim transaction limits, and the timelocks themselves.
//!
//! What a change touches: tier ratios, the time gate and the claim window are fixed on each stake when
//! it is made (a tier-ratio raise still reaches it: the higher value applies); cooldown and vesting
//! are read when a claim is approved, the approve window when it is admitted, the inactivity window
//! when it starts paying; a claim keeps the values it started with. Stake bounds, pool cap, daily
//! rates, yield split, backer waits, drip and pause lengths apply from the change on.
//!
//! The list, its numbering, bounds, defaults and cross-rules follow the multichain pool
//! (`protection-pool/src/settings.rs`), in seconds where it counts ledgers.

use crate::params::*;
use crate::{apply_bps, CoreError, Result};

/// Slots in the pool's settings array. `SETTING_COUNT` are used; the rest are room for upgrades.
pub const SETTING_SLOTS: usize = 32;

/// Every adjustable number. The discriminant is the slot index and part of the client interface:
/// append, never renumber. Same numbering as the multichain pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SettingKey {
    /// Min stake, bps of the pool cap.
    MinStakeBps = 0,
    /// Max stake, bps of the pool cap.
    MaxStakeBps = 1,
    /// Share of yield on staker capital paid to stakers, bps; the rest is protocol revenue.
    StakerYieldBps = 2,
    /// Wait between approval and the first payout.
    CooldownSecs = 3,
    /// Linear vesting of the payout after the cooldown.
    VestingSecs = 4,
    /// New entitlement admitted per day, bps of capacity, by utilisation band.
    AdmitLowBps = 5,
    AdmitMidBps = 6,
    AdmitHighBps = 7,
    /// Claim payouts per day, bps of the payout base, by utilisation band.
    PayoutLowBps = 8,
    PayoutMidBps = 9,
    PayoutHighBps = 10,
    /// Unused: the backer notice, replaced by the daily drip. Kept so the slot numbers match
    /// multichain; no proposal can name it.
    Retired11 = 11,
    /// Most USDC all stakes together may hold, in USDC base units. Raised through this timelock;
    /// lowered at once by the admin (`lower_pool_cap`, a safety brake).
    PoolCap = 12,
    /// Share of yield on matured backer capital paid to backers, bps; the rest is protocol revenue.
    BackerYieldBps = 13,
    /// Longest single pause.
    PauseMaxSecs = 14,
    /// A new pause may start only more than this long after the last one ended.
    PauseGapSecs = 15,
    /// Tier coverage ceilings, multiples of the stake. A >= B >= C. Fixed per stake; a raise reaches
    /// every stake (the higher of the stake's value and the live one applies).
    TierARatio = 16,
    TierBRatio = 17,
    TierCRatio = 18,
    /// A stake must be this old before a claim on it can be approved. Fixed per stake.
    TimeGateSecs = 19,
    /// A claim must be filed within this long of the hack. Fixed per stake.
    ClaimWindowSecs = 20,
    /// Once the time gate is met, the staker has this long to approve the claim.
    ApproveWindowSecs = 21,
    /// An active claim with no collection for this long can be expired by anyone.
    InactivitySecs = 22,
    /// New backer money counts toward capacity only after this wait.
    BackerMaturitySecs = 23,
    /// Backer exit drip length: min when the pool is idle, max at high utilisation. Min <= max.
    DripMinSecs = 24,
    DripMaxSecs = 25,
    /// Points per USDC per day, in units of `POINTS_RATE_ONE`.
    PointsRateStaker = 26,
    PointsRateBacker = 27,
}

/// Settings in use. Slots at and above this index are free for upgrades.
pub const SETTING_COUNT: usize = 28;

impl SettingKey {
    pub const ALL: [SettingKey; SETTING_COUNT] = [
        SettingKey::MinStakeBps,
        SettingKey::MaxStakeBps,
        SettingKey::StakerYieldBps,
        SettingKey::CooldownSecs,
        SettingKey::VestingSecs,
        SettingKey::AdmitLowBps,
        SettingKey::AdmitMidBps,
        SettingKey::AdmitHighBps,
        SettingKey::PayoutLowBps,
        SettingKey::PayoutMidBps,
        SettingKey::PayoutHighBps,
        SettingKey::Retired11,
        SettingKey::PoolCap,
        SettingKey::BackerYieldBps,
        SettingKey::PauseMaxSecs,
        SettingKey::PauseGapSecs,
        SettingKey::TierARatio,
        SettingKey::TierBRatio,
        SettingKey::TierCRatio,
        SettingKey::TimeGateSecs,
        SettingKey::ClaimWindowSecs,
        SettingKey::ApproveWindowSecs,
        SettingKey::InactivitySecs,
        SettingKey::BackerMaturitySecs,
        SettingKey::DripMinSecs,
        SettingKey::DripMaxSecs,
        SettingKey::PointsRateStaker,
        SettingKey::PointsRateBacker,
    ];

    /// The key in slot `index`; an unused or retired slot is an error.
    pub fn from_index(index: u8) -> Result<Self> {
        Self::ALL
            .get(index as usize)
            .copied()
            .filter(|k| *k != SettingKey::Retired11)
            .ok_or(CoreError::UnknownSetting)
    }

    pub const fn index(self) -> usize {
        self as usize
    }

    /// Hard `(min, max)`, inclusive. Changing one means a program upgrade. Multichain bounds; a
    /// duration's lower end is shorter in the demo build so fast-clock tests can reach it.
    pub const fn bounds(self) -> (i64, i64) {
        const DAY: i64 = SECONDS_PER_DAY;
        const MIN: i64 = SECONDS_PER_MINUTE;
        const HOUR: i64 = SECONDS_PER_HOUR;
        match self {
            // 0.01% to 1.25% of the cap per stake.
            SettingKey::MinStakeBps | SettingKey::MaxStakeBps => (1, 125),
            SettingKey::StakerYieldBps | SettingKey::BackerYieldBps => (0, BPS_DENOMINATOR as i64),
            SettingKey::CooldownSecs => (clock(7 * DAY, MIN), clock(30 * DAY, HOUR)),
            SettingKey::VestingSecs => (clock(30 * DAY, MIN), clock(90 * DAY, HOUR)),
            // 25%/day, the default low band.
            SettingKey::AdmitLowBps | SettingKey::AdmitMidBps | SettingKey::AdmitHighBps => {
                (1, 2_500)
            }
            // 6%/day ceiling on claim money leaving.
            SettingKey::PayoutLowBps | SettingKey::PayoutMidBps | SettingKey::PayoutHighBps => {
                (1, 600)
            }
            SettingKey::Retired11 => (0, 0),
            SettingKey::PoolCap => (1, i64::MAX),
            SettingKey::PauseMaxSecs => (clock(DAY, MIN), 30 * DAY),
            SettingKey::PauseGapSecs => (30 * DAY, 90 * DAY),
            SettingKey::TierARatio | SettingKey::TierBRatio | SettingKey::TierCRatio => (1, 20),
            // Never below 90 days in the real build.
            SettingKey::TimeGateSecs => (clock(90 * DAY, MIN), clock(365 * DAY, DAY)),
            // Real time in both builds.
            SettingKey::ClaimWindowSecs => (14 * DAY, 90 * DAY),
            SettingKey::ApproveWindowSecs | SettingKey::InactivitySecs => {
                (clock(30 * DAY, MIN), 365 * DAY)
            }
            SettingKey::BackerMaturitySecs => (0, clock(7 * DAY, HOUR)),
            SettingKey::DripMinSecs => (clock(DAY, MIN), clock(90 * DAY, HOUR)),
            SettingKey::DripMaxSecs => (clock(7 * DAY, MIN), clock(180 * DAY, DAY)),
            SettingKey::PointsRateStaker | SettingKey::PointsRateBacker => {
                (0, 10 * POINTS_RATE_ONE as i64)
            }
        }
    }
}

/// Tier coverage ratios (multiples of the stake), A >= B >= C.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierRatios {
    pub a: u64,
    pub b: u64,
    pub c: u64,
}

impl TierRatios {
    /// Each tier at the higher of `self` (fixed on a stake) and `live`: a cut never reaches an
    /// existing stake, a raise does.
    pub fn at_least(self, live: TierRatios) -> TierRatios {
        TierRatios {
            a: self.a.max(live.a),
            b: self.b.max(live.b),
            c: self.c.max(live.c),
        }
    }
}

/// Daily rates by utilisation band (low / mid / high), bps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rates {
    pub low: u64,
    pub mid: u64,
    pub high: u64,
}

/// A view over the pool's settings array. The one place a setting is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub values: [i64; SETTING_SLOTS],
}

impl Settings {
    /// The values a new pool starts with (the `params.rs` defaults). Unused slots are zero.
    pub fn defaults(pool_cap: u64) -> Result<Self> {
        let mut values = [0i64; SETTING_SLOTS];
        for key in SettingKey::ALL {
            values[key.index()] = match key {
                SettingKey::MinStakeBps => MIN_STAKE_BPS as i64,
                SettingKey::MaxStakeBps => MAX_STAKE_BPS as i64,
                SettingKey::StakerYieldBps => STAKER_YIELD_BPS as i64,
                SettingKey::CooldownSecs => COOLDOWN_SECS,
                SettingKey::VestingSecs => VESTING_SECS,
                SettingKey::AdmitLowBps => ADMIT_LOW_BPS as i64,
                SettingKey::AdmitMidBps => ADMIT_MID_BPS as i64,
                SettingKey::AdmitHighBps => ADMIT_HIGH_BPS as i64,
                SettingKey::PayoutLowBps => PAYOUT_LOW_BPS as i64,
                SettingKey::PayoutMidBps => PAYOUT_MID_BPS as i64,
                SettingKey::PayoutHighBps => PAYOUT_HIGH_BPS as i64,
                SettingKey::Retired11 => 0,
                SettingKey::PoolCap => i64::try_from(pool_cap).map_err(|_| CoreError::Overflow)?,
                SettingKey::BackerYieldBps => BACKER_YIELD_BPS as i64,
                SettingKey::PauseMaxSecs => PAUSE_MAX_SECS,
                SettingKey::PauseGapSecs => PAUSE_GAP_SECS,
                SettingKey::TierARatio => TIER_A_RATIO as i64,
                SettingKey::TierBRatio => TIER_B_RATIO as i64,
                SettingKey::TierCRatio => TIER_C_RATIO as i64,
                SettingKey::TimeGateSecs => TIME_GATE_SECS,
                SettingKey::ClaimWindowSecs => CLAIM_WINDOW_SECS,
                SettingKey::ApproveWindowSecs => APPROVE_WINDOW_SECS,
                SettingKey::InactivitySecs => COLLECTION_INACTIVITY_SECS,
                SettingKey::BackerMaturitySecs => BACKER_MATURITY_SECS,
                SettingKey::DripMinSecs => DRIP_MIN_SECS,
                SettingKey::DripMaxSecs => DRIP_MAX_SECS,
                SettingKey::PointsRateStaker | SettingKey::PointsRateBacker => {
                    POINTS_RATE_ONE as i64
                }
            };
        }
        let s = Settings { values };
        for key in SettingKey::ALL {
            s.check_value(key, s.get(key))?;
        }
        Ok(s)
    }

    /// Live value of a setting.
    pub fn get(&self, key: SettingKey) -> i64 {
        self.values[key.index()]
    }

    /// A bps or USDC-amount setting. Bounds keep every one of them non-negative.
    pub fn amount(&self, key: SettingKey) -> u64 {
        self.get(key).max(0) as u64
    }

    pub fn pool_cap(&self) -> u64 {
        self.amount(SettingKey::PoolCap)
    }

    pub fn admit_rates(&self) -> Rates {
        Rates {
            low: self.amount(SettingKey::AdmitLowBps),
            mid: self.amount(SettingKey::AdmitMidBps),
            high: self.amount(SettingKey::AdmitHighBps),
        }
    }

    /// The live tier ratios. A stake keeps its own (`StakeRecord`), combined with these through
    /// [`TierRatios::at_least`].
    pub fn tier_ratios(&self) -> TierRatios {
        TierRatios {
            a: self.amount(SettingKey::TierARatio),
            b: self.amount(SettingKey::TierBRatio),
            c: self.amount(SettingKey::TierCRatio),
        }
    }

    pub fn payout_rates(&self) -> Rates {
        Rates {
            low: self.amount(SettingKey::PayoutLowBps),
            mid: self.amount(SettingKey::PayoutMidBps),
            high: self.amount(SettingKey::PayoutHighBps),
        }
    }

    /// A change through the timelock: [`Settings::check_value`], and the pool cap only goes up this
    /// way (lowering it is the admin's immediate brake, [`Settings::check_lower_cap`]). Checked at
    /// proposal and again at execution.
    pub fn check_change(&self, key: SettingKey, value: i64) -> Result<()> {
        self.check_value(key, value)?;
        if key == SettingKey::PoolCap && value <= self.get(SettingKey::PoolCap) {
            return Err(CoreError::SettingOrderInvalid);
        }
        Ok(())
    }

    /// The admin's immediate cap cut: strictly lower, and still a non-zero min stake.
    pub fn check_lower_cap(&self, value: i64) -> Result<()> {
        if value >= self.get(SettingKey::PoolCap) {
            return Err(CoreError::SettingOrderInvalid);
        }
        self.check_value(SettingKey::PoolCap, value)
    }

    /// `value` fits `key` beside every other live setting: inside the hard bounds, min stake <= max
    /// stake with a non-zero min stake, each rate band no looser than the band below it, tier ratios
    /// A >= B >= C, drip min <= max, and a pause gap no shorter than the longest pause or the claim
    /// window (so at most one pause ever falls inside a filing window).
    pub fn check_value(&self, key: SettingKey, value: i64) -> Result<()> {
        let (min, max) = key.bounds();
        if value < min || value > max {
            return Err(CoreError::SettingOutOfBounds);
        }
        let g = |k| self.get(k);
        let min_stake_positive = |cap: i64, bps: i64| -> Result<bool> {
            Ok(apply_bps(cap.max(0) as u64, bps.max(0) as u64)? > 0)
        };
        let ok = match key {
            SettingKey::MinStakeBps => {
                value <= g(SettingKey::MaxStakeBps)
                    && min_stake_positive(g(SettingKey::PoolCap), value)?
            }
            SettingKey::MaxStakeBps => value >= g(SettingKey::MinStakeBps),
            SettingKey::PoolCap => min_stake_positive(value, g(SettingKey::MinStakeBps))?,
            SettingKey::AdmitLowBps => value >= g(SettingKey::AdmitMidBps),
            SettingKey::AdmitMidBps => {
                value <= g(SettingKey::AdmitLowBps) && value >= g(SettingKey::AdmitHighBps)
            }
            SettingKey::AdmitHighBps => value <= g(SettingKey::AdmitMidBps),
            SettingKey::PayoutLowBps => value >= g(SettingKey::PayoutMidBps),
            SettingKey::PayoutMidBps => {
                value <= g(SettingKey::PayoutLowBps) && value >= g(SettingKey::PayoutHighBps)
            }
            SettingKey::PayoutHighBps => value <= g(SettingKey::PayoutMidBps),
            // Paused at most half the time.
            SettingKey::PauseMaxSecs => value <= g(SettingKey::PauseGapSecs),
            SettingKey::PauseGapSecs => {
                value >= g(SettingKey::PauseMaxSecs) && value >= g(SettingKey::ClaimWindowSecs)
            }
            SettingKey::ClaimWindowSecs => value <= g(SettingKey::PauseGapSecs),
            SettingKey::TierARatio => value >= g(SettingKey::TierBRatio),
            SettingKey::TierBRatio => {
                value <= g(SettingKey::TierARatio) && value >= g(SettingKey::TierCRatio)
            }
            SettingKey::TierCRatio => value <= g(SettingKey::TierBRatio),
            SettingKey::DripMinSecs => value <= g(SettingKey::DripMaxSecs),
            SettingKey::DripMaxSecs => value >= g(SettingKey::DripMinSecs),
            _ => true,
        };
        if !ok {
            return Err(CoreError::SettingOrderInvalid);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Pause clock
// ---------------------------------------------------------------------------------------------

/// Seconds the pool had spent paused by time `t`, from the stored pause record:
/// `paused_before` (every pause before the last one), and the last pause `[started_at, until)`.
/// For a `t` before the last pause this returns `paused_before`, which is exact only if no earlier
/// pause ended after `t`; otherwise it overstates the mark and shortens the window. The pause gap
/// (never below the claim window setting) rules that out for every claim still inside its filing window
/// for this reason: a hack before an earlier pause is past the window before the next pause starts.
pub fn paused_secs_at(paused_before: i64, started_at: i64, until: i64, t: i64) -> i64 {
    if started_at == 0 || t <= started_at {
        return paused_before;
    }
    paused_before.saturating_add(t.min(until).saturating_sub(started_at))
}

/// The claim clock: `now` minus the time paused since `mark` (the paused total when the window
/// started). Every claim window compares its deadline against this, so a pause stops the clock.
pub fn claim_clock(now: i64, paused_now: i64, mark: i64) -> i64 {
    now.saturating_sub(paused_now.saturating_sub(mark).max(0))
}

/// After an earlier pause, more than the gap has passed since it ended (the first pause needs no
/// gap). Strictly more: with the gap at the filing window, a hack during one pause is then always
/// past its window before the next pause starts, so `paused_secs_at` is exact.
/// The caller checks separately that the pool is not paused now.
pub fn pause_gap_passed(now: i64, started_at: i64, until: i64, gap: i64) -> bool {
    started_at == 0 || now > until.saturating_add(gap)
}

/// Time the pool could not pay, as of now (multichain `get_blocked_secs`): seconds paused
/// plus one whole day for every day a payout found the pool short of cash. Only differences of it are
/// used (a claim window adds back what it grew by since the window started), so it never resets.
pub fn blocked_secs(paused_secs: i64, cash_short_days: u64) -> i64 {
    let days = i64::try_from(cash_short_days).unwrap_or(i64::MAX);
    paused_secs.saturating_add(days.saturating_mul(crate::params::SECONDS_PER_DAY))
}

/// Counts `day` as a cash-short day, once: returns the new `(last_day, days)`.
pub fn mark_cash_short_day(last_day: i64, days: u64, day: i64) -> (i64, u64) {
    if last_day == day && days > 0 {
        return (last_day, days);
    }
    (day, days.saturating_add(1))
}
