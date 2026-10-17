//! Intel Arc tuning through the public Graphics Control Library (IGCL).
//!
//! Discovery and monitoring never change tuning; IGCL's tuning waiver is set only right before a
//! write. Every scalar write is range/grid validated, skipped when unchanged (so a general Apply
//! does not disturb a custom V/F curve), and verified by readback. See `docs/intel-arc-b60.md`.

use std::collections::HashSet;
use std::ptr;
use std::thread;
use std::time::Duration;

use roch_core::util::{clamp_i32, round_even, round_even_i32};
use roch_core::{
    FanCurve, FanMode, FanPoint, GpuBackend, GpuCapabilities, GpuDevice, GpuGraphicsInfo, GpuIdentity, GpuTelemetry,
    GpuTuningState, SupplementalSensor, VfCurveSample, VoltageControlStyle,
};

use crate::igcl::{self, Buf, CtlHandle, Fault, Igcl, Point, R, check, fail, sensor, swallow};

// The main window's editor increments on Intel (MainViewModel keys them off
// `VoltageControlStyle::Percent`). They are the UI's grid, coarser than the native API granularity
// the capabilities report (1 MHz core, 1 Mbps memory, 1 %); a loaded off-grid value is shown as
// read and never rounded onto this grid. The backend itself accepts any on-native-grid value.

/// Editor increment for the voltage-limit percentage.
pub const EDITOR_VOLTAGE_STEP_PERCENT: i32 = 5;
/// Editor increment for the core offset, MHz.
pub const EDITOR_CORE_STEP_MHZ: i32 = 15;
/// Editor increment for the absolute memory speed, Mbps.
pub const EDITOR_MEMORY_STEP_MBPS: i32 = 100;
/// Editor increment for the power limit percentage.
pub const EDITOR_POWER_STEP_PERCENT: i32 = 5;
/// Editor increment for the temperature-limit percentage.
pub const EDITOR_TEMP_STEP_PERCENT: i32 = 5;

// `ctl_oc_properties_t` control slots.
const GPU_FREQUENCY_OFFSET: usize = 0;
const GPU_VOLTAGE_OFFSET: usize = 1;
const POWER_LIMIT: usize = 4;
const TEMPERATURE_LIMIT: usize = 5;
const VRAM_MEM_SPEED_LIMIT: usize = 6;
const VF_CURVE_VOLTAGE_LIMIT: usize = 7;
const VF_CURVE_FREQUENCY_LIMIT: usize = 8;

// `ctl_units_t`.
const UNITS_MHZ: i32 = 0;
const UNITS_PERCENT: i32 = 11;
const UNITS_GBPS: i32 = 12;
const UNITS_MILLIVOLTS: i32 = 13;

/// PCI device id of the Arc Pro B60 (the validated card).
const B60_DEVICE_ID: i32 = 0xE211;

const POLL_ATTEMPTS: usize = 10;
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One `ctl_oc_control_info_t`, with malformed limits folded into `supported = false`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Control {
    pub(crate) supported: bool,
    pub(crate) units: i32,
    pub(crate) min: f64,
    pub(crate) max: f64,
    pub(crate) step: f64,
    pub(crate) default: f64,
}

impl Control {
    pub(crate) fn read(b: &[u8], index: usize) -> Control {
        let at = 8 + index * igcl::OC_CONTROL_STRIDE;
        let c = Control {
            supported: b.get(at).is_some_and(|&v| v != 0),
            units: igcl::int(b, at + 4),
            min: igcl::f64_at(b, at + 8),
            max: igcl::f64_at(b, at + 16),
            step: igcl::f64_at(b, at + 24),
            default: igcl::f64_at(b, at + 32),
        };
        Control {
            supported: c.supported
                && c.min.is_finite()
                && c.max.is_finite()
                && c.min <= c.max
                && c.step > 0.0
                && c.step.is_finite()
                && c.default.is_finite(),
            ..c
        }
    }

    /// Reject anything outside the driver's range or off its step grid; the value is returned
    /// unchanged (never snapped).
    pub(crate) fn validate(&self, value: f64) -> R<f64> {
        if !self.supported || !value.is_finite() || value < self.min || value > self.max {
            fail!("Intel value must be within the supported range {}–{}.", self.min, self.max);
        }
        let snapped = self.min + round_even((value - self.min) / self.step) * self.step;
        if (snapped - value).abs() > 0.00001 {
            fail!("Intel value must use steps of {} from {}.", self.step, self.min);
        }
        Ok(value)
    }
}

struct Card {
    handle: CtlHandle,
    core_frequency: CtlHandle,
    frequency_min: i32,
    frequency_max: i32,
    fans: Vec<CtlHandle>,
    memory: Vec<CtlHandle>,
    temperatures: Vec<(CtlHandle, i32)>,
    controls: Vec<Control>,
    caps: GpuCapabilities,
    graphics: GpuGraphicsInfo,
    // Previous cumulative counters, for rates between telemetry samples.
    gpu_energy: f64,
    vram_energy: f64,
    render_activity: f64,
    media_activity: f64,
    time: f64,
    energy: f64,
    activity: f64,
}

impl Card {
    fn control(&self, index: usize) -> Control {
        self.controls.get(index).copied().unwrap_or(Control {
            supported: false,
            units: -1,
            min: f64::NAN,
            max: f64::NAN,
            step: f64::NAN,
            default: f64::NAN,
        })
    }
}

/// Intel Arc backend over IGCL (`ControlLib.dll`).
pub struct IntelBackend {
    api: Option<Igcl>,
    context: CtlHandle,
    cards: Vec<Card>,
    devices: Vec<GpuDevice>,
}

impl Default for IntelBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl IntelBackend {
    /// An uninitialised backend; `initialize()` loads `ControlLib.dll` and enumerates cards.
    pub fn new() -> Self {
        IntelBackend { api: None, context: CtlHandle::NULL, cards: Vec::new(), devices: Vec::new() }
    }

    fn parts(&mut self, gpu: usize) -> R<(&Igcl, &mut Card)> {
        let Some(card) = self.cards.get_mut(gpu) else { fail!("Intel GPU index {gpu} is out of range.") };
        let Some(api) = self.api.as_ref() else { fail!("Intel API is not initialized.") };
        Ok((api, card))
    }

    fn initialize_impl(&mut self) -> roch_core::Result<()> {
        if !self.cards.is_empty() {
            return Ok(());
        }
        if self.api.is_some() {
            roch_core::bail!("Intel API initialization previously failed.");
        }
        // Only a successful load counts as an attempt: a missing DLL may be retried.
        self.api = Some(Igcl::load()?);
        self.enumerate().map_err(Into::into)
    }

    fn enumerate(&mut self) -> R<()> {
        let Some(api) = self.api.as_ref() else { fail!("Intel API is not initialized.") };
        let init_fn = api.init_fn()?;
        let mut init = Buf::data(igcl::INIT_ARGS_SIZE, 0);
        init.put(8, igcl::INIT_APP_VERSION);
        init.put(12, igcl::INIT_FLAGS_WITH_FIRMWARE);
        let mut context = ptr::null_mut();
        // SAFETY: `init` is a sized ctl_init_args_t; `context` receives the API handle.
        let result = unsafe { init_fn(init.as_mut_ptr(), &mut context) };
        if result != 0 {
            // Older runtimes may not support firmware discovery. Keep tuning available.
            init.put(12, igcl::INIT_FLAGS_BASIC);
            // SAFETY: as above.
            let retry = unsafe { init_fn(init.as_mut_ptr(), &mut context) };
            self.context = CtlHandle(context);
            check(retry, "initialize")?;
        }
        self.context = CtlHandle(context);

        for h in api.handles("ctlEnumerateDevices", self.context)? {
            let mut props = Buf::data(igcl::DEVICE_PROPERTIES_SIZE, igcl::DEVICE_PROPERTIES_VERSION);
            // pOsDeviceID@8 must point at a LUID-sized buffer, device_id_size@16 = 8.
            let mut luid = [0u64; 1];
            props.put_u64(8, luid.as_mut_ptr() as u64);
            props.put(16, 8);
            let status = api.call_buffer("ctlGetDeviceProperties", h, &mut props)?;
            props.put_u64(8, 0); // `luid` is gone after this scope; never keep a dangling pointer.
            check(status, "device properties")?;
            // Graphics device type, Intel vendor, not integrated.
            if props.int(20) != 1 || props.int(64) != 0x8086 || (props.int(188) & 1) != 0 {
                continue;
            }
            let card = probe_card(api, h, &props, self.cards.len())?;
            self.devices.push(card.0);
            self.cards.push(card.1);
        }
        if self.cards.is_empty() {
            fail!("No discrete Intel Arc GPU found.");
        }
        Ok(())
    }

    fn set_scalar(&mut self, gpu: usize, control: usize, name: &str, value: f64) -> R<()> {
        let (api, c) = self.parts(gpu)?;
        c.control(control).validate(value)?;
        if (get(api, c, name)? - value).abs() < 0.00001 {
            return Ok(());
        }
        waiver(api, c)?;
        api.set_double(&format!("ctlOverclock{name}SetV2"), c.handle, value, &format!("{name} write"))?;
        let actual = get(api, c, name)?;
        if (actual - value).abs() > 0.00001 {
            fail!("Intel {name} readback mismatch: requested {value}, got {actual}.");
        }
        Ok(())
    }

    fn set_clock_range_impl(&mut self, gpu: usize, min_mhz: i32, max_mhz: i32) -> R<()> {
        let (api, c) = self.parts(gpu)?;
        if !c.caps.can_lock_clocks {
            fail!("Intel core clock range control is unavailable.");
        }
        let mut request = clock_range_request(min_mhz, max_mhz, c.frequency_min, c.frequency_max)?;
        let before = read_frequency_range(api, c.core_frequency)?;
        let (lo, hi) = (request.f64(8), request.f64(16));
        if (before.0 - lo).abs() < 0.5 && (before.1 - hi).abs() < 0.5 {
            return Ok(());
        }
        let attempt = (|| -> R<()> {
            check(api.call_buffer("ctlFrequencySetRange", c.core_frequency, &mut request)?, "core clock range")?;
            let actual = read_frequency_range(api, c.core_frequency)?;
            // The -1/-1 factory sentinel reads back as the factory range, not -1.
            if lo >= 0.0 && ((actual.0 - lo).abs() > 0.5 || (actual.1 - hi).abs() > 0.5) {
                fail!("Intel clock range readback mismatch: requested {lo}–{hi}, got {}–{} MHz.", actual.0, actual.1);
            }
            Ok(())
        })();
        let Err(Fault::Backend(message)) = attempt else { return attempt };
        // Restore and verify the previous range, then report the original failure.
        let mut restore = Buf::data(igcl::FREQ_RANGE_SIZE, 0);
        restore.put_f64(8, before.0);
        restore.put_f64(16, before.1);
        let status = api.call_buffer("ctlFrequencySetRange", c.core_frequency, &mut restore)?;
        if status != 0 {
            fail!("{message} Restoring the previous clock range also failed (0x{status:08X}).");
        }
        let restored = read_frequency_range(api, c.core_frequency)?;
        if (restored.0 - before.0).abs() > 0.5 || (restored.1 - before.1).abs() > 0.5 {
            fail!("{message} Previous clock range could not be verified after restore.");
        }
        Err(Fault::Backend(message))
    }

    fn read_tuning_state_impl(&mut self, gpu: usize) -> R<GpuTuningState> {
        let (api, c) = self.parts(gpu)?;
        let value = |control: usize, name: &str, scale: f64| -> R<i32> {
            Ok(if c.control(control).supported { round_even_i32(get(api, c, name)? * scale) } else { 0 })
        };
        let mut mode = FanMode::Auto;
        let mut percent = 0;
        let mut fan_curve = None;
        if let Some(&fan) = c.fans.first() {
            let cfg = api.read(fan, "ctlFanGetConfig", igcl::FAN_CONFIG_SIZE, 0)?;
            mode = match cfg.int(8) {
                1 => FanMode::Fixed,
                2 => FanMode::Curve,
                _ => FanMode::Auto,
            };
            if mode == FanMode::Fixed && cfg.int(24) == 1 {
                percent = cfg.int(20);
            }
            if mode == FanMode::Curve {
                let count = cfg.int(36);
                if (1..=32).contains(&count) {
                    let mut points = Vec::new();
                    for j in 0..count as usize {
                        let at = 40 + igcl::FAN_POINT_SIZE * j;
                        if cfg.int(at + 24) != 1 {
                            points.clear();
                            break;
                        }
                        points.push(FanPoint::new(cfg.int(at + 8) as f64, cfg.int(at + 20) as f64));
                    }
                    if let Some(first) = points.first().copied() {
                        // A flat table is how Fixed duty is written; read it back as Fixed.
                        if points.iter().all(|p| p.fan_percent == first.fan_percent) {
                            mode = FanMode::Fixed;
                            percent = first.fan_percent as i32;
                        }
                        fan_curve = Some(FanCurve::with_points(points));
                    }
                }
            }
        }
        let clock_range =
            if c.caps.can_lock_clocks { read_frequency_range(api, c.core_frequency)? } else { (0.0, 0.0) };
        Ok(GpuTuningState {
            locked_clock_min_mhz: round_even_i32(clock_range.0),
            locked_clock_max_mhz: round_even_i32(clock_range.1),
            core_offset_mhz: value(GPU_FREQUENCY_OFFSET, "GpuFrequencyOffset", 1.0)?,
            memory_offset_mhz: value(VRAM_MEM_SPEED_LIMIT, "VramMemSpeedLimit", 1000.0)?,
            voltage_boost_percent: value(GPU_VOLTAGE_OFFSET, "GpuMaxVoltageOffset", 1.0)?,
            power_limit_percent: value(POWER_LIMIT, "PowerLimit", 1.0)?,
            temp_limit_c: value(TEMPERATURE_LIMIT, "TemperatureLimit", 1.0)?,
            fan_manual: mode == FanMode::Fixed,
            fan_percent: percent,
            detected_fan_mode: Some(mode),
            hardware_fan_curve: fan_curve,
            ..GpuTuningState::default()
        })
    }

    fn read_vf_curve_impl(&mut self, gpu: usize) -> R<Vec<VfCurveSample>> {
        let (api, c) = self.parts(gpu)?;
        let stock = curve(api, c.handle, igcl::VF_CURVE_STOCK)?;
        let live = curve(api, c.handle, igcl::VF_CURVE_LIVE)?;
        if stock.len() != live.len() {
            fail!("Intel stock and live curve counts differ; reload the curve after the driver settles.");
        }
        Ok(stock
            .iter()
            .zip(&live)
            .enumerate()
            .map(|(j, (p, l))| VfCurveSample::new(j as i32, l.voltage as i32, p.frequency as i32, l.frequency as i32))
            .collect())
    }

    fn set_vf_curve_targets_impl(&mut self, gpu: usize, targets: &[VfCurveSample]) -> R<()> {
        let (api, c) = self.parts(gpu)?;
        if !c.caps.can_edit_vf_curve {
            fail!("Intel V/F editing is unavailable.");
        }
        let mut points = curve(api, c.handle, igcl::VF_CURVE_STOCK)?;
        let mut seen = HashSet::new();
        for t in targets {
            let Ok(index) = usize::try_from(t.index) else { fail!("Invalid Intel curve point index or voltage.") };
            if index >= points.len() || !seen.insert(index) {
                fail!("Invalid Intel curve point index or voltage.");
            }
            // IGCL's voltage coordinates move with operating conditions. This editor changes
            // frequencies by index, so use freshly read voltage coordinates.
            c.control(VF_CURVE_FREQUENCY_LIMIT).validate(t.live_mhz as f64)?;
            points[index].frequency = t.live_mhz as u32;
        }
        let stock = curve(api, c.handle, igcl::VF_CURVE_STOCK)?;
        if points.len() == stock.len() && same_frequencies(&points, &stock) {
            // A zero global offset clears a custom table without re-interpolating a stale voltage
            // grid. Writing stock points directly can shift frequencies.
            waiver(api, c)?;
            api.set_double("ctlOverclockGpuFrequencyOffsetSetV2", c.handle, 0.0, "clear custom V/F curve")?;
            for _ in 0..POLL_ATTEMPTS {
                thread::sleep(POLL_INTERVAL);
                if same_frequencies(&curve(api, c.handle, igcl::VF_CURVE_LIVE)?, &stock) {
                    return Ok(());
                }
            }
            fail!("Intel could not confirm that the custom curve was cleared.");
        }
        for j in 1..points.len() {
            if points[j].voltage <= points[j - 1].voltage || points[j].frequency < points[j - 1].frequency {
                fail!("Intel curve frequencies must not decrease as voltage increases.");
            }
        }
        let before = curve(api, c.handle, igcl::VF_CURVE_LIVE)?;
        if points == before {
            return Ok(());
        }
        waiver(api, c)?;
        let write = api.write_curve_fn()?;
        // SAFETY: `points` holds exactly `len` ctl_voltage_frequency_point_t entries.
        let status = unsafe { write(c.handle.0, points.len() as u32, points.as_mut_ptr()) };
        check(status, "write V/F curve")?;
        // IGCL accepts the request before firmware publishes the new curve. Immediate reads can
        // return the old curve or a transient point count. Bound the wait.
        for attempt in 0..POLL_ATTEMPTS {
            thread::sleep(POLL_INTERVAL);
            match curve(api, c.handle, igcl::VF_CURVE_LIVE) {
                Ok(live) if same_frequencies(&points, &live) => return Ok(()),
                Ok(_) => {}
                Err(Fault::Backend(_)) if attempt < POLL_ATTEMPTS - 1 => {}
                Err(e) => return Err(e),
            }
        }
        fail!("Intel adjusted or did not apply the requested V/F curve. Reload to see the actual points.");
    }

    fn set_fan_curve_impl(&mut self, gpu: usize, fan_curve: &FanCurve) -> R<()> {
        let (api, c) = self.parts(gpu)?;
        if !c.caps.can_set_fan_speed {
            fail!("This Intel driver does not advertise percentage fan-curve writes.");
        }
        let mut table = build_fan_table(fan_curve, c.caps.fan_curve_points)?;
        let point_count = table.int(8);
        waiver(api, c)?;
        for &fan in &c.fans {
            check(api.call_buffer("ctlFanSetSpeedTableMode", fan, &mut table)?, "fan curve write")?;
            let mut verified = false;
            for _ in 0..POLL_ATTEMPTS {
                if verified {
                    break;
                }
                thread::sleep(POLL_INTERVAL);
                let cfg = api.read(fan, "ctlFanGetConfig", igcl::FAN_CONFIG_SIZE, 0)?;
                verified = cfg.int(8) == 2
                    && cfg.int(36) == point_count
                    && (0..point_count.max(0) as usize).all(|j| {
                        let k = j * igcl::FAN_POINT_SIZE;
                        cfg.int(48 + k) == table.int(20 + k)
                            && cfg.int(60 + k) == table.int(32 + k)
                            && cfg.int(64 + k) == 1
                    });
            }
            if !verified {
                fail!("Intel fan curve readback mismatch. Reload to see the actual curve.");
            }
        }
        Ok(())
    }

    fn set_fan_auto_impl(&mut self, gpu: usize) -> R<()> {
        let (api, c) = self.parts(gpu)?;
        if !c.caps.can_set_fan_speed {
            fail!("Intel fan control is unavailable through this driver.");
        }
        for &fan in &c.fans {
            check(api.call_handle("ctlFanSetDefaultMode", fan)?, "restore automatic fans")?;
            let mut verified = false;
            for _ in 0..POLL_ATTEMPTS {
                if verified {
                    break;
                }
                thread::sleep(POLL_INTERVAL);
                verified = api.read(fan, "ctlFanGetConfig", igcl::FAN_CONFIG_SIZE, 0)?.int(8) == 0;
            }
            if !verified {
                fail!("Intel automatic fan mode did not pass readback verification.");
            }
        }
        Ok(())
    }

    fn reset_to_defaults_impl(&mut self, gpu: usize) -> R<()> {
        let (api, c) = self.parts(gpu)?;
        waiver(api, c)?;
        check(api.call_handle("ctlOverclockResetToDefault", c.handle)?, "restore tuning defaults")?;
        let (lock, fans) = (c.caps.can_lock_clocks, c.caps.can_set_fan_speed);
        if lock {
            self.set_clock_range_impl(gpu, 0, 0)?;
        }
        if fans {
            self.set_fan_auto_impl(gpu)?;
        }
        Ok(())
    }

    fn read_telemetry_impl(&mut self, gpu: usize) -> R<GpuTelemetry> {
        let (api, c) = self.parts(gpu)?;
        let b =
            api.read(c.handle, "ctlPowerTelemetryGetV2", igcl::POWER_TELEMETRY_SIZE, igcl::POWER_TELEMETRY_VERSION)?;
        let b = b.bytes();
        let (time, energy, activity) = (sensor(b, 8), sensor(b, 376), sensor(b, 128));
        let dt = time - c.time;
        let power = rate(energy, c.energy, dt);
        let load = rate(activity, c.activity, dt) * 100.0;
        c.time = time;
        c.energy = energy;
        c.activity = activity;
        let fans: Vec<f64> = (0..5).map(|j| sensor(b, 680 + j * 24)).filter(|v| v.is_finite()).collect();

        let (mut used, mut available, mut memory_read) = (0.0, 0.0, false);
        for &m in &c.memory {
            if let Some(state) = swallow(api.read(m, "ctlMemoryGetState", igcl::MEMORY_STATE_SIZE, 0))? {
                let (free, total) = (state.u64(8), state.u64(16));
                if total >= free {
                    used += (total - free) as f64 / 1048576.0;
                    available += free as f64 / 1048576.0;
                    memory_read = true;
                }
            }
        }

        let mut extras = Vec::new();
        let extra = |extras: &mut Vec<SupplementalSensor>, offset: usize, key: &str, name: &str, unit: &str| {
            let v = sensor(b, offset);
            if v.is_finite() {
                extras.push(SupplementalSensor::new(key, name, unit, v));
            }
        };
        // A counter is listed once it reads, even before a second sample makes its rate known.
        let counter = |extras: &mut Vec<SupplementalSensor>,
                       offset: usize,
                       previous: &mut f64,
                       key: &str,
                       name: &str,
                       unit: &str,
                       scale: f64| {
            let current = sensor(b, offset);
            let value = rate(current, *previous, dt) * scale;
            *previous = current;
            if current.is_finite() {
                extras.push(SupplementalSensor::new(key, name, unit, value));
            }
        };
        counter(&mut extras, 32, &mut c.gpu_energy, "intel-gpu-power", "GPU power", "W", 1.0);
        counter(&mut extras, 208, &mut c.vram_energy, "intel-vram-power", "Memory power", "W", 1.0);
        counter(&mut extras, 152, &mut c.render_activity, "intel-render-load", "Render / compute", "%", 100.0);
        counter(&mut extras, 176, &mut c.media_activity, "intel-media-load", "Media engine", "%", 100.0);
        extra(&mut extras, 848, "intel-sa-vr", "SA voltage regulator", "°C");
        extra(&mut extras, 232, "intel-memory-voltage", "Memory voltage", "V");
        if memory_read {
            extras.push(SupplementalSensor::new("intel-memory-free", "Available VRAM", "MB", available));
            if used + available > 0.0 {
                extras.push(SupplementalSensor::new(
                    "intel-memory-usage",
                    "VRAM usage",
                    "%",
                    used / (used + available) * 100.0,
                ));
            }
        }
        const LIMITS: [(usize, &str, &str); 5] = [
            (200, "power", "Power"),
            (201, "thermal", "Thermal"),
            (202, "current", "Current"),
            (203, "voltage", "Voltage"),
            (204, "utilization", "Utilization"),
        ];
        let flag = |at: usize| b.get(at).is_some_and(|&v| v != 0);
        for (offset, key, name) in LIMITS {
            extras.push(SupplementalSensor::new(
                format!("intel-limit-{key}"),
                format!("{name} limit active"),
                "state",
                if flag(offset) { 1.0 } else { 0.0 },
            ));
        }
        for &(handle, kind) in &c.temperatures {
            let name = match kind {
                0 => "GPU global",
                1 => "GPU core",
                _ => continue,
            };
            if let Some(t) = swallow(api.get_double("ctlTemperatureGetState", handle, "temperature"))?
                && t.is_finite()
            {
                extras.push(SupplementalSensor::new(format!("intel-temperature-{kind}"), name, "°C", t));
            }
        }
        let pcie = (|| -> R<(i32, i32)> {
            let mut pci = Buf::data(igcl::PCI_STATE_SIZE, 0);
            pci.put(8, igcl::PCI_NESTED_SIZE);
            check(api.call_buffer("ctlPciGetState", c.handle, &mut pci)?, "PCIe state")?;
            Ok((pci.int(16), pci.int(20)))
        })();
        if let Some((generation, width)) = swallow(pcie)? {
            let speed = match generation {
                1 => 2.5,
                2 => 5.0,
                3 => 8.0,
                4 => 16.0,
                5 => 32.0,
                6 => 64.0,
                _ => f64::NAN,
            };
            if speed.is_finite() {
                extras.push(SupplementalSensor::new("intel-pcie-speed", "PCIe link speed", "GT/s", speed));
            }
            if width > 0 {
                extras.push(SupplementalSensor::new("intel-pcie-width", "PCIe link width", "lanes", width as f64));
            }
        }
        extra(&mut extras, 872, "intel-effective-clock", "GPU core (effective)", "MHz");
        extra(&mut extras, 800, "intel-gpu-vr", "GPU voltage regulator", "°C");
        extra(&mut extras, 824, "intel-vram-vr", "Memory voltage regulator", "°C");
        extra(&mut extras, 968, "intel-memory-read", "Memory read bandwidth", "MB/s");
        extra(&mut extras, 992, "intel-memory-write", "Memory write bandwidth", "MB/s");

        let limit_reason = LIMITS
            .iter()
            .filter(|(offset, _, _)| flag(*offset))
            .map(|(_, _, name)| *name)
            .collect::<Vec<_>>()
            .join(" / ");
        Ok(GpuTelemetry {
            core_clock_mhz: sensor(b, 80),
            memory_clock_mhz: sensor(b, 256),
            temperature_c: sensor(b, 104),
            voltage_mv: sensor(b, 56) * 1000.0,
            hot_spot_c: f64::NAN,
            memory_temperature_c: sensor(b, 352),
            power_watts: power,
            power_percent: sensor(b, 920),
            gpu_load_percent: if load.is_finite() { load.clamp(0.0, 100.0) } else { f64::NAN },
            memory_load_percent: f64::NAN,
            memory_used_mb: if memory_read { used } else { f64::NAN },
            fan_rpm: fans.first().copied().unwrap_or(f64::NAN),
            fan_rpms: fans,
            fan_percent: f64::NAN,
            supplemental_sensors: extras,
            limit_reason,
            ..GpuTelemetry::default()
        })
    }
}

impl Drop for IntelBackend {
    fn drop(&mut self) {
        if let Some(api) = self.api.take() {
            if !self.context.is_null() {
                let _ = api.call_handle("ctlClose", self.context);
                self.context = CtlHandle::NULL;
            }
            drop(api);
        }
        self.cards.clear();
        self.devices.clear();
    }
}

/// Everything discovered about one discrete Arc card. Read-only: no tuning call is made here.
fn probe_card(api: &Igcl, h: CtlHandle, props: &Buf, index: usize) -> R<(GpuDevice, Card)> {
    let oc = api.read(h, "ctlOverclockGetProperties", igcl::OC_PROPERTIES_SIZE, igcl::OC_PROPERTIES_VERSION)?;
    let controls: Vec<Control> = (0..igcl::OC_CONTROL_COUNT).map(|i| Control::read(oc.bytes(), i)).collect();
    let has = |i: usize, units: i32| oc.byte(5) != 0 && controls[i].supported && controls[i].units == units;
    let fans = swallow(api.handles("ctlEnumFans", h))?.unwrap_or_default();
    let memory = swallow(api.handles("ctlEnumMemoryModules", h))?.unwrap_or_default();
    let temperatures = swallow((|| -> R<Vec<(CtlHandle, i32)>> {
        api.handles("ctlEnumTemperatureSensors", h)?
            .into_iter()
            .map(|t| Ok((t, api.read(t, "ctlTemperatureGetProperties", igcl::TEMP_PROPERTIES_SIZE, 0)?.int(8))))
            .collect()
    })())?
    .unwrap_or_default();

    let mut vram: i64 = 0;
    let mut memory_properties = Vec::new();
    for &m in &memory {
        if let Some(p) = swallow(api.read(m, "ctlMemoryGetProperties", igcl::MEMORY_PROPERTIES_SIZE, 0))? {
            if p.int(12) != 1 {
                continue; // Device-local memory only.
            }
            vram = vram.wrapping_add((p.u64(16) / 1048576) as i64);
            memory_properties.push(p);
        }
    }

    let mut fan_control = !fans.is_empty();
    let mut fan_points = 32;
    for &fan in &fans {
        let p = api.read(fan, "ctlFanGetProperties", igcl::FAN_PROPERTIES_SIZE, 0)?;
        // B60 32.0.101.9030 advertises RPM/fixed only, yet exposes and accepts percent tables
        // (verified on hardware). An existing percent table is also evidence of support; no
        // probing writes during initialization.
        let cfg = api.read(fan, "ctlFanGetConfig", igcl::FAN_CONFIG_SIZE, 0)?;
        let count = cfg.int(36);
        let percent_table = cfg.int(8) == 2
            && (2..=32).contains(&count)
            && (0..count as usize).all(|j| cfg.int(64 + j * igcl::FAN_POINT_SIZE) == 1);
        let b60_table = props.int(68) == B60_DEVICE_ID && p.int(20) == 10;
        // Table mode + percent units, or the evidence above.
        fan_control &= p.byte(5) != 0 && (((p.int(8) & 4) != 0 && (p.int(12) & 2) != 0) || percent_table || b60_table);
        fan_points = fan_points.min(p.int(20));
    }

    let curve_editable = swallow((|| -> R<bool> {
        Ok(has(VF_CURVE_VOLTAGE_LIMIT, UNITS_MILLIVOLTS)
            && has(VF_CURVE_FREQUENCY_LIMIT, UNITS_MHZ)
            && !curve(api, h, igcl::VF_CURVE_STOCK)?.is_empty()
            && !curve(api, h, igcl::VF_CURVE_LIVE)?.is_empty())
    })())?
    .unwrap_or(false);

    let (mut core_frequency, mut frequency_min, mut frequency_max) = (CtlHandle::NULL, 0, 0);
    swallow((|| -> R<()> {
        for frequency in api.handles("ctlEnumFrequencyDomains", h)? {
            let info = api.read(frequency, "ctlFrequencyGetProperties", igcl::FREQ_PROPERTIES_SIZE, 0)?;
            let (lo, hi) = (info.f64(16), info.f64(24));
            // GPU domain, controllable, sane limits.
            if info.int(8) != 0
                || info.byte(12) == 0
                || !lo.is_finite()
                || !hi.is_finite()
                || lo <= 0.0
                || hi < lo
                || hi > 100000.0
            {
                continue;
            }
            read_frequency_range(api, frequency)?; // A usable control must also support readback.
            core_frequency = frequency;
            frequency_min = lo.ceil() as i32;
            frequency_max = hi.floor() as i32;
            break;
        }
        Ok(())
    })())?;

    let ctl = |i: usize| controls[i];
    let caps = GpuCapabilities {
        can_lock_clocks: !core_frequency.is_null(),
        clock_lock_min_mhz: frequency_min,
        clock_lock_max_mhz: frequency_max,
        can_set_core_offset: has(GPU_FREQUENCY_OFFSET, UNITS_MHZ),
        core_offset_min_mhz: ctl(GPU_FREQUENCY_OFFSET).min as i32,
        core_offset_max_mhz: ctl(GPU_FREQUENCY_OFFSET).max as i32,
        core_offset_step_mhz: (ctl(GPU_FREQUENCY_OFFSET).step as i32).max(1),
        // Memory is an absolute speed limit in Gbps; the UI works in Mbps.
        can_set_memory_offset: has(VRAM_MEM_SPEED_LIMIT, UNITS_GBPS),
        memory_clock_is_absolute: true,
        memory_clock_unit: "Mbps".into(),
        memory_offset_min_mhz: round_even_i32(ctl(VRAM_MEM_SPEED_LIMIT).min * 1000.0),
        memory_offset_max_mhz: round_even_i32(ctl(VRAM_MEM_SPEED_LIMIT).max * 1000.0),
        memory_clock_default_mhz: round_even_i32(ctl(VRAM_MEM_SPEED_LIMIT).default * 1000.0),
        can_set_power_limit: has(POWER_LIMIT, UNITS_PERCENT),
        power_limit_min_percent: ctl(POWER_LIMIT).min as i32,
        power_limit_max_percent: ctl(POWER_LIMIT).max as i32,
        power_limit_default_percent: ctl(POWER_LIMIT).default as i32,
        // Temperature limit is a percentage of thermal margin, not °C.
        can_set_temp_limit: has(TEMPERATURE_LIMIT, UNITS_PERCENT),
        temp_limit_is_percent: true,
        temp_limit_min_c: ctl(TEMPERATURE_LIMIT).min as i32,
        temp_limit_max_c: ctl(TEMPERATURE_LIMIT).max as i32,
        temp_limit_default_c: ctl(TEMPERATURE_LIMIT).default as i32,
        voltage_style: VoltageControlStyle::Percent,
        can_set_voltage_boost: has(GPU_VOLTAGE_OFFSET, UNITS_PERCENT),
        can_read_voltage: true,
        voltage_boost_min_percent: ctl(GPU_VOLTAGE_OFFSET).min as i32,
        voltage_boost_max_percent: ctl(GPU_VOLTAGE_OFFSET).max as i32,
        voltage_boost_default_percent: ctl(GPU_VOLTAGE_OFFSET).default as i32,
        can_edit_vf_curve: curve_editable,
        vf_curve_step_mhz: (ctl(VF_CURVE_FREQUENCY_LIMIT).step as i32).max(1),
        vf_curve_min_mhz: ctl(VF_CURVE_FREQUENCY_LIMIT).min as i32,
        vf_curve_max_mhz: ctl(VF_CURVE_FREQUENCY_LIMIT).max as i32,
        min_voltage_mv: ctl(VF_CURVE_VOLTAGE_LIMIT).min as i32,
        max_voltage_mv: ctl(VF_CURVE_VOLTAGE_LIMIT).max as i32,
        can_set_fan_speed: fan_control && fan_points >= 2,
        fan_curve_is_hardware: true,
        can_read_hardware_fan_curve: true,
        fan_curve_points: clamp_i32(fan_points, 0, 32),
        fan_count: fans.len() as i32,
        fan_curve_min_temp_c: 25,
        fan_curve_max_temp_c: 100,
        ..GpuCapabilities::default()
    };

    let driver = props.u64(32);
    let name =
        String::from_utf8_lossy(props.bytes().get(88..188).unwrap_or_default()).trim_end_matches('\0').to_string();
    let version =
        format!("{}.{}.{}.{}", driver >> 48, (driver >> 32) & 0xffff, (driver >> 16) & 0xffff, driver & 0xffff);
    let mut bios = String::new();
    // Firmware discovery is optional on older runtimes: both a failure and a missing export are
    // tolerated here.
    let firmware = (|| -> R<()> {
        for fw in api.handles("ctlEnumerateFirmwareComponents", h)? {
            let candidate = option_rom_version(
                api.read(fw, "ctlGetFirmwareComponentProperties", igcl::FIRMWARE_PROPERTIES_SIZE, 0)?.bytes(),
            );
            if !candidate.is_empty() {
                bios = candidate;
                break;
            }
        }
        Ok(())
    })();
    let _ = firmware;

    let mut device = GpuDevice {
        index,
        name,
        vendor: "Intel".into(),
        bus_id: String::new(),
        driver_version: version,
        vram_megabytes: vram,
        bios_version: bios,
    };
    let memory_refs: Vec<&[u8]> = memory_properties.iter().map(Buf::bytes).collect();
    let mut graphics = decode_graphics(&device, props.bytes(), &memory_refs);
    let pci = (|| -> R<Buf> {
        let mut pci = Buf::data(igcl::PCI_PROPERTIES_SIZE, 0);
        pci.put(8, igcl::PCI_NESTED_SIZE);
        pci.put(32, igcl::PCI_NESTED_SIZE);
        check(api.call_buffer("ctlPciGetProperties", h, &mut pci)?, "PCIe properties")?;
        Ok(pci)
    })();
    if let Some(pci) = swallow(pci)? {
        let (interface, address, rebar) = decode_pci(pci.bytes());
        device.bus_id = address.clone();
        graphics.bus_address = address;
        graphics.bus_interface = interface;
        graphics.resizable_bar = rebar;
    }

    let card = Card {
        handle: h,
        core_frequency,
        frequency_min,
        frequency_max,
        fans,
        memory,
        temperatures,
        controls,
        caps,
        graphics,
        gpu_energy: f64::NAN,
        vram_energy: f64::NAN,
        render_activity: f64::NAN,
        media_activity: f64::NAN,
        time: f64::NAN,
        energy: f64::NAN,
        activity: f64::NAN,
    };
    Ok((device, card))
}

/// `ctlOverclock{name}GetV2`, rejecting non-finite values.
fn get(api: &Igcl, c: &Card, name: &str) -> R<f64> {
    let value = api.get_double(&format!("ctlOverclock{name}GetV2"), c.handle, &format!("{name} read"))?;
    if !value.is_finite() {
        fail!("Intel returned a non-finite tuning value.");
    }
    Ok(value)
}

/// IGCL's tuning waiver; set only immediately before a write.
fn waiver(api: &Igcl, c: &Card) -> R<()> {
    check(api.call_handle("ctlOverclockWaiverSet", c.handle)?, "enable tuning")
}

fn read_frequency_range(api: &Igcl, handle: CtlHandle) -> R<(f64, f64)> {
    let data = api.read(handle, "ctlFrequencyGetRange", igcl::FREQ_RANGE_SIZE, 0)?;
    let (lo, hi) = (data.f64(8), data.f64(16));
    if !lo.is_finite() || !hi.is_finite() || lo < 0.0 || hi < lo {
        fail!("Intel did not report a usable core clock range.");
    }
    Ok((lo, hi))
}

/// Read one V/F curve (stock or live) with the two-call count/fill pattern.
fn curve(api: &Igcl, h: CtlHandle, kind: i32) -> R<Vec<Point>> {
    let f = api.read_curve_fn()?;
    let mut count = 0u32;
    // SAFETY: a null table with a count pointer asks for the point count.
    check(unsafe { f(h.0, kind, igcl::VF_CURVE_SIMPLIFIED, &mut count, ptr::null_mut()) }, "V/F curve count")?;
    if count == 0 {
        return Ok(Vec::new());
    }
    if count > 1024 {
        fail!("Intel returned an invalid curve size.");
    }
    let mut points = vec![Point::default(); count as usize];
    // SAFETY: `points` has room for `count` entries.
    check(unsafe { f(h.0, kind, igcl::VF_CURVE_SIMPLIFIED, &mut count, points.as_mut_ptr()) }, "read V/F curve")?;
    if count as usize != points.len() {
        fail!("Intel V/F curve changed while reading.");
    }
    Ok(points)
}

fn same_frequencies(a: &[Point], b: &[Point]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.frequency == y.frequency)
}

/// Decode `ctl_pci_properties_t` (x64, nested Size/Version headers) → (interface, address, ReBAR).
pub(crate) fn decode_pci(pci: &[u8]) -> (String, String, String) {
    let interface = GpuIdentity::pcie_bus_interface(igcl::int(pci, 40), igcl::int(pci, 44), 0, 0);
    let address = format!(
        "{:04X}:{:02X}:{:02X}.{}",
        igcl::int(pci, 16),
        igcl::int(pci, 20),
        igcl::int(pci, 24),
        igcl::int(pci, 28)
    );
    let byte = |at: usize| pci.get(at).copied().unwrap_or(0);
    let rebar = if byte(56) == 0 {
        "Not supported"
    } else if byte(57) != 0 {
        "Enabled"
    } else {
        "Disabled"
    };
    (interface, address, rebar.to_string())
}

/// The VBIOS code version from a firmware component, or "" for anything else. GSC firmware and
/// OptionRomData are not the VBIOS code version.
pub(crate) fn option_rom_version(firmware: &[u8]) -> String {
    let field = |at: usize| roch_core::native::c_str(firmware.get(at..at + 64).unwrap_or_default());
    let (name, version) = (field(5), field(69));
    if name.eq_ignore_ascii_case("OptionRomCode")
        && !version.is_empty()
        && !version.eq_ignore_ascii_case("unknown")
        && !version.eq_ignore_ascii_case("Not Implemented")
    {
        version
    } else {
        String::new()
    }
}

/// Graphics-window identity from the public IGCL device v3 / memory properties ABI. Memory vendor
/// and ROP counts are not inferred.
pub(crate) fn decode_graphics(device: &GpuDevice, props: &[u8], memory: &[&[u8]]) -> GpuGraphicsInfo {
    let b60 = igcl::int(props, 68) == B60_DEVICE_ID;
    let cores = igcl::u32_at(props, 204);
    let memory_type = |t: i32| match t {
        0 => "HBM",
        1 => "DDR",
        2 => "DDR3",
        3 => "DDR4",
        4 => "DDR5",
        5 => "LPDDR",
        6 => "LPDDR3",
        7 => "LPDDR4",
        8 => "LPDDR5",
        9 => "GDDR4",
        10 => "GDDR5",
        11 => "GDDR5X",
        12 => "GDDR6",
        13 => "GDDR6X",
        14 => "GDDR7",
        _ => "",
    };
    let mut types: Vec<&str> = Vec::new();
    for t in memory.iter().map(|p| memory_type(igcl::int(p, 8))) {
        if !t.is_empty() && !types.contains(&t) {
            types.push(t);
        }
    }
    GpuGraphicsInfo {
        board_manufacturer: GpuIdentity::board_vendor(igcl::u16_at(props, 198) as i32),
        revision: format!("{:02X}", igcl::int(props, 72)),
        cores: if cores > 0 { format!("{cores} Xe cores") } else { String::new() },
        code_name: if b60 { "Battlemage BMG-G21 WKSTN".into() } else { String::new() },
        // Intel B60 specifications: intel.com/content/www/us/en/products/sku/243916/
        technology: if b60 { "5 nm (TSMC N5)".into() } else { String::new() },
        memory_type: types.join(" / "),
        // Avoid inventing an aggregate width when a driver exposes multiple modules.
        bus_width: match memory {
            [only] if igcl::int(only, 24) > 0 => format!("{} bit", igcl::int(only, 24)),
            _ => String::new(),
        },
        resizable_bar: "Unavailable".into(),
        ..GpuGraphicsInfo::from_device(device)
    }
}

/// Counter delta per second; NaN for a first sample, a counter reset or a non-positive interval.
pub(crate) fn rate(current: f64, previous: f64, elapsed: f64) -> f64 {
    if current.is_finite() && previous.is_finite() && elapsed.is_finite() && elapsed > 0.0 && current >= previous {
        (current - previous) / elapsed
    } else {
        f64::NAN
    }
}

/// `ctl_freq_range_t` for a core clock range request. 0/0 is the factory reset, sent as Intel's
/// -1/-1 sentinel (hardware maxima and factory defaults differ); a single 0 means "that end at the
/// supported limit".
pub(crate) fn clock_range_request(min_mhz: i32, max_mhz: i32, supported_min: i32, supported_max: i32) -> R<Buf> {
    let reset = min_mhz == 0 && max_mhz == 0;
    let lo = if min_mhz == 0 { supported_min } else { min_mhz };
    let hi = if max_mhz == 0 { supported_max } else { max_mhz };
    if !reset && (lo < supported_min || hi > supported_max || lo > hi) {
        fail!("Core clock range must be ordered and within {supported_min}–{supported_max} MHz.");
    }
    let mut data = Buf::data(igcl::FREQ_RANGE_SIZE, 0);
    data.put_f64(8, if reset { -1.0 } else { lo as f64 });
    data.put_f64(16, if reset { -1.0 } else { hi as f64 });
    Ok(data)
}

/// `ctl_fan_speed_table_t` in percent units. Rejects anything but distinct increasing
/// temperatures (25–100 °C) with non-decreasing duty (0–100 %): a falling duty can make the B60
/// driver report success while reverting to automatic mode.
pub(crate) fn build_fan_table(fan_curve: &FanCurve, max_points: i32) -> R<Buf> {
    let mut points = fan_curve.points.clone();
    // Stable, as LINQ's OrderBy.
    points.sort_by(|a, b| a.temperature_c.partial_cmp(&b.temperature_c).unwrap_or(std::cmp::Ordering::Equal));
    let invalid_point = |p: &FanPoint| {
        !p.temperature_c.is_finite()
            || !p.fan_percent.is_finite()
            || p.temperature_c < 25.0
            || p.temperature_c > 100.0
            || p.fan_percent < 0.0
            || p.fan_percent > 100.0
    };
    if points.len() < 2
        || points.len() as i64 > max_points.min(32) as i64
        || points.iter().any(invalid_point)
        || points.windows(2).any(|w| {
            round_even(w[0].temperature_c) >= round_even(w[1].temperature_c) || w[0].fan_percent > w[1].fan_percent
        })
    {
        fail!("Intel fan curves require increasing temperatures (25–100°C) and non-decreasing speeds (0–100%).");
    }
    let mut table = Buf::data(igcl::FAN_TABLE_SIZE, 0);
    table.put(8, points.len() as i32);
    for (j, p) in points.iter().enumerate() {
        // Preserve the zero nested headers used in the driver's own fan table.
        let at = 12 + j * igcl::FAN_POINT_SIZE;
        table.put(at + 8, round_even_i32(p.temperature_c));
        table.put(at + 20, round_even_i32(p.fan_percent));
        table.put(at + 24, 1); // CTL_FAN_SPEED_UNITS_PERCENT
    }
    Ok(table)
}

impl GpuBackend for IntelBackend {
    fn backend_name(&self) -> String {
        "Intel IGCL".into()
    }

    fn initialize(&mut self) -> roch_core::Result<()> {
        self.initialize_impl()
    }

    fn devices(&self) -> &[GpuDevice] {
        &self.devices
    }

    fn read_graphics_info(&mut self, gpu: usize) -> GpuGraphicsInfo {
        self.cards.get(gpu).map(|c| c.graphics.clone()).unwrap_or_default()
    }

    fn capabilities(&mut self, gpu: usize) -> roch_core::Result<GpuCapabilities> {
        match self.cards.get(gpu) {
            Some(c) => Ok(c.caps.clone()),
            None => roch_core::bail!("Intel GPU index {gpu} is out of range."),
        }
    }

    fn read_telemetry(&mut self, gpu: usize) -> roch_core::Result<GpuTelemetry> {
        Ok(self.read_telemetry_impl(gpu)?)
    }

    fn read_tuning_state(&mut self, gpu: usize) -> roch_core::Result<GpuTuningState> {
        Ok(self.read_tuning_state_impl(gpu)?)
    }

    fn set_core_offset(&mut self, gpu: usize, offset_mhz: i32) -> roch_core::Result<()> {
        Ok(self.set_scalar(gpu, GPU_FREQUENCY_OFFSET, "GpuFrequencyOffset", offset_mhz as f64)?)
    }

    /// Intel's memory control is an absolute speed limit: `offset_mhz` is the target in Mbps.
    fn set_memory_offset(&mut self, gpu: usize, offset_mhz: i32) -> roch_core::Result<()> {
        Ok(self.set_scalar(gpu, VRAM_MEM_SPEED_LIMIT, "VramMemSpeedLimit", offset_mhz as f64 / 1000.0)?)
    }

    fn set_power_limit(&mut self, gpu: usize, percent: i32) -> roch_core::Result<()> {
        Ok(self.set_scalar(gpu, POWER_LIMIT, "PowerLimit", percent as f64)?)
    }

    /// Intel's temperature limit is a percentage of thermal margin, not °C.
    fn set_temp_limit(&mut self, gpu: usize, celsius: i32) -> roch_core::Result<()> {
        Ok(self.set_scalar(gpu, TEMPERATURE_LIMIT, "TemperatureLimit", celsius as f64)?)
    }

    /// Intel's voltage limit, as a percentage of voltage headroom.
    fn set_voltage_boost(&mut self, gpu: usize, percent: i32) -> roch_core::Result<()> {
        Ok(self.set_scalar(gpu, GPU_VOLTAGE_OFFSET, "GpuMaxVoltageOffset", percent as f64)?)
    }

    fn set_voltage_curve_offset(
        &mut self,
        _gpu: usize,
        _offset_mv: i32,
        _extra_clock_mhz: i32,
    ) -> roch_core::Result<()> {
        roch_core::bail!("Use Intel's V/F curve editor or voltage limit percentage.")
    }

    fn read_voltage_lock_mv(&mut self, _gpu: usize) -> i32 {
        0
    }

    fn set_fan_curve(&mut self, gpu: usize, curve: &FanCurve) -> roch_core::Result<()> {
        Ok(self.set_fan_curve_impl(gpu, curve)?)
    }

    fn set_clock_range(&mut self, gpu: usize, min_mhz: i32, max_mhz: i32) -> roch_core::Result<()> {
        Ok(self.set_clock_range_impl(gpu, min_mhz, max_mhz)?)
    }

    /// Fixed duty is a flat hardware curve applied to every fan together.
    fn set_fan_speed(&mut self, gpu: usize, fan_index: i32, percent: i32) -> roch_core::Result<()> {
        if fan_index > 0 {
            roch_core::bail!("Intel fan curve controls all fans together.");
        }
        let p = percent as f64;
        Ok(self
            .set_fan_curve_impl(gpu, &FanCurve::with_points(vec![FanPoint::new(25.0, p), FanPoint::new(100.0, p)]))?)
    }

    fn set_fan_auto(&mut self, gpu: usize) -> roch_core::Result<()> {
        Ok(self.set_fan_auto_impl(gpu)?)
    }

    fn reset_to_defaults(&mut self, gpu: usize) -> roch_core::Result<()> {
        Ok(self.reset_to_defaults_impl(gpu)?)
    }

    fn diagnostics(&mut self, gpu: usize) -> String {
        let Some(device) = self.devices.get(gpu).cloned() else {
            return format!("Intel GPU index {gpu} is out of range.");
        };
        let caps = self.cards.get(gpu).map(|c| format!("{:#?}", c.caps)).unwrap_or_default();
        let state = match self.read_tuning_state_impl(gpu) {
            Ok(s) => format!("{s:#?}"),
            Err(e) => format!("error: {}", e.message()),
        };
        let curve = match self.read_vf_curve_impl(gpu) {
            Ok(c) => format!("{c:#?}"),
            Err(e) => format!("error: {}", e.message()),
        };
        format!("Device: {device:#?}\nCapabilities: {caps}\nState: {state}\nCurve: {curve}")
    }

    fn read_vf_curve(&mut self, gpu: usize) -> roch_core::Result<Vec<VfCurveSample>> {
        Ok(self.read_vf_curve_impl(gpu)?)
    }

    fn set_vf_curve_targets(&mut self, gpu: usize, targets: &[VfCurveSample]) -> roch_core::Result<()> {
        Ok(self.set_vf_curve_targets_impl(gpu, targets)?)
    }
}
