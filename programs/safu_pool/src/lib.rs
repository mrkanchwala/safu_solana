//! SAFU Staking on Solana: a SOL protection pool, staked through Marinade.
//!
//! Stakers and backers only. Rules follow the multichain protection pool; every number comes from
//! `pool-core`. See `docs/PORT_MAP.md` for what was ported, what changed and what was left out.

use anchor_lang::prelude::*;

pub mod constants;

declare_id!("Aa6ncthjKmWaDX91jrY3HeUdHfP2fnhaPLQAPPVsG686");

#[program]
pub mod safu_pool {}
