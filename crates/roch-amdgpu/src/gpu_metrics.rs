//! `gpu_metrics`: the binary sample the power management firmware publishes, holding the sensors
//! hwmon does not carry (VRM temperatures, SoC and memory voltages, video clocks, throttling).
//!
//! The layout is `struct gpu_metrics_v1_3` in the kernel's `kgd_pp_interface.h`, used by discrete
//! cards from Navi 1x to Navi 4x. Any other format is left unread: every field here is a nice-to-have
//! beside hwmon, and misreading a layout would be worse than a blank row.

/// A field the firmware does not fill is all ones.
const UNSET: u16 = u16::MAX;

/// The fields used, `None` where the firmware left one unset. Temperatures in °C, voltages in mV,
/// clocks in MHz.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GpuMetrics {
    pub vr_gfx_c: Option<u16>,
    pub vr_soc_c: Option<u16>,
    pub vr_mem_c: Option<u16>,
    pub soc_clock_mhz: Option<u16>,
    pub vclk0_mhz: Option<u16>,
    pub dclk0_mhz: Option<u16>,
    pub vclk1_mhz: Option<u16>,
    pub dclk1_mhz: Option<u16>,
    pub pcie_link_width: Option<u16>,
    /// In 0.1 GT/s.
    pub pcie_link_speed: Option<u16>,
    pub voltage_soc_mv: Option<u16>,
    pub voltage_mem_mv: Option<u16>,
    pub throttle: Option<u64>,
}

impl GpuMetrics {
    /// `None` for any format other than v1.3, or a sample shorter than one.
    pub fn parse(bytes: &[u8]) -> Option<GpuMetrics> {
        const SIZE: usize = 120;
        if bytes.len() < SIZE || bytes[2] != 1 || bytes[3] != 3 {
            return None;
        }
        let u16_at = |offset: usize| {
            let value = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
            (value != UNSET).then_some(value)
        };
        let raw_throttle = u64::from_le_bytes(bytes[112..120].try_into().ok()?);
        Some(GpuMetrics {
            vr_gfx_c: u16_at(10),
            vr_soc_c: u16_at(12),
            vr_mem_c: u16_at(14),
            soc_clock_mhz: u16_at(56),
            vclk0_mhz: u16_at(60),
            dclk0_mhz: u16_at(62),
            vclk1_mhz: u16_at(64),
            dclk1_mhz: u16_at(66),
            pcie_link_width: u16_at(74),
            pcie_link_speed: u16_at(76),
            voltage_soc_mv: u16_at(104),
            voltage_mem_mv: u16_at(108),
            throttle: (raw_throttle != u64::MAX).then_some(raw_throttle),
        })
    }

    /// The PCIe generation the link speed is, for the same words the Windows build uses.
    pub fn pcie_generation(&self) -> Option<i32> {
        generation_of_tenths(self.pcie_link_speed?)
    }
}

/// 0.1 GT/s → PCIe generation.
fn generation_of_tenths(tenths: u16) -> Option<i32> {
    match tenths {
        25 => Some(1),
        50 => Some(2),
        80 => Some(3),
        160 => Some(4),
        320 => Some(5),
        640 => Some(6),
        _ => None,
    }
}

/// `16.0 GT/s PCIe` → 4.
pub fn generation_of_link_speed(text: &str) -> Option<i32> {
    let gts: f64 = text.split_whitespace().next()?.parse().ok()?;
    generation_of_tenths((gts * 10.0).round() as u16)
}

const POWER_BITS: u64 = 0xFF;
const CURRENT_BITS: u64 = 0xFF << 16;
const THERMAL_BITS: u64 = 0xFFFF << 32;

/// Take out the thermal and power bits unless the card really is near that limit. The driver raises a
/// bit whenever the firmware's throttling percentage for it is above zero, and that is not the same as
/// the card being held back. An RX 7900 XT reports hot spot throttling at idle, at 48 °C against a
/// limit of 110 °C, and power throttling in the burst of activity after boot, at 40 % of its power cap.
/// Showing "Thermal" beside a cold card only sends people looking for a problem they do not have.
pub fn plausible_limits(status: u64, near_temperature_limit: bool, near_power_limit: bool) -> u64 {
    let mut kept = status;
    if !near_temperature_limit {
        kept &= !THERMAL_BITS;
    }
    if !near_power_limit {
        kept &= !POWER_BITS;
    }
    kept
}

/// `indep_throttle_status`, in the same words as the Windows build: the ASIC-independent bits from
/// the kernel's `amdgpu_smu.h`. Power is bits 0-7, current 16-23, temperature 32-47; the rest is
/// reported as "Other" rather than left out.
pub fn throttle_text(status: u64) -> String {
    if status == 0 {
        return "None".into();
    }
    let mut hits = Vec::new();
    if status & POWER_BITS != 0 {
        hits.push("Power");
    }
    if status & THERMAL_BITS != 0 {
        hits.push("Thermal");
    }
    if status & CURRENT_BITS != 0 {
        hits.push("Current");
    }
    if hits.is_empty() { "Other".into() } else { hits.join(" + ") }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read from a real RX 7900 XT at idle.
    const IDLE_7900XT: [u8; 120] = [
        0x78, 0x00, 0x01, 0x03, 0x2b, 0x00, 0x32, 0x00, 0x34, 0x00, 0x2f, 0x00, 0x2d, 0x00, 0x2d, 0x00, // 0
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0f, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // 16
        0xf1, 0x7e, 0x24, 0xbe, 0xa9, 0x01, 0x00, 0x00, 0x1a, 0x00, 0xff, 0xff, 0x42, 0x00, 0x19, 0x00, // 32
        0x19, 0x00, 0x19, 0x00, 0x19, 0x00, 0x1a, 0x00, 0x58, 0x02, 0x60, 0x00, 0x19, 0x00, 0x19, 0x00, // 48
        0x19, 0x00, 0x19, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0xa0, 0x00, 0xff, 0xff, // 64
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // 80
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xbe, 0x02, 0xac, 0x02, 0xa6, 0x02, 0xff, 0xff, // 96
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // 112
    ];

    #[test]
    fn a_real_sample_decodes() {
        let m = GpuMetrics::parse(&IDLE_7900XT).unwrap();
        assert_eq!((m.vr_gfx_c, m.vr_soc_c, m.vr_mem_c), (Some(47), Some(45), Some(45)));
        assert_eq!(m.soc_clock_mhz, Some(600));
        assert_eq!((m.vclk0_mhz, m.dclk0_mhz, m.vclk1_mhz, m.dclk1_mhz), (Some(25), Some(25), Some(25), Some(25)));
        assert_eq!((m.pcie_link_width, m.pcie_link_speed), (Some(8), Some(160)));
        assert_eq!(m.pcie_generation(), Some(4));
        assert_eq!((m.voltage_soc_mv, m.voltage_mem_mv), (Some(702), Some(678)));
        assert_eq!(m.throttle, Some(0));
    }

    #[test]
    fn other_formats_and_short_reads_are_left_alone() {
        let mut v14 = IDLE_7900XT;
        v14[3] = 4;
        assert_eq!(GpuMetrics::parse(&v14), None);
        let mut v20 = IDLE_7900XT;
        v20[2] = 2;
        assert_eq!(GpuMetrics::parse(&v20), None);
        assert_eq!(GpuMetrics::parse(&IDLE_7900XT[..100]), None);
        assert_eq!(GpuMetrics::parse(&[]), None);
    }

    #[test]
    fn all_ones_is_no_reading() {
        let mut sample = IDLE_7900XT;
        sample[10..12].fill(0xff);
        sample[112..120].fill(0xff);
        let m = GpuMetrics::parse(&sample).unwrap();
        assert_eq!(m.vr_gfx_c, None);
        assert_eq!(m.throttle, None);
    }

    #[test]
    fn throttling_reads_in_plain_words() {
        assert_eq!(throttle_text(0), "None");
        assert_eq!(throttle_text(1 << 0), "Power");
        assert_eq!(throttle_text(1 << 36), "Thermal");
        assert_eq!(throttle_text(1 << 22), "Current");
        assert_eq!(throttle_text(1 | (1 << 36)), "Power + Thermal");
        assert_eq!(throttle_text(1 << 56), "Other");
    }

    #[test]
    fn a_limit_is_only_believed_when_the_card_is_near_it() {
        // What a real RX 7900 XT reports at idle: the hot spot bit, at 48 °C.
        let idle_hot_spot = 1u64 << 36;
        assert_eq!(throttle_text(plausible_limits(idle_hot_spot, false, false)), "None");
        assert_eq!(throttle_text(plausible_limits(idle_hot_spot, true, false)), "Thermal");
        // And after boot: the power bit, at 40 % of its cap.
        assert_eq!(throttle_text(plausible_limits(1, false, false)), "None");
        assert_eq!(throttle_text(plausible_limits(1, false, true)), "Power");
        assert_eq!(throttle_text(plausible_limits(1 | idle_hot_spot, true, true)), "Power + Thermal");
        // Nothing else is second-guessed.
        assert_eq!(throttle_text(plausible_limits(1 << 22, false, false)), "Current");
        assert_eq!(throttle_text(plausible_limits(1 << 56, false, false)), "Other");
    }

    #[test]
    fn link_speeds_name_their_generation() {
        assert_eq!(generation_of_link_speed("16.0 GT/s PCIe"), Some(4));
        assert_eq!(generation_of_link_speed("2.5 GT/s PCIe"), Some(1));
        assert_eq!(generation_of_link_speed("Unknown"), None);
    }
}
