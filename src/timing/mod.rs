//! Timing and synchronization infrastructure
//!
//! This module provides high-resolution timing measurement and
//! per-frame flip information for stimulus presentation timing.

mod clock;
mod flip_info;
pub(crate) mod provider;
mod timing_source;

pub use clock::{Clock, Timestamp};
pub use flip_info::FlipInfo;
pub use provider::{CpuTimingProvider, GoogleDisplayTimingProvider, TimingProvider};
pub use timing_source::TimingSource;
