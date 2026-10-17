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

/// A .NET *custom* numeric format of a double with `min..=max` decimals, invariant culture:
/// `"0"` = (0, 0), `"0.0"` = (1, 1), `"0.#"` = (0, 1), `"0.##"` = (0, 2).
///
/// .NET first takes the value to 15 significant digits and then rounds that half away from zero,
/// so 45.25 → "45.3", 2422.5 → "2423", 2.675 → "2.68" and 0.15 → "0.2". Rust's `{:.N}` rounds the
/// exact binary value half to even ("45.2", "2422", "2.67", "0.1") — which is what .NET's
/// *standard* `F<n>` format does, so `F<n>` sites keep using `{:.N}`. A negative value keeps its
/// sign even when it rounds to zero ("-0", "-0.0"), as in .NET.
pub fn dotnet_custom(v: f64, min: usize, max: usize) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "∞".into() } else { "-∞".into() };
    }
    let max = max.max(min);
    // 15 significant digits: "d.dddddddddddddd" and a decimal exponent.
    let sci = format!("{:.14e}", v.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i64 = exp.parse().unwrap_or(0);
    let mut digits: Vec<u8> = mantissa.bytes().filter(u8::is_ascii_digit).map(|b| b - b'0').collect();
    // `point` digits sit before the decimal point.
    let mut point = exp + 1;
    if point < 0 {
        let mut padded = vec![0u8; (-point) as usize];
        padded.extend(digits);
        digits = padded;
        point = 0;
    }
    let mut point = point as usize;
    let keep = point + max;
    if keep < digits.len() {
        let up = digits[keep] >= 5;
        digits.truncate(keep);
        if up {
            let mut i = digits.len();
            loop {
                if i == 0 {
                    digits.insert(0, 1);
                    point += 1;
                    break;
                }
                i -= 1;
                if digits[i] == 9 {
                    digits[i] = 0;
                } else {
                    digits[i] += 1;
                    break;
                }
            }
        }
    }
    digits.resize(digits.len().max(point + max), 0);
    let int: String = digits[..point].iter().map(|d| char::from(b'0' + d)).collect();
    let int = int.trim_start_matches('0');
    let mut frac: Vec<u8> = digits[point..point + max].to_vec();
    while frac.len() > min && frac.last() == Some(&0) {
        frac.pop();
    }
    let mut s = String::new();
    if v.is_sign_negative() {
        s.push('-');
    }
    s.push_str(if int.is_empty() { "0" } else { int });
    if !frac.is_empty() {
        s.push('.');
        s.extend(frac.iter().map(|d| char::from(b'0' + d)));
    }
    s
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

    /// Pinned against .NET 10 on en-US (the audit's format probes, `docs/rust-port-audit.md`).
    #[test]
    fn custom_formats_match_dotnet() {
        let cases: &[(f64, usize, usize, &str)] = &[
            (45.25, 1, 1, "45.3"),
            (2422.5, 0, 0, "2423"),
            (1010.5, 0, 0, "1011"),
            (110.25, 1, 1, "110.3"),
            (0.15, 1, 1, "0.2"),
            (0.25, 0, 1, "0.3"),
            (2.675, 0, 2, "2.68"),
            (1.005, 0, 2, "1.01"),
            (9.995, 0, 2, "10"),
            (99.95, 1, 1, "100.0"),
            (0.95, 0, 0, "1"),
            (0.5, 0, 0, "1"),
            (-0.5, 0, 0, "-1"),
            (-2.5, 0, 0, "-3"),
            (-0.04, 0, 0, "-0"),
            (-0.04, 1, 1, "-0.0"),
            (0.04, 0, 1, "0"),
            (0.04, 0, 2, "0.04"),
            (0.0, 1, 1, "0.0"),
            (1e-7, 0, 2, "0"),
            (0.000123456, 1, 1, "0.0"),
            (11.994140625, 0, 1, "12"),
            (11.994140625, 0, 2, "11.99"),
            (15.9208984375, 0, 1, "15.9"),
            (12.125, 0, 2, "12.13"),
            (30.5, 0, 1, "30.5"),
            (1234567.891, 0, 0, "1234568"),
            (123456789.125, 0, 2, "123456789.13"),
            (1e15 + 0.5, 1, 1, "1000000000000000.0"),
        ];
        for &(v, min, max, want) in cases {
            assert_eq!(dotnet_custom(v, min, max), want, "{v} with {min}..{max} decimals");
        }
        assert_eq!(dotnet_custom(f64::NAN, 0, 0), "NaN");
    }

    #[test]
    fn clamp_never_panics() {
        assert_eq!(clamp_i32(5, 10, 0), 10);
        assert_eq!(clamp_i32(5, 0, 10), 5);
        assert!(clamp_f64(f64::NAN, 0.0, 1.0).is_nan());
    }
}
