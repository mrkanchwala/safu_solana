//! initialize, pause, pool cap, stake / withdraw / emergency exit / beneficiary, backers,
//! covered wallets. One test per error path.

mod common;

use common::*;
use pool_core::settings::SettingKey as K;
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

/// The fast-clock demo build can never become a mainnet pool.
#[test]
fn a_demo_build_refuses_mainnet() {
    let mut env = Env::uninitialized();
    let admin = env.admin.insecure_clone();
    let mut args = env.init_args();
    args.cluster = CLUSTER_MAINNET;
    let ix = env.init_ix(&admin.pubkey(), args, env.usdc_mint);
    let res = env.send(&[ix], &[&admin]);
    if DEMO_BUILD {
        assert_err(res, PoolError::InvalidCluster);
    } else {
        res.unwrap();
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
        .set_account(
            mint,
            solana_account::Account {
                lamports,
                data,
                owner: other_program,
                executable: false,
                rent_epoch: 0,
            },
        )
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
    let src = ix
        .accounts
        .iter()
        .position(|a| a.pubkey == env.ata(&s.pubkey()))
        .unwrap();
    ix.accounts[src].pubkey = env.ata(&other.pubkey());
    assert_err(env.send(&[ix], &[&s]), PoolError::WrongTokenAccount);
    // A token account of another mint, owned by the staker.
    let fake_mint = Keypair::new().pubkey();
    set_token_program_account(
        &mut env.svm,
        fake_mint,
        mint_data(pool_core::params::USDC_DECIMALS, 0),
    );
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
    for amount in [0, min - 1] {
        let ix = env.stake_ix(&s.pubkey(), amount, s.pubkey());
        assert_err(env.send(&[ix], &[&s]), PoolError::StakeOutOfRange);
    }
}

#[test]
fn a_deposit_above_the_max_stakes_the_max_and_backs_the_rest() {
    let mut env = Env::new();
    let (_, max) = bounds();
    let s = env.funded(3 * max);
    let ix = env.stake_ix(&s.pubkey(), 3 * max, s.pubkey());
    env.ok(&[ix], &[&s]);
    assert_eq!(env.stake_state(&s.pubkey()).amount, max);
    let b = env.backer_state(&s.pubkey());
    assert_eq!(
        (b.backer, b.amount, b.pending_count),
        (s.pubkey(), 2 * max, 0)
    );
    let p = env.pool_state();
    assert_eq!((p.total_staked, p.total_backed), (max, 2 * max));
    // One transfer for both parts.
    assert_eq!((env.vault_liquid(), env.usdc(&s.pubkey())), (3 * max, 0));
    // The backing part leaves like any backing, never with the stake.
    let ix = env.request_backer_ix(&s.pubkey(), 2 * max);
    env.ok(&[ix], &[&s]);
}

#[test]
fn an_overflow_needs_the_backer_record_account() {
    let mut env = Env::new();
    let (_, max) = bounds();
    let s = env.funded(2 * max);
    let ix = env.stake_ix_with(&s.pubkey(), max + 1, s.pubkey(), None);
    assert_err(env.send(&[ix], &[&s]), PoolError::BackerRecordRequired);
    // Within the max it is not needed.
    let ix = env.stake_ix_with(&s.pubkey(), max, s.pubkey(), None);
    env.ok(&[ix], &[&s]);
    assert!(!env.exists(&env.backer_record(&s.pubkey())));
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
    // The overflow never turns a refused stake into backing.
    let ix = env.stake_ix(&s.pubkey(), 2 * max, s.pubkey());
    assert_err(env.send(&[ix], &[&s]), PoolError::PoolCapExceeded);
    assert_eq!(env.pool_state().total_backed, 0);
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
    assert_err(
        env.send(std::slice::from_ref(&ix), &[&s]),
        PoolError::AlreadyStaked,
    );
    // Approved once means never again, even with the stake restored and then withdrawn.
    env.edit::<StakeRecord>(&rec, |r| {
        (r.forfeited, r.claim_approved, r.amount) = (false, true, 0);
    });
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
fn withdraw_pays_the_beneficiary_and_keeps_the_record() {
    let mut env = Env::new();
    let (_, max) = bounds();
    // Enough for two stakes: the first withdrawal goes to the beneficiary, not back here.
    let s = env.funded(2 * max);
    let ben = Keypair::new().pubkey();
    env.open_usdc(&ben);
    let ix = env.stake_ix(&s.pubkey(), max, ben);
    env.ok(&[ix], &[&s]);
    env.warp(SECONDS_PER_DAY);
    let ix = env.withdraw_ix(&s.pubkey(), &ben);
    env.ok(&[ix], &[&s]);
    assert_eq!(env.usdc(&ben), max);
    // The record stays: its points (one day's) live on it.
    let r = env.stake_state(&s.pubkey());
    assert_eq!((r.amount, r.points), (0, max / USDC_UNIT));
    let p = env.pool_state();
    assert_eq!(
        (p.total_staked, p.total_stakers, env.vault_liquid()),
        (0, 0, 0)
    );
    // The address can stake again, and keeps its points.
    let ix = env.stake_ix(&s.pubkey(), max, s.pubkey());
    env.ok(&[ix], &[&s]);
    assert_eq!(env.stake_state(&s.pubkey()).points, max / USDC_UNIT);
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
fn the_rounding_reserve_covers_only_a_small_gap() {
    let mut env = Env::new();
    let (min, _) = bounds();
    let s = env.staker(min);
    let f = env.funded(unit());
    let ix = env.fund_reserve_ix(&f.pubkey(), MAX_ROUNDING_DRAW);
    env.ok(&[ix], &[&f]);
    assert_eq!(env.pool_state().rounding_reserve, MAX_ROUNDING_DRAW);
    // The reserve is not capacity and not the treasury's.
    assert_eq!(env.pool_state().total_staked, min);
    let t = env.treasury.pubkey();
    let admin = env.admin.insecure_clone();
    let ix = env.withdraw_yield_ix(&admin.pubkey(), &t, 1);
    assert_err(env.send(&[ix], &[&admin]), PoolError::ExceedsYieldBalance);
    // Cash one unit short of the books (venue rounding): the reserve pays the gap.
    env.edit::<Pool>(&env.pool(), |p| p.rounding_reserve += 1);
    let ix = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    env.ok(&[ix], &[&s]);
    assert_eq!(env.pool_state().rounding_reserve, MAX_ROUNDING_DRAW);
    assert_eq!(env.vault_liquid(), MAX_ROUNDING_DRAW);
    // A gap above the cap is a real loss: refused.
    let s = env.staker(min);
    env.edit::<Pool>(&env.pool(), |p| p.rounding_reserve += MAX_ROUNDING_DRAW + 1);
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
    assert_eq!(env.stake_state(&s.pubkey()).amount, 0);
}

// ------------------------------------------------------------------ backers

/// Drip length today, in days, from the live settings and the pool's utilisation now.
fn drip_days(env: &Env) -> u64 {
    let p = env.pool_state();
    let s = p.settings();
    let capacity = p.total_staked + p.total_backed;
    let util = pool_core::drip::utilisation_bps(p.total_allocated, capacity).unwrap();
    pool_core::drip::drip_days(util, s.get(K::DripMinSecs), s.get(K::DripMaxSecs)).unwrap()
}

fn collect(env: &mut Env, b: &Keypair) -> Result<(), String> {
    let ix = env.complete_backer_ix(&b.pubkey());
    env.send(&[ix], &[b])
}

#[test]
fn backing_counts_from_day_one() {
    let mut env = Env::new();
    assert_eq!(env.setting(K::BackerMaturitySecs), 0);
    let b = env.funded(10 * unit());
    let ix = env.back_ix(&b.pubkey(), 2 * unit());
    env.ok(&[ix], &[&b]);
    let p = env.pool_state();
    assert_eq!((p.total_backed, p.total_backed_pending), (2 * unit(), 0));
    assert_eq!(env.backer_state(&b.pubkey()).amount, 2 * unit());
    let m = env.mature_ix(&b.pubkey());
    assert_err(env.send(&[m], &[&b]), PoolError::NoPendingBacking);
}

#[test]
fn with_a_maturity_each_deposit_keeps_its_own_clock() {
    let mut env = Env::new();
    let (_, top) = K::BackerMaturitySecs.bounds();
    env.set_setting(K::BackerMaturitySecs, top);
    let b = env.funded(10 * unit());
    let ix = env.back_ix(&b.pubkey(), unit());
    env.ok(&[ix], &[&b]);
    env.warp(top / 2);
    let ix = env.back_ix(&b.pubkey(), 2 * unit());
    env.ok(&[ix], &[&b]);
    let m = env.mature_ix(&b.pubkey());
    let anyone = env.funded(unit());
    assert_err(
        env.send(std::slice::from_ref(&m), &[&anyone]),
        PoolError::BackingNotMature,
    );
    // The first counts on its own clock; the top-up keeps waiting.
    env.warp(top - top / 2);
    env.ok(std::slice::from_ref(&m), &[&anyone]);
    let r = env.backer_state(&b.pubkey());
    assert_eq!(
        (r.amount, r.pending_count, r.pending[0].amount),
        (unit(), 1, 2 * unit())
    );
    env.warp(top / 2);
    env.ok(std::slice::from_ref(&m), &[&anyone]);
    let p = env.pool_state();
    assert_eq!((p.total_backed, p.total_backed_pending), (3 * unit(), 0));
    assert_err(env.send(&[m], &[&anyone]), PoolError::NoPendingBacking);
}

#[test]
fn at_most_the_limit_of_deposits_wait_at_once() {
    let mut env = Env::new();
    let (_, top) = K::BackerMaturitySecs.bounds();
    env.set_setting(K::BackerMaturitySecs, top);
    let b = env.funded(10 * unit());
    for i in 0..MAX_PENDING_DEPOSITS as u64 {
        let ix = env.back_ix(&b.pubkey(), 1 + i);
        env.ok(&[ix], &[&b]);
    }
    let ix = env.back_ix(&b.pubkey(), 1);
    assert_err(env.send(&[ix], &[&b]), PoolError::TooManyPendingDeposits);
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
    // Still counted while it drips.
    let p = env.pool_state();
    assert_eq!((p.total_backed, p.total_backer_open), (2 * unit(), unit()));
    env.ok(&[cancel], &[&b]);
    assert_eq!(env.backer_state(&b.pubkey()).withdraw_amount, 0);
    assert_eq!(env.pool_state().total_backer_open, 0);
}

#[test]
fn a_withdrawal_drips_out_one_part_a_day() {
    let mut env = Env::new();
    let w = 7 * unit();
    let b = env.matured_backer(w);
    let days = drip_days(&env);
    assert_err(collect(&mut env, &b), PoolError::NoBackerWithdrawal);
    let ix = env.request_backer_ix(&b.pubkey(), w);
    let logs = env.send_logs(&[ix], &[&b]).unwrap();
    // The request says when its last part is due: the start of its last drip day.
    let req: safu_pool::events::BackerWithdrawalRequested = event(&logs).unwrap();
    let last_day = days as i64 - 1;
    assert_eq!(
        req.ready_at,
        env.now + last_day * pool_core::params::SECONDS_PER_DAY
    );
    let before = env.usdc(&b.pubkey());
    for day in 0..days {
        collect(&mut env, &b).unwrap();
        let want = pool_core::drip::unlocked(w, day, days).unwrap();
        assert_eq!(env.usdc(&b.pubkey()) - before, want);
        if day + 1 < days {
            // Today's part is out: nothing more until tomorrow.
            assert_err(collect(&mut env, &b), PoolError::NothingToCollectYet);
            env.warp(pool_core::params::SECONDS_PER_DAY);
        }
    }
    let p = env.pool_state();
    assert_eq!((p.total_backed, p.total_backer_open), (0, 0));
    let r = env.backer_state(&b.pubkey());
    assert_eq!((r.amount, r.withdraw_amount, r.withdraw_paid), (0, 0, 0));
}

#[test]
fn the_drip_is_longer_when_open_claims_use_the_pool() {
    let mut env = Env::new();
    let w = 4 * unit();
    let b = env.matured_backer(w);
    let idle = drip_days(&env);
    // At the full-drip utilisation and above, the longest drip.
    env.edit::<Pool>(&env.pool(), |p| {
        p.total_allocated = pool_core::apply_bps(w, DRIP_FULL_UTIL_BPS).unwrap()
    });
    let busy = drip_days(&env);
    let s = env.pool_state().settings();
    let day = pool_core::params::SECONDS_PER_DAY as u64;
    assert_eq!(busy, (s.get(K::DripMaxSecs) as u64).div_ceil(day).max(1));
    assert!(busy >= idle);
    let ix = env.request_backer_ix(&b.pubkey(), w / 4);
    env.ok(&[ix], &[&b]);
    let before = env.usdc(&b.pubkey());
    collect(&mut env, &b).unwrap();
    let want = pool_core::drip::unlocked(w / 4, 0, busy).unwrap();
    assert_eq!(env.usdc(&b.pubkey()) - before, want);
}

#[test]
fn a_day_when_open_claims_need_every_dollar_pays_nothing() {
    let mut env = Env::new();
    let b = env.matured_backer(2 * unit());
    let ix = env.request_backer_ix(&b.pubkey(), unit());
    env.ok(&[ix], &[&b]);
    env.edit::<Pool>(&env.pool(), |p| p.total_allocated = 2 * unit());
    assert_err(collect(&mut env, &b), PoolError::BackerCapitalNotFree);
    // Capital frees up: the drip resumes the next day (today's snapshot had none free).
    env.edit::<Pool>(&env.pool(), |p| p.total_allocated = 0);
    env.warp(pool_core::params::SECONDS_PER_DAY);
    collect(&mut env, &b).unwrap();
}

#[test]
fn requests_share_the_days_free_capital_pro_rata_whatever_the_order() {
    let mut env = Env::new();
    let w = 2 * unit();
    let (b1, b2) = (env.matured_backer(w), env.matured_backer(w));
    for b in [&b1, &b2] {
        let ix = env.request_backer_ix(&b.pubkey(), w);
        env.ok(&[ix], &[b]);
    }
    // Only a sliver is free from tomorrow (today's snapshot was taken at the requests).
    let free = 1_000;
    env.edit::<Pool>(&env.pool(), |p| p.total_allocated = 2 * w - free);
    env.warp(pool_core::params::SECONDS_PER_DAY);
    let (u1, u2) = (env.usdc(&b1.pubkey()), env.usdc(&b2.pubkey()));
    collect(&mut env, &b2).unwrap();
    collect(&mut env, &b1).unwrap();
    // Each gets half the sliver: collecting first took nothing from the other.
    let half = pool_core::drip::share(free, w, 2 * w).unwrap();
    assert_eq!(env.usdc(&b1.pubkey()) - u1, half);
    assert_eq!(env.usdc(&b2.pubkey()) - u2, half);
    // More capital frees up later the same day: today's share is spent, so a second collect pays
    // nothing until tomorrow.
    env.edit::<Pool>(&env.pool(), |p| p.total_allocated -= unit());
    let u1 = env.usdc(&b1.pubkey());
    assert_err(collect(&mut env, &b1), PoolError::NothingToCollectYet);
    assert_eq!(env.usdc(&b1.pubkey()), u1);
    // And never more than was free.
    let p = env.pool_state();
    assert!(p.total_allocated <= p.total_staked + p.total_backed);
}

#[test]
fn cancel_leaves_the_unpaid_part_in_the_pool() {
    let mut env = Env::new();
    let w = 7 * unit();
    let b = env.matured_backer(w);
    let ix = env.request_backer_ix(&b.pubkey(), w);
    env.ok(&[ix], &[&b]);
    let before = env.usdc(&b.pubkey());
    collect(&mut env, &b).unwrap();
    let paid = env.usdc(&b.pubkey()) - before;
    let ix = env.backer_only_ix(
        &b.pubkey(),
        safu_pool::instruction::CancelBackerWithdrawal {},
    );
    if paid < w {
        env.ok(&[ix], &[&b]);
        let r = env.backer_state(&b.pubkey());
        assert_eq!((r.amount, r.withdraw_amount), (w - paid, 0));
    } else {
        // A one-day drip (demo clocks) pays it all at once: nothing left to cancel.
        assert_err(env.send(&[ix], &[&b]), PoolError::NoBackerWithdrawal);
    }
    assert_eq!(env.pool_state().total_backer_open, 0);
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
    let before = env.usdc(&b.pubkey());
    collect(&mut env, &b).unwrap();
    assert!(env.usdc(&b.pubkey()) > before);
    let cancel = env.backer_only_ix(
        &b.pubkey(),
        safu_pool::instruction::CancelBackerWithdrawal {},
    );
    let _ = env.send(&[cancel], &[&b]);
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

#[test]
fn a_cancel_releases_only_its_own_unpaid_part() {
    let mut env = Env::new();
    let w = 7 * unit();
    let (b1, b2) = (env.matured_backer(w), env.matured_backer(w));
    let ix = env.request_backer_ix(&b1.pubkey(), w);
    env.ok(&[ix], &[&b1]);
    let before = env.usdc(&b1.pubkey());
    collect(&mut env, &b1).unwrap();
    let paid = env.usdc(&b1.pubkey()) - before;
    let ix = env.request_backer_ix(&b2.pubkey(), w);
    env.ok(&[ix], &[&b2]);
    assert_eq!(env.pool_state().total_backer_open, 2 * w - paid);
    if paid == w {
        // A one-day drip (demo clocks) paid it all at once: nothing left to cancel.
        return;
    }
    let cancel = env.backer_only_ix(
        &b1.pubkey(),
        safu_pool::instruction::CancelBackerWithdrawal {},
    );
    let logs = env.send_logs(&[cancel], &[&b1]).unwrap();
    let ev: safu_pool::events::BackerWithdrawalCancelled = event(&logs).unwrap();
    assert_eq!(ev.amount, w - paid);
    // The other backer's request is still counted in full.
    assert_eq!(env.pool_state().total_backer_open, w);
}

#[test]
fn only_a_payment_that_draws_on_the_rounding_reserve_reports_it() {
    let mut env = Env::new();
    let (min, _) = bounds();
    let s = env.staker(min);
    let ix = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    let logs = env.send_logs(&[ix], &[&s]).unwrap();
    assert!(event::<safu_pool::events::RoundingReserveUsed>(&logs).is_none());
    // Cash one unit short of the books: the draw is reported with its size.
    let s = env.staker(min);
    let f = env.funded(unit());
    let ix = env.fund_reserve_ix(&f.pubkey(), MAX_ROUNDING_DRAW);
    env.ok(&[ix], &[&f]);
    env.edit::<Pool>(&env.pool(), |p| p.rounding_reserve += 1);
    let ix = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    let logs = env.send_logs(&[ix], &[&s]).unwrap();
    let used = event::<safu_pool::events::RoundingReserveUsed>(&logs).map(|e| e.amount);
    assert_eq!(used, Some(1));
}

#[test]
fn a_payment_must_name_the_pools_usdc_mint() {
    let mut env = Env::new();
    let s = env.staker(bounds().0);
    // A real 6-decimal mint, just not the pool's.
    let other = anchor_lang::prelude::Pubkey::new_unique();
    set_token_program_account(&mut env.svm, other, mint_data(6, 0));
    let mut ix = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    for m in ix.accounts.iter_mut().filter(|m| m.pubkey == env.usdc_mint) {
        m.pubkey = other;
    }
    assert_err(env.send(&[ix], &[&s]), PoolError::WrongMint);
}
