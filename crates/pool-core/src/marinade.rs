//! Facts about Marinade's on-chain layout, in one place.
//!
//! Source: `marinade-finance/liquid-staking-program` (`programs/marinade-finance/src/state/*.rs`),
//! read 2026-09-30. Offsets checked against the live `State` account on devnet and mainnet the same
//! day. Marinade is upgradeable: a layout change would move these, so the program checks the account
//! owner, discriminator and length before reading, and fails closed.

/// `State` fields used here: byte offset of each (8-byte Anchor discriminator + Borsh).
pub const STATE_MSOL_MINT: usize = 8;
pub const STATE_TREASURY_MSOL: usize = 104;
pub const STATE_LIQ_POOL_MSOL_LEG: usize = 420;
pub const STATE_LP_LIQUIDITY_TARGET: usize = 452;
pub const STATE_LP_MAX_FEE_BPS: usize = 460;
pub const STATE_LP_MIN_FEE_BPS: usize = 464;
/// Marinade pays unstakes from `total_virtual_staked / msol_supply`, which includes this reserve;
/// `msol_price` is that ratio cached at each epoch update. Tests move both to simulate rewards.
pub const STATE_AVAILABLE_RESERVE_BALANCE: usize = 496;
pub const STATE_MSOL_SUPPLY: usize = 504;
pub const STATE_MSOL_PRICE: usize = 512;
pub const STATE_MIN_DEPOSIT: usize = 544;
pub const STATE_MIN_WITHDRAW: usize = 552;
pub const STATE_PAUSED: usize = 608;
/// Shortest `State` this code can read (the account is 2,048 bytes on devnet, with spare space).
pub const STATE_MIN_LEN: usize = STATE_PAUSED + 1;
/// `msol_price` is SOL per mSOL, scaled by 2^32 (Marinade `State::PRICE_DENOMINATOR`).
pub const PRICE_DENOMINATOR: u128 = 0x1_0000_0000;

/// PDA seeds, each prefixed by the state address.
pub const SEED_LIQ_POOL_SOL_LEG: &[u8] = b"liq_sol";
pub const SEED_LIQ_POOL_MSOL_LEG_AUTHORITY: &[u8] = b"liq_st_sol_authority";
pub const SEED_RESERVE: &[u8] = b"reserve";
pub const SEED_MSOL_MINT_AUTHORITY: &[u8] = b"st_mint";

/// Anchor discriminators: first 8 bytes of sha256("global:<name>") and sha256("account:State").
pub const IX_DEPOSIT: [u8; 8] = [242, 35, 198, 137, 82, 225, 242, 182];
pub const IX_LIQUID_UNSTAKE: [u8; 8] = [30, 30, 119, 240, 191, 227, 12, 16];
pub const ACCOUNT_STATE: [u8; 8] = [216, 146, 107, 94, 104, 75, 182, 177];

/// Marinade's `liquid_unstake` fee in bps (`LiqPool::linear_fee`): the minimum once the SOL left in its
/// liquidity pool after the unstake is at or above the target, rising linearly to the maximum as that
/// SOL falls to zero. Clients show it before a payout that needs an unstake.
pub fn unstake_fee_bps(
    sol_left_after: u64,
    liquidity_target: u64,
    min_fee_bps: u32,
    max_fee_bps: u32,
) -> u32 {
    if sol_left_after >= liquidity_target || liquidity_target == 0 {
        return min_fee_bps;
    }
    let span = max_fee_bps.saturating_sub(min_fee_bps) as u128;
    let cut = span * sol_left_after as u128 / liquidity_target as u128;
    max_fee_bps - cut as u32
}
