pub mod admin;
pub mod backer;
pub mod claim;
pub mod registry;
pub mod stake;

pub use admin::*;
pub use backer::*;
pub use claim::*;
pub use registry::*;
pub use stake::*;

use anchor_lang::prelude::*;

pub(crate) fn now() -> Result<i64> {
    Ok(Clock::get()?.unix_timestamp)
}
