//! Piecewise-linear temperature → fan% curve with hysteresis and a minimum step, mirroring what
//! Afterburner's "user defined" curve does.

use serde::{Deserialize, Serialize};

use crate::util::clamp_f64;

/// A single (temperature °C, fan %) point.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct FanPoint {
    pub temperature_c: f64,
    pub fan_percent: f64,
}

impl FanPoint {
    pub const fn new(temperature_c: f64, fan_percent: f64) -> Self {
        Self { temperature_c, fan_percent }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct FanCurve {
    pub points: Vec<FanPoint>,
    /// Degrees the temperature must fall below the last trigger before the fan may slow down.
    pub hysteresis_c: f64,
    /// Ignore fan changes smaller than this (%) to avoid constant tiny adjustments.
    pub minimum_step_percent: f64,
    /// Below the first point the fan may stop entirely (if the card supports 0%).
    pub zero_rpm_below_first_point: bool,

    #[serde(skip)]
    last_temp: f64,
    #[serde(skip)]
    last_output: f64,
}

impl Default for FanCurve {
    fn default() -> Self {
        Self {
            points: Self::default_points(),
            hysteresis_c: 3.0,
            minimum_step_percent: 2.0,
            zero_rpm_below_first_point: false,
            last_temp: f64::NAN,
            last_output: f64::NAN,
        }
    }
}

impl PartialEq for FanCurve {
    /// Configuration equality; the stepping state is not part of a curve's identity.
    fn eq(&self, other: &Self) -> bool {
        self.points == other.points
            && self.hysteresis_c == other.hysteresis_c
            && self.minimum_step_percent == other.minimum_step_percent
            && self.zero_rpm_below_first_point == other.zero_rpm_below_first_point
    }
}

impl FanCurve {
    pub fn default_points() -> Vec<FanPoint> {
        vec![
            FanPoint::new(30.0, 30.0),
            FanPoint::new(50.0, 40.0),
            FanPoint::new(60.0, 55.0),
            FanPoint::new(70.0, 70.0),
            FanPoint::new(80.0, 90.0),
            FanPoint::new(90.0, 100.0),
        ]
    }

    pub fn with_points(points: Vec<FanPoint>) -> Self {
        Self { points, ..Self::default() }
    }

    fn sorted_points(&self) -> Vec<FanPoint> {
        let mut pts = self.points.clone();
        // Stable sort, as LINQ's OrderBy is.
        pts.sort_by(|a, b| a.temperature_c.partial_cmp(&b.temperature_c).unwrap_or(std::cmp::Ordering::Equal));
        pts
    }

    /// Pure interpolation without hysteresis. Public so the UI can draw the curve.
    pub fn evaluate(&self, temperature_c: f64) -> f64 {
        let pts = self.sorted_points();
        if pts.is_empty() {
            return 50.0;
        }
        let first = pts[0];
        let last = pts[pts.len() - 1];
        if temperature_c <= first.temperature_c {
            return if self.zero_rpm_below_first_point && temperature_c < first.temperature_c {
                0.0
            } else {
                first.fan_percent
            };
        }
        if temperature_c >= last.temperature_c {
            return last.fan_percent;
        }
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            if temperature_c >= a.temperature_c && temperature_c <= b.temperature_c {
                if (b.temperature_c - a.temperature_c).abs() < 1e-9 {
                    return b.fan_percent;
                }
                let t = (temperature_c - a.temperature_c) / (b.temperature_c - a.temperature_c);
                return a.fan_percent + t * (b.fan_percent - a.fan_percent);
            }
        }
        last.fan_percent
    }

    /// Stateful evaluation applying hysteresis and minimum step. `None` when no change should be
    /// sent to the GPU.
    pub fn step(&mut self, temperature_c: f64) -> Option<f64> {
        let target = self.evaluate(temperature_c);

        if self.last_output.is_nan() {
            self.last_temp = temperature_c;
            self.last_output = target;
            return Some(target);
        }

        // Rising: always follow immediately. Falling: only after the hysteresis band is cleared.
        let falling = target < self.last_output;
        if falling && temperature_c > self.last_temp - self.hysteresis_c {
            return None;
        }
        if (target - self.last_output).abs() < self.minimum_step_percent {
            return None;
        }
        self.last_temp = temperature_c;
        self.last_output = target;
        Some(target)
    }

    pub fn reset_state(&mut self) {
        self.last_temp = f64::NAN;
        self.last_output = f64::NAN;
    }

    /// A copy of the configuration with fresh stepping state (C# `Clone`).
    pub fn clone_config(&self) -> FanCurve {
        FanCurve {
            points: self.points.clone(),
            hysteresis_c: self.hysteresis_c,
            minimum_step_percent: self.minimum_step_percent,
            zero_rpm_below_first_point: self.zero_rpm_below_first_point,
            last_temp: f64::NAN,
            last_output: f64::NAN,
        }
    }

    /// Clamp and sort points so the curve is monotonic in temperature and within 0..100%.
    pub fn normalize(&mut self) {
        let mut pts: Vec<FanPoint> = self
            .points
            .iter()
            .map(|p| FanPoint::new(clamp_f64(p.temperature_c, 0.0, 110.0), clamp_f64(p.fan_percent, 0.0, 100.0)))
            .collect();
        pts.sort_by(|a, b| a.temperature_c.partial_cmp(&b.temperature_c).unwrap_or(std::cmp::Ordering::Equal));
        self.points = pts;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hysteresis_and_min_step() {
        let mut h = FanCurve::with_points(vec![FanPoint::new(40.0, 30.0), FanPoint::new(80.0, 100.0)]);
        h.hysteresis_c = 5.0;
        h.minimum_step_percent = 2.0;
        let s1 = h.step(60.0);
        assert!((s1.unwrap() - 65.0).abs() < 1e-6, "first step returns");
        assert_eq!(h.step(60.5), None, "tiny change ignored");
        let s2 = h.step(70.0);
        assert!((s2.unwrap() - 82.5).abs() < 1e-6, "rising follows immediately");
        assert_eq!(h.step(66.0), None, "falling within hysteresis ignored");
        let s3 = h.step(64.0);
        assert!((s3.unwrap() - 72.0).abs() < 1e-6, "falling past hysteresis applies");
    }

    #[test]
    fn evaluate_edges() {
        let p = FanPoint::new;
        let mut c = FanCurve::with_points(vec![p(30.0, 20.0), p(60.0, 50.0), p(80.0, 100.0)]);
        assert_eq!(c.evaluate(10.0), 20.0);
        assert_eq!(c.evaluate(30.0), 20.0);
        assert_eq!(c.evaluate(45.0), 35.0);
        assert_eq!(c.evaluate(70.0), 75.0);
        assert_eq!(c.evaluate(95.0), 100.0);
        c.zero_rpm_below_first_point = true;
        assert_eq!(c.evaluate(25.0), 0.0);
        assert_eq!(c.evaluate(30.0), 20.0);
        let unsorted = FanCurve::with_points(vec![p(80.0, 100.0), p(30.0, 20.0), p(60.0, 50.0)]);
        assert_eq!(unsorted.evaluate(45.0), 35.0);
        assert_eq!(FanCurve::with_points(vec![]).evaluate(50.0), 50.0);
    }

    #[test]
    fn serializes_like_dotnet() {
        let c = FanCurve::default();
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"Points\""));
        assert!(json.contains("\"TemperatureC\""));
        assert!(json.contains("\"HysteresisC\""));
        let back: FanCurve = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }
}
