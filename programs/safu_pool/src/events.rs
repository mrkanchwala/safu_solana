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
/// Setting changes: every step is public, so stakers see a change a full timelock ahead.
#[event]
pub struct SettingProposed {
    pub key: u8,
    pub value: i64,
}
#[event]
pub struct SettingApproved {
    pub key: u8,
    pub value: i64,
    pub eta: i64,
}
#[event]
pub struct SettingExecuted {
    pub key: u8,
    pub old_value: i64,
    pub new_value: i64,
}
#[event]
pub struct SettingCancelled {
    pub key: u8,
    pub by: Pubkey,
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
    /// Principal left in the stake (0 = fully withdrawn, record closed).
    pub remaining: u64,
}
#[event]
pub struct EmergencyExited {
    pub staker: Pubkey,
    pub principal: u64,
    pub yield_paid: u64,
    /// Principal left in the stake (0 = fully withdrawn, record closed).
    pub remaining: u64,
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

// B3: Marinade leg and yield.
#[event]
pub struct Deployed {
    pub lamports: u64,
    pub msol: u64,
}
/// mSOL received below the expected amount by more than the rebalance limit (multichain `PushBelowFloor`).
#[event]
pub struct DeployBelowFloor {
    pub lamports: u64,
    pub msol: u64,
    pub min_msol: u64,
}
#[event]
pub struct Unstaked {
    pub msol: u64,
    pub expected: u64,
    pub received: u64,
    pub payee_fee: u64,
}
/// The pool's part of an unstake fee above the growth that came back: marked off `total_staked`
/// (multichain `DeploymentShortfall`).
#[event]
pub struct DeploymentLoss {
    pub loss: u64,
}
#[event]
pub struct YieldCredited {
    pub amount: u64,
    pub staker_share: u64,
    pub backer_share: u64,
    pub protocol_share: u64,
}
#[event]
pub struct Harvested {
    pub msol: u64,
    pub received: u64,
}
#[event]
pub struct YieldClaimed {
    pub owner: Pubkey,
    pub amount: u64,
    pub backer: bool,
}
#[event]
pub struct YieldWithdrawn {
    pub treasury: Pubkey,
    pub amount: u64,
}
