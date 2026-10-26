//! The yield venue slot: the one place the pool picks an adapter for `Pool.yield_venue`. The money
//! logic (`crate::leg`) talks only to `Venue` and `Quote`; each adapter (today `crate::kamino`) owns
//! its accounts, its checks and its maths.
//!
//! The adapter is chosen by the program that owns the venue account, matched against program ids
//! compiled into this program. An unknown owner is refused, so governance can only point the pool at
//! a venue whose adapter was audited with the program; adding one needs a program upgrade.
//!
//! Rules for an adapter:
//! - Check owner, discriminator and length before reading any byte; fail closed on a layout change.
//! - Check every account it is passed against the venue account before any call (arbitrary-CPI guard).
//! - Check in advance everything its deposit and redeem need (`deposit_ok`, `redeem_ok`): Solana cannot
//!   catch a failed call, and a best-effort step must skip, never fail the staker's call.
//! - A program owning more than one account type tells them apart by discriminator inside its arm.
//! - Conversion maths stays in the adapter: venues round and scale differently.

use anchor_lang::prelude::*;
use pool_core::kamino as k;

use crate::constants::KLEND_PROGRAM_ID;
use crate::errors::{CoreResultExt, PoolError};
use crate::kamino::{self, Leg};
use crate::state::Pool;
use crate::vault::Cash;

/// A venue's state as last read: what the money logic needs to size a call or count growth.
#[derive(Clone, Copy)]
pub enum Quote {
    Kamino(k::Reserve),
}

impl Quote {
    /// Venue units (collateral) minted for `liquidity` USDC, rounded down.
    pub fn collateral_for(&self, liquidity: u64) -> Result<u64> {
        match self {
            Quote::Kamino(r) => r.collateral_for(liquidity).core(),
        }
    }

    /// USDC that `collateral` redeems for, rounded down.
    pub fn liquidity_for(&self, collateral: u64) -> Result<u64> {
        match self {
            Quote::Kamino(r) => r.liquidity_for(collateral).core(),
        }
    }

    /// The mint of the venue units the pool holds (its collateral account's mint).
    pub fn collateral_mint(&self) -> Pubkey {
        match self {
            Quote::Kamino(r) => Pubkey::new_from_array(r.collateral_mint),
        }
    }
}

/// Reads a venue account, any pool (views).
pub fn read(info: &AccountInfo) -> Result<Quote> {
    if *info.owner == KLEND_PROGRAM_ID {
        return Ok(Quote::Kamino(kamino::read_reserve(info)?));
    }
    err!(PoolError::VenueNotSupported)
}

/// A venue this pool can use: a supported adapter, and the adapter's own checks against the pool.
pub fn check(pool: &Pool, info: &AccountInfo) -> Result<Quote> {
    if *info.owner == KLEND_PROGRAM_ID {
        return Ok(Quote::Kamino(kamino::check_venue(pool, info)?));
    }
    err!(PoolError::VenueNotSupported)
}

/// The venue, loaded and checked, for one instruction.
pub enum Venue<'info> {
    Kamino(Leg<'info>),
}

impl<'info> Venue<'info> {
    /// `None` while the pool has no venue, or before its collateral account exists (then nothing can
    /// have been deployed). The venue account must be `pool.yield_venue`.
    pub fn load(cash: &Cash<'info>, pool: &Pool, pool_key: &Pubkey) -> Result<Option<Self>> {
        if pool.yield_venue == Pubkey::default() {
            return Ok(None);
        }
        let info = cash
            .reserve
            .as_ref()
            .ok_or_else(|| error!(PoolError::YieldAccountsMissing))?;
        require_keys_eq!(
            info.key(),
            pool.yield_venue,
            PoolError::YieldAccountsMissing
        );
        if *info.owner == KLEND_PROGRAM_ID {
            return Ok(cash.load_leg(pool, pool_key)?.map(Venue::Kamino));
        }
        err!(PoolError::VenueNotSupported)
    }

    /// The venue as last read (refreshed by `refresh`, re-read after every call).
    pub fn quote(&self) -> Quote {
        match self {
            Venue::Kamino(l) => Quote::Kamino(l.state),
        }
    }

    /// Brings the venue's rate up to date, once per instruction, before any money moves.
    pub fn refresh(&mut self) -> Result<()> {
        match self {
            Venue::Kamino(l) => l.refresh(),
        }
    }

    /// A deposit of `amount` USDC would go through now.
    pub fn deposit_ok(&self, amount: u64) -> Result<bool> {
        match self {
            Venue::Kamino(l) => l.state.deposit_ok(amount, l.market_emergency).core(),
        }
    }

    /// A redeem of `collateral` would go through now and pay out at least one unit.
    pub fn redeem_ok(&self, collateral: u64, now: i64) -> Result<bool> {
        match self {
            Venue::Kamino(l) => l
                .state
                .redeem_ok(collateral, l.market_emergency, now)
                .core(),
        }
    }

    /// Venue units in the pool's collateral account, read now.
    pub fn collateral_held(&self) -> Result<u64> {
        match self {
            Venue::Kamino(l) => l.collateral_held(),
        }
    }

    /// Deposits `amount` USDC from the vault; venue units go to the pool's collateral account.
    pub fn deposit(
        &mut self,
        pool: &Pool,
        pool_info: &AccountInfo<'info>,
        cash: &Cash<'info>,
        vault: &AccountInfo<'info>,
        amount: u64,
    ) -> Result<()> {
        match self {
            Venue::Kamino(l) => l.deposit(pool, pool_info, cash, vault, amount),
        }
    }

    /// Redeems `collateral` venue units; USDC goes to the vault.
    pub fn redeem(
        &mut self,
        pool: &Pool,
        pool_info: &AccountInfo<'info>,
        cash: &Cash<'info>,
        vault: &AccountInfo<'info>,
        collateral: u64,
    ) -> Result<()> {
        match self {
            Venue::Kamino(l) => l.redeem(pool, pool_info, cash, vault, collateral),
        }
    }
}
