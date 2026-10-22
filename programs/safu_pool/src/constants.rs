//! What clients read from the IDL: every rule number (re-exported from `pool_core::params`, never
//! retyped) and every PDA seed. Backend and frontend use these, not their own copies.

use anchor_lang::prelude::*;
use pool_core::params as p;

#[constant]
pub const DEMO_BUILD: bool = p::DEMO_BUILD;

// Fixed clocks (seconds). Demo values when built with `--features demo`.
#[constant]
pub const SECONDS_PER_DAY: i64 = p::SECONDS_PER_DAY;
#[constant]
pub const PENALTY_LOCK_SECS: i64 = p::PENALTY_LOCK_SECS;
#[constant]
pub const MAX_APPROVAL_WINDOW_SECS: i64 = p::MAX_APPROVAL_WINDOW_SECS;
#[constant]
pub const SETTINGS_TIMELOCK_SECS: i64 = p::SETTINGS_TIMELOCK_SECS;
#[constant]
pub const GOV_DELAY_SECS: i64 = p::GOV_DELAY_SECS;
#[constant]
pub const RECOVERY_DELAY_SECS: i64 = p::RECOVERY_DELAY_SECS;
#[constant]
pub const MAX_PENDING_DEPOSITS: u8 = p::MAX_PENDING_DEPOSITS as u8;
#[constant]
pub const USDC_UNIT: u64 = p::USDC_UNIT;
#[constant]
pub const MAX_ROUNDING_DRAW: u64 = p::MAX_ROUNDING_DRAW;
#[constant]
pub const DRIP_FULL_UTIL_BPS: u64 = p::DRIP_FULL_UTIL_BPS;

// Governance kinds: the slot in `Governance.pending` (`state::GovKind`).
use crate::state::{GovKind as G, GOV_KINDS as KINDS};
#[constant]
pub const GOV_KIND_COUNT: u8 = KINDS as u8;
#[constant]
pub const GOV_KIND_ADMIN: u8 = G::Admin as u8;
#[constant]
pub const GOV_KIND_CO_SIGNER: u8 = G::CoSigner as u8;
#[constant]
pub const GOV_KIND_GUARDIAN: u8 = G::Guardian as u8;
#[constant]
pub const GOV_KIND_ORACLE: u8 = G::Oracle as u8;
#[constant]
pub const GOV_KIND_YIELD_VENUE: u8 = G::YieldVenue as u8;
#[constant]
pub const GOV_KIND_TREASURY: u8 = G::Treasury as u8;
#[constant]
pub const GOV_KIND_DEPLOY_BPS: u8 = G::DeployBps as u8;
#[constant]
pub const GOV_KIND_REGISTRY_WRITER: u8 = G::RegistryWriter as u8;

// Adjustable settings: the live value is `Pool.settings[SETTING_x]`, never a constant. Slot numbers
// are `pool_core::settings::SettingKey`, re-exported so clients never retype them.
use pool_core::settings::{SettingKey as K, SETTING_COUNT as COUNT, SETTING_SLOTS as SLOTS};
#[constant]
pub const SETTING_SLOTS: u8 = SLOTS as u8;
#[constant]
pub const SETTING_COUNT: u8 = COUNT as u8;
#[constant]
pub const SETTING_MIN_STAKE_BPS: u8 = K::MinStakeBps as u8;
#[constant]
pub const SETTING_MAX_STAKE_BPS: u8 = K::MaxStakeBps as u8;
#[constant]
pub const SETTING_STAKER_YIELD_BPS: u8 = K::StakerYieldBps as u8;
#[constant]
pub const SETTING_BACKER_YIELD_BPS: u8 = K::BackerYieldBps as u8;
#[constant]
pub const SETTING_COOLDOWN_SECS: u8 = K::CooldownSecs as u8;
#[constant]
pub const SETTING_VESTING_SECS: u8 = K::VestingSecs as u8;
#[constant]
pub const SETTING_ADMIT_LOW_BPS: u8 = K::AdmitLowBps as u8;
#[constant]
pub const SETTING_ADMIT_MID_BPS: u8 = K::AdmitMidBps as u8;
#[constant]
pub const SETTING_ADMIT_HIGH_BPS: u8 = K::AdmitHighBps as u8;
#[constant]
pub const SETTING_PAYOUT_LOW_BPS: u8 = K::PayoutLowBps as u8;
#[constant]
pub const SETTING_PAYOUT_MID_BPS: u8 = K::PayoutMidBps as u8;
#[constant]
pub const SETTING_PAYOUT_HIGH_BPS: u8 = K::PayoutHighBps as u8;
#[constant]
pub const SETTING_BACKER_NOTICE_SECS: u8 = K::BackerNoticeSecs as u8;
#[constant]
pub const SETTING_BACKER_MATURITY_SECS: u8 = K::BackerMaturitySecs as u8;
#[constant]
pub const SETTING_PAUSE_MAX_SECS: u8 = K::PauseMaxSecs as u8;
#[constant]
pub const SETTING_PAUSE_GAP_SECS: u8 = K::PauseGapSecs as u8;
#[constant]
pub const SETTING_APPROVE_WINDOW_SECS: u8 = K::ApproveWindowSecs as u8;
#[constant]
pub const SETTING_INACTIVITY_SECS: u8 = K::InactivitySecs as u8;
#[constant]
pub const SETTING_POOL_CAP: u8 = K::PoolCap as u8;
#[constant]
pub const SETTING_TIER_A_RATIO: u8 = K::TierARatio as u8;
#[constant]
pub const SETTING_TIER_B_RATIO: u8 = K::TierBRatio as u8;
#[constant]
pub const SETTING_TIER_C_RATIO: u8 = K::TierCRatio as u8;
#[constant]
pub const SETTING_TIME_GATE_SECS: u8 = K::TimeGateSecs as u8;
#[constant]
pub const SETTING_CLAIM_WINDOW_SECS: u8 = K::ClaimWindowSecs as u8;
#[constant]
pub const SETTING_DRIP_MIN_SECS: u8 = K::DripMinSecs as u8;
#[constant]
pub const SETTING_DRIP_MAX_SECS: u8 = K::DripMaxSecs as u8;
#[constant]
pub const SETTING_POINTS_RATE_STAKER: u8 = K::PointsRateStaker as u8;
#[constant]
pub const SETTING_POINTS_RATE_BACKER: u8 = K::PointsRateBacker as u8;
#[constant]
pub const POINTS_RATE_ONE: u64 = p::POINTS_RATE_ONE;

// Coverage.
#[constant]
pub const BPS_DENOMINATOR: u64 = p::BPS_DENOMINATOR;
#[constant]
pub const MAX_COVERED_WALLETS: u8 = p::MAX_COVERED_WALLETS;
#[constant]
pub const MAX_TXIDS_PER_CLAIM: u8 = p::MAX_TXIDS_PER_CLAIM;
#[constant]
pub const TIER_A: u8 = p::TIER_A;
#[constant]
pub const TIER_B: u8 = p::TIER_B;
#[constant]
pub const TIER_C: u8 = p::TIER_C;
#[constant]
pub const TIER_COVERAGE_BPS: u64 = p::TIER_COVERAGE_BPS;
#[constant]
pub const USDC_DECIMALS: u8 = p::USDC_DECIMALS;

// Yield leg and yield indexes.
#[constant]
pub const MAX_DEPLOY_BPS: u64 = p::MAX_DEPLOY_BPS;
/// Clients read it from the IDL to show yield owed (`pool_core::yields::owed`).
#[constant]
pub const YIELD_INDEX_PRECISION: u128 = p::YIELD_INDEX_PRECISION;

// PDA seeds.
#[constant]
pub const SEED_POOL: &[u8] = b"pool";
#[constant]
pub const SEED_VAULT: &[u8] = b"vault";
#[constant]
pub const SEED_STAKE: &[u8] = b"stake";
#[constant]
pub const SEED_BACKER: &[u8] = b"backer";
#[constant]
pub const SEED_CLAIM: &[u8] = b"claim";
#[constant]
pub const SEED_OVERRIDE: &[u8] = b"override";
#[constant]
pub const SEED_REVOKED: &[u8] = b"revoked";
#[constant]
pub const SEED_COVERED: &[u8] = b"covered";
#[constant]
pub const SEED_STAKER_WALLETS: &[u8] = b"staker_wallets";
#[constant]
pub const SEED_GOVERNANCE: &[u8] = b"governance";
#[constant]
pub const SEED_QUEUE: &[u8] = b"queue";
#[constant]
pub const SEED_WATCH: &[u8] = b"watch";
/// Watch-list places each upkeep checks.
#[constant]
pub const UPKEEP_WATCH_PER_CALL: u64 = p::UPKEEP_WATCH_PER_CALL as u64;

// Oracle approval domain tag (version it when the message layout or the entitlement unit changes).
// USDC pool: entitlement in USDC base units, a new tag so no SOL-pool approval can ever match.
#[constant]
pub const APPROVAL_DOMAIN: &[u8] = b"SAFU_CLAIM_APPROVAL_SOLANA_USDC_V1";

// Cluster tag, fixed at `initialize` and signed into every oracle approval.
#[constant]
pub const CLUSTER_LOCALNET: u8 = 0;
#[constant]
pub const CLUSTER_DEVNET: u8 = 1;
#[constant]
pub const CLUSTER_MAINNET: u8 = 2;
