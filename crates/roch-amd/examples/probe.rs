//! Read-only probe: initialise the AMD backend and print what it found. Never writes.
//!
//! `cargo run -p roch-amd --example probe`

use roch_amd::AmdBackend;
use roch_core::GpuBackend;

fn main() {
    let mut backend = AmdBackend::new();
    println!("backend: {}", backend.backend_name());
    match backend.initialize() {
        Ok(()) => {
            println!("initialize: Ok, {} device(s)", backend.devices().len());
            for i in 0..backend.devices().len() {
                println!("{:?}", backend.devices()[i]);
                match backend.capabilities(i) {
                    Ok(c) => println!("capabilities: {c:?}"),
                    Err(e) => println!("capabilities: Err({e})"),
                }
                match backend.read_telemetry(i) {
                    Ok(t) => println!("telemetry: {t:?}"),
                    Err(e) => println!("telemetry: Err({e})"),
                }
                println!("{}", backend.diagnostics(i));
            }
        }
        Err(e) => println!("initialize: Err({e})"),
    }
    // Second call after a failure must also fail cleanly (and not leak or crash).
    println!("initialize again: {:?}", backend.initialize().map(|_| backend.devices().len()));
}
