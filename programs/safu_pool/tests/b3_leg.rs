//! B3: the Marinade leg and yield, against the real Marinade program and its devnet accounts.
//! Deposit, harvest, payout unstakes, rebalance, yield claims, protocol revenue, compute units.

mod common;

use common::*;
use pool_core::params::{MAX_REBALANCE_SLIPPAGE_BPS, SECONDS_PER_DAY};
use pool_core::{apply_bps, liquidity, yields};
use safu_pool::constants::*;
use safu_pool::errors::PoolError;
use safu_pool::state::{Pool, StakeRecord};
use solana_keypair::Keypair;
use solana_signer::Signer;

const BACKING: u64 = 10 * SOL;
const TX: [u8; 32] = [9; 32];

/// Marinade at its liquidity target, a matured backer, then one max staker (whose stake deploys the
/// pool to its line): both sides earn.
fn funded_pool() -> (Env, Keypair, Keypair) {
    let mut env = Env::new();
    env.marinade_liquidity_at_target();
    let b = env.matured_backer(BACKING);
    let s = env.staker(bounds().1);
    (env, s, b)
}

/// One day passes and Marinade's staking rewards raise mSOL's value by `bps`.
fn grow(env: &mut Env, bps: u64) {
    env.warp(SECONDS_PER_DAY);
    env.marinade_rewards(bps);
}

fn harvest(env: &mut Env) {
    let ix = env.upkeep_ix(safu_pool::instruction::Harvest {});
    let anyone = env.funded(SOL);
    env.ok(&[ix], &[&anyone]);
}

/// Yield books moved exactly as `yields::credit` splits `amount` over `before`'s capacity.
fn assert_credited(before: &Pool, after: &Pool) -> u64 {
    let amount = after.total_extracted_yield - before.total_extracted_yield;
    let c = yields::credit(
        amount,
        before.total_staked,
        before.total_backed,
        STAKER_YIELD_BPS,
        BACKER_YIELD_BPS,
    )
    .unwrap();
    assert_eq!(
        after.staker_yield_reserved - before.staker_yield_reserved,
        c.staker_share
    );
    assert_eq!(
        after.backer_yield_reserved - before.backer_yield_reserved,
        c.backer_share
    );
    assert_eq!(
        after.protocol_yield_balance - before.protocol_yield_balance,
        c.protocol_share
    );
    amount
}

// ------------------------------------------------------------------ deposit

#[test]
fn stakes_deploy_to_the_line_and_small_amounts_wait() {
    let (mut env, _, _) = funded_pool();
    let p = env.pool_state();
    let capacity = p.total_staked + p.total_backed;
    assert_eq!(
        p.deployed_book,
        liquidity::push_amount(capacity, 0, capacity, 0).unwrap()
    );
    assert_eq!(env.pool_msol_amount(), p.deployed_msol);
    assert_eq!(env.vault_liquid(), capacity - p.deployed_book);
    assert_eq!(p.last_harvest_at, env.now);
    // A minimum stake leaves less than `AUTO_PUSH_MIN_BPS` of capacity to deploy: it stays liquid.
    env.staker(bounds().0);
    assert_eq!(env.pool_state().deployed_book, p.deployed_book);
}

#[test]
fn backing_deploys_only_once_it_counts() {
    let mut env = Env::new();
    env.matured_backer(BACKING);
    // Pending money is not capacity, so nothing was deployed; maturing does not deploy either.
    assert_eq!(env.pool_state().deployed_book, 0);
    let ix = env.upkeep_ix(safu_pool::instruction::Rebalance {});
    let anyone = env.funded(SOL);
    env.ok(&[ix], &[&anyone]);
    let p = env.pool_state();
    assert_eq!(
        p.deployed_book,
        liquidity::push_amount(BACKING, 0, BACKING, 0).unwrap()
    );
}

// ------------------------------------------------------------------ payouts through Marinade

#[test]
fn devnet_liquidity_the_payee_pays_marinades_fee_the_pool_does_not() {
    // Real devnet dump: Marinade's liquidity pool is far below target, so its fee is near its maximum.
    let mut env = Env::new();
    let (_, max) = bounds();
    let s = env.staker(max);
    let deployed = env.pool_state().deployed_book;
    let fee_bps = env.marinade_fee_bps(deployed);
    assert!(u64::from(fee_bps) > MAX_REBALANCE_SLIPPAGE_BPS);

    // Payout: goes through, the staker pays Marinade's fee on the unstaked part.
    let fee = apply_bps(deployed, fee_bps.into()).unwrap();
    let rent = env.lamports(&env.stake_record(&s.pubkey()));
    let before = env.lamports(&s.pubkey());
    let ix = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    env.ok(&[ix], &[&s]);
    let got = env.lamports(&s.pubkey()) - before;
    let slack = apply_bps(deployed, 1).unwrap() + SOL / 1_000;
    assert!(
        got + fee <= max + rent + slack && got + fee + slack >= max + rent,
        "got {got}, fee {fee}"
    );

    // Rebalance: the pool would pay, so the 5% limit holds and it is refused.
    let b = env.matured_backer(BACKING);
    let ix = env.upkeep_ix(safu_pool::instruction::Rebalance {});
    env.ok(std::slice::from_ref(&ix), &[&b]);
    let req = env.request_backer_ix(&b.pubkey(), BACKING / 2);
    env.ok(&[req], &[&b]);
    env.warp(BACKER_NOTICE_SECS);
    let done = env.complete_backer_ix(&b.pubkey());
    env.ok(&[done], &[&b]);
    assert_err(env.send(&[ix], &[&b]), PoolError::UnstakeFeeTooHigh);
}

#[test]
fn withdraw_unstakes_and_the_payee_pays_marinade_fee() {
    let mut env = Env::new();
    env.marinade_liquidity_at_target();
    let (_, max) = bounds();
    let s = env.staker(max);
    let deployed = env.pool_state().deployed_book;
    let fee = apply_bps(deployed, env.marinade_fee_bps(deployed).into()).unwrap();
    let rent = env.lamports(&env.stake_record(&s.pubkey()));
    let before = env.lamports(&s.pubkey());
    let ix = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    env.ok(&[ix], &[&s]);
    // Principal less Marinade's fee on the unstaked part (rent back, tx fee paid: within 1 bp).
    let got = env.lamports(&s.pubkey()) - before;
    let slack = apply_bps(deployed, 1).unwrap();
    assert!(
        got + fee <= max + rent + slack && got + fee + slack + SOL / 1_000 >= max + rent,
        "got {got}"
    );
    let p = env.pool_state();
    assert_eq!(
        (p.deployed_msol, p.deployed_book, p.total_staked),
        (0, 0, 0)
    );
}

#[test]
fn payout_unstake_never_touches_set_aside_yield() {
    let (mut env, s, _) = funded_pool();
    grow(&mut env, 100);
    harvest(&mut env);
    // Leave only the set-aside plus a little in cash: the stream must unstake.
    let p = env.pool_state();
    let reserved = p.staker_yield_reserved + p.backer_yield_reserved;
    assert!(reserved > 0);
    let v = env.vault();
    let mut a = env.svm.get_account(&v).unwrap();
    a.lamports = env.rent_floor() + reserved + bounds().0;
    env.svm.set_account(v, a).unwrap();

    env.warp(TIME_GATE_SECS);
    let a = env.approval(&s.pubkey(), TX, bounds().1, TIER_A);
    env.submit(&a).unwrap();
    let approve = env.approve_claim_ix(&s.pubkey(), &TX);
    env.ok(&[approve], &[&s]);
    env.warp(COOLDOWN_SECS + VESTING_SECS);
    let ix = env.stream_ix(&s.pubkey(), &TX, &s.pubkey());
    env.ok(&[ix], &[&s]);
    let p = env.pool_state();
    assert!(
        p.deployed_book < env.pool_state().deployed_book + 1
            && env.vault_liquid() >= p.staker_yield_reserved + p.backer_yield_reserved
    );
}

// ------------------------------------------------------------------ harvest

#[test]
fn harvest_credits_growth_up_to_the_daily_limit() {
    let (mut env, _, _) = funded_pool();
    let before = env.pool_state();
    grow(&mut env, 100); // 1% in a day, far above the 10 bp/day limit
    harvest(&mut env);
    let after = env.pool_state();
    let credited = assert_credited(&before, &after);
    let limit = yields::harvest_limit(before.deployed_book, SECONDS_PER_DAY).unwrap();
    assert!(
        credited > 0 && credited <= limit,
        "credited {credited}, limit {limit}"
    );
    assert!(after.staker_yield_reserved > 0 && after.backer_yield_reserved > 0);
    // Book value never moves on a harvest; only mSOL leaves.
    assert_eq!(after.deployed_book, before.deployed_book);
    assert!(after.deployed_msol < before.deployed_msol);
    assert_eq!(after.last_harvest_at, env.now);
    // Same second again: no time, no limit, nothing taken.
    harvest(&mut env);
    assert_eq!(env.pool_state().deployed_msol, after.deployed_msol);
}

#[test]
fn harvest_is_a_no_op_without_growth_while_paused_or_when_marinade_cannot_pay() {
    let (mut env, _, _) = funded_pool();
    let untouched = |env: &Env, p: &Pool| {
        let q = env.pool_state();
        assert_eq!(
            (q.deployed_msol, q.last_harvest_at, q.total_extracted_yield),
            (p.deployed_msol, p.last_harvest_at, p.total_extracted_yield)
        );
    };
    let p = env.pool_state();
    env.warp(SECONDS_PER_DAY);
    harvest(&mut env);
    untouched(&env, &p);
    grow(&mut env, 100);
    env.pause();
    harvest(&mut env);
    untouched(&env, &p);
    let admin = env.admin.insecure_clone();
    let ix = env.admin_ix(safu_pool::instruction::Unpause {});
    env.ok(&[ix], &[&admin]);
    env.set_marinade_liquidity(0);
    harvest(&mut env);
    untouched(&env, &p);
}

#[test]
fn new_stake_and_backing_harvest_first_so_growth_goes_to_the_money_already_in() {
    let (mut env, _, _) = funded_pool();
    grow(&mut env, 100);
    let before = env.pool_state();
    env.staker(bounds().1);
    assert!(assert_credited(&before, &env.pool_state()) > 0);

    grow(&mut env, 100);
    let before = env.pool_state();
    let b = env.funded(2 * SOL);
    let ix = env.back_ix(&b.pubkey(), SOL);
    env.ok(&[ix], &[&b]);
    assert!(assert_credited(&before, &env.pool_state()) > 0);
}

// ------------------------------------------------------------------ yield claims

#[test]
fn staker_yield_goes_to_the_beneficiary_and_principal_stays() {
    let (mut env, s, _) = funded_pool();
    grow(&mut env, 100);
    harvest(&mut env);
    let p = env.pool_state();
    let r = env.stake_state(&s.pubkey());
    let owed = yields::owed(r.amount, p.staker_yield_index, r.yield_index_at).unwrap();
    assert!(owed > 0);
    let before = env.lamports(&s.pubkey());
    let ix = env.claim_yield_ix(&s.pubkey(), &s.pubkey());
    env.ok(std::slice::from_ref(&ix), &[&s]);
    // Paid from set-aside cash: no unstake, no fee (the tx fee is the staker's).
    assert!(env.lamports(&s.pubkey()) + SOL / 1_000 > before + owed);
    assert_eq!(
        env.pool_state().staker_yield_reserved,
        p.staker_yield_reserved - owed
    );
    assert_eq!(env.stake_state(&s.pubkey()).amount, r.amount);
    assert_err(env.send(&[ix], &[&s]), PoolError::NothingToClaim);
}

#[test]
fn staker_yield_claim_refusals() {
    let (mut env, s, _) = funded_pool();
    grow(&mut env, 100);
    harvest(&mut env);
    let ix = env.claim_yield_ix(&s.pubkey(), &Keypair::new().pubkey());
    assert_err(env.send(&[ix], &[&s]), PoolError::WrongBeneficiary);
    let ix = env.claim_yield_ix(&s.pubkey(), &s.pubkey());
    let rec = env.stake_record(&s.pubkey());
    env.edit::<StakeRecord>(&rec, |r| r.forfeited = true);
    assert_err(
        env.send(std::slice::from_ref(&ix), &[&s]),
        PoolError::StakeForfeited,
    );
    env.edit::<StakeRecord>(&rec, |r| r.forfeited = false);
    env.pause();
    assert_err(env.send(&[ix], &[&s]), PoolError::Paused);
}

#[test]
fn backer_yield_claim() {
    let (mut env, _, b) = funded_pool();
    grow(&mut env, 100);
    harvest(&mut env);
    let owed = {
        let p = env.pool_state();
        let r = env.backer_state(&b.pubkey());
        r.yield_owed + yields::owed(r.amount, p.backer_yield_index, r.yield_index_at).unwrap()
    };
    assert!(owed > 0);
    let before = env.lamports(&b.pubkey());
    let ix = env.claim_backer_yield_ix(&b.pubkey());
    env.ok(std::slice::from_ref(&ix), &[&b]);
    assert!(env.lamports(&b.pubkey()) + SOL / 1_000 > before + owed);
    assert_eq!(env.backer_state(&b.pubkey()).amount, BACKING);
    assert_err(env.send(&[ix], &[&b]), PoolError::NothingToClaim);
}

// ------------------------------------------------------------------ rebalance

#[test]
fn rebalance_with_nothing_to_do_is_refused() {
    let (mut env, _, _) = funded_pool();
    let ix = env.upkeep_ix(safu_pool::instruction::Rebalance {});
    let anyone = env.funded(SOL);
    assert_err(env.send(&[ix], &[&anyone]), PoolError::NothingToRebalance);
}

#[test]
fn rebalance_covers_open_claims_from_marinade() {
    let (mut env, s, _) = funded_pool();
    // A claim larger than free cash: the pool unstakes the difference.
    let free = env.vault_liquid();
    let entitlement = free + SOL / 2;
    let a = env.approval(&s.pubkey(), TX, entitlement, TIER_A);
    env.submit(&a).unwrap();
    assert_eq!(env.pool_state().total_allocated, entitlement);
    let ix = env.upkeep_ix(safu_pool::instruction::Rebalance {});
    let anyone = env.funded(SOL);
    env.ok(&[ix], &[&anyone]);
    // Book value of the shortfall came back less the pool's Marinade fee (multichain pulls the same).
    assert!(
        env.vault_liquid() < entitlement
            && env.vault_liquid() + Env::max_unstake_fee(SOL / 2) >= entitlement
    );
}

#[test]
fn rebalance_pulls_back_to_the_line_and_the_pool_pays_the_fee() {
    let (mut env, s, b) = funded_pool();
    // The backer leaves: capacity shrinks and the deployment sits above the line.
    let req = env.request_backer_ix(&b.pubkey(), BACKING);
    env.ok(&[req], &[&b]);
    env.warp(BACKER_NOTICE_SECS);
    let done = env.complete_backer_ix(&b.pubkey());
    env.ok(&[done], &[&b]);
    let p = env.pool_state();
    let line = apply_bps(p.total_staked + p.total_backed, DEPLOY_BPS).unwrap();
    assert!(p.deployed_book > line);
    let ix = env.upkeep_ix(safu_pool::instruction::Rebalance {});
    let anyone = env.funded(SOL);
    env.ok(&[ix], &[&anyone]);
    let q = env.pool_state();
    assert!(q.deployed_book <= line);
    // Flat price: the pool's fee is a loss, marked off total_staked (multichain `DeploymentShortfall`).
    assert!(q.total_staked < p.total_staked);
    assert_eq!(env.stake_state(&s.pubkey()).amount, bounds().1);
}

#[test]
fn rebalance_growth_pays_the_pools_fee_first() {
    let (mut env, _, b) = funded_pool();
    let req = env.request_backer_ix(&b.pubkey(), BACKING);
    env.ok(&[req], &[&b]);
    env.warp(BACKER_NOTICE_SECS);
    let done = env.complete_backer_ix(&b.pubkey());
    env.ok(&[done], &[&b]);
    // Unharvested growth well above Marinade's fee.
    env.marinade_rewards(200);
    let before = env.pool_state();
    let ix = env.upkeep_ix(safu_pool::instruction::Rebalance {});
    let anyone = env.funded(SOL);
    env.ok(&[ix], &[&anyone]);
    let after = env.pool_state();
    assert_eq!(after.total_staked, before.total_staked);
    assert!(after.total_extracted_yield > before.total_extracted_yield);
}

// ------------------------------------------------------------------ protocol revenue

#[test]
fn treasury_takes_protocol_revenue_within_the_surplus_only() {
    let (mut env, _, _) = funded_pool();
    let admin = env.admin.insecure_clone();
    let treasury = env.treasury.pubkey();
    let revenue = SOL / 10;
    env.edit::<Pool>(&env.pool(), |p| p.protocol_yield_balance = revenue);
    // Counted but not held: claims spent it (multichain W2). No surplus, no withdrawal.
    let ix = env.withdraw_yield_ix(&admin.pubkey(), &treasury, revenue);
    assert_err(
        env.send(std::slice::from_ref(&ix), &[&admin]),
        PoolError::ExceedsYieldBalance,
    );
    let v = env.vault();
    env.svm.airdrop(&v, revenue).unwrap();
    let over = env.withdraw_yield_ix(&admin.pubkey(), &treasury, revenue + 1);
    assert_err(env.send(&[over], &[&admin]), PoolError::ExceedsYieldBalance);
    let zero = env.withdraw_yield_ix(&admin.pubkey(), &treasury, 0);
    assert_err(env.send(&[zero], &[&admin]), PoolError::AmountNotPositive);
    let other = env.funded(SOL);
    let ix_other = env.withdraw_yield_ix(&other.pubkey(), &treasury, revenue);
    assert_err(env.send(&[ix_other], &[&other]), PoolError::NotAdmin);
    let before = env.lamports(&treasury);
    env.ok(&[ix], &[&admin]);
    assert_eq!(env.lamports(&treasury), before + revenue);
    assert_eq!(env.pool_state().protocol_yield_balance, 0);
}

#[test]
fn treasury_payout_goes_only_to_the_stored_treasury() {
    let (mut env, _, _) = funded_pool();
    env.edit::<Pool>(&env.pool(), |p| p.protocol_yield_balance = SOL);
    let admin = env.admin.insecure_clone();
    let ix = env.withdraw_yield_ix(&admin.pubkey(), &Keypair::new().pubkey(), SOL);
    assert!(env.send(&[ix], &[&admin]).is_err());
}

// ------------------------------------------------------------------ compute units

/// The heaviest paths, each under the client's compute-unit limit from `config/pool.devnet.json`.
#[test]
fn compute_units_fit_the_client_limit() {
    let limit = config().compute_unit_limit;
    let mut env = Env::new();
    env.marinade_liquidity_at_target();
    let mut used = Vec::new();
    let (_, max) = bounds();

    let s = env.funded(max + SOL);
    let ix = env.stake_ix(&s.pubkey(), max, s.pubkey());
    used.push(("stake + deposit", env.send_cu(&[ix], &[&s]).unwrap()));

    grow(&mut env, 100);
    let ix = env.upkeep_ix(safu_pool::instruction::Harvest {});
    used.push(("harvest", env.send_cu(&[ix], &[&s]).unwrap()));

    grow(&mut env, 100);
    let ix = env.claim_yield_ix(&s.pubkey(), &s.pubkey());
    used.push(("claim_yield + harvest", env.send_cu(&[ix], &[&s]).unwrap()));

    grow(&mut env, 100);
    let ix = env.withdraw_ix(&s.pubkey(), &s.pubkey());
    used.push((
        "withdraw + harvest + unstake",
        env.send_cu(&[ix], &[&s]).unwrap(),
    ));

    for (path, cu) in &used {
        eprintln!("CU {path}: {cu}");
        assert!(*cu <= limit, "{path} used {cu}, limit {limit}");
    }
}
