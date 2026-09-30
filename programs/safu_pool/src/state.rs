//! Accounts. Seeds live in `constants.rs`; see `docs/PORT_MAP.md` for the multichain equivalent of each.

use anchor_lang::prelude::*;
use pool_core::params::MAX_COVERED_WALLETS;
use pool_core::settings::{self, Settings, SETTING_SLOTS};

/// Layout version of every record below. An upgrade that changes a layout bumps it and migrates;
/// the spare `reserved` bytes let most new fields land without resizing anyone's account.
pub const ACCOUNT_VERSION: u8 = 1;

/// A setting change on its way: proposed by the admin, approved by the co-signer (starting the
/// timelock), executed by anyone after `eta`.
#[derive(
    AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace,
)]
pub struct PendingSetting {
    pub value: i64,
    /// Earliest execution; 0 until approved.
    pub eta: i64,
    /// 0 none, 1 proposed, 2 approved.
    pub state: u8,
}

pub const PENDING_NONE: u8 = 0;
pub const PENDING_PROPOSED: u8 = 1;
pub const PENDING_APPROVED: u8 = 2;

/// The one pool. PDA `[SEED_POOL]`. Holds roles, Marinade addresses, settings, totals, yield books
/// and the current day's counters (only today is ever read).
#[account]
#[derive(InitSpace)]
pub struct Pool {
    pub version: u8,
    /// What the pool holds and pays in. The native SOL mint today; a later upgrade can move the
    /// pool to another asset (e.g. USDC) and migrate, without a new pool.
    pub asset_mint: Pubkey,
    pub admin: Pubkey,
    pub co_signer: Pubkey,
    /// Signs `submit_claim` and the Ed25519 approval in front of it.
    pub oracle: Pubkey,
    /// Backend key that registers covered wallets after an off-chain ownership proof.
    pub registry_writer: Pubkey,
    /// Receives protocol revenue (`withdraw_yield`).
    pub treasury: Pubkey,
    pub marinade_program: Pubkey,
    pub marinade_state: Pubkey,
    pub msol_mint: Pubkey,
    /// The pool's mSOL token account, owned by the vault PDA.
    pub pool_msol: Pubkey,
    /// Fixed at `initialize`, signed into every oracle approval.
    pub cluster: u8,
    pub bump: u8,
    pub vault_bump: u8,
    /// Every adjustable number, one slot per `pool_core::settings::SettingKey`. Read only through
    /// [`Pool::settings`]. Unused slots are room for settings an upgrade adds.
    pub settings: [i64; SETTING_SLOTS],
    pub pending_settings: [PendingSetting; SETTING_SLOTS],
    /// Unix seconds; paused while `now < paused_until`. After `unpause` it holds the moment the
    /// pause ended, so it is always the end of the last pause (0 = never paused).
    pub paused_until: i64,
    /// Start of the last pause (0 = never paused).
    pub pause_started_at: i64,
    /// Seconds paused in every pause before the last one. With the two above, the pause clock that
    /// stops claim windows during a pause (audit X1).
    pub paused_before: i64,

    pub total_staked: u64,
    pub total_stakers: u64,
    /// Matured backer money: counts toward capacity.
    pub total_backed: u64,
    /// Backer money still maturing: held, not counted.
    pub total_backed_pending: u64,
    /// Unpaid entitlement of every admitted claim.
    pub total_allocated: u64,

    pub staker_yield_index: u128,
    pub backer_yield_index: u128,
    /// Yield owed to stakers / backers, kept liquid, never spent on claims.
    pub staker_yield_reserved: u64,
    pub backer_yield_reserved: u64,
    /// Protocol revenue: the treasury may withdraw it; until then claims may use it.
    pub protocol_yield_balance: u64,
    pub total_extracted_yield: u64,

    /// mSOL the pool holds on its books (a donation to the token account is not counted).
    pub deployed_msol: u64,
    /// SOL cost of `deployed_msol` (book value, never marked to market).
    pub deployed_book: u64,
    pub last_harvest_at: i64,

    /// Day number (`unix / SECONDS_PER_DAY`) the counters below belong to.
    pub day: i64,
    pub day_admitted: u64,
    pub day_oracle_count: u64,
    pub day_outflow: u64,
    /// Room for fields a later upgrade adds.
    pub reserved: [u8; 256],
}

impl Pool {
    pub fn is_paused(&self, now: i64) -> bool {
        now < self.paused_until
    }

    /// The live settings: the one read path for every adjustable number.
    pub fn settings(&self) -> Settings {
        Settings {
            values: self.settings,
        }
    }

    /// Seconds spent paused by time `t` (see `pool_core::settings::paused_secs_at`).
    pub fn paused_secs_at(&self, t: i64) -> i64 {
        settings::paused_secs_at(
            self.paused_before,
            self.pause_started_at,
            self.paused_until,
            t,
        )
    }

    /// Claim clock for a window that started when the paused total was `mark`.
    pub fn claim_clock(&self, now: i64, mark: i64) -> i64 {
        settings::claim_clock(now, self.paused_secs_at(now), mark)
    }
}

/// One staker's stake. PDA `[SEED_STAKE, pool, staker]`. Closed when all of it is withdrawn (or
/// taken by emergency exit); a forfeited record is never closed, so that address can never stake
/// again.
#[account]
#[derive(InitSpace)]
pub struct StakeRecord {
    pub version: u8,
    pub staker: Pubkey,
    /// Receives withdrawals, yield and claim payouts.
    pub beneficiary: Pubkey,
    /// Principal; a partial withdrawal lowers it (and with it the coverage ceiling). Kept (not
    /// zeroed) when forfeited: the override path reads it.
    pub amount: u64,
    pub yield_index_at: u128,
    pub staked_at: i64,
    pub penalty_locked_until: i64,
    /// Set when a claim on this stake is approved. Cleared only by `cancel_claim` undoing it.
    pub forfeited: bool,
    /// The claim whose approval forfeited this stake. Only that claim may re-use the forfeited
    /// principal (a 2-of-2 override correcting it); any other claim on this stake is refused.
    pub forfeited_by: Option<Pubkey>,
    /// Admin hold on payouts. Does not block principal withdrawal.
    pub suspended: bool,
    /// Claim account open on this stake (admitted, not yet terminal).
    pub active_claim: Option<Pubkey>,
    /// Claim account queued on this stake.
    pub reserved_claim: Option<Pubkey>,
    pub bump: u8,
    /// Room for fields a later upgrade adds.
    pub reserved: [u8; 64],
}

/// One backer. PDA `[SEED_BACKER, pool, backer]`.
#[account]
#[derive(InitSpace)]
pub struct BackerRecord {
    pub version: u8,
    pub backer: Pubkey,
    /// Matured money.
    pub amount: u64,
    pub pending_amount: u64,
    pub pending_matures_at: i64,
    /// Open withdrawal request (0 = none). Still counts toward capacity until completed.
    pub withdraw_amount: u64,
    pub withdraw_ready_at: i64,
    pub yield_index_at: u128,
    pub yield_owed: u64,
    pub bump: u8,
    /// Room for fields a later upgrade adds.
    pub reserved: [u8; 64],
}

/// The covered wallets of one staker. PDA `[SEED_STAKER_WALLETS, pool, staker]`.
#[account]
#[derive(InitSpace)]
pub struct StakerWallets {
    pub version: u8,
    pub staker: Pubkey,
    pub count: u8,
    pub wallet_hashes: [[u8; 32]; MAX_COVERED_WALLETS as usize],
    pub bump: u8,
    /// Room for fields a later upgrade adds (e.g. more covered wallets: 32 bytes each).
    pub reserved: [u8; 64],
}

/// One covered wallet, bound to one staker forever. PDA `[SEED_COVERED, pool, wallet_hash]`.
/// `wallet_hash = sha256(chain_id || normalized address)`, computed by the backend.
#[account]
#[derive(InitSpace)]
pub struct CoveredWallet {
    pub staker: Pubkey,
    pub wallet_hash: [u8; 32],
    pub registered_at: i64,
    pub bump: u8,
}

/// Claim lifecycle (multichain `ClaimStatus`). `Unused` is the zero value of a fresh account (an
/// override approval can create the account before the claim exists).
#[derive(
    AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq, InitSpace, Default,
)]
pub enum ClaimStatus {
    #[default]
    Unused,
    /// Queued: genuine but not admittable now (insolvent, over the stress cap, or oracle limit).
    Reserved,
    /// Admitted; waiting for the stake's time gate.
    PendingTime,
    /// Time gate met; the staker has the claim's `approve_window` to approve.
    AwaitingApproval,
    /// Approved: stake forfeited, cooling down or paying out.
    Active,
    Completed,
    Cancelled,
    Expired,
}

/// One claim. PDA `[SEED_CLAIM, pool, staker, tx_hash]`: the address is the claim id.
#[account]
#[derive(InitSpace)]
pub struct Claim {
    pub version: u8,
    pub staker: Pubkey,
    pub tx_hash: [u8; 32],
    pub hack_timestamp: i64,
    pub entitlement: u64,
    pub streamed: u64,
    /// Stake principal behind the claim (restored to `total_staked` by a cancel).
    pub stake: u64,
    pub cooldown_ends: i64,
    pub vesting_ends: i64,
    /// Capacity just before this claim's forfeiture: the floor of its daily payout base.
    pub capacity_snapshot: u64,
    pub tier: u8,
    pub status: ClaimStatus,
    pub approve_deadline: i64,
    pub last_collected: i64,
    pub bump: u8,
    /// Approve window this claim was admitted with (the setting at admission).
    pub approve_window: i64,
    /// Inactivity window this claim started paying with (the setting at approval).
    pub inactivity_window: i64,
    /// Paused total (`Pool::paused_secs_at`) when the claim's current window started: the hack
    /// window while queued, the approve window, then the collection window. Pause time after it
    /// does not count against the window (audit X1).
    pub pause_mark: i64,
    /// Room for fields a later upgrade adds.
    pub reserved: [u8; 64],
}

/// 2-of-2 override request (admin + co-signer). PDA `[SEED_OVERRIDE, claim]`. Deleted on execution.
#[account]
#[derive(InitSpace)]
pub struct OverrideRequest {
    pub claim: Pubkey,
    pub entitlement: u64,
    pub tier: u8,
    /// The approving keys, not flags: a rotated role's old approval no longer counts.
    pub admin_approver: Option<Pubkey>,
    pub co_signer_approver: Option<Pubkey>,
    pub bump: u8,
}

/// A revoked oracle approval. PDA `[SEED_REVOKED, pool, sha256(approval message)]`. Permanent.
#[account]
#[derive(InitSpace)]
pub struct RevokedApproval {
    pub hash: [u8; 32],
    pub bump: u8,
}
