//! FlipInfo - timing receipt returned by every flip() call.

use super::clock::Timestamp;
use super::timing_source::TimingSource;

/// Resolve a displayed frame's timestamp provenance independently of the selected backend.
///
/// The EXT backend can still yield a host-clock estimate when no scanout observation is
/// available for that frame. In that case the receipt must identify the value as a CPU estimate.
pub(crate) fn resolved_present_timing_source(
    backend: TimingSource,
    has_scanout_time: bool,
) -> TimingSource {
    match (backend, has_scanout_time) {
        (TimingSource::ExtPresentTiming, true) => TimingSource::ExtPresentTiming,
        (TimingSource::ExtPresentTiming, false) => TimingSource::CpuEstimate,
        (source, _) => source,
    }
}

/// Compute an interval only when both timestamps have the same provenance and clock domain.
pub(crate) fn comparable_frame_duration(
    previous: Option<(Timestamp, TimingSource)>,
    current: Timestamp,
    current_source: TimingSource,
) -> Option<std::time::Duration> {
    let (previous, previous_source) = previous?;
    (previous_source == current_source).then(|| current.duration_since(previous))
}

/// Timing receipt for one frame request.
///
/// Returned by `RenderContext::flip()` and carried by buffered confirmations. The receipt records
/// the strongest timestamp VSE obtained; inspect [`timing_source`](Self::timing_source) before
/// interpreting its clock domain or comparing it with another timestamp. See
/// `docs/timing-conformance.md`.
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
/// The meaning — and clock domain — of `present_time` depends on `timing_source`:
/// - `CpuEstimate`: host `CLOCK_MONOTONIC` reading after the fence signals (µs since session
///   start).
/// - `ExtPresentTiming`: scanout-domain evidence, rebased to the session's scanout `t=0`. This is
///   either per-present `IMAGE_FIRST_PIXEL_OUT` feedback or, on the synchronous path, a calibrated
///   present-stage clock sample after a successful present wait.
/// - `Offscreen`: a synthesized nominal frame time; no presentation occurred.
///
/// If a session selected the EXT backend but one frame lacks usable scanout evidence, that frame
/// reports `CpuEstimate`. `RenderContext::timing_source()` continues to report the selected backend.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FlipInfo {
    /// Monotonically increasing frame number (0-indexed from first flip)
    pub frame_number: u64,

    /// Source and clock domain of this frame's `present_time`.
    pub timing_source: TimingSource,

    /// Host-clock timestamp just before command buffer submission.
    pub submit_time: Timestamp,

    /// Present timestamp (meaning depends on timing_source)
    pub present_time: Timestamp,

    /// The `VK_KHR_present_id2` id assigned to this present, for correlation with raw
    /// driver timing logs and external systems. Zero for the CPU-estimate path and for
    /// skipped frames (no present was submitted).
    pub present_id: u64,

    /// The requested target present time, if any. `None` requests the next presentation
    /// opportunity. A populated target records intent; it does not prove driver enforcement.
    pub target_time: Option<Timestamp>,

    /// Whether comparable scanout evidence was at or after `target_time`.
    ///
    /// `true` is also used by convention for unscheduled frames and frames without comparable
    /// scanout evidence. It is evidentiary only when a target exists and `timing_source` is
    /// `ExtPresentTiming`.
    pub on_target: bool,

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
            present_id: 0,
            target_time: None,
            on_target: true,
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
    fn ext_backend_fallback_receipt_reports_cpu_estimate() {
        assert_eq!(
            resolved_present_timing_source(TimingSource::ExtPresentTiming, false),
            TimingSource::CpuEstimate
        );
        assert_eq!(
            resolved_present_timing_source(TimingSource::ExtPresentTiming, true),
            TimingSource::ExtPresentTiming
        );
    }

    #[test]
    fn frame_duration_requires_matching_timestamp_sources() {
        let previous = Some((
            Timestamp::from_micros(10_000),
            TimingSource::ExtPresentTiming,
        ));
        assert_eq!(
            comparable_frame_duration(
                previous,
                Timestamp::from_micros(20_000),
                TimingSource::CpuEstimate,
            ),
            None
        );
        assert_eq!(
            comparable_frame_duration(
                previous,
                Timestamp::from_micros(20_000),
                TimingSource::ExtPresentTiming,
            ),
            Some(std::time::Duration::from_micros(10_000))
        );
    }

    #[test]
    fn test_flip_info_clone() {
        let info = FlipInfo {
            frame_number: 10,
            timing_source: TimingSource::CpuEstimate,
            submit_time: Timestamp::from_micros(1000),
            present_time: Timestamp::from_micros(2000),
            present_id: 11,
            target_time: None,
            on_target: true,
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
