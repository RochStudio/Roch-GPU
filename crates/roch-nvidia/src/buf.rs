//! Zeroed, 8-byte-aligned scratch buffers for the hand-built private NVAPI calls, plus
//! never-panicking little-endian readers and writers over byte slices.
//!
//! The C# used `Marshal.AllocHGlobal` + `Marshal.ReadInt32`; an out-of-range read there threw. Here
//! an out-of-range read returns 0 and an out-of-range write is dropped, because nothing driven by
//! driver data may panic.

use std::ffi::c_void;

/// A zeroed buffer of `size + slack` bytes (rounded up to 8), 8-byte aligned.
pub struct Buf {
    words: Vec<u64>,
    size: usize,
}

impl Buf {
    /// `size` is what the version word will declare; `slack` is extra room past it so a driver that
    /// writes more than declared lands in our own memory.
    pub fn new(size: usize, slack: usize) -> Self {
        let total = size.saturating_add(slack);
        Buf { words: vec![0u64; total.div_ceil(8).max(1)], size }
    }

    /// Declared size (without slack).
    pub fn size(&self) -> usize {
        self.size
    }

    pub fn ptr(&mut self) -> *mut c_void {
        self.words.as_mut_ptr() as *mut c_void
    }

    /// The whole allocation as bytes.
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: a u64 slice reinterpreted as bytes; same allocation, length scaled by 8.
        unsafe { std::slice::from_raw_parts(self.words.as_ptr() as *const u8, self.words.len() * 8) }
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above, exclusive.
        unsafe { std::slice::from_raw_parts_mut(self.words.as_mut_ptr() as *mut u8, self.words.len() * 8) }
    }

    /// The declared region only, copied out.
    pub fn declared(&self) -> Vec<u8> {
        let b = self.bytes();
        b[..self.size.min(b.len())].to_vec()
    }

    pub fn i32(&self, off: usize) -> i32 {
        rd_i32(self.bytes(), off)
    }

    pub fn u32(&self, off: usize) -> u32 {
        rd_i32(self.bytes(), off) as u32
    }

    pub fn put(&mut self, off: usize, v: i32) {
        wr_i32(self.bytes_mut(), off, v)
    }

    pub fn put_u32(&mut self, off: usize, v: u32) {
        wr_i32(self.bytes_mut(), off, v as i32)
    }

    /// The declared region as 32-bit words (`size / 4` of them), as `CallRaw` returned it.
    pub fn words(&self) -> Vec<i32> {
        (0..self.size / 4).map(|i| self.i32(i * 4)).collect()
    }

    /// Copy `data` to the start of the buffer (truncating to the allocation).
    pub fn load(&mut self, data: &[u8]) {
        let b = self.bytes_mut();
        let n = data.len().min(b.len());
        b[..n].copy_from_slice(&data[..n]);
    }
}

/// Little-endian i32 at `off`; 0 when out of range.
pub fn rd_i32(b: &[u8], off: usize) -> i32 {
    match b.get(off..off.saturating_add(4)) {
        Some(s) if s.len() == 4 => i32::from_le_bytes([s[0], s[1], s[2], s[3]]),
        _ => 0,
    }
}

pub fn rd_u32(b: &[u8], off: usize) -> u32 {
    rd_i32(b, off) as u32
}

/// Little-endian i16 at `off`; 0 when out of range.
pub fn rd_i16(b: &[u8], off: usize) -> i16 {
    match b.get(off..off.saturating_add(2)) {
        Some(s) if s.len() == 2 => i16::from_le_bytes([s[0], s[1]]),
        _ => 0,
    }
}

/// Write a little-endian i32 at `off`; dropped when out of range.
pub fn wr_i32(b: &mut [u8], off: usize, v: i32) {
    if let Some(s) = b.get_mut(off..off.saturating_add(4))
        && s.len() == 4
    {
        s.copy_from_slice(&v.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_never_panic() {
        let b = [1u8, 0, 0, 0, 0xFF, 0xFF];
        assert_eq!(rd_i32(&b, 0), 1);
        assert_eq!(rd_i32(&b, 4), 0);
        assert_eq!(rd_i32(&b, usize::MAX), 0);
        assert_eq!(rd_i16(&b, 4), -1);
        let mut m = [0u8; 4];
        wr_i32(&mut m, 2, 7);
        assert_eq!(m, [0; 4]);
        let mut buf = Buf::new(12, 4);
        buf.put(8, -5);
        assert_eq!(buf.i32(8), -5);
        assert_eq!(buf.words(), vec![0, 0, -5]);
        assert_eq!(buf.ptr() as usize % 8, 0);
    }
}
