//! SAFU Staking on Solana: a SOL protection pool, staked through Marinade.
//!
//! Stakers and backers only. Rules follow the multichain protection pool; every number comes from
//! `pool-core`. See `docs/PORT_MAP.md` for what was ported, what changed and what was left out.

use anchor_lang::prelude::*;

pub mod constants;
pub mod errors;
pub mod events;
pub mod instructions;
pub mod marinade;
pub mod state;
pub mod vault;

use instructions::*;

declare_id!("Aa6ncthjKmWaDX91jrY3HeUdHfP2fnhaPLQAPPVsG686");

#[program]
pub mod safu_pool {
    use super::*;

    // Setup and admin.
    pub fn initialize(ctx: Context<Initialize>, args: InitArgs) -> Result<()> {
        instructions::admin::initialize(ctx, args)
    }
    pub fn pause(ctx: Context<AdminOnly>) -> Result<()> {
        instructions::admin::pause(ctx)
    }
    pub fn unpause(ctx: Context<AdminOnly>) -> Result<()> {
        instructions::admin::unpause(ctx)
    }
    pub fn set_pool_cap(ctx: Context<AdminOnly>, pool_cap: u64) -> Result<()> {
        instructions::admin::set_pool_cap(ctx, pool_cap)
    }

    // Stakers.
    pub fn stake(ctx: Context<Stake>, amount: u64, beneficiary: Pubkey) -> Result<()> {
        instructions::stake::stake(ctx, amount, beneficiary)
    }
    pub fn set_beneficiary(ctx: Context<SetBeneficiary>, beneficiary: Pubkey) -> Result<()> {
        instructions::stake::set_beneficiary(ctx, beneficiary)
    }
    pub fn withdraw(ctx: Context<Withdraw>) -> Result<()> {
        instructions::stake::withdraw(ctx)
    }
    pub fn emergency_exit(ctx: Context<EmergencyExit>) -> Result<()> {
        instructions::stake::emergency_exit(ctx)
    }

    // Backers.
    pub fn back(ctx: Context<Back>, amount: u64) -> Result<()> {
        instructions::backer::back(ctx, amount)
    }
    pub fn mature_backing(ctx: Context<MatureBacking>) -> Result<()> {
        instructions::backer::mature_backing(ctx)
    }
    pub fn request_backer_withdrawal(ctx: Context<BackerOnly>, amount: u64) -> Result<()> {
        instructions::backer::request_backer_withdrawal(ctx, amount)
    }
    pub fn cancel_backer_withdrawal(ctx: Context<BackerOnly>) -> Result<()> {
        instructions::backer::cancel_backer_withdrawal(ctx)
    }
    pub fn complete_backer_withdrawal(ctx: Context<CompleteBackerWithdrawal>) -> Result<()> {
        instructions::backer::complete_backer_withdrawal(ctx)
    }

    // Covered wallets.
    pub fn register_wallet(ctx: Context<RegisterWallet>, staker: Pubkey, wallet_hash: [u8; 32]) -> Result<()> {
        instructions::registry::register_wallet(ctx, staker, wallet_hash)
    }
}
