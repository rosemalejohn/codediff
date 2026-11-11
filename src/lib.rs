//! CodeDiff library crate.
pub mod controller;
pub mod diff;
pub mod error;
pub mod highlight;
pub mod history;
pub mod model;
pub mod recent;
pub mod repo;
pub mod sbs;
pub mod stage;
pub mod status;
pub mod watch;

pub use error::{CoreError, Result};
