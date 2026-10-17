//! Roch GPU engine: vendor-neutral models, the backend trait every vendor implements, the
//! simulated backend, and the services (profiles, recovery journal, history, startup task) the
//! window and the command line share.

pub mod backend;
pub mod mock;
pub mod models;
pub mod native;
pub mod services;
pub mod util;

pub use backend::GpuBackend;
pub use models::*;

use std::fmt;

/// The one error type every backend and service reports. Carries a message meant for a person,
/// because that is all the callers ever do with it: show it, log it, or fold it into an apply's
/// list of messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuError {
    pub message: String,
}

impl GpuError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }

    /// Wrap another error with context, keeping its text.
    pub fn context(message: impl Into<String>, inner: impl fmt::Display) -> Self {
        Self { message: format!("{}: {inner}", message.into()) }
    }
}

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GpuError {}

impl From<std::io::Error> for GpuError {
    fn from(e: std::io::Error) -> Self {
        Self::new(e.to_string())
    }
}

impl From<serde_json::Error> for GpuError {
    fn from(e: serde_json::Error) -> Self {
        Self::new(e.to_string())
    }
}

impl From<String> for GpuError {
    fn from(message: String) -> Self {
        Self { message }
    }
}

impl From<&str> for GpuError {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

pub type Result<T> = std::result::Result<T, GpuError>;

/// Shorthand for `Err(GpuError::new(..))`.
#[macro_export]
macro_rules! bail {
    ($($arg:tt)*) => { return Err($crate::GpuError::new(format!($($arg)*))) };
}

/// Apply messages that start with this are information, not failure: a lever the card does not
/// have, a value the driver rounded. An apply whose messages are all notes succeeded.
pub const NOTE_PREFIX: &str = "Note:";

/// True when there is at least one message and every message is a note, i.e. the apply itself
/// succeeded. An empty list is false here; callers test `is_empty() || only_notes(..)`.
pub fn only_notes<S: AsRef<str>>(messages: &[S]) -> bool {
    !messages.is_empty() && messages.iter().all(|m| m.as_ref().starts_with(NOTE_PREFIX))
}

/// The version the executable reports.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
