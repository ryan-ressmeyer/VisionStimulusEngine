//! FlipInfo - timing receipt returned by every flip() call.

use std::time::Duration;

use super::clock::Timestamp;

/// Information about a single frame flip (presentation).
///
/// Returned by `RenderContext::flip()`. Contains timestamps that let
/// you verify timing precision and correlate with external recordings.
///
/// # Timing Model
///
/// ```text
/// CPU timeline:
///   [submit_time]----[present_complete_time]
///         |                    |
///         v                    v
///   Command buffer       Fence signals
///   submitted to GPU     (GPU done, frame
///                         queued for display)
/// ```
///
/// `present_complete_time` is the closest CPU-side approximation of
/// when the frame reaches the display.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FlipInfo {
    /// Monotonically increasing frame number (0-indexed from first flip)
    pub frame_number: u64,

    /// Timestamp just before command buffer submission
    pub submit_time: Timestamp,

    /// Timestamp after fence signal (GPU finished, frame queued for display)
    pub present_complete_time: Timestamp,

    /// Duration of this frame (time since previous flip's present_complete_time).
    /// None for the very first frame.
    #[serde(skip)]
    pub frame_duration: Option<Duration>,

    /// Expected frame duration based on display refresh rate.
    #[serde(skip)]
    pub expected_frame_duration: Duration,

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
            submit_time: Timestamp::from_micros(0),
            present_complete_time: Timestamp::from_micros(0),
            frame_duration: None,
            expected_frame_duration: Duration::from_micros(16_667),
            missed: false,
            missed_count: 0,
            skipped: true,
        }
    }

    /// Get frame duration in microseconds, if available.
    pub fn frame_duration_us(&self) -> Option<u64> {
        self.frame_duration.map(|d| d.as_micros() as u64)
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
        assert!(info.frame_duration.is_none());
    }

    #[test]
    fn test_flip_info_clone() {
        let info = FlipInfo {
            frame_number: 10,
            submit_time: Timestamp::from_micros(1000),
            present_complete_time: Timestamp::from_micros(2000),
            frame_duration: Some(Duration::from_micros(16_667)),
            expected_frame_duration: Duration::from_micros(16_667),
            missed: false,
            missed_count: 0,
            skipped: false,
        };
        let cloned = info.clone();
        assert_eq!(cloned.frame_number, 10);
        assert_eq!(cloned.submit_time, info.submit_time);
    }

    #[test]
    fn test_frame_duration_us() {
        let info = FlipInfo {
            frame_number: 0,
            submit_time: Timestamp::from_micros(0),
            present_complete_time: Timestamp::from_micros(16_667),
            frame_duration: Some(Duration::from_micros(16_667)),
            expected_frame_duration: Duration::from_micros(16_667),
            missed: false,
            missed_count: 0,
            skipped: false,
        };
        assert_eq!(info.frame_duration_us(), Some(16_667));

        let skipped = FlipInfo::skipped(0);
        assert_eq!(skipped.frame_duration_us(), None);
    }
}
