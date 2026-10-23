//! A real Kamino Lend market in LiteSVM, shared by the yield-leg tests and the fuzz: mainnet klend
//! (`fixtures/klend.so`) with the main-market USDC reserve (Scope-priced) or the devnet USDC reserve
//! (Pyth-priced), loaded from dumped fixtures. Detail on the dumps: `tests/kamino.rs`.

use super::*;
use anchor_lang::solana_program::instruction::Instruction;
use base64::Engine;
use pool_core::kamino as k;
use safu_pool::constants::{KLEND_PROGRAM_ID, SEED_LEG};
use safu_pool::state::Pool;
use solana_account::Account;
use solana_signer::Signer;

pub const USDC_MINT: Pubkey =
    anchor_lang::prelude::pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
pub const RESERVE: Pubkey =
    anchor_lang::prelude::pubkey!("D6q6wuQSrifJKZYpR1M8R4YawnLDtDsMmWM1NbBmgJ59");
pub const MARKET: Pubkey =
    anchor_lang::prelude::pubkey!("7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF");
pub const SCOPE: Pubkey =
    anchor_lang::prelude::pubkey!("3t4JZcueEzTbVP6kLxXrL3VpWx45jDer4eqysweBchNH");
pub const COLLATERAL_MINT: Pubkey =
    anchor_lang::prelude::pubkey!("B8V6WVjPxW1UGwVDfxH2d2r8SyT4cqn7dQRK6XneVa7D");
pub const SUPPLY_VAULT: Pubkey =
    anchor_lang::prelude::pubkey!("Bgq7trRgVMeq33yt235zM2onQ4bRDBsY5EWiTetF4qw6");
/// Mainnet slot and unix time when the fixtures were dumped.
pub const DUMP_SLOT: u64 = 460_019_490;
pub const DUMP_UNIX: i64 = 1_792_668_744;
/// Mainnet slots per second (400 ms slots), to move the slot with the clock.
pub const SLOTS_PER_SEC: u64 = 5;

/// Scope `OraclePrices`: discriminator, mapping key, then 512 `DatedPrice` of 56 bytes each, whose
/// slot and unix time sit at +16 and +24.
pub const SCOPE_PRICES_AT: usize = 8 + 32;
pub const DATED_PRICE_LEN: usize = 56;

pub fn fixture(name: &str) -> Account {
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
pub fn warp(env: &mut Env, secs: i64) {
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
pub fn fresh_prices(env: &mut Env) {
    let c: anchor_lang::prelude::Clock = env.svm.get_sysvar();
    let mut a = env.svm.get_account(&SCOPE).unwrap();
    for i in 0..512 {
        let at = SCOPE_PRICES_AT + i * DATED_PRICE_LEN;
        a.data[at + 16..at + 24].copy_from_slice(&c.slot.to_le_bytes());
        a.data[at + 24..at + 32].copy_from_slice(&(c.unix_timestamp as u64).to_le_bytes());
    }
    env.svm.set_account(SCOPE, a).unwrap();
}

pub fn venue(env: &Env) -> &Venue {
    env.venue.as_ref().unwrap()
}

pub fn reserve(env: &Env) -> k::Reserve {
    k::parse_reserve(&env.svm.get_account(&venue(env).reserve).unwrap().data).unwrap()
}

pub fn pool_collateral(env: &Env) -> Pubkey {
    venue(env).pool_collateral
}

/// A klend market as dumped: its USDC mint, accounts and fixture names, and the clock to start at.
pub struct Market {
    pub usdc: Pubkey,
    pub reserve: (Pubkey, &'static str),
    pub market: (Pubkey, &'static str),
    pub oracle: (Pubkey, &'static str),
    pub collateral_mint: (Pubkey, &'static str),
    pub supply: (Pubkey, &'static str),
    pub slot: u64,
    pub unix: i64,
}

pub const MAINNET: Market = Market {
    usdc: USDC_MINT,
    reserve: (RESERVE, "klend_usdc_reserve"),
    market: (MARKET, "klend_main_market"),
    oracle: (SCOPE, "scope_prices"),
    collateral_mint: (COLLATERAL_MINT, "klend_usdc_collateral_mint"),
    supply: (SUPPLY_VAULT, "klend_usdc_supply_vault"),
    slot: DUMP_SLOT + 10,
    unix: DUMP_UNIX,
};

/// Pyth `PriceUpdateV2` (fully verified): publish time and previous publish time at +93 / +101.
pub const PYTH_PUBLISH_AT: usize = 93;
pub const PYTH_PRICE: Pubkey =
    anchor_lang::prelude::pubkey!("Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX");

pub const DEVNET: Market = Market {
    usdc: anchor_lang::prelude::pubkey!("4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU"),
    reserve: (
        anchor_lang::prelude::pubkey!("HRwMj8uuoGVWCanKzKvpTWN5ZvXjtjKGxcFbn2qTPKMW"),
        "klend_devnet_usdc_reserve",
    ),
    market: (
        anchor_lang::prelude::pubkey!("6aaNTBEmwdN19AAdTwbNrWyUo6iEyiLguxCTePEzSqoH"),
        "klend_devnet_market",
    ),
    oracle: (PYTH_PRICE, "pyth_devnet_usdc_price"),
    collateral_mint: (
        anchor_lang::prelude::pubkey!("6FY2rwh5wWrtSveAG9t9ANc2YsrChNasVSEpMQubJcXd"),
        "klend_devnet_usdc_collateral_mint",
    ),
    supply: (
        anchor_lang::prelude::pubkey!("6icVFmuKEsH5dzDwTSrxzrnJ14N27gDKRc2XAxPtB4ep"),
        "klend_devnet_usdc_supply_vault",
    ),
    slot: 514_196_601,
    unix: 1_792_746_643,
};

/// Stamps the Pyth price with the current clock, as Pyth's price pusher would.
pub fn fresh_pyth(env: &mut Env) {
    let c: anchor_lang::prelude::Clock = env.svm.get_sysvar();
    let mut a = env.svm.get_account(&PYTH_PRICE).unwrap();
    for at in [PYTH_PUBLISH_AT, PYTH_PUBLISH_AT + 8] {
        a.data[at..at + 8].copy_from_slice(&c.unix_timestamp.to_le_bytes());
    }
    env.svm.set_account(PYTH_PRICE, a).unwrap();
}

/// A pool on the market's USDC mint with klend and the reserve loaded and the venue set (as
/// governance would, with nothing deployed), `deploy_bps` of capacity allowed out, the collateral
/// account open.
pub fn setup_on(m: &Market, deploy_bps: u64) -> Env {
    let mut env = Env::new_at(m.usdc, m.unix);
    env.svm
        .add_program(KLEND_PROGRAM_ID, include_bytes!("../fixtures/klend.so"))
        .unwrap();
    for (key, name) in [m.reserve, m.market, m.oracle, m.collateral_mint, m.supply] {
        env.svm.set_account(key, fixture(name)).unwrap();
    }
    let mut c: anchor_lang::prelude::Clock = env.svm.get_sysvar();
    c.slot = m.slot;
    env.svm.set_sysvar(&c);
    if m.oracle.0 == SCOPE {
        fresh_prices(&mut env);
    } else {
        fresh_pyth(&mut env);
    }
    env.edit::<Pool>(&env.pool(), |p| {
        p.yield_venue = m.reserve.0;
        p.deploy_bps = deploy_bps;
    });
    let authority = Pubkey::find_program_address(
        &[k::LENDING_MARKET_AUTH_SEED, m.market.0.as_ref()],
        &KLEND_PROGRAM_ID,
    )
    .0;
    let pool_collateral = pda(&[SEED_LEG, env.pool().as_ref(), m.collateral_mint.0.as_ref()]);
    env.venue = Some(Venue {
        reserve: m.reserve.0,
        market: m.market.0,
        authority,
        supply: m.supply.0,
        collateral_mint: m.collateral_mint.0,
        pool_collateral,
        oracle: m.oracle.0,
    });
    let payer = env.admin.insecure_clone();
    let ix = init_yield_account_ix(&env, &payer.pubkey());
    env.ok(&[ix], &[&payer]);
    env
}

pub fn setup(deploy_bps: u64) -> Env {
    setup_on(&MAINNET, deploy_bps)
}

pub fn init_yield_account_ix(env: &Env, payer: &Pubkey) -> Instruction {
    env.ix(
        safu_pool::accounts::InitYieldAccount {
            payer: *payer,
            pool: env.pool(),
            reserve: venue(env).reserve,
            collateral_mint: venue(env).collateral_mint,
            pool_collateral: pool_collateral(env),
            token_program: TOKEN,
            system_program: SYSTEM,
        },
        safu_pool::instruction::InitYieldAccount {},
    )
}

pub fn leg_call(env: &Env, data: impl anchor_lang::InstructionData) -> Instruction {
    env.ix(
        safu_pool::accounts::LegCall {
            pool: env.pool(),
            vault: env.vault(),
            cash: env.cash(),
        },
        data,
    )
}

pub fn collateral_held(env: &Env) -> u64 {
    env.token_amount(&pool_collateral(env))
}

pub fn provide_ix(env: &Env, collateral: u64, min_out: u64) -> Instruction {
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
pub fn deploy(env: &mut Env) -> u64 {
    let before = collateral_held(env);
    let anyone = env.funded(0);
    let ix = leg_call(env, safu_pool::instruction::AutoDeployLiquidity {});
    env.ok(&[ix], &[&anyone]);
    collateral_held(env) - before
}

/// Anyone's klend `refresh_reserve`, as mainnet users send all the time: accrues interest.
pub fn refresh_reserve(env: &mut Env) {
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
pub fn drain_reserve(env: &mut Env) {
    let mut a = env.svm.get_account(&RESERVE).unwrap();
    a.data[k::R_LIQ_AVAILABLE..k::R_LIQ_AVAILABLE + 8].copy_from_slice(&0u64.to_le_bytes());
    env.svm.set_account(RESERVE, a).unwrap();
}

pub fn admin_provide(env: &mut Env, collateral: u64, min_out: u64) -> Result<(), String> {
    let ix = provide_ix(env, collateral, min_out);
    let admin = env.admin.insecure_clone();
    env.send(&[ix], &[&admin])
}
