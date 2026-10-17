//! Intel Arc backend over the public Graphics Control Library (IGCL, `ControlLib.dll`).
//!
//! Units differ from the other vendors: memory is an absolute speed in Mbps, the temperature
//! limit is a percentage of thermal margin, and voltage is a percentage of headroom. Fan curves run
//! in hardware, the V/F curve is edited per point, and the core clock range uses Intel's -1/-1
//! factory sentinel for reset.

mod backend;
mod igcl;

pub use backend::{
    EDITOR_CORE_STEP_MHZ, EDITOR_MEMORY_STEP_MBPS, EDITOR_POWER_STEP_PERCENT, EDITOR_TEMP_STEP_PERCENT,
    EDITOR_VOLTAGE_STEP_PERCENT, IntelBackend,
};

#[cfg(test)]
mod tests;
