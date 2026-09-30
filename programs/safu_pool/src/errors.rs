//! Error codes. Names follow the multichain `PoolError` where the rule is the same.
//! New variants go at the end: the numbers are part of the client interface.

use anchor_lang::prelude::*;
use pool_core::CoreError;

#[error_code]
pub enum PoolError {
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("Signer is not the pool admin")]
    NotAdmin,
    #[msg("Signer is not the upgrade authority of this program")]
    NotUpgradeAuthority,
    #[msg("Admin, oracle and co-signer must be three different keys")]
    RoleCollision,
    #[msg("Unknown cluster tag")]
    InvalidCluster,
    #[msg("Pool cap is too small for the stake bounds")]
    InvalidPoolCap,
    #[msg("Marinade account does not match the pool's Marinade")]
    WrongMarinadeAccount,
    #[msg("Pool is paused")]
    Paused,
    #[msg("Pool is not paused")]
    NotPaused,
    #[msg("New pool cap must be higher than the current one")]
    PoolCapNotIncreased,
    #[msg("Stake is outside the pool's stake bounds")]
    StakeOutOfRange,
    #[msg("Stake would take the pool above its cap")]
    PoolCapExceeded,
    #[msg("This address already has a live stake")]
    AlreadyStaked,
    #[msg("A claim on this address was approved: it can never stake again")]
    AddressHasApprovedClaim,
    #[msg("Beneficiary cannot be the oracle")]
    BeneficiaryIsOracle,
    #[msg("Beneficiary cannot be the admin")]
    BeneficiaryIsAdmin,
    #[msg("Beneficiary cannot be the co-signer")]
    BeneficiaryIsCoSigner,
    #[msg("No live stake")]
    NoActiveStake,
    #[msg("Stake was forfeited to an approved claim")]
    StakeForfeited,
    #[msg("A claim is open on this stake")]
    ClaimActive,
    #[msg("A claim is queued on this stake")]
    ClaimQueuedForStake,
    #[msg("Withdrawal is locked after a cancelled claim")]
    PenaltyLockActive,
    #[msg("Beneficiary account does not match the stake")]
    WrongBeneficiary,
    #[msg("Not enough liquid SOL in the pool right now")]
    InsufficientLiquidity,
    #[msg("Amount must be above zero")]
    AmountNotPositive,
    #[msg("Nothing pending to mature")]
    NoPendingBacking,
    #[msg("Backing has not matured yet")]
    BackingNotMature,
    #[msg("A backer withdrawal is already pending")]
    BackerWithdrawalPending,
    #[msg("Amount is above the matured balance")]
    BackerAmountExceedsBalance,
    #[msg("No backer withdrawal pending")]
    NoBackerWithdrawal,
    #[msg("Backer notice period has not passed")]
    BackerNoticeNotPassed,
    #[msg("Open claims need this capital")]
    BackerCapitalNotFree,
    #[msg("Signer is not the registry writer")]
    NotRegistryWriter,
    #[msg("This staker already registered this wallet")]
    AlreadyRegistered,
    #[msg("Another staker registered this wallet; registrations are permanent")]
    WalletTakenByOtherStaker,
    #[msg("Staker already has the maximum number of covered wallets")]
    StakerLimitReached,
}

impl From<CoreError> for PoolError {
    fn from(e: CoreError) -> Self {
        match e {
            CoreError::Overflow | CoreError::DivideByZero | CoreError::InvalidParameter => {
                PoolError::MathOverflow
            }
            CoreError::StakeOutOfRange => PoolError::StakeOutOfRange,
            CoreError::PoolCapExceeded => PoolError::PoolCapExceeded,
            CoreError::BackerCapitalNotFree => PoolError::BackerCapitalNotFree,
        }
    }
}

/// `?` on a `pool_core` result inside an instruction.
pub trait CoreResultExt<T> {
    fn core(self) -> Result<T>;
}

impl<T> CoreResultExt<T> for pool_core::Result<T> {
    fn core(self) -> Result<T> {
        self.map_err(|e| error!(PoolError::from(e)))
    }
}
