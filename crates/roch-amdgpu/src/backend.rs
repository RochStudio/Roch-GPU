//! The backend. Everything it can do is discovered from what the driver prints, as on Windows: a
//! control whose file or range is missing is a control this card does not offer.
//!
//! **A sleeping card is never touched.** The kernel powers an idle card down, and most attributes then
//! either refuse (hwmon, which is harmless) or block until the card has woken (PCI link attributes
//! and every write). A card that fails to resume blocks them for good, in a sleep the process cannot
//! be killed out of; a second RX 7900 XT did exactly this, retrying its resume every four seconds from
//! boot. So [`AmdgpuBackend::is_awake`] guards everything that is not on the short list of attributes
//! the driver answers from memory (see [`AmdgpuBackend::describe_card`]), and nothing opens the card's
//! DRM node, which would wake it.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use roch_core::models::graphics_info::trim_decimals;
use roch_core::util::clamp_i32;
use roch_core::{
    FanCurve, FanMode, FanPoint, GpuBackend, GpuCapabilities, GpuDevice, GpuError, GpuGraphicsInfo, GpuIdentity,
    GpuTelemetry, GpuTuningState, Result, SupplementalSensor, VoltageControlStyle, bail,
};

use crate::gpu_metrics::{self, GpuMetrics};
use crate::identity;
use crate::od::{self, FanCurveTable, FanTableMode, OdTable, Range};
use crate::sysfs::{self, AttrWriter, KernelWriter, read_hex, read_i64, read_text};

const DRM_ROOT: &str = "/sys/class/drm";
const MODULE_PARAMETERS: &str = "/sys/module/amdgpu/parameters";

const OD_FILE: &str = "pp_od_clk_voltage";
const FAN_CURVE_FILE: &str = "gpu_od/fan_ctrl/fan_curve";
const ZERO_RPM_FILE: &str = "gpu_od/fan_ctrl/fan_zero_rpm_enable";
const PERFORMANCE_LEVEL_FILE: &str = "power_dpm_force_performance_level";

/// `amdgpu.ppfeaturemask`'s overdrive bit (`PP_OVERDRIVE_MASK`).
pub const OVERDRIVE_BIT: u64 = 0x4000;

/// The first Graphics Core generation whose fans belong to the power management firmware (RDNA 3).
/// It still lists `pwm1` as writable, but taking manual control there switches off the firmware's own
/// fan loop for a register path it may not honour, which risks a stopped fan on a hot card. LACT
/// reaches the same conclusion: on these cards the only fan control is the firmware's `fan_curve`.
const FIRMWARE_FANS_FROM_GC: u32 = 11;

/// How close to its critical temperature a sensor must be before a thermal throttling bit is believed.
const NEAR_THERMAL_LIMIT_C: f64 = 20.0;
/// How much of its cap the board must be drawing before a power throttling bit is believed. Well under
/// the cap, because the reading is an average and the limit acts on short spikes too.
const NEAR_POWER_LIMIT: f64 = 0.75;

/// One card, as found at start-up.
struct Card {
    /// `.../cardN/device`.
    dir: PathBuf,
    name: String,
    pci: String,
    hwmon: Option<PathBuf>,
    device_id: u32,
    revision: u32,
    subsystem_vendor: u32,
    boot_vga: bool,
    code_name: String,
    /// See [`identity::memory_clock_scale`].
    memory_scale: i32,
}

impl Card {
    fn attr(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn hw(&self, name: &str) -> Option<PathBuf> {
        Some(self.hwmon.as_ref()?.join(name))
    }

    fn hw_i64(&self, name: &str) -> Option<i64> {
        read_i64(&self.hw(name)?)
    }

    fn hw_scaled(&self, name: &str, per_unit: f64) -> f64 {
        self.hw_i64(name).map_or(f64::NAN, |v| v as f64 / per_unit)
    }
}

/// The overdrive values the card booted with. The driver does not publish them, so they are recorded
/// the first time the card is read after a boot and kept outside the process: a clock this app set
/// on an earlier run must not become "stock" on the next one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct OdStock {
    sclk_min: i32,
    sclk_max: i32,
    mclk_max: i32,
}

/// How this card's fans can be driven.
enum FanControl {
    /// The firmware runs a curve we hand it (RDNA 3 and later, with overdrive on).
    Curve(FanCurveTable),
    /// A duty written to hwmon, which a resident process steps for a curve (older cards).
    Pwm,
    None,
}

pub struct AmdgpuBackend {
    root: PathBuf,
    module_parameters: PathBuf,
    stock_dir: Option<PathBuf>,
    pci_ids: Vec<PathBuf>,
    writer: Box<dyn AttrWriter>,
    cards: Vec<Card>,
    devices: Vec<GpuDevice>,
    stock: HashMap<usize, OdStock>,
}

impl Default for AmdgpuBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// `cardN`, not `cardN-DP-1`.
fn is_card_name(name: &str) -> bool {
    name.strip_prefix("card").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The last path component of a symlink. Reading a link asks nothing of the device behind it.
fn link_name(path: &Path) -> Option<String> {
    Some(fs::read_link(path).ok()?.file_name()?.to_string_lossy().into_owned())
}

fn vendor_name(id: u32) -> &'static str {
    match id {
        0x10DE => "NVIDIA",
        0x8086 => "Intel",
        0x1002 => "AMD",
        _ => "Other",
    }
}

/// The runtime directory the boot-time values are kept in: per-user, and gone at logout, which at
/// worst records them again from a card nobody has tuned yet. Under sudo the variable is gone, so
/// the invoking user's directory is found by number.
fn runtime_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return Some(PathBuf::from(dir));
    }
    let uid = std::env::var("SUDO_UID").or_else(|_| std::env::var("PKEXEC_UID")).ok()?;
    let dir = PathBuf::from(format!("/run/user/{}", uid.trim()));
    dir.is_dir().then_some(dir)
}

/// The percentages a power cap may move through, as a +/- offset around the default. Rounded inward,
/// so the ends of the slider are always caps the driver accepts.
fn power_window(min_uw: i64, max_uw: i64, default_uw: i64) -> Option<Range> {
    if default_uw <= 0 || max_uw <= min_uw {
        return None;
    }
    let percent = |uw: i64| (uw - default_uw) as f64 / default_uw as f64 * 100.0;
    Some(Range { min: percent(min_uw).ceil() as i32, max: percent(max_uw).floor() as i32 })
}

/// The cap for a percentage, in the whole watts the driver keeps (it divides by a million and drops
/// the rest), and inside its window.
fn power_cap_uw(percent: i32, min_uw: i64, max_uw: i64, default_uw: i64) -> i64 {
    let watts = (default_uw as f64 * (100.0 + f64::from(percent)) / 100.0 / 1e6).round() as i64;
    (watts * 1_000_000).clamp(min_uw, max_uw)
}

/// The reading if it is believable for its unit, otherwise NaN. A failed read is already NaN.
fn believable(value: f64, unit: &str) -> f64 {
    let out_of_range =
        !value.is_finite() || value < 0.0 || (unit == "°C" && value > 256.0) || (unit == "%" && value > 100.0);
    if out_of_range { f64::NAN } else { value }
}

/// The window a memory clock limit may be set in: the driver's, without the part under the card's stock
/// clock. See `set_memory_offset`.
fn memory_window(driver: Range, stock: i32) -> Range {
    Range { min: driver.min.max(stock).min(driver.max), max: driver.max }
}

/// `0000:04:00.0`.
fn is_pci_address(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 12
        && bytes.iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b':',
            10 => *b == b'.',
            _ => b.is_ascii_hexdigit(),
        })
}

/// The PCIe generation and width of the link the card really has, as (generation, lanes), 0 where
/// unknown. A card's own attributes will not say: a discrete AMD GPU sits behind a PCIe switch on its
/// own board, and the GPU's link is only the one to that switch, which reads x16 even in a slot wired
/// for x8. So this takes the narrowest and slowest hop on the way up to the CPU.
///
/// The speed comes from what each hop is capable of, because a card drops its link speed to save power
/// and the reading should not change while it idles. The width comes from what each hop negotiated,
/// because that is what shows a slot running narrower than either end can go.
fn pcie_link(device: &Path) -> (i32, i32) {
    let Ok(device) = fs::canonicalize(device) else { return (0, 0) };
    let (mut generation, mut width) = (0, 0);
    for hop in device.ancestors().take_while(|p| p.file_name().is_some_and(|n| is_pci_address(&n.to_string_lossy()))) {
        if let Some(g) = read_text(&hop.join("max_link_speed")).and_then(|s| gpu_metrics::generation_of_link_speed(&s))
        {
            generation = if generation == 0 { g } else { generation.min(g) };
        }
        // A hop with no real link prints 0 or an out-of-range width.
        let lanes = read_i64(&hop.join("current_link_width"))
            .or_else(|| read_i64(&hop.join("max_link_width")))
            .filter(|lanes| (1..=32).contains(lanes));
        if let Some(lanes) = lanes {
            width = if width == 0 { lanes as i32 } else { width.min(lanes as i32) };
        }
    }
    (generation, width)
}

/// "samsung" → "Samsung". The driver prints the vendor in lower case.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}

impl AmdgpuBackend {
    pub fn new() -> Self {
        Self::with_paths(PathBuf::from(DRM_ROOT), PathBuf::from(MODULE_PARAMETERS), Box::new(KernelWriter))
            .keeping_stock_in(runtime_dir())
            .reading_names_from(identity::pci_ids_paths())
    }

    fn with_paths(root: PathBuf, module_parameters: PathBuf, writer: Box<dyn AttrWriter>) -> Self {
        Self {
            root,
            module_parameters,
            stock_dir: None,
            pci_ids: Vec::new(),
            writer,
            cards: Vec::new(),
            devices: Vec::new(),
            stock: HashMap::new(),
        }
    }

    fn keeping_stock_in(mut self, dir: Option<PathBuf>) -> Self {
        self.stock_dir = dir;
        self
    }

    fn reading_names_from(mut self, paths: Vec<PathBuf>) -> Self {
        self.pci_ids = paths;
        self
    }

    /// What the card is, from what the driver answers from memory. This is the complete list of
    /// attributes read from a card that may be asleep; each was checked against a card stuck
    /// mid-resume. Anything else goes behind [`Self::is_awake`].
    fn describe_card(dir: &Path, name: String, pci: String, pci_ids: &str) -> Card {
        let device_id = read_hex(&dir.join("device")).unwrap_or(0);
        let code_name = identity::code_name_in(pci_ids, device_id);
        let hwmon = fs::read_dir(dir.join("hwmon"))
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with("hwmon"))
            .map(|e| e.path());
        Card {
            dir: dir.to_path_buf(),
            name,
            pci,
            hwmon,
            device_id,
            revision: read_hex(&dir.join("revision")).unwrap_or(0),
            subsystem_vendor: read_hex(&dir.join("subsystem_vendor")).unwrap_or(0),
            boot_vga: read_text(&dir.join("boot_vga")).as_deref() == Some("1"),
            memory_scale: identity::memory_clock_scale(&code_name),
            code_name,
        }
    }

    /// Find the cards. Returns the ones this backend does not drive, for the message when none is
    /// left: "no GPU" would be a poor answer on a machine with an RTX in it.
    fn discover(&mut self) -> Vec<String> {
        let mut others = Vec::new();
        let mut cards = Vec::new();
        let pci_ids = self.pci_ids.iter().find_map(|p| fs::read_to_string(p).ok()).unwrap_or_default();
        for entry in fs::read_dir(&self.root).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_card_name(&name) {
                continue;
            }
            let dir = entry.path().join("device");
            let Some(pci) = link_name(&dir) else { continue };
            let driver = link_name(&dir.join("driver")).unwrap_or_default();
            if driver == "amdgpu" {
                cards.push(Self::describe_card(&dir, name, pci, &pci_ids));
            } else if let Some(vendor) = read_hex(&dir.join("vendor")) {
                others.push(format!("{} GPU at {pci} (driver {driver})", vendor_name(vendor)));
            }
        }
        // The card driving the desktop comes first, so `--gpu 0` and a first launch mean the card a
        // person is looking at. Then by PCI address, which unlike cardN does not move between boots.
        cards.sort_by(|a, b| b.boot_vga.cmp(&a.boot_vga).then_with(|| a.pci.cmp(&b.pci)));
        self.cards = cards;
        others
    }

    fn card(&self, gpu: usize) -> Result<&Card> {
        self.cards.get(gpu).ok_or_else(|| GpuError::new(format!("No AMD GPU at index {gpu}.")))
    }

    /// May this card be asked anything beyond [`Self::describe_card`]? Reading the status is itself
    /// safe. A kernel without runtime power management reports no status, or "unsupported".
    fn is_awake(&self, gpu: usize) -> bool {
        let Ok(card) = self.card(gpu) else { return false };
        match read_text(&card.attr("power/runtime_status")).as_deref() {
            None | Some("active" | "unsupported") => true,
            Some(_) => false,
        }
    }

    fn require_awake(&self, gpu: usize) -> Result<()> {
        if self.is_awake(gpu) {
            return Ok(());
        }
        let card = self.card(gpu)?;
        bail!("GPU {gpu} ({}) is asleep. A write would have to wake it, so nothing was changed.", card.pci)
    }

    fn write(&mut self, path: &Path, value: &str) -> Result<()> {
        self.writer.write(path, value).map_err(|e| sysfs::write_error(path, value, &e))
    }

    /// The overdrive table, when this card has one and is awake to print it.
    fn read_od(&self, gpu: usize) -> Option<OdTable> {
        if !self.is_awake(gpu) {
            return None;
        }
        Some(OdTable::parse(&read_text(&self.card(gpu).ok()?.attr(OD_FILE))?))
    }

    fn read_fan_curve(&self, gpu: usize) -> Option<FanCurveTable> {
        if !self.is_awake(gpu) {
            return None;
        }
        Some(FanCurveTable::parse(&read_text(&self.card(gpu).ok()?.attr(FAN_CURVE_FILE))?))
    }

    /// The zero-RPM switch's state and whether it can move.
    fn read_zero_rpm(&self, gpu: usize) -> Option<(bool, bool)> {
        if !self.is_awake(gpu) {
            return None;
        }
        let text = read_text(&self.card(gpu).ok()?.attr(ZERO_RPM_FILE))?;
        let (value, range) = od::parse_single(&text);
        Some((value? != 0, range.is_some()))
    }

    /// `ip_discovery` exists from Navi 1x on, so a card without it is an older one.
    fn graphics_core_major(&self, gpu: usize) -> Option<u32> {
        read_text(&self.card(gpu).ok()?.attr("ip_discovery/die/0/GC/0/major"))?.parse().ok()
    }

    fn fan_control(&self, gpu: usize) -> FanControl {
        if !self.is_awake(gpu) {
            return FanControl::None;
        }
        if let Some(table) = self.read_fan_curve(gpu)
            && table.is_writable()
        {
            return FanControl::Curve(table);
        }
        if self.graphics_core_major(gpu).is_some_and(|major| major >= FIRMWARE_FANS_FROM_GC) {
            return FanControl::None;
        }
        let Ok(card) = self.card(gpu) else { return FanControl::None };
        // hwmon strips the write bit from a fan it cannot manage.
        let manageable = |name: &str| {
            card.hw(name)
                .and_then(|p| fs::metadata(p).ok())
                .is_some_and(|m| std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o200 != 0)
        };
        if manageable("pwm1") && manageable("pwm1_enable") { FanControl::Pwm } else { FanControl::None }
    }

    /// The window and default for the power cap, in microwatts.
    fn power_limits(&self, gpu: usize) -> Option<(i64, i64, i64)> {
        if !self.is_awake(gpu) {
            return None;
        }
        let card = self.card(gpu).ok()?;
        Some((card.hw_i64("power1_cap_min")?, card.hw_i64("power1_cap_max")?, card.hw_i64("power1_cap_default")?))
    }

    fn stock_path(&self, pci: &str) -> Option<PathBuf> {
        let boot = read_text(Path::new("/proc/sys/kernel/random/boot_id"))?;
        let boot: String = boot.chars().filter(char::is_ascii_hexdigit).take(16).collect();
        Some(self.stock_dir.as_ref()?.join(format!("roch-gpu-stock-{pci}-{boot}")))
    }

    /// See [`OdStock`]. `od` is this card's table as it reads now, which is the record if none exists.
    ///
    /// The driver prints a blank table when the firmware has nothing to say. So a record missing a value
    /// this table has came from one of those, and does not stand: a stock of 0 would take the floor off
    /// the memory clock for the rest of the boot.
    fn od_stock(&mut self, gpu: usize, od: &OdTable) -> OdStock {
        let complete =
            |s: &OdStock| (od.mclk_max.is_none() || s.mclk_max > 0) && (od.sclk_max.is_none() || s.sclk_max > 0);
        if let Some(stock) = self.stock.get(&gpu).filter(|s| complete(s)) {
            return *stock;
        }
        let path = self.card(gpu).ok().and_then(|c| self.stock_path(&c.pci.clone()));
        let saved = path.as_ref().and_then(|p| read_text(p)).and_then(|text| {
            let mut values = text.split_whitespace().map(|v| v.parse::<i32>().ok());
            Some(OdStock { sclk_min: values.next()??, sclk_max: values.next()??, mclk_max: values.next()?? })
        });
        let saved = saved.filter(|s| complete(s));
        let stock = saved.unwrap_or_else(|| {
            let stock = OdStock {
                sclk_min: od.sclk_min.unwrap_or(0),
                sclk_max: od.sclk_max.unwrap_or(0),
                mclk_max: od.mclk_max.unwrap_or(0),
            };
            if let Some(p) = path.as_ref().filter(|_| od.mclk_max.is_some() || od.sclk_max.is_some()) {
                // Best effort: without it the record lasts as long as this process.
                let _ = fs::write(p, format!("{} {} {}\n", stock.sclk_min, stock.sclk_max, stock.mclk_max));
            }
            stock
        });
        self.stock.insert(gpu, stock);
        stock
    }

    /// One edit to the overdrive table, then the commit that sends it to the card. The driver only
    /// insists on the `manual` performance level on some APUs, and there it answers EINVAL. So a
    /// refusal is retried once in manual before being believed, rather than leaving every card in
    /// manual for the sake of those.
    fn od_write(&mut self, gpu: usize, command: &str) -> Result<()> {
        self.require_awake(gpu)?;
        let card = self.card(gpu)?;
        let (path, level_path) = (card.attr(OD_FILE), card.attr(PERFORMANCE_LEVEL_FILE));
        let line = format!("{command}\n");
        if let Err(first) = self.write(&path, &line) {
            let level = read_text(&level_path);
            let retry = first.message.contains("refused") && level.as_deref().is_some_and(|l| l != "manual");
            if !retry {
                return Err(first);
            }
            self.write(&level_path, "manual\n")?;
            if self.write(&path, &line).is_err() {
                let _ = self.write(&level_path, &format!("{}\n", level.unwrap_or_else(|| "auto".into())));
                return Err(first);
            }
        }
        self.write(&path, "c\n")
    }

    fn write_fan_curve(&mut self, gpu: usize, table: &FanCurveTable, speed_at: impl Fn(f64) -> f64) -> Result<()> {
        let (Some(temp), Some(speed)) = (table.temp_range, table.speed_range) else {
            bail!("This card does not allow changing its fan curve.");
        };
        let path = self.card(gpu)?.attr(FAN_CURVE_FILE);
        for (i, t, s) in od::plan_fan_curve(speed_at, table.points.len(), temp, speed) {
            self.write(&path, &format!("{i} {t} {s}\n"))?;
        }
        self.write(&path, "c\n")
    }

    fn read_metrics(&self, card: &Card) -> Option<GpuMetrics> {
        GpuMetrics::parse(&fs::read(card.attr("gpu_metrics")).ok()?)
    }

    fn describe_diagnostics(&self, gpu: usize, sb: &mut String) -> Result<()> {
        let card = self.card(gpu)?;
        let device = self.devices.get(gpu).cloned().unwrap_or_default();
        let _ = writeln!(sb, "GPU  : {}   ({}, {})", device.name, card.pci, card.name);
        let _ = writeln!(
            sb,
            "PCI  : device {:04X} revision {:02X} board vendor {:04X}, boot VGA: {}",
            card.device_id, card.revision, card.subsystem_vendor, card.boot_vga
        );
        let status = |n: &str| read_text(&card.attr(n)).unwrap_or_else(|| "n/a".into());
        let _ = writeln!(sb, "Power: {} ({})", status("power/runtime_status"), status("power_state"));
        let mask = read_text(&self.module_parameters.join("ppfeaturemask")).unwrap_or_else(|| "n/a".into());
        let _ = writeln!(sb, "amdgpu: ppfeaturemask {mask}, runpm {}", {
            read_text(&self.module_parameters.join("runpm")).unwrap_or_else(|| "n/a".into())
        });
        if !self.is_awake(gpu) {
            sb.push_str("\nThe card is asleep, so nothing else was read from it.\n");
            return Ok(());
        }
        let _ = writeln!(sb, "Graphics core: {:?}", self.graphics_core_major(gpu));
        let _ = writeln!(sb, "Performance level: {}", status(PERFORMANCE_LEVEL_FILE));
        for file in [OD_FILE, FAN_CURVE_FILE, ZERO_RPM_FILE] {
            let _ = writeln!(sb, "\n--- {file} ---");
            let text = read_text(&card.attr(file)).unwrap_or_else(|| "(not offered)".into());
            let _ = writeln!(sb, "{text}");
        }
        sb.push_str("\n--- hwmon ---\n");
        if let Some(dir) = &card.hwmon {
            let mut names: Vec<String> = fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into())
                .collect();
            names.sort();
            for name in names.iter().filter(|n| *n != "uevent") {
                let path = dir.join(name);
                if path.is_file() {
                    let _ = writeln!(sb, "  {name} = {}", read_text(&path).unwrap_or_else(|| "(refused)".into()));
                }
            }
        }
        let _ = writeln!(sb, "\n--- gpu_metrics ---\n{:?}", self.read_metrics(card));
        Ok(())
    }
}

impl GpuBackend for AmdgpuBackend {
    fn backend_name(&self) -> String {
        "AMD (Linux amdgpu)".into()
    }

    fn initialize(&mut self) -> Result<()> {
        // Idempotent: the factory initialises while probing, then the tuning service does again.
        if !self.cards.is_empty() {
            return Ok(());
        }
        let others = self.discover();
        if self.cards.is_empty() {
            let found = if others.is_empty() {
                String::new()
            } else {
                format!(" Found {}; on Linux this build only drives AMD cards so far.", others.join(", "))
            };
            bail!("No GPU using the amdgpu kernel driver was found.{found}");
        }
        let driver =
            read_text(Path::new("/proc/sys/kernel/osrelease")).map_or_else(String::new, |r| format!("Linux {r}"));
        self.devices = self
            .cards
            .iter()
            .enumerate()
            .map(|(index, card)| GpuDevice {
                index,
                name: identity::product_name(card.device_id, card.revision),
                vendor: "AMD".into(),
                bus_id: card.pci.clone(),
                driver_version: driver.clone(),
                vram_megabytes: read_i64(&card.attr("mem_info_vram_total")).map_or(0, |b| b / (1024 * 1024)),
                bios_version: read_text(&card.attr("vbios_version")).unwrap_or_default(),
            })
            .collect();
        Ok(())
    }

    fn devices(&self) -> &[GpuDevice] {
        &self.devices
    }

    fn read_graphics_info(&mut self, gpu: usize) -> GpuGraphicsInfo {
        let (Ok(card), Some(device)) = (self.card(gpu), self.devices.get(gpu)) else {
            return GpuGraphicsInfo::default();
        };
        let base = GpuGraphicsInfo {
            board_manufacturer: GpuIdentity::board_vendor(card.subsystem_vendor as i32),
            code_name: card.code_name.clone(),
            revision: format!("{:02X}", card.revision),
            ..GpuGraphicsInfo::from_device(device)
        };
        if !self.is_awake(gpu) {
            return base;
        }
        // The link attributes read PCI config space, which waits for a sleeping card.
        let metrics = self.read_metrics(card).unwrap_or_default();
        let (generation, lanes) = pcie_link(&card.dir);
        GpuGraphicsInfo {
            memory_type: identity::memory_type_of(&base.code_name).to_string(),
            memory_vendor: read_text(&card.attr("mem_info_vram_vendor")).map(|v| capitalized(&v)).unwrap_or_default(),
            bus_interface: GpuIdentity::pcie_bus_interface(
                generation,
                lanes,
                metrics.pcie_generation().unwrap_or(0),
                metrics.pcie_link_width.map_or(0, i32::from),
            ),
            ..base
        }
    }

    fn capabilities(&mut self, gpu: usize) -> Result<GpuCapabilities> {
        self.card(gpu)?;
        // Nothing to clamp against until a control below says otherwise: a window left at its default
        // would read as a real one in the profile clamps.
        let mut c = GpuCapabilities {
            can_read_voltage: true,
            memory_clock_is_absolute: true,
            power_limit_is_offset: true,
            voltage_style: VoltageControlStyle::None,
            core_offset_min_mhz: 0,
            core_offset_max_mhz: 0,
            memory_offset_min_mhz: 0,
            memory_offset_max_mhz: 0,
            power_limit_min_percent: 0,
            power_limit_max_percent: 0,
            power_limit_default_percent: 0,
            temp_limit_min_c: 0,
            temp_limit_max_c: 0,
            temp_limit_default_c: 0,
            voltage_offset_min_mv: 0,
            voltage_offset_max_mv: 0,
            voltage_boost_max_percent: 0,
            curve_unavailable_reason: "amdgpu exposes a voltage offset rather than an editable V/F curve".into(),
            ..GpuCapabilities::default()
        };
        if !self.is_awake(gpu) {
            return Ok(c);
        }

        if let Some(od) = self.read_od(gpu) {
            let stock = self.od_stock(gpu, &od);
            if od.core_is_absolute()
                && let Some(range) = od.sclk_range
            {
                c.can_set_core_offset = true;
                c.core_clock_is_absolute = true;
                c.core_offset_min_mhz = range.min;
                c.core_offset_max_mhz = range.max;
                c.core_clock_default_mhz = stock.sclk_max;
                if od.can_set_core_min() {
                    c.can_set_core_min_clock = true;
                    c.core_min_clock_min_mhz = range.min;
                    c.core_min_clock_max_mhz = range.max;
                    c.core_min_clock_default_mhz = stock.sclk_min;
                }
            } else if od.core_is_offset()
                && let Some(range) = od.sclk_offset_range
            {
                c.can_set_core_offset = true;
                c.core_offset_min_mhz = range.min;
                c.core_offset_max_mhz = range.max;
            }
            if od.can_set_memory()
                && let Some(range) = od.mclk_range
            {
                let window = memory_window(range, stock.mclk_max);
                let scale = self.card(gpu)?.memory_scale;
                c.can_set_memory_offset = true;
                c.memory_offset_min_mhz = window.min * scale;
                c.memory_offset_max_mhz = window.max * scale;
                c.memory_clock_default_mhz = stock.mclk_max * scale;
            }
            if od.can_set_voltage_offset()
                && let Some(range) = od.voltage_offset_range
            {
                c.voltage_style = VoltageControlStyle::Offset;
                c.voltage_offset_min_mv = range.min;
                c.voltage_offset_max_mv = range.max;
            }
        }

        if let Some((min, max, default)) = self.power_limits(gpu)
            && let Some(window) = power_window(min, max, default)
            && self.card(gpu)?.hw("power1_cap").is_some_and(|p| p.exists())
        {
            c.can_set_power_limit = true;
            c.power_limit_min_percent = window.min;
            c.power_limit_max_percent = window.max;
        }

        match self.fan_control(gpu) {
            FanControl::Curve(table) => {
                c.can_set_fan_speed = true;
                c.can_read_hardware_fan_curve = true;
                c.fan_curve_is_hardware = true;
                c.fan_curve_points = table.points.len() as i32;
                c.fan_count = 1;
                if let (Some(temp), Some(speed)) = (table.temp_range, table.speed_range) {
                    c.fan_curve_min_temp_c = temp.min;
                    c.fan_curve_max_temp_c = temp.max;
                    c.fan_min_percent = speed.min;
                    c.fan_max_percent = speed.max;
                }
            }
            FanControl::Pwm => {
                c.can_set_fan_speed = true;
                c.fan_count = 1;
            }
            FanControl::None => {}
        }
        if let Some((_, movable)) = self.read_zero_rpm(gpu) {
            c.can_set_zero_rpm = movable;
        }
        Ok(c)
    }

    fn read_telemetry(&mut self, gpu: usize) -> Result<GpuTelemetry> {
        let nan = f64::NAN;
        let asleep = GpuTelemetry {
            core_clock_mhz: nan,
            memory_clock_mhz: nan,
            temperature_c: nan,
            hot_spot_c: nan,
            memory_temperature_c: nan,
            voltage_mv: nan,
            power_percent: nan,
            power_watts: nan,
            gpu_load_percent: nan,
            memory_load_percent: nan,
            memory_used_mb: nan,
            fan_percent: nan,
            fan_rpm: nan,
            limit_reason: "Asleep".into(),
            ..GpuTelemetry::default()
        };
        self.card(gpu)?;
        if !self.is_awake(gpu) {
            return Ok(asleep);
        }
        let card = self.card(gpu)?;

        let (mut edge, mut junction, mut memory) = (nan, nan, nan);
        // No limit readable means no way to judge, and then the firmware's word stands.
        let (mut near_a_limit, mut limits_known) = (false, false);
        for i in 1..=8 {
            let Some(label) = card.hw(&format!("temp{i}_label")).and_then(|p| read_text(&p)) else { break };
            let value = card.hw_scaled(&format!("temp{i}_input"), 1000.0);
            let limit = card.hw_scaled(&format!("temp{i}_crit"), 1000.0);
            limits_known |= limit.is_finite();
            near_a_limit |= value >= limit - NEAR_THERMAL_LIMIT_C;
            match label.as_str() {
                "edge" => edge = value,
                "junction" => junction = value,
                "mem" => memory = value,
                _ => {}
            }
        }
        if edge.is_nan() {
            edge = card.hw_scaled("temp1_input", 1000.0);
        }

        let watts = {
            let average = card.hw_scaled("power1_average", 1e6);
            if average.is_nan() { card.hw_scaled("power1_input", 1e6) } else { average }
        };
        let cap_watts = card.hw_scaled("power1_cap", 1e6);
        let default_watts = card.hw_scaled("power1_cap_default", 1e6);
        let duty = card.hw_scaled("pwm1", 255.0 / 100.0);
        let rpm = card.hw_scaled("fan1_input", 1.0);
        let dpm_clock = |file: &str| read_text(&card.attr(file)).and_then(|t| od::current_level_mhz(&t));

        let metrics = self.read_metrics(card);
        let m = metrics.clone().unwrap_or_default();
        let mut extra = Vec::new();
        let mut sensor = |key: &str, name: &str, unit: &str, value: Option<f64>| {
            if let Some(v) = value.map(|v| believable(v, unit)).filter(|v| v.is_finite()) {
                extra.push(SupplementalSensor::new(format!("amdgpu:{key}"), name, unit, v));
            }
        };
        let from = |v: Option<u16>| v.map(f64::from);
        sensor("vr_gfx", "Core VRM", "°C", from(m.vr_gfx_c));
        sensor("vr_mem", "Memory VRM", "°C", from(m.vr_mem_c));
        sensor("vr_soc", "SoC VRM", "°C", from(m.vr_soc_c));
        sensor("v_soc", "SoC voltage", "mV", from(m.voltage_soc_mv));
        sensor("v_mem", "Memory voltage", "mV", from(m.voltage_mem_mv));
        sensor("vclk0", "VCN clock 1", "MHz", from(m.vclk0_mhz));
        sensor("dclk0", "VCN decode clock 1", "MHz", from(m.dclk0_mhz));
        sensor("vclk1", "VCN clock 2", "MHz", from(m.vclk1_mhz));
        sensor("dclk1", "VCN decode clock 2", "MHz", from(m.dclk1_mhz));
        sensor("power_cap", "dGPU power limit", "W", Some(cap_watts));
        sensor("pcie_gen", "PCIe link generation", "gen", m.pcie_generation().map(f64::from));
        sensor("pcie_width", "PCIe link width", "lanes", from(m.pcie_link_width));
        sensor(
            "gtt",
            "Shared GPU memory used",
            "MB",
            read_i64(&card.attr("mem_info_gtt_used")).map(|b| b as f64 / (1024.0 * 1024.0)),
        );

        Ok(GpuTelemetry {
            core_clock_mhz: believable(card.hw_scaled("freq1_input", 1e6), "MHz"),
            memory_clock_mhz: believable(card.hw_scaled("freq2_input", 1e6) * f64::from(card.memory_scale), "MHz"),
            fabric_clock_mhz: dpm_clock("pp_dpm_fclk").map_or(nan, f64::from),
            soc_clock_mhz: m
                .soc_clock_mhz
                .map(f64::from)
                .or_else(|| dpm_clock("pp_dpm_socclk").map(f64::from))
                .unwrap_or(nan),
            temperature_c: believable(edge, "°C"),
            hot_spot_c: believable(junction, "°C"),
            memory_temperature_c: believable(memory, "°C"),
            voltage_mv: believable(card.hw_scaled("in0_input", 1.0), "mV"),
            power_watts: believable(watts, "W"),
            // Against the default cap, so it reads as the % of TDP the column is named for.
            power_percent: if default_watts > 0.0 { believable(watts, "W") / default_watts * 100.0 } else { nan },
            gpu_load_percent: read_i64(&card.attr("gpu_busy_percent")).map_or(nan, |v| believable(v as f64, "%")),
            memory_load_percent: read_i64(&card.attr("mem_busy_percent")).map_or(nan, |v| believable(v as f64, "%")),
            memory_used_mb: read_i64(&card.attr("mem_info_vram_used")).map_or(nan, |b| b as f64 / (1024.0 * 1024.0)),
            fan_percent: believable(duty, "%"),
            fan_rpm: believable(rpm, "RPM"),
            fan_percents: vec![believable(duty, "%")],
            fan_rpms: vec![believable(rpm, "RPM")],
            supplemental_sensors: extra,
            limit_reason: m.throttle.filter(|_| metrics.is_some()).map_or_else(
                || "Unavailable".into(),
                |status| {
                    // No limit to compare against means no way to judge: the firmware's word stands.
                    let near_temperature = near_a_limit || !limits_known;
                    let cap_known = cap_watts > 0.0;
                    let near_power = !cap_known || watts >= cap_watts * NEAR_POWER_LIMIT;
                    gpu_metrics::throttle_text(gpu_metrics::plausible_limits(status, near_temperature, near_power))
                },
            ),
            ..GpuTelemetry::default()
        })
    }

    /// Never fails, as on Windows: a tick that exists to step a fan curve gets an empty sample. Its
    /// temperature is NaN, which the curve skips, because 0 °C would drop the fan to its lowest duty.
    fn read_temperature_only(&mut self, gpu: usize) -> Result<GpuTelemetry> {
        let edge = if self.is_awake(gpu) {
            self.card(gpu).ok().and_then(|c| c.hw_i64("temp1_input")).map(|v| v as f64 / 1000.0)
        } else {
            None
        };
        Ok(GpuTelemetry { temperature_c: edge.unwrap_or(f64::NAN), ..GpuTelemetry::default() })
    }

    fn read_tuning_state(&mut self, gpu: usize) -> Result<GpuTuningState> {
        let card = self.card(gpu)?;
        let mut state = GpuTuningState { power_limit_percent: 0, temp_limit_c: 0, ..GpuTuningState::default() };
        if !self.is_awake(gpu) {
            return Ok(state);
        }
        if let Some(cap) = card.hw_i64("power1_cap")
            && let Some((_, _, default)) = self.power_limits(gpu)
            && default > 0
        {
            state.power_limit_percent = ((cap - default) as f64 / default as f64 * 100.0).round() as i32;
        }
        if let Some(od) = self.read_od(gpu) {
            state.core_offset_mhz = od.sclk_max.or(od.sclk_offset).unwrap_or(0);
            state.core_min_clock_mhz = od.sclk_min.unwrap_or(0);
            state.memory_offset_mhz = od.mclk_max.unwrap_or(0) * card.memory_scale;
            state.voltage_offset_mv = od.voltage_offset.unwrap_or(0);
        }
        if let Some((zero_rpm, _)) = self.read_zero_rpm(gpu) {
            state.zero_rpm = zero_rpm;
        }
        match self.fan_control(gpu) {
            FanControl::Curve(table) => match table.mode() {
                FanTableMode::Auto => state.detected_fan_mode = Some(FanMode::Auto),
                FanTableMode::Fixed(percent) => {
                    state.detected_fan_mode = Some(FanMode::Fixed);
                    state.fan_manual = true;
                    state.fan_percent = percent;
                }
                FanTableMode::Curve => {
                    state.detected_fan_mode = Some(FanMode::Curve);
                    let points = table.points.iter().map(|&(t, s)| FanPoint::new(f64::from(t), f64::from(s)));
                    state.hardware_fan_curve = Some(FanCurve::with_points(points.collect()));
                }
            },
            FanControl::Pwm => {
                let card = self.card(gpu)?;
                state.fan_manual = card.hw_i64("pwm1_enable") == Some(1);
                if state.fan_manual {
                    state.fan_percent = (card.hw_scaled("pwm1", 255.0 / 100.0)).round() as i32;
                }
            }
            FanControl::None => {}
        }
        Ok(state)
    }

    /// The maximum core clock on RDNA 2 and 3, an offset on RDNA 4: whichever the card prints.
    fn set_core_offset(&mut self, gpu: usize, mhz: i32) -> Result<()> {
        self.require_awake(gpu)?;
        let Some(od) = self.read_od(gpu) else { bail!("This card does not expose a core clock control.") };
        // A value already on the card is not written. An Apply covers every control, and only what
        // changed should reach the firmware.
        if od.core_is_absolute()
            && let Some(range) = od.sclk_range
        {
            let target = range.clamp(mhz);
            return if od.sclk_max == Some(target) { Ok(()) } else { self.od_write(gpu, &format!("s 1 {target}")) };
        }
        if od.core_is_offset()
            && let Some(range) = od.sclk_offset_range
        {
            let target = range.clamp(mhz);
            return if od.sclk_offset == Some(target) { Ok(()) } else { self.od_write(gpu, &format!("s {target}")) };
        }
        bail!("This card does not expose a core clock control.");
    }

    fn set_core_min_clock(&mut self, gpu: usize, mhz: i32) -> Result<()> {
        self.require_awake(gpu)?;
        let od = self.read_od(gpu).unwrap_or_default();
        let (true, Some(range)) = (od.can_set_core_min(), od.sclk_range) else {
            bail!("This card does not expose a minimum core clock.");
        };
        let target = range.clamp(mhz);
        if od.sclk_min == Some(target) {
            return Ok(());
        }
        self.od_write(gpu, &format!("s 0 {target}"))
    }

    /// The memory clock's upper limit, in the units and range the Windows driver gives it: 2500 to 3000
    /// MHz on an RX 7900 XT, default 2500. This driver counts the memory controller's clock, half that
    /// on GDDR6, and would also take anything down to 97 MHz.
    ///
    /// The range stops at stock for a reason beyond matching Windows. Capping an RX 7900 XT at 800 MHz
    /// here (1250 stock) while it ran a compute job hung its power management firmware and the machine
    /// with it: "SMU: No response" in the kernel log, then nothing, and a hard reset. Lowering it gives up
    /// nothing, since the card already drops its memory clock when idle.
    ///
    /// Unlike the other controls here, it is written even when the card already has the value, because
    /// the Windows build sends it on every Apply. That has not been tried on a busy card.
    fn set_memory_offset(&mut self, gpu: usize, mhz: i32) -> Result<()> {
        self.require_awake(gpu)?;
        let od = self.read_od(gpu).unwrap_or_default();
        let (true, Some(range)) = (od.can_set_memory(), od.mclk_range) else {
            bail!("This card does not expose a memory clock control.");
        };
        let stock = self.od_stock(gpu, &od).mclk_max;
        let scale = self.card(gpu)?.memory_scale;
        let target = memory_window(range, stock).clamp(mhz.saturating_add(scale / 2) / scale);
        self.od_write(gpu, &format!("m 1 {target}"))
    }

    fn set_power_limit(&mut self, gpu: usize, percent: i32) -> Result<()> {
        self.require_awake(gpu)?;
        let Some((min, max, default)) = self.power_limits(gpu) else {
            bail!("This card does not expose a power limit.");
        };
        let Some(path) = self.card(gpu)?.hw("power1_cap") else {
            bail!("This card does not expose a power limit.");
        };
        let target = power_cap_uw(percent, min, max, default);
        if read_i64(&path) == Some(target) {
            return Ok(());
        }
        self.write(&path, &format!("{target}\n"))
    }

    /// amdgpu's thermal limits are read-only.
    fn set_temp_limit(&mut self, _gpu: usize, _celsius: i32) -> Result<()> {
        bail!("The amdgpu driver keeps the temperature limit to itself.");
    }

    fn set_voltage_boost(&mut self, _gpu: usize, percent: i32) -> Result<()> {
        if percent != 0 {
            bail!("AMD cards have no voltage boost — use the voltage offset.");
        }
        Ok(())
    }

    fn set_voltage_curve_offset(&mut self, gpu: usize, offset_mv: i32, _extra_clock_mhz: i32) -> Result<()> {
        let od = self.read_od(gpu).unwrap_or_default();
        let (true, Some(range)) = (od.can_set_voltage_offset(), od.voltage_offset_range) else {
            if offset_mv == 0 {
                return Ok(());
            }
            bail!("This card does not expose a voltage offset.");
        };
        let target = clamp_i32(offset_mv, range.min, range.max);
        if od.voltage_offset == Some(target) {
            return Ok(());
        }
        self.od_write(gpu, &format!("vo {target}"))
    }

    fn set_zero_rpm(&mut self, gpu: usize, enabled: bool) -> Result<()> {
        self.require_awake(gpu)?;
        let Some((on, true)) = self.read_zero_rpm(gpu) else {
            bail!("This card does not expose zero RPM control.");
        };
        if on == enabled {
            return Ok(());
        }
        let path = self.card(gpu)?.attr(ZERO_RPM_FILE);
        self.write(&path, if enabled { "1\n" } else { "0\n" })?;
        self.write(&path, "c\n")
    }

    /// One duty. On the firmware's curve that is the same duty at every point. As on Windows, zero RPM is
    /// left to its own control, so with it on the firmware keeps the fan stopped below its stop temperature
    /// whatever the curve says.
    fn set_fan_speed(&mut self, gpu: usize, _fan_index: i32, percent: i32) -> Result<()> {
        self.require_awake(gpu)?;
        match self.fan_control(gpu) {
            FanControl::Curve(table) => {
                let Some(speed) = table.speed_range else { bail!("This card does not allow changing its fan curve.") };
                let duty = f64::from(speed.clamp(percent));
                self.write_fan_curve(gpu, &table, |_| duty)
            }
            FanControl::Pwm => {
                let card = self.card(gpu)?;
                let (Some(enable), Some(pwm)) = (card.hw("pwm1_enable"), card.hw("pwm1")) else {
                    bail!("This card does not expose fan control.");
                };
                // The driver refuses a duty until the fan is in manual.
                self.write(&enable, "1\n")?;
                let duty = (clamp_i32(percent, 0, 100) * 255 + 50) / 100;
                self.write(&pwm, &format!("{duty}\n"))
            }
            FanControl::None => bail!("This card does not expose fan control."),
        }
    }

    /// Five points for the firmware to run, which keep working after this process exits.
    fn set_fan_curve(&mut self, gpu: usize, curve: &FanCurve) -> Result<()> {
        self.require_awake(gpu)?;
        let FanControl::Curve(table) = self.fan_control(gpu) else {
            bail!("This card has no hardware fan curve.");
        };
        self.write_fan_curve(gpu, &table, |t| curve.evaluate(t))
    }

    fn set_fan_auto(&mut self, gpu: usize) -> Result<()> {
        self.require_awake(gpu)?;
        match self.fan_control(gpu) {
            FanControl::Curve(table) if table.mode() == FanTableMode::Auto => Ok(()),
            FanControl::Curve(_) => {
                // The driver commits a restore itself on current firmware; the second write is for the
                // ones that wait to be told.
                let path = self.card(gpu)?.attr(FAN_CURVE_FILE);
                self.write(&path, "r\n")?;
                self.write(&path, "c\n")
            }
            FanControl::Pwm => {
                let Some(enable) = self.card(gpu)?.hw("pwm1_enable") else { return Ok(()) };
                if read_i64(&enable) == Some(2) {
                    return Ok(());
                }
                self.write(&enable, "2\n")
            }
            FanControl::None => Ok(()),
        }
    }

    fn reset_to_defaults(&mut self, gpu: usize) -> Result<()> {
        self.require_awake(gpu)?;
        let mut errors = Vec::new();
        let card = self.card(gpu)?;
        let (od_path, level_path, cap_path) =
            (card.attr(OD_FILE), card.attr(PERFORMANCE_LEVEL_FILE), card.hw("power1_cap"));
        if od_path.exists()
            && let Err(e) = self.write(&od_path, "r\n").and_then(|()| self.write(&od_path, "c\n"))
        {
            errors.push(e.message);
        }
        if let (Some(path), Some((_, _, default))) = (cap_path, self.power_limits(gpu))
            && read_i64(&path) != Some(default)
            && let Err(e) = self.write(&path, &format!("{default}\n"))
        {
            errors.push(e.message);
        }
        if let Err(e) = self.set_fan_auto(gpu) {
            errors.push(e.message);
        }
        if read_text(&level_path).is_some_and(|l| l != "auto")
            && let Err(e) = self.write(&level_path, "auto\n")
        {
            errors.push(e.message);
        }
        if !errors.is_empty() {
            bail!("Reset partially failed: {}", errors.join(" | "));
        }
        Ok(())
    }

    fn diagnostics(&mut self, gpu: usize) -> String {
        let mut sb = String::new();
        if let Err(e) = self.describe_diagnostics(gpu, &mut sb) {
            return e.message;
        }
        if self.is_awake(gpu) {
            sb.push_str("\n--- telemetry ---\n");
            if let Ok(t) = self.read_telemetry(gpu) {
                let _ = writeln!(
                    sb,
                    "  core {} MHz, memory {} MHz, fabric {} MHz, SoC {} MHz, {} mV",
                    t.core_clock_mhz, t.memory_clock_mhz, t.fabric_clock_mhz, t.soc_clock_mhz, t.voltage_mv
                );
                let _ = writeln!(
                    sb,
                    "  edge {} °C, junction {} °C, memory {} °C, {} W, load {} %",
                    t.temperature_c, t.hot_spot_c, t.memory_temperature_c, t.power_watts, t.gpu_load_percent
                );
                let _ = writeln!(sb, "  limit reason: {}", t.limit_reason);
                for s in &t.supplemental_sensors {
                    let _ = writeln!(sb, "  [{}] {}: {} {}", s.key, s.name, trim_decimals(s.value, 3), s.unit);
                }
            }
        }
        sb
    }

    /// What stands between this card and its controls, in terms of what to do about it.
    fn notes(&mut self, gpu: usize) -> Vec<String> {
        let Ok(card) = self.card(gpu) else { return Vec::new() };
        if !self.is_awake(gpu) {
            let state = read_text(&card.attr("power_state")).unwrap_or_else(|| "off".into());
            return vec![format!(
                "GPU {gpu} ({}) is asleep ({state}). The kernel powers an idle card down, and Roch GPU does not wake \
                 it, so its readings and controls are unavailable until something else does. If it never comes back, \
                 look for amdgpu resume errors with: journalctl -k | grep {}",
                card.pci, card.pci
            )];
        }
        let mut notes = Vec::new();
        let mask = read_text(&self.module_parameters.join("ppfeaturemask"))
            .and_then(|m| u64::from_str_radix(m.trim_start_matches("0x"), 16).ok());
        if mask.is_some_and(|m| m & OVERDRIVE_BIT == 0) {
            notes.push(format!(
                "Overdrive is off, so the clock, voltage and fan controls are not offered. To turn it on, run `{}` \
                     and reboot.",
                sysfs::setup_command(" --overdrive")
            ));
        }
        let writable = [card.hw("power1_cap"), Some(card.attr(OD_FILE))]
            .into_iter()
            .flatten()
            .filter(|p| p.exists())
            .collect::<Vec<_>>();
        if !writable.is_empty() && !writable.iter().all(|p| sysfs::can_write(p)) {
            notes.push(format!("Reading works, but a change will be refused. {}", sysfs::permission_hint()));
        }
        notes
    }
}

#[cfg(test)]
mod tests;
