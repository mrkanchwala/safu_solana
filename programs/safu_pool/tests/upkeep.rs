//! The upkeep every user transaction does (multichain `with_upkeep`): the head of the claim line
//! and two watch-list places, passed as remaining accounts. Every clock is read from the live
//! settings, so each test holds in both the normal and the demo build.

mod common;

use common::*;
use pool_core::params::SECONDS_PER_DAY;
use pool_core::settings::SettingKey as K;
use safu_pool::constants::*;
use safu_pool::errors::PoolError;
use safu_pool::state::{ClaimStatus, StakeRecord};
use solana_keypair::Keypair;
use solana_signer::Signer;

const TX: [u8; 32] = [7; 32];
const TX2: [u8; 32] = [8; 32];
const TX3: [u8; 32] = [9; 32];

fn backing() -> u64 {
    40 * unit()
}

/// One max staker per tx, each claim admitted (a day apart, so the oracle's daily limit stays out;
/// the first at once, before its time gate in either build).
fn claimants(env: &mut Env, txs: &[[u8; 32]]) -> Vec<Keypair> {
    let (_, max) = bounds();
    let stakers: Vec<Keypair> = txs.iter().map(|_| env.staker(max)).collect();
    for (i, (s, tx)) in stakers.iter().zip(txs).enumerate() {
        if i > 0 {
            env.warp(SECONDS_PER_DAY);
        }
        let a = env.approval(&s.pubkey(), *tx, max, TIER_A);
        env.submit(&a).unwrap();
    }
    stakers
}

/// One claim in line: a tier-A claim larger than a pool with no backing can hold.
fn queued(env: &mut Env) -> Keypair {
    let (_, max) = bounds();
    let s = env.staker(max);
    let a = env.approval(&s.pubkey(), TX, env.tier_cap(max, TIER_A), TIER_A);
    env.submit(&a).unwrap();
    assert_eq!(
        env.claim_state(&s.pubkey(), &TX).status,
        ClaimStatus::Reserved
    );
    s
}

/// Any user transaction (a small backing), which carries the upkeep.
fn any_tx(env: &mut Env) {
    let b = env.funded(unit());
    let ix = env.back_ix(&b.pubkey(), unit());
    env.ok(&[ix], &[&b]);
}

#[test]
fn a_user_transaction_without_the_upkeep_accounts_is_refused() {
    let mut env = Env::new();
    queued(&mut env);
    let b = env.funded(unit());
    let ix = env.back_ix(&b.pubkey(), unit());
    assert_err(
        env.send_bare(std::slice::from_ref(&ix), &[&b]).map(|_| ()),
        PoolError::UpkeepAccountsMissing,
    );
    // A wrong account in a place is refused the same way.
    let mut metas = env.upkeep_metas();
    metas.swap(1, 2);
    let mut wrong = ix.clone();
    wrong.accounts.extend(metas);
    assert_err(
        env.send_bare(&[wrong], &[&b]).map(|_| ()),
        PoolError::UpkeepAccountsMissing,
    );
    // Admin instructions carry none.
    env.pause();
    env.unpause();
}

#[test]
fn nothing_is_needed_or_done_while_paused() {
    let mut env = Env::new();
    let s = queued(&mut env);
    env.pause();
    assert!(env.upkeep_metas().is_empty());
    let ix = env.claim_yield_ix(&s.pubkey(), &s.pubkey());
    // Nothing to claim, but the refusal is the instruction's own, not the upkeep's.
    assert_err(
        env.send_bare(&[ix], &[&s]).map(|_| ()),
        PoolError::NothingToClaim,
    );
    assert_eq!(
        env.claim_state(&s.pubkey(), &TX).status,
        ClaimStatus::Reserved
    );
}

#[test]
fn a_claim_past_the_gate_is_approved_in_one_click_with_its_window_from_the_gate() {
    let mut env = Env::new();
    env.matured_backer(backing());
    let s = claimants(&mut env, &[TX]).remove(0);
    assert_eq!(
        env.claim_state(&s.pubkey(), &TX).status,
        ClaimStatus::PendingTime
    );
    let r = env.stake_state(&s.pubkey());
    let gate_at = r.staked_at + r.time_gate_secs;
    let window = env.setting(K::ApproveWindowSecs);
    // Past the gate and past the window counted from it: too late, with no call in between.
    env.warp(gate_at + window + 1 - env.now);
    let ix = env.approve_claim_ix(&s.pubkey(), &TX);
    assert_err(
        env.send(std::slice::from_ref(&ix), &[&s]),
        PoolError::ApprovalWindowExpired,
    );
    // Same claim, one second earlier: approved directly from pending.
    let mut env = Env::new();
    env.matured_backer(backing());
    let s = claimants(&mut env, &[TX]).remove(0);
    env.warp(gate_at + window - env.now);
    let ix = env.approve_claim_ix(&s.pubkey(), &TX);
    env.ok(&[ix], &[&s]);
    assert_eq!(
        env.claim_state(&s.pubkey(), &TX).status,
        ClaimStatus::Active
    );
}

#[test]
fn an_unapproved_claim_expires_on_its_own_and_its_place_closes() {
    let mut env = Env::new();
    env.matured_backer(backing());
    let s = claimants(&mut env, &[TX]).remove(0);
    let entitlement = env.claim_state(&s.pubkey(), &TX).entitlement;
    assert_eq!(env.pool_state().total_allocated, entitlement);
    assert_eq!(env.watch_state(0).claim, env.claim_addr(&s.pubkey(), &TX));
    let r = env.stake_state(&s.pubkey());
    let window = env.setting(K::ApproveWindowSecs);
    env.warp(r.staked_at + r.time_gate_secs + window + 1 - env.now);
    let oracle_before = env.lamports(&env.oracle.pubkey());
    any_tx(&mut env);
    assert_eq!(
        env.claim_state(&s.pubkey(), &TX).status,
        ClaimStatus::Expired
    );
    let p = env.pool_state();
    assert_eq!((p.total_allocated, p.watch_head, p.watch_tail), (0, 1, 1));
    assert!(!env.exists(&env.watch_entry(0)));
    assert!(env.lamports(&env.oracle.pubkey()) > oracle_before);
    assert_eq!(env.stake_state(&s.pubkey()).active_claim, None);
}

#[test]
fn an_uncollected_claim_goes_stale_on_its_own() {
    let mut env = Env::new();
    env.matured_backer(backing());
    let s = claimants(&mut env, &[TX]).remove(0);
    let r = env.stake_state(&s.pubkey());
    env.warp(r.staked_at + r.time_gate_secs - env.now);
    let ix = env.approve_claim_ix(&s.pubkey(), &TX);
    env.ok(&[ix], &[&s]);
    let c = env.claim_state(&s.pubkey(), &TX);
    env.warp(c.last_collected + c.inactivity_window - env.now);
    any_tx(&mut env);
    assert_eq!(
        env.claim_state(&s.pubkey(), &TX).status,
        ClaimStatus::Active
    );
    env.warp(1);
    any_tx(&mut env);
    assert_eq!(
        env.claim_state(&s.pubkey(), &TX).status,
        ClaimStatus::Expired
    );
    assert_eq!(env.pool_state().total_allocated, 0);
}

#[test]
fn the_watch_list_is_walked_two_places_a_transaction_and_shrinks_from_the_head() {
    let mut env = Env::new();
    // Windows long enough that nothing here expires on time, in either build.
    env.edit::<safu_pool::state::Pool>(&env.pool(), |p| {
        for k in [K::ApproveWindowSecs, K::InactivitySecs] {
            p.settings[k.index()] = k.bounds().1;
        }
    });
    env.matured_backer(4 * backing());
    let s = claimants(&mut env, &[TX, TX2, TX3]);
    // The middle one finishes first (cancelled): its place is marked done, not closed.
    let admin = env.admin.insecure_clone();
    let ix = env.cancel_claim_ix(&admin.pubkey(), &s[1].pubkey(), &TX2);
    env.ok(&[ix], &[&admin]);
    let p = env.pool_state();
    assert_eq!((p.watch_head, p.watch_tail), (0, 3));
    let places = pool_core::upkeep::watch_places(p.watch_head, p.watch_tail, p.watch_cursor);
    any_tx(&mut env);
    let after = env.pool_state();
    assert_eq!(
        after.watch_cursor,
        places.iter().flatten().last().unwrap() + 1
    );
    // The first two still wait at most for their gates; walk until the middle place is checked.
    // Two places a transaction over three: the middle one is checked within two more.
    let mut walks = 0;
    while !env.watch_state(1).done {
        assert!(walks < 2, "the done place was never marked");
        any_tx(&mut env);
        walks += 1;
    }
    assert_eq!(env.pool_state().watch_head, 0);
    // Cancel the head: the next pass closes it, and the done place behind it.
    let ix = env.cancel_claim_ix(&admin.pubkey(), &s[0].pubkey(), &TX);
    env.ok(&[ix], &[&admin]);
    for _ in 0..4 {
        any_tx(&mut env);
    }
    let p = env.pool_state();
    assert_eq!((p.watch_head, p.watch_tail), (2, 3));
    assert!(!env.exists(&env.watch_entry(0)) && !env.exists(&env.watch_entry(1)));
}

#[test]
fn a_place_whose_accounts_the_transaction_holds_waits_for_the_next_one() {
    let mut env = Env::new();
    let s = queued(&mut env);
    let rec = env.stake_record(&s.pubkey());
    env.edit::<StakeRecord>(&rec, |r| hold(r, true));
    env.matured_backer(backing());
    env.edit::<StakeRecord>(&rec, |r| hold(r, false));
    // The claimant's own yield claim holds its stake record: the head is left alone.
    env.edit::<safu_pool::state::Pool>(&env.pool(), |p| p.staker_yield_reserved = 1);
    env.edit::<StakeRecord>(&rec, |r| r.yield_owed = 1);
    let ix = env.claim_yield_ix(&s.pubkey(), &s.pubkey());
    env.ok(&[ix], &[&s]);
    assert_eq!(env.pool_state().queue_head, 0);
    assert_eq!(
        env.claim_state(&s.pubkey(), &TX).status,
        ClaimStatus::Reserved
    );
    any_tx(&mut env);
    assert_ne!(
        env.claim_state(&s.pubkey(), &TX).status,
        ClaimStatus::Reserved
    );
}

#[test]
fn the_largest_transaction_fits_one_packet() {
    use solana_message::Message;
    let mut env = Env::new();
    env.matured_backer(4 * backing());
    claimants(&mut env, &[TX, TX2]);
    queued(&mut env);
    // Line and both watch places in use: a claim submission carries the most accounts.
    assert_eq!(
        env.upkeep_metas().len(),
        3 * safu_pool::instructions::upkeep::PLACE_ACCOUNTS
    );
    let (_, max) = bounds();
    let s = env.staker(max);
    let a = env.approval(&s.pubkey(), [10; 32], max, TIER_A);
    let oracle = env.oracle.insecure_clone();
    let ed = ed25519_ix(&oracle, &env.message(&a), [0, 0, 0]);
    let mut ix = env.submit_ix(&oracle.pubkey(), &a);
    ix.accounts.extend(env.upkeep_metas());
    let msg = Message::new(&[ed, ix], Some(&oracle.pubkey()));
    let size = 1 + 64 * msg.header.num_required_signatures as usize + msg.serialize().len();
    println!("submit with full upkeep: {size} of 1232 bytes");
    assert!(size <= 1232, "submit transaction is {size} bytes");
    // And its compute stays well inside one instruction's default budget.
    let b = env.funded(unit());
    let ix = env.back_ix(&b.pubkey(), unit());
    let cu = env.send_cu(&[ix], &[&b]).unwrap();
    println!("backing with full upkeep: {cu} compute units");
    assert!(cu < 100_000);
}

#[test]
fn a_claim_in_the_line_stays_on_the_watch_list() {
    let mut env = Env::new();
    let s = queued(&mut env);
    assert_eq!(env.watch_state(0).claim, env.claim_addr(&s.pubkey(), &TX));
    any_tx(&mut env);
    any_tx(&mut env);
    // Still in the line: watched until the line releases or clears it.
    assert!(env.exists(&env.watch_entry(0)) && !env.watch_state(0).done);
    assert_eq!(env.pool_state().watch_head, 0);
}

#[test]
fn upkeep_places_must_be_at_their_own_addresses() {
    let mut env = Env::new();
    env.edit::<safu_pool::state::Pool>(&env.pool(), |p| {
        for k in [K::ApproveWindowSecs, K::InactivitySecs] {
            p.settings[k.index()] = k.bounds().1;
        }
    });
    env.matured_backer(4 * backing());
    claimants(&mut env, &[TX, TX2]);
    let mut metas = env.upkeep_metas();
    assert_eq!(metas.len(), 8);
    // The two watch places, each complete, passed in each other's slot.
    metas.rotate_left(4);
    let b = env.funded(unit());
    let mut ix = env.back_ix(&b.pubkey(), unit());
    ix.accounts.extend(metas);
    assert_err(
        env.send_bare(&[ix], &[&b]).map(|_| ()),
        PoolError::UpkeepAccountsMissing,
    );
}
