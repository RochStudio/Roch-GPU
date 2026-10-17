//! Data types shared by every backend and front end.

pub mod clock_step;
pub mod fan_curve;
pub mod gpu;
pub mod graphics_info;
pub mod ocp_bounds;
pub mod profile;
pub mod sensor_stat;
pub mod voltage_plan;
pub mod xoc_lever;

pub use fan_curve::{FanCurve, FanPoint};
pub use gpu::*;
pub use graphics_info::{GpuGraphicsInfo, GpuGraphicsRow, GpuIdentity};
pub use profile::{FanMode, TuningProfile};
pub use sensor_stat::SensorStat;
pub use xoc_lever::XocLever;
