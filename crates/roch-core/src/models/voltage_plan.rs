//! Translates one absolute "I want the core to run at N mV" target into the two levers NVIDIA
//! actually exposes, which pull in opposite directions:
//!
//!   above the stock curve top  → voltage boost %, raising the ceiling
//!   below the stock curve top  → V/F curve flatten, so the card stops asking for more
//!   exactly at the top         → neither; stock behaviour
//!
//! Presenting them as one slider is honest because they are mutually exclusive.

use crate::util::round_even_i32;

/// Returns the boost percent (0-100) and the negative mV curve offset (0 = no flatten).
pub fn compute(target_mv: i32, stock_max_mv: i32, max_mv: i32) -> (i32, i32) {
    if target_mv <= 0 || stock_max_mv <= 0 {
        return (0, 0); // "leave it alone"
    }
    if target_mv > stock_max_mv {
        let headroom = max_mv - stock_max_mv;
        if headroom <= 0 {
            return (0, 0); // no boost available on this card
        }
        let pct = round_even_i32((target_mv - stock_max_mv) as f64 * 100.0 / headroom as f64);
        return (pct.clamp(0, 100), 0);
    }
    if target_mv == stock_max_mv {
        return (0, 0);
    }
    (0, target_mv - stock_max_mv) // negative → flatten above target
}

/// The voltage a given boost/offset pair corresponds to — used to seed the slider.
pub fn to_target_mv(boost_percent: i32, curve_offset_mv: i32, stock_max_mv: i32, max_mv: i32) -> i32 {
    if stock_max_mv <= 0 {
        return 0;
    }
    if curve_offset_mv < 0 {
        return stock_max_mv + curve_offset_mv;
    }
    if boost_percent > 0 {
        return stock_max_mv + round_even_i32(boost_percent as f64 / 100.0 * (max_mv - stock_max_mv) as f64);
    }
    stock_max_mv
}

/// The highest voltage the card can actually select right now: its measured roof with any boost
/// applied, brought down by a flattened curve when that sits lower.
///
/// The two inputs are measured against different baselines. `stock_ceiling_mv` is what the card
/// has been observed to reach under load; `curve_offset_mv` is measured against `curve_top_mv`,
/// the top of the V/F table. Convert the offset back to an absolute voltage against its own
/// baseline first, then take whichever roof is lower.
pub fn ceiling_mv(
    boost_percent: i32,
    curve_offset_mv: i32,
    stock_ceiling_mv: i32,
    max_mv: i32,
    curve_top_mv: i32,
) -> i32 {
    let mut ceiling = to_target_mv(boost_percent, 0, stock_ceiling_mv, max_mv);
    if curve_offset_mv < 0 && curve_top_mv > 0 {
        ceiling = ceiling.min(curve_top_mv + curve_offset_mv);
    }
    ceiling
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan() {
        assert_eq!(compute(0, 1100, 1160), (0, 0));
        assert_eq!(compute(1100, 1100, 1160), (0, 0));
        assert_eq!(compute(1000, 1100, 1160), (0, -100));
        assert_eq!(compute(1130, 1100, 1160), (50, 0));
        assert_eq!(compute(1300, 1100, 1160), (100, 0));
        assert_eq!(compute(1130, 1100, 1100), (0, 0));
        assert_eq!(to_target_mv(50, 0, 1100, 1160), 1130);
        assert_eq!(to_target_mv(0, -100, 1100, 1160), 1000);
        assert_eq!(ceiling_mv(0, -240, 1035, 1100, 1240), 1000);
    }
}
