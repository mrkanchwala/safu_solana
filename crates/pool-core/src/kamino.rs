//! Facts about Kamino Lend (klend) in one place: account layouts, instruction discriminators, and the
//! reserve arithmetic the pool mirrors before every call.
//!
//! Source: the klend IDL shipped in `@kamino-finance/klend-sdk` 9.1.6 (`src/idl/klend.json`), offsets
//! computed field by field from it, read 2026-10-22. Checked the same day against the live mainnet USDC
//! reserve `D6q6wuQSrifJKZYpR1M8R4YawnLDtDsMmWM1NbBmgJ59`: discriminator, market, mints and vaults all
//! decode at these offsets. klend is upgradeable: a layout change would move them, so the program checks
//! owner, discriminator and length before reading, and fails closed.
//!
//! Solana cannot catch a failed call, so the pool checks here, in advance, everything a klend call
//! needs; a check that is wrong in the safe direction only skips a call that would have worked.

use crate::{to_u64, CoreError, Result};

/// Lending-market authority PDA seed: `[LENDING_MARKET_AUTH_SEED, market]` under klend.
pub const LENDING_MARKET_AUTH_SEED: &[u8] = b"lma";

/// Anchor discriminators: first 8 bytes of sha256("global:<name>") / sha256("account:<Name>").
pub const IX_REFRESH_RESERVE: [u8; 8] = [2, 218, 138, 235, 79, 201, 25, 102];
pub const IX_DEPOSIT_RESERVE_LIQUIDITY: [u8; 8] = [169, 201, 30, 126, 6, 205, 102, 68];
pub const IX_REDEEM_RESERVE_COLLATERAL: [u8; 8] = [234, 117, 181, 125, 185, 142, 220, 29];
pub const ACCOUNT_RESERVE: [u8; 8] = [43, 242, 204, 202, 26, 247, 59, 127];
pub const ACCOUNT_LENDING_MARKET: [u8; 8] = [246, 114, 50, 98, 72, 157, 28, 120];

/// `Reserve` (zero-copy, 8-byte discriminator first): byte offset of each field used.
pub const RESERVE_LEN: usize = 8624;
pub const R_LENDING_MARKET: usize = 32;
pub const R_LIQ_MINT: usize = 128;
pub const R_LIQ_SUPPLY_VAULT: usize = 160;
pub const R_LIQ_AVAILABLE: usize = 224;
pub const R_LIQ_BORROWED_SF: usize = 232;
pub const R_LIQ_PROTOCOL_FEES_SF: usize = 344;
pub const R_LIQ_REFERRER_FEES_SF: usize = 360;
pub const R_LIQ_PENDING_REFERRER_FEES_SF: usize = 376;
pub const R_LIQ_TOKEN_PROGRAM: usize = 408;
pub const R_COLL_MINT: usize = 2560;
pub const R_COLL_SUPPLY: usize = 2592;
pub const R_STATUS: usize = 4856;
pub const R_EMERGENCY: usize = 4864;
pub const R_DEPOSIT_LIMIT: usize = 5016;
pub const R_SCOPE_FEED: usize = 5112;
pub const R_SWITCHBOARD_PRICE: usize = 5160;
pub const R_SWITCHBOARD_TWAP: usize = 5192;
pub const R_PYTH: usize = 5224;
pub const R_CAP_CAPACITY: usize = 5416;
pub const R_CAP_CURRENT: usize = 5424;
pub const R_CAP_START: usize = 5432;
pub const R_CAP_INTERVAL: usize = 5440;
pub const R_QUEUED_COLLATERAL: usize = 6968;

/// `LendingMarket`: length and the one flag read.
pub const MARKET_MIN_LEN: usize = 123;
pub const M_EMERGENCY: usize = 122;

/// klend's placeholder for "no account" in a reserve's price config (`nu11111…`); all zeros means the
/// same.
pub const NULL_KEY: [u8; 32] = [
    11, 193, 238, 216, 208, 116, 241, 195, 55, 212, 76, 22, 75, 202, 40, 216, 76, 206, 27, 169,
    138, 64, 177, 28, 19, 90, 156, 0, 0, 0, 0, 0,
];

/// Where a reserve's price comes from. The pool passes klend one price account, so it supports a
/// reserve priced by Scope alone (the mainnet markets) or Pyth alone (the devnet markets); any other
/// setup is `Other` and refused as a venue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceSource {
    Scope([u8; 32]),
    Pyth([u8; 32]),
    Other,
}

impl PriceSource {
    /// The price account klend reads, `None` for `Other`.
    pub fn account(&self) -> Option<[u8; 32]> {
        match self {
            PriceSource::Scope(k) | PriceSource::Pyth(k) => Some(*k),
            PriceSource::Other => None,
        }
    }
}

fn price_source(d: &[u8]) -> PriceSource {
    let set = |at| {
        let k = key_at(d, at);
        (k != [0; 32] && k != NULL_KEY).then_some(k)
    };
    let switchboard = set(R_SWITCHBOARD_PRICE).or(set(R_SWITCHBOARD_TWAP));
    match (set(R_SCOPE_FEED), set(R_PYTH), switchboard) {
        (Some(k), None, None) => PriceSource::Scope(k),
        (None, Some(k), None) => PriceSource::Pyth(k),
        _ => PriceSource::Other,
    }
}

/// klend `ReserveStatus::Active`.
pub const STATUS_ACTIVE: u8 = 0;

/// klend scaled fractions (`*_sf`) carry 60 fractional bits.
pub const SF_SHIFT: u32 = 60;

fn u64_at(d: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&d[at..at + 8]);
    u64::from_le_bytes(b)
}
fn i64_at(d: &[u8], at: usize) -> i64 {
    u64_at(d, at) as i64
}
fn u128_at(d: &[u8], at: usize) -> u128 {
    let mut b = [0u8; 16];
    b.copy_from_slice(&d[at..at + 16]);
    u128::from_le_bytes(b)
}
fn key_at(d: &[u8], at: usize) -> [u8; 32] {
    let mut k = [0u8; 32];
    k.copy_from_slice(&d[at..at + 32]);
    k
}

/// The reserve fields the pool reads. `total` is klend's `total_supply()`: liquidity owned by
/// collateral holders (available + borrowed − fees owed to klend and referrers), rounded down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reserve {
    pub lending_market: [u8; 32],
    pub liquidity_mint: [u8; 32],
    pub supply_vault: [u8; 32],
    pub liquidity_token_program: [u8; 32],
    pub collateral_mint: [u8; 32],
    pub price: PriceSource,
    pub available: u64,
    pub total: u64,
    pub collateral_supply: u64,
    pub status: u8,
    pub emergency: bool,
    pub deposit_limit: u64,
    pub cap: WithdrawCap,
    pub queued_collateral: u64,
}

/// klend `WithdrawalCaps` (the deposit/withdrawal one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WithdrawCap {
    pub capacity: i64,
    pub current: i64,
    pub start: u64,
    pub interval: u64,
}

/// Parses a reserve. The caller has already checked owner, discriminator and length.
pub fn parse_reserve(d: &[u8]) -> Result<Reserve> {
    if d.len() < RESERVE_LEN || d[..8] != ACCOUNT_RESERVE {
        return Err(CoreError::BadAccount);
    }
    let available = u64_at(d, R_LIQ_AVAILABLE);
    let owed = u128_at(d, R_LIQ_PROTOCOL_FEES_SF)
        .checked_add(u128_at(d, R_LIQ_REFERRER_FEES_SF))
        .and_then(|v| v.checked_add(u128_at(d, R_LIQ_PENDING_REFERRER_FEES_SF)))
        .ok_or(CoreError::Overflow)?;
    let total_sf = ((available as u128) << SF_SHIFT)
        .checked_add(u128_at(d, R_LIQ_BORROWED_SF))
        .ok_or(CoreError::Overflow)?
        .saturating_sub(owed);
    Ok(Reserve {
        lending_market: key_at(d, R_LENDING_MARKET),
        liquidity_mint: key_at(d, R_LIQ_MINT),
        supply_vault: key_at(d, R_LIQ_SUPPLY_VAULT),
        liquidity_token_program: key_at(d, R_LIQ_TOKEN_PROGRAM),
        collateral_mint: key_at(d, R_COLL_MINT),
        price: price_source(d),
        available,
        total: to_u64(total_sf >> SF_SHIFT)?,
        collateral_supply: u64_at(d, R_COLL_SUPPLY),
        status: d[R_STATUS],
        emergency: d[R_EMERGENCY] != 0,
        deposit_limit: u64_at(d, R_DEPOSIT_LIMIT),
        cap: WithdrawCap {
            capacity: i64_at(d, R_CAP_CAPACITY),
            current: i64_at(d, R_CAP_CURRENT),
            start: u64_at(d, R_CAP_START),
            interval: u64_at(d, R_CAP_INTERVAL),
        },
        queued_collateral: u64_at(d, R_QUEUED_COLLATERAL),
    })
}

/// The market's emergency flag. The caller has already checked owner.
pub fn market_emergency(d: &[u8]) -> Result<bool> {
    if d.len() < MARKET_MIN_LEN || d[..8] != ACCOUNT_LENDING_MARKET {
        return Err(CoreError::BadAccount);
    }
    Ok(d[M_EMERGENCY] != 0)
}

impl Reserve {
    /// Collateral minted for `liquidity`, rounded down (klend `liquidity_to_collateral`; 1:1 on an
    /// empty reserve).
    pub fn collateral_for(&self, liquidity: u64) -> Result<u64> {
        if self.collateral_supply == 0 || self.total == 0 {
            return Ok(liquidity);
        }
        crate::mul_div_floor(liquidity, self.collateral_supply, self.total)
    }

    /// Liquidity `collateral` redeems for, rounded down (klend `collateral_to_liquidity`).
    pub fn liquidity_for(&self, collateral: u64) -> Result<u64> {
        if self.collateral_supply == 0 {
            return Ok(collateral);
        }
        crate::mul_div_floor(collateral, self.total, self.collateral_supply)
    }

    /// Liquidity a redeem can take now: what sits in the reserve, less what queued withdraw tickets
    /// are owed (they go first), less one unit for klend's own rounding.
    pub fn redeemable(&self) -> Result<u64> {
        let queued = self.liquidity_for(self.queued_collateral)?;
        Ok(self.available.saturating_sub(queued).saturating_sub(1))
    }

    /// A deposit of `liquidity` would go through: reserve active, no emergency, under the deposit
    /// limit, and it mints at least one unit of collateral.
    pub fn deposit_ok(&self, liquidity: u64, market_emergency: bool) -> Result<bool> {
        if liquidity == 0 || self.status != STATUS_ACTIVE || self.emergency || market_emergency {
            return Ok(false);
        }
        let after = self
            .total
            .checked_add(liquidity)
            .ok_or(CoreError::Overflow)?;
        if after > self.deposit_limit {
            return Ok(false);
        }
        Ok(self.collateral_for(liquidity)? > 0)
    }

    /// A redeem of `collateral` would go through and pay out at least one unit: no emergency, enough
    /// cash in the reserve, and room under the withdrawal cap at `now`.
    pub fn redeem_ok(&self, collateral: u64, market_emergency: bool, now: i64) -> Result<bool> {
        if collateral == 0 || self.emergency || market_emergency {
            return Ok(false);
        }
        let liquidity = self.liquidity_for(collateral)?;
        if liquidity == 0 || liquidity > self.redeemable()? {
            return Ok(false);
        }
        Ok(self.cap.room(now) >= liquidity)
    }
}

impl WithdrawCap {
    /// What may still be withdrawn in the current interval (`u64::MAX` when the cap is off). A cap
    /// whose interval has run out counts from zero again, as klend resets it on the next call.
    pub fn room(&self, now: i64) -> u64 {
        if self.capacity <= 0 || self.interval == 0 {
            return u64::MAX;
        }
        let elapsed = (now.max(0) as u64).saturating_sub(self.start);
        let current = if elapsed >= self.interval {
            0
        } else {
            self.current
        };
        (self.capacity as i128 - current as i128).clamp(0, u64::MAX as i128) as u64
    }
}
