//! Numeric helpers that reproduce .NET semantics the original relied on.
//!
//! The C# code used `Math.Round(x)`, which rounds halves to even, and `Math.Clamp`, which throws
//! when the bounds are inverted. Rust's `f64::round` rounds halves away from zero and `clamp`
//! panics, so ports go through these instead.

/// `Math.Round(x)`: nearest integer, halves to even.
#[inline]
pub fn round_even(x: f64) -> f64 {
    x.round_ties_even()
}

/// `Math.Round(x, digits)`: halves to even at `digits` decimals.
#[inline]
pub fn round_even_to(x: f64, digits: i32) -> f64 {
    let scale = 10f64.powi(digits);
    (x * scale).round_ties_even() / scale
}

/// `(int)Math.Round(x)`.
#[inline]
pub fn round_even_i32(x: f64) -> i32 {
    x.round_ties_even() as i32
}

/// `Math.Round(x, MidpointRounding.AwayFromZero)`.
#[inline]
pub fn round_away(x: f64) -> f64 {
    x.round()
}

/// `Math.Clamp` for integers that never panics: with inverted bounds the lower bound wins, which
/// is what a slider whose range collapsed would show.
#[inline]
pub fn clamp_i32(v: i32, lo: i32, hi: i32) -> i32 {
    if lo > hi { lo } else { v.clamp(lo, hi) }
}

/// `Math.Clamp` for doubles that never panics (NaN passes through, as in .NET).
#[inline]
pub fn clamp_f64(v: f64, lo: f64, hi: f64) -> f64 {
    if v.is_nan() {
        return v;
    }
    if lo > hi { lo } else { v.max(lo).min(hi) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_matches_dotnet() {
        assert_eq!(round_even(2.5), 2.0);
        assert_eq!(round_even(3.5), 4.0);
        assert_eq!(round_even(-2.5), -2.0);
        assert_eq!(round_away(2.5), 3.0);
        assert_eq!(round_away(-2.5), -3.0);
        assert_eq!(round_even_to(1.25, 1), 1.2);
    }

    #[test]
    fn clamp_never_panics() {
        assert_eq!(clamp_i32(5, 10, 0), 10);
        assert_eq!(clamp_i32(5, 0, 10), 5);
        assert!(clamp_f64(f64::NAN, 0.0, 1.0).is_nan());
    }
}
