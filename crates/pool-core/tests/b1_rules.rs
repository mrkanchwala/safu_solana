//! Stake bounds, capacity, backer rule 3, yield split and liquidity lines.

use pool_core::params::*;
use pool_core::settings::Settings;
use pool_core::{liquidity, stake, yields, CoreError};
use proptest::prelude::*;

const SOL: u64 = 1_000_000_000;
const CAP: u64 = 50 * SOL;

/// A new pool's settings at `cap`.
fn settings(cap: u64) -> Settings {
    Settings::defaults(cap).unwrap()
}

#[test]
fn stake_bounds_are_the_founder_values_at_a_50_sol_cap() {
    // 0.05 - 0.5 SOL (founder default 2026-09-30).
    assert_eq!(stake::bounds(&settings(CAP)), Ok((SOL / 20, SOL / 2)));
}

#[test]
fn stake_edges() {
    let (min, max) = stake::bounds(&settings(CAP)).unwrap();
    assert_eq!(stake::check_new_stake(min, 0, &settings(CAP)), Ok(()));
    assert_eq!(stake::check_new_stake(max, 0, &settings(CAP)), Ok(()));
    assert_eq!(stake::check_new_stake(min - 1, 0, &settings(CAP)), Err(CoreError::StakeOutOfRange));
    assert_eq!(stake::check_new_stake(max + 1, 0, &settings(CAP)), Err(CoreError::StakeOutOfRange));
    assert_eq!(stake::check_new_stake(0, 0, &settings(CAP)), Err(CoreError::StakeOutOfRange));
    assert_eq!(stake::check_new_stake(max, CAP - max, &settings(CAP)), Ok(()));
    assert_eq!(stake::check_new_stake(max, CAP - max + 1, &settings(CAP)), Err(CoreError::PoolCapExceeded));
}

#[test]
fn backer_rule_3() {
    assert_eq!(pool_core::check_capital_free(60, 100, 40), Ok(()));
    assert_eq!(pool_core::check_capital_free(61, 100, 40), Err(CoreError::CapitalNotFree));
    // Saturating: taking more than the capacity leaves zero, which still covers zero claims (the last
    // staker out after a Marinade loss); any open claim then blocks it.
    assert_eq!(pool_core::check_capital_free(0, 10, 11), Ok(()));
    assert_eq!(pool_core::check_capital_free(1, 10, 11), Err(CoreError::CapitalNotFree));
}

#[test]
fn credit_all_to_protocol_when_nobody_is_in() {
    let c = yields::credit(1_000, 0, 0, STAKER_YIELD_BPS, BACKER_YIELD_BPS).unwrap();
    assert_eq!(c.protocol_share, 1_000);
    assert_eq!(c.staker_share + c.backer_share, 0);
}

#[test]
fn credit_default_split_gives_everything_to_stakers_and_backers() {
    // 100% / 100% by default: 3:1 capacity split, nothing to the protocol beyond dust.
    let c = yields::credit(4_000_000, 3 * SOL, SOL, STAKER_YIELD_BPS, BACKER_YIELD_BPS).unwrap();
    assert_eq!(c.staker_share, 3_000_000);
    assert_eq!(c.backer_share, 1_000_000);
    assert_eq!(c.protocol_share, 0);
}

#[test]
fn owed_follows_the_index_additively() {
    // A late joiner is owed only what was credited after it joined.
    let c1 = yields::credit(1_000_000, SOL, 0, STAKER_YIELD_BPS, BACKER_YIELD_BPS).unwrap();
    let joined_at = c1.staker_index_bump;
    let c2 = yields::credit(2_000_000, 2 * SOL, 0, STAKER_YIELD_BPS, BACKER_YIELD_BPS).unwrap();
    let now = joined_at + c2.staker_index_bump;
    assert_eq!(yields::owed(SOL, now, 0), Ok(1_000_000 + 1_000_000));
    assert_eq!(yields::owed(SOL, now, joined_at), Ok(1_000_000));
    assert_eq!(yields::owed(SOL, joined_at, now), Ok(0));
}

#[test]
fn harvest_limit_is_10bp_per_day() {
    assert_eq!(yields::harvest_limit(10 * SOL, SECONDS_PER_DAY), Ok(10 * SOL / 1_000));
    assert_eq!(yields::harvest_limit(10 * SOL, 0), Ok(0));
    assert_eq!(yields::harvest_limit(10 * SOL, -5), Ok(0));
}

#[test]
fn buffer_and_push_lines() {
    assert_eq!(liquidity::buffer(10 * SOL), Ok(2 * SOL));
    // Idle 5 SOL, nothing open, nothing deployed: pushes up to the 80% line (8 SOL) -> 5.
    assert_eq!(liquidity::push_amount(5 * SOL, 0, 10 * SOL, 0), Ok(5 * SOL));
    // Already at the line.
    assert_eq!(liquidity::push_amount(5 * SOL, 0, 10 * SOL, 8 * SOL), Ok(0));
    // Below AUTO_PUSH_MIN_BPS of capacity: skipped.
    assert_eq!(liquidity::push_amount(SOL / 20, 0, 10 * SOL, 0), Ok(0));
    // Open claims keep their cash.
    assert_eq!(liquidity::push_amount(5 * SOL, 4 * SOL, 10 * SOL, 0), Ok(SOL));
    assert_eq!(liquidity::free_liquid(5, 7), 0);
}

proptest! {
    #[test]
    fn credit_conserves_every_lamport(amount in 0u64..=1_000_000 * SOL, staked in 0u64..=1_000_000 * SOL, backed in 0u64..=1_000_000 * SOL) {
        let c = yields::credit(amount, staked, backed, STAKER_YIELD_BPS, BACKER_YIELD_BPS).unwrap();
        prop_assert_eq!(c.staker_share + c.backer_share + c.protocol_share, amount);
    }

    #[test]
    fn index_owes_no_more_than_was_set_aside(amount in 1u64..=1_000 * SOL, staked in 1u64..=1_000 * SOL) {
        let c = yields::credit(amount, staked, 0, STAKER_YIELD_BPS, BACKER_YIELD_BPS).unwrap();
        // Everyone together (total principal = staked) is owed at most the set-aside.
        prop_assert!(yields::owed(staked, c.staker_index_bump, 0).unwrap() <= c.staker_share);
    }

    #[test]
    fn push_never_crosses_the_deploy_line_or_open_claims(free in 0u64..=100 * SOL, alloc in 0u64..=100 * SOL, cap in 0u64..=100 * SOL, book in 0u64..=100 * SOL) {
        let p = liquidity::push_amount(free, alloc, cap, book).unwrap();
        prop_assert!(p <= free.saturating_sub(alloc));
        prop_assert!(book + p <= (cap as u128 * DEPLOY_BPS as u128 / BPS_DENOMINATOR as u128) as u64 || p == 0);
    }
}
