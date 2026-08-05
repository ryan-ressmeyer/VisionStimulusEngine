//! Timing source classification for flip timing data.

/// Identifies the source and clock domain of one frame's `present_time`.
///
/// This is written into every `FlipInfo` and frame record. It is per-frame provenance, not the
/// selected session backend; `RenderContext::timing_source()` reports backend selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum TimingSource {
    /// Scanout-domain evidence obtained through the EXT timing path.
    ExtPresentTiming,
    /// Host-clock observation after GPU completion or an unavailable scanout observation.
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
