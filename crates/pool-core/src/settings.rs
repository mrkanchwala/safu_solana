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
//! Never a setting (compile-time constants in `params.rs`): the tier ceilings, the time gate, the
//! claim window, the penalty lock, the oracle approval life, the covered-wallet and per-claim
//! transaction limits, and the timelock itself.
//!
//! What a change touches: cooldown and vesting are read when a claim is approved, the approve window
//! when it is admitted, the inactivity window when it starts paying; a claim keeps the values it
//! started with. Stake bounds, pool cap, daily rates, yield split, backer waits and pause lengths
//! apply from the change on.

use crate::params::*;
use crate::{apply_bps, CoreError, Result};

/// Slots in the pool's settings array. `SETTING_COUNT` are used; the rest are room for upgrades.
pub const SETTING_SLOTS: usize = 32;

/// Every adjustable number. The discriminant is the slot index and part of the client interface:
/// append, never renumber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SettingKey {
    /// Min stake, bps of the pool cap.
    MinStakeBps = 0,
    /// Max stake, bps of the pool cap.
    MaxStakeBps = 1,
    /// Share of yield on staker capital paid to stakers, bps; the rest is protocol revenue.
    StakerYieldBps = 2,
    /// Share of yield on matured backer capital paid to backers, bps; the rest is protocol revenue.
    BackerYieldBps = 3,
    /// Wait between approval and the first payout.
    CooldownSecs = 4,
    /// Linear vesting of the payout after the cooldown.
    VestingSecs = 5,
    /// New entitlement admitted per day, bps of capacity, by utilisation band.
    AdmitLowBps = 6,
    AdmitMidBps = 7,
    AdmitHighBps = 8,
    /// Claim payouts per day, bps of the payout base, by utilisation band.
    PayoutLowBps = 9,
    PayoutMidBps = 10,
    PayoutHighBps = 11,
    /// Notice between a backer's withdrawal request and completion.
    BackerNoticeSecs = 12,
    /// New backer money counts toward capacity only after this wait.
    BackerMaturitySecs = 13,
    /// Longest single pause.
    PauseMaxSecs = 14,
    /// A new pause may start only more than this long after the last one ended.
    PauseGapSecs = 15,
    /// Once the time gate is met, the staker has this long to approve the claim.
    ApproveWindowSecs = 16,
    /// An active claim with no collection for this long can be expired by anyone.
    InactivitySecs = 17,
    /// Most SOL all stakes together may hold, in lamports.
    PoolCap = 18,
}

/// Settings in use. Slots at and above this index are free for upgrades.
pub const SETTING_COUNT: usize = 19;

impl SettingKey {
    pub const ALL: [SettingKey; SETTING_COUNT] = [
        SettingKey::MinStakeBps,
        SettingKey::MaxStakeBps,
        SettingKey::StakerYieldBps,
        SettingKey::BackerYieldBps,
        SettingKey::CooldownSecs,
        SettingKey::VestingSecs,
        SettingKey::AdmitLowBps,
        SettingKey::AdmitMidBps,
        SettingKey::AdmitHighBps,
        SettingKey::PayoutLowBps,
        SettingKey::PayoutMidBps,
        SettingKey::PayoutHighBps,
        SettingKey::BackerNoticeSecs,
        SettingKey::BackerMaturitySecs,
        SettingKey::PauseMaxSecs,
        SettingKey::PauseGapSecs,
        SettingKey::ApproveWindowSecs,
        SettingKey::InactivitySecs,
        SettingKey::PoolCap,
    ];

    /// The key in slot `index`; an unused slot is an error.
    pub fn from_index(index: u8) -> Result<Self> {
        Self::ALL
            .get(index as usize)
            .copied()
            .ok_or(CoreError::UnknownSetting)
    }

    pub const fn index(self) -> usize {
        self as usize
    }

    /// Hard `(min, max)`, inclusive. Changing one means a program upgrade.
    pub const fn bounds(self) -> (i64, i64) {
        const DAY: i64 = SECONDS_PER_DAY;
        const MIN: i64 = SECONDS_PER_MINUTE;
        match self {
            // Up to 5% of the cap per stake.
            SettingKey::MinStakeBps | SettingKey::MaxStakeBps => (1, 500),
            SettingKey::StakerYieldBps | SettingKey::BackerYieldBps => (0, BPS_DENOMINATOR as i64),
            SettingKey::CooldownSecs => (clock(7 * DAY, MIN), clock(30 * DAY, SECONDS_PER_HOUR)),
            SettingKey::VestingSecs => (clock(30 * DAY, MIN), clock(90 * DAY, SECONDS_PER_HOUR)),
            // 25%/day, the default low band.
            SettingKey::AdmitLowBps | SettingKey::AdmitMidBps | SettingKey::AdmitHighBps => {
                (1, 2_500)
            }
            // 6%/day ceiling on claim money leaving (multichain).
            SettingKey::PayoutLowBps | SettingKey::PayoutMidBps | SettingKey::PayoutHighBps => {
                (1, 600)
            }
            SettingKey::BackerNoticeSecs => (0, clock(90 * DAY, SECONDS_PER_HOUR)),
            SettingKey::BackerMaturitySecs => (clock(DAY, MIN), clock(30 * DAY, SECONDS_PER_HOUR)),
            SettingKey::PauseMaxSecs => (clock(DAY, MIN), 30 * DAY),
            // Never below the filing window: see `PAUSE_GAP_SECS`.
            SettingKey::PauseGapSecs => (CLAIM_WINDOW_SECS, 90 * DAY),
            SettingKey::ApproveWindowSecs => (clock(7 * DAY, MIN), 180 * DAY),
            SettingKey::InactivitySecs => (clock(30 * DAY, MIN), 365 * DAY),
            SettingKey::PoolCap => (1, i64::MAX),
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
                SettingKey::BackerYieldBps => BACKER_YIELD_BPS as i64,
                SettingKey::CooldownSecs => COOLDOWN_SECS,
                SettingKey::VestingSecs => VESTING_SECS,
                SettingKey::AdmitLowBps => ADMIT_LOW_BPS as i64,
                SettingKey::AdmitMidBps => ADMIT_MID_BPS as i64,
                SettingKey::AdmitHighBps => ADMIT_HIGH_BPS as i64,
                SettingKey::PayoutLowBps => PAYOUT_LOW_BPS as i64,
                SettingKey::PayoutMidBps => PAYOUT_MID_BPS as i64,
                SettingKey::PayoutHighBps => PAYOUT_HIGH_BPS as i64,
                SettingKey::BackerNoticeSecs => BACKER_NOTICE_SECS,
                SettingKey::BackerMaturitySecs => BACKER_MATURITY_SECS,
                SettingKey::PauseMaxSecs => PAUSE_MAX_SECS,
                SettingKey::PauseGapSecs => PAUSE_GAP_SECS,
                SettingKey::ApproveWindowSecs => APPROVE_WINDOW_SECS,
                SettingKey::InactivitySecs => COLLECTION_INACTIVITY_SECS,
                SettingKey::PoolCap => i64::try_from(pool_cap).map_err(|_| CoreError::Overflow)?,
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

    /// A bps or lamport setting. Bounds keep every one of them non-negative.
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

    pub fn payout_rates(&self) -> Rates {
        Rates {
            low: self.amount(SettingKey::PayoutLowBps),
            mid: self.amount(SettingKey::PayoutMidBps),
            high: self.amount(SettingKey::PayoutHighBps),
        }
    }

    /// `value` may replace `key` now: inside the hard bounds, and consistent with every other live
    /// setting (min stake <= max stake, a non-zero min stake, each rate band no looser than the band
    /// below it in utilisation, a pause no longer than the gap after it). Checked at proposal and
    /// again at execution.
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
            // Paused at most half the time. The hard bounds already imply it; kept as
            // a rule so a later change to either range cannot loosen it.
            SettingKey::PauseMaxSecs => value <= g(SettingKey::PauseGapSecs),
            SettingKey::PauseGapSecs => value >= g(SettingKey::PauseMaxSecs),
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
/// (never below `CLAIM_WINDOW_SECS`) rules that out for every claim still inside its filing window
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
