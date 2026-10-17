//! `GetDiagnostics`: the raw dump users paste into bug reports. The text follows the C# line for
//! line (section titles, field order, padding, .NET number formats), because reports from both
//! versions get read side by side. Read-only throughout: getters, read-only probes and the shape
//! sweeps of GET entry points (no setter is ever swept).

use std::fmt::Write as _;

use super::set::two_dp;
use super::{Fail, NvidiaBackend};
use crate::nvapi;
use crate::nvml;
use crate::private;
use crate::vf_curve;
use crate::wrapper;

/// .NET `bool.ToString()`.
fn tf(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

/// .NET fixed-point formats (`0`, `0.0`, `0.000`, `F4`): halves away from zero.
pub(crate) fn fixed(v: f64, decimals: usize) -> String {
    if !v.is_finite() {
        return if v.is_nan() {
            "NaN".into()
        } else if v > 0.0 {
            "∞".into()
        } else {
            "-∞".into()
        };
    }
    let scale = 10f64.powi(decimals as i32);
    let r = (v * scale).round() / scale;
    format!("{r:.decimals$}")
}

/// .NET `N<d>`: fixed with thousands separators (en-US grouping).
pub(crate) fn grouped(v: f64, decimals: usize) -> String {
    let s = fixed(v, decimals);
    let (sign, rest) = s.strip_prefix('-').map(|r| ("-", r)).unwrap_or(("", s.as_str()));
    let (int, frac) = rest.split_once('.').map(|(i, f)| (i, Some(f))).unwrap_or((rest, None));
    if !int.bytes().all(|b| b.is_ascii_digit()) {
        return s;
    }
    let mut out = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    match frac {
        Some(f) => format!("{sign}{out}.{f}"),
        None => format!("{sign}{out}"),
    }
}

type Body<'a> = Box<dyn FnOnce(&mut NvidiaBackend, &mut String) -> Result<(), Fail> + 'a>;

impl NvidiaBackend {
    fn section(&mut self, sb: &mut String, title: &str, body: Body<'_>) {
        let _ = writeln!(sb, "--- {title} ---");
        if let Err(e) = body(self, sb) {
            let _ = writeln!(sb, "  <{}: {}>", e.type_name(), e.message());
        }
        sb.push('\n');
    }

    pub(super) fn diagnostics_impl(&mut self, gpu: usize) -> String {
        let g = match self.gpu(gpu) {
            Ok(g) => g,
            Err(e) => return e.message,
        };
        let dev = self.devices.get(gpu).cloned().unwrap_or_default();
        let mut sb = String::new();
        let _ = writeln!(sb, "GPU  : {}", dev.name);
        let _ = writeln!(sb, "Driver {}   vBIOS {}", dev.driver_version, dev.bios_version);
        sb.push('\n');

        self.section(
            &mut sb,
            "Power policy INFO (per entry)",
            Box::new(move |_, sb| {
                for i in nvapi::power_policies_info(g)? {
                    let _ = writeln!(
                        sb,
                        "  pstate={}  minPCM={}  defPCM={}  maxPCM={}",
                        nvapi::pstate_name(i.pstate),
                        i.min_pcm,
                        i.default_pcm,
                        i.max_pcm
                    );
                }
                Ok(())
            }),
        );

        self.section(
            &mut sb,
            "Power policy STATUS (raw entries)",
            Box::new(move |_, sb| {
                let entries = nvapi::power_policies_status(g)?;
                let _ = writeln!(sb, "  count={}", entries.len());
                for (i, e) in entries.iter().enumerate() {
                    let _ = writeln!(
                        sb,
                        "  [{i}] pstate={} unk1={} powerPCM={} unk2={}",
                        nvapi::pstate_name(e.pstate),
                        e.unknown1,
                        e.target_pcm,
                        e.unknown2
                    );
                }
                Ok(())
            }),
        );

        self.section(
            &mut sb,
            "Thermal policy",
            Box::new(move |_, sb| {
                for i in nvapi::thermal_policies_info(g)? {
                    let _ = writeln!(
                        sb,
                        "  info  ctrl={} min={} def={} max={}",
                        nvapi::thermal_controller_name(i.controller),
                        wrapper::thermal_info_c(i.min_c),
                        wrapper::thermal_info_c(i.default_c),
                        wrapper::thermal_info_c(i.max_c)
                    );
                }
                for p in nvapi::thermal_policies_status(g)? {
                    let _ = writeln!(
                        sb,
                        "  status ctrl={} pstate={} target={}",
                        nvapi::thermal_controller_name(p.controller),
                        nvapi::pstate_name(p.pstate),
                        p.target_c()
                    );
                }
                Ok(())
            }),
        );

        self.section(
            &mut sb,
            "Power rails (read-only)",
            Box::new(move |_, sb| {
                for r in private::read_power_rails(g) {
                    let (a, v) = (r.milliamps as f64 / 1000.0, r.microvolts as f64 / 1_000_000.0);
                    let _ = writeln!(
                        sb,
                        "    slot {:>2}  {:>8} A  {:>8} V  = {:>8} W",
                        r.slot,
                        fixed(a, 3),
                        fixed(v, 3),
                        fixed(a * v, 2)
                    );
                }
                Ok(())
            }),
        );

        self.section(
            &mut sb,
            "OCP ranges the driver reports (read-only)",
            Box::new(move |_, sb| {
                for r in private::read_ocp_ranges(g) {
                    let _ = writeln!(
                        sb,
                        "    {:>8} .. {:>8} A, at {} A",
                        two_dp(r.min_ma as f64 / 1000.0),
                        two_dp(r.max_ma as f64 / 1000.0),
                        two_dp(r.value_ma as f64 / 1000.0)
                    );
                }
                Ok(())
            }),
        );

        self.section(
            &mut sb,
            "OCP current limits (read-only)",
            Box::new(move |_, sb| {
                // Only the one shape known to be good. An earlier version swept sizes to find the
                // family, and a size smaller than the driver expects makes it write past the buffer:
                // the heap is corrupted and the process dies later, somewhere else entirely.
                let ch = private::read_ocp_channels(g);
                let _ = writeln!(sb, "  channels: {}", ch.len());
                for c in ch {
                    let _ = writeln!(
                        sb,
                        "    slot {:>2} selector={:<6} value={:<8} {}",
                        c.slot, c.selector, c.value, c.words
                    );
                }
                Ok(())
            }),
        );

        self.section(
            &mut sb,
            "Policy shapes (thermal + power, read-only)",
            Box::new(move |_, sb| {
                // The thermal policy family comes back empty on Blackwell through the v2 structs. Ask
                // each policy GET entry point which sizes/versions it accepts, what it says to a version
                // it cannot accept (the driver names the ones it does), and whether an empty answer is
                // the mask trap — a count that must be filled in before the driver fills anything out.
                let points: [(&str, u32); 4] = [
                    ("ClientThermalPoliciesGetInfo", 0x0D25_8BB5),
                    ("ClientThermalPoliciesGetLimit", 0xE9C4_25A1),
                    ("ClientPowerPoliciesGetInfo", 0x3420_6D86),
                    ("ClientPowerPoliciesGetStatus", 0x7091_6171),
                ];
                for (name, id) in points {
                    let _ = writeln!(sb, "  {name} (0x{id:08X}) exported={}", tf(private::exposes(id)));
                    let (bad, _) = private::call_raw(g, id, 0x400, 9, None, 0);
                    let msg = if bad == -1 {
                        "(not called)".to_string()
                    } else {
                        nvapi::error_message(bad).unwrap_or_default()
                    };
                    let _ = writeln!(sb, "      version 9 -> status={bad}  \"{msg}\"");
                    for hit in private::sweep_shapes(g, id, 0x08, 0x400, &[1, 2, 3, 4], private::SWEEP_MAX_WORDS) {
                        let _ =
                            writeln!(sb, "      accepted size=0x{:03X} ver={}  {}", hit.size, hit.version, hit.words);
                    }
                    for ver in [1u32, 2, 3] {
                        for size in [0x38usize, 0x58, 0x88, 0xB8, 0x100] {
                            for pre in [1u32, 4, 0xFF] {
                                let (st, w) = private::call_raw(g, id, size, ver, Some(4), pre);
                                if st != 0 || w.is_empty() {
                                    continue;
                                }
                                let nz: Vec<String> = (2..w.len())
                                    .filter(|&i| w[i] != 0)
                                    .take(12)
                                    .map(|i| format!("+0x{:03X}={}", i * 4, w[i]))
                                    .collect();
                                if !nz.is_empty() {
                                    let _ = writeln!(
                                        sb,
                                        "      prefill count={pre} size=0x{size:03X} ver={ver}  {}",
                                        nz.join(" ")
                                    );
                                }
                            }
                        }
                    }
                }
                Ok(())
            }),
        );

        self.section(
            &mut sb,
            "Domain control blocks (looking for a per-point crossbar table)",
            Box::new(move |_, sb| {
                for slot in [private::SLOT_CORE, private::SLOT_XBAR, private::SLOT_SYS, private::SLOT_VIDEO] {
                    for line in private::dump_domain_block(g, slot) {
                        let _ = writeln!(sb, "  {line}");
                    }
                    for line in private::dump_domain_info(g, slot) {
                        let _ = writeln!(sb, "  {line}");
                    }
                }
                Ok(())
            }),
        );

        self.section(
            &mut sb,
            "Voltage",
            Box::new(move |this, sb| {
                let _ = writeln!(sb, "  current      = {} mV", fixed(nvapi::current_voltage_uv(g)? as f64 / 1000.0, 0));
                let _ = writeln!(sb, "  boost percent= {}", nvapi::core_voltage_boost_percent(g)?);
                let _ = writeln!(sb, "  lock (cap)   = {} mV   (0 = none)", this.read_voltage_lock_mv_impl(gpu));
                let _ = writeln!(sb, "  mechanism    = {}", this.voltage_lock_mechanism);
                for e in nvapi::clock_boost_locks(g)? {
                    let _ = writeln!(
                        sb,
                        "  lock entry   : domain={} mode={} volt={} mV",
                        nvapi::clock_domain_name(e.clock_domain),
                        nvapi::lock_mode_name(e.lock_mode),
                        fixed(e.voltage_uv as f64 / 1000.0, 0)
                    );
                }
                let idx = this.nvml_index_for(gpu);
                let mut line =
                    format!("  NVML         = {}", if nvml::is_available() { "available" } else { "unavailable" });
                if let Some(le) = nvml::last_error() {
                    let _ = write!(line, " ({le})");
                }
                if let Some(nm) = nvml::device_name(idx) {
                    let _ = write!(line, ", device[{idx}] = {nm}");
                }
                let _ = writeln!(sb, "{line}");
                for mv in [900, 950, 1000, 1050] {
                    let mhz = this.curve_frequency_at_mv(gpu, g, mv);
                    let _ = writeln!(sb, "  curve says {mv} mV -> {mhz} MHz (clock cap used if the lock is ignored)");
                }
                Ok(())
            }),
        );

        self.section(
            &mut sb,
            "PStates20 (P0 clock deltas)",
            Box::new(move |_, sb| {
                let ps = nvapi::pstates20(g)?;
                let _ = writeln!(sb, "  editable={}", tf(ps.is_editable));
                if let Some(clocks) = ps.clocks_for(0) {
                    for c in clocks {
                        let d = c.freq_delta_khz;
                        let _ = writeln!(
                            sb,
                            "  {:<9} editable={} delta={} MHz range=[{}..{}]",
                            nvapi::clock_domain_name(c.domain_id),
                            tf(c.editable()),
                            d.value / 1000,
                            d.min / 1000,
                            d.max / 1000
                        );
                    }
                }
                Ok(())
            }),
        );

        // Every clock domain the private family admits to. The measure family numbers domains the
        // same way the info family types them (0 core, 1 crossbar, 4 memory all agree), so a slot
        // whose type reports a plausible frequency is the domain it describes. The control offset
        // is predicted from the block arithmetic the crossbar confirmed.
        self.section(
            &mut sb,
            "Crossbar (XBAR) probe",
            Box::new(move |_, sb| {
                let x = private::probe_xbar(g);
                let _ = writeln!(
                    sb,
                    "  entry points resolved: info={} control={} set={} measure={}",
                    tf(x.info_resolved),
                    tf(x.control_resolved),
                    tf(x.set_resolved),
                    tf(x.measure_resolved)
                );
                let st = |s: i32| match s {
                    i32::MIN => "not called / threw".to_string(),
                    0 => "0 (Ok)".to_string(),
                    s => s.to_string(),
                };
                let _ = writeln!(sb, "  GetInfo status    : {}", st(x.info_status));
                let _ = writeln!(sb, "  GetControl status : {}", st(x.control_status));
                let _ = writeln!(
                    sb,
                    "  measured clocks   : core {} MHz, xbar {} MHz, memory {} MHz",
                    grouped(x.core_khz as f64 / 1000.0, 0),
                    grouped(x.xbar_khz as f64 / 1000.0, 0),
                    grouped(x.memory_khz as f64 / 1000.0, 0)
                );
                if x.core_khz > 0 && x.xbar_khz > 0 {
                    let _ = writeln!(sb, "  xbar : core ratio = {}", fixed(x.xbar_khz as f64 / x.core_khz as f64, 4));
                }
                if x.entry_types.is_empty() {
                    let _ = writeln!(sb, "  info entry types  : <none read>");
                } else {
                    let types: Vec<String> = x.entry_types.iter().map(|t| t.to_string()).collect();
                    let _ = writeln!(sb, "  info entry types  : {}", types.join(","));
                }
                let _ = writeln!(sb, "  type-1 entry index: {}", x.type_one_index.map(|i| i as i64).unwrap_or(-1));
                let _ = writeln!(sb, "  domains listed (slot / type / range / predicted control offset):");
                for de in private::read_domain_entries(g) {
                    let range = if de.has_range {
                        format!("{:>6}..{:<6} MHz", de.min_mhz, de.max_mhz)
                    } else {
                        "no range found    ".to_string()
                    };
                    let _ = writeln!(
                        sb,
                        "    slot {:>2}  type {:>3}  {range}  -> +0x{:04X}   measured {:>6} MHz",
                        de.index,
                        de.kind,
                        private::control_offset_for(de.index.max(0) as usize),
                        private::measure_clock_khz(g, de.kind) / 1000
                    );
                }
                if !x.type_one_words.is_empty() {
                    let _ = writeln!(sb, "  type-1 entry words (offset: value  [signed lo/hi halves]):");
                    for (i, &w) in x.type_one_words.iter().enumerate() {
                        let (lo, hi) = ((w & 0xFFFF) as i16, (w >> 16) as i16);
                        let _ = writeln!(sb, "      +0x{:03X}  {w:>12}  0x{w:08X}  [{lo:>6} / {hi:>6}]", i * 4);
                    }
                }
                if !x.control_words.is_empty() {
                    let _ = writeln!(sb, "  control buffer, non-zero words ({} shown):", x.control_words.len() / 2);
                    for pair in x.control_words.as_chunks::<2>().0 {
                        let _ = writeln!(sb, "      +0x{:04X}  {:>12}  0x{:08X}", pair[0], pair[1], pair[1]);
                    }
                } else {
                    let _ = writeln!(sb, "  control buffer: entirely zero");
                }
                let threw = |s: i32| if s == i32::MIN { "threw".to_string() } else { s.to_string() };
                let _ = writeln!(sb, "  GetControl shapes (read-only, how much the driver fills in):");
                for e in private::explore_xbar_control(g) {
                    let _ = writeln!(
                        sb,
                        "      {:<32} status={:<5} nonZero={:<4} {}",
                        e.label,
                        threw(e.status),
                        e.non_zero,
                        e.head
                    );
                }
                let _ = writeln!(sb, "  GetControl accepted shapes (-9 = wrong struct version, 0 = accepted):");
                for (shape, status) in private::probe_xbar_control_shapes(g) {
                    let _ = writeln!(sb, "      {shape:<16} status={}", threw(status));
                }
                let xi = private::read_xbar_info(g);
                let _ = writeln!(
                    sb,
                    "  ReadXbarInfo      : supported={} range=[{}..{}]",
                    tf(xi.supported),
                    xi.min_offset_mhz,
                    xi.max_offset_mhz
                );
                let _ = writeln!(sb, "  current offset    : {} MHz", private::read_xbar_offset_mhz(g));
                Ok(())
            }),
        );

        // NV_GPU_CLOCK_FREQUENCIES carries 32 domain slots and the wrapper names four (0 Graphics,
        // 4 Memory, 7 Processor, 8 Video), so an undocumented domain would sit in a slot nobody
        // normally looks at. On a 4070 Ti this reports slots 0 and 4 only.
        self.section(
            &mut sb,
            "Clock domains (all 32 slots, not just the named ones)",
            Box::new(move |_, sb| {
                for (label, kind) in [("current", 0u32), ("base", 1), ("boost", 2)] {
                    let freqs = nvapi::clock_frequencies(g, kind)?;
                    let slots = &freqs.clocks;
                    let live: Vec<usize> =
                        (0..slots.len()).filter(|&i| slots[i].present() || slots[i].frequency_khz != 0).collect();
                    let list: Vec<String> = live.iter().map(|i| i.to_string()).collect();
                    let _ = writeln!(
                        sb,
                        "  {label:<8} {} of {} slots populated: [{}]",
                        live.len(),
                        slots.len(),
                        list.join(", ")
                    );
                    for i in live {
                        let name = match i {
                            0 => "Graphics",
                            4 => "Memory",
                            7 => "Processor",
                            8 => "Video",
                            _ => "UNDOCUMENTED  <-- not a public domain",
                        };
                        let _ = writeln!(
                            sb,
                            "      slot {i:>2}  {:>9} MHz  {name}",
                            grouped(slots[i].frequency_khz as f64 / 1000.0, 1)
                        );
                    }
                }
                // The other surface that enumerates domains: a tunable domain the wrapper has no name
                // for shows up here as a raw id rather than an enum member.
                let ps = nvapi::pstates20(g)?;
                let _ = writeln!(sb, "  pstate domains (editable={}):", tf(ps.is_editable));
                for (pstate, clocks) in &ps.clocks {
                    for c in clocks {
                        let named = nvapi::clock_domain_defined(c.domain_id);
                        let _ = writeln!(
                            sb,
                            "      {:<20} domainId={:<3} {:<14} editable={}",
                            nvapi::pstate_name(*pstate),
                            c.domain_id as i32,
                            if named { nvapi::clock_domain_name(c.domain_id) } else { "UNDOCUMENTED".into() },
                            tf(c.editable())
                        );
                    }
                }
                Ok(())
            }),
        );

        self.section(
            &mut sb,
            "V/F curve — private call probe",
            Box::new(move |this, sb| {
                let (cb, tb) = private::struct_sizes();
                let _ = writeln!(sb, "  entry points resolved = {}", tf(private::curve_available()));
                let _ = writeln!(sb, "  struct sizes: curve={cb} table={tb}");
                let boost_mask = match nvapi::clock_boost_mask(g) {
                    Ok(m) => Some(m),
                    Err(e) => {
                        let _ = writeln!(sb, "  GetClockBoostMask failed: {}", e.type_name());
                        None
                    }
                };
                if let Some(m) = boost_mask {
                    let _ = writeln!(sb, "  boost mask = {:08X} {:08X} {:08X} {:08X}", m[0], m[1], m[2], m[3]);
                }
                for (label, mask) in
                    [("driver mask", boost_mask.unwrap_or(private::FULL_MASK)), ("all-ones", private::FULL_MASK)]
                {
                    let pts = private::read_curve(g, &mask);
                    let valid = pts.iter().filter(|p| p.0 > 0 && p.1 > 0).count();
                    let mut line = format!("  ReadCurve[{label}] -> {valid} valid point(s)");
                    if valid > 0 {
                        let lo = pts.iter().filter(|p| p.0 > 0).map(|p| p.0).min().unwrap_or(0);
                        let hi = pts.iter().map(|p| p.0).max().unwrap_or(0);
                        let _ = write!(line, ", {}–{} mV", lo / 1000, hi / 1000);
                    }
                    let _ = writeln!(sb, "{line}");
                    let dl = private::read_deltas(g, &mask);
                    let nz = dl.iter().filter(|&&x| x != 0).count();
                    let _ = writeln!(sb, "  ReadDeltas[{label}] -> {} slot(s), {nz} non-zero", dl.len());
                }
                let _ = writeln!(sb, "  chosen mask = 0x{:08X}", this.curve_mask(gpu, g)[0]);
                let _ = writeln!(sb, "  layout probe (entries x version -> status / points / mV range / max MHz):");
                for pr in private::probe_curve_layouts(g) {
                    let tail = if pr.status == 0 {
                        format!("points={:>4}  {}-{} mV  max {} MHz", pr.valid_points, pr.min_mv, pr.max_mv, pr.max_mhz)
                    } else {
                        String::new()
                    };
                    let _ = writeln!(sb, "    {:>4} x v{}  status={:<3} {tail}", pr.gpu_entries, pr.version, pr.status);
                }
                let chosen = this.curve_layout(gpu, g);
                // The points actually read, not what the struct's field split would allow: the two
                // disagree by 24 on a 5070 Ti, and "103 curve points" next to a probe line saying 127
                // sends the reader after the wrong thing.
                let chosen_points = this.read_effective_curve(gpu, g)?.len();
                let _ = writeln!(
                    sb,
                    "  chosen layout = {} entries, version {}, {chosen_points} curve points (buffer holds up to {})",
                    chosen.0,
                    chosen.1,
                    private::total_points(chosen.0)
                );
                let mask = this.curve_mask(gpu, g);
                let (arr_a, arr_b) = private::read_trailing_arrays(g, &mask, chosen.0, chosen.1);
                if !arr_a.is_empty() {
                    let join = |v: &[i32]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
                    let _ = writeln!(sb, "  trailing array 0 = {}", join(&arr_a));
                    let _ = writeln!(sb, "  trailing array 1 = {}", join(&arr_b));
                    match this.write_recipe.get(&gpu) {
                        None => {
                            let _ = writeln!(
                                sb,
                                "  write recipe: not yet established (no curve write attempted this session)"
                            );
                        }
                        Some((m, flag)) => {
                            let _ = writeln!(
                                sb,
                                "  write recipe: mask {:08X}-{:08X}, entry flag {}",
                                m[0],
                                m[3],
                                flag.unwrap_or(-1)
                            );
                        }
                    }
                }
                Ok(())
            }),
        );

        self.section(
            &mut sb,
            "V/F curve (stock, valid points only)",
            Box::new(move |this, sb| {
                let base = this.read_base_curve(gpu, g)?;
                let eff = this.read_effective_curve(gpu, g)?;
                let _ = writeln!(
                    sb,
                    "  stock max voltage = {} mV",
                    fixed(vf_curve::max_voltage_uv(&base) as f64 / 1000.0, 0)
                );
                let (r_min, r_max) = NvidiaBackend::boost_delta_range(g);
                let _ = writeln!(sb, "  delta range = [{}..{}] MHz", r_min / 1000, r_max / 1000);
                let cap = vf_curve::infer_cap_voltage_uv(&eff);
                let cap_text = if cap == 0 {
                    "none (curve not flattened)".to_string()
                } else {
                    format!("{} mV", fixed(cap as f64 / 1000.0, 0))
                };
                let _ = writeln!(sb, "  inferred cap = {cap_text}");
                for (i, p) in base.iter().enumerate() {
                    if p.voltage_uv == 0 || p.frequency_khz == 0 {
                        continue;
                    }
                    let Some(live) = eff.get(i) else {
                        return Err(Fail::Other(
                            "ArgumentOutOfRangeException",
                            "Index was out of range. Must be non-negative and less than the size of the collection. \
                         (Parameter 'index')"
                                .into(),
                        ));
                    };
                    let _ = writeln!(
                        sb,
                        "  [{i:>2}] {:>6} mV  stock {:>5} MHz  live {:>5} MHz",
                        fixed(p.voltage_uv as f64 / 1000.0, 0),
                        p.frequency_khz / 1000,
                        live.frequency_khz / 1000
                    );
                }
                Ok(())
            }),
        );

        self.section(
            &mut sb,
            "Voltage rail status entries (looking for a per-rail voltage)",
            Box::new(move |_, sb| {
                for (rail, words) in private::dump_rail_status(g) {
                    let _ = writeln!(sb, "  rail {rail}: {words}");
                }
                Ok(())
            }),
        );

        self.section(
            &mut sb,
            "Power topology",
            Box::new(move |_, sb| {
                // NV_GPU_CLIENT_POWER_TOPOLOGY_STATUS (0xEDCF624E): version, count, then four entries of
                // {domain, reserved, usage, reserved} — 0x48 bytes. Domains are GPU and Board, figures
                // per-cent-mille of the limit rather than watts (hence board power from NVML). There are
                // no per-channel power *limits* anywhere in this family.
                let (st, w) = private::call_raw(g, 0xEDCF_624E, 0x48, 1, None, 0);
                if st != 0 || w.len() < 4 {
                    let _ = writeln!(sb, "  status {st}");
                    return Ok(());
                }
                let count = w[1];
                let _ = writeln!(sb, "  count = {count}");
                for i in 0..count.clamp(0, 4) as usize {
                    let at = 2 + i * 4;
                    if at + 2 >= w.len() {
                        break;
                    }
                    let domain = match w[at] {
                        0 => "GPU".to_string(),
                        1 => "Board".to_string(),
                        d => format!("domain {d}"),
                    };
                    let _ = writeln!(sb, "  {domain:<6} usage {} % of limit", fixed(w[at + 2] as f64 / 1000.0, 3));
                }
                Ok(())
            }),
        );

        self.section(
            &mut sb,
            "Voltage lock (SetClockBoostLock)",
            Box::new(move |this, sb| {
                for e in nvapi::clock_boost_locks(g)? {
                    let _ = writeln!(
                        sb,
                        "  domain={} mode={} voltage={} mV",
                        nvapi::clock_domain_name(e.clock_domain),
                        nvapi::lock_mode_name(e.lock_mode),
                        fixed(e.voltage_uv as f64 / 1000.0, 0)
                    );
                }
                let _ =
                    writeln!(sb, "  -> interpreted lock = {} mV (0 = unlocked)", this.read_voltage_lock_mv_impl(gpu));
                Ok(())
            }),
        );

        self.section(
            &mut sb,
            "Coolers",
            Box::new(move |_, sb| {
                for c in wrapper::coolers(g)? {
                    let _ = writeln!(
                        sb,
                        "  id={} policy={} level={}% rpm={} range=[{}..{}]",
                        c.cooler_id,
                        nvapi::cooler_policy_name(c.current_policy),
                        c.current_level,
                        c.current_rpm,
                        c.default_min_level,
                        c.default_max_level
                    );
                }
                Ok(())
            }),
        );

        self.section(
            &mut sb,
            "Thermal settings (public API sensors)",
            Box::new(move |_, sb| {
                for s in nvapi::thermal_settings(g)? {
                    let _ = writeln!(
                        sb,
                        "  target={} ctrl={} temp={}",
                        nvapi::thermal_target_name(s.target),
                        nvapi::thermal_controller_name(s.controller),
                        s.current
                    );
                }
                Ok(())
            }),
        );

        let name = dev.name.clone();
        self.section(
            &mut sb,
            "Private thermal sensors (all 32 slots)",
            Box::new(move |_, sb| {
                let mask = private::probe_mask(g);
                let _ = writeln!(sb, "  probed mask = 0x{mask:X}");
                if mask == 0 {
                    let _ = writeln!(sb, "  (call unavailable on this driver)");
                    return Ok(());
                }
                let slots = private::read_thermal_slots(g, mask);
                for (i, &v) in slots.iter().enumerate() {
                    if v != 0.0 {
                        let _ = writeln!(sb, "  slot[{i:>2}] = {} °C", fixed(v, 1));
                    }
                }
                let (hs, mj) = private::interpret(&name, &slots);
                let _ = writeln!(sb, "  -> interpreted hotspot={}  memory={}", fixed(hs, 1), fixed(mj, 1));
                Ok(())
            }),
        );

        // All eight of the v3 struct's channels: a card populating more than this one's two will
        // show it.
        self.section(
            &mut sb,
            "Private thermal channels (v3 struct, all eight)",
            Box::new(move |_, sb| {
                let chans = private::read_thermal_channels(g);
                if chans.is_empty() {
                    let _ = writeln!(sb, "  (v3 struct not accepted by this driver)");
                    return Ok(());
                }
                for c in chans {
                    let reading = if c.present {
                        format!("{} °C", fixed(c.celsius, 1))
                    } else {
                        "absent (255 °C marker)".to_string()
                    };
                    let _ = writeln!(sb, "  slot {}  type={:<4} {reading}", c.slot, c.kind);
                }
                Ok(())
            }),
        );

        self.section(
            &mut sb,
            "Private thermal shapes (version x mask -> live slots)",
            Box::new(move |_, sb| {
                for r in private::probe_thermal(g) {
                    let _ =
                        writeln!(sb, "  v{}  mask=0x{:08X}  status={:<4}  {}", r.version, r.mask, r.status, r.slots);
                }
                Ok(())
            }),
        );

        sb
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotnet_number_formats() {
        assert_eq!(fixed(1.1535, 3), "1.154");
        assert_eq!(fixed(2.5, 0), "3");
        assert_eq!(fixed(-2.5, 0), "-3");
        assert_eq!(fixed(f64::NAN, 1), "NaN");
        assert_eq!(fixed(14.30591, 4), "14.3059");
        assert_eq!(grouped(2295.0, 1), "2,295.0");
        assert_eq!(grouped(14001.0, 1), "14,001.0");
        assert_eq!(grouped(532.0, 1), "532.0");
        assert_eq!(grouped(885.4, 0), "885");
        assert_eq!(grouped(-1234567.0, 0), "-1,234,567");
        assert_eq!(format!("{:>9}", grouped(2295.0, 1)), "  2,295.0");
    }
}
