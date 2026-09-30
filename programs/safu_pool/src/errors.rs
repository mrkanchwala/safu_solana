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
    // ---- claims (B2)
    #[msg("Signer is not the oracle")]
    NotOracle,
    #[msg("Signer is neither the admin nor the co-signer")]
    NotAdminOrCoSigner,
    #[msg("Unknown tier")]
    InvalidTier,
    #[msg("Entitlement must be above zero")]
    EntitlementNotPositive,
    #[msg("Entitlement is above the tier ceiling for this stake")]
    EntitlementExceedsTierCap,
    #[msg("Hack time is in the future")]
    HackTimestampInFuture,
    #[msg("Hack happened before the stake")]
    HackPredatesStake,
    #[msg("Claim window after the hack has passed")]
    ClaimWindowExpired,
    #[msg("Oracle approval has expired")]
    SignatureExpired,
    #[msg("Oracle approval deadline is too far out")]
    SignatureDeadlineTooFar,
    #[msg("This oracle approval was revoked")]
    ApprovalRevoked,
    #[msg("No Ed25519 instruction directly before this instruction")]
    MissingEd25519Instruction,
    #[msg("Ed25519 instruction data is malformed or offsets are out of bounds")]
    MalformedEd25519Instruction,
    #[msg("Ed25519 instruction must carry exactly one signature")]
    WrongSignatureCount,
    #[msg("Ed25519 offsets must all point into the precompile instruction itself")]
    OffsetsOutsideEd25519Instruction,
    #[msg("Approval is not signed by the oracle")]
    WrongOracleSigner,
    #[msg("Signed message does not match this approval")]
    ApprovalMessageMismatch,
    #[msg("Claim submission must be a top-level instruction")]
    ApprovalNotTopLevel,
    #[msg("Payouts on this stake are suspended")]
    StakeSuspended,
    #[msg("A claim is already open on this stake")]
    ClaimAlreadyActiveForStake,
    #[msg("A claim is already queued on this stake")]
    ClaimAlreadyQueued,
    #[msg("A claim for this wallet and transaction already exists")]
    ClaimAlreadyExists,
    #[msg("Claim is not queued")]
    NoSuchQueuedClaim,
    #[msg("A different claim is open on this stake")]
    WalletHasDifferentActiveClaim,
    #[msg("The stake behind this queued claim changed")]
    QueuedClaimStakeChanged,
    #[msg("Queued claim still does not fit")]
    QueueReleaseNotYetEligible,
    #[msg("Queued claim is still inside its claim window")]
    QueueNotYetExpired,
    #[msg("Claim is not waiting on the time gate")]
    ClaimNotPending,
    #[msg("Time gate not met yet")]
    TimeGateNotMet,
    #[msg("Claim is not waiting for approval")]
    ClaimNotAwaitingApproval,
    #[msg("Approval window has passed")]
    ApprovalWindowExpired,
    #[msg("Claim does not belong to this stake")]
    ClaimStakeMismatch,
    #[msg("Approval window has not passed")]
    ApprovalWindowNotExpired,
    #[msg("Claim is not active")]
    ClaimNotActive,
    #[msg("Claim is fully paid")]
    ClaimFullyStreamed,
    #[msg("Claim has been collected recently")]
    ClaimNotStale,
    #[msg("Cooldown has not passed")]
    CooldownNotPassed,
    #[msg("Nothing vested yet")]
    NothingVested,
    #[msg("Daily payout cap reached; try again tomorrow")]
    DailyOutflowCapReached,
    #[msg("Claim cannot be cancelled in this state")]
    ClaimNotCancellable,
    #[msg("Override terms differ from the pending request")]
    OverrideParamsMismatch,
    #[msg("Claim is already completed")]
    ClaimAlreadyCompleted,
    #[msg("Pool cannot cover this claim")]
    Insolvent,
    #[msg("Revocation account does not match this approval")]
    WrongRevocationAccount,
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
            CoreError::InvalidTier => PoolError::InvalidTier,
            CoreError::EntitlementNotPositive => PoolError::EntitlementNotPositive,
            CoreError::EntitlementExceedsTierCap => PoolError::EntitlementExceedsTierCap,
            CoreError::HackTimestampInFuture => PoolError::HackTimestampInFuture,
            CoreError::HackPredatesStake => PoolError::HackPredatesStake,
            CoreError::ClaimWindowExpired => PoolError::ClaimWindowExpired,
            CoreError::SignatureExpired => PoolError::SignatureExpired,
            CoreError::SignatureDeadlineTooFar => PoolError::SignatureDeadlineTooFar,
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
