//! Pure V/F curve maths — no NVAPI, so it is unit-tested without a GPU.
//!
//! NVIDIA exposes no voltage knob. The only way to undervolt is to edit the V/F curve: pick a
//! target voltage, then clamp every point ABOVE it down to the frequency at that point. Once
//! higher voltages buy no extra clock, the boost algorithm stops asking for them — that's the
//! undervolt. It is what Afterburner's Ctrl+F does, done arithmetically.

use std::collections::BTreeMap;

use roch_core::util::clamp_i32;

/// One point of the voltage/frequency curve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VfPoint {
    pub voltage_uv: u32,
    pub frequency_khz: i32,
}

impl VfPoint {
    pub const fn new(voltage_uv: u32, frequency_khz: i32) -> Self {
        Self { voltage_uv, frequency_khz }
    }
}

fn is_valid(p: &VfPoint) -> bool {
    p.voltage_uv > 0 && p.frequency_khz > 0
}

/// Per-point frequency deltas (kHz) that flatten the curve at `cap_voltage_uv`.
///
/// Points below the cap keep their stock frequency (delta 0). The point at the cap is lifted by
/// `extra_clock_khz`. Every point above the cap is clamped to that same frequency, which yields
/// negative deltas — the actual undervolt. `base_points` is the STOCK curve.
pub fn compute_flatten_deltas(
    base_points: &[VfPoint],
    cap_voltage_uv: u32,
    extra_clock_khz: i32,
    range_min_khz: i32,
    range_max_khz: i32,
) -> Vec<i32> {
    let mut deltas = vec![0i32; base_points.len()];
    if base_points.is_empty() || cap_voltage_uv == 0 {
        return deltas;
    }

    // Index of the highest-voltage valid point at or below the cap.
    let mut t: Option<usize> = None;
    for (i, p) in base_points.iter().enumerate() {
        if !is_valid(p) {
            continue;
        }
        if p.voltage_uv <= cap_voltage_uv && t.is_none_or(|t| p.voltage_uv > base_points[t].voltage_uv) {
            t = Some(i);
        }
    }

    // Cap is below the entire curve: clamp everything to the lowest valid point instead of
    // returning garbage. Better to underclock predictably than to write nonsense.
    if t.is_none() {
        for (i, p) in base_points.iter().enumerate() {
            if is_valid(p) && t.is_none_or(|t| p.voltage_uv < base_points[t].voltage_uv) {
                t = Some(i);
            }
        }
    }
    let Some(t) = t else { return deltas };

    let target_freq = base_points[t].frequency_khz.wrapping_add(extra_clock_khz);
    let t_volt = base_points[t].voltage_uv;

    for (i, p) in base_points.iter().enumerate() {
        if !is_valid(p) || p.voltage_uv < t_volt {
            deltas[i] = 0; // invalid, or below the cap: leave stock
            continue;
        }
        deltas[i] = clamp_i32(target_freq.wrapping_sub(p.frequency_khz), range_min_khz, range_max_khz);
    }
    deltas
}

/// Whether an effective curve has been flattened, and where: the lowest voltage of the top
/// plateau, or 0 if the curve still rises to the top. Reports real state back to the UI rather
/// than echoing what was asked for.
pub fn infer_cap_voltage_uv(effective_points: &[VfPoint]) -> u32 {
    let mut pts: Vec<VfPoint> = effective_points.iter().copied().filter(is_valid).collect();
    pts.sort_by_key(|p| p.voltage_uv); // stable, as OrderBy
    if pts.len() < 5 {
        return 0;
    }
    let top = pts[pts.len() - 1];
    let top_freq = top.frequency_khz;
    let mut i = pts.len() - 1;
    while i > 0 && (pts[i - 1].frequency_khz as i64 - top_freq as i64).abs() <= 1000 {
        i -= 1;
    }

    // Stock curves flatten out naturally at the top — a 4070 Ti has two adjacent points at
    // 2820 MHz with no edit applied. Only call it a flatten when the plateau is long enough and
    // wide enough in volts that it cannot be the curve's own shape.
    let plateau_points = pts.len() - i;
    let plateau_mv = (top.voltage_uv - pts[i].voltage_uv) / 1000;
    if plateau_points < 4 || plateau_mv < 25 {
        return 0;
    }
    pts[i].voltage_uv
}

/// Highest voltage on the curve — the stock ceiling a negative offset is measured from.
pub fn max_voltage_uv(points: &[VfPoint]) -> u32 {
    points.iter().filter(|p| is_valid(p)).map(|p| p.voltage_uv).max().unwrap_or(0)
}

/// Per-point deltas that drive each edited point to an explicit target frequency (graphical
/// editor). Indices absent from the map keep stock; deltas are clamped to the driver's range.
pub fn compute_target_deltas(
    base_points: &[VfPoint],
    target_khz_by_index: &BTreeMap<i32, i32>,
    range_min_khz: i32,
    range_max_khz: i32,
) -> Vec<i32> {
    let mut deltas = vec![0i32; base_points.len()];
    for (&i, &target) in target_khz_by_index {
        if i < 0 || i as usize >= base_points.len() {
            continue;
        }
        let p = &base_points[i as usize];
        if !is_valid(p) {
            continue;
        }
        deltas[i as usize] = clamp_i32(target.wrapping_sub(p.frequency_khz), range_min_khz, range_max_khz);
    }
    deltas
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANY: (i32, i32) = (i32::MIN, i32::MAX);

    fn flatten(p: &[VfPoint], cap: u32) -> Vec<i32> {
        compute_flatten_deltas(p, cap, 0, ANY.0, ANY.1)
    }

    fn stock() -> Vec<VfPoint> {
        // 700 mV→1500 MHz rising to 1100 mV→2700 MHz, 150 MHz per 50 mV.
        (0..=8).map(|k| VfPoint::new(((700 + k * 50) * 1000) as u32, (1500 + k * 150) * 1000)).collect()
    }

    #[test]
    fn undervolt_maths() {
        let stock = stock();
        assert_eq!(max_voltage_uv(&stock), 1_100_000);

        let d = flatten(&stock, 900_000);
        assert_eq!(d[0], 0, "below cap untouched (700mV)");
        assert_eq!(d[3], 0, "below cap untouched (850mV)");
        assert_eq!(d[4], 0, "at cap unchanged");
        assert_eq!(d[5], -150_000, "950mV clamped down 150MHz");
        assert_eq!(d[8], -600_000, "1100mV clamped down 600MHz");

        let eff: Vec<VfPoint> =
            stock.iter().zip(&d).map(|(p, d)| VfPoint::new(p.voltage_uv, p.frequency_khz + d)).collect();
        let mut plateau: Vec<i32> = eff[4..].iter().map(|p| p.frequency_khz).collect();
        plateau.dedup();
        assert_eq!(plateau.len(), 1, "all points >= cap share one frequency");
        assert_eq!(eff[8].frequency_khz, 2_100_000, "plateau sits at the cap frequency");

        assert_eq!(infer_cap_voltage_uv(&eff), 900_000, "infer cap from flattened curve");
        assert_eq!(infer_cap_voltage_uv(&stock), 0, "infer cap from stock curve = none");

        let mut natural = stock.clone();
        let n = natural.len();
        natural[n - 1] = VfPoint::new(natural[n - 1].voltage_uv, natural[n - 2].frequency_khz);
        assert_eq!(infer_cap_voltage_uv(&natural), 0, "natural top-of-curve flat spot is not a cap");

        let d2 = compute_flatten_deltas(&stock, 900_000, 100_000, ANY.0, ANY.1);
        assert_eq!(d2[4], 100_000, "flatten+offset: cap point lifted");
        assert_eq!(d2[8], -500_000, "flatten+offset: top clamped to lifted target");

        let d3 = compute_flatten_deltas(&stock, 900_000, 0, -200_000, 200_000);
        assert_eq!(d3[8], -200_000, "clamped to driver min delta");

        assert!(flatten(&stock, 0).iter().all(|&x| x == 0), "cap 0 = no deltas");

        let d4 = flatten(&stock, 500_000);
        assert_eq!(d4[0], 0, "cap below curve: lowest point kept");
        assert_eq!(d4[8], 1_500_000 - 2_700_000, "cap below curve: top clamped to lowest freq");

        let targets = BTreeMap::from([(4, 2_200_000), (6, 2_300_000)]);
        let dt = compute_target_deltas(&stock, &targets, ANY.0, ANY.1);
        assert_eq!(dt[4], 2_200_000 - 2_100_000);
        assert_eq!(dt[6], 2_300_000 - 2_400_000);
        assert_eq!(dt[0], 0);
        assert_eq!(dt[7], 0);
        let clamp = compute_target_deltas(&stock, &BTreeMap::from([(8, 9_000_000)]), -200_000, 200_000);
        assert_eq!(clamp[8], 200_000, "target clamped to driver max");
        let oob = compute_target_deltas(&stock, &BTreeMap::from([(99, 5_000_000), (-1, 5_000_000)]), ANY.0, ANY.1);
        assert!(oob.iter().all(|&x| x == 0), "out-of-range indices ignored");

        let sparse = [
            VfPoint::new(0, 0),
            VfPoint::new(800_000, 1_800_000),
            VfPoint::new(0, 0),
            VfPoint::new(1_000_000, 2_400_000),
        ];
        let d5 = flatten(&sparse, 800_000);
        assert_eq!(d5[0], 0, "sparse: empty slot gets no delta");
        assert_eq!(d5[2], 0, "sparse: empty slot 2 gets no delta");
        assert_eq!(d5[3], 1_800_000 - 2_400_000, "sparse: real point clamped");
        assert!(flatten(&[], 900_000).is_empty(), "empty curve is safe");
    }
}
