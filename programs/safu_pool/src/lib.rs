//! SAFU Staking on Solana: a USDC protection pool.
//!
//! Stakers and backers only. Rules follow the multichain protection pool; every number comes from
//! `pool-core`. See `docs/PORT_MAP.md` for what was ported, what changed and what was left out.

use anchor_lang::prelude::*;

pub mod approval;
pub mod constants;
pub mod errors;
pub mod events;
pub mod instructions;
pub mod kamino;
pub mod leg;
pub mod state;
pub mod vault;

use approval::ClaimApproval;
use instructions::*;
use state::GovChange;

declare_id!("6nE296ZjUgVifngxv1Z5jWp4xvBexaBiepFHA8UVYC7X");

/// Runs a user instruction with the in-transaction upkeep (`instructions::upkeep::with_upkeep`).
macro_rules! upkept {
    ($ctx:ident, $body:expr) => {{
        let mut ctx = $ctx;
        instructions::upkeep::with_upkeep(&mut ctx, |a| &mut a.pool, $body)
    }};
}

#[program]
pub mod safu_pool {
    use super::*;

    // Setup and admin.
    pub fn initialize(ctx: Context<Initialize>, args: InitArgs) -> Result<()> {
        instructions::admin::initialize(ctx, args)
    }
    /// After an upgrade: brings the pool's data to this version (anyone; a no-op the second time).
    pub fn migrate(ctx: Context<Migrate>) -> Result<u8> {
        instructions::admin::migrate(ctx)
    }
    pub fn pause(ctx: Context<AdminOnly>) -> Result<()> {
        instructions::admin::pause(ctx)
    }
    pub fn unpause(ctx: Context<AdminOnly>) -> Result<()> {
        instructions::admin::unpause(ctx)
    }
    /// The admin's immediate brake: the cap only goes down here (up: the settings timelock).
    pub fn lower_pool_cap(ctx: Context<AdminOnly>, value: i64) -> Result<()> {
        instructions::admin::lower_pool_cap(ctx, value)
    }

    // Governance (2-of-3 roles: admin, co-signer, guardian).
    pub fn propose_change(ctx: Context<GovCall>, change: GovChange) -> Result<()> {
        instructions::governance::propose_change(ctx, change)
    }
    pub fn approve_change(ctx: Context<GovCall>, change: GovChange) -> Result<()> {
        instructions::governance::approve_change(ctx, change)
    }
    pub fn propose_recovery(ctx: Context<GovCall>, change: GovChange) -> Result<()> {
        instructions::governance::propose_recovery(ctx, change)
    }
    pub fn cancel_change(ctx: Context<GovCall>, kind: u8) -> Result<()> {
        instructions::governance::cancel_change(ctx, kind)
    }
    pub fn execute_change(ctx: Context<GovExecute>, kind: u8) -> Result<()> {
        instructions::governance::execute_change(ctx, kind)
    }

    // Settings timelock (pool cap, clocks, rates, yield split, stake bounds).
    pub fn propose_setting(ctx: Context<AdminOnly>, key: u8, value: i64) -> Result<()> {
        instructions::settings::propose_setting(ctx, key, value)
    }
    pub fn approve_setting(ctx: Context<ApproveSetting>, key: u8, value: i64) -> Result<()> {
        instructions::settings::approve_setting(ctx, key, value)
    }
    pub fn execute_setting(ctx: Context<ExecuteSetting>, key: u8) -> Result<()> {
        instructions::settings::execute_setting(ctx, key)
    }
    pub fn cancel_setting(ctx: Context<CancelSetting>, key: u8) -> Result<()> {
        instructions::settings::cancel_setting(ctx, key)
    }

    // Stakers.
    pub fn stake(ctx: Context<Stake>, amount: u64, beneficiary: Pubkey) -> Result<()> {
        upkept!(ctx, |c| instructions::stake::stake(c, amount, beneficiary))
    }
    pub fn set_beneficiary(ctx: Context<SetBeneficiary>, beneficiary: Pubkey) -> Result<()> {
        instructions::stake::set_beneficiary(ctx, beneficiary)
    }
    pub fn withdraw(ctx: Context<Withdraw>, amount: u64) -> Result<()> {
        upkept!(ctx, |c| instructions::stake::withdraw(c, amount))
    }
    pub fn emergency_exit(ctx: Context<EmergencyExit>, amount: u64) -> Result<()> {
        instructions::stake::emergency_exit(ctx, amount)
    }
    pub fn claim_yield(ctx: Context<ClaimYield>) -> Result<()> {
        upkept!(ctx, instructions::stake::claim_yield)
    }

    // Backers.
    pub fn back(ctx: Context<Back>, amount: u64) -> Result<()> {
        upkept!(ctx, |c| instructions::backer::back(c, amount))
    }
    pub fn mature_backing(ctx: Context<MatureBacking>) -> Result<()> {
        upkept!(ctx, instructions::backer::mature_backing)
    }
    pub fn request_backer_withdrawal(
        ctx: Context<RequestBackerWithdrawal>,
        amount: u64,
    ) -> Result<()> {
        upkept!(ctx, |c| instructions::backer::request_backer_withdrawal(
            c, amount
        ))
    }
    pub fn cancel_backer_withdrawal(ctx: Context<BackerOnly>) -> Result<()> {
        upkept!(ctx, instructions::backer::cancel_backer_withdrawal)
    }
    pub fn complete_backer_withdrawal(ctx: Context<CompleteBackerWithdrawal>) -> Result<()> {
        upkept!(ctx, instructions::backer::complete_backer_withdrawal)
    }
    pub fn claim_backer_yield(ctx: Context<ClaimBackerYield>) -> Result<()> {
        upkept!(ctx, instructions::backer::claim_backer_yield)
    }

    // Protocol revenue.
    pub fn withdraw_yield(ctx: Context<WithdrawYield>, amount: u64) -> Result<()> {
        instructions::upkeep::withdraw_yield(ctx, amount)
    }
    pub fn fund_rounding_reserve(ctx: Context<FundRoundingReserve>, amount: u64) -> Result<()> {
        instructions::upkeep::fund_rounding_reserve(ctx, amount)
    }

    // Yield venue (keepers and admin; user calls harvest, push and pull on their own).
    pub fn init_yield_account(ctx: Context<InitYieldAccount>) -> Result<()> {
        instructions::yield_leg::init_yield_account(ctx)
    }
    pub fn harvest(ctx: Context<LegCall>) -> Result<()> {
        instructions::yield_leg::harvest(ctx)
    }
    pub fn auto_deploy_liquidity(ctx: Context<LegCall>) -> Result<()> {
        instructions::yield_leg::auto_deploy_liquidity(ctx)
    }
    pub fn ensure_liquidity(ctx: Context<LegCall>) -> Result<()> {
        instructions::yield_leg::ensure_liquidity(ctx)
    }
    pub fn provide_liquidity(
        ctx: Context<ProvideLiquidity>,
        collateral: u64,
        min_out: u64,
    ) -> Result<()> {
        instructions::yield_leg::provide_liquidity(ctx, collateral, min_out)
    }

    // Views (simulate; the answer is the return data).
    pub fn view_staker(ctx: Context<ViewStaker>) -> Result<StakerView> {
        instructions::views::view_staker(ctx)
    }
    pub fn view_backer(ctx: Context<ViewBacker>) -> Result<BackerView> {
        instructions::views::view_backer(ctx)
    }

    // Covered wallets.
    pub fn register_wallet(
        ctx: Context<RegisterWallet>,
        staker: Pubkey,
        wallet_hash: [u8; 32],
    ) -> Result<()> {
        instructions::registry::register_wallet(ctx, staker, wallet_hash)
    }

    // Claims.
    pub fn submit_claim(ctx: Context<SubmitClaim>, approval: ClaimApproval) -> Result<()> {
        upkept!(ctx, |c| instructions::claim::submit_claim(c, approval))
    }
    pub fn try_release_queued_claim(ctx: Context<ReleaseQueuedClaim>) -> Result<()> {
        upkept!(ctx, instructions::claim::try_release_queued_claim)
    }
    pub fn advance_queue(ctx: Context<AdvanceQueue>) -> Result<()> {
        upkept!(ctx, instructions::claim::advance_queue)
    }
    pub fn expire_queued_claim(ctx: Context<ClaimTransition>) -> Result<()> {
        upkept!(ctx, instructions::claim::expire_queued_claim)
    }
    pub fn unlock_pending_claim(ctx: Context<ClaimTransition>) -> Result<()> {
        upkept!(ctx, instructions::claim::unlock_pending_claim)
    }
    pub fn expire_pending_approval(ctx: Context<ClaimTransition>) -> Result<()> {
        upkept!(ctx, instructions::claim::expire_pending_approval)
    }
    pub fn expire_stale_claim(ctx: Context<ClaimTransition>) -> Result<()> {
        upkept!(ctx, instructions::claim::expire_stale_claim)
    }
    pub fn approve_claim(ctx: Context<ApproveClaim>) -> Result<()> {
        upkept!(ctx, instructions::claim::approve_claim)
    }
    pub fn claim_stream(ctx: Context<ClaimStream>) -> Result<()> {
        upkept!(ctx, instructions::claim::claim_stream)
    }
    pub fn cancel_claim(ctx: Context<CancelClaim>) -> Result<()> {
        instructions::claim::cancel_claim(ctx)
    }
    pub fn suspend_stake(ctx: Context<AdminStake>, staker: Pubkey) -> Result<()> {
        instructions::claim::suspend_stake(ctx, staker)
    }
    pub fn renew_suspension(ctx: Context<RenewSuspension>, staker: Pubkey) -> Result<()> {
        instructions::claim::renew_suspension(ctx, staker)
    }
    pub fn unsuspend_stake(ctx: Context<AdminStake>, staker: Pubkey) -> Result<()> {
        instructions::claim::unsuspend_stake(ctx, staker)
    }
    pub fn revoke_approval(
        ctx: Context<RevokeApproval>,
        approval: ClaimApproval,
        hash: [u8; 32],
    ) -> Result<()> {
        instructions::claim::revoke_approval(ctx, approval, hash)
    }
    pub fn approve_override(
        ctx: Context<ApproveOverride>,
        staker: Pubkey,
        tx_hash: [u8; 32],
        entitlement: u64,
        tier: u8,
    ) -> Result<()> {
        instructions::claim::approve_override(ctx, staker, tx_hash, entitlement, tier)
    }
    pub fn cancel_pending_override(ctx: Context<CancelPendingOverride>) -> Result<()> {
        instructions::claim::cancel_pending_override(ctx)
    }
}
