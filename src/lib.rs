//! CodeDiff library crate.
pub mod diff;
pub mod error;
pub mod history;
pub mod model;
pub mod recent;
pub mod repo;
pub mod sbs;
pub mod stage;
pub mod status;

pub use error::{CoreError, Result};
