//! The yield leg against the real Kamino Lend program: mainnet klend (`fixtures/klend.so`) and its
//! main-market USDC reserve, market and Scope price account as dumped 2026-10-22 (mainnet slot
//! 460019490). The pool's USDC mint sits at the real USDC address, so the reserve accepts it.
//!
//! Scope prices carry their own timestamps: `fresh_prices` stamps them with the test clock, as a
//! live Scope keeper would; leaving them alone is the stale-price case.
mod common;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::Instruction;
use base64::Engine;
use common::*;
use pool_core::kamino as k;
use safu_pool::constants::{KLEND_PROGRAM_ID, SEED_LEG};
use safu_pool::state::Pool;
use solana_account::Account;
use solana_signer::Signer;

const USDC_MINT: Pubkey =
    anchor_lang::prelude::pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
const RESERVE: Pubkey =
    anchor_lang::prelude::pubkey!("D6q6wuQSrifJKZYpR1M8R4YawnLDtDsMmWM1NbBmgJ59");
const MARKET: Pubkey =
    anchor_lang::prelude::pubkey!("7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF");
const SCOPE: Pubkey = anchor_lang::prelude::pubkey!("3t4JZcueEzTbVP6kLxXrL3VpWx45jDer4eqysweBchNH");
const COLLATERAL_MINT: Pubkey =
    anchor_lang::prelude::pubkey!("B8V6WVjPxW1UGwVDfxH2d2r8SyT4cqn7dQRK6XneVa7D");
const SUPPLY_VAULT: Pubkey =
    anchor_lang::prelude::pubkey!("Bgq7trRgVMeq33yt235zM2onQ4bRDBsY5EWiTetF4qw6");
/// Mainnet slot and unix time when the fixtures were dumped.
const DUMP_SLOT: u64 = 460_019_490;
const DUMP_UNIX: i64 = 1_792_668_744;
/// Mainnet slots per second (400 ms slots), to move the slot with the clock.
const SLOTS_PER_SEC: u64 = 5;

/// Scope `OraclePrices`: discriminator, mapping key, then 512 `DatedPrice` of 56 bytes each, whose
/// slot and unix time sit at +16 and +24.
const SCOPE_PRICES_AT: usize = 8 + 32;
const DATED_PRICE_LEN: usize = 56;

fn fixture(name: &str) -> Account {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let j: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let a = &j["account"];
    Account {
        lamports: a["lamports"].as_u64().unwrap(),
        data: base64::engine::general_purpose::STANDARD
            .decode(a["data"][0].as_str().unwrap())
            .unwrap(),
        owner: a["owner"].as_str().unwrap().parse().unwrap(),
        executable: false,
        rent_epoch: 0,
    }
}

/// Moves the clock and the slot together (klend accrues interest by slot).
fn warp(env: &mut Env, secs: i64) {
    env.warp(secs);
    let slot =
        env.svm.get_sysvar::<anchor_lang::prelude::Clock>().slot + secs as u64 * SLOTS_PER_SEC;
    let now = env.now;
    env.svm.warp_to_slot(slot);
    let mut c: anchor_lang::prelude::Clock = env.svm.get_sysvar();
    c.unix_timestamp = now;
    env.svm.set_sysvar(&c);
}

/// Stamps every Scope price with the current clock.
fn fresh_prices(env: &mut Env) {
    let c: anchor_lang::prelude::Clock = env.svm.get_sysvar();
    let mut a = env.svm.get_account(&SCOPE).unwrap();
    for i in 0..512 {
        let at = SCOPE_PRICES_AT + i * DATED_PRICE_LEN;
        a.data[at + 16..at + 24].copy_from_slice(&c.slot.to_le_bytes());
        a.data[at + 24..at + 32].copy_from_slice(&(c.unix_timestamp as u64).to_le_bytes());
    }
    env.svm.set_account(SCOPE, a).unwrap();
}

fn reserve(env: &Env) -> k::Reserve {
    k::parse_reserve(&env.svm.get_account(&RESERVE).unwrap().data).unwrap()
}

fn pool_collateral(env: &Env) -> Pubkey {
    pda(&[SEED_LEG, env.pool().as_ref(), COLLATERAL_MINT.as_ref()])
}

/// A pool on the real USDC mint with klend and the reserve loaded and the venue set (as governance
/// would, with nothing deployed), `deploy_bps` of capacity allowed out, the collateral account open.
fn setup(deploy_bps: u64) -> Env {
    let mut env = Env::new_at(USDC_MINT, DUMP_UNIX);
    env.svm
        .add_program(KLEND_PROGRAM_ID, include_bytes!("fixtures/klend.so"))
        .unwrap();
    for (key, name) in [
        (RESERVE, "klend_usdc_reserve"),
        (MARKET, "klend_main_market"),
        (SCOPE, "scope_prices"),
        (COLLATERAL_MINT, "klend_usdc_collateral_mint"),
        (SUPPLY_VAULT, "klend_usdc_supply_vault"),
    ] {
        env.svm.set_account(key, fixture(name)).unwrap();
    }
    let mut c: anchor_lang::prelude::Clock = env.svm.get_sysvar();
    c.slot = DUMP_SLOT + 10;
    env.svm.set_sysvar(&c);
    fresh_prices(&mut env);
    env.edit::<Pool>(&env.pool(), |p| {
        p.yield_venue = RESERVE;
        p.deploy_bps = deploy_bps;
    });
    let authority = Pubkey::find_program_address(
        &[k::LENDING_MARKET_AUTH_SEED, MARKET.as_ref()],
        &KLEND_PROGRAM_ID,
    )
    .0;
    env.venue = Some(Venue {
        reserve: RESERVE,
        market: MARKET,
        authority,
        supply: SUPPLY_VAULT,
        collateral_mint: COLLATERAL_MINT,
        pool_collateral: pool_collateral(&env),
        oracle: SCOPE,
    });
    let payer = env.admin.insecure_clone();
    let ix = init_yield_account_ix(&env, &payer.pubkey());
    env.ok(&[ix], &[&payer]);
    env
}

fn init_yield_account_ix(env: &Env, payer: &Pubkey) -> Instruction {
    env.ix(
        safu_pool::accounts::InitYieldAccount {
            payer: *payer,
            pool: env.pool(),
            reserve: RESERVE,
            collateral_mint: COLLATERAL_MINT,
            pool_collateral: pool_collateral(env),
            token_program: TOKEN,
            system_program: SYSTEM,
        },
        safu_pool::instruction::InitYieldAccount {},
    )
}

fn leg_call(env: &Env, data: impl anchor_lang::InstructionData) -> Instruction {
    env.ix(
        safu_pool::accounts::LegCall {
            pool: env.pool(),
            vault: env.vault(),
            cash: env.cash(),
        },
        data,
    )
}

fn collateral_held(env: &Env) -> u64 {
    env.token_amount(&pool_collateral(env))
}

fn provide_ix(env: &Env, collateral: u64, min_out: u64) -> Instruction {
    env.ix(
        safu_pool::accounts::ProvideLiquidity {
            admin: env.admin.pubkey(),
            pool: env.pool(),
            vault: env.vault(),
            cash: env.cash(),
        },
        safu_pool::instruction::ProvideLiquidity {
            collateral,
            min_out,
        },
    )
}

/// The keeper's deploy call; returns the collateral minted.
fn deploy(env: &mut Env) -> u64 {
    let before = collateral_held(env);
    let anyone = env.funded(0);
    let ix = leg_call(env, safu_pool::instruction::AutoDeployLiquidity {});
    env.ok(&[ix], &[&anyone]);
    collateral_held(env) - before
}

/// Anyone's klend `refresh_reserve`, as mainnet users send all the time: accrues interest.
fn refresh_reserve(env: &mut Env) {
    use anchor_lang::solana_program::instruction::AccountMeta;
    let ix = Instruction {
        program_id: KLEND_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(RESERVE, false),
            AccountMeta::new_readonly(MARKET, false),
            AccountMeta::new_readonly(KLEND_PROGRAM_ID, false),
            AccountMeta::new_readonly(KLEND_PROGRAM_ID, false),
            AccountMeta::new_readonly(KLEND_PROGRAM_ID, false),
            AccountMeta::new_readonly(SCOPE, false),
        ],
        data: k::IX_REFRESH_RESERVE.to_vec(),
    };
    let anyone = env.funded(0);
    env.send_bare(&[ix], &[&anyone]).unwrap();
}

/// Breaks the reserve's cash: klend would refuse any redeem now (`redeem_ok` sees no liquidity).
fn drain_reserve(env: &mut Env) {
    let mut a = env.svm.get_account(&RESERVE).unwrap();
    a.data[k::R_LIQ_AVAILABLE..k::R_LIQ_AVAILABLE + 8].copy_from_slice(&0u64.to_le_bytes());
    env.svm.set_account(RESERVE, a).unwrap();
}

fn admin_provide(env: &mut Env, collateral: u64, min_out: u64) -> Result<(), String> {
    let ix = provide_ix(env, collateral, min_out);
    let admin = env.admin.insecure_clone();
    env.send(&[ix], &[&admin])
}

const TX: [u8; 32] = [7; 32];

/// The staker's claim, approved, cooldown and vesting over: all of it collectable.
fn vested_claim(env: &mut Env, s: &solana_keypair::Keypair) {
    let gate = env.time_gate();
    warp(env, gate);
    let a = env.approval(&s.pubkey(), TX, bounds().1, safu_pool::constants::TIER_A);
    env.submit(&a).unwrap();
    let ix = env.approve_claim_ix(&s.pubkey(), &TX);
    env.ok(&[ix], &[s]);
    warp(env, COOLDOWN_SECS + VESTING_SECS);
}

#[test]
fn new_backing_goes_to_klend_and_comes_back_by_cpi() {
    let mut env = setup(5_000);
    // Interest accrues by slot: refreshed now, the rate holds for the rest of this slot.
    refresh_reserve(&mut env);
    let r = reserve(&env);
    env.matured_backer(100_000 * USDC);
    // Maturing made capacity; half of it went to klend at the reserve's own rate, rounded down.
    let p = env.pool_state();
    assert_eq!(p.deployed_book, 50_000 * USDC);
    assert_eq!(
        p.deployed_collateral,
        r.collateral_for(50_000 * USDC).unwrap()
    );
    assert_eq!(collateral_held(&env), p.deployed_collateral);
    assert_eq!(env.vault_liquid(), 50_000 * USDC);
    assert_eq!(reserve(&env).available, r.available + 50_000 * USDC);

    // Nothing idle above the line: the keeper call has nothing to do.
    assert_eq!(deploy(&mut env), 0);

    // The admin brings all of it back: at least book value less klend's rounding.
    assert_err(
        admin_provide(&mut env, p.deployed_collateral + 1, 0),
        safu_pool::errors::PoolError::RedeemExceedsDeployed,
    );
    admin_provide(&mut env, p.deployed_collateral, 50_000 * USDC - 2).unwrap();
    let p = env.pool_state();
    assert_eq!((p.deployed_book, p.deployed_collateral), (0, 0));
    assert_eq!(collateral_held(&env), 0);
    assert!(env.vault_liquid() >= 100_000 * USDC - 2);
}

#[test]
fn a_stale_price_does_not_stop_deposits_or_redeems() {
    let mut env = setup(5_000);
    env.matured_backer(100_000 * USDC);
    // A day on, no Scope update: klend's refresh still runs, deposit and redeem still work.
    warp(&mut env, 86_400);
    let (min, _) = bounds();
    env.staker(min.max(2_000 * USDC));
    // The stake is under the auto-push minimum; the keeper call has no minimum.
    assert!(deploy(&mut env) > 0);
    admin_provide(&mut env, 1_000 * USDC, 0).unwrap();
}

#[test]
fn harvest_takes_only_the_growth_as_yield() {
    let mut env = setup(5_000);
    env.matured_backer(100_000 * USDC);
    let before = env.pool_state();
    // Refreshed here only to measure the growth the harvest should take.
    warp(&mut env, 86_400);
    fresh_prices(&mut env);
    refresh_reserve(&mut env);
    let value = reserve(&env)
        .liquidity_for(before.deployed_collateral)
        .unwrap();
    let growth = value - before.deployed_book;
    assert!(growth > 0);
    let anyone = env.funded(0);
    let ix = leg_call(&env, safu_pool::instruction::Harvest {});
    env.ok(&[ix], &[&anyone]);
    let after = env.pool_state();
    // Book value never moves; the collateral that left was worth the growth, rounded down.
    assert_eq!(after.deployed_book, before.deployed_book);
    assert!(after.deployed_collateral < before.deployed_collateral);
    let taken = after.total_extracted_yield - before.total_extracted_yield;
    assert!(
        taken > 0 && taken <= growth,
        "taken {taken} growth {growth}"
    );
    assert!(growth - taken <= 2, "taken {taken} growth {growth}");
    let left = reserve(&env)
        .liquidity_for(after.deployed_collateral)
        .unwrap();
    assert!(left + 2 >= after.deployed_book && left <= after.deployed_book + 2);
    // Credited as yield: the stakers' and backers' shares are set aside.
    assert!(after.backer_yield_reserved > before.backer_yield_reserved);
}

#[test]
fn harvest_refreshes_the_rate_itself() {
    let mut env = setup(5_000);
    env.matured_backer(100_000 * USDC);
    let before = env.pool_state();
    // Nobody outside refreshes the reserve: the harvest must accrue klend's interest on its own.
    warp(&mut env, 86_400);
    let anyone = env.funded(0);
    let ix = leg_call(&env, safu_pool::instruction::Harvest {});
    env.ok(&[ix], &[&anyone]);
    let after = env.pool_state();
    assert!(after.total_extracted_yield > before.total_extracted_yield);
    assert_eq!(after.deployed_book, before.deployed_book);
}

#[test]
fn views_count_growth_not_yet_harvested() {
    let mut env = setup(5_000);
    let b = env.matured_backer(100_000 * USDC);
    warp(&mut env, 86_400);
    fresh_prices(&mut env);
    refresh_reserve(&mut env);
    let seen = env.view_backer(&b.pubkey()).yield_owed;
    let none = {
        let v = env.venue.take();
        let y = env.view_backer(&b.pubkey()).yield_owed;
        env.venue = v;
        y
    };
    assert!(seen > none, "with growth {seen}, without {none}");
    let anyone = env.funded(0);
    let ix = leg_call(&env, safu_pool::instruction::Harvest {});
    env.ok(&[ix], &[&anyone]);
    // The harvest redeems collateral rounded down: at most a unit or two under the view.
    let paid = env.view_backer(&b.pubkey()).yield_owed;
    assert!(
        paid <= seen && seen - paid <= 2,
        "view {seen}, after harvest {paid}"
    );
}

#[test]
fn a_claim_payout_pulls_from_klend_when_the_vault_is_short() {
    let mut env = setup(8_000);
    env.matured_backer(100_000 * USDC);
    let s = env.staker(bounds().1);
    vested_claim(&mut env, &s);
    // All of the vault's cash is out of reach: the payment must come from klend.
    let liquid = env.vault_liquid();
    env.edit::<Pool>(&env.pool(), |p| p.rounding_reserve = liquid);
    let deployed = env.pool_state().deployed_collateral;
    let before = env.usdc(&s.pubkey());
    let ix = env.stream_ix(&s.pubkey(), &TX, &s.pubkey());
    env.ok(&[ix], &[&s]);
    assert!(env.usdc(&s.pubkey()) > before);
    assert!(env.pool_state().deployed_collateral < deployed);
    assert_eq!(env.pool_state().cash_short_days, 0);
}

#[test]
fn a_claim_payout_klend_cannot_cover_counts_a_cash_short_day() {
    let mut env = setup(8_000);
    env.matured_backer(100_000 * USDC);
    let s = env.staker(bounds().1);
    vested_claim(&mut env, &s);
    let liquid = env.vault_liquid();
    env.edit::<Pool>(&env.pool(), |p| p.rounding_reserve = liquid);
    drain_reserve(&mut env);
    let before = env.usdc(&s.pubkey());
    let ix = env.stream_ix(&s.pubkey(), &TX, &s.pubkey());
    env.ok(&[ix], &[&s]);
    assert_eq!(env.usdc(&s.pubkey()), before);
    assert_eq!(env.claim_state(&s.pubkey(), &TX).streamed, 0);
    assert_eq!(env.pool_state().cash_short_days, 1);
}

#[test]
fn withdraw_pulls_the_shortfall_and_a_buffer() {
    let mut env = setup(8_000);
    env.matured_backer(100_000 * USDC);
    let (_, max) = bounds();
    let s = env.staker(max);
    let p = env.pool_state();
    let idle = env.vault_liquid();
    assert!(max > 0 && p.deployed_book > 0);
    // The rounding reserve keeps all idle cash: the whole withdrawal comes from klend.
    env.edit::<Pool>(&env.pool(), |p| p.rounding_reserve = idle);
    let ix = env.withdraw_part_ix(&s.pubkey(), &s.pubkey(), max);
    let cu = env.send_cu(&[ix], &[&s]).unwrap();
    assert!(
        cu <= env.cfg.compute_unit_limit,
        "withdraw with a pull used {cu} CU"
    );
    assert_eq!(env.usdc(&s.pubkey()), max);
    let after = env.pool_state();
    assert!(after.deployed_book < p.deployed_book - max + 1);
}

/// The heaviest calls that harvest, a day on with no Scope update (klend's refresh costs most then):
/// at the site's compute limit the harvest runs inside them; at Solana's 200k default it steps aside
/// and the call still goes through. A harvest is the only thing that moves `last_harvest_at` (a pull
/// also books growth as yield, so yield alone does not tell).
#[test]
fn heavy_calls_harvest_at_the_site_limit_and_never_fail_at_the_default() {
    for site in [true, false] {
        let send = |env: &mut Env, ix: Instruction, s: &solana_keypair::Keypair| {
            let before = env.pool_state().last_harvest_at;
            if site {
                env.send_cu(&[ix], &[s]).unwrap();
            } else {
                env.send_default_limit(&[ix], &[s]).unwrap();
            }
            env.pool_state().last_harvest_at != before
        };
        let (min, max) = bounds();

        // Withdraw that pulls its whole amount from klend.
        let mut env = setup(8_000);
        env.matured_backer(100_000 * USDC);
        let s = env.staker(max);
        warp(&mut env, 86_400);
        let idle = env.vault_liquid();
        env.edit::<Pool>(&env.pool(), |p| p.rounding_reserve = idle);
        let ix = env.withdraw_part_ix(&s.pubkey(), &s.pubkey(), max);
        assert_eq!(send(&mut env, ix, &s), site, "withdraw, site limit {site}");

        // New stake and new backing: the harvest, then the push of the new cash into klend.
        let mut env = setup(8_000);
        env.matured_backer(100_000 * USDC);
        warp(&mut env, 86_400);
        let amount = min.max(2_000 * USDC);
        let k = env.funded(amount);
        let ix = env.stake_ix(&k.pubkey(), amount, k.pubkey());
        assert_eq!(send(&mut env, ix, &k), site, "stake, site limit {site}");
        warp(&mut env, 86_400);
        let b = env.funded(50_000 * USDC);
        let ix = env.back_ix(&b.pubkey(), 50_000 * USDC);
        assert_eq!(send(&mut env, ix, &b), site, "back, site limit {site}");
    }
}

#[test]
fn the_venue_is_checked_before_any_call() {
    let mut env = setup(5_000);
    env.matured_backer(100_000 * USDC);
    // A wrong reserve, market or collateral account in any slot: refused before klend is called.
    let good = env.venue.unwrap();
    for bad in [
        Venue {
            reserve: MARKET,
            ..good
        },
        Venue {
            market: RESERVE,
            ..good
        },
        Venue {
            supply: env.vault(),
            ..good
        },
        Venue {
            pool_collateral: env.vault(),
            ..good
        },
        Venue {
            oracle: MARKET,
            ..good
        },
    ] {
        env.venue = Some(bad);
        let anyone = env.funded(0);
        let ix = leg_call(&env, safu_pool::instruction::EnsureLiquidity {});
        assert!(env.send(&[ix], &[&anyone]).is_err(), "{bad:?}");
    }
    // A money call that leaves the venue out entirely is refused too.
    env.venue = None;
    let s = env.funded(2_000 * USDC);
    let ix = env.stake_ix(&s.pubkey(), 2_000 * USDC, s.pubkey());
    assert_err(
        env.send(&[ix], &[&s]),
        safu_pool::errors::PoolError::YieldAccountsMissing,
    );
}

#[test]
fn governance_installs_only_a_klend_usdc_reserve_and_never_while_deployed() {
    use safu_pool::errors::PoolError;
    use safu_pool::instruction::{ApproveChange, ProposeChange};
    use safu_pool::state::GovChange;
    let mut env = setup(5_000);
    // Start with no venue, as a fresh pool.
    env.edit::<Pool>(&env.pool(), |p| p.yield_venue = Pubkey::default());
    env.venue = None;
    let change = GovChange::YieldVenue(RESERVE);
    let (admin, co) = (env.admin.insecure_clone(), env.co_signer.insecure_clone());
    let ix = env.gov_ix(&admin.pubkey(), ProposeChange { change });
    env.ok(&[ix], &[&admin]);
    let ix = env.gov_ix(&co.pubkey(), ApproveChange { change });
    env.ok(&[ix], &[&co]);
    warp(&mut env, pool_core::params::GOV_DELAY_SECS);
    let anyone = env.funded(0);
    let kind = safu_pool::constants::GOV_KIND_YIELD_VENUE;
    let execute = |env: &mut Env, extra: Option<Pubkey>| {
        let mut ix = env.execute_change_ix(kind, None);
        if let Some(a) = extra {
            ix.accounts.push(
                anchor_lang::solana_program::instruction::AccountMeta::new_readonly(a, false),
            );
        }
        env.send(&[ix], &[&anyone])
    };
    // The reserve must come with the call, and be the one proposed.
    assert_err(execute(&mut env, None), PoolError::YieldAccountsMissing);
    assert_err(
        execute(&mut env, Some(MARKET)),
        PoolError::YieldAccountsMissing,
    );
    execute(&mut env, Some(RESERVE)).unwrap();
    assert_eq!(env.pool_state().yield_venue, RESERVE);

    // Money in the venue: no change of venue can even be proposed.
    env.venue = Some(Venue {
        reserve: RESERVE,
        market: MARKET,
        authority: Pubkey::find_program_address(
            &[k::LENDING_MARKET_AUTH_SEED, MARKET.as_ref()],
            &KLEND_PROGRAM_ID,
        )
        .0,
        supply: SUPPLY_VAULT,
        collateral_mint: COLLATERAL_MINT,
        pool_collateral: pool_collateral(&env),
        oracle: SCOPE,
    });
    fresh_prices(&mut env);
    env.matured_backer(100_000 * USDC);
    assert!(env.pool_state().deployed_collateral > 0);
    let off = GovChange::YieldVenue(Pubkey::default());
    let ix = env.gov_ix(&admin.pubkey(), ProposeChange { change: off });
    assert_err(
        env.send(&[ix], &[&admin]),
        PoolError::VenueChangeWhileDeployed,
    );
}

/// Wire size of a one-signer legacy transaction carrying `ix` and `extra` more writable accounts
/// (the worst-case upkeep places).
fn legacy_size(ix: &Instruction, payer: &Pubkey, extra: usize) -> usize {
    let mut ix = ix.clone();
    for _ in 0..extra {
        ix.accounts
            .push(anchor_lang::solana_program::instruction::AccountMeta::new(
                Pubkey::new_unique(),
                false,
            ));
    }
    let msg = solana_message::Message::new(&[ix], Some(payer));
    1 + 64 + msg.serialize().len()
}

#[test]
fn transaction_sizes_with_the_venue_and_full_upkeep() {
    let env = setup(5_000);
    let s = Pubkey::new_unique();
    // Head of the line plus `UPKEEP_WATCH_PER_CALL` watch places, 4 accounts each.
    let upkeep = (1 + pool_core::params::UPKEEP_WATCH_PER_CALL) * 4;
    let tx = [7u8; 32];
    for (name, ix) in [
        ("stake", env.stake_ix(&s, USDC, s)),
        ("withdraw", env.withdraw_ix(&s, &s)),
        ("claim_stream", env.stream_ix(&s, &tx, &s)),
        ("claim_yield", env.claim_yield_ix(&s, &s)),
        ("complete_backer", env.complete_backer_ix(&s)),
    ] {
        let bare = legacy_size(&ix, &s, 0);
        let full = legacy_size(&ix, &s, upkeep);
        println!("{name}: {bare} bytes, {full} with full upkeep (limit 1232)");
        // Room left for the compute-budget instructions (~64 bytes).
        assert!(full <= 1232 - 64, "{name} {full}");
    }
}
