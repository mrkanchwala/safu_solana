//! Stake bounds, capacity, backer free capital, yield split and liquidity lines.

use pool_core::params::*;
use pool_core::settings::Settings;
use pool_core::{liquidity, stake, yields, CoreError};
use proptest::prelude::*;

const USDC: u64 = 1_000_000;
const CAP: u64 = 50 * USDC;

/// A new pool's settings at `cap`.
fn settings(cap: u64) -> Settings {
    Settings::defaults(cap).unwrap()
}

#[test]
fn stake_bounds_are_the_default_bps_of_the_cap() {
    let bps = |b: u64| CAP * b / BPS_DENOMINATOR;
    assert_eq!(
        stake::bounds(&settings(CAP)),
        Ok((bps(MIN_STAKE_BPS), bps(MAX_STAKE_BPS)))
    );
    // The multichain pool's launch numbers: $10 to $100 at a $100,000 cap.
    assert_eq!(
        stake::bounds(&settings(100_000 * USDC)),
        Ok((10 * USDC, 100 * USDC))
    );
}

#[test]
fn stake_edges() {
    let s = settings(CAP);
    let (min, max) = stake::bounds(&s).unwrap();
    assert_eq!(stake::split_new_stake(min, 0, &s), Ok((min, 0)));
    assert_eq!(stake::split_new_stake(max, 0, &s), Ok((max, 0)));
    // Above the max: the max is staked, the rest is backing.
    assert_eq!(stake::split_new_stake(max + 1, 0, &s), Ok((max, 1)));
    assert_eq!(stake::split_new_stake(3 * max, 0, &s), Ok((max, 2 * max)));
    for below in [0, min - 1] {
        assert_eq!(
            stake::split_new_stake(below, 0, &s),
            Err(CoreError::StakeOutOfRange)
        );
    }
    assert_eq!(stake::split_new_stake(max, CAP - max, &s), Ok((max, 0)));
    // A pool full for stakes refuses the whole deposit, overflow included.
    assert_eq!(
        stake::split_new_stake(max, CAP - max + 1, &s),
        Err(CoreError::PoolCapExceeded)
    );
    assert_eq!(
        stake::split_new_stake(2 * max, CAP - max + 1, &s),
        Err(CoreError::PoolCapExceeded)
    );
}

#[test]
fn backer_rule_3() {
    assert_eq!(pool_core::check_capital_free(60, 100, 40), Ok(()));
    assert_eq!(
        pool_core::check_capital_free(61, 100, 40),
        Err(CoreError::CapitalNotFree)
    );
    // Saturating: taking more than the capacity leaves zero, which still covers zero claims (the last
    // staker out after a yield-venue loss); any open claim then blocks it.
    assert_eq!(pool_core::check_capital_free(0, 10, 11), Ok(()));
    assert_eq!(
        pool_core::check_capital_free(1, 10, 11),
        Err(CoreError::CapitalNotFree)
    );
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
    let c = yields::credit(
        4_000_000,
        3 * USDC,
        USDC,
        STAKER_YIELD_BPS,
        BACKER_YIELD_BPS,
    )
    .unwrap();
    assert_eq!(c.staker_share, 3_000_000);
    assert_eq!(c.backer_share, 1_000_000);
    assert_eq!(c.protocol_share, 0);
}

#[test]
fn owed_follows_the_index_additively() {
    // A late joiner is owed only what was credited after it joined.
    let c1 = yields::credit(1_000_000, USDC, 0, STAKER_YIELD_BPS, BACKER_YIELD_BPS).unwrap();
    let joined_at = c1.staker_index_bump;
    let c2 = yields::credit(2_000_000, 2 * USDC, 0, STAKER_YIELD_BPS, BACKER_YIELD_BPS).unwrap();
    let now = joined_at + c2.staker_index_bump;
    assert_eq!(yields::owed(USDC, now, 0), Ok(1_000_000 + 1_000_000));
    assert_eq!(yields::owed(USDC, now, joined_at), Ok(1_000_000));
    assert_eq!(yields::owed(USDC, joined_at, now), Ok(0));
}

#[test]
fn harvest_limit_is_10bp_per_day() {
    assert_eq!(
        yields::harvest_limit(10 * USDC, SECONDS_PER_DAY),
        Ok(10 * USDC / 1_000)
    );
    assert_eq!(yields::harvest_limit(10 * USDC, 0), Ok(0));
    assert_eq!(yields::harvest_limit(10 * USDC, -5), Ok(0));
}

#[test]
fn buffer_and_push_lines() {
    assert_eq!(liquidity::buffer(10 * USDC, MAX_DEPLOY_BPS), Ok(2 * USDC));
    // Idle 5 USDC, nothing open, nothing deployed: pushes up to the 80% line (8 USDC) -> 5.
    assert_eq!(
        liquidity::push_amount(5 * USDC, 0, 10 * USDC, 0, MAX_DEPLOY_BPS),
        Ok(5 * USDC)
    );
    // Already at the line.
    assert_eq!(
        liquidity::push_amount(5 * USDC, 0, 10 * USDC, 8 * USDC, MAX_DEPLOY_BPS),
        Ok(0)
    );
    // Below AUTO_PUSH_MIN_BPS of capacity: skipped.
    assert_eq!(
        liquidity::push_amount(USDC / 20, 0, 10 * USDC, 0, MAX_DEPLOY_BPS),
        Ok(0)
    );
    // Open claims keep their cash.
    assert_eq!(
        liquidity::push_amount(5 * USDC, 4 * USDC, 10 * USDC, 0, MAX_DEPLOY_BPS),
        Ok(USDC)
    );
    assert_eq!(liquidity::free_liquid(5, 7), 0);
    // The keeper deploys below the push minimum; the room is the same line.
    assert_eq!(
        liquidity::deploy_room(USDC / 20, 0, 10 * USDC, 0, MAX_DEPLOY_BPS),
        Ok(USDC / 20)
    );
}

proptest! {
    #[test]
    fn credit_conserves_every_unit(amount in 0u64..=1_000_000 * USDC, staked in 0u64..=1_000_000 * USDC, backed in 0u64..=1_000_000 * USDC) {
        let c = yields::credit(amount, staked, backed, STAKER_YIELD_BPS, BACKER_YIELD_BPS).unwrap();
        prop_assert_eq!(c.staker_share + c.backer_share + c.protocol_share, amount);
    }

    #[test]
    fn index_owes_no_more_than_was_set_aside(amount in 1u64..=1_000 * USDC, staked in 1u64..=1_000 * USDC) {
        let c = yields::credit(amount, staked, 0, STAKER_YIELD_BPS, BACKER_YIELD_BPS).unwrap();
        // Everyone together (total principal = staked) is owed at most the set-aside.
        prop_assert!(yields::owed(staked, c.staker_index_bump, 0).unwrap() <= c.staker_share);
    }

    #[test]
    fn push_never_crosses_the_deploy_line_or_open_claims(free in 0u64..=100 * USDC, alloc in 0u64..=100 * USDC, cap in 0u64..=100 * USDC, book in 0u64..=100 * USDC) {
        let p = liquidity::push_amount(free, alloc, cap, book, MAX_DEPLOY_BPS).unwrap();
        prop_assert!(p <= free.saturating_sub(alloc));
        prop_assert!(book + p <= (cap as u128 * MAX_DEPLOY_BPS as u128 / BPS_DENOMINATOR as u128) as u64 || p == 0);
    }
}

#[test]
fn stake_bounds_match_the_shared_vector() {
    let mut n = 0;
    for line in include_str!("vectors/stake_bounds.txt")
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        let (input, want) = line.split_once(':').unwrap();
        let v: Vec<u64> = input
            .split_whitespace()
            .map(|x| x.parse().unwrap())
            .collect();
        let w: Vec<u64> = want
            .split_whitespace()
            .map(|x| x.parse().unwrap())
            .collect();
        let mut s = settings(CAP);
        s.values[pool_core::settings::SettingKey::PoolCap.index()] = v[0] as i64;
        s.values[pool_core::settings::SettingKey::MinStakeBps.index()] = v[1] as i64;
        s.values[pool_core::settings::SettingKey::MaxStakeBps.index()] = v[2] as i64;
        assert_eq!(stake::bounds(&s), Ok((w[0], w[1])), "{line}");
        n += 1;
    }
    assert!(n >= 4);
}
