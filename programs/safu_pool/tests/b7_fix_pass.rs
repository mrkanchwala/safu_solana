//! B7 fix pass: adjustable settings (timelock), pause gap (X2), pause-proof claim windows (X1), one
//! free-capital rule for every exit with partial stake withdrawal (X3), no admin yield while paused
//! (X4), beneficiary never a pool account, and the room left for upgrades (versions, spare bytes,
//! asset field).
//!
//! Every clock is read from the live settings, so each test holds in both the normal and the demo
//! build.

mod common;

use common::*;
use pool_core::params::{CLAIM_WINDOW_SECS, SECONDS_PER_DAY, SETTINGS_TIMELOCK_SECS, TIME_GATE_SECS};
use pool_core::settings::{SettingKey as K, SETTING_COUNT};
use safu_pool::constants::*;
use safu_pool::errors::PoolError;
use safu_pool::state::{ClaimStatus, Pool, StakeRecord, StakerWallets, ACCOUNT_VERSION, PENDING_APPROVED, PENDING_PROPOSED};
use solana_keypair::Keypair;
use solana_signer::Signer;

const TX: [u8; 32] = [7; 32];
const TX2: [u8; 32] = [8; 32];

fn max_stake() -> u64 {
    bounds().1
}

fn min_stake() -> u64 {
    bounds().0
}

fn transition(env: &mut Env, s: &Keypair, tx: &[u8; 32], data: impl anchor_lang::InstructionData) -> Result<(), String> {
    let ix = env.transition_ix(&s.pubkey(), tx, data);
    let anyone = env.funded(SOL);
    env.send(&[ix], &[&anyone])
}

fn expire_approval(env: &mut Env, s: &Keypair) -> Result<(), String> {
    transition(env, s, &TX, safu_pool::instruction::ExpirePendingApproval {})
}

fn expire_stale(env: &mut Env, s: &Keypair) -> Result<(), String> {
    transition(env, s, &TX, safu_pool::instruction::ExpireStaleClaim {})
}

fn expire_queued(env: &mut Env, s: &Keypair) -> Result<(), String> {
    transition(env, s, &TX, safu_pool::instruction::ExpireQueuedClaim {})
}

fn try_pause(env: &mut Env) -> Result<(), String> {
    let admin = env.admin.insecure_clone();
    let ix = env.admin_ix(safu_pool::instruction::Pause {});
    env.send(&[ix], &[&admin])
}

/// A backed pool and one max staker whose claim waits for approval.
fn awaiting_claim() -> (Env, Keypair) {
    let mut env = Env::new();
    env.matured_backer(40 * SOL);
    let s = env.staker(max_stake());
    env.warp(TIME_GATE_SECS);
    let a = env.approval(&s.pubkey(), TX, max_stake(), TIER_A);
    env.submit(&a).unwrap();
    assert_eq!(env.claim_state(&s.pubkey(), &TX).status, ClaimStatus::AwaitingApproval);
    (env, s)
}

/// As `awaiting_claim`, approved: cooldown running.
fn active_claim() -> (Env, Keypair) {
    let (mut env, s) = awaiting_claim();
    let ix = env.approve_claim_ix(&s.pubkey(), &TX);
    env.ok(&[ix], &[&s]);
    (env, s)
}

/// The founder's example, scaled to the stake bounds. Ten max stakes (capacity 10M) and a claim of
/// 2.4M admitted on the first (time gate not met, so it stays allocated): 7.6M is free capital.
/// Returns the pool, the claimant, and the nine other stakers.
fn tight_pool() -> (Env, Keypair, Vec<Keypair>) {
    let mut env = Env::new();
    let m = max_stake();
    let claimant = env.staker(m);
    let others: Vec<Keypair> = (0..9).map(|_| env.staker(m)).collect();
    let a = env.approval(&claimant.pubkey(), TX, m * 24 / 10, TIER_A);
    env.submit(&a).unwrap();
    assert_eq!(env.claim_state(&claimant.pubkey(), &TX).status, ClaimStatus::PendingTime);
    assert_eq!(env.pool_state().total_allocated, m * 24 / 10);
    (env, claimant, others)
}

fn withdraw(env: &mut Env, s: &Keypair, amount: u64) -> Result<(), String> {
    let ix = env.withdraw_part_ix(&s.pubkey(), &s.pubkey(), amount);
    env.send(&[ix], &[s])
}

fn emergency(env: &mut Env, s: &Keypair, amount: u64) -> Result<(), String> {
    let ix = env.emergency_exit_part_ix(&s.pubkey(), amount);
    env.send(&[ix], &[s])
}

// ================================================================== settings

#[test]
fn a_new_pool_starts_on_the_default_settings() {
    let env = Env::new();
    let p = env.pool_state();
    let defaults = pool_core::settings::Settings::defaults(env.cfg.pool_cap).unwrap();
    assert_eq!(p.settings, defaults.values);
    assert!(p.pending_settings.iter().all(|x| x.state == 0));
    assert_eq!(env.setting(K::PauseGapSecs), PAUSE_GAP_SECS);
}

#[test]
fn a_setting_changes_only_after_propose_cosign_and_the_timelock() {
    let mut env = Env::new();
    let (admin, co) = (env.admin.insecure_clone(), env.co_signer.insecure_clone());
    let key = K::PauseGapSecs as u8;
    let value = PAUSE_GAP_SECS * 2;

    // Nothing to approve or execute yet.
    let ix = env.approve_setting_ix(&co.pubkey(), key, value);
    assert_err(env.send(&[ix], &[&co]), PoolError::NoPendingSetting);
    let ix = env.execute_setting_ix(key);
    assert_err(env.send(&[ix], &[&admin]), PoolError::NoPendingSetting);

    // Only the admin proposes.
    let other = env.funded(SOL);
    let ix = env.ix(
        safu_pool::accounts::AdminOnly { admin: other.pubkey(), pool: env.pool() },
        safu_pool::instruction::ProposeSetting { key, value },
    );
    assert_err(env.send(&[ix], &[&other]), PoolError::NotAdmin);
    let ix = env.propose_ix(key, value);
    env.ok(&[ix], &[&admin]);
    assert_eq!(env.pool_state().pending_settings[key as usize].state, PENDING_PROPOSED);

    // Proposed is not enough.
    let ix = env.execute_setting_ix(key);
    assert_err(env.send(&[ix], &[&admin]), PoolError::SettingNotReady);

    // Only the co-signer approves, and only the same value.
    let ix = env.approve_setting_ix(&admin.pubkey(), key, value);
    assert_err(env.send(&[ix], &[&admin]), PoolError::NotAdminOrCoSigner);
    let ix = env.approve_setting_ix(&co.pubkey(), key, value + 1);
    assert_err(env.send(&[ix], &[&co]), PoolError::SettingValueMismatch);
    let ix = env.approve_setting_ix(&co.pubkey(), key, value);
    env.ok(&[ix], &[&co]);
    let ix = env.approve_setting_ix(&co.pubkey(), key, value);
    assert_err(env.send(&[ix], &[&co]), PoolError::SettingAlreadyApproved);
    let pending = env.pool_state().pending_settings[key as usize];
    assert_eq!((pending.state, pending.eta), (PENDING_APPROVED, env.now + SETTINGS_TIMELOCK_SECS));

    // Not before the timelock; the live value is untouched meanwhile.
    env.warp(SETTINGS_TIMELOCK_SECS - 1);
    let ix = env.execute_setting_ix(key);
    assert_err(env.send(&[ix], &[&admin]), PoolError::SettingNotReady);
    assert_eq!(env.setting(K::PauseGapSecs), PAUSE_GAP_SECS);

    // Then anyone executes.
    env.warp(1);
    let anyone = env.funded(SOL);
    let ix = env.execute_setting_ix(key);
    env.ok(&[ix], &[&anyone]);
    assert_eq!(env.setting(K::PauseGapSecs), value);
    assert_eq!(env.pool_state().pending_settings[key as usize].state, 0);
}

#[test]
fn either_signer_can_cancel_a_pending_change_nobody_else() {
    let mut env = Env::new();
    let (admin, co) = (env.admin.insecure_clone(), env.co_signer.insecure_clone());
    let key = K::PauseGapSecs as u8;
    for canceller in [&admin, &co] {
        let ix = env.propose_ix(key, PAUSE_GAP_SECS * 2);
        env.ok(&[ix], &[&admin]);
        let other = env.funded(SOL);
        let ix = env.cancel_setting_ix(&other.pubkey(), key);
        assert_err(env.send(&[ix], &[&other]), PoolError::NotAdminOrCoSigner);
        let ix = env.cancel_setting_ix(&canceller.pubkey(), key);
        env.ok(&[ix], &[canceller]);
        let ix = env.cancel_setting_ix(&canceller.pubkey(), key);
        assert_err(env.send(&[ix], &[canceller]), PoolError::NoPendingSetting);
    }
}

#[test]
fn a_new_proposal_drops_the_earlier_approval_and_clock() {
    let mut env = Env::new();
    let (admin, co) = (env.admin.insecure_clone(), env.co_signer.insecure_clone());
    let key = K::PauseGapSecs as u8;
    let ix = env.propose_ix(key, PAUSE_GAP_SECS * 2);
    env.ok(&[ix], &[&admin]);
    let ix = env.approve_setting_ix(&co.pubkey(), key, PAUSE_GAP_SECS * 2);
    env.ok(&[ix], &[&co]);
    env.warp(SETTINGS_TIMELOCK_SECS);
    let ix = env.propose_ix(key, PAUSE_GAP_SECS * 3);
    env.ok(&[ix], &[&admin]);
    let ix = env.execute_setting_ix(key);
    assert_err(env.send(&[ix], &[&admin]), PoolError::SettingNotReady);
}

#[test]
fn values_outside_the_range_and_unused_slots_are_refused() {
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    for key in K::ALL {
        let (min, _) = key.bounds();
        let ix = env.propose_ix(key as u8, min - 1);
        assert_err(env.send(&[ix], &[&admin]), PoolError::SettingOutOfBounds);
    }
    for slot in [SETTING_COUNT as u8, SETTING_SLOTS - 1, 255] {
        let ix = env.propose_ix(slot, 1);
        assert_err(env.send(&[ix], &[&admin]), PoolError::UnknownSetting);
    }
    // Order is checked at proposal too.
    let ix = env.propose_ix(K::MinStakeBps as u8, MAX_STAKE_BPS as i64 + 1);
    assert_err(env.send(&[ix], &[&admin]), PoolError::SettingOrderInvalid);
}

#[test]
fn order_between_settings_is_checked_again_at_execution() {
    let mut env = Env::new();
    let (admin, co) = (env.admin.insecure_clone(), env.co_signer.insecure_clone());
    // Min 50 fits the live max (100); max 40 fits the live min (10). Together they cross.
    for (key, value) in [(K::MinStakeBps, 50), (K::MaxStakeBps, 40)] {
        let ix = env.propose_ix(key as u8, value);
        env.ok(&[ix], &[&admin]);
        let ix = env.approve_setting_ix(&co.pubkey(), key as u8, value);
        env.ok(&[ix], &[&co]);
    }
    env.warp(SETTINGS_TIMELOCK_SECS);
    let ix = env.execute_setting_ix(K::MaxStakeBps as u8);
    env.ok(&[ix], &[&admin]);
    let ix = env.execute_setting_ix(K::MinStakeBps as u8);
    assert_err(env.send(&[ix], &[&admin]), PoolError::SettingOrderInvalid);
    assert!(env.setting(K::MinStakeBps) <= env.setting(K::MaxStakeBps));
}

#[test]
fn stake_bounds_and_pool_cap_follow_the_live_settings() {
    let mut env = Env::new();
    let old_min = min_stake();
    env.set_setting(K::MinStakeBps, (MIN_STAKE_BPS * 2) as i64);
    let k = env.funded(SOL);
    let ix = env.stake_ix(&k.pubkey(), old_min, k.pubkey());
    assert_err(env.send(&[ix], &[&k]), PoolError::StakeOutOfRange);
    let ix = env.stake_ix(&k.pubkey(), old_min * 2, k.pubkey());
    env.ok(&[ix], &[&k]);

    // A cap lowered to what is staked blocks new stakes; existing ones stay. (Stake bounds are bps
    // of the cap, so they shrink with it: use one that fits the new bounds.)
    let staked = env.pool_state().total_staked;
    env.set_setting(K::PoolCap, staked as i64);
    let (_, new_max) = pool_core::stake::bounds(&env.pool_state().settings()).unwrap();
    let k2 = env.funded(SOL);
    let ix = env.stake_ix(&k2.pubkey(), new_max, k2.pubkey());
    assert_err(env.send(&[ix], &[&k2]), PoolError::PoolCapExceeded);
    assert_eq!(env.stake_state(&k.pubkey()).amount, old_min * 2);
}

#[test]
fn backer_waits_follow_the_live_settings() {
    let mut env = Env::new();
    env.set_setting(K::BackerNoticeSecs, 0);
    let (maturity_min, _) = K::BackerMaturitySecs.bounds();
    env.set_setting(K::BackerMaturitySecs, maturity_min);
    let b = env.funded(2 * SOL);
    let ix = env.back_ix(&b.pubkey(), SOL);
    env.ok(&[ix], &[&b]);
    assert_eq!(env.backer_state(&b.pubkey()).pending_matures_at, env.now + maturity_min);
    env.warp(maturity_min);
    let ix = env.mature_ix(&b.pubkey());
    env.ok(&[ix], &[&b]);
    // No notice: request and complete in one go.
    let ix = env.request_backer_ix(&b.pubkey(), SOL);
    env.ok(&[ix], &[&b]);
    let ix = env.complete_backer_ix(&b.pubkey());
    env.ok(&[ix], &[&b]);
    assert_eq!(env.backer_state(&b.pubkey()).amount, 0);
}

#[test]
fn daily_payout_rate_follows_the_live_settings() {
    let mut env = Env::new();
    // Lowest rate in every band (high first, so low >= mid >= high holds at each step).
    for key in [K::PayoutHighBps, K::PayoutMidBps, K::PayoutLowBps] {
        env.set_setting(key, 1);
    }
    env.matured_backer(40 * SOL);
    let s = env.staker(max_stake());
    env.warp(TIME_GATE_SECS);
    let a = env.approval(&s.pubkey(), TX, max_stake(), TIER_A);
    env.submit(&a).unwrap();
    let ix = env.approve_claim_ix(&s.pubkey(), &TX);
    env.ok(&[ix], &[&s]);
    let c = env.claim_state(&s.pubkey(), &TX);
    env.warp(COOLDOWN_SECS + VESTING_SECS);
    let ix = env.stream_ix(&s.pubkey(), &TX, &s.pubkey());
    env.ok(&[ix], &[&s]);
    let p = env.pool_state();
    let base = (p.total_staked + p.total_backed).max(c.capacity_snapshot);
    assert_eq!(env.claim_state(&s.pubkey(), &TX).streamed, pool_core::apply_bps(base, 1).unwrap());
}

#[test]
fn yield_split_follows_the_live_settings() {
    let mut env = Env::new();
    env.marinade_liquidity_at_target();
    env.set_setting(K::StakerYieldBps, 0);
    env.matured_backer(40 * SOL);
    env.staker(max_stake());
    env.warp(SECONDS_PER_DAY);
    env.marinade_rewards(50);
    let before = env.pool_state();
    let ix = env.upkeep_ix(safu_pool::instruction::Harvest {});
    let anyone = env.funded(SOL);
    env.ok(&[ix], &[&anyone]);
    let after = env.pool_state();
    let amount = after.total_extracted_yield - before.total_extracted_yield;
    assert!(amount > 0);
    // Stakers get nothing at 0 bps; backers still get their full share.
    assert_eq!(after.staker_yield_reserved, before.staker_yield_reserved);
    let c = pool_core::yields::credit(amount, before.total_staked, before.total_backed, 0, BACKER_YIELD_BPS).unwrap();
    assert_eq!(after.backer_yield_reserved - before.backer_yield_reserved, c.backer_share);
    assert_eq!(after.protocol_yield_balance - before.protocol_yield_balance, c.protocol_share);
}

#[test]
fn running_claims_keep_the_clocks_they_started_with() {
    let (mut env, s) = active_claim();
    let started = env.claim_state(&s.pubkey(), &TX);
    let (_, cooldown_max) = K::CooldownSecs.bounds();
    let (_, approve_max) = K::ApproveWindowSecs.bounds();
    let (_, inactivity_max) = K::InactivitySecs.bounds();
    env.set_setting(K::CooldownSecs, cooldown_max);
    env.set_setting(K::ApproveWindowSecs, approve_max);
    env.set_setting(K::InactivitySecs, inactivity_max);
    let kept = env.claim_state(&s.pubkey(), &TX);
    assert_eq!(
        (kept.cooldown_ends, kept.vesting_ends, kept.approve_window, kept.inactivity_window),
        (started.cooldown_ends, started.vesting_ends, started.approve_window, started.inactivity_window)
    );
    assert_eq!(started.inactivity_window, COLLECTION_INACTIVITY_SECS);

    // A claim admitted and approved after the change uses the new values.
    let s2 = env.staker(max_stake());
    // Past the time gate and into a new day (the oracle's one claim a day at this pool size).
    env.warp(TIME_GATE_SECS.max(SECONDS_PER_DAY));
    let a = env.approval(&s2.pubkey(), TX2, max_stake(), TIER_A);
    env.submit(&a).unwrap();
    assert_eq!(env.claim_state(&s2.pubkey(), &TX2).approve_window, approve_max);
    let ix = env.approve_claim_ix(&s2.pubkey(), &TX2);
    env.ok(&[ix], &[&s2]);
    let c2 = env.claim_state(&s2.pubkey(), &TX2);
    assert_eq!((c2.cooldown_ends, c2.inactivity_window), (env.now + cooldown_max, inactivity_max));
}

// ================================================================== X2: pause gap

#[test]
fn no_pause_on_top_of_a_pause_and_a_gap_before_the_next() {
    let mut env = Env::new();
    env.pause();
    assert_err(try_pause(&mut env), PoolError::Paused);
    env.warp(60);
    env.unpause();
    let ended = env.now;
    assert_eq!(env.pool_state().paused_until, ended);
    assert_err(try_pause(&mut env), PoolError::PauseGapNotPassed);
    env.warp(PAUSE_GAP_SECS - 1);
    assert_err(try_pause(&mut env), PoolError::PauseGapNotPassed);
    env.warp(1);
    try_pause(&mut env).unwrap();
}

#[test]
fn a_pause_that_runs_out_also_needs_the_gap() {
    let mut env = Env::new();
    env.pause();
    env.warp(PAUSE_MAX_SECS);
    assert!(!env.pool_state().is_paused(env.now));
    assert_err(try_pause(&mut env), PoolError::PauseGapNotPassed);
    env.warp(PAUSE_GAP_SECS);
    try_pause(&mut env).unwrap();
}

#[test]
fn pause_length_and_gap_follow_the_live_settings() {
    let mut env = Env::new();
    let (pause_min, _) = K::PauseMaxSecs.bounds();
    let (gap_min, _) = K::PauseGapSecs.bounds();
    env.set_setting(K::PauseMaxSecs, pause_min);
    env.set_setting(K::PauseGapSecs, gap_min);
    env.pause();
    assert_eq!(env.pool_state().paused_until, env.now + pause_min);
    env.warp(pause_min + gap_min);
    try_pause(&mut env).unwrap();
}

#[test]
fn the_pause_clock_adds_up_every_pause() {
    let mut env = Env::new();
    env.pause();
    env.warp(100);
    env.unpause();
    env.warp(PAUSE_GAP_SECS);
    env.pause();
    env.warp(50);
    let p = env.pool_state();
    assert_eq!(p.paused_before, 100);
    assert_eq!(p.paused_secs_at(env.now), 150);
}

// ================================================================== X1: a pause never uses up a claim window

#[test]
fn a_pause_does_not_use_up_the_approve_window() {
    let (mut env, s) = awaiting_claim();
    let w = env.claim_state(&s.pubkey(), &TX).approve_window;
    env.warp(w - w / 10);
    env.pause();
    // Nobody can expire it while paused.
    let pause_len = w / 5;
    env.warp(pause_len);
    assert_err(expire_approval(&mut env, &s), PoolError::Paused);
    env.unpause();
    // Real time is past the deadline; the claim clock is not (the pause did not count).
    assert!(env.now > env.claim_state(&s.pubkey(), &TX).approve_deadline);
    assert_err(expire_approval(&mut env, &s), PoolError::ApprovalWindowNotExpired);
    // The staker can still approve.
    let ix = env.approve_claim_ix(&s.pubkey(), &TX);
    env.ok(&[ix], &[&s]);
    assert_eq!(env.claim_state(&s.pubkey(), &TX).status, ClaimStatus::Active);
}

#[test]
fn the_approve_window_still_ends_after_the_paused_time_is_added_back() {
    let (mut env, s) = awaiting_claim();
    let w = env.claim_state(&s.pubkey(), &TX).approve_window;
    env.warp(w / 2);
    env.pause();
    env.warp(w / 5);
    env.unpause();
    env.warp(w / 2 + 1);
    let ix = env.approve_claim_ix(&s.pubkey(), &TX);
    assert_err(env.send(&[ix], &[&s]), PoolError::ApprovalWindowExpired);
    expire_approval(&mut env, &s).unwrap();
    assert_eq!(env.pool_state().total_allocated, 0);
}

#[test]
fn a_pause_does_not_make_an_active_claim_stale() {
    let (mut env, s) = active_claim();
    let c = env.claim_state(&s.pubkey(), &TX);
    let w = c.inactivity_window;
    env.warp(c.last_collected - env.now + w - w / 10);
    env.pause();
    env.warp(w / 5);
    assert_err(expire_stale(&mut env, &s), PoolError::Paused);
    env.unpause();
    assert_err(expire_stale(&mut env, &s), PoolError::ClaimNotStale);
    env.warp(w / 10 + 1);
    expire_stale(&mut env, &s).unwrap();
}

#[test]
fn a_pause_does_not_use_up_a_queued_claims_window() {
    // Insolvent claim (no backing): queued, nothing allocated.
    let mut env = Env::new();
    let s = env.staker(max_stake());
    let a = env.approval(&s.pubkey(), TX, max_stake() * 2, TIER_A);
    env.submit(&a).unwrap();
    assert_eq!(env.claim_state(&s.pubkey(), &TX).status, ClaimStatus::Reserved);
    env.warp(CLAIM_WINDOW_SECS - SECONDS_PER_DAY);
    env.pause();
    env.warp(10 * SECONDS_PER_DAY);
    assert_err(expire_queued(&mut env, &s), PoolError::Paused);
    env.unpause();
    assert_err(expire_queued(&mut env, &s), PoolError::QueueNotYetExpired);
    env.warp(SECONDS_PER_DAY + 1);
    expire_queued(&mut env, &s).unwrap();
}

#[test]
fn a_hack_during_a_long_pause_can_still_be_claimed_after_it() {
    let mut env = Env::new();
    env.matured_backer(40 * SOL);
    let s = env.staker(max_stake());
    env.warp(10);
    let hack = env.now;
    env.warp(5 * SECONDS_PER_DAY);
    env.pause();
    env.warp(PAUSE_MAX_SECS - 1);
    env.unpause();
    // 35 days after the hack in real time; 5 days on the claim clock.
    assert!(env.now > hack + CLAIM_WINDOW_SECS);
    let mut a = env.approval(&s.pubkey(), TX, max_stake(), TIER_A);
    a.hack_timestamp = hack;
    env.submit(&a).unwrap();
}

#[test]
fn a_window_already_closed_before_a_pause_stays_closed() {
    let mut env = Env::new();
    env.matured_backer(40 * SOL);
    let s = env.staker(max_stake());
    env.warp(10);
    let hack = env.now;
    env.warp(CLAIM_WINDOW_SECS + SECONDS_PER_DAY);
    env.pause();
    env.warp(SECONDS_PER_DAY);
    env.unpause();
    let mut a = env.approval(&s.pubkey(), TX, max_stake(), TIER_A);
    a.hack_timestamp = hack;
    assert_err(env.submit(&a), PoolError::ClaimWindowExpired);
}

// ================================================================== X3: one rule for every exit, partial withdrawal

#[test]
fn a_part_of_a_stake_can_be_withdrawn_and_coverage_follows_it() {
    let mut env = Env::new();
    env.marinade_liquidity_at_target();
    env.matured_backer(40 * SOL);
    let m = max_stake();
    let s = env.staker(m);
    let before = env.lamports(&s.pubkey());
    withdraw(&mut env, &s, m / 2).unwrap();
    let r = env.stake_state(&s.pubkey());
    assert_eq!(r.amount, m / 2);
    assert_eq!(r.yield_index_at, env.pool_state().staker_yield_index);
    let p = env.pool_state();
    assert_eq!((p.total_staked, p.total_stakers), (m / 2, 1));
    assert!(env.lamports(&s.pubkey()) >= before + m / 2 - Env::max_unstake_fee(m / 2));

    // The ceiling is now on half the stake.
    env.warp(TIME_GATE_SECS);
    let too_much = env.approval(&s.pubkey(), TX, m * 15, TIER_A);
    assert_err(env.submit(&too_much), PoolError::EntitlementExceedsTierCap);
    let fits = env.approval(&s.pubkey(), TX2, m / 2 * 15, TIER_A);
    env.submit(&fits).unwrap();
}

#[test]
fn what_stays_is_zero_or_at_least_the_min_stake() {
    let mut env = Env::new();
    let m = max_stake();
    let s = env.staker(m);
    assert_err(withdraw(&mut env, &s, m - min_stake() + 1), PoolError::StakeBelowMinimum);
    assert_err(withdraw(&mut env, &s, m + 1), PoolError::AmountExceedsStake);
    assert_err(withdraw(&mut env, &s, 0), PoolError::AmountNotPositive);
    withdraw(&mut env, &s, m - min_stake()).unwrap();
    assert_eq!(env.stake_state(&s.pubkey()).amount, min_stake());
    // The rest in full closes the record; the address can stake again.
    withdraw(&mut env, &s, min_stake()).unwrap();
    assert!(!env.exists(&env.stake_record(&s.pubkey())));
    assert_eq!(env.pool_state().total_stakers, 0);
    let ix = env.stake_ix(&s.pubkey(), m, s.pubkey());
    env.ok(&[ix], &[&s]);
}

#[test]
fn a_partial_withdrawal_follows_the_same_claim_and_lock_rules() {
    let (mut env, s) = awaiting_claim();
    assert_err(withdraw(&mut env, &s, min_stake()), PoolError::ClaimActive);
    let mut env = Env::new();
    let s = env.staker(max_stake());
    let now = env.now;
    env.edit::<StakeRecord>(&env.stake_record(&s.pubkey()), |r| r.penalty_locked_until = now + 100);
    assert_err(withdraw(&mut env, &s, min_stake()), PoolError::PenaltyLockActive);
    env.pause();
    assert_err(withdraw(&mut env, &s, min_stake()), PoolError::Paused);
}

#[test]
fn stakers_take_only_free_capital_and_wait_for_the_rest() {
    let (mut env, claimant, others) = tight_pool();
    let m = max_stake();
    // 7 full exits fit (capacity 10M -> 3M, claims need 2.4M).
    for s in &others[..7] {
        withdraw(&mut env, s, m).unwrap();
    }
    // The 8th cannot take a whole stake: only 0.6M is free.
    let bob = &others[7];
    assert_err(withdraw(&mut env, bob, m), PoolError::CapitalNotFree);
    // It can take the free part now.
    withdraw(&mut env, bob, m * 6 / 10).unwrap();
    assert_err(withdraw(&mut env, bob, min_stake()), PoolError::CapitalNotFree);
    let p = env.pool_state();
    assert_eq!(p.total_allocated, p.total_staked + p.total_backed);

    // Yield is not capital: it still comes out.
    let ix = env.claim_yield_ix(&bob.pubkey(), &bob.pubkey());
    let r = env.send(&[ix], &[bob]);
    assert!(r.is_ok() || r.as_ref().unwrap_err().contains(&format!("Custom({})", anchor_lang::error::ERROR_CODE_OFFSET + PoolError::NothingToClaim as u32)));

    // Once the claim is resolved the rest leaves in full: waiting, never lost.
    let admin = env.admin.insecure_clone();
    let ix = env.cancel_claim_ix(&admin.pubkey(), &claimant.pubkey(), &TX);
    env.ok(&[ix], &[&admin]);
    withdraw(&mut env, bob, m * 4 / 10).unwrap();
    withdraw(&mut env, &others[8], m).unwrap();
    withdraw(&mut env, &claimant, m).unwrap();
    let p = env.pool_state();
    assert_eq!((p.total_staked, p.total_allocated, p.total_stakers), (0, 0, 0));
}

#[test]
fn a_pause_cannot_be_used_to_take_capital_claims_need() {
    let (mut env, _, others) = tight_pool();
    let m = max_stake();
    for s in &others[..7] {
        withdraw(&mut env, s, m).unwrap();
    }
    env.pause();
    let bob = &others[7];
    assert_err(emergency(&mut env, bob, m), PoolError::CapitalNotFree);
    // A staker whose capital is free is never locked in by a pause.
    emergency(&mut env, bob, m * 6 / 10).unwrap();
    assert_eq!(env.stake_state(&bob.pubkey()).amount, m * 4 / 10);
    assert_err(emergency(&mut env, &others[8], m), PoolError::CapitalNotFree);
    // Emergency exit is only for a pause.
    env.unpause();
    assert_err(emergency(&mut env, &others[8], min_stake()), PoolError::NotPaused);
}

#[test]
fn a_full_emergency_exit_closes_the_record() {
    let mut env = Env::new();
    env.marinade_liquidity_at_target();
    let s = env.staker(max_stake());
    env.pause();
    let before = env.lamports(&s.pubkey());
    emergency(&mut env, &s, max_stake()).unwrap();
    assert!(!env.exists(&env.stake_record(&s.pubkey())));
    // The payee pays Marinade's unstake fee (up to its maximum) and gets the record's rent back.
    assert!(env.lamports(&s.pubkey()) + Env::max_unstake_fee(max_stake()) > before + max_stake());
}

#[test]
fn stakers_and_backers_follow_the_same_free_capital_line() {
    // Claimant M, 8 stakers M, one backer M: capacity 10M, claim 2.4M.
    let mut env = Env::new();
    let m = max_stake();
    let b = env.matured_backer(m);
    let claimant = env.staker(m);
    let others: Vec<Keypair> = (0..8).map(|_| env.staker(m)).collect();
    let a = env.approval(&claimant.pubkey(), TX, m * 24 / 10, TIER_A);
    env.submit(&a).unwrap();
    for s in &others[..7] {
        withdraw(&mut env, s, m).unwrap();
    }
    // 0.6M free. Neither side can take a whole M ...
    assert_err(withdraw(&mut env, &others[7], m), PoolError::CapitalNotFree);
    let ix = env.request_backer_ix(&b.pubkey(), m);
    env.ok(&[ix], &[&b]);
    env.warp(BACKER_NOTICE_SECS);
    let ix = env.complete_backer_ix(&b.pubkey());
    assert_err(env.send(&[ix], &[&b]), PoolError::CapitalNotFree);
    // ... and whoever takes the free part first leaves the other waiting.
    let ix = env.backer_only_ix(&b.pubkey(), safu_pool::instruction::CancelBackerWithdrawal {});
    env.ok(&[ix], &[&b]);
    let ix = env.request_backer_ix(&b.pubkey(), m * 6 / 10);
    env.ok(&[ix], &[&b]);
    env.warp(BACKER_NOTICE_SECS);
    let ix = env.complete_backer_ix(&b.pubkey());
    env.ok(&[ix], &[&b]);
    assert_err(withdraw(&mut env, &others[7], min_stake()), PoolError::CapitalNotFree);
}

// ================================================================== X4, beneficiary, upgrade room

#[test]
fn protocol_yield_cannot_move_while_paused() {
    let mut env = Env::new();
    env.staker(max_stake());
    env.pause();
    let (admin, treasury) = (env.admin.insecure_clone(), env.treasury.pubkey());
    let ix = env.withdraw_yield_ix(&admin.pubkey(), &treasury, 1);
    assert_err(env.send(&[ix], &[&admin]), PoolError::Paused);
}

#[test]
fn beneficiary_can_never_be_the_pool_or_its_vault() {
    let mut env = Env::new();
    let k = env.funded(SOL);
    for bad in [env.pool(), env.vault()] {
        let ix = env.stake_ix(&k.pubkey(), max_stake(), bad);
        assert_err(env.send(&[ix], &[&k]), PoolError::BeneficiaryIsPoolAccount);
    }
    let s = env.staker(max_stake());
    for bad in [env.pool(), env.vault()] {
        let ix = env.set_beneficiary_ix(&s.pubkey(), bad);
        assert_err(env.send(&[ix], &[&s]), PoolError::BeneficiaryIsPoolAccount);
    }
}

#[test]
fn every_record_carries_a_version_and_the_pool_its_asset() {
    let (mut env, s) = awaiting_claim();
    let p: Pool = env.pool_state();
    assert_eq!((p.version, p.asset_mint), (ACCOUNT_VERSION, NATIVE_SOL_MINT));
    assert!(p.reserved.iter().all(|b| *b == 0));
    assert_eq!(env.stake_state(&s.pubkey()).version, ACCOUNT_VERSION);
    assert_eq!(env.claim_state(&s.pubkey(), &TX).version, ACCOUNT_VERSION);
    let b = env.matured_backer(SOL);
    assert_eq!(env.backer_state(&b.pubkey()).version, ACCOUNT_VERSION);
    let writer = env.writer.insecure_clone();
    let ix = env.register_ix(&writer.pubkey(), s.pubkey(), [9; 32]);
    env.ok(&[ix], &[&writer]);
    let w: StakerWallets = env.read(&env.staker_wallets(&s.pubkey()));
    assert_eq!(w.version, ACCOUNT_VERSION);
}
