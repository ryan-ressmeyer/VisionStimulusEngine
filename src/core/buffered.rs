//! Buffered flip types — `BufferedConfig`, `FlipEvent<T>`, and internal fence abstraction.

use crate::data::OverflowBehavior;
use crate::timing::FlipInfo;

/// Configuration for [`crate::core::VSEContext::run_buffered`].
///
/// Controls the pipeline depth and overflow policy of the buffered flip loop.
/// [`Default`] provides `depth = 1` with blocking overflow — the right choice
/// for the vast majority of closed-loop experiments.
///
/// # Example
///
/// ```
/// use vision_stimulus_engine::prelude::*;
/// // Most experiments: one frame of pipelining, never drop data.
/// let cfg = BufferedConfig::default();
/// assert_eq!(cfg.depth, 1);
///
/// // High-throughput rendering where occasional data loss is acceptable:
/// let cfg2 = BufferedConfig {
///     depth: 2,
///     overflow: OverflowBehavior::DropWithWarning,
/// };
/// ```
#[derive(Debug, Clone)]
pub struct BufferedConfig {
    /// Number of frames to pipeline ahead of confirmed GPU scanout.
    ///
    /// | `depth` | Swapchain images | Latency at 60 Hz | Recommended for |
    /// |---------|-----------------|------------------|-----------------|
    /// | `1`     | 2               | ~16 ms           | Most experiments (default) |
    /// | `2`     | 3               | ~33 ms           | High GPU utilization |
    ///
    /// With `depth = 1`, the CPU is always one frame ahead of the last confirmed
    /// scanout. When `FlipEvent::Presented` fires for frame N, frame N+1 has already
    /// been submitted to the GPU. Closed-loop updates in `Presented` take effect
    /// from frame N+2 onward.
    ///
    /// Higher values increase GPU pipeline fill and can improve frame rate
    /// stability, but each additional level adds one frame (~16 ms at 60 Hz)
    /// of closed-loop reaction latency.
    pub depth: usize,

    /// What to do when the pending-confirmation queue is full.
    ///
    /// - [`OverflowBehavior::Block`]: stall the render loop until space is available.
    ///   No data loss. **Default.**
    /// - [`OverflowBehavior::DropWithWarning`]: discard the oldest unconfirmed frame
    ///   and emit `tracing::warn!`. Never stalls; risk of data loss if the writer
    ///   falls behind the render loop.
    pub overflow: OverflowBehavior,
}

impl Default for BufferedConfig {
    fn default() -> Self {
        Self {
            depth: 1,
            overflow: OverflowBehavior::Block,
        }
    }
}

/// Events dispatched by [`crate::core::VSEContext::run_buffered`].
///
/// Each vblank produces two events (after the warm-up period):
///
/// 1. **`Presented`** — the GPU has confirmed that frame `N - depth` was scanned out.
///    `flip_info.present_time` carries a hardware-verified timestamp. Call
///    `RenderContext::record_frame` here.
/// 2. **`Render`** — build and submit frame `N`. Call
///    `RenderContext::flip_with_payload` before returning.
///
/// During the first `depth` iterations the queue is warming up and only `Render`
/// fires — there are no confirmed frames yet.
///
/// # Two-phase frame timeline (depth = 1)
///
/// ```text
/// vblank N:   [Render N]   → submit non-blocking → GPU processing
/// vblank N+1: [Presented N] → record confirmed timing
///             [Render N+1] → submit non-blocking → GPU processing
/// vblank N+2: [Presented N+1] → record confirmed timing
///             [Render N+2] → ...
/// ```
///
/// # Pattern matching
///
/// Because this enum is `#[non_exhaustive]`, always include a catch-all arm:
///
/// ```rust,ignore
/// use vision_stimulus_engine::prelude::*;
///
/// #[derive(serde::Serialize)]
/// struct FrameData { contrast: f32 }
///
/// let mut contrast = 1.0f32;
///
/// context.run_buffered::<FrameData, _>(BufferedConfig::default(), move |event, vse| {
///     match event {
///         FlipEvent::Render => {
///             vse.clear()?;
///             // draw stimulus at current contrast …
///             vse.flip_with_payload(None, FrameData { contrast })?;
///         }
///         FlipEvent::Presented { flip_info, payload } => {
///             // Confirmed timing — safe to record
///             vse.record_frame(payload)?;
///             // Closed-loop: reduce contrast when a frame is missed
///             if flip_info.missed { contrast *= 0.9; }
///         }
///         _ => {}
///     }
///     Ok(())
/// })?;
/// # Ok::<(), vision_stimulus_engine::prelude::VSEError>(())
/// ```
#[non_exhaustive]
pub enum FlipEvent<T> {
    /// Build and submit the next frame.
    ///
    /// Call `RenderContext::flip_with_payload` before returning from this arm.
    /// Drawing calls (`clear`, `draw_rect`, etc.) work normally here.
    /// `record_frame()` is **not** valid in this arm — call it in `Presented`.
    Render,

    /// A frame has been confirmed by the GPU/driver.
    ///
    /// `flip_info.present_time` is a confirmed hardware scanout timestamp when
    /// `GoogleDisplayTiming` is active; otherwise it is derived from the fence signal.
    ///
    /// `payload` is the value passed to `flip_with_payload()` when this frame was rendered.
    ///
    /// Call `vse.record_frame(payload)?` here to record with confirmed timing.
    /// Closed-loop stimulus adjustments based on `flip_info.missed` belong here.
    Presented {
        /// Confirmed timing for this frame.
        flip_info: FlipInfo,
        /// The per-frame data payload from the matching `Render` invocation.
        payload: T,
    },
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

/// A pending frame in the confirmation queue (local to `run_buffered()`).
pub(crate) struct PendingFrame<T> {
    pub frame_number: u64,
    pub payload: T,
    /// Best available timing at submit time. Replaced by confirmed timing in `Presented`.
    pub estimated_flip: FlipInfo,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffered_config_default() {
        let cfg = BufferedConfig::default();
        assert_eq!(cfg.depth, 1);
    }

    #[test]
    fn flip_event_render_matches() {
        let event: FlipEvent<u32> = FlipEvent::Render;
        match event {
            FlipEvent::Render => {}
            FlipEvent::Presented { .. } => {}
            // catch-all required by #[non_exhaustive]
            _ => {}
        }
    }

    #[test]
    fn flip_event_presented_matches() {
        use crate::timing::{FlipInfo, Timestamp, TimingSource};
        let flip = FlipInfo {
            frame_number: 0,
            timing_source: TimingSource::CpuEstimate,
            submit_time: Timestamp::from_micros(0),
            present_time: Timestamp::from_micros(16_667),
            missed: false,
            missed_count: 0,
            skipped: false,
        };
        let event: FlipEvent<u32> = FlipEvent::Presented {
            flip_info: flip,
            payload: 42,
        };
        match event {
            FlipEvent::Presented { payload, .. } => assert_eq!(payload, 42),
            _ => panic!("expected Presented"),
        }
    }
}
