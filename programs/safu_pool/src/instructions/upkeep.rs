//! Protocol revenue (`withdraw_yield`), the rounding reserve, and the upkeep every user transaction
//! does (multichain `with_upkeep`): no keeper, nothing waits on a separate call.

use anchor_lang::prelude::*;
use anchor_spl::token::TokenAccount;
use pool_core::params::UPKEEP_WATCH_PER_CALL;
use pool_core::{add, sub, yields};

use super::claim::{
    expire_stale, expire_unapproved, queued_claim_dead, release_head, roll_day, skip_head,
};
use super::now;
use crate::constants::{SEED_POOL, SEED_QUEUE, SEED_STAKE, SEED_VAULT, SEED_WATCH};
use crate::errors::{CoreResultExt, PoolError};
use crate::events::YieldWithdrawn;
use crate::leg::Money;
// Glob: `#[derive(Accounts)]` on a struct holding `Cash` needs its generated client modules.
use crate::state::{Claim, ClaimStatus, Pool, QueueEntry, StakeRecord, WatchEntry};
use crate::vault::{self, *};

#[derive(Accounts)]
pub struct WithdrawYield<'info> {
    pub admin: Signer<'info>,
    #[account(
        mut,
        seeds = [SEED_POOL],
        bump = pool.bump,
        has_one = admin @ PoolError::NotAdmin,
    )]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    /// The treasury's USDC account (any token account `pool.treasury` owns).
    #[account(
        mut,
        constraint = treasury_usdc.mint == pool.usdc_mint @ PoolError::WrongMint,
        constraint = treasury_usdc.owner == pool.treasury @ PoolError::WrongTokenAccount,
    )]
    pub treasury_usdc: Box<Account<'info, TokenAccount>>,
    pub cash: Cash<'info>,
}

/// Protocol revenue to the treasury. Never from the rounding reserve. Within the protocol's balance AND the pool's surplus over
/// everything it owes (stakes, backing, set-aside yield, open claims), because claims may have spent
/// protocol revenue that the balance still counts (as multichain). Not while paused
/// for the same reason: stakers and backers cannot claim yield then.
pub fn withdraw_yield(ctx: Context<WithdrawYield>, amount: u64) -> Result<()> {
    let now = super::now()?;
    let vault = ctx.accounts.vault.to_account_info();
    let mut money = Money::new(
        &ctx.accounts.pool,
        ctx.accounts.pool.to_account_info(),
        vault.clone(),
        &ctx.accounts.cash,
    )?;
    let pool = &mut ctx.accounts.pool;
    require!(!pool.is_paused(now), PoolError::Paused);
    require!(amount > 0, PoolError::AmountNotPositive);
    require!(
        amount <= pool.protocol_yield_balance,
        PoolError::ExceedsYieldBalance
    );
    let held = add(vault::free_liquid(pool, &vault)?, pool.deployed_book).core()?;
    let owed = [
        pool.total_backed,
        pool.total_backed_pending,
        pool.staker_yield_reserved,
        pool.backer_yield_reserved,
        pool.total_allocated,
    ]
    .into_iter()
    .try_fold(pool.total_staked, add)
    .core()?;
    require!(
        amount <= yields::protocol_surplus(held, owed),
        PoolError::ExceedsYieldBalance
    );

    pool.protocol_yield_balance = sub(pool.protocol_yield_balance, amount).core()?;
    emit!(YieldWithdrawn {
        treasury: pool.treasury,
        amount
    });
    let to = ctx.accounts.treasury_usdc.to_account_info();
    money.pay_strict(pool, &to, amount, now)
}

#[derive(Accounts)]
pub struct FundRoundingReserve<'info> {
    pub funder: Signer<'info>,
    #[account(mut, seeds = [SEED_POOL], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, seeds = [SEED_VAULT, pool.key().as_ref()], bump = pool.vault_bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = funder_usdc.mint == pool.usdc_mint @ PoolError::WrongMint,
        constraint = funder_usdc.owner == funder.key() @ PoolError::WrongTokenAccount,
    )]
    pub funder_usdc: Box<Account<'info, TokenAccount>>,
    pub cash: Cash<'info>,
}

/// Anyone tops up the rounding reserve (multichain `fund_rounding_reserve`). Never withdrawn, never
/// capacity, earns nothing; only a payment short by at most `MAX_ROUNDING_DRAW` reaches it.
pub fn fund_rounding_reserve(ctx: Context<FundRoundingReserve>, amount: u64) -> Result<()> {
    require!(amount > 0, PoolError::AmountNotPositive);
    let from = ctx.accounts.funder_usdc.to_account_info();
    let owner = ctx.accounts.funder.to_account_info();
    let vault = ctx.accounts.vault.to_account_info();
    let pool = &mut ctx.accounts.pool;
    pool.rounding_reserve = add(pool.rounding_reserve, amount).core()?;
    emit!(crate::events::RoundingReserveFunded {
        from: ctx.accounts.funder.key(),
        amount
    });
    vault::receive(pool, &ctx.accounts.cash, &from, &owner, &vault, amount)
}

// ------------------------------------------------------------------ in-transaction upkeep

/// Accounts per place, in order: the place (`QueueEntry` / `WatchEntry`), its claim, the claim's
/// stake record, and the place's rent payer. All writable.
pub const PLACE_ACCOUNTS: usize = 4;

/// Where upkeep looks, read before the instruction runs: the head of the claim line, then the watch
/// places (`pool_core::upkeep::watch_places`). Clients pass `PLACE_ACCOUNTS` remaining accounts for
/// each, in that order, read from the pool as it is before their transaction.
#[derive(Clone, Copy)]
struct Snap {
    queue: Option<u64>,
    watch: [Option<u64>; UPKEEP_WATCH_PER_CALL],
}

impl Snap {
    fn of(pool: &Pool) -> Snap {
        Snap {
            queue: (pool.queue_head < pool.queue_tail).then_some(pool.queue_head),
            watch: pool_core::upkeep::watch_places(
                pool.watch_head,
                pool.watch_tail,
                pool.watch_cursor,
            ),
        }
    }

    fn accounts_needed(&self) -> usize {
        PLACE_ACCOUNTS * (self.queue.is_some() as usize + self.watch.iter().flatten().count())
    }
}

/// Runs the instruction, then the upkeep: releases the first claim in line if there is room now (or
/// moves the line past it if it can never go), then checks the watch places, expiring a claim whose
/// window ran out. A step that cannot run is skipped and never fails the caller; only missing or
/// wrong upkeep accounts do. Skipped while paused (nothing moves or expires then). A place whose
/// accounts the instruction itself holds is left for the next transaction.
pub(crate) fn with_upkeep<'a, 'info, T: anchor_lang::Bumps + ToAccountInfos<'info>>(
    ctx: &mut Context<'a, T>,
    pool_of: impl Fn(&mut T) -> &mut Account<'info, Pool>,
    body: impl FnOnce(&mut Context<'a, T>) -> Result<()>,
) -> Result<()> {
    let held: Vec<Pubkey> = ctx
        .accounts
        .to_account_infos()
        .iter()
        .map(|a| a.key())
        .collect();
    let snap = Snap::of(pool_of(ctx.accounts));
    body(ctx)?;
    let remaining = ctx.remaining_accounts;
    run(pool_of(ctx.accounts), snap, remaining, &held, now()?)
}

fn run(
    pool: &mut Pool,
    snap: Snap,
    accounts: &[AccountInfo],
    held: &[Pubkey],
    now: i64,
) -> Result<()> {
    if pool.is_paused(now) {
        return Ok(());
    }
    require!(
        accounts.len() >= snap.accounts_needed(),
        PoolError::UpkeepAccountsMissing
    );
    let pool_key = Pubkey::create_program_address(&[SEED_POOL, &[pool.bump]], &crate::ID)
        .map_err(|_| error!(PoolError::MathOverflow))?;
    let mut places = accounts.chunks_exact(PLACE_ACCOUNTS);
    // Counters first, so the steps below write only once they know they can finish.
    roll_day(pool, now);
    if let Some(head) = snap.queue {
        let place = places.next().ok_or(PoolError::UpkeepAccountsMissing)?;
        // The instruction itself may have released or skipped this place already.
        if pool.queue_head == head {
            queue_step(pool, &pool_key, head, place, held, now)?;
        }
    }
    for pos in snap.watch.into_iter().flatten() {
        let place = places.next().ok_or(PoolError::UpkeepAccountsMissing)?;
        watch_step(pool, &pool_key, pos, place, held, now)?;
    }
    Ok(())
}

fn missing() -> Error {
    error!(PoolError::UpkeepAccountsMissing)
}

/// A program account of type `T`, writable, read from its current data.
fn load<T: AccountDeserialize + Owner>(info: &AccountInfo) -> Result<Box<T>> {
    require!(
        info.owner == &T::owner() && info.is_writable,
        PoolError::UpkeepAccountsMissing
    );
    let data = info.try_borrow_data()?;
    T::try_deserialize(&mut &data[..])
        .map(Box::new)
        .map_err(|_| missing())
}

fn store<T: AccountSerialize>(info: &AccountInfo, value: &T) -> Result<()> {
    let mut data = info.try_borrow_mut_data()?;
    let mut out: &mut [u8] = &mut data;
    value.try_serialize(&mut out)
}

fn require_pda(info: &AccountInfo, seeds: &[&[u8]]) -> Result<()> {
    let address = Pubkey::create_program_address(seeds, &crate::ID).map_err(|_| missing())?;
    require_keys_eq!(info.key(), address, PoolError::UpkeepAccountsMissing);
    Ok(())
}

/// Closes a place, its rent back to whoever paid it.
fn close_place(place: &AccountInfo, to: &AccountInfo) -> Result<()> {
    let rent = place.lamports();
    let to_now = to.lamports();
    **to.try_borrow_mut_lamports()? = to_now.checked_add(rent).ok_or(PoolError::MathOverflow)?;
    **place.try_borrow_mut_lamports()? = 0;
    place.assign(&anchor_lang::system_program::ID);
    place.resize(0)?;
    Ok(())
}

/// A place's claim and stake record, checked against the place.
struct Held<'a, 'info> {
    claim_info: &'a AccountInfo<'info>,
    stake_info: &'a AccountInfo<'info>,
    claim: Box<Claim>,
    stake: Box<StakeRecord>,
}

fn load_claim<'a, 'info>(
    pool_key: &Pubkey,
    place: &'a [AccountInfo<'info>],
    claim_key: Pubkey,
    payer: Pubkey,
) -> Result<Held<'a, 'info>> {
    let [_, claim_info, stake_info, rent_to] = place else {
        return Err(missing());
    };
    require_keys_eq!(
        claim_info.key(),
        claim_key,
        PoolError::UpkeepAccountsMissing
    );
    require!(
        rent_to.key() == payer && rent_to.is_writable,
        PoolError::UpkeepAccountsMissing
    );
    let claim = load::<Claim>(claim_info)?;
    let stake = load::<StakeRecord>(stake_info)?;
    require_pda(
        stake_info,
        &[
            SEED_STAKE,
            pool_key.as_ref(),
            claim.staker.as_ref(),
            &[stake.bump],
        ],
    )?;
    Ok(Held {
        claim_info,
        stake_info,
        claim,
        stake,
    })
}

fn is_held(held: &[Pubkey], infos: &[&AccountInfo]) -> bool {
    infos.iter().any(|i| held.contains(i.key))
}

/// The head of the line: released if it fits now, skipped if it can never go, else left.
fn queue_step<'info>(
    pool: &mut Pool,
    pool_key: &Pubkey,
    head: u64,
    place: &[AccountInfo<'info>],
    held: &[Pubkey],
    now: i64,
) -> Result<()> {
    let entry_info = &place[0];
    let entry = load::<QueueEntry>(entry_info)?;
    require_pda(
        entry_info,
        &[
            SEED_QUEUE,
            pool_key.as_ref(),
            &head.to_le_bytes(),
            &[entry.bump],
        ],
    )?;
    let Held {
        claim_info,
        stake_info,
        mut claim,
        mut stake,
    } = load_claim(pool_key, place, entry.claim, entry.payer)?;
    if is_held(held, &[entry_info, claim_info, stake_info]) {
        return Ok(());
    }
    let key = claim_info.key();
    // Work on a copy: nothing is written unless the step goes through.
    let mut p = Box::new(pool.clone());
    let done = if queued_claim_dead(&claim, &key, Some(&stake)) {
        skip_head(&mut p, &mut claim, key, Some(&mut stake))
    } else {
        release_head(&mut p, &mut claim, key, &mut stake, now)
    };
    if done.is_err() {
        return Ok(());
    }
    *pool = *p;
    store(claim_info, &*claim)?;
    store(stake_info, &*stake)?;
    close_place(entry_info, &place[3])
}

/// One watched claim: expired if its window ran out. A claim that needs no more checks marks its
/// place done; a done place at the head of the list closes and the head moves on.
fn watch_step<'info>(
    pool: &mut Pool,
    pool_key: &Pubkey,
    pos: u64,
    place: &[AccountInfo<'info>],
    held: &[Pubkey],
    now: i64,
) -> Result<()> {
    let entry_info = &place[0];
    let mut entry = load::<WatchEntry>(entry_info)?;
    require_pda(
        entry_info,
        &[
            SEED_WATCH,
            pool_key.as_ref(),
            &pos.to_le_bytes(),
            &[entry.bump],
        ],
    )?;
    let Held {
        claim_info,
        stake_info,
        mut claim,
        mut stake,
    } = load_claim(pool_key, place, entry.claim, entry.payer)?;
    pool.watch_cursor = pos + 1;
    if is_held(held, &[entry_info, claim_info, stake_info]) {
        return Ok(());
    }
    let key = claim_info.key();
    let mut p = Box::new(pool.clone());
    let done = entry.done
        || match claim.status {
            // Not yet approvable, or its window still open: keep watching.
            ClaimStatus::PendingTime | ClaimStatus::AwaitingApproval => {
                expire_unapproved(&mut p, &mut claim, key, &mut stake, now).is_ok()
            }
            ClaimStatus::Active => {
                claim.streamed >= claim.entitlement
                    || expire_stale(&mut p, &mut claim, key, &mut stake, now).is_ok()
            }
            // In the line: the line releases or clears it.
            ClaimStatus::Reserved => false,
            // Finished.
            _ => true,
        };
    if !done {
        return Ok(());
    }
    if claim.status == ClaimStatus::Expired && !entry.done {
        *pool = *p;
        store(claim_info, &*claim)?;
        store(stake_info, &*stake)?;
    }
    if pos == pool.watch_head {
        close_place(entry_info, &place[3])?;
        pool.watch_head += 1;
    } else if !entry.done {
        entry.done = true;
        store(entry_info, &*entry)?;
    }
    Ok(())
}
