//! Covered wallets (multichain `covered-registry`, here inside the pool program).
//! Up to `MAX_COVERED_WALLETS` per staker; a wallet belongs to one staker forever (no deregister,
//! swap or override). Coverage = active stake: the backend checks the staker's live stake at claim
//! time. Writer = the backend key, after an off-chain ownership proof.

use anchor_lang::prelude::*;

use super::now;
use crate::constants::{SEED_COVERED, SEED_POOL, SEED_STAKER_WALLETS};
use crate::errors::PoolError;
use crate::events::WalletRegistered;
use crate::state::{CoveredWallet, Pool, StakerWallets};
use pool_core::params::MAX_COVERED_WALLETS;

#[derive(Accounts)]
#[instruction(staker: Pubkey, wallet_hash: [u8; 32])]
pub struct RegisterWallet<'info> {
    #[account(mut)]
    pub registry_writer: Signer<'info>,
    #[account(seeds = [SEED_POOL], bump = pool.bump, has_one = registry_writer @ PoolError::NotRegistryWriter)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(
        init_if_needed,
        payer = registry_writer,
        space = 8 + StakerWallets::INIT_SPACE,
        seeds = [SEED_STAKER_WALLETS, pool.key().as_ref(), staker.as_ref()],
        bump,
    )]
    pub staker_wallets: Box<Account<'info, StakerWallets>>,
    #[account(
        init_if_needed,
        payer = registry_writer,
        space = 8 + CoveredWallet::INIT_SPACE,
        seeds = [SEED_COVERED, pool.key().as_ref(), wallet_hash.as_ref()],
        bump,
    )]
    pub covered_wallet: Box<Account<'info, CoveredWallet>>,
    pub system_program: Program<'info, System>,
}

pub fn register_wallet(ctx: Context<RegisterWallet>, staker: Pubkey, wallet_hash: [u8; 32]) -> Result<()> {
    let covered = &mut ctx.accounts.covered_wallet;
    if covered.staker != Pubkey::default() {
        require_keys_neq!(covered.staker, staker, PoolError::AlreadyRegistered);
        return err!(PoolError::WalletTakenByOtherStaker);
    }
    let wallets = &mut ctx.accounts.staker_wallets;
    if wallets.staker == Pubkey::default() {
        wallets.staker = staker;
        wallets.bump = ctx.bumps.staker_wallets;
    }
    require!(wallets.count < MAX_COVERED_WALLETS, PoolError::StakerLimitReached);
    let slot = wallets.count as usize;
    wallets.wallet_hashes[slot] = wallet_hash;
    wallets.count += 1;
    covered.set_inner(CoveredWallet {
        staker,
        wallet_hash,
        registered_at: now()?,
        bump: ctx.bumps.covered_wallet,
    });
    emit!(WalletRegistered { staker, wallet_hash });
    Ok(())
}
