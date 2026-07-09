//! Timing and synchronization infrastructure
//!
//! This module provides high-resolution timing measurement and
//! per-frame flip information for stimulus presentation timing.

mod bridge;
mod clock;
mod flip_info;
pub(crate) mod provider;
mod timing_source;

pub use bridge::HostClockBridge;
pub use clock::{Clock, ScanoutClock, ScanoutTimestamp, Timestamp};
pub use flip_info::FlipInfo;
pub use provider::{
    CalibrationSample, CpuTimingProvider, ExtPresentTimingProvider, TimingProvider,
};
pub use timing_source::TimingSource;
