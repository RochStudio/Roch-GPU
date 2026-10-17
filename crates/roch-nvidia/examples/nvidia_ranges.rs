//! Read-only: print every tuning range the NVIDIA driver itself reports, before any narrowing.
//! `cargo run -p roch-nvidia --example nvidia_ranges`

use roch_nvidia::{nvapi, nvml, private};

fn main() {
    if let Err(e) = nvapi::initialize() {
        eprintln!("NVAPI init failed: {e:?}");
        return;
    }
    let gpus = nvapi::enum_physical_gpus().unwrap_or_default();
    for (i, &g) in gpus.iter().enumerate() {
        println!("== GPU {i}");
        match nvapi::pstates20(g) {
            Ok(ps) => {
                println!("pstates20 editable={}", ps.is_editable);
                for (id, clocks) in &ps.clocks {
                    for c in clocks {
                        let r = c.freq_delta_khz;
                        println!(
                            "  P{id} domain {} editable={} delta {}..{} kHz (now {})",
                            c.domain_id,
                            c.editable(),
                            r.min,
                            r.max,
                            r.value
                        );
                    }
                }
            }
            Err(e) => println!("pstates20: {e:?}"),
        }
        match nvapi::power_policies_info(g) {
            Ok(v) => v.iter().for_each(|p| println!("power policy: {p:?}")),
            Err(e) => println!("power policies: {e:?}"),
        }
        match nvapi::thermal_policies_info(g) {
            Ok(v) if v.is_empty() => println!("thermal policies: none reported"),
            Ok(v) => v.iter().for_each(|t| println!("thermal policy: {t:?}")),
            Err(e) => println!("thermal policies: {e:?}"),
        }
        println!("voltage boost now: {:?}", nvapi::core_voltage_boost_percent(g));
        let xi = private::read_xbar_info(g);
        println!("xbar info: {xi:?}");
        for d in private::read_domain_entries(g) {
            println!("domain entry: {d:?}");
        }
        for r in private::read_volt_rails(g) {
            println!("volt rail: {r:?}");
        }
        for r in private::read_ocp_ranges(g) {
            println!("ocp range: {r:?}");
        }
        for (name, t) in [
            ("shutdown", nvml::THRESHOLD_SHUTDOWN),
            ("slowdown", nvml::THRESHOLD_SLOWDOWN),
            ("mem max", nvml::THRESHOLD_MEM_MAX),
            ("gpu max", nvml::THRESHOLD_GPU_MAX),
            ("acoustic min", nvml::THRESHOLD_ACOUSTIC_MIN),
            ("acoustic curr (target)", nvml::THRESHOLD_ACOUSTIC_CURR),
            ("acoustic max", nvml::THRESHOLD_ACOUSTIC_MAX),
            ("gps curr", nvml::THRESHOLD_GPS_CURR),
        ] {
            println!("nvml temperature threshold {name}: {:?}", nvml::temperature_threshold_c(i as i32, t));
        }
        println!(
            "nvml graphics clock lock: {}..{} MHz",
            nvml::min_graphics_clock_mhz(i as i32),
            nvml::max_graphics_clock_mhz(i as i32)
        );
    }
}
