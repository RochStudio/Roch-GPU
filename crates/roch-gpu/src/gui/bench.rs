//! The scripted window benchmark behind `ROCH_TRACE=1 ROCH_TRACE_BENCH=1` (see `crate::trace`).
//!
//! Once the main window is up it measures idle CPU and frame count with nothing open, opens
//! Telemetry and measures again, closes and reopens it, opens and closes each other child window,
//! then exits. Every step goes through the same app methods a click does; the frame-by-frame
//! timings come from the app's own trace lines. Only for `--mock` runs.

use std::time::{Duration, Instant};

/// A window the benchmark opens and closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Win {
    Monitor,
    Curve,
    Xoc,
    Fan,
    Graphics,
}

/// What the app should do for the current step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Open(Win),
    Close(Win),
    Exit,
}

/// (what to do on entering the step, how long to stay in it, measure idle CPU/frames over it).
const SCRIPT: &[(Option<Step>, u64, bool)] = &[
    (None, 2000, false),
    (None, 10_000, true),
    (Some(Step::Open(Win::Monitor)), 3000, false),
    (None, 10_000, true),
    (Some(Step::Close(Win::Monitor)), 1500, false),
    (Some(Step::Open(Win::Monitor)), 2000, false),
    (Some(Step::Close(Win::Monitor)), 1000, false),
    (Some(Step::Open(Win::Curve)), 1500, false),
    (Some(Step::Close(Win::Curve)), 1000, false),
    (Some(Step::Open(Win::Xoc)), 1500, false),
    (Some(Step::Close(Win::Xoc)), 1000, false),
    (Some(Step::Open(Win::Fan)), 1500, false),
    (Some(Step::Close(Win::Fan)), 1000, false),
    (Some(Step::Open(Win::Graphics)), 1500, false),
    (Some(Step::Close(Win::Graphics)), 1000, false),
    (Some(Step::Exit), 0, false),
];

pub struct Bench {
    script: &'static [(Option<Step>, u64, bool)],
    /// Index of the step in progress; `None` until the first frame was drawn.
    step: Option<usize>,
    since: Instant,
    cpu: Duration,
    frames: u64,
}

impl Bench {
    pub fn new(kind: crate::trace::Bench) -> Self {
        let script = match kind {
            crate::trace::Bench::Full => SCRIPT,
            crate::trace::Bench::Startup => &[(Some(Step::Exit), 0, false)],
        };
        Self { script, step: None, since: Instant::now(), cpu: Duration::ZERO, frames: 0 }
    }

    /// Advance when the current step's time is up. `frames` is the app's frame counter. Returns
    /// the action for a step just entered and how long until the next check.
    pub fn tick(&mut self, frames: u64) -> (Option<Step>, Duration) {
        let mut action = None;
        let due = match self.step {
            None => true,
            Some(i) => self.since.elapsed() >= Duration::from_millis(self.script[i].1),
        };
        if due && frames > 0 {
            if let Some(i) = self.step
                && self.script[i].2
            {
                let cpu = crate::trace::cpu_time().saturating_sub(self.cpu);
                let wall = self.since.elapsed();
                let (ws, private) = crate::trace::memory_mib();
                crate::trace!(
                    "bench: idle {:.1} s: {} frames, CPU {:.0} ms ({:.2}% of one core), working set {ws:.1} MiB, private {private:.1} MiB",
                    wall.as_secs_f64(),
                    frames - self.frames,
                    cpu.as_secs_f64() * 1000.0,
                    100.0 * cpu.as_secs_f64() / wall.as_secs_f64()
                );
            }
            let next = self.step.map_or(0, |i| i + 1).min(self.script.len() - 1);
            self.step = Some(next);
            self.since = Instant::now();
            self.cpu = crate::trace::cpu_time();
            self.frames = frames;
            action = self.script[next].0;
            if let Some(a) = action {
                let (ws, private) = crate::trace::memory_mib();
                crate::trace!("bench: {a:?} (working set {ws:.1} MiB, private {private:.1} MiB)");
            }
        }
        let i = self.step.unwrap_or(0);
        let wait = Duration::from_millis(self.script[i].1).saturating_sub(self.since.elapsed());
        (action, wait.max(Duration::from_millis(1)))
    }
}
