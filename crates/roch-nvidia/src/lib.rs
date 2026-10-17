//! NVIDIA backend: [`NvidiaBackend`] (port of `NvApiBackend.cs`) on NVAPI, with NVML for board
//! watts and clock locks.
//!
//! The layers under it: [`nvapi`] (the NvAPIWrapper subset), [`wrapper`] (the wrapper's
//! aggregation objects rebuilt on the raw calls), [`private`] (the private NVAPI families),
//! [`nvml`], [`supplemental`], and the pure helpers [`vf_curve`], [`ocp`] and [`domain_offset`].
//! See `PRIVATE_API.md` for how they map onto the C#, and `BACKEND_NOTES.md` for the port's status.

mod backend;
pub(crate) mod buf;
pub mod domain_offset;
pub mod nvapi;
pub mod nvml;
pub mod ocp;
pub mod private;
pub mod supplemental;
pub mod vf_curve;
pub mod wrapper;

pub use backend::{ClockDomain, NvidiaBackend, domain_name, power_rail_name};
