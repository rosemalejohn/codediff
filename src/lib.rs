//! CodeDiff library crate.
pub mod branch;
pub mod controller;
pub mod diff;
pub mod error;
pub mod flow;
pub mod flow_finish;
pub mod highlight;
pub mod history;
pub mod model;
pub mod recent;
pub mod repo;
pub mod sbs;
pub mod settings;
pub mod stage;
pub mod status;
pub mod switcher;
pub mod ui;
pub mod ui_bridge;
pub mod watch;

pub use error::{CoreError, Result};
