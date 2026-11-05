//! The overdrive tables, as the driver prints them. Pure text in, plain values out.
//!
//! `pp_od_clk_voltage` differs by generation. RDNA 2 and 3 print a minimum and maximum core clock
//! ("OD_SCLK: 0: 500Mhz / 1: 2500Mhz"), RDNA 4 a core clock offset ("OD_SCLK_OFFSET: 100Mhz"), and
//! Polaris and Vega 10 a table of clock and voltage per level, which nothing here drives. The
//! fan files under `gpu_od/fan_ctrl` print a value and, only when it can be changed, a range.

/// A window the driver allows, inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub min: i32,
    pub max: i32,
}

impl Range {
    pub fn clamp(&self, value: i32) -> i32 {
        value.clamp(self.min, self.max.max(self.min))
    }
}

/// The leading number of a token such as `500Mhz`, `-50mV` or `23%`.
fn number(token: &str) -> Option<i32> {
    let end = token.char_indices().find(|&(i, c)| !(c.is_ascii_digit() || (i == 0 && c == '-'))).map(|(i, _)| i);
    token[..end.unwrap_or(token.len())].parse().ok()
}

/// `SCLK: 500Mhz 3000Mhz` → (`SCLK`, the window).
fn range_line(line: &str) -> Option<(&str, Range)> {
    let (key, rest) = line.split_once(':')?;
    let mut values = rest.split_whitespace().map(number);
    let (min, max) = (values.next()??, values.next()??);
    Some((key.trim(), Range { min, max }))
}

/// `pp_od_clk_voltage`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OdTable {
    pub sclk_min: Option<i32>,
    pub sclk_max: Option<i32>,
    pub sclk_offset: Option<i32>,
    pub mclk_min: Option<i32>,
    pub mclk_max: Option<i32>,
    pub voltage_offset: Option<i32>,
    pub sclk_range: Option<Range>,
    pub sclk_offset_range: Option<Range>,
    pub mclk_range: Option<Range>,
    pub voltage_offset_range: Option<Range>,
}

impl OdTable {
    pub fn parse(text: &str) -> OdTable {
        #[derive(PartialEq)]
        enum Section {
            None,
            Sclk,
            SclkOffset,
            Mclk,
            VoltageOffset,
            Range,
        }
        let mut table = OdTable::default();
        let mut section = Section::None;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if line.starts_with("OD_") {
                section = match line.trim_end_matches(':') {
                    "OD_SCLK" => Section::Sclk,
                    "OD_SCLK_OFFSET" => Section::SclkOffset,
                    "OD_MCLK" => Section::Mclk,
                    "OD_VDDGFX_OFFSET" => Section::VoltageOffset,
                    "OD_RANGE" => Section::Range,
                    // OD_VDDC_CURVE, OD_FCLK, OD_CCLK: real, and not driven from here.
                    _ => Section::None,
                };
                continue;
            }
            match section {
                Section::Sclk | Section::Mclk => {
                    let mut tokens = line.split_whitespace();
                    let level = tokens.next().and_then(number);
                    let value = tokens.next().and_then(number);
                    // A third column is a voltage: the per-level table of Polaris and Vega 10.
                    if tokens.next().is_some() {
                        continue;
                    }
                    let (min, max) = if section == Section::Sclk {
                        (&mut table.sclk_min, &mut table.sclk_max)
                    } else {
                        (&mut table.mclk_min, &mut table.mclk_max)
                    };
                    match level {
                        Some(0) => *min = value,
                        Some(1) => *max = value,
                        _ => {}
                    }
                }
                Section::SclkOffset => table.sclk_offset = number(line),
                Section::VoltageOffset => table.voltage_offset = number(line),
                Section::Range => {
                    if let Some((key, range)) = range_line(line) {
                        match key {
                            "SCLK" => table.sclk_range = Some(range),
                            "SCLK_OFFSET" => table.sclk_offset_range = Some(range),
                            "MCLK" => table.mclk_range = Some(range),
                            "VDDGFX_OFFSET" => table.voltage_offset_range = Some(range),
                            _ => {}
                        }
                    }
                }
                Section::None => {}
            }
        }
        table
    }

    /// The core control is a maximum clock (RDNA 2 and 3) rather than an offset (RDNA 4).
    pub fn core_is_absolute(&self) -> bool {
        self.sclk_max.is_some() && self.sclk_range.is_some()
    }

    pub fn core_is_offset(&self) -> bool {
        self.sclk_offset.is_some() && self.sclk_offset_range.is_some()
    }

    pub fn can_set_core_min(&self) -> bool {
        self.sclk_min.is_some() && self.sclk_range.is_some()
    }

    pub fn can_set_memory(&self) -> bool {
        self.mclk_max.is_some() && self.mclk_range.is_some()
    }

    pub fn can_set_voltage_offset(&self) -> bool {
        self.voltage_offset.is_some() && self.voltage_offset_range.is_some()
    }
}

/// `gpu_od/fan_ctrl/fan_curve`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FanCurveTable {
    /// (temperature °C, fan %) per anchor point.
    pub points: Vec<(i32, i32)>,
    pub temp_range: Option<Range>,
    pub speed_range: Option<Range>,
}

impl FanCurveTable {
    pub fn parse(text: &str) -> FanCurveTable {
        let mut table = FanCurveTable::default();
        let mut in_range = false;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if line.starts_with("OD_RANGE") {
                in_range = true;
            } else if line.starts_with("OD_FAN_CURVE") {
                in_range = false;
            } else if in_range {
                if let Some((key, range)) = range_line(line) {
                    if key.contains("temp") {
                        table.temp_range = Some(range);
                    } else if key.contains("speed") {
                        table.speed_range = Some(range);
                    }
                }
            } else {
                let mut tokens = line.split_whitespace().skip(1).map(number);
                if let (Some(Some(temp)), Some(Some(speed))) = (tokens.next(), tokens.next()) {
                    table.points.push((temp, speed));
                }
            }
        }
        table
    }

    /// Both windows are there: the file prints them only when the curve can be changed.
    pub fn is_writable(&self) -> bool {
        !self.points.is_empty() && self.temp_range.is_some() && self.speed_range.is_some()
    }

    /// The mode the table shows. Nothing reports the firmware's fan mode directly, but a card that
    /// has not been given a curve lists every point as 0 °C at 0 %, and a duty written here is the
    /// same duty at every point.
    pub fn mode(&self) -> FanTableMode {
        let Some(&(_, first)) = self.points.first() else { return FanTableMode::Auto };
        if self.points.iter().all(|&p| p == (0, 0)) {
            FanTableMode::Auto
        } else if self.points.iter().all(|&(_, speed)| speed == first) {
            FanTableMode::Fixed(first)
        } else {
            FanTableMode::Curve
        }
    }
}

/// See [`FanCurveTable::mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanTableMode {
    Auto,
    Fixed(i32),
    Curve,
}

/// The attributes under `gpu_od/fan_ctrl` that hold one number, such as `fan_zero_rpm_enable`: the
/// value, and the window it may move in when it may move at all.
pub fn parse_single(text: &str) -> (Option<i32>, Option<Range>) {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let mut value = None;
    let mut range = None;
    let mut in_range = false;
    for line in &mut lines {
        if line.starts_with("OD_RANGE") {
            in_range = true;
        } else if in_range {
            range = range_line(line).map(|(_, r)| r);
        } else if !line.ends_with(':') {
            value = number(line);
        }
    }
    (value, range)
}

/// The clock marked current in a `pp_dpm_*` list: `1: 1000Mhz *`.
pub fn current_level_mhz(text: &str) -> Option<i32> {
    let line = text.lines().find(|l| l.trim_end().ends_with('*'))?;
    number(line.split_whitespace().nth(1)?)
}

/// `count` anchor points: temperatures spread evenly across the window, each taking its speed from
/// `speed_at`. Temperatures stay strictly rising and speeds never fall, since the firmware runs a
/// straight line between neighbours and a falling one has no meaning to it.
pub fn plan_fan_curve(
    speed_at: impl Fn(f64) -> f64,
    count: usize,
    temp: Range,
    speed: Range,
) -> Vec<(usize, i32, i32)> {
    let mut points = Vec::with_capacity(count);
    let steps = count.saturating_sub(1).max(1) as i32;
    let (mut last_temp, mut last_speed) = (i32::MIN, speed.min);
    for i in 0..count {
        let spread = temp.min + (temp.max - temp.min) * i as i32 / steps;
        // Rising is the firmware's rule and the window is the driver's; if they ever conflict, the
        // window wins, because the driver refuses the whole write otherwise.
        let t = temp.clamp(spread.max(last_temp.saturating_add(1)));
        let s = speed.clamp(roch_core::util::round_even_i32(speed_at(f64::from(t)))).max(last_speed);
        points.push((i, t, s));
        last_temp = t;
        last_speed = s;
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    // As smu_v13_0_0_ppt.c prints them (the RX 7900 family), including its mixed Mhz and MHz.
    const RDNA3: &str = "OD_SCLK:\n0: 500Mhz\n1: 2525Mhz\nOD_MCLK:\n0: 97Mhz\n1: 1250MHz\nOD_VDDGFX_OFFSET:\n0mV\n\
                         OD_RANGE:\nSCLK:     500Mhz       3000Mhz\nMCLK:      97Mhz       1500Mhz\n\
                         VDDGFX_OFFSET:    -200mv          0mv\n";
    // Exactly what a real RX 7900 XT printed under kernel 7.2: the range section comes before the
    // voltage offset, and the offset's window is wider than the kernel source suggests.
    const REAL_7900XT: &str = "OD_SCLK:\n0: 500Mhz\n1: 2815Mhz\nOD_MCLK:\n0: 97Mhz\n1: 1250MHz\nOD_RANGE:\n\
                               SCLK:     500Mhz       5000Mhz\nMCLK:      97Mhz       1500Mhz\n\
                               VDDGFX_OFFSET:    -450mv          0mv\nOD_VDDGFX_OFFSET:\n0mV\n";
    // smu_v14_0_2_ppt.c (RDNA 4).
    const RDNA4: &str = "OD_SCLK_OFFSET:\n100Mhz\nOD_MCLK:\n0: 97Mhz\n1: 1218MHz\nOD_VDDGFX_OFFSET:\n-50mV\n\
                         OD_RANGE:\nSCLK_OFFSET:    -500Mhz       1000Mhz\nMCLK:      97Mhz       1500Mhz\n\
                         VDDGFX_OFFSET:    -200mv          0mv\n";
    // Polaris: clock and voltage per level.
    const POLARIS: &str = "OD_SCLK:\n0:        300MHz        750mV\n1:        588MHz        765mV\n\
                           OD_MCLK:\n0:        300MHz        750mV\n1:       2000MHz        800mV\n\
                           OD_RANGE:\nSCLK:     300MHz       2000MHz\nMCLK:     300MHz       2250MHz\n\
                           VDDC:     750mV        1150mV\n";
    // Vega 20 and Navi 1x: a maximum memory clock only, and a voltage curve.
    const NAVI10: &str = "OD_SCLK:\n0: 800Mhz\n1: 2100Mhz\nOD_MCLK:\n1: 875MHz\nOD_VDDC_CURVE:\n0: 800MHz 711mV\n\
                          OD_RANGE:\nSCLK:     800Mhz       2100Mhz\nMCLK:     625Mhz        930Mhz\n\
                          VDDC_CURVE_SCLK[0]:     800Mhz       2100Mhz\n";

    #[test]
    fn rdna3_is_an_absolute_clock_window_and_a_voltage_offset() {
        let t = OdTable::parse(RDNA3);
        assert_eq!((t.sclk_min, t.sclk_max), (Some(500), Some(2525)));
        assert_eq!((t.mclk_min, t.mclk_max), (Some(97), Some(1250)));
        assert_eq!(t.voltage_offset, Some(0));
        assert_eq!(t.sclk_range, Some(Range { min: 500, max: 3000 }));
        assert_eq!(t.mclk_range, Some(Range { min: 97, max: 1500 }));
        assert_eq!(t.voltage_offset_range, Some(Range { min: -200, max: 0 }));
        assert!(t.core_is_absolute() && !t.core_is_offset() && t.can_set_core_min());
        assert!(t.can_set_memory() && t.can_set_voltage_offset());
    }

    #[test]
    fn a_real_table_reads_whatever_order_its_sections_come_in() {
        let t = OdTable::parse(REAL_7900XT);
        assert_eq!((t.sclk_min, t.sclk_max), (Some(500), Some(2815)));
        assert_eq!((t.mclk_min, t.mclk_max), (Some(97), Some(1250)));
        assert_eq!(t.voltage_offset, Some(0), "it follows the range section here");
        assert_eq!(t.sclk_range, Some(Range { min: 500, max: 5000 }));
        assert_eq!(t.mclk_range, Some(Range { min: 97, max: 1500 }));
        assert_eq!(t.voltage_offset_range, Some(Range { min: -450, max: 0 }));
        assert!(t.core_is_absolute() && t.can_set_core_min() && t.can_set_memory() && t.can_set_voltage_offset());
    }

    #[test]
    fn rdna4_is_a_core_offset() {
        let t = OdTable::parse(RDNA4);
        assert_eq!(t.sclk_offset, Some(100));
        assert_eq!(t.sclk_offset_range, Some(Range { min: -500, max: 1000 }));
        assert_eq!(t.voltage_offset, Some(-50));
        assert!(t.core_is_offset() && !t.core_is_absolute() && !t.can_set_core_min());
    }

    #[test]
    fn per_level_tables_are_not_read_as_a_window() {
        let t = OdTable::parse(POLARIS);
        assert_eq!((t.sclk_min, t.sclk_max), (None, None));
        assert!(!t.core_is_absolute() && !t.can_set_memory() && !t.can_set_voltage_offset());
    }

    #[test]
    fn a_table_with_no_memory_minimum_still_offers_the_maximum() {
        let t = OdTable::parse(NAVI10);
        assert_eq!((t.mclk_min, t.mclk_max), (None, Some(875)));
        assert!(t.can_set_memory() && t.core_is_absolute());
        assert!(!t.can_set_voltage_offset(), "a voltage curve is not an offset");
    }

    #[test]
    fn an_empty_file_offers_nothing() {
        assert_eq!(OdTable::parse("\n"), OdTable::default());
        assert!(!FanCurveTable::parse("").is_writable());
    }

    // A real RX 7900 XT's table as it boots, apart from its floor of 15 %.
    const FAN_FRESH: &str = "OD_FAN_CURVE:\n0: 0C 0%\n1: 0C 0%\n2: 0C 0%\n3: 0C 0%\n4: 0C 0%\nOD_RANGE:\n\
                             FAN_CURVE(hotspot temp): 25C 100C\nFAN_CURVE(fan speed): 23% 100%\n";

    #[test]
    fn a_fresh_fan_table_reads_as_auto_and_gives_its_windows() {
        let t = FanCurveTable::parse(FAN_FRESH);
        assert_eq!(t.points, vec![(0, 0); 5]);
        assert_eq!(t.temp_range, Some(Range { min: 25, max: 100 }));
        assert_eq!(t.speed_range, Some(Range { min: 23, max: 100 }));
        assert!(t.is_writable());
        assert_eq!(t.mode(), FanTableMode::Auto);
    }

    #[test]
    fn fan_modes_are_read_from_the_table() {
        let flat = "OD_FAN_CURVE:\n0: 25C 60%\n1: 43C 60%\n2: 62C 60%\n3: 81C 60%\n4: 100C 60%\n";
        assert_eq!(FanCurveTable::parse(flat).mode(), FanTableMode::Fixed(60));
        let curve = "OD_FAN_CURVE:\n0: 25C 30%\n1: 43C 40%\n2: 62C 60%\n3: 81C 80%\n4: 100C 100%\n";
        assert_eq!(FanCurveTable::parse(curve).mode(), FanTableMode::Curve);
        assert!(!FanCurveTable::parse(curve).is_writable(), "no range printed: locked");
    }

    #[test]
    fn single_values_read_with_and_without_a_range() {
        let t = "FAN_ZERO_RPM_ENABLE:\n1\nOD_RANGE:\nZERO_RPM_ENABLE: 0 1\n";
        assert_eq!(parse_single(t), (Some(1), Some(Range { min: 0, max: 1 })));
        assert_eq!(parse_single("FAN_ZERO_RPM_ENABLE:\n0\n"), (Some(0), None));
        assert_eq!(parse_single(""), (None, None));
    }

    #[test]
    fn current_level_is_the_starred_one() {
        // The real lists from an RX 7900 XT, including the S: line for a clock off the table.
        assert_eq!(current_level_mhz("0: 601Mhz \n1: 1000Mhz *\n2: 1200Mhz \n"), Some(1000));
        assert_eq!(current_level_mhz("S: 121Mhz *\n1: 500Mhz \n2: 2075Mhz \n"), Some(121));
        assert_eq!(current_level_mhz("0: 601Mhz \n1: 1000Mhz \n"), None);
    }

    #[test]
    fn a_planned_curve_is_spread_rising_and_inside_the_windows() {
        let (temp, speed) = (Range { min: 25, max: 100 }, Range { min: 23, max: 100 });
        let plan = plan_fan_curve(|c| c, 5, temp, speed);
        assert_eq!(plan, vec![(0, 25, 25), (1, 43, 43), (2, 62, 62), (3, 81, 81), (4, 100, 100)]);
        // A duty under the floor is raised to it, and a falling curve is held level.
        let plan = plan_fan_curve(|c| 120.0 - c, 5, temp, speed);
        assert!(plan.iter().all(|&(_, _, s)| s == 95), "{plan:?}");
        assert_eq!(plan_fan_curve(|_| 0.0, 5, temp, speed).last(), Some(&(4, 100, 23)));
        // A window too narrow for five rising points stays in the window.
        let plan = plan_fan_curve(|_| 50.0, 5, Range { min: 30, max: 32 }, speed);
        assert!(plan.iter().all(|&(_, t, _)| (30..=32).contains(&t)), "{plan:?}");
    }

    #[test]
    fn the_curve_a_real_card_was_given() {
        // Read back from an RX 7900 XT after this plan: 25C 20%, 43C 30%, 62C 50%, 81C 82%, 100C 100%.
        let curve = roch_core::FanCurve::with_points(vec![
            roch_core::FanPoint::new(30.0, 20.0),
            roch_core::FanPoint::new(50.0, 35.0),
            roch_core::FanPoint::new(70.0, 60.0),
            roch_core::FanPoint::new(90.0, 100.0),
        ]);
        let plan = plan_fan_curve(|t| curve.evaluate(t), 5, Range { min: 25, max: 100 }, Range { min: 15, max: 100 });
        assert_eq!(plan, vec![(0, 25, 20), (1, 43, 30), (2, 62, 50), (3, 81, 82), (4, 100, 100)]);
    }

    #[test]
    fn numbers_keep_their_sign_and_lose_their_units() {
        assert_eq!(number("-50mV"), Some(-50));
        assert_eq!(number("2525Mhz"), Some(2525));
        assert_eq!(number("23%"), Some(23));
        assert_eq!(number("mV"), None);
        assert_eq!(number(""), None);
    }
}
