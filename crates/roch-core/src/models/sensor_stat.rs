//! Current, minimum, maximum and running mean for one sensor.
//!
//! The mean is a running total rather than a window, so it describes the whole session since the
//! last reset. A NaN reading is a sensor the card does not expose: it clears `current` but is not
//! folded in as zero, because a zero minimum on a temperature that was never read is a lie.

#[derive(Debug, Clone)]
pub struct SensorStat {
    pub current: f64,
    pub minimum: f64,
    pub maximum: f64,
    total: f64,
    count: i64,
}

impl Default for SensorStat {
    fn default() -> Self {
        Self { current: f64::NAN, minimum: f64::NAN, maximum: f64::NAN, total: 0.0, count: 0 }
    }
}

impl SensorStat {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mean of every reading since the last reset; NaN before the first one.
    pub fn average(&self) -> f64 {
        if self.count > 0 { self.total / self.count as f64 } else { f64::NAN }
    }

    /// True once at least one real reading has arrived.
    pub fn has_data(&self) -> bool {
        self.count > 0
    }

    pub fn add(&mut self, value: f64) {
        if !value.is_finite() {
            self.current = f64::NAN;
            return;
        }
        self.current = value;
        if self.minimum.is_nan() || value < self.minimum {
            self.minimum = value;
        }
        if self.maximum.is_nan() || value > self.maximum {
            self.maximum = value;
        }
        self.total += value;
        self.count += 1;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnect_clears_current_but_preserves_history() {
        let mut s = SensorStat::new();
        s.add(100.0);
        s.add(f64::NAN);
        assert!(s.current.is_nan());
        assert_eq!(s.minimum, 100.0);
        assert_eq!(s.average(), 100.0);
    }
}
