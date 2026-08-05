//! Structured buffered presentation types and internal fence abstraction.

use crate::timing::FlipInfo;

/// Configuration for [`crate::core::VSEContext::run_buffered`].
///
/// Controls the pipeline depth of the buffered flip loop.
/// [`Default`] provides `depth = 1`, the right choice for the vast majority of
/// closed-loop experiments.
///
/// # Example
///
/// ```
/// use vision_stimulus_engine::prelude::*;
/// // Most experiments: one frame of pipelining, never drop data.
/// let cfg = BufferedConfig::default();
/// assert_eq!(cfg.depth, 1);
///
/// // More pipelining for a GPU-bound workload:
/// let cfg2 = BufferedConfig { depth: 2 };
/// ```
#[derive(Debug, Clone)]
pub struct BufferedConfig {
    /// Number of frames to pipeline ahead of the most recently retired submission.
    ///
    /// | `depth` | Swapchain images | Latency at 60 Hz | Recommended for |
    /// |---------|-----------------|------------------|-----------------|
    /// | `1`     | 2               | ~16 ms           | Most experiments (default) |
    /// | `2`     | 3               | ~33 ms           | High GPU utilization |
    ///
    /// With `depth = 1`, the CPU is one frame ahead of the last confirmation callback.
    /// When confirmation arrives for frame N, frame N+1 has already
    /// been submitted to the GPU. Closed-loop updates in the confirmation callback
    /// take effect from frame N+2 onward.
    ///
    /// Higher values increase GPU pipeline fill and can improve frame rate
    /// stability, but each additional level adds one frame (~16 ms at 60 Hz)
    /// of closed-loop reaction latency.
    pub depth: usize,
}

impl Default for BufferedConfig {
    fn default() -> Self {
        Self { depth: 1 }
    }
}

/// One frame returned by a buffered render callback.
///
/// VSE submits the frame after the callback returns, guaranteeing one submission
/// for every successful render invocation.
#[derive(Debug)]
pub struct BufferedFrame<T> {
    /// Optional scanout-clock target for this presentation.
    pub target_time: Option<crate::timing::Timestamp>,
    /// Per-frame data returned with the matching confirmation.
    pub payload: T,
}

impl<T> BufferedFrame<T> {
    /// Present at the next available vblank.
    pub fn new(payload: T) -> Self {
        Self {
            target_time: None,
            payload,
        }
    }

    /// Present at a specific scanout-clock target.
    pub fn at(target_time: crate::timing::Timestamp, payload: T) -> Self {
        Self {
            target_time: Some(target_time),
            payload,
        }
    }
}

/// One retired buffered submission paired with its payload and timing receipt.
///
/// “Confirmed” describes FIFO payload/submission correlation. Whether the receipt contains
/// scanout evidence depends on `flip_info.timing_source`.
#[derive(Debug)]
pub struct ConfirmedFrame<T> {
    /// Best available timing and presentation metadata for the retired submission.
    pub flip_info: FlipInfo,
    /// Payload returned by the render callback for this frame.
    pub payload: T,
}

/// Type-erased in-flight GPU future.
///
/// Keeps the `FenceSignalFuture` alive (dropping it would block) while allowing
/// non-blocking polling and deferred blocking drain on shutdown.
pub(crate) trait InFlightFuture {
    /// Returns `true` if the GPU fence has signaled (non-blocking poll).
    fn is_complete(&self) -> bool;

    /// Block until the GPU fence signals. Used during shutdown drain.
    fn wait_blocking(&self);
}

/// A submitted frame awaiting confirmation in the buffered FIFO.
pub(crate) struct PendingFrame<T> {
    pub payload: T,
    /// Submission metadata and completion owned by this exact payload.
    pub submission: crate::core::flip::Submission,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CompleteFuture;

    impl InFlightFuture for CompleteFuture {
        fn is_complete(&self) -> bool {
            true
        }

        fn wait_blocking(&self) {}
    }

    #[test]
    fn pending_frame_owns_payload_timing_and_completion() {
        use crate::timing::{Timestamp, TimingSource};

        let pending = PendingFrame {
            payload: 42_u32,
            submission: crate::core::flip::Submission {
                estimated_flip: FlipInfo {
                    frame_number: 3,
                    timing_source: TimingSource::CpuEstimate,
                    submit_time: Timestamp::from_micros(10),
                    present_time: Timestamp::from_micros(20),
                    present_id: 0,
                    target_time: None,
                    on_target: true,
                    missed: false,
                    missed_count: 0,
                    skipped: false,
                },
                completion: Box::new(CompleteFuture),
                backend: crate::core::flip::SubmissionBackend::Vulkano,
            },
        };

        assert_eq!(pending.payload, 42);
        assert_eq!(pending.submission.estimated_flip.frame_number, 3);
        assert!(pending.submission.completion.is_complete());
    }

    #[test]
    fn buffered_config_default() {
        let cfg = BufferedConfig::default();
        assert_eq!(cfg.depth, 1);
    }

    #[test]
    fn buffered_frame_constructors_set_target_and_payload() {
        use crate::timing::Timestamp;

        let immediate = BufferedFrame::new(42_u32);
        assert_eq!(immediate.target_time, None);
        assert_eq!(immediate.payload, 42);

        let target = Timestamp::from_micros(50_000);
        let scheduled = BufferedFrame::at(target, "frame");
        assert_eq!(scheduled.target_time, Some(target));
        assert_eq!(scheduled.payload, "frame");
    }
}
