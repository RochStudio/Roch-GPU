//! Simulated GPU for developing / demoing the UI without hardware (run with --mock). Produces
//! plausible telemetry that reacts to the applied settings, and records every mutating call so
//! tests can pin down apply ordering.

use crate::Result;
use crate::backend::GpuBackend;
use crate::models::*;
use crate::util::{clamp_f64, round_even, round_even_to};

/// Small deterministic PRNG (xorshift64*), standing in for `new Random(42)`.
struct Rng(u64);

impl Rng {
    fn next_f64(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

pub struct MockBackend {
    rng: Rng,
    devices: Vec<GpuDevice>,
    core: i32,
    mem: i32,
    power: i32,
    temp: i32,
    fan: i32,
    volt_boost: i32,
    volt_offset: i32,
    rail_max: i32,
    msvdd_max: i32,
    rail_floor: i32,
    msvdd_floor: i32,
    xbar: i32,
    sys: i32,
    video: i32,
    lock_lo: i32,
    lock_hi: i32,
    nvvdd_ocp: i32,
    msvdd_ocp: i32,
    fan_manual: bool,
    load: f64,
    sim_temp: f64,
    volt_lock_mv: i32,
    curve_delta_mhz: [i32; 8],
    /// Every mutating call, in order. A setting written before something that clears it as a
    /// side effect looks fine but lands nowhere; this is what lets tests see that.
    pub calls: Vec<String>,
    /// One duty per cooler, because the real driver addresses them separately and a mock that
    /// forgets that agrees with any bug about it.
    pub fan_percents: [i32; 3],
    /// The card has a settable temperature limit of its own (default). False models an RTX 50 card:
    /// no thermal policy, so the software temperature limit is offered instead.
    pub hardware_temp_limit: bool,
    /// Hold the simulated load here instead of random-walking it (tests).
    pub fixed_load: Option<f64>,
    /// Every `set_clock_range` call, in order.
    pub clock_range_writes: Vec<(i32, i32)>,
    /// Make `set_clock_range` fail (tests of the error paths).
    pub fail_clock_range: bool,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockBackend {
    pub fn new() -> Self {
        Self {
            rng: Rng(42),
            devices: vec![GpuDevice {
                index: 0,
                name: "Simulated RTX 4070 SUPER".into(),
                vendor: "Mock".into(),
                bus_id: "PCI bus 1, slot 0".into(),
                driver_version: "560.94".into(),
                vram_megabytes: 12282,
                bios_version: "95.04.4E.00.6B".into(),
            }],
            core: 0,
            mem: 0,
            power: 100,
            temp: 83,
            fan: 30,
            volt_boost: 0,
            volt_offset: 0,
            rail_max: 1035,
            msvdd_max: 985,
            rail_floor: 0,
            msvdd_floor: 0,
            xbar: 0,
            sys: 0,
            video: 0,
            lock_lo: 0,
            lock_hi: 0,
            nvvdd_ocp: 300_000,
            msvdd_ocp: 120_000,
            fan_manual: false,
            load: 5.0,
            sim_temp: 40.0,
            volt_lock_mv: 0,
            curve_delta_mhz: [0; 8],
            calls: Vec::new(),
            fan_percents: [50, 50, 50],
            hardware_temp_limit: true,
            fixed_load: None,
            clock_range_writes: Vec::new(),
            fail_clock_range: false,
        }
    }

    /// A card like an RTX 5070 Ti: no settable temperature limit, so the software one is offered,
    /// with a driver slowdown at 90 °C (own limit 88 °C, window 50..105 °C).
    pub fn without_hardware_temp_limit() -> Self {
        let mut m = Self { hardware_temp_limit: false, ..Self::new() };
        if let Some(d) = m.devices.first_mut() {
            d.name = "Simulated RTX 5070 Ti".into();
            d.vram_megabytes = 16303;
        }
        m
    }

    /// A machine with two cards, for the multi-GPU selection tests and `--mock multi`. The mock
    /// ignores the index for reads and writes, but enumerates both so selection can be exercised.
    pub fn with_second_gpu() -> Self {
        let mut m = Self::new();
        let mut second = m.devices[0].clone();
        second.index = 1;
        second.name = "Simulated RTX 4070 SUPER (2)".into();
        second.bus_id = "PCI bus 2, slot 0".into();
        m.devices.push(second);
        m
    }

    /// The simulated temperature right now, before the next reading moves it.
    pub fn sim_temp_c(&self) -> f64 {
        self.sim_temp
    }

    /// Start the simulated temperature somewhere (tests).
    pub fn set_sim_temp_c(&mut self, c: f64) {
        self.sim_temp = c;
    }

    /// The clock window currently pinned, 0/0 when unpinned.
    pub fn clock_range(&self) -> (i32, i32) {
        (self.lock_lo, self.lock_hi)
    }

    fn call(&mut self, name: &str) {
        self.calls.push(name.to_string());
    }

    fn stock_mv(i: i32) -> i32 {
        700 + i * 50
    }

    fn stock_mhz(i: i32) -> i32 {
        1500 + i * 150
    }
}

impl GpuBackend for MockBackend {
    fn backend_name(&self) -> String {
        "Simulated GPU".into()
    }

    fn initialize(&mut self) -> Result<()> {
        Ok(())
    }

    fn devices(&self) -> &[GpuDevice] {
        &self.devices
    }

    fn capabilities(&mut self, _gpu: usize) -> Result<GpuCapabilities> {
        let hw_temp = self.hardware_temp_limit;
        // The thresholds an RTX 5070 Ti reports: GPU max 88 °C, slowdown 90 °C.
        let (soft_min, soft_max) = soft_thermal_window(Some(88), Some(90)).unwrap_or((0, 0));
        let soft_card = soft_thermal_card_limit(Some(88), Some(90));
        Ok(GpuCapabilities {
            can_set_core_offset: true,
            can_set_memory_offset: true,
            can_set_power_limit: true,
            can_set_temp_limit: hw_temp,
            // As the NVIDIA backend decides it: a clock that can be capped, and no thermal policy.
            can_set_soft_thermal_limit: !hw_temp,
            soft_thermal_limit_min_c: soft_min,
            soft_thermal_limit_max_c: soft_max,
            soft_thermal_card_limit_c: if hw_temp { 0 } else { soft_card },
            can_set_fan_speed: true,
            can_read_voltage: true,
            can_set_voltage_boost: true,
            can_set_voltage_curve: true,
            voltage_offset_min_mv: -300,
            voltage_offset_max_mv: 0,
            stock_max_voltage_mv: 1100,
            min_voltage_mv: 850,
            max_voltage_mv: 1160,
            core_offset_min_mhz: -1000,
            core_offset_max_mhz: 1000,
            memory_offset_min_mhz: -2000,
            memory_offset_max_mhz: 4000,
            power_limit_min_percent: 50,
            power_limit_max_percent: 115,
            power_limit_default_percent: 100,
            // Zeros without a thermal policy, as the NVIDIA backend reports it.
            temp_limit_min_c: if hw_temp { 65 } else { 0 },
            temp_limit_max_c: if hw_temp { 88 } else { 0 },
            temp_limit_default_c: if hw_temp { 83 } else { 0 },
            voltage_boost_min_percent: 0,
            voltage_boost_max_percent: 100,
            fan_min_percent: 0,
            fan_max_percent: 100,
            fan_count: 3,
            // The gated levers, with a stock MSVDD ceiling below its NVVDD twin the way a real
            // Blackwell card ships, so the disarm path is tested against two different defaults.
            can_set_voltage_rail: true,
            can_set_msvdd_rail: true,
            can_set_xbar_offset: true,
            can_set_ocp: true,
            nvvdd_ocp_stock_milliamps: 300_000,
            msvdd_ocp_stock_milliamps: 120_000,
            can_lock_clocks: true,
            clock_lock_min_mhz: 210,
            clock_lock_max_mhz: 3090,
            can_set_sys_offset: true,
            sys_offset_min_mhz: -150,
            sys_offset_max_mhz: 495,
            can_set_video_offset: true,
            video_offset_min_mhz: -150,
            video_offset_max_mhz: 495,
            voltage_rail_min_mv: 800,
            voltage_rail_max_mv: 1150,
            voltage_rail_stock_max_mv: 1035,
            msvdd_rail_min_mv: 800,
            msvdd_rail_max_mv: 1150,
            msvdd_rail_stock_max_mv: 985,
            voltage_rail_floor_min_mv: 800,
            voltage_rail_floor_max_mv: 915,
            voltage_rail_stock_floor_mv: 800,
            msvdd_rail_floor_min_mv: 800,
            msvdd_rail_floor_max_mv: 915,
            msvdd_rail_stock_floor_mv: 800,
            xbar_offset_min_mhz: -300,
            xbar_offset_max_mhz: 750,
            ..Default::default()
        })
    }

    fn read_telemetry(&mut self, _gpu: usize) -> Result<GpuTelemetry> {
        // Random-walk load; temperature follows load minus fan cooling.
        let walked = clamp_f64(self.load + (self.rng.next_f64() - 0.5) * 20.0, 0.0, 100.0);
        self.load = self.fixed_load.map(|l| clamp_f64(l, 0.0, 100.0)).unwrap_or(walked);
        let fan =
            if self.fan_manual { self.fan as f64 } else { clamp_f64(30.0 + (self.sim_temp - 40.0) * 1.5, 30.0, 100.0) };

        let core = self.core as f64;
        let free_clock = if self.load < 5.0 {
            210.0
        } else {
            (2610.0 + core).min(2810.0 + core) - if self.sim_temp > 70.0 { (self.sim_temp - 70.0) * 15.0 } else { 0.0 }
        };
        // A pinned window holds the clock inside it.
        let mut core_clock = free_clock;
        if self.lock_hi > 0 {
            core_clock = core_clock.min(self.lock_hi as f64);
        }
        if self.lock_lo > 0 && self.load >= 5.0 {
            core_clock = core_clock.max(self.lock_lo as f64);
        }
        // Heat goes roughly with the square of the clock held down against the one the card wanted,
        // so a lower ceiling cools the simulated card (what the software temperature limit relies on).
        let clock_heat = if free_clock > 0.0 { (core_clock / free_clock).clamp(0.0, 1.5).powi(2) } else { 1.0 };
        let target_temp = 35.0 + self.load * 0.45 * (self.power as f64 / 100.0) * clock_heat - fan * 0.08;
        self.sim_temp += (target_temp - self.sim_temp) * 0.15;
        if self.hardware_temp_limit {
            self.sim_temp = self.sim_temp.min(self.temp as f64 + 1.0);
        }
        let mem_clock = if self.load < 5.0 { 405.0 } else { 10501.0 + self.mem as f64 };
        let volt_ceiling =
            if self.volt_lock_mv > 0 { self.volt_lock_mv as f64 } else { 1100.0 + self.volt_offset as f64 };
        let volt = if self.load < 5.0 {
            700.0
        } else {
            clamp_f64(750.0 + (core_clock - 1900.0) * 0.35 + self.volt_boost as f64 * 1.5, 750.0, volt_ceiling)
        };
        let power_pct = if self.load < 5.0 { 8.0 } else { (self.power as f64).min(30.0 + self.load * 0.75) };

        Ok(GpuTelemetry {
            core_clock_mhz: round_even(core_clock),
            memory_clock_mhz: round_even(mem_clock),
            temperature_c: round_even_to(self.sim_temp, 1),
            hot_spot_c: round_even_to(self.sim_temp + 12.0, 1),
            memory_temperature_c: round_even_to(self.sim_temp + 6.0, 1),
            voltage_mv: round_even(volt),
            power_percent: round_even_to(power_pct, 1),
            gpu_load_percent: round_even(self.load),
            memory_load_percent: round_even(self.load * 0.6),
            memory_used_mb: 1500.0 + self.load * 60.0,
            fan_percent: round_even(fan),
            fan_rpm: round_even(fan * 27.0),
            fan_percents: vec![round_even(fan), round_even(fan)],
            fan_rpms: vec![round_even(fan * 27.0), round_even(fan * 26.0)],
            perf_state: if self.load < 5.0 { "P8" } else { "P0" }.into(),
            limit_reason: if power_pct >= self.power as f64 - 0.5 {
                "Power"
            } else if self.hardware_temp_limit && self.sim_temp >= self.temp as f64 {
                "Temperature"
            } else {
                "None"
            }
            .into(),
            ..Default::default()
        })
    }

    fn read_tuning_state(&mut self, _gpu: usize) -> Result<GpuTuningState> {
        Ok(GpuTuningState {
            core_offset_mhz: self.core,
            memory_offset_mhz: self.mem,
            power_limit_percent: self.power,
            temp_limit_c: self.temp,
            voltage_boost_percent: self.volt_boost,
            voltage_offset_mv: self.volt_offset,
            fan_manual: self.fan_manual,
            fan_percent: self.fan,
            voltage_rail_max_mv: self.rail_max,
            msvdd_rail_max_mv: self.msvdd_max,
            voltage_rail_floor_mv: self.rail_floor,
            msvdd_rail_floor_mv: self.msvdd_floor,
            xbar_offset_mhz: self.xbar,
            sys_offset_mhz: self.sys,
            video_offset_mhz: self.video,
            nvvdd_ocp_milliamps: self.nvvdd_ocp,
            msvdd_ocp_milliamps: self.msvdd_ocp,
            locked_clock_min_mhz: self.lock_lo,
            locked_clock_max_mhz: self.lock_hi,
            ..Default::default()
        })
    }

    fn set_voltage_rail_max(&mut self, _gpu: usize, millivolts: i32) -> Result<()> {
        self.call("SetVoltageRailMax");
        self.rail_max = millivolts;
        Ok(())
    }

    fn set_msvdd_rail_max(&mut self, _gpu: usize, millivolts: i32) -> Result<()> {
        self.call("SetMsvddRailMax");
        self.msvdd_max = millivolts;
        Ok(())
    }

    fn set_voltage_rail_floor(&mut self, _gpu: usize, millivolts: i32) -> Result<()> {
        self.rail_floor = millivolts;
        Ok(())
    }

    fn set_msvdd_rail_floor(&mut self, _gpu: usize, millivolts: i32) -> Result<()> {
        self.msvdd_floor = millivolts;
        Ok(())
    }

    fn set_xbar_offset(&mut self, _gpu: usize, offset_mhz: i32) -> Result<()> {
        self.call("SetXbarOffset");
        self.xbar = offset_mhz;
        Ok(())
    }

    fn set_nvvdd_ocp_milliamps(&mut self, _gpu: usize, milliamps: i32) -> Result<()> {
        self.call("SetNvvddOcpMilliamps");
        self.nvvdd_ocp = milliamps;
        Ok(())
    }

    fn set_msvdd_ocp_milliamps(&mut self, _gpu: usize, milliamps: i32) -> Result<()> {
        self.call("SetMsvddOcpMilliamps");
        self.msvdd_ocp = milliamps;
        Ok(())
    }

    fn set_sys_offset(&mut self, _gpu: usize, offset_mhz: i32) -> Result<()> {
        self.sys = offset_mhz;
        Ok(())
    }

    fn set_video_offset(&mut self, _gpu: usize, offset_mhz: i32) -> Result<()> {
        self.video = offset_mhz;
        Ok(())
    }

    fn set_clock_range(&mut self, _gpu: usize, min_mhz: i32, max_mhz: i32) -> Result<()> {
        self.clock_range_writes.push((min_mhz, max_mhz));
        if self.fail_clock_range {
            crate::bail!("Simulated clock-range failure");
        }
        self.lock_lo = min_mhz;
        self.lock_hi = max_mhz;
        Ok(())
    }

    fn set_core_offset(&mut self, _gpu: usize, offset_mhz: i32) -> Result<()> {
        self.call("SetCoreOffset");
        self.core = offset_mhz;
        Ok(())
    }

    fn set_memory_offset(&mut self, _gpu: usize, offset_mhz: i32) -> Result<()> {
        self.call("SetMemoryOffset");
        self.mem = offset_mhz;
        Ok(())
    }

    fn set_power_limit(&mut self, _gpu: usize, percent: i32) -> Result<()> {
        self.power = percent;
        Ok(())
    }

    fn set_temp_limit(&mut self, _gpu: usize, celsius: i32) -> Result<()> {
        self.temp = celsius;
        Ok(())
    }

    /// Synthetic V/F curve: 8 points 700→1050 mV, 1500→2550 MHz. Deltas remembered.
    fn read_vf_curve(&mut self, _gpu: usize) -> Result<Vec<VfCurveSample>> {
        Ok((0..8)
            .map(|i| {
                VfCurveSample::new(
                    i,
                    Self::stock_mv(i),
                    Self::stock_mhz(i),
                    Self::stock_mhz(i) + self.curve_delta_mhz[i as usize],
                )
            })
            .collect())
    }

    fn set_vf_curve_targets(&mut self, _gpu: usize, targets: &[VfCurveSample]) -> Result<()> {
        self.curve_delta_mhz = [0; 8];
        for t in targets {
            if (0..8).contains(&t.index) {
                self.curve_delta_mhz[t.index as usize] = t.live_mhz - Self::stock_mhz(t.index);
            }
        }
        Ok(())
    }

    fn set_voltage_boost(&mut self, _gpu: usize, percent: i32) -> Result<()> {
        self.call("SetVoltageBoost");
        // Model a card whose driver-owned NVVDD/MSVDD ceilings move with Voltage Boost. The 20 mV
        // full-range travel matches the development card; production code must read whatever its
        // own card reports rather than depend on this value.
        let rail_shift_mv = (percent - self.volt_boost) / 5;
        self.rail_max += rail_shift_mv;
        self.msvdd_max += rail_shift_mv;
        self.volt_boost = percent;
        Ok(())
    }

    fn set_voltage_lock(&mut self, _gpu: usize, target_mv: i32) -> Result<()> {
        self.call("SetVoltageLock");
        self.volt_lock_mv = target_mv;
        Ok(())
    }

    fn read_voltage_lock_mv(&mut self, _gpu: usize) -> i32 {
        self.volt_lock_mv
    }

    fn set_voltage_curve_offset(&mut self, _gpu: usize, offset_mv: i32, extra_clock_mhz: i32) -> Result<()> {
        self.call("SetVoltageCurveOffset");
        self.volt_offset = offset_mv;
        if offset_mv != 0 {
            self.core = extra_clock_mhz; // curve carries the core offset while undervolting
        }
        Ok(())
    }

    fn set_fan_speed(&mut self, _gpu: usize, fan_index: i32, percent: i32) -> Result<()> {
        self.fan_manual = true;
        self.fan = percent;
        // -1 is every cooler; otherwise the id is 1-based, as NVAPI numbers them.
        for (i, f) in self.fan_percents.iter_mut().enumerate() {
            if fan_index < 0 || fan_index == i as i32 + 1 {
                *f = percent;
            }
        }
        Ok(())
    }

    fn set_fan_auto(&mut self, _gpu: usize) -> Result<()> {
        self.fan_manual = false;
        Ok(())
    }

    fn reset_to_defaults(&mut self, _gpu: usize) -> Result<()> {
        self.core = 0;
        self.mem = 0;
        self.power = 100;
        self.temp = 83;
        self.volt_boost = 0;
        self.volt_offset = 0;
        self.volt_lock_mv = 0;
        self.fan_manual = false;
        // The private domains too: a mock that forgets the same thing agrees with the bug.
        self.xbar = 0;
        self.sys = 0;
        self.video = 0;
        self.lock_lo = 0;
        self.lock_hi = 0;
        self.nvvdd_ocp = 300_000;
        self.msvdd_ocp = 120_000;
        Ok(())
    }

    fn diagnostics(&mut self, _gpu: usize) -> String {
        let fan = if self.fan_manual { format!("{}%", self.fan) } else { "auto".into() };
        format!(
            "Simulated GPU — nothing real to dump.\r\ncore={} mem={} power={} temp={} vboost={} fan={}\r\n",
            self.core, self.mem, self.power, self.temp, self.volt_boost, fan
        )
    }

    fn as_mock(&mut self) -> Option<&mut MockBackend> {
        Some(self)
    }
}
