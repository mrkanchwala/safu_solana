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
