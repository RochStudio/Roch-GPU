//! Read-only probe: initialise IGCL and print what it reports. Never calls a write/set API.
//!
//! `cargo run -p roch-intel --example probe`

use roch_core::GpuBackend;
use roch_intel::IntelBackend;

fn main() {
    let mut backend = IntelBackend::new();
    println!("Backend: {}", backend.backend_name());
    match backend.initialize() {
        Err(e) => println!("initialize() failed (expected without an Intel Arc GPU): {e}"),
        Ok(()) => {
            let count = backend.devices().len();
            println!("initialize() succeeded: {count} device(s)");
            for gpu in 0..count {
                println!("--- device {gpu}: {:#?}", backend.devices()[gpu]);
                println!("graphics: {:#?}", backend.read_graphics_info(gpu));
                match backend.capabilities(gpu) {
                    Ok(c) => println!("capabilities: {c:#?}"),
                    Err(e) => println!("capabilities: error: {e}"),
                }
                match backend.read_tuning_state(gpu) {
                    Ok(s) => println!("tuning state: {s:#?}"),
                    Err(e) => println!("tuning state: error: {e}"),
                }
                match backend.read_vf_curve(gpu) {
                    Ok(c) => println!("V/F curve: {} point(s): {c:?}", c.len()),
                    Err(e) => println!("V/F curve: error: {e}"),
                }
                // Power and load need two samples.
                for sample in 0..2 {
                    match backend.read_telemetry(gpu) {
                        Ok(t) => println!("telemetry #{sample}: {t:#?}"),
                        Err(e) => println!("telemetry #{sample}: error: {e}"),
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
        }
    }
    // A second call must not panic either: it reports the earlier failure (or is a no-op).
    match backend.initialize() {
        Ok(()) => println!("second initialize(): ok"),
        Err(e) => println!("second initialize(): {e}"),
    }
}
