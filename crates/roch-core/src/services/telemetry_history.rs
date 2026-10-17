//! Fixed-capacity ring buffer of telemetry samples for the graphs.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::models::GpuTelemetry;

pub struct TelemetryHistory {
    capacity: usize,
    buf: Mutex<VecDeque<GpuTelemetry>>,
}

impl TelemetryHistory {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(2);
        Self { capacity, buf: Mutex::new(VecDeque::with_capacity(capacity)) }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn add(&self, t: GpuTelemetry) {
        let mut buf = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        if buf.len() == self.capacity {
            buf.pop_front();
        }
        buf.push_back(t);
    }

    pub fn clear(&self) {
        self.buf.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    pub fn len(&self) -> usize {
        self.buf.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Oldest → newest snapshot.
    pub fn snapshot(&self) -> Vec<GpuTelemetry> {
        self.buf.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect()
    }

    /// Convenience: a single series projected out of the history.
    pub fn series(&self, selector: impl Fn(&GpuTelemetry) -> f64) -> Vec<f64> {
        self.buf.lock().unwrap_or_else(|e| e.into_inner()).iter().map(selector).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_wraps_oldest_first() {
        let h = TelemetryHistory::new(3);
        for i in 0..5 {
            h.add(GpuTelemetry { core_clock_mhz: i as f64, ..Default::default() });
        }
        assert_eq!(h.series(|t| t.core_clock_mhz), vec![2.0, 3.0, 4.0]);
        h.clear();
        assert!(h.is_empty());
        assert_eq!(TelemetryHistory::new(0).capacity(), 2);
    }
}
