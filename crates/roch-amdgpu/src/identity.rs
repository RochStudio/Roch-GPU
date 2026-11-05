//! What a card is called. The kernel only knows numbers; the names live in two tables most
//! distributions install: libdrm's `amdgpu.ids`, which tells a 7900 XT from a 7900 XTX by revision,
//! and `pci.ids`, which knows every card but only by family.

use std::fs;
use std::path::Path;

const AMDGPU_IDS: &str = "/usr/share/libdrm/amdgpu.ids";
const PCI_IDS: [&str; 3] = ["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids", "/usr/share/pci.ids"];

/// `744C,\tCC,\tAMD Radeon RX 7900 XT` → the product name for this device and revision.
pub fn product_from_amdgpu_ids(table: &str, device: u32, revision: u32) -> Option<String> {
    table.lines().find_map(|line| {
        let mut fields = line.splitn(3, ',').map(str::trim);
        let (d, r, name) = (fields.next()?, fields.next()?, fields.next()?);
        let matches = u32::from_str_radix(d, 16).ok()? == device && u32::from_str_radix(r, 16).ok()? == revision;
        (matches && !name.is_empty()).then(|| name.to_string())
    })
}

/// The device's line in `pci.ids`: tab-indented under its vendor, `\t744c  Navi 31 [Radeon RX ...]`.
pub fn device_from_pci_ids(table: &str, vendor: u32, device: u32) -> Option<String> {
    let mut in_vendor = false;
    for line in table.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if !line.starts_with('\t') {
            let id = line.split_whitespace().next().and_then(|t| u32::from_str_radix(t, 16).ok());
            // A second top-level entry past the vendor ends the search.
            if in_vendor {
                return None;
            }
            in_vendor = id == Some(vendor);
        } else if in_vendor && !line.starts_with("\t\t") {
            let body = line.trim_start();
            let (id, name) = body.split_once(char::is_whitespace)?;
            if u32::from_str_radix(id, 16).ok() == Some(device) {
                return Some(name.trim().to_string());
            }
        }
    }
    None
}

/// `Navi 31 [Radeon RX 7900 XT/7900 XTX]` → (`Navi 31`, `Radeon RX 7900 XT/7900 XTX`).
pub fn split_pci_name(name: &str) -> (Option<&str>, &str) {
    match name.split_once('[') {
        Some((code, rest)) => {
            let code = code.trim();
            ((!code.is_empty()).then_some(code), rest.trim_end().trim_end_matches(']'))
        }
        None => (None, name.trim()),
    }
}

fn read_pci_ids() -> Option<String> {
    PCI_IDS.iter().find_map(|p| fs::read_to_string(Path::new(p)).ok())
}

/// The name to show for this card. Family names are what `pci.ids` has, so they are only used when
/// the exact table does not list the card; failing both it is named by its number.
pub fn product_name(device: u32, revision: u32) -> String {
    fs::read_to_string(AMDGPU_IDS)
        .ok()
        .and_then(|t| product_from_amdgpu_ids(&t, device, revision))
        .or_else(|| {
            let line = device_from_pci_ids(&read_pci_ids()?, 0x1002, device)?;
            Some(format!("AMD {}", split_pci_name(&line).1))
        })
        .unwrap_or_else(|| format!("AMD GPU {device:04X}"))
}

/// Where a distribution may have installed `pci.ids`.
pub fn pci_ids_paths() -> Vec<std::path::PathBuf> {
    PCI_IDS.iter().map(std::path::PathBuf::from).collect()
}

/// The silicon's code name, such as `Navi 31`, when this copy of `pci.ids` gives one.
pub fn code_name_in(table: &str, device: u32) -> String {
    device_from_pci_ids(table, 0x1002, device)
        .and_then(|line| split_pci_name(&line).0.map(str::to_string))
        .unwrap_or_default()
}

/// The memory a family of silicon uses. The driver only logs it at boot, for root to read, but it does
/// not vary inside these families. Navi 12 is the odd one out, with HBM2. Anything else is left blank
/// rather than guessed.
pub fn memory_type_of(code_name: &str) -> &'static str {
    if code_name.starts_with("Navi ") && !code_name.starts_with("Navi 12") { "GDDR6" } else { "" }
}

/// How many times the clock people quote is the memory controller's own, which is what the driver
/// counts in. From the kernel's documentation of `pp_od_clk_voltage`: twice on GDDR6, once on HBM and
/// GDDR5. The Windows driver reports the former, so with this a 2500 MHz memory clock is the same number
/// on both systems.
pub fn memory_clock_scale(code_name: &str) -> i32 {
    if memory_type_of(code_name) == "GDDR6" { 2 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDS: &str = "# List of AMDGPU IDs\n#\n# Syntax:\n# device_id,\trevision_id,\tproduct_name\n1.0.0\n\
                       744C,\tC8,\tAMD Radeon RX 7900 XTX\n744C,\tCC,\tAMD Radeon RX 7900 XT\n";

    #[test]
    fn memory_type_is_only_named_where_it_is_certain() {
        assert_eq!(memory_type_of("Navi 31"), "GDDR6");
        assert_eq!(memory_type_of("Navi 21 GL-XL"), "GDDR6");
        assert_eq!(memory_type_of("Navi 12"), "", "HBM2");
        assert_eq!(memory_type_of("Vega 20"), "");
        assert_eq!(memory_type_of(""), "");
    }

    #[test]
    fn memory_clocks_are_scaled_only_on_gddr6() {
        assert_eq!(memory_clock_scale("Navi 31"), 2);
        assert_eq!(memory_clock_scale("Navi 48"), 2);
        assert_eq!(memory_clock_scale("Navi 12"), 1);
        assert_eq!(memory_clock_scale("Vega 20"), 1);
        assert_eq!(memory_clock_scale(""), 1, "an unknown card keeps the driver's own number");
    }

    #[test]
    fn the_revision_tells_siblings_apart() {
        assert_eq!(product_from_amdgpu_ids(IDS, 0x744C, 0xCC).as_deref(), Some("AMD Radeon RX 7900 XT"));
        assert_eq!(product_from_amdgpu_ids(IDS, 0x744C, 0xC8).as_deref(), Some("AMD Radeon RX 7900 XTX"));
        assert_eq!(product_from_amdgpu_ids(IDS, 0x744C, 0x00), None);
        assert_eq!(product_from_amdgpu_ids(IDS, 0x1234, 0xCC), None);
    }

    const PCI: &str = "# comment\n\n1000  Broadcom\n\t744c  Not this one\n\
                       1002  Advanced Micro Devices, Inc. [AMD/ATI]\n\t743f  Navi 24 [Radeon RX 6400]\n\
                       \t\t1002 744c  A subsystem line, not a device\n\
                       \t744c  Navi 31 [Radeon RX 7900 XT/7900 XTX/7900 GRE/7900M]\n\
                       \t\t1da2 471e  Something\n\t7c00  Plain Name\n1003  ULSI Systems\n\t744d  Beyond the vendor\n";

    #[test]
    fn pci_ids_finds_the_device_under_its_vendor() {
        let line = device_from_pci_ids(PCI, 0x1002, 0x744c).unwrap();
        assert_eq!(line, "Navi 31 [Radeon RX 7900 XT/7900 XTX/7900 GRE/7900M]");
        assert_eq!(split_pci_name(&line), (Some("Navi 31"), "Radeon RX 7900 XT/7900 XTX/7900 GRE/7900M"));
        assert_eq!(device_from_pci_ids(PCI, 0x1002, 0x744d), None, "the next vendor's device");
        assert_eq!(split_pci_name("Plain Name"), (None, "Plain Name"));
    }
}
