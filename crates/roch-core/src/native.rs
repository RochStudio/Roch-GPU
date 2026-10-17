//! Loading vendor DLLs from the installed driver.
//!
//! Every vendor library is resolved from System32 only — never beside the elevated executable or
//! in its working folder, where a planted DLL would run with administrator rights.

use crate::{GpuError, Result};

pub use libloading::Library;

/// Load `name` (e.g. "nvapi64.dll") from System32 only.
#[cfg(windows)]
pub fn load_system32(name: &str) -> Result<Library> {
    use libloading::os::windows::{LOAD_LIBRARY_SEARCH_SYSTEM32, Library as WinLibrary};
    // SAFETY: loading a driver-supplied library runs its DllMain; that is the point of the call,
    // and the search is restricted to System32.
    unsafe { WinLibrary::load_with_flags(name, LOAD_LIBRARY_SEARCH_SYSTEM32) }
        .map(Library::from)
        .map_err(|e| GpuError::new(format!("Could not load {name}: {e}")))
}

#[cfg(not(windows))]
pub fn load_system32(name: &str) -> Result<Library> {
    Err(GpuError::new(format!("{name} is only available on Windows")))
}

/// Resolve an export as a copied function pointer. `F` must be an `extern "system"`/`"C"` fn
/// pointer type matching the export's real signature.
///
/// # Safety
/// The caller asserts the signature; a mismatch is undefined behaviour at call time. The pointer
/// is only valid while `lib` stays loaded.
pub unsafe fn symbol<F: Copy>(lib: &Library, name: &str) -> Result<F> {
    let mut cname = name.as_bytes().to_vec();
    cname.push(0);
    unsafe { lib.get::<F>(&cname) }.map(|s| *s).map_err(|e| GpuError::new(format!("Missing export {name}: {e}")))
}

/// Same as [`symbol`] but `None` when the export is absent (optional entry points).
///
/// # Safety
/// See [`symbol`].
pub unsafe fn optional_symbol<F: Copy>(lib: &Library, name: &str) -> Option<F> {
    unsafe { symbol::<F>(lib, name) }.ok()
}

/// Decode a NUL-terminated byte buffer (ANSI/UTF-8 from a vendor struct) into a String.
pub fn c_str(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

/// Decode a NUL-terminated i8 buffer (C `char[]`).
pub fn c_str_i8(chars: &[i8]) -> String {
    let bytes: Vec<u8> = chars.iter().map(|&c| c as u8).collect();
    c_str(&bytes)
}
