//! Accounts. Seeds live in `constants.rs`; see `docs/PORT_MAP.md` for the multichain equivalent of each.

use anchor_lang::prelude::*;
use pool_core::params::{MAX_COVERED_WALLETS, MAX_PENDING_DEPOSITS};
use pool_core::points::{self, SIDES};
use pool_core::settings::{self, Settings, TierRatios, SETTING_SLOTS};

/// Layout version of every record below. An upgrade that changes a layout bumps it and adds the
/// step to `migrate` (the pool) or reads the record's own `version` (a per-user record); the spare
/// `reserved` bytes let most new fields land without resizing anyone's account.
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

/// What governance changes (multichain `GovKind`, same numbering; no `Upgrade`: code upgrades go
/// through the program's upgrade authority, a Squads 2-of-3 with a 7-day timelock). The value is
/// the slot in `Governance.pending`: append only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum GovKind {
    Admin = 0,
    CoSigner = 1,
    Guardian = 2,
    /// The oracle key (it signs both `submit_claim` and the Ed25519 approvals).
    Oracle = 3,
    /// The yield venue (multichain `Vault`).
    YieldVenue = 4,
    Treasury = 5,
    /// Share of capacity allowed in the yield venue, bps, at most `MAX_DEPLOY_BPS`.
    DeployBps = 6,
    RegistryWriter = 7,
}

/// Governance slots in `Governance.pending`.
pub const GOV_KINDS: usize = 8;

impl GovKind {
    pub const ALL: [GovKind; GOV_KINDS] = [
        GovKind::Admin,
        GovKind::CoSigner,
        GovKind::Guardian,
        GovKind::Oracle,
        GovKind::YieldVenue,
        GovKind::Treasury,
        GovKind::DeployBps,
        GovKind::RegistryWriter,
    ];
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// One governance change, as clients send it.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum GovChange {
    Admin(Pubkey),
    CoSigner(Pubkey),
    Guardian(Pubkey),
    Oracle(Pubkey),
    YieldVenue(Pubkey),
    Treasury(Pubkey),
    DeployBps(u64),
    RegistryWriter(Pubkey),
}

impl GovChange {
    pub fn kind(&self) -> GovKind {
        match self {
            GovChange::Admin(_) => GovKind::Admin,
            GovChange::CoSigner(_) => GovKind::CoSigner,
            GovChange::Guardian(_) => GovKind::Guardian,
            GovChange::Oracle(_) => GovKind::Oracle,
            GovChange::YieldVenue(_) => GovKind::YieldVenue,
            GovChange::Treasury(_) => GovKind::Treasury,
            GovChange::DeployBps(_) => GovKind::DeployBps,
            GovChange::RegistryWriter(_) => GovKind::RegistryWriter,
        }
    }
    /// The key a change installs, if it installs one.
    pub fn key(&self) -> Option<Pubkey> {
        match *self {
            GovChange::DeployBps(_) => None,
            GovChange::Admin(k)
            | GovChange::CoSigner(k)
            | GovChange::Guardian(k)
            | GovChange::Oracle(k)
            | GovChange::YieldVenue(k)
            | GovChange::Treasury(k)
            | GovChange::RegistryWriter(k) => Some(k),
        }
    }
}

/// The three governance roles; the index is the slot in a proposal's vote arrays.
pub const ROLE_ADMIN: usize = 0;
pub const ROLE_CO_SIGNER: usize = 1;
pub const ROLE_GUARDIAN: usize = 2;
pub const ROLES: usize = 3;

/// A governance change on its way (multichain `GovProposal`), in its kind's slot of
/// `Governance.pending`. Votes are stored as the voting key and count only while that key still
/// holds the role, so a rotation voids stale votes. `Pubkey::default()` = no vote (it is the System
/// Program's id: nothing can sign as it).
#[zero_copy]
#[derive(Default, Debug, PartialEq, Eq)]
pub struct GovProposal {
    /// The change: its key, or its number (`DeployBps`).
    pub key: Pubkey,
    pub proposer: Pubkey,
    /// By role: admin, co-signer, guardian.
    pub approvers: [Pubkey; ROLES],
    pub cancellers: [Pubkey; ROLES],
    pub value: u64,
    /// Earliest execution; 0 until two roles have approved (standard path).
    pub eta: i64,
    pub active: u8,
    /// Last-resort recovery: one role, `RECOVERY_DELAY_SECS`, any other role cancels at once.
    pub solo: u8,
    pub padding: [u8; 6],
}

/// The one pool. PDA `[SEED_POOL]`. Holds roles, the USDC mint and vault, settings, totals, yield
/// books and the current day's counters (only today is ever read). Also the vault's authority.
#[account]
#[derive(InitSpace)]
pub struct Pool {
    pub version: u8,
    /// What the pool holds and pays in: USDC (classic SPL Token, 6 decimals), fixed at `initialize`.
    pub usdc_mint: Pubkey,
    /// The pool's USDC token account, PDA `[SEED_VAULT, pool]`, authority = this pool.
    pub vault: Pubkey,
    pub admin: Pubkey,
    pub co_signer: Pubkey,
    /// Third governance role: breaks a tie between the other two (`governance.rs`).
    pub guardian: Pubkey,
    /// Signs `submit_claim` and the Ed25519 approval in front of it.
    pub oracle: Pubkey,
    /// Backend key that registers covered wallets after an off-chain ownership proof.
    pub registry_writer: Pubkey,
    /// Receives protocol revenue (`withdraw_yield`).
    pub treasury: Pubkey,
    /// Fixed at `initialize`, signed into every oracle approval.
    pub cluster: u8,
    pub bump: u8,
    pub vault_bump: u8,
    /// Every adjustable number, one slot per `pool_core::settings::SettingKey`. Read only through
    /// [`Pool::settings`]. Unused slots are room for settings an upgrade adds.
    pub settings: [i64; SETTING_SLOTS],
    pub pending_settings: [PendingSetting; SETTING_SLOTS],
    /// Time of the first deposit (0 = never funded), never cleared. Until then setup changes (yield
    /// venue, treasury, deploy share) need two roles but no wait; one approved before it gets the
    /// full wait from it.
    pub funded_at: i64,
    /// Live share of capacity allowed in the yield venue, bps. Starts at 0 (nothing deployed until
    /// governance turns it on), at most `MAX_DEPLOY_BPS`.
    pub deploy_bps: u64,
    /// The yield venue (unset until governance sets it).
    pub yield_venue: Pubkey,
    /// Unix seconds; paused while `now < paused_until`. After `unpause` it holds the moment the
    /// pause ended, so it is always the end of the last pause (0 = never paused).
    pub paused_until: i64,
    /// Start of the last pause (0 = never paused).
    pub pause_started_at: i64,
    /// Seconds paused in every pause before the last one. With the two above, the pause clock that
    /// stops claim windows during a pause.
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

    /// USDC cost of the yield position (book value, never marked to market).
    pub deployed_book: u64,
    pub last_harvest_at: i64,

    /// Day number (`unix / SECONDS_PER_DAY`) the counters below belong to.
    pub day: i64,
    pub day_admitted: u64,
    pub day_oracle_count: u64,
    pub day_outflow: u64,
    /// Backer money requested out and not yet paid (it still counts toward capacity until paid).
    pub total_backer_open: u64,
    /// The backer drip's day snapshot, taken at the day's first backer exit action: free capital,
    /// open requests, utilisation (bps). Every request's share that day comes from it.
    pub drip_snap_day: i64,
    pub drip_snap_free: u64,
    pub drip_snap_open: u64,
    pub drip_snap_util: u64,
    /// The claim payout day snapshot, taken at the day's first collection: capacity and what open
    /// claims are owed. Every claim's share of the day's cap comes from it.
    pub payout_snap_day: i64,
    pub payout_snap_capacity: u64,
    pub payout_snap_allocated: u64,
    /// The claim line: `QueueEntry` places `[queue_head, queue_tail)`. Only the place at the head
    /// can be released; a new claim joins at the tail while anyone waits.
    pub queue_head: u64,
    pub queue_tail: u64,
    /// The watch list: every claim, from submission, until it needs no more checks (`WatchEntry`).
    /// Upkeep checks the place at `watch_cursor` and the next; places close only at the head.
    pub watch_head: u64,
    pub watch_tail: u64,
    pub watch_cursor: u64,
    /// Points index per side (`pool_core::points::STAKER` / `BACKER`): stored value and when.
    pub points_index: [u128; SIDES],
    pub points_at: [i64; SIDES],
    /// Not pool cash: only a payment short by at most `MAX_ROUNDING_DRAW` reaches it. Funded by
    /// anyone, never withdrawn, never capacity.
    pub rounding_reserve: u64,
    /// Venue collateral the pool holds, on the books (a donation to the collateral account is not
    /// counted).
    pub deployed_collateral: u64,
    /// Days a claim payout found the pool short of cash (rule 18), and the last one counted.
    pub cash_short_days: u64,
    pub last_cash_short_day: i64,
    /// Circle's CctpForwarder on Stellar (contract id): payouts to a Stellar CCTP owner go through it
    /// (`pool_core::cctp::forward_hook`). Deploy config, fixed at `initialize`.
    pub stellar_forwarder: [u8; 32],
    /// Room for fields a later upgrade adds.
    pub reserved: [u8; 200], // hardcode-ok: spare layout bytes, not a rule
}

impl Pool {
    /// The pool's data is on this code's layout. Every instruction but `migrate` requires it, so
    /// code from an upgrade never runs on data an older version wrote.
    pub fn is_current(&self) -> bool {
        self.version == ACCOUNT_VERSION
    }

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

    /// Time the pool could not pay, as of `now` (rule 18): paused seconds plus cash-short days.
    pub fn blocked_secs(&self, now: i64) -> i64 {
        settings::blocked_secs(self.paused_secs_at(now), self.cash_short_days)
    }

    /// Claim clock for a window that started when the blocked total was `mark`: blocked time since
    /// then does not count.
    pub fn claim_clock(&self, now: i64, mark: i64) -> i64 {
        settings::claim_clock(now, self.blocked_secs(now), mark)
    }

    /// Counts the day of `now` as a cash-short day (once per day). Returns the total.
    pub fn mark_cash_short_day(&mut self, now: i64) -> u64 {
        let day = pool_core::claim::day_of(now);
        (self.last_cash_short_day, self.cash_short_days) =
            settings::mark_cash_short_day(self.last_cash_short_day, self.cash_short_days, day);
        self.cash_short_days
    }

    /// A points side's index at `now`, under its live rate setting.
    pub fn points_index(&self, side: usize, now: i64) -> pool_core::Result<u128> {
        let rate = self.settings().amount(points_rate_key(side));
        points::index(self.points_index[side], self.points_at[side], rate, now)
    }

    /// Stores the side's index as of now: before a rate change (so the old rate covers the time
    /// before it), and on every stake and backing (so the clock runs before anyone holds money).
    pub fn points_checkpoint(&mut self, side: usize, now: i64) -> pool_core::Result<()> {
        self.points_index[side] = self.points_index(side, now)?;
        self.points_at[side] = now.max(1);
        Ok(())
    }
}

/// The rate setting of a points side.
pub fn points_rate_key(side: usize) -> settings::SettingKey {
    if side == points::STAKER {
        settings::SettingKey::PointsRateStaker
    } else {
        settings::SettingKey::PointsRateBacker
    }
}

/// Pending governance changes, one slot per `GovKind`. PDA `[SEED_GOVERNANCE, pool]`, created by
/// `initialize`. Its own zero-copy account: read in place, so neither it nor the pool record ever
/// has to fit on the 4 KB stack.
#[account(zero_copy)]
#[derive(Debug)]
pub struct Governance {
    pub pending: [GovProposal; GOV_KINDS],
}

/// One staker's stake. PDA `[SEED_STAKE, pool, staker]`. Never closed: after a full withdraw it keeps
/// the unpaid yield and points (and can stake again, unless a claim was ever approved on it).
#[account]
#[derive(InitSpace)]
pub struct StakeRecord {
    pub version: u8,
    pub staker: Pubkey,
    /// Receives withdrawals, yield and claim payouts, into its own USDC token account.
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
    /// Admin hold on payouts, in force until `suspended_until` (`is_suspended`). Does not block
    /// principal withdrawal.
    pub suspended: bool,
    /// Settings fixed when this stake was made, part of the promise to this staker. Tier ratios:
    /// the higher of these and the live setting applies (a cut never reaches this stake, a raise
    /// does).
    pub tier_a_ratio: u64,
    pub tier_b_ratio: u64,
    pub tier_c_ratio: u64,
    pub time_gate_secs: i64,
    pub claim_window_secs: i64,
    /// Claim account open on this stake (admitted, not yet terminal).
    pub active_claim: Option<Pubkey>,
    /// Claim account queued on this stake.
    pub reserved_claim: Option<Pubkey>,
    /// Yield settled and not yet paid: kept by a claimant at approval, and whatever the set-aside
    /// could not cover at an exit. Paid on request (`claim_yield`).
    pub yield_owed: u64,
    /// Points banked so far, and the staker points index they run from. Burned when a claim is
    /// approved; kept through a withdrawal.
    pub points: u64,
    pub points_index_at: u128,
    /// A claim on this stake was ever approved: this address never stakes again, even if a cancel
    /// restored the stake and it was then withdrawn.
    pub claim_approved: bool,
    pub bump: u8,
    /// End of the last suspension (0 = never suspended). Claim windows never start before it.
    pub suspended_until: i64,
    /// Room for fields a later upgrade adds.
    pub reserved: [u8; 56],
}

impl StakeRecord {
    /// Payouts held: suspended, and the suspension has not lapsed.
    pub fn is_suspended(&self, now: i64) -> bool {
        self.suspended && now < self.suspended_until
    }

    /// This stake's tier ratios against the live ones: the higher of each.
    pub fn tier_ratios(&self, live: TierRatios) -> TierRatios {
        TierRatios {
            a: self.tier_a_ratio,
            b: self.tier_b_ratio,
            c: self.tier_c_ratio,
        }
        .at_least(live)
    }
}

/// One deposit waiting to count toward capacity (only while the maturity setting is above zero).
#[derive(
    AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, Debug, PartialEq, Eq, InitSpace,
)]
pub struct PendingDeposit {
    pub amount: u64,
    pub matures_at: i64,
}

/// One backer. PDA `[SEED_BACKER, pool, backer]`.
#[account]
#[derive(InitSpace)]
pub struct BackerRecord {
    pub version: u8,
    pub backer: Pubkey,
    /// Counted money: backs claims, can be requested out.
    pub amount: u64,
    /// Deposits still waiting to count, each with its own clock: `pending[..pending_count]`.
    pub pending: [PendingDeposit; MAX_PENDING_DEPOSITS],
    pub pending_count: u8,
    /// Open withdrawal (0 = none): requested, paid so far, and when the drip started. Unpaid money
    /// still counts toward capacity.
    pub withdraw_amount: u64,
    pub withdraw_paid: u64,
    pub withdraw_started_at: i64,
    /// This request's share of the day's free capital (`drip_snap_*`) and the day it was set.
    pub share_day: i64,
    pub share_left: u64,
    /// Points banked so far, and the backer points index they run from (counted money only).
    pub points: u64,
    pub points_index_at: u128,
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
    /// Registered wallet hashes in order; the first `count` are in use.
    pub wallet_hashes: [[u8; 32]; MAX_COVERED_WALLETS as usize],
    pub bump: u8,
    /// Room for fields a later upgrade adds.
    pub reserved: [u8; 64],
}

impl StakerWallets {
    /// The registered wallet hashes, in order.
    pub fn hashes(&self) -> &[[u8; 32]] {
        &self.wallet_hashes[..self.count as usize]
    }
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
    /// Blocked total (`Pool::blocked_secs`) when the claim's current window started: the approve
    /// window, then the collection window. Blocked time after it (pause, cash-short days) does not
    /// count against the window.
    pub blocked_mark: i64,
    /// Place in the claim line while queued (`QueueEntry` seed).
    pub queue_seq: u64,
    /// This claim's share of today's payout cap and the day it was set (`payout_snap_*`).
    pub share_day: i64,
    pub share_left: u64,
    /// Room for fields a later upgrade adds.
    pub reserved: [u8; 64],
}

/// One place in the claim line. PDA `[SEED_QUEUE, pool, seq (u64 LE)]`. Created when a claim
/// queues, closed (rent back to `payer`) when the head of the line moves past it.
#[account]
#[derive(InitSpace)]
pub struct QueueEntry {
    pub claim: Pubkey,
    /// Paid the rent (the oracle that submitted the claim).
    pub payer: Pubkey,
    pub bump: u8,
}

/// One place on the watch list. PDA `[SEED_WATCH, pool, seq (u64 LE)]`. Created with the claim
/// (submission or a new override), marked `done` once the claim needs no more checks, closed (rent
/// back to `payer`) when it is done at the head of the list.
#[account]
#[derive(InitSpace)]
pub struct WatchEntry {
    pub claim: Pubkey,
    pub payer: Pubkey,
    pub done: bool,
    pub bump: u8,
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

/// A CCTP owner's account: someone who staked or backed from Ethereum or Stellar with USDC over
/// Circle CCTP and holds no Solana key. PDA `[SEED_CCTP_ACCOUNT, identity]`.
///
/// `identity` (PDA `[SEED_CCTP_IDENTITY, pool, mode, home domain, owner]`, no data) is the staker or
/// backer the pool sees; only this program signs for it, after checking the owner's home-wallet
/// signature (`instructions::cctp`). Its USDC waits in `held` (PDA `[SEED_CCTP_HELD, identity]`)
/// and only ever leaves into the pool or home to the owner.
#[account]
#[derive(InitSpace)]
pub struct CctpAccount {
    pub version: u8,
    /// `pool_core::cctp::MODE_STAKE` or `MODE_BACK`: what deposits through this account are for.
    pub mode: u8,
    /// Circle domain of the owner's home chain.
    pub home_domain: u32,
    /// The pool's cluster tag, signed into every owner action.
    pub cluster: u8,
    /// The owner as a CCTP `bytes32` (Ethereum addresses left-padded).
    pub home_owner: [u8; 32],
    /// The next owner signature must carry this nonce; each one is used once.
    pub nonce: u64,
    /// Arrived by CCTP and not yet staked or backed: anyone may deploy it (`cctp_deploy`), as the
    /// owner's burn asked. Sending home clears it.
    pub to_deploy: u64,
    pub bump: u8,
    pub identity_bump: u8,
    /// Room for fields a later upgrade adds.
    pub reserved: [u8; 64],
}
