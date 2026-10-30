//! The vendor-neutral control surface. One implementation per vendor API (NVAPI/NVML, ADL/ADLX,
//! IGCL) plus a mock for running the UI without hardware.
//!
//! Methods take `&mut self`: callers (TuningService) serialise every driver call behind one lock,
//! so a backend never sees two calls at once and can keep plain mutable state.

use std::collections::BTreeMap;

use crate::models::*;
use crate::{GpuError, Result};

pub trait GpuBackend: Send {
    fn backend_name(&self) -> String;

    /// Initialise the vendor API and enumerate devices. Fails if the driver/API isn't available.
    fn initialize(&mut self) -> Result<()>;

    fn devices(&self) -> &[GpuDevice];

    /// Static identity shown by the read-only Graphics window.
    fn read_graphics_info(&mut self, gpu: usize) -> GpuGraphicsInfo {
        self.devices().get(gpu).map(GpuGraphicsInfo::from_device).unwrap_or_default()
    }

    fn capabilities(&mut self, gpu: usize) -> Result<GpuCapabilities>;
    fn read_telemetry(&mut self, gpu: usize) -> Result<GpuTelemetry>;

    /// Temperature-only read, for ticks where nothing is on screen and the sample exists purely
    /// to step a fan curve. Only `temperature_c` is guaranteed to be populated — never store the
    /// result or show it. The default falls back to the full read.
    fn read_temperature_only(&mut self, gpu: usize) -> Result<GpuTelemetry> {
        self.read_telemetry(gpu)
    }

    fn read_tuning_state(&mut self, gpu: usize) -> Result<GpuTuningState>;

    fn set_core_offset(&mut self, gpu: usize, offset_mhz: i32) -> Result<()>;
    /// Minimum core clock in MHz, for a backend reporting `can_set_core_min_clock`.
    fn set_core_min_clock(&mut self, _gpu: usize, _mhz: i32) -> Result<()> {
        Err(GpuError::new("This card does not expose a minimum core clock."))
    }
    fn set_memory_offset(&mut self, gpu: usize, offset_mhz: i32) -> Result<()>;
    fn set_power_limit(&mut self, gpu: usize, percent: i32) -> Result<()>;
    fn set_temp_limit(&mut self, gpu: usize, celsius: i32) -> Result<()>;

    /// Core voltage boost in percent (0 = stock). Raises the ceiling of the V/F curve.
    fn set_voltage_boost(&mut self, gpu: usize, percent: i32) -> Result<()>;

    /// Negative mV offset: caps the V/F curve that far below its stock top. 0 clears the edit.
    /// `extra_clock_mhz` lifts the clock at the cap point — pass the user's core offset here,
    /// because on Pascal and later the pstate offset and the curve delta table are the same
    /// storage, so writing both would clobber the flatten.
    fn set_voltage_curve_offset(&mut self, gpu: usize, offset_mv: i32, extra_clock_mhz: i32) -> Result<()>;

    /// Cap the core at an absolute voltage in mV; 0 clears it.
    fn set_voltage_lock(&mut self, _gpu: usize, _target_mv: i32) -> Result<()> {
        Ok(())
    }

    /// The absolute voltage the core is currently locked to, in mV; 0 = no lock; -1 when the
    /// backend cannot report it.
    fn read_voltage_lock_mv(&mut self, _gpu: usize) -> i32 {
        -1
    }

    /// Which lever is currently holding the voltage cap ("none" when uncapped).
    fn voltage_lock_mechanism(&self) -> String {
        "n/a".into()
    }

    /// Fans stop completely below the driver's idle threshold (AMD "zero RPM").
    fn set_zero_rpm(&mut self, _gpu: usize, _enabled: bool) -> Result<()> {
        Ok(())
    }

    /// Select a memory timing preset by index into `GpuCapabilities::memory_timing_options`.
    fn set_memory_timing(&mut self, _gpu: usize, _level: i32) -> Result<()> {
        Ok(())
    }

    /// Hand a fan curve to the driver, for cards whose curve runs in hardware.
    fn set_fan_curve(&mut self, _gpu: usize, _curve: &FanCurve) -> Result<()> {
        Err(GpuError::new("This backend has no hardware fan curve."))
    }

    /// Move the core rail's voltage ceiling to an absolute mV value; 0 restores the default.
    fn set_voltage_rail_max(&mut self, _gpu: usize, _millivolts: i32) -> Result<()> {
        Ok(())
    }

    /// Move the MSVDD rail's ceiling to an absolute mV value; 0 restores the default.
    fn set_msvdd_rail_max(&mut self, _gpu: usize, _millivolts: i32) -> Result<()> {
        Ok(())
    }

    /// Move a rail's floor to an absolute mV value; 0 restores the card's default.
    fn set_voltage_rail_floor(&mut self, _gpu: usize, _millivolts: i32) -> Result<()> {
        Ok(())
    }
    fn set_msvdd_rail_floor(&mut self, _gpu: usize, _millivolts: i32) -> Result<()> {
        Ok(())
    }

    /// Offset the crossbar (interconnect) clock, in MHz.
    fn set_xbar_offset(&mut self, _gpu: usize, _offset_mhz: i32) -> Result<()> {
        Ok(())
    }

    /// Offset the SYS and video clocks, in MHz.
    fn set_sys_offset(&mut self, _gpu: usize, _offset_mhz: i32) -> Result<()> {
        Ok(())
    }
    fn set_video_offset(&mut self, _gpu: usize, _offset_mhz: i32) -> Result<()> {
        Ok(())
    }

    /// Pin the graphics clock between min and max MHz; 0/0 hands the range back to the driver.
    fn set_clock_range(&mut self, _gpu: usize, _min_mhz: i32, _max_mhz: i32) -> Result<()> {
        Ok(())
    }

    /// The graphics-clock ceiling the voltage cap is currently enforced through, in MHz; 0 when the
    /// cap holds no clock lock. NVIDIA falls back to an NVML clock lock when the driver ignores its
    /// voltage lock, and that is the same lock `set_clock_range` moves.
    fn voltage_cap_clock_mhz(&self, _gpu: usize) -> i32 {
        0
    }

    /// Pin the memory clock between min and max MHz (equal ends hold one clock); 0/0 hands it back
    /// to the driver.
    fn set_memory_lock(&mut self, _gpu: usize, _min_mhz: i32, _max_mhz: i32) -> Result<()> {
        Ok(())
    }

    /// How many display-driver resets this backend has noticed and recovered from. A reset clears
    /// state written before it (a clock lock, say), so a change means "write it again".
    fn driver_reset_count(&self) -> u64 {
        0
    }

    /// Over-current protection limit for a rail, in milliamps.
    fn set_nvvdd_ocp_milliamps(&mut self, _gpu: usize, _milliamps: i32) -> Result<()> {
        Ok(())
    }
    fn set_msvdd_ocp_milliamps(&mut self, _gpu: usize, _milliamps: i32) -> Result<()> {
        Ok(())
    }

    /// Clock domains beyond core and memory, keyed for display. Empty by default.
    fn measure_extra_clocks(&mut self, _gpu: usize) -> BTreeMap<String, f64> {
        BTreeMap::new()
    }

    /// Force a fan to a fixed duty. `fan_index = -1` means all fans; otherwise 1-based.
    fn set_fan_speed(&mut self, gpu: usize, fan_index: i32, percent: i32) -> Result<()>;

    /// Return fan control to the driver/VBIOS.
    fn set_fan_auto(&mut self, gpu: usize) -> Result<()>;

    /// Restore all tunables to their driver defaults.
    fn reset_to_defaults(&mut self, gpu: usize) -> Result<()>;

    /// Raw dump of what the vendor API reports, for debugging unsupported cards.
    fn diagnostics(&mut self, _gpu: usize) -> String {
        "No diagnostics for this backend.".into()
    }

    /// Read the editable V/F curve points (valid points only). Empty if unsupported.
    fn read_vf_curve(&mut self, _gpu: usize) -> Result<Vec<VfCurveSample>> {
        Ok(Vec::new())
    }

    /// Write explicit per-point target frequencies. Each sample's `index` + `live_mhz` (used as
    /// the target) define the point; every other point is returned to stock. Empty clears.
    fn set_vf_curve_targets(&mut self, _gpu: usize, _targets: &[VfCurveSample]) -> Result<()> {
        Err(GpuError::new("V/F curve editing is not supported by this backend."))
    }

    /// Test hook: the mock records every mutating call. Real backends return `None`.
    fn as_mock(&mut self) -> Option<&mut crate::mock::MockBackend> {
        None
    }

    /// Downcast hook for vendor-specific extras (the CLI's NVIDIA `domains` verb). A backend that
    /// offers such extras returns `Some(self)`; the default offers none.
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }
}
