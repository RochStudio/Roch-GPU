//! Ported from `tests/GpuTuner.Core.Tests/Program.cs` ("Intel native data and write guards",
//! "Intel identity ABI", clock-range request), plus layout checks for the buffers the C# pinned.

use roch_core::{FanCurve, FanPoint, GpuBackend, GpuCapabilities, GpuDevice, TuningProfile, VoltageControlStyle};

use crate::backend::{
    Control, build_fan_table, clock_range_request, decode_graphics, decode_pci, option_rom_version, rate,
};
use crate::igcl::{self, Buf, Point, sensor};

fn curve(points: &[(f64, f64)]) -> FanCurve {
    FanCurve::with_points(points.iter().map(|&(t, p)| FanPoint::new(t, p)).collect())
}

fn intel_device() -> GpuDevice {
    GpuDevice {
        index: 0,
        name: "Intel B60".into(),
        vendor: "Intel".into(),
        bus_id: "0000:03:00.0".into(),
        driver_version: String::new(),
        vram_megabytes: 24576,
        bios_version: "23.1066.00.00".into(),
    }
}

#[test]
fn graphics_decodes_b60_identity() {
    let mut gpu_props = Buf::data(320, 3);
    gpu_props.put(68, 0xE211);
    gpu_props.put_u16(198, 0x1849);
    gpu_props.put(204, 20);
    let mut memory_props = Buf::data(32, 0);
    memory_props.put(8, 12);
    memory_props.put(24, 192);
    let device = intel_device();
    let g = decode_graphics(&device, gpu_props.bytes(), &[memory_props.bytes()]);
    assert_eq!(g.board_manufacturer, "ASRock", "Intel graphics decodes subsystem vendor");
    assert_eq!(g.cores, "20 Xe cores", "Intel graphics decodes native Xe core count");
    assert_eq!(g.revision, "00", "Intel graphics retains revision zero");
    assert!(g.memory_type == "GDDR6" && g.bus_width == "192 bit", "Intel graphics decodes GDDR6 and memory width");
    assert!(
        g.code_name == "Battlemage BMG-G21 WKSTN" && g.technology == "5 nm (TSMC N5)",
        "Intel graphics identifies B60"
    );
    assert!(
        g.memory_vendor.is_empty() && g.rops_tmus.is_empty(),
        "Intel graphics does not invent memory vendor or ROPs"
    );
    assert_eq!(g.resizable_bar, "Unavailable");
    assert_eq!(g.gpu, "Intel B60");

    gpu_props.put(68, 0xFFFF);
    gpu_props.put(204, 0);
    memory_props.put(8, 15);
    memory_props.put(24, -1);
    let g = decode_graphics(&device, gpu_props.bytes(), &[memory_props.bytes()]);
    assert!(
        g.code_name.is_empty() && g.technology.is_empty() && g.cores.is_empty(),
        "Intel unknown chip keeps unavailable identity blank"
    );
    assert!(
        g.memory_type.is_empty() && g.bus_width.is_empty(),
        "Intel unknown memory keeps unavailable properties blank"
    );
}

#[test]
fn graphics_never_invents_aggregate_width_and_dedups_types() {
    let mut a = Buf::data(32, 0);
    a.put(8, 12);
    a.put(24, 96);
    let b = a.clone();
    let g = decode_graphics(&intel_device(), Buf::data(320, 3).bytes(), &[a.bytes(), b.bytes()]);
    assert_eq!(g.memory_type, "GDDR6");
    assert_eq!(g.bus_width, "");
}

#[test]
fn sensor_abi() {
    let mut s = Buf::zeroed(24);
    s.put_u8(0, 1);
    s.put(8, 9);
    s.put_f64(16, 12.5);
    assert_eq!(sensor(s.bytes(), 0), 12.5, "Intel double telemetry ABI");
    s.put_u8(0, 0);
    assert!(sensor(s.bytes(), 0).is_nan(), "Intel missing sensor is unavailable");
    s.put_u8(0, 1);
    s.put(8, 99);
    assert!(sensor(s.bytes(), 0).is_nan(), "Intel unknown sensor type is unavailable");
    assert!(sensor(Buf::zeroed(8).bytes(), 0).is_nan(), "Intel truncated sensor is unavailable");
    // Signed byte and float decode as the C# did.
    s.put(8, 0);
    s.put_u8(16, 0xFF);
    assert_eq!(sensor(s.bytes(), 0), -1.0);
    s.put(8, 8);
    s.put_slice(16, &1.5f32.to_le_bytes());
    assert_eq!(sensor(s.bytes(), 0), 1.5);
    s.put(8, 9);
    s.put_f64(16, f64::INFINITY);
    assert!(sensor(s.bytes(), 0).is_nan(), "non-finite values are unavailable");
    assert!(sensor(s.bytes(), usize::MAX - 4).is_nan(), "offset overflow is unavailable, not a panic");
}

#[test]
fn energy_rates() {
    assert_eq!(rate(120.0, 100.0, 0.5), 40.0, "Intel energy delta computes watts");
    assert!(rate(10.0, f64::NAN, 1.0).is_nan(), "Intel first sample has no fabricated power");
    assert!(rate(5.0, 10.0, 1.0).is_nan(), "Intel counter reset is unavailable");
    assert!(rate(10.0, 5.0, 0.0).is_nan(), "Intel zero elapsed time is unavailable");
}

#[test]
fn control_validation() {
    let control = Control { supported: true, units: 12, min: 19.0, max: 22.0, step: 0.001, default: 19.0 };
    assert_eq!(control.validate(19.001), Ok(19.001), "Intel fractional memory speed");
    assert!(control.validate(18.9).is_err(), "Intel below range");
    assert!(control.validate(22.1).is_err(), "Intel above range");
    assert!(control.validate(f64::NAN).is_err(), "Intel NaN request");
    assert!(control.validate(19.0005).is_err(), "Intel fractional off-grid request");
    assert!(Control { supported: false, ..control }.validate(19.0).is_err(), "Intel unsupported control");
}

#[test]
fn control_read_layout_and_malformed_limits() {
    let mut properties = Buf::data(440, 1);
    properties.put_u8(8, 1);
    properties.put_f64(16, f64::NAN);
    assert!(!Control::read(properties.bytes(), 0).supported, "Intel malformed limits disabled");

    // Slot 6 (VRAM speed limit): 8 + 6 * 48.
    let at = 8 + 6 * 48;
    properties.put_u8(at, 1);
    properties.put(at + 4, 12);
    properties.put_f64(at + 8, 19.0);
    properties.put_f64(at + 16, 22.0);
    properties.put_f64(at + 24, 0.001);
    properties.put_f64(at + 32, 19.0);
    let c = Control::read(properties.bytes(), 6);
    assert_eq!(c, Control { supported: true, units: 12, min: 19.0, max: 22.0, step: 0.001, default: 19.0 });
    // Zero step and inverted limits are malformed too.
    properties.put_f64(at + 24, 0.0);
    assert!(!Control::read(properties.bytes(), 6).supported);
    properties.put_f64(at + 24, 0.001);
    properties.put_f64(at + 8, 23.0);
    assert!(!Control::read(properties.bytes(), 6).supported);
    // The last slot ends exactly at the structure size.
    assert_eq!(8 + 9 * 48, properties.bytes().len());
}

#[test]
fn fan_table_abi_and_guards() {
    let good = curve(&[(25.0, 30.0), (70.0, 50.0), (100.0, 100.0)]);
    let native = build_fan_table(&good, 10).expect("valid curve");
    assert!(native.bytes().len() == 908 && native.int(0) == 908 && native.int(8) == 3, "Intel fan ABI size and count");
    assert!(native.int(20) == 25 && native.int(32) == 30 && native.int(36) == 1, "Intel fan percent encoding");
    assert!(native.int(12) == 0 && native.int(24) == 0, "Intel nested fan headers match driver");
    assert!(build_fan_table(&curve(&[(25.0, 31.0), (35.0, 30.0)]), 10).is_err(), "Intel descending fan duty blocked");
    assert!(
        build_fan_table(&curve(&[(25.0, 30.0), (25.1, 40.0)]), 10).is_err(),
        "Intel duplicate temperatures blocked"
    );
    assert!(
        build_fan_table(&curve(&[(24.0, 30.0), (35.0, 40.0)]), 10).is_err(),
        "Intel invalid fan temperature blocked"
    );
    assert!(
        build_fan_table(&curve(&[(25.0, f64::NAN), (35.0, 40.0)]), 10).is_err(),
        "Intel nonfinite fan value blocked"
    );
    assert!(build_fan_table(&good, 2).is_err(), "Intel excess fan points blocked");
    assert!(build_fan_table(&curve(&[(50.0, 50.0)]), 10).is_err(), "a single point is not a curve");
}

#[test]
fn fan_table_sorts_by_temperature() {
    let t = build_fan_table(&curve(&[(100.0, 100.0), (25.0, 30.0)]), 10).expect("valid curve");
    assert_eq!((t.int(20), t.int(32)), (25, 30));
    assert_eq!((t.int(20 + 28), t.int(32 + 28)), (100, 100));
    assert_eq!(t.int(36 + 28), 1);
}

#[test]
fn intel_capability_shape_in_profiles() {
    let caps = GpuCapabilities {
        voltage_style: VoltageControlStyle::Percent,
        can_set_voltage_boost: true,
        can_edit_vf_curve: true,
        voltage_boost_default_percent: 100,
        can_set_core_offset: true,
        core_offset_min_mhz: -300,
        core_offset_max_mhz: 1000,
        core_offset_step_mhz: 1,
        can_set_memory_offset: true,
        memory_clock_is_absolute: true,
        memory_clock_unit: "Mbps".into(),
        memory_clock_default_mhz: 19000,
        memory_offset_min_mhz: 19000,
        memory_offset_max_mhz: 22000,
        temp_limit_is_percent: true,
        can_set_fan_speed: true,
        fan_curve_is_hardware: true,
        fan_curve_points: 10,
        ..GpuCapabilities::default()
    };
    assert_eq!(TuningProfile::stock(&caps, None).memory_offset_mhz, 19000, "Intel stock memory is 19 Gbps");
    assert_eq!(TuningProfile::stock(&caps, None).voltage_boost_percent, 100, "Intel stock voltage uses driver default");
    assert_eq!(
        TuningProfile::stock(&GpuCapabilities::default(), None).voltage_boost_percent,
        0,
        "NVIDIA stock voltage boost remains zero"
    );
    assert!(!caps.can_set_voltage() && caps.temp_limit_unit() == "%", "Intel percent has no fabricated mV cap");
}

#[test]
fn pci_identity_abi() {
    let mut pci = Buf::zeroed(64);
    pci.put(20, 3);
    pci.put(40, 5);
    pci.put(44, 8);
    pci.put_u8(56, 1);
    let (interface, address, rebar) = decode_pci(pci.bytes());
    assert!(interface == "5.0 x8" && address == "0000:03:00.0", "Intel PCIe maximum decoded");
    assert_eq!(rebar, "Disabled", "Intel supported but disabled ReBAR");
    pci.put_u8(57, 1);
    assert_eq!(decode_pci(pci.bytes()).2, "Enabled", "Intel enabled ReBAR");
    pci.put_u8(56, 0);
    assert_eq!(decode_pci(pci.bytes()).2, "Not supported", "Intel unsupported ReBAR");
}

#[test]
fn firmware_option_rom_version() {
    let mut fw = Buf::zeroed(156);
    fw.put_slice(5, b"OptionRomCode");
    fw.put_slice(69, b"23.1066.00.00");
    assert_eq!(option_rom_version(fw.bytes()), "23.1066.00.00", "Intel VBIOS code version");
    fw.put_slice(5, b"OptionRomData");
    assert_eq!(option_rom_version(fw.bytes()), "", "Intel ROM data is not VBIOS");
    fw.put_slice(5, b"optionromcode");
    fw.put_slice(69, b"unknown\0\0\0\0\0\0");
    assert_eq!(option_rom_version(fw.bytes()), "", "an unknown version is not a version");
    assert_eq!(option_rom_version(&[0u8; 10]), "", "a short buffer never panics");
}

#[test]
fn clock_range_requests() {
    let range = clock_range_request(500, 2300, 400, 3000).expect("in range");
    assert!(
        range.bytes().len() == 24 && range.int(0) == 24 && range.f64(8) == 500.0 && range.f64(16) == 2300.0,
        "Intel native range buffer"
    );
    let reset = clock_range_request(0, 0, 400, 3000).expect("reset");
    assert!(reset.f64(8) == -1.0 && reset.f64(16) == -1.0, "Intel range reset uses factory sentinel");
    let partial = clock_range_request(0, 2200, 400, 3000).expect("partial");
    assert_eq!(partial.f64(8), 400.0, "Intel omitted floor uses supported minimum");
    assert_eq!(partial.f64(16), 2200.0);
    assert!(clock_range_request(2500, 2400, 400, 3000).is_err(), "Intel reversed range rejected");
    assert!(
        clock_range_request(399, 2400, 400, 3000).is_err() && clock_range_request(400, 3001, 400, 3000).is_err(),
        "Intel out-of-range limits rejected"
    );
}

#[test]
fn buffers_have_headers_and_alignment() {
    let b = Buf::data(1016, 1);
    assert_eq!(b.int(0), 1016);
    assert_eq!(b.byte(4), 1);
    assert_eq!(b.bytes().as_ptr() as usize % 8, 0, "doubles inside IGCL structs stay aligned");
    // Reads past the end are inert, never a panic.
    assert_eq!(b.int(1014), 0);
    assert!(b.f64(1012).is_nan());
    assert_eq!(size_of::<Point>(), 8);
    assert_eq!(igcl::FAN_CONFIG_SIZE, 936);
    assert_eq!(igcl::FAN_TABLE_SIZE, 908);
}

#[test]
fn uninitialized_backend_errors_instead_of_panicking() {
    let mut b = crate::IntelBackend::new();
    assert_eq!(b.backend_name(), "Intel IGCL");
    assert!(b.devices().is_empty());
    assert!(b.capabilities(0).is_err());
    assert!(b.read_telemetry(0).is_err());
    assert!(b.read_tuning_state(0).is_err());
    assert!(b.set_core_offset(0, 0).is_err());
    assert!(b.set_clock_range(0, 0, 0).is_err());
    assert!(b.set_voltage_curve_offset(0, -50, 0).is_err());
    assert!(b.set_fan_speed(0, 1, 50).is_err(), "per-fan duty is refused");
    assert_eq!(b.read_voltage_lock_mv(0), 0);
    assert_eq!(b.read_graphics_info(3), Default::default());
}
