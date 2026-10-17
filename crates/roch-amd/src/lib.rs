//! AMD backend: Overdrive 8 over ADL (atiadlxx.dll) for control, plus read-only ADLX
//! (amdadlx64.dll) metrics as a telemetry fallback.

pub mod adl;
pub mod adlx;
mod backend;
pub mod mapper;

pub use adlx::{AdlxTelemetry, Metric};
pub use backend::{AdlBackend, AmdBackend, build_timing_options, infer_fan_mode};
pub use mapper::{AmdTelemetryMapper, Sensor};
