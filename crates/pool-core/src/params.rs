//! Every rule constant, in one place.
//!
//! The program re-exports the ones clients need as Anchor `#[constant]`s, so they land in the IDL.
//! Backend and frontend read the IDL, never a copy of these numbers.
//!
//! Values follow the multichain protection pool (`safu_multichain/contracts/protection-pool`)
//! unless a line says otherwise. A value marked **(setting default)** is where a new pool's
//! adjustable setting starts; the live value is in the pool's settings (`settings.rs`), and clients
//! read that, not this. Everything else here is fixed.
//!
//! Clocks: the `demo` feature swaps every duration for its fast devnet value. Both values sit on
//! one line through [`clock`], so a reader always sees the pair.

/// Basis points in 100%.
pub const BPS_DENOMINATOR: u64 = 10_000;

pub const SECONDS_PER_MINUTE: i64 = 60;
pub const SECONDS_PER_HOUR: i64 = 60 * SECONDS_PER_MINUTE;
pub const SECONDS_PER_DAY: i64 = 24 * SECONDS_PER_HOUR;

/// Picks the normal or the demo value of a duration.
pub(crate) const fn clock(normal: i64, demo: i64) -> i64 {
    if cfg!(feature = "demo") {
        demo
    } else {
        normal
    }
}

/// `true` in the fast-clock devnet build. Exported so a client can show which build it is talking to.
pub const DEMO_BUILD: bool = cfg!(feature = "demo");

// -----------------------------------------------------------------------
// Claim clocks
// -----------------------------------------------------------------------

/// A stake must be this old before a claim on it can be approved. (setting default; each stake keeps
/// the value it staked under)
pub const TIME_GATE_SECS: i64 = clock(90 * SECONDS_PER_DAY, 2 * SECONDS_PER_MINUTE);
/// Wait between approval and the first payout. (setting default)
pub const COOLDOWN_SECS: i64 = clock(7 * SECONDS_PER_DAY, SECONDS_PER_MINUTE);
/// Linear vesting of the payout, starting at cooldown end. (setting default)
pub const VESTING_SECS: i64 = clock(45 * SECONDS_PER_DAY, 5 * SECONDS_PER_MINUTE);
/// Once the time gate is met, the staker has this long to approve the claim. (setting default)
pub const APPROVE_WINDOW_SECS: i64 = clock(100 * SECONDS_PER_DAY, SECONDS_PER_HOUR);
/// An active claim with no collection for this long can be expired by anyone. (setting default)
pub const COLLECTION_INACTIVITY_SECS: i64 = clock(100 * SECONDS_PER_DAY, SECONDS_PER_HOUR);
/// Withdraw lock after a false-positive cancel of an approved claim.
pub const PENALTY_LOCK_SECS: i64 = clock(365 * SECONDS_PER_DAY, 10 * SECONDS_PER_MINUTE);
/// A claim must be filed within this long of the hack. Real time in both builds. (setting default;
/// each stake keeps the value it staked under)
pub const CLAIM_WINDOW_SECS: i64 = 30 * SECONDS_PER_DAY;
/// Longest life of one oracle approval signature. Real time in both builds.
pub const MAX_APPROVAL_WINDOW_SECS: i64 = 24 * SECONDS_PER_HOUR;
/// Longest pause. Real time in both builds (multichain `PAUSE_MAX_SECONDS`). (setting default)
pub const PAUSE_MAX_SECS: i64 = 30 * SECONDS_PER_DAY;
/// A new pause may start only more than this long after the last one ended (back-to-back
/// pauses would otherwise freeze claims without limit). Real time in both builds and never below
/// `CLAIM_WINDOW_SECS`: the pool is paused at most half the time, and a claim can
/// still be filed across one pause only, which the stored pause record credits exactly.
/// (setting default)
pub const PAUSE_GAP_SECS: i64 = CLAIM_WINDOW_SECS;
/// Wait between a setting change's co-signer approval and its execution. Fixed: a setting could
/// otherwise shorten its own notice.
pub const SETTINGS_TIMELOCK_SECS: i64 = clock(7 * SECONDS_PER_DAY, 5 * SECONDS_PER_MINUTE);
/// Governance: wait between the second role's approval and execution (multichain `GOV_DELAY_SECONDS`).
pub const GOV_DELAY_SECS: i64 = clock(7 * SECONDS_PER_DAY, 5 * SECONDS_PER_MINUTE);
/// Governance: a role replacement proposed by one role alone (the other two lost) waits this long,
/// and any other working role can cancel it (multichain `RECOVERY_DELAY_SECONDS`).
pub const RECOVERY_DELAY_SECS: i64 = clock(90 * SECONDS_PER_DAY, 15 * SECONDS_PER_MINUTE);

// -----------------------------------------------------------------------
// Backers
// -----------------------------------------------------------------------

/// New backer money counts toward capacity only after this wait: from day 1. (setting default)
pub const BACKER_MATURITY_SECS: i64 = 0;
/// Deposits one backer may have waiting to count at once (only when the maturity setting is above
/// zero). Each keeps its own clock.
pub const MAX_PENDING_DEPOSITS: usize = 8;
/// Watch-list places each user transaction's upkeep checks (multichain `UPKEEP_WATCH_PER_CALL`).
pub const UPKEEP_WATCH_PER_CALL: usize = 2;
/// Notice before a backer withdrawal. UNUSED since the daily drip replaced it; the setting slot is
/// kept so the numbering matches multichain. (setting default)
pub const BACKER_NOTICE_SECS: i64 = clock(30 * SECONDS_PER_DAY, SECONDS_PER_MINUTE);
/// Backer exit drip length: from the min (idle pool) to the max (at `DRIP_FULL_UTIL_BPS` utilisation
/// and above). (setting defaults)
pub const DRIP_MIN_SECS: i64 = clock(7 * SECONDS_PER_DAY, 2 * SECONDS_PER_MINUTE);
pub const DRIP_MAX_SECS: i64 = clock(90 * SECONDS_PER_DAY, 10 * SECONDS_PER_MINUTE);
/// Utilisation (allocated / capacity, bps) at which the drip is longest.
pub const DRIP_FULL_UTIL_BPS: u64 = 5_000;

// -----------------------------------------------------------------------
// Stake bounds, as bps of the pool cap (setting defaults; the cap starts at the deploy value in
// `config/` and is a setting too).
// -----------------------------------------------------------------------

/// 0.01% and 0.1% of the cap: $10 and $100 at a $100,000 cap (multichain).
pub const MIN_STAKE_BPS: u64 = 1;
pub const MAX_STAKE_BPS: u64 = 10;

/// Covered wallets per staker, forever, any mix of chains (Solana, Ethereum, Stellar). Same as
/// multichain.
pub const MAX_COVERED_WALLETS: u8 = 3;

/// Drain transactions one claim may list. The program sees one claim either way: the claim API checks
/// this, and the site reads it from the IDL. Same as multichain.
pub const MAX_TXIDS_PER_CLAIM: u8 = 20;

/// USDC decimals. `initialize` refuses a mint with any other.
pub const USDC_DECIMALS: u8 = 6;
/// Base units in one USDC.
pub const USDC_UNIT: u64 = 10u64.pow(USDC_DECIMALS as u32);
/// Largest gap a payment may take from the rounding reserve: 0.001 USDC (multichain: the same 0.001
/// of a unit). Covers venue rounding of a few base units; a bigger gap is a real loss and still
/// refuses.
pub const MAX_ROUNDING_DRAW: u64 = USDC_UNIT / 1_000;

// -----------------------------------------------------------------------
// Tiers: coverage = stake x ratio x TIER_COVERAGE_BPS / BPS_DENOMINATOR. Tier codes 1=A, 2=B, 3=C.
// Ratios are setting defaults; each stake keeps the ratios it staked under, and a later raise reaches
// it too (the higher of the two applies).
// -----------------------------------------------------------------------

pub const TIER_A: u8 = 1;
pub const TIER_B: u8 = 2;
pub const TIER_C: u8 = 3;
pub const TIER_A_RATIO: u64 = 15;
pub const TIER_B_RATIO: u64 = 10;
pub const TIER_C_RATIO: u64 = 5;
pub const TIER_COVERAGE_BPS: u64 = 10_000;

// -----------------------------------------------------------------------
// Daily caps, by utilisation (allocated / capacity)
// -----------------------------------------------------------------------

/// Below this utilisation the LOW rates apply.
pub const UTIL_LOW_BPS: u64 = 2_000;
/// Below this (and at or above LOW) the MID rates apply; at or above it, HIGH.
pub const UTIL_MID_BPS: u64 = 5_000;
/// New entitlement admitted per day, bps of capacity. (setting defaults)
pub const ADMIT_LOW_BPS: u64 = 2_500;
pub const ADMIT_MID_BPS: u64 = 1_000;
pub const ADMIT_HIGH_BPS: u64 = 300;
/// Claim payouts per day, bps of max(capacity now, capacity when the claim was approved). (setting
/// defaults)
pub const PAYOUT_LOW_BPS: u64 = 500;
pub const PAYOUT_MID_BPS: u64 = 300;
pub const PAYOUT_HIGH_BPS: u64 = 100;
/// Rate used when the payout base is zero (multichain `dynamic_outflow_bps`).
pub const PAYOUT_EMPTY_POOL_BPS: u64 = 100;
/// Oracle submissions per day = total stakers / this, at least 1.
pub const ORACLE_DAILY_LIMIT_DIVISOR: u64 = 10;

// -----------------------------------------------------------------------
// Yield leg (multichain: DeFindex vault)
// -----------------------------------------------------------------------

/// Ceiling on the share of capacity in the yield venue. The live share (`Pool.deploy_bps`) starts at
/// 0 and changes only through governance (multichain `MAX_DEPLOY_BPS`).
pub const MAX_DEPLOY_BPS: u64 = 8_000;
/// Idle USDC is deployed only once it is at least this share of capacity.
pub const AUTO_PUSH_MIN_BPS: u64 = 100;
/// Most a rebalance may lose to the venue's rate, bps of the expected amount.
pub const MAX_REBALANCE_SLIPPAGE_BPS: u64 = 500;
/// Most growth one harvest may book, per day since the last harvest, bps of book value.
pub const HARVEST_MAX_GROWTH_BPS_PER_DAY: u64 = 10;

/// Compute units a harvest inside a user call needs left before it starts: klend's refresh on a stale
/// price, the redeem, and the rest of the heaviest money call after it. Measured 22 Oct at ~200k (a
/// new stake: harvest, then the push into klend), plus ~20% margin. Above Solana's 200k default, so
/// a caller on the default skips the harvest; the site asks for 400k (`computeUnitLimit`). Checked
/// by `programs/safu_pool/tests/kamino.rs` `heavy_calls_harvest_at_the_site_limit_and_never_fail_at_the_default`.
pub const HARVEST_MIN_CU: u64 = 240_000;

/// Compute units the push of new cash into the venue needs left before it starts: klend's refresh on
/// a stale price, the deposit, and the rest of the call. Measured 22 Oct at ~146k, plus ~20% margin.
/// A caller on Solana's 200k default skips the push (the keeper's `auto_deploy_liquidity` does it
/// later); the site asks for 400k.
pub const PUSH_MIN_CU: u64 = 175_000;

// -----------------------------------------------------------------------
// Yield split
// -----------------------------------------------------------------------

/// Share of yield on staker capital that goes to stakers; the rest is protocol revenue. (setting
/// default)
pub const STAKER_YIELD_BPS: u64 = BPS_DENOMINATOR;
/// Share of yield on matured backer capital that goes to backers; the rest is protocol revenue.
/// (setting default)
pub const BACKER_YIELD_BPS: u64 = BPS_DENOMINATOR;
/// Points rate unit: a rate of this many is 1 point per USDC per day. (both points settings default
/// to it)
pub const POINTS_RATE_ONE: u64 = 10_000;
/// Fixed-point scale of the additive yield indexes.
pub const YIELD_INDEX_PRECISION: u128 = 1_000_000_000_000;

const _: () = assert!(MIN_STAKE_BPS <= MAX_STAKE_BPS);
const _: () = assert!(UTIL_LOW_BPS < UTIL_MID_BPS);
const _: () = assert!(ADMIT_LOW_BPS >= ADMIT_MID_BPS && ADMIT_MID_BPS >= ADMIT_HIGH_BPS);
const _: () = assert!(PAYOUT_LOW_BPS >= PAYOUT_MID_BPS && PAYOUT_MID_BPS >= PAYOUT_HIGH_BPS);
const _: () = assert!(MAX_DEPLOY_BPS <= BPS_DENOMINATOR);
const _: () = assert!(TIER_A_RATIO >= TIER_B_RATIO && TIER_B_RATIO >= TIER_C_RATIO);
const _: () = assert!(DRIP_MIN_SECS <= DRIP_MAX_SECS);
const _: () = assert!(STAKER_YIELD_BPS <= BPS_DENOMINATOR && BACKER_YIELD_BPS <= BPS_DENOMINATOR);
const _: () = assert!(ORACLE_DAILY_LIMIT_DIVISOR > 0);
