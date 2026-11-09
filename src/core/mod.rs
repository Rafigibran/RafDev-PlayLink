//! RafDev PlayLink core types, configuration, and shared state.
//!
//! Everything in this module is platform independent. Windows-only code
//! lives behind `#[cfg(windows)]` in the `platform` modules.

pub mod config;
pub mod error;
pub mod state;
pub mod types;

pub use config::Settings;
pub use error::{PlayLinkError, Result};
pub use state::SharedState;
pub use types::*;
