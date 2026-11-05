//! AMD backend for Linux. There is no vendor library to load: the kernel's amdgpu driver is the whole
//! interface, and it speaks through text files under `/sys/class/drm/card*/device`.
//!
//! * [`sysfs`] reads and writes those files, and turns a failed write into something a person can act on
//! * [`od`] parses the overdrive tables (`pp_od_clk_voltage`, `gpu_od/fan_ctrl/*`) and plans a fan curve
//! * [`gpu_metrics`] decodes the binary sample behind the sensors hwmon does not carry
//! * [`identity`] names a card from the distribution's `amdgpu.ids` and `pci.ids`
//! * [`backend`] puts them behind [`roch_core::GpuBackend`]
//!
//! Empty on every other platform, so the workspace still builds and tests on Windows.

#![cfg(target_os = "linux")]

pub mod backend;
pub mod gpu_metrics;
pub mod identity;
pub mod od;
pub mod sysfs;

pub use backend::AmdgpuBackend;
