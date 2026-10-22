//! The vault: the pool's USDC token account, PDA `[SEED_VAULT, pool]`, whose authority is the pool
//! PDA. Classic SPL Token only (`initialize` refuses anything else), so a transfer moves exactly the
//! amount asked. Every balance is read from the account's live data, never from a copy cached
//! before a transfer in the same instruction.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, TransferChecked};
use pool_core::liquidity;

use crate::constants::SEED_POOL;
use crate::errors::{CoreResultExt, PoolError};
use crate::state::Pool;

/// The accounts every money-moving instruction needs besides the vault: the USDC mint and the token
/// program. The mint is checked against `Pool::usdc_mint` before any transfer.
#[derive(Accounts)]
pub struct Cash<'info> {
    pub usdc_mint: Box<Account<'info, Mint>>,
    pub token_program: Program<'info, Token>,
}

impl Cash<'_> {
    pub fn check(&self, pool: &Pool) -> Result<()> {
        require_keys_eq!(self.usdc_mint.key(), pool.usdc_mint, PoolError::WrongMint);
        Ok(())
    }
}

/// USDC in a token account, read from its current data.
pub fn token_amount(account: &AccountInfo) -> Result<u64> {
    let data = account.try_borrow_data()?;
    Ok(TokenAccount::try_deserialize(&mut &data[..])?.amount)
}

/// USDC in the vault.
pub fn liquid_balance(vault: &AccountInfo) -> Result<u64> {
    token_amount(vault)
}

/// Liquid USDC minus set-aside staker and backer yield.
pub fn free_liquid(pool: &Pool, vault: &AccountInfo) -> Result<u64> {
    let reserved = pool_core::add(pool.staker_yield_reserved, pool.backer_yield_reserved).core()?;
    Ok(liquidity::free_liquid(liquid_balance(vault)?, reserved))
}

/// Typed failure instead of a failed transfer, checked before any state changes.
pub fn require_free_liquid(pool: &Pool, vault: &AccountInfo, amount: u64) -> Result<()> {
    require!(
        free_liquid(pool, vault)? >= amount,
        PoolError::InsufficientLiquidity
    );
    Ok(())
}

/// USDC in, from a token account its owner signs for.
pub fn receive<'info>(
    pool: &Pool,
    cash: &Cash<'info>,
    from: &AccountInfo<'info>,
    owner: &AccountInfo<'info>,
    vault: &AccountInfo<'info>,
    amount: u64,
) -> Result<()> {
    cash.check(pool)?;
    token::transfer_checked(
        CpiContext::new(
            cash.token_program.key(),
            TransferChecked {
                from: from.clone(),
                mint: cash.usdc_mint.to_account_info(),
                to: vault.clone(),
                authority: owner.clone(),
            },
        ),
        amount,
        cash.usdc_mint.decimals,
    )
}

/// USDC out of the vault, signed by the pool PDA.
pub fn pay<'info>(
    pool: &Pool,
    pool_info: &AccountInfo<'info>,
    cash: &Cash<'info>,
    vault: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    amount: u64,
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    cash.check(pool)?;
    let bump = [pool.bump];
    let seeds: &[&[&[u8]]] = &[&[SEED_POOL, &bump]];
    token::transfer_checked(
        CpiContext::new_with_signer(
            cash.token_program.key(),
            TransferChecked {
                from: vault.clone(),
                mint: cash.usdc_mint.to_account_info(),
                to: to.clone(),
                authority: pool_info.clone(),
            },
            seeds,
        ),
        amount,
        cash.usdc_mint.decimals,
    )
}

/// Pays `amount` of free liquid USDC to `to`. Call after the books are updated: on any failure the
/// whole transaction reverts.
pub fn pay_out<'info>(
    pool: &Pool,
    pool_info: &AccountInfo<'info>,
    cash: &Cash<'info>,
    vault: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    amount: u64,
) -> Result<()> {
    require_free_liquid(pool, vault, amount)?;
    pay(pool, pool_info, cash, vault, to, amount)
}
