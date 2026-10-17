//! The software temperature limit: a small controller that holds the GPU under a chosen temperature
//! by capping the graphics clock, for cards whose driver offers no thermal limit of its own (an RTX
//! 5070 Ti reports no NVAPI thermal policy and NVML refuses its target temperature).
//!
//! It is pure logic: [`SoftThermalController::step`] takes one temperature reading and says what to
//! write, and the caller ([`super::TuningService`]'s poll loop, about once a second) does the write
//! under the driver lock and reports back with [`SoftThermalController::confirm`]. The lever is the
//! same NVML locked-clock window the XOC Clock Range uses, never narrower than the user's own range.
//!
//! # Behaviour
//!
//! * At or above the limit it lowers a ceiling on the graphics clock, 15 MHz a tick, 30 from 2 °C
//!   over, 45 from 4 °C over. The first cap starts from the clock the card is running, so it bites
//!   on the first tick. Within 2 °C of the limit and already cooling it waits: the temperature lags
//!   the clock by seconds, and stepping on through that lag is how a controller like this ends up
//!   far below where it needed to be.
//! * Between the limit and [`HYSTERESIS_C`] below it, it holds.
//! * At or below that it raises the ceiling 15 MHz a tick, slower than it lowers it: the card heats
//!   up faster than a cooler can catch it. Once the ceiling reaches the top (the card's maximum, or
//!   the user's own ceiling) it lets go: the user's range goes back, or the clock goes back to the
//!   driver.
//! * Never below a floor: half the card's maximum clock, the card's lowest lockable clock or the
//!   user's own floor, whichever is highest. Below that the driver's own slowdown is the protection.
//! * A write happens only when the window changes, except that a held window is written again every
//!   [`REASSERT_TICKS`] ticks: a display-driver reset clears the lock without telling anyone, and
//!   re-asserting is what puts it back.
//! * A failed write backs off for [`BACKOFF_TICKS`] ticks; after [`MAX_FAILURES`] in a row the
//!   controller stops until the next Apply rather than hammer a driver that keeps refusing.

/// How far below the limit the temperature must fall before the ceiling is raised again, in °C.
pub const HYSTERESIS_C: f64 = 3.0;
/// The clock grid, and the smallest step, in MHz. NVIDIA clocks move in 15 MHz bins.
pub const STEP_MHZ: i32 = 15;
/// The largest step down in one tick, in MHz.
pub const MAX_STEP_DOWN_MHZ: i32 = 45;
/// The ceiling never drops below this fraction of the card's maximum clock.
pub const FLOOR_FRACTION: f64 = 0.5;
/// A held, unchanged window is written again after this many ticks.
pub const REASSERT_TICKS: u32 = 30;
/// Ticks to wait after a failed write.
pub const BACKOFF_TICKS: u32 = 30;
/// Failed writes in a row before the controller gives up until the next Apply.
pub const MAX_FAILURES: u32 = 3;

/// The clock window the controller works inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThermalBounds {
    /// The highest ceiling it holds; climbing back to it releases the clock. 0 = nothing to cap.
    pub top_mhz: i32,
    /// The lowest ceiling it holds.
    pub floor_mhz: i32,
    /// The lower edge written with every cap: the user's own floor, or 0 for none.
    pub min_mhz: i32,
    /// What a release writes: the user's own range when the Clock Range lever is armed (as the
    /// lever itself writes it), else 0/0, which hands the clock back to the driver.
    pub restore: (i32, i32),
}

impl ThermalBounds {
    /// Bounds for a card that can lock `card_min..=card_max` MHz, with the user's armed Clock Range
    /// (`(min, max)`, 0 = that side not asked for) when there is one, and any other clock ceiling
    /// already on the card that must not be lifted (`other_cap_mhz`, 0 = none) — on NVIDIA the
    /// voltage cap can be enforced through the same lock.
    pub fn new(card_min: i32, card_max: i32, user: Option<(i32, i32)>, other_cap_mhz: i32) -> Self {
        let (user_lo, user_hi) = user.unwrap_or((0, 0));
        // A floor-only range pins the clock at that floor (that is how the lever writes it).
        let user_top = if user_hi > 0 {
            user_hi
        } else if user_lo > 0 {
            user_lo
        } else {
            card_max
        };
        let mut top = if card_max > 0 { card_max.min(user_top) } else { user_top.max(0) };
        if other_cap_mhz > 0 {
            top = top.min(other_cap_mhz);
        }
        let fraction = (card_max.max(0) as f64 * FLOOR_FRACTION) as i32;
        let fraction = (fraction + STEP_MHZ - 1) / STEP_MHZ * STEP_MHZ;
        let floor = fraction.max(card_min).max(user_lo).min(top).max(0);
        ThermalBounds { top_mhz: top.max(0), floor_mhz: floor, min_mhz: user_lo.max(0), restore: (user_lo, user_hi) }
    }
}

/// What one tick asks the caller to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThermalAction {
    /// Nothing.
    None,
    /// Hold the graphics clock inside `min_mhz..=max_mhz` (`set_clock_range`).
    Cap { min_mhz: i32, max_mhz: i32 },
    /// Let go: write `min_mhz`/`max_mhz` (the user's range, or 0/0 for the driver).
    Release { min_mhz: i32, max_mhz: i32 },
}

/// What [`SoftThermalController::confirm`] reports about the controller after a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmed {
    Ok,
    /// The write failed; the controller waits [`BACKOFF_TICKS`] before trying again.
    BackingOff,
    /// The write failed [`MAX_FAILURES`] times in a row; the controller has stopped.
    Stopped,
}

/// See the module docs.
#[derive(Debug, Clone)]
pub struct SoftThermalController {
    /// The ceiling being held; `None` while the clock is free.
    ceiling: Option<i32>,
    /// The window last written while holding; `None` = the card may not carry it (never written,
    /// written by someone else since, or a driver reset).
    written: Option<(i32, i32)>,
    /// The action handed out and not yet confirmed.
    pending: ThermalAction,
    top_at_pending: i32,
    last_temp_c: f64,
    since_write: u32,
    backoff: u32,
    failures: u32,
    stopped: bool,
}

impl Default for SoftThermalController {
    fn default() -> Self {
        Self::new()
    }
}

/// How far to step down for a reading `over` °C at or above the limit.
pub fn step_down_mhz(over_c: f64) -> i32 {
    let over = if over_c.is_finite() { over_c.max(0.0) } else { 0.0 };
    (STEP_MHZ * (1 + (over / 2.0).floor() as i32)).min(MAX_STEP_DOWN_MHZ)
}

impl SoftThermalController {
    pub fn new() -> Self {
        Self {
            ceiling: None,
            written: None,
            pending: ThermalAction::None,
            top_at_pending: 0,
            last_temp_c: f64::NAN,
            since_write: 0,
            backoff: 0,
            failures: 0,
            stopped: false,
        }
    }

    /// The clock is being held down.
    pub fn is_holding(&self) -> bool {
        self.ceiling.is_some()
    }

    /// The ceiling being held, in MHz.
    pub fn ceiling_mhz(&self) -> Option<i32> {
        self.ceiling
    }

    /// The window last written while holding, if the card is known to carry it.
    pub fn written(&self) -> Option<(i32, i32)> {
        self.written
    }

    /// The last temperature stepped, NaN before the first.
    pub fn last_temp_c(&self) -> f64 {
        self.last_temp_c
    }

    /// Gave up after repeated driver errors.
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// True when the next step at `temp_c` would start a hold, so the caller knows to fetch the
    /// current clock first (a background reading carries only the temperature).
    pub fn would_engage(&self, limit_c: i32, temp_c: f64) -> bool {
        !self.stopped && self.backoff == 0 && self.ceiling.is_none() && temp_c.is_finite() && temp_c >= limit_c as f64
    }

    /// Back to the start: not holding, nothing written, errors forgotten. The caller has already
    /// put the clock back (or the card was reset).
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// The card may no longer carry the written window (an apply rewrote the clock range, a driver
    /// reset cleared it): keep the ceiling, write it again on the next step.
    pub fn forget_written(&mut self) {
        self.written = None;
    }

    /// One reading. `clock_mhz` is the card's current graphics clock (NaN or 0 when unknown); it
    /// only matters when a hold starts.
    pub fn step(&mut self, limit_c: i32, temp_c: f64, clock_mhz: f64, b: &ThermalBounds) -> ThermalAction {
        self.pending = ThermalAction::None;
        if !temp_c.is_finite() || self.stopped || limit_c <= 0 {
            return ThermalAction::None;
        }
        let prev = self.last_temp_c;
        self.last_temp_c = temp_c;
        if self.backoff > 0 {
            self.backoff -= 1;
            return ThermalAction::None;
        }
        self.since_write = self.since_write.saturating_add(1);
        let top = b.top_mhz;
        if top <= 0 {
            return ThermalAction::None;
        }
        let floor = b.floor_mhz.clamp(0, top);
        let limit = limit_c as f64;
        let over = temp_c - limit;

        let next = match self.ceiling {
            None => {
                if over < 0.0 {
                    return ThermalAction::None;
                }
                // Start where the card is, so the first cap bites; on the grid, under the top.
                let start = if clock_mhz.is_finite() && clock_mhz >= 1.0 { (clock_mhz as i32).min(top) } else { top };
                let start = start / STEP_MHZ * STEP_MHZ;
                (start - step_down_mhz(over)).max(floor)
            }
            Some(c) => {
                let c = c.clamp(floor, top);
                if over >= 0.0 {
                    let cooling = prev.is_finite() && temp_c < prev;
                    if over < 2.0 && cooling { c } else { (c - step_down_mhz(over)).max(floor) }
                } else if temp_c <= limit - HYSTERESIS_C {
                    let up = c + STEP_MHZ;
                    if up >= top {
                        self.ceiling = None;
                        self.written = None;
                        self.pending = ThermalAction::Release { min_mhz: b.restore.0, max_mhz: b.restore.1 };
                        self.top_at_pending = top;
                        return self.pending;
                    }
                    up
                } else {
                    c
                }
            }
        };
        self.ceiling = Some(next);
        let range = (b.min_mhz.min(next), next);
        if self.written == Some(range) && self.since_write < REASSERT_TICKS {
            return ThermalAction::None;
        }
        self.pending = ThermalAction::Cap { min_mhz: range.0, max_mhz: range.1 };
        self.pending
    }

    /// How the write for the last step's action went.
    pub fn confirm(&mut self, ok: bool) -> Confirmed {
        let action = std::mem::replace(&mut self.pending, ThermalAction::None);
        if ok {
            if let ThermalAction::Cap { min_mhz, max_mhz } = action {
                self.written = Some((min_mhz, max_mhz));
            }
            self.since_write = 0;
            self.failures = 0;
            return Confirmed::Ok;
        }
        self.written = None;
        // A release that did not land leaves the cap on the card: keep holding at the top, so the
        // next cool step tries the release again.
        if matches!(action, ThermalAction::Release { .. }) {
            self.ceiling = Some(self.top_at_pending);
        }
        self.failures += 1;
        if self.failures >= MAX_FAILURES {
            self.stopped = true;
            return Confirmed::Stopped;
        }
        self.backoff = BACKOFF_TICKS;
        Confirmed::BackingOff
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 5070 Ti-like card: lockable 210..3090 MHz, nothing else holding the clock.
    fn card() -> ThermalBounds {
        ThermalBounds::new(210, 3090, None, 0)
    }

    fn cap(max: i32) -> ThermalAction {
        ThermalAction::Cap { min_mhz: 0, max_mhz: max }
    }

    /// Step and, when something is to be written, confirm it landed.
    fn tick(c: &mut SoftThermalController, limit: i32, temp: f64, clock: f64, b: &ThermalBounds) -> ThermalAction {
        let a = c.step(limit, temp, clock, b);
        if a != ThermalAction::None {
            c.confirm(true);
        }
        a
    }

    #[test]
    fn bounds_from_the_card_and_the_user() {
        let b = card();
        assert_eq!((b.top_mhz, b.floor_mhz, b.min_mhz, b.restore), (3090, 1545, 0, (0, 0)));
        // The user's ceiling is the top; their floor is the lower edge and the floor.
        let b = ThermalBounds::new(210, 3090, Some((1800, 2700)), 0);
        assert_eq!((b.top_mhz, b.floor_mhz, b.min_mhz, b.restore), (2700, 1800, 1800, (1800, 2700)));
        // A floor-only range pins the clock there: nothing left to move.
        let b = ThermalBounds::new(210, 3090, Some((2000, 0)), 0);
        assert_eq!((b.top_mhz, b.floor_mhz), (2000, 2000));
        // Another ceiling already on the card is never lifted.
        assert_eq!(ThermalBounds::new(210, 3090, None, 2400).top_mhz, 2400);
        // A card that cannot report its maximum has nothing to cap.
        assert_eq!(ThermalBounds::new(210, 0, None, 0).top_mhz, 0);
    }

    #[test]
    fn steps_scale_with_how_far_over() {
        assert_eq!([0.0, 1.9, 2.0, 3.5, 4.0, 20.0, f64::NAN].map(step_down_mhz), [15, 15, 30, 30, 45, 45, 15]);
    }

    #[test]
    fn below_the_limit_nothing_happens() {
        let mut c = SoftThermalController::new();
        assert_eq!(c.step(83, 82.0, 2800.0, &card()), ThermalAction::None);
        assert!(!c.is_holding());
        assert_eq!(c.last_temp_c(), 82.0);
        assert_eq!(c.step(83, f64::NAN, 2800.0, &card()), ThermalAction::None, "no reading, no action");
        assert_eq!(c.step(0, 99.0, 2800.0, &card()), ThermalAction::None, "0 is off");
    }

    #[test]
    fn the_first_cap_starts_from_the_live_clock() {
        let mut c = SoftThermalController::new();
        assert!(c.would_engage(83, 84.0));
        // 2662 MHz live → 2655 on the grid, minus one step.
        assert_eq!(tick(&mut c, 83, 84.0, 2662.0, &card()), cap(2640));
        assert_eq!(c.ceiling_mhz(), Some(2640));
        // No clock (a background reading): start from the top.
        let mut c = SoftThermalController::new();
        assert_eq!(tick(&mut c, 83, 88.0, f64::NAN, &card()), cap(3090 - 45));
    }

    #[test]
    fn steps_down_while_hot_and_never_below_the_floor() {
        let mut c = SoftThermalController::new();
        let b = card();
        tick(&mut c, 80, 81.0, 1600.0, &b); // 1590 - 15 = 1575
        assert_eq!(c.ceiling_mhz(), Some(1575));
        assert_eq!(tick(&mut c, 80, 85.0, 0.0, &b), cap(1545), "45 down, held at the floor");
        assert_eq!(tick(&mut c, 80, 86.0, 0.0, &b), ThermalAction::None, "at the floor: nothing new to write");
        assert_eq!(c.ceiling_mhz(), Some(1545));
    }

    #[test]
    fn hysteresis_holds_then_climbs_then_releases() {
        let mut c = SoftThermalController::new();
        let b = ThermalBounds::new(210, 2700, None, 0);
        assert_eq!(tick(&mut c, 80, 80.0, 2700.0, &b), cap(2685));
        // 79 and 78 °C: inside the band, hold.
        assert_eq!(tick(&mut c, 80, 79.0, 0.0, &b), ThermalAction::None);
        assert_eq!(tick(&mut c, 80, 78.0, 0.0, &b), ThermalAction::None);
        assert_eq!(c.ceiling_mhz(), Some(2685));
        // 77 °C = limit - 3: one step up reaches the top, so let go.
        assert_eq!(tick(&mut c, 80, 77.0, 0.0, &b), ThermalAction::Release { min_mhz: 0, max_mhz: 0 });
        assert!(!c.is_holding());
        assert_eq!(c.written(), None);
    }

    #[test]
    fn climbs_one_step_a_tick() {
        let mut c = SoftThermalController::new();
        let b = card();
        tick(&mut c, 80, 90.0, 2400.0, &b);
        assert_eq!(c.ceiling_mhz(), Some(2355));
        for want in [2370, 2385, 2400] {
            assert_eq!(tick(&mut c, 80, 70.0, 0.0, &b), cap(want));
        }
    }

    #[test]
    fn just_over_and_already_cooling_waits() {
        let mut c = SoftThermalController::new();
        let b = card();
        tick(&mut c, 80, 81.0, 2415.0, &b);
        assert_eq!(c.ceiling_mhz(), Some(2400));
        tick(&mut c, 80, 81.5, 0.0, &b);
        assert_eq!(c.ceiling_mhz(), Some(2385), "rising: step");
        assert_eq!(tick(&mut c, 80, 80.5, 0.0, &b), ThermalAction::None, "falling, under 2 °C over: wait");
        assert_eq!(c.ceiling_mhz(), Some(2385));
        tick(&mut c, 80, 80.0, 0.0, &b);
        assert_eq!(c.ceiling_mhz(), Some(2385), "still falling");
        tick(&mut c, 80, 80.0, 0.0, &b);
        assert_eq!(c.ceiling_mhz(), Some(2370), "flat at the limit: step");
        tick(&mut c, 80, 82.5, 0.0, &b);
        assert_eq!(c.ceiling_mhz(), Some(2340), "2 °C over steps by 30");
    }

    #[test]
    fn an_unchanged_ceiling_is_not_rewritten_until_the_reassert() {
        let mut c = SoftThermalController::new();
        let b = card();
        tick(&mut c, 80, 81.0, 2415.0, &b);
        let mut writes = 0;
        for _ in 0..REASSERT_TICKS - 1 {
            if tick(&mut c, 80, 79.0, 0.0, &b) != ThermalAction::None {
                writes += 1;
            }
        }
        assert_eq!(writes, 0, "holding in the band writes nothing");
        assert_eq!(tick(&mut c, 80, 79.0, 0.0, &b), cap(2400), "re-asserted after {REASSERT_TICKS} ticks");
        assert_eq!(tick(&mut c, 80, 79.0, 0.0, &b), ThermalAction::None);
    }

    #[test]
    fn a_forgotten_window_is_written_again_at_once() {
        let mut c = SoftThermalController::new();
        let b = card();
        tick(&mut c, 80, 81.0, 2415.0, &b);
        c.forget_written();
        assert_eq!(tick(&mut c, 80, 79.0, 0.0, &b), cap(2400), "same ceiling, rewritten");
    }

    #[test]
    fn the_users_range_is_the_frame() {
        let mut c = SoftThermalController::new();
        let b = ThermalBounds::new(210, 3090, Some((1800, 2500)), 0);
        // Live clock above the user's ceiling (it cannot be, but the top still wins).
        assert_eq!(tick(&mut c, 80, 81.0, 2800.0, &b), ThermalAction::Cap { min_mhz: 1800, max_mhz: 2475 });
        for _ in 0..40 {
            tick(&mut c, 80, 95.0, 0.0, &b);
        }
        assert_eq!(c.ceiling_mhz(), Some(1800), "never below the user's floor");
        for _ in 0..60 {
            let a = tick(&mut c, 80, 60.0, 0.0, &b);
            if let ThermalAction::Release { min_mhz, max_mhz } = a {
                assert_eq!((min_mhz, max_mhz), (1800, 2500), "releases to exactly the user's range");
                return;
            }
        }
        panic!("never released");
    }

    #[test]
    fn a_narrower_frame_pulls_the_ceiling_in() {
        let mut c = SoftThermalController::new();
        tick(&mut c, 80, 81.0, 2800.0, &card());
        assert_eq!(c.ceiling_mhz(), Some(2775));
        // The user armed a 2400 ceiling meanwhile: hold under it.
        let b = ThermalBounds::new(210, 3090, Some((0, 2400)), 0);
        assert_eq!(tick(&mut c, 80, 79.0, 0.0, &b), cap(2400));
    }

    #[test]
    fn failures_back_off_then_stop() {
        let mut c = SoftThermalController::new();
        let b = card();
        assert_eq!(c.step(80, 81.0, 2415.0, &b), cap(2400));
        assert_eq!(c.confirm(false), Confirmed::BackingOff);
        for _ in 0..BACKOFF_TICKS {
            assert_eq!(c.step(80, 90.0, 0.0, &b), ThermalAction::None, "backing off");
        }
        assert!(matches!(c.step(80, 90.0, 0.0, &b), ThermalAction::Cap { .. }), "tries again");
        assert_eq!(c.confirm(false), Confirmed::BackingOff);
        for _ in 0..BACKOFF_TICKS {
            c.step(80, 90.0, 0.0, &b);
        }
        assert!(matches!(c.step(80, 90.0, 0.0, &b), ThermalAction::Cap { .. }));
        assert_eq!(c.confirm(false), Confirmed::Stopped);
        assert!(c.is_stopped());
        for _ in 0..100 {
            assert_eq!(c.step(80, 99.0, 0.0, &b), ThermalAction::None, "stopped: no more writes");
        }
        c.reset();
        assert!(!c.is_stopped());
        assert!(matches!(c.step(80, 99.0, 0.0, &b), ThermalAction::Cap { .. }), "an apply starts it again");
    }

    #[test]
    fn a_success_clears_the_failure_count() {
        let mut c = SoftThermalController::new();
        let b = card();
        for _ in 0..(MAX_FAILURES * 3) {
            c.step(80, 81.0, 2415.0, &b);
            c.confirm(false);
            for _ in 0..BACKOFF_TICKS {
                c.step(80, 81.0, 0.0, &b);
            }
            c.forget_written();
            if c.step(80, 81.0, 0.0, &b) != ThermalAction::None {
                assert_eq!(c.confirm(true), Confirmed::Ok);
            }
        }
        assert!(!c.is_stopped(), "never three failures in a row");
    }

    #[test]
    fn a_failed_release_is_retried() {
        let mut c = SoftThermalController::new();
        let b = ThermalBounds::new(210, 2700, None, 0);
        tick(&mut c, 80, 80.0, 2700.0, &b);
        assert!(matches!(c.step(80, 70.0, 0.0, &b), ThermalAction::Release { .. }));
        assert_eq!(c.confirm(false), Confirmed::BackingOff);
        assert!(c.is_holding(), "the cap is still on the card");
        for _ in 0..BACKOFF_TICKS {
            c.step(80, 70.0, 0.0, &b);
        }
        assert!(matches!(c.step(80, 70.0, 0.0, &b), ThermalAction::Release { .. }));
        assert_eq!(c.confirm(true), Confirmed::Ok);
        assert!(!c.is_holding());
    }
}
