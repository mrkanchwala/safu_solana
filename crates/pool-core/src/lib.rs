//! pool-core: every rule of the SAFU Solana pool as a pure function over integers.
//!
//! The Anchor program (`programs/safu_pool`) owns accounts, signers and CPIs, and calls in here for
//! every number, so no formula is written twice. Rules follow the multichain protection pool.
//!
//! - **No floats, no allocation, no chain dependencies.**
//! - **Never panics on input.** Overflow and bad input return [`CoreError`].
//! - **Rounding favours the pool:** payouts and yield round down, amounts owed to the pool round up.
#![no_std]

pub mod liquidity;
pub mod marinade;
pub mod params;
pub mod stake;
pub mod yields;

/// Maths failures and rule violations. The program maps each one to its own error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreError {
    Overflow,
    DivideByZero,
    InvalidParameter,
    /// Stake outside `[MIN_STAKE_BPS, MAX_STAKE_BPS]` of the pool cap.
    StakeOutOfRange,
    /// Stake would take total staked above the pool cap.
    PoolCapExceeded,
    /// A backer withdrawal would leave open claims without the capital they need (backer rule 3).
    BackerCapitalNotFree,
}

pub type Result<T> = core::result::Result<T, CoreError>;

pub(crate) fn to_u64(v: u128) -> Result<u64> {
    u64::try_from(v).map_err(|_| CoreError::Overflow)
}

/// Checked `a + b`.
pub fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).ok_or(CoreError::Overflow)
}

/// Checked `a - b`.
pub fn sub(a: u64, b: u64) -> Result<u64> {
    a.checked_sub(b).ok_or(CoreError::Overflow)
}

/// Pool capacity: staker principal plus matured backer money.
pub fn capacity(total_staked: u64, total_backed: u64) -> Result<u64> {
    add(total_staked, total_backed)
}

/// Backer rule 3: only free capital can leave. After taking `amount` out, open claims
/// (`total_allocated`) must still fit in capacity.
pub fn check_backer_capital_free(total_allocated: u64, capacity: u64, amount: u64) -> Result<()> {
    match capacity.checked_sub(amount) {
        Some(left) if total_allocated <= left => Ok(()),
        _ => Err(CoreError::BackerCapitalNotFree),
    }
}

/// `amount x numerator / denominator`, rounded down.
pub fn mul_div_floor(amount: u64, numerator: u64, denominator: u64) -> Result<u64> {
    if denominator == 0 {
        return Err(CoreError::DivideByZero);
    }
    to_u64(amount as u128 * numerator as u128 / denominator as u128)
}

/// `amount x numerator / denominator`, rounded up.
pub fn mul_div_ceil(amount: u64, numerator: u64, denominator: u64) -> Result<u64> {
    if denominator == 0 {
        return Err(CoreError::DivideByZero);
    }
    to_u64((amount as u128 * numerator as u128).div_ceil(denominator as u128))
}

/// `amount x bps / BPS_DENOMINATOR`, rounded down.
pub fn apply_bps(amount: u64, bps: u64) -> Result<u64> {
    mul_div_floor(amount, bps, params::BPS_DENOMINATOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mul_div_rounds_each_way() {
        assert_eq!(mul_div_floor(10, 1, 3), Ok(3));
        assert_eq!(mul_div_ceil(10, 1, 3), Ok(4));
        assert_eq!(mul_div_ceil(9, 1, 3), Ok(3));
    }

    #[test]
    fn divide_by_zero_and_overflow_are_errors() {
        assert_eq!(mul_div_floor(1, 1, 0), Err(CoreError::DivideByZero));
        assert_eq!(mul_div_ceil(1, 1, 0), Err(CoreError::DivideByZero));
        assert_eq!(mul_div_floor(u64::MAX, u64::MAX, 1), Err(CoreError::Overflow));
    }

    #[test]
    fn demo_clocks_are_shorter() {
        use params::*;
        if DEMO_BUILD {
            assert_eq!(TIME_GATE_SECS, 2 * SECONDS_PER_MINUTE);
            assert_eq!(COOLDOWN_SECS, SECONDS_PER_MINUTE);
            assert_eq!(VESTING_SECS, 5 * SECONDS_PER_MINUTE);
        } else {
            assert_eq!(TIME_GATE_SECS, 90 * SECONDS_PER_DAY);
            assert_eq!(COOLDOWN_SECS, 7 * SECONDS_PER_DAY);
            assert_eq!(VESTING_SECS, 45 * SECONDS_PER_DAY);
        }
        // Real time in both builds.
        assert_eq!(CLAIM_WINDOW_SECS, 30 * SECONDS_PER_DAY);
        assert_eq!(MAX_APPROVAL_WINDOW_SECS, 24 * SECONDS_PER_HOUR);
    }
}
