//! Key-loss-safe governance (multichain `governance.rs`).
//!
//! The pool must survive losing or leaking any key for its whole life.
//!
//! - THREE roles: admin, co-signer, guardian. Each is meant to be a Squads 2-of-3 vault, so a single
//!   lost or leaked key never reaches this program at all.
//! - Standard path: any role proposes, a SECOND role approves the same change, then it waits
//!   `GOV_DELAY_SECS` in public, then anyone executes. Cancelling also takes two roles, so a lone
//!   thief cannot keep cancelling honest changes. The role a change replaces never counts, cannot
//!   propose it and cannot cancel it, so a stolen role can neither block nor clog its own removal.
//! - Last-resort recovery: if two roles are gone, the one left can propose a role replacement alone.
//!   It waits `RECOVERY_DELAY_SECS`, and any other role still working can cancel it, including the
//!   one it replaces. So it only lands when nobody else is there to object.
//!
//! What cannot be recovered: all three roles lost, or one honest role against one thief, where each
//! cancels the other. Stakers can still withdraw and collect claims in both cases, and a pause
//! expires on its own, so no money is frozen forever.
//!
//! Changes handled here (everything that grants power or moves where money goes): the three roles,
//! the oracle, the yield venue, the treasury, the deploy share and the registry writer. Code upgrades
//! are not here: the program's upgrade authority is a Squads 2-of-3 with a 7-day timelock.
//! Only role replacements can use the recovery path.

use anchor_lang::prelude::*;
use pool_core::params::{GOV_DELAY_SECS, MAX_DEPLOY_BPS, RECOVERY_DELAY_SECS};

use super::now;
use crate::constants::{SEED_GOVERNANCE, SEED_POOL};
use crate::errors::PoolError;
use crate::events::{GovApproved, GovCancelVote, GovCancelled, GovExecuted, GovProposed};
use crate::kamino::check_venue;
use crate::state::{
    GovChange, GovKind, GovProposal, Governance, Pool, ROLES, ROLE_ADMIN, ROLE_CO_SIGNER,
    ROLE_GUARDIAN,
};

/// The setup kinds that apply with no wait while the pool has never held money.
const INSTANT_SETUP_KINDS: [GovKind; 3] =
    [GovKind::YieldVenue, GovKind::Treasury, GovKind::DeployBps];

/// No vote in a vote slot (`Pubkey::default()`: nothing can sign as it).
const NO_VOTE: Pubkey = Pubkey::new_from_array([0; 32]);

#[derive(Accounts)]
pub struct GovCall<'info> {
    pub caller: Signer<'info>,
    #[account(seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_GOVERNANCE, pool.key().as_ref()], bump)]
    pub governance: AccountLoader<'info, Governance>,
}

#[derive(Accounts)]
pub struct GovExecute<'info> {
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_GOVERNANCE, pool.key().as_ref()], bump)]
    pub governance: AccountLoader<'info, Governance>,
    /// The key being installed, when the change installs a role, oracle or registry writer: it signs
    /// its own installation, so a mistyped or unusable key can never take a role.
    pub new_key: Option<Signer<'info>>,
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn roles(pool: &Pool) -> [Pubkey; ROLES] {
    let mut r = [Pubkey::default(); ROLES];
    r[ROLE_ADMIN] = pool.admin;
    r[ROLE_CO_SIGNER] = pool.co_signer;
    r[ROLE_GUARDIAN] = pool.guardian;
    r
}

fn role_of(pool: &Pool, caller: &Pubkey) -> Result<usize> {
    roles(pool)
        .iter()
        .position(|k| k == caller)
        .ok_or_else(|| error!(PoolError::GovCallerNotRole))
}

/// The role a change replaces, if any. That role has no say in it.
fn target_of(kind: GovKind) -> Option<usize> {
    match kind {
        GovKind::Admin => Some(ROLE_ADMIN),
        GovKind::CoSigner => Some(ROLE_CO_SIGNER),
        GovKind::Guardian => Some(ROLE_GUARDIAN),
        _ => None,
    }
}

/// Votes that still count: from a role's CURRENT key, never from the role being replaced.
fn live_votes(pool: &Pool, kind: GovKind, votes: &[Pubkey; ROLES]) -> usize {
    let current = roles(pool);
    (0..ROLES)
        .filter(|&r| Some(r) != target_of(kind) && votes[r] != NO_VOTE && votes[r] == current[r])
        .count()
}

/// The stored change, rebuilt from its slot.
fn change_of(kind: GovKind, p: &GovProposal) -> GovChange {
    match kind {
        GovKind::Admin => GovChange::Admin(p.key),
        GovKind::CoSigner => GovChange::CoSigner(p.key),
        GovKind::Guardian => GovChange::Guardian(p.key),
        GovKind::Oracle => GovChange::Oracle(p.key),
        GovKind::YieldVenue => GovChange::YieldVenue(p.key),
        GovKind::Treasury => GovChange::Treasury(p.key),
        GovKind::DeployBps => GovChange::DeployBps(p.value),
        GovKind::RegistryWriter => GovChange::RegistryWriter(p.key),
    }
}

fn kind_from(index: u8) -> Result<GovKind> {
    GovKind::ALL
        .get(index as usize)
        .copied()
        .ok_or_else(|| error!(PoolError::GovNoPendingChange))
}

/// Admin, co-signer, guardian and oracle stay four different keys, so no single key ever holds two
/// votes. Also used by `initialize`.
pub fn check_roles_distinct(
    admin: &Pubkey,
    co: &Pubkey,
    guardian: &Pubkey,
    oracle: &Pubkey,
) -> Result<()> {
    let all = [admin, co, guardian, oracle];
    for i in 0..all.len() {
        for j in (i + 1)..all.len() {
            require!(all[i] != all[j], PoolError::GovRoleNotDistinct);
        }
    }
    Ok(())
}

/// Checked when proposed AND again when executed, against live state.
fn validate(pool: &Pool, change: &GovChange) -> Result<()> {
    let (mut admin, mut co, mut guardian, mut oracle) =
        (pool.admin, pool.co_signer, pool.guardian, pool.oracle);
    match *change {
        GovChange::Admin(k) => admin = k,
        GovChange::CoSigner(k) => co = k,
        GovChange::Guardian(k) => guardian = k,
        GovChange::Oracle(k) => oracle = k,
        GovChange::YieldVenue(_) => {
            require!(
                pool.deployed_book == 0 && pool.deployed_collateral == 0,
                PoolError::VenueChangeWhileDeployed
            );
            return Ok(());
        }
        GovChange::DeployBps(bps) => {
            require!(bps <= MAX_DEPLOY_BPS, PoolError::DeployBpsTooHigh);
            return Ok(());
        }
        GovChange::Treasury(_) | GovChange::RegistryWriter(_) => return Ok(()),
    }
    check_roles_distinct(&admin, &co, &guardian, &oracle)
}

/// Installs the change. A role, oracle or registry-writer key must sign its own installation.
fn apply(pool: &mut Pool, change: &GovChange, new_key: Option<Pubkey>) -> Result<()> {
    let needs_signature = !matches!(
        change,
        GovChange::YieldVenue(_) | GovChange::Treasury(_) | GovChange::DeployBps(_)
    );
    if needs_signature {
        require!(new_key == change.key(), PoolError::GovNewKeyMustSign);
    }
    match *change {
        GovChange::Admin(k) => pool.admin = k,
        GovChange::CoSigner(k) => pool.co_signer = k,
        GovChange::Guardian(k) => pool.guardian = k,
        GovChange::Oracle(k) => pool.oracle = k,
        GovChange::YieldVenue(k) => pool.yield_venue = k,
        GovChange::Treasury(k) => pool.treasury = k,
        GovChange::DeployBps(bps) => pool.deploy_bps = bps,
        GovChange::RegistryWriter(k) => pool.registry_writer = k,
    }
    Ok(())
}

fn new_proposal(
    change: &GovChange,
    proposer: Pubkey,
    role: usize,
    solo: bool,
    eta: i64,
) -> GovProposal {
    let mut p = GovProposal {
        key: change.key().unwrap_or_default(),
        proposer,
        value: match *change {
            GovChange::DeployBps(bps) => bps,
            _ => 0,
        },
        eta,
        active: 1,
        solo: solo as u8,
        ..GovProposal::default()
    };
    p.approvers[role] = proposer;
    p
}

/// When a change may execute. A setup change approved with no wait while the pool was empty gets
/// the full wait from the first deposit instead (multichain `end_instant_setup`), so money that
/// arrives meanwhile always sees it coming. Only ever later than the stored `eta`.
fn ready_at(pool: &Pool, kind: GovKind, p: &GovProposal) -> Result<i64> {
    if pool.funded_at != 0 && INSTANT_SETUP_KINDS.contains(&kind) {
        let from_funding = pool
            .funded_at
            .checked_add(GOV_DELAY_SECS)
            .ok_or(PoolError::MathOverflow)?;
        return Ok(p.eta.max(from_funding));
    }
    Ok(p.eta)
}

// ---------------------------------------------------------------------------------------------
// Standard path: propose -> second role approves -> GOV_DELAY_SECS -> anyone executes
// ---------------------------------------------------------------------------------------------

/// Any role proposes; its own approval is recorded. One pending change per kind: cancel it first to
/// propose a different one. A role never proposes its own replacement.
pub fn propose_change(ctx: Context<GovCall>, change: GovChange) -> Result<()> {
    let caller = ctx.accounts.caller.key();
    let pool = &ctx.accounts.pool;
    let gov = &mut ctx.accounts.governance.load_mut()?;
    let role = role_of(pool, &caller)?;
    let kind = change.kind();
    require!(
        target_of(kind) != Some(role),
        PoolError::GovTargetCannotApprove
    );
    require!(
        gov.pending[kind.index()].active == 0,
        PoolError::GovChangePending
    );
    validate(pool, &change)?;
    gov.pending[kind.index()] = new_proposal(&change, caller, role, false, 0);
    emit!(GovProposed {
        kind: kind as u8,
        change,
        proposer: caller,
        solo: false,
        eta: 0
    });
    Ok(())
}

/// A second role approves the SAME change. On the second live approval the public wait starts
/// (none for a setup change before the pool has ever held money).
pub fn approve_change(ctx: Context<GovCall>, change: GovChange) -> Result<()> {
    let caller = ctx.accounts.caller.key();
    let now = now()?;
    let pool = &ctx.accounts.pool;
    let gov = &mut ctx.accounts.governance.load_mut()?;
    let role = role_of(pool, &caller)?;
    let kind = change.kind();
    let p = &mut gov.pending[kind.index()];
    require!(p.active != 0, PoolError::GovNoPendingChange);
    require!(change_of(kind, p) == change, PoolError::GovChangeMismatch);
    require!(p.solo == 0, PoolError::GovSoloProposal);
    require!(
        target_of(kind) != Some(role),
        PoolError::GovTargetCannotApprove
    );
    require!(p.approvers[role] != caller, PoolError::GovAlreadyApproved);
    p.approvers[role] = caller;
    if p.eta == 0 && live_votes(pool, kind, &p.approvers) >= 2 {
        let instant = pool.funded_at == 0 && INSTANT_SETUP_KINDS.contains(&kind);
        // `max(1)`: 0 means "not approved yet".
        p.eta = if instant {
            now.max(1)
        } else {
            now.checked_add(GOV_DELAY_SECS)
                .ok_or(PoolError::MathOverflow)?
        };
    }
    emit!(GovApproved {
        kind: kind as u8,
        approver: caller,
        eta: p.eta
    });
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Last-resort recovery: one role, RECOVERY_DELAY_SECS, any other role can cancel
// ---------------------------------------------------------------------------------------------

pub fn propose_recovery(ctx: Context<GovCall>, change: GovChange) -> Result<()> {
    let caller = ctx.accounts.caller.key();
    let now = now()?;
    let pool = &ctx.accounts.pool;
    let gov = &mut ctx.accounts.governance.load_mut()?;
    let role = role_of(pool, &caller)?;
    let kind = change.kind();
    match target_of(kind) {
        Some(t) if t != role => {}
        _ => return err!(PoolError::GovRecoveryRoleOnly),
    }
    require!(
        gov.pending[kind.index()].active == 0,
        PoolError::GovChangePending
    );
    validate(pool, &change)?;
    let eta = now
        .checked_add(RECOVERY_DELAY_SECS)
        .ok_or(PoolError::MathOverflow)?;
    gov.pending[kind.index()] = new_proposal(&change, caller, role, true, eta);
    emit!(GovProposed {
        kind: kind as u8,
        change,
        proposer: caller,
        solo: true,
        eta
    });
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Cancel and execute
// ---------------------------------------------------------------------------------------------

/// Standard change: two roles must vote to cancel, never counting the role being replaced. The
/// first vote waits for a second. Recovery: any single role cancels at once, including the one it
/// would replace.
pub fn cancel_change(ctx: Context<GovCall>, kind: u8) -> Result<()> {
    let caller = ctx.accounts.caller.key();
    let kind = kind_from(kind)?;
    let pool = &ctx.accounts.pool;
    let gov = &mut ctx.accounts.governance.load_mut()?;
    let role = role_of(pool, &caller)?;
    let p = &mut gov.pending[kind.index()];
    require!(p.active != 0, PoolError::GovNoPendingChange);
    if p.solo == 0 {
        require!(target_of(kind) != Some(role), PoolError::GovCannotCancel);
        require!(p.cancellers[role] != caller, PoolError::GovAlreadyApproved);
        p.cancellers[role] = caller;
        if live_votes(pool, kind, &p.cancellers) < 2 {
            emit!(GovCancelVote {
                kind: kind as u8,
                by: caller
            });
            return Ok(());
        }
    }
    *p = GovProposal::default();
    emit!(GovCancelled {
        kind: kind as u8,
        by: caller
    });
    Ok(())
}

/// Anyone, once the wait has passed. Approvals and the change itself are re-checked against live
/// state. A yield-venue change passes the new reserve as the first remaining account.
pub fn execute_change(ctx: Context<GovExecute>, kind: u8) -> Result<()> {
    let kind = kind_from(kind)?;
    let now = now()?;
    let new_key = ctx.accounts.new_key.as_ref().map(|k| k.key());
    let pool = &mut ctx.accounts.pool;
    let gov = &mut ctx.accounts.governance.load_mut()?;
    let p = gov.pending[kind.index()];
    require!(p.active != 0, PoolError::GovNoPendingChange);
    require!(
        p.eta != 0 && now >= ready_at(pool, kind, &p)?,
        PoolError::GovNotReady
    );
    if p.solo != 0 {
        // The proposer must still hold its role.
        require!(role_of(pool, &p.proposer).is_ok(), PoolError::GovNotReady);
    } else {
        require!(
            live_votes(pool, kind, &p.approvers) >= 2,
            PoolError::GovNotReady
        );
    }
    let change = change_of(kind, &p);
    validate(pool, &change)?;
    if let GovChange::YieldVenue(k) = change {
        // A new venue must be a klend USDC reserve, passed as the first remaining account; the
        // default key turns the venue off.
        if k != Pubkey::default() {
            let reserve = ctx
                .remaining_accounts
                .first()
                .filter(|a| a.key() == k)
                .ok_or_else(|| error!(PoolError::YieldAccountsMissing))?;
            check_venue(pool, reserve)?;
        }
    }
    apply(pool, &change, new_key)?;
    gov.pending[kind.index()] = GovProposal::default();
    emit!(GovExecuted {
        kind: kind as u8,
        change
    });
    Ok(())
}
