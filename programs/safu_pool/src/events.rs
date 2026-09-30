use anchor_lang::prelude::*;

#[event]
pub struct PoolInitialized {
    pub admin: Pubkey,
    pub pool_cap: u64,
    pub cluster: u8,
}
#[event]
pub struct PausedUntil {
    pub until: i64,
}
#[event]
pub struct Unpaused {}
#[event]
pub struct PoolCapRaised {
    pub pool_cap: u64,
}
#[event]
pub struct Staked {
    pub staker: Pubkey,
    pub amount: u64,
}
#[event]
pub struct BeneficiarySet {
    pub staker: Pubkey,
    pub beneficiary: Pubkey,
}
#[event]
pub struct Withdrawn {
    pub staker: Pubkey,
    pub principal: u64,
    pub yield_paid: u64,
}
#[event]
pub struct EmergencyExited {
    pub staker: Pubkey,
    pub principal: u64,
    pub yield_paid: u64,
}
#[event]
pub struct Backed {
    pub backer: Pubkey,
    pub amount: u64,
    pub matures_at: i64,
}
#[event]
pub struct BackingMatured {
    pub backer: Pubkey,
    pub amount: u64,
}
#[event]
pub struct BackerWithdrawalRequested {
    pub backer: Pubkey,
    pub amount: u64,
    pub ready_at: i64,
}
#[event]
pub struct BackerWithdrawalCancelled {
    pub backer: Pubkey,
    pub amount: u64,
}
#[event]
pub struct BackerWithdrawn {
    pub backer: Pubkey,
    pub amount: u64,
    pub yield_paid: u64,
}
#[event]
pub struct WalletRegistered {
    pub staker: Pubkey,
    pub wallet_hash: [u8; 32],
}
#[event]
pub struct ClaimSubmitted {
    pub staker: Pubkey,
    pub claim: Pubkey,
    pub entitlement: u64,
}
#[event]
pub struct ClaimQueued {
    pub staker: Pubkey,
    pub claim: Pubkey,
    pub entitlement: u64,
}
#[event]
pub struct ClaimQueueReleased {
    pub claim: Pubkey,
}
#[event]
pub struct ClaimQueueExpired {
    pub claim: Pubkey,
}
#[event]
pub struct ClaimUnlocked {
    pub claim: Pubkey,
}
#[event]
pub struct ClaimApproved {
    pub claim: Pubkey,
}
#[event]
pub struct ClaimExpired {
    pub claim: Pubkey,
    pub released: u64,
}
#[event]
pub struct ClaimStreamed {
    pub claim: Pubkey,
    pub amount: u64,
}
#[event]
pub struct ClaimCancelled {
    pub claim: Pubkey,
}
#[event]
pub struct OverrideApproved {
    pub claim: Pubkey,
    pub approver: Pubkey,
    pub entitlement: u64,
    pub tier: u8,
}
#[event]
pub struct OverrideExecuted {
    pub claim: Pubkey,
}
#[event]
pub struct OverrideCancelled {
    pub claim: Pubkey,
}
#[event]
pub struct ApprovalRevoked {
    pub staker: Pubkey,
    pub hash: [u8; 32],
}
#[event]
pub struct StakeSuspended {
    pub staker: Pubkey,
}
#[event]
pub struct StakeUnsuspended {
    pub staker: Pubkey,
}
