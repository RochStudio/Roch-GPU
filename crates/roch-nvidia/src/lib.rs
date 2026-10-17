//! NVIDIA (NVAPI) backend. Stub until the port lands: initialisation fails, so the factory moves on.

use roch_core::{GpuBackend, GpuCapabilities, GpuDevice, GpuError, GpuTelemetry, GpuTuningState, Result};

#[derive(Default)]
pub struct NvidiaBackend {
    devices: Vec<GpuDevice>,
}

impl NvidiaBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

fn not_ported<T>() -> Result<T> {
    Err(GpuError::new("NVIDIA (NVAPI) backend is not ported yet"))
}

impl GpuBackend for NvidiaBackend {
    fn backend_name(&self) -> String {
        "NVIDIA (NVAPI)".into()
    }
    fn initialize(&mut self) -> Result<()> {
        not_ported()
    }
    fn devices(&self) -> &[GpuDevice] {
        &self.devices
    }
    fn capabilities(&mut self, _gpu: usize) -> Result<GpuCapabilities> {
        not_ported()
    }
    fn read_telemetry(&mut self, _gpu: usize) -> Result<GpuTelemetry> {
        not_ported()
    }
    fn read_tuning_state(&mut self, _gpu: usize) -> Result<GpuTuningState> {
        not_ported()
    }
    fn set_core_offset(&mut self, _gpu: usize, _offset_mhz: i32) -> Result<()> {
        not_ported()
    }
    fn set_memory_offset(&mut self, _gpu: usize, _offset_mhz: i32) -> Result<()> {
        not_ported()
    }
    fn set_power_limit(&mut self, _gpu: usize, _percent: i32) -> Result<()> {
        not_ported()
    }
    fn set_temp_limit(&mut self, _gpu: usize, _celsius: i32) -> Result<()> {
        not_ported()
    }
    fn set_voltage_boost(&mut self, _gpu: usize, _percent: i32) -> Result<()> {
        not_ported()
    }
    fn set_voltage_curve_offset(&mut self, _gpu: usize, _offset_mv: i32, _extra_clock_mhz: i32) -> Result<()> {
        not_ported()
    }
    fn set_fan_speed(&mut self, _gpu: usize, _fan_index: i32, _percent: i32) -> Result<()> {
        not_ported()
    }
    fn set_fan_auto(&mut self, _gpu: usize) -> Result<()> {
        not_ported()
    }
    fn reset_to_defaults(&mut self, _gpu: usize) -> Result<()> {
        not_ported()
    }
}
