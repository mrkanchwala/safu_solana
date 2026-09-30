//! SAFU Staking on Solana: a SOL protection pool, staked through Marinade.
//!
//! Stakers and backers only. Rules follow the multichain protection pool; every number comes from
//! `pool-core`. See `docs/PORT_MAP.md` for what was ported, what changed and what was left out.

use anchor_lang::prelude::*;

pub mod approval;
pub mod constants;
pub mod errors;
pub mod events;
pub mod instructions;
pub mod leg;
pub mod marinade;
pub mod state;
pub mod vault;

use approval::ClaimApproval;
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
    pub fn claim_yield(ctx: Context<ClaimYield>) -> Result<()> {
        instructions::stake::claim_yield(ctx)
    }

    // Backers.
    pub fn back(ctx: Context<Back>, amount: u64) -> Result<()> {
        instructions::backer::back(ctx, amount)
    }
    pub fn mature_backing(ctx: Context<MatureBacking>) -> Result<()> {
        instructions::backer::mature_backing(ctx)
    }
    pub fn request_backer_withdrawal(ctx: Context<RequestBackerWithdrawal>, amount: u64) -> Result<()> {
        instructions::backer::request_backer_withdrawal(ctx, amount)
    }
    pub fn cancel_backer_withdrawal(ctx: Context<BackerOnly>) -> Result<()> {
        instructions::backer::cancel_backer_withdrawal(ctx)
    }
    pub fn complete_backer_withdrawal(ctx: Context<CompleteBackerWithdrawal>) -> Result<()> {
        instructions::backer::complete_backer_withdrawal(ctx)
    }
    pub fn claim_backer_yield(ctx: Context<ClaimBackerYield>) -> Result<()> {
        instructions::backer::claim_backer_yield(ctx)
    }

    // Marinade leg and protocol revenue.
    pub fn harvest(ctx: Context<Upkeep>) -> Result<()> {
        instructions::upkeep::harvest(ctx)
    }
    pub fn rebalance(ctx: Context<Upkeep>) -> Result<()> {
        instructions::upkeep::rebalance(ctx)
    }
    pub fn withdraw_yield(ctx: Context<WithdrawYield>, amount: u64) -> Result<()> {
        instructions::upkeep::withdraw_yield(ctx, amount)
    }

    // Covered wallets.
    pub fn register_wallet(ctx: Context<RegisterWallet>, staker: Pubkey, wallet_hash: [u8; 32]) -> Result<()> {
        instructions::registry::register_wallet(ctx, staker, wallet_hash)
    }

    // Claims.
    pub fn submit_claim(ctx: Context<SubmitClaim>, approval: ClaimApproval) -> Result<()> {
        instructions::claim::submit_claim(ctx, approval)
    }
    pub fn try_release_queued_claim(ctx: Context<ClaimTransition>) -> Result<()> {
        instructions::claim::try_release_queued_claim(ctx)
    }
    pub fn expire_queued_claim(ctx: Context<ClaimTransition>) -> Result<()> {
        instructions::claim::expire_queued_claim(ctx)
    }
    pub fn unlock_pending_claim(ctx: Context<ClaimTransition>) -> Result<()> {
        instructions::claim::unlock_pending_claim(ctx)
    }
    pub fn expire_pending_approval(ctx: Context<ClaimTransition>) -> Result<()> {
        instructions::claim::expire_pending_approval(ctx)
    }
    pub fn expire_stale_claim(ctx: Context<ClaimTransition>) -> Result<()> {
        instructions::claim::expire_stale_claim(ctx)
    }
    pub fn approve_claim(ctx: Context<ApproveClaim>) -> Result<()> {
        instructions::claim::approve_claim(ctx)
    }
    pub fn claim_stream(ctx: Context<ClaimStream>) -> Result<()> {
        instructions::claim::claim_stream(ctx)
    }
    pub fn cancel_claim(ctx: Context<CancelClaim>) -> Result<()> {
        instructions::claim::cancel_claim(ctx)
    }
    pub fn suspend_stake(ctx: Context<AdminStake>, staker: Pubkey) -> Result<()> {
        instructions::claim::suspend_stake(ctx, staker)
    }
    pub fn unsuspend_stake(ctx: Context<AdminStake>, staker: Pubkey) -> Result<()> {
        instructions::claim::unsuspend_stake(ctx, staker)
    }
    pub fn revoke_approval(ctx: Context<RevokeApproval>, approval: ClaimApproval, hash: [u8; 32]) -> Result<()> {
        instructions::claim::revoke_approval(ctx, approval, hash)
    }
    pub fn approve_override(ctx: Context<ApproveOverride>, staker: Pubkey, tx_hash: [u8; 32], entitlement: u64, tier: u8) -> Result<()> {
        instructions::claim::approve_override(ctx, staker, tx_hash, entitlement, tier)
    }
    pub fn cancel_pending_override(ctx: Context<CancelPendingOverride>) -> Result<()> {
        instructions::claim::cancel_pending_override(ctx)
    }
}
