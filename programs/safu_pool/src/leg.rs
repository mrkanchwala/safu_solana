//! Yield: the one place new yield is split between stakers, backers and the protocol (multichain
//! `credit_yield`). The yield venue itself is not wired yet; until it is, nothing credits yield and
//! every payment comes from the vault's liquid USDC.

use anchor_lang::prelude::*;
use pool_core::settings::SettingKey;
use pool_core::{add, yields};

use crate::errors::{CoreResultExt, PoolError};
use crate::events::YieldCredited;
use crate::state::Pool;

pub fn credit_yield(pool: &mut Pool, amount: u64) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    let st = pool.settings();
    let c = yields::credit(
        amount,
        pool.total_staked,
        pool.total_backed,
        st.amount(SettingKey::StakerYieldBps),
        st.amount(SettingKey::BackerYieldBps),
    )
    .core()?;
    pool.staker_yield_index = pool
        .staker_yield_index
        .checked_add(c.staker_index_bump)
        .ok_or(PoolError::MathOverflow)?;
    pool.backer_yield_index = pool
        .backer_yield_index
        .checked_add(c.backer_index_bump)
        .ok_or(PoolError::MathOverflow)?;
    pool.staker_yield_reserved = add(pool.staker_yield_reserved, c.staker_share).core()?;
    pool.backer_yield_reserved = add(pool.backer_yield_reserved, c.backer_share).core()?;
    pool.protocol_yield_balance = add(pool.protocol_yield_balance, c.protocol_share).core()?;
    pool.total_extracted_yield = add(pool.total_extracted_yield, amount).core()?;
    emit!(YieldCredited {
        amount,
        staker_share: c.staker_share,
        backer_share: c.backer_share,
        protocol_share: c.protocol_share,
    });
    Ok(())
}
