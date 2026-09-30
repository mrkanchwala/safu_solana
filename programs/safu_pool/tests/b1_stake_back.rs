//! B1: initialize, pause, pool cap, stake / withdraw / emergency exit / beneficiary, backers,
//! covered wallets. One test per error path.

mod common;

use common::*;
use safu_pool::constants::*;
use safu_pool::errors::PoolError;
use safu_pool::state::{CoveredWallet, Pool, StakeRecord, StakerWallets};
use solana_keypair::Keypair;
use solana_signer::Signer;

// ------------------------------------------------------------------ initialize

#[test]
fn initialize_sets_roles_marinade_and_funds_the_vault() {
    let env = Env::new();
    let p = env.pool_state();
    assert_eq!(p.admin, env.admin.pubkey());
    assert_eq!(p.oracle, env.oracle.pubkey());
    assert_eq!(p.pool_cap, env.cfg.pool_cap);
    assert_eq!(p.marinade_state, env.cfg.marinade_state);
    assert_eq!(p.msol_mint, env.cfg.msol_mint);
    assert_eq!(p.pool_msol, env.pool_msol());
    assert_eq!(env.lamports(&env.vault()), env.rent_floor());
    assert_eq!(env.vault_liquid(), 0);
}

#[test]
fn initialize_only_by_upgrade_authority() {
    let mut env = Env::uninitialized();
    let intruder = env.funded(10 * SOL);
    let ix = env.init_ix(&intruder.pubkey(), env.init_args(), env.cfg.marinade_state, env.cfg.msol_mint);
    assert_err(env.send(&[ix], &[&intruder]), PoolError::NotUpgradeAuthority);
}

#[test]
fn initialize_refuses_overlapping_roles() {
    for which in 0..3 {
        let mut env = Env::uninitialized();
        let mut args = env.init_args();
        match which {
            0 => args.oracle = env.admin.pubkey(),
            1 => args.co_signer = env.admin.pubkey(),
            _ => args.co_signer = args.oracle,
        }
        let admin = env.admin.insecure_clone();
        let ix = env.init_ix(&admin.pubkey(), args, env.cfg.marinade_state, env.cfg.msol_mint);
        assert_err(env.send(&[ix], &[&admin]), PoolError::RoleCollision);
    }
}

#[test]
fn initialize_refuses_unknown_cluster_and_tiny_cap() {
    let mut env = Env::uninitialized();
    let admin = env.admin.insecure_clone();
    let mut args = env.init_args();
    args.cluster = CLUSTER_MAINNET + 1;
    let ix = env.init_ix(&admin.pubkey(), args, env.cfg.marinade_state, env.cfg.msol_mint);
    assert_err(env.send(&[ix], &[&admin]), PoolError::InvalidCluster);

    let mut args = env.init_args();
    // Smallest cap whose min stake rounds to zero.
    args.pool_cap = BPS_DENOMINATOR / MIN_STAKE_BPS - 1;
    let ix = env.init_ix(&admin.pubkey(), args, env.cfg.marinade_state, env.cfg.msol_mint);
    assert_err(env.send(&[ix], &[&admin]), PoolError::InvalidPoolCap);
}

#[test]
fn initialize_refuses_a_fake_marinade_state() {
    let mut env = Env::uninitialized();
    let admin = env.admin.insecure_clone();
    // The mSOL mint is a real account but not owned by Marinade.
    let ix = env.init_ix(&admin.pubkey(), env.init_args(), env.cfg.msol_mint, env.cfg.msol_mint);
    assert_err(env.send(&[ix], &[&admin]), PoolError::WrongMarinadeAccount);
}

#[test]
fn initialize_refuses_a_mint_that_is_not_marinades() {
    let mut env = Env::uninitialized();
    let admin = env.admin.insecure_clone();
    // A token mint of our own making.
    let fake = Keypair::new();
    let mut mint = env.svm.get_account(&env.cfg.msol_mint).unwrap();
    mint.data[4..36].copy_from_slice(fake.pubkey().as_ref());
    env.svm.set_account(fake.pubkey(), mint).unwrap();
    let ix = env.init_ix(&admin.pubkey(), env.init_args(), env.cfg.marinade_state, fake.pubkey());
    assert_err(env.send(&[ix], &[&admin]), PoolError::WrongMarinadeAccount);
}

#[test]
fn marinade_fixture_layout_matches_pool_core() {
    // The layout facts in pool_core::marinade, checked against the dumped devnet account.
    let env = Env::uninitialized();
    let d = env.svm.get_account(&env.cfg.marinade_state).unwrap().data;
    assert_eq!(d[..8], pool_core::marinade::ACCOUNT_STATE);
    assert_eq!(&d[pool_core::marinade::STATE_MSOL_MINT..][..32], env.cfg.msol_mint.as_ref());
    assert!(d.len() >= pool_core::marinade::STATE_MIN_LEN);
}

// ------------------------------------------------------------------ admin

#[test]
fn pause_blocks_stake_and_expires_on_its_own() {
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    let ix = env.admin_ix(safu_pool::instruction::Pause {});
    env.ok(&[ix], &[&admin]);
    let s = env.funded(SOL);
    let (min, _) = bounds();
    let ix = env.stake_ix(&s.pubkey(), min, s.pubkey());
    assert_err(env.send(&[ix.clone()], &[&s]), PoolError::Paused);
    env.warp(PAUSE_MAX_SECS);
    env.ok(&[ix], &[&s]);
}

#[test]
fn only_admin_pauses_and_unpause_needs_a_pause() {
    let mut env = Env::new();
    let other = env.funded(SOL);
    let ix = env.ix(safu_pool::accounts::AdminOnly { admin: other.pubkey(), pool: env.pool() }, safu_pool::instruction::Pause {});
    assert_err(env.send(&[ix], &[&other]), PoolError::NotAdmin);
    let admin = env.admin.insecure_clone();
    let ix = env.admin_ix(safu_pool::instruction::Unpause {});
    assert_err(env.send(&[ix], &[&admin]), PoolError::NotPaused);
}

#[test]
fn pool_cap_only_goes_up() {
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    let cap = env.cfg.pool_cap;
    let ix = env.admin_ix(safu_pool::instruction::SetPoolCap { pool_cap: cap });
    assert_err(env.send(&[ix], &[&admin]), PoolError::PoolCapNotIncreased);
    let ix = env.admin_ix(safu_pool::instruction::SetPoolCap { pool_cap: cap + 1 });
    env.ok(&[ix], &[&admin]);
    assert_eq!(env.pool_state().pool_cap, cap + 1);
}

// ------------------------------------------------------------------ stake

#[test]
fn stake_moves_sol_into_the_vault_and_books_it() {
    let mut env = Env::new();
    let (_, max) = bounds();
    let s = env.staker(max);
    let r = env.stake_state(&s.pubkey());
    assert_eq!((r.amount, r.beneficiary, r.staked_at, r.forfeited), (max, s.pubkey(), START, false));
    let p = env.pool_state();
    assert_eq!((p.total_staked, p.total_stakers), (max, 1));
    assert_eq!(env.vault_liquid(), max);
}

#[test]
fn stake_bounds_enforced() {
    let mut env = Env::new();
    let (min, max) = bounds();
    let s = env.funded(2 * max);
    for amount in [0, min - 1, max + 1] {
        let ix = env.stake_ix(&s.pubkey(), amount, s.pubkey());
        assert_err(env.send(&[ix], &[&s]), PoolError::StakeOutOfRange);
    }
}

#[test]
fn stake_refused_above_the_pool_cap() {
    let mut env = Env::new();
    let (_, max) = bounds();
    let full = env.cfg.pool_cap / max;
    for _ in 0..full {
        env.staker(max);
    }
    assert_eq!(env.pool_state().total_staked, full * max);
    let s = env.funded(2 * max);
    let ix = env.stake_ix(&s.pubkey(), max, s.pubkey());
    assert_err(env.send(&[ix], &[&s]), PoolError::PoolCapExceeded);
}

#[test]
fn one_live_stake_per_address() {
    let mut env = Env::new();
    let (min, _) = bounds();
    let s = env.staker(min);
    let ix = env.stake_ix(&s.pubkey(), min, s.pubkey());
    assert_err(env.send(&[ix], &[&s]), PoolError::AlreadyStaked);
}

#[test]
fn forfeited_address_can_never_stake_again() {
    let mut env = Env::new();
    let (min, _) = bounds();
    let s = env.staker(min);
    let rec = env.stake_record(&s.pubkey());
    env.edit::<StakeRecord>(&rec, |r| r.forfeited = true);
    let ix = env.stake_ix(&s.pubkey(), min, s.pubkey());
    assert_err(env.send(&[ix], &[&s]), PoolError::AddressHasApprovedClaim);
}

#[test]
fn beneficiary_cannot_be_a_role() {
    let mut env = Env::new();
    let (min, _) = bounds();
    let s = env.funded(SOL);
    for (who, e) in [
        (env.oracle.pubkey(), PoolError::BeneficiaryIsOracle),
        (env.admin.pubkey(), PoolError::BeneficiaryIsAdmin),
        (env.co_signer.pubkey(), PoolError::BeneficiaryIsCoSigner),
    ] {
        let ix = env.stake_ix(&s.pubkey(), min, who);
        assert_err(env.send(&[ix], &[&s]), e);
        let st = env.staker(min);
        let ix = env.set_beneficiary_ix(&st.pubkey(), who);
        assert_err(env.send(&[ix], &[&st]), e);
    }
}

#[test]
fn set_beneficiary_blocked_while_a_claim_is_open_or_queued() {
    let mut env = Env::new();
    let (min, _) = bounds();
    let s = env.staker(min);
    let rec = env.stake_record(&s.pubkey());
    let new = Keypair::new().pubkey();
    let marker = Keypair::new().pubkey();
    env.edit::<StakeRecord>(&rec, |r| r.active_claim = Some(marker));
    let ix = env.set_beneficiary_ix(&s.pubkey(), new);
    assert_err(env.send(&[ix.clone()], &[&s]), PoolError::ClaimActive);
    env.edit::<StakeRecord>(&rec, |r| {
        r.active_claim = None;
        r.reserved_claim = Some(marker);
    });
    assert_err(env.send(&[ix.clone()], &[&s]), PoolError::ClaimQueuedForStake);
    env.edit::<StakeRecord>(&rec, |r| r.reserved_claim = None);
    env.ok(&[ix], &[&s]);
    assert_eq!(env.stake_state(&s.pubkey()).beneficiary, new);
}

// ------------------------------------------------------------------ withdraw

#[test]
fn withdraw_pays_the_beneficiary_and_closes_the_record() {
    let mut env = Env::new();
    let (_, max) = bounds();
    // Enough for two stakes: the first withdrawal goes to the beneficiary, not back here.
    let s = env.funded(2 * max + SOL);
    let ben = Keypair::new().pubkey();
    let ix = env.stake_ix(&s.pubkey(), max, ben);
    env.ok(&[ix], &[&s]);
    let rec = env.stake_record(&s.pubkey());
    let rent = env.lamports(&rec);
    let before = env.lamports(&s.pubkey());
    let ix = env.withdraw_ix(&s.pubkey(), &ben);
    env.ok(&[ix], &[&s]);
    assert_eq!(env.lamports(&ben), max);
    assert!(!env.exists(&rec));
    // Rent comes back to the staker (the fee is paid by the staker too).
    assert!(env.lamports(&s.pubkey()) > before + rent - SOL / 1_000);
    let p = env.pool_state();
    assert_eq!((p.total_staked, p.total_stakers), (0, 0));
    assert_eq!(env.vault_liquid(), 0);
    // The address can stake again: the record is re-created.
    let ix = env.stake_ix(&s.pubkey(), max, s.pubkey());
    env.ok(&[ix], &[&s]);
}

#[test]
fn withdraw_to_the_wrong_beneficiary_refused() {
    let mut env = Env::new();
    let (min, _) = bounds();
    let s = env.staker(min);
    let ix = env.withdraw_ix(&s.pubkey(), &Keypair::new().pubkey());
    assert_err(env.send(&[ix], &[&s]), PoolError::WrongBeneficiary);
}

#[test]
fn withdraw_refused_while_paused() {
    let mut env = Env::new();
    let (min, _) = bounds();
    let s = env.staker(min);
    let admin = env.admin.insecure_clone();
    let ix = env.admin_ix(safu_pool::instruction::Pause {});
    env.ok(&[ix], &[&admin]);
    let ix = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    assert_err(env.send(&[ix], &[&s]), PoolError::Paused);
}

#[test]
fn withdraw_refused_when_forfeited_claimed_queued_or_penalty_locked() {
    let mut env = Env::new();
    let (min, _) = bounds();
    let s = env.staker(min);
    let rec = env.stake_record(&s.pubkey());
    let marker = Keypair::new().pubkey();
    let ix = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    for (edit, e) in [
        (0, PoolError::StakeForfeited),
        (1, PoolError::ClaimActive),
        (2, PoolError::ClaimQueuedForStake),
        (3, PoolError::PenaltyLockActive),
    ] {
        let now = env.now;
        env.edit::<StakeRecord>(&rec, |r| {
            *r = StakeRecord { forfeited: false, active_claim: None, reserved_claim: None, penalty_locked_until: 0, ..r.clone() };
            match edit {
                0 => r.forfeited = true,
                1 => r.active_claim = Some(marker),
                2 => r.reserved_claim = Some(marker),
                _ => r.penalty_locked_until = now + PENALTY_LOCK_SECS,
            }
        });
        assert_err(env.send(&[ix.clone()], &[&s]), e);
    }
    env.warp(PENALTY_LOCK_SECS);
    env.ok(&[ix], &[&s]);
}

#[test]
fn withdraw_refused_when_liquid_sol_is_short() {
    let mut env = Env::new();
    let (min, _) = bounds();
    let s = env.staker(min);
    // Set-aside yield is not the stake's to take.
    env.edit::<Pool>(&env.pool(), |p| p.staker_yield_reserved = 1);
    let ix = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    assert_err(env.send(&[ix], &[&s]), PoolError::InsufficientLiquidity);
}

#[test]
fn withdraw_pays_unpaid_yield_from_the_set_aside() {
    let mut env = Env::new();
    let (_, max) = bounds();
    let s = env.staker(max);
    // One credit of 1% on the stake: index + set-aside + the SOL behind it.
    let credit = pool_core::yields::credit(max / 100, max, 0).unwrap();
    env.edit::<Pool>(&env.pool(), |p| {
        p.staker_yield_index += credit.staker_index_bump;
        p.staker_yield_reserved += credit.staker_share;
    });
    let v = env.vault();
    env.svm.airdrop(&v, credit.staker_share).unwrap();
    let ix = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    let before = env.lamports(&s.pubkey());
    env.ok(&[ix], &[&s]);
    assert!(env.lamports(&s.pubkey()) >= before + max + credit.staker_share);
    assert_eq!(env.pool_state().staker_yield_reserved, 0);
    assert_eq!(env.vault_liquid(), 0);
}

#[test]
fn emergency_exit_only_while_paused_and_honours_the_penalty_lock() {
    let mut env = Env::new();
    let (min, _) = bounds();
    let s = env.staker(min);
    let ix = env.emergency_exit_ix(&s.pubkey());
    assert_err(env.send(&[ix.clone()], &[&s]), PoolError::NotPaused);
    let admin = env.admin.insecure_clone();
    let p = env.admin_ix(safu_pool::instruction::Pause {});
    env.ok(&[p], &[&admin]);
    let rec = env.stake_record(&s.pubkey());
    let until = env.now + PENALTY_LOCK_SECS;
    env.edit::<StakeRecord>(&rec, |r| r.penalty_locked_until = until);
    assert_err(env.send(&[ix.clone()], &[&s]), PoolError::PenaltyLockActive);
    env.edit::<StakeRecord>(&rec, |r| r.penalty_locked_until = 0);
    let before = env.lamports(&s.pubkey());
    env.ok(&[ix], &[&s]);
    assert!(env.lamports(&s.pubkey()) > before + min);
    assert!(!env.exists(&rec));
}

// ------------------------------------------------------------------ backers

#[test]
fn backing_counts_only_after_maturity() {
    let mut env = Env::new();
    let b = env.funded(10 * SOL);
    let ix = env.back_ix(&b.pubkey(), 2 * SOL);
    env.ok(&[ix], &[&b]);
    let p = env.pool_state();
    assert_eq!((p.total_backed, p.total_backed_pending), (0, 2 * SOL));
    let m = env.mature_ix(&b.pubkey());
    assert_err(env.send(&[m.clone()], &[&b]), PoolError::BackingNotMature);
    env.warp(BACKER_MATURITY_SECS);
    // Permissionless: anyone can mature it.
    let anyone = env.funded(SOL);
    env.ok(&[m.clone()], &[&anyone]);
    let p = env.pool_state();
    assert_eq!((p.total_backed, p.total_backed_pending), (2 * SOL, 0));
    assert_err(env.send(&[m], &[&anyone]), PoolError::NoPendingBacking);
}

#[test]
fn top_up_restarts_the_wait_for_the_whole_pending_amount() {
    let mut env = Env::new();
    let b = env.funded(10 * SOL);
    let ix = env.back_ix(&b.pubkey(), SOL);
    env.ok(&[ix.clone()], &[&b]);
    env.warp(BACKER_MATURITY_SECS - 1);
    env.ok(&[ix], &[&b]);
    env.warp(1);
    let m = env.mature_ix(&b.pubkey());
    assert_err(env.send(&[m], &[&b]), PoolError::BackingNotMature);
    assert_eq!(env.backer_state(&b.pubkey()).pending_amount, 2 * SOL);
}

#[test]
fn back_refuses_zero_and_pause() {
    let mut env = Env::new();
    let b = env.funded(10 * SOL);
    let ix = env.back_ix(&b.pubkey(), 0);
    assert_err(env.send(&[ix], &[&b]), PoolError::AmountNotPositive);
    let admin = env.admin.insecure_clone();
    let p = env.admin_ix(safu_pool::instruction::Pause {});
    env.ok(&[p], &[&admin]);
    let ix = env.back_ix(&b.pubkey(), SOL);
    assert_err(env.send(&[ix], &[&b]), PoolError::Paused);
}

fn matured_backer(env: &mut Env, amount: u64) -> Keypair {
    let b = env.funded(amount + SOL);
    let ix = env.back_ix(&b.pubkey(), amount);
    env.ok(&[ix], &[&b]);
    env.warp(BACKER_MATURITY_SECS);
    let m = env.mature_ix(&b.pubkey());
    env.ok(&[m], &[&b]);
    b
}

#[test]
fn backer_withdrawal_request_rules() {
    let mut env = Env::new();
    let b = matured_backer(&mut env, 2 * SOL);
    let req = |env: &Env, a| env.backer_only_ix(&b.pubkey(), safu_pool::instruction::RequestBackerWithdrawal { amount: a });
    assert_err(env.send(&[req(&env, 0)], &[&b]), PoolError::AmountNotPositive);
    assert_err(env.send(&[req(&env, 2 * SOL + 1)], &[&b]), PoolError::BackerAmountExceedsBalance);
    let cancel = env.backer_only_ix(&b.pubkey(), safu_pool::instruction::CancelBackerWithdrawal {});
    assert_err(env.send(&[cancel.clone()], &[&b]), PoolError::NoBackerWithdrawal);
    env.ok(&[req(&env, SOL)], &[&b]);
    assert_err(env.send(&[req(&env, SOL)], &[&b]), PoolError::BackerWithdrawalPending);
    // Still counted during the notice (rule 2).
    assert_eq!(env.pool_state().total_backed, 2 * SOL);
    env.ok(&[cancel], &[&b]);
    assert_eq!(env.backer_state(&b.pubkey()).withdraw_amount, 0);
}

#[test]
fn backer_withdrawal_waits_for_notice_and_free_capital() {
    let mut env = Env::new();
    let b = matured_backer(&mut env, 2 * SOL);
    let complete = env.complete_backer_ix(&b.pubkey());
    assert_err(env.send(&[complete.clone()], &[&b]), PoolError::NoBackerWithdrawal);
    let req = env.backer_only_ix(&b.pubkey(), safu_pool::instruction::RequestBackerWithdrawal { amount: SOL });
    env.ok(&[req], &[&b]);
    assert_err(env.send(&[complete.clone()], &[&b]), PoolError::BackerNoticeNotPassed);
    env.warp(BACKER_NOTICE_SECS);
    // Open claims need more than what would be left (rule 3).
    env.edit::<Pool>(&env.pool(), |p| p.total_allocated = SOL + 1);
    assert_err(env.send(&[complete.clone()], &[&b]), PoolError::BackerCapitalNotFree);
    env.edit::<Pool>(&env.pool(), |p| p.total_allocated = SOL);
    let before = env.lamports(&b.pubkey());
    env.ok(&[complete], &[&b]);
    assert!(env.lamports(&b.pubkey()) > before + SOL - SOL / 1_000);
    assert_eq!(env.pool_state().total_backed, SOL);
    assert_eq!(env.backer_state(&b.pubkey()).amount, SOL);
}

#[test]
fn backer_withdrawal_works_while_paused() {
    let mut env = Env::new();
    let b = matured_backer(&mut env, SOL);
    let admin = env.admin.insecure_clone();
    let p = env.admin_ix(safu_pool::instruction::Pause {});
    env.ok(&[p], &[&admin]);
    let req = env.backer_only_ix(&b.pubkey(), safu_pool::instruction::RequestBackerWithdrawal { amount: SOL });
    env.ok(&[req], &[&b]);
    env.warp(BACKER_NOTICE_SECS);
    let complete = env.complete_backer_ix(&b.pubkey());
    env.ok(&[complete], &[&b]);
    assert_eq!(env.pool_state().total_backed, 0);
}

// ------------------------------------------------------------------ covered wallets

fn hash(n: u8) -> [u8; 32] {
    [n; 32]
}

#[test]
fn register_up_to_the_limit() {
    let mut env = Env::new();
    let writer = env.writer.insecure_clone();
    let staker = Keypair::new().pubkey();
    for n in 0..MAX_COVERED_WALLETS {
        let ix = env.register_ix(&writer.pubkey(), staker, hash(n));
        env.ok(&[ix], &[&writer]);
    }
    let w: StakerWallets = env.read(&env.staker_wallets(&staker));
    assert_eq!(w.count, MAX_COVERED_WALLETS);
    let c: CoveredWallet = env.read(&env.covered(&hash(0)));
    assert_eq!(c.staker, staker);
    let ix = env.register_ix(&writer.pubkey(), staker, hash(MAX_COVERED_WALLETS));
    assert_err(env.send(&[ix], &[&writer]), PoolError::StakerLimitReached);
}

#[test]
fn a_wallet_belongs_to_one_staker_forever() {
    let mut env = Env::new();
    let writer = env.writer.insecure_clone();
    let (a, b) = (Keypair::new().pubkey(), Keypair::new().pubkey());
    let ix = env.register_ix(&writer.pubkey(), a, hash(7));
    env.ok(&[ix], &[&writer]);
    let ix = env.register_ix(&writer.pubkey(), a, hash(7));
    assert_err(env.send(&[ix], &[&writer]), PoolError::AlreadyRegistered);
    let ix = env.register_ix(&writer.pubkey(), b, hash(7));
    assert_err(env.send(&[ix], &[&writer]), PoolError::WalletTakenByOtherStaker);
}

#[test]
fn only_the_registry_writer_registers() {
    let mut env = Env::new();
    let other = env.funded(SOL);
    let ix = env.register_ix(&other.pubkey(), Keypair::new().pubkey(), hash(1));
    assert_err(env.send(&[ix], &[&other]), PoolError::NotRegistryWriter);
}

#[test]
fn a_donation_to_the_vault_changes_no_stake() {
    // Eng review D2: extra SOL only ever looks like surplus.
    let mut env = Env::new();
    let (min, _) = bounds();
    env.staker(min);
    let v = env.vault();
    env.svm.airdrop(&v, SOL).unwrap();
    let p = env.pool_state();
    assert_eq!((p.total_staked, p.total_allocated), (min, 0));
}
