//! Host machine information capture
//!
//! Provides comprehensive snapshots of the host system state
//! for reproducibility and audit trails in vision science experiments.

pub(crate) mod capture;
pub(crate) mod edid;
mod host_info;

pub use host_info::{
    BuildInfo, CpuInfo, DisplayInfo, EdidInfo, GpuInfo, HostInfo, MemoryInfo, OsInfo,
    PipelineConfig, RuntimeEnv, SwapchainInfo,
};
