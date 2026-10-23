//! the claim path. Oracle approvals are really signed and verified by the Ed25519 precompile.
//! One test per error path, plus the full stake → claim → approve → cooldown → vest → paid flow.

mod common;

use anchor_lang::prelude::Pubkey;
use common::*;
use safu_pool::approval::approval_hash;
use safu_pool::constants::*;
use safu_pool::errors::PoolError;
use safu_pool::state::{ClaimStatus, Pool, StakeRecord};
use solana_keypair::Keypair;
use solana_signer::Signer;

const TX: [u8; 32] = [7; 32];
const TX2: [u8; 32] = [8; 32];
/// Backer capital so claims are solvent and under the stress cap.
fn backing() -> u64 {
    40 * unit()
}

/// Pool with matured backing and one max-size staker (beneficiary = itself).
fn setup() -> (Env, Keypair) {
    let mut env = Env::new();
    env.matured_backer(backing());
    let (_, max) = bounds();
    let s = env.staker(max);
    (env, s)
}

fn entitlement() -> u64 {
    bounds().1
}

/// Staker's claim admitted, time gate not yet met.
fn pending(env: &mut Env, s: &Keypair) {
    let a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    env.submit(&a).unwrap();
}

/// Staker's claim waiting for approval (gate met before the hack).
fn awaiting(env: &mut Env, s: &Keypair) {
    let gate = env.time_gate();
    env.warp(gate);
    pending(env, s);
    assert_eq!(
        env.claim_state(&s.pubkey(), &TX).status,
        ClaimStatus::AwaitingApproval
    );
}

/// Approved: stake forfeited, cooldown running.
fn active(env: &mut Env, s: &Keypair) {
    awaiting(env, s);
    let ix = env.approve_claim_ix(&s.pubkey(), &TX);
    env.ok(&[ix], &[s]);
}

// ------------------------------------------------------------------ submit: happy paths

#[test]
fn submit_before_the_gate_is_pending_and_allocates() {
    let (mut env, s) = setup();
    pending(&mut env, &s);
    let c = env.claim_state(&s.pubkey(), &TX);
    assert_eq!(
        (c.status, c.entitlement, c.stake, c.tier),
        (
            ClaimStatus::PendingTime,
            entitlement(),
            entitlement(),
            TIER_A
        )
    );
    let p = env.pool_state();
    assert_eq!(
        (p.total_allocated, p.day_admitted, p.day_oracle_count),
        (entitlement(), entitlement(), 1)
    );
    assert_eq!(
        env.stake_state(&s.pubkey()).active_claim,
        Some(env.claim_addr(&s.pubkey(), &TX))
    );
}

#[test]
fn full_claim_flow_pays_the_beneficiary_in_full() {
    let (mut env, s) = setup();
    active(&mut env, &s);
    let p = env.pool_state();
    assert_eq!((p.total_staked, p.total_stakers), (0, 0));
    let r = env.stake_state(&s.pubkey());
    assert!(r.forfeited);
    let c = env.claim_state(&s.pubkey(), &TX);
    assert_eq!(c.capacity_snapshot, backing() + entitlement());

    env.warp(COOLDOWN_SECS + VESTING_SECS / 2);
    let before = env.usdc(&s.pubkey());
    let ix = env.stream_ix(&s.pubkey(), &TX, &s.pubkey());
    env.ok(std::slice::from_ref(&ix), &[&s]);
    let half = entitlement() / 2;
    // Half the vesting window: half the entitlement, give or take one base unit of rounding.
    assert!(env.usdc(&s.pubkey()).abs_diff(before + half) <= 1);
    env.warp(VESTING_SECS);
    env.ok(&[ix], &[&s]);
    let c = env.claim_state(&s.pubkey(), &TX);
    assert_eq!(
        (c.status, c.streamed),
        (ClaimStatus::Completed, entitlement())
    );
    assert_eq!(env.pool_state().total_allocated, 0);
}

// ------------------------------------------------------------------ submit: refusals

#[test]
fn only_the_oracle_submits() {
    let (mut env, s) = setup();
    let a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    let other = env.funded(unit());
    let ed = ed25519_ix(&env.oracle.insecure_clone(), &env.message(&a), [0, 0, 0]);
    let ix = env.submit_ix(&other.pubkey(), &a);
    assert_err(env.send(&[ed, ix], &[&other]), PoolError::NotOracle);
}

#[test]
fn submit_refused_while_paused() {
    let (mut env, s) = setup();
    env.pause();
    let a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    assert_err(env.submit(&a), PoolError::Paused);
}

#[test]
fn submit_argument_checks() {
    let (mut env, s) = setup();
    let a = env.approval(&s.pubkey(), TX, 0, TIER_A);
    assert_err(env.submit(&a), PoolError::EntitlementNotPositive);
    for tier in [0, TIER_C + 1] {
        let a = env.approval(&s.pubkey(), TX, entitlement(), tier);
        assert_err(env.submit(&a), PoolError::InvalidTier);
    }
    let mut a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    a.deadline = env.now - 1;
    assert_err(env.submit(&a), PoolError::SignatureExpired);
    a.deadline = env.now + MAX_APPROVAL_WINDOW_SECS + 1;
    assert_err(env.submit(&a), PoolError::SignatureDeadlineTooFar);
}

#[test]
fn revoked_approval_refused() {
    let (mut env, s) = setup();
    let a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    let admin = env.admin.insecure_clone();
    let hash = approval_hash(&env.message(&a));
    let ix = env.revoke_ix(&admin.pubkey(), &a, hash);
    env.ok(&[ix], &[&admin]);
    assert_err(env.submit(&a), PoolError::ApprovalRevoked);
}

#[test]
fn lamports_sent_to_the_revocation_address_does_not_block_the_approval() {
    let (mut env, s) = setup();
    let a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    // Anyone can send SOL to the (derivable) revocation address; only a real revocation record counts.
    env.svm.airdrop(&env.revoked_addr(&a), SOL).unwrap();
    env.submit(&a).unwrap();
    assert_eq!(
        env.claim_state(&s.pubkey(), &TX).status,
        ClaimStatus::PendingTime
    );
}

#[test]
fn revocation_account_must_match_the_approval() {
    let (mut env, s) = setup();
    let a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    let oracle = env.oracle.insecure_clone();
    let ed = ed25519_ix(&oracle, &env.message(&a), [0, 0, 0]);
    let mut ix = env.submit_ix(&oracle.pubkey(), &a);
    ix.accounts[4].pubkey = Keypair::new().pubkey();
    assert_err(
        env.send(&[ed, ix], &[&oracle]),
        PoolError::WrongRevocationAccount,
    );
}

#[test]
fn revoke_rules() {
    let (mut env, s) = setup();
    let admin = env.admin.insecure_clone();
    let mut a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    let ix = env.revoke_ix(&admin.pubkey(), &a, [0; 32]);
    assert_err(
        env.send(&[ix], &[&admin]),
        PoolError::WrongRevocationAccount,
    );
    a.deadline = env.now - 1;
    let hash = approval_hash(&env.message(&a));
    let ix = env.revoke_ix(&admin.pubkey(), &a, hash);
    assert_err(env.send(&[ix], &[&admin]), PoolError::SignatureExpired);
    let other = env.funded(unit());
    let a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    let hash = approval_hash(&env.message(&a));
    let ix = env.revoke_ix(&other.pubkey(), &a, hash);
    assert_err(env.send(&[ix], &[&other]), PoolError::NotAdmin);
}

#[test]
fn ed25519_instruction_must_be_right_before() {
    let (mut env, s) = setup();
    let a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    let oracle = env.oracle.insecure_clone();
    let ix = env.submit_ix(&oracle.pubkey(), &a);
    assert_err(
        env.send(std::slice::from_ref(&ix), &[&oracle]),
        PoolError::MissingEd25519Instruction,
    );
    let ed = ed25519_ix(&oracle, &env.message(&a), [0, 0, 0]);
    let spacer = solana_system_interface::instruction::transfer(
        &oracle.pubkey(),
        &Keypair::new().pubkey(),
        SOL,
    );
    assert_err(
        env.send(&[ed, spacer, ix], &[&oracle]),
        PoolError::MissingEd25519Instruction,
    );
}

#[test]
fn approval_signed_by_another_key_refused() {
    let (mut env, s) = setup();
    let a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    let oracle = env.oracle.insecure_clone();
    let ed = ed25519_ix(&Keypair::new(), &env.message(&a), [0, 0, 0]);
    let ix = env.submit_ix(&oracle.pubkey(), &a);
    assert_err(
        env.send(&[ed, ix], &[&oracle]),
        PoolError::WrongOracleSigner,
    );
}

#[test]
fn approval_for_other_terms_or_cluster_refused() {
    let (mut env, s) = setup();
    let a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    let oracle = env.oracle.insecure_clone();
    let mut signed = a.clone();
    signed.entitlement += 1;
    let ed = ed25519_ix(&oracle, &env.message(&signed), [0, 0, 0]);
    let ix = env.submit_ix(&oracle.pubkey(), &a);
    assert_err(
        env.send(&[ed, ix.clone()], &[&oracle]),
        PoolError::ApprovalMessageMismatch,
    );
    let mainnet = safu_pool::approval::encode_message(&safu_pool::ID, CLUSTER_MAINNET, &a);
    let ed = ed25519_ix(&oracle, &mainnet, [0, 0, 0]);
    assert_err(
        env.send(&[ed, ix], &[&oracle]),
        PoolError::ApprovalMessageMismatch,
    );
}

#[test]
fn ed25519_offsets_must_stay_inside_the_precompile() {
    let (mut env, s) = setup();
    let a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    let oracle = env.oracle.insecure_clone();
    // The message offsets point at instruction 1 (the submit itself), which the runtime reads too.
    let mut ed = ed25519_ix(&oracle, &env.message(&a), [0, 0, 1]);
    ed.data[14..16].copy_from_slice(&1u16.to_le_bytes());
    let ix = env.submit_ix(&oracle.pubkey(), &a);
    let err = env.send(&[ed, ix], &[&oracle]).expect_err("must fail");
    // The precompile itself or our offset check rejects it; either way nothing is admitted.
    assert!(
        err.contains("Custom")
            || err.contains("PrecompileError")
            || err.contains("InvalidAccountData"),
        "{err}"
    );
    assert!(!env.exists(&env.claim_addr(&s.pubkey(), &TX)));
}

#[test]
fn stake_state_checks_on_submit() {
    let (mut env, s) = setup();
    let rec = env.stake_record(&s.pubkey());
    let marker = Keypair::new().pubkey();
    let a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    for (edit, e) in [
        (0, PoolError::StakeForfeited),
        (1, PoolError::StakeSuspended),
        (2, PoolError::ClaimAlreadyActiveForStake),
        (3, PoolError::ClaimAlreadyQueued),
    ] {
        env.edit::<StakeRecord>(&rec, |r| {
            r.forfeited = edit == 0;
            hold(r, edit == 1);
            r.active_claim = (edit == 2).then_some(marker);
            r.reserved_claim = (edit == 3).then_some(marker);
        });
        assert_err(env.submit(&a), e);
    }
}

#[test]
fn entitlement_above_the_tier_ceiling_refused() {
    let (mut env, s) = setup();
    let cap = env.tier_cap(entitlement(), TIER_C);
    let a = env.approval(&s.pubkey(), TX, cap + 1, TIER_C);
    assert_err(env.submit(&a), PoolError::EntitlementExceedsTierCap);
}

#[test]
fn hack_time_checks_on_submit() {
    let (mut env, s) = setup();
    let staked_at = env.stake_state(&s.pubkey()).staked_at;
    let mut a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    a.hack_timestamp = env.now + 1;
    assert_err(env.submit(&a), PoolError::HackTimestampInFuture);
    a.hack_timestamp = staked_at - 1;
    assert_err(env.submit(&a), PoolError::HackPredatesStake);
    let window = env.claim_window();
    env.warp(window + 1);
    let mut a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    a.hack_timestamp = staked_at;
    assert_err(env.submit(&a), PoolError::ClaimWindowExpired);
}

#[test]
fn same_wallet_and_transaction_can_never_be_claimed_twice() {
    let (mut env, s) = setup();
    pending(&mut env, &s);
    let admin = env.admin.insecure_clone();
    let ix = env.cancel_claim_ix(&admin.pubkey(), &s.pubkey(), &TX);
    env.ok(&[ix], &[&admin]);
    let a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    assert_err(env.submit(&a), PoolError::ClaimAlreadyExists);
}

// ------------------------------------------------------------------ queue

/// One max staker per tx, each with a tier-A claim larger than an empty pool can hold: queued.
fn queued_stakers(env: &mut Env, txs: &[[u8; 32]]) -> Vec<Keypair> {
    let (_, max) = bounds();
    let stakers: Vec<Keypair> = txs.iter().map(|_| env.staker(max)).collect();
    for (s, tx) in stakers.iter().zip(txs) {
        // One oracle submission a day per two stakers: a day apart keeps the limit out of it.
        env.warp(pool_core::params::SECONDS_PER_DAY);
        let a = env.approval(&s.pubkey(), *tx, env.tier_cap(max, TIER_A), TIER_A);
        env.submit(&a).unwrap();
        assert_eq!(
            env.claim_state(&s.pubkey(), tx).status,
            ClaimStatus::Reserved
        );
    }
    stakers
}

#[test]
fn insolvent_claim_is_queued_then_released_by_the_next_transaction_once_capital_arrives() {
    let mut env = Env::new();
    let s = queued_stakers(&mut env, &[TX]).remove(0);
    let claim = env.claim_addr(&s.pubkey(), &TX);
    let c = env.claim_state(&s.pubkey(), &TX);
    let p = env.pool_state();
    assert_eq!((c.queue_seq, p.queue_head, p.queue_tail), (0, 0, 1));
    assert_eq!(env.queue_state(0).claim, claim);
    assert_eq!(env.stake_state(&s.pubkey()).reserved_claim, Some(claim));
    assert_eq!(p.total_allocated, 0);
    assert_err(
        env.release(&s.pubkey(), &TX),
        PoolError::QueueReleaseNotYetEligible,
    );
    // The backer's own transaction releases it (upkeep); the place's rent goes back to the oracle.
    let oracle_before = env.lamports(&env.oracle.pubkey());
    env.matured_backer(backing());
    assert!(env.lamports(&env.oracle.pubkey()) > oracle_before);
    assert!(!env.exists(&env.queue_entry(0)));
    let r = env.stake_state(&s.pubkey());
    assert_eq!((r.active_claim, r.reserved_claim), (Some(claim), None));
    let p = env.pool_state();
    assert_eq!((p.total_allocated, p.queue_head), (c.entitlement, 1));
}

#[test]
fn the_line_is_first_come_first_served() {
    let mut env = Env::new();
    let s = queued_stakers(&mut env, &[TX, TX2]);
    assert_err(
        env.release(&s[1].pubkey(), &TX2),
        PoolError::NotFirstInQueue,
    );
    // Room for both: each transaction releases the head, one place at a time.
    env.matured_backer(4 * backing());
    assert_eq!(env.pool_state().queue_head, 1);
    assert_eq!(
        env.claim_state(&s[1].pubkey(), &TX2).status,
        ClaimStatus::Reserved
    );
    env.release(&s[1].pubkey(), &TX2).unwrap();
    let p = env.pool_state();
    assert_eq!((p.queue_head, p.queue_tail), (2, 2));
}

#[test]
fn a_new_claim_joins_the_back_while_anyone_waits() {
    let mut env = Env::new();
    let s = queued_stakers(&mut env, &[TX]).remove(0);
    // The head cannot go yet (suspended), though there is room now.
    let rec = env.stake_record(&s.pubkey());
    env.edit::<StakeRecord>(&rec, |r| hold(r, true));
    env.matured_backer(4 * backing());
    // There is room for a small claim, but someone is in line: it queues behind.
    let (_, max) = bounds();
    let late = env.staker(max);
    env.warp(pool_core::params::SECONDS_PER_DAY);
    let a = env.approval(&late.pubkey(), TX2, entitlement(), TIER_A);
    env.submit(&a).unwrap();
    let c = env.claim_state(&late.pubkey(), &TX2);
    assert_eq!((c.status, c.queue_seq), (ClaimStatus::Reserved, 1));
    env.edit::<StakeRecord>(&rec, |r| hold(r, false));
    env.release(&s.pubkey(), &TX).unwrap();
    env.release(&late.pubkey(), &TX2).unwrap();
    // Line empty again: the next claim is admitted directly.
    let next = env.staker(max);
    env.warp(pool_core::params::SECONDS_PER_DAY);
    let a = env.approval(&next.pubkey(), TX, entitlement(), TIER_A);
    env.submit(&a).unwrap();
    assert_ne!(
        env.claim_state(&next.pubkey(), &TX).status,
        ClaimStatus::Reserved
    );
}

#[test]
fn submit_must_name_the_back_of_the_line() {
    let mut env = Env::new();
    let (_, max) = bounds();
    let s = env.staker(max);
    let a = env.approval(&s.pubkey(), TX, env.tier_cap(max, TIER_A), TIER_A);
    let oracle = env.oracle.insecure_clone();
    let ed = ed25519_ix(&oracle, &env.message(&a), [0, 0, 0]);
    let mut ix = env.submit_ix(&oracle.pubkey(), &a);
    let at = ix
        .accounts
        .iter()
        .position(|m| m.pubkey == env.queue_entry(0))
        .unwrap();
    ix.accounts[at].pubkey = env.queue_entry(1);
    assert_err(env.send(&[ed, ix], &[&oracle]), PoolError::WrongQueueEntry);
}

#[test]
fn lamports_sent_to_the_next_place_do_not_block_queuing() {
    let mut env = Env::new();
    let place = env.queue_entry(0);
    let griefer = env.funded(0);
    let ix = anchor_lang::solana_program::system_instruction::transfer(
        &griefer.pubkey(),
        &place,
        1_000_000,
    );
    env.ok(&[ix], &[&griefer]);
    let s = queued_stakers(&mut env, &[TX]).remove(0);
    assert_eq!(env.queue_state(0).claim, env.claim_addr(&s.pubkey(), &TX));
}

#[test]
fn oracle_daily_limit_queues_instead_of_losing_the_claim() {
    let (mut env, s1) = setup();
    let (_, max) = bounds();
    let s2 = env.staker(max);
    // Two stakers: limit is 1 per day.
    pending(&mut env, &s1);
    let a = env.approval(&s2.pubkey(), TX2, entitlement(), TIER_A);
    env.submit(&a).unwrap();
    assert_eq!(
        env.claim_state(&s2.pubkey(), &TX2).status,
        ClaimStatus::Reserved
    );
    // Anyone can release it; a release never counts toward the oracle's limit.
    env.release(&s2.pubkey(), &TX2).unwrap();
    assert_eq!(env.pool_state().day_oracle_count, 1);
}

#[test]
fn stress_cap_queues_and_resets_the_next_day() {
    let mut env = Env::new();
    // Small backing: stress cap below one tier-A claim.
    env.matured_backer(4 * unit());
    let (_, max) = bounds();
    let s = env.staker(max);
    let capacity = 4 * unit() + max;
    let cap = pool_core::claim::stress_cap(capacity, 0, env.pool_state().settings().admit_rates())
        .unwrap();
    let a = env.approval(&s.pubkey(), TX, cap + 1, TIER_A);
    env.submit(&a).unwrap();
    assert_eq!(
        env.claim_state(&s.pubkey(), &TX).status,
        ClaimStatus::Reserved
    );
    assert_err(
        env.release(&s.pubkey(), &TX),
        PoolError::QueueReleaseNotYetEligible,
    );
}

#[test]
fn a_queued_claim_never_expires_on_time() {
    let mut env = Env::new();
    let s = queued_stakers(&mut env, &[TX]).remove(0);
    let window = env.claim_window();
    env.warp(10 * window);
    let expire = env.transition_ix(
        &s.pubkey(),
        &TX,
        safu_pool::instruction::ExpireQueuedClaim {},
    );
    let anyone = env.funded(unit());
    assert_err(
        env.send(&[expire], &[&anyone]),
        PoolError::QueueNotYetExpired,
    );
    assert_err(env.advance(&s.pubkey(), &TX), PoolError::QueueHeadLive);
    // Still first in line: released by the next transaction once there is room.
    env.matured_backer(backing());
    assert_ne!(
        env.claim_state(&s.pubkey(), &TX).status,
        ClaimStatus::Reserved
    );
}

#[test]
fn a_dead_queued_claim_expires_and_the_line_moves_past_it() {
    let mut env = Env::new();
    let s = queued_stakers(&mut env, &[TX, TX2]);
    // The first claim's stake now points elsewhere: it can never be released.
    let rec = env.stake_record(&s[0].pubkey());
    env.edit::<StakeRecord>(&rec, |r| r.forfeited = true);
    // Expiring it frees its stake; its place stays until the head reaches it.
    let expire = env.transition_ix(
        &s[0].pubkey(),
        &TX,
        safu_pool::instruction::ExpireQueuedClaim {},
    );
    let anyone = env.funded(unit());
    env.ok(&[expire], &[&anyone]);
    assert_eq!(
        env.claim_state(&s[0].pubkey(), &TX).status,
        ClaimStatus::Expired
    );
    assert!(env.exists(&env.queue_entry(0)));
    // The next transaction moves the line past the dead place (upkeep), the one after releases the
    // second claim.
    env.matured_backer(4 * backing());
    assert!(!env.exists(&env.queue_entry(0)));
    assert_eq!(env.pool_state().queue_head, 1);
    env.release(&s[1].pubkey(), &TX2).unwrap();
    // Every place is gone once the line is empty: nothing left to advance.
    assert!(env.advance(&s[1].pubkey(), &TX2).is_err());
}

#[test]
fn advance_expires_a_dead_head_in_one_step() {
    let mut env = Env::new();
    let s = queued_stakers(&mut env, &[TX]).remove(0);
    let rec = env.stake_record(&s.pubkey());
    env.edit::<StakeRecord>(&rec, |r| r.reserved_claim = None);
    env.advance(&s.pubkey(), &TX).unwrap();
    assert_eq!(
        env.claim_state(&s.pubkey(), &TX).status,
        ClaimStatus::Expired
    );
    let p = env.pool_state();
    assert_eq!((p.queue_head, p.queue_tail), (1, 1));
}

#[test]
fn release_refused_when_the_stake_changed_or_is_suspended() {
    let mut env = Env::new();
    let s = queued_stakers(&mut env, &[TX]).remove(0);
    let rec = env.stake_record(&s.pubkey());
    let other = Keypair::new().pubkey();
    env.edit::<StakeRecord>(&rec, |r| r.active_claim = Some(other));
    env.matured_backer(backing());
    assert_err(
        env.release(&s.pubkey(), &TX),
        PoolError::WalletHasDifferentActiveClaim,
    );
    env.edit::<StakeRecord>(&rec, |r| {
        r.active_claim = None;
        r.forfeited = true;
    });
    assert_err(
        env.release(&s.pubkey(), &TX),
        PoolError::QueuedClaimStakeChanged,
    );
    env.edit::<StakeRecord>(&rec, |r| {
        r.forfeited = false;
        hold(r, true);
    });
    assert_err(env.release(&s.pubkey(), &TX), PoolError::StakeSuspended);
}

// ------------------------------------------------------------------ gate and approval

#[test]
fn unlock_after_the_time_gate() {
    let (mut env, s) = setup();
    pending(&mut env, &s);
    let unlock = env.transition_ix(
        &s.pubkey(),
        &TX,
        safu_pool::instruction::UnlockPendingClaim {},
    );
    let anyone = env.funded(unit());
    assert_err(
        env.send(std::slice::from_ref(&unlock), &[&anyone]),
        PoolError::TimeGateNotMet,
    );
    let gate = env.time_gate();
    env.warp(gate);
    env.ok(std::slice::from_ref(&unlock), &[&anyone]);
    let c = env.claim_state(&s.pubkey(), &TX);
    assert_eq!(
        (c.status, c.approve_deadline),
        (ClaimStatus::AwaitingApproval, env.now + APPROVE_WINDOW_SECS)
    );
    assert_err(env.send(&[unlock], &[&anyone]), PoolError::ClaimNotPending);
}

#[test]
fn approve_rules() {
    let (mut env, s) = setup();
    pending(&mut env, &s);
    let approve = env.approve_claim_ix(&s.pubkey(), &TX);
    assert_err(
        env.send(std::slice::from_ref(&approve), &[&s]),
        PoolError::ClaimNotAwaitingApproval,
    );
    let gate = env.time_gate();
    env.warp(gate);
    let unlock = env.transition_ix(
        &s.pubkey(),
        &TX,
        safu_pool::instruction::UnlockPendingClaim {},
    );
    env.ok(&[unlock], &[&s]);
    let rec = env.stake_record(&s.pubkey());
    env.edit::<StakeRecord>(&rec, |r| hold(r, true));
    assert_err(
        env.send(std::slice::from_ref(&approve), &[&s]),
        PoolError::StakeSuspended,
    );
    env.edit::<StakeRecord>(&rec, |r| {
        hold(r, false);
        r.active_claim = None;
    });
    assert_err(
        env.send(std::slice::from_ref(&approve), &[&s]),
        PoolError::ClaimStakeMismatch,
    );
    let claim = env.claim_addr(&s.pubkey(), &TX);
    env.edit::<StakeRecord>(&rec, |r| r.active_claim = Some(claim));
    env.warp(APPROVE_WINDOW_SECS + 1);
    assert_err(
        env.send(&[approve], &[&s]),
        PoolError::ApprovalWindowExpired,
    );
}

#[test]
fn approval_keeps_the_claimants_yield_and_burns_their_points() {
    let (mut env, s) = setup();
    awaiting(&mut env, &s);
    // Points earned over the time gate, burned by the approval.
    let points = env.view_staker(&s.pubkey()).points;
    assert_eq!(
        points,
        (entitlement() / USDC_UNIT) * (env.time_gate() / SECONDS_PER_DAY) as u64
    );
    let credit = pool_core::yields::credit(
        unit() / 100,
        entitlement(),
        0,
        STAKER_YIELD_BPS,
        BACKER_YIELD_BPS,
    )
    .unwrap();
    env.edit::<Pool>(&env.pool(), |p| {
        p.staker_yield_index += credit.staker_index_bump;
        p.staker_yield_reserved += credit.staker_share;
    });
    let ix = env.approve_claim_ix(&s.pubkey(), &TX);
    env.ok(&[ix], &[&s]);
    // Still set aside and owed to the claimant: the protocol gets none of it.
    let p = env.pool_state();
    assert_eq!(
        (p.staker_yield_reserved, p.protocol_yield_balance),
        (credit.staker_share, 0)
    );
    let r = env.stake_state(&s.pubkey());
    assert_eq!((r.yield_owed, r.points), (credit.staker_share, 0));
    assert_eq!(
        env.view_staker(&s.pubkey()),
        safu_pool::instructions::StakerView {
            yield_owed: credit.staker_share,
            points: 0
        }
    );
    // Collected any time after: the forfeited stake earns nothing more.
    env.warp(SECONDS_PER_DAY);
    let before = env.usdc(&s.pubkey());
    let ix = env.claim_yield_ix(&s.pubkey(), &s.pubkey());
    env.ok(&[ix], &[&s]);
    assert_eq!(env.usdc(&s.pubkey()), before + credit.staker_share);
    assert_eq!(env.stake_state(&s.pubkey()).yield_owed, 0);
    assert_eq!(env.view_staker(&s.pubkey()).points, 0);
}

#[test]
fn forfeited_address_can_never_stake_again() {
    let (mut env, s) = setup();
    active(&mut env, &s);
    let ix = env.stake_ix(&s.pubkey(), entitlement(), s.pubkey());
    assert_err(env.send(&[ix], &[&s]), PoolError::AddressHasApprovedClaim);
}

#[test]
fn unapproved_claim_expires_and_releases_its_reservation() {
    let (mut env, s) = setup();
    awaiting(&mut env, &s);
    let expire = env.transition_ix(
        &s.pubkey(),
        &TX,
        safu_pool::instruction::ExpirePendingApproval {},
    );
    let anyone = env.funded(unit());
    assert_err(
        env.send(std::slice::from_ref(&expire), &[&anyone]),
        PoolError::ApprovalWindowNotExpired,
    );
    env.warp(APPROVE_WINDOW_SECS + 1);
    let rec = env.stake_record(&s.pubkey());
    env.edit::<StakeRecord>(&rec, |r| hold(r, true));
    assert_err(
        env.send(std::slice::from_ref(&expire), &[&anyone]),
        PoolError::StakeSuspended,
    );
    env.edit::<StakeRecord>(&rec, |r| hold(r, false));
    env.ok(std::slice::from_ref(&expire), &[&anyone]);
    assert_eq!(env.pool_state().total_allocated, 0);
    assert_eq!(env.stake_state(&s.pubkey()).active_claim, None);
    assert_err(
        env.send(&[expire], &[&anyone]),
        PoolError::ClaimNotAwaitingApproval,
    );
}

// ------------------------------------------------------------------ stream

#[test]
fn stream_waits_for_cooldown_and_vesting() {
    let (mut env, s) = setup();
    pending(&mut env, &s);
    let ix = env.stream_ix(&s.pubkey(), &TX, &s.pubkey());
    assert_err(
        env.send(std::slice::from_ref(&ix), &[&s]),
        PoolError::ClaimNotActive,
    );
    let gate = env.time_gate();
    env.warp(gate);
    let unlock = env.transition_ix(
        &s.pubkey(),
        &TX,
        safu_pool::instruction::UnlockPendingClaim {},
    );
    let approve = env.approve_claim_ix(&s.pubkey(), &TX);
    env.ok(&[unlock, approve], &[&s]);
    assert_err(
        env.send(std::slice::from_ref(&ix), &[&s]),
        PoolError::CooldownNotPassed,
    );
    env.warp(COOLDOWN_SECS);
    assert_err(env.send(&[ix], &[&s]), PoolError::NothingVested);
}

#[test]
fn stream_goes_only_to_the_beneficiary_and_not_while_suspended() {
    let (mut env, s) = setup();
    active(&mut env, &s);
    env.warp(COOLDOWN_SECS + VESTING_SECS);
    let other = Keypair::new().pubkey();
    env.open_usdc(&other);
    let ix = env.stream_ix(&s.pubkey(), &TX, &other);
    assert_err(env.send(&[ix], &[&s]), PoolError::WrongBeneficiary);
    let rec = env.stake_record(&s.pubkey());
    env.edit::<StakeRecord>(&rec, |r| hold(r, true));
    let ix = env.stream_ix(&s.pubkey(), &TX, &s.pubkey());
    assert_err(env.send(&[ix], &[&s]), PoolError::StakeSuspended);
}

#[test]
fn stream_capped_per_day_and_resumes_tomorrow() {
    let (mut env, s) = setup();
    active(&mut env, &s);
    env.warp(COOLDOWN_SECS + VESTING_SECS);
    // Today's share already taken.
    let today = pool_core::claim::day_of(env.now);
    let claim = env.claim_addr(&s.pubkey(), &TX);
    env.edit::<safu_pool::state::Claim>(&claim, |c| {
        c.share_day = today;
        c.share_left = 0;
    });
    let ix = env.stream_ix(&s.pubkey(), &TX, &s.pubkey());
    assert_err(
        env.send(std::slice::from_ref(&ix), &[&s]),
        PoolError::DailyOutflowCapReached,
    );
    env.warp(pool_core::params::SECONDS_PER_DAY);
    env.ok(std::slice::from_ref(&ix), &[&s]);
    assert_err(env.send(&[ix], &[&s]), PoolError::ClaimNotActive);
}

#[test]
fn claims_share_the_days_cap_pro_rata_whatever_the_order() {
    let (mut env, s1) = setup();
    // The first claim waits uncollected through the second's gate: a window long enough to keep it.
    let longest = pool_core::settings::SettingKey::InactivitySecs.bounds().1;
    env.edit::<Pool>(&env.pool(), |p| {
        p.settings[pool_core::settings::SettingKey::InactivitySecs.index()] = longest
    });
    active(&mut env, &s1);
    let (_, max) = bounds();
    let s2 = env.staker(max);
    env.warp(pool_core::params::SECONDS_PER_DAY);
    let gate = env.time_gate();
    env.warp(gate);
    let a = env.approval(&s2.pubkey(), TX2, entitlement(), TIER_A);
    env.submit(&a).unwrap();
    let ix = env.approve_claim_ix(&s2.pubkey(), &TX2);
    env.ok(&[ix], &[&s2]);
    env.warp(COOLDOWN_SECS + VESTING_SECS);
    // A tiny payout rate, so the day's cap is far below what the two are owed.
    env.edit::<Pool>(&env.pool(), |p| {
        for k in [
            pool_core::settings::SettingKey::PayoutLowBps,
            pool_core::settings::SettingKey::PayoutMidBps,
            pool_core::settings::SettingKey::PayoutHighBps,
        ] {
            p.settings[k.index()] = 1;
        }
    });
    let p = env.pool_state();
    let capacity = p.total_staked + p.total_backed;
    let snap_allocated = p.total_allocated;
    let share = |env: &Env, c: &safu_pool::state::Claim| {
        let p = env.pool_state();
        let base = capacity.max(c.capacity_snapshot);
        let cap =
            pool_core::claim::payout_cap(base, p.total_allocated, p.settings().payout_rates())
                .unwrap();
        pool_core::drip::share(cap, c.entitlement - c.streamed, snap_allocated).unwrap()
    };
    // The later claim collects first; the earlier one's share is untouched by it.
    let (c1, c2) = (
        env.claim_state(&s1.pubkey(), &TX),
        env.claim_state(&s2.pubkey(), &TX2),
    );
    let (want1, want2) = (share(&env, &c1), share(&env, &c2));
    assert!(want1 + want2 < (c1.entitlement - c1.streamed) + (c2.entitlement - c2.streamed));
    for (s, tx) in [(&s2, &TX2), (&s1, &TX)] {
        let ix = env.stream_ix(&s.pubkey(), tx, &s.pubkey());
        env.ok(&[ix], &[s]);
    }
    let (after1, after2) = (
        env.claim_state(&s1.pubkey(), &TX),
        env.claim_state(&s2.pubkey(), &TX2),
    );
    assert_eq!(after1.streamed - c1.streamed, want1);
    assert_eq!(after2.streamed - c2.streamed, want2);
    // Each took its whole share today: nothing more until tomorrow.
    let ix = env.stream_ix(&s1.pubkey(), &TX, &s1.pubkey());
    assert_err(env.send(&[ix], &[&s1]), PoolError::DailyOutflowCapReached);
}

#[test]
fn stream_short_of_cash_pays_nothing_and_counts_the_day() {
    // Cash short (multichain `claim_stream`): no payment, the day counts once as blocked time.
    let (mut env, s) = setup();
    active(&mut env, &s);
    env.warp(COOLDOWN_SECS + VESTING_SECS);
    // All liquid USDC is rounding reserve: never claim cash.
    let liquid = env.vault_liquid();
    env.edit::<Pool>(&env.pool(), |p| p.rounding_reserve = liquid);
    let before = env.usdc(&s.pubkey());
    let stream = |env: &mut Env| {
        let ix = env.stream_ix(&s.pubkey(), &TX, &s.pubkey());
        env.ok(&[ix], &[&s]);
    };
    stream(&mut env);
    assert_eq!(env.usdc(&s.pubkey()), before);
    assert_eq!(env.claim_state(&s.pubkey(), &TX).streamed, 0);
    assert_eq!(env.pool_state().cash_short_days, 1);
    let blocked = env.pool_state().blocked_secs(env.now);
    assert_eq!(env.claim_state(&s.pubkey(), &TX).blocked_mark, blocked);
    // Same day: counted once.
    env.warp(60);
    stream(&mut env);
    assert_eq!(env.pool_state().cash_short_days, 1);
    // Next day: counted again.
    env.warp(pool_core::params::SECONDS_PER_DAY);
    stream(&mut env);
    assert_eq!(env.pool_state().cash_short_days, 2);
    // Cash back: it pays, and the count stays.
    env.edit::<Pool>(&env.pool(), |p| p.rounding_reserve = 0);
    env.warp(60);
    stream(&mut env);
    assert!(env.usdc(&s.pubkey()) > before);
    assert_eq!(env.pool_state().cash_short_days, 2);
}

#[test]
fn stale_claim_returns_the_unpaid_rest() {
    let (mut env, s) = setup();
    active(&mut env, &s);
    let expire = env.transition_ix(
        &s.pubkey(),
        &TX,
        safu_pool::instruction::ExpireStaleClaim {},
    );
    let anyone = env.funded(unit());
    env.warp(COOLDOWN_SECS + COLLECTION_INACTIVITY_SECS);
    assert_err(
        env.send(std::slice::from_ref(&expire), &[&anyone]),
        PoolError::ClaimNotStale,
    );
    env.warp(1);
    let rec = env.stake_record(&s.pubkey());
    env.edit::<StakeRecord>(&rec, |r| hold(r, true));
    assert_err(
        env.send(std::slice::from_ref(&expire), &[&anyone]),
        PoolError::StakeSuspended,
    );
    env.edit::<StakeRecord>(&rec, |r| hold(r, false));
    env.ok(std::slice::from_ref(&expire), &[&anyone]);
    assert_eq!(
        env.claim_state(&s.pubkey(), &TX).status,
        ClaimStatus::Expired
    );
    assert_eq!(env.pool_state().total_allocated, 0);
    assert_err(env.send(&[expire], &[&anyone]), PoolError::ClaimNotActive);
}

#[test]
fn stale_sweep_refused_on_a_fully_paid_claim_record() {
    let (mut env, s) = setup();
    active(&mut env, &s);
    let claim = env.claim_addr(&s.pubkey(), &TX);
    env.edit::<safu_pool::state::Claim>(&claim, |c| c.streamed = c.entitlement);
    env.warp(COOLDOWN_SECS + COLLECTION_INACTIVITY_SECS + 1);
    let expire = env.transition_ix(
        &s.pubkey(),
        &TX,
        safu_pool::instruction::ExpireStaleClaim {},
    );
    let anyone = env.funded(unit());
    assert_err(
        env.send(&[expire], &[&anyone]),
        PoolError::ClaimFullyStreamed,
    );
}

// ------------------------------------------------------------------ cancel, suspend

#[test]
fn cancel_of_an_approved_claim_restores_the_stake_under_penalty() {
    let (mut env, s) = setup();
    active(&mut env, &s);
    let admin = env.admin.insecure_clone();
    let ix = env.cancel_claim_ix(&admin.pubkey(), &s.pubkey(), &TX);
    env.ok(std::slice::from_ref(&ix), &[&admin]);
    let r = env.stake_state(&s.pubkey());
    assert!(!r.forfeited && r.active_claim.is_none());
    assert_eq!(r.penalty_locked_until, env.now + PENALTY_LOCK_SECS);
    let p = env.pool_state();
    assert_eq!(
        (p.total_staked, p.total_stakers, p.total_allocated),
        (entitlement(), 1, 0)
    );
    assert_err(env.send(&[ix], &[&admin]), PoolError::ClaimNotCancellable);
    let w = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    assert_err(env.send(&[w], &[&s]), PoolError::PenaltyLockActive);
}

#[test]
fn cancel_of_a_partly_paid_claim_restores_only_the_unpaid_stake() {
    let (mut env, s) = setup();
    active(&mut env, &s);
    env.warp(COOLDOWN_SECS + VESTING_SECS / 2);
    let ix = env.stream_ix(&s.pubkey(), &TX, &s.pubkey());
    env.ok(&[ix], &[&s]);
    let paid = env.claim_state(&s.pubkey(), &TX).streamed;
    assert!(paid > 0 && paid < entitlement());
    let admin = env.admin.insecure_clone();
    let ix = env.cancel_claim_ix(&admin.pubkey(), &s.pubkey(), &TX);
    env.ok(&[ix], &[&admin]);
    // What was paid stays paid and comes off the stake.
    let r = env.stake_state(&s.pubkey());
    assert!(!r.forfeited && r.forfeited_by.is_none());
    assert_eq!(r.amount, entitlement() - paid);
    let p = env.pool_state();
    assert_eq!(
        (p.total_staked, p.total_stakers, p.total_allocated),
        (entitlement() - paid, 1, 0)
    );
    // The rest leaves in full once the penalty lock has passed, even below the min stake.
    env.warp(PENALTY_LOCK_SECS);
    let w = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    env.ok(&[w], &[&s]);
    assert_eq!(env.pool_state().total_staked, 0);
}

#[test]
fn cancel_of_a_claim_that_paid_the_whole_stake_leaves_it_spent() {
    let (mut env, s) = setup();
    active(&mut env, &s);
    let claim = env.claim_addr(&s.pubkey(), &TX);
    env.edit::<safu_pool::state::Claim>(&claim, |c| c.streamed = c.stake);
    let admin = env.admin.insecure_clone();
    let ix = env.cancel_claim_ix(&admin.pubkey(), &s.pubkey(), &TX);
    env.ok(&[ix], &[&admin]);
    let r = env.stake_state(&s.pubkey());
    assert!(r.forfeited && r.forfeited_by.is_none() && r.active_claim.is_none());
    let p = env.pool_state();
    assert_eq!((p.total_staked, p.total_stakers), (0, 0));
    // Nothing to withdraw, and no override may count the spent principal as capacity again.
    let w = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    assert_err(env.send(&[w], &[&s]), PoolError::StakeForfeited);
    assert_err(
        both_approve(&mut env, &s.pubkey(), TX, entitlement(), TIER_A),
        PoolError::StakeForfeited,
    );
}

#[test]
fn cancel_before_approval_has_no_penalty() {
    let (mut env, s) = setup();
    pending(&mut env, &s);
    let admin = env.admin.insecure_clone();
    let ix = env.cancel_claim_ix(&admin.pubkey(), &s.pubkey(), &TX);
    env.ok(&[ix], &[&admin]);
    let r = env.stake_state(&s.pubkey());
    assert_eq!((r.penalty_locked_until, r.active_claim), (0, None));
    let w = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    env.ok(&[w], &[&s]);
}

#[test]
fn only_admin_cancels() {
    let (mut env, s) = setup();
    pending(&mut env, &s);
    let other = env.funded(unit());
    let ix = env.cancel_claim_ix(&other.pubkey(), &s.pubkey(), &TX);
    assert_err(env.send(&[ix], &[&other]), PoolError::NotAdmin);
}

#[test]
fn unsuspend_gives_a_fresh_approval_window() {
    let (mut env, s) = setup();
    awaiting(&mut env, &s);
    let admin = env.admin.insecure_clone();
    let claim = env.claim_addr(&s.pubkey(), &TX);
    let ix = env.suspend_ix(&s.pubkey(), None, true);
    env.ok(&[ix], &[&admin]);
    env.warp(APPROVE_WINDOW_SECS + 1);
    let ix = env.suspend_ix(&s.pubkey(), Some(claim), false);
    env.ok(&[ix], &[&admin]);
    assert_eq!(
        env.claim_state(&s.pubkey(), &TX).approve_deadline,
        env.now + APPROVE_WINDOW_SECS
    );
    let approve = env.approve_claim_ix(&s.pubkey(), &TX);
    env.ok(&[approve], &[&s]);
}

#[test]
fn suspend_refused_on_a_forfeited_stake_without_a_claim() {
    let (mut env, s) = setup();
    let rec = env.stake_record(&s.pubkey());
    env.edit::<StakeRecord>(&rec, |r| r.forfeited = true);
    let admin = env.admin.insecure_clone();
    let ix = env.suspend_ix(&s.pubkey(), None, true);
    assert_err(env.send(&[ix], &[&admin]), PoolError::StakeForfeited);
}

/// Taking the whole stake out and staking again does not clear a suspension.
#[test]
fn a_restake_keeps_the_suspension() {
    let (mut env, s) = setup();
    let admin = env.admin.insecure_clone();
    let ix = env.suspend_ix(&s.pubkey(), None, true);
    env.ok(&[ix], &[&admin]);
    let until = env.stake_state(&s.pubkey()).suspended_until;
    let w = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    env.ok(&[w], &[&s]);
    let st = env.stake_ix(&s.pubkey(), bounds().1, s.pubkey());
    env.ok(&[st], &[&s]);
    let rec = env.stake_state(&s.pubkey());
    assert!(rec.suspended);
    assert_eq!(rec.suspended_until, until);
}

/// The admin's suspension lapses on its own, and the claim's window then runs from its end: a hold
/// that ran out never costs the staker the claim.
#[test]
fn a_suspension_lapses_and_the_window_runs_from_its_end() {
    let (mut env, s) = setup();
    awaiting(&mut env, &s);
    let admin = env.admin.insecure_clone();
    let deadline = env.claim_state(&s.pubkey(), &TX).approve_deadline;
    env.warp(deadline - env.now - SUSPEND_MAX_SECS / 2);
    let ix = env.suspend_ix(&s.pubkey(), None, true);
    env.ok(&[ix], &[&admin]);
    let until = env.now + SUSPEND_MAX_SECS;
    assert_eq!(env.stake_state(&s.pubkey()).suspended_until, until);
    let approve = env.approve_claim_ix(&s.pubkey(), &TX);
    assert_err(
        env.send(std::slice::from_ref(&approve), &[&s]),
        PoolError::StakeSuspended,
    );
    // Lapsed, past the old deadline: the window counts from the end of the suspension.
    env.warp(SUSPEND_MAX_SECS);
    assert!(env.now > deadline);
    let expire = env.transition_ix(
        &s.pubkey(),
        &TX,
        safu_pool::instruction::ExpirePendingApproval {},
    );
    let anyone = env.funded(unit());
    assert_err(
        env.send(&[expire], &[&anyone]),
        PoolError::ApprovalWindowNotExpired,
    );
    env.ok(&[approve], &[&s]);
}

/// One key cannot hold a claimant for good: the admin alone suspends once per term and its end;
/// the admin plus the co-signer or guardian renew it.
#[test]
fn suspending_again_soon_needs_a_second_role() {
    let (mut env, s) = setup();
    let (admin, co, guardian) = (
        env.admin.insecure_clone(),
        env.co_signer.insecure_clone(),
        env.guardian.insecure_clone(),
    );
    let alone = |env: &mut Env| {
        let ix = env.suspend_ix(&s.pubkey(), None, true);
        env.send(&[ix], &[&admin])
    };
    alone(&mut env).unwrap();
    assert_err(alone(&mut env), PoolError::SuspendNeedsSecondRole);
    env.warp(SUSPEND_MAX_SECS);
    assert!(!env.stake_state(&s.pubkey()).is_suspended(env.now));
    assert_err(alone(&mut env), PoolError::SuspendNeedsSecondRole);
    let other = env.funded(0);
    let ix = env.renew_suspension_ix(&s.pubkey(), &other.pubkey());
    assert_err(
        env.send(&[ix], &[&admin, &other]),
        PoolError::ApproverNotSecondRole,
    );
    for second in [&co, &guardian] {
        let ix = env.renew_suspension_ix(&s.pubkey(), &second.pubkey());
        env.ok(&[ix], &[&admin, second]);
        let r = env.stake_state(&s.pubkey());
        assert!(r.is_suspended(env.now));
        assert_eq!(r.suspended_until, env.now + SUSPEND_MAX_SECS);
        env.warp(SUSPEND_MAX_SECS / 2);
    }
    // A full term after the last one ended, the admin alone may again.
    env.warp(2 * SUSPEND_MAX_SECS);
    alone(&mut env).unwrap();
    // Unsuspend ends it now.
    let ix = env.suspend_ix(&s.pubkey(), None, false);
    env.ok(&[ix], &[&admin]);
    assert_eq!(env.stake_state(&s.pubkey()).suspended_until, env.now);
}

// ------------------------------------------------------------------ override

#[test]
fn override_needs_both_signers_then_executes() {
    let (mut env, s) = setup();
    let (admin, co) = (env.admin.insecure_clone(), env.co_signer.insecure_clone());
    let claim = env.claim_addr(&s.pubkey(), &TX);
    let ix = env.override_ix(&admin.pubkey(), &s.pubkey(), TX, entitlement(), TIER_B);
    env.ok(std::slice::from_ref(&ix), &[&admin]);
    assert_eq!(
        env.claim_state(&s.pubkey(), &TX).status,
        ClaimStatus::Unused
    );
    // Mismatched terms from the second signer are refused.
    let bad = env.override_ix(&co.pubkey(), &s.pubkey(), TX, entitlement() + 1, TIER_B);
    assert_err(env.send(&[bad], &[&co]), PoolError::OverrideParamsMismatch);
    let ix = env.override_ix(&co.pubkey(), &s.pubkey(), TX, entitlement(), TIER_B);
    env.ok(&[ix], &[&co]);
    let c = env.claim_state(&s.pubkey(), &TX);
    assert_eq!((c.status, c.tier), (ClaimStatus::Active, TIER_B));
    assert!(env.stake_state(&s.pubkey()).forfeited);
    assert!(!env.exists(&env.override_addr(&claim)));
}

#[test]
fn override_roles_and_cancel() {
    let (mut env, s) = setup();
    let other = env.funded(unit());
    let ix = env.override_ix(&other.pubkey(), &s.pubkey(), TX, entitlement(), TIER_A);
    assert_err(env.send(&[ix], &[&other]), PoolError::NotAdminOrCoSigner);
    let admin = env.admin.insecure_clone();
    let ix = env.override_ix(&admin.pubkey(), &s.pubkey(), TX, entitlement(), TIER_A);
    env.ok(&[ix], &[&admin]);
    let claim = env.claim_addr(&s.pubkey(), &TX);
    let ix = env.cancel_override_ix(&other.pubkey(), &claim);
    assert_err(env.send(&[ix], &[&other]), PoolError::NotAdmin);
    let ix = env.cancel_override_ix(&admin.pubkey(), &claim);
    env.ok(&[ix], &[&admin]);
    assert!(!env.exists(&env.override_addr(&claim)));
}

fn both_approve(
    env: &mut Env,
    s: &Pubkey,
    tx: [u8; 32],
    entitlement: u64,
    tier: u8,
) -> Result<(), String> {
    let (admin, co) = (env.admin.insecure_clone(), env.co_signer.insecure_clone());
    let ix = env.override_ix(&admin.pubkey(), s, tx, entitlement, tier);
    env.send(&[ix], &[&admin])?;
    let ix = env.override_ix(&co.pubkey(), s, tx, entitlement, tier);
    env.send(&[ix], &[&co])
}

#[test]
fn override_refusals() {
    let (mut env, s) = setup();
    let cap = env.tier_cap(entitlement(), TIER_C);
    assert_err(
        both_approve(&mut env, &s.pubkey(), TX, cap + 1, TIER_C),
        PoolError::EntitlementExceedsTierCap,
    );
    // A different claim is open on this stake.
    pending(&mut env, &s);
    assert_err(
        both_approve(&mut env, &s.pubkey(), TX2, entitlement(), TIER_A),
        PoolError::WalletHasDifferentActiveClaim,
    );
}

#[test]
fn override_refused_when_insolvent_or_completed() {
    let mut env = Env::new();
    let (_, max) = bounds();
    let s = env.staker(max);
    let big = env.tier_cap(max, TIER_A);
    assert_err(
        both_approve(&mut env, &s.pubkey(), TX, big, TIER_A),
        PoolError::Insolvent,
    );

    let (mut env, s) = setup();
    active(&mut env, &s);
    let claim = env.claim_addr(&s.pubkey(), &TX);
    env.edit::<safu_pool::state::Claim>(&claim, |c| c.status = ClaimStatus::Completed);
    assert_err(
        both_approve(&mut env, &s.pubkey(), TX, entitlement(), TIER_A),
        PoolError::ClaimAlreadyCompleted,
    );
}

#[test]
fn override_on_a_forfeited_stake_only_for_the_claim_that_forfeited_it() {
    let (mut env, s) = setup();
    active(&mut env, &s);
    // The forfeiting claim goes stale and returns its unpaid rest; the stake stays forfeited.
    env.warp(COOLDOWN_SECS + COLLECTION_INACTIVITY_SECS + 1);
    let expire = env.transition_ix(
        &s.pubkey(),
        &TX,
        safu_pool::instruction::ExpireStaleClaim {},
    );
    let anyone = env.funded(unit());
    env.ok(&[expire], &[&anyone]);
    assert_eq!(
        env.stake_state(&s.pubkey()).forfeited_by,
        Some(env.claim_addr(&s.pubkey(), &TX))
    );
    // A new claim may not count the same forfeited principal as capacity again.
    assert_err(
        both_approve(&mut env, &s.pubkey(), TX2, entitlement(), TIER_A),
        PoolError::StakeForfeited,
    );
    // Re-executing the claim that forfeited it still works.
    both_approve(&mut env, &s.pubkey(), TX, entitlement(), TIER_A).unwrap();
    assert_eq!(
        env.claim_state(&s.pubkey(), &TX).status,
        ClaimStatus::Active
    );
}

#[test]
fn override_correction_carries_what_was_already_paid() {
    let (mut env, s) = setup();
    active(&mut env, &s);
    env.warp(COOLDOWN_SECS + VESTING_SECS / 2);
    let ix = env.stream_ix(&s.pubkey(), &TX, &s.pubkey());
    env.ok(&[ix], &[&s]);
    let paid = env.claim_state(&s.pubkey(), &TX).streamed;
    let corrected = 2 * entitlement();
    both_approve(&mut env, &s.pubkey(), TX, corrected, TIER_A).unwrap();
    let c = env.claim_state(&s.pubkey(), &TX);
    assert_eq!(
        (c.entitlement, c.streamed, c.status),
        (corrected, paid, ClaimStatus::Active)
    );
    // Allocation = new entitlement (the old unpaid part was released first).
    assert_eq!(env.pool_state().total_allocated, corrected);
}

#[test]
fn override_takes_over_the_stakes_own_queued_claim() {
    let mut env = Env::new();
    let (_, max) = bounds();
    let s = env.staker(max);
    let big = env.tier_cap(max, TIER_A);
    let a = env.approval(&s.pubkey(), TX, big, TIER_A);
    // Stays queued: the pool cannot hold it. The override's smaller amount fits the stake alone.
    env.submit(&a).unwrap();
    both_approve(&mut env, &s.pubkey(), TX, entitlement(), TIER_A).unwrap();
    let r = env.stake_state(&s.pubkey());
    assert_eq!(
        (r.reserved_claim, r.active_claim),
        (None, Some(env.claim_addr(&s.pubkey(), &TX)))
    );
    // It keeps its place number, so its place can still be dropped and the line moves on.
    assert_eq!(env.claim_state(&s.pubkey(), &TX).queue_seq, 0);
    assert_err(env.release(&s.pubkey(), &TX), PoolError::NoSuchQueuedClaim);
    env.advance(&s.pubkey(), &TX).unwrap();
    assert_eq!(
        env.claim_state(&s.pubkey(), &TX).status,
        ClaimStatus::Active
    );
    assert_eq!(env.pool_state().queue_head, 1);
}
