//! Read-modify-write-verify for one clock domain's offset in the private clock-domain control
//! block (crossbar, SYS, video). Fails closed: an unknown layout or a header the driver did not
//! fill in means no write is attempted, and a write that does not read back is reported.

use crate::buf::{rd_i32, wr_i32};

pub const SIZE: usize = 0x61A4;
pub const VERSION: i32 = 0x261A4;
pub const INVALID_REQUEST: i32 = -10001;
pub const INVALID_LAYOUT: i32 = -10002;
pub const VERIFICATION_FAILED: i32 = -10003;

/// Byte offset of `slot`'s offset field, or -1 when the block layout is not one we know.
///
/// The leading block word identifies the layout, not a domain count. Verified layout 10 (Ada) uses
/// +0x10C; layout 15 uses +0x114.
pub fn offset_field(control: &[u8], slot: i32) -> i32 {
    if !(0..32).contains(&slot) || control.len() != SIZE {
        return -1;
    }
    let block = 0x124 + slot as usize * 0x304;
    if block + 0x118 > control.len() {
        return -1;
    }
    match rd_i32(control, block) {
        10 => (block + 0x10C) as i32,
        15 => (block + 0x114) as i32,
        _ => -1,
    }
}

/// Apply `mhz` to `slot`. `get` and `set` call the driver on a `SIZE`-byte buffer and return its
/// status. Returns 0 on a verified write (or when the value is already there).
pub fn apply(slot: i32, mhz: i32, mut get: impl FnMut(&mut [u8]) -> i32, mut set: impl FnMut(&mut [u8]) -> i32) -> i32 {
    if !(0..32).contains(&slot) || !(i32::MIN / 1000..=i32::MAX / 1000).contains(&mhz) {
        return INVALID_REQUEST;
    }
    let mask = 1i32 << slot;
    let new_request = || {
        let mut b = vec![0u8; SIZE];
        wr_i32(&mut b, 0, VERSION);
        wr_i32(&mut b, 8, mask);
        b
    };
    let valid = |b: &[u8]| rd_i32(b, 0) == VERSION && (rd_i32(b, 8) & mask) == mask;

    let mut control = new_request();
    let status = get(&mut control);
    if status != 0 {
        return status;
    }
    if !valid(&control) {
        return INVALID_LAYOUT;
    }
    let field = offset_field(&control, slot);
    if field < 0 {
        return INVALID_LAYOUT;
    }
    let requested = mhz * 1000;
    if rd_i32(&control, field as usize) == requested {
        return 0;
    }
    // Preserve the getter payload, selecting only the requested domain.
    wr_i32(&mut control, 8, mask);
    wr_i32(&mut control, field as usize, requested);
    let status = set(&mut control);
    if status != 0 {
        return status;
    }
    let mut verify = new_request();
    let status = get(&mut verify);
    if status != 0 {
        return status;
    }
    if !valid(&verify) || offset_field(&verify, slot) != field {
        return INVALID_LAYOUT;
    }
    if rd_i32(&verify, field as usize) == requested { 0 } else { VERIFICATION_FAILED }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn fails_closed_and_verifies() {
        for layout in [10, 15] {
            for slot in [1, 3] {
                let live = RefCell::new(vec![0u8; SIZE]);
                {
                    let mut l = live.borrow_mut();
                    wr_i32(&mut l, 0, VERSION);
                    wr_i32(&mut l, 8, 1 << slot);
                    wr_i32(&mut l, 0x124 + slot as usize * 0x304, layout);
                    l[100] = 42;
                }
                let expected_field = 0x124 + slot as usize * 0x304 + if layout == 10 { 0x10C } else { 0x114 };
                let writes = Cell::new(0);
                let read = |b: &mut [u8]| {
                    b.copy_from_slice(&live.borrow());
                    0
                };
                let write = |b: &mut [u8]| {
                    writes.set(writes.get() + 1);
                    {
                        let l = live.borrow();
                        let same = b
                            .iter()
                            .zip(l.iter())
                            .enumerate()
                            .all(|(i, (x, y))| (i >= expected_field && i < expected_field + 4) || x == y);
                        assert!(same, "domain changes only selected offset");
                    }
                    assert_eq!(b[100], 42, "domain preserves unrelated payload");
                    live.borrow_mut().copy_from_slice(b);
                    0
                };

                assert_eq!(apply(slot, -15, |_| -9, write), -9, "getter failure prevents setter");
                assert_eq!(writes.get(), 0);
                assert_eq!(
                    apply(
                        slot,
                        -15,
                        |b| {
                            b.fill(0);
                            0
                        },
                        write
                    ),
                    INVALID_LAYOUT,
                    "invalid header prevents setter"
                );
                assert_eq!(writes.get(), 0);
                assert_eq!(
                    apply(
                        slot,
                        -15,
                        |b| {
                            read(b);
                            wr_i32(b, 0x124 + slot as usize * 0x304, 99);
                            0
                        },
                        write
                    ),
                    INVALID_LAYOUT,
                    "unknown inner layout prevents write"
                );
                assert_eq!(writes.get(), 0);
                assert_eq!(apply(slot, 0, read, write), 0, "unchanged is no-op");
                assert_eq!(writes.get(), 0);
                assert_eq!(apply(slot, -15, read, |_| -1), -1, "rejected write propagates");
                assert_eq!(apply(slot, -15, read, |_| 0), VERIFICATION_FAILED, "ignored write fails verification");
                assert_eq!(apply(slot, -15, read, write), 0, "applies negative offset");
                assert_eq!(writes.get(), 1);
                assert_eq!(offset_field(&live.borrow(), slot), expected_field as i32, "correct layout offset field");
                assert_eq!(apply(slot, 0, read, write), 0, "restores offset");
                assert_eq!(writes.get(), 2);
            }
        }
        let never = |_: &mut [u8]| -> i32 { panic!("must not be called") };
        assert_eq!(apply(-1, 0, never, never), INVALID_REQUEST, "negative domain blocked");
        assert_eq!(apply(32, 0, never, never), INVALID_REQUEST, "oversized domain blocked");
        assert_eq!(apply(1, i32::MAX, never, never), INVALID_REQUEST, "overflowing offset blocked");
    }
}
