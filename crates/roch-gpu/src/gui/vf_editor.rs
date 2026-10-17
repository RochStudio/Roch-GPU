//! The voltage/frequency curve editor's state, axes, hit-testing and edits (C#
//! `Controls/VfCurveEditor.cs`) — the Afterburner Ctrl+F equivalent, without any drawing.
//!
//! * drag a point: move that point's frequency (voltage is fixed by the card)
//! * Ctrl + drag: move the point AND flatten everything to its right (the undervolt)
//! * Shift + drag: move every point from here rightwards by the same amount
//! * click a point: select it, then Up/Down nudge it by one driver step (Shift for five steps,
//!   Ctrl to flatten above it, Left/Right to walk the curve)
//! * double-click a point: return just that point to stock
//! * right-click: return every point to stock
//!
//! Coordinates are the control's own: (0,0) is its top-left corner, `w`×`h` its size.

use roch_core::VfCurveSample;
use roch_core::util::round_even;

pub const PAD_L: f64 = 58.0;
pub const PAD_R: f64 = 18.0;
pub const PAD_T: f64 = 18.0;
pub const PAD_B: f64 = 42.0;
const HIT_RADIUS: f64 = 10.0;

/// Fixed frame: 700–1200 mV by 600–3400 MHz. A fixed frame keeps the curve's shape comparable
/// between cards and runs, and stops the plot rescaling under the cursor mid-drag. Widened only if
/// a card's curve genuinely falls outside it.
const AXIS_F_MIN: i32 = 600;
const AXIS_F_MAX: i32 = 3400;

/// The plotted ranges: voltage on X, frequency on Y.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Axes {
    pub v_min: i32,
    pub v_max: i32,
    pub f_min: i32,
    pub f_max: i32,
}

/// The control's size, for mapping between data and screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VfGeom {
    pub w: f64,
    pub h: f64,
}

impl VfGeom {
    pub fn plot_w(&self) -> f64 {
        (self.w - PAD_L - PAD_R).max(1.0)
    }
    pub fn plot_h(&self) -> f64 {
        (self.h - PAD_T - PAD_B).max(1.0)
    }
}

/// Arrow keys the editor handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfKey {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone)]
pub struct VfEditor {
    /// One press of Up/Down; the driver's granularity (5 MHz on NVIDIA, the Intel step there).
    pub step_mhz: i32,
    pub freq_min_mhz: i32,
    pub freq_max_mhz: i32,
    /// Intel: plot the driver's whole frequency window rather than the fixed frame.
    pub full_frequency_range: bool,
    pub volt_min_mv: i32,
    pub volt_max_mv: i32,

    /// Index/voltage/stock fixed; `live_mhz` is what gets edited.
    points: Vec<VfCurveSample>,
    drag: Option<usize>,
    /// Survives the drag, so the arrow keys have a target.
    selected: Option<usize>,
    drag_start_mhz: i32,
    /// Snapshot of every `live_mhz` for Shift-drag, and to restore the tail when Ctrl is let go.
    drag_start_all: Option<Vec<i32>>,
    /// Held still while dragging.
    frozen: Option<Axes>,

    /// Active voltage-lock ceiling in mV, 0 when unlocked.
    cap_mv: i32,
    /// Index of the last point at or below the cap (cached: depends only on the cap and the
    /// points' voltages).
    cap_index: Option<usize>,
    /// Highest voltage the card may reach with the applied boost; 0 when it adds nothing.
    ceiling_mv: i32,
    live_volt_mv: f64,
    live_clock_mhz: f64,
}

impl Default for VfEditor {
    fn default() -> Self {
        Self {
            step_mhz: 5,
            freq_min_mhz: 0,
            freq_max_mhz: 10000,
            full_frequency_range: false,
            volt_min_mv: 700,
            volt_max_mv: 1200,
            points: Vec::new(),
            drag: None,
            selected: None,
            drag_start_mhz: 0,
            drag_start_all: None,
            frozen: None,
            cap_mv: 0,
            cap_index: None,
            ceiling_mv: 0,
            live_volt_mv: f64::NAN,
            live_clock_mhz: f64::NAN,
        }
    }
}

impl VfEditor {
    // ------------------------------------------------------------------ state

    pub fn points(&self) -> &[VfCurveSample] {
        &self.points
    }
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }
    pub fn dragging(&self) -> Option<usize> {
        self.drag
    }
    pub fn cap_mv(&self) -> i32 {
        self.cap_mv
    }
    pub fn ceiling_mv(&self) -> i32 {
        self.ceiling_mv
    }
    pub fn live(&self) -> (f64, f64) {
        (self.live_volt_mv, self.live_clock_mhz)
    }
    /// Coarse nudge (Shift + arrow).
    pub fn coarse_step_mhz(&self) -> i32 {
        self.step_mhz.saturating_mul(5)
    }

    /// True when any point differs from stock.
    #[cfg(test)]
    pub fn is_modified(&self) -> bool {
        self.points.iter().any(|p| p.live_mhz != p.stock_mhz)
    }

    /// The points that differ from stock — what Apply sends; the backend returns the rest to stock.
    pub fn targets(&self) -> Vec<VfCurveSample> {
        self.points.iter().filter(|p| p.live_mhz != p.stock_mhz).copied().collect()
    }

    pub fn set_points(&mut self, pts: &[VfCurveSample]) {
        self.points = pts.to_vec();
        self.points.sort_by_key(|p| p.voltage_mv);
        self.recompute_cap_index();
        self.drag = None;
        // Indices refer to the old list; a reload invalidates the selection.
        self.selected = None;
        self.drag_start_all = None;
        self.frozen = None;
    }

    /// Return every point to its stock frequency.
    pub fn reset_to_stock(&mut self) {
        for p in &mut self.points {
            p.live_mhz = p.stock_mhz;
        }
        self.recompute_cap_index();
    }

    /// Flatten every point at or above the given voltage to that point's current frequency.
    /// False when no point sits at or below that voltage (nothing changed).
    pub fn flatten_from(&mut self, voltage_mv: i32) -> bool {
        let Some(anchor) = self.points.iter().rposition(|p| p.voltage_mv <= voltage_mv) else { return false };
        self.flatten_from_index(anchor);
        true
    }

    fn flatten_from_index(&mut self, anchor: usize) {
        let target = self.points[anchor].live_mhz;
        for p in self.points.iter_mut().skip(anchor + 1) {
            p.live_mhz = target;
        }
    }

    /// The lock caps the operating point rather than rewriting the curve, so the stored points
    /// still slope upward past it — drawing the cap is the only way to show what the card does.
    pub fn set_voltage_cap(&mut self, cap_mv: i32) {
        self.cap_mv = cap_mv;
        self.recompute_cap_index();
        // A cap that has just swallowed the selected point would leave a ring on a dot the arrow
        // keys can no longer move.
        if self.selected.is_some_and(|i| self.is_capped(i)) {
            self.selected = None;
        }
    }

    pub fn set_voltage_ceiling(&mut self, mv: i32) {
        self.ceiling_mv = mv;
    }

    pub fn set_live(&mut self, voltage_mv: f64, clock_mhz: f64) {
        self.live_volt_mv = voltage_mv;
        self.live_clock_mhz = clock_mhz;
    }

    fn recompute_cap_index(&mut self) {
        self.cap_index =
            if self.cap_mv <= 0 { None } else { self.points.iter().rposition(|p| p.voltage_mv <= self.cap_mv) };
    }

    /// Frequency of the last point at or below the cap — the level the card flattens to.
    pub fn cap_flat_mhz(&self) -> i32 {
        self.cap_index.and_then(|i| self.points.get(i)).map_or(0, |p| p.live_mhz)
    }

    /// The point at or below the cap, if any (the cap label's "→ N MHz").
    pub fn cap_point(&self) -> Option<VfCurveSample> {
        self.cap_index.and_then(|i| self.points.get(i)).copied()
    }

    /// Points the cap has made unreachable: still stored and written, but the card never runs
    /// there, so they are drawn flat and not edited.
    pub fn is_capped(&self, i: usize) -> bool {
        self.cap_mv > 0 && i < self.points.len() && self.points[i].voltage_mv > self.cap_mv && self.cap_flat_mhz() > 0
    }

    /// The frequency to draw point `i` at: flat past an active cap (what Afterburner plots and
    /// what the card does). `live_mhz` is left alone and reappears when the cap lifts.
    pub fn display_mhz(&self, i: usize) -> i32 {
        if self.is_capped(i) { self.cap_flat_mhz() } else { self.points[i].live_mhz }
    }

    // ------------------------------------------------------------------ axes

    pub fn axes(&self) -> Axes {
        if let Some(a) = self.frozen {
            return a;
        }
        // The low voltage end is fixed rather than fitted: idle points from ~450 mV would squash
        // the useful range into the right-hand third. The top ends grow to fit, because clipping
        // there hides real data (a 5070 Ti's curve runs to 1240 mV and 3247 MHz).
        let mut v_max = self.volt_max_mv;
        let mut f_max = if self.full_frequency_range { self.freq_max_mhz } else { self.freq_max_mhz.min(AXIS_F_MAX) };
        if let Some(last) = self.points.last() {
            if last.voltage_mv > v_max {
                v_max = (last.voltage_mv + 24) / 25 * 25; // next 25 mV tick
            }
            let top = (0..self.points.len()).map(|i| self.display_mhz(i)).max().unwrap_or(0);
            if top > f_max {
                f_max = (top + 199) / 200 * 200; // next 200 MHz tick
            }
        }
        let v_min = self.points.first().map_or(self.volt_min_mv, |p| self.volt_min_mv.min(p.voltage_mv));
        let f_min =
            self.points.iter().map(|p| p.live_mhz.min(p.stock_mhz)).min().map_or(AXIS_F_MIN, |m| m.min(AXIS_F_MIN));
        Axes {
            v_min,
            v_max,
            f_min: if self.full_frequency_range { self.freq_min_mhz } else { self.freq_min_mhz.max(f_min) },
            f_max: self.freq_max_mhz.min(f_max),
        }
    }

    #[cfg(test)]
    pub fn to_screen(&self, g: &VfGeom, voltage_mv: f64, freq_mhz: f64) -> (f64, f64) {
        self.to_screen_in(&self.axes(), g, voltage_mv, freq_mhz)
    }

    /// As [`Self::to_screen`] with the axes already computed (one call per frame, not per point).
    pub fn to_screen_in(&self, a: &Axes, g: &VfGeom, voltage_mv: f64, freq_mhz: f64) -> (f64, f64) {
        let dv = f64::from((a.v_max - a.v_min).max(1));
        let df = f64::from((a.f_max - a.f_min).max(1));
        let x = PAD_L + (voltage_mv - f64::from(a.v_min)) / dv * g.plot_w();
        let y = PAD_T + g.plot_h() - (freq_mhz - f64::from(a.f_min)) / df * g.plot_h();
        (x, y)
    }

    /// Screen Y → frequency on the driver's step grid, kept inside the frame so a drag off-screen
    /// never asks for an absurd clock.
    pub fn freq_from_y(&self, g: &VfGeom, y: f64) -> i32 {
        let a = self.axes();
        let f = f64::from(a.f_min) + (PAD_T + g.plot_h() - y) / g.plot_h() * f64::from(a.f_max - a.f_min);
        let step = f64::from(self.step_mhz.max(1));
        let mhz = (round_even(f / step) * step).clamp(i32::MIN as f64, i32::MAX as f64) as i32;
        mhz.clamp(a.f_min, a.f_max.max(a.f_min))
    }

    // ------------------------------------------------------------------ pointer

    /// Capped points are drawn flat and can't change what the card does, so they aren't
    /// grabbable. Hit-test the drawn position, never the stored one.
    pub fn hit_test(&self, g: &VfGeom, x: f64, y: f64) -> Option<usize> {
        let a = self.axes();
        let mut best = None;
        let mut best_d = HIT_RADIUS;
        for i in 0..self.points.len() {
            if self.is_capped(i) {
                continue;
            }
            let (px, py) =
                self.to_screen_in(&a, g, f64::from(self.points[i].voltage_mv), f64::from(self.display_mhz(i)));
            let d = (px - x).hypot(py - y);
            if d <= best_d {
                best_d = d;
                best = Some(i);
            }
        }
        best
    }

    /// Left button down; `double` = second press of a double-click. Returns true when the curve
    /// changed.
    pub fn press(&mut self, g: &VfGeom, x: f64, y: f64, double: bool) -> bool {
        let Some(hit) = self.hit_test(g, x, y) else {
            // Clicking the empty plot drops the selection, so the arrow keys never move a point
            // the user has stopped thinking about.
            self.selected = None;
            return false;
        };
        self.selected = Some(hit);
        if double {
            self.points[hit].live_mhz = self.points[hit].stock_mhz;
            return true;
        }
        self.frozen = Some(self.axes()); // snapshot while the axes still reflect the live points
        self.drag = Some(hit);
        self.drag_start_mhz = self.points[hit].live_mhz;
        self.drag_start_all = Some(self.points.iter().map(|p| p.live_mhz).collect());
        false
    }

    /// Pointer moved with the button held. Returns true when the curve changed.
    pub fn drag_to(&mut self, g: &VfGeom, y: f64, shift: bool, ctrl: bool) -> bool {
        let (Some(i), Some(start)) = (self.drag, self.drag_start_all.as_ref()) else { return false };
        if i >= self.points.len() || start.len() != self.points.len() {
            return false;
        }
        let start = start.clone();
        let new_freq = self.freq_from_y(g, y).max(0);
        if shift {
            // This point and everything to its right, by the same amount.
            let delta = new_freq - self.drag_start_mhz;
            for (p, s) in self.points.iter_mut().zip(&start).skip(i) {
                p.live_mhz = s.saturating_add(delta).max(0);
            }
        } else {
            self.points[i].live_mhz = new_freq;
            // Restore the untouched tail first, so letting go of Ctrl un-flattens.
            for (p, s) in self.points.iter_mut().zip(&start).skip(i + 1) {
                p.live_mhz = *s;
            }
            if ctrl {
                self.flatten_from_index(i);
            }
        }
        true
    }

    /// Button released or capture lost: unfreeze the axes and drop the readout.
    pub fn end_drag(&mut self) {
        self.drag = None;
        self.drag_start_all = None;
        self.frozen = None;
    }

    /// Right button: every point back to stock (not mid-drag — the next move would restore the
    /// tail from the snapshot anyway). Returns true when it did.
    pub fn right_click(&mut self) -> bool {
        if self.drag.is_some() {
            return false;
        }
        self.reset_to_stock();
        true
    }

    // ------------------------------------------------------------------ keyboard

    /// Up/Down nudge the selected point by one step (`coarse` = five), `ctrl` also flattens
    /// everything above it; Left/Right walk the selection along the visible, uncapped points.
    /// Returns (handled, curve changed). Nudges add to the point's own value, so an off-grid
    /// frequency stays off-grid rather than being snapped.
    pub fn key(&mut self, key: VfKey, coarse: bool, ctrl: bool) -> (bool, bool) {
        if self.points.is_empty() {
            return (false, false);
        }
        let a = self.axes();
        match key {
            VfKey::Left | VfKey::Right => {
                // Only points inside the frame: selecting an invisible idle point below 700 mV
                // would look like the keys had stopped working.
                let first = self.points.iter().position(|p| p.voltage_mv >= a.v_min);
                let last = self
                    .points
                    .iter()
                    .rposition(|p| p.voltage_mv <= a.v_max && (self.cap_mv <= 0 || p.voltage_mv <= self.cap_mv));
                let (Some(first), Some(last)) = (first, last) else { return (false, false) };
                if last < first {
                    return (false, false);
                }
                let right = key == VfKey::Right;
                self.selected = Some(match self.selected {
                    None => {
                        if right {
                            first
                        } else {
                            last
                        }
                    }
                    Some(s) => {
                        let s = s as i64 + if right { 1 } else { -1 };
                        s.clamp(first as i64, last as i64) as usize
                    }
                });
                (true, false)
            }
            VfKey::Up | VfKey::Down => {
                let Some(i) = self.selected.filter(|&i| i < self.points.len()) else { return (false, false) };
                let step = if coarse { self.coarse_step_mhz() } else { self.step_mhz };
                let cur = self.points[i].live_mhz;
                let next = cur
                    .saturating_add(if key == VfKey::Up { step } else { -step })
                    .clamp(a.f_min, a.f_max.max(a.f_min));
                if next == cur {
                    return (true, false); // already against the top or bottom of the frame
                }
                self.points[i].live_mhz = next;
                if ctrl {
                    self.flatten_from_index(i);
                }
                (true, true)
            }
        }
    }

    // ------------------------------------------------------------------ text

    /// The readout for whichever point is being worked on: dragged, else selected.
    pub fn readout(&self) -> Option<(usize, String)> {
        let i = self.drag.or(self.selected).filter(|&i| i < self.points.len())?;
        let p = self.points[i];
        Some((
            i,
            format!("{} mV → {} MHz  ({})", p.voltage_mv, p.live_mhz, super::format::signed(p.live_mhz - p.stock_mhz)),
        ))
    }

    /// The status line after an edit (C# `CurveWindow.UpdateStatus`); `None` with no points.
    pub fn edit_status(&self) -> Option<String> {
        if self.points.is_empty() {
            return None;
        }
        let changed = self.points.iter().filter(|p| p.live_mhz != p.stock_mhz).count();
        if changed == 0 {
            return Some("Curve matches stock.".into());
        }
        let up = self.points.iter().map(|p| p.live_mhz - p.stock_mhz).max().unwrap_or(0);
        let down = self.points.iter().map(|p| p.live_mhz - p.stock_mhz).min().unwrap_or(0);
        Some(format!(
            "{changed} of {} points edited (max {} / {} MHz). Not applied yet.",
            self.points.len(),
            super::format::signed(up),
            super::format::signed(down)
        ))
    }

    /// "N curve points loaded (lo–hi mV)…" after a read (C# `CurveWindow.LoadCurve`).
    pub fn loaded_status(&self) -> Option<String> {
        let first = self.points.first()?;
        let top = self.points.iter().map(|p| p.voltage_mv).max().unwrap_or(first.voltage_mv);
        let low = self.points.iter().map(|p| p.voltage_mv).min().unwrap_or(first.voltage_mv);
        // The table's top is a hardware fact, so say where it is when a boost has pushed the
        // ceiling past it — otherwise the curve just looks like it stops short for no reason.
        let boost = if self.ceiling_mv > top {
            format!(
                " The table stops at {top} mV; the boost lets the card run to {} mV, but adds no points.",
                self.ceiling_mv
            )
        } else {
            String::new()
        };
        Some(format!(
            "{} curve points loaded ({low}–{top} mV).{boost} Drag a point, or use ↑/↓ ({} MHz, Shift = {} MHz). Then Apply to GPU.",
            self.points.len(),
            self.step_mhz,
            self.coarse_step_mhz()
        ))
    }
}

/// How many 25 mV gridlines to skip between labels so they do not collide on a narrow window.
pub fn voltage_label_every(plot_w: f64, a: &Axes, v_step: i32) -> usize {
    let lines = f64::from(a.v_max - a.v_min) / f64::from(v_step.max(1));
    let px_per_line = (plot_w / lines.max(1e-9)).max(1.0);
    ((30.0 / px_per_line).ceil() as usize).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 points, 750..975 mV every 25, stock 1500..2400 every 100.
    fn curve() -> Vec<VfCurveSample> {
        (0..10).map(|i| VfCurveSample::new(i, 750 + 25 * i, 1500 + 100 * i, 1500 + 100 * i)).collect()
    }

    fn editor() -> VfEditor {
        let mut e = VfEditor::default();
        e.set_points(&curve());
        e
    }

    // Plot 500×280 px.
    const G: VfGeom = VfGeom { w: 58.0 + 500.0 + 18.0, h: 18.0 + 280.0 + 42.0 };

    fn screen(e: &VfEditor, i: usize) -> (f64, f64) {
        let p = e.points()[i];
        e.to_screen(&G, f64::from(p.voltage_mv), f64::from(e.display_mhz(i)))
    }

    #[test]
    fn fixed_frame_and_growth() {
        let e = editor();
        assert_eq!(e.axes(), Axes { v_min: 700, v_max: 1200, f_min: 600, f_max: 3400 });
        let mut e = VfEditor::default();
        e.set_points(&[VfCurveSample::new(0, 450, 300, 300), VfCurveSample::new(1, 1240, 3247, 3500)]);
        // Voltage low end follows a lower point; the tops grow to the next tick.
        assert_eq!(e.axes(), Axes { v_min: 450, v_max: 1250, f_min: 300, f_max: 3600 });
        // Intel: the driver's whole window.
        let mut i = VfEditor {
            full_frequency_range: true,
            freq_min_mhz: 100,
            freq_max_mhz: 4000,
            volt_min_mv: 600,
            volt_max_mv: 1300,
            ..Default::default()
        };
        i.set_points(&curve());
        assert_eq!(i.axes(), Axes { v_min: 600, v_max: 1300, f_min: 100, f_max: 4000 });
    }

    #[test]
    fn drag_snaps_to_step_and_freezes_axes() {
        let mut e = editor();
        let (x, y) = screen(&e, 4);
        assert!(!e.press(&G, x + 2.0, y - 2.0, false));
        assert_eq!(e.selected(), Some(4));
        assert_eq!(e.dragging(), Some(4));
        // 2800 MHz span over 280 px = 10 MHz/px; 13 px up = +130 → 2030, snapped to 5 MHz.
        assert!(e.drag_to(&G, y - 13.0, false, false));
        assert_eq!(e.points()[4].live_mhz, 2030);
        assert_eq!(e.points()[5].live_mhz, 2000, "the tail is untouched");
        // Off the top of the frame clamps to the frozen frame.
        e.drag_to(&G, -10_000.0, false, false);
        assert_eq!(e.points()[4].live_mhz, 3400);
        e.end_drag();
        assert_eq!(e.dragging(), None);
        assert_eq!(e.selected(), Some(4));
        assert_eq!(e.readout().map(|r| r.1), Some("850 mV → 3400 MHz  (+1500)".to_owned()));
    }

    #[test]
    fn ctrl_flattens_and_releasing_ctrl_unflattens() {
        let mut e = editor();
        let (x, y) = screen(&e, 3);
        e.press(&G, x, y, false);
        e.drag_to(&G, y, false, true);
        assert!(e.points()[4..].iter().all(|p| p.live_mhz == 1800));
        e.drag_to(&G, y, false, false);
        assert_eq!(e.points()[9].live_mhz, 2400, "the snapshot comes back without Ctrl");
    }

    #[test]
    fn shift_moves_the_whole_tail() {
        let mut e = editor();
        let (x, y) = screen(&e, 6);
        e.press(&G, x, y, false);
        e.drag_to(&G, y + 5.0, true, false); // -50 MHz
        assert_eq!(e.points()[5].live_mhz, 2000);
        assert!((6..10).all(|i| e.points()[i].live_mhz == 1500 + 100 * i as i32 - 50));
        assert_eq!(e.targets().len(), 4);
        assert_eq!(e.edit_status().as_deref(), Some("4 of 10 points edited (max 0 / -50 MHz). Not applied yet."));
    }

    #[test]
    fn double_click_resets_one_point_and_right_click_all() {
        let mut e = editor();
        e.press(&G, screen(&e, 2).0, screen(&e, 2).1, false);
        e.drag_to(&G, screen(&e, 2).1 - 20.0, false, false);
        e.end_drag();
        let (x, y) = screen(&e, 2);
        e.press(&G, x, y, false);
        e.end_drag();
        assert!(e.is_modified());
        assert!(e.press(&G, x, y, true));
        assert!(!e.is_modified());
        assert_eq!(e.edit_status().as_deref(), Some("Curve matches stock."));

        e.flatten_from(800);
        assert!(e.is_modified());
        e.press(&G, screen(&e, 1).0, screen(&e, 1).1, false);
        assert!(!e.right_click(), "not mid-drag");
        e.end_drag();
        assert!(e.right_click());
        assert!(!e.is_modified());
    }

    #[test]
    fn flatten_from_voltage() {
        let mut e = editor();
        assert!(!e.flatten_from(700), "nothing at or below 700 mV");
        assert!(e.flatten_from(860)); // anchor 850 mV (index 4)
        assert!(e.points()[5..].iter().all(|p| p.live_mhz == 1900));
        assert_eq!(e.points()[4].live_mhz, 1900);
        assert_eq!(e.points()[3].live_mhz, 1800);
    }

    #[test]
    fn cap_draws_flat_and_blocks_editing_above_it() {
        let mut e = editor();
        e.press(&G, screen(&e, 8).0, screen(&e, 8).1, false);
        e.end_drag();
        assert_eq!(e.selected(), Some(8));
        e.set_voltage_cap(860);
        assert_eq!(e.selected(), None, "a cap that swallows the selection drops it");
        assert_eq!(e.cap_flat_mhz(), 1900);
        assert!(e.is_capped(5) && !e.is_capped(4));
        assert_eq!(e.display_mhz(9), 1900);
        assert_eq!(e.points()[9].live_mhz, 2400, "stored points are untouched");
        // A capped point can't be grabbed even where it is drawn.
        let (x, y) = screen(&e, 9);
        assert!(!e.press(&G, x, y, false));
        assert_ne!(e.dragging(), Some(9));
        // Walking right stops at the last uncapped point.
        e.selected = None;
        assert_eq!(e.key(VfKey::Left, false, false), (true, false));
        assert_eq!(e.selected(), Some(4));
        e.key(VfKey::Right, false, false);
        assert_eq!(e.selected(), Some(4));
        e.set_voltage_cap(0);
        assert!(!e.is_capped(9));
        assert_eq!(e.display_mhz(9), 2400);
    }

    #[test]
    fn arrow_keys_nudge_without_snapping() {
        let mut e = VfEditor { step_mhz: 15, ..Default::default() };
        let mut pts = curve();
        pts[2].live_mhz = 1703; // off the 15 MHz grid, as an Intel card can report
        e.set_points(&pts);
        assert_eq!(e.key(VfKey::Up, false, false), (false, false), "no selection, nothing to nudge");
        e.key(VfKey::Right, false, false);
        assert_eq!(e.selected(), Some(0));
        e.key(VfKey::Right, false, false);
        e.key(VfKey::Right, false, false);
        assert_eq!(e.selected(), Some(2));
        assert_eq!(e.key(VfKey::Up, false, false), (true, true));
        assert_eq!(e.points()[2].live_mhz, 1718, "off-grid value preserved");
        e.key(VfKey::Down, true, false);
        assert_eq!(e.points()[2].live_mhz, 1643);
        e.key(VfKey::Up, false, true);
        assert!(e.points()[3..].iter().all(|p| p.live_mhz == 1658), "Ctrl flattens above");
        // Against the top of the frame: handled, unchanged.
        e.points[2].live_mhz = 3400;
        assert_eq!(e.key(VfKey::Up, false, false), (true, false));
    }

    #[test]
    fn statuses_and_label_density() {
        let mut e = editor();
        e.set_voltage_ceiling(1100);
        let s = e.loaded_status().unwrap_or_default();
        assert!(s.starts_with(
            "10 curve points loaded (750–975 mV). The table stops at 975 mV; the boost lets the card run to 1100 mV"
        ));
        assert!(s.ends_with("(5 MHz, Shift = 25 MHz). Then Apply to GPU."));
        assert_eq!(VfEditor::default().loaded_status(), None);
        let a = e.axes();
        assert_eq!(voltage_label_every(500.0, &a, 25), 2); // 25 px per line → every other
        assert_eq!(voltage_label_every(1000.0, &a, 25), 1);
    }

    #[test]
    fn empty_plot_click_clears_selection() {
        let mut e = editor();
        e.press(&G, screen(&e, 1).0, screen(&e, 1).1, false);
        e.end_drag();
        assert!(!e.press(&G, G.w - 20.0, 25.0, false));
        assert_eq!(e.selected(), None);
    }
}
