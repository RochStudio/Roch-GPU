//! Small text helpers shared by the view model and the windows: the .NET format strings the WPF
//! build used (`+#;-#;0`, `0.#`), the value-box parser, and the "Label: value" split.

/// Signed display, the way an offset reads on a slider: "+150", "-90", "0" (C# `+#;-#;0`).
pub fn signed(v: i32) -> String {
    if v > 0 { format!("+{v}") } else { v.to_string() }
}

/// At most one decimal, trailing zero dropped (C# `0.#`), with .NET's custom-format rounding:
/// exact ties go away from zero (15.25 → "15.3", where `{:.1}` gives "15.2").
pub fn one_decimal(v: f64) -> String {
    roch_core::util::dotnet_custom(v, 0, 1)
}

/// Parse a user-typed value: leading +/- allowed, whitespace trimmed, trailing units ignored
/// ("+15", " -20 ", "60 MHz"). Garbage or an empty box is `None`, which leaves the value alone.
pub fn parse_signed(s: &str) -> Option<i32> {
    let s = s.trim();
    let (sign, digits) = match s.as_bytes().first()? {
        b'+' => (1i64, &s[1..]),
        b'-' => (-1i64, &s[1..]),
        _ => (1i64, s),
    };
    let end = digits.bytes().take_while(u8::is_ascii_digit).count();
    if end == 0 {
        return None;
    }
    let magnitude: i64 = digits[..end].parse().ok()?;
    i32::try_from(sign * magnitude).ok()
}

/// Split "Driver: 581.42" into ("Driver: ", "581.42") so the value can take the accent colour.
/// A line with no ": " is all value (C# `InformationLinePartConverter`).
pub fn info_line_parts(text: &str) -> (&str, &str) {
    match text.find(": ") {
        Some(i) => (&text[..i + 2], &text[i + 2..]),
        None => ("", text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_matches_dotnet_format() {
        assert_eq!(signed(150), "+150");
        assert_eq!(signed(-90), "-90");
        assert_eq!(signed(0), "0");
    }

    #[test]
    fn one_decimal_trims() {
        assert_eq!(one_decimal(15.9), "15.9");
        assert_eq!(one_decimal(16.0), "16");
        assert_eq!(one_decimal(12.04), "12");
        assert_eq!(one_decimal(15.25), "15.3", "an exact tie rounds away from zero, as .NET's 0.#");
        assert_eq!(one_decimal(0.15), "0.2", ".NET rounds the 15-digit value");
    }

    #[test]
    fn value_box_parsing() {
        assert_eq!(parse_signed("+15"), Some(15));
        assert_eq!(parse_signed(" -20 "), Some(-20));
        assert_eq!(parse_signed("60 MHz"), Some(60));
        assert_eq!(parse_signed("+"), None);
        assert_eq!(parse_signed(""), None);
        assert_eq!(parse_signed("abc"), None);
        assert_eq!(parse_signed("99999999999"), None);
    }

    #[test]
    fn info_lines_split_on_the_first_colon() {
        assert_eq!(info_line_parts("Driver: 581.42"), ("Driver: ", "581.42"));
        assert_eq!(info_line_parts("Simulated"), ("", "Simulated"));
        assert_eq!(info_line_parts(""), ("", ""));
    }
}
