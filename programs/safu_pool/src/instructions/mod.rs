pub mod admin;
pub mod backer;
pub mod cctp;
pub mod claim;
pub mod governance;
pub mod registry;
pub mod settings;
pub mod stake;
pub mod upkeep;
pub mod views;
pub mod yield_leg;

pub use admin::*;
pub use backer::*;
pub use cctp::*;
pub use claim::*;
pub use governance::*;
pub use registry::*;
pub use settings::*;
pub use stake::*;
pub use upkeep::*;
pub use views::*;
pub use yield_leg::*;

use anchor_lang::prelude::*;

pub(crate) fn now() -> Result<i64> {
    Ok(Clock::get()?.unix_timestamp)
}
