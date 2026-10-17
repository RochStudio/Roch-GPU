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

/// Practical offset ranges for the sliders, intersected with whatever the driver reports. The
/// driver describes the width of its delta field (±1000 core on Ada), not anything a card will do.
/// The ends are on the 15/25 MHz grids so they stay reachable. These only ever narrow.
pub const CORE_OFFSET_PRACTICAL_MIN_MHZ: i32 = -150;
pub const CORE_OFFSET_PRACTICAL_MAX_MHZ: i32 = 750;
pub const MEMORY_OFFSET_PRACTICAL_MIN_MHZ: i32 = -250;
pub const MEMORY_OFFSET_PRACTICAL_MAX_MHZ: i32 = 4000;

/// Practical crossbar offset range. Nothing here has been shown to be stable; a crossbar offset
/// that is too high browns the card out rather than failing. 750 is on the 15 MHz grid.
pub const XBAR_OFFSET_PRACTICAL_MIN_MHZ: i32 = -150;
pub const XBAR_OFFSET_PRACTICAL_MAX_MHZ: i32 = 750;

/// Intersect a driver-reported range with a practical one; never widens. A driver range that
/// misses the practical window entirely is left alone rather than inverted.
pub fn narrow(driver_min: i32, driver_max: i32, practical_min: i32, practical_max: i32) -> (i32, i32) {
    let min = driver_min.max(practical_min);
    let max = driver_max.min(practical_max);
    if min <= max { (min, max) } else { (driver_min, driver_max) }
}

/// Nearest multiple of `step`, anchored on zero. Halfway rounds away from zero so -22 and +22
/// land symmetrically.
pub fn snap(value: i32, step: i32) -> i32 {
    if step <= 1 {
        return value;
    }
    ((value as f64 / step as f64).round() as i32).wrapping_mul(step)
}

/// Snap, then keep the result inside [min, max] without falling off the grid: stepping back
/// toward zero gives the nearest in-range value that is still a multiple of the step.
pub fn snap_within(value: i32, step: i32, min: i32, max: i32) -> i32 {
    if min > max {
        return value;
    }
    let mut snapped = snap(clamp_i32(value, min, max), step);
    if step <= 1 {
        return clamp_i32(snapped, min, max);
    }
    if snapped > max {
        snapped = max / step * step; // integer division truncates toward zero
    }
    if snapped < min {
        snapped = -(-min / step * step);
    }
    clamp_i32(snapped, min, max)
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
        assert_eq!(snap_within(1000, 15, -1000, 1000), 990);
        assert_eq!(snap_within(-1000, 15, -1000, 1000), -990);
        assert_eq!(narrow(-1000, 1000, -150, 750), (-150, 750));
        assert_eq!(narrow(800, 1000, -150, 750), (800, 1000));
    }
}
