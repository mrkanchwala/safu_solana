//! initialize, pause, pool cap, stake / withdraw / emergency exit / beneficiary, backers,
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
fn initialize_sets_roles_usdc_and_an_empty_vault_owned_by_the_pool() {
    let env = Env::new();
    let p = env.pool_state();
    assert_eq!(p.admin, env.admin.pubkey());
    assert_eq!(p.oracle, env.oracle.pubkey());
    assert_eq!(p.settings().pool_cap(), env.cfg.pool_cap);
    assert_eq!((p.usdc_mint, p.vault), (env.usdc_mint, env.vault()));
    let v = env.svm.get_account(&env.vault()).unwrap();
    assert_eq!(v.owner, TOKEN);
    // Token account layout: mint, then owner (the authority that can move it) = the pool PDA.
    assert_eq!(&v.data[..32], env.usdc_mint.as_ref());
    assert_eq!(&v.data[32..64], env.pool().as_ref());
    assert_eq!(env.vault_liquid(), 0);
}

#[test]
fn initialize_only_by_upgrade_authority() {
    let mut env = Env::uninitialized();
    let intruder = env.funded(10 * unit());
    let ix = env.init_ix(&intruder.pubkey(), env.init_args(), env.usdc_mint);
    assert_err(
        env.send(&[ix], &[&intruder]),
        PoolError::NotUpgradeAuthority,
    );
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
        let ix = env.init_ix(&admin.pubkey(), args, env.usdc_mint);
        assert_err(env.send(&[ix], &[&admin]), PoolError::RoleCollision);
    }
}

#[test]
fn initialize_refuses_unknown_cluster_and_tiny_cap() {
    let mut env = Env::uninitialized();
    let admin = env.admin.insecure_clone();
    let mut args = env.init_args();
    args.cluster = CLUSTER_MAINNET + 1;
    let ix = env.init_ix(&admin.pubkey(), args, env.usdc_mint);
    assert_err(env.send(&[ix], &[&admin]), PoolError::InvalidCluster);

    let mut args = env.init_args();
    // Smallest cap whose min stake rounds to zero.
    args.pool_cap = BPS_DENOMINATOR / MIN_STAKE_BPS - 1;
    let ix = env.init_ix(&admin.pubkey(), args, env.usdc_mint);
    assert_err(env.send(&[ix], &[&admin]), PoolError::InvalidPoolCap);
}

#[test]
fn initialize_refuses_a_mint_with_other_decimals() {
    let mut env = Env::uninitialized();
    let admin = env.admin.insecure_clone();
    let mint = Keypair::new().pubkey();
    let decimals = pool_core::params::USDC_DECIMALS + 1;
    set_token_program_account(&mut env.svm, mint, mint_data(decimals, 0));
    let ix = env.init_ix(&admin.pubkey(), env.init_args(), mint);
    assert_err(env.send(&[ix], &[&admin]), PoolError::WrongMint);
}

#[test]
fn initialize_refuses_a_mint_not_owned_by_classic_spl_token() {
    // A Token-2022 mint (or anything else) could carry transfer fees or hooks: refused by owner.
    let mut env = Env::uninitialized();
    let admin = env.admin.insecure_clone();
    let mint = Keypair::new().pubkey();
    let data = mint_data(pool_core::params::USDC_DECIMALS, 0);
    let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
    let other_program = Keypair::new().pubkey();
    env.svm
        .set_account(mint, solana_account::Account { lamports, data, owner: other_program, executable: false, rent_epoch: 0 })
        .unwrap();
    let ix = env.init_ix(&admin.pubkey(), env.init_args(), mint);
    let err = env.send(&[ix], &[&admin]).expect_err("must fail");
    let code = anchor_lang::error::ErrorCode::AccountOwnedByWrongProgram as u32;
    assert!(err.contains(&format!("Custom({code})")), "{err}");
}

// ------------------------------------------------------------------ admin

#[test]
fn pause_blocks_stake_and_expires_on_its_own() {
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    let ix = env.admin_ix(safu_pool::instruction::Pause {});
    env.ok(&[ix], &[&admin]);
    let s = env.funded(unit());
    let (min, _) = bounds();
    let ix = env.stake_ix(&s.pubkey(), min, s.pubkey());
    assert_err(
        env.send(std::slice::from_ref(&ix), &[&s]),
        PoolError::Paused,
    );
    env.warp(PAUSE_MAX_SECS);
    env.ok(&[ix], &[&s]);
}

#[test]
fn only_admin_pauses_and_unpause_needs_a_pause() {
    let mut env = Env::new();
    let other = env.funded(unit());
    let ix = env.ix(
        safu_pool::accounts::AdminOnly {
            admin: other.pubkey(),
            pool: env.pool(),
        },
        safu_pool::instruction::Pause {},
    );
    assert_err(env.send(&[ix], &[&other]), PoolError::NotAdmin);
    let admin = env.admin.insecure_clone();
    let ix = env.admin_ix(safu_pool::instruction::Unpause {});
    assert_err(env.send(&[ix], &[&admin]), PoolError::NotPaused);
}

#[test]
fn pool_cap_changes_through_the_settings_timelock() {
    let mut env = Env::new();
    let cap = env.cfg.pool_cap;
    env.set_setting(SettingKey::PoolCap, (cap * 2) as i64);
    assert_eq!(env.pool_state().settings().pool_cap(), cap * 2);
}

// ------------------------------------------------------------------ stake

#[test]
fn stake_moves_usdc_into_the_vault_and_books_it() {
    let mut env = Env::new();
    let (_, max) = bounds();
    let s = env.staker(max);
    let r = env.stake_state(&s.pubkey());
    assert_eq!(
        (r.amount, r.beneficiary, r.staked_at, r.forfeited),
        (max, s.pubkey(), START, false)
    );
    let p = env.pool_state();
    assert_eq!((p.total_staked, p.total_stakers), (max, 1));
    assert_eq!((env.vault_liquid(), env.usdc(&s.pubkey())), (max, 0));
}

#[test]
fn stake_refuses_someone_elses_usdc_or_another_mint() {
    let mut env = Env::new();
    let (min, _) = bounds();
    let s = env.funded(min);
    let other = env.funded(min);
    // Another wallet's USDC account as the source.
    let mut ix = env.stake_ix(&s.pubkey(), min, s.pubkey());
    let src = ix.accounts.iter().position(|a| a.pubkey == env.ata(&s.pubkey())).unwrap();
    ix.accounts[src].pubkey = env.ata(&other.pubkey());
    assert_err(env.send(&[ix], &[&s]), PoolError::WrongTokenAccount);
    // A token account of another mint, owned by the staker.
    let fake_mint = Keypair::new().pubkey();
    set_token_program_account(&mut env.svm, fake_mint, mint_data(pool_core::params::USDC_DECIMALS, 0));
    let fake = Keypair::new().pubkey();
    set_token_program_account(&mut env.svm, fake, token_data(&fake_mint, &s.pubkey(), min));
    let mut ix = env.stake_ix(&s.pubkey(), min, s.pubkey());
    ix.accounts[src].pubkey = fake;
    assert_err(env.send(&[ix], &[&s]), PoolError::WrongMint);
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
    let s = env.funded(unit());
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
    assert_err(
        env.send(std::slice::from_ref(&ix), &[&s]),
        PoolError::ClaimActive,
    );
    env.edit::<StakeRecord>(&rec, |r| {
        r.active_claim = None;
        r.reserved_claim = Some(marker);
    });
    assert_err(
        env.send(std::slice::from_ref(&ix), &[&s]),
        PoolError::ClaimQueuedForStake,
    );
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
    let s = env.funded(2 * max);
    let ben = Keypair::new().pubkey();
    env.open_usdc(&ben);
    let ix = env.stake_ix(&s.pubkey(), max, ben);
    env.ok(&[ix], &[&s]);
    let rec = env.stake_record(&s.pubkey());
    let rent = env.lamports(&rec);
    let before = env.lamports(&s.pubkey());
    let ix = env.withdraw_ix(&s.pubkey(), &ben);
    env.ok(&[ix], &[&s]);
    assert_eq!(env.usdc(&ben), max);
    assert!(!env.exists(&rec));
    // Rent (SOL) comes back to the staker, less this transaction's fee.
    assert!(env.lamports(&s.pubkey()) > before + rent - SOL / 1_000);
    let p = env.pool_state();
    assert_eq!((p.total_staked, p.total_stakers, env.vault_liquid()), (0, 0, 0));
    // The address can stake again: the record is re-created.
    let ix = env.stake_ix(&s.pubkey(), max, s.pubkey());
    env.ok(&[ix], &[&s]);
}

#[test]
fn withdraw_to_the_wrong_beneficiary_refused() {
    let mut env = Env::new();
    let (min, _) = bounds();
    let s = env.staker(min);
    let other = Keypair::new().pubkey();
    env.open_usdc(&other);
    let ix = env.withdraw_ix(&s.pubkey(), &other);
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
            *r = StakeRecord {
                forfeited: false,
                forfeited_by: None,
                active_claim: None,
                reserved_claim: None,
                penalty_locked_until: 0,
                ..r.clone()
            };
            match edit {
                0 => r.forfeited = true,
                1 => r.active_claim = Some(marker),
                2 => r.reserved_claim = Some(marker),
                _ => r.penalty_locked_until = now + PENALTY_LOCK_SECS,
            }
        });
        assert_err(env.send(std::slice::from_ref(&ix), &[&s]), e);
    }
    env.warp(PENALTY_LOCK_SECS);
    env.ok(&[ix], &[&s]);
}

#[test]
fn withdraw_never_spends_the_yield_set_aside() {
    let mut env = Env::new();
    let (min, _) = bounds();
    let s = env.staker(min);
    // The vault holds exactly the stake; set-aside yield is not the stake's to take.
    env.edit::<Pool>(&env.pool(), |p| p.staker_yield_reserved = 1);
    let ix = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    assert_err(env.send(&[ix], &[&s]), PoolError::InsufficientLiquidity);
}

#[test]
fn withdraw_pays_unpaid_yield_from_the_set_aside() {
    let mut env = Env::new();
    let (_, max) = bounds();
    let s = env.staker(max);
    // One credit of 1% on the stake: index + set-aside + the USDC behind it.
    let credit =
        pool_core::yields::credit(max / 100, max, 0, STAKER_YIELD_BPS, BACKER_YIELD_BPS).unwrap();
    env.edit::<Pool>(&env.pool(), |p| {
        p.staker_yield_index += credit.staker_index_bump;
        p.staker_yield_reserved += credit.staker_share;
    });
    let v = env.vault();
    env.add_token(&v, credit.staker_share);
    let ix = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    let before = env.usdc(&s.pubkey());
    env.ok(&[ix], &[&s]);
    assert_eq!(env.usdc(&s.pubkey()), before + max + credit.staker_share);
    assert_eq!(env.pool_state().staker_yield_reserved, 0);
}

#[test]
fn emergency_exit_only_while_paused_and_honours_the_penalty_lock() {
    let mut env = Env::new();
    let (min, _) = bounds();
    let s = env.staker(min);
    let ix = env.emergency_exit_ix(&s.pubkey());
    assert_err(
        env.send(std::slice::from_ref(&ix), &[&s]),
        PoolError::NotPaused,
    );
    let admin = env.admin.insecure_clone();
    let p = env.admin_ix(safu_pool::instruction::Pause {});
    env.ok(&[p], &[&admin]);
    let rec = env.stake_record(&s.pubkey());
    let until = env.now + PENALTY_LOCK_SECS;
    env.edit::<StakeRecord>(&rec, |r| r.penalty_locked_until = until);
    assert_err(
        env.send(std::slice::from_ref(&ix), &[&s]),
        PoolError::PenaltyLockActive,
    );
    env.edit::<StakeRecord>(&rec, |r| r.penalty_locked_until = 0);
    let before = env.usdc(&s.pubkey());
    env.ok(&[ix], &[&s]);
    assert_eq!(env.usdc(&s.pubkey()), before + min);
    assert!(!env.exists(&rec));
}

// ------------------------------------------------------------------ backers

#[test]
fn backing_counts_only_after_maturity() {
    let mut env = Env::new();
    let b = env.funded(10 * unit());
    let ix = env.back_ix(&b.pubkey(), 2 * unit());
    env.ok(&[ix], &[&b]);
    let p = env.pool_state();
    assert_eq!((p.total_backed, p.total_backed_pending), (0, 2 * unit()));
    let m = env.mature_ix(&b.pubkey());
    assert_err(
        env.send(std::slice::from_ref(&m), &[&b]),
        PoolError::BackingNotMature,
    );
    env.warp(BACKER_MATURITY_SECS);
    // Permissionless: anyone can mature it.
    let anyone = env.funded(unit());
    env.ok(std::slice::from_ref(&m), &[&anyone]);
    let p = env.pool_state();
    assert_eq!((p.total_backed, p.total_backed_pending), (2 * unit(), 0));
    assert_err(env.send(&[m], &[&anyone]), PoolError::NoPendingBacking);
}

#[test]
fn top_up_restarts_the_wait_for_the_whole_pending_amount() {
    let mut env = Env::new();
    let b = env.funded(10 * unit());
    let ix = env.back_ix(&b.pubkey(), unit());
    env.ok(std::slice::from_ref(&ix), &[&b]);
    env.warp(BACKER_MATURITY_SECS - 1);
    env.ok(&[ix], &[&b]);
    env.warp(1);
    let m = env.mature_ix(&b.pubkey());
    assert_err(env.send(&[m], &[&b]), PoolError::BackingNotMature);
    assert_eq!(env.backer_state(&b.pubkey()).pending_amount, 2 * unit());
}

#[test]
fn back_refuses_zero_and_pause() {
    let mut env = Env::new();
    let b = env.funded(10 * unit());
    let ix = env.back_ix(&b.pubkey(), 0);
    assert_err(env.send(&[ix], &[&b]), PoolError::AmountNotPositive);
    let admin = env.admin.insecure_clone();
    let p = env.admin_ix(safu_pool::instruction::Pause {});
    env.ok(&[p], &[&admin]);
    let ix = env.back_ix(&b.pubkey(), unit());
    assert_err(env.send(&[ix], &[&b]), PoolError::Paused);
}

#[test]
fn backer_withdrawal_request_rules() {
    let mut env = Env::new();
    let b = env.matured_backer(2 * unit());
    let req = |env: &Env, a| env.request_backer_ix(&b.pubkey(), a);
    assert_err(
        env.send(&[req(&env, 0)], &[&b]),
        PoolError::AmountNotPositive,
    );
    assert_err(
        env.send(&[req(&env, 2 * unit() + 1)], &[&b]),
        PoolError::BackerAmountExceedsBalance,
    );
    let cancel = env.backer_only_ix(
        &b.pubkey(),
        safu_pool::instruction::CancelBackerWithdrawal {},
    );
    assert_err(
        env.send(std::slice::from_ref(&cancel), &[&b]),
        PoolError::NoBackerWithdrawal,
    );
    env.ok(&[req(&env, unit())], &[&b]);
    assert_err(
        env.send(&[req(&env, unit())], &[&b]),
        PoolError::BackerWithdrawalPending,
    );
    // Still counted during the notice (rule 2).
    assert_eq!(env.pool_state().total_backed, 2 * unit());
    env.ok(&[cancel], &[&b]);
    assert_eq!(env.backer_state(&b.pubkey()).withdraw_amount, 0);
}

#[test]
fn backer_withdrawal_waits_for_notice_and_free_capital() {
    let mut env = Env::new();
    let b = env.matured_backer(2 * unit());
    let complete = env.complete_backer_ix(&b.pubkey());
    assert_err(
        env.send(std::slice::from_ref(&complete), &[&b]),
        PoolError::NoBackerWithdrawal,
    );
    let req = env.request_backer_ix(&b.pubkey(), unit());
    env.ok(&[req], &[&b]);
    assert_err(
        env.send(std::slice::from_ref(&complete), &[&b]),
        PoolError::BackerNoticeNotPassed,
    );
    env.warp(BACKER_NOTICE_SECS);
    // Open claims need more than what would be left (rule 3).
    env.edit::<Pool>(&env.pool(), |p| p.total_allocated = unit() + 1);
    assert_err(
        env.send(std::slice::from_ref(&complete), &[&b]),
        PoolError::CapitalNotFree,
    );
    env.edit::<Pool>(&env.pool(), |p| p.total_allocated = unit());
    let before = env.usdc(&b.pubkey());
    env.ok(&[complete], &[&b]);
    assert_eq!(env.usdc(&b.pubkey()), before + unit());
    assert_eq!(env.pool_state().total_backed, unit());
    assert_eq!(env.backer_state(&b.pubkey()).amount, unit());
}

#[test]
fn backer_withdrawal_works_while_paused() {
    let mut env = Env::new();
    let b = env.matured_backer(unit());
    let admin = env.admin.insecure_clone();
    let p = env.admin_ix(safu_pool::instruction::Pause {});
    env.ok(&[p], &[&admin]);
    let req = env.request_backer_ix(&b.pubkey(), unit());
    env.ok(&[req], &[&b]);
    env.warp(BACKER_NOTICE_SECS);
    let complete = env.complete_backer_ix(&b.pubkey());
    env.ok(&[complete], &[&b]);
    assert_eq!(env.pool_state().total_backed, 0);
}

#[test]
fn anyone_can_count_matured_backing() {
    let mut env = Env::new();
    let b = env.funded(10 * unit());
    let crank = env.funded(unit());
    let back = env.back_ix(&b.pubkey(), unit());
    env.ok(&[back], &[&b]);
    let mature = env.mature_ix(&b.pubkey());
    assert_err(
        env.send(std::slice::from_ref(&mature), &[&crank]),
        PoolError::BackingNotMature,
    );
    env.warp(BACKER_MATURITY_SECS);
    env.ok(&[mature], &[&crank]);
    assert_eq!(env.backer_state(&b.pubkey()).amount, unit());
    assert_eq!(env.pool_state().total_backed, unit());
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
    assert_eq!(
        w.hashes().to_vec(),
        (0..MAX_COVERED_WALLETS).map(hash).collect::<Vec<_>>()
    );
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
    assert_err(
        env.send(&[ix], &[&writer]),
        PoolError::WalletTakenByOtherStaker,
    );
}

#[test]
fn only_the_registry_writer_registers() {
    let mut env = Env::new();
    let other = env.funded(unit());
    let ix = env.register_ix(&other.pubkey(), Keypair::new().pubkey(), hash(1));
    assert_err(env.send(&[ix], &[&other]), PoolError::NotRegistryWriter);
}

#[test]
fn a_donation_to_the_vault_changes_no_stake() {
    // Extra USDC only ever looks like surplus.
    let mut env = Env::new();
    let (min, _) = bounds();
    env.staker(min);
    let v = env.vault();
    env.add_token(&v, unit());
    let p = env.pool_state();
    assert_eq!((p.total_staked, p.total_allocated), (min, 0));
    // It sits above everything owed: protocol surplus, never stake.
    assert_eq!(env.vault_liquid(), min + unit());
}
