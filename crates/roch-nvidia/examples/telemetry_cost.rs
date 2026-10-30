//! Read-only: how long each driver call behind one telemetry sample takes on this card, and the
//! whole sample through the backend. Nothing is written.
//! `cargo run --release -p roch-nvidia --example telemetry_cost`

use std::time::Instant;

use roch_core::GpuBackend;
use roch_nvidia::{NvidiaBackend, nvapi, nvml, private, supplemental, wrapper};

const RUNS: usize = 30;

fn time(name: &str, mut f: impl FnMut()) {
    f(); // first call can carry one-off setup
    let mut ms: Vec<f64> = (0..RUNS)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    ms.sort_by(f64::total_cmp);
    println!("{name:<34} median {:>7.3} ms   max {:>7.3} ms", ms[RUNS / 2], ms[RUNS - 1]);
}

fn main() {
    let mut b = NvidiaBackend::new();
    if let Err(e) = b.initialize() {
        eprintln!("backend init failed: {}", e.message);
        return;
    }
    let g = nvapi::enum_physical_gpus().unwrap_or_default()[0];
    let mask = private::probe_mask(g);

    println!("-- single calls (one sample makes each once)");
    time("private::read_power_rails", || drop(private::read_power_rails(g)));
    time("nvapi::clock_frequencies", || drop(nvapi::clock_frequencies(g, 0)));
    time("nvapi::thermal_settings", || drop(nvapi::thermal_settings(g)));
    time("private::read_thermal_slots", || drop(private::read_thermal_slots(g, mask)));
    time("nvapi::current_voltage_uv", || drop(nvapi::current_voltage_uv(g)));
    time("nvml::power_watts", || {
        let _ = nvml::power_watts(0);
    });
    time("private::read_rail_voltages_uv", || drop(private::read_rail_voltages_uv(g)));
    time("nvapi::power_topology_status", || drop(nvapi::power_topology_status(g)));
    time("wrapper::utilization_domains", || drop(wrapper::utilization_domains(g)));
    time("nvapi::memory_info", || drop(nvapi::memory_info(g)));
    time("wrapper::coolers", || drop(wrapper::coolers(g)));
    time("nvapi::current_pstate", || drop(nvapi::current_pstate(g)));
    time("nvapi::performance_limit", || drop(nvapi::performance_limit(g)));
    time("supplemental::read (nothing reused)", || drop(supplemental::read(g, 0, Default::default())));
    time("nvml::default_power_limit_watts", || {
        let _ = nvml::default_power_limit_watts(0);
    });
    println!("-- whole operations");
    time("backend.read_temperature_only", || drop(b.read_temperature_only(0)));
    time("backend.read_telemetry", || drop(b.read_telemetry(0)));
    time("backend.measure_extra_clocks", || drop(b.measure_extra_clocks(0)));
    time("backend.read_tuning_state", || drop(b.read_tuning_state(0)));

    println!("-- power percentage: NVML watts / default limit against the topology query");
    let tdp = nvml::default_power_limit_watts(0);
    for _ in 0..5 {
        let w = nvml::power_watts(0);
        let topo =
            nvapi::power_topology_status(g).ok().and_then(|e| e.first().map(|e| wrapper::pcm_percent(e.usage_pcm)));
        println!("  {w:.1} W / {tdp:.0} W = {:.1}%   topology {:.1}%", w / tdp * 100.0, topo.unwrap_or(f64::NAN));
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}
