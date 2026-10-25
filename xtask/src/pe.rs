//! Bounds-checked PE export-name reader; never executes code from the inspected file.
use crate::Result;

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16> {
    let end = offset.checked_add(2).ok_or("PE offset overflow")?;
    Ok(u16::from_le_bytes(bytes.get(offset..end).ok_or("Truncated PE")?.try_into().unwrap()))
}
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    let end = offset.checked_add(4).ok_or("PE offset overflow")?;
    Ok(u32::from_le_bytes(bytes.get(offset..end).ok_or("Truncated PE")?.try_into().unwrap()))
}

pub fn exports(bytes: &[u8]) -> Result<Vec<String>> {
    if bytes.get(..2) != Some(b"MZ") {
        return Err("Not a PE file".into());
    }
    let pe = u32_at(bytes, 0x3c)? as usize;
    if bytes.get(pe..pe.checked_add(4).ok_or("PE offset overflow")?) != Some(b"PE\0\0") {
        return Err("Invalid PE signature".into());
    }
    let sections = usize::from(u16_at(bytes, pe + 6)?);
    if sections > 96 {
        return Err("Invalid section count".into());
    }
    let optional = pe + 24;
    let optional_size = usize::from(u16_at(bytes, pe + 20)?);
    let directory = match u16_at(bytes, optional)? {
        0x20b => 112,
        0x10b => 96,
        _ => return Err("Unknown PE optional header".into()),
    };
    if optional_size < directory + 8 {
        return Err("Truncated PE data directory".into());
    }
    let headers = u32_at(bytes, optional + 60)?;
    let section_table = optional.checked_add(optional_size).ok_or("PE offset overflow")?;
    let rva = |address: u32| -> Result<usize> {
        if address < headers && (address as usize) < bytes.len() {
            return Ok(address as usize);
        }
        for index in 0..sections {
            let at = section_table + index * 40;
            let start = u32_at(bytes, at + 12)?;
            let size = u32_at(bytes, at + 16)?;
            if address >= start && address - start < size {
                let raw = u32_at(bytes, at + 20)?.checked_add(address - start).ok_or("PE RVA overflow")? as usize;
                if raw < bytes.len() {
                    return Ok(raw);
                }
            }
        }
        Err("PE RVA outside file-backed sections".into())
    };
    let export_rva = u32_at(bytes, optional + directory)?;
    if export_rva == 0 {
        return Ok(Vec::new());
    }
    let export = rva(export_rva)?;
    let count = u32_at(bytes, export + 24)? as usize;
    if count > 100_000 {
        return Err("Unreasonable PE export count".into());
    }
    let names = rva(u32_at(bytes, export + 32)?)?;
    let mut result = Vec::with_capacity(count);
    for index in 0..count {
        let at = rva(u32_at(bytes, names + index * 4)?)?;
        let tail = bytes.get(at..).ok_or("Invalid export name")?;
        let end = tail.iter().take(512).position(|b| *b == 0).ok_or("Unterminated export name")?;
        result.push(std::str::from_utf8(&tail[..end]).map_err(|e| e.to_string())?.to_owned());
    }
    result.sort();
    result.dedup();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Vec<u8> {
        let mut b = vec![0u8; 0x400];
        b[..2].copy_from_slice(b"MZ");
        b[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        b[0x86..0x88].copy_from_slice(&1u16.to_le_bytes());
        b[0x94..0x96].copy_from_slice(&0xf0u16.to_le_bytes());
        b[0x98..0x9a].copy_from_slice(&0x20bu16.to_le_bytes());
        for (at, value) in [
            (0x98 + 112, 0x1000u32),
            (0x188 + 12, 0x1000),
            (0x188 + 16, 0x200),
            (0x188 + 20, 0x200),
            (0x200 + 24, 1),
            (0x200 + 32, 0x1040),
            (0x240, 0x1080),
        ] {
            b[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        b[0x280..0x28a].copy_from_slice(b"ADL2_Test\0");
        b
    }
    #[test]
    fn reads_export_directory_and_rejects_bad_rvas_and_truncated_files() {
        let good = fixture();
        assert_eq!(exports(&good).unwrap(), ["ADL2_Test"]);
        for length in [0, 2, 0x40, 0x99, 0x200, 0x245, 0x285] {
            assert!(exports(&good[..length]).is_err());
        }
        let mut bad = good;
        bad[0x240..0x244].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(exports(&bad).is_err());
    }
    #[test]
    #[cfg(windows)]
    fn reads_actual_windows_exports_including_forwarded_names() {
        let path = std::path::PathBuf::from(std::env::var_os("WINDIR").unwrap()).join("System32/kernel32.dll");
        let names = exports(&std::fs::read(path).unwrap()).unwrap();
        assert!(names.iter().any(|s| s == "GetCurrentProcess"));
        assert!(names.iter().any(|s| s == "LoadLibraryW"));
    }
}
