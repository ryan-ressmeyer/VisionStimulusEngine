//! FlipInfo - timing receipt returned by every flip() call.

use super::clock::Timestamp;
use super::timing_source::TimingSource;

/// Information about a single frame flip (presentation).
///
/// Returned by `RenderContext::flip()`. Contains timestamps that let
/// you verify timing precision and correlate with external recordings.
///
/// # Timing Model
///
/// ```text
/// CPU timeline:
///   [submit_time]----[present_time]
///         |                |
///         v                v
///   Command buffer    Present timestamp
///   submitted to GPU  (source depends on TimingSource)
/// ```
///
/// The meaning of `present_time` depends on `timing_source`:
/// - `CpuEstimate`: CPU clock reading after fence signal
/// - `GoogleDisplayTiming`: driver-reported actual present time
/// - `ExtPresentTiming`: hardware scanout timestamp (future)
#[derive(Debug, Clone, serde::Serialize)]
pub struct FlipInfo {
    /// Monotonically increasing frame number (0-indexed from first flip)
    pub frame_number: u64,

    /// Which timing backend provided this data
    pub timing_source: TimingSource,

    /// Timestamp just before command buffer submission
    pub submit_time: Timestamp,

    /// Present timestamp (meaning depends on timing_source)
    pub present_time: Timestamp,

    /// Whether this frame was likely missed (frame_duration > 1.5 * expected)
    pub missed: bool,

    /// Number of frames missed (0 = on time, 1 = one frame late, etc.)
    pub missed_count: u32,

    /// Whether this frame was skipped (minimized window, swapchain recreation)
    pub skipped: bool,
}

impl FlipInfo {
    /// Create a FlipInfo for a skipped frame (minimized or swapchain recreation).
    ///
    /// Skipped frames are not recorded by the FlipLogger.
    pub fn skipped(frame_number: u64) -> Self {
        Self {
            frame_number,
            timing_source: TimingSource::CpuEstimate,
            submit_time: Timestamp::from_micros(0),
            present_time: Timestamp::from_micros(0),
            missed: false,
            missed_count: 0,
            skipped: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flip_info_skipped() {
        let info = FlipInfo::skipped(42);
        assert_eq!(info.frame_number, 42);
        assert!(info.skipped);
        assert!(!info.missed);
        assert_eq!(info.missed_count, 0);
        assert_eq!(info.timing_source, TimingSource::CpuEstimate);
    }

    #[test]
    fn test_flip_info_clone() {
        let info = FlipInfo {
            frame_number: 10,
            timing_source: TimingSource::CpuEstimate,
            submit_time: Timestamp::from_micros(1000),
            present_time: Timestamp::from_micros(2000),
            missed: false,
            missed_count: 0,
            skipped: false,
        };
        let cloned = info.clone();
        assert_eq!(cloned.frame_number, 10);
        assert_eq!(cloned.submit_time, info.submit_time);
        assert_eq!(cloned.timing_source, TimingSource::CpuEstimate);
    }
}
