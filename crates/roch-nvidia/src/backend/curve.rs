//! The V/F curve (mask, layout, live/stock reads, verified delta writes) and the voltage lock.
//!
//! Reads here are getters only. The writes — `write_curve_deltas`, `try_write_boost_lock`,
//! `enable_oc_pstates`, the NVML clock cap — are reached only from the setters.

use std::collections::BTreeMap;

use roch_core::util::clamp_i32;
use roch_core::{GpuError, Result, VfCurveSample};

use super::{Fail, MIN_LOCKABLE_MHZ, NvidiaBackend};
use crate::nvapi::{self, ClockBoostLock, GpuHandle, NvError, NvResult};
use crate::nvml;
use crate::private;
use crate::vf_curve::{self, VfPoint};

/// ClockLockMode.None / .Manual.
const LOCK_NONE: u32 = 0;
const LOCK_MANUAL: u32 = 3;
/// PublicClockDomain.Graphics.
const DOMAIN_GRAPHICS: u32 = 0;

impl NvidiaBackend {
    /// The mask telling the driver which curve points to fill. NvAPIWrapper leaves it at zero, which
    /// makes the driver return Ok with an entirely empty curve, so a real one is resolved and reused:
    /// preferably the one GetClockBoostMask fills in (if it reads a curve), else every point.
    pub(super) fn curve_mask(&mut self, gpu: usize, g: GpuHandle) -> [u32; 4] {
        if let Some(m) = self.curve_mask.get(&gpu) {
            return *m;
        }
        let mask = match nvapi::clock_boost_mask(g) {
            Ok(m) if m.iter().any(|&x| x != 0) && private::read_curve(g, &m).iter().any(|p| p.0 > 0) => m,
            _ => private::FULL_MASK,
        };
        self.curve_mask.insert(gpu, mask);
        mask
    }

    /// How many GPU curve points this driver's struct carries, and which struct version.
    /// NvAPIWrapper hardcodes 80, which truncates cards whose curve runs higher — a 4070 Ti stops at
    /// 945 mV while boosting near 1095 mV. Probe once for the widest layout the driver accepts.
    pub(super) fn curve_layout(&mut self, gpu: usize, g: GpuHandle) -> (usize, u32) {
        if let Some(l) = self.curve_layout.get(&gpu) {
            return *l;
        }
        let mask = self.curve_mask(gpu, g);
        let mut best = (80usize, 1u32);
        let (mut best_points, mut best_mv) = (0usize, 0i32);
        for entries in [80usize, 103, 128, 160, 255] {
            for ver in [1u32, 2, 3] {
                let (status, pts) = private::read_curve_raw(g, &mask, entries, ver);
                if status != 0 || pts.is_empty() {
                    continue;
                }
                // A layout only wins if it reaches higher up the curve, and its delta table must
                // round-trip too — otherwise we could read points we can never write back.
                let max_mv = pts.iter().map(|p| p.0).max().unwrap_or(0);
                if max_mv <= best_mv && pts.len() <= best_points {
                    continue;
                }
                let (dst, deltas) = private::read_deltas_raw(g, &mask, entries, ver);
                if dst != 0 || deltas.len() < pts.len() {
                    continue;
                }
                best = (entries, ver);
                best_points = pts.len();
                best_mv = max_mv;
            }
        }
        self.curve_layout.insert(gpu, best);
        best
    }

    /// Live curve, read with the widest layout the driver accepts.
    pub(super) fn read_effective_curve(&mut self, gpu: usize, g: GpuHandle) -> NvResult<Vec<VfPoint>> {
        let (entries, ver) = self.curve_layout(gpu, g);
        let mask = self.curve_mask(gpu, g);
        let (status, pts) = private::read_curve_raw(g, &mask, entries, ver);
        if status == 0 && !pts.is_empty() {
            return Ok(pts
                .iter()
                .map(|&(mv, mhz)| VfPoint::new(mv.wrapping_mul(1000) as u32, mhz.wrapping_mul(1000)))
                .collect());
        }

        let raw = private::read_curve(g, &mask);
        if raw.iter().any(|p| p.0 > 0 && p.1 > 0) {
            return Ok(raw.iter().filter(|p| p.0 > 0 && p.1 > 0).map(|&(uv, khz)| VfPoint::new(uv, khz)).collect());
        }

        // Last resort, through the wrapper's typed call. Both regions again: its "memory" entries
        // carry the tail of the GPU curve on cards with more than 80 points, so reading only the
        // first array would hand back a curve that stops 145 mV short on a 4070 Ti.
        Ok(nvapi::vfp_curve_entries(g)?
            .iter()
            .filter(|e| e.voltage_uv > 0 && e.frequency_khz > 0)
            .map(|e| VfPoint::new(e.voltage_uv, e.frequency_khz as i32))
            .collect())
    }

    fn read_curve_deltas(&mut self, gpu: usize, g: GpuHandle, count: usize) -> Vec<i32> {
        let (entries, ver) = self.curve_layout(gpu, g);
        let mask = self.curve_mask(gpu, g);
        let (status, d) = private::read_deltas_raw(g, &mask, entries, ver);
        if status == 0 && !d.is_empty() {
            return d;
        }
        let legacy = private::read_deltas(g, &mask);
        if !legacy.is_empty() {
            return legacy;
        }
        // Either wrapper exception (a status, or the entry point missing altogether) → zeros.
        nvapi::clock_boost_table_gpu_deltas(g).unwrap_or_else(|_| vec![0; count])
    }

    /// The stock curve: the live curve with any currently-applied deltas subtracted out.
    pub(super) fn read_base_curve(&mut self, gpu: usize, g: GpuHandle) -> NvResult<Vec<VfPoint>> {
        let curve = self.read_effective_curve(gpu, g)?;
        let deltas = self.read_curve_deltas(gpu, g, curve.len());
        Ok(curve
            .iter()
            .enumerate()
            .map(|(i, p)| VfPoint::new(p.voltage_uv, p.frequency_khz.wrapping_sub(deltas.get(i).copied().unwrap_or(0))))
            .collect())
    }

    /// The driver's delta range, ordered (a reversed pair would make a clamp meaningless); ±1 GHz
    /// when it reports none.
    pub(super) fn boost_delta_range(g: GpuHandle) -> (i32, i32) {
        if let Ok(ranges) = nvapi::clock_boost_ranges(g) {
            for r in ranges {
                if r.range_min_khz != 0 || r.range_max_khz != 0 {
                    return if r.range_min_khz <= r.range_max_khz {
                        (r.range_min_khz, r.range_max_khz)
                    } else {
                        (r.range_max_khz, r.range_min_khz)
                    };
                }
            }
        }
        (-1_000_000, 1_000_000)
    }

    // ---- voltage lock ----------------------------------------------------------------------
    //
    // The delta table has 80 slots but the curve has 103+ points, and a uniform write to those 80
    // slots shifts all of them — the driver derives the top points from the table rather than
    // exposing them. That makes "flatten everything above 1000 mV" unreachable through the table.
    // SetClockBoostLock says it directly: pin the boost to a voltage. That is the right primitive
    // for a cap, and it works regardless of where the curve point sits.

    /// Cap the core at a voltage; 0 clears the cap. Two mechanisms, in order:
    ///   1. SetClockBoostLock — returns Ok whether or not it does anything, so the write is proved
    ///      by reading the lock back; several struct shapes are tried and the one that sticks kept.
    ///   2. NVML locked clocks (`nvidia-smi -lgc`): the curve says which frequency the target
    ///      voltage reaches, and capping the clock there stops the card asking for more volts.
    pub(super) fn set_voltage_lock_impl(&mut self, gpu: usize, target_mv: i32) -> Result<()> {
        let g = self.gpu(gpu)?;
        let mut target_mv = target_mv;
        // Defensive: the value is multiplied into microvolts, so a wild figure would be nonsense to
        // the driver and could overflow the conversion.
        if target_mv > 0 {
            let caps = self.capabilities_impl(gpu)?;
            let lo = if caps.min_voltage_mv > 0 { caps.min_voltage_mv } else { 600 };
            let hi = if caps.max_voltage_mv > 0 { caps.max_voltage_mv } else { 1200 };
            target_mv = clamp_i32(target_mv, lo, hi);
        }

        if target_mv <= 0 {
            let nvml_err = self.clear_nvml_clock_cap(gpu);
            let recipe = if self.lock_recipe >= 0 { self.lock_recipe } else { 0 };
            // Not swallowed: the C# let a failed unlock propagate before the mechanism is cleared.
            self.try_write_boost_lock(g, 0, recipe)?;
            self.voltage_lock_mechanism = "none".into();
            if let Some(e) = nvml_err
                && self.nvml_clock_cap_mhz > 0
            {
                return Err(GpuError::new(format!("Failed to release the clock cap: {e}")));
            }
            return Ok(());
        }

        // ---- 1. private boost lock, proved by read-back
        if !self.prefer_nvml {
            let recipes: Vec<i32> = if self.lock_recipe >= 0 { vec![self.lock_recipe] } else { vec![0, 1, 2, 3] };
            for r in recipes {
                if self.try_write_boost_lock(g, target_mv, r).is_err() {
                    continue;
                }
                if (read_boost_lock_mv(g) - target_mv).abs() <= 10 {
                    self.lock_recipe = r;
                    let _ = self.clear_nvml_clock_cap(gpu); // one mechanism at a time
                    self.voltage_lock_mechanism = format!("NVAPI boost lock (recipe {r})");
                    return Ok(());
                }
            }
        }

        // ---- 2. NVML clock cap at the frequency the curve reaches at that voltage
        let mut mhz = self.curve_frequency_at_mv(gpu, g, target_mv);
        if mhz <= 0 {
            return Err(GpuError::new(format!(
                "The driver ignored the {target_mv} mV voltage lock and the V/F curve could not be read, \
                 so there is no frequency to cap instead."
            )));
        }
        // An explicit clock range owns the NVML lock; capping through it would silently move the
        // window the user set. The boost lock above is the cap's own mechanism.
        if self.locked_min_mhz > 0 || self.locked_max_mhz > 0 {
            return Err(GpuError::new(format!(
                "The driver ignored the {target_mv} mV voltage lock, and the clock cap it would fall back to is \
                 held by the {}-{} MHz clock range. Clear the range to cap by clock instead.",
                self.locked_min_mhz, self.locked_max_mhz
            )));
        }

        mhz = mhz.max(MIN_LOCKABLE_MHZ);
        let nvml_index = self.nvml_index_for(gpu);
        let mut err = nvml::lock_graphics_clocks(nvml_index, 0, mhz);
        if let Some(first) = err {
            // Some driver branches reject a 0 floor; 210 MHz is the lowest 3D clock these cards use.
            err = nvml::lock_graphics_clocks(nvml_index, MIN_LOCKABLE_MHZ, mhz)
                .map(|retry| format!("{first}; with a 210 MHz floor: {retry}"));
        }
        if let Some(e) = err {
            return Err(GpuError::new(format!(
                "The driver ignored the {target_mv} mV voltage lock, and capping the clock at {mhz} MHz instead \
                 failed: {e}"
            )));
        }

        self.nvml_clock_cap_mhz = mhz;
        self.nvml_cap_voltage_mv = target_mv;
        self.prefer_nvml = true; // don't re-probe four unverifiable NVAPI writes on every apply
        // Only one lever should hold the cap, and this is the one that just proved it works.
        let _ = self.try_write_boost_lock(g, 0, 0);
        self.voltage_lock_mechanism = format!("NVML clock cap {mhz} MHz");
        Ok(())
    }

    /// One attempt at the private lock. Recipes differ in the struct shape handed to the driver:
    /// 0 the graphics domain alone; 1 read-modify-write of every entry the driver reported (graphics
    /// changed, the rest unlocked); 2 unlock the overclocked p-states first, then recipe 0; 3 recipe
    /// 1 after unlocking the p-states.
    pub(super) fn try_write_boost_lock(&mut self, g: GpuHandle, target_mv: i32, recipe: i32) -> Result<()> {
        let mode = if target_mv > 0 { LOCK_MANUAL } else { LOCK_NONE };
        let uv = if target_mv > 0 { target_mv.wrapping_mul(1000) as u32 } else { 0 };
        let entry = |domain: u32, mode: u32, uv: u32| ClockBoostLock {
            clock_domain: domain,
            unknown1: 0,
            lock_mode: mode,
            unknown2: 0,
            voltage_uv: uv,
            unknown3: 0,
        };
        let run = |this: &mut Self| -> NvResult<()> {
            let payload: Vec<ClockBoostLock> = match recipe {
                0 => vec![entry(DOMAIN_GRAPHICS, mode, uv)],
                1 | 3 => {
                    let current = nvapi::clock_boost_locks(g)?;
                    let entries = if current.is_empty() {
                        vec![entry(DOMAIN_GRAPHICS, mode, uv)]
                    } else {
                        current
                            .iter()
                            .map(|e| {
                                if e.clock_domain == DOMAIN_GRAPHICS {
                                    entry(DOMAIN_GRAPHICS, mode, uv)
                                } else {
                                    entry(e.clock_domain, LOCK_NONE, 0)
                                }
                            })
                            .collect()
                    };
                    if recipe == 3 {
                        this.enable_oc_pstates(g);
                    }
                    entries
                }
                _ => {
                    this.enable_oc_pstates(g);
                    vec![entry(DOMAIN_GRAPHICS, mode, uv)]
                }
            };
            nvapi::set_clock_boost_locks(g, &payload)
        };
        match run(self) {
            Ok(()) => Ok(()),
            Err(e @ NvError::Api(_)) => {
                Err(GpuError::new(format!("Failed to set the voltage lock to {target_mv} mV: {}.", e.status_text())))
            }
            Err(NvError::NotSupported(_)) => Err(GpuError::new("This driver does not expose the voltage lock.")),
        }
    }

    /// EnableOverclockedPStates, once per backend; errors ignored. A write: only from setters.
    pub(super) fn enable_oc_pstates(&mut self, g: GpuHandle) {
        if self.oc_enabled {
            return;
        }
        self.oc_enabled = true;
        let _ = nvapi::enable_overclocked_pstates(g);
    }

    /// Release an NVML clock cap this process applied; `None` on success or when there was none.
    pub(super) fn clear_nvml_clock_cap(&mut self, gpu: usize) -> Option<String> {
        if self.nvml_clock_cap_mhz <= 0 {
            return None;
        }
        let err = nvml::reset_graphics_clocks(self.nvml_index_for(gpu));
        if err.is_none() {
            self.nvml_clock_cap_mhz = 0;
            self.nvml_cap_voltage_mv = 0;
        }
        err
    }

    /// Highest stock curve frequency at or below `mv`, in MHz. 0 if unknown.
    pub(super) fn curve_frequency_at_mv(&mut self, gpu: usize, g: GpuHandle, mv: i32) -> i32 {
        let Ok(base) = self.read_base_curve(gpu, g) else { return 0 };
        let limit = (mv as u32).wrapping_mul(1000);
        base.iter()
            .filter(|p| p.voltage_uv > 0 && p.frequency_khz > 0 && p.voltage_uv <= limit)
            .map(|p| p.frequency_khz / 1000)
            .fold(0, i32::max)
    }

    /// The voltage the core is currently capped to, or 0 when uncapped. NVML exposes no getter for
    /// the locked range, so a cap we applied through it is tracked in-process.
    pub(super) fn read_voltage_lock_mv_impl(&mut self, gpu: usize) -> i32 {
        let Ok(g) = self.gpu(gpu) else { return 0 };
        let mv = read_boost_lock_mv(g);
        if mv > 0 {
            return mv;
        }
        if self.nvml_clock_cap_mhz > 0 { self.nvml_cap_voltage_mv } else { 0 }
    }

    /// Whether any targeted point moved between `before` and a fresh read.
    fn curve_moved(&mut self, gpu: usize, g: GpuHandle, before: &[VfPoint], targeted: &[usize]) -> NvResult<bool> {
        let after = self.read_effective_curve(gpu, g)?;
        Ok(targeted
            .iter()
            .any(|&i| i < after.len() && i < before.len() && after[i].frequency_khz != before[i].frequency_khz))
    }

    /// Write per-point deltas and prove they landed.
    ///
    /// SetClockBoostTable returns Ok whether or not it does anything, so the call's success means
    /// nothing. The mask the write wants and whether each changed entry needs a validity flag are
    /// unknown, so snapshot the curve, try each combination, and keep the one that moves it.
    pub(super) fn write_curve_deltas(
        &mut self,
        gpu: usize,
        g: GpuHandle,
        deltas: &[i32],
    ) -> std::result::Result<(), Fail> {
        let (entries, ver) = self.curve_layout(gpu, g);

        // Some drivers refuse curve edits until overclocked p-states are unlocked. Harmless if absent.
        self.enable_oc_pstates(g);

        let targeted: Vec<usize> = (0..deltas.len()).filter(|&i| deltas[i] != 0).collect();

        // Clearing the table can't be verified by "did anything move" — use what is known to work.
        if targeted.is_empty() {
            let (m0, f0) = match self.write_recipe.get(&gpu) {
                Some(r) => *r,
                None => (self.curve_mask(gpu, g), None),
            };
            if private::write_deltas_raw(g, &m0, entries, ver, deltas, f0) == 0 {
                return Ok(());
            }
            let mask = self.curve_mask(gpu, g);
            private::write_deltas(g, &mask, deltas);
            return Ok(());
        }

        let before = self.read_effective_curve(gpu, g)?;

        // Known-good recipe first, then the grid. Which trailing array held the high points, and
        // whether to drop them, used to be probed too: both existed because the table was modelled
        // as 80 entries followed by two arrays of bare ints. It is one flat run of 36-byte entries,
        // so those points were written into the middle of entry 80's record and the driver refused
        // the whole call. With the offsets right nothing is left to select (8 cycles, not 32).
        let mut recipes: Vec<([u32; 4], Option<i32>)> = Vec::new();
        if let Some(r) = self.write_recipe.get(&gpu) {
            recipes.push(*r);
        }
        let curve_mask = self.curve_mask(gpu, g);
        for mask in [curve_mask, private::MASK_103, private::mask_for(deltas), private::FULL_MASK] {
            for flag in [None, Some(1)] {
                recipes.push((mask, flag));
            }
        }

        for (mask, flag) in recipes {
            if private::write_deltas_raw(g, &mask, entries, ver, deltas, flag) != 0 {
                continue;
            }
            if !self.curve_moved(gpu, g, &before, &targeted)? {
                continue;
            }
            self.write_recipe.insert(gpu, (mask, flag));
            return Ok(());
        }

        // Last resort: the struct-layout typed write.
        if private::write_deltas(g, &curve_mask, deltas) == 0 && self.curve_moved(gpu, g, &before, &targeted)? {
            return Ok(());
        }

        Err(Fail::Gpu(GpuError::new(
            "The driver accepted the V/F curve write but the curve did not change. Close MSI Afterburner or any \
             other tuning tool holding the curve, then try again.",
        )))
    }

    /// Apply a negative voltage offset by flattening the V/F curve that far below its stock top;
    /// `offset_mv >= 0` clears the curve edit.
    pub(super) fn set_voltage_curve_offset_impl(
        &mut self,
        gpu: usize,
        offset_mv: i32,
        extra_clock_mhz: i32,
    ) -> Result<()> {
        let g = self.gpu(gpu)?;
        let run = |this: &mut Self| -> std::result::Result<(), Fail> {
            if offset_mv >= 0 {
                // Clear the curve deltas ONLY. This used to drop the voltage lock as well, which was
                // fine while a negative offset was how undervolts were expressed — but once one
                // absolute slider drove both levers, every apply passed 0 here and silently undid the
                // cap written moments earlier. The lock belongs to set_voltage_lock.
                let total = private::total_points(this.curve_layout(gpu, g).0);
                return this.write_curve_deltas(gpu, g, &vec![0; total]);
            }

            let base = this.read_base_curve(gpu, g)?;
            let stock_max_uv = vf_curve::max_voltage_uv(&base);
            if stock_max_uv == 0 {
                return Err(Fail::Gpu(GpuError::new("Could not read the V/F curve (no valid points).")));
            }
            // Never cap below 500 mV — that would be nonsense.
            let cap_uv = (stock_max_uv as i64 + offset_mv as i64 * 1000).max(500_000);

            let (r_min, r_max) = Self::boost_delta_range(g);
            // The lock is what enforces the cap; the flatten only reaches points the table owns
            // (below ~945 mV here) and is applied as a belt-and-braces extra.
            this.set_voltage_lock_impl(gpu, (cap_uv / 1000) as i32)?;
            let deltas = vf_curve::compute_flatten_deltas(
                &base,
                cap_uv as u32,
                extra_clock_mhz.wrapping_mul(1000),
                r_min,
                r_max,
            );
            match this.write_curve_deltas(gpu, g, &deltas) {
                Ok(()) | Err(Fail::Gpu(_)) => Ok(()), // the lock already carries the cap
                Err(e) => Err(e),
            }
        };
        match run(self) {
            Ok(()) => Ok(()),
            Err(f @ (Fail::Gpu(_) | Fail::Other(..))) => Err(f.into()),
            Err(Fail::Nv(e @ NvError::Api(_))) => Err(GpuError::new(format!(
                "Failed to apply {offset_mv} mV voltage offset: {}. Curve editing needs admin and no other OC tool \
                 holding the curve.",
                e.status_text()
            ))),
            // Thrown when nvapi64.dll has no such entry point (pre-Pascal card or a driver branch
            // that dropped the private curve calls).
            Err(Fail::Nv(NvError::NotSupported(_))) => Err(GpuError::new(format!(
                "Failed to apply {offset_mv} mV voltage offset: this driver/GPU does not expose the private V/F \
                 curve calls."
            ))),
        }
    }

    pub(super) fn read_vf_curve_impl(&mut self, gpu: usize) -> Result<Vec<VfCurveSample>> {
        let g = self.gpu(gpu)?;
        let (Ok(base), Ok(eff)) = (self.read_base_curve(gpu, g), self.read_effective_curve(gpu, g)) else {
            return Ok(Vec::new());
        };
        Ok(base
            .iter()
            .enumerate()
            .filter(|(_, p)| p.voltage_uv != 0 && p.frequency_khz != 0)
            .map(|(i, p)| {
                let live = eff.get(i).map(|e| e.frequency_khz).unwrap_or(p.frequency_khz);
                VfCurveSample::new(i as i32, (p.voltage_uv / 1000) as i32, p.frequency_khz / 1000, live / 1000)
            })
            .collect())
    }

    pub(super) fn set_vf_curve_targets_impl(&mut self, gpu: usize, targets: &[VfCurveSample]) -> Result<()> {
        let g = self.gpu(gpu)?;
        let run = |this: &mut Self| -> std::result::Result<(), Fail> {
            if targets.is_empty() {
                let total = private::total_points(this.curve_layout(gpu, g).0);
                return this.write_curve_deltas(gpu, g, &vec![0; total]);
            }
            let base = this.read_base_curve(gpu, g)?;
            let (r_min, r_max) = Self::boost_delta_range(g);
            // Last sample per index wins (GroupBy(...).Last()).
            let mut target_khz = BTreeMap::new();
            for t in targets.iter().filter(|t| t.index >= 0 && (t.index as usize) < base.len()) {
                target_khz.insert(t.index, t.live_mhz.wrapping_mul(1000));
            }
            this.write_curve_deltas(gpu, g, &vf_curve::compute_target_deltas(&base, &target_khz, r_min, r_max))
        };
        match run(self) {
            Ok(()) => Ok(()),
            Err(f @ (Fail::Gpu(_) | Fail::Other(..))) => Err(f.into()),
            Err(Fail::Nv(e @ NvError::Api(_))) => {
                Err(GpuError::new(format!("Failed to write V/F curve: {}.", e.status_text())))
            }
            Err(Fail::Nv(NvError::NotSupported(_))) => {
                Err(GpuError::new("V/F curve editing is not supported by this driver."))
            }
        }
    }
}

/// The voltage the boost lock holds (a Manual entry with a voltage), or 0.
pub(super) fn read_boost_lock_mv(g: GpuHandle) -> i32 {
    nvapi::clock_boost_locks(g)
        .ok()
        .and_then(|locks| locks.into_iter().find(|e| e.lock_mode == LOCK_MANUAL && e.voltage_uv > 0))
        .map(|e| (e.voltage_uv / 1000) as i32)
        .unwrap_or(0)
}
