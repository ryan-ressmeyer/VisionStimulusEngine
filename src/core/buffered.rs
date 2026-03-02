//! Buffered flip types — `BufferedConfig`, `FlipEvent<T>`, and internal fence abstraction.

use crate::data::OverflowBehavior;
use crate::timing::FlipInfo;

/// Configuration for [`VSEContext::run_buffered`].
///
/// # Example
/// ```
/// use vision_stimulus_engine::prelude::*;
/// let cfg = BufferedConfig { depth: 1, overflow: OverflowBehavior::Block };
/// ```
#[derive(Debug, Clone)]
pub struct BufferedConfig {
    /// Number of frames to pipeline ahead of confirmed GPU scanout.
    ///
    /// - `1` (double-buffering): CPU is one frame ahead of confirmed GPU output.
    ///   Swapchain image count is set to 2.
    /// - `2` (triple-buffering): CPU is two frames ahead. Swapchain image count is 3.
    ///
    /// Higher values increase GPU utilization but add closed-loop reaction latency.
    /// For most experiments `depth = 1` is the right choice.
    pub depth: usize,

    /// What to do when the pending-confirmation queue is full.
    ///
    /// - [`OverflowBehavior::Block`]: stall the render loop until space is available.
    ///   No data loss. Default.
    /// - [`OverflowBehavior::DropWithWarning`]: discard the oldest unconfirmed frame
    ///   and emit `tracing::warn!`. Never stalls; risk of data loss if writer falls behind.
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

/// Events dispatched by [`VSEContext::run_buffered`].
///
/// The loop alternates between two phases each vblank:
///
/// 1. **`Presented`** (once the GPU confirms frame `N - depth` was scanned out)
/// 2. **`Render`** (build and submit frame `N`)
///
/// During startup (the first `depth` iterations), only `Render` fires — there are no
/// confirmed frames yet.
///
/// # Pattern matching
///
/// Because this enum is `#[non_exhaustive]`, always include a catch-all arm:
///
/// ```rust,ignore
/// match event {
///     FlipEvent::Render => { /* build frame */ }
///     FlipEvent::Presented { flip_info, payload } => { /* react + record */ }
///     _ => {}
/// }
/// ```
#[non_exhaustive]
pub enum FlipEvent<T> {
    /// Build and submit the next frame.
    ///
    /// Call [`RenderContext::flip_with_payload`] before returning from this arm.
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
