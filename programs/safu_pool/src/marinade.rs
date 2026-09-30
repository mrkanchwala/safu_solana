//! Reading Marinade's `State` (layout facts in `pool_core::marinade`). Marinade is upgradeable and
//! outside our control: owner, discriminator and length are checked before any byte is read.

use anchor_lang::prelude::*;
use pool_core::marinade as m;

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
