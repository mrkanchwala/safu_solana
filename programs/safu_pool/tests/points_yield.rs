//! Points (dollar-days), yield kept as owed, and the views (multichain `points.rs`, same rules on
//! yield). Every number comes from the live settings and params, so each test holds in both the
//! normal and the demo build.

mod common;

use common::*;
use pool_core::params::POINTS_RATE_ONE;
use pool_core::settings::SettingKey as K;
use safu_pool::constants::*;
use safu_pool::state::Pool;
use solana_signer::Signer;

/// Points `amount` earns over `rate_secs` (rate x seconds, summed), as the program counts them.
fn points(amount: u64, rate_secs: u128) -> u64 {
    (amount as u128 * rate_secs
        / (USDC_UNIT as u128 * POINTS_RATE_ONE as u128 * SECONDS_PER_DAY as u128)) as u64
}

/// Credits `amount` of yield to the stakers' index (as a harvest would), with the cash.
fn credit_stakers(env: &mut Env, amount: u64) -> u64 {
    let p = env.pool_state();
    let c = pool_core::yields::credit(
        amount,
        p.total_staked,
        p.total_backed,
        BPS_DENOMINATOR,
        BPS_DENOMINATOR,
    )
    .unwrap();
    env.edit::<Pool>(&env.pool(), |p| {
        p.staker_yield_index += c.staker_index_bump;
        p.staker_yield_reserved += c.staker_share;
        p.backer_yield_index += c.backer_index_bump;
        p.backer_yield_reserved += c.backer_share;
    });
    let v = env.vault();
    env.add_token(&v, amount);
    c.staker_share + c.backer_share
}

#[test]
fn a_rate_change_counts_from_its_moment_on() {
    let mut env = Env::new();
    let (_, max) = bounds();
    let s = env.staker(max);
    let one = POINTS_RATE_ONE as u128;
    env.warp(SECONDS_PER_DAY);
    // `set_setting` waits out the timelock at the old rate, then switches to double.
    env.set_setting(K::PointsRateStaker, 2 * POINTS_RATE_ONE as i64);
    let before = (SECONDS_PER_DAY + SETTINGS_TIMELOCK_SECS) as u128 * one;
    assert_eq!(env.view_staker(&s.pubkey()).points, points(max, before));
    env.warp(SECONDS_PER_DAY);
    let after = before + SECONDS_PER_DAY as u128 * 2 * one;
    assert_eq!(env.view_staker(&s.pubkey()).points, points(max, after));
    // Rate 0: points stop, nothing earned is taken back.
    env.set_setting(K::PointsRateStaker, 0);
    let stopped = env.view_staker(&s.pubkey()).points;
    env.warp(10 * SECONDS_PER_DAY);
    assert_eq!(env.view_staker(&s.pubkey()).points, stopped);
}

#[test]
fn backers_earn_points_and_yield_on_counted_money_and_see_what_they_can_collect() {
    let mut env = Env::new();
    let (_, max) = bounds();
    env.staker(max);
    let b = env.matured_backer(max);
    let start = env.now;
    let shares = credit_stakers(&mut env, unit());
    env.warp(SECONDS_PER_DAY);
    let v = env.view_backer(&b.pubkey());
    let rate_secs = (env.now - start) as u128 * POINTS_RATE_ONE as u128;
    assert_eq!(v.points, points(max, rate_secs));
    assert_eq!(v.yield_owed, env.pool_state().backer_yield_reserved);
    assert!(v.yield_owed > 0 && v.yield_owed < shares);
    assert_eq!(v.collectable, 0);
    // A withdrawal request: the view shows exactly what a collection then pays.
    let ix = env.request_backer_ix(&b.pubkey(), max);
    env.ok(&[ix], &[&b]);
    env.warp(SECONDS_PER_DAY);
    let v = env.view_backer(&b.pubkey());
    assert!(v.collectable > 0);
    let before = env.usdc(&b.pubkey());
    let ix = env.complete_backer_ix(&b.pubkey());
    env.ok(&[ix], &[&b]);
    assert_eq!(env.usdc(&b.pubkey()), before + v.collectable + v.yield_owed);
}

#[test]
fn yield_is_paid_while_paused() {
    let mut env = Env::new();
    let (_, max) = bounds();
    let s = env.staker(max);
    let b = env.matured_backer(max);
    credit_stakers(&mut env, unit());
    env.pause();
    let (owed_s, owed_b) = (
        env.view_staker(&s.pubkey()).yield_owed,
        env.view_backer(&b.pubkey()).yield_owed,
    );
    assert!(owed_s > 0 && owed_b > 0);
    let (us, ub) = (env.usdc(&s.pubkey()), env.usdc(&b.pubkey()));
    let ix = env.claim_yield_ix(&s.pubkey(), &s.pubkey());
    env.ok(&[ix], &[&s]);
    let ix = env.claim_backer_yield_ix(&b.pubkey());
    env.ok(&[ix], &[&b]);
    assert_eq!(
        (env.usdc(&s.pubkey()), env.usdc(&b.pubkey())),
        (us + owed_s, ub + owed_b)
    );
}

#[test]
fn yield_the_set_aside_cannot_cover_stays_owed_through_a_full_exit_and_a_new_stake() {
    let mut env = Env::new();
    let (_, max) = bounds();
    let s = env.staker(max);
    credit_stakers(&mut env, unit());
    let owed = env.view_staker(&s.pubkey()).yield_owed;
    // A venue loss left the set-aside short by a third.
    let short = owed / 3;
    env.edit::<Pool>(&env.pool(), |p| p.staker_yield_reserved -= short);
    let before = env.usdc(&s.pubkey());
    let ix = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    env.ok(&[ix], &[&s]);
    assert_eq!(env.usdc(&s.pubkey()), before + max + owed - short);
    assert_eq!(env.stake_state(&s.pubkey()).yield_owed, short);
    // Nothing to pay it from yet.
    let ix = env.claim_yield_ix(&s.pubkey(), &s.pubkey());
    assert_err(
        env.send(std::slice::from_ref(&ix), &[&s]),
        safu_pool::errors::PoolError::NothingToClaim,
    );
    // Staking again keeps it owed; once the set-aside has it, it is paid.
    let ix2 = env.stake_ix(&s.pubkey(), max, s.pubkey());
    env.ok(&[ix2], &[&s]);
    assert_eq!(env.stake_state(&s.pubkey()).yield_owed, short);
    env.edit::<Pool>(&env.pool(), |p| p.staker_yield_reserved += short);
    let before = env.usdc(&s.pubkey());
    env.ok(&[ix], &[&s]);
    assert_eq!(env.usdc(&s.pubkey()), before + short);
}
