//! Replacements for the five historical AMD developer scripts.
mod native;
use crate::Result;
use std::fmt::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Probe,
    Exports,
    Features,
    WriteCheck,
    Telemetry,
}

struct Options {
    mode: Mode,
    mock: bool,
    apply: bool,
    adapter: Option<i32>,
    offset: i32,
    out: Option<std::path::PathBuf>,
}
impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let mode = match args.first().map(String::as_str) {
            Some("probe") => Mode::Probe,
            Some("exports") => Mode::Exports,
            Some("features") => Mode::Features,
            Some("write-check") => Mode::WriteCheck,
            Some("telemetry") => Mode::Telemetry,
            _ => {
                return Err(
                    "Choose AMD probe, exports, features, write-check, or telemetry. See cargo xtask --help.".into()
                );
            }
        };
        let mut value = Self { mode, mock: false, apply: false, adapter: None, offset: 200, out: None };
        let mut args = args[1..].iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--mock" => value.mock = true,
                "--apply" => value.apply = true,
                "--adapter" => {
                    let n = args.next().ok_or("--adapter needs an index")?.parse::<i32>().map_err(|e| e.to_string())?;
                    if n < 0 {
                        return Err("Adapter index must be nonnegative".into());
                    }
                    value.adapter = Some(n);
                }
                "--offset" => {
                    value.offset = args
                        .next()
                        .ok_or("--offset needs MHz")?
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?
                }
                "--out" => value.out = Some(args.next().ok_or("--out needs a filename")?.into()),
                _ => return Err(format!("Unknown AMD option: {arg}")),
            }
        }
        if value.mode == Mode::WriteCheck && !value.mock && !value.apply {
            return Err("Real write-check requires --apply. Use --mock to test without hardware.".into());
        }
        if value.mode != Mode::WriteCheck && (value.apply || value.offset != 200) {
            return Err("--apply and --offset are only for write-check".into());
        }
        Ok(value)
    }
}

#[derive(Clone)]
struct Adapter {
    index: i32,
    name: String,
}
struct Feature {
    id: usize,
    minimum: i32,
    maximum: i32,
    default: i32,
}
trait Driver {
    fn adapters(&mut self) -> Result<Vec<Adapter>>;
    fn details(&mut self, adapter: i32) -> Result<String>;
    fn features(&mut self, adapter: i32, seed: i32) -> Result<(i32, Vec<Feature>)>;
    fn current(&mut self, adapter: i32) -> Result<Vec<i32>>;
    fn sensors(&mut self, adapter: i32) -> Result<Vec<(usize, i32)>>;
    fn stream(&mut self, adapter: i32) -> Result<String>;
    fn write(&mut self, adapter: i32, pairs: &[(usize, i32)], count: i32) -> Result<()>;
    fn wait(&self, milliseconds: u64);
}

pub fn run(args: &[String]) -> Result<()> {
    let options = Options::parse(args)?;
    let mut text = String::from("Roch GPU native AMD diagnostics\n");
    if options.mock {
        text.push_str("SIMULATED — no driver loaded\n");
    }
    if options.mode == Mode::Exports {
        let exports = if options.mock {
            vec!["ADL2_Overdrive8_Init_SettingX2_Get".into(), "ADL2_New_QueryPMLogData_Get".into()]
        } else {
            let path = native::system32().join("atiadlxx.dll");
            crate::pe::exports(&std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?)?
        };
        for name in exports.iter().filter(|s| {
            (s.starts_with("ADL_") || s.starts_with("ADL2_"))
                && ["Overdrive", "PerfTuning", "PMLog", "PowerTune", "Tuning"].iter().any(|part| s.contains(part))
        }) {
            writeln!(text, "{name}").unwrap();
        }
    }
    if options.mock {
        report(&options, &mut Fake::default(), &mut text)?;
    } else {
        for library in ["atiadlxx.dll", "amdadlx64.dll", "amdadlx32.dll"] {
            writeln!(
                text,
                "{library}: {}",
                if native::system32().join(library).is_file() { "present" } else { "missing" }
            )
            .unwrap();
        }
        report(&options, &mut native::Session::new()?, &mut text)?;
    }
    if let Some(path) = options.out {
        std::fs::write(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    print!("{text}");
    Ok(())
}

fn report(options: &Options, driver: &mut impl Driver, text: &mut String) -> Result<()> {
    let adapters = driver.adapters()?;
    let selected: Vec<_> =
        adapters.into_iter().filter(|a| options.adapter.is_none_or(|index| a.index == index)).collect();
    if selected.is_empty() {
        return Err("No matching AMD adapter was reported".into());
    }
    for adapter in selected {
        writeln!(text, "\nAdapter {}: {}", adapter.index, adapter.name).unwrap();
        match options.mode {
            Mode::WriteCheck => write_check(driver, adapter.index, options.offset, text)?,
            Mode::Telemetry => text.push_str(&driver.stream(adapter.index)?),
            mode => {
                text.push_str(&driver.details(adapter.index)?);
                let seeds: &[i32] = if mode == Mode::Exports { &[0, 75] } else { &[75] };
                for &seed in seeds {
                    match driver.features(adapter.index, seed) {
                        Ok((caps, features)) => {
                            writeln!(text, "OD8 capabilities=0x{caps:08X}, count seed={seed}").unwrap();
                            let values = driver.current(adapter.index).unwrap_or_default();
                            for feature in features {
                                writeln!(
                                    text,
                                    "[{:2}] {:40} [{} .. {}] default={} current={}",
                                    feature.id,
                                    native::feature_name(feature.id),
                                    feature.minimum,
                                    feature.maximum,
                                    feature.default,
                                    values.get(feature.id).map_or("unavailable".into(), i32::to_string)
                                )
                                .unwrap();
                            }
                        }
                        Err(error) => {
                            writeln!(text, "OD8 count seed={seed}: {error}").unwrap();
                        }
                    }
                }
                for (id, value) in driver.sensors(adapter.index)? {
                    writeln!(text, "PMLog sensor[{id}] = {value}").unwrap();
                }
            }
        }
    }
    Ok(())
}

struct Restore<'a, D: Driver> {
    driver: &'a mut D,
    adapter: i32,
    baseline: Vec<i32>,
    touched: Vec<usize>,
    armed: bool,
}
impl<D: Driver> Restore<'_, D> {
    fn restore(&mut self) -> Result<()> {
        let current = self.driver.current(self.adapter);
        let pairs: Vec<_> = match current {
            Ok(values) => self
                .baseline
                .iter()
                .enumerate()
                .filter(|(id, value)| values.get(*id) != Some(*value))
                .map(|(id, value)| (id, *value))
                .collect(),
            Err(_) => self.touched.iter().map(|id| (*id, self.baseline[*id])).collect(),
        };
        if !pairs.is_empty() {
            self.driver.write(self.adapter, &pairs, 75)?;
        }
        let after = self.driver.current(self.adapter)?;
        if after != self.baseline {
            return Err("Restoration readback differs from the original OD8 snapshot".into());
        }
        self.armed = false;
        Ok(())
    }
}
impl<D: Driver> Drop for Restore<'_, D> {
    fn drop(&mut self) {
        if self.armed
            && let Err(error) = self.restore()
        {
            eprintln!("AMD restoration failed: {error}");
        }
    }
}

fn write_check(driver: &mut impl Driver, adapter: i32, offset: i32, text: &mut String) -> Result<()> {
    let (_, features) = driver.features(adapter, 75)?;
    let core = features.iter().find(|f| f.id == 0).ok_or("No core-clock offset feature")?;
    if offset < core.minimum || offset > core.maximum || core.minimum == core.maximum {
        return Err("Requested offset is outside the driver's adjustable range".into());
    }
    let baseline = driver.current(adapter)?;
    if baseline.len() <= 37 {
        return Err("Incomplete OD8 snapshot; refusing to write".into());
    }
    let recipes = [
        ("FMAX, count=75", 75, vec![(0, offset)]),
        ("FMAX, count=77", 77, vec![(0, offset)]),
        ("FMAX + original FMIN", 75, vec![(0, offset), (1, baseline[1])]),
        ("FMAX + original UCLK/VOLT", 75, vec![(0, offset), (8, baseline[8]), (37, baseline[37])]),
    ];
    let mut guard = Restore { driver, adapter, baseline, touched: Vec::new(), armed: false };
    for (name, count, pairs) in recipes {
        guard.armed = true; // Arm before calling: a failed native write can still partially apply.
        guard.touched = pairs.iter().map(|(id, _)| *id).collect();
        let write = guard.driver.write(adapter, &pairs, count);
        guard.driver.wait(400);
        let read = guard.driver.current(adapter);
        let stuck = write.is_ok() && read.as_ref().is_ok_and(|v| v.first() == Some(&offset));
        writeln!(text, "{name}: {}", if stuck { "readback matched" } else { "did not match" }).unwrap();
        if let Err(error) = write {
            writeln!(text, "  {error}").unwrap();
        }
        if stuck {
            for _ in 0..5 {
                writeln!(
                    text,
                    "  live core MHz: {:?}",
                    guard.driver.sensors(adapter)?.into_iter().find(|(id, _)| *id == 1).map(|(_, value)| value)
                )
                .unwrap();
                guard.driver.wait(700);
            }
        }
        guard.restore()?;
        writeln!(text, "  original snapshot restored and verified").unwrap();
        read?;
    }
    Ok(())
}

struct Fake {
    values: Vec<i32>,
    fail_after_write: bool,
    fail_sensors: bool,
    writes: usize,
}
impl Default for Fake {
    fn default() -> Self {
        let mut values = vec![0; 77];
        values[0] = 75;
        values[1] = -25;
        values[8] = 1000;
        values[37] = -50;
        Self { values, fail_after_write: false, fail_sensors: false, writes: 0 }
    }
}
impl Driver for Fake {
    fn adapters(&mut self) -> Result<Vec<Adapter>> {
        Ok(vec![Adapter { index: 0, name: "Simulated AMD GPU".into() }])
    }
    fn details(&mut self, _: i32) -> Result<String> {
        Ok("Overdrive supported=1 enabled=1 version=8\nADLX version: simulated\n".into())
    }
    fn features(&mut self, _: i32, seed: i32) -> Result<(i32, Vec<Feature>)> {
        if seed == 0 {
            return Err("null pointer (legacy zero-count call)".into());
        }
        Ok((1, vec![Feature { id: 0, minimum: -1000, maximum: 1000, default: 0 }]))
    }
    fn current(&mut self, _: i32) -> Result<Vec<i32>> {
        Ok(self.values.clone())
    }
    fn sensors(&mut self, _: i32) -> Result<Vec<(usize, i32)>> {
        if self.fail_sensors {
            return Err("injected telemetry failure".into());
        }
        Ok(vec![(1, 2500 + self.values[0]), (8, 40), (73, 120)])
    }
    fn stream(&mut self, a: i32) -> Result<String> {
        Ok(format!(
            "Streaming version=1 sample_ms=1000\n{:?}\nTelemetry collection stopped; device destroyed.\n",
            self.sensors(a)?
        ))
    }
    fn write(&mut self, _: i32, pairs: &[(usize, i32)], _: i32) -> Result<()> {
        self.writes += 1;
        for &(id, value) in pairs {
            self.values[id] = value;
        }
        if self.fail_after_write && pairs == [(0, 200)] { Err("injected partial write failure".into()) } else { Ok(()) }
    }
    fn wait(&self, _: u64) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_writes_require_explicit_flag_before_loading_a_driver() {
        assert!(Options::parse(&["write-check".into()]).is_err());
        assert!(Options::parse(&["write-check".into(), "--mock".into()]).is_ok());
        assert!(Options::parse(&["probe".into(), "--apply".into()]).is_err());
    }
    #[test]
    fn all_four_shapes_restore_nonzero_original_settings_even_after_partial_failure() {
        for fail_after_write in [false, true] {
            let mut fake = Fake { fail_after_write, ..Default::default() };
            let baseline = fake.values.clone();
            let mut report = String::new();
            write_check(&mut fake, 0, 200, &mut report).unwrap();
            assert_eq!(fake.values, baseline);
            assert_eq!(fake.writes, 8);
            assert_eq!(report.matches("original snapshot restored and verified").count(), 4);
        }
    }
    #[test]
    fn invalid_range_is_rejected_before_the_first_write() {
        let mut fake = Fake::default();
        assert!(write_check(&mut fake, 0, 2000, &mut String::new()).is_err());
        assert_eq!(fake.writes, 0);
    }
    #[test]
    fn telemetry_failure_after_a_write_restores_the_original_snapshot() {
        let mut fake = Fake { fail_sensors: true, ..Default::default() };
        let original = fake.values.clone();
        assert!(write_check(&mut fake, 0, 200, &mut String::new()).is_err());
        assert_eq!(fake.values, original);
        assert_eq!(fake.writes, 2);
    }
    #[test]
    fn read_only_modes_never_write() {
        for mode in [Mode::Probe, Mode::Exports, Mode::Features, Mode::Telemetry] {
            let options = Options { mode, mock: true, apply: false, adapter: None, offset: 200, out: None };
            let mut fake = Fake::default();
            let mut text = String::new();
            report(&options, &mut fake, &mut text).unwrap();
            assert_eq!(fake.writes, 0);
            assert!(text.contains("Simulated AMD"));
        }
    }
}
