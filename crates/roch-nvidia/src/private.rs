//! Private NVAPI families the wrapper did not cover (port of `NvApiPrivate.cs`): thermal sensors,
//! the V/F curve and its delta table at arbitrary layouts, voltage rails, OCP limits, the power
//! monitor, rail ADCs, and the clock-domain (crossbar/SYS/video) family, plus the read-only probes
//! the diagnostics report uses.
//!
//! Most of these structs are described by sizes worked out on hardware rather than by a published
//! header, so they are driven through raw buffers at explicit offsets, exactly as the C# did. The
//! decoding of every buffer is split into a pure `parse_*`/`stage_*` function so it can be unit
//! tested without touching a GPU; the driver-facing functions only allocate, call and hand over.
//!
//! Every buffer handed to the driver here carries [`CALL_SLACK`] (or the probes' larger slack) past
//! its declared size. The C# omitted it on some of these calls; adding it changes nothing the driver
//! sees, because the version word still declares the size we mean.
//!
//! Return conventions follow the C# so the backend port maps one to one: NVAPI statuses as `i32`
//! (0 = Ok, -1 = entry point not exported), `Option<String>` for "None on success, else the
//! reason", and empty collections when a family is unavailable.

use std::collections::BTreeMap;
use std::mem::{offset_of, size_of};
use std::sync::atomic::{AtomicI32, Ordering};

use crate::buf::{Buf, rd_i16, rd_i32, rd_u32, wr_i32};
use crate::domain_offset;
use crate::nvapi::{self, GpuHandle, RawFn, id};
use crate::ocp;

/// Slack allocated past the end of every private-call buffer.
///
/// These structures are described by a size we worked out, not by a header anyone published. Get
/// that size too small and the driver writes past the end of the allocation, which corrupts the
/// heap and kills the process later, somewhere else entirely, with an access violation that says
/// nothing about where it came from. That happened twice while the OCP family was being found. The
/// version word still carries the size we mean, so the driver's behaviour is unchanged; this only
/// decides how much room a wrong guess has to be wrong in.
pub const CALL_SLACK: usize = 8192;

/// The read-only probes' over-allocation (256 KB, far past any of these structures).
const PROBE_SLACK: usize = 0x40000;

fn raw(fid: u32) -> Option<RawFn> {
    // SAFETY: every id used in this module takes `(NvPhysicalGpuHandle, void*)`.
    unsafe { nvapi::resolve::<RawFn>(fid) }.ok()
}

/// Call `f` on `buf`.
fn invoke(f: RawFn, h: GpuHandle, buf: &mut Buf) -> i32 {
    // SAFETY: the buffer is live, zero-initialised past what we wrote, and at least as large as
    // its version word declares (plus slack).
    unsafe { f(h.0, buf.ptr()) }
}

/// A zeroed buffer of `size` (+ `slack`) with `word` at offset 0.
fn request(size: usize, slack: usize, word: u32) -> Buf {
    let mut b = Buf::new(size, slack);
    b.put_u32(0, word);
    b
}

/// Run `f` on a copy of `data` inside a slack-padded buffer, copying the result back when `always`
/// is set or the call succeeded. Returns the status.
fn call_on(f: RawFn, h: GpuHandle, data: &mut [u8], always: bool) -> i32 {
    let mut buf = Buf::new(data.len(), CALL_SLACK);
    buf.load(data);
    let status = invoke(f, h, &mut buf);
    if always || status == 0 {
        let n = data.len();
        data.copy_from_slice(&buf.bytes()[..n]);
    }
    status
}

/// Whether the driver exports this private entry point at all (`NvApiPrivate.Exposes`).
pub fn exposes(fid: u32) -> bool {
    nvapi::exposes(fid)
}

/// .NET's `{x:0.0}`: one decimal, halves away from zero.
fn fmt1(x: f64) -> String {
    format!("{:.1}", (x * 10.0).round() / 10.0)
}

/// `+0x{off}={w}` for every non-zero word at `base + off`, `off` in `from..to` stepping 4, at most
/// `max` of them. `width` is the hex digit padding the C# used at that call site.
fn nonzero_words(b: &[u8], base: usize, from: usize, to: usize, width: usize, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut off = from;
    while off + 4 <= to && out.len() < max {
        let w = rd_i32(b, base + off);
        if w != 0 {
            out.push(format!("+0x{off:0width$X}={w}"));
        }
        off += 4;
    }
    out
}

// ================================================================== thermal sensors (0x65FE3AAD)
//
// NvAPI_GPU_GetThermalSensors — hot-spot and memory-junction temperatures. Same call and slot
// layout LibreHardwareMonitor / HWiNFO use. The mask selects slots; a zero mask returns nothing.

const FN_THERMAL_SENSORS: u32 = 0x65FE_3AAD;
const THERMAL_V2_SIZE: usize = size_of::<nvapi::ThermalSensorsV2>();
const THERMAL_V2_MASK: usize = offset_of!(nvapi::ThermalSensorsV2, mask);
const THERMAL_V2_TEMPS: usize = offset_of!(nvapi::ThermalSensorsV2, temperatures);

/// One v2-shaped call at `version` with `mask`; `(status, buffer)`, `None` when not exported.
fn thermal_call(h: GpuHandle, version: u32, mask: u32) -> Option<(i32, Buf)> {
    let f = raw(FN_THERMAL_SENSORS)?;
    let mut b = request(THERMAL_V2_SIZE, CALL_SLACK, nvapi::version_word(THERMAL_V2_SIZE, version));
    b.put_u32(THERMAL_V2_MASK, mask);
    let st = invoke(f, h, &mut b);
    Some((st, b))
}

/// The 32 raw temperature slots of a v2 buffer, in °C (raw / 256).
pub fn parse_thermal_slots(b: &[u8]) -> Vec<f64> {
    (0..32).map(|i| rd_i32(b, THERMAL_V2_TEMPS + i * 4) as f64 / 256.0).collect()
}

/// `ProbeMask`: the widest sensor mask the driver accepts (LHM's trick: grow bit by bit until it
/// errors). 0 if the call is unavailable.
pub fn probe_mask(h: GpuHandle) -> u32 {
    if raw(FN_THERMAL_SENSORS).is_none() {
        return 0;
    }
    let mut mask = 0u32;
    for bit in 0..32 {
        let try_mask = 1u32 << bit;
        match thermal_call(h, 2, try_mask) {
            Some((0, _)) => mask = try_mask,
            _ => break,
        }
    }
    // LHM uses (highest accepted bit << 1) - 1, i.e. all bits up to the highest one.
    if mask == 0 { 0 } else { (mask << 1).wrapping_sub(1) }
}

/// One row of [`probe_thermal`].
#[derive(Debug, Clone, PartialEq)]
pub struct ThermalProbe {
    pub version: i32,
    pub mask: u32,
    pub status: i32,
    /// `[i]=t.t` for every live slot, or `(none)`.
    pub slots: String,
}

/// `ProbeThermal`: what each (struct version, mask) pair returns — which slots come back with a
/// real reading rather than the 255 that means "no sensor here". Two sensors on a 5070 Ti against
/// the four a monitoring tool shows means a wider mask or another version reaches the rest, or
/// that they are not in this call at all, which is equally worth knowing.
pub fn probe_thermal(h: GpuHandle) -> Vec<ThermalProbe> {
    let mut results = Vec::new();
    if raw(FN_THERMAL_SENSORS).is_none() {
        return results;
    }
    for version in [1u32, 2, 3] {
        for mask in [0x7FFFFu32, 0xFFFF, 0xFFFF_FFFF, 0x1FF, 0x7] {
            let Some((status, b)) = thermal_call(h, version, mask) else { continue };
            let mut live = Vec::new();
            if status == 0 {
                for (i, t) in parse_thermal_slots(b.bytes()).into_iter().enumerate() {
                    if t > 0.0 && t < 200.0 {
                        live.push(format!("[{i}]={}", fmt1(t)));
                    }
                }
            }
            let slots = if live.is_empty() { "(none)".to_string() } else { live.join(" ") };
            results.push(ThermalProbe { version: version as i32, mask, status, slots });
        }
    }
    results
}

/// `Read`: raw temperature slots (°C, already divided by 256). Empty on failure or a zero mask.
pub fn read_thermal_slots(h: GpuHandle, mask: u32) -> Vec<f64> {
    if mask == 0 {
        return Vec::new();
    }
    match thermal_call(h, 2, mask) {
        Some((0, b)) => parse_thermal_slots(b.bytes()),
        _ => Vec::new(),
    }
}

/// `Interpret`: `(hotspot, memory)` °C from the slots, NaN where absent. Slot layout differs by
/// generation (from LibreHardwareMonitor):
///
/// | GPU    | slot 1  | memory  | hotspot |
/// |--------|---------|---------|---------|
/// | RTX 50 | GPU     | slot 2  | none    |
/// | RTX 40 | hotspot | slot 7  | slot 1  |
/// | older  | hotspot | slot 9  | slot 1  |
pub fn interpret(gpu_name: &str, slots: &[f64]) -> (f64, f64) {
    if slots.len() < 32 {
        return (f64::NAN, f64::NAN);
    }
    let pick = |i: usize| match slots.get(i) {
        Some(&t) if t > 0.0 && t < 200.0 => t,
        _ => f64::NAN,
    };
    let name = gpu_name.to_lowercase();
    // Blackwell exposes exactly two sensors through this call, and no hot spot among them: every
    // struct version and mask width returns the same two slots (a three-bit mask returns as much as
    // a nineteen-bit one). Slot 1 tracks the public GPU reading within half a degree; slot 2 sits
    // about eight above it, which is where memory junction sits. A tool that shows hot spot on this
    // card reads it somewhere else.
    if name.contains("rtx 50") {
        return (f64::NAN, pick(2));
    }
    if name.contains("rtx 40") {
        return (pick(1), pick(7));
    }
    (pick(1), pick(9))
}

/// One thermal channel from the v3 struct.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThermalChannel {
    pub slot: i32,
    pub kind: i32,
    /// False for the 255 °C "no sensor" reading.
    pub present: bool,
    pub celsius: f64,
}

const THERM_V3_SIZE: usize = 0x34C8;
const THERM_V3_MASK: usize = 0x08;
const THERM_V3_ENTRY0: usize = 0x48;
const THERM_V3_STRIDE: usize = 0x8C;
const THERM_V3_CHANNELS: usize = 8;

/// Decode the v3 thermal words (as [`call_raw`] returns them).
pub fn parse_thermal_channels(w: &[i32]) -> Vec<ThermalChannel> {
    let mut out = Vec::new();
    for i in 0..THERM_V3_CHANNELS {
        let at = (THERM_V3_ENTRY0 + i * THERM_V3_STRIDE) / 4;
        if at + 1 >= w.len() {
            break;
        }
        let c = w[at] as f64 / 256.0;
        out.push(ThermalChannel { slot: i as i32, kind: w[at + 1], present: c < 200.0, celsius: c });
    }
    out
}

/// `ReadThermalChannels`: eight v3 slots, each a reading and a type, 255 °C meaning no sensor. v2
/// exposes the same two live channels, so this records what the fuller struct holds rather than
/// finding more.
pub fn read_thermal_channels(h: GpuHandle) -> Vec<ThermalChannel> {
    let (status, w) = call_raw(h, FN_THERMAL_SENSORS, THERM_V3_SIZE, 3, Some(THERM_V3_MASK), 0xFF);
    if status != 0 || w.is_empty() {
        return Vec::new();
    }
    parse_thermal_channels(&w)
}

// ================================================================== V/F curve
//
// NvAPIWrapper builds these structs zeroed and calls straight through, which leaves the *mask* at
// 0. The mask selects which curve points the driver fills, so with mask 0 the call returns Ok and
// an entirely empty curve — exactly what a 4070 Ti on 591.86 does. So the mask is set here.
//
// Layouts mirror PrivateVFPCurveV1 / PrivateClockBoostTableV1 (nvapi.rs), driven as raw buffers:
// header = version + mask[4] + unknown[12] = 68 bytes, then curve entries of 7 words (frequency at
// +4, voltage at +8) or delta entries of 9 words (delta at +20).

/// GPU-region entries of the struct layout.
pub const CURVE_ENTRIES: usize = 80;
/// Entries of the region after it ("memory" in the wrapper; really the curve continuing).
pub const MEM_CURVE_ENTRIES: usize = 23;
/// Delta-table GPU entries of the struct layout.
pub const DELTA_ENTRIES: usize = 80;
/// Bytes before the first entry: version + `mask[4]` + `unknown1[12]`.
pub const HEADER_BYTES: usize = 4 + 16 + 48;
/// Curve entry: 7 words.
pub const ENTRY_BYTES: usize = 28;
/// Delta entry: 9 words.
pub const DELTA_ENTRY_BYTES: usize = 36;
/// Words of tail past the named curve arrays. The driver fills curve points into it.
pub const CURVE_TAIL_UINTS: usize = 1064;
/// Delta-table tail: memoryFilled[23] + memoryDeltas[23] + unknown[1529].
const DELTA_TAIL_BYTES: usize = 23 * 4 + 23 * 4 + 1529 * 4;
/// Byte offset of the frequency delta inside a delta entry.
const DELTA_FIELD: usize = 20;

/// The real bound on curve points. The wrapper's struct splits the array into an 80-entry "GPU"
/// region and a 23-entry one after it, and stopping where those end truncates the curve: a 5070 Ti
/// returns a contiguous run to 1240 mV, of which the split shows the first 103 (to 1090 mV). The
/// selection mask is 128 bits wide, and that — not the struct's internal division — is the bound.
pub const MASK_POINTS: usize = 128;

const VFP_CURVE_SIZE: usize = size_of::<nvapi::VfpCurveV1>();
const BOOST_TABLE_SIZE: usize = size_of::<nvapi::ClockBoostTableV1>();
const MEMORY_FILLED: usize = offset_of!(nvapi::ClockBoostTableV1, memory_filled);
const MEMORY_DELTAS: usize = offset_of!(nvapi::ClockBoostTableV1, memory_deltas);

const _: () = assert!(HEADER_BYTES == offset_of!(nvapi::VfpCurveV1, gpu_entries));
const _: () = assert!(HEADER_BYTES == offset_of!(nvapi::ClockBoostTableV1, gpu_deltas));
const _: () = assert!(ENTRY_BYTES == size_of::<nvapi::VfpCurveEntry>());
const _: () = assert!(DELTA_ENTRY_BYTES == size_of::<nvapi::ClockBoostDelta>());
const _: () = assert!(DELTA_FIELD == offset_of!(nvapi::ClockBoostDelta, frequency_delta_khz));
const _: () = assert!(curve_table_size(CURVE_ENTRIES) == VFP_CURVE_SIZE);
const _: () = assert!(delta_table_size(DELTA_ENTRIES) == BOOST_TABLE_SIZE);

/// All 128 bits set. Fine for reads; some drivers reject it on writes.
pub const FULL_MASK: [u32; 4] = [0xFFFF_FFFF; 4];
/// Exactly the 103 bits of the struct layout's points, nothing above.
pub const MASK_103: [u32; 4] = [0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF, 0x0000_007F];

/// `MaskFor`: only the bits for points actually being changed.
pub fn mask_for(deltas_khz: &[i32]) -> [u32; 4] {
    let mut m = [0u32; 4];
    for (i, &d) in deltas_khz.iter().enumerate().take(MASK_POINTS) {
        if d != 0 {
            m[i / 32] |= 1u32 << (i % 32);
        }
    }
    m
}

/// Bytes the curve call wants for the layout [`read_curve_raw`] builds.
pub const fn curve_table_size(gpu_entries: usize) -> usize {
    HEADER_BYTES + (gpu_entries + MEM_CURVE_ENTRIES) * ENTRY_BYTES + CURVE_TAIL_UINTS * 4
}

/// Bytes of the delta table for `gpu_entries`: header, the entries, then the fixed tail.
pub const fn delta_table_size(gpu_entries: usize) -> usize {
    HEADER_BYTES + gpu_entries * DELTA_ENTRY_BYTES + DELTA_TAIL_BYTES
}

/// Points the named struct fields reach (the "GPU" array plus the 23 after it): 103 for 80. A
/// property of the wrapper's declaration, not of the card — only the struct readers are bound by it.
pub const fn struct_points(gpu_entries: usize) -> usize {
    gpu_entries + MEM_CURVE_ENTRIES
}

/// `TotalPoints`: points the curve buffer really carries, which is what the raw readers and every
/// writer address. Bounded by the mask, since a point outside it can never be selected.
///
/// This used to be [`struct_points`], on the reasoning that a 4070 Ti's 103 points were "exactly
/// the mask's bit count". They were not: the mask is 128 bits and that card had 103 points. A
/// 5070 Ti returns 127 in the same buffer, one contiguous run from 450 to 1240 mV, so the split
/// reached 81% of it and left the top 25 slots unwritten on every reset.
pub const fn total_points(gpu_entries: usize) -> usize {
    let n = (curve_table_size(gpu_entries) - HEADER_BYTES) / ENTRY_BYTES;
    if n < MASK_POINTS { n } else { MASK_POINTS }
}

/// Delta entries the boost table holds: the mask's width, bounded by the buffer.
pub const fn delta_points(gpu_entries: usize) -> usize {
    let n = (delta_table_size(gpu_entries) - HEADER_BYTES) / DELTA_ENTRY_BYTES;
    if n < MASK_POINTS { n } else { MASK_POINTS }
}

/// Byte offset of point `i`'s frequency delta inside the boost table buffer.
///
/// One flat array, 36 bytes per entry, all the way. The struct this was modelled on splits after
/// 80 entries into two 23-entry arrays of bare ints, and writing points past 80 into those puts
/// them inside entry 80's record instead — the driver validates those fields and rejects the whole
/// call, taking the valid points with it. Verified by driving a known +150 MHz offset through the
/// table: 127 entries moved, every one at stride 0x24 + 20, nothing anywhere else.
pub const fn delta_offset(i: usize) -> usize {
    HEADER_BYTES + i * DELTA_ENTRY_BYTES + DELTA_FIELD
}

/// Which of the two trailing 23-int arrays the struct reader takes the deltas of points past the
/// GPU array from: 1 = the second ("MemoryDeltas"), 0 = the first ("MemoryFilled").
static TRAILING_ARRAY: AtomicI32 = AtomicI32::new(1);

/// `TrailingArray` (default 1). Which one the driver reads is decided by writing and checking the
/// curve moved.
pub fn trailing_array() -> i32 {
    TRAILING_ARRAY.load(Ordering::Relaxed)
}

/// Set `TrailingArray`.
pub fn set_trailing_array(v: i32) {
    TRAILING_ARRAY.store(v, Ordering::Relaxed);
}

/// Version word plus mask, the header every curve/table request starts with.
fn stage_header(b: &mut [u8], size: usize, version: u32, mask: &[u32; 4]) {
    wr_i32(b, 0, nvapi::version_word(size, version) as i32);
    for (i, &m) in mask.iter().enumerate() {
        wr_i32(b, 4 + i * 4, m as i32);
    }
}

fn curve_request(size: usize, version: u32, mask: &[u32; 4]) -> Buf {
    let mut b = Buf::new(size, CALL_SLACK);
    stage_header(b.bytes_mut(), size, version, mask);
    b
}

/// `CurveAvailable`: the private curve read entry points exist on this driver.
pub fn curve_available() -> bool {
    nvapi::exposes(id::GPU_GET_VFP_CURVE) && nvapi::exposes(id::GPU_GET_CLOCK_BOOST_TABLE)
}

/// `StructSizes`: `(curve bytes, boost-table bytes)` of the struct layout.
pub const fn struct_sizes() -> (usize, usize) {
    (VFP_CURVE_SIZE, BOOST_TABLE_SIZE)
}

/// Decode a struct-layout curve buffer: `(voltage µV, frequency kHz)` for the 103 struct points.
pub fn parse_curve_struct(b: &[u8]) -> Vec<(u32, i32)> {
    (0..struct_points(CURVE_ENTRIES))
        .map(|i| {
            let off = HEADER_BYTES + i * ENTRY_BYTES;
            (rd_u32(b, off + 8), rd_i32(b, off + 4))
        })
        .collect()
}

/// `ReadCurve`: the struct-layout reader with an explicit mask; `(voltage µV, frequency kHz)` per
/// slot, empty on failure.
///
/// Both regions, not just the "GPU" one: stopping at the 80-entry array truncated a 4070 Ti at
/// 945 mV instead of 1090. This reader still cannot see the whole curve — a 5070 Ti has 127 points
/// and this returns the first 103 — so it is a fallback for drivers that refuse the raw call;
/// anything measured against it (the stock ceiling, an undervolt target) comes out against the wrong
/// top. Prefer [`read_curve_raw`].
pub fn read_curve(h: GpuHandle, mask: &[u32; 4]) -> Vec<(u32, i32)> {
    let Some(f) = raw(id::GPU_GET_VFP_CURVE) else { return Vec::new() };
    let mut b = curve_request(VFP_CURVE_SIZE, 1, mask);
    if invoke(f, h, &mut b) != 0 {
        return Vec::new();
    }
    parse_curve_struct(b.bytes())
}

/// Decode a struct-layout boost table: the 80 GPU deltas, then the 23 past it from the trailing
/// array `trailing` selects.
pub fn parse_deltas_struct(b: &[u8], trailing: i32) -> Vec<i32> {
    let mut r: Vec<i32> = (0..DELTA_ENTRIES).map(|i| rd_i32(b, delta_offset(i))).collect();
    let base = if trailing == 1 { MEMORY_DELTAS } else { MEMORY_FILLED };
    r.extend((0..MEM_CURVE_ENTRIES).map(|i| rd_i32(b, base + i * 4)));
    r
}

/// `ReadDeltas`: per-point frequency deltas (kHz), struct layout, explicit mask. Empty on failure.
/// Struct-bounded like [`read_curve`].
pub fn read_deltas(h: GpuHandle, mask: &[u32; 4]) -> Vec<i32> {
    let Some(f) = raw(id::GPU_GET_CLOCK_BOOST_TABLE) else { return Vec::new() };
    let mut b = curve_request(BOOST_TABLE_SIZE, 1, mask);
    if invoke(f, h, &mut b) != 0 {
        return Vec::new();
    }
    parse_deltas_struct(b.bytes(), trailing_array())
}

/// `WriteDeltas`: write the first 80 deltas (kHz) into a fresh struct-layout table. Returns the
/// NVAPI status; -1 when the setter is not exported.
pub fn write_deltas(h: GpuHandle, mask: &[u32; 4], deltas_khz: &[i32]) -> i32 {
    let Some(f) = raw(id::GPU_SET_CLOCK_BOOST_TABLE) else { return -1 };
    let mut b = curve_request(BOOST_TABLE_SIZE, 1, mask);
    for (i, &d) in deltas_khz.iter().enumerate().take(DELTA_ENTRIES) {
        b.put(delta_offset(i), d);
    }
    invoke(f, h, &mut b)
}

// ---- raw layouts ---------------------------------------------------------------------------
//
// The V1 struct holds only 80 GPU points; an Ada card whose curve runs past that gets truncated
// (a 4070 Ti stops at 945 mV while boosting at ~1095 mV). These drive the calls with hand-built
// buffers so other entry counts and struct versions can be tried.

/// One curve-layout candidate from [`probe_curve_layouts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurveProbe {
    pub gpu_entries: usize,
    pub version: u32,
    pub status: i32,
    pub valid_points: usize,
    pub min_mv: i32,
    pub max_mv: i32,
    pub max_mhz: i32,
}

/// Decode a raw curve buffer of declared `size`: `(mV, MHz)` for every valid slot up to the mask
/// bound, stopping where the voltage steps back down.
pub fn parse_curve_raw(b: &[u8], size: usize) -> Vec<(i32, i32)> {
    // Parse every slot, GPU region and the region after it, so a curve that spills past the GPU
    // array shows up instead of being silently dropped.
    let total = MASK_POINTS.min(size.saturating_sub(HEADER_BYTES) / ENTRY_BYTES);
    let mut pts = Vec::with_capacity(total);
    let mut last_uv = 0;
    for i in 0..total {
        let off = HEADER_BYTES + i * ENTRY_BYTES;
        let freq_khz = rd_i32(b, off + 4);
        let volt_uv = rd_i32(b, off + 8);
        if volt_uv <= 0 || freq_khz <= 0 {
            continue;
        }
        // The buffer carries more than one clock domain. The graphics curve climbs the whole way,
        // so a point that steps back down in voltage is the next domain starting — on a 5070 Ti
        // that is slot 127, at 515 mV and the 405 MHz memory clock, right after the 1240 mV top.
        if volt_uv < last_uv {
            break;
        }
        last_uv = volt_uv;
        pts.push((volt_uv / 1000, freq_khz / 1000));
    }
    pts
}

/// `ReadCurveRaw` with the default trailing layout (23 entries, 1064-word tail).
pub fn read_curve_raw(h: GpuHandle, mask: &[u32; 4], gpu_entries: usize, version: u32) -> (i32, Vec<(i32, i32)>) {
    read_curve_raw_with(h, mask, gpu_entries, version, MEM_CURVE_ENTRIES, CURVE_TAIL_UINTS)
}

/// `ReadCurveRaw`: GetVFPCurve with a raw buffer sized for `gpu_entries` (+ `mem_entries`) points
/// and a `tail_uints` tail. Returns the status and the parsed `(mV, MHz)` points; -1 when the entry
/// point is not exported.
pub fn read_curve_raw_with(
    h: GpuHandle,
    mask: &[u32; 4],
    gpu_entries: usize,
    version: u32,
    mem_entries: usize,
    tail_uints: usize,
) -> (i32, Vec<(i32, i32)>) {
    let Some(f) = raw(id::GPU_GET_VFP_CURVE) else { return (-1, Vec::new()) };
    let size = HEADER_BYTES + (gpu_entries + mem_entries) * ENTRY_BYTES + tail_uints * 4;
    let mut b = curve_request(size, version, mask);
    let status = invoke(f, h, &mut b);
    if status != 0 {
        return (status, Vec::new());
    }
    (status, parse_curve_raw(b.bytes(), size))
}

/// `ProbeCurveLayouts`: try a spread of entry counts and versions and report what each returns.
pub fn probe_curve_layouts(h: GpuHandle) -> Vec<CurveProbe> {
    let mut results = Vec::new();
    for entries in [80usize, 103, 128, 160, 255] {
        for ver in [1u32, 2, 3] {
            let (status, pts) = read_curve_raw(h, &FULL_MASK, entries, ver);
            results.push(CurveProbe {
                gpu_entries: entries,
                version: ver,
                status,
                valid_points: pts.len(),
                min_mv: pts.iter().map(|p| p.0).min().unwrap_or(0),
                max_mv: pts.iter().map(|p| p.0).max().unwrap_or(0),
                max_mhz: pts.iter().map(|p| p.1).max().unwrap_or(0),
            });
        }
    }
    results
}

fn raw_table() -> Option<(RawFn, RawFn)> {
    Some((raw(id::GPU_GET_CLOCK_BOOST_TABLE)?, raw(id::GPU_SET_CLOCK_BOOST_TABLE)?))
}

/// Decode a raw delta table: one delta per point up to [`delta_points`].
pub fn parse_deltas_raw(b: &[u8], gpu_entries: usize) -> Vec<i32> {
    (0..delta_points(gpu_entries)).map(|i| rd_i32(b, delta_offset(i))).collect()
}

/// `ReadDeltasRaw`: per-point frequency deltas (kHz) for all points in a layout. -1 when either
/// table entry point is missing (the C# required both).
pub fn read_deltas_raw(h: GpuHandle, mask: &[u32; 4], gpu_entries: usize, version: u32) -> (i32, Vec<i32>) {
    let Some((get, _)) = raw_table() else { return (-1, Vec::new()) };
    let size = delta_table_size(gpu_entries);
    let mut b = curve_request(size, version, mask);
    let status = invoke(get, h, &mut b);
    if status != 0 {
        return (status, Vec::new());
    }
    (status, parse_deltas_raw(b.bytes(), gpu_entries))
}

/// Prepare a fetched table for writing: restore the header and mask (the getter may rewrite them),
/// put each delta at its flat offset, and — when `entry_flag` is `Some(f >= 0)` — stamp `f` into
/// the leading word of every changed GPU-region entry.
pub fn stage_deltas_raw(
    b: &mut [u8],
    mask: &[u32; 4],
    gpu_entries: usize,
    version: u32,
    deltas_khz: &[i32],
    entry_flag: Option<i32>,
) {
    stage_header(b, delta_table_size(gpu_entries), version, mask);
    for (i, &d) in deltas_khz.iter().enumerate().take(delta_points(gpu_entries)) {
        wr_i32(b, delta_offset(i), d);
        if let Some(flag) = entry_flag
            && flag >= 0
            && i < gpu_entries
            && d != 0
        {
            wr_i32(b, HEADER_BYTES + i * DELTA_ENTRY_BYTES, flag);
        }
    }
}

/// `WriteDeltasRaw`: write per-point frequency deltas (kHz). Read-modify-write: the current table
/// is fetched first and only the delta fields overwritten, so every flag and reserved field the
/// driver cares about goes back exactly as it gave them (the fetch's own status is ignored, as in
/// the C#). `entry_flag`: some driver branches treat an entry's leading word as "this entry is
/// valid" and ignore entries that leave it at 0. Returns the NVAPI status; -1 when unexported.
pub fn write_deltas_raw(
    h: GpuHandle,
    mask: &[u32; 4],
    gpu_entries: usize,
    version: u32,
    deltas_khz: &[i32],
    entry_flag: Option<i32>,
) -> i32 {
    let Some((get, set)) = raw_table() else { return -1 };
    let size = delta_table_size(gpu_entries);
    let mut b = curve_request(size, version, mask);
    // Pull the current table so unknown fields survive the round trip.
    invoke(get, h, &mut b);
    stage_deltas_raw(b.bytes_mut(), mask, gpu_entries, version, deltas_khz, entry_flag);
    invoke(set, h, &mut b)
}

/// Decode the two trailing 23-int arrays after the GPU region: `(first, second)`.
pub fn parse_trailing_arrays(b: &[u8], gpu_entries: usize) -> (Vec<i32>, Vec<i32>) {
    let end = HEADER_BYTES + gpu_entries * DELTA_ENTRY_BYTES;
    let a = (0..MEM_CURVE_ENTRIES).map(|i| rd_i32(b, end + i * 4)).collect();
    let second = (0..MEM_CURVE_ENTRIES).map(|i| rd_i32(b, end + MEM_CURVE_ENTRIES * 4 + i * 4)).collect();
    (a, second)
}

/// `ReadTrailingArrays`: raw dump of the two trailing arrays, to confirm where high-point deltas
/// live. Both empty on failure.
pub fn read_trailing_arrays(h: GpuHandle, mask: &[u32; 4], gpu_entries: usize, version: u32) -> (Vec<i32>, Vec<i32>) {
    let Some((get, _)) = raw_table() else { return (Vec::new(), Vec::new()) };
    let mut b = curve_request(delta_table_size(gpu_entries), version, mask);
    if invoke(get, h, &mut b) != 0 {
        return (Vec::new(), Vec::new());
    }
    parse_trailing_arrays(b.bytes(), gpu_entries)
}

// ================================================================== generic raw calls

/// `CallRaw`: one call with an explicit shape and mask; `(status, buffer as words)`. `(-1, [])`
/// when the entry point is not exported. The words come back even when the status is non-zero.
pub fn call_raw(
    h: GpuHandle,
    fid: u32,
    size: usize,
    version: u32,
    mask_offset: Option<usize>,
    mask: u32,
) -> (i32, Vec<i32>) {
    let Some(f) = raw(fid) else { return (-1, Vec::new()) };
    let mut b = request(size, CALL_SLACK, nvapi::version_word(size, version));
    if let Some(off) = mask_offset
        && off + 4 <= b.size()
    {
        b.put_u32(off, mask);
    }
    let status = invoke(f, h, &mut b);
    (status, b.words())
}

/// One accepted shape from [`sweep_shapes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapeHit {
    pub size: usize,
    pub version: u32,
    /// Up to `max_words` non-zero words past the version word, or `(all zero)`.
    pub words: String,
}

/// `SweepShapes`' default `maxWords`.
pub const SWEEP_MAX_WORDS: usize = 20;

/// Format a swept buffer's non-zero words past the version word.
pub fn sweep_words(b: &[u8], size: usize, max_words: usize) -> String {
    let words = nonzero_words(b, 0, 4, size, 3, max_words);
    if words.is_empty() { "(all zero)".into() } else { words.join(" ") }
}

/// `SweepShapes`: find the struct size an entry point wants by trying every size in
/// `min_size..=max_size` (step 4) for each version. A wrong size answers -9 and nothing else
/// happens, so the first status of 0 is the answer — which is how the crossbar's 0x61A4 was found.
/// Each attempt carries [`CALL_SLACK`] past the size being tried.
pub fn sweep_shapes(
    h: GpuHandle,
    fid: u32,
    min_size: usize,
    max_size: usize,
    versions: &[u32],
    max_words: usize,
) -> Vec<ShapeHit> {
    let mut hits = Vec::new();
    let Some(f) = raw(fid) else { return hits };
    for &version in versions {
        let mut size = min_size;
        while size <= max_size {
            let mut b = request(size, CALL_SLACK, nvapi::version_word(size, version));
            if invoke(f, h, &mut b) == 0 {
                hits.push(ShapeHit { size, version, words: sweep_words(b.bytes(), size, max_words) });
            }
            size += 4;
        }
    }
    hits
}

// ================================================================== voltage rails
//
// NVVDD (the core rail) and MSVDD (the rail feeding XBAR/SYS/video) are a separate private family
// from the V/F curve. Same trap: the calls succeed with a zeroed mask and return an empty
// structure, so the rail mask from GetInfo is written into the Status and Control buffers.
//
// Entry layouts were confirmed on hardware: MSVDD carried a -50000 µV primary-maximum offset
// against a 1055000 µV limit, and the tool that set it displayed exactly 1.005 V.

const FN_VOLT_RAILS_GET_INFO: u32 = 0x2C73_AFDC;
const FN_VOLT_RAILS_GET_STATUS: u32 = 0x5D06_34EE;
const FN_VOLT_RAILS_GET_CONTROL: u32 = 0xA307_0DB0;
const FN_VOLT_RAILS_SET_CONTROL: u32 = 0x87C5_5C8A;
pub const VOLT_RAILS_INFO_SIZE: usize = 0x184C;
pub const VOLT_RAILS_STATUS_SIZE: usize = 0x1620;
pub const VOLT_RAILS_CONTROL_SIZE: usize = 0x0AC8;
const VOLT_RAILS_VERSION: u32 = 2;
const RAIL_MASK_OFFSET: usize = 0x04;
pub const STATUS_ENTRIES: usize = 0xA0;
pub const STATUS_STRIDE: usize = 0xAC;
pub const CONTROL_ENTRIES: usize = 0x48;
pub const CONTROL_STRIDE: usize = 0x54;
/// Signed µV offset applied to the rail's primary maximum.
pub const CONTROL_MAX_OFFSET: usize = 0x04;
/// Signed µV offset applied to the rail's ALTERNATE maximum — the one the boost algorithm obeys.
/// Moving the primary alone raises the reported number while the card keeps selecting the old
/// voltage: primary at 1100 mV with the alternate at 1055 peaked at 1045 mV under load; raising
/// both reached 1095 mV.
pub const CONTROL_ALT_MAX_OFFSET: usize = 0x08;
/// Signed µV offset applied to the rail's minimum.
pub const CONTROL_MIN_OFFSET: usize = 0x10;

/// Rail index 0 in the mask: the core supply.
pub const RAIL_NVVDD: i32 = 0;
/// Rail index 1: the supply behind XBAR/SYS/video.
pub const RAIL_MSVDD: i32 = 1;

/// One power rail's live state, in microvolts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VoltRail {
    pub index: i32,
    pub current_uv: u32,
    pub max_uv: u32,
    pub min_uv: u32,
    pub max_offset_uv: i32,
    pub min_offset_uv: i32,
}

/// One rail call into a buffer of `size`; the declared bytes on success, `None` otherwise.
fn rail_call(h: GpuHandle, fid: u32, size: usize, mask: u32) -> Option<Vec<u8>> {
    let f = raw(fid)?;
    let mut b = request(size, CALL_SLACK, nvapi::version_word(size, VOLT_RAILS_VERSION));
    if mask != 0 {
        b.put_u32(RAIL_MASK_OFFSET, mask);
    }
    if invoke(f, h, &mut b) != 0 {
        return None;
    }
    Some(b.declared())
}

/// `ReadRailMask`: the mask of rails this GPU exposes; 0 when the family is unavailable.
pub fn read_rail_mask(h: GpuHandle) -> u32 {
    rail_call(h, FN_VOLT_RAILS_GET_INFO, VOLT_RAILS_INFO_SIZE, 0).map_or(0, |info| rd_u32(&info, RAIL_MASK_OFFSET))
}

/// Position of `rail` among the set bits of `mask` (the entry slot it occupies), `None` when the
/// rail is not in the mask or out of range.
pub fn rail_slot(mask: u32, rail: i32) -> Option<usize> {
    if !(0..32).contains(&rail) || mask & (1u32 << rail) == 0 {
        return None;
    }
    Some((mask & ((1u32 << rail) - 1)).count_ones() as usize)
}

/// Byte offset of `field` in control entry `slot`.
pub const fn rail_control_field(slot: usize, field: usize) -> usize {
    CONTROL_ENTRIES + slot * CONTROL_STRIDE + field
}

/// Decode the status (and, when present, control) buffers into one [`VoltRail`] per masked rail.
pub fn parse_volt_rails(mask: u32, status: &[u8], control: Option<&[u8]>) -> Vec<VoltRail> {
    let mut rails = Vec::new();
    let mut slot = 0usize;
    for r in 0..32 {
        if mask & (1u32 << r) == 0 {
            continue;
        }
        let s = STATUS_ENTRIES + slot * STATUS_STRIDE;
        let c = CONTROL_ENTRIES + slot * CONTROL_STRIDE;
        slot += 1;
        if s + STATUS_STRIDE > status.len() {
            break;
        }
        let ctl = control.filter(|ctl| c + CONTROL_STRIDE <= ctl.len());
        rails.push(VoltRail {
            index: r,
            current_uv: rd_u32(status, s + 0x04),
            max_uv: rd_u32(status, s + 0x08),
            min_uv: rd_u32(status, s + 0x18),
            max_offset_uv: ctl.map_or(0, |ctl| rd_i32(ctl, c + CONTROL_MAX_OFFSET)),
            min_offset_uv: ctl.map_or(0, |ctl| rd_i32(ctl, c + CONTROL_MIN_OFFSET)),
        });
    }
    rails
}

/// Every non-zero word of each masked rail's status entry, as `(rail, "+0xNN=v ...")`.
pub fn parse_rail_status_dump(mask: u32, status: &[u8]) -> Vec<(i32, String)> {
    let mut out = Vec::new();
    let mut slot = 0usize;
    for r in 0..32 {
        if mask & (1u32 << r) == 0 {
            continue;
        }
        let s = STATUS_ENTRIES + slot * STATUS_STRIDE;
        slot += 1;
        if s + STATUS_STRIDE > status.len() {
            break;
        }
        out.push((r, nonzero_words(status, s, 0, STATUS_STRIDE, 2, usize::MAX).join(" ")));
    }
    out
}

/// `DumpRailStatus`: every non-zero word of each rail's status entry, so a field that differs
/// between rails can be found rather than guessed. The first attempt at a per-rail voltage took
/// +0x04 and got the same number for both rails, which would have labelled the core voltage MSVDD.
pub fn dump_rail_status(h: GpuHandle) -> Vec<(i32, String)> {
    let mask = read_rail_mask(h);
    if mask == 0 {
        return Vec::new();
    }
    match rail_call(h, FN_VOLT_RAILS_GET_STATUS, VOLT_RAILS_STATUS_SIZE, mask) {
        Some(status) => parse_rail_status_dump(mask, &status),
        None => Vec::new(),
    }
}

/// `ReadVoltRails`: every rail the GPU reports. Empty when unsupported.
pub fn read_volt_rails(h: GpuHandle) -> Vec<VoltRail> {
    let mask = read_rail_mask(h);
    if mask == 0 {
        return Vec::new();
    }
    let status = rail_call(h, FN_VOLT_RAILS_GET_STATUS, VOLT_RAILS_STATUS_SIZE, mask);
    let control = rail_call(h, FN_VOLT_RAILS_GET_CONTROL, VOLT_RAILS_CONTROL_SIZE, mask);
    match status {
        Some(status) => parse_volt_rails(mask, &status, control.as_deref()),
        None => Vec::new(),
    }
}

/// Read-modify-write of the rail control block, setting `fields` (offsets within the rail's entry).
/// The fetch's status is ignored, as the C# did. -1 when the rail is absent or the setter missing.
fn write_rail_fields(h: GpuHandle, rail: i32, fields: &[usize], offset_uv: i32) -> i32 {
    let mask = read_rail_mask(h);
    let Some(slot) = rail_slot(mask, rail) else { return -1 };
    let Some(set) = raw(FN_VOLT_RAILS_SET_CONTROL) else { return -1 };
    let word = nvapi::version_word(VOLT_RAILS_CONTROL_SIZE, VOLT_RAILS_VERSION);
    let mut b = request(VOLT_RAILS_CONTROL_SIZE, CALL_SLACK, word);
    b.put_u32(RAIL_MASK_OFFSET, mask);
    if let Some(get) = raw(FN_VOLT_RAILS_GET_CONTROL) {
        invoke(get, h, &mut b);
    }
    b.put_u32(0, word);
    b.put_u32(RAIL_MASK_OFFSET, mask);
    for &field in fields {
        b.put(rail_control_field(slot, field), offset_uv);
    }
    invoke(set, h, &mut b)
}

/// `WriteVoltRailMinOffset`: signed µV offset on a rail's minimum, raising or lowering the floor it
/// may drop to. One field, unlike the maximum: verified by moving NVVDD's floor 800 → 850 mV and
/// back with MSVDD's floor unmoved. Returns the NVAPI status; -1 when unavailable.
pub fn write_volt_rail_min_offset(h: GpuHandle, rail: i32, offset_uv: i32) -> i32 {
    write_rail_fields(h, rail, &[CONTROL_MIN_OFFSET], offset_uv)
}

/// `WriteVoltRailMaxOffset`: signed µV offset on a rail's maximum, primary and alternate together —
/// the alternate is what binds, so moving one without the other changes only the reported ceiling.
/// Read-modify-write, so every reserved field the driver validates goes back as it arrived.
/// Returns the NVAPI status; -1 when unavailable.
pub fn write_volt_rail_max_offset(h: GpuHandle, rail: i32, offset_uv: i32) -> i32 {
    write_rail_fields(h, rail, &[CONTROL_MAX_OFFSET, CONTROL_ALT_MAX_OFFSET], offset_uv)
}

// ================================================================== OCP current limits
//
// The per-rail over-current protection limit: a monitor shows 300 A on NVVDD and 120 A on MSVDD;
// the reference tuning tool calls it Core/Mem OCP. Not in the voltage-rail family (all three of its
// buffers were scanned for those figures in every plausible unit). The layout was read out of the
// reference tool's own NVAPI.dll: NvApi_SetCoreOcpLimit / SetMemOcpLimit are thunks differing only
// in a selector (1 core, 0x10 memory) into one implementation that reads this family, checks the
// value against a min and max, and only then writes.
//
// The write id took two goes: pairing each stored result with the nearest preceding id in that
// tool's initialiser is off by one (the compiler schedules the next id's load before the previous
// store), which gave 0xEDCF624E — refused with INCOMPATIBLE_STRUCT_VERSION. The right pairing gives
// 0xAFFC2279, which the driver accepts. The fourth id, 0x67F31384, is the range call (0xA70 v4): a
// min, default and max per channel (250000/300000/350000 mA for core) and the live current as -1.

const FN_OCP_GET_CONTROL: u32 = 0x8B3E_7343;
const FN_OCP_SET_CONTROL: u32 = 0xAFFC_2279;
const FN_OCP_GET_RANGES: u32 = 0x67F3_1384;
pub const OCP_RANGE_SIZE: usize = 0x0A70;
const OCP_RANGE_VERSION: u32 = 4;
pub const OCP_SIZE: usize = 0x0A4C;
const OCP_VERSION: u32 = 1;
const OCP_MASK_OFFSET: usize = 0x04;
const OCP_MASK_ALL: u32 = 0x7FFF;
pub const OCP_ENTRIES: usize = 0x1C;
pub const OCP_STRIDE: usize = 0x28;
pub const OCP_ENTRY_VALUE: usize = 0x04;

/// The OCP table slot carrying the NVVDD limit.
pub const OCP_SLOT_NVVDD: i32 = 13;
/// The OCP table slot carrying the MSVDD limit.
pub const OCP_SLOT_MSVDD: i32 = 14;

/// Byte offset of `slot`'s limit (mA) in the legacy OCP control buffer.
pub const fn ocp_value_offset(slot: usize) -> usize {
    OCP_ENTRIES + slot * OCP_STRIDE + OCP_ENTRY_VALUE
}

/// `CallOcpBuffer`: run an OCP entry point on `data` (padded with slack), copying the result back
/// only on success. -3 (NVAPI_NO_IMPLEMENTATION) when the id is not exported.
fn call_ocp_buffer(h: GpuHandle, fid: u32, data: &mut [u8]) -> i32 {
    match raw(fid) {
        None => -3,
        Some(f) => call_on(f, h, data, false),
    }
}

/// `WriteOcpLimit`: write one OCP channel's limit, in milliamps. `None` on success, else the reason.
///
/// Read-modify-write against the buffer the getter fills, as the reference tool does: every other
/// channel keeps what it had. Measured on a 5070 Ti: NVVDD 300 A → 290 A read back as 290, and back.
/// The driver bounds it to its own window and refuses anything outside. On R615+ (driver >= 61500)
/// a changed limit goes through the modern full-payload layout ([`ocp::modern_apply`]); a failed
/// modern write is never retried with the legacy layout.
pub fn write_ocp_limit(h: GpuHandle, slot: i32, milliamps: i32) -> Option<String> {
    let mut b = request(OCP_SIZE, CALL_SLACK, nvapi::version_word(OCP_SIZE, OCP_VERSION));
    b.put_u32(OCP_MASK_OFFSET, OCP_MASK_ALL);
    let Some(get) = raw(FN_OCP_GET_CONTROL) else {
        return Some("the driver does not export the OCP read entry point".into());
    };
    let st = invoke(get, h, &mut b);
    if st != 0 {
        return Some(format!("reading the OCP limits first: status {st}"));
    }
    if slot != OCP_SLOT_NVVDD && slot != OCP_SLOT_MSVDD {
        return Some(format!("Unsupported OCP slot {slot}; no write attempted"));
    }
    let at = ocp_value_offset(slot as usize);
    let current_ma = b.i32(at);
    let driver = nvapi::driver_version().unwrap_or(0);
    if driver >= 61500 && current_ma != milliamps {
        // R615+ accepts legacy reads but needs the full modern control layout for writes.
        return ocp::modern_apply(
            slot,
            milliamps,
            |d| call_ocp_buffer(h, FN_OCP_GET_CONTROL, d),
            |d| call_ocp_buffer(h, FN_OCP_GET_RANGES, d),
            |d| call_ocp_buffer(h, FN_OCP_SET_CONTROL, d),
        );
    }
    ocp::legacy_apply(driver, current_ma, milliamps, || {
        let Some(set) = raw(FN_OCP_SET_CONTROL) else { return -3 }; // NVAPI_NO_IMPLEMENTATION
        b.put(at, milliamps);
        invoke(set, h, &mut b)
    })
}

/// One OCP channel: its selector, the limit it carries, and the entry's other words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcpChannel {
    pub slot: i32,
    pub selector: i32,
    pub value: i32,
    pub words: String,
}

/// Decode legacy OCP control words into every non-empty channel.
pub fn parse_ocp_channels(w: &[i32]) -> Vec<OcpChannel> {
    let mut out = Vec::new();
    let mut slot = 0usize;
    loop {
        let b = OCP_ENTRIES + slot * OCP_STRIDE;
        if (b + OCP_STRIDE) / 4 >= w.len() {
            break;
        }
        let words: Vec<String> = (0..OCP_STRIDE)
            .step_by(4)
            .filter_map(|off| {
                let v = w[(b + off) / 4];
                (v != 0).then(|| format!("+0x{off:02X}={v}"))
            })
            .collect();
        if !words.is_empty() {
            out.push(OcpChannel {
                slot: slot as i32,
                selector: w[b / 4],
                value: w[(b + OCP_ENTRY_VALUE) / 4],
                words: words.join(" "),
            });
        }
        slot += 1;
    }
    out
}

/// `ReadOcpChannels`: every OCP channel this card reports; empty when unavailable. Read-only — the
/// same call the reference tool's setter makes on the way in.
pub fn read_ocp_channels(h: GpuHandle) -> Vec<OcpChannel> {
    let (status, w) = call_raw(h, FN_OCP_GET_CONTROL, OCP_SIZE, OCP_VERSION, Some(OCP_MASK_OFFSET), OCP_MASK_ALL);
    if status != 0 || w.is_empty() {
        return Vec::new();
    }
    parse_ocp_channels(&w)
}

/// One OCP channel's range, in milliamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OcpRange {
    pub min_ma: i32,
    pub value_ma: i32,
    pub max_ma: i32,
}

/// Find ranges by shape rather than an assumed stride: a minimum, a value and a maximum in
/// consecutive words followed by the live-current word the driver fills with -1. Three ascending
/// numbers are common in a buffer this size; three followed by that sentinel are not.
pub fn parse_ocp_ranges(w: &[i32]) -> Vec<OcpRange> {
    w.windows(4)
        .filter_map(|q| {
            let (min, val, max, live) = (q[0], q[1], q[2], q[3]);
            if live != -1 || min < 1 || val <= min || max <= val || max > 10_000_000 {
                return None; // no sentinel, not ascending, or a counter rather than amps
            }
            Some(OcpRange { min_ma: min, value_ma: val, max_ma: max })
        })
        .collect()
}

/// `ReadOcpRanges`: the per-channel OCP ranges the driver reports. Every card reports its own — the
/// window is a property of the board's power stage, not the model.
pub fn read_ocp_ranges(h: GpuHandle) -> Vec<OcpRange> {
    let (st, w) =
        call_raw(h, FN_OCP_GET_RANGES, OCP_RANGE_SIZE, OCP_RANGE_VERSION, Some(OCP_MASK_OFFSET), OCP_MASK_ALL);
    if st != 0 || w.is_empty() {
        return Vec::new();
    }
    parse_ocp_ranges(&w)
}

/// `RangeFor`: the tightest range offered for a channel currently at `limit_ma`.
///
/// A limit can appear in more than one entry — a 5070 Ti's core one shows as 250000..350000 and as
/// 1..5001000, the second being the driver declining to constrain it. A range running past three
/// times the limit is skipped for that reason, and the narrowest of the rest wins.
pub fn range_for(ranges: &[OcpRange], limit_ma: i32) -> Option<OcpRange> {
    let mut best: Option<OcpRange> = None;
    for r in ranges {
        if r.value_ma != limit_ma || r.max_ma > limit_ma.wrapping_mul(3) {
            continue;
        }
        let width = r.max_ma.wrapping_sub(r.min_ma);
        if best.is_none_or(|b| width < b.max_ma.wrapping_sub(b.min_ma)) {
            best = Some(*r);
        }
    }
    best
}

// ================================================================== power monitor
//
// The family the reference tool calls for its rail snapshots, at the two shapes it uses, short one
// first (sizes and versions from its call sites — sweeping sizes here is what corrupted the heap
// twice). Current is at +0x00 in mA and voltage at +0x04 in µV: the first channel reads 1782 mA at
// 11891134 µV = 21.19 W, exactly the 21190 the struct carries as the board total.
//
// Seven channels on a 5070 Ti, all on the 12 V side; they match a comparison monitor one for one.
// The word at +0x04 of the request is a bitmask, and bit 6 is invalid (every mask containing it is
// refused; bits from 8 up are refused too), so 0xBF is every valid channel. The sub-volt channels
// (NVVDD output, MSVDD — what an OCP limit guards) are in neither this family nor the ADC one.

const POWER_CHANNEL_MASK: u32 = 0xBF;
const FN_POWER_MONITOR_STATUS: u32 = 0xF402_38EF;
/// `(size, first entry, stride)` of the two shapes, in the order tried.
pub const POWER_SHAPES: [(usize, usize, usize); 2] = [(0x059C, 0x28, 0x2C), (0x24D8, 0x5C, 0xD8)];

/// One power-monitor channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerRail {
    pub slot: i32,
    pub milliamps: i32,
    pub microvolts: i32,
}

/// Decode one power-monitor shape's words: every channel with a non-negative, non-empty reading.
pub fn parse_power_rails(w: &[i32], entry0: usize, stride: usize) -> Vec<PowerRail> {
    let mut out = Vec::new();
    for slot in 0..32 {
        let b = entry0 + slot * stride;
        if (b + stride) / 4 >= w.len() {
            break;
        }
        let (ma, uv) = (w[b / 4], w[(b + 4) / 4]);
        if (ma == 0 && uv == 0) || ma < 0 || uv < 0 {
            continue;
        }
        out.push(PowerRail { slot: slot as i32, milliamps: ma, microvolts: uv });
    }
    out
}

/// `ReadPowerRails`: every power-monitor channel, decoded. Read-only. Empty when neither shape
/// answers with a channel.
pub fn read_power_rails(h: GpuHandle) -> Vec<PowerRail> {
    for (size, entry0, stride) in POWER_SHAPES {
        let (st, w) = call_raw(h, FN_POWER_MONITOR_STATUS, size, 1, Some(0x04), POWER_CHANNEL_MASK);
        if st != 0 || w.is_empty() {
            continue;
        }
        let out = parse_power_rails(&w, entry0, stride);
        if !out.is_empty() {
            return out;
        }
    }
    Vec::new()
}

// ================================================================== rail voltages (ADC)
//
// The only family that reads the rails apart. The rail status call reports the same figure for
// every rail (810 mV for both on a 5070 Ti); this one gives MSVDD pinned at 800.000 mV and NVVDD
// moving below its VID (measured droop). Empty until a mask is written at +0x04.

const FN_VOLT_ADC: u32 = 0x43D9_B26A;
pub const ADC_SIZE: usize = 0x340;
const ADC_MASK_OFFSET: usize = 0x04;
pub const ADC_ENTRY0: usize = 0x048;
pub const ADC_STRIDE: usize = 0x4C;
const ADC_VOLTAGE: usize = 0x04;
/// Which channel is which rail — NOT mask order, which gets them backwards. Settled by forcing each
/// rail's floor to 900 mV and watching which channel moved (channel 0 followed MSVDD).
pub const ADC_CHANNEL_MSVDD: usize = 0;
pub const ADC_CHANNEL_NVVDD: usize = 1;

/// Decode an ADC buffer into µV per rail index ([`RAIL_NVVDD`]/[`RAIL_MSVDD`]).
pub fn parse_rail_voltages(b: &[u8]) -> BTreeMap<i32, u32> {
    let mut found = BTreeMap::new();
    for (channel, rail) in [(ADC_CHANNEL_MSVDD, RAIL_MSVDD), (ADC_CHANNEL_NVVDD, RAIL_NVVDD)] {
        let off = ADC_ENTRY0 + channel * ADC_STRIDE + ADC_VOLTAGE;
        if off + 4 > ADC_SIZE {
            continue;
        }
        let uv = rd_u32(b, off);
        if uv > 0 && uv < 2_000_000 {
            found.insert(rail, uv);
        }
    }
    found
}

/// `ReadRailVoltagesUv`: measured voltage per rail index, in µV. Empty when the family is absent.
pub fn read_rail_voltages_uv(h: GpuHandle) -> BTreeMap<i32, u32> {
    let Some(f) = raw(FN_VOLT_ADC) else { return BTreeMap::new() };
    let mask = read_rail_mask(h);
    if mask == 0 {
        return BTreeMap::new();
    }
    let mut b = request(ADC_SIZE, CALL_SLACK, nvapi::version_word(ADC_SIZE, 1));
    b.put_u32(ADC_MASK_OFFSET, mask);
    if invoke(f, h, &mut b) != 0 {
        return BTreeMap::new();
    }
    parse_rail_voltages(b.bytes())
}

// ================================================================== clock domains (XBAR/SYS/video)
//
// The crossbar is the GPU's interconnect clock, which NVAPI's public surface does not expose: it is
// not a PStates20 domain (writes to every id but graphics and memory are refused) and is not in the
// clock-domain enumeration. It has its own control family and its own hardware frequency counter
// ([`measure_clock_khz`]), which makes a write verifiable rather than merely acknowledged.

const FN_XBAR_GET_INFO: u32 = 0x57B5_A5DF;
const FN_XBAR_GET_CONTROL: u32 = 0xF589_38F5;
const FN_XBAR_SET_CONTROL: u32 = 0xD14B_69CF;
const FN_XBAR_MEASURE: u32 = 0x527F_C458;
pub const XBAR_INFO_SIZE: usize = 0x86AC;
pub const XBAR_CONTROL_SIZE: usize = 0x61A4;
const XBAR_MEASURE_SIZE: usize = 0x000C;
const XBAR_INFO_VERSION: u32 = 0x0004_86AC;
const XBAR_CONTROL_VERSION: u32 = 0x0002_61A4;
const XBAR_MEASURE_VERSION: u32 = 0x0001_000C;
pub const XBAR_INFO_ENTRIES: usize = 0xB0;
pub const XBAR_INFO_STRIDE: usize = 0x430;
/// Window searched for the int16 offset-range pair inside an info entry.
const XBAR_RANGE_SEARCH_START: usize = 0x38;
const XBAR_RANGE_SEARCH_END: usize = 0x40;
/// The control request carries a domain selector here; it comes back empty without it.
const XBAR_CONTROL_SELECTOR: usize = 0x08;
/// Block layout of the control buffer; see [`control_offset_for`].
const XBAR_CONTROL_BLOCK0: usize = 0x0124;
const XBAR_CONTROL_BLOCK_STRIDE: usize = 0x304;
const XBAR_CONTROL_OFFSET_IN_BLOCK: usize = 0x114;

const _: () = assert!(XBAR_CONTROL_SIZE == domain_offset::SIZE);
const _: () = assert!(XBAR_CONTROL_VERSION as i32 == domain_offset::VERSION);

/// Clock domain index for [`measure_clock_khz`]: graphics. Confirmed on hardware.
pub const DOMAIN_CORE: i32 = 0;
/// Crossbar.
pub const DOMAIN_XBAR: i32 = 1;
/// Memory.
pub const DOMAIN_MEMORY: i32 = 4;

/// Slots in the info list, which are also the domains this family offsets (and the selector bit).
pub const SLOT_CORE: i32 = 0;
pub const SLOT_XBAR: i32 = 1;
pub const SLOT_SYS: i32 = 3;
pub const SLOT_VIDEO: i32 = 4;

/// Crossbar offset range the driver reports; `supported` is false when the family is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct XbarInfo {
    pub supported: bool,
    pub min_offset_mhz: i32,
    pub max_offset_mhz: i32,
}

/// One clock domain as the info call describes it. A domain with a range is one the driver will
/// discuss; whether it accepts a write is a separate question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DomainEntry {
    pub index: i32,
    pub kind: i32,
    pub has_range: bool,
    pub min_mhz: i32,
    pub max_mhz: i32,
}

/// One call into a buffer of `size` with a raw version word and an optional selector at +0x08;
/// the declared bytes on success.
fn xbar_call(h: GpuHandle, fid: u32, size: usize, version: u32, selector: Option<i32>) -> Option<Vec<u8>> {
    let f = raw(fid)?;
    let mut b = request(size, CALL_SLACK, version);
    if let Some(s) = selector {
        b.put(XBAR_CONTROL_SELECTOR, s);
    }
    if invoke(f, h, &mut b) != 0 {
        return None;
    }
    Some(b.declared())
}

/// `MeasureClockKhz`: a clock domain measured by the GPU's own counter, in kHz; 0 when
/// unavailable. What the domain really runs, not the setpoint it was asked for.
pub fn measure_clock_khz(h: GpuHandle, domain: i32) -> u32 {
    let Some(f) = raw(FN_XBAR_MEASURE) else { return 0 };
    let mut b = request(XBAR_MEASURE_SIZE, CALL_SLACK, XBAR_MEASURE_VERSION);
    b.put(0x04, domain);
    if invoke(f, h, &mut b) == 0 { b.u32(0x08) } else { 0 }
}

/// The int16 offset-range pair inside the info entry at `e`, if one looks like a range.
///
/// Its offset is not the same on every driver: a 5070 Ti (610.88) carries it at +0x3C, a 4070 Ti
/// (591.86) at +0x3A, both reading -1000..+1000. Reading one fixed offset found (1000, 0) on Ada and
/// reported a working crossbar as unsupported, so the pair is located instead. A candidate must look
/// like an offset range — negative floor, positive ceiling, neither past ±2000 — which the
/// neighbouring words (3598, 1024, 258, 15000) cannot pass for.
fn find_range(info: &[u8], e: usize) -> Option<(i32, i32)> {
    (XBAR_RANGE_SEARCH_START..=XBAR_RANGE_SEARCH_END).step_by(2).find_map(|off| {
        let lo = rd_i16(info, e + off);
        let hi = rd_i16(info, e + off + 2);
        (lo < 0 && hi > 0 && lo >= -2000 && hi <= 2000).then_some((lo as i32, hi as i32))
    })
}

/// Decode the info buffer's crossbar entry (found by type 1, not by index).
pub fn parse_xbar_info(info: &[u8]) -> XbarInfo {
    for i in 0..32 {
        let e = XBAR_INFO_ENTRIES + i * XBAR_INFO_STRIDE;
        if e + XBAR_RANGE_SEARCH_END + 4 > info.len() {
            break;
        }
        if rd_u32(info, e) != 1 {
            continue;
        }
        // Right entry, unrecognised layout: say so rather than guess a range.
        return match find_range(info, e) {
            Some((min, max)) => XbarInfo { supported: true, min_offset_mhz: min, max_offset_mhz: max },
            None => XbarInfo::default(),
        };
    }
    XbarInfo::default()
}

/// `ReadXbarInfo`: the crossbar offset range.
pub fn read_xbar_info(h: GpuHandle) -> XbarInfo {
    match xbar_call(h, FN_XBAR_GET_INFO, XBAR_INFO_SIZE, XBAR_INFO_VERSION, None) {
        Some(info) => parse_xbar_info(&info),
        None => XbarInfo::default(),
    }
}

/// Decode every domain the info call lists (the crossbar reader's walk without its type filter).
pub fn parse_domain_entries(info: &[u8]) -> Vec<DomainEntry> {
    let mut found = Vec::new();
    for i in 0..32 {
        let e = XBAR_INFO_ENTRIES + i * XBAR_INFO_STRIDE;
        if e + XBAR_RANGE_SEARCH_END + 4 > info.len() {
            break;
        }
        let kind = rd_i32(info, e);
        if kind == 0 && i > 0 {
            continue; // an empty slot, not a domain numbered zero
        }
        let range = find_range(info, e);
        found.push(DomainEntry {
            index: i as i32,
            kind,
            has_range: range.is_some(),
            min_mhz: range.map_or(0, |r| r.0),
            max_mhz: range.map_or(0, |r| r.1),
        });
    }
    found
}

/// `ReadDomainEntries`: every domain the info call lists, with its range.
pub fn read_domain_entries(h: GpuHandle) -> Vec<DomainEntry> {
    match xbar_call(h, FN_XBAR_GET_INFO, XBAR_INFO_SIZE, XBAR_INFO_VERSION, None) {
        Some(info) => parse_domain_entries(&info),
        None => Vec::new(),
    }
}

/// `ControlOffsetFor`: where a domain's offset sits in the control buffer on the layout the
/// crossbar confirmed — one block per slot at `0x124 + n * 0x304`, the offset `0x114` into it. Slot
/// 1 gives 0x53C, the one word that moved 0 → 30000 when a +30 MHz crossbar offset was applied.
/// (The actual reader/writer go through [`domain_offset::offset_field`], which also knows the
/// +0x10C layout.)
pub const fn control_offset_for(entry_index: usize) -> usize {
    XBAR_CONTROL_BLOCK0 + entry_index * XBAR_CONTROL_BLOCK_STRIDE + XBAR_CONTROL_OFFSET_IN_BLOCK
}

/// `ReadDomainOffsetMhz`: the offset a domain currently carries, in MHz; 0 when unreadable.
pub fn read_domain_offset_mhz(h: GpuHandle, slot: i32) -> i32 {
    if !(0..32).contains(&slot) {
        return 0;
    }
    let Some(c) = xbar_call(h, FN_XBAR_GET_CONTROL, XBAR_CONTROL_SIZE, XBAR_CONTROL_VERSION, Some(1 << slot)) else {
        return 0;
    };
    let field = domain_offset::offset_field(&c, slot);
    if field < 0 { 0 } else { rd_i32(&c, field as usize) / 1000 }
}

/// `ReadXbarOffsetMhz`.
pub fn read_xbar_offset_mhz(h: GpuHandle) -> i32 {
    read_domain_offset_mhz(h, SLOT_XBAR)
}

/// `WriteDomainOffsetMhz`: offset one clock domain, in MHz, through [`domain_offset::apply`]
/// (read-modify-write-verify; the driver refuses a request whose surrounding fields it did not fill
/// itself). Returns the status rather than failing: -1 here means refused, which on some cards is
/// the honest answer for a domain that reports a range and will not move.
pub fn write_domain_offset_mhz(h: GpuHandle, slot: i32, mhz: i32) -> i32 {
    let (Some(set), Some(get)) = (raw(FN_XBAR_SET_CONTROL), raw(FN_XBAR_GET_CONTROL)) else { return -1 };
    domain_offset::apply(slot, mhz, |b| call_on(get, h, b, true), |b| call_on(set, h, b, true))
}

/// `WriteXbarOffsetMhz`.
pub fn write_xbar_offset_mhz(h: GpuHandle, mhz: i32) -> i32 {
    write_domain_offset_mhz(h, SLOT_XBAR, mhz)
}

// ---- read-only domain probes (diagnostics) ---------------------------------------------------

/// `ProbeCall`: a read-only call with a deliberately oversized allocation, so a driver that writes
/// more than declared lands in our own memory. `(status, declared bytes)`; `None` when unexported.
fn probe_call(h: GpuHandle, fid: u32, declared: usize, version: u32, selector: Option<i32>) -> Option<(i32, Vec<u8>)> {
    let f = raw(fid)?;
    let mut b = request(declared, PROBE_SLACK, version);
    if let Some(s) = selector {
        b.put(XBAR_CONTROL_SELECTOR, s);
    }
    let status = invoke(f, h, &mut b);
    Some((status, b.declared()))
}

/// Summary line plus non-zero words of one info entry (see [`dump_domain_info`]).
pub fn format_domain_info(info: &[u8], slot: i32) -> Vec<String> {
    let e = XBAR_INFO_ENTRIES.wrapping_add((slot as usize).wrapping_mul(XBAR_INFO_STRIDE));
    if slot < 0 || e + XBAR_INFO_STRIDE > info.len() {
        return vec![format!("slot {slot} entry runs past the buffer")];
    }
    let kind = rd_i32(info, e);
    let mut words = Vec::new();
    let (mut non_zero, mut longest, mut run) = (0, 0, 0);
    for o in (0..XBAR_INFO_STRIDE).step_by(4) {
        let w = rd_i32(info, e + o);
        if w != 0 {
            non_zero += 1;
            run += 1;
            longest = longest.max(run);
            if words.len() < 24 {
                words.push(format!("+0x{o:03X}={w}"));
            }
        } else {
            run = 0;
        }
    }
    vec![
        format!("slot {slot} type={kind} nonZero={non_zero}/{} longestRun={longest}", XBAR_INFO_STRIDE / 4),
        format!("  {}", if words.is_empty() { "(all zero)".to_string() } else { words.join(" ") }),
    ]
}

/// `DumpDomainInfo`: one domain's 0x430-byte info entry, read-only. Room for a per-point table,
/// which is what a crossbar curve would need; long runs of similar values are the shape to look for.
pub fn dump_domain_info(h: GpuHandle, slot: i32) -> Vec<String> {
    match xbar_call(h, FN_XBAR_GET_INFO, XBAR_INFO_SIZE, XBAR_INFO_VERSION, None) {
        Some(info) => format_domain_info(&info, slot),
        None => vec!["GetInfo returned nothing".into()],
    }
}

/// `DumpDomainBlock`: every word of one domain's control block, read-only and over-allocated. The
/// reference tool keeps a 128-entry xbar_curve_points array beside its curve points; this shows
/// whether such a table is in the block the flat offset is written into.
pub fn dump_domain_block(h: GpuHandle, slot: i32) -> Vec<String> {
    let Some(f) = raw(FN_XBAR_GET_CONTROL) else { return vec!["GetControl unresolved".into()] };
    if !(0..32).contains(&slot) {
        return vec![format!("slot {slot} is out of range")];
    }
    let mut b = request(XBAR_CONTROL_SIZE, PROBE_SLACK, XBAR_CONTROL_VERSION);
    b.put(XBAR_CONTROL_SELECTOR, 1 << slot);
    let status = invoke(f, h, &mut b);
    let start = XBAR_CONTROL_BLOCK0 + slot as usize * XBAR_CONTROL_BLOCK_STRIDE;
    let words = nonzero_words(b.bytes(), start, 0, XBAR_CONTROL_BLOCK_STRIDE, 3, usize::MAX);
    vec![
        format!(
            "slot {slot} selector=0x{:X} status={status} block at +0x{start:04X}, {XBAR_CONTROL_BLOCK_STRIDE} bytes",
            1 << slot
        ),
        if words.is_empty() { "  block is all zero".into() } else { format!("  {}", words.join(" ")) },
    ]
}

/// One row of [`explore_xbar_control`]: `status` is `i32::MIN` when unresolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlExplore {
    pub label: String,
    pub status: i32,
    pub non_zero: usize,
    pub head: String,
}

/// `ExploreXbarControl`: read-only — call GetControl with different mask/selector shapes and report
/// how much of the buffer the driver fills. The voltage-rail request only returns data with a mask;
/// this checks whether this family wants one too. GetControl only.
pub fn explore_xbar_control(h: GpuHandle) -> Vec<ControlExplore> {
    let Some(f) = raw(FN_XBAR_GET_CONTROL) else {
        return vec![ControlExplore {
            label: "GetControl unresolved".into(),
            status: i32::MIN,
            non_zero: 0,
            head: String::new(),
        }];
    };
    let tries: [(&str, Option<u32>, Option<i32>); 10] = [
        ("selector=2, no mask (current)", None, Some(2)),
        ("selector=2, mask=0x02 at +0x04", Some(0x02), Some(2)),
        ("selector=2, mask=0xFFFFFFFF", Some(0xFFFF_FFFF), Some(2)),
        ("no selector, mask=0x02", Some(0x02), None),
        ("selector=1, mask=0x02", Some(0x02), Some(1)),
        ("selector=3, mask=0x02", Some(0x02), Some(3)),
        ("selector=2, mask=0x01", Some(0x01), Some(2)),
        // The selector is a bitmask of sub-structures rather than a magic 2: bit 0 fills a count at
        // +0x0124, bit 1 one at +0x0428, and 3 fills both. Ask for everything.
        ("selector=0xFF", Some(0x02), Some(0xFF)),
        ("selector=0xFFFF", Some(0x02), Some(0xFFFF)),
        ("selector=0xFFFFFFF", Some(0x02), Some(0x0FFF_FFFF)),
    ];
    tries
        .into_iter()
        .map(|(label, mask, selector)| {
            let mut b = request(XBAR_CONTROL_SIZE, PROBE_SLACK, XBAR_CONTROL_VERSION);
            if let Some(m) = mask {
                b.put_u32(0x04, m);
            }
            if let Some(s) = selector {
                b.put(XBAR_CONTROL_SELECTOR, s);
            }
            let status = invoke(f, h, &mut b);
            let bytes = b.bytes();
            let non_zero = (0..XBAR_CONTROL_SIZE / 4).filter(|i| rd_i32(bytes, i * 4) != 0).count();
            let head = nonzero_words(bytes, 0, 0, XBAR_CONTROL_SIZE, 4, 10).join(" ");
            ControlExplore { label: label.into(), status, non_zero, head }
        })
        .collect()
}

/// `ProbeXbarControlShapes`: read-only — which (size, version) shapes GetControl accepts. A wrong
/// version answers -9 and wrong content -1, so this detects a different outer structure on this
/// driver build; it does not validate inner offsets (the block tag selects +0x10C or +0x114, see
/// [`domain_offset`]). `(label, status)`, `i32::MIN` when unresolved.
pub fn probe_xbar_control_shapes(h: GpuHandle) -> Vec<(String, i32)> {
    let Some(f) = raw(FN_XBAR_GET_CONTROL) else { return vec![("GetControl unresolved".into(), i32::MIN)] };
    let mut shapes: Vec<(usize, u32)> = [1u32, 2, 3, 4].iter().map(|&v| (XBAR_CONTROL_SIZE, v)).collect();
    // Sizes either side, and ones a differently-sized block array would produce.
    shapes.extend([0x61A4 - 0x304, 0x61A4 + 0x304, 0x4000, 0x5000, 0x7000, 0x86AC].iter().map(|&s| (s, 2)));
    shapes
        .into_iter()
        .map(|(size, ver)| {
            let mut b = request(size, PROBE_SLACK, nvapi::version_word(size, ver));
            b.put(XBAR_CONTROL_SELECTOR, 2);
            (format!("0x{size:04X} x v{ver}"), invoke(f, h, &mut b))
        })
        .collect()
}

/// `XbarProbe`: why the crossbar came back unsupported — entry points missing, a status, or a
/// layout with no type-1 entry. Statuses are `i32::MIN` when the entry point did not resolve.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct XbarProbe {
    pub info_resolved: bool,
    pub control_resolved: bool,
    pub set_resolved: bool,
    pub measure_resolved: bool,
    pub info_status: i32,
    pub control_status: i32,
    pub core_khz: u32,
    pub xbar_khz: u32,
    pub memory_khz: u32,
    /// The type word of each info entry.
    pub entry_types: Vec<i32>,
    /// Index of the first type-1 entry (`TypeOneIndex`, -1 in the C# when absent).
    pub type_one_index: Option<usize>,
    /// The first 0x80 bytes of that entry, as words.
    pub type_one_words: Vec<i32>,
    /// `(offset, word)` pairs flattened, for every non-zero control word (at most 120 pairs).
    pub control_words: Vec<i32>,
}

/// Fill the buffer-derived parts of an [`XbarProbe`]: the info entry type words, the first type-1
/// entry's leading words, and the non-zero control words (`(offset, value)` flattened, capped at
/// 240 numbers). `info`/`ctrl` are the buffers of calls that returned 0.
pub fn parse_xbar_probe(info: Option<&[u8]>, ctrl: Option<&[u8]>) -> XbarProbe {
    let mut p = XbarProbe::default();
    if let Some(d) = info {
        for i in 0..32 {
            let e = XBAR_INFO_ENTRIES + i * XBAR_INFO_STRIDE;
            if e + 4 > d.len() {
                break;
            }
            p.entry_types.push(rd_i32(d, e));
        }
    }
    // The reader locates the range at a fixed offset inside the type-1 entry; dump its leading
    // words so the actual layout can be read off rather than assumed.
    p.type_one_index = p.entry_types.iter().position(|&t| t == 1);
    if let (Some(idx), Some(d)) = (p.type_one_index, info) {
        let e = XBAR_INFO_ENTRIES + idx * XBAR_INFO_STRIDE;
        let mut off = 0;
        while off < 0x80 && e + off + 4 <= d.len() {
            p.type_one_words.push(rd_i32(d, e + off));
            off += 4;
        }
    }
    // If the control layout has moved, a non-zero write lands in a field the driver validates and
    // the whole call is refused; record where the buffer is NOT zero so the structure shows itself.
    if let Some(d) = ctrl {
        let mut off = 0;
        while off + 4 <= d.len() {
            let w = rd_i32(d, off);
            if w != 0 {
                p.control_words.push(off as i32);
                p.control_words.push(w);
            }
            if p.control_words.len() >= 240 {
                break;
            }
            off += 4;
        }
    }
    p
}

/// `ProbeXbar`: read-only — GetInfo, GetControl and the frequency counter, never SetControl.
pub fn probe_xbar(h: GpuHandle) -> XbarProbe {
    let info = probe_call(h, FN_XBAR_GET_INFO, XBAR_INFO_SIZE, XBAR_INFO_VERSION, None);
    let ctrl = probe_call(h, FN_XBAR_GET_CONTROL, XBAR_CONTROL_SIZE, XBAR_CONTROL_VERSION, Some(2));
    let ok = |r: &Option<(i32, Vec<u8>)>| r.as_ref().filter(|(s, _)| *s == 0).map(|(_, d)| d.clone());
    let (info_ok, ctrl_ok) = (ok(&info), ok(&ctrl));
    XbarProbe {
        info_resolved: nvapi::exposes(FN_XBAR_GET_INFO),
        control_resolved: nvapi::exposes(FN_XBAR_GET_CONTROL),
        set_resolved: nvapi::exposes(FN_XBAR_SET_CONTROL),
        measure_resolved: nvapi::exposes(FN_XBAR_MEASURE),
        info_status: info.as_ref().map_or(i32::MIN, |r| r.0),
        control_status: ctrl.as_ref().map_or(i32::MIN, |r| r.0),
        core_khz: measure_clock_khz(h, DOMAIN_CORE),
        xbar_khz: measure_clock_khz(h, DOMAIN_XBAR),
        memory_khz: measure_clock_khz(h, DOMAIN_MEMORY),
        ..parse_xbar_probe(info_ok.as_deref(), ctrl_ok.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(b: &mut [u8], off: usize, v: i32) {
        wr_i32(b, off, v);
    }

    #[test]
    fn curve_span_is_the_mask() {
        // TotalPoints is the buffer's capacity, mask-bounded — not the struct's field split.
        assert_eq!(total_points(80), 128, "curve span is the mask, not the struct split");
        assert_eq!(total_points(128), MASK_POINTS, "span never exceeds the mask");
        assert!(total_points(80) >= 127, "span covers a 127-point Blackwell curve");
        assert!(total_points(80) >= 103, "span covers a 103-point Ada curve");
        assert_eq!(trailing_array(), 1, "trailing array defaults to the second");
        assert_eq!(delta_points(80), 128);
        assert_eq!(struct_points(80), 103);
    }

    #[test]
    fn layout_constants() {
        assert_eq!(struct_sizes(), (7208, 9248));
        assert_eq!(curve_table_size(80), 7208);
        assert_eq!(delta_table_size(80), 9248);
        // The flat delta array: entry 80 sits where the struct's trailing arrays begin, which is
        // exactly why writing high points into those arrays landed inside entry 80's record.
        assert_eq!(delta_offset(0), 68 + 20);
        assert_eq!(delta_offset(127), 68 + 127 * 36 + 20);
        assert_eq!(HEADER_BYTES + 80 * DELTA_ENTRY_BYTES, MEMORY_FILLED);
        assert_eq!(MEMORY_DELTAS, MEMORY_FILLED + 23 * 4);
        assert_eq!(control_offset_for(1), 0x53C);
        assert_eq!(ocp_value_offset(13), 0x228);
        assert_eq!(rail_control_field(1, CONTROL_ALT_MAX_OFFSET), 0x48 + 0x54 + 8);
        assert_eq!(nvapi::version_word(VOLT_RAILS_CONTROL_SIZE, VOLT_RAILS_VERSION), 0x0002_0AC8);
        assert_eq!(nvapi::version_word(OCP_SIZE, OCP_VERSION), 0x0001_0A4C);
        assert_eq!(nvapi::version_word(OCP_RANGE_SIZE, OCP_RANGE_VERSION), 0x0004_0A70);
        assert_eq!(nvapi::version_word(THERMAL_V2_SIZE, 2), 168 | (2 << 16));
        assert_eq!(nvapi::version_word(THERM_V3_SIZE, 3), 0x0003_34C8);
    }

    #[test]
    fn masks() {
        let bits = |m: [u32; 4]| m.iter().map(|w| w.count_ones()).sum::<u32>();
        assert_eq!(bits(MASK_103), 103);
        assert_eq!(bits(FULL_MASK), 128);
        let mut d = vec![0i32; 130];
        d[0] = 1;
        d[33] = -5;
        d[127] = 7;
        d[129] = 9; // past the mask: ignored
        assert_eq!(mask_for(&d), [1, 2, 0, 0x8000_0000]);
        assert_eq!(mask_for(&[]), [0; 4]);
    }

    #[test]
    fn raw_curve_parsing() {
        let size = curve_table_size(80);
        let mut b = vec![0u8; size];
        let point = |b: &mut [u8], i: usize, mv: i32, mhz: i32| {
            put(b, HEADER_BYTES + i * ENTRY_BYTES + 4, mhz * 1000);
            put(b, HEADER_BYTES + i * ENTRY_BYTES + 8, mv * 1000);
        };
        point(&mut b, 0, 450, 800);
        point(&mut b, 2, 500, 900); // slot 1 empty: skipped
        point(&mut b, 102, 1090, 2800);
        point(&mut b, 126, 1240, 3100); // past the struct split, still read
        point(&mut b, 127, 515, 405); // next domain starting: stop
        let pts = parse_curve_raw(&b, size);
        assert_eq!(pts, vec![(450, 800), (500, 900), (1090, 2800), (1240, 3100)]);
        assert!(parse_curve_raw(&b, 10).is_empty(), "undersized declared buffer is safe");

        // The struct reader stops at 103 points.
        let s = parse_curve_struct(&b);
        assert_eq!(s.len(), 103);
        assert_eq!(s[0], (450_000, 800_000));
        assert_eq!(s[102], (1_090_000, 2_800_000));
    }

    #[test]
    fn delta_table_round_trip() {
        let size = delta_table_size(80);
        let mut b = vec![0u8; size];
        put(&mut b, 0, 0x1234); // the getter may rewrite the header
        put(&mut b, HEADER_BYTES + 5 * DELTA_ENTRY_BYTES + 4, 77); // driver field must survive
        let mut deltas = vec![0i32; 128];
        deltas[5] = 150_000;
        deltas[100] = -30_000;
        stage_deltas_raw(&mut b, &MASK_103, 80, 1, &deltas, Some(1));
        assert_eq!(rd_u32(&b, 0), nvapi::version_word(size, 1));
        assert_eq!(rd_u32(&b, 4 + 12), 0x7F);
        assert_eq!(rd_i32(&b, delta_offset(5)), 150_000);
        assert_eq!(rd_i32(&b, delta_offset(100)), -30_000);
        assert_eq!(rd_i32(&b, HEADER_BYTES + 5 * DELTA_ENTRY_BYTES), 1, "changed GPU entry flagged");
        assert_eq!(rd_i32(&b, HEADER_BYTES + 6 * DELTA_ENTRY_BYTES), 0, "unchanged entry not flagged");
        assert_eq!(rd_i32(&b, HEADER_BYTES + 100 * DELTA_ENTRY_BYTES), 0, "past the GPU region not flagged");
        assert_eq!(rd_i32(&b, HEADER_BYTES + 5 * DELTA_ENTRY_BYTES + 4), 77, "other fields preserved");
        let back = parse_deltas_raw(&b, 80);
        assert_eq!(back.len(), 128);
        assert_eq!(back, deltas);

        // Without a flag nothing but the deltas changes.
        let mut c = vec![0u8; size];
        stage_deltas_raw(&mut c, &FULL_MASK, 80, 1, &deltas, None);
        assert_eq!(rd_i32(&c, HEADER_BYTES + 5 * DELTA_ENTRY_BYTES), 0);
        stage_deltas_raw(&mut c, &FULL_MASK, 80, 1, &deltas, Some(-1));
        assert_eq!(rd_i32(&c, HEADER_BYTES + 5 * DELTA_ENTRY_BYTES), 0, "negative flag means none");

        // Struct reader: first 80 flat, then the trailing array TrailingArray picks.
        let mut t = vec![0u8; size];
        put(&mut t, delta_offset(3), -9);
        put(&mut t, MEMORY_FILLED, 11);
        put(&mut t, MEMORY_DELTAS + 22 * 4, 22);
        let s1 = parse_deltas_struct(&t, 1);
        assert_eq!(s1.len(), 103);
        assert_eq!((s1[3], s1[80], s1[102]), (-9, 0, 22));
        let s0 = parse_deltas_struct(&t, 0);
        assert_eq!((s0[80], s0[102]), (11, 0));
        let (a, second) = parse_trailing_arrays(&t, 80);
        assert_eq!((a[0], second[22], a.len(), second.len()), (11, 22, 23, 23));
    }

    #[test]
    fn volt_rails() {
        assert_eq!(rail_slot(0b11, RAIL_NVVDD), Some(0));
        assert_eq!(rail_slot(0b11, RAIL_MSVDD), Some(1));
        assert_eq!(rail_slot(0b101, 2), Some(1));
        assert_eq!(rail_slot(0b101, 1), None);
        assert_eq!(rail_slot(0b11, -1), None);
        assert_eq!(rail_slot(u32::MAX, 32), None);
        assert_eq!(rail_slot(u32::MAX, 31), Some(31));

        let mut status = vec![0u8; VOLT_RAILS_STATUS_SIZE];
        let mut control = vec![0u8; VOLT_RAILS_CONTROL_SIZE];
        for (slot, (cur, max, min, off)) in
            [(810_000, 1_100_000, 700_000, 0), (810_000, 1_055_000, 750_000, -50_000)].into_iter().enumerate()
        {
            let s = STATUS_ENTRIES + slot * STATUS_STRIDE;
            put(&mut status, s + 4, cur);
            put(&mut status, s + 8, max);
            put(&mut status, s + 0x18, min);
            put(&mut control, rail_control_field(slot, CONTROL_MAX_OFFSET), off);
            put(&mut control, rail_control_field(slot, CONTROL_MIN_OFFSET), off / 2);
        }
        let rails = parse_volt_rails(0b11, &status, Some(&control));
        assert_eq!(rails.len(), 2);
        assert_eq!(
            rails[1],
            VoltRail {
                index: 1,
                current_uv: 810_000,
                max_uv: 1_055_000,
                min_uv: 750_000,
                max_offset_uv: -50_000,
                min_offset_uv: -25_000
            }
        );
        let no_ctl = parse_volt_rails(0b11, &status, None);
        assert_eq!((no_ctl[1].max_offset_uv, no_ctl[1].max_uv), (0, 1_055_000));
        assert!(parse_volt_rails(0b11, &status[..0x100], None).is_empty(), "short status is safe");

        let dump = parse_rail_status_dump(0b10, &status);
        assert_eq!(dump, vec![(1, "+0x04=810000 +0x08=1100000 +0x18=700000".to_string())]);
    }

    #[test]
    fn ocp_parsing() {
        let mut w = vec![0i32; OCP_SIZE / 4];
        let base = (OCP_ENTRIES + 13 * OCP_STRIDE) / 4;
        w[base] = 1;
        w[base + 1] = 300_000;
        w[(OCP_ENTRIES + 14 * OCP_STRIDE) / 4 + 1] = 120_000;
        let ch = parse_ocp_channels(&w);
        assert_eq!(ch.len(), 2);
        assert_eq!(ch[0], OcpChannel { slot: 13, selector: 1, value: 300_000, words: "+0x00=1 +0x04=300000".into() });
        assert_eq!((ch[1].slot, ch[1].selector, ch[1].value), (14, 0, 120_000));
        assert!(parse_ocp_channels(&[]).is_empty());

        // Ranges: anchored by the -1 sentinel.
        let r = parse_ocp_ranges(&[7, 250_000, 300_000, 350_000, -1, 1, 2, 3, 4, 5, 300_000, 5_001_000, -1, 0]);
        assert_eq!(
            r,
            vec![
                OcpRange { min_ma: 250_000, value_ma: 300_000, max_ma: 350_000 },
                OcpRange { min_ma: 5, value_ma: 300_000, max_ma: 5_001_000 }
            ]
        );
        assert!(parse_ocp_ranges(&[1, 2, 20_000_000, -1]).is_empty(), "a counter is not amps");
        assert!(parse_ocp_ranges(&[3, 2, 5, -1]).is_empty(), "not ascending");
        assert!(parse_ocp_ranges(&[1, 2]).is_empty());
    }

    #[test]
    fn ocp_range_for() {
        // A limit can appear in more than one entry; the one that runs to many times the limit is
        // the driver declining to constrain it rather than a window worth offering.
        let ranges = [
            OcpRange { min_ma: 250_000, value_ma: 300_000, max_ma: 350_000 }, // the real window
            OcpRange { min_ma: 1, value_ma: 300_000, max_ma: 5_001_000 },     // same limit, unconstrained
            OcpRange { min_ma: 1, value_ma: 120_000, max_ma: 5_001_000 },     // only the unconstrained one
        ];
        let core = range_for(&ranges, 300_000);
        assert!(
            matches!(core, Some(OcpRange { min_ma: 250_000, max_ma: 350_000, .. })),
            "the tight window wins over the unconstrained one"
        );
        assert_eq!(range_for(&ranges, 120_000), None, "a limit with no real window gets none");
        assert_eq!(range_for(&ranges, 999_000), None, "a limit that is not there gets none");
        let two = [
            OcpRange { min_ma: 200_000, value_ma: 300_000, max_ma: 400_000 },
            OcpRange { min_ma: 250_000, value_ma: 300_000, max_ma: 350_000 },
        ];
        assert_eq!(range_for(&two, 300_000), Some(two[1]), "narrowest wins");
    }

    #[test]
    fn power_rails() {
        let (size, entry0, stride) = POWER_SHAPES[0];
        let mut w = vec![0i32; size / 4];
        w[entry0 / 4] = 1782;
        w[entry0 / 4 + 1] = 11_891_134;
        w[(entry0 + 2 * stride) / 4] = -5; // negative: skipped
        w[(entry0 + 2 * stride) / 4 + 1] = 12_000_000;
        w[(entry0 + 3 * stride) / 4 + 1] = 12_000_000; // 0 mA, live voltage: kept
        let rails = parse_power_rails(&w, entry0, stride);
        assert_eq!(
            rails,
            vec![
                PowerRail { slot: 0, milliamps: 1782, microvolts: 11_891_134 },
                PowerRail { slot: 3, milliamps: 0, microvolts: 12_000_000 }
            ]
        );
        // The board total the layout was proven against: 1782 mA × 11.891134 V ≈ 21.19 W.
        let watts = rails[0].milliamps as f64 * rails[0].microvolts as f64 / 1e9;
        assert!((watts - 21.19).abs() < 0.01);
        assert!(parse_power_rails(&w[..4], entry0, stride).is_empty());
    }

    #[test]
    fn rail_voltages_channel_mapping() {
        let mut b = vec![0u8; ADC_SIZE];
        put(&mut b, ADC_ENTRY0 + ADC_CHANNEL_MSVDD * ADC_STRIDE + 4, 800_000);
        put(&mut b, ADC_ENTRY0 + ADC_CHANNEL_NVVDD * ADC_STRIDE + 4, 794_000);
        let v = parse_rail_voltages(&b);
        assert_eq!(v.get(&RAIL_MSVDD), Some(&800_000), "channel 0 is MSVDD");
        assert_eq!(v.get(&RAIL_NVVDD), Some(&794_000), "channel 1 is NVVDD");
        put(&mut b, ADC_ENTRY0 + 4, 2_000_000);
        put(&mut b, ADC_ENTRY0 + ADC_STRIDE + 4, -1);
        assert!(parse_rail_voltages(&b).is_empty(), "implausible readings dropped");
    }

    #[test]
    fn thermal() {
        let mut b = vec![0u8; THERMAL_V2_SIZE];
        put(&mut b, THERMAL_V2_TEMPS + 4, 45 * 256);
        put(&mut b, THERMAL_V2_TEMPS + 8, 53 * 256 + 128);
        let slots = parse_thermal_slots(&b);
        assert_eq!(slots.len(), 32);
        assert_eq!((slots[1], slots[2]), (45.0, 53.5));
        let (hot, mem) = interpret("NVIDIA GeForce RTX 5070 Ti", &slots);
        assert!(hot.is_nan());
        assert_eq!(mem, 53.5);
        let (hot, mem) = interpret("NVIDIA GeForce RTX 4070 Ti", &slots);
        assert_eq!(hot, 45.0);
        assert!(mem.is_nan());
        let (hot, mem) = interpret("GTX 1080", &slots[..10]);
        assert!(hot.is_nan() && mem.is_nan(), "short slot list is safe");

        let mut w = vec![0i32; THERM_V3_SIZE / 4];
        w[THERM_V3_ENTRY0 / 4] = 40 * 256;
        w[THERM_V3_ENTRY0 / 4 + 1] = 1;
        w[(THERM_V3_ENTRY0 + THERM_V3_STRIDE) / 4] = 255 * 256;
        let ch = parse_thermal_channels(&w);
        assert_eq!(ch.len(), 8);
        assert_eq!(ch[0], ThermalChannel { slot: 0, kind: 1, present: true, celsius: 40.0 });
        assert!(!ch[1].present);
        assert!(parse_thermal_channels(&w[..19]).is_empty());

        assert_eq!(fmt1(0.25), "0.3", ".NET rounds the tie away from zero");
        assert_eq!(fmt1(45.0), "45.0");
    }

    #[test]
    fn xbar_info_and_domains() {
        let mut info = vec![0u8; XBAR_INFO_SIZE];
        let entry = |i: usize| XBAR_INFO_ENTRIES + i * XBAR_INFO_STRIDE;
        let put16 = |b: &mut [u8], off: usize, v: i16| b[off..off + 2].copy_from_slice(&v.to_le_bytes());
        // Domain 0 (type 0 is a real domain at index 0), crossbar at slot 1 with its range at +0x3C
        // (Blackwell), SYS at slot 3 with the range at +0x3A (Ada) and neighbouring noise.
        put(&mut info, entry(1), 1);
        put16(&mut info, entry(1) + 0x3C, -1000);
        put16(&mut info, entry(1) + 0x3E, 1000);
        put(&mut info, entry(3), 3);
        put16(&mut info, entry(3) + 0x38, 3598);
        put16(&mut info, entry(3) + 0x3A, -1000);
        put16(&mut info, entry(3) + 0x3C, 1000);
        put(&mut info, entry(4), 4);
        put16(&mut info, entry(4) + 0x3C, 1024);
        put16(&mut info, entry(4) + 0x3E, 258);
        assert_eq!(parse_xbar_info(&info), XbarInfo { supported: true, min_offset_mhz: -1000, max_offset_mhz: 1000 });

        let d = parse_domain_entries(&info);
        assert_eq!(d.iter().map(|e| e.index).collect::<Vec<_>>(), vec![0, 1, 3, 4], "empty slots skipped");
        assert_eq!(d[0], DomainEntry { index: 0, kind: 0, has_range: false, min_mhz: 0, max_mhz: 0 });
        assert_eq!(d[2], DomainEntry { index: 3, kind: 3, has_range: true, min_mhz: -1000, max_mhz: 1000 });
        assert!(!d[3].has_range, "noise is not a range");

        // Right entry, unrecognised layout: unsupported rather than a guessed range.
        let mut odd = vec![0u8; XBAR_INFO_SIZE];
        put(&mut odd, entry(1), 1);
        put16(&mut odd, entry(1) + 0x3C, 1000);
        assert_eq!(parse_xbar_info(&odd), XbarInfo::default());
        assert_eq!(parse_xbar_info(&[]), XbarInfo::default());

        let lines = format_domain_info(&info, 1);
        assert_eq!(lines[0], "slot 1 type=1 nonZero=2/268 longestRun=1");
        assert_eq!(format_domain_info(&info, 40), vec!["slot 40 entry runs past the buffer".to_string()]);
        assert_eq!(format_domain_info(&info, -1), vec!["slot -1 entry runs past the buffer".to_string()]);

        let mut ctrl = vec![0u8; XBAR_CONTROL_SIZE];
        put(&mut ctrl, control_offset_for(1), 30_000);
        let p = parse_xbar_probe(Some(&info), Some(&ctrl));
        assert_eq!(p.entry_types.len(), 32);
        assert_eq!(p.type_one_index, Some(1));
        assert_eq!(p.type_one_words.len(), 0x80 / 4);
        assert_eq!(p.control_words, vec![0x53C, 30_000]);
        assert_eq!(parse_xbar_probe(None, None), XbarProbe::default());
    }

    #[test]
    fn sweep_word_format() {
        let mut b = vec![0u8; 0x20];
        put(&mut b, 0, 0x10020); // the version word is not listed
        put(&mut b, 8, 5);
        put(&mut b, 0x1C, -2);
        assert_eq!(sweep_words(&b, 0x20, SWEEP_MAX_WORDS), "+0x008=5 +0x01C=-2");
        assert_eq!(sweep_words(&b, 0x20, 1), "+0x008=5");
        assert_eq!(sweep_words(&[0; 8], 8, 20), "(all zero)");
    }

    #[test]
    fn buffers_are_aligned_and_padded() {
        let mut b = request(VOLT_RAILS_CONTROL_SIZE, CALL_SLACK, 1);
        assert_eq!(b.ptr() as usize % 8, 0);
        assert!(b.bytes().len() >= VOLT_RAILS_CONTROL_SIZE + CALL_SLACK);
        assert_eq!(b.declared().len(), VOLT_RAILS_CONTROL_SIZE);
    }
}
