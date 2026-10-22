//! The yield leg (multichain `vault.rs`): push idle USDC into the venue, harvest growth, pull USDC
//! out for a payment, credit yield. Maths in `pool_core::leg` and `pool_core::kamino`; the calls in
//! `crate::kamino`.
//!
//! The pool rebalances itself inside user calls (multichain v1, 2026-09-24): money out pulls from the
//! venue when cash is short and refills the buffer; money in pushes idle cash to the `deploy_bps`
//! line. Book value (`deployed_book`) is never marked to market; growth is booked only when it comes
//! back, as yield.
//!
//! Solana cannot catch a failed call (multichain uses `try_` calls), so every best-effort step checks
//! in advance everything a klend call needs (`pool_core::kamino::Reserve::{deposit_ok, redeem_ok}`)
//! and skips if any is not met. A venue that cannot pay never fails the staker's call here: the
//! payment then refuses as short of cash, or (claim payouts) counts a cash-short day.

use anchor_lang::prelude::*;
use pool_core::leg as lg;
use pool_core::settings::SettingKey;
use pool_core::{add, liquidity, sub, yields};

use crate::errors::{CoreResultExt, PoolError};
use crate::events::{
    DeployBelowFloor, Deployed, DeploymentShortfall, Harvested, Redeemed, YieldCredited,
    LEG_ADMIN_PROVIDE, LEG_KEEPER_DEPLOY, LEG_KEEPER_REBALANCE, LEG_PULL, LEG_PUSH,
};
use crate::kamino::Leg;
use crate::state::Pool;
use crate::vault::{self, Cash};

pub fn credit_yield(pool: &mut Pool, amount: u64) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    let st = pool.settings();
    let c = yields::credit(
        amount,
        pool.total_staked,
        pool.total_backed,
        st.amount(SettingKey::StakerYieldBps),
        st.amount(SettingKey::BackerYieldBps),
    )
    .core()?;
    pool.staker_yield_index = pool
        .staker_yield_index
        .checked_add(c.staker_index_bump)
        .ok_or(PoolError::MathOverflow)?;
    pool.backer_yield_index = pool
        .backer_yield_index
        .checked_add(c.backer_index_bump)
        .ok_or(PoolError::MathOverflow)?;
    pool.staker_yield_reserved = add(pool.staker_yield_reserved, c.staker_share).core()?;
    pool.backer_yield_reserved = add(pool.backer_yield_reserved, c.backer_share).core()?;
    pool.protocol_yield_balance = add(pool.protocol_yield_balance, c.protocol_share).core()?;
    pool.total_extracted_yield = add(pool.total_extracted_yield, amount).core()?;
    emit!(YieldCredited {
        amount,
        staker_share: c.staker_share,
        backer_share: c.backer_share,
        protocol_share: c.protocol_share,
    });
    Ok(())
}

/// Growth `harvest` would book now from the reserve as last read, under the daily limit. 0 while
/// paused, with nothing deployed, or before the harvest clock starts.
pub fn harvestable(pool: &Pool, reserve: &pool_core::kamino::Reserve, now: i64) -> Result<u64> {
    if pool.is_paused(now)
        || pool.deployed_collateral == 0
        || pool.deployed_book == 0
        || pool.last_harvest_at == 0
    {
        return Ok(0);
    }
    let value = reserve.liquidity_for(pool.deployed_collateral).core()?;
    let elapsed = now.saturating_sub(pool.last_harvest_at);
    Ok(value
        .saturating_sub(pool.deployed_book)
        .min(yields::harvest_limit(pool.deployed_book, elapsed).core()?))
}

fn capacity(pool: &Pool) -> Result<u64> {
    pool_core::capacity(pool.total_staked, pool.total_backed).core()
}

/// Shared accounting for every deposit.
fn record_deposit(pool: &mut Pool, amount: u64, collateral: u64, now: i64, kind: u8) -> Result<()> {
    // CSO M2: the harvest growth limit's clock starts with the first money deployed.
    if pool.last_harvest_at == 0 {
        pool.last_harvest_at = now.max(1);
    }
    pool.deployed_collateral = add(pool.deployed_collateral, collateral).core()?;
    pool.deployed_book = add(pool.deployed_book, amount).core()?;
    emit!(Deployed {
        kind,
        amount,
        collateral
    });
    Ok(())
}

/// Shared accounting for every redeem except a harvest: book value out, growth that came back is
/// yield, a shortfall is a loss marked off `total_staked`.
fn record_redeem(pool: &mut Pool, collateral: u64, received: u64, kind: u8) -> Result<()> {
    let principal =
        lg::book_part(pool.deployed_book, collateral, pool.deployed_collateral).core()?;
    pool.deployed_collateral = sub(pool.deployed_collateral, collateral).core()?;
    pool.deployed_book = sub(pool.deployed_book, principal).core()?;
    let r = lg::settle_redeem(principal, received).core()?;
    credit_yield(pool, r.gain)?;
    if r.loss > 0 {
        pool.total_staked = pool.total_staked.saturating_sub(r.loss);
        emit!(DeploymentShortfall {
            principal_expected: principal,
            received
        });
    }
    emit!(Redeemed {
        kind,
        collateral,
        received
    });
    Ok(())
}

/// The money one instruction moves: the pool account, the vault, the cash accounts and the venue,
/// loaded once (`None` without one).
pub struct Money<'a, 'info> {
    pub pool_info: AccountInfo<'info>,
    pub vault: AccountInfo<'info>,
    pub cash: &'a Cash<'info>,
    pub leg: Option<Leg<'info>>,
}

impl<'a, 'info> Money<'a, 'info> {
    pub fn new(
        pool: &Pool,
        pool_info: AccountInfo<'info>,
        vault: AccountInfo<'info>,
        cash: &'a Cash<'info>,
    ) -> Result<Self> {
        let leg = cash.venue(pool, &pool_info.key())?;
        Ok(Money {
            pool_info,
            vault,
            cash,
            leg,
        })
    }

    fn leg(&mut self) -> Result<&mut Leg<'info>> {
        self.leg
            .as_mut()
            .ok_or_else(|| error!(PoolError::VenueNotSet))
    }

    /// Deposits `amount` (already checked to go through) and books what came back.
    fn deposit(&mut self, pool: &mut Pool, amount: u64, now: i64, kind: u8) -> Result<u64> {
        let (pool_info, vault, cash) = (self.pool_info.clone(), self.vault.clone(), self.cash);
        let leg = self.leg()?;
        let expected = leg.state.collateral_for(amount).core()?;
        let before = leg.collateral_held()?;
        leg.deposit(pool, &pool_info, cash, &vault, amount)?;
        let collateral = sub(leg.collateral_held()?, before).core()?;
        let min_collateral = lg::min_collateral(expected).core()?;
        if collateral < min_collateral {
            emit!(DeployBelowFloor {
                amount,
                collateral,
                min_collateral
            });
        }
        record_deposit(pool, amount, collateral, now, kind)?;
        Ok(collateral)
    }

    /// Redeems `collateral` (already checked to go through); returns the USDC that arrived.
    fn redeem(&mut self, pool: &Pool, collateral: u64) -> Result<u64> {
        let (pool_info, vault, cash) = (self.pool_info.clone(), self.vault.clone(), self.cash);
        let before = vault::liquid_balance(&vault)?;
        self.leg()?
            .redeem(pool, &pool_info, cash, &vault, collateral)?;
        sub(vault::liquid_balance(&vault)?, before).core()
    }

    /// After USDC comes in (stake, back, maturing backing): deposits idle cash above what live
    /// claims need, up to the `deploy_bps` line, once that is at least `AUTO_PUSH_MIN_BPS` of
    /// capacity. Best effort: skipped if paused, no venue, or the reserve would refuse. Returns what
    /// was deposited.
    pub fn push_idle(&mut self, pool: &mut Pool, now: i64) -> Result<u64> {
        if pool.is_paused(now) || self.leg.is_none() {
            return Ok(0);
        }
        let amount = liquidity::push_amount(
            vault::free_liquid(pool, &self.vault)?,
            pool.total_allocated,
            capacity(pool)?,
            pool.deployed_book,
            pool.deploy_bps,
        )
        .core()?;
        if amount == 0 {
            return Ok(0);
        }
        let leg = self.leg()?;
        leg.refresh()?;
        if !leg.state.deposit_ok(amount, leg.market_emergency).core()? {
            return Ok(0);
        }
        self.deposit(pool, amount, now, LEG_PUSH)?;
        Ok(amount)
    }

    /// Redeems enough collateral for `target` USDC (sized on book, rounded up, capped at the whole
    /// position) if the reserve can pay it now. True if it redeemed.
    fn try_pull(&mut self, pool: &mut Pool, target: u64, now: i64) -> Result<bool> {
        if target == 0 || pool.deployed_collateral == 0 || pool.deployed_book == 0 {
            return Ok(false);
        }
        let collateral =
            lg::collateral_for(target, pool.deployed_book, pool.deployed_collateral).core()?;
        let leg = self.leg()?;
        if !leg
            .state
            .redeem_ok(collateral, leg.market_emergency, now)
            .core()?
        {
            return Ok(false);
        }
        let received = self.redeem(pool, collateral)?;
        record_redeem(pool, collateral, received, LEG_PULL)?;
        Ok(true)
    }

    /// Before money goes out (multichain `pull_cash`): if free cash is short of `amount`, redeems the
    /// shortfall plus a refill up to the buffer (`capacity × (1 − deploy_bps)`); if the reserve
    /// cannot pay that much, the shortfall alone. Never fails on the venue: the payment that follows
    /// then refuses as short of cash. Works while paused (emergency exit).
    pub fn pull_cash(&mut self, pool: &mut Pool, amount: u64, now: i64) -> Result<()> {
        let free = vault::free_liquid(pool, &self.vault)?;
        if free >= amount || self.leg.is_none() || pool.deployed_collateral == 0 {
            return Ok(());
        }
        let shortfall = amount - free;
        let buffer = liquidity::buffer(capacity(pool)?, pool.deploy_bps).core()?;
        self.leg()?.refresh()?;
        if !self.try_pull(pool, add(shortfall, buffer).core()?, now)? {
            self.try_pull(pool, shortfall, now)?;
        }
        Ok(())
    }

    /// Best effort (multichain `harvest`): redeems only the growth above book value, up to the daily
    /// growth limit, and credits it as yield. Book value never changes here. Skipped while paused or
    /// if the reserve cannot pay it now. Returns the yield credited.
    pub fn harvest(&mut self, pool: &mut Pool, now: i64) -> Result<u64> {
        if self.leg.is_none()
            || pool.is_paused(now)
            || pool.deployed_collateral == 0
            || pool.deployed_book == 0
        {
            return Ok(0);
        }
        // CSO M2: the growth limit's clock starts at the first attempt and moves only when a harvest
        // goes through, so frequent callers cannot starve it.
        if pool.last_harvest_at == 0 {
            pool.last_harvest_at = now.max(1);
            return Ok(0);
        }
        let leg = self.leg()?;
        // Nothing to take at the last-read rate: skip the refresh too.
        if harvestable(pool, &leg.state, now)? == 0 {
            return Ok(0);
        }
        leg.refresh()?;
        let value = leg.state.liquidity_for(pool.deployed_collateral).core()?;
        let collateral = lg::harvest_collateral(
            pool.deployed_collateral,
            pool.deployed_book,
            value,
            now.saturating_sub(pool.last_harvest_at),
        )
        .core()?;
        if !leg
            .state
            .redeem_ok(collateral, leg.market_emergency, now)
            .core()?
        {
            return Ok(0);
        }
        let received = self.redeem(pool, collateral)?;
        pool.deployed_collateral = sub(pool.deployed_collateral, collateral).core()?;
        pool.last_harvest_at = now;
        credit_yield(pool, received)?;
        emit!(Harvested {
            collateral,
            received
        });
        Ok(received)
    }

    // ---------------------------------------------------------------- keeper and admin calls

    /// Permissionless (multichain `auto_deploy_liquidity`): deposits idle cash up to the line, with
    /// no minimum size. Refuses while paused, or if the reserve would refuse.
    pub fn keeper_deploy(&mut self, pool: &mut Pool, now: i64) -> Result<u64> {
        require!(!pool.is_paused(now), PoolError::Paused);
        let idle = vault::free_liquid(pool, &self.vault)?.saturating_sub(pool.total_allocated);
        let room = pool_core::apply_bps(capacity(pool)?, pool.deploy_bps)
            .core()?
            .saturating_sub(pool.deployed_book);
        let amount = idle.min(room);
        let leg = self.leg()?;
        if amount == 0 {
            return Ok(0);
        }
        leg.refresh()?;
        require!(
            leg.state.deposit_ok(amount, leg.market_emergency).core()?,
            PoolError::VenueUnavailable
        );
        let expected = leg.state.collateral_for(amount).core()?;
        let collateral = self.deposit(pool, amount, now, LEG_KEEPER_DEPLOY)?;
        require!(
            collateral >= lg::min_collateral(expected).core()?,
            PoolError::MinAmountNotMet
        );
        Ok(collateral)
    }

    /// Permissionless (multichain `ensure_liquidity`): redeems the larger of open claims not covered
    /// by free cash and deployment above the `deploy_bps` line. Refuses if the reserve cannot pay it.
    pub fn keeper_rebalance(&mut self, pool: &mut Pool, now: i64) -> Result<u64> {
        let shortfall = liquidity::rebalance_shortfall(
            vault::free_liquid(pool, &self.vault)?,
            pool.total_allocated,
            capacity(pool)?,
            pool.deployed_book,
            pool.deploy_bps,
        )
        .core()?;
        if shortfall == 0 {
            return Ok(0);
        }
        require!(
            pool.deployed_collateral > 0 && pool.deployed_book > 0,
            PoolError::NothingDeployed
        );
        let collateral =
            pool_core::mul_div_floor(shortfall, pool.deployed_collateral, pool.deployed_book)
                .core()?
                .min(pool.deployed_collateral);
        self.redeem_checked(pool, collateral, None, now, LEG_KEEPER_REBALANCE)
    }

    /// Admin (multichain `provide_liquidity`): redeems `collateral`, at least `min_out` back. Works
    /// while paused.
    pub fn admin_provide(
        &mut self,
        pool: &mut Pool,
        collateral: u64,
        min_out: u64,
        now: i64,
    ) -> Result<u64> {
        require!(collateral > 0, PoolError::AmountNotPositive);
        require!(pool.deployed_collateral > 0, PoolError::NothingDeployed);
        require!(
            collateral <= pool.deployed_collateral,
            PoolError::RedeemExceedsDeployed
        );
        self.redeem_checked(pool, collateral, Some(min_out), now, LEG_ADMIN_PROVIDE)
    }

    /// A keeper or admin redeem: refresh, refuse if the reserve cannot pay, enforce the minimum
    /// (`min_out`, or `MAX_REBALANCE_SLIPPAGE_BPS` below the expected amount), book it.
    fn redeem_checked(
        &mut self,
        pool: &mut Pool,
        collateral: u64,
        min_out: Option<u64>,
        now: i64,
        kind: u8,
    ) -> Result<u64> {
        let leg = self.leg()?;
        leg.refresh()?;
        require!(
            leg.state
                .redeem_ok(collateral, leg.market_emergency, now)
                .core()?,
            PoolError::VenueUnavailable
        );
        let min = match min_out {
            Some(m) => m,
            None => lg::min_collateral(leg.state.liquidity_for(collateral).core()?).core()?,
        };
        let received = self.redeem(pool, collateral)?;
        require!(received >= min, PoolError::MinAmountNotMet);
        record_redeem(pool, collateral, received, kind)?;
        Ok(received)
    }

    // ---------------------------------------------------------------- payments

    /// Pulls if short, then pays `amount` to `to` (`vault::pay_out`: rounding-reserve draw, typed
    /// refusal if still short). Call after the books are updated: on any failure the whole
    /// transaction reverts.
    pub fn pay(
        &mut self,
        pool: &mut Pool,
        to: &AccountInfo<'info>,
        amount: u64,
        now: i64,
    ) -> Result<()> {
        self.pull_cash(pool, amount, now)?;
        vault::pay_out(pool, &self.pool_info, self.cash, &self.vault, to, amount)
    }

    /// `pay` for protocol revenue: never from the rounding reserve.
    pub fn pay_strict(
        &mut self,
        pool: &mut Pool,
        to: &AccountInfo<'info>,
        amount: u64,
        now: i64,
    ) -> Result<()> {
        self.pull_cash(pool, amount, now)?;
        vault::pay_out_strict(pool, &self.pool_info, self.cash, &self.vault, to, amount)
    }
}
