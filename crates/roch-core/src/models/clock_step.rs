//! Granularity the driver actually applies clock offsets at.
//!
//! NVIDIA quantises the core offset to 15 MHz — every clock in the V/F table is a multiple of it —
//! so a finer number is rounded away somewhere between the slider and the card. Snapping in the UI
//! keeps the value typed and the value applied the same thing.

use crate::util::clamp_i32;

/// Core clock offset granularity, MHz.
pub const CORE_MHZ: i32 = 15;

/// Memory clock offset granularity, MHz. Applies to an *offset* only: an absolute memory clock
/// (AMD) must not be snapped, or reading the card's state would overclock it.
pub const MEMORY_MHZ: i32 = 25;

/// Fan duty granularity, percent. The UI's grid, not the driver's.
pub const FAN_PERCENT: i32 = 5;

/// Over-voltage granularity, percent. The UI's grid; the CLI stays exact.
pub const VOLTAGE_BOOST_PERCENT: i32 = 5;

/// Nearest multiple of `step`, anchored on zero. Halfway rounds away from zero so -22 and +22
/// land symmetrically.
pub fn snap(value: i32, step: i32) -> i32 {
    if step <= 1 {
        return value;
    }
    ((value as f64 / step as f64).round() as i32).wrapping_mul(step)
}

/// Snap into [min, max]. Values between the limits land on the grid; the limits themselves are
/// always reachable, even off the grid, because they are the driver's own reported range: a card
/// reporting -1000..+1000 MHz must be able to sit at -1000, not stop at the nearest multiple of 15
/// (-990). A value whose grid point falls outside the range takes the limit, which is the nearer
/// reachable value.
pub fn snap_within(value: i32, step: i32, min: i32, max: i32) -> i32 {
    if min > max {
        return value;
    }
    if value <= min {
        return min;
    }
    if value >= max {
        return max;
    }
    clamp_i32(snap(value, step), min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapping() {
        assert_eq!(snap(22, 15), 15);
        assert_eq!(snap(-22, 15), -15);
        assert_eq!(snap(23, 15), 30);
        assert_eq!(snap(-23, 15), -30);
        assert_eq!(snap(7, 1), 7);
        // The driver's limits are reachable even off the 15 MHz grid…
        assert_eq!(snap_within(1000, 15, -1000, 1000), 1000);
        assert_eq!(snap_within(-1000, 15, -1000, 1000), -1000);
        assert_eq!(snap_within(5000, 15, -1000, 1000), 1000);
        // …while everything between them stays on it.
        assert_eq!(snap_within(996, 15, -1000, 1000), 990);
        assert_eq!(snap_within(998, 15, -1000, 1000), 1000, "grid point past the limit takes the limit");
        assert_eq!(snap_within(-998, 15, -1000, 1000), -1000);
        assert_eq!(snap_within(-22, 15, -1000, 1000), -15);
        assert_eq!(snap_within(3000, 25, -1000, 3000), 3000);
        assert_eq!(snap_within(7, 5, 30, 100), 30);
    }
}
