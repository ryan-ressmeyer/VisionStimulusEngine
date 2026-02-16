//! Timing and synchronization infrastructure
//!
//! This module provides high-resolution timing measurement,
//! per-frame flip logging, and timing statistics for validating
//! stimulus presentation precision.

mod clock;
mod flip_info;
mod flip_logger;
mod stats;
mod timing_source;

pub use clock::{Clock, Timestamp};
pub use flip_info::FlipInfo;
pub use flip_logger::FlipLogger;
pub use stats::TimingStats;
pub use timing_source::TimingSource;
