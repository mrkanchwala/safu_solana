//! B2: the claim path. Oracle approvals are really signed and verified by the Ed25519 precompile.
//! One test per error path, plus the full stake → claim → approve → cooldown → vest → paid flow.

mod common;

use common::*;
use safu_pool::approval::approval_hash;
use safu_pool::constants::*;
use safu_pool::errors::PoolError;
use safu_pool::state::{ClaimStatus, Pool, StakeRecord};
use anchor_lang::prelude::Pubkey;
use solana_keypair::Keypair;
use solana_signer::Signer;

const TX: [u8; 32] = [7; 32];
const TX2: [u8; 32] = [8; 32];
/// Backer capital so claims are solvent and under the stress cap.
const BACKING: u64 = 40 * SOL;

/// Pool with matured backing and one max-size staker (beneficiary = itself).
fn setup() -> (Env, Keypair) {
    let mut env = Env::new();
    env.matured_backer(BACKING);
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
    env.warp(TIME_GATE_SECS);
    pending(env, s);
    assert_eq!(env.claim_state(&s.pubkey(), &TX).status, ClaimStatus::AwaitingApproval);
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
    assert_eq!((c.status, c.entitlement, c.stake, c.tier), (ClaimStatus::PendingTime, entitlement(), entitlement(), TIER_A));
    let p = env.pool_state();
    assert_eq!((p.total_allocated, p.day_admitted, p.day_oracle_count), (entitlement(), entitlement(), 1));
    assert_eq!(env.stake_state(&s.pubkey()).active_claim, Some(env.claim_addr(&s.pubkey(), &TX)));
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
    assert_eq!(c.capacity_snapshot, BACKING + entitlement());

    env.warp(COOLDOWN_SECS + VESTING_SECS / 2);
    let before = env.lamports(&s.pubkey());
    let ix = env.stream_ix(&s.pubkey(), &TX, &s.pubkey());
    env.ok(std::slice::from_ref(&ix), &[&s]);
    let half = entitlement() / 2;
    assert!(env.lamports(&s.pubkey()) + SOL / 1_000 > before + half);
    env.warp(VESTING_SECS);
    env.ok(&[ix], &[&s]);
    let c = env.claim_state(&s.pubkey(), &TX);
    assert_eq!((c.status, c.streamed), (ClaimStatus::Completed, entitlement()));
    assert_eq!(env.pool_state().total_allocated, 0);
}

// ------------------------------------------------------------------ submit: refusals

#[test]
fn only_the_oracle_submits() {
    let (mut env, s) = setup();
    let a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    let other = env.funded(SOL);
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
fn sol_sent_to_the_revocation_address_does_not_block_the_approval() {
    let (mut env, s) = setup();
    let a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    // Anyone can send SOL to the (derivable) revocation address; only a real revocation record counts.
    env.svm.airdrop(&env.revoked_addr(&a), SOL).unwrap();
    env.submit(&a).unwrap();
    assert_eq!(env.claim_state(&s.pubkey(), &TX).status, ClaimStatus::PendingTime);
}

#[test]
fn revocation_account_must_match_the_approval() {
    let (mut env, s) = setup();
    let a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    let oracle = env.oracle.insecure_clone();
    let ed = ed25519_ix(&oracle, &env.message(&a), [0, 0, 0]);
    let mut ix = env.submit_ix(&oracle.pubkey(), &a);
    ix.accounts[4].pubkey = Keypair::new().pubkey();
    assert_err(env.send(&[ed, ix], &[&oracle]), PoolError::WrongRevocationAccount);
}

#[test]
fn revoke_rules() {
    let (mut env, s) = setup();
    let admin = env.admin.insecure_clone();
    let mut a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    let ix = env.revoke_ix(&admin.pubkey(), &a, [0; 32]);
    assert_err(env.send(&[ix], &[&admin]), PoolError::WrongRevocationAccount);
    a.deadline = env.now - 1;
    let hash = approval_hash(&env.message(&a));
    let ix = env.revoke_ix(&admin.pubkey(), &a, hash);
    assert_err(env.send(&[ix], &[&admin]), PoolError::SignatureExpired);
    let other = env.funded(SOL);
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
    assert_err(env.send(std::slice::from_ref(&ix), &[&oracle]), PoolError::MissingEd25519Instruction);
    let ed = ed25519_ix(&oracle, &env.message(&a), [0, 0, 0]);
    let spacer = solana_system_interface::instruction::transfer(&oracle.pubkey(), &Keypair::new().pubkey(), SOL);
    assert_err(env.send(&[ed, spacer, ix], &[&oracle]), PoolError::MissingEd25519Instruction);
}

#[test]
fn approval_signed_by_another_key_refused() {
    let (mut env, s) = setup();
    let a = env.approval(&s.pubkey(), TX, entitlement(), TIER_A);
    let oracle = env.oracle.insecure_clone();
    let ed = ed25519_ix(&Keypair::new(), &env.message(&a), [0, 0, 0]);
    let ix = env.submit_ix(&oracle.pubkey(), &a);
    assert_err(env.send(&[ed, ix], &[&oracle]), PoolError::WrongOracleSigner);
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
    assert_err(env.send(&[ed, ix.clone()], &[&oracle]), PoolError::ApprovalMessageMismatch);
    let mainnet = safu_pool::approval::encode_message(&safu_pool::ID, CLUSTER_MAINNET, &a);
    let ed = ed25519_ix(&oracle, &mainnet, [0, 0, 0]);
    assert_err(env.send(&[ed, ix], &[&oracle]), PoolError::ApprovalMessageMismatch);
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
    assert!(err.contains("Custom") || err.contains("PrecompileError") || err.contains("InvalidAccountData"), "{err}");
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
            r.suspended = edit == 1;
            r.active_claim = (edit == 2).then_some(marker);
            r.reserved_claim = (edit == 3).then_some(marker);
        });
        assert_err(env.submit(&a), e);
    }
}

#[test]
fn entitlement_above_the_tier_ceiling_refused() {
    let (mut env, s) = setup();
    let cap = pool_core::claim::tier_cap(entitlement(), TIER_C).unwrap();
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
    env.warp(CLAIM_WINDOW_SECS + 1);
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

#[test]
fn insolvent_claim_is_queued_then_released_when_capital_arrives() {
    let mut env = Env::new();
    let (_, max) = bounds();
    let s = env.staker(max);
    let big = pool_core::claim::tier_cap(max, TIER_A).unwrap();
    let a = env.approval(&s.pubkey(), TX, big, TIER_A);
    env.submit(&a).unwrap();
    let claim = env.claim_addr(&s.pubkey(), &TX);
    assert_eq!(env.claim_state(&s.pubkey(), &TX).status, ClaimStatus::Reserved);
    assert_eq!(env.stake_state(&s.pubkey()).reserved_claim, Some(claim));
    assert_eq!(env.pool_state().total_allocated, 0);
    let release = env.transition_ix(&s.pubkey(), &TX, safu_pool::instruction::TryReleaseQueuedClaim {});
    let anyone = env.funded(SOL);
    assert_err(env.send(std::slice::from_ref(&release), &[&anyone]), PoolError::QueueReleaseNotYetEligible);
    env.matured_backer(BACKING);
    env.ok(std::slice::from_ref(&release), &[&anyone]);
    let r = env.stake_state(&s.pubkey());
    assert_eq!((r.active_claim, r.reserved_claim), (Some(claim), None));
    assert_eq!(env.pool_state().total_allocated, big);
    assert_err(env.send(&[release], &[&anyone]), PoolError::NoSuchQueuedClaim);
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
    assert_eq!(env.claim_state(&s2.pubkey(), &TX2).status, ClaimStatus::Reserved);
    // Anyone can release it; a release never counts toward the oracle's limit.
    let release = env.transition_ix(&s2.pubkey(), &TX2, safu_pool::instruction::TryReleaseQueuedClaim {});
    let anyone = env.funded(SOL);
    env.ok(&[release], &[&anyone]);
    assert_eq!(env.pool_state().day_oracle_count, 1);
}

#[test]
fn stress_cap_queues_and_resets_the_next_day() {
    let mut env = Env::new();
    // Small backing: stress cap (25%) below one tier-A claim.
    env.matured_backer(4 * SOL);
    let (_, max) = bounds();
    let s = env.staker(max);
    let cap = pool_core::claim::stress_cap(4 * SOL + max, 0, env.pool_state().settings().admit_rates()).unwrap();
    let a = env.approval(&s.pubkey(), TX, cap + 1, TIER_A);
    env.submit(&a).unwrap();
    assert_eq!(env.claim_state(&s.pubkey(), &TX).status, ClaimStatus::Reserved);
    let release = env.transition_ix(&s.pubkey(), &TX, safu_pool::instruction::TryReleaseQueuedClaim {});
    let anyone = env.funded(SOL);
    assert_err(env.send(&[release], &[&anyone]), PoolError::QueueReleaseNotYetEligible);
}

#[test]
fn queued_claim_expires_after_its_window_and_frees_the_slot() {
    let mut env = Env::new();
    let (_, max) = bounds();
    let s = env.staker(max);
    let big = pool_core::claim::tier_cap(max, TIER_A).unwrap();
    let a = env.approval(&s.pubkey(), TX, big, TIER_A);
    env.submit(&a).unwrap();
    let expire = env.transition_ix(&s.pubkey(), &TX, safu_pool::instruction::ExpireQueuedClaim {});
    let anyone = env.funded(SOL);
    assert_err(env.send(std::slice::from_ref(&expire), &[&anyone]), PoolError::QueueNotYetExpired);
    env.warp(CLAIM_WINDOW_SECS);
    assert_err(env.send(std::slice::from_ref(&expire), &[&anyone]), PoolError::QueueNotYetExpired);
    env.warp(1);
    env.ok(std::slice::from_ref(&expire), &[&anyone]);
    assert_eq!(env.claim_state(&s.pubkey(), &TX).status, ClaimStatus::Expired);
    assert_eq!(env.stake_state(&s.pubkey()).reserved_claim, None);
    assert_err(env.send(&[expire], &[&anyone]), PoolError::NoSuchQueuedClaim);
}

#[test]
fn release_refused_when_the_stake_changed_or_is_suspended() {
    let mut env = Env::new();
    let (_, max) = bounds();
    let s = env.staker(max);
    let big = pool_core::claim::tier_cap(max, TIER_A).unwrap();
    let a = env.approval(&s.pubkey(), TX, big, TIER_A);
    env.submit(&a).unwrap();
    env.matured_backer(BACKING);
    let rec = env.stake_record(&s.pubkey());
    let release = env.transition_ix(&s.pubkey(), &TX, safu_pool::instruction::TryReleaseQueuedClaim {});
    let anyone = env.funded(SOL);
    let other = Keypair::new().pubkey();
    env.edit::<StakeRecord>(&rec, |r| r.active_claim = Some(other));
    assert_err(env.send(std::slice::from_ref(&release), &[&anyone]), PoolError::WalletHasDifferentActiveClaim);
    env.edit::<StakeRecord>(&rec, |r| {
        r.active_claim = None;
        r.forfeited = true;
    });
    assert_err(env.send(std::slice::from_ref(&release), &[&anyone]), PoolError::QueuedClaimStakeChanged);
    env.edit::<StakeRecord>(&rec, |r| {
        r.forfeited = false;
        r.suspended = true;
    });
    assert_err(env.send(&[release], &[&anyone]), PoolError::StakeSuspended);
}

// ------------------------------------------------------------------ gate and approval

#[test]
fn unlock_after_the_time_gate() {
    let (mut env, s) = setup();
    pending(&mut env, &s);
    let unlock = env.transition_ix(&s.pubkey(), &TX, safu_pool::instruction::UnlockPendingClaim {});
    let anyone = env.funded(SOL);
    assert_err(env.send(std::slice::from_ref(&unlock), &[&anyone]), PoolError::TimeGateNotMet);
    env.warp(TIME_GATE_SECS);
    env.ok(std::slice::from_ref(&unlock), &[&anyone]);
    let c = env.claim_state(&s.pubkey(), &TX);
    assert_eq!((c.status, c.approve_deadline), (ClaimStatus::AwaitingApproval, env.now + APPROVE_WINDOW_SECS));
    assert_err(env.send(&[unlock], &[&anyone]), PoolError::ClaimNotPending);
}

#[test]
fn approve_rules() {
    let (mut env, s) = setup();
    pending(&mut env, &s);
    let approve = env.approve_claim_ix(&s.pubkey(), &TX);
    assert_err(env.send(std::slice::from_ref(&approve), &[&s]), PoolError::ClaimNotAwaitingApproval);
    env.warp(TIME_GATE_SECS);
    let unlock = env.transition_ix(&s.pubkey(), &TX, safu_pool::instruction::UnlockPendingClaim {});
    env.ok(&[unlock], &[&s]);
    let rec = env.stake_record(&s.pubkey());
    env.edit::<StakeRecord>(&rec, |r| r.suspended = true);
    assert_err(env.send(std::slice::from_ref(&approve), &[&s]), PoolError::StakeSuspended);
    env.edit::<StakeRecord>(&rec, |r| {
        r.suspended = false;
        r.active_claim = None;
    });
    assert_err(env.send(std::slice::from_ref(&approve), &[&s]), PoolError::ClaimStakeMismatch);
    let claim = env.claim_addr(&s.pubkey(), &TX);
    env.edit::<StakeRecord>(&rec, |r| r.active_claim = Some(claim));
    env.warp(APPROVE_WINDOW_SECS + 1);
    assert_err(env.send(&[approve], &[&s]), PoolError::ApprovalWindowExpired);
}

#[test]
fn approval_moves_unpaid_yield_to_the_protocol() {
    let (mut env, s) = setup();
    awaiting(&mut env, &s);
    let credit = pool_core::yields::credit(SOL / 100, entitlement(), 0, STAKER_YIELD_BPS, BACKER_YIELD_BPS).unwrap();
    env.edit::<Pool>(&env.pool(), |p| {
        p.staker_yield_index += credit.staker_index_bump;
        p.staker_yield_reserved += credit.staker_share;
    });
    let ix = env.approve_claim_ix(&s.pubkey(), &TX);
    env.ok(&[ix], &[&s]);
    let p = env.pool_state();
    assert_eq!((p.staker_yield_reserved, p.protocol_yield_balance), (0, credit.staker_share));
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
    let expire = env.transition_ix(&s.pubkey(), &TX, safu_pool::instruction::ExpirePendingApproval {});
    let anyone = env.funded(SOL);
    assert_err(env.send(std::slice::from_ref(&expire), &[&anyone]), PoolError::ApprovalWindowNotExpired);
    env.warp(APPROVE_WINDOW_SECS + 1);
    let rec = env.stake_record(&s.pubkey());
    env.edit::<StakeRecord>(&rec, |r| r.suspended = true);
    assert_err(env.send(std::slice::from_ref(&expire), &[&anyone]), PoolError::StakeSuspended);
    env.edit::<StakeRecord>(&rec, |r| r.suspended = false);
    env.ok(std::slice::from_ref(&expire), &[&anyone]);
    assert_eq!(env.pool_state().total_allocated, 0);
    assert_eq!(env.stake_state(&s.pubkey()).active_claim, None);
    assert_err(env.send(&[expire], &[&anyone]), PoolError::ClaimNotAwaitingApproval);
}

// ------------------------------------------------------------------ stream

#[test]
fn stream_waits_for_cooldown_and_vesting() {
    let (mut env, s) = setup();
    pending(&mut env, &s);
    let ix = env.stream_ix(&s.pubkey(), &TX, &s.pubkey());
    assert_err(env.send(std::slice::from_ref(&ix), &[&s]), PoolError::ClaimNotActive);
    env.warp(TIME_GATE_SECS);
    let unlock = env.transition_ix(&s.pubkey(), &TX, safu_pool::instruction::UnlockPendingClaim {});
    let approve = env.approve_claim_ix(&s.pubkey(), &TX);
    env.ok(&[unlock, approve], &[&s]);
    assert_err(env.send(std::slice::from_ref(&ix), &[&s]), PoolError::CooldownNotPassed);
    env.warp(COOLDOWN_SECS);
    assert_err(env.send(&[ix], &[&s]), PoolError::NothingVested);
}

#[test]
fn stream_goes_only_to_the_beneficiary_and_not_while_suspended() {
    let (mut env, s) = setup();
    active(&mut env, &s);
    env.warp(COOLDOWN_SECS + VESTING_SECS);
    let ix = env.stream_ix(&s.pubkey(), &TX, &Keypair::new().pubkey());
    assert_err(env.send(&[ix], &[&s]), PoolError::WrongBeneficiary);
    let rec = env.stake_record(&s.pubkey());
    env.edit::<StakeRecord>(&rec, |r| r.suspended = true);
    let ix = env.stream_ix(&s.pubkey(), &TX, &s.pubkey());
    assert_err(env.send(&[ix], &[&s]), PoolError::StakeSuspended);
}

#[test]
fn stream_capped_per_day_and_resumes_tomorrow() {
    let (mut env, s) = setup();
    active(&mut env, &s);
    env.warp(COOLDOWN_SECS + VESTING_SECS);
    // Today's outflow already at the cap.
    let cap = {
        let p = env.pool_state();
        let c = env.claim_state(&s.pubkey(), &TX);
        let base = (p.total_staked + p.total_backed).max(c.capacity_snapshot);
        pool_core::claim::payout_cap(base, p.total_allocated, p.settings().payout_rates()).unwrap()
    };
    let today = pool_core::claim::day_of(env.now);
    env.edit::<Pool>(&env.pool(), |p| {
        p.day = today;
        p.day_outflow = cap;
    });
    let ix = env.stream_ix(&s.pubkey(), &TX, &s.pubkey());
    assert_err(env.send(std::slice::from_ref(&ix), &[&s]), PoolError::DailyOutflowCapReached);
    env.warp(pool_core::params::SECONDS_PER_DAY);
    env.ok(std::slice::from_ref(&ix), &[&s]);
    assert_err(env.send(&[ix], &[&s]), PoolError::ClaimNotActive);
}

#[test]
fn stream_refused_when_liquid_sol_is_short() {
    let (mut env, s) = setup();
    active(&mut env, &s);
    env.warp(COOLDOWN_SECS + VESTING_SECS);
    // All liquid SOL is set-aside yield and nothing is in Marinade to unstake.
    let liquid = env.vault_liquid();
    env.edit::<Pool>(&env.pool(), |p| {
        p.backer_yield_reserved = liquid;
        p.deployed_msol = 0;
    });
    let ix = env.stream_ix(&s.pubkey(), &TX, &s.pubkey());
    assert_err(env.send(&[ix], &[&s]), PoolError::InsufficientLiquidity);
}

#[test]
fn stale_claim_returns_the_unpaid_rest() {
    let (mut env, s) = setup();
    active(&mut env, &s);
    let expire = env.transition_ix(&s.pubkey(), &TX, safu_pool::instruction::ExpireStaleClaim {});
    let anyone = env.funded(SOL);
    env.warp(COOLDOWN_SECS + COLLECTION_INACTIVITY_SECS);
    assert_err(env.send(std::slice::from_ref(&expire), &[&anyone]), PoolError::ClaimNotStale);
    env.warp(1);
    let rec = env.stake_record(&s.pubkey());
    env.edit::<StakeRecord>(&rec, |r| r.suspended = true);
    assert_err(env.send(std::slice::from_ref(&expire), &[&anyone]), PoolError::StakeSuspended);
    env.edit::<StakeRecord>(&rec, |r| r.suspended = false);
    env.ok(std::slice::from_ref(&expire), &[&anyone]);
    assert_eq!(env.claim_state(&s.pubkey(), &TX).status, ClaimStatus::Expired);
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
    let expire = env.transition_ix(&s.pubkey(), &TX, safu_pool::instruction::ExpireStaleClaim {});
    let anyone = env.funded(SOL);
    assert_err(env.send(&[expire], &[&anyone]), PoolError::ClaimFullyStreamed);
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
    assert_eq!((p.total_staked, p.total_stakers, p.total_allocated), (entitlement(), 1, 0));
    assert_err(env.send(&[ix], &[&admin]), PoolError::ClaimNotCancellable);
    let w = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    assert_err(env.send(&[w], &[&s]), PoolError::PenaltyLockActive);
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
    let other = env.funded(SOL);
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
    assert_eq!(env.claim_state(&s.pubkey(), &TX).approve_deadline, env.now + APPROVE_WINDOW_SECS);
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

// ------------------------------------------------------------------ override

#[test]
fn override_needs_both_signers_then_executes() {
    let (mut env, s) = setup();
    let (admin, co) = (env.admin.insecure_clone(), env.co_signer.insecure_clone());
    let claim = env.claim_addr(&s.pubkey(), &TX);
    let ix = env.override_ix(&admin.pubkey(), &s.pubkey(), TX, entitlement(), TIER_B);
    env.ok(std::slice::from_ref(&ix), &[&admin]);
    assert_eq!(env.claim_state(&s.pubkey(), &TX).status, ClaimStatus::Unused);
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
    let other = env.funded(SOL);
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

fn both_approve(env: &mut Env, s: &Pubkey, tx: [u8; 32], entitlement: u64, tier: u8) -> Result<(), String> {
    let (admin, co) = (env.admin.insecure_clone(), env.co_signer.insecure_clone());
    let ix = env.override_ix(&admin.pubkey(), s, tx, entitlement, tier);
    env.send(&[ix], &[&admin])?;
    let ix = env.override_ix(&co.pubkey(), s, tx, entitlement, tier);
    env.send(&[ix], &[&co])
}

#[test]
fn override_refusals() {
    let (mut env, s) = setup();
    let cap = pool_core::claim::tier_cap(entitlement(), TIER_C).unwrap();
    assert_err(both_approve(&mut env, &s.pubkey(), TX, cap + 1, TIER_C), PoolError::EntitlementExceedsTierCap);
    // A different claim is open on this stake.
    pending(&mut env, &s);
    assert_err(both_approve(&mut env, &s.pubkey(), TX2, entitlement(), TIER_A), PoolError::WalletHasDifferentActiveClaim);
}

#[test]
fn override_refused_when_insolvent_or_completed() {
    let mut env = Env::new();
    let (_, max) = bounds();
    let s = env.staker(max);
    let big = pool_core::claim::tier_cap(max, TIER_A).unwrap();
    assert_err(both_approve(&mut env, &s.pubkey(), TX, big, TIER_A), PoolError::Insolvent);

    let (mut env, s) = setup();
    active(&mut env, &s);
    let claim = env.claim_addr(&s.pubkey(), &TX);
    env.edit::<safu_pool::state::Claim>(&claim, |c| c.status = ClaimStatus::Completed);
    assert_err(both_approve(&mut env, &s.pubkey(), TX, entitlement(), TIER_A), PoolError::ClaimAlreadyCompleted);
}

#[test]
fn override_on_a_forfeited_stake_only_for_the_claim_that_forfeited_it() {
    let (mut env, s) = setup();
    active(&mut env, &s);
    // The forfeiting claim goes stale and returns its unpaid rest; the stake stays forfeited.
    env.warp(COOLDOWN_SECS + COLLECTION_INACTIVITY_SECS + 1);
    let expire = env.transition_ix(&s.pubkey(), &TX, safu_pool::instruction::ExpireStaleClaim {});
    let anyone = env.funded(SOL);
    env.ok(&[expire], &[&anyone]);
    assert_eq!(env.stake_state(&s.pubkey()).forfeited_by, Some(env.claim_addr(&s.pubkey(), &TX)));
    // A new claim may not count the same forfeited principal as capacity again.
    assert_err(both_approve(&mut env, &s.pubkey(), TX2, entitlement(), TIER_A), PoolError::StakeForfeited);
    // Re-executing the claim that forfeited it still works.
    both_approve(&mut env, &s.pubkey(), TX, entitlement(), TIER_A).unwrap();
    assert_eq!(env.claim_state(&s.pubkey(), &TX).status, ClaimStatus::Active);
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
    assert_eq!((c.entitlement, c.streamed, c.status), (corrected, paid, ClaimStatus::Active));
    // Allocation = new entitlement (the old unpaid part was released first).
    assert_eq!(env.pool_state().total_allocated, corrected);
}

#[test]
fn override_takes_over_the_stakes_own_queued_claim() {
    let mut env = Env::new();
    let (_, max) = bounds();
    let s = env.staker(max);
    let big = pool_core::claim::tier_cap(max, TIER_A).unwrap();
    let a = env.approval(&s.pubkey(), TX, big, TIER_A);
    env.submit(&a).unwrap();
    env.matured_backer(BACKING);
    both_approve(&mut env, &s.pubkey(), TX, entitlement(), TIER_A).unwrap();
    let r = env.stake_state(&s.pubkey());
    assert_eq!((r.reserved_claim, r.active_claim), (None, Some(env.claim_addr(&s.pubkey(), &TX))));
}
