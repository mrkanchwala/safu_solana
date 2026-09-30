//! Accounts. Seeds live in `constants.rs`; see `docs/PORT_MAP.md` for the multichain equivalent of each.

use anchor_lang::prelude::*;
use pool_core::params::MAX_COVERED_WALLETS;

/// The one pool. PDA `[SEED_POOL]`. Holds roles, Marinade addresses, totals, yield books and the
/// current day's counters (only today is ever read).
#[account]
#[derive(InitSpace)]
pub struct Pool {
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
    pub pool_cap: u64,
    /// Unix seconds; paused while `now < paused_until`.
    pub paused_until: i64,

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
}

impl Pool {
    pub fn is_paused(&self, now: i64) -> bool {
        now < self.paused_until
    }
}

/// One staker's stake. PDA `[SEED_STAKE, pool, staker]`. Closed on withdraw / emergency exit;
/// a forfeited record is never closed, so that address can never stake again.
#[account]
#[derive(InitSpace)]
pub struct StakeRecord {
    pub staker: Pubkey,
    /// Receives withdrawals, yield and claim payouts.
    pub beneficiary: Pubkey,
    /// Principal. Kept (not zeroed) when forfeited: the override path reads it.
    pub amount: u64,
    pub yield_index_at: u128,
    pub staked_at: i64,
    pub penalty_locked_until: i64,
    /// Set when a claim on this stake is approved. Cleared only by `cancel_claim` undoing it.
    pub forfeited: bool,
    /// Admin hold on payouts. Does not block principal withdrawal.
    pub suspended: bool,
    /// Claim account open on this stake (admitted, not yet terminal).
    pub active_claim: Option<Pubkey>,
    /// Claim account queued on this stake.
    pub reserved_claim: Option<Pubkey>,
    pub bump: u8,
}

/// One backer. PDA `[SEED_BACKER, pool, backer]`.
#[account]
#[derive(InitSpace)]
pub struct BackerRecord {
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
}

/// The covered wallets of one staker. PDA `[SEED_STAKER_WALLETS, pool, staker]`.
#[account]
#[derive(InitSpace)]
pub struct StakerWallets {
    pub staker: Pubkey,
    pub count: u8,
    pub wallet_hashes: [[u8; 32]; MAX_COVERED_WALLETS as usize],
    pub bump: u8,
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
