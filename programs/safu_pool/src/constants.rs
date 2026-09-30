//! What clients read from the IDL: every rule number (re-exported from `pool_core::params`, never
//! retyped) and every PDA seed. Backend and frontend use these, not their own copies.

use anchor_lang::prelude::*;
use pool_core::params as p;

#[constant]
pub const DEMO_BUILD: bool = p::DEMO_BUILD;

// Clocks (seconds). Demo values when built with `--features demo`.
#[constant]
pub const TIME_GATE_SECS: i64 = p::TIME_GATE_SECS;
#[constant]
pub const COOLDOWN_SECS: i64 = p::COOLDOWN_SECS;
#[constant]
pub const VESTING_SECS: i64 = p::VESTING_SECS;
#[constant]
pub const APPROVE_WINDOW_SECS: i64 = p::APPROVE_WINDOW_SECS;
#[constant]
pub const COLLECTION_INACTIVITY_SECS: i64 = p::COLLECTION_INACTIVITY_SECS;
#[constant]
pub const PENALTY_LOCK_SECS: i64 = p::PENALTY_LOCK_SECS;
#[constant]
pub const CLAIM_WINDOW_SECS: i64 = p::CLAIM_WINDOW_SECS;
#[constant]
pub const MAX_APPROVAL_WINDOW_SECS: i64 = p::MAX_APPROVAL_WINDOW_SECS;
#[constant]
pub const PAUSE_MAX_SECS: i64 = p::PAUSE_MAX_SECS;
#[constant]
pub const BACKER_MATURITY_SECS: i64 = p::BACKER_MATURITY_SECS;
#[constant]
pub const BACKER_NOTICE_SECS: i64 = p::BACKER_NOTICE_SECS;

// Stake bounds and coverage.
#[constant]
pub const BPS_DENOMINATOR: u64 = p::BPS_DENOMINATOR;
#[constant]
pub const MIN_STAKE_BPS: u64 = p::MIN_STAKE_BPS;
#[constant]
pub const MAX_STAKE_BPS: u64 = p::MAX_STAKE_BPS;
#[constant]
pub const MAX_COVERED_WALLETS: u8 = p::MAX_COVERED_WALLETS;
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

// Yield split.
#[constant]
pub const STAKER_YIELD_BPS: u64 = p::STAKER_YIELD_BPS;
#[constant]
pub const BACKER_YIELD_BPS: u64 = p::BACKER_YIELD_BPS;
#[constant]
pub const DEPLOY_BPS: u64 = p::DEPLOY_BPS;

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

// Cluster tag, fixed at `initialize` and signed into every oracle approval.
#[constant]
pub const CLUSTER_LOCALNET: u8 = 0;
#[constant]
pub const CLUSTER_DEVNET: u8 = 1;
#[constant]
pub const CLUSTER_MAINNET: u8 = 2;
