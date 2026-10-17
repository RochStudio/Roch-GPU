//! The levers behind their own Enable/Disable button in the XOC window. Each is armed separately,
//! because they fail in unrelated ways: a rail ceiling that browns the card out says nothing about
//! whether a crossbar offset is stable.

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// A set of XOC levers (a .NET `[Flags]` enum in the original).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct XocLever(pub u32);

impl XocLever {
    pub const NONE: XocLever = XocLever(0);
    pub const NVVDD: XocLever = XocLever(1);
    pub const MSVDD: XocLever = XocLever(2);
    pub const XBAR: XocLever = XocLever(4);
    pub const SYS: XocLever = XocLever(8);
    pub const VIDEO: XocLever = XocLever(16);
    pub const CLOCK_RANGE: XocLever = XocLever(32);
    pub const NVVDD_OCP: XocLever = XocLever(64);
    pub const MSVDD_OCP: XocLever = XocLever(128);
    pub const ALL: XocLever = XocLever(255);

    /// Every single lever, in bit order, with the name .NET's enum formatting uses.
    pub const NAMED: [(XocLever, &'static str); 8] = [
        (Self::NVVDD, "Nvvdd"),
        (Self::MSVDD, "Msvdd"),
        (Self::XBAR, "Xbar"),
        (Self::SYS, "Sys"),
        (Self::VIDEO, "Video"),
        (Self::CLOCK_RANGE, "ClockRange"),
        (Self::NVVDD_OCP, "NvvddOcp"),
        (Self::MSVDD_OCP, "MsvddOcp"),
    ];

    /// `(set & one) == one`.
    pub fn has(self, one: XocLever) -> bool {
        self.0 & one.0 == one.0
    }

    pub fn with(self, one: XocLever, on: bool) -> XocLever {
        if on { XocLever(self.0 | one.0) } else { XocLever(self.0 & !one.0) }
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Parse a single lever, a number, or a .NET flags list such as "Nvvdd, Msvdd"
    /// (case-insensitive).
    pub fn parse(text: &str) -> Option<XocLever> {
        let text = text.trim();
        if let Ok(n) = text.parse::<i64>() {
            return Some(XocLever(n as u32));
        }
        let mut bits = 0u32;
        for part in text.split(',') {
            let part = part.trim();
            if part.eq_ignore_ascii_case("None") {
                continue;
            }
            if part.eq_ignore_ascii_case("All") {
                bits |= Self::ALL.0;
                continue;
            }
            let (lever, _) = Self::NAMED.iter().find(|(_, n)| n.eq_ignore_ascii_case(part))?;
            bits |= lever.0;
        }
        Some(XocLever(bits))
    }

    /// The single levers contained in this set, in bit order.
    pub fn levers(self) -> impl Iterator<Item = XocLever> {
        Self::NAMED.into_iter().map(|(l, _)| l).filter(move |l| self.has(*l))
    }
}

impl fmt::Display for XocLever {
    /// Matches .NET `[Flags]` formatting: "None", "All", or names joined by ", " in bit order.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0 {
            return f.write_str("None");
        }
        if self.0 == Self::ALL.0 {
            return f.write_str("All");
        }
        if self.0 & !Self::ALL.0 != 0 {
            return write!(f, "{}", self.0);
        }
        let names: Vec<&str> = Self::NAMED.iter().filter(|(l, _)| self.has(*l)).map(|(_, n)| *n).collect();
        f.write_str(&names.join(", "))
    }
}

impl std::ops::BitOr for XocLever {
    type Output = XocLever;
    fn bitor(self, rhs: XocLever) -> XocLever {
        XocLever(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for XocLever {
    fn bitor_assign(&mut self, rhs: XocLever) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitAnd for XocLever {
    type Output = XocLever;
    fn bitand(self, rhs: XocLever) -> XocLever {
        XocLever(self.0 & rhs.0)
    }
}

impl std::ops::Not for XocLever {
    type Output = XocLever;
    /// Complement within the known levers.
    fn not(self) -> XocLever {
        XocLever(!self.0 & Self::ALL.0)
    }
}

impl Serialize for XocLever {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for XocLever {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = XocLever;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an XocLever name list or number")
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<XocLever, E> {
                Ok(XocLever(v as u32))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<XocLever, E> {
                Ok(XocLever(v as u32))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<XocLever, E> {
                XocLever::parse(v).ok_or_else(|| E::custom(format!("unknown XocLever '{v}'")))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<XocLever, E> {
                Ok(XocLever::NONE)
            }
        }
        d.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotnet_flag_names_round_trip() {
        let set = XocLever::NVVDD | XocLever::MSVDD;
        assert_eq!(set.to_string(), "Nvvdd, Msvdd");
        assert_eq!(XocLever::parse("Nvvdd, Msvdd"), Some(set));
        assert_eq!(XocLever::ALL.to_string(), "All");
        assert_eq!(XocLever::NONE.to_string(), "None");
        assert_eq!(XocLever::parse("3"), Some(set));
        assert_eq!(serde_json::to_string(&set).unwrap(), "\"Nvvdd, Msvdd\"");
        let back: XocLever = serde_json::from_str("3").unwrap();
        assert_eq!(back, set);
        assert!(XocLever::ALL.has(XocLever::MSVDD_OCP));
        assert_eq!(XocLever::ALL.with(XocLever::XBAR, false).has(XocLever::XBAR), false);
    }
}
