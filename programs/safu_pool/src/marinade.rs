//! Marinade: reading its `State` and the two calls the pool makes (`deposit`, `liquid_unstake`).
//! Layout facts live in `pool_core::marinade`. Marinade is upgradeable and outside our control: owner,
//! discriminator and length are checked before any byte is read, and program, state, mint and the
//! pool's mSOL account are checked against `Pool` before any call (arbitrary-CPI guard). The other
//! accounts are Marinade's own PDAs and are checked by Marinade itself.
//!
//! Instructions are built by hand (discriminator + accounts): no `marinade-cpi` crate, whose Anchor
//! version would fight this one.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;
use anchor_spl::token::{Token, TokenAccount};
use pool_core::marinade as m;

use crate::constants::SEED_VAULT;
use crate::errors::PoolError;
use crate::state::Pool;

/// The `State` fields this program uses.
pub struct MarinadeState {
    pub msol_mint: Pubkey,
    pub treasury_msol: Pubkey,
    pub liq_pool_msol_leg: Pubkey,
    pub lp_max_fee_bps: u32,
    pub lp_min_fee_bps: u32,
    pub msol_price: u64,
    pub min_deposit: u64,
    pub min_withdraw: u64,
    pub paused: bool,
}

fn pubkey_at(d: &[u8], at: usize) -> Pubkey {
    let mut k = [0u8; 32];
    k.copy_from_slice(&d[at..at + 32]);
    Pubkey::new_from_array(k)
}
fn u64_at(d: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&d[at..at + 8]);
    u64::from_le_bytes(b)
}
fn u32_at(d: &[u8], at: usize) -> u32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&d[at..at + 4]);
    u32::from_le_bytes(b)
}

pub fn read_state(state: &AccountInfo, marinade_program: &Pubkey) -> Result<MarinadeState> {
    require_keys_eq!(*state.owner, *marinade_program, PoolError::WrongMarinadeAccount);
    let d = state.try_borrow_data()?;
    require!(d.len() >= m::STATE_MIN_LEN, PoolError::WrongMarinadeAccount);
    require!(d[..8] == m::ACCOUNT_STATE, PoolError::WrongMarinadeAccount);
    Ok(MarinadeState {
        msol_mint: pubkey_at(&d, m::STATE_MSOL_MINT),
        treasury_msol: pubkey_at(&d, m::STATE_TREASURY_MSOL),
        liq_pool_msol_leg: pubkey_at(&d, m::STATE_LIQ_POOL_MSOL_LEG),
        lp_max_fee_bps: u32_at(&d, m::STATE_LP_MAX_FEE_BPS),
        lp_min_fee_bps: u32_at(&d, m::STATE_LP_MIN_FEE_BPS),
        msol_price: u64_at(&d, m::STATE_MSOL_PRICE),
        min_deposit: u64_at(&d, m::STATE_MIN_DEPOSIT),
        min_withdraw: u64_at(&d, m::STATE_MIN_WITHDRAW),
        paused: d[m::STATE_PAUSED] != 0,
    })
}

/// The pool's own Marinade: program, state and mint must be the ones stored at `initialize`.
pub fn read_pool_state(pool: &Pool, program: &AccountInfo, state: &AccountInfo) -> Result<MarinadeState> {
    require_keys_eq!(program.key(), pool.marinade_program, PoolError::WrongMarinadeAccount);
    require_keys_eq!(state.key(), pool.marinade_state, PoolError::WrongMarinadeAccount);
    let s = read_state(state, &pool.marinade_program)?;
    require_keys_eq!(s.msol_mint, pool.msol_mint, PoolError::WrongMarinadeAccount);
    Ok(s)
}

/// Every account the pool's Marinade calls need. Part of each instruction that can move SOL in or out
/// of Marinade.
#[derive(Accounts)]
pub struct MarinadeLeg<'info> {
    /// CHECK: must equal `pool.marinade_program` (`MarinadeLeg::state`).
    pub marinade_program: UncheckedAccount<'info>,
    /// CHECK: must equal `pool.marinade_state`; owner, discriminator, length checked on read.
    #[account(mut)]
    pub marinade_state: UncheckedAccount<'info>,
    /// CHECK: must equal `pool.msol_mint`.
    #[account(mut)]
    pub msol_mint: UncheckedAccount<'info>,
    /// CHECK: Marinade PDA `[state, liq_sol]`, checked by Marinade. Its balance is read only to skip a
    /// call that would fail; a wrong account makes Marinade reject the whole transaction.
    #[account(mut)]
    pub liq_pool_sol_leg: UncheckedAccount<'info>,
    /// CHECK: `state.liq_pool.msol_leg`, checked by Marinade.
    #[account(mut)]
    pub liq_pool_msol_leg: UncheckedAccount<'info>,
    /// CHECK: Marinade PDA `[state, liq_st_sol_authority]`, checked by Marinade.
    pub liq_pool_msol_leg_authority: UncheckedAccount<'info>,
    /// CHECK: Marinade PDA `[state, reserve]`, checked by Marinade.
    #[account(mut)]
    pub reserve: UncheckedAccount<'info>,
    /// CHECK: Marinade PDA `[state, st_mint]`, checked by Marinade.
    pub msol_mint_authority: UncheckedAccount<'info>,
    /// CHECK: `state.treasury_msol_account`, checked by Marinade.
    #[account(mut)]
    pub treasury_msol: UncheckedAccount<'info>,
    /// CHECK: must equal `pool.pool_msol` (created at `initialize`, authority = vault).
    #[account(mut)]
    pub pool_msol: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

impl<'info> MarinadeLeg<'info> {
    /// Checks the accounts the pool is responsible for and reads Marinade's state.
    pub fn state(&self, pool: &Pool) -> Result<MarinadeState> {
        require_keys_eq!(self.msol_mint.key(), pool.msol_mint, PoolError::WrongMarinadeAccount);
        require_keys_eq!(self.pool_msol.key(), pool.pool_msol, PoolError::WrongMarinadeAccount);
        read_pool_state(pool, &self.marinade_program, &self.marinade_state)
    }

    /// mSOL in the pool's token account.
    pub fn pool_msol_amount(&self) -> Result<u64> {
        let data = self.pool_msol.try_borrow_data()?;
        Ok(TokenAccount::try_deserialize(&mut &data[..])?.amount)
    }

    /// SOL Marinade's liquidity pool can pay out now (above its rent floor).
    pub fn sol_leg_available(&self) -> Result<u64> {
        Ok(self.liq_pool_sol_leg.lamports().saturating_sub(crate::vault::rent_floor()?))
    }

    fn call(&self, pool: &Pool, pool_key: &Pubkey, vault: &AccountInfo<'info>, data: Vec<u8>, metas: Vec<AccountMeta>) -> Result<()> {
        let ix = Instruction { program_id: pool.marinade_program, accounts: metas, data };
        let infos = [
            self.marinade_state.to_account_info(),
            self.msol_mint.to_account_info(),
            self.liq_pool_sol_leg.to_account_info(),
            self.liq_pool_msol_leg.to_account_info(),
            self.liq_pool_msol_leg_authority.to_account_info(),
            self.reserve.to_account_info(),
            self.msol_mint_authority.to_account_info(),
            self.treasury_msol.to_account_info(),
            self.pool_msol.to_account_info(),
            vault.clone(),
            self.system_program.to_account_info(),
            self.token_program.to_account_info(),
            self.marinade_program.to_account_info(),
        ];
        let bump = [pool.vault_bump];
        invoke_signed(&ix, &infos, &[&[SEED_VAULT, pool_key.as_ref(), &bump]])?;
        Ok(())
    }

    /// Marinade `deposit`: `lamports` from the vault, mSOL to the pool's account.
    pub fn deposit(&self, pool: &Pool, pool_key: &Pubkey, vault: &AccountInfo<'info>, lamports: u64) -> Result<()> {
        let mut data = m::IX_DEPOSIT.to_vec();
        data.extend_from_slice(&lamports.to_le_bytes());
        let metas = vec![
            AccountMeta::new(self.marinade_state.key(), false),
            AccountMeta::new(self.msol_mint.key(), false),
            AccountMeta::new(self.liq_pool_sol_leg.key(), false),
            AccountMeta::new(self.liq_pool_msol_leg.key(), false),
            AccountMeta::new_readonly(self.liq_pool_msol_leg_authority.key(), false),
            AccountMeta::new(self.reserve.key(), false),
            AccountMeta::new(vault.key(), true),
            AccountMeta::new(self.pool_msol.key(), false),
            AccountMeta::new_readonly(self.msol_mint_authority.key(), false),
            AccountMeta::new_readonly(self.system_program.key(), false),
            AccountMeta::new_readonly(self.token_program.key(), false),
        ];
        self.call(pool, pool_key, vault, data, metas)
    }

    /// Marinade `liquid_unstake`: `msol` from the pool's account, SOL (less Marinade's fee) to the vault.
    pub fn liquid_unstake(&self, pool: &Pool, pool_key: &Pubkey, vault: &AccountInfo<'info>, msol: u64) -> Result<()> {
        let mut data = m::IX_LIQUID_UNSTAKE.to_vec();
        data.extend_from_slice(&msol.to_le_bytes());
        let metas = vec![
            AccountMeta::new(self.marinade_state.key(), false),
            AccountMeta::new(self.msol_mint.key(), false),
            AccountMeta::new(self.liq_pool_sol_leg.key(), false),
            AccountMeta::new(self.liq_pool_msol_leg.key(), false),
            AccountMeta::new(self.treasury_msol.key(), false),
            AccountMeta::new(self.pool_msol.key(), false),
            AccountMeta::new(vault.key(), true),
            AccountMeta::new(vault.key(), false),
            AccountMeta::new_readonly(self.system_program.key(), false),
            AccountMeta::new_readonly(self.token_program.key(), false),
        ];
        self.call(pool, pool_key, vault, data, metas)
    }
}
