//! Picks the backend for whatever card is in the machine (C# `BackendFactory`).
//!
//! Detection is by attempting initialisation, not by sniffing device IDs: a vendor library that
//! loads, initialises and enumerates at least one GPU is the definition of "this backend works
//! here". Each attempt's failure is kept so a machine with no supported card can be told exactly
//! what was tried and why it didn't take.

use std::panic::{AssertUnwindSafe, catch_unwind};

use roch_core::mock::MockBackend;
use roch_core::{GpuBackend, GpuError, Result};

/// One backend that was tried and did not take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub name: String,
    pub error: String,
}

/// A constructor for one vendor backend.
pub type Maker = fn() -> Box<dyn GpuBackend>;

/// NVIDIA first, then AMD, then Intel — the C# order.
pub const VENDORS: [Maker; 3] = [
    || Box::new(roch_nvidia::NvidiaBackend::new()),
    || Box::new(roch_amd::AmdBackend::new()),
    || Box::new(roch_intel::IntelBackend::new()),
];

/// Initialise the first vendor backend that works. On failure the error lists every attempt.
/// On success the attempts that failed before it are returned alongside.
pub fn create_and_initialize() -> Result<(Box<dyn GpuBackend>, Vec<Attempt>)> {
    create_from(&VENDORS)
}

/// The word after `--mock` that picks the simulated RTX 50: a card whose driver offers no
/// temperature limit, so the software one is shown instead.
pub const MOCK_RTX50: &str = "rtx50";

/// The simulated GPU for `--mock [variant]`, uninitialised like a real one fresh from its
/// constructor. `rtx50` (any case) is the card without a hardware
/// temperature limit; anything else, or nothing, the default card.
pub fn mock_variant(variant: Option<&str>) -> Box<dyn GpuBackend> {
    if variant.is_some_and(|v| v.trim().eq_ignore_ascii_case(MOCK_RTX50)) {
        Box::new(MockBackend::without_hardware_temp_limit())
    } else {
        Box::new(MockBackend::new())
    }
}

/// [`create_and_initialize`] over an explicit list, for tests.
pub fn create_from(makers: &[Maker]) -> Result<(Box<dyn GpuBackend>, Vec<Attempt>)> {
    let mut tried = Vec::new();
    for make in makers {
        // A vendor library that panics must not stop the next vendor being tried; the C# caught
        // every exception here for the same reason.
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let mut backend = make();
            let result = backend.initialize();
            (backend, result)
        }));
        match outcome {
            Ok((backend, Ok(()))) => {
                if !backend.devices().is_empty() {
                    return Ok((backend, tried));
                }
                tried.push(Attempt { name: backend.backend_name(), error: "initialised but reported no GPUs".into() });
            }
            Ok((backend, Err(e))) => tried.push(Attempt { name: backend.backend_name(), error: e.message }),
            Err(_) => tried.push(Attempt { name: "unknown".into(), error: "the backend panicked".into() }),
        }
        // The failed backend is dropped here (C# Dispose).
    }
    Err(GpuError::new(no_gpu_message(&tried)))
}

fn no_gpu_message(tried: &[Attempt]) -> String {
    let lines: Vec<String> = tried.iter().map(|a| format!("  {}: {}", a.name, a.error)).collect();
    format!("No supported GPU found. Tried:\n{}", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use roch_core::*;

    /// A backend whose initialise outcome and device list are chosen by the test.
    struct Fake {
        name: &'static str,
        fail: Option<&'static str>,
        devices: Vec<GpuDevice>,
    }

    impl GpuBackend for Fake {
        fn backend_name(&self) -> String {
            self.name.into()
        }
        fn initialize(&mut self) -> Result<()> {
            match self.fail {
                Some(m) => Err(GpuError::new(m)),
                None => Ok(()),
            }
        }
        fn devices(&self) -> &[GpuDevice] {
            &self.devices
        }
        fn capabilities(&mut self, _: usize) -> Result<GpuCapabilities> {
            Ok(GpuCapabilities::default())
        }
        fn read_telemetry(&mut self, _: usize) -> Result<GpuTelemetry> {
            Ok(GpuTelemetry::default())
        }
        fn read_tuning_state(&mut self, _: usize) -> Result<GpuTuningState> {
            Ok(GpuTuningState::default())
        }
        fn set_core_offset(&mut self, _: usize, _: i32) -> Result<()> {
            Ok(())
        }
        fn set_memory_offset(&mut self, _: usize, _: i32) -> Result<()> {
            Ok(())
        }
        fn set_power_limit(&mut self, _: usize, _: i32) -> Result<()> {
            Ok(())
        }
        fn set_temp_limit(&mut self, _: usize, _: i32) -> Result<()> {
            Ok(())
        }
        fn set_voltage_boost(&mut self, _: usize, _: i32) -> Result<()> {
            Ok(())
        }
        fn set_voltage_curve_offset(&mut self, _: usize, _: i32, _: i32) -> Result<()> {
            Ok(())
        }
        fn set_fan_speed(&mut self, _: usize, _: i32, _: i32) -> Result<()> {
            Ok(())
        }
        fn set_fan_auto(&mut self, _: usize) -> Result<()> {
            Ok(())
        }
        fn reset_to_defaults(&mut self, _: usize) -> Result<()> {
            Ok(())
        }
    }

    fn one_gpu() -> Vec<GpuDevice> {
        vec![GpuDevice { name: "Card".into(), ..Default::default() }]
    }

    #[test]
    fn every_failure_is_listed() {
        let makers: [Maker; 3] = [
            || Box::new(Fake { name: "A", fail: Some("no dll"), devices: vec![] }),
            || Box::new(Fake { name: "B", fail: None, devices: vec![] }),
            || panic!("boom"),
        ];
        let err = create_from(&makers).err().unwrap();
        assert_eq!(
            err.message,
            "No supported GPU found. Tried:\n  A: no dll\n  B: initialised but reported no GPUs\n  unknown: the backend panicked"
        );
    }

    #[test]
    fn first_working_backend_wins() {
        let makers: [Maker; 3] = [
            || Box::new(Fake { name: "A", fail: Some("no dll"), devices: vec![] }),
            || Box::new(Fake { name: "B", fail: None, devices: one_gpu() }),
            || Box::new(Fake { name: "C", fail: None, devices: one_gpu() }),
        ];
        let (backend, tried) = create_from(&makers).unwrap();
        assert_eq!(backend.backend_name(), "B");
        assert_eq!(tried, vec![Attempt { name: "A".into(), error: "no dll".into() }]);
    }
}
