//! The window an OCP current limit may be moved within, in whole amps: half the stock figure at
//! the bottom, half again above it at the top. The driver refuses anything outside its own window
//! regardless, so a slider offering more would only offer values that come back as errors.

pub fn min_a(stock_milliamps: i32) -> i32 {
    if stock_milliamps > 0 { stock_milliamps / 2000 } else { 0 }
}

pub fn max_a(stock_milliamps: i32) -> i32 {
    if stock_milliamps > 0 { stock_milliamps.wrapping_mul(3) / 2000 } else { 0 }
}
