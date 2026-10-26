//! The yield leg against the real Kamino Lend program: mainnet klend (`fixtures/klend.so`) and its
//! main-market USDC reserve, market and Scope price account as dumped 2026-10-22 (mainnet slot
//! 460019490). The pool's USDC mint sits at the real USDC address, so the reserve accepts it.
//!
//! Scope prices carry their own timestamps: `fresh_prices` stamps them with the test clock, as a
//! live Scope keeper would; leaving them alone is the stale-price case.
//!
//! The devnet markets are priced by Pyth instead: `DEVNET` is the live devnet USDC reserve on Circle's
//! devnet USDC, its market and its Pyth price account as dumped 2026-10-23 (devnet slot 514196601),
//! run against the same mainnet klend binary.
mod common;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::Instruction;
use common::klend::*;
use common::*;
use pool_core::kamino as k;
use safu_pool::constants::{KLEND_PROGRAM_ID, SEED_LEG};
use safu_pool::state::Pool;
use solana_signer::Signer;

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

/// The busiest a user call gets: harvest on a stale price, the push of new cash into klend, then a
/// full upkeep (the head of the line released, both watch places expiring their claims). It still
/// fits the site's compute limit.
#[test]
fn a_backing_with_harvest_push_and_full_upkeep_fits_the_site_limit() {
    use safu_pool::constants::TIER_A;
    use safu_pool::state::ClaimStatus;
    let mut env = setup(5_000);
    env.matured_backer(160 * unit());
    let (_, max) = bounds();
    // Two admitted claims (watch places 0 and 1), a day apart for the oracle's daily limit.
    let txs = [[21u8; 32], [22u8; 32]];
    let claimants: Vec<_> = txs.iter().map(|_| env.staker(max)).collect();
    for (i, (s, tx)) in claimants.iter().zip(&txs).enumerate() {
        if i > 0 {
            warp(&mut env, 86_400);
        }
        let a = env.approval(&s.pubkey(), *tx, max, TIER_A);
        env.submit(&a).unwrap();
    }
    // One claim in line: too big for the pool as it is.
    let q = env.staker(max);
    let a = env.approval(&q.pubkey(), TX, env.tier_cap(max, TIER_A), TIER_A);
    env.submit(&a).unwrap();
    assert_eq!(
        env.claim_state(&q.pubkey(), &TX).status,
        ClaimStatus::Reserved
    );
    // Turn the watch cursor so this call's two watch places are the two admitted claims (places
    // 0 and 1); the calls that turn it run before any window ends, so they change nothing else.
    for _ in 0..3 {
        let pool = env.pool_state();
        let places =
            pool_core::upkeep::watch_places(pool.watch_head, pool.watch_tail, pool.watch_cursor);
        if places[..2] == [Some(0), Some(1)] {
            break;
        }
        let b = env.funded(USDC);
        let ix = env.back_ix(&b.pubkey(), USDC);
        env.send(&[ix], &[&b]).unwrap();
    }
    // Past both watched claims' approve deadlines, a new day, no Scope update.
    let deadline = claimants
        .iter()
        .zip(&txs)
        .map(|(c, tx)| {
            let r = env.stake_state(&c.pubkey());
            r.staked_at + r.time_gate_secs + env.claim_state(&c.pubkey(), tx).approve_window
        })
        .max()
        .unwrap();
    let wait = (deadline + 86_400 - env.now).max(86_400);
    warp(&mut env, wait);
    let harvested_at = env.pool_state().last_harvest_at;
    // A large backing makes room for the claim in line.
    let b = env.funded(100_000 * USDC);
    let ix = env.back_ix(&b.pubkey(), 100_000 * USDC);
    let cu = env.send_cu(&[ix], &[&b]).unwrap();
    println!("backing with harvest, push and full upkeep: {cu} compute units");
    assert_ne!(
        env.pool_state().last_harvest_at,
        harvested_at,
        "harvest ran"
    );
    assert_ne!(
        env.claim_state(&q.pubkey(), &TX).status,
        ClaimStatus::Reserved,
        "line released"
    );
    for (s, tx) in claimants.iter().zip(&txs) {
        assert_eq!(
            env.claim_state(&s.pubkey(), tx).status,
            ClaimStatus::Expired
        );
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
        pool_collateral: pda(&[SEED_LEG, env.pool().as_ref(), COLLATERAL_MINT.as_ref()]),
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

/// Proposes, approves and (after the wait) executes a venue change, passing `extra` with the call.
fn change_venue(env: &mut Env, to: Pubkey, extra: Option<Pubkey>) -> Result<(), String> {
    use safu_pool::instruction::{ApproveChange, ProposeChange};
    use safu_pool::state::GovChange;
    let change = GovChange::YieldVenue(to);
    let (admin, co) = (env.admin.insecure_clone(), env.co_signer.insecure_clone());
    let ix = env.gov_ix(&admin.pubkey(), ProposeChange { change });
    env.send(&[ix], &[&admin])?;
    let ix = env.gov_ix(&co.pubkey(), ApproveChange { change });
    env.send(&[ix], &[&co])?;
    warp(env, pool_core::params::GOV_DELAY_SECS);
    let anyone = env.funded(0);
    let mut ix = env.execute_change_ix(safu_pool::constants::GOV_KIND_YIELD_VENUE, None);
    if let Some(a) = extra {
        ix.accounts
            .push(anchor_lang::solana_program::instruction::AccountMeta::new_readonly(a, false));
    }
    env.send(&[ix], &[&anyone])
}

#[test]
fn governance_refuses_a_venue_whose_program_has_no_adapter() {
    let mut env = setup(5_000);
    env.edit::<Pool>(&env.pool(), |p| p.yield_venue = Pubkey::default());
    env.venue = None;
    // A token account: owned by the token program, which no adapter serves.
    let vault = env.vault();
    assert_err(
        change_venue(&mut env, vault, Some(vault)),
        safu_pool::errors::PoolError::VenueNotSupported,
    );
    assert_eq!(env.pool_state().yield_venue, Pubkey::default());
}

#[test]
fn a_venue_account_moved_to_another_program_is_refused_on_load_and_in_views() {
    let mut env = setup(5_000);
    let b = env.matured_backer(100_000 * USDC);
    assert!(env.pool_state().deployed_collateral > 0);
    // The venue account now belongs to a program with no adapter.
    let reserve_key = venue(&env).reserve;
    let mut a = env.svm.get_account(&reserve_key).unwrap();
    a.owner = Pubkey::new_unique();
    env.svm.set_account(reserve_key, a).unwrap();
    let anyone = env.funded(0);
    let ix = leg_call(&env, safu_pool::instruction::EnsureLiquidity {});
    assert_err(
        env.send(&[ix], &[&anyone]),
        safu_pool::errors::PoolError::VenueNotSupported,
    );
    let ix = env.ix(
        safu_pool::accounts::ViewBacker {
            pool: env.pool(),
            backer_record: env.backer_record(&b.pubkey()),
            reserve: Some(reserve_key),
        },
        safu_pool::instruction::ViewBacker {},
    );
    assert_err(
        env.send(&[ix], &[&anyone]),
        safu_pool::errors::PoolError::VenueNotSupported,
    );
}

#[test]
fn a_full_exit_lets_governance_switch_the_venue() {
    let mut env = setup(5_000);
    env.matured_backer(100_000 * USDC);
    let p = env.pool_state();
    // Everything back from the venue: both books end at exactly zero.
    admin_provide(&mut env, p.deployed_collateral, 0).unwrap();
    let p = env.pool_state();
    assert_eq!((p.deployed_book, p.deployed_collateral), (0, 0));
    // The venue can now be turned off and set again.
    change_venue(&mut env, Pubkey::default(), None).unwrap();
    assert_eq!(env.pool_state().yield_venue, Pubkey::default());
    change_venue(&mut env, RESERVE, Some(RESERVE)).unwrap();
    assert_eq!(env.pool_state().yield_venue, RESERVE);
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

#[test]
fn a_pyth_priced_reserve_takes_and_returns_usdc_by_cpi() {
    // The devnet market: priced by Pyth, so the price account goes in klend's Pyth slot.
    let mut env = setup_on(&DEVNET, 5_000);
    assert_eq!(
        reserve(&env).price,
        k::PriceSource::Pyth(PYTH_PRICE.to_bytes())
    );
    let r = reserve(&env);
    env.matured_backer(10_000 * USDC);
    let p = env.pool_state();
    assert_eq!(p.deployed_book, 5_000 * USDC);
    assert_eq!(collateral_held(&env), p.deployed_collateral);
    assert_eq!(reserve(&env).available, r.available + 5_000 * USDC);
    admin_provide(&mut env, p.deployed_collateral, 5_000 * USDC - 2).unwrap();
    assert_eq!(env.pool_state().deployed_collateral, 0);
}

#[test]
fn only_the_reserves_own_price_account_is_accepted_and_two_sources_are_not_a_venue() {
    let mut env = setup_on(&DEVNET, 5_000);
    env.matured_backer(10_000 * USDC);
    // The mainnet Scope account in place of the reserve's Pyth feed: refused before any klend call.
    let good = env.venue.unwrap();
    env.venue = Some(Venue {
        oracle: SCOPE,
        ..good
    });
    let anyone = env.funded(0);
    let ix = leg_call(&env, safu_pool::instruction::EnsureLiquidity {});
    assert_err(
        env.send(&[ix], &[&anyone]),
        safu_pool::errors::PoolError::YieldAccountsMissing,
    );
    // The reserve gains a second source (Scope next to Pyth): one price account can't serve it.
    env.venue = Some(good);
    let mut a = env.svm.get_account(&DEVNET.reserve.0).unwrap();
    a.data[k::R_SCOPE_FEED..k::R_SCOPE_FEED + 32].copy_from_slice(&SCOPE.to_bytes());
    env.svm.set_account(DEVNET.reserve.0, a).unwrap();
    let ix = leg_call(&env, safu_pool::instruction::EnsureLiquidity {});
    assert_err(
        env.send(&[ix], &[&anyone]),
        safu_pool::errors::PoolError::VenueNotKamino,
    );
}

#[test]
fn views_count_no_growth_while_paused() {
    let mut env = setup(5_000);
    let b = env.matured_backer(100_000 * USDC);
    env.pause();
    warp(&mut env, 86_400);
    fresh_prices(&mut env);
    refresh_reserve(&mut env);
    let p = env.pool_state();
    assert!(reserve(&env).liquidity_for(p.deployed_collateral).unwrap() > p.deployed_book);
    // The growth is there, but no harvest runs while paused, so the views leave it out.
    let seen = env.view_backer(&b.pubkey()).yield_owed;
    let v = env.venue.take();
    let none = env.view_backer(&b.pubkey()).yield_owed;
    env.venue = v;
    assert_eq!(seen, none);
}

#[test]
fn a_venue_loss_is_marked_off_the_stakes() {
    let mut env = setup(8_000);
    env.matured_backer(100_000 * USDC);
    let (_, max) = bounds();
    let s = env.staker(max);
    env.staker(max);
    let idle = env.vault_liquid();
    // The whole withdrawal comes from klend, after the reserve lost 0.01% of its value (a loss
    // smaller than the stakes left, so none of it is cut off at zero).
    env.edit::<Pool>(&env.pool(), |p| p.rounding_reserve = idle);
    cut_reserve_value(&mut env, 1);
    let before = env.pool_state();
    let ix = env.withdraw_part_ix(&s.pubkey(), &s.pubkey(), max);
    let logs = env.send_logs(&[ix], &[&s]).unwrap();
    let short: safu_pool::events::DeploymentShortfall =
        event(&logs).expect("the redeem came back short of its book value");
    let loss = short.principal_expected - short.received;
    assert!(loss > 0);
    assert_eq!(env.usdc(&s.pubkey()), max);
    assert_eq!(
        env.pool_state().total_staked,
        before.total_staked - max - loss
    );
}

#[test]
fn a_withdrawal_reverts_if_a_venue_loss_on_the_way_out_leaves_open_claims_uncovered() {
    let (_, max) = bounds();
    let run = |loss_bps: u64| {
        let mut env = setup(8_000);
        env.matured_backer(100_000 * USDC);
        let s = env.staker(max);
        env.staker(max);
        let idle = env.vault_liquid();
        let p = env.pool_state();
        let capacity = pool_core::capacity(p.total_staked, p.total_backed).unwrap();
        // Open claims hold all the capital but this withdrawal and a 10-unit margin.
        env.edit::<Pool>(&env.pool(), |p| {
            p.rounding_reserve = idle;
            p.total_allocated = capacity - max - 10;
        });
        if loss_bps > 0 {
            cut_reserve_value(&mut env, loss_bps);
        }
        let ix = env.withdraw_part_ix(&s.pubkey(), &s.pubkey(), max);
        env.send(&[ix], &[&s])
    };
    // klend's rounding fits the margin; a 1% loss does not, and the whole payment reverts.
    run(0).unwrap();
    assert_err(run(100), safu_pool::errors::PoolError::CapitalNotFree);
}

#[test]
fn a_yield_account_opens_only_for_the_reserves_collateral_mint() {
    let mut env = setup(5_000);
    let payer = env.funded(0);
    let mut ix = init_yield_account_ix(&env, &payer.pubkey());
    let (real_mint, real_account) = (venue(&env).collateral_mint, pool_collateral(&env));
    // USDC is the reserve's liquidity mint, not its collateral mint.
    let wrong = env.usdc_mint;
    let wrong_account = Pubkey::find_program_address(
        &[SEED_LEG, env.pool().as_ref(), wrong.as_ref()],
        &safu_pool::ID,
    )
    .0;
    for m in ix.accounts.iter_mut() {
        if m.pubkey == real_mint {
            m.pubkey = wrong;
        } else if m.pubkey == real_account {
            m.pubkey = wrong_account;
        }
    }
    assert_err(
        env.send(&[ix], &[&payer]),
        safu_pool::errors::PoolError::YieldAccountsMissing,
    );
}

/// While paused nothing moves into or out of klend on its own: a harvest takes no growth and backing
/// that matures stays in the vault. Once unpaused both go ahead.
#[test]
fn nothing_goes_into_or_out_of_klend_on_its_own_while_paused() {
    use pool_core::settings::SettingKey;
    use safu_pool::events::{DeployBelowFloor, Deployed};
    let mut env = setup(5_000);
    // An hour to mature (inside the setting's range on both builds).
    env.set_setting(SettingKey::BackerMaturitySecs, 3_600);
    env.matured_backer(100_000 * USDC);
    // 50,000 more backing, not yet capacity: nothing to push for it yet.
    let b = env.funded(50_000 * USDC);
    let ix = env.back_ix(&b.pubkey(), 50_000 * USDC);
    env.ok(&[ix], &[&b]);
    assert_eq!(env.pool_state().deployed_book, 50_000 * USDC);
    env.pause();
    warp(&mut env, 86_400);
    fresh_prices(&mut env);
    let before = env.pool_state();
    let (held, liquid) = (collateral_held(&env), env.vault_liquid());
    let anyone = env.funded(0);
    let ix = leg_call(&env, safu_pool::instruction::Harvest {});
    env.ok(&[ix], &[&anyone]);
    // The backing matures (permissionless, pause or not): capacity, but no push while paused.
    let ix = env.mature_ix(&b.pubkey());
    env.ok(&[ix], &[&b]);
    let p = env.pool_state();
    assert_eq!(p.total_backed, before.total_backed + 50_000 * USDC);
    assert_eq!(
        (p.deployed_book, p.deployed_collateral, p.last_harvest_at),
        (
            before.deployed_book,
            before.deployed_collateral,
            before.last_harvest_at
        )
    );
    assert_eq!(p.total_extracted_yield, before.total_extracted_yield);
    assert_eq!((collateral_held(&env), env.vault_liquid()), (held, liquid));

    // Unpaused: the harvest takes the day's growth, and the keeper pushes the matured backing to
    // the line (half of 150,000), at the reserve's own rate.
    env.unpause();
    let ix = leg_call(&env, safu_pool::instruction::Harvest {});
    env.ok(&[ix], &[&anyone]);
    assert!(env.pool_state().total_extracted_yield > before.total_extracted_yield);
    let ix = leg_call(&env, safu_pool::instruction::AutoDeployLiquidity {});
    let logs = env.send_logs(&[ix], &[&anyone]).unwrap();
    let d: Deployed = event(&logs).expect("the keeper deposited");
    assert_eq!(d.amount, 25_000 * USDC);
    assert!(event::<DeployBelowFloor>(&logs).is_none());
    assert_eq!(env.pool_state().deployed_book, 75_000 * USDC);
}

/// The keeper's rebalance redeems the deployment above the `deploy_bps` line, books the growth
/// that came back with it as yield (no shortfall), and with nothing above the line does nothing.
#[test]
fn the_keeper_rebalance_brings_deployment_back_to_the_line() {
    use safu_pool::events::{DeploymentShortfall, Redeemed, LEG_KEEPER_REBALANCE};
    let mut env = setup(5_000);
    env.matured_backer(100_000 * USDC);
    // 50,000 deployed; the line moves down to 40% of the 100,000 capacity: 10,000 above it.
    env.edit::<Pool>(&env.pool(), |p| p.deploy_bps = 4_000);
    warp(&mut env, 86_400);
    fresh_prices(&mut env);
    let before = env.pool_state();
    let liquid = env.vault_liquid();
    let anyone = env.funded(0);
    let ix = leg_call(&env, safu_pool::instruction::EnsureLiquidity {});
    let logs = env.send_logs(&[ix], &[&anyone]).unwrap();
    let r: Redeemed = event(&logs).expect("the keeper redeemed");
    assert_eq!(r.kind, LEG_KEEPER_REBALANCE);
    let after = env.pool_state();
    // Collateral for 10,000 of book, rounded up: the book left is the line, or a unit under it.
    assert!(after.deployed_book <= 40_000 * USDC && after.deployed_book + 1 >= 40_000 * USDC);
    assert_eq!(
        after.deployed_collateral,
        before.deployed_collateral - r.collateral
    );
    assert_eq!(env.vault_liquid(), liquid + r.received);
    // A day of interest came back with it: more than the book that left, so yield and no loss.
    let principal = before.deployed_book - after.deployed_book;
    assert!(r.received > principal);
    assert_eq!(
        after.total_extracted_yield - before.total_extracted_yield,
        r.received - principal
    );
    assert_eq!(after.total_staked, before.total_staked);
    assert!(event::<DeploymentShortfall>(&logs).is_none());

    // At the line: the call goes through and changes nothing.
    let held = collateral_held(&env);
    let ix = leg_call(&env, safu_pool::instruction::EnsureLiquidity {});
    env.ok(&[ix], &[&anyone]);
    assert_eq!(collateral_held(&env), held);
    assert_eq!(env.pool_state().deployed_book, after.deployed_book);
}

/// A payment the vault's free cash covers never touches klend.
#[test]
fn a_payment_the_vault_covers_leaves_klend_alone() {
    let mut env = setup(5_000);
    env.matured_backer(100_000 * USDC);
    let (_, max) = bounds();
    let s = env.staker(max);
    let before = env.pool_state();
    let held = collateral_held(&env);
    assert!(env.vault_liquid() - before.rounding_reserve > max);
    let ix = env.withdraw_part_ix(&s.pubkey(), &s.pubkey(), max);
    env.ok(&[ix], &[&s]);
    assert_eq!(env.usdc(&s.pubkey()), max);
    let p = env.pool_state();
    assert_eq!(
        (p.deployed_book, p.deployed_collateral),
        (before.deployed_book, before.deployed_collateral)
    );
    assert_eq!(collateral_held(&env), held);
}

/// Short of cash, a payment pulls the shortfall plus the buffer: the free cash left after it is the
/// buffer (`capacity × (1 − deploy_bps)`), give or take klend's rounding.
#[test]
fn a_payment_short_of_cash_pulls_the_shortfall_and_refills_the_buffer() {
    let mut env = setup(8_000);
    env.matured_backer(100_000 * USDC);
    let (_, max) = bounds();
    let s = env.staker(max);
    // Half the payment is free in the vault; the rest of the vault's cash is out of reach.
    let free = max / 2;
    let liquid = env.vault_liquid();
    env.edit::<Pool>(&env.pool(), |p| p.rounding_reserve = liquid - free);
    let ix = env.withdraw_part_ix(&s.pubkey(), &s.pubkey(), max);
    env.ok(&[ix], &[&s]);
    assert_eq!(env.usdc(&s.pubkey()), max);
    let p = env.pool_state();
    assert_eq!(p.rounding_reserve, liquid - free);
    let capacity = pool_core::capacity(p.total_staked, p.total_backed).unwrap();
    let buffer = capacity * (10_000 - 8_000) / 10_000;
    let left = env.vault_liquid() - p.rounding_reserve;
    assert!(
        left.abs_diff(buffer) <= 2,
        "free after {left}, buffer {buffer}"
    );
}

/// With nothing deployed, a payment short of cash does not call klend at all: the claim counts a
/// cash-short day and the reserve account is untouched (no refresh).
#[test]
fn a_short_payment_with_nothing_deployed_never_calls_klend() {
    let mut env = setup(8_000);
    env.matured_backer(100_000 * USDC);
    let s = env.staker(bounds().1);
    vested_claim(&mut env, &s);
    let deployed = env.pool_state().deployed_collateral;
    admin_provide(&mut env, deployed, 0).unwrap();
    assert_eq!(env.pool_state().deployed_collateral, 0);
    warp(&mut env, 60);
    let liquid = env.vault_liquid();
    env.edit::<Pool>(&env.pool(), |p| p.rounding_reserve = liquid);
    let reserve_data = env.svm.get_account(&RESERVE).unwrap().data;
    let ix = env.stream_ix(&s.pubkey(), &TX, &s.pubkey());
    env.ok(&[ix], &[&s]);
    assert_eq!(env.pool_state().cash_short_days, 1);
    assert_eq!(env.svm.get_account(&RESERVE).unwrap().data, reserve_data);
}
