//! The engine: owns a backend, applies profiles, runs the polling loop and the fan-curve controller.
//! UI-agnostic, so the window, the command line (`apply-profile` at logon) and tests all drive the
//! same code.
//!
//! # Threading
//!
//! Share it as `Arc<TuningService>`; every method takes `&self`.
//!
//! * One `Mutex` (the C# `_lock`) owns the backend together with the software fan-curve state, so
//!   every driver call is serialised: the vendor libraries are not re-entrant and the polling thread
//!   reads telemetry through the same handle.
//! * A second `Mutex` holds the engine state (capabilities, applied profile, voltage ceilings...).
//!   It is only ever taken *after* the driver lock, never the other way round, and getters take it
//!   alone, so the window can read capabilities without waiting behind a 13 ms telemetry call.
//!
//! # Events
//!
//! Two ways to hear from the service, usable together:
//!
//! * [`TuningService::subscribe`] returns a bounded `mpsc::Receiver<ServiceEvent>`. This is the one
//!   an egui app wants: drain it with `try_recv()` at the top of `update()`. A full channel drops
//!   the event rather than stalling the poll thread.
//! * [`TuningService::add_telemetry_listener`] / [`TuningService::add_log_listener`] register
//!   callbacks (the C# `TelemetryUpdated` / `Log` events). Telemetry callbacks run on the polling
//!   thread; a GUI typically calls `ctx.request_repaint()` from one.
//!
//! Callbacks and channel sends are never made while the service holds its locks, so a listener may
//! call back into the service. Log lines produced while a write holds the lock are buffered and
//! delivered, in order, right after it releases.
//!
//! # Software temperature limit
//!
//! A profile's `soft_thermal_limit_c` is enforced here, by the poll loop, on cards that offer it
//! (`GpuCapabilities::can_set_soft_thermal_limit`): see [`super::soft_thermal`] for the controller.
//! Its state lives with the backend, under the driver lock, like the software fan curve's. Every
//! path that stops it — an apply without it, Reset, [`TuningService::release_soft_thermal_limit`]
//! (exit), a GPU switch, dropping the service — puts the clock range back to exactly what the user
//! armed, or hands it to the driver.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use crate::backend::GpuBackend;
use crate::models::voltage_plan;
use crate::models::*;
use crate::services::TelemetryHistory;
use crate::services::soft_thermal::{self, Confirmed, SoftThermalController, ThermalAction, ThermalBounds};
use crate::util::{clamp_i32, round_even_i32};
use crate::{GpuError, NOTE_PREFIX, Result};

/// How far above the curve's top point a reading can be and still count as "stock".
const STOCK_VID_TOLERANCE_MV: i32 = 25;

/// Load below which a voltage sample says nothing about the ceiling — an idle card sits near
/// 800 mV because it has nothing to do, not because that is as high as it goes.
const CEILING_SAMPLE_MIN_LOAD_PERCENT: f64 = 40.0;

/// Boost level an observation must be taken at to measure the full headroom.
const FULL_BOOST_PERCENT: i32 = 100;

/// Default history depth, in samples (one per second at the default poll interval).
pub const DEFAULT_HISTORY_SECONDS: usize = 120;

/// How long [`TuningService::stop_polling`] waits for a poll stuck inside the driver.
pub const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Capacity of a [`TuningService::subscribe`] channel before events are dropped.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 1024;

/// The software temperature limit steps at most this often, whatever the poll interval: its step
/// sizes are per second.
const THERMAL_PERIOD: Duration = Duration::from_millis(900);

// ------------------------------------------------------------------ events

/// What the service publishes.
#[derive(Debug, Clone)]
pub enum ServiceEvent {
    /// A full sample from a foreground poll (never the temperature-only background reads).
    /// Shared, so fanning one sample out to several subscribers copies nothing.
    Telemetry(Arc<GpuTelemetry>),
    /// A human-readable log line ("Core offset: ok", "Poll error: ...").
    Log(String),
}

/// Identifies a registered listener, for [`TuningService::remove_listener`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListenerId(u64);

type TelemetryFn = Arc<dyn Fn(&GpuTelemetry) + Send + Sync>;
type LogFn = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Default)]
struct Events {
    next_id: AtomicU64,
    telemetry: RwLock<Vec<(u64, TelemetryFn)>>,
    log: RwLock<Vec<(u64, LogFn)>>,
    channels: Mutex<Vec<SyncSender<ServiceEvent>>>,
}

impl Events {
    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn log(&self, message: &str) {
        // Snapshot first so a listener can (un)register without deadlocking on this lock.
        let listeners: Vec<LogFn> =
            self.log.read().unwrap_or_else(|e| e.into_inner()).iter().map(|(_, f)| f.clone()).collect();
        for f in listeners {
            f(message);
        }
        self.send(ServiceEvent::Log(message.to_string()));
    }

    fn logs(&self, messages: Vec<String>) {
        for m in messages {
            self.log(&m);
        }
    }

    fn telemetry(&self, t: &GpuTelemetry) {
        let listeners: Vec<TelemetryFn> =
            self.telemetry.read().unwrap_or_else(|e| e.into_inner()).iter().map(|(_, f)| f.clone()).collect();
        for f in listeners {
            f(t);
        }
        self.send(ServiceEvent::Telemetry(Arc::new(t.clone())));
    }

    fn send(&self, event: ServiceEvent) {
        let mut channels = lock(&self.channels);
        if channels.is_empty() {
            return;
        }
        // A full channel loses this one event; a dropped receiver unsubscribes itself.
        channels.retain(|tx| !matches!(tx.try_send(event.clone()), Err(TrySendError::Disconnected(_))));
    }
}

fn lock<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `{x:+#;-#;0}`.
fn signed(v: i32) -> String {
    if v > 0 { format!("+{v}") } else { v.to_string() }
}

// ------------------------------------------------------------------ state

/// Everything the C# kept under `_lock` that is about talking to the driver.
struct Driver {
    backend: Box<dyn GpuBackend>,
    /// The software fan curve this process is stepping; `None` when the driver owns the fans.
    active_curve: Option<FanCurve>,
    last_curve_fan_sent: f64,
    /// The software temperature limit this process is running.
    thermal: ThermalRun,
}

/// What the software temperature limit is doing, for the UI. A copy; see
/// [`TuningService::soft_thermal_status`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SoftThermalStatus {
    /// An applied profile asks for a limit on this GPU and the card can enforce it.
    pub enabled: bool,
    pub limit_c: i32,
    /// The core clock is being held down.
    pub active: bool,
    /// The ceiling being held, in MHz; 0 while not active.
    pub ceiling_mhz: i32,
    /// The last temperature the controller saw; NaN before the first.
    pub last_temp_c: f64,
    /// Gave up after repeated driver errors; the next Apply tries again.
    pub stopped: bool,
}

impl Default for SoftThermalStatus {
    fn default() -> Self {
        Self { enabled: false, limit_c: 0, active: false, ceiling_mhz: 0, last_temp_c: f64::NAN, stopped: false }
    }
}

/// The software temperature limit as the poll loop runs it.
struct ThermalRun {
    ctl: SoftThermalController,
    /// The GPU the applied limit belongs to; `None` = nothing to run (no limit applied, released at
    /// exit, or another GPU selected since).
    gpu: Option<usize>,
    limit_c: i32,
    /// The backend's `driver_reset_count` as of the last write.
    resets_seen: u64,
    /// The voltage cap's own clock lock, `(mV, MHz)`, when the controller took over that lock: a
    /// release puts the cap back.
    voltage_cap: Option<(i32, i32)>,
    /// `voltage_cap` has been looked for since the clock range was last written by someone else.
    voltage_cap_checked: bool,
    last_tick: Option<Instant>,
}

impl ThermalRun {
    fn new() -> Self {
        Self {
            ctl: SoftThermalController::new(),
            gpu: None,
            limit_c: 0,
            resets_seen: 0,
            voltage_cap: None,
            voltage_cap_checked: false,
            last_tick: None,
        }
    }

    fn wants_samples(&self, gpu: usize) -> bool {
        self.gpu == Some(gpu) && self.limit_c > 0 && !self.ctl.is_stopped()
    }

    /// Someone else just wrote the clock range (an apply, the Clock Range lever, a new voltage cap,
    /// a driver reset): the held window is gone from the card, so the next step writes it again,
    /// after looking for a voltage cap that might now share the lock. `voltage_cap_replaced`: that
    /// write also settled the voltage cap, so the one remembered is stale.
    fn clock_range_rewritten(&mut self, voltage_cap_replaced: bool) {
        self.ctl.forget_written();
        self.voltage_cap_checked = false;
        if voltage_cap_replaced {
            self.voltage_cap = None;
        }
        self.last_tick = None;
    }

    /// Not running any more. The caller has put the clock back (or the card was reset).
    fn stop(&mut self) {
        let resets_seen = self.resets_seen;
        *self = Self::new();
        self.resets_seen = resets_seen;
    }

    fn status(&self) -> SoftThermalStatus {
        let enabled = self.gpu.is_some() && self.limit_c > 0;
        SoftThermalStatus {
            enabled,
            limit_c: if enabled { self.limit_c } else { 0 },
            active: self.ctl.is_holding(),
            ceiling_mhz: self.ctl.ceiling_mhz().unwrap_or(0),
            last_temp_c: self.ctl.last_temp_c(),
            stopped: self.ctl.is_stopped(),
        }
    }
}

/// The window the thermal controller works in: the card's lockable range, inside the user's own
/// Clock Range when that lever is on the card, and under a voltage cap's clock lock it took over.
fn thermal_bounds(st: &EngineState, voltage_cap_mhz: i32) -> ThermalBounds {
    let user = if st.armed_on_card.has(XocLever::CLOCK_RANGE) {
        st.applied.as_ref().map(|a| (a.clock_lock_min_mhz, a.clock_lock_max_mhz))
    } else {
        None
    };
    ThermalBounds::new(st.caps.clock_lock_min_mhz, st.caps.clock_lock_max_mhz, user, voltage_cap_mhz)
}

/// Put the clock back the way the user left it — their own range, or the driver's — then the voltage
/// cap's clock lock if the controller had taken it over (`restore_voltage_cap`).
fn thermal_restore(drv: &mut Driver, gpu: usize, b: &ThermalBounds, restore_voltage_cap: bool) -> Result<()> {
    drv.backend.set_clock_range(gpu, b.restore.0, b.restore.1)?;
    let cap = drv.thermal.voltage_cap.take();
    if restore_voltage_cap && let Some((mv, _)) = cap.filter(|(mv, _)| *mv > 0) {
        drv.backend.set_voltage_lock(gpu, mv)?;
    }
    Ok(())
}

/// Hand a held clock back and stop the controller, logging what happened. For every path that turns
/// the limit off without writing the clock range itself.
fn thermal_release_and_stop(drv: &mut Driver, st: &EngineState, gpu: usize, why: &str, logs: &mut Vec<String>) {
    if drv.thermal.ctl.is_holding() {
        let mhz = drv.thermal.voltage_cap.map(|(_, mhz)| mhz).unwrap_or(0);
        let b = thermal_bounds(st, mhz);
        match thermal_restore(drv, gpu, &b, true) {
            Ok(()) => logs.push(format!("Thermal limit: {why}; {}", released_to(&b))),
            Err(e) => logs.push(format!("Thermal limit: {why}, but the clock could not be released: {}", e.message)),
        }
    }
    drv.thermal.stop();
}

fn released_to(b: &ThermalBounds) -> String {
    match b.restore {
        (0, 0) => "core clock handed back to the driver".into(),
        (lo, 0) => format!("core clock back to your {lo} MHz clock range"),
        (lo, hi) => format!("core clock back to your {lo}-{hi} MHz clock range"),
    }
}

fn whole(c: f64) -> String {
    format!("{c:.0}")
}

/// Engine state. Locked after `Driver` when both are needed.
struct EngineState {
    devices: Vec<GpuDevice>,
    backend_name: String,
    caps: GpuCapabilities,
    applied: Option<TuningProfile>,
    armed_on_card: XocLever,
    latest: Option<GpuTelemetry>,
    observed_max_voltage_mv: i32,
    observed_max_boosted_voltage_mv: i32,
    nvvdd_default_max_mv: i32,
    msvdd_default_max_mv: i32,
    nvvdd_ocp_default_ma: i32,
    msvdd_ocp_default_ma: i32,
    /// Voltage state the card is actually carrying, refreshed when we change it.
    live_boost_percent: i32,
    live_lock_mv: i32,
    live_rail_max_mv: i32,
    voltage_lock_mechanism: String,
    manual_curve_active: bool,
    thermal_status: SoftThermalStatus,
}

impl EngineState {
    fn new() -> Self {
        Self {
            devices: Vec::new(),
            backend_name: String::new(),
            caps: GpuCapabilities::default(),
            applied: None,
            armed_on_card: XocLever::NONE,
            latest: None,
            observed_max_voltage_mv: 0,
            observed_max_boosted_voltage_mv: 0,
            nvvdd_default_max_mv: 0,
            msvdd_default_max_mv: 0,
            nvvdd_ocp_default_ma: 0,
            msvdd_ocp_default_ma: 0,
            live_boost_percent: 0,
            live_lock_mv: 0,
            live_rail_max_mv: 0,
            voltage_lock_mechanism: "none".into(),
            manual_curve_active: false,
            thermal_status: SoftThermalStatus::default(),
        }
    }

    fn device(&self, gpu: usize) -> GpuDevice {
        self.devices.get(gpu).cloned().unwrap_or_default()
    }

    /// Floor for an observed ceiling: the lowest voltage this card's own controls go down to.
    ///
    /// Not a fixed drop below the curve's top point: the table's top is the table's extent, not a
    /// bound on what the card reaches (a 5070 Ti's curve runs to 1240 mV against a ceiling of
    /// 1035). The load gate in `note_voltage_observation` is what keeps idle samples out.
    fn observed_ceiling_floor_mv(&self) -> i32 {
        if self.caps.min_voltage_mv > 0 { self.caps.min_voltage_mv } else { 700 }
    }

    /// Stock ceiling: the curve's top point, corrected to the VID the card really uses.
    ///
    /// Only readings within one V/F step above the curve top are trusted. Anything higher was
    /// reached *because* a boost was applied; folding it in would raise the ceiling to the top of
    /// the slider, after which Apply computes neither a boost nor a cap and writes nothing while
    /// reporting success. Readings below the top are trusted much further, because a card that
    /// never reaches its table's last point is common.
    fn stock_ceiling_mv(&self) -> i32 {
        let curve_top = self.caps.stock_max_voltage_mv;
        if curve_top <= 0 {
            return if self.observed_max_voltage_mv > 0 {
                self.observed_max_voltage_mv
            } else {
                self.caps.max_voltage_mv
            };
        }
        // Nothing sampled under load yet, so the table's top is the only honest answer.
        if self.observed_max_voltage_mv <= 0 {
            return curve_top;
        }
        clamp_i32(self.observed_max_voltage_mv, self.observed_ceiling_floor_mv(), curve_top + STOCK_VID_TOLERANCE_MV)
    }

    /// The highest voltage this card reaches with the boost wound fully open. A measured value
    /// always wins over the compiled-in guess (curve top + fixed headroom measured once on a
    /// 4070 Ti; a 5070 Ti gains ~15 mV where the constant assumes 60).
    fn boost_ceiling_mv(&self) -> i32 {
        let stock = self.stock_ceiling_mv();
        let ceiling = if self.observed_max_boosted_voltage_mv > stock {
            self.observed_max_boosted_voltage_mv
        } else {
            stock.max(self.caps.max_voltage_mv)
        };
        // Raising the rail's own ceiling lifts the roof everything else works under.
        ceiling.max(self.live_rail_max_mv)
    }

    /// The highest voltage the card can select once a given boost is applied — the ceiling a cap
    /// is measured against, and the value that means "cap nothing".
    fn reachable_ceiling_mv(&self, boost_percent: i32) -> i32 {
        let stock = self.stock_ceiling_mv();
        voltage_plan::ceiling_mv(
            boost_percent,
            0,
            if stock > 0 { stock } else { self.caps.max_voltage_mv },
            self.boost_ceiling_mv(),
            self.caps.stock_max_voltage_mv,
        )
    }

    /// True while no voltage lever is engaged, so live voltage still reflects stock.
    fn voltage_is_untouched(&self) -> bool {
        if self.live_boost_percent != 0 || self.live_lock_mv > 0 {
            return false;
        }
        let Some(a) = &self.applied else { return true };
        if a.voltage_boost_percent != 0 || a.voltage_offset_mv != 0 {
            return false;
        }
        a.target_voltage_mv <= 0 || a.target_voltage_mv >= self.caps.stock_max_voltage_mv
    }

    fn note_voltage_observation(&mut self, mv: f64, load_percent: f64) {
        if mv.is_nan() || mv >= 1400.0 {
            return;
        }
        // An idle reading is the card resting, not the card's limit.
        if load_percent.is_nan() || load_percent < CEILING_SAMPLE_MIN_LOAD_PERCENT {
            return;
        }
        // A full boost with nothing capping it measures the OTHER ceiling: how far this card can be
        // driven above stock. No NVAPI call reports that headroom.
        if self.live_boost_percent >= FULL_BOOST_PERCENT && self.live_lock_mv <= 0 {
            if mv > self.observed_max_boosted_voltage_mv as f64 {
                self.observed_max_boosted_voltage_mv = round_even_i32(mv);
            }
            return;
        }
        if !self.voltage_is_untouched() {
            return;
        }
        let curve_top = self.caps.stock_max_voltage_mv;
        // More than one V/F step above the curve top is boost, not stock — one such sample would
        // poison the ceiling for the rest of the session.
        if curve_top > 0 && mv > (curve_top + STOCK_VID_TOLERANCE_MV) as f64 {
            return;
        }
        if mv > self.observed_max_voltage_mv as f64 {
            self.observed_max_voltage_mv = round_even_i32(mv);
        }
    }

    fn seed_observed_max_voltage(&mut self, mv: i32) {
        if mv <= 0 || mv >= 1400 {
            return;
        }
        let curve_top = self.caps.stock_max_voltage_mv;
        if curve_top > 0 && mv > curve_top + STOCK_VID_TOLERANCE_MV {
            return;
        }
        if mv < self.observed_ceiling_floor_mv() {
            return;
        }
        if mv > self.observed_max_voltage_mv {
            self.observed_max_voltage_mv = mv;
        }
    }

    fn seed_observed_max_boosted_voltage(&mut self, mv: i32) {
        if mv <= 0 || mv >= 1400 {
            return;
        }
        // Must sit above stock (a boost that buys nothing measures no headroom) and no higher than
        // the compiled-in guess, which is generous rather than tight.
        if mv <= self.stock_ceiling_mv() || (self.caps.max_voltage_mv > 0 && mv > self.caps.max_voltage_mv) {
            return;
        }
        if mv > self.observed_max_boosted_voltage_mv {
            self.observed_max_boosted_voltage_mv = mv;
        }
    }

    /// The levers this card exposes at all; the rest can never be armed.
    fn supported_levers(&self) -> XocLever {
        let c = &self.caps;
        let mut l = XocLever::NONE;
        if c.can_set_voltage_rail {
            l |= XocLever::NVVDD;
        }
        if c.can_set_msvdd_rail {
            l |= XocLever::MSVDD;
        }
        if c.can_set_xbar_offset {
            l |= XocLever::XBAR;
        }
        if c.can_set_sys_offset {
            l |= XocLever::SYS;
        }
        if c.can_set_video_offset {
            l |= XocLever::VIDEO;
        }
        if c.can_lock_clocks {
            l |= XocLever::CLOCK_RANGE;
        }
        l
    }
}

/// Re-read the levers the card is holding. "Has anything been applied" cannot be answered from
/// the applied profile alone: a boost set in an earlier run, or by another tool, is still on the
/// card when we start, and treating that as stock would record a boosted voltage as the ceiling.
fn refresh_live_voltage_state(b: &mut dyn GpuBackend, gpu: usize, st: &mut EngineState) {
    match b.read_tuning_state(gpu) {
        Ok(live) => {
            st.live_boost_percent = live.voltage_boost_percent;
            st.live_rail_max_mv = live.voltage_rail_max_mv;
            st.live_lock_mv = b.read_voltage_lock_mv(gpu).max(0);
        }
        Err(_) => {
            st.live_boost_percent = 0;
            st.live_lock_mv = 0;
            st.live_rail_max_mv = 0;
        }
    }
}

/// The C# `Try` helper: run one write, record the failure, log the outcome. Log lines are
/// buffered so they can be published once the locks are released.
struct Attempts {
    errors: Vec<String>,
    logs: Vec<String>,
    dash: &'static str,
    silent: bool,
}

impl Attempts {
    fn new(dash: &'static str) -> Self {
        Self { errors: Vec::new(), logs: Vec::new(), dash, silent: false }
    }

    /// Reset's variant: failures are swallowed and nothing is logged.
    fn silent() -> Self {
        Self { silent: true, ..Self::new("-") }
    }

    /// Returns whether the write succeeded.
    fn run(&mut self, what: &str, r: Result<()>) -> bool {
        match r {
            Ok(()) => {
                if !self.silent {
                    self.logs.push(format!("{what}: ok"));
                }
                true
            }
            Err(e) => {
                if !self.silent {
                    self.errors.push(format!("{what}: {}", e.message));
                    self.logs.push(format!("{what}: FAILED {} {}", self.dash, e.message));
                }
                false
            }
        }
    }

    fn log(&mut self, message: String) {
        self.logs.push(message);
    }

    fn any_error_starting(&self, prefix: &str) -> bool {
        self.errors.iter().any(|e| e.starts_with(prefix))
    }
}

/// Note every value the card's limits had to narrow. Clamping is right — the driver's range is the
/// range — but applying a different number than the one asked for without saying so is how
/// "+3500 MHz memory" read as success while the card actually ran +3000.
fn add_clamp_notes(notes: &mut Vec<String>, caps: &GpuCapabilities, asked: &TuningProfile, applied: &TuningProfile) {
    let mut note = |what: &str, a: i32, b: i32, unit: &str| {
        if a != b {
            notes.push(format!(
                "{NOTE_PREFIX} {what} {a}{unit} is outside this card's range — applied {b}{unit} instead."
            ));
        }
    };

    note("core offset", asked.core_offset_mhz, applied.core_offset_mhz, " MHz");
    note("memory tuning", asked.memory_offset_mhz, applied.memory_offset_mhz, &format!(" {}", caps.memory_clock_unit));
    note("power limit", asked.power_limit_percent, applied.power_limit_percent, "%");
    // Only when the card has a thermal policy at all; otherwise the profile carries a default
    // nobody asked for (a hidden, driver-owned limit on RDNA 4) and noting it is noise.
    if caps.can_set_temp_limit {
        note("temperature limit", asked.temp_limit_c, applied.temp_limit_c, caps.temp_limit_unit());
    }
    note("voltage boost", asked.voltage_boost_percent, applied.voltage_boost_percent, "%");
    note("undervolt", asked.voltage_offset_mv, applied.voltage_offset_mv, " mV");
    // Only for an armed lever; the rest are written back to the driver's own values.
    if asked.xoc_armed.has(XocLever::NVVDD) {
        if asked.voltage_rail_max_mv > 0 {
            note("core rail ceiling", asked.voltage_rail_max_mv, applied.voltage_rail_max_mv, " mV");
        }
        if asked.voltage_rail_floor_mv > 0 {
            note("core rail floor", asked.voltage_rail_floor_mv, applied.voltage_rail_floor_mv, " mV");
        }
    }
    if asked.xoc_armed.has(XocLever::MSVDD) {
        if asked.msvdd_rail_max_mv > 0 {
            note("MSVDD ceiling", asked.msvdd_rail_max_mv, applied.msvdd_rail_max_mv, " mV");
        }
        if asked.msvdd_rail_floor_mv > 0 {
            note("MSVDD floor", asked.msvdd_rail_floor_mv, applied.msvdd_rail_floor_mv, " mV");
        }
    }
    if asked.xoc_armed.has(XocLever::XBAR) && caps.can_set_xbar_offset {
        note("crossbar offset", asked.xbar_offset_mhz, applied.xbar_offset_mhz, " MHz");
    }
    if asked.xoc_armed.has(XocLever::SYS) && caps.can_set_sys_offset {
        note("SYS clock offset", asked.sys_offset_mhz, applied.sys_offset_mhz, " MHz");
    }
    if asked.xoc_armed.has(XocLever::VIDEO) && caps.can_set_video_offset {
        note("video clock offset", asked.video_offset_mhz, applied.video_offset_mhz, " MHz");
    }
    if asked.xoc_armed.has(XocLever::CLOCK_RANGE) && caps.can_lock_clocks {
        // Only the side that was asked for: a floor-only request pins the card at that figure, so
        // the ceiling reads back non-zero and would otherwise be reported as a clamp.
        if asked.clock_lock_min_mhz > 0 {
            note("clock floor", asked.clock_lock_min_mhz, applied.clock_lock_min_mhz, " MHz");
        }
        if asked.clock_lock_max_mhz > 0 {
            note("clock ceiling", asked.clock_lock_max_mhz, applied.clock_lock_max_mhz, " MHz");
        }
    }
    // 0 means "no cap" and never clamps.
    if asked.target_voltage_mv > 0 {
        note("voltage cap", asked.target_voltage_mv, applied.target_voltage_mv, " mV");
    }
    // An auto-fan profile still carries whatever fixed duty it last had.
    if asked.fan_mode == FanMode::Fixed {
        note("fan speed", asked.fixed_fan_percent, applied.fixed_fan_percent, "%");
    }
    if caps.can_set_soft_thermal_limit && asked.soft_thermal_limit_c > 0 {
        let card = caps.soft_thermal_limit_range().1;
        if applied.soft_thermal_limit_c == 0 {
            // At or above the card's own limit there is nothing for the software to do.
            notes.push(format!(
                "{NOTE_PREFIX} temperature limit {} °C is at or above this card's own limit ({card} °C) — left to the card.",
                asked.soft_thermal_limit_c
            ));
        } else if asked.soft_thermal_limit_c != applied.soft_thermal_limit_c {
            notes.push(format!(
                "{NOTE_PREFIX} software temperature limit {} °C is outside this card's range — applied {} °C instead.",
                asked.soft_thermal_limit_c, applied.soft_thermal_limit_c
            ));
        }
    }
}

/// Write the gated (XOC) levers. Armed levers take the profile's value; a lever this service
/// previously armed is restored when it becomes disarmed. Never-armed voltage rails are left to
/// the driver: Voltage Boost moves their card-specific baselines, and rewriting a startup snapshot
/// after the boost call would cancel that movement.
///
/// `which` narrows it to the levers a single button touched, so one Enable cannot pull a rail that
/// was deliberately left alone into whatever the sliders happened to show.
fn write_xoc(
    b: &mut dyn GpuBackend,
    gpu: usize,
    st: &mut EngineState,
    p: &TuningProfile,
    t: &mut Attempts,
    which: XocLever,
    force_restore_disarmed: bool,
) {
    let on = p.xoc_armed;
    let was_on = st.armed_on_card;
    // Recorded here because every lever write goes through this routine. Masked by what the card
    // can do, so a profile asking for a domain this GPU has not got cannot report itself live.
    st.armed_on_card = (st.armed_on_card & !which) | (on & which & st.supported_levers());
    let c = &st.caps;

    if which.has(XocLever::NVVDD) && c.can_set_voltage_rail {
        let armed = on.has(XocLever::NVVDD);
        let restore = !armed && (force_restore_disarmed || was_on.has(XocLever::NVVDD));
        if armed {
            if p.voltage_rail_max_mv > 0 {
                t.run("Core rail ceiling", b.set_voltage_rail_max(gpu, p.voltage_rail_max_mv));
            }
            t.run("Core rail floor", b.set_voltage_rail_floor(gpu, p.voltage_rail_floor_mv));
        } else if restore {
            // A default we never saw is left alone rather than guessed at (guessing low browns the
            // card out). Floors have a real default of zero offset, unlike ceilings.
            if st.nvvdd_default_max_mv > 0 {
                t.run("Core rail ceiling", b.set_voltage_rail_max(gpu, st.nvvdd_default_max_mv));
            }
            t.run("Core rail floor", b.set_voltage_rail_floor(gpu, 0));
        }
    }

    if which.has(XocLever::MSVDD) && c.can_set_msvdd_rail {
        let armed = on.has(XocLever::MSVDD);
        let restore = !armed && (force_restore_disarmed || was_on.has(XocLever::MSVDD));
        if armed {
            if p.msvdd_rail_max_mv > 0 {
                t.run("MSVDD ceiling", b.set_msvdd_rail_max(gpu, p.msvdd_rail_max_mv));
            }
            t.run("MSVDD floor", b.set_msvdd_rail_floor(gpu, p.msvdd_rail_floor_mv));
        } else if restore {
            if st.msvdd_default_max_mv > 0 {
                t.run("MSVDD ceiling", b.set_msvdd_rail_max(gpu, st.msvdd_default_max_mv));
            }
            t.run("MSVDD floor", b.set_msvdd_rail_floor(gpu, 0));
        }
    }

    if which.has(XocLever::XBAR) && c.can_set_xbar_offset {
        let mhz = if on.has(XocLever::XBAR) { p.xbar_offset_mhz } else { 0 };
        t.run("Crossbar offset", b.set_xbar_offset(gpu, mhz));
    }

    if which.has(XocLever::SYS) && c.can_set_sys_offset {
        let mhz = if on.has(XocLever::SYS) { p.sys_offset_mhz } else { 0 };
        t.run("SYS clock offset", b.set_sys_offset(gpu, mhz));
    }

    if which.has(XocLever::VIDEO) && c.can_set_video_offset {
        let mhz = if on.has(XocLever::VIDEO) { p.video_offset_mhz } else { 0 };
        t.run("Video clock offset", b.set_video_offset(gpu, mhz));
    }

    // Disarmed restores the stock limit rather than leaving whatever was last written: an OCP limit
    // survives a reboot, and a card left with its over-current protection wound off is exactly the
    // state nobody chose and nobody can see.
    if which.has(XocLever::NVVDD_OCP) && c.can_set_ocp {
        let ma = if on.has(XocLever::NVVDD_OCP) && p.nvvdd_ocp_milliamps > 0 {
            p.nvvdd_ocp_milliamps
        } else {
            st.nvvdd_ocp_default_ma
        };
        if ma > 0 {
            t.run("NVVDD OCP limit", b.set_nvvdd_ocp_milliamps(gpu, ma));
        }
    }

    if which.has(XocLever::MSVDD_OCP) && c.can_set_ocp {
        let ma = if on.has(XocLever::MSVDD_OCP) && p.msvdd_ocp_milliamps > 0 {
            p.msvdd_ocp_milliamps
        } else {
            st.msvdd_ocp_default_ma
        };
        if ma > 0 {
            t.run("MSVDD OCP limit", b.set_msvdd_ocp_milliamps(gpu, ma));
        }
    }

    if which.has(XocLever::CLOCK_RANGE) && c.can_lock_clocks {
        // 0/0 hands the range back to the driver, which is exactly what disarmed means here.
        let armed = on.has(XocLever::CLOCK_RANGE);
        let (lo, hi) = if armed { (p.clock_lock_min_mhz, p.clock_lock_max_mhz) } else { (0, 0) };
        t.run("Clock range", b.set_clock_range(gpu, lo, hi));
    }
}

/// The one place fans are written. An apply and the fan window both come through here: while they
/// were two copies they drifted — one clamped the duties and the other did not, and a one-element
/// list meant "just cooler 1" to one and "all of them" to the other.
#[allow(clippy::too_many_arguments)]
fn write_fans(
    drv: &mut Driver,
    caps: &GpuCapabilities,
    gpu: usize,
    mode: FanMode,
    percent: i32,
    per_fan: &[i32],
    curve: &FanCurve,
    t: &mut Attempts,
) {
    let lo = caps.fan_min_percent;
    let hi = if caps.fan_max_percent <= 0 { 100 } else { caps.fan_max_percent };
    match mode {
        FanMode::Auto => {
            drv.active_curve = None;
            t.run("Fan auto", drv.backend.set_fan_auto(gpu));
        }
        FanMode::Fixed => {
            drv.active_curve = None;
            // Fewer than two duties is one duty for every fan: -1 is "all coolers", and a single
            // entry addressed to cooler 1 would leave the rest wherever they were.
            if per_fan.len() < 2 {
                t.run("Fan fixed", drv.backend.set_fan_speed(gpu, -1, clamp_i32(percent, lo, hi)));
            } else {
                // Cooler ids start at 1: the list position is the fan, the id is what the driver
                // answers to.
                for (i, &duty) in per_fan.iter().enumerate() {
                    let id = i as i32 + 1;
                    let duty = clamp_i32(duty, lo, hi);
                    t.run(&format!("Fan {id} fixed"), drv.backend.set_fan_speed(gpu, id, duty));
                }
            }
        }
        FanMode::Curve => {
            if caps.fan_curve_is_hardware {
                // The driver runs it, so it survives this app closing — and no polling loop means
                // no fan oscillation if the app is busy.
                drv.active_curve = None;
                t.run("Fan curve", drv.backend.set_fan_curve(gpu, curve));
            } else {
                drv.active_curve = Some(curve.clone_config());
                drv.last_curve_fan_sent = f64::NAN;
                // First step happens on the next poll, using the live temperature.
            }
        }
    }
}

/// Shared between the service handle and its polling thread.
struct Inner {
    driver: Mutex<Driver>,
    state: Mutex<EngineState>,
    gpu_index: AtomicUsize,
    history: TelemetryHistory,
    background_mode: AtomicBool,
    events: Events,
}

impl Inner {
    fn gpu(&self) -> usize {
        self.gpu_index.load(Ordering::SeqCst)
    }

    fn poll_foreground(&self) -> Result<()> {
        let gpu = self.gpu();
        let t = lock(&self.driver).backend.read_telemetry(gpu)?;
        {
            let mut st = lock(&self.state);
            st.latest = Some(t.clone());
            st.note_voltage_observation(t.voltage_mv, t.gpu_load_percent);
        }
        self.history.add(t.clone());
        self.run_fan_curve(&t);
        self.run_soft_thermal(&t, true);
        self.events.telemetry(&t);
        Ok(())
    }

    /// Nothing is on screen, so a sample is only worth taking if a fan curve or the software
    /// temperature limit needs one — and then only the temperature. The reading is stepped into
    /// them and dropped: it is not stored in `latest` or the history and not published, because it
    /// has one valid field and would show up in the graphs as a run of zeroes. The graphs keep a gap
    /// for the time you were gaming.
    fn poll_background(&self) -> Result<()> {
        let gpu = self.gpu();
        let wanted = {
            let drv = lock(&self.driver);
            drv.active_curve.is_some() || drv.thermal.wants_samples(gpu)
        };
        if !wanted {
            return Ok(());
        }
        let t = lock(&self.driver).backend.read_temperature_only(gpu)?;
        self.run_fan_curve(&t);
        self.run_soft_thermal(&t, true);
        Ok(())
    }

    /// One step of the software temperature limit. `gated`: the poll loop's call, held to one step
    /// per [`THERMAL_PERIOD`].
    fn run_soft_thermal(&self, t: &GpuTelemetry, gated: bool) {
        let gpu = self.gpu();
        let mut logs = Vec::new();
        {
            let mut guard = lock(&self.driver);
            let drv = &mut *guard;
            if !drv.thermal.wants_samples(gpu) {
                return;
            }
            if gated && drv.thermal.last_tick.is_some_and(|at| at.elapsed() < THERMAL_PERIOD) {
                return;
            }
            drv.thermal.last_tick = Some(Instant::now());
            let limit = drv.thermal.limit_c;
            let temp = t.temperature_c;

            // A display-driver reset clears the lock without a word; the backend counts the resets
            // it recovers from. Whatever was written before one is not on the card any more.
            let resets = drv.backend.driver_reset_count();
            if resets != drv.thermal.resets_seen {
                drv.thermal.resets_seen = resets;
                if drv.thermal.ctl.is_holding() {
                    logs.push("Thermal limit: the display driver was reset; writing the clock ceiling again".into());
                }
                drv.thermal.clock_range_rewritten(false);
                drv.thermal.last_tick = Some(Instant::now());
            }
            // A voltage cap held through the same clock lock is a ceiling too: the controller works
            // under it, and puts it back when it lets go.
            if !drv.thermal.voltage_cap_checked {
                drv.thermal.voltage_cap_checked = true;
                let mhz = drv.backend.voltage_cap_clock_mhz(gpu);
                if mhz > 0 {
                    let mv = drv.backend.read_voltage_lock_mv(gpu);
                    drv.thermal.voltage_cap = Some((mv, mhz));
                }
            }

            let mut st = lock(&self.state);
            let b = thermal_bounds(&st, drv.thermal.voltage_cap.map(|(_, mhz)| mhz).unwrap_or(0));
            // A hold starts from the clock the card is running; a background reading has none.
            let mut clock = t.core_clock_mhz;
            if drv.thermal.ctl.would_engage(limit, temp) && (clock.is_nan() || clock < 1.0) {
                clock = drv.backend.read_telemetry(gpu).map(|f| f.core_clock_mhz).unwrap_or(f64::NAN);
            }
            let before = drv.thermal.ctl.ceiling_mhz();
            let action = drv.thermal.ctl.step(limit, temp, clock, &b);
            let result = match action {
                ThermalAction::None => None,
                ThermalAction::Cap { min_mhz, max_mhz } => Some(drv.backend.set_clock_range(gpu, min_mhz, max_mhz)),
                ThermalAction::Release { .. } => Some(thermal_restore(drv, gpu, &b, true)),
            };
            if let Some(r) = result {
                let failure = r.as_ref().err().map(|e| e.message.clone()).unwrap_or_default();
                match drv.thermal.ctl.confirm(r.is_ok()) {
                    Confirmed::Ok => {
                        drv.thermal.resets_seen = drv.backend.driver_reset_count();
                        match action {
                            ThermalAction::Cap { max_mhz, .. } if before != Some(max_mhz) => {
                                logs.push(if before.is_some_and(|b| max_mhz > b) {
                                    format!("Thermal limit: {} °C, core ceiling raised to {max_mhz} MHz", whole(temp))
                                } else if temp >= limit as f64 {
                                    format!(
                                        "Thermal limit: {} °C ≥ {limit} °C, core capped at {max_mhz} MHz",
                                        whole(temp)
                                    )
                                } else {
                                    format!("Thermal limit: core capped at {max_mhz} MHz")
                                });
                            }
                            ThermalAction::Release { .. } => logs.push(format!(
                                "Thermal limit: {} °C ≤ {} °C, {}",
                                whole(temp),
                                whole(limit as f64 - soft_thermal::HYSTERESIS_C),
                                released_to(&b)
                            )),
                            _ => {}
                        }
                    }
                    Confirmed::BackingOff => logs.push(format!(
                        "Thermal limit: could not move the core clock ceiling: {failure} — trying again in {} s",
                        soft_thermal::BACKOFF_TICKS
                    )),
                    Confirmed::Stopped => {
                        // Best effort: a driver refusing every write probably refuses this too, but a
                        // cap left standing with nobody managing it is the worse outcome.
                        let _ = thermal_restore(drv, gpu, &b, true);
                        logs.push(format!(
                            "Thermal limit: could not move the core clock ceiling: {failure} — stopped after {} \
                             failed attempts. Press Apply to try again.",
                            soft_thermal::MAX_FAILURES
                        ));
                    }
                }
            }
            st.thermal_status = drv.thermal.status();
        }
        self.events.logs(logs);
    }

    fn run_fan_curve(&self, t: &GpuTelemetry) {
        let gpu = self.gpu();
        let mut failure = None;
        {
            let mut guard = lock(&self.driver);
            let drv = &mut *guard;
            // A fan-mode change must not be followed by a write from the old curve.
            let Some(curve) = drv.active_curve.as_mut() else { return };
            if !t.temperature_c.is_finite() {
                return;
            }
            let Some(next) = curve.step(t.temperature_c) else { return };
            let pct = round_even_i32(next);
            if !drv.last_curve_fan_sent.is_nan() && (pct as f64 - drv.last_curve_fan_sent).abs() < 1.0 {
                return;
            }
            match drv.backend.set_fan_speed(gpu, -1, pct) {
                Ok(()) => drv.last_curve_fan_sent = pct as f64,
                Err(e) => failure = Some(format!("Fan curve: {}", e.message)),
            }
        }
        if let Some(m) = failure {
            self.events.log(&m);
        }
    }

    fn poll_loop(&self, interval: Duration, cancel: Receiver<PollSignal>) {
        loop {
            if !matches!(cancel.try_recv(), Err(TryRecvError::Empty) | Ok(PollSignal::Wake)) {
                break;
            }
            let background = self.background_mode.load(Ordering::SeqCst);
            // A backend that panics must not take the loop with it (the C# caught everything).
            let r = catch_unwind(AssertUnwindSafe(|| {
                if background { self.poll_background() } else { self.poll_foreground() }
            }));
            match r {
                Ok(Ok(())) => {}
                Ok(Err(e)) => self.events.log(&format!("Poll error: {}", e.message)),
                Err(_) => self.events.log("Poll error: the backend panicked"),
            }
            match cancel.recv_timeout(interval) {
                Err(RecvTimeoutError::Timeout) | Ok(PollSignal::Wake) => continue,
                _ => break,
            }
        }
    }
}

/// What the polling thread can be told between samples.
enum PollSignal {
    Stop,
    /// Sample now rather than at the end of the interval (a window that shows readings opened).
    Wake,
}

struct Poller {
    cancel: Sender<PollSignal>,
    done: Receiver<()>,
    handle: Option<JoinHandle<()>>,
    thread_id: ThreadId,
}

/// The tuning engine. See the module docs for threading and events.
pub struct TuningService {
    inner: Arc<Inner>,
    poll: Mutex<Option<Poller>>,
}

impl TuningService {
    /// Wrap a backend (not yet initialised) with the default 120-sample history.
    pub fn new(backend: Box<dyn GpuBackend>) -> Self {
        Self::with_history(backend, DEFAULT_HISTORY_SECONDS)
    }

    pub fn with_history(backend: Box<dyn GpuBackend>, history_seconds: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                driver: Mutex::new(Driver {
                    backend,
                    active_curve: None,
                    last_curve_fan_sent: f64::NAN,
                    thermal: ThermalRun::new(),
                }),
                state: Mutex::new(EngineState::new()),
                gpu_index: AtomicUsize::new(0),
                history: TelemetryHistory::new(history_seconds),
                background_mode: AtomicBool::new(false),
                events: Events::default(),
            }),
            poll: Mutex::new(None),
        }
    }

    fn state(&self) -> MutexGuard<'_, EngineState> {
        lock(&self.inner.state)
    }

    fn gpu(&self) -> usize {
        self.inner.gpu()
    }

    fn log(&self, message: &str) {
        self.inner.events.log(message);
    }

    // ------------------------------------------------------------------ events

    /// A bounded channel of every telemetry sample and log line from now on. Drain it from the UI
    /// thread; when it is full, new events are dropped. Dropping the receiver unsubscribes.
    pub fn subscribe(&self) -> Receiver<ServiceEvent> {
        self.subscribe_with_capacity(DEFAULT_CHANNEL_CAPACITY)
    }

    pub fn subscribe_with_capacity(&self, capacity: usize) -> Receiver<ServiceEvent> {
        let (tx, rx) = mpsc::sync_channel(capacity.max(1));
        lock(&self.inner.events.channels).push(tx);
        rx
    }

    /// Raised on the polling thread after each foreground sample (C# `TelemetryUpdated`). The UI
    /// must marshal to its own thread.
    pub fn add_telemetry_listener(&self, f: impl Fn(&GpuTelemetry) + Send + Sync + 'static) -> ListenerId {
        let id = self.inner.events.id();
        self.inner.events.telemetry.write().unwrap_or_else(|e| e.into_inner()).push((id, Arc::new(f)));
        ListenerId(id)
    }

    /// C# `Log`. Called on whichever thread produced the line.
    pub fn add_log_listener(&self, f: impl Fn(&str) + Send + Sync + 'static) -> ListenerId {
        let id = self.inner.events.id();
        self.inner.events.log.write().unwrap_or_else(|e| e.into_inner()).push((id, Arc::new(f)));
        ListenerId(id)
    }

    /// Unregister a listener added with either `add_*_listener`.
    pub fn remove_listener(&self, id: ListenerId) {
        let ev = &self.inner.events;
        ev.telemetry.write().unwrap_or_else(|e| e.into_inner()).retain(|(i, _)| *i != id.0);
        ev.log.write().unwrap_or_else(|e| e.into_inner()).retain(|(i, _)| *i != id.0);
    }

    // ------------------------------------------------------------------ backend access

    /// Run `f` against the raw backend under the driver lock (the C# `Backend` property). Keep it
    /// short: the polling thread waits behind it.
    pub fn with_backend<R>(&self, f: impl FnOnce(&mut dyn GpuBackend) -> R) -> R {
        let mut drv = lock(&self.inner.driver);
        f(drv.backend.as_mut())
    }

    /// Initialise the vendor API, then select `gpu_index`.
    pub fn initialize(&self, gpu_index: usize) -> Result<()> {
        {
            let mut drv = lock(&self.inner.driver);
            drv.backend.initialize()?;
            let devices = drv.backend.devices().to_vec();
            let name = drv.backend.backend_name();
            let mut st = self.state();
            st.devices = devices;
            st.backend_name = name;
        }
        self.select_gpu(gpu_index)
    }

    /// Switch to another GPU of the same backend. Fails for an index the backend did not report.
    pub fn select_gpu(&self, index: usize) -> Result<()> {
        let message;
        let mut logs = Vec::new();
        {
            let mut guard = lock(&self.inner.driver);
            let drv = &mut *guard;
            let mut st = self.state();
            st.devices = drv.backend.devices().to_vec();
            st.backend_name = drv.backend.backend_name();
            if index >= st.devices.len() {
                return Err(GpuError::new(format!("No GPU at index {index} ({} found).", st.devices.len())));
            }
            // The thermal limit belongs to the card it was applied to: give that card its clock back
            // (with the old card's capabilities still in place to say what its range was).
            if let Some(old) = drv.thermal.gpu {
                thermal_release_and_stop(drv, &st, old, "switching GPU", &mut logs);
            }
            st.thermal_status = drv.thermal.status();
            self.inner.gpu_index.store(index, Ordering::SeqCst);
            st.caps = drv.backend.capabilities(index)?;
            self.inner.history.clear();
            // Whatever the card is already carrying — possibly from an earlier run — decides
            // whether its live voltage can be trusted as "stock".
            refresh_live_voltage_state(drv.backend.as_mut(), index, &mut st);
            message = format!("Selected {} via {}", st.device(index).name, st.backend_name);
        }
        self.inner.events.logs(logs);
        self.log(&message);
        Ok(())
    }

    // ------------------------------------------------------------------ simple reads

    pub fn gpu_index(&self) -> usize {
        self.gpu()
    }

    /// What the selected card can do (a copy).
    pub fn capabilities(&self) -> GpuCapabilities {
        self.state().caps.clone()
    }

    /// The selected device.
    pub fn device(&self) -> GpuDevice {
        let gpu = self.gpu();
        self.state().device(gpu)
    }

    /// Every device the backend reported.
    pub fn devices(&self) -> Vec<GpuDevice> {
        self.state().devices.clone()
    }

    pub fn backend_name(&self) -> String {
        self.state().backend_name.clone()
    }

    pub fn read_graphics_info(&self) -> GpuGraphicsInfo {
        let gpu = self.gpu();
        lock(&self.inner.driver).backend.read_graphics_info(gpu)
    }

    pub fn read_tuning_state(&self) -> Result<GpuTuningState> {
        let gpu = self.gpu();
        lock(&self.inner.driver).backend.read_tuning_state(gpu)
    }

    pub fn read_voltage_lock_mv(&self) -> i32 {
        let gpu = self.gpu();
        lock(&self.inner.driver).backend.read_voltage_lock_mv(gpu)
    }

    /// Curve-editor cap changes share the driver lock with full-profile writes.
    pub fn set_voltage_lock(&self, target_mv: i32) -> Result<()> {
        let gpu = self.gpu();
        let mut guard = lock(&self.inner.driver);
        let drv = &mut *guard;
        // NVIDIA may hold the cap through the same clock lock the thermal limit is holding, and
        // refuses to while another ceiling owns it: hand the lock back first. The next step writes
        // the ceiling again, under whatever cap this sets.
        if drv.thermal.ctl.is_holding() {
            let b = thermal_bounds(&self.state(), 0);
            thermal_restore(drv, gpu, &b, false)?;
        }
        drv.thermal.clock_range_rewritten(true);
        drv.backend.set_voltage_lock(gpu, target_mv)?;
        let mut st = self.state();
        refresh_live_voltage_state(drv.backend.as_mut(), gpu, &mut st);
        Ok(())
    }

    /// The profile currently applied to hardware (`None` = untouched since launch).
    pub fn applied_profile(&self) -> Option<TuningProfile> {
        self.state().applied.clone()
    }

    /// Which gated levers this run has actually written to the card. Separate from a profile's
    /// `xoc_armed`, which is only ever a request: loading a profile arms levers in the editor
    /// without touching the hardware.
    pub fn armed_on_card(&self) -> XocLever {
        self.state().armed_on_card
    }

    /// Latest foreground telemetry sample.
    pub fn latest(&self) -> Option<GpuTelemetry> {
        self.state().latest.clone()
    }

    pub fn history(&self) -> &TelemetryHistory {
        &self.inner.history
    }

    /// True when the graphical curve editor has written per-point deltas. The delta table is the
    /// same storage the voltage-offset slider and the Pascal+ core-offset path use, so an apply
    /// overwrites those edits — Apply says so rather than silently wiping them.
    pub fn manual_curve_active(&self) -> bool {
        self.state().manual_curve_active
    }

    /// Which lever last held the voltage cap, for the status line.
    pub fn voltage_lock_mechanism(&self) -> String {
        self.state().voltage_lock_mechanism.clone()
    }

    pub fn supported_levers(&self) -> XocLever {
        self.state().supported_levers()
    }

    /// What the software temperature limit is doing (a copy, from the engine state alone).
    pub fn soft_thermal_status(&self) -> SoftThermalStatus {
        self.state().thermal_status
    }

    /// One step of the software temperature limit on this reading, as the poll loop takes it
    /// (without its once-a-second pacing). For tests and tools; the poll loop calls it by itself.
    pub fn step_soft_thermal(&self, t: &GpuTelemetry) {
        self.inner.run_soft_thermal(t, false);
    }

    // ------------------------------------------------------------------ voltage ceilings

    /// Highest core voltage actually observed. The V/F curve's last point isn't the ceiling — the
    /// card selects a VID a step above it, and that figure is what the UI should call "stock".
    pub fn observed_max_voltage_mv(&self) -> i32 {
        self.state().observed_max_voltage_mv
    }

    /// Highest core voltage seen while the boost was wound fully open; 0 until measured.
    pub fn observed_max_boosted_voltage_mv(&self) -> i32 {
        self.state().observed_max_boosted_voltage_mv
    }

    pub fn stock_ceiling_mv(&self) -> i32 {
        self.state().stock_ceiling_mv()
    }

    pub fn boost_ceiling_mv(&self) -> i32 {
        self.state().boost_ceiling_mv()
    }

    /// At boost 0 the stock ceiling; wound open, the roof the boost can reach (1100 mV against a
    /// stock 1035 on a 5070 Ti — exactly the range a cap is for).
    pub fn reachable_ceiling_mv(&self, boost_percent: i32) -> i32 {
        self.state().reachable_ceiling_mv(boost_percent)
    }

    /// Feed a live voltage reading into the ceiling estimate. Only samples taken while the card
    /// runs its own voltage, under at least 40% load, count. (C# default `load_percent` = 100.)
    pub fn note_voltage_observation(&self, mv: f64, load_percent: f64) {
        self.state().note_voltage_observation(mv, load_percent);
    }

    /// Seed the ceiling estimate with a figure measured in an earlier session, held to the same
    /// bounds as a live observation so a stale or hand-edited settings file cannot move it.
    pub fn seed_observed_max_voltage(&self, mv: i32) {
        self.state().seed_observed_max_voltage(mv);
    }

    pub fn seed_observed_max_boosted_voltage(&self, mv: i32) {
        self.state().seed_observed_max_boosted_voltage(mv);
    }

    /// Rail ceilings to restore on Reset, in mV; 0 when unknown. Seeded from what was recorded the
    /// first time this GPU was seen (a rail offset survives a reboot, so it cannot be read back).
    pub fn nvvdd_default_max_mv(&self) -> i32 {
        self.state().nvvdd_default_max_mv
    }

    pub fn msvdd_default_max_mv(&self) -> i32 {
        self.state().msvdd_default_max_mv
    }

    pub fn seed_rail_defaults(&self, nvvdd_mv: i32, msvdd_mv: i32) {
        let mut st = self.state();
        if nvvdd_mv > 0 {
            st.nvvdd_default_max_mv = nvvdd_mv;
        }
        if msvdd_mv > 0 {
            st.msvdd_default_max_mv = msvdd_mv;
        }
    }

    /// Stock OCP limits to restore when a lever is disarmed, in milliamps; 0 when unknown.
    pub fn nvvdd_ocp_default_ma(&self) -> i32 {
        self.state().nvvdd_ocp_default_ma
    }

    pub fn msvdd_ocp_default_ma(&self) -> i32 {
        self.state().msvdd_ocp_default_ma
    }

    pub fn seed_ocp_defaults(&self, nvvdd_ma: i32, msvdd_ma: i32) {
        let mut st = self.state();
        if nvvdd_ma > 0 {
            st.nvvdd_ocp_default_ma = nvvdd_ma;
        }
        if msvdd_ma > 0 {
            st.msvdd_ocp_default_ma = msvdd_ma;
        }
    }

    // ------------------------------------------------------------------ apply

    /// Apply every setting in the profile. Returns the per-setting failures plus `Note:` messages
    /// (empty, or only notes, on success — see [`crate::only_notes`]).
    pub fn apply(&self, profile: &TuningProfile) -> Vec<String> {
        let gpu = self.gpu();
        let mut guard = lock(&self.inner.driver);
        let drv = &mut *guard;
        let mut st_guard = self.state();
        let st = &mut *st_guard;
        let caps = st.caps.clone();
        let device_name = st.device(gpu).name;

        if caps.voltage_style == VoltageControlStyle::Percent
            && let Some(owner) = profile.gpu_name.as_deref().filter(|n| !n.is_empty() && *n != device_name)
        {
            return vec![format!(
                "Profile belongs to {owner}. Save a new profile for {device_name}; Intel tuning units differ."
            )];
        }

        let mut a = Attempts::new("—");
        let mut p = profile.clone_profile();
        p.clamp_to(&caps);
        add_clamp_notes(&mut a.errors, &caps, profile, &p);
        let mut curve_offset_mv = p.voltage_offset_mv;
        let mut lock_mv = 0; // hoisted: the verify pass below needs it

        // Order matters a little: raise power/temp limits before pushing clocks, lower clocks
        // before lowering limits.
        let raising = st.applied.as_ref().is_none_or(|ap| p.power_limit_percent >= ap.power_limit_percent);
        if raising {
            if caps.can_set_power_limit {
                a.run("Power limit", drv.backend.set_power_limit(gpu, p.power_limit_percent));
            }
            if caps.can_set_temp_limit {
                a.run("Temp limit", drv.backend.set_temp_limit(gpu, p.temp_limit_c));
            }
        }

        let mut boost_pct = p.voltage_boost_percent;
        if caps.voltage_style == VoltageControlStyle::Offset {
            // One signed offset, no ceiling arithmetic: the vendor applies it to the whole curve.
            curve_offset_mv = p.voltage_offset_mv;
            boost_pct = 0;
            a.log(format!("Voltage offset: {} mV", signed(curve_offset_mv)));
        } else if caps.voltage_style == VoltageControlStyle::Absolute {
            // Two independent levers, the way Afterburner exposes them: the boost raises the
            // ceiling above the V/F table's top, the target caps the core at or below a chosen
            // voltage. A card can carry both, so neither is derived from the other.
            //
            // A ceiling of 0 means the curve read failed; falling through with it would make every
            // target look like "at or above stock" and write nothing, so use the slider's top.
            let stock = st.stock_ceiling_mv();
            let ceiling = if stock > 0 { stock } else { caps.max_voltage_mv };

            // Profiles from before the boost had its own control encoded it in the target voltage:
            // a target above the ceiling meant "boost this far". Honour that when the profile
            // carries no boost of its own.
            boost_pct = p.voltage_boost_percent;
            if boost_pct == 0 && p.target_voltage_mv > ceiling {
                boost_pct = voltage_plan::compute(p.target_voltage_mv, ceiling, st.boost_ceiling_mv()).0;
                if boost_pct > 0 {
                    a.log(format!(
                        "Legacy profile: target {} mV above ceiling → boost {boost_pct}%",
                        p.target_voltage_mv
                    ));
                }
            }

            // Below the ceiling the cap is enforced by the absolute lock. It is measured against
            // the ceiling this profile leaves behind, not the stock one: a boost lifts the roof, and
            // comparing against stock silently dropped a 1050 mV cap on a fully boosted card.
            let reach = st.reachable_ceiling_mv(boost_pct);
            lock_mv = if p.target_voltage_mv > 0 && p.target_voltage_mv < reach { p.target_voltage_mv } else { 0 };

            // A negative offset is how the CLI asks for an undervolt: --uv -90 means "hold 90 mV
            // under the ceiling". This backend caps by absolute voltage, so convert. An explicit
            // cap wins, being the more specific instruction.
            if lock_mv == 0 && p.voltage_offset_mv < 0 && ceiling > 0 {
                lock_mv = caps.min_voltage_mv.max(ceiling + p.voltage_offset_mv);
                p.target_voltage_mv = lock_mv; // keep the applied profile describing what was done
                a.log(format!(
                    "Undervolt {} mV → cap {lock_mv} mV (ceiling {ceiling} mV)",
                    signed(p.voltage_offset_mv)
                ));
            }

            curve_offset_mv = 0;
            a.log(format!(
                "Voltage: boost {boost_pct}%, cap {lock_mv} mV vs stock ceiling {ceiling} mV (curve top {}, seen {})",
                caps.stock_max_voltage_mv, st.observed_max_voltage_mv
            ));
        }

        // A rail that is being disabled must be restored at 0% boost first. Its saved default is
        // the card's 0%-boost figure; restoring that absolute value after boost is applied would
        // cancel the driver's card-specific rail movement. Reapplying boost afterwards lets the
        // driver derive the live NVVDD/MSVDD ceilings itself.
        let rails_turning_off = (st.armed_on_card & !p.xoc_armed) & (XocLever::NVVDD | XocLever::MSVDD);
        if !rails_turning_off.is_empty() {
            let mut at_stock_boost = !caps.can_set_voltage_boost;
            if caps.can_set_voltage_boost {
                at_stock_boost = a.run("Voltage boost (rail restore)", drv.backend.set_voltage_boost(gpu, 0));
            }
            if at_stock_boost {
                write_xoc(drv.backend.as_mut(), gpu, st, &p, &mut a, rails_turning_off, false);
            }
        }

        if caps.can_set_voltage_boost {
            a.run("Voltage boost", drv.backend.set_voltage_boost(gpu, boost_pct));
        }
        // Armed rails take their explicit targets after boost. Disabled rails that were never armed
        // are not touched, so the driver-owned, card-specific baseline remains visible.
        let which = if caps.voltage_style == VoltageControlStyle::Percent {
            XocLever::ALL & !XocLever::CLOCK_RANGE
        } else {
            XocLever::ALL
        };
        write_xoc(drv.backend.as_mut(), gpu, st, &p, &mut a, which, false);

        // The undervolt is enforced by the voltage lock, independent of the p-state clock offset,
        // so both must be written. Either lever routes through set_voltage_curve_offset: NVIDIA's
        // curve flatten and AMD's whole-curve offset (AMD has no editable curve at all, so gating
        // on can_set_voltage_curve alone would drop its write).
        if caps.can_set_voltage_curve || caps.voltage_style == VoltageControlStyle::Offset {
            a.run("Voltage offset", drv.backend.set_voltage_curve_offset(gpu, curve_offset_mv, 0));
        }
        if caps.can_set_core_offset {
            a.run("Core offset", drv.backend.set_core_offset(gpu, p.core_offset_mhz));
        }
        if caps.can_set_memory_offset {
            a.run("Memory offset", drv.backend.set_memory_offset(gpu, p.memory_offset_mhz));
        }
        // The cap goes in LAST, on purpose. The curve-offset and core-offset writes touch the same
        // delta table and have historically cleared it as a side effect.
        if caps.voltage_style == VoltageControlStyle::Absolute {
            a.run("Voltage cap", drv.backend.set_voltage_lock(gpu, lock_mv));
        }

        if !raising {
            if caps.can_set_power_limit {
                a.run("Power limit", drv.backend.set_power_limit(gpu, p.power_limit_percent));
            }
            if caps.can_set_temp_limit {
                a.run("Temp limit", drv.backend.set_temp_limit(gpu, p.temp_limit_c));
            }
        }

        if caps.can_set_zero_rpm {
            a.run("Zero RPM", drv.backend.set_zero_rpm(gpu, p.zero_rpm));
        }
        if caps.can_set_memory_timing {
            a.run("Memory timing", drv.backend.set_memory_timing(gpu, p.memory_timing_level));
        }

        if caps.can_set_fan_speed {
            let (mode, pct, per_fan, curve) =
                (p.fan_mode, p.fixed_fan_percent, p.fixed_fan_percents.clone(), p.fan_curve.clone());
            write_fans(drv, &caps, gpu, mode, pct, &per_fan, &curve, &mut a);
        }

        if st.manual_curve_active && !caps.can_edit_vf_curve && (caps.can_set_voltage_curve || caps.can_set_core_offset)
        {
            a.errors.push(
                "Note: your hand-edited V/F curve was overwritten — the curve editor and these \
                 sliders share the same delta table. Re-open the curve editor to redo it."
                    .into(),
            );
            st.manual_curve_active = false;
        }

        // Intel frequency limits go after offset writes, which may touch the same policy.
        if caps.voltage_style == VoltageControlStyle::Percent && caps.can_lock_clocks {
            write_xoc(drv.backend.as_mut(), gpu, st, &p, &mut a, XocLever::CLOCK_RANGE, false);
        }
        st.applied = Some(p.clone());

        // The software temperature limit. The clock-range write above (every card that can lock its
        // clock gets one) put the user's own range, or the driver's, back on the card, so whatever
        // ceiling the limit held is gone from it: the next poll writes it again. A ceiling already
        // worked out is kept, so re-applying does not start over from the top.
        if caps.can_set_soft_thermal_limit && p.soft_thermal_limit_c > 0 {
            if drv.thermal.gpu != Some(gpu) || drv.thermal.ctl.is_stopped() {
                drv.thermal.stop();
            }
            drv.thermal.gpu = Some(gpu);
            drv.thermal.limit_c = p.soft_thermal_limit_c;
            drv.thermal.clock_range_rewritten(true);
            a.log(format!(
                "Thermal limit (software): {} °C — caps the core clock while Roch GPU runs",
                p.soft_thermal_limit_c
            ));
        } else {
            drv.thermal.stop();
        }
        st.thermal_status = drv.thermal.status();

        // Verify-after-write, still holding the lock: the vendor libraries are not re-entrant and
        // the polling thread reads telemetry through the same handle.
        match drv.backend.read_tuning_state(gpu) {
            Ok(actual) => {
                // Voltage boost is deliberately NOT verified: on several Ada cards the write takes
                // effect while GetCoreVoltageBoostPercent still reports 0.
                if caps.can_set_power_limit
                    && (actual.power_limit_percent - p.power_limit_percent).abs() > 1
                    && !a.any_error_starting("Power limit")
                {
                    a.errors.push(format!(
                        "Power limit: asked for {}%, card reports {}%.",
                        p.power_limit_percent, actual.power_limit_percent
                    ));
                }
                // An undervolt that only partially lands is worse than one that fails loudly.
                if curve_offset_mv < 0 && caps.voltage_style == VoltageControlStyle::Absolute {
                    let (want, got) = (curve_offset_mv, actual.voltage_offset_mv);
                    if got == 0 {
                        a.errors.push(format!(
                            "Undervolt: asked to flatten above {} mV but the curve is unchanged — the driver \
                             rejected the high-voltage points.",
                            caps.stock_max_voltage_mv + want
                        ));
                    } else if (got - want).abs() > 15 {
                        a.errors.push(format!(
                            "Undervolt: asked for {want} mV, curve reads {got} mV — only part of the curve moved. \
                             Re-apply once; the high-point write path is auto-detected on first use."
                        ));
                    }
                }
                if caps.voltage_style == VoltageControlStyle::Offset
                    && curve_offset_mv != actual.voltage_offset_mv
                    && !a.any_error_starting("Voltage offset")
                {
                    a.errors.push(format!(
                        "Voltage offset: asked for {} mV, card reports {} mV.",
                        signed(curve_offset_mv),
                        signed(actual.voltage_offset_mv)
                    ));
                }

                // Read the lock back so an accepted-but-ignored write shows up here rather than as
                // "applied, nothing changed".
                let got_lock = drv.backend.read_voltage_lock_mv(gpu);
                if got_lock >= 0 && !a.any_error_starting("Voltage cap") {
                    let mechanism = drv.backend.voltage_lock_mechanism();
                    a.log(format!("Voltage lock read-back: {got_lock} mV (asked {lock_mv} mV) via {mechanism}"));
                    st.voltage_lock_mechanism = mechanism;
                    // A zero read-back is ambiguous — some drivers accept the lock but never report
                    // it — so that case is logged, not shown as an error.
                    if lock_mv > 0 && (got_lock - lock_mv).abs() > 10 && got_lock > 0 {
                        a.errors.push(format!("Core voltage: asked for {lock_mv} mV, card reports {got_lock} mV."));
                    } else if lock_mv == 0 && got_lock > 0 {
                        a.errors.push(format!(
                            "Core voltage: a {got_lock} mV cap is still in place — press Apply again to clear it."
                        ));
                    }
                }
                if caps.can_set_core_offset
                    && actual.core_offset_mhz != p.core_offset_mhz
                    && !a.any_error_starting("Core offset")
                {
                    a.errors.push(format!(
                        "Core offset: asked for {} MHz, card reports {} MHz.",
                        signed(p.core_offset_mhz),
                        signed(actual.core_offset_mhz)
                    ));
                }
            }
            Err(e) => a.log(format!("Verify failed: {}", e.message)),
        }

        // The levers just moved, so what counts as a stock voltage reading moved with them.
        refresh_live_voltage_state(drv.backend.as_mut(), gpu, st);

        drop(st_guard);
        drop(guard);
        self.inner.events.logs(a.logs);
        a.errors
    }

    // ------------------------------------------------------------------ extreme OC (XOC)

    /// Arm or disarm one lever and write it now, leaving every other lever and all the ungated
    /// settings alone. The profile supplies the value to arm with; disarming ignores it.
    pub fn set_xoc_lever(&self, profile: &TuningProfile, lever: XocLever, on: bool) -> Vec<String> {
        let gpu = self.gpu();
        let mut guard = lock(&self.inner.driver);
        let drv = &mut *guard;
        let mut st_guard = self.state();
        let st = &mut *st_guard;

        let mut a = Attempts::new("-");
        let mut p = profile.clone_profile();
        p.xoc_armed = p.xoc_armed.with(lever, on);
        p.clamp_to(&st.caps);

        let disabling_voltage_rail =
            !on && (lever == XocLever::NVVDD || lever == XocLever::MSVDD) && st.armed_on_card.has(lever);
        if disabling_voltage_rail && st.caps.can_set_voltage_boost {
            // The persisted rail default is the 0%-boost value. Restore it there, then put the live
            // boost back so the driver can derive this card's effective ceiling. Reading the current
            // value keeps this per-lever action from changing an unrelated boost setting.
            let restore_boost =
                drv.backend.read_tuning_state(gpu).map(|s| s.voltage_boost_percent).unwrap_or(p.voltage_boost_percent); // the editor value is the best safe fallback

            let at_stock_boost = a.run("Voltage boost (rail restore)", drv.backend.set_voltage_boost(gpu, 0));
            if at_stock_boost {
                write_xoc(drv.backend.as_mut(), gpu, st, &p, &mut a, lever, false);
                a.run("Voltage boost (rail restore complete)", drv.backend.set_voltage_boost(gpu, restore_boost));
            }
        } else {
            write_xoc(drv.backend.as_mut(), gpu, st, &p, &mut a, lever, false);
        }
        if lever.has(XocLever::CLOCK_RANGE) && st.caps.can_lock_clocks {
            // The lever just wrote the user's range over any thermal ceiling (and, on NVIDIA, over a
            // voltage cap's clock lock); the next step caps inside the new range.
            drv.thermal.clock_range_rewritten(true);
        }

        // Keep the applied profile describing the card, so the summary line agrees with it.
        if let Some(ap) = st.applied.as_mut() {
            ap.xoc_armed = ap.xoc_armed.with(lever, on);
            let v = |x: i32| if on { x } else { 0 };
            if lever == XocLever::NVVDD {
                ap.voltage_rail_max_mv = v(p.voltage_rail_max_mv);
                ap.voltage_rail_floor_mv = v(p.voltage_rail_floor_mv);
            } else if lever == XocLever::MSVDD {
                ap.msvdd_rail_max_mv = v(p.msvdd_rail_max_mv);
                ap.msvdd_rail_floor_mv = v(p.msvdd_rail_floor_mv);
            } else if lever == XocLever::XBAR {
                ap.xbar_offset_mhz = v(p.xbar_offset_mhz);
            } else if lever == XocLever::SYS {
                ap.sys_offset_mhz = v(p.sys_offset_mhz);
            } else if lever == XocLever::VIDEO {
                ap.video_offset_mhz = v(p.video_offset_mhz);
            } else if lever == XocLever::CLOCK_RANGE {
                ap.clock_lock_min_mhz = v(p.clock_lock_min_mhz);
                ap.clock_lock_max_mhz = v(p.clock_lock_max_mhz);
            }
        }
        // The NVVDD ceiling is one of the inputs to the boosted ceiling, so refresh.
        refresh_live_voltage_state(drv.backend.as_mut(), gpu, st);
        a.log(format!("{lever} {}", if on { "armed" } else { "disarmed" }));

        drop(st_guard);
        drop(guard);
        self.inner.events.logs(a.logs);
        a.errors
    }

    /// Restore every tunable to the driver's defaults, every gated lever included.
    pub fn reset_to_defaults(&self) -> Result<()> {
        let gpu = self.gpu();
        {
            let mut guard = lock(&self.inner.driver);
            let drv = &mut *guard;
            let mut st_guard = self.state();
            let st = &mut *st_guard;
            drv.active_curve = None;
            st.manual_curve_active = false;
            // The driver reset below releases the clock lock along with everything else, and the
            // disarmed Clock Range write after it hands the range back to the driver regardless.
            drv.thermal.stop();
            st.thermal_status = drv.thermal.status();
            drv.backend.reset_to_defaults(gpu)?;
            // Every gated lever, through the one routine that knows them all. Listing them here by
            // hand is how the SYS and video offsets came to survive a Reset. A disarmed write_xoc
            // puts each back — rails to the figures recorded on first sight, private domains to no
            // offset, the clock range to unpinned — and picks up any lever added later for free.
            let mut silent = Attempts::silent();
            write_xoc(drv.backend.as_mut(), gpu, st, &TuningProfile::default(), &mut silent, XocLever::ALL, true);
            let name = st.device(gpu).name;
            st.applied = Some(TuningProfile::stock(&st.caps, Some(&name)));
            refresh_live_voltage_state(drv.backend.as_mut(), gpu, st);
        }
        self.log("Reset to driver defaults");
        Ok(())
    }

    // ------------------------------------------------------------------ V/F curve
    // The polling thread touches the backend every second; the curve editor must go through these
    // rather than reaching the backend from the UI thread unlocked.

    /// The extra clock counters, under the same lock as everything else.
    pub fn measure_extra_clocks(&self) -> BTreeMap<String, f64> {
        let gpu = self.gpu();
        lock(&self.inner.driver).backend.measure_extra_clocks(gpu)
    }

    /// Read the editable V/F curve points. Empty when the card/driver doesn't expose them.
    pub fn read_vf_curve(&self) -> Result<Vec<VfCurveSample>> {
        let gpu = self.gpu();
        lock(&self.inner.driver).backend.read_vf_curve(gpu)
    }

    /// Write explicit per-point curve targets; every other point returns to stock. Empty clears.
    pub fn set_vf_curve_targets(&self, targets: &[VfCurveSample]) -> Result<()> {
        let gpu = self.gpu();
        let mut drv = lock(&self.inner.driver);
        drv.backend.set_vf_curve_targets(gpu, targets)?;
        self.state().manual_curve_active = !targets.is_empty();
        Ok(())
    }

    /// Read what the driver currently has, as a profile (seeds the sliders on launch).
    pub fn read_current_as_profile(&self) -> Result<TuningProfile> {
        let gpu = self.gpu();
        let mut drv = lock(&self.inner.driver);
        let holding = drv.thermal.ctl.is_holding();
        let b = drv.backend.as_mut();
        let mut s = b.read_tuning_state(gpu)?;
        let lk = b.read_voltage_lock_mv(gpu);
        let st = self.state();
        let c = &st.caps;
        // A ceiling the thermal limit is holding is not the user's clock range: show theirs.
        if holding {
            (s.locked_clock_min_mhz, s.locked_clock_max_mhz) = thermal_bounds(&st, 0).restore;
        }
        let soft_thermal_limit_c = st.applied.as_ref().map(|a| a.soft_thermal_limit_c).unwrap_or(0);
        let percent = c.voltage_style == VoltageControlStyle::Percent;
        Ok(TuningProfile {
            name: "Current".into(),
            xoc_armed: if percent && c.can_lock_clocks && (s.locked_clock_min_mhz > 0 || s.locked_clock_max_mhz > 0) {
                XocLever::CLOCK_RANGE
            } else {
                XocLever::NONE
            },
            gpu_name: Some(st.device(gpu).name),
            core_offset_mhz: s.core_offset_mhz,
            memory_offset_mhz: s.memory_offset_mhz,
            power_limit_percent: s.power_limit_percent,
            temp_limit_c: s.temp_limit_c,
            voltage_boost_percent: s.voltage_boost_percent,
            // Not carried on an absolute-voltage card: there the figure is inferred against the top
            // of the V/F table, a different baseline from the cap's, and Apply's --uv fallback reads
            // a negative offset as "hold this far under the ceiling" — a 1000 mV cap would come
            // back as a 795 mV one.
            voltage_offset_mv: if c.voltage_style == VoltageControlStyle::Offset { s.voltage_offset_mv } else { 0 },
            voltage_rail_max_mv: s.voltage_rail_max_mv,
            msvdd_rail_max_mv: s.msvdd_rail_max_mv,
            voltage_rail_floor_mv: s.voltage_rail_floor_mv,
            msvdd_rail_floor_mv: s.msvdd_rail_floor_mv,
            clock_lock_min_mhz: s.locked_clock_min_mhz,
            clock_lock_max_mhz: s.locked_clock_max_mhz,
            xbar_offset_mhz: s.xbar_offset_mhz,
            sys_offset_mhz: s.sys_offset_mhz,
            video_offset_mhz: s.video_offset_mhz,
            zero_rpm: s.zero_rpm,
            memory_timing_level: s.memory_timing_level,
            // The cap is whatever lock is actually on the card, not inferred from the boost (that
            // made a pure boost read back as a cap above the ceiling). -1 = the backend can't
            // report a lock: fall back to the old inference so the slider lands somewhere sane.
            target_voltage_mv: if lk >= 0 {
                lk
            } else {
                voltage_plan::ceiling_mv(
                    s.voltage_boost_percent,
                    s.voltage_offset_mv,
                    st.stock_ceiling_mv(),
                    st.boost_ceiling_mv(),
                    c.stock_max_voltage_mv,
                )
            },
            fan_mode: if s.hardware_fan_curve.is_some() {
                s.detected_fan_mode.unwrap_or(FanMode::Curve)
            } else if s.fan_manual {
                FanMode::Fixed
            } else {
                FanMode::Auto
            },
            fan_curve: s.hardware_fan_curve.as_ref().map(FanCurve::clone_config).unwrap_or_default(),
            fixed_fan_percent: s.fan_percent,
            // Not a driver setting, so the card cannot report it: the one this run is enforcing.
            soft_thermal_limit_c,
            ..TuningProfile::default()
        })
    }

    // ------------------------------------------------------------------ fans

    /// Write only the fans. The fan window is a decision about fans, so it does not carry a
    /// half-finished clock edit onto the card as a side effect. `per_fan` empty means one duty for
    /// every fan.
    pub fn set_fans(
        &self,
        mode: FanMode,
        percent: i32,
        per_fan: &[i32],
        curve: &FanCurve,
        zero_rpm: Option<bool>,
    ) -> Vec<String> {
        let caps = self.capabilities();
        if !caps.can_set_fan_speed {
            return vec!["This card exposes no fan control.".into()];
        }
        let gpu = self.gpu();
        let mut a = Attempts::new("-");
        {
            let mut guard = lock(&self.inner.driver);
            let drv = &mut *guard;
            write_fans(drv, &caps, gpu, mode, percent, per_fan, curve, &mut a);
            if let Some(z) = zero_rpm
                && caps.can_set_zero_rpm
            {
                a.run("Zero RPM", drv.backend.set_zero_rpm(gpu, z));
            }

            // Keep the applied profile describing the card.
            let mut st = self.state();
            if a.errors.is_empty()
                && let Some(ap) = st.applied.as_mut()
            {
                ap.fan_mode = mode;
                ap.fixed_fan_percent = percent;
                ap.fixed_fan_percents = per_fan.to_vec();
                ap.fan_curve = curve.clone_config();
                if let Some(z) = zero_rpm
                    && caps.can_set_zero_rpm
                {
                    ap.zero_rpm = z;
                }
            }
        }
        self.inner.events.logs(a.logs);
        a.errors
    }

    /// Safety: on exit (or from a crash handler) hand fans back to the driver, so a fixed low fan
    /// speed can't persist after the app that was supposed to manage it is gone.
    ///
    /// Only for a curve *this* app runs. A driver-owned curve keeps running whether this process
    /// exists or not, and resetting it here threw away the user's fan settings on every exit.
    pub fn release_fan_control(&self) {
        let gpu = self.gpu();
        let caps = self.capabilities();
        let mut released = false;
        {
            let mut drv = lock(&self.inner.driver);
            drv.active_curve = None;
            if caps.can_set_fan_speed && !caps.fan_curve_is_hardware {
                released = drv.backend.set_fan_auto(gpu).is_ok();
            }
        }
        if released {
            self.log("Fans returned to auto");
        }
    }

    /// Safety, like [`Self::release_fan_control`]: on exit hand a clock the software temperature
    /// limit is holding back to the user's own range (or the driver), and stop the limit — it is
    /// enforced by this process, and a ceiling left behind would outlive it with nobody managing it.
    /// The next Apply starts it again. Safe to call any number of times.
    pub fn release_soft_thermal_limit(&self) {
        self.release_soft_thermal(true);
    }

    /// `wait`: false from `Drop` when the poll could not be stopped — the driver lock may be held
    /// by a call that never returns, and an exit must not hang behind it.
    fn release_soft_thermal(&self, wait: bool) {
        let mut logs = Vec::new();
        {
            let mut guard = if wait {
                lock(&self.inner.driver)
            } else {
                match self.inner.driver.try_lock() {
                    Ok(g) => g,
                    Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
                    Err(std::sync::TryLockError::WouldBlock) => return,
                }
            };
            let drv = &mut *guard;
            let Some(gpu) = drv.thermal.gpu else { return };
            let mut st = self.state();
            thermal_release_and_stop(drv, &st, gpu, "released", &mut logs);
            st.thermal_status = drv.thermal.status();
        }
        self.inner.events.logs(logs);
    }

    // ------------------------------------------------------------------ polling

    /// Set while no window of ours is on screen (minimised to tray, monitor closed). The poll then
    /// does the least work that still keeps a fan curve running, and no driver call at all when no
    /// curve is active — a full sample is ~13 ms of synchronous NVAPI work.
    pub fn set_background_mode(&self, on: bool) {
        let was = self.inner.background_mode.swap(on, Ordering::SeqCst);
        // Leaving the background: take the first full sample now instead of up to an interval
        // later. (`try_lock`: never wait behind a stop that is waiting on the driver.)
        if was
            && !on
            && let Ok(poll) = self.poll.try_lock()
            && let Some(p) = poll.as_ref()
        {
            let _ = p.cancel.send(PollSignal::Wake);
        }
    }

    pub fn background_mode(&self) -> bool {
        self.inner.background_mode.load(Ordering::SeqCst)
    }

    pub fn is_polling(&self) -> bool {
        lock(&self.poll).is_some()
    }

    /// Start (or restart) the polling thread. Fails if a previous poll could not be stopped.
    pub fn start_polling(&self, interval_ms: u64) -> Result<()> {
        self.stop_polling()?;
        let (cancel_tx, cancel_rx) = mpsc::channel::<PollSignal>();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let inner = Arc::clone(&self.inner);
        let interval = Duration::from_millis(interval_ms);
        let handle = thread::Builder::new()
            .name("RochGPU poll".into())
            .spawn(move || {
                inner.poll_loop(interval, cancel_rx);
                let _ = done_tx.send(());
            })
            .map_err(|e| GpuError::context("Could not start polling", e))?;
        let thread_id = handle.thread().id();
        *lock(&self.poll) = Some(Poller { cancel: cancel_tx, done: done_rx, handle: Some(handle), thread_id });
        Ok(())
    }

    /// Stop polling, waiting up to 10 s for a poll that is inside the driver.
    pub fn stop_polling(&self) -> Result<()> {
        self.stop_polling_timeout(DEFAULT_STOP_TIMEOUT)
    }

    /// Stop polling. On timeout the thread is kept (it still owns a reference to the backend, so
    /// the library is never unloaded under a stalled call) and a later stop can finish the job.
    pub fn stop_polling_timeout(&self, timeout: Duration) -> Result<()> {
        let mut slot = lock(&self.poll);
        let Some(poller) = slot.as_mut() else { return Ok(()) };
        let _ = poller.cancel.send(PollSignal::Stop);
        if thread::current().id() == poller.thread_id {
            // Called from a listener on the poll thread itself: it cannot wait for itself. The loop
            // exits as soon as the listener returns.
            *slot = None;
            return Ok(());
        }
        match poller.done.recv_timeout(timeout) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                if let Some(h) = poller.handle.take() {
                    let _ = h.join();
                }
                *slot = None;
                Ok(())
            }
            Err(RecvTimeoutError::Timeout) => Err(GpuError::new(
                "GPU polling did not stop in time; the driver connection was kept open. Try closing again after \
                 the driver responds.",
            )),
        }
    }
}

impl Drop for TuningService {
    /// The C# `Dispose`: stop polling, then release the backend. The backend lives in state shared
    /// with the polling thread, so if the poll cannot be stopped in time it is released only when
    /// that thread finally leaves the driver.
    fn drop(&mut self) {
        let stopped = self.stop_polling().is_ok();
        // After the poll, so no step can take the clock again behind the release.
        self.release_soft_thermal(stopped);
    }
}
