//! The backend against a fake `/sys`: a real directory tree for reads, and a simulated kernel behind
//! the writes, because a plain file cannot refuse a value or hold an edit back until a commit.

use std::collections::BTreeMap;
use std::io;
use std::os::unix::fs::symlink;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

const OD_RDNA3: &str = "OD_SCLK:\n0: 500Mhz\n1: 2525Mhz\nOD_MCLK:\n0: 97Mhz\n1: 1250MHz\nOD_VDDGFX_OFFSET:\n0mV\n\
                        OD_RANGE:\nSCLK:     500Mhz       3000Mhz\nMCLK:      97Mhz       1500Mhz\n\
                        VDDGFX_OFFSET:    -200mv          0mv\n";
const OD_RDNA4: &str = "OD_SCLK_OFFSET:\n0Mhz\nOD_MCLK:\n0: 97Mhz\n1: 1218MHz\nOD_VDDGFX_OFFSET:\n0mV\n\
                        OD_RANGE:\nSCLK_OFFSET:    -500Mhz       1000Mhz\nMCLK:      97Mhz       1500Mhz\n\
                        VDDGFX_OFFSET:    -200mv          0mv\n";

const FAN_FRESH: &str = "OD_FAN_CURVE:\n0: 0C 0%\n1: 0C 0%\n2: 0C 0%\n3: 0C 0%\n4: 0C 0%\nOD_RANGE:\n\
                         FAN_CURVE(hotspot temp): 25C 100C\nFAN_CURVE(fan speed): 23% 100%\n";
const ZERO_ON: &str = "FAN_ZERO_RPM_ENABLE:\n1\nOD_RANGE:\nZERO_RPM_ENABLE: 0 1\n";

/// Real numbers from an RX 7900 XT at idle.
const METRICS: [u8; 120] = [
    0x78, 0x00, 0x01, 0x03, 0x2b, 0x00, 0x32, 0x00, 0x34, 0x00, 0x2f, 0x00, 0x2d, 0x00, 0x2d, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x0f, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xf1, 0x7e, 0x24, 0xbe, 0xa9, 0x01,
    0x00, 0x00, 0x1a, 0x00, 0xff, 0xff, 0x42, 0x00, 0x19, 0x00, 0x19, 0x00, 0x19, 0x00, 0x19, 0x00, 0x1a, 0x00, 0x58,
    0x02, 0x60, 0x00, 0x19, 0x00, 0x19, 0x00, 0x19, 0x00, 0x19, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00,
    0xa0, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xbe, 0x02, 0xac, 0x02, 0xa6, 0x02, 0xff, 0xff, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// A directory the test owns, gone when it ends.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Tree {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("roch-amdgpu-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("class/drm")).unwrap();
        fs::create_dir_all(dir.join("module")).unwrap();
        Tree(dir)
    }

    fn root(&self) -> PathBuf {
        self.0.join("class/drm")
    }

    fn module(&self) -> PathBuf {
        self.0.join("module")
    }

    /// A card directory with the attributes every amdgpu card has, at `pci`, driven by `driver`.
    fn card(&self, name: &str, pci: &str, driver: &str) -> PathBuf {
        let device = self.0.join("devices").join(pci);
        let drm = self.0.join("devices").join(pci).join("drm").join(name);
        fs::create_dir_all(&drm).unwrap();
        fs::create_dir_all(device.join("power")).unwrap();
        fs::create_dir_all(device.join("hwmon/hwmon3")).unwrap();
        fs::create_dir_all(self.0.join("drivers").join(driver)).unwrap();
        symlink(&device, drm.join("device")).unwrap();
        symlink(self.0.join("drivers").join(driver), device.join("driver")).unwrap();
        symlink(&drm, self.root().join(name)).unwrap();
        for (file, text) in [
            ("vendor", "0x1002\n"),
            ("device", "0x744c\n"),
            ("revision", "0xcc\n"),
            ("subsystem_vendor", "0x1da2\n"),
            ("boot_vga", "0\n"),
            ("power/runtime_status", "active\n"),
            ("power_state", "D0\n"),
            ("vbios_version", "113-D70401XT-P11\n"),
            ("mem_info_vram_total", "21458059264\n"),
        ] {
            fs::write(device.join(file), text).unwrap();
        }
        device
    }

    /// The sensors an awake RDNA 3 card shows.
    fn awake_sensors(&self, device: &Path) {
        for (file, text) in [
            ("gpu_busy_percent", "2\n"),
            ("mem_busy_percent", "1\n"),
            ("mem_info_vram_used", "613990400\n"),
            ("mem_info_gtt_used", "104857600\n"),
            ("mem_info_vram_vendor", "samsung\n"),
            ("max_link_speed", "16.0 GT/s PCIe\n"),
            ("max_link_width", "16\n"),
            ("power_dpm_force_performance_level", "auto\n"),
            ("pp_dpm_fclk", "0: 601Mhz \n1: 1000Mhz *\n2: 1200Mhz \n"),
            ("pp_dpm_socclk", "0: 500Mhz \n1: 600Mhz *\n"),
            ("hwmon/hwmon3/temp1_label", "edge\n"),
            ("hwmon/hwmon3/temp1_input", "43000\n"),
            ("hwmon/hwmon3/temp2_label", "junction\n"),
            ("hwmon/hwmon3/temp2_input", "50000\n"),
            ("hwmon/hwmon3/temp3_label", "mem\n"),
            ("hwmon/hwmon3/temp3_input", "52000\n"),
            ("hwmon/hwmon3/freq1_input", "26000000\n"),
            ("hwmon/hwmon3/freq2_input", "96000000\n"),
            ("hwmon/hwmon3/in0_input", "682\n"),
            ("hwmon/hwmon3/power1_average", "14000000\n"),
            ("hwmon/hwmon3/power1_cap", "265000000\n"),
            ("hwmon/hwmon3/power1_cap_default", "265000000\n"),
            ("hwmon/hwmon3/power1_cap_min", "238000000\n"),
            ("hwmon/hwmon3/power1_cap_max", "290000000\n"),
            ("hwmon/hwmon3/fan1_input", "1200\n"),
            ("hwmon/hwmon3/pwm1", "128\n"),
            ("hwmon/hwmon3/pwm1_enable", "2\n"),
        ] {
            fs::write(device.join(file), text).unwrap();
        }
        fs::write(device.join("gpu_metrics"), METRICS).unwrap();
    }

    fn overdrive(&self, device: &Path, od: &str) {
        fs::write(device.join(OD_FILE), od).unwrap();
    }

    fn firmware_fans(&self, device: &Path) {
        fs::create_dir_all(device.join("gpu_od/fan_ctrl")).unwrap();
        fs::write(device.join(FAN_CURVE_FILE), FAN_FRESH).unwrap();
        fs::write(device.join(ZERO_RPM_FILE), ZERO_ON).unwrap();
    }

    fn graphics_core(&self, device: &Path, major: u32) {
        fs::create_dir_all(device.join("ip_discovery/die/0/GC/0")).unwrap();
        fs::write(device.join("ip_discovery/die/0/GC/0/major"), format!("{major}\n")).unwrap();
    }

    fn feature_mask(&self, mask: u64) {
        fs::write(self.module().join("ppfeaturemask"), format!("0x{mask:08x}\n")).unwrap();
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// What the simulated kernel holds, shared with the test so it can look afterwards.
#[derive(Default)]
struct Sim {
    /// Every write, as "file value" in order.
    log: Vec<String>,
    /// Edits waiting for a commit, per file.
    pending: BTreeMap<PathBuf, Vec<String>>,
    /// The next writes fail with this OS error.
    refuse: Option<i32>,
}

/// Applies the amdgpu rules: an overdrive edit is range-checked now and reaches the file only on a
/// commit; a restore commits itself; a fan duty needs manual mode first.
#[derive(Clone, Default)]
struct KernelSim(Arc<Mutex<Sim>>);

impl KernelSim {
    fn log(&self) -> Vec<String> {
        self.0.lock().unwrap().log.clone()
    }

    fn refuse(&self, os_error: i32) {
        self.0.lock().unwrap().refuse = Some(os_error);
    }

    fn pending(&self) -> usize {
        self.0.lock().unwrap().pending.values().map(Vec::len).sum()
    }
}

fn einval<T>() -> io::Result<T> {
    Err(io::Error::from_raw_os_error(22))
}

fn rewrite_od_line(table: &str, section: &str, index: Option<&str>, value: &str, unit: &str) -> String {
    let mut out = Vec::new();
    let mut inside = false;
    for line in table.lines() {
        if line.starts_with("OD_") {
            inside = line.trim_end_matches(':') == section;
            out.push(line.to_string());
        } else if inside && index.is_none_or(|i| line.starts_with(&format!("{i}:"))) {
            out.push(match index {
                Some(i) => format!("{i}: {value}{unit}"),
                None => format!("{value}{unit}"),
            });
        } else {
            out.push(line.to_string());
        }
    }
    out.join("\n") + "\n"
}

impl KernelSim {
    fn commit_od(table: String, edits: &[String]) -> String {
        let mut table = table;
        for edit in edits {
            let fields: Vec<&str> = edit.split_whitespace().collect();
            table = match fields.as_slice() {
                ["s", i, v] => rewrite_od_line(&table, "OD_SCLK", Some(i), v, "Mhz"),
                ["s", v] => rewrite_od_line(&table, "OD_SCLK_OFFSET", None, v, "Mhz"),
                ["m", i, v] => rewrite_od_line(&table, "OD_MCLK", Some(i), v, "MHz"),
                ["vo", v] => rewrite_od_line(&table, "OD_VDDGFX_OFFSET", None, v, "mV"),
                _ => table,
            };
        }
        table
    }

    fn write_od(sim: &mut Sim, path: &Path, command: &str, reset_text: &str) -> io::Result<()> {
        let table = OdTable::parse(&fs::read_to_string(path)?);
        let fields: Vec<&str> = command.split_whitespace().collect();
        let in_window = |range: Option<Range>, value: &str| {
            let value: i32 = value.parse().unwrap();
            range.is_some_and(|r| (r.min..=r.max).contains(&value))
        };
        match fields.as_slice() {
            ["c"] => {
                let edits = sim.pending.remove(path).unwrap_or_default();
                fs::write(path, Self::commit_od(fs::read_to_string(path)?, &edits))?;
            }
            ["r"] => {
                sim.pending.remove(path);
                fs::write(path, reset_text)?;
            }
            ["s", i @ ("0" | "1"), v] if table.sclk_range.is_some() => {
                if !in_window(table.sclk_range, v) {
                    return einval();
                }
                // The driver keeps the minimum at or below the maximum on every edit.
                let (min, max) = (table.sclk_min.unwrap_or(0), table.sclk_max.unwrap_or(0));
                let v: i32 = v.parse().unwrap();
                if (*i == "0" && v > max) || (*i == "1" && v < min) {
                    return einval();
                }
                sim.pending.entry(path.to_path_buf()).or_default().push(command.trim().into());
            }
            ["s", v] if in_window(table.sclk_offset_range, v) => {
                sim.pending.entry(path.to_path_buf()).or_default().push(command.trim().into());
            }
            ["m", "0" | "1", v] if in_window(table.mclk_range, v) => {
                sim.pending.entry(path.to_path_buf()).or_default().push(command.trim().into());
            }
            ["vo", v] if in_window(table.voltage_offset_range, v) => {
                sim.pending.entry(path.to_path_buf()).or_default().push(command.trim().into());
            }
            _ => return einval(),
        }
        Ok(())
    }

    fn write_fan_curve_file(sim: &mut Sim, path: &Path, command: &str) -> io::Result<()> {
        let table = FanCurveTable::parse(&fs::read_to_string(path)?);
        let fields: Vec<&str> = command.split_whitespace().collect();
        match fields.as_slice() {
            ["c"] => {
                let edits = sim.pending.remove(path).unwrap_or_default();
                let mut points = table.points.clone();
                for edit in &edits {
                    let v: Vec<i32> = edit.split_whitespace().map(|x| x.parse().unwrap()).collect();
                    points[v[0] as usize] = (v[1], v[2]);
                }
                let lines: Vec<String> =
                    points.iter().enumerate().map(|(i, (t, s))| format!("{i}: {t}C {s}%")).collect();
                let tail = FAN_FRESH.split_once("OD_RANGE").unwrap().1;
                fs::write(path, format!("OD_FAN_CURVE:\n{}\nOD_RANGE{tail}", lines.join("\n")))?;
            }
            ["r"] => {
                sim.pending.remove(path);
                fs::write(path, FAN_FRESH)?;
            }
            [_, t, s] => {
                let (t, s): (i32, i32) = (t.parse().unwrap(), s.parse().unwrap());
                let (temp, speed) = (table.temp_range.unwrap(), table.speed_range.unwrap());
                if !(temp.min..=temp.max).contains(&t) || !(speed.min..=speed.max).contains(&s) {
                    return einval();
                }
                sim.pending.entry(path.to_path_buf()).or_default().push(command.trim().into());
            }
            _ => return einval(),
        }
        Ok(())
    }
}

impl AttrWriter for KernelSim {
    fn write(&mut self, path: &Path, value: &str) -> io::Result<()> {
        let mut guard = self.0.lock().unwrap();
        let sim = &mut *guard;
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        sim.log.push(format!("{file} {}", value.trim()));
        if let Some(raw) = sim.refuse {
            return Err(io::Error::from_raw_os_error(raw));
        }
        // A missing attribute fails the way open() does, without creating anything.
        if !path.exists() {
            return Err(io::Error::from_raw_os_error(2));
        }
        match file.as_str() {
            "pp_od_clk_voltage" => {
                let reset = if fs::read_to_string(path)?.contains("OD_SCLK_OFFSET") { OD_RDNA4 } else { OD_RDNA3 };
                Self::write_od(sim, path, value, reset)
            }
            "fan_curve" => Self::write_fan_curve_file(sim, path, value),
            "fan_zero_rpm_enable" => match value.trim() {
                "c" => {
                    if let Some(edit) = sim.pending.remove(path).and_then(|e| e.last().cloned()) {
                        fs::write(path, format!("FAN_ZERO_RPM_ENABLE:\n{edit}\nOD_RANGE:\nZERO_RPM_ENABLE: 0 1\n"))?;
                    }
                    Ok(())
                }
                "r" => fs::write(path, ZERO_ON),
                v => {
                    sim.pending.entry(path.to_path_buf()).or_default().push(v.into());
                    Ok(())
                }
            },
            "power1_cap" => {
                let dir = path.parent().unwrap();
                let bound = |n: &str| read_i64(&dir.join(n)).unwrap();
                let uw: i64 = value.trim().parse().map_err(|_| io::Error::from_raw_os_error(22))?;
                if uw < bound("power1_cap_min") || uw > bound("power1_cap_max") {
                    return einval();
                }
                fs::write(path, value)
            }
            "pwm1" => {
                if read_i64(&path.with_file_name("pwm1_enable")) != Some(1) {
                    return einval();
                }
                fs::write(path, value)
            }
            _ => fs::write(path, value),
        }
    }
}

fn backend(tree: &Tree) -> (AmdgpuBackend, KernelSim) {
    let sim = KernelSim::default();
    let mut b = AmdgpuBackend::with_paths(tree.root(), tree.module(), Box::new(sim.clone()));
    b.initialize().unwrap();
    (b, sim)
}

/// One awake RDNA 3 card with overdrive and firmware fans.
fn rdna3(tag: &str) -> (Tree, PathBuf) {
    let tree = Tree::new(tag);
    let device = tree.card("card1", "0000:04:00.0", "amdgpu");
    tree.awake_sensors(&device);
    tree.overdrive(&device, OD_RDNA3);
    tree.firmware_fans(&device);
    tree.graphics_core(&device, 11);
    (tree, device)
}

/// Run on a thread and give up after a few seconds: what is being guarded against is a read that
/// never returns, so hanging the test binary would be a poor way to report it.
fn must_finish<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(10)).unwrap_or_else(|_| panic!("{what} blocked on a sleeping card"))
}

/// A FIFO with nobody on the other end: opening it blocks forever, as these attributes do on a card
/// stuck resuming.
fn plant_trap(path: &Path) {
    let _ = fs::remove_file(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let status = std::process::Command::new("mkfifo").arg(path).status().unwrap();
    assert!(status.success());
}

#[test]
fn a_sleeping_card_is_never_asked_anything_that_blocks() {
    let tree = Tree::new("asleep");
    let awake = tree.card("card1", "0000:04:00.0", "amdgpu");
    tree.awake_sensors(&awake);
    fs::write(awake.join("boot_vga"), "1\n").unwrap();
    let asleep = tree.card("card0", "0000:07:00.0", "amdgpu");
    fs::write(asleep.join("power/runtime_status"), "resuming\n").unwrap();
    fs::write(asleep.join("power_state"), "D3cold\n").unwrap();
    // Everything not on the short list of attributes answered from memory.
    for file in [
        "current_link_speed",
        "current_link_width",
        "max_link_speed",
        "max_link_width",
        "gpu_busy_percent",
        "mem_busy_percent",
        "mem_info_vram_used",
        "mem_info_vram_vendor",
        "gpu_metrics",
        "pp_od_clk_voltage",
        "pp_dpm_fclk",
        "pp_dpm_socclk",
        "power_dpm_force_performance_level",
        "gpu_od/fan_ctrl/fan_curve",
        "gpu_od/fan_ctrl/fan_zero_rpm_enable",
        "ip_discovery/die/0/GC/0/major",
        "hwmon/hwmon3/temp1_input",
        "hwmon/hwmon3/temp1_label",
        "hwmon/hwmon3/freq1_input",
        "hwmon/hwmon3/power1_cap",
        "hwmon/hwmon3/power1_cap_default",
        "hwmon/hwmon3/pwm1",
    ] {
        plant_trap(&asleep.join(file));
    }

    let (mut b, sim) = backend(&tree);
    assert_eq!(b.devices()[0].bus_id, "0000:04:00.0", "the desktop's card comes first");
    assert_eq!(b.devices()[1].bus_id, "0000:07:00.0");
    assert_eq!(b.devices()[1].bios_version, "113-D70401XT-P11");
    assert_eq!(b.devices()[1].vram_megabytes, 20464);

    let results = must_finish("the read path", move || {
        let caps = b.capabilities(1).unwrap();
        let telemetry = b.read_telemetry(1).unwrap();
        let temperature = b.read_temperature_only(1).unwrap();
        let state = b.read_tuning_state(1).unwrap();
        let info = b.read_graphics_info(1);
        let notes = b.notes(1);
        let diag = b.diagnostics(1);
        let writes = [
            b.set_core_offset(1, 2500).is_err(),
            b.set_core_min_clock(1, 500).is_err(),
            b.set_memory_offset(1, 1200).is_err(),
            b.set_power_limit(1, 0).is_err(),
            b.set_voltage_curve_offset(1, -50, 0).is_err(),
            b.set_zero_rpm(1, true).is_err(),
            b.set_fan_speed(1, -1, 50).is_err(),
            b.set_fan_curve(1, &FanCurve::default()).is_err(),
            b.set_fan_auto(1).is_err(),
            b.reset_to_defaults(1).is_err(),
        ];
        (caps, telemetry, temperature, state, info, notes, diag, writes)
    });
    let (caps, telemetry, temperature, state, info, notes, diag, writes) = results;

    assert!(!caps.can_set_core_offset && !caps.can_set_power_limit && !caps.can_set_fan_speed);
    assert!(telemetry.temperature_c.is_nan() && telemetry.core_clock_mhz.is_nan());
    assert_eq!(telemetry.limit_reason, "Asleep");
    assert_eq!(temperature.temperature_c, 0.0);
    assert_eq!(state.power_limit_percent, 0);
    assert_eq!(info.bios_version, "113-D70401XT-P11");
    assert_eq!(info.memory_vendor, "");
    assert!(notes[0].contains("asleep (D3cold)"), "{notes:?}");
    assert!(notes[0].contains("0000:07:00.0"), "{notes:?}");
    assert!(diag.contains("asleep"), "{diag}");
    assert!(writes.iter().all(|&refused| refused), "{writes:?}");
    assert!(sim.log().is_empty(), "a write reached the sleeping card: {:?}", sim.log());
}

#[test]
fn only_amdgpu_cards_are_driven_and_the_others_are_named() {
    let tree = Tree::new("others");
    let nvidia = tree.card("card0", "0000:01:00.0", "nvidia");
    fs::write(nvidia.join("vendor"), "0x10de\n").unwrap();
    let sim = KernelSim::default();
    let mut b = AmdgpuBackend::with_paths(tree.root(), tree.module(), Box::new(sim));
    let message = b.initialize().unwrap_err().message;
    assert!(message.contains("NVIDIA GPU at 0000:01:00.0 (driver nvidia)"), "{message}");
    assert!(message.contains("only drives AMD"), "{message}");

    let empty = Tree::new("empty");
    let mut b = AmdgpuBackend::with_paths(empty.root(), empty.module(), Box::new(KernelSim::default()));
    assert!(b.initialize().unwrap_err().message.starts_with("No GPU using the amdgpu"));
}

#[test]
fn connectors_are_not_cards_and_order_survives_renumbering() {
    let tree = Tree::new("order");
    tree.card("card2", "0000:07:00.0", "amdgpu");
    let boot = tree.card("card3", "0000:09:00.0", "amdgpu");
    fs::write(boot.join("boot_vga"), "1\n").unwrap();
    tree.card("card1", "0000:04:00.0", "amdgpu");
    fs::create_dir_all(tree.root().join("card1-DP-1")).unwrap();
    let (b, _) = backend(&tree);
    let order: Vec<&str> = b.devices().iter().map(|d| d.bus_id.as_str()).collect();
    assert_eq!(order, ["0000:09:00.0", "0000:04:00.0", "0000:07:00.0"]);
    assert!(b.devices().iter().enumerate().all(|(i, d)| d.index == i));
}

#[test]
fn rdna3_offers_what_the_driver_prints() {
    let (tree, _) = rdna3("caps");
    let (mut b, _) = backend(&tree);
    let c = b.capabilities(0).unwrap();
    assert!(c.can_set_core_offset && c.core_clock_is_absolute);
    assert_eq!((c.core_offset_min_mhz, c.core_offset_max_mhz, c.core_clock_default_mhz), (500, 3000, 2525));
    assert!(c.can_set_core_min_clock);
    assert_eq!((c.core_min_clock_min_mhz, c.core_min_clock_max_mhz, c.core_min_clock_default_mhz), (500, 3000, 500));
    assert!(c.can_set_memory_offset && c.memory_clock_is_absolute);
    assert_eq!(
        (c.memory_offset_min_mhz, c.memory_offset_max_mhz, c.memory_clock_default_mhz),
        (1250, 1500, 1250),
        "up from stock only: see set_memory_offset"
    );
    assert_eq!(c.voltage_style, VoltageControlStyle::Offset);
    assert!(!c.voltage_offset_is_absolute);
    assert_eq!((c.voltage_offset_min_mv, c.voltage_offset_max_mv), (-200, 0));
    assert!(c.can_set_power_limit && c.power_limit_is_offset);
    assert_eq!((c.power_limit_min_percent, c.power_limit_max_percent, c.power_limit_default_percent), (-10, 9, 0));
    assert!(c.can_set_fan_speed && c.fan_curve_is_hardware && c.can_read_hardware_fan_curve);
    assert_eq!((c.fan_curve_points, c.fan_curve_min_temp_c, c.fan_curve_max_temp_c), (5, 25, 100));
    assert_eq!((c.fan_min_percent, c.fan_max_percent, c.fan_count), (23, 100, 1));
    assert!(c.can_set_zero_rpm);
    assert!(!c.can_set_temp_limit, "amdgpu's thermal limits are read-only");
    assert!(!c.can_set_memory_timing && !c.can_lock_clocks && !c.can_edit_vf_curve);
}

#[test]
fn with_overdrive_off_only_the_power_cap_remains_and_the_fans_are_left_to_the_firmware() {
    let tree = Tree::new("od-off");
    let device = tree.card("card1", "0000:04:00.0", "amdgpu");
    tree.awake_sensors(&device);
    tree.graphics_core(&device, 11);
    tree.feature_mask(0xfff7_bfff);
    let (mut b, _) = backend(&tree);
    let c = b.capabilities(0).unwrap();
    assert!(c.can_set_power_limit);
    assert!(!c.can_set_core_offset && !c.can_set_memory_offset && !c.can_set_core_min_clock);
    assert_eq!(c.voltage_style, VoltageControlStyle::None);
    // pwm1 is right there and writable, and on these cards is exactly what must not be used.
    assert!(device.join("hwmon/hwmon3/pwm1").exists());
    assert!(!c.can_set_fan_speed, "no pwm1 fan control on firmware-fan cards");
    assert!(b.set_fan_speed(0, -1, 50).is_err());
    let notes = b.notes(0);
    assert!(notes.iter().any(|n| n.contains(&sysfs::setup_command(" --overdrive"))), "{notes:?}");
}

#[test]
fn the_overdrive_note_goes_when_the_bit_is_set() {
    let (tree, _) = rdna3("od-on");
    tree.feature_mask(0xfff7_ffff);
    let (mut b, _) = backend(&tree);
    assert!(!b.notes(0).iter().any(|n| n.contains("Overdrive")));
}

#[test]
fn older_cards_are_driven_through_hwmon() {
    let tree = Tree::new("older");
    let device = tree.card("card0", "0000:01:00.0", "amdgpu");
    tree.awake_sensors(&device);
    tree.graphics_core(&device, 10);
    let (mut b, sim) = backend(&tree);
    let c = b.capabilities(0).unwrap();
    assert!(c.can_set_fan_speed && !c.fan_curve_is_hardware);
    b.set_fan_speed(0, -1, 50).unwrap();
    assert_eq!(sim.log(), ["pwm1_enable 1", "pwm1 128"], "manual first: the driver refuses a duty otherwise");
    let s = b.read_tuning_state(0).unwrap();
    assert!(s.fan_manual);
    assert_eq!(s.fan_percent, 50);
    b.set_fan_auto(0).unwrap();
    assert_eq!(read_i64(&device.join("hwmon/hwmon3/pwm1_enable")), Some(2));
    assert!(!b.read_tuning_state(0).unwrap().fan_manual);
    assert!(b.set_fan_curve(0, &FanCurve::default()).is_err(), "a software curve is the service's job");
}

#[test]
fn telemetry_comes_from_hwmon_and_metrics() {
    let (tree, _) = rdna3("telemetry");
    let (mut b, _) = backend(&tree);
    let t = b.read_telemetry(0).unwrap();
    assert_eq!((t.core_clock_mhz, t.memory_clock_mhz), (26.0, 96.0));
    assert_eq!((t.temperature_c, t.hot_spot_c, t.memory_temperature_c), (43.0, 50.0, 52.0));
    assert_eq!((t.voltage_mv, t.power_watts), (682.0, 14.0));
    assert!((t.power_percent - 14.0 / 265.0 * 100.0).abs() < 1e-9);
    assert_eq!((t.gpu_load_percent, t.memory_load_percent), (2.0, 1.0));
    assert!((t.memory_used_mb - 585.5).abs() < 1.0);
    assert!((t.fan_percent - 50.2).abs() < 0.1);
    assert_eq!(t.fan_rpm, 1200.0);
    assert_eq!((t.fabric_clock_mhz, t.soc_clock_mhz), (1000.0, 600.0));
    assert_eq!(t.limit_reason, "None");
    let sensor = |key: &str| t.supplemental_sensors.iter().find(|s| s.key == key).map(|s| s.value);
    assert_eq!(sensor("amdgpu:vr_gfx"), Some(47.0));
    assert_eq!(sensor("amdgpu:v_soc"), Some(702.0));
    assert_eq!(sensor("amdgpu:power_cap"), Some(265.0));
    assert_eq!(sensor("amdgpu:pcie_gen"), Some(4.0));
    assert_eq!(sensor("amdgpu:pcie_width"), Some(8.0));
    assert_eq!(b.read_temperature_only(0).unwrap().temperature_c, 43.0);
}

#[test]
fn a_thermal_bit_on_a_cold_card_is_not_reported_but_one_near_its_limit_is() {
    let (tree, device) = rdna3("thermal");
    let hw = device.join("hwmon/hwmon3");
    fs::write(hw.join("temp2_crit"), "110000\n").unwrap();
    // The real idle report: the hot spot throttling bit.
    let mut metrics = METRICS;
    metrics[112..120].copy_from_slice(&(1u64 << 36).to_le_bytes());
    fs::write(device.join("gpu_metrics"), metrics).unwrap();
    let (mut b, _) = backend(&tree);
    assert_eq!(b.read_telemetry(0).unwrap().limit_reason, "None", "50 °C against a 110 °C limit");

    fs::write(hw.join("temp2_input"), "95000\n").unwrap();
    assert_eq!(b.read_telemetry(0).unwrap().limit_reason, "Thermal");

    // With no limit to compare against, the firmware's word stands.
    fs::remove_file(hw.join("temp2_crit")).unwrap();
    fs::write(hw.join("temp2_input"), "50000\n").unwrap();
    assert_eq!(b.read_telemetry(0).unwrap().limit_reason, "Thermal");
}

#[test]
fn a_power_bit_is_only_reported_when_the_board_is_near_its_cap() {
    let (tree, device) = rdna3("power-bit");
    let hw = device.join("hwmon/hwmon3");
    let mut metrics = METRICS;
    metrics[112..120].copy_from_slice(&1u64.to_le_bytes());
    fs::write(device.join("gpu_metrics"), metrics).unwrap();
    let (mut b, _) = backend(&tree);
    // What a real card reported just after boot: the bit, at about 40 % of its 265 W cap.
    fs::write(hw.join("power1_average"), "108000000\n").unwrap();
    assert_eq!(b.read_telemetry(0).unwrap().limit_reason, "None");

    fs::write(hw.join("power1_average"), "260000000\n").unwrap();
    assert_eq!(b.read_telemetry(0).unwrap().limit_reason, "Power");

    // A lowered cap counts: this is the limit actually in force, not the default.
    fs::write(hw.join("power1_cap"), "238000000\n").unwrap();
    fs::write(hw.join("power1_average"), "200000000\n").unwrap();
    assert_eq!(b.read_telemetry(0).unwrap().limit_reason, "Power");

    // With no cap to compare against, the firmware's word stands.
    fs::remove_file(hw.join("power1_cap")).unwrap();
    fs::write(hw.join("power1_average"), "20000000\n").unwrap();
    assert_eq!(b.read_telemetry(0).unwrap().limit_reason, "Power");
}

#[test]
fn a_missing_sensor_is_nan_not_zero() {
    let (tree, device) = rdna3("missing");
    for file in
        ["hwmon/hwmon3/fan1_input", "hwmon/hwmon3/pwm1", "hwmon/hwmon3/in0_input", "gpu_metrics", "gpu_busy_percent"]
    {
        fs::remove_file(device.join(file)).unwrap();
    }
    let (mut b, _) = backend(&tree);
    let t = b.read_telemetry(0).unwrap();
    assert!(t.fan_rpm.is_nan() && t.fan_percent.is_nan() && t.voltage_mv.is_nan() && t.gpu_load_percent.is_nan());
    assert_eq!(t.limit_reason, "Unavailable");
    assert_eq!(t.temperature_c, 43.0, "the rest still reads");
}

#[test]
fn power_limits_are_whole_watts_inside_the_window() {
    assert_eq!(power_window(238_000_000, 290_000_000, 265_000_000), Some(Range { min: -10, max: 9 }));
    assert_eq!(power_window(0, 0, 265_000_000), None);
    assert_eq!(power_window(1, 2, 0), None);
    assert_eq!(power_cap_uw(0, 238_000_000, 290_000_000, 265_000_000), 265_000_000);
    assert_eq!(power_cap_uw(-5, 238_000_000, 290_000_000, 265_000_000), 252_000_000);
    assert_eq!(power_cap_uw(50, 238_000_000, 290_000_000, 265_000_000), 290_000_000);
    assert_eq!(power_cap_uw(-50, 238_000_000, 290_000_000, 265_000_000), 238_000_000);
    // Every percent in the window survives the round trip through whole watts.
    for percent in -10..=9 {
        let uw = power_cap_uw(percent, 238_000_000, 290_000_000, 265_000_000);
        let back = ((uw - 265_000_000) as f64 / 265_000_000.0 * 100.0).round() as i32;
        assert_eq!(back, percent);
    }
}

#[test]
fn a_power_limit_reaches_the_card_and_reads_back() {
    let (tree, device) = rdna3("power");
    let (mut b, sim) = backend(&tree);
    b.set_power_limit(0, -5).unwrap();
    assert_eq!(sim.log(), ["power1_cap 252000000"]);
    assert_eq!(read_i64(&device.join("hwmon/hwmon3/power1_cap")), Some(252_000_000));
    assert_eq!(b.read_tuning_state(0).unwrap().power_limit_percent, -5);
}

#[test]
fn clock_edits_are_committed_one_by_one_and_read_back() {
    let (tree, _) = rdna3("clocks");
    let (mut b, sim) = backend(&tree);
    b.capabilities(0).unwrap();
    b.set_core_offset(0, 2400).unwrap();
    b.set_core_min_clock(0, 700).unwrap();
    b.set_memory_offset(0, 1300).unwrap();
    b.set_voltage_curve_offset(0, -60, 0).unwrap();
    assert_eq!(
        sim.log(),
        [
            "pp_od_clk_voltage s 1 2400",
            "pp_od_clk_voltage c",
            "pp_od_clk_voltage s 0 700",
            "pp_od_clk_voltage c",
            "pp_od_clk_voltage m 1 1300",
            "pp_od_clk_voltage c",
            "pp_od_clk_voltage vo -60",
            "pp_od_clk_voltage c",
        ]
    );
    assert_eq!(sim.pending(), 0);
    let s = b.read_tuning_state(0).unwrap();
    assert_eq!(
        (s.core_offset_mhz, s.core_min_clock_mhz, s.memory_offset_mhz, s.voltage_offset_mv),
        (2400, 700, 1300, -60)
    );
}

#[test]
fn out_of_range_values_are_clamped_to_the_drivers_window() {
    let (tree, _) = rdna3("clamp");
    let (mut b, sim) = backend(&tree);
    b.set_core_offset(0, 9000).unwrap();
    b.set_memory_offset(0, 9000).unwrap();
    b.set_voltage_curve_offset(0, -999, 0).unwrap();
    let log = sim.log();
    assert!(log.contains(&"pp_od_clk_voltage s 1 3000".to_string()), "{log:?}");
    assert!(log.contains(&"pp_od_clk_voltage m 1 1500".to_string()), "{log:?}");
    assert!(log.contains(&"pp_od_clk_voltage vo -200".to_string()), "{log:?}");
}

/// The write that hung a real RX 7900 XT's firmware: `m 1 800` on a 1250 MHz card, under load. No
/// value and no state may get it to the card again.
#[test]
fn the_memory_clock_can_never_be_capped_below_stock() {
    let (tree, _) = rdna3("mem-floor");
    let (mut b, sim) = backend(&tree);
    for asked in [800, 97, 0, -1, 1249] {
        b.set_memory_offset(0, asked).unwrap();
        assert!(sim.log().is_empty(), "{asked} MHz reached the card: {:?}", sim.log());
    }
    assert_eq!(b.read_tuning_state(0).unwrap().memory_offset_mhz, 1250);

    // Busy or not, and after a raise too: back down to stock is as far as it goes.
    b.set_memory_offset(0, 1300).unwrap();
    b.set_memory_offset(0, 800).unwrap();
    assert_eq!(b.read_tuning_state(0).unwrap().memory_offset_mhz, 1250);
    assert!(!sim.log().iter().any(|l| l.contains("m 1 800")), "{:?}", sim.log());

    // A card whose tables print a stock above the window still gets a value inside it.
    assert_eq!(memory_window(Range { min: 97, max: 1500 }, 1250), Range { min: 1250, max: 1500 });
    assert_eq!(memory_window(Range { min: 97, max: 1000 }, 1250), Range { min: 1000, max: 1000 });
    assert_eq!(memory_window(Range { min: 97, max: 1500 }, 0), Range { min: 97, max: 1500 }, "stock unknown");
}

#[test]
fn the_memory_clock_moves_on_a_busy_card_too_as_on_windows() {
    let (tree, device) = rdna3("mem-busy");
    fs::write(device.join("gpu_busy_percent"), "80\n").unwrap();
    let (mut b, sim) = backend(&tree);
    b.set_memory_offset(0, 1300).unwrap();
    assert_eq!(sim.log(), ["pp_od_clk_voltage m 1 1300", "pp_od_clk_voltage c"]);
}

/// What the Windows driver reports for a real RX 7900 XT: 2500 to 3000 MHz, default 2500. The driver here
/// counts the memory controller's clock, half of that, and prints a bottom of 97.
#[test]
fn gddr6_memory_is_in_the_same_units_and_range_as_windows() {
    let (tree, _) = rdna3("mem-units");
    let table = tree.0.join("pci.ids");
    let names = "1002  Advanced Micro Devices, Inc. [AMD/ATI]\n\t744c  Navi 31 [Radeon RX 7900 XT/7900 XTX]\n";
    fs::write(&table, names).unwrap();
    let sim = KernelSim::default();
    let mut b =
        AmdgpuBackend::with_paths(tree.root(), tree.module(), Box::new(sim.clone())).reading_names_from(vec![table]);
    b.initialize().unwrap();

    let c = b.capabilities(0).unwrap();
    assert_eq!((c.memory_offset_min_mhz, c.memory_offset_max_mhz, c.memory_clock_default_mhz), (2500, 3000, 2500));
    assert_eq!(b.read_tuning_state(0).unwrap().memory_offset_mhz, 2500);
    assert_eq!(b.read_telemetry(0).unwrap().memory_clock_mhz, 192.0, "96 MHz on the controller");
    let info = b.read_graphics_info(0);
    assert_eq!((info.code_name.as_str(), info.memory_type.as_str()), ("Navi 31", "GDDR6"));

    // Stock is already on the card, so nothing is sent.
    b.set_memory_offset(0, 2500).unwrap();
    assert!(sim.log().is_empty(), "{:?}", sim.log());

    // The driver gets its own number, and it reads back in the shown one.
    b.set_memory_offset(0, 2600).unwrap();
    assert_eq!(sim.log(), ["pp_od_clk_voltage m 1 1300", "pp_od_clk_voltage c"]);
    assert_eq!(b.read_tuning_state(0).unwrap().memory_offset_mhz, 2600);

    // Both ends of the window hold in these units too. The 800 that hung a real card here would have
    // been 400 on the controller, and comes out as stock.
    b.set_memory_offset(0, 9000).unwrap();
    assert_eq!(b.read_tuning_state(0).unwrap().memory_offset_mhz, 3000);
    b.set_memory_offset(0, 800).unwrap();
    assert_eq!(b.read_tuning_state(0).unwrap().memory_offset_mhz, 2500);
    assert!(!sim.log().iter().any(|l| l.contains("m 1 400") || l.contains("m 1 800")), "{:?}", sim.log());
}

#[test]
fn a_value_already_on_the_card_is_not_written() {
    let (tree, _) = rdna3("unchanged");
    let (mut b, sim) = backend(&tree);
    b.set_core_offset(0, 2525).unwrap();
    b.set_core_min_clock(0, 500).unwrap();
    b.set_memory_offset(0, 1250).unwrap();
    b.set_voltage_curve_offset(0, 0, 0).unwrap();
    b.set_power_limit(0, 0).unwrap();
    b.set_zero_rpm(0, true).unwrap();
    b.set_fan_auto(0).unwrap();
    assert!(sim.log().is_empty(), "nothing changed, so nothing should reach the firmware: {:?}", sim.log());

    // Nor on an older card's fan.
    let older = Tree::new("unchanged-older");
    let device = older.card("card0", "0000:01:00.0", "amdgpu");
    older.awake_sensors(&device);
    older.graphics_core(&device, 10);
    let (mut b, sim) = backend(&older);
    b.set_fan_auto(0).unwrap();
    assert!(sim.log().is_empty(), "{:?}", sim.log());
}

#[test]
fn rdna4_writes_a_core_offset() {
    let tree = Tree::new("rdna4");
    let device = tree.card("card1", "0000:04:00.0", "amdgpu");
    tree.awake_sensors(&device);
    tree.overdrive(&device, OD_RDNA4);
    let (mut b, sim) = backend(&tree);
    let c = b.capabilities(0).unwrap();
    assert!(c.can_set_core_offset && !c.core_clock_is_absolute && !c.can_set_core_min_clock);
    assert_eq!((c.core_offset_min_mhz, c.core_offset_max_mhz), (-500, 1000));
    b.set_core_offset(0, 150).unwrap();
    assert_eq!(sim.log(), ["pp_od_clk_voltage s 150", "pp_od_clk_voltage c"]);
    assert_eq!(b.read_tuning_state(0).unwrap().core_offset_mhz, 150);
}

#[test]
fn a_fixed_duty_is_a_flat_curve_and_leaves_zero_rpm_alone_as_on_windows() {
    let (tree, device) = rdna3("fan-fixed");
    let (mut b, sim) = backend(&tree);
    b.set_fan_speed(0, -1, 60).unwrap();
    assert_eq!(
        sim.log(),
        [
            "fan_curve 0 25 60",
            "fan_curve 1 43 60",
            "fan_curve 2 62 60",
            "fan_curve 3 81 60",
            "fan_curve 4 100 60",
            "fan_curve c",
        ]
    );
    let s = b.read_tuning_state(0).unwrap();
    assert_eq!(s.detected_fan_mode, Some(FanMode::Fixed));
    assert!(s.fan_manual);
    assert_eq!(s.fan_percent, 60);
    assert!(s.zero_rpm, "it has its own control");

    // Below the firmware's floor it is raised to it rather than refused.
    b.set_fan_speed(0, -1, 5).unwrap();
    assert_eq!(b.read_tuning_state(0).unwrap().fan_percent, 23);

    // Back to auto restores the curve and nothing else.
    b.set_zero_rpm(0, false).unwrap();
    sim.0.lock().unwrap().log.clear();
    b.set_fan_auto(0).unwrap();
    assert_eq!(sim.log(), ["fan_curve r", "fan_curve c"]);
    let s = b.read_tuning_state(0).unwrap();
    assert_eq!(s.detected_fan_mode, Some(FanMode::Auto));
    assert!(!s.fan_manual);
    assert!(!s.zero_rpm, "what the user set stays set");
    assert_eq!(fs::read_to_string(device.join(FAN_CURVE_FILE)).unwrap(), FAN_FRESH);
}

#[test]
fn a_hardware_curve_is_sampled_down_and_reads_back_as_one() {
    let (tree, _) = rdna3("fan-curve");
    let (mut b, sim) = backend(&tree);
    let curve = FanCurve::with_points(vec![FanPoint::new(30.0, 30.0), FanPoint::new(90.0, 100.0)]);
    b.set_fan_curve(0, &curve).unwrap();
    let log = sim.log();
    assert_eq!(log.len(), 6, "{log:?}");
    assert_eq!(log.last().map(String::as_str), Some("fan_curve c"));
    assert!(!log.iter().any(|l| l.starts_with("fan_zero_rpm")), "a curve leaves zero RPM to the user");
    let s = b.read_tuning_state(0).unwrap();
    assert_eq!(s.detected_fan_mode, Some(FanMode::Curve));
    assert!(!s.fan_manual);
    let points = s.hardware_fan_curve.unwrap().points;
    assert_eq!(points.len(), 5);
    assert!(points.windows(2).all(|w| w[0].temperature_c < w[1].temperature_c));
    assert!(points.windows(2).all(|w| w[0].fan_percent <= w[1].fan_percent));
    assert_eq!(points[4].fan_percent, 100.0);
}

#[test]
fn zero_rpm_is_written_and_committed() {
    let (tree, _) = rdna3("zero");
    let (mut b, sim) = backend(&tree);
    b.set_zero_rpm(0, false).unwrap();
    assert!(!b.read_tuning_state(0).unwrap().zero_rpm);
    b.set_zero_rpm(0, true).unwrap();
    assert!(b.read_tuning_state(0).unwrap().zero_rpm);
    assert_eq!(sim.log().len(), 4);
}

#[test]
fn reset_restores_everything_and_reports_what_failed() {
    let (tree, device) = rdna3("reset");
    fs::write(device.join(PERFORMANCE_LEVEL_FILE), "manual\n").unwrap();
    let (mut b, sim) = backend(&tree);
    b.set_core_offset(0, 2400).unwrap();
    b.set_power_limit(0, 5).unwrap();
    b.set_fan_speed(0, -1, 60).unwrap();
    b.reset_to_defaults(0).unwrap();
    let s = b.read_tuning_state(0).unwrap();
    assert_eq!((s.core_offset_mhz, s.power_limit_percent, s.fan_manual), (2525, 0, false));
    assert_eq!(s.detected_fan_mode, Some(FanMode::Auto));
    assert_eq!(read_text(&device.join(PERFORMANCE_LEVEL_FILE)).as_deref(), Some("auto"));

    sim.refuse(13);
    let message = b.reset_to_defaults(0).unwrap_err().message;
    assert!(message.starts_with("Reset partially failed"), "{message}");
    assert!(message.contains(&sysfs::setup_command("")), "{message}");
}

#[test]
fn a_refused_write_explains_itself_and_changes_nothing() {
    let (tree, device) = rdna3("refused");
    let (mut b, sim) = backend(&tree);
    sim.refuse(13);
    let message = b.set_power_limit(0, -5).unwrap_err().message;
    assert!(message.contains("power1_cap") && message.contains(&sysfs::setup_command("")), "{message}");
    assert_eq!(read_i64(&device.join("hwmon/hwmon3/power1_cap")), Some(265_000_000));
}

#[test]
fn a_notes_line_names_the_fix_when_writes_would_be_refused() {
    let (tree, device) = rdna3("perm");
    let cap = device.join("hwmon/hwmon3/power1_cap");
    let mut mode = fs::metadata(&cap).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o444);
    fs::set_permissions(&cap, mode).unwrap();
    let (mut b, _) = backend(&tree);
    // Root can write anything, and there the note is rightly absent.
    if fs::OpenOptions::new().write(true).open(&cap).is_err() {
        assert!(b.notes(0).iter().any(|n| n.contains(&sysfs::setup_command(""))));
    } else {
        assert!(b.notes(0).is_empty());
    }
}

#[test]
fn a_refusal_is_retried_in_manual_mode_and_then_believed() {
    /// Refuses overdrive edits until the level is manual, as Renoir and Van Gogh do.
    #[derive(Clone)]
    struct NeedsManual(KernelSim);
    impl AttrWriter for NeedsManual {
        fn write(&mut self, path: &Path, value: &str) -> io::Result<()> {
            let edit = path.ends_with(OD_FILE) && value.trim() != "c" && value.trim() != "r";
            let level = read_text(&path.with_file_name(PERFORMANCE_LEVEL_FILE));
            if edit && level.as_deref() != Some("manual") {
                self.0.0.lock().unwrap().log.push(format!("refused {}", value.trim()));
                return einval();
            }
            self.0.write(path, value)
        }
    }
    let (tree, device) = rdna3("manual");
    let sim = KernelSim::default();
    let mut b = AmdgpuBackend::with_paths(tree.root(), tree.module(), Box::new(NeedsManual(sim.clone())));
    b.initialize().unwrap();
    b.set_core_offset(0, 2400).unwrap();
    assert_eq!(read_text(&device.join(PERFORMANCE_LEVEL_FILE)).as_deref(), Some("manual"));
    assert_eq!(b.read_tuning_state(0).unwrap().core_offset_mhz, 2400);

    // A genuine refusal leaves the level as it found it.
    let (tree, device) = rdna3("manual-refused");
    let (mut b, sim) = backend(&tree);
    let message = b.od_write(0, "s 1 9999").unwrap_err().message;
    assert!(message.contains("refused 's 1 9999'"), "{message}");
    assert_eq!(read_text(&device.join(PERFORMANCE_LEVEL_FILE)).as_deref(), Some("auto"));
    assert_eq!(sim.pending(), 0);
}

#[test]
fn stock_values_outlive_the_process_so_a_tuned_clock_is_not_mistaken_for_stock() {
    let (tree, _) = rdna3("stock");
    let keep = tree.0.join("run");
    fs::create_dir_all(&keep).unwrap();
    let open = |t: &Tree| {
        let mut b = AmdgpuBackend::with_paths(t.root(), t.module(), Box::new(KernelSim::default()))
            .keeping_stock_in(Some(keep.clone()));
        b.initialize().unwrap();
        b
    };
    let mut first = open(&tree);
    assert_eq!(first.capabilities(0).unwrap().core_clock_default_mhz, 2525);
    first.set_core_offset(0, 2400).unwrap();
    drop(first);
    let mut second = open(&tree);
    assert_eq!(second.read_tuning_state(0).unwrap().core_offset_mhz, 2400, "the card keeps the clock");
    assert_eq!(second.capabilities(0).unwrap().core_clock_default_mhz, 2525, "and stock is still stock");
}

/// A card at the end of the path a real RX 7900 XT sits on: root port, the two ports of the switch on the
/// card's own board, then the GPU. (current width, maximum speed, maximum width) per hop, CPU first.
fn card_on_a_path(tag: &str, hops: [(&str, &str, &str); 4]) -> (Tree, PathBuf) {
    let tree = Tree::new(tag);
    let path = "0000:00:06.0/0000:02:00.0/0000:03:00.0/0000:04:00.0";
    let device = tree.card("card1", path, "amdgpu");
    tree.awake_sensors(&device);
    fs::remove_file(device.join("max_link_speed")).unwrap();
    fs::remove_file(device.join("max_link_width")).unwrap();
    let mut hop = tree.0.join("devices");
    for (name, (current_width, maximum_speed, maximum_width)) in
        ["0000:00:06.0", "0000:02:00.0", "0000:03:00.0", "0000:04:00.0"].into_iter().zip(hops)
    {
        hop = hop.join(name);
        fs::write(hop.join("current_link_width"), current_width).unwrap();
        fs::write(hop.join("max_link_speed"), maximum_speed).unwrap();
        fs::write(hop.join("max_link_width"), maximum_width).unwrap();
    }
    (tree, device)
}

const GEN5: &str = "32.0 GT/s PCIe\n";
const GEN4: &str = "16.0 GT/s PCIe\n";
const GEN3: &str = "8.0 GT/s PCIe\n";

#[test]
fn the_bus_reading_is_the_real_slot_not_the_link_inside_the_card() {
    // Exactly what a real RX 7900 XT in an x8 slot reports: the GPU and the switch's lower port say x16
    // because they are talking to each other on the card's own board.
    let (tree, _) = card_on_a_path(
        "bus-x8",
        [("8\n", GEN5, "8\n"), ("8\n", GEN4, "16\n"), ("16\n", GEN4, "16\n"), ("16\n", GEN4, "16\n")],
    );
    let (mut b, _) = backend(&tree);
    assert_eq!(b.read_graphics_info(0).bus_interface, "4.0 x8");
}

#[test]
fn the_bus_reading_shows_a_slot_running_below_what_it_is_capable_of() {
    // Both ends can do x16, and they settled on x8.
    let (tree, _) = card_on_a_path(
        "bus-degraded",
        [("8\n", GEN4, "16\n"), ("8\n", GEN4, "16\n"), ("16\n", GEN4, "16\n"), ("16\n", GEN4, "16\n")],
    );
    let (mut b, _) = backend(&tree);
    assert_eq!(b.read_graphics_info(0).bus_interface, "4.0 x8");
}

#[test]
fn the_bus_reading_is_held_back_by_an_older_board() {
    let (tree, _) = card_on_a_path(
        "bus-gen3",
        [("16\n", GEN3, "16\n"), ("16\n", GEN4, "16\n"), ("16\n", GEN4, "16\n"), ("16\n", GEN4, "16\n")],
    );
    let (mut b, _) = backend(&tree);
    assert_eq!(b.read_graphics_info(0).bus_interface, "3.0 x16");
}

#[test]
fn a_hop_with_no_real_link_does_not_count() {
    let (tree, _) = card_on_a_path(
        "bus-unknown",
        [("0\n", "Unknown\n", "255\n"), ("16\n", GEN4, "16\n"), ("16\n", GEN4, "16\n"), ("16\n", GEN4, "16\n")],
    );
    let (mut b, _) = backend(&tree);
    assert_eq!(b.read_graphics_info(0).bus_interface, "4.0 x16");
}

#[test]
fn pci_addresses_are_told_from_other_directories() {
    assert!(is_pci_address("0000:04:00.0"));
    assert!(is_pci_address("0000:00:06.3"));
    assert!(!is_pci_address("pci0000:00"));
    assert!(!is_pci_address("devices"));
    assert!(!is_pci_address("0000:04:00"));
}

#[test]
fn graphics_info_names_the_board() {
    let (tree, _) = rdna3("info");
    let (mut b, _) = backend(&tree);
    let info = b.read_graphics_info(0);
    assert_eq!(info.board_manufacturer, "Sapphire Technology");
    assert_eq!(info.revision, "CC");
    assert_eq!(info.memory_vendor, "Samsung");
    assert_eq!(info.bus_interface, "4.0 x16");
    assert_eq!(info.bios_version, "113-D70401XT-P11");
    assert_eq!(info.bus_address, "0000:04:00.0");
    assert_eq!(info.memory_size, "19.98 GB");
}
