//! Services shared by the window and the command line.

pub mod profile_store;
pub mod recovery;
pub mod startup_task;
pub mod telemetry_history;

pub use profile_store::{AppSettings, ProfileStore};
pub use recovery::ProfileRecovery;
pub use telemetry_history::TelemetryHistory;
