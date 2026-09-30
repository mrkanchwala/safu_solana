//! What clients read from the IDL: every rule number (re-exported from `pool_core::params`, never
//! retyped) and every PDA seed. Backend and frontend use these, not their own copies.

use anchor_lang::prelude::*;
use pool_core::params as p;

#[constant]
pub const DEMO_BUILD: bool = p::DEMO_BUILD;

// Fixed clocks (seconds). Demo values when built with `--features demo`.
#[constant]
pub const TIME_GATE_SECS: i64 = p::TIME_GATE_SECS;
#[constant]
pub const PENALTY_LOCK_SECS: i64 = p::PENALTY_LOCK_SECS;
#[constant]
pub const CLAIM_WINDOW_SECS: i64 = p::CLAIM_WINDOW_SECS;
#[constant]
pub const MAX_APPROVAL_WINDOW_SECS: i64 = p::MAX_APPROVAL_WINDOW_SECS;
#[constant]
pub const SETTINGS_TIMELOCK_SECS: i64 = p::SETTINGS_TIMELOCK_SECS;

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
pub const TIER_A_RATIO: u64 = p::TIER_A_RATIO;
#[constant]
pub const TIER_B_RATIO: u64 = p::TIER_B_RATIO;
#[constant]
pub const TIER_C_RATIO: u64 = p::TIER_C_RATIO;
#[constant]
pub const TIER_COVERAGE_BPS: u64 = p::TIER_COVERAGE_BPS;

// Marinade leg and yield indexes.
#[constant]
pub const DEPLOY_BPS: u64 = p::DEPLOY_BPS;
/// Clients read it from the IDL to show yield owed (`pool_core::yields::owed`).
#[constant]
pub const YIELD_INDEX_PRECISION: u128 = p::YIELD_INDEX_PRECISION;

// PDA seeds.
#[constant]
pub const SEED_POOL: &[u8] = b"pool";
#[constant]
pub const SEED_VAULT: &[u8] = b"vault";
#[constant]
pub const SEED_MSOL: &[u8] = b"msol";
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

/// The pool's asset today (`Pool::asset_mint`): the native SOL mint address.
pub const NATIVE_SOL_MINT: Pubkey =
    anchor_lang::prelude::pubkey!("So11111111111111111111111111111111111111112"); // hardcode-ok: the network's native mint, not a deploy value

// Oracle approval domain tag (version it when the message layout changes).
#[constant]
pub const APPROVAL_DOMAIN: &[u8] = b"SAFU_CLAIM_APPROVAL_SOLANA_V1";

// Cluster tag, fixed at `initialize` and signed into every oracle approval.
#[constant]
pub const CLUSTER_LOCALNET: u8 = 0;
#[constant]
pub const CLUSTER_DEVNET: u8 = 1;
#[constant]
pub const CLUSTER_MAINNET: u8 = 2;

// Marinade facts clients need (re-exported from `pool_core::marinade`, never retyped): the leg's
// PDA seeds, the `State` offsets of the two token accounts it reads, and the fee inputs the site
// shows before a payout that needs an instant unstake.
use pool_core::marinade as m;
#[constant]
pub const MARINADE_SEED_LIQ_POOL_SOL_LEG: &[u8] = m::SEED_LIQ_POOL_SOL_LEG;
#[constant]
pub const MARINADE_SEED_LIQ_POOL_MSOL_LEG_AUTHORITY: &[u8] = m::SEED_LIQ_POOL_MSOL_LEG_AUTHORITY;
#[constant]
pub const MARINADE_SEED_RESERVE: &[u8] = m::SEED_RESERVE;
#[constant]
pub const MARINADE_SEED_MSOL_MINT_AUTHORITY: &[u8] = m::SEED_MSOL_MINT_AUTHORITY;
#[constant]
pub const MARINADE_STATE_TREASURY_MSOL: u64 = m::STATE_TREASURY_MSOL as u64;
#[constant]
pub const MARINADE_STATE_LIQ_POOL_MSOL_LEG: u64 = m::STATE_LIQ_POOL_MSOL_LEG as u64;
#[constant]
pub const MARINADE_STATE_LP_LIQUIDITY_TARGET: u64 = m::STATE_LP_LIQUIDITY_TARGET as u64;
#[constant]
pub const MARINADE_STATE_LP_MAX_FEE_BPS: u64 = m::STATE_LP_MAX_FEE_BPS as u64;
#[constant]
pub const MARINADE_STATE_LP_MIN_FEE_BPS: u64 = m::STATE_LP_MIN_FEE_BPS as u64;
