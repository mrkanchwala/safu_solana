//! The vault PDA `[SEED_VAULT, pool]`: system-owned, no data, holds the pool's SOL and owns the
//! pool's mSOL. It keeps its rent-exempt minimum; everything above that is liquid.

use anchor_lang::prelude::*;
use anchor_lang::system_program::{self, Transfer};
use pool_core::liquidity;

use crate::constants::SEED_VAULT;
use crate::errors::{CoreResultExt, PoolError};
use crate::state::Pool;

/// Rent-exempt minimum of a data-less account, read from the sysvar (never typed).
pub fn rent_floor() -> Result<u64> {
    Ok(Rent::get()?.minimum_balance(0))
}

/// SOL in the vault above its rent floor.
pub fn liquid_balance(vault: &AccountInfo) -> Result<u64> {
    Ok(vault.lamports().saturating_sub(rent_floor()?))
}

/// Liquid SOL minus set-aside staker and backer yield.
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

/// SOL in, from a signing wallet.
pub fn receive<'info>(
    from: &AccountInfo<'info>,
    vault: &AccountInfo<'info>,
    amount: u64,
) -> Result<()> {
    system_program::transfer(
        CpiContext::new(
            system_program::ID,
            Transfer {
                from: from.clone(),
                to: vault.clone(),
            },
        ),
        amount,
    )
}

/// SOL out, signed by the vault PDA.
pub fn pay<'info>(
    pool_key: &Pubkey,
    vault_bump: u8,
    vault: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    amount: u64,
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    let bump = [vault_bump];
    let seeds: &[&[&[u8]]] = &[&[SEED_VAULT, pool_key.as_ref(), &bump]];
    system_program::transfer(
        CpiContext::new_with_signer(
            system_program::ID,
            Transfer {
                from: vault.clone(),
                to: to.clone(),
            },
            seeds,
        ),
        amount,
    )
}
