//! The two OCP write guards: the legacy one (pre-R615 layout) and the modern full-payload one.
//!
//! Both fail closed. An unchanged limit never reaches a setter, an unknown driver or an unreadable
//! current limit blocks the write, and the modern path verifies the requested rail moved and the
//! other one did not.

use crate::buf::{rd_i32, wr_i32};

/// `OcpWriteGuard.Apply`. `None` on success, else the reason.
pub fn legacy_apply(
    driver_version: u32,
    current_ma: i32,
    requested_ma: i32,
    write: impl FnOnce() -> i32,
) -> Option<String> {
    if current_ma <= 0 || requested_ma <= 0 {
        return Some("Cannot verify a positive OCP limit; no write attempted.".into());
    }
    // Reading an already-correct limit is sufficient, even if the setter is unavailable.
    if current_ma == requested_ma {
        return None;
    }
    // The modern path has its own full-payload validation. This guard is legacy-only.
    if driver_version == 0 || driver_version >= 61500 {
        return Some(
            "Legacy OCP writes are unavailable on this driver; the validated modern layout is required. No write attempted."
                .into(),
        );
    }
    let status = write();
    if status == 0 { None } else { Some(format!("nvapi 0xAFFC2279: status {status}")) }
}

// Verified against the reference tool's modern OCP call sites and live 616.92 read/write.
// These are full allocation sizes, NOT the low 16 bits of the version word.
pub const CONTROL_SIZE: usize = 0x2486E0;
pub const CONTROL_VERSION: i32 = 0x2786E0;
pub const RANGE_SIZE: usize = 0x2BA030;

fn entry(slot: i32) -> usize {
    0xA68 + slot as usize * 0x2448
}

fn value(slot: i32) -> usize {
    entry(slot) + 0x44
}

fn new_control() -> Vec<u8> {
    let mut b = vec![0u8; CONTROL_SIZE];
    wr_i32(&mut b, 0, CONTROL_VERSION);
    wr_i32(&mut b, 0x88, 0x1FFFF);
    b
}

fn valid(b: &[u8]) -> bool {
    b.len() == CONTROL_SIZE
        && rd_i32(b, 0) == CONTROL_VERSION
        && (rd_i32(b, 0x88) & 0x6000) == 0x6000
        && rd_i32(b, entry(13)) == 19
        && rd_i32(b, entry(14)) == 19
        && rd_i32(b, value(13)) > 0
        && rd_i32(b, value(14)) > 0
}

/// `ModernOcpControl.Apply`. `get`/`info`/`set` call the driver on the given buffer and return its
/// status. `None` on success, else the reason.
pub fn modern_apply(
    slot: i32,
    requested_ma: i32,
    mut get: impl FnMut(&mut [u8]) -> i32,
    mut info: impl FnMut(&mut [u8]) -> i32,
    mut set: impl FnMut(&mut [u8]) -> i32,
) -> Option<String> {
    if !(slot == 13 || slot == 14) || requested_ma <= 0 {
        return Some("Invalid OCP rail or nonpositive limit; no write attempted.".into());
    }
    let mut control = new_control();
    let status = get(&mut control);
    if status != 0 {
        return Some(format!("Reading modern OCP control: status {status}; no write attempted."));
    }
    if !valid(&control) {
        return Some("Modern OCP layout/rail validation failed; no write attempted.".into());
    }
    let current = rd_i32(&control, value(slot));
    if current == requested_ma {
        return None;
    }
    let other_slot = if slot == 13 { 14 } else { 13 };
    let other = rd_i32(&control, value(other_slot));

    let mut ranges = vec![0u8; RANGE_SIZE];
    wr_i32(&mut ranges, 0, RANGE_SIZE as i32);
    let status = info(&mut ranges);
    if status != 0 {
        return Some(format!("Reading modern OCP bounds: status {status}; no write attempted."));
    }
    let at = 0x4B0 + slot as usize * 0x296C;
    if rd_i32(&ranges, 0) != RANGE_SIZE as i32 || rd_i32(&ranges, at) != 19 {
        return Some("Modern OCP range layout/rail validation failed; no write attempted.".into());
    }
    let min = rd_i32(&ranges, at + 0xC);
    let def = rd_i32(&ranges, at + 0x10);
    let max = rd_i32(&ranges, at + 0x14);
    if min <= 0 || min > def || def > max || current < min || current > max || requested_ma < min || requested_ma > max
    {
        return Some("OCP request or metadata is outside the driver-reported bounds; no write attempted.".into());
    }

    // Preserve the full getter payload; modify only the selected output-current field.
    wr_i32(&mut control, value(slot), requested_ma);
    let status = set(&mut control);
    if status != 0 {
        return Some(format!("nvapi 0xAFFC2279 (modern OCP layout): status {status}"));
    }
    let mut actual = new_control();
    let status = get(&mut actual);
    if status != 0 || !valid(&actual) {
        return Some(format!("OCP write returned success but read-back could not be verified: status {status}"));
    }
    if rd_i32(&actual, value(slot)) != requested_ma || rd_i32(&actual, value(other_slot)) != other {
        return Some("OCP read-back mismatch: requested rail or untouched rail differs; recovery required.".into());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn legacy_guard() {
        let writes = Cell::new(0);
        let set = || {
            writes.set(writes.get() + 1);
            0
        };
        assert_eq!(legacy_apply(61692, 300000, 300000, set), None, "R615 unchanged OCP is verified no-op");
        assert_eq!(writes.get(), 0);
        assert!(legacy_apply(61656, 300000, 310000, set).is_some(), "R615 changed OCP blocked before driver write");
        assert_eq!(writes.get(), 0);
        assert!(legacy_apply(0, 300000, 310000, set).is_some(), "unknown driver OCP fails closed");
        assert_eq!(writes.get(), 0);
        assert!(legacy_apply(61088, 0, 300000, set).is_some(), "unknown current OCP fails closed");
        assert_eq!(writes.get(), 0);
        assert_eq!(legacy_apply(61088, 300000, 290000, set), None, "R610 changed OCP retains supported write");
        assert_eq!(writes.get(), 1);
        assert!(legacy_apply(61088, 300000, 290000, || -1).is_some(), "OCP rejection remains error");
    }

    #[test]
    fn modern_guard() {
        let control = RefCell::new(vec![0u8; 0x2486e0]);
        let ranges = RefCell::new(vec![0u8; 0x2ba030]);
        {
            let mut c = control.borrow_mut();
            let mut r = ranges.borrow_mut();
            wr_i32(&mut c, 0, 0x2786e0);
            wr_i32(&mut c, 0x88, 0x1ffff);
            wr_i32(&mut r, 0, 0x2ba030);
            for slot in [13usize, 14] {
                let value = if slot == 13 { 300000 } else { 120000 };
                let at = 0xa68 + slot * 0x2448;
                wr_i32(&mut c, at, 19);
                wr_i32(&mut c, at + 0x44, value);
                let at = 0x4b0 + slot * 0x296c;
                wr_i32(&mut r, at, 19);
                wr_i32(&mut r, at + 0xc, 100000);
                wr_i32(&mut r, at + 0x10, value);
                wr_i32(&mut r, at + 0x14, 350000);
            }
        }
        let put = |at: usize, v: i32| wr_i32(&mut control.borrow_mut(), at, v);
        let get = |b: &mut [u8]| {
            b.copy_from_slice(&control.borrow());
            0
        };
        let info = |b: &mut [u8]| {
            b.copy_from_slice(&ranges.borrow());
            0
        };
        let writes = Cell::new(0);
        let set = |b: &mut [u8]| {
            writes.set(writes.get() + 1);
            control.borrow_mut().copy_from_slice(b);
            0
        };
        let never = |_: &mut [u8]| -> i32 { panic!("must not be called") };

        assert_eq!(modern_apply(13, 290000, get, info, set), None, "writes selected rail and verifies");
        assert_eq!(writes.get(), 1);
        assert_eq!(rd_i32(&control.borrow(), 0x1e254), 290000);
        assert_eq!(rd_i32(&control.borrow(), 0x2069c), 120000);

        assert_eq!(modern_apply(13, 290000, get, never, never), None, "unchanged does not resolve setter or bounds");
        assert!(modern_apply(12, 290000, never, info, set).is_some(), "unknown rail fails before calls");
        assert!(modern_apply(13, -1, get, info, set).is_some(), "negative request blocked");
        assert!(modern_apply(13, 280000, |_| -1, info, set).is_some(), "control error blocks write");
        assert!(modern_apply(13, 280000, get, |_| -1, set).is_some(), "range error blocks write");
        assert!(modern_apply(13, 400000, get, info, set).is_some(), "out-of-range blocked");
        put(0x1e210, 18);
        assert!(modern_apply(13, 280000, get, info, set).is_some(), "wrong rail type blocked");
        put(0x1e210, 19);
        put(0, 0);
        assert!(modern_apply(13, 280000, get, info, set).is_some(), "changed header blocked");
        put(0, 0x2786e0);
        put(0x88, 0);
        assert!(modern_apply(13, 280000, get, info, set).is_some(), "missing channel mask blocked");
        put(0x88, 0x1ffff);
        assert_eq!(writes.get(), 1, "invalid modern cases never wrote");
        assert!(modern_apply(13, 280000, get, info, |_| -1).is_some(), "rejection propagated");
        assert!(modern_apply(13, 280000, get, info, |_| 0).is_some(), "ignored write detected");
        assert_eq!(modern_apply(14, 110000, get, info, set), None, "memory rail applies");
        assert_eq!(writes.get(), 2);
        let tamper = |b: &mut [u8]| {
            wr_i32(b, 0x1e254, 270000);
            set(b)
        };
        assert!(modern_apply(14, 120000, get, info, tamper).is_some(), "other rail change detected");
    }
}
