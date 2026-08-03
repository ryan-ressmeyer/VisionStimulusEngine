//! Timing source classification for flip timing data.

/// Identifies which Vulkan extension (or fallback) provided the timing data.
///
/// This is written into every FlipInfo and CSV log so researchers always
/// know the precision tier of their timing data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum TimingSource {
    /// VK_EXT_present_timing — hardware scanout timestamps + scheduled presents.
    ExtPresentTiming,
    /// CPU estimation via std::time::Instant around fence wait.
    CpuEstimate,
    /// No display at all — headless (offscreen) rendering. Frame times are
    /// synthesized from a nominal refresh interval, never measured. Data
    /// recorded under this source describes regenerated stimuli, not a
    /// presented session.
    Offscreen,
}

impl std::fmt::Display for TimingSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TimingSource::ExtPresentTiming => write!(f, "ExtPresentTiming"),
            TimingSource::CpuEstimate => write!(f, "CpuEstimate"),
            TimingSource::Offscreen => write!(f, "Offscreen"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timing_source_display() {
        assert_eq!(TimingSource::CpuEstimate.to_string(), "CpuEstimate");
        assert_eq!(
            TimingSource::ExtPresentTiming.to_string(),
            "ExtPresentTiming"
        );
    }

    #[test]
    fn test_timing_source_equality() {
        assert_eq!(TimingSource::CpuEstimate, TimingSource::CpuEstimate);
        assert_ne!(TimingSource::CpuEstimate, TimingSource::ExtPresentTiming);
    }
}
