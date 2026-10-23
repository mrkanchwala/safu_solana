//! Kamino Lend (klend), the pool's yield venue: the accounts its calls need, the checks on them, and
//! the three calls the pool makes (`refresh_reserve`, `deposit_reserve_liquidity`,
//! `redeem_reserve_collateral`). Layout facts and reserve maths live in `pool_core::kamino`.
//!
//! The venue is `Pool.yield_venue`, a klend reserve set by governance. klend is upgradeable and
//! outside our control: owner, discriminator and length are checked before any byte is read, and
//! every account the pool is responsible for is checked against the reserve before any call
//! (arbitrary-CPI guard). The price account is klend's to check; the pool only passes it on.
//!
//! Instructions are built by hand (discriminator + accounts): no klend CPI crate, whose Anchor
//! version would fight this one.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;
use anchor_spl::token::TokenAccount;
use pool_core::kamino as k;

use crate::constants::{KLEND_PROGRAM_ID, SEED_LEG, SEED_POOL};
use crate::errors::{CoreResultExt, PoolError};
use crate::state::Pool;
use crate::vault::Cash;

/// The venue, loaded and checked, with the reserve as last read.
pub struct Leg<'info> {
    program: AccountInfo<'info>,
    reserve: AccountInfo<'info>,
    market: AccountInfo<'info>,
    authority: AccountInfo<'info>,
    supply: AccountInfo<'info>,
    collateral_mint: AccountInfo<'info>,
    pub pool_collateral: AccountInfo<'info>,
    oracle: AccountInfo<'info>,
    instructions: AccountInfo<'info>,
    /// The reserve: refreshed by klend once per instruction before any money moves, re-read after
    /// every call.
    pub state: k::Reserve,
    pub market_emergency: bool,
    refreshed: bool,
}

fn missing() -> Error {
    error!(PoolError::YieldAccountsMissing)
}

fn take<'info>(a: &Option<UncheckedAccount<'info>>) -> Result<AccountInfo<'info>> {
    a.as_ref().map(|a| a.to_account_info()).ok_or_else(missing)
}

/// Reads a klend reserve: owner, discriminator, length, then the fields.
pub fn read_reserve(info: &AccountInfo) -> Result<k::Reserve> {
    require_keys_eq!(*info.owner, KLEND_PROGRAM_ID, PoolError::VenueNotKamino);
    let d = info.try_borrow_data()?;
    k::parse_reserve(&d).map_err(|_| error!(PoolError::VenueNotKamino))
}

/// A reserve the pool can use as its venue: klend, the pool's USDC, classic SPL Token on both legs,
/// priced by Scope alone or Pyth alone (`k::PriceSource`).
pub fn check_venue(pool: &Pool, info: &AccountInfo) -> Result<k::Reserve> {
    let r = read_reserve(info)?;
    require!(
        Pubkey::new_from_array(r.liquidity_mint) == pool.usdc_mint
            && Pubkey::new_from_array(r.liquidity_token_program) == anchor_spl::token::ID
            && r.price != k::PriceSource::Other,
        PoolError::VenueNotKamino
    );
    Ok(r)
}

/// The pool's collateral account for `collateral_mint`.
pub fn collateral_address(pool_key: &Pubkey, collateral_mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[SEED_LEG, pool_key.as_ref(), collateral_mint.as_ref()],
        &crate::ID,
    )
    .0
}

impl<'info> Cash<'info> {
    /// The leg accounts (in `Cash`): `None` while the pool has no venue, or before its collateral account exists (then nothing can
    /// have been deployed). Otherwise every account, checked against the pool and the reserve.
    pub fn load_leg(&self, pool: &Pool, pool_key: &Pubkey) -> Result<Option<Leg<'info>>> {
        if pool.yield_venue == Pubkey::default() {
            return Ok(None);
        }
        let program = take(&self.klend_program)?;
        let reserve = take(&self.reserve)?;
        let market = take(&self.lending_market)?;
        let authority = take(&self.lending_market_authority)?;
        let supply = take(&self.reserve_liquidity_supply)?;
        let collateral_mint = take(&self.reserve_collateral_mint)?;
        let pool_collateral = take(&self.pool_collateral)?;
        let oracle = take(&self.oracle)?;
        let instructions = take(&self.instructions)?;

        require_keys_eq!(
            program.key(),
            KLEND_PROGRAM_ID,
            PoolError::YieldAccountsMissing
        );
        require_keys_eq!(
            reserve.key(),
            pool.yield_venue,
            PoolError::YieldAccountsMissing
        );
        let state = check_venue(pool, &reserve)?;
        require_keys_eq!(
            market.key(),
            Pubkey::new_from_array(state.lending_market),
            PoolError::YieldAccountsMissing
        );
        require_keys_eq!(
            *market.owner,
            KLEND_PROGRAM_ID,
            PoolError::YieldAccountsMissing
        );
        let market_emergency = k::market_emergency(&market.try_borrow_data()?).core()?;
        let (auth, _) = Pubkey::find_program_address(
            &[k::LENDING_MARKET_AUTH_SEED, market.key().as_ref()],
            &KLEND_PROGRAM_ID,
        );
        require_keys_eq!(authority.key(), auth, PoolError::YieldAccountsMissing);
        require_keys_eq!(
            supply.key(),
            Pubkey::new_from_array(state.supply_vault),
            PoolError::YieldAccountsMissing
        );
        require_keys_eq!(
            collateral_mint.key(),
            Pubkey::new_from_array(state.collateral_mint),
            PoolError::YieldAccountsMissing
        );
        require_keys_eq!(
            pool_collateral.key(),
            collateral_address(pool_key, &collateral_mint.key()),
            PoolError::YieldAccountsMissing
        );
        require!(
            state.price.account() == Some(oracle.key().to_bytes()),
            PoolError::YieldAccountsMissing
        );
        require_keys_eq!(
            instructions.key(),
            solana_sdk_ids::sysvar::instructions::ID,
            PoolError::YieldAccountsMissing
        );
        if pool_collateral.data_is_empty() {
            // `init_yield_account` not run yet: nothing was ever deployed into this venue.
            require!(
                pool.deployed_collateral == 0,
                PoolError::YieldAccountsMissing
            );
            return Ok(None);
        }
        {
            require_keys_eq!(
                *pool_collateral.owner,
                anchor_spl::token::ID,
                PoolError::YieldAccountsMissing
            );
            let d = pool_collateral.try_borrow_data()?;
            let t = TokenAccount::try_deserialize(&mut &d[..])?;
            require!(
                t.mint == collateral_mint.key() && t.owner == *pool_key,
                PoolError::YieldAccountsMissing
            );
        }
        Ok(Some(Leg {
            program,
            reserve,
            market,
            authority,
            supply,
            collateral_mint,
            pool_collateral,
            oracle,
            instructions,
            state,
            market_emergency,
            refreshed: false,
        }))
    }
}

impl<'info> Leg<'info> {
    /// Collateral in the pool's account, read now.
    pub fn collateral_held(&self) -> Result<u64> {
        crate::vault::token_amount(&self.pool_collateral)
    }

    /// Re-reads the reserve and market after a klend call.
    fn reload(&mut self) -> Result<()> {
        self.state = read_reserve(&self.reserve)?;
        self.market_emergency = k::market_emergency(&self.market.try_borrow_data()?).core()?;
        Ok(())
    }

    /// klend `refresh_reserve`, once per instruction: deposit and redeem need a reserve refreshed in
    /// the same slot. Accrues interest, so the rate read after it is current.
    pub fn refresh(&mut self) -> Result<()> {
        if self.refreshed {
            return Ok(());
        }
        // klend's price slots: Pyth, Switchboard price, Switchboard TWAP, Scope. The reserve's one
        // source goes in its slot; unused slots are passed as the klend program (None).
        let none = self.program.key();
        let oracle = self.oracle.key();
        let (pyth, scope) = match self.state.price {
            k::PriceSource::Pyth(_) => (oracle, none),
            _ => (none, oracle),
        };
        let metas = vec![
            AccountMeta::new(self.reserve.key(), false),
            AccountMeta::new_readonly(self.market.key(), false),
            AccountMeta::new_readonly(pyth, false),
            AccountMeta::new_readonly(none, false),
            AccountMeta::new_readonly(none, false),
            AccountMeta::new_readonly(scope, false),
        ];
        let ix = Instruction {
            program_id: KLEND_PROGRAM_ID,
            accounts: metas,
            data: k::IX_REFRESH_RESERVE.to_vec(),
        };
        let infos = [
            self.reserve.clone(),
            self.market.clone(),
            self.oracle.clone(),
            self.program.clone(),
        ];
        invoke_signed(&ix, &infos, &[])?;
        self.refreshed = true;
        self.reload()
    }

    fn call(
        &mut self,
        pool: &Pool,
        pool_info: &AccountInfo<'info>,
        cash: &Cash<'info>,
        vault: &AccountInfo<'info>,
        data: Vec<u8>,
        metas: Vec<AccountMeta>,
    ) -> Result<()> {
        let ix = Instruction {
            program_id: KLEND_PROGRAM_ID,
            accounts: metas,
            data,
        };
        let infos = [
            pool_info.clone(),
            self.reserve.clone(),
            self.market.clone(),
            self.authority.clone(),
            cash.usdc_mint.to_account_info(),
            self.supply.clone(),
            self.collateral_mint.clone(),
            vault.clone(),
            self.pool_collateral.clone(),
            cash.token_program.to_account_info(),
            self.instructions.clone(),
            self.program.clone(),
        ];
        let bump = [pool.bump];
        invoke_signed(&ix, &infos, &[&[SEED_POOL, &bump]])?;
        self.reload()
    }

    /// klend `deposit_reserve_liquidity`: `amount` USDC from the vault, collateral to the pool's
    /// account. The pool signs as owner of both.
    pub fn deposit(
        &mut self,
        pool: &Pool,
        pool_info: &AccountInfo<'info>,
        cash: &Cash<'info>,
        vault: &AccountInfo<'info>,
        amount: u64,
    ) -> Result<()> {
        let mut data = k::IX_DEPOSIT_RESERVE_LIQUIDITY.to_vec();
        data.extend_from_slice(&amount.to_le_bytes());
        let token = cash.token_program.key();
        let metas = vec![
            AccountMeta::new_readonly(pool_info.key(), true),
            AccountMeta::new(self.reserve.key(), false),
            AccountMeta::new_readonly(self.market.key(), false),
            AccountMeta::new_readonly(self.authority.key(), false),
            AccountMeta::new_readonly(cash.usdc_mint.key(), false),
            AccountMeta::new(self.supply.key(), false),
            AccountMeta::new(self.collateral_mint.key(), false),
            AccountMeta::new(vault.key(), false),
            AccountMeta::new(self.pool_collateral.key(), false),
            AccountMeta::new_readonly(token, false),
            AccountMeta::new_readonly(token, false),
            AccountMeta::new_readonly(self.instructions.key(), false),
        ];
        self.call(pool, pool_info, cash, vault, data, metas)
    }

    /// klend `redeem_reserve_collateral`: `collateral` from the pool's account, USDC to the vault.
    pub fn redeem(
        &mut self,
        pool: &Pool,
        pool_info: &AccountInfo<'info>,
        cash: &Cash<'info>,
        vault: &AccountInfo<'info>,
        collateral: u64,
    ) -> Result<()> {
        let mut data = k::IX_REDEEM_RESERVE_COLLATERAL.to_vec();
        data.extend_from_slice(&collateral.to_le_bytes());
        let token = cash.token_program.key();
        let metas = vec![
            AccountMeta::new_readonly(pool_info.key(), true),
            AccountMeta::new_readonly(self.market.key(), false),
            AccountMeta::new(self.reserve.key(), false),
            AccountMeta::new_readonly(self.authority.key(), false),
            AccountMeta::new_readonly(cash.usdc_mint.key(), false),
            AccountMeta::new(self.collateral_mint.key(), false),
            AccountMeta::new(self.supply.key(), false),
            AccountMeta::new(self.pool_collateral.key(), false),
            AccountMeta::new(vault.key(), false),
            AccountMeta::new_readonly(token, false),
            AccountMeta::new_readonly(token, false),
            AccountMeta::new_readonly(self.instructions.key(), false),
        ];
        self.call(pool, pool_info, cash, vault, data, metas)
    }
}
