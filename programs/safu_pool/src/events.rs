use anchor_lang::prelude::*;

#[event]
pub struct PoolInitialized {
    pub admin: Pubkey,
    pub pool_cap: u64,
    pub cluster: u8,
}
/// The pool's data moved to a newer layout after an upgrade.
#[event]
pub struct PoolMigrated {
    pub from: u8,
    pub to: u8,
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
    /// Principal left in the stake (0 = fully withdrawn).
    pub remaining: u64,
}
#[event]
pub struct EmergencyExited {
    pub staker: Pubkey,
    pub principal: u64,
    pub yield_paid: u64,
    /// Principal left in the stake (0 = fully withdrawn).
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
    /// Place in line.
    pub seq: u64,
}
#[event]
pub struct ClaimQueueReleased {
    pub claim: Pubkey,
}
#[event]
pub struct ClaimQueueExpired {
    pub claim: Pubkey,
}
/// The head of the line moved past a place whose claim no longer waits.
#[event]
pub struct QueueAdvanced {
    pub seq: u64,
    pub claim: Pubkey,
}
#[event]
pub struct ClaimUnlocked {
    pub claim: Pubkey,
}
#[event]
pub struct ClaimApproved {
    pub claim: Pubkey,
    /// The staker's points, burned by the approval.
    pub points_burned: u64,
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
    pub until: i64,
}
/// A suspension started or extended by the admin plus a second role.
#[event]
pub struct SuspensionRenewed {
    pub staker: Pubkey,
    pub approver: Pubkey,
    pub until: i64,
}
#[event]
pub struct StakeUnsuspended {
    pub staker: Pubkey,
}

// Yield.
#[event]
pub struct YieldCredited {
    pub amount: u64,
    pub staker_share: u64,
    pub backer_share: u64,
    pub protocol_share: u64,
}
#[event]
pub struct YieldClaimed {
    pub owner: Pubkey,
    pub amount: u64,
    pub backer: bool,
}
#[event]
pub struct RoundingReserveFunded {
    pub from: Pubkey,
    pub amount: u64,
}
/// A payment took `amount` from the rounding reserve.
#[event]
pub struct RoundingReserveUsed {
    pub amount: u64,
}
#[event]
pub struct YieldWithdrawn {
    pub treasury: Pubkey,
    pub amount: u64,
}

// Governance: every step is public.
#[event]
pub struct GovProposed {
    pub kind: u8,
    pub change: crate::state::GovChange,
    pub proposer: Pubkey,
    pub solo: bool,
    pub eta: i64,
}
#[event]
pub struct GovApproved {
    pub kind: u8,
    pub approver: Pubkey,
    pub eta: i64,
}
#[event]
pub struct GovCancelVote {
    pub kind: u8,
    pub by: Pubkey,
}
#[event]
pub struct GovCancelled {
    pub kind: u8,
    pub by: Pubkey,
}
#[event]
pub struct GovExecuted {
    pub kind: u8,
    pub change: crate::state::GovChange,
}
#[event]
pub struct PoolCapLowered {
    pub old_value: i64,
    pub new_value: i64,
}

// Yield leg (multichain `vault.rs` events). `kind`: what moved the money.
pub const LEG_PUSH: u8 = 0;
pub const LEG_PULL: u8 = 1;
pub const LEG_KEEPER_DEPLOY: u8 = 2;
pub const LEG_KEEPER_REBALANCE: u8 = 3;
pub const LEG_ADMIN_PROVIDE: u8 = 4;
#[event]
pub struct Deployed {
    pub kind: u8,
    pub amount: u64,
    pub collateral: u64,
}
/// A deposit minted less collateral than the reserve's own rate allows (recorded as received).
#[event]
pub struct DeployBelowFloor {
    pub amount: u64,
    pub collateral: u64,
    pub min_collateral: u64,
}
#[event]
pub struct Redeemed {
    pub kind: u8,
    pub collateral: u64,
    pub received: u64,
}
/// A redeem returned less than the book value that left: the venue lost money. Marked off
/// `total_staked`.
#[event]
pub struct DeploymentShortfall {
    pub principal_expected: u64,
    pub received: u64,
}
#[event]
pub struct Harvested {
    pub collateral: u64,
    pub received: u64,
}
/// A claim payout found the pool short of cash even after the venue: today counts as blocked time
/// (rule 18), the claim's collection window restarts, nothing is paid.
#[event]
pub struct CashShortDay {
    pub claim: Pubkey,
    pub day: i64,
    pub days_total: u64,
}
