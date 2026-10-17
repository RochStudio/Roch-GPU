//! Read-only smoke test of the NVIDIA backend against the real card.
//!
//! Calls ONLY getters: initialize, devices, capabilities, read_graphics_info, telemetry and
//! temperature-only samples, tuning state, voltage lock readback and mechanism, the V/F curve,
//! measure_extra_clocks, read_clock_domains and the diagnostics dump. No setter, reset or fan call
//! appears here (each of those lives in backend/set.rs or backend/curve.rs's write paths, none of
//! which a getter reaches).
//!
//! `cargo run -p roch-nvidia --example probe [-- --diag-file out.txt]`

use roch_core::GpuBackend;
use roch_nvidia::NvidiaBackend;

fn main() {
    let diag_file = std::env::args().skip_while(|a| a != "--diag-file").nth(1);
    let mut b = NvidiaBackend::new();
    if let Err(e) = b.initialize() {
        eprintln!("initialize failed: {e}");
        std::process::exit(1);
    }
    println!("backend: {}", b.backend_name());
    for d in b.devices() {
        println!("device: {d:?}");
    }
    let gpu = 0;

    println!("\n== capabilities");
    match b.capabilities(gpu) {
        Ok(c) => println!("{c:#?}"),
        Err(e) => println!("ERROR {e}"),
    }

    println!("\n== graphics info");
    for row in b.read_graphics_info(gpu).rows() {
        println!("  {:<20} {}", row.name, row.value);
    }

    println!("\n== telemetry");
    for _ in 0..3 {
        match b.read_telemetry(gpu) {
            Ok(t) => println!("{t:#?}"),
            Err(e) => println!("ERROR {e}"),
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    for _ in 0..3 {
        match b.read_temperature_only(gpu) {
            Ok(t) => println!("temperature only: {} °C", t.temperature_c),
            Err(e) => println!("ERROR {e}"),
        }
    }

    println!("\n== tuning state");
    match b.read_tuning_state(gpu) {
        Ok(s) => println!("{s:#?}"),
        Err(e) => println!("ERROR {e}"),
    }
    println!("voltage lock: {} mV, mechanism: {}", b.read_voltage_lock_mv(gpu), b.voltage_lock_mechanism());

    println!("\n== V/F curve");
    match b.read_vf_curve(gpu) {
        Ok(c) => {
            println!("{} points", c.len());
            for p in c.iter().step_by(8).chain(c.last()) {
                println!(
                    "  [{:>3}] {:>5} mV  stock {:>5} MHz  live {:>5} MHz",
                    p.index, p.voltage_mv, p.stock_mhz, p.live_mhz
                );
            }
        }
        Err(e) => println!("ERROR {e}"),
    }

    println!("\n== extra clocks");
    for (k, v) in b.measure_extra_clocks(gpu) {
        println!("  {k:<14} {v:.1} MHz");
    }

    println!("\n== clock domains");
    println!("slot  type  range              measured   offset");
    for d in b.read_clock_domains(gpu) {
        let range =
            if d.has_range { format!("{:>6}..{:<6} MHz", d.min_mhz, d.max_mhz) } else { "no range          ".into() };
        println!("{:>4}  {:>4}  {range}  {:>6} MHz  {:>6} MHz", d.slot, d.type_, d.measured_mhz, d.offset_mhz);
    }

    println!("\n== diagnostics");
    let dump = b.diagnostics(gpu);
    match diag_file {
        Some(path) => {
            std::fs::write(&path, &dump).expect("write diagnostics");
            println!("({} bytes written to {path})", dump.len());
        }
        None => println!("{dump}"),
    }
}
