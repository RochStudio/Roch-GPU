//! The temperature → fan % curve editor's state and geometry (C# `Controls/FanCurveEditor.cs`),
//! without any drawing — the fan window paints it and feeds it pointer events.
//!
//! * drag a point: move it (clamped, kept in temperature order)
//! * double-click: add a point
//! * right-click a point: remove it (minimum two)
//!
//! Coordinates are the control's own: (0,0) is its top-left corner, `w`×`h` its size.

use roch_core::util::{clamp_f64, round_even};
use roch_core::{FanCurve, FanPoint};

pub const PAD_L: f64 = 34.0;
pub const PAD_R: f64 = 12.0;
pub const PAD_T: f64 = 12.0;
pub const PAD_B: f64 = 22.0;
pub const T_MIN: f64 = 0.0;
pub const T_MAX: f64 = 110.0;
pub const F_MIN: f64 = 0.0;
pub const F_MAX: f64 = 100.0;
const HIT_RADIUS: f64 = 9.0;

/// The control's size, for mapping between data and screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FanGeom {
    pub w: f64,
    pub h: f64,
}

impl FanGeom {
    pub fn plot_w(self) -> f64 {
        (self.w - PAD_L - PAD_R).max(1.0)
    }
    pub fn plot_h(self) -> f64 {
        (self.h - PAD_T - PAD_B).max(1.0)
    }
    pub fn to_screen(self, p: FanPoint) -> (f64, f64) {
        (
            PAD_L + (p.temperature_c - T_MIN) / (T_MAX - T_MIN) * self.plot_w(),
            PAD_T + self.plot_h() - (p.fan_percent - F_MIN) / (F_MAX - F_MIN) * self.plot_h(),
        )
    }
    /// Screen → data, rounded to whole degrees and percent (`Math.Round`: halves to even).
    pub fn to_data(self, x: f64, y: f64) -> FanPoint {
        FanPoint::new(
            clamp_f64(round_even(T_MIN + (x - PAD_L) / self.plot_w() * (T_MAX - T_MIN)), T_MIN, T_MAX),
            clamp_f64(round_even(F_MAX - (y - PAD_T) / self.plot_h() * (F_MAX - F_MIN)), F_MIN, F_MAX),
        )
    }
}

/// What an interaction changed, so the window knows what to refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FanEdit {
    /// The points moved (C# `CurveChanged`): copy them into the view model and mark it dirty.
    pub curve: bool,
    /// A different point is selected (C# `SelectionChanged`).
    pub selection: bool,
}

#[derive(Debug, Clone)]
pub struct FanEditor {
    points: Vec<FanPoint>,
    drag: Option<usize>,
    selected: Option<usize>,
    /// A hardware curve holds at most this many points (Intel/AMD `fan_curve_points`); `None` for
    /// a curve this app steps itself.
    max_points: Option<usize>,
    pub read_only: bool,
}

impl Default for FanEditor {
    fn default() -> Self {
        Self { points: FanCurve::default_points(), drag: None, selected: None, max_points: None, read_only: false }
    }
}

fn sort(points: &mut [FanPoint]) {
    // Stable, as LINQ's OrderBy is.
    points.sort_by(|a, b| a.temperature_c.partial_cmp(&b.temperature_c).unwrap_or(std::cmp::Ordering::Equal));
}

impl FanEditor {
    pub fn points(&self) -> &[FanPoint] {
        &self.points
    }
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }
    pub fn dragging(&self) -> Option<usize> {
        self.drag
    }
    pub fn max_points(&self) -> Option<usize> {
        self.max_points
    }
    /// Limit how many points a double-click may add (a driver-run curve has a fixed table size).
    pub fn set_max_points(&mut self, max: Option<usize>) {
        self.max_points = max.filter(|&m| m >= 2);
    }

    /// Replace the points; this invalidates any index into the old ones.
    pub fn set_points(&mut self, pts: &[FanPoint]) {
        self.points = pts.to_vec();
        sort(&mut self.points);
        if self.points.len() < 2 {
            self.points = FanCurve::default_points();
        }
        self.selected = None;
        self.drag = None;
    }

    /// Highlight one point without a click on the plot (typing into its row).
    pub fn select_point(&mut self, index: usize) -> bool {
        self.select(if index < self.points.len() { Some(index) } else { None })
    }

    fn select(&mut self, index: Option<usize>) -> bool {
        if self.selected == index {
            return false;
        }
        self.selected = index;
        true
    }

    /// Lowest and highest temperature point `i` may take: strictly between its neighbours, so
    /// the curve stays a function the fan controller can read.
    fn temp_window(&self, i: usize) -> (f64, f64) {
        let lo = if i > 0 { self.points[i - 1].temperature_c + 1.0 } else { T_MIN };
        let hi = if i + 1 < self.points.len() { self.points[i + 1].temperature_c - 1.0 } else { T_MAX };
        (lo, hi)
    }

    /// Move point `i` by typed values, either of which may be left alone. Returns true when the
    /// curve changed. The temperature is held between its neighbours, so the caller reads the
    /// point back: the number that lands is not always the number typed.
    pub fn try_update_point(&mut self, i: usize, temp_c: Option<f64>, fan_percent: Option<f64>) -> bool {
        if i >= self.points.len() {
            return false;
        }
        let cur = self.points[i];
        let (lo, hi) = self.temp_window(i);
        let t = clamp_f64(round_even(temp_c.unwrap_or(cur.temperature_c)), lo.max(T_MIN), hi.min(T_MAX));
        let f = clamp_f64(round_even(fan_percent.unwrap_or(cur.fan_percent)), F_MIN, F_MAX);
        if t == cur.temperature_c && f == cur.fan_percent {
            return false;
        }
        self.points[i] = FanPoint::new(t, f);
        true
    }

    pub fn hit_test(&self, g: &FanGeom, x: f64, y: f64) -> Option<usize> {
        self.points.iter().position(|p| {
            let (px, py) = g.to_screen(*p);
            (px - x).hypot(py - y) <= HIT_RADIUS
        })
    }

    /// Left button down. `double` = the second press of a double-click, which adds a point there
    /// and starts dragging it.
    pub fn press(&mut self, g: &FanGeom, x: f64, y: f64, double: bool) -> FanEdit {
        let mut e = FanEdit::default();
        if self.read_only {
            return e;
        }
        if double {
            if self.max_points.is_some_and(|m| self.points.len() >= m) {
                return e;
            }
            let np = g.to_data(x, y);
            self.points.push(np);
            sort(&mut self.points);
            let at = self.points.iter().position(|p| *p == np);
            self.drag = at;
            e.selection = self.select(at);
            e.curve = true;
            return e;
        }
        let hit = self.hit_test(g, x, y);
        // Clicking the empty plot clears the selection, so the boxes stop offering to edit a
        // point the user is no longer pointing at.
        e.selection = self.select(hit);
        self.drag = hit;
        e
    }

    /// Pointer moved while the button is held.
    pub fn drag_to(&mut self, g: &FanGeom, x: f64, y: f64) -> FanEdit {
        let Some(i) = self.drag.filter(|&i| i < self.points.len()) else { return FanEdit::default() };
        let d = g.to_data(x, y);
        let (lo, hi) = self.temp_window(i);
        self.points[i] = FanPoint::new(clamp_f64(d.temperature_c, lo, hi), d.fan_percent);
        // The boxes track the drag too.
        FanEdit { curve: true, selection: true }
    }

    pub fn release(&mut self) {
        self.drag = None;
    }

    /// Right button: remove the point under it, keeping at least two.
    pub fn remove_at(&mut self, g: &FanGeom, x: f64, y: f64) -> FanEdit {
        if self.read_only {
            return FanEdit::default();
        }
        match self.hit_test(g, x, y) {
            Some(hit) if self.points.len() > 2 => {
                self.points.remove(hit);
                if self.selected.is_some_and(|s| s >= self.points.len()) {
                    self.selected = None;
                }
                self.drag = None;
                FanEdit { curve: true, selection: true }
            }
            _ => FanEdit::default(),
        }
    }
}

/// Parse a typed number the way `double.TryParse` would accept it (finite only).
pub fn parse_number(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: FanGeom = FanGeom { w: 34.0 + 110.0 * 4.0 + 12.0, h: 12.0 + 200.0 + 22.0 };

    fn pt(t: f64, f: f64) -> FanPoint {
        FanPoint::new(t, f)
    }

    #[test]
    fn mapping_round_trips() {
        // 4 px per degree, 2 px per percent.
        assert_eq!(G.to_screen(pt(0.0, 0.0)), (34.0, 212.0));
        assert_eq!(G.to_screen(pt(110.0, 100.0)), (474.0, 12.0));
        assert_eq!(G.to_data(34.0 + 50.0 * 4.0, 212.0 - 60.0 * 2.0), pt(50.0, 60.0));
        // Off the plot clamps to the axes.
        assert_eq!(G.to_data(-100.0, 1000.0), pt(0.0, 0.0));
        assert_eq!(G.to_data(10_000.0, -100.0), pt(110.0, 100.0));
    }

    #[test]
    fn set_points_sorts_and_refuses_a_single_point() {
        let mut e = FanEditor::default();
        e.set_points(&[pt(70.0, 80.0), pt(40.0, 30.0)]);
        assert_eq!(e.points(), &[pt(40.0, 30.0), pt(70.0, 80.0)]);
        e.set_points(&[pt(40.0, 30.0)]);
        assert_eq!(e.points(), FanCurve::default_points().as_slice());
    }

    #[test]
    fn drag_keeps_temperature_between_neighbours() {
        let mut e = FanEditor::default(); // 30/30 50/40 60/55 70/70 80/90 90/100
        let (x, y) = G.to_screen(e.points()[1]);
        let edit = e.press(&G, x + 3.0, y - 3.0, false);
        assert!(edit.selection);
        assert_eq!(e.selected(), Some(1));
        assert_eq!(e.dragging(), Some(1));
        // Far to the right and above the top: held at 59 °C, 100 %.
        let edit = e.drag_to(&G, 10_000.0, -50.0);
        assert!(edit.curve);
        assert_eq!(e.points()[1], pt(59.0, 100.0));
        e.drag_to(&G, -50.0, 10_000.0);
        assert_eq!(e.points()[1], pt(31.0, 0.0));
        e.release();
        assert_eq!(e.dragging(), None);
        assert_eq!(e.selected(), Some(1), "the selection outlives the drag");
        // Clicking the empty plot clears it.
        let edit = e.press(&G, 470.0, 200.0, false);
        assert!(edit.selection);
        assert_eq!(e.selected(), None);
    }

    #[test]
    fn double_click_adds_and_right_click_removes() {
        let mut e = FanEditor::default();
        let (x, y) = G.to_screen(pt(40.0, 35.0));
        let edit = e.press(&G, x, y, true);
        assert!(edit.curve && edit.selection);
        assert_eq!(e.points().len(), 7);
        assert_eq!(e.points()[1], pt(40.0, 35.0));
        assert_eq!(e.selected(), Some(1));
        assert_eq!(e.dragging(), Some(1));
        e.release();

        e.set_points(&[pt(30.0, 30.0), pt(60.0, 60.0), pt(90.0, 100.0)]);
        e.select_point(2);
        let (x, y) = G.to_screen(pt(90.0, 100.0));
        assert!(e.remove_at(&G, x, y).curve);
        assert_eq!(e.points().len(), 2);
        assert_eq!(e.selected(), None, "a selection past the end is dropped");
        let (x, y) = G.to_screen(pt(30.0, 30.0));
        assert!(!e.remove_at(&G, x, y).curve, "never fewer than two points");
        assert!(!e.remove_at(&G, 300.0, 100.0).curve, "nothing under the pointer");
    }

    #[test]
    fn hardware_curve_point_limit() {
        let mut e = FanEditor::default();
        e.set_max_points(Some(6));
        let (x, y) = G.to_screen(pt(40.0, 35.0));
        assert!(!e.press(&G, x, y, true).curve, "a full hardware table takes no more points");
        assert_eq!(e.points().len(), 6);
        let (x, y) = G.to_screen(pt(90.0, 100.0));
        e.remove_at(&G, x, y);
        assert!(e.press(&G, 200.0, 150.0, true).curve);
        assert_eq!(e.points().len(), 6);
        e.set_max_points(Some(1));
        assert_eq!(e.max_points(), None, "a limit under two means none");
    }

    #[test]
    fn typed_values_are_rounded_and_held_between_neighbours() {
        let mut e = FanEditor::default();
        assert!(e.try_update_point(2, Some(64.6), None));
        assert_eq!(e.points()[2], pt(65.0, 55.0));
        assert!(e.try_update_point(2, Some(99.0), Some(140.0)));
        assert_eq!(e.points()[2], pt(69.0, 100.0));
        assert!(e.try_update_point(0, Some(-20.0), None));
        assert_eq!(e.points()[0], pt(0.0, 30.0));
        assert!(e.try_update_point(5, Some(500.0), None));
        assert_eq!(e.points()[5], pt(110.0, 100.0));
        assert!(!e.try_update_point(5, None, None), "nothing typed, nothing changed");
        assert!(!e.try_update_point(9, Some(1.0), None), "out of range index");
        assert_eq!(parse_number(" 42 "), Some(42.0));
        assert_eq!(parse_number("4x"), None);
        assert_eq!(parse_number("NaN"), None);
    }

    #[test]
    fn read_only_ignores_edits() {
        let mut e = FanEditor { read_only: true, ..Default::default() };
        let (x, y) = G.to_screen(e.points()[0]);
        assert_eq!(e.press(&G, x, y, true), FanEdit::default());
        assert_eq!(e.remove_at(&G, x, y), FanEdit::default());
        assert_eq!(e.points().len(), 6);
    }
}
