//! VSEContext - Top-level VisionStimulusEngine environment
//!
//! This module provides the main entry point for VSE, managing all Vulkan
//! resources and providing a clean API for rendering operations.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tracing::{debug, info, warn};
use winit::{
    dpi::{LogicalPosition, PhysicalSize},
    event::{ElementState, Event, MouseScrollDelta, WindowEvent},
    event_loop::{ControlFlow, EventLoop, EventLoopWindowTarget},
    keyboard::PhysicalKey,
    window::{Fullscreen, Window, WindowBuilder},
};

use std::path::Path;

use super::buffered::{BufferedConfig, FlipEvent};
use super::input::{
    AcquisitionMethod, DisplayBackend, InputEvent, InputState, KeyCode, MonitorInfo,
    MonitorSelection, MouseButton, VideoModeInfo, WindowMode,
};
use super::present_engine::{PresentEngine, ScheduledTarget};
use super::present_timing_ext::ScanoutFeedback;
use super::{
    device::{DeviceError, DeviceSelector, GPUPreference},
    frame::{FrameBuilder, FrameError},
    swapchain::{PresentMode, SwapchainConfig, SwapchainError, SwapchainManager},
};
use vulkano::sync::GpuFuture;

use crate::data::messages::FrameMessage;
use crate::data::ExperimentSession;
use crate::drawing::primitives::{default_circle_segments, DrawCommand};
use crate::drawing::renderer::{Renderer, RendererError};
use crate::drawing::{Color, GaborParams, GratingParams, NoiseParams, TextureHandle};
use crate::timing::{
    Clock, CpuTimingProvider, ExtPresentTimingProvider, FlipInfo, HostClockBridge, ScanoutClock,
    ScanoutTimestamp, Timestamp, TimingProvider, TimingSource,
};

/// Errors that can occur in VSEContext
#[derive(Error, Debug)]
pub enum VSEError {
    /// Device-related error
    #[error("Device error: {0}")]
    Device(#[from] DeviceError),

    /// Swapchain-related error
    #[error("Swapchain error: {0}")]
    Swapchain(#[from] SwapchainError),

    /// Frame-related error
    #[error("Frame error: {0}")]
    Frame(#[from] FrameError),

    /// Renderer error
    #[error("Renderer error: {0}")]
    Renderer(#[from] RendererError),

    /// Window creation error
    #[error("Window error: {0}")]
    Window(String),

    /// Event loop error
    #[error("Event loop error: {0}")]
    EventLoop(String),

    /// All acquisition methods were tried and failed.
    /// The string contains a formatted diagnostic listing each failure reason.
    #[error("Direct display mode unavailable: {0}")]
    DirectDisplayUnavailable(String),

    /// Acquisition succeeded but a subsequent setup step failed.
    #[error("Direct display setup failed (acquired via {method:?}): {reason}")]
    DirectDisplaySetupFailed {
        method: AcquisitionMethod,
        reason: String,
    },

    /// record_frame() called before flip() in the current frame.
    #[error("record_frame() called before flip() — call flip() first")]
    NoFlipPending,

    /// External-renderer frame source error (see `core::external_frame`).
    #[error("External frame error: {0}")]
    ExternalFrame(#[from] crate::core::external_frame::ExternalFrameError),

    /// No ExperimentSession attached. Call VSEContextBuilder::with_session() to enable recording.
    #[error("no ExperimentSession attached — call .with_session() on the builder")]
    NoSession,

    /// Data recording error.
    #[error("Data recording error: {0}")]
    DataRecording(String),

    /// `record_frame()` called in `FlipEvent::Render` arm of `run_buffered()`.
    /// Move the `record_frame()` call to the `FlipEvent::Presented` arm.
    #[error(
        "record_frame() is only valid in the FlipEvent::Presented arm — \
             move it out of the Render arm"
    )]
    NoConfirmedFlip,

    /// `flip_with_payload()` called outside of `run_buffered()`.
    /// Use `flip()` in the standard `run()` loop instead.
    #[error("flip_with_payload() requires run_buffered() — use flip() inside run()")]
    NotInBufferedMode,

    /// `flip()` called inside `run_buffered()`.
    /// Replace with `flip_with_payload()` in the Render arm.
    #[error("flip() is not supported in run_buffered() — use flip_with_payload() instead")]
    NotSupportedInBufferedMode,
}

/// Configuration for VSEContext
#[derive(Debug, Clone)]
pub struct VSEConfig {
    /// Window width in pixels
    pub window_width: u32,
    /// Window height in pixels
    pub window_height: u32,
    /// Window title
    pub window_title: String,
    /// GPU selection preference
    pub gpu_preference: GPUPreference,
    /// Presentation mode
    pub present_mode: PresentMode,
    /// Clear color (RGBA, 0.0-1.0)
    pub clear_color: [f32; 4],
    /// Expected refresh rate in Hz (used for missed frame detection).
    /// If None, auto-detected from first 10 frames.
    pub expected_refresh_rate: Option<f64>,
    /// Window display mode (windowed, borderless fullscreen, exclusive fullscreen).
    pub window_mode: WindowMode,
    /// Which monitor to use for fullscreen modes.
    pub monitor_selection: MonitorSelection,
    /// Whether the cursor is visible. None means auto (hidden in fullscreen, visible in windowed).
    pub cursor_visible: Option<bool>,
    /// Override video mode for DirectDisplay (width, height, refresh_hz).
    /// Default: highest refresh rate at native resolution.
    pub direct_display_video_mode: Option<(u32, u32, f64)>,
    /// Override acquisition probe order for DirectDisplay mode.
    /// Default: [NoCompositor, DrmAcquire, XlibAcquire].
    pub direct_display_acquisition_order: Option<Vec<AcquisitionMethod>>,
    /// Enable the opt-in host↔scanout clock bridge (see [`VSEContextBuilder::with_host_clock_bridge`]).
    /// Off by default: display timing lives in the scanout clock and needs no bridge.
    pub host_clock_bridge: bool,
}

impl Default for VSEConfig {
    fn default() -> Self {
        Self {
            window_width: 800,
            window_height: 600,
            window_title: "VisionStimulusEngine".to_string(),
            gpu_preference: GPUPreference::Discrete,
            present_mode: PresentMode::Fifo,
            clear_color: [0.0, 0.0, 0.0, 1.0], // Black
            expected_refresh_rate: None,
            window_mode: WindowMode::default(),
            monitor_selection: MonitorSelection::default(),
            cursor_visible: None,
            direct_display_video_mode: None,
            direct_display_acquisition_order: None,
            host_clock_bridge: false,
        }
    }
}

/// Builder for VSEContext with sensible defaults
///
/// # Example
///
/// ```no_run
/// use vision_stimulus_engine::prelude::*;
///
/// let context = VSEContext::builder()
///     .with_window_size(1920, 1080)
///     .with_title("My Experiment")
///     .with_clear_color(0.5, 0.5, 0.5, 1.0)
///     .build()?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct VSEContextBuilder {
    config: VSEConfig,
    session: Option<ExperimentSession>,
}

impl VSEContextBuilder {
    /// Create a new builder with default settings
    pub fn new() -> Self {
        Self {
            config: VSEConfig::default(),
            session: None,
        }
    }

    /// Set window dimensions
    ///
    /// # Arguments
    ///
    /// * `width` - Window width in pixels
    /// * `height` - Window height in pixels
    pub fn with_window_size(mut self, width: u32, height: u32) -> Self {
        self.config.window_width = width;
        self.config.window_height = height;
        self
    }

    /// Set window title
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.config.window_title = title.into();
        self
    }

    /// Set GPU preference
    pub fn with_gpu_preference(mut self, preference: GPUPreference) -> Self {
        self.config.gpu_preference = preference;
        self
    }

    /// Set presentation mode
    ///
    /// - `Fifo`: VSync enabled (recommended for timing precision)
    /// - `Immediate`: No VSync (may cause tearing)
    /// - `Mailbox`: Low latency without tearing
    pub fn with_present_mode(mut self, mode: PresentMode) -> Self {
        self.config.present_mode = mode;
        self
    }

    /// Set initial clear color (RGBA, 0.0-1.0 range)
    pub fn with_clear_color(mut self, r: f32, g: f32, b: f32, a: f32) -> Self {
        self.config.clear_color = [r, g, b, a];
        self
    }

    /// Set expected refresh rate for missed frame detection.
    ///
    /// If not set, the refresh rate is auto-detected from the
    /// first 10 frames.
    pub fn with_expected_refresh_rate(mut self, hz: f64) -> Self {
        self.config.expected_refresh_rate = Some(hz);
        self
    }

    /// Enable the host↔scanout clock bridge.
    ///
    /// VSE's primary clock is the display's scanout clock; display timing needs no host clock.
    /// Enable this only when you must place host-originated events (key presses, network
    /// messages) into scanout time, or read a host-clock value for a scanout timestamp. It runs
    /// a low-rate background calibration ([`RenderContext::host_to_scanout`]) and requires the
    /// `VK_EXT_present_timing` backend; it is a no-op on the CPU-estimate path.
    pub fn with_host_clock_bridge(mut self) -> Self {
        self.config.host_clock_bridge = true;
        self
    }

    /// Set the window display mode.
    ///
    /// - `Windowed`: Standard resizable window (default)
    /// - `BorderlessFullscreen`: Borderless window covering the monitor
    /// - `ExclusiveFullscreen`: Exclusive fullscreen for lowest latency
    pub fn with_window_mode(mut self, mode: WindowMode) -> Self {
        self.config.window_mode = mode;
        self
    }

    /// Select which monitor to use for fullscreen modes.
    ///
    /// - `Primary`: Use the primary monitor (default)
    /// - `Index(n)`: Select by 0-based index
    /// - `Name(s)`: Select by case-insensitive name substring match
    pub fn with_monitor(mut self, selection: MonitorSelection) -> Self {
        self.config.monitor_selection = selection;
        self
    }

    /// Set whether the mouse cursor is visible.
    ///
    /// By default the cursor is hidden in fullscreen modes and visible
    /// in windowed mode. This override applies regardless of window mode.
    pub fn with_cursor_visible(mut self, visible: bool) -> Self {
        self.config.cursor_visible = Some(visible);
        self
    }

    /// Override the video mode selected in DirectDisplay mode.
    ///
    /// Default: highest refresh rate at native resolution.
    pub fn with_direct_display_video_mode(
        mut self,
        width: u32,
        height: u32,
        refresh_hz: f64,
    ) -> Self {
        self.config.direct_display_video_mode = Some((width, height, refresh_hz));
        self
    }

    /// Override the acquisition probe order for DirectDisplay mode.
    ///
    /// Default: [NoCompositor, DrmAcquire, XlibAcquire].
    /// Use this if you know your environment and want to skip failed probes.
    pub fn with_acquisition_order(mut self, order: Vec<AcquisitionMethod>) -> Self {
        self.config.direct_display_acquisition_order = Some(order);
        self
    }

    /// Attach an experiment session for data recording.
    ///
    /// Enables `record_frame()`, `record_annotation()`, and `record_event()`
    /// on `RenderContext`. If not set, data recording is disabled.
    pub fn with_session(mut self, session: ExperimentSession) -> Self {
        self.session = Some(session);
        self
    }

    /// Build the VSEContext
    ///
    /// This creates the event loop but does not yet create the window.
    /// The window is created when `run()` is called.
    ///
    /// # Errors
    ///
    /// Returns `VSEError` if initialization fails.
    pub fn build(self) -> Result<VSEContext, VSEError> {
        // Skip winit EventLoop creation in DirectDisplay mode — no compositor is present
        // (e.g. bare TTY), so EventLoop::new() would fail immediately.
        let event_loop = if self.config.window_mode == WindowMode::DirectDisplay {
            None
        } else {
            Some(EventLoop::new().map_err(|e| VSEError::EventLoop(e.to_string()))?)
        };

        info!(
            "VSEContext created with config: {}x{}, {:?}",
            self.config.window_width, self.config.window_height, self.config.present_mode
        );

        Ok(VSEContext {
            config: self.config,
            session: self.session,
            event_loop,
        })
    }
}

impl Default for VSEContextBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Source of input events for the current session.
enum InputSource {
    /// Events from winit (compositor mode).
    Winit,
    /// Events from evdev (direct display mode, Linux only).
    #[cfg(target_os = "linux")]
    Evdev(crate::core::evdev_input::EvdevReader),
}

/// Tracks per-frame recording state between flip() and record_frame() calls.
struct RecordingState {
    session: ExperimentSession,
    /// FlipInfo from the most recent flip(), available for record_frame().
    pending_flip: Option<FlipInfo>,
    /// frame_number of the most recently claimed flip (had record_frame called).
    last_claimed_frame: Option<u64>,
}

impl RecordingState {
    /// Called by flip() after FlipInfo is computed.
    /// Sends timing-only row for the previous unclaimed flip, then caches new flip.
    pub(crate) fn on_flip(&mut self, new_flip: FlipInfo) {
        if let Some(prev) = self.pending_flip.take() {
            let already_claimed = self.last_claimed_frame == Some(prev.frame_number);
            if !already_claimed {
                let _ = self.session.send_frame(FrameMessage {
                    flip: prev,
                    payload: None,
                    schema_name: "",
                });
            }
        }
        self.pending_flip = Some(new_flip);
    }

    /// Called on session shutdown — flushes final pending flip as timing-only if unclaimed.
    pub(crate) fn on_shutdown(&mut self) {
        if let Some(flip) = self.pending_flip.take() {
            let claimed = self.last_claimed_frame == Some(flip.frame_number);
            if !claimed {
                let _ = self.session.send_frame(FrameMessage {
                    flip,
                    payload: None,
                    schema_name: "",
                });
            }
        }
    }
}

/// Internal state that requires an active window
struct VSEState {
    window: Option<Arc<Window>>, // None in DirectDisplay mode
    device_selector: DeviceSelector,
    device: Arc<vulkano::device::Device>,
    queue: Arc<vulkano::device::Queue>,
    swapchain: SwapchainManager,
    #[allow(dead_code)]
    frame_builder: FrameBuilder,
    renderer: Renderer,
    should_close: bool,
    minimized: bool,
    input: InputState,
    cursor_visible: bool,
    window_mode: WindowMode,
    // Timing state
    clock: Clock,
    timing_provider: Box<dyn TimingProvider>,
    /// Raw acquire/submit/present engine for the EXT present-timing path. `None` on the
    /// CPU-estimate path (which keeps using vulkano's present).
    present_engine: Option<PresentEngine>,
    /// Scanout-timing records drained from the driver on the most recent flip.
    /// `vkGetPastPresentationTimingEXT` dequeues records, so they are read exactly once per frame
    /// here and cached for `scanout_feedback()` to return without re-draining. Empty on the
    /// CPU-estimate path.
    recent_scanouts: Vec<ScanoutFeedback>,
    /// Confirmed scanout records accumulated across flips, keyed by `present_id`. Feedback for a
    /// present arrives a frame or two after submission, so the buffered path stores records here
    /// and looks them up by the confirming frame's `present_id` (see `build_confirmed_flip`).
    /// Pruned on lookup to stay bounded (present ids are monotonic).
    scanout_by_present_id: std::collections::HashMap<u64, ScanoutFeedback>,
    frame_number: u64,
    last_present_time: Option<Timestamp>,
    /// `IMAGE_FIRST_PIXEL_OUT` scanout time (present-stage-local ns) of the last frame confirmed
    /// with hardware feedback, for computing scanout-delta missed detection on the buffered path.
    /// `None` until the first confirmed scanout record arrives.
    last_scanout_ns: Option<u64>,
    /// `present_id` paired with [`last_scanout_ns`]. A scanout delta is only trusted between
    /// *consecutive* present ids (`this == last + 1`); otherwise a lagged or dropped feedback
    /// record would inflate the delta into a false miss, so we fall back to the CPU delta.
    last_scanout_present_id: Option<u64>,
    expected_frame_duration: Option<Duration>,
    refresh_detect_samples: Vec<Duration>,
    /// Scanout-clock epoch (present-stage-local `t=0`), established on the first flip under the
    /// EXT backend. `None` on the CPU-estimate path (no scanout clock available).
    scanout_clock: Option<ScanoutClock>,
    // --- Driver-conformance observation (advertised present-timing features may be unimplemented) ---
    /// Whether the driver actually fills `IMAGE_FIRST_PIXEL_OUT` in feedback: `Some(true)` once a
    /// non-zero value is seen; `Some(false)` after enough feedback records arrive all-zero; `None`
    /// until determined. Recorded into `HostInfo` and drives a one-time guardrail warning.
    scanout_feedback_populated: Option<bool>,
    /// Count of feedback records seen while `scanout_feedback_populated` is still undetermined.
    scanout_feedback_probe_count: u32,
    /// One-time-warning latches for driver-conformance guardrails (feedback stubbed; scheduling
    /// software-paced), so the warnings fire once per session rather than every frame.
    warned_feedback_stub: bool,
    warned_sw_pacing: bool,
    /// Opt-in host↔scanout bridge (see `with_host_clock_bridge`). `None` unless requested and
    /// the EXT backend is active.
    host_bridge: Option<HostClockBridge>,
    /// VSE-clock time of the last bridge sample, for rate-limiting sampling off the hot path.
    last_bridge_sample_ts: Option<Timestamp>,
    input_source: InputSource,
    /// Physical display dimensions (from window or VkDisplaySurfaceKHR).
    display_size: (u32, u32),
    /// Which acquisition method succeeded, if in DirectDisplay mode.
    acquired_display: Option<AcquisitionMethod>,
    /// Optional data recording session.
    recording: Option<RecordingState>,

    // --- Buffered flip state (None/false when using synchronous run()) ---
    /// Transit slot: flip_with_payload() stores the payload here as a type-erased
    /// Box<dyn Any>. run_buffered() takes it out after the Render callback returns
    /// and downcasts it back to T. Always None outside the Render callback.
    buffered_pending_payload: Option<Box<dyn std::any::Any + Send + 'static>>,

    /// The confirmed FlipInfo for the frame being delivered in a Presented callback.
    /// Set by run_buffered() before invoking the Presented arm; cleared after.
    /// record_frame() reads this field instead of pending_flip when Some.
    buffered_confirmed_flip: Option<FlipInfo>,

    /// True while run_buffered() is executing. Guards flip_with_payload() and
    /// prevents flip() from being called in that context.
    in_buffered_mode: bool,

    /// In-flight fences paired with estimated FlipInfo. Populated by flip_with_payload(),
    /// drained by run_buffered() when GPU confirmation arrives.
    /// VecDeque because we always drain from the front (FIFO confirmation order).
    buffered_in_flight:
        std::collections::VecDeque<(FlipInfo, Box<dyn crate::core::buffered::InFlightFuture>)>,

    /// Tracks whether record_frame() was called during the current Presented callback.
    /// Reset to false before each Presented callback by run_buffered().
    buffered_record_called_this_presented: bool,

    /// Present-timing sub-features enabled at device creation (`Some` on the EXT backend).
    /// Carries the queue global-priority outcome into host-info snapshots.
    ext_features: Option<crate::core::present_timing_ext::EnabledPresentTimingFeatures>,

    /// Imported external-renderer frame ring (see `core::external_frame`).
    /// `None` unless a source was attached via `attach_external_frame_source`.
    external_source: Option<crate::core::external_frame::ExternalFrameRing>,

    /// One-shot readback buffer for the next consumed external frame
    /// (determinism-harness hook, armed via `arm_external_readback`).
    external_readback: Option<vulkano::buffer::Subbuffer<[u8]>>,
}

/// Main VisionStimulusEngine context
///
/// This is the primary interface for creating windows and managing
/// the rendering environment. Use the builder pattern to configure
/// the context before running.
///
/// # Example
///
/// ```no_run
/// use vision_stimulus_engine::prelude::*;
///
/// fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let context = VSEContext::builder()
///         .with_window_size(800, 600)
///         .with_title("VSE Example")
///         .with_clear_color(0.5, 0.5, 0.5, 1.0)
///         .build()?;
///
///     context.run(|vse| {
///         vse.clear()?;
///         let _info = vse.flip(None)?;
///         Ok(())
///     })?;
///
///     Ok(())
/// }
/// ```
pub struct VSEContext {
    config: VSEConfig,
    session: Option<ExperimentSession>,
    event_loop: Option<EventLoop<()>>,
}

/// Select and construct the timing backend for a freshly created device + swapchain.
///
/// When the device was created with `VK_EXT_present_timing` (`ext_features` is `Some`), use
/// the hardware backend; otherwise (or if its function pointers fail to load) fall back
/// loudly to CPU estimation.
fn build_timing_provider(
    device: &std::sync::Arc<vulkano::device::Device>,
    swapchain: &std::sync::Arc<vulkano::swapchain::Swapchain>,
    ext_features: Option<crate::core::present_timing_ext::EnabledPresentTimingFeatures>,
) -> Box<dyn TimingProvider> {
    if let Some(enabled) = ext_features {
        match unsafe { ExtPresentTimingProvider::new(device, swapchain, enabled) } {
            Some(p) => return Box::new(p),
            None => {
                warn!("VK_EXT_present_timing function pointers unavailable; using CPU estimation");
            }
        }
    }
    Box::new(CpuTimingProvider::new())
}

/// Build the opt-in host↔scanout bridge, if requested and supported.
///
/// The bridge needs the `VK_EXT_present_timing` backend's calibrated-timestamp sampler; on the
/// CPU-estimate path it cannot function, so the request is refused loudly rather than silently
/// producing a dead bridge.
fn build_host_bridge(config: &VSEConfig, provider: &dyn TimingProvider) -> Option<HostClockBridge> {
    if !config.host_clock_bridge {
        return None;
    }
    if provider.source() == TimingSource::ExtPresentTiming {
        // 2 s window: measured offset stability plateaus by ~1-2 s (docs/clock-synchronization.md).
        Some(HostClockBridge::new(Duration::from_secs(2)))
    } else {
        warn!("host_clock_bridge requested but EXT present-timing backend is unavailable; bridge disabled");
        None
    }
}

/// Build the raw present engine for the EXT present-timing path.
///
/// Returns `None` on the CPU-estimate path (which keeps using vulkano's present) and, loudly,
/// if the EXT backend is active but the engine's function pointers / sync objects cannot be
/// created — in which case `flip()` degrades to the vulkano present with no present-id or
/// scanout feedback.
fn build_present_engine(
    device: &Arc<vulkano::device::Device>,
    image_count: u32,
    provider: &dyn TimingProvider,
) -> Option<PresentEngine> {
    if provider.source() != TimingSource::ExtPresentTiming {
        return None;
    }
    match PresentEngine::new(device, image_count) {
        Some(e) => Some(e),
        None => {
            warn!(
                "EXT present-timing backend active but PresentEngine unavailable; \
                 falling back to vulkano present (no present-id / scanout feedback)"
            );
            None
        }
    }
}

impl VSEState {
    /// Recreate the swapchain from the current surface and notify the timing provider so it
    /// refreshes any cached swapchain handle (a retired handle is UB to query).
    fn recreate_swapchain(&mut self, win_size: [u32; 2]) -> Result<(), SwapchainError> {
        self.swapchain.recreate_from_surface(win_size)?;
        self.timing_provider
            .on_swapchain_recreated(self.swapchain.swapchain());
        Ok(())
    }

    /// Per-flip clock maintenance: establish the scanout epoch on the first flip and, when the
    /// host-clock bridge is enabled, feed it a low-rate calibration sample. One calibrated read
    /// serves both. A no-op on the CPU-estimate path (the provider's sampler returns `None`).
    fn update_clocks(&mut self) {
        let need_epoch = self.scanout_clock.is_none();
        let bridge_due = self.host_bridge.is_some() && self.bridge_sample_due();
        if !need_epoch && !bridge_due {
            return;
        }
        if let Some(sample) = self.timing_provider.sample_present_calibration() {
            if need_epoch {
                // First scanout reading is the session's scanout `t=0`.
                self.scanout_clock = Some(ScanoutClock::new(sample.stage_ns));
            }
            if bridge_due {
                self.last_bridge_sample_ts = Some(self.clock.now());
                if let Some(bridge) = &mut self.host_bridge {
                    bridge.push(sample);
                }
            }
        }
    }

    /// Whether enough time has elapsed to take another bridge sample (~10 Hz), keeping the
    /// calibrated-timestamp read off the presentation hot path.
    fn bridge_sample_due(&self) -> bool {
        const RESAMPLE_US: u64 = 100_000; // 100 ms
        let now = self.clock.now().as_micros();
        self.last_bridge_sample_ts
            .map_or(true, |t| now.saturating_sub(t.as_micros()) >= RESAMPLE_US)
    }

    /// Record confirmed scanout feedback drained this frame: index each record by its
    /// `present_id` for later buffered confirmation, and keep the raw list for
    /// `scanout_feedback()`. The read is destructive, so this is called exactly once per flip.
    fn ingest_scanout_feedback(&mut self, feedback: Vec<ScanoutFeedback>) {
        for fb in &feedback {
            self.scanout_by_present_id.insert(fb.present_id, *fb);
        }
        self.observe_feedback_conformance(&feedback);
        self.recent_scanouts = feedback;
    }

    /// Passively observe whether the driver actually populates `IMAGE_FIRST_PIXEL_OUT` in
    /// present-timing feedback (some drivers advertise `VK_EXT_present_timing` but return
    /// zero-valued stage timestamps). Latches `scanout_feedback_populated` and, on the first
    /// determination that it is stubbed, emits a one-time guardrail warning naming the workaround
    /// VSE is using. Cheap and automatic — no extra presents.
    fn observe_feedback_conformance(&mut self, feedback: &[ScanoutFeedback]) {
        if self.scanout_feedback_populated == Some(true) {
            return;
        }
        if feedback
            .iter()
            .any(|f| f.first_pixel_out_ns.is_some_and(|v| v != 0))
        {
            self.scanout_feedback_populated = Some(true);
            return;
        }
        // All-zero so far. Once enough records have arrived all-zero, conclude the driver stubs the
        // stage timestamps (16 ≈ the driver's timing-ring depth — a full turnover with no real value).
        self.scanout_feedback_probe_count = self
            .scanout_feedback_probe_count
            .saturating_add(feedback.len() as u32);
        if self.scanout_feedback_populated.is_none() && self.scanout_feedback_probe_count >= 16 {
            self.scanout_feedback_populated = Some(false);
            if !self.warned_feedback_stub {
                self.warned_feedback_stub = true;
                warn!(
                    "VK_EXT_present_timing: the driver returns present-timing feedback that \
                     correlates by present_id but stubs the scanout stage timestamps \
                     (IMAGE_FIRST_PIXEL_OUT = 0) — advertised but not implemented (seen on \
                     Intel/ANV/Mesa 26.1). present_time is derived from the calibrated \
                     PRESENT_STAGE_LOCAL clock instead; scanout timing stays valid. See \
                     docs/clock-synchronization.md."
                );
            }
        }
    }

    /// Take the confirmed scanout record for `present_id`, pruning it and every older entry
    /// (present ids are monotonic, so records ≤ `present_id` are past and never looked up again).
    /// Keeps `scanout_by_present_id` bounded to the in-flight + feedback-lag window.
    fn take_scanout_for(&mut self, present_id: u64) -> Option<ScanoutFeedback> {
        let found = self.scanout_by_present_id.remove(&present_id);
        self.scanout_by_present_id.retain(|&id, _| id > present_id);
        found
    }

    /// Rebase a driver `IMAGE_FIRST_PIXEL_OUT` scanout time (present-stage-local ns) into a
    /// scanout-domain [`Timestamp`] (µs since the session's scanout `t=0`). This is the value
    /// stored in `FlipInfo.present_time` under `ExtPresentTiming` — a hardware scanout time, not a
    /// host-clock time (see the clock model in `docs/clock-synchronization.md`). `None` before the
    /// scanout epoch is established, or when the driver reports no real scanout time.
    ///
    /// A real `IMAGE_FIRST_PIXEL_OUT` is an absolute present-stage-local value (~10¹³ ns), never
    /// zero. Windowed compositors that cannot observe true scanout report the stage with time `0`
    /// (seen on windowed Wayland / ANV); that is *not* a scanout time, so it yields `None` and the
    /// caller falls back to CPU fence time. Real values appear on the direct-display path.
    fn scanout_present_time(&self, first_pixel_out_ns: u64) -> Option<Timestamp> {
        if first_pixel_out_ns == 0 {
            return None;
        }
        let clock = self.scanout_clock?;
        Some(Timestamp::from_micros(
            clock.rebase(first_pixel_out_ns).as_micros(),
        ))
    }

    /// Sample the present-stage-local scanout clock **now** and rebase it to a scanout-domain
    /// [`Timestamp`] (µs since `t=0`). `None` before the scanout epoch is established or on the
    /// CPU backend.
    ///
    /// Used by the synchronous `flip()` immediately after `wait_for_present` returns — i.e. right
    /// at this frame's scanout — to obtain a real scanout `present_time`. This is the fallback for
    /// the (measured) case where the driver stubs `vkGetPastPresentationTimingEXT`'s stage times
    /// to zero (Intel/ANV/Mesa 26.1): the feedback still correlates by `present_id` but carries no
    /// timestamp, whereas the calibrated `PRESENT_STAGE_LOCAL` clock reports real, vblank-cadence
    /// values. The sampled value is scanout-begin plus the calibrated-read latency (tens of µs —
    /// far below the display-panel latency floor that a photodiode measures anyway).
    fn sample_scanout_now(&self) -> Option<Timestamp> {
        let clock = self.scanout_clock?;
        let sample = self.timing_provider.sample_present_calibration()?;
        Some(Timestamp::from_micros(clock.rebase(sample.stage_ns).as_micros()))
    }

    /// Convert a scheduling target (`target`, scanout-domain µs since `t=0`) into an absolute
    /// [`ScheduledTarget`] the driver can schedule against: absolute present-stage-local ns in the
    /// swapchain's `PRESENT_STAGE_LOCAL` domain. `None` until the scanout epoch and domain id are
    /// known (the first flip, before `t=0` is established) — the caller then presents unscheduled.
    fn scheduled_target(&self, target: Timestamp) -> Option<ScheduledTarget> {
        let epoch = self.scanout_clock?.epoch_stage_ns();
        let time_domain_id = self.timing_provider.present_stage_domain_id()?;
        let target_time_ns = epoch.saturating_add(target.as_micros().saturating_mul(1_000));
        Some(ScheduledTarget {
            target_time_ns,
            time_domain_id,
        })
    }

    /// The display's refresh interval, if known (driver-reported, else the auto-detected estimate).
    fn refresh_interval(&self) -> Option<Duration> {
        self.timing_provider
            .refresh_cycle_duration()
            .or(self.expected_frame_duration)
    }

    /// One-time guardrail note (per session) that scheduled presents are being software-paced,
    /// since hardware `targetTime` enforcement is driver-dependent and unverified at runtime.
    fn note_scheduling_once(&mut self) {
        if !self.warned_sw_pacing {
            self.warned_sw_pacing = true;
            info!(
                "Scheduled present requested: VSE paces it against the scanout clock (software). \
                 Hardware targetTime enforcement is driver-dependent and NOT verified here — it is \
                 ignored on Intel/ANV/Mesa 26.1. Characterize your driver with \
                 `examples/13_direct_display_scanout` (reports absolute_scheduling_enforced)."
            );
        }
    }

    /// Software scanout-domain pacing for a scheduled flip (sync path only).
    ///
    /// Many drivers advertise `presentAtAbsoluteTime` but do **not** enforce
    /// `VkPresentTimingInfoEXT.targetTime` (measured: Intel/ANV/Mesa 26.1 ignores it), so VSE paces
    /// the present itself. FIFO quantizes every present to a vblank, so we need only issue the
    /// present within the target vblank's preceding refresh interval — sleep on the scanout clock
    /// until then. The wait is a scanout-domain *duration* (≈ real time to within the ~2 ppm
    /// scanout↔CPU drift), so no absolute cross-clock math enters the loop. Harmless on drivers that
    /// *do* honor `targetTime` — they would present at the same vblank; we just avoid submitting
    /// early. No-op when the scanout clock isn't established yet, or the target is already imminent.
    fn pace_to_scanout_target(&self, target: Timestamp, refresh: Duration) {
        let Some(now) = self.sample_scanout_now() else {
            return;
        };
        // Aim to submit ~half a refresh before the target vblank: FIFO then shows it at that vblank,
        // robust to ±½-refresh sleep jitter.
        let present_at_us = target.as_micros().saturating_sub(refresh.as_micros() as u64 / 2);
        let now_us = now.as_micros();
        if present_at_us > now_us {
            std::thread::sleep(Duration::from_micros(present_at_us - now_us));
        }
    }

    /// Build a confirmed `FlipInfo` from an estimated one captured at submit time.
    ///
    /// On the EXT backend, keys on the frame's `present_id`: if this frame's
    /// `IMAGE_FIRST_PIXEL_OUT` record has arrived (it was presented `depth+1` frames ago, so it
    /// normally has), `present_time` is the real hardware scanout time (rebased to the scanout
    /// epoch) and missed detection uses the scanout delta; otherwise it falls back to CPU fence
    /// time. `timing_source` records which domain the value is in.
    fn build_confirmed_flip(&mut self, estimated: FlipInfo) -> FlipInfo {
        // Look up this frame's confirmed scanout record by present_id, pruning consumed/past
        // entries (present ids are monotonic, so anything ≤ this id is no longer needed).
        let scanout = self.take_scanout_for(estimated.present_id);

        // This frame's real scanout-begin time, if the driver reported one. Zero means "no real
        // scanout time" — filtered here so both present_time and missed detection below fall back
        // to the CPU clock rather than treating 0 as a timestamp.
        //
        // Note: unlike the synchronous `flip()` path — which blocks on `wait_for_present` and can
        // therefore sample the calibrated scanout clock at the frame's scanout — the buffered path
        // is pipelined and never blocks per frame, so it can only use the driver's per-present
        // feedback. Where that feedback is stubbed to 0 (Intel/ANV/Mesa 26.1), buffered
        // `present_time` falls back to CPU time; it becomes scanout-native automatically on drivers
        // that populate `IMAGE_FIRST_PIXEL_OUT`.
        let scanout_ns = scanout
            .and_then(|fb| fb.first_pixel_out_ns)
            .filter(|&ns| ns != 0);

        // present_time is the hardware scanout time for this frame when available, else CPU fence
        // time. `timing_source` (already ExtPresentTiming) plus a real scanout record is the domain
        // guard; there is no separate field (see the B3 schema decision).
        let confirmed_present = scanout_ns
            .and_then(|ns| self.scanout_present_time(ns))
            .unwrap_or_else(|| self.timing_provider.record_present_time(&self.clock));

        let cpu_frame_duration = self
            .last_present_time
            .map(|prev| confirmed_present.duration_since(prev));

        let expected = self
            .expected_frame_duration
            .unwrap_or(Duration::from_micros(16_667));

        // Auto-detect refresh rate using the same logic as flip()
        if self.expected_frame_duration.is_none() {
            if let Some(dur) = self.timing_provider.refresh_cycle_duration() {
                self.expected_frame_duration = Some(dur);
            } else if let Some(dur) = cpu_frame_duration {
                self.refresh_detect_samples.push(dur);
                if self.refresh_detect_samples.len() >= 10 {
                    let total: Duration = self.refresh_detect_samples.iter().copied().sum();
                    let avg = total / self.refresh_detect_samples.len() as u32;
                    self.expected_frame_duration = Some(avg);
                }
            }
        }

        // Missed detection prefers the real scanout delta (present-stage-local ns), but only
        // between *consecutive* present ids where both records are present — feedback for a
        // present lags its submission (at depth 1 it usually arrives after this frame's
        // confirmation), so a non-consecutive delta would span a gap and false-flag a miss.
        // Otherwise fall back to the CPU present-time delta.
        let frame_duration = match scanout_ns {
            Some(scanout_ns) => {
                let consecutive =
                    self.last_scanout_present_id == Some(estimated.present_id.wrapping_sub(1));
                let dur = match (self.last_scanout_ns, consecutive) {
                    (Some(prev), true) => {
                        Some(Duration::from_nanos(scanout_ns.saturating_sub(prev)))
                    }
                    _ => cpu_frame_duration,
                };
                self.last_scanout_ns = Some(scanout_ns);
                self.last_scanout_present_id = Some(estimated.present_id);
                dur
            }
            None => cpu_frame_duration,
        };

        let (missed, missed_count) = match frame_duration {
            Some(dur) => {
                let ratio = dur.as_micros() as f64 / expected.as_micros() as f64;
                if ratio > 1.5 {
                    (true, (ratio.round() as u32).saturating_sub(1))
                } else {
                    (false, 0)
                }
            }
            None => (false, 0),
        };

        // on_target: with a real scanout time (same domain as the target) the scheduled present
        // met its deadline iff scanout landed at/after target. Without one (windowed), keep the
        // estimate rather than claim a verification we cannot make.
        let on_target = match (estimated.target_time, scanout_ns) {
            (Some(target), Some(_)) => confirmed_present.as_micros() >= target.as_micros(),
            _ => estimated.on_target,
        };

        let flip = FlipInfo {
            frame_number: estimated.frame_number,
            timing_source: self.timing_provider.source(),
            submit_time: estimated.submit_time,
            present_time: confirmed_present,
            present_id: estimated.present_id,
            target_time: estimated.target_time,
            on_target,
            missed,
            missed_count,
            skipped: false,
        };

        self.last_present_time = Some(confirmed_present);

        flip
    }
}

impl VSEContext {
    /// Create a new VSE context with default settings
    ///
    /// For more control over initialization, use [`VSEContext::builder()`].
    ///
    /// # Errors
    ///
    /// Returns `VSEError` if initialization fails.
    pub fn new() -> Result<Self, VSEError> {
        Self::builder().build()
    }

    /// Create a builder for custom configuration
    pub fn builder() -> VSEContextBuilder {
        VSEContextBuilder::new()
    }

    /// Initialize Vulkan state from an event loop window target
    fn initialize_compositor(
        elwt: &EventLoopWindowTarget<()>,
        config: &VSEConfig,
    ) -> Result<VSEState, VSEError> {
        // --- Resolve target monitor ---
        let target_monitor = match &config.monitor_selection {
            MonitorSelection::Primary => elwt.primary_monitor(),
            MonitorSelection::Index(idx) => {
                let monitors: Vec<_> = elwt.available_monitors().collect();
                if *idx < monitors.len() {
                    Some(monitors[*idx].clone())
                } else {
                    warn!(
                        "Monitor index {} out of range ({}  available), falling back to primary",
                        idx,
                        monitors.len()
                    );
                    elwt.primary_monitor()
                }
            }
            MonitorSelection::Name(name) => {
                let name_lower = name.to_lowercase();
                let found = elwt.available_monitors().find(|m| {
                    m.name()
                        .map(|n| n.to_lowercase().contains(&name_lower))
                        .unwrap_or(false)
                });
                if found.is_none() {
                    warn!(
                        "No monitor matching '{}' found, falling back to primary",
                        name
                    );
                }
                found.or_else(|| elwt.primary_monitor())
            }
        };

        // --- Build fullscreen setting ---
        let fullscreen = match config.window_mode {
            WindowMode::Windowed | WindowMode::DirectDisplay => None,
            WindowMode::BorderlessFullscreen => {
                Some(Fullscreen::Borderless(target_monitor.clone()))
            }
            WindowMode::ExclusiveFullscreen => {
                if let Some(ref monitor) = target_monitor {
                    // Find best video mode: match configured resolution if possible,
                    // then pick highest refresh rate, fall back to native resolution.
                    let modes: Vec<_> = monitor.video_modes().collect();

                    let best = modes
                        .iter()
                        .filter(|m| {
                            m.size().width == config.window_width
                                && m.size().height == config.window_height
                        })
                        .max_by(|a, b| {
                            a.refresh_rate_millihertz()
                                .cmp(&b.refresh_rate_millihertz())
                        })
                        .or_else(|| {
                            // Fall back to native resolution (highest refresh rate)
                            modes.iter().max_by(|a, b| {
                                let area_a = a.size().width * a.size().height;
                                let area_b = b.size().width * b.size().height;
                                area_a.cmp(&area_b).then(
                                    a.refresh_rate_millihertz()
                                        .cmp(&b.refresh_rate_millihertz()),
                                )
                            })
                        });

                    match best {
                        Some(mode) => {
                            info!(
                                "Exclusive fullscreen: {}x{} @ {:.1} Hz",
                                mode.size().width,
                                mode.size().height,
                                mode.refresh_rate_millihertz() as f64 / 1000.0
                            );
                            Some(Fullscreen::Exclusive(mode.clone()))
                        }
                        None => {
                            warn!("No video modes found, falling back to borderless fullscreen");
                            Some(Fullscreen::Borderless(target_monitor.clone()))
                        }
                    }
                } else {
                    warn!("No monitor found for exclusive fullscreen, falling back to borderless");
                    Some(Fullscreen::Borderless(None))
                }
            }
        };

        let window = WindowBuilder::new()
            .with_title(&config.window_title)
            .with_inner_size(PhysicalSize::new(config.window_width, config.window_height))
            .with_fullscreen(fullscreen)
            .build(elwt)
            .map_err(|e| VSEError::Window(e.to_string()))?;

        let window = Arc::new(window);

        // Apply cursor visibility: auto-hide in fullscreen, visible in windowed, overridable
        let cursor_visible = config
            .cursor_visible
            .unwrap_or(matches!(config.window_mode, WindowMode::Windowed));
        window.set_cursor_visible(cursor_visible);

        let actual_size = window.inner_size();
        info!(
            "Window created: {}x{} mode={:?} cursor_visible={}",
            actual_size.width, actual_size.height, config.window_mode, cursor_visible
        );

        // Initialize Vulkan
        let (device_selector, surface) =
            DeviceSelector::with_surface(config.gpu_preference, window.clone())?;

        let (device, queue, ext_features) = device_selector.create_device()?;

        // Use the actual window size, not the configured size. In fullscreen
        // modes the OS/compositor will have already sized the window to the
        // monitor before Vulkan initialization runs.
        let win_size = window.inner_size();
        let swapchain_config = SwapchainConfig {
            width: win_size.width,
            height: win_size.height,
            present_mode: config.present_mode,
            image_count: 2,
        };

        // Opt the swapchain into present-id2 / present-wait2 (raw create) when the device enabled
        // presentWait2, so the synchronous flip() can block on vkWaitForPresent2KHR for this
        // frame's real scanout time.
        let present_opt_in = ext_features.map(|f| f.present_wait2).unwrap_or(false);
        let swapchain = SwapchainManager::new_with_present_opt_in(
            device.clone(),
            surface,
            swapchain_config,
            present_opt_in,
        )?;
        let frame_builder = FrameBuilder::new(device.clone(), queue.clone());
        let renderer = Renderer::new(device.clone(), queue.clone(), swapchain.format())?;

        // Initialize timing
        let clock = Clock::new();

        let timing_provider: Box<dyn TimingProvider> =
            build_timing_provider(&device, swapchain.swapchain(), ext_features);
        let host_bridge = build_host_bridge(config, timing_provider.as_ref());
        let present_engine = build_present_engine(
            &device,
            swapchain.images().len() as u32,
            timing_provider.as_ref(),
        );

        let expected_frame_duration = config
            .expected_refresh_rate
            .map(|hz| Duration::from_micros((1_000_000.0 / hz) as u64));

        info!("Vulkan initialization complete");

        let win_size = (actual_size.width, actual_size.height);

        Ok(VSEState {
            window: Some(window),
            device_selector,
            device,
            queue,
            swapchain,
            frame_builder,
            renderer,
            should_close: false,
            minimized: false,
            input: InputState::new(),
            cursor_visible,
            window_mode: config.window_mode,
            clock,
            timing_provider,
            present_engine,
            recent_scanouts: Vec::new(),
            scanout_by_present_id: std::collections::HashMap::new(),
            frame_number: 0,
            last_present_time: None,
            last_scanout_ns: None,
            last_scanout_present_id: None,
            expected_frame_duration,
            refresh_detect_samples: Vec::with_capacity(10),
            scanout_clock: None,
            scanout_feedback_populated: None,
            scanout_feedback_probe_count: 0,
            warned_feedback_stub: false,
            warned_sw_pacing: false,
            host_bridge,
            last_bridge_sample_ts: None,
            input_source: InputSource::Winit,
            display_size: win_size,
            acquired_display: None,
            recording: None,
            buffered_pending_payload: None,
            buffered_confirmed_flip: None,
            in_buffered_mode: false,
            buffered_in_flight: std::collections::VecDeque::new(),
            buffered_record_called_this_presented: false,
            ext_features,
            external_source: None,
            external_readback: None,
        })
    }

    /// Initialize Vulkan state for direct display mode (no winit, no compositor).
    #[cfg(target_os = "linux")]
    fn initialize_direct(config: &VSEConfig) -> Result<VSEState, VSEError> {
        use crate::core::direct_display::{acquire_display, default_acquisition_order};
        use crate::core::evdev_input::EvdevReader;
        use vulkano::VulkanObject;

        let target_name = match &config.monitor_selection {
            MonitorSelection::Name(n) => Some(n.as_str()),
            _ => None,
        };

        let (device_selector, instance) =
            DeviceSelector::with_direct_display(config.gpu_preference).map_err(VSEError::Device)?;

        let phys_dev = device_selector.physical_device().handle();

        let order = config
            .direct_display_acquisition_order
            .clone()
            .unwrap_or_else(default_acquisition_order);

        let direct_surface = acquire_display(
            &instance,
            phys_dev,
            target_name,
            config.direct_display_video_mode,
            &order,
        )?;

        let (width, height) = (direct_surface.width, direct_surface.height);
        let method = direct_surface.method;
        let surface = direct_surface.surface;

        let (device, queue, ext_features) =
            device_selector.create_device().map_err(VSEError::Device)?;

        let swapchain_config = SwapchainConfig {
            width,
            height,
            present_mode: config.present_mode,
            image_count: 2,
        };

        // Opt into present-id2 / present-wait2 (raw swapchain) when presentWait2 was enabled.
        let present_opt_in = ext_features.map(|f| f.present_wait2).unwrap_or(false);
        let swapchain = SwapchainManager::new_with_present_opt_in(
            device.clone(),
            surface,
            swapchain_config,
            present_opt_in,
        )?;
        let frame_builder = FrameBuilder::new(device.clone(), queue.clone());
        let renderer = Renderer::new(device.clone(), queue.clone(), swapchain.format())?;

        let clock = Clock::new();

        let timing_provider: Box<dyn TimingProvider> =
            build_timing_provider(&device, swapchain.swapchain(), ext_features);
        let host_bridge = build_host_bridge(config, timing_provider.as_ref());
        let present_engine = build_present_engine(
            &device,
            swapchain.images().len() as u32,
            timing_provider.as_ref(),
        );

        let expected_frame_duration = config
            .expected_refresh_rate
            .map(|hz| Duration::from_micros((1_000_000.0 / hz) as u64));

        let evdev_reader = match EvdevReader::open() {
            Ok(mut r) => {
                r.set_display_size(width, height);
                r
            }
            Err(msg) => {
                warn!("evdev input unavailable: {}", msg);
                EvdevReader::empty()
            }
        };

        info!("Direct display initialization complete");

        Ok(VSEState {
            window: None,
            device_selector,
            device,
            queue,
            swapchain,
            frame_builder,
            renderer,
            should_close: false,
            minimized: false,
            input: InputState::new(),
            cursor_visible: false,
            window_mode: WindowMode::DirectDisplay,
            clock,
            timing_provider,
            present_engine,
            recent_scanouts: Vec::new(),
            scanout_by_present_id: std::collections::HashMap::new(),
            recording: None,
            frame_number: 0,
            last_present_time: None,
            last_scanout_ns: None,
            last_scanout_present_id: None,
            expected_frame_duration,
            refresh_detect_samples: Vec::with_capacity(10),
            scanout_clock: None,
            scanout_feedback_populated: None,
            scanout_feedback_probe_count: 0,
            warned_feedback_stub: false,
            warned_sw_pacing: false,
            host_bridge,
            last_bridge_sample_ts: None,
            input_source: InputSource::Evdev(evdev_reader),
            display_size: (width, height),
            acquired_display: Some(method),
            buffered_pending_payload: None,
            buffered_confirmed_flip: None,
            in_buffered_mode: false,
            buffered_in_flight: std::collections::VecDeque::new(),
            buffered_record_called_this_presented: false,
            ext_features,
            external_source: None,
            external_readback: None,
        })
    }

    /// Run the direct display render loop (no winit).
    #[cfg(target_os = "linux")]
    fn run_direct<F>(mut self, mut render_fn: F) -> Result<(), VSEError>
    where
        F: FnMut(&mut RenderContext) -> Result<(), VSEError> + 'static,
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let mut state = Self::initialize_direct(&self.config)?;
        state.recording = self.session.take().map(|session| RecordingState {
            session,
            pending_flip: None,
            last_claimed_frame: None,
        });
        let mut config = self.config;

        // Install a SIGINT (Ctrl+C) handler so the loop can exit cleanly,
        // running all Drop implementations and releasing the display surface
        // before the process terminates.  Without this, Ctrl+C kills the
        // process mid-flight, Vulkan never releases the display, and the
        // TTY is left with a blank screen.
        let quit_flag = Arc::new(AtomicBool::new(false));
        let quit_clone = quit_flag.clone();
        let _ = ctrlc::set_handler(move || {
            quit_clone.store(true, Ordering::SeqCst);
        });

        // Capture whether we need to restore the VT console on exit.
        // Only applies to bare-TTY acquisition paths (not Xlib).
        let restore_vt = matches!(
            state.acquired_display,
            Some(AcquisitionMethod::NoCompositor) | Some(AcquisitionMethod::DrmAcquire)
        );

        let loop_result: Option<VSEError> = loop {
            // Check both the SIGINT flag and any in-callback exit request.
            if quit_flag.load(Ordering::SeqCst) || state.should_close {
                info!("Direct display loop exiting");
                break None;
            }

            if let InputSource::Evdev(ref mut reader) = state.input_source {
                reader.poll(&mut state.input, &state.clock);
            }

            let mut render_ctx = RenderContext {
                state: &mut state,
                config: &mut config,
            };

            if let Err(e) = render_fn(&mut render_ctx) {
                warn!("Render error: {}", e);
                break Some(e);
            }

            // Clear per-frame input state AFTER the callback runs, mirroring
            // the winit path — poll() populates keys_just_pressed, so
            // begin_frame() must not run before the callback or those events
            // are erased before the user ever sees them.
            state.input.begin_frame();
        };

        // Flush final pending flip before dropping
        if let Some(recording) = &mut state.recording {
            recording.on_shutdown();
        }

        // Drop Vulkan state first so the display is released before we
        // attempt to restore the VT text mode.
        drop(state);

        if restore_vt {
            use std::os::unix::io::AsRawFd;

            // Restore the VT text console after Vulkan releases DRM.
            //
            // When drop(state) closes the Vulkan device the kernel transfers
            // DRM master back to fbcon asynchronously (~5–20 ms on i915).
            // Simply writing to /dev/tty is not enough: fbcon's GEM
            // framebuffer is not yet wired to the CRTC scanout plane, so
            // text writes update fbcon's virtual buffer but never appear.
            //
            // Correct sequence:
            //   1. Poll FBIO_WAITFORVSYNC on /dev/fb0 until fbcon has an
            //      active DRM CRTC (condition-based; no fixed sleep needed).
            //   2. FBIOBLANK(FB_BLANK_UNBLANK) on the same fd — triggers
            //      drm_client_modeset_commit(), which performs the atomic
            //      commit that wires fbcon's GEM buffer to the CRTC.
            //   3. Flush the TTY input queue so evdev-captured keystrokes
            //      (e.g. Escape) are not left for the shell to misread.
            //   4. KDSETMODE(KD_TEXT) + VT_ACTIVATE to re-initialise the VT.
            //   5. Write the ANSI clear sequence so the terminal content is
            //      refreshed cleanly.

            // _IOW('F', 0x20, u32)
            const FBIO_WAITFORVSYNC: libc::c_ulong = 0x40044620;
            // _IO('F', 0x11)
            const FBIOBLANK: libc::c_ulong = 0x4611;

            let poll_start = std::time::Instant::now();
            let poll_deadline = poll_start + std::time::Duration::from_secs(5);
            while std::time::Instant::now() < poll_deadline {
                if let Ok(fb) = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open("/dev/fb0")
                {
                    let arg: u32 = 0;
                    let r = unsafe { libc::ioctl(fb.as_raw_fd(), FBIO_WAITFORVSYNC, &arg) };
                    if r == 0 {
                        unsafe { libc::ioctl(fb.as_raw_fd(), FBIOBLANK, 0i32) };
                        info!(
                            "fbcon DRM handoff complete ({}ms)",
                            poll_start.elapsed().as_millis()
                        );
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            const KDSETMODE: libc::c_ulong = 0x4B3A;
            const KD_TEXT: libc::c_int = 0;
            const VT_GETSTATE: libc::c_ulong = 0x5603;
            const VT_ACTIVATE: libc::c_ulong = 0x5606;
            const VT_WAITACTIVE: libc::c_ulong = 0x5607;

            #[repr(C)]
            struct VtStat {
                v_active: libc::c_ushort,
                v_signal: libc::c_ushort,
                v_state: libc::c_ushort,
            }

            if let Ok(mut tty) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
            {
                let fd = tty.as_raw_fd();
                unsafe {
                    libc::tcflush(fd, libc::TCIFLUSH);
                    libc::ioctl(fd, KDSETMODE, KD_TEXT);
                    let mut vtstat = VtStat {
                        v_active: 0,
                        v_signal: 0,
                        v_state: 0,
                    };
                    if libc::ioctl(fd, VT_GETSTATE, &mut vtstat) == 0 {
                        let vt = vtstat.v_active as libc::c_int;
                        libc::ioctl(fd, VT_ACTIVATE, vt);
                        libc::ioctl(fd, VT_WAITACTIVE, vt);
                    }
                }
                use std::io::Write;
                let _ = tty.write_all(b"\x1b[H\x1b[2J");
                info!("VT text mode restored");
            }
        }

        info!("Direct display loop exited");
        match loop_result {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Run the main event loop
    ///
    /// This method takes ownership of the context and runs the event loop
    /// until the window is closed. The provided callback is called once
    /// per frame.
    ///
    /// # Arguments
    ///
    /// * `render_fn` - A callback that is called each frame for rendering
    ///
    /// # Errors
    ///
    /// Returns `VSEError` if an error occurs during rendering.
    pub fn run<F>(mut self, mut render_fn: F) -> Result<(), VSEError>
    where
        F: FnMut(&mut RenderContext) -> Result<(), VSEError> + 'static,
    {
        // Branch for direct display mode (Linux only — no winit event loop)
        #[cfg(target_os = "linux")]
        if self.config.window_mode == WindowMode::DirectDisplay {
            return self.run_direct(render_fn);
        }
        #[cfg(not(target_os = "linux"))]
        if self.config.window_mode == WindowMode::DirectDisplay {
            return Err(VSEError::DirectDisplayUnavailable(
                "Direct display mode is only supported on Linux".to_string(),
            ));
        }

        let event_loop = self
            .event_loop
            .take()
            .ok_or_else(|| VSEError::EventLoop("Event loop already consumed".into()))?;

        let mut config = self.config;
        let mut session = self.session;
        let mut state: Option<VSEState> = None;
        let error: Rc<RefCell<Option<VSEError>>> = Rc::new(RefCell::new(None));
        let error_clone = error.clone();

        event_loop
            .run(move |event, elwt| {
                elwt.set_control_flow(ControlFlow::Poll);

                match event {
                    Event::Resumed => {
                        if state.is_some() {
                            return;
                        }

                        match Self::initialize_compositor(elwt, &config) {
                            Ok(mut s) => {
                                s.recording = session.take().map(|sess| RecordingState {
                                    session: sess,
                                    pending_flip: None,
                                    last_claimed_frame: None,
                                });
                                state = Some(s);
                            }
                            Err(e) => {
                                *error_clone.borrow_mut() = Some(e);
                                elwt.exit();
                            }
                        }
                    }
                    Event::WindowEvent {
                        event: window_event,
                        ..
                    } => {
                        let s = match &mut state {
                            Some(s) => s,
                            None => return,
                        };

                        match window_event {
                            WindowEvent::CloseRequested => {
                                info!("Window close requested");
                                s.should_close = true;
                                elwt.exit();
                            }
                            WindowEvent::Resized(new_size) => {
                                debug!("Window resized to {}x{}", new_size.width, new_size.height);
                                if new_size.width == 0 || new_size.height == 0 {
                                    s.minimized = true;
                                } else {
                                    s.minimized = false;
                                    s.display_size = (new_size.width, new_size.height);
                                    s.swapchain.mark_needs_recreation();
                                }
                            }
                            WindowEvent::KeyboardInput { event, .. } => {
                                if let PhysicalKey::Code(key_code) = event.physical_key {
                                    let timestamp = s.clock.now();
                                    let logical_key = event.logical_key.clone();
                                    match event.state {
                                        ElementState::Pressed => {
                                            let repeat = s.input.keys_down.contains(&key_code);
                                            s.input.keys_down.insert(key_code);
                                            if !repeat {
                                                s.input.keys_just_pressed.insert(key_code);
                                            }
                                            s.input.events.push(InputEvent::KeyDown {
                                                key_code,
                                                logical_key,
                                                timestamp,
                                                repeat,
                                            });
                                        }
                                        ElementState::Released => {
                                            s.input.keys_down.remove(&key_code);
                                            s.input.keys_just_released.insert(key_code);
                                            s.input.events.push(InputEvent::KeyUp {
                                                key_code,
                                                logical_key,
                                                timestamp,
                                            });
                                        }
                                    }
                                }
                            }
                            WindowEvent::CursorMoved { position, .. } => {
                                let timestamp = s.clock.now();
                                s.input.mouse_position = (position.x, position.y);
                                s.input.events.push(InputEvent::MouseMove {
                                    x: position.x,
                                    y: position.y,
                                    timestamp,
                                });
                            }
                            WindowEvent::MouseInput {
                                state: btn_state,
                                button,
                                ..
                            } => {
                                let timestamp = s.clock.now();
                                let btn: MouseButton = button.into();
                                let (mx, my) = s.input.mouse_position;
                                match btn_state {
                                    ElementState::Pressed => {
                                        s.input.buttons_down.insert(btn);
                                        s.input.buttons_just_pressed.insert(btn);
                                        s.input.events.push(InputEvent::MouseDown {
                                            button: btn,
                                            x: mx,
                                            y: my,
                                            timestamp,
                                        });
                                    }
                                    ElementState::Released => {
                                        s.input.buttons_down.remove(&btn);
                                        s.input.events.push(InputEvent::MouseUp {
                                            button: btn,
                                            x: mx,
                                            y: my,
                                            timestamp,
                                        });
                                    }
                                }
                            }
                            WindowEvent::MouseWheel { delta, .. } => {
                                let timestamp = s.clock.now();
                                let (dx, dy) = match delta {
                                    MouseScrollDelta::LineDelta(x, y) => (x as f64, y as f64),
                                    MouseScrollDelta::PixelDelta(pos) => (pos.x, pos.y),
                                };
                                s.input.events.push(InputEvent::MouseWheel {
                                    delta_x: dx,
                                    delta_y: dy,
                                    timestamp,
                                });
                            }
                            WindowEvent::RedrawRequested => {
                                if s.minimized {
                                    return;
                                }

                                let mut render_ctx = RenderContext {
                                    state: s,
                                    config: &mut config,
                                };

                                if let Err(e) = render_fn(&mut render_ctx) {
                                    warn!("Render error: {}", e);
                                    *error_clone.borrow_mut() = Some(e);
                                    elwt.exit();
                                }

                                // Clear per-frame input state AFTER the callback runs.
                                // KeyboardInput/MouseInput events arrive before RedrawRequested
                                // in the same event loop iteration, so begin_frame() must run
                                // after the callback — not before — or it would erase those events
                                // before the callback ever sees them.
                                s.input.begin_frame();

                                // Honor a callback's request_exit(): break the loop after this
                                // frame, mirroring the buffered and direct-display paths.
                                if s.should_close {
                                    elwt.exit();
                                }
                            }
                            _ => {}
                        }
                    }
                    Event::AboutToWait => {
                        if let Some(s) = &state {
                            if let Some(w) = &s.window {
                                w.request_redraw();
                            }
                        }
                    }
                    Event::LoopExiting => {
                        if let Some(s) = &mut state {
                            if let Some(recording) = &mut s.recording {
                                recording.on_shutdown();
                            }
                        }
                    }
                    _ => {}
                }
            })
            .map_err(|e| VSEError::EventLoop(e.to_string()))?;

        // Check if any error occurred during the event loop
        if let Some(err) = error.borrow_mut().take() {
            return Err(err);
        }

        info!("VSEContext shut down cleanly");
        Ok(())
    }

    /// Run the experiment loop in buffered (pipelined) mode.
    ///
    /// Unlike [`Self::run`], which blocks on every GPU fence, `run_buffered` pipelines CPU
    /// and GPU work across frames. The callback receives two alternating event variants:
    ///
    /// - [`FlipEvent::Render`]: build and submit frame `N` via
    ///   [`flip_with_payload()`](RenderContext::flip_with_payload). Fires every vblank.
    /// - [`FlipEvent::Presented`]: GPU has confirmed frame `N - depth` was scanned out.
    ///   `flip_info.present_time` is a confirmed timestamp. Call `record_frame(payload)?`
    ///   here to record data with accurate timing.
    ///
    /// During the first `config.depth` iterations only `Render` fires (queue warming up).
    /// On clean exit, all pending `Presented` events are drained before returning.
    ///
    /// # Closed-loop experiments
    ///
    /// The B-frame latency is explicit and predictable: when `Presented` fires for frame
    /// `N`, frame `N+1` has already been submitted. Stimulus updates in `Presented` take
    /// effect from frame `N+2` onward.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::{cell::RefCell, rc::Rc};
    /// use vision_stimulus_engine::prelude::*;
    ///
    /// #[derive(serde::Serialize)]
    /// struct FrameData { trial: u32, contrast: f32 }
    ///
    /// let context = VSEContext::builder().with_window_size(800, 600).build()?;
    ///
    /// let contrast = Rc::new(RefCell::new(1.0f32));
    /// let trial    = Rc::new(RefCell::new(0u32));
    /// let c = contrast.clone();
    /// let t = trial.clone();
    ///
    /// context.run_buffered::<FrameData, _>(BufferedConfig::default(), move |event, vse| {
    ///     match event {
    ///         FlipEvent::Render => {
    ///             vse.clear()?;
    ///             // draw stimulus …
    ///             let data = FrameData { trial: *t.borrow(), contrast: *c.borrow() };
    ///             vse.flip_with_payload(None, data)?;
    ///         }
    ///         FlipEvent::Presented { flip_info, payload } => {
    ///             // Confirmed hardware timing — safe to record
    ///             vse.record_frame(payload)?;
    ///             // Closed-loop: reduce contrast on missed frames
    ///             if flip_info.missed {
    ///                 *c.borrow_mut() *= 0.9;
    ///             }
    ///         }
    ///         _ => {}
    ///     }
    ///     Ok(())
    /// })?;
    ///
    /// # Ok::<(), VSEError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Propagates any `VSEError` returned by the callback, or returns
    /// `VSEError::EventLoop` if the underlying windowing system fails.
    pub fn run_buffered<T, F>(
        mut self,
        config: BufferedConfig,
        mut callback: F,
    ) -> Result<(), VSEError>
    where
        T: std::any::Any + serde::Serialize + Send + 'static,
        F: FnMut(FlipEvent<T>, &mut RenderContext<'_>) -> Result<(), VSEError> + 'static,
    {
        use crate::core::buffered::PendingFrame;
        use std::collections::VecDeque;

        // Branch for direct display mode
        #[cfg(target_os = "linux")]
        if self.config.window_mode == WindowMode::DirectDisplay {
            return Err(VSEError::EventLoop(
                "run_buffered() does not support DirectDisplay mode".into(),
            ));
        }

        let event_loop = self
            .event_loop
            .take()
            .ok_or_else(|| VSEError::EventLoop("Event loop already consumed".into()))?;

        let mut vse_config = self.config;
        let mut session = self.session;
        let mut state: Option<VSEState> = None;

        // pending_frames lives alongside in_flight fences; same FIFO order.
        let pending_frames: Rc<RefCell<VecDeque<PendingFrame<T>>>> =
            Rc::new(RefCell::new(VecDeque::with_capacity(config.depth + 1)));

        let error: Rc<RefCell<Option<VSEError>>> = Rc::new(RefCell::new(None));
        let error_clone = error.clone();

        event_loop
            .run(move |event, elwt| {
                elwt.set_control_flow(ControlFlow::Poll);

                match event {
                    Event::Resumed => {
                        if state.is_some() {
                            return;
                        }
                        match Self::initialize_compositor(elwt, &vse_config) {
                            Ok(mut s) => {
                                s.recording = session.take().map(|sess| RecordingState {
                                    session: sess,
                                    pending_flip: None,
                                    last_claimed_frame: None,
                                });
                                s.in_buffered_mode = true;
                                let required = (config.depth + 1) as u32;
                                if let Err(e) = s.swapchain.ensure_image_count(required) {
                                    *error_clone.borrow_mut() = Some(e.into());
                                    elwt.exit();
                                    return;
                                }
                                // The raw present engine pipelines `depth + 1` frames, so its
                                // sync ring must have at least that many slots (+1 slack) or a
                                // slot's fence would be reset while its frame is still in flight.
                                if let Some(engine) = &mut s.present_engine {
                                    let slots = s.swapchain.images().len() + 1;
                                    if !engine.ensure_ring(slots) {
                                        *error_clone.borrow_mut() = Some(VSEError::Swapchain(
                                            SwapchainError::CreationFailed(
                                                "failed to grow present engine sync ring".into(),
                                            ),
                                        ));
                                        elwt.exit();
                                        return;
                                    }
                                }
                                state = Some(s);
                            }
                            Err(e) => {
                                *error_clone.borrow_mut() = Some(e);
                                elwt.exit();
                            }
                        }
                    }
                    Event::WindowEvent {
                        event: window_event,
                        ..
                    } => {
                        let s = match &mut state {
                            Some(s) => s,
                            None => return,
                        };
                        match window_event {
                            WindowEvent::CloseRequested => {
                                info!("Window close requested");
                                s.should_close = true;
                                // Do NOT call elwt.exit() yet — let RedrawRequested drain.
                            }
                            WindowEvent::Resized(new_size) => {
                                debug!("Window resized to {}x{}", new_size.width, new_size.height);
                                if new_size.width == 0 || new_size.height == 0 {
                                    s.minimized = true;
                                } else {
                                    s.minimized = false;
                                    s.display_size = (new_size.width, new_size.height);
                                    s.swapchain.mark_needs_recreation();
                                }
                            }
                            WindowEvent::KeyboardInput { event, .. } => {
                                if let PhysicalKey::Code(key_code) = event.physical_key {
                                    let timestamp = s.clock.now();
                                    let logical_key = event.logical_key.clone();
                                    match event.state {
                                        ElementState::Pressed => {
                                            let repeat = s.input.keys_down.contains(&key_code);
                                            s.input.keys_down.insert(key_code);
                                            if !repeat {
                                                s.input.keys_just_pressed.insert(key_code);
                                            }
                                            s.input.events.push(InputEvent::KeyDown {
                                                key_code,
                                                logical_key,
                                                timestamp,
                                                repeat,
                                            });
                                        }
                                        ElementState::Released => {
                                            s.input.keys_down.remove(&key_code);
                                            s.input.keys_just_released.insert(key_code);
                                            s.input.events.push(InputEvent::KeyUp {
                                                key_code,
                                                logical_key,
                                                timestamp,
                                            });
                                        }
                                    }
                                }
                            }
                            WindowEvent::CursorMoved { position, .. } => {
                                let timestamp = s.clock.now();
                                s.input.mouse_position = (position.x, position.y);
                                s.input.events.push(InputEvent::MouseMove {
                                    x: position.x,
                                    y: position.y,
                                    timestamp,
                                });
                            }
                            WindowEvent::MouseInput {
                                state: btn_state,
                                button,
                                ..
                            } => {
                                let timestamp = s.clock.now();
                                let btn: MouseButton = button.into();
                                let (mx, my) = s.input.mouse_position;
                                match btn_state {
                                    ElementState::Pressed => {
                                        s.input.buttons_down.insert(btn);
                                        s.input.buttons_just_pressed.insert(btn);
                                        s.input.events.push(InputEvent::MouseDown {
                                            button: btn,
                                            x: mx,
                                            y: my,
                                            timestamp,
                                        });
                                    }
                                    ElementState::Released => {
                                        s.input.buttons_down.remove(&btn);
                                        s.input.events.push(InputEvent::MouseUp {
                                            button: btn,
                                            x: mx,
                                            y: my,
                                            timestamp,
                                        });
                                    }
                                }
                            }
                            WindowEvent::MouseWheel { delta, .. } => {
                                let timestamp = s.clock.now();
                                let (dx, dy) = match delta {
                                    MouseScrollDelta::LineDelta(x, y) => (x as f64, y as f64),
                                    MouseScrollDelta::PixelDelta(pos) => (pos.x, pos.y),
                                };
                                s.input.events.push(InputEvent::MouseWheel {
                                    delta_x: dx,
                                    delta_y: dy,
                                    timestamp,
                                });
                            }
                            WindowEvent::RedrawRequested => {
                                if s.minimized {
                                    return;
                                }

                                // ── Phase 1: Check for confirmed presentation ──────────
                                let oldest_complete = s
                                    .buffered_in_flight
                                    .front()
                                    .map(|(_, fence)| fence.is_complete())
                                    .unwrap_or(false);

                                if oldest_complete {
                                    let (estimated_flip, fence) =
                                        s.buffered_in_flight.pop_front().unwrap();
                                    fence.wait_blocking();

                                    if let Some(pf) = pending_frames.borrow_mut().pop_front() {
                                        debug_assert_eq!(
                                            pf.frame_number, pf.estimated_flip.frame_number,
                                            "PendingFrame FIFO mismatch"
                                        );
                                        let confirmed = s.build_confirmed_flip(estimated_flip);
                                        s.buffered_confirmed_flip = Some(confirmed.clone());
                                        s.buffered_record_called_this_presented = false;

                                        let mut render_ctx = RenderContext {
                                            state: s,
                                            config: &mut vse_config,
                                        };
                                        if let Err(e) = callback(
                                            FlipEvent::Presented {
                                                flip_info: confirmed,
                                                payload: pf.payload,
                                            },
                                            &mut render_ctx,
                                        ) {
                                            *error_clone.borrow_mut() = Some(e);
                                            elwt.exit();
                                            return;
                                        }
                                        s.buffered_confirmed_flip = None;
                                    }
                                }

                                // Early exit if callback requested close during Presented
                                if s.should_close {
                                    Self::drain_buffered(
                                        s,
                                        &mut vse_config,
                                        &pending_frames,
                                        &mut callback,
                                    );
                                    if let Some(recording) = &mut s.recording {
                                        recording.on_shutdown();
                                    }
                                    s.in_buffered_mode = false;
                                    elwt.exit();
                                    return;
                                }

                                // ── Phase 2: Render ────────────────────────────────────
                                {
                                    let mut render_ctx = RenderContext {
                                        state: s,
                                        config: &mut vse_config,
                                    };
                                    if let Err(e) = callback(FlipEvent::Render, &mut render_ctx) {
                                        *error_clone.borrow_mut() = Some(e);
                                        elwt.exit();
                                        return;
                                    }

                                    // Pick up payload stored by flip_with_payload()
                                    if let Some(raw) = s.buffered_pending_payload.take() {
                                        let payload = *raw
                                            .downcast::<T>()
                                            .expect("buffered payload type mismatch");
                                        if let Some((estimated_flip, _)) =
                                            s.buffered_in_flight.back()
                                        {
                                            let ef = estimated_flip.clone();
                                            pending_frames.borrow_mut().push_back(PendingFrame {
                                                frame_number: ef.frame_number,
                                                payload,
                                                estimated_flip: ef,
                                            });
                                        }
                                    }
                                }

                                s.input.begin_frame();

                                if s.should_close {
                                    Self::drain_buffered(
                                        s,
                                        &mut vse_config,
                                        &pending_frames,
                                        &mut callback,
                                    );
                                    if let Some(recording) = &mut s.recording {
                                        recording.on_shutdown();
                                    }
                                    s.in_buffered_mode = false;
                                    elwt.exit();
                                }
                            }
                            _ => {}
                        }
                    }
                    Event::AboutToWait => {
                        if let Some(s) = &state {
                            if let Some(w) = &s.window {
                                w.request_redraw();
                            }
                        }
                    }
                    Event::LoopExiting => {
                        if let Some(s) = &mut state {
                            s.in_buffered_mode = false;
                            if let Some(recording) = &mut s.recording {
                                recording.on_shutdown();
                            }
                        }
                    }
                    _ => {}
                }
            })
            .map_err(|e| VSEError::EventLoop(e.to_string()))?;

        if let Some(err) = error.borrow_mut().take() {
            return Err(err);
        }

        info!("VSEContext (buffered) shut down cleanly");
        Ok(())
    }

    /// Drain all remaining in-flight fences and fire Presented events.
    ///
    /// Called on clean shutdown from within `run_buffered()`.
    fn drain_buffered<T, F>(
        state: &mut VSEState,
        config: &mut VSEConfig,
        pending_frames: &Rc<
            RefCell<std::collections::VecDeque<crate::core::buffered::PendingFrame<T>>>,
        >,
        callback: &mut F,
    ) where
        T: std::any::Any + serde::Serialize + Send + 'static,
        F: FnMut(FlipEvent<T>, &mut RenderContext<'_>) -> Result<(), VSEError>,
    {
        while let Some((estimated_flip, fence)) = state.buffered_in_flight.pop_front() {
            fence.wait_blocking();
            if let Some(pf) = pending_frames.borrow_mut().pop_front() {
                debug_assert_eq!(
                    pf.frame_number, pf.estimated_flip.frame_number,
                    "PendingFrame FIFO mismatch"
                );
                let confirmed = state.build_confirmed_flip(estimated_flip);
                state.buffered_confirmed_flip = Some(confirmed.clone());
                state.buffered_record_called_this_presented = false;
                let mut render_ctx = RenderContext { state, config };
                let _ = callback(
                    FlipEvent::Presented {
                        flip_info: confirmed,
                        payload: pf.payload,
                    },
                    &mut render_ctx,
                );
                state.buffered_confirmed_flip = None;
            }
        }
    }
}

/// Render context passed to the render callback
///
/// This provides access to rendering operations during the frame callback.
pub struct RenderContext<'a> {
    state: &'a mut VSEState,
    config: &'a mut VSEConfig,
}

impl<'a> RenderContext<'a> {
    /// Clear the screen with the configured clear color
    ///
    /// This records a clear command to the current frame's command buffer.
    /// The actual clear operation happens during [`flip()`](Self::flip).
    ///
    /// # Errors
    ///
    /// Returns `VSEError::Frame` if command buffer recording fails.
    pub fn clear(&mut self) -> Result<(), VSEError> {
        // Clear is handled as part of the frame in flip()
        Ok(())
    }

    /// Present the current frame to the screen
    ///
    /// Optionally accepts a target presentation time. When provided:
    /// - With `GoogleDisplayTiming`: schedules the present via the driver
    /// - With `CpuEstimate`: spin-waits until the target time
    ///
    /// Pass `None` for immediate presentation (VSync-locked).
    ///
    /// # Errors
    ///
    /// Returns `VSEError` if presentation fails.
    pub fn flip(&mut self, target_time: Option<Timestamp>) -> Result<FlipInfo, VSEError> {
        if self.state.minimized {
            let info = FlipInfo::skipped(self.state.frame_number);
            self.state.frame_number += 1;
            return Ok(info);
        }

        // Handle swapchain recreation if needed
        let (dsw, dsh) = self.state.display_size;
        let win_size_arr = [dsw, dsh];
        if self.state.swapchain.needs_recreation() {
            // Drain in-flight raw presents before the swapchain (and its images) is retired.
            if let Some(engine) = &mut self.state.present_engine {
                engine.wait_idle();
            }
            self.state.recreate_swapchain(win_size_arr)?;
        }

        // On the EXT present-timing backend, take the raw acquire/submit/present path (attaches
        // present-id + timing pNext, reads scanout feedback). The CPU-estimate path below is
        // unchanged.
        if self.state.present_engine.is_some() {
            return self.flip_ext(target_time);
        }

        // Acquire next image
        let (image_index, _suboptimal, acquire_future) =
            match self.state.swapchain.acquire_next_image() {
                Ok(result) => result,
                Err(SwapchainError::OutOfDate) => {
                    self.state.recreate_swapchain(win_size_arr)?;
                    let info = FlipInfo::skipped(self.state.frame_number);
                    self.state.frame_number += 1;
                    return Ok(info);
                }
                Err(e) => return Err(e.into()),
            };

        // Get the image to render to
        let image = self.state.swapchain.images()[image_index as usize].clone();
        let extent = self.state.swapchain.extent();

        // Record and execute drawing commands via renderer
        let command_buffer = self
            .state
            .renderer
            .render(image, self.config.clear_color, extent)?;

        let future = acquire_future
            .then_execute(self.state.queue.clone(), command_buffer)
            .map_err(|e: vulkano::command_buffer::CommandBufferExecError| {
                FrameError::ExecutionFailed(e.to_string())
            })?;

        // If target time specified, wait/schedule
        if let Some(target) = target_time {
            self.state
                .timing_provider
                .wait_for_target(target, &self.state.clock);
        }

        // --- TIMING: capture submit time ---
        let submit_time = self.state.clock.now();

        // Present (submits to GPU and waits for fence)
        match self
            .state
            .swapchain
            .present(self.state.queue.clone(), image_index, future)
        {
            Ok(()) => {}
            Err(SwapchainError::OutOfDate) => {
                // Will recreate on next frame
                let info = FlipInfo::skipped(self.state.frame_number);
                self.state.frame_number += 1;
                return Ok(info);
            }
            Err(e) => return Err(e.into()),
        }

        // --- TIMING: capture present time via provider ---
        let present_time = self
            .state
            .timing_provider
            .record_present_time(&self.state.clock);

        // Establish the scanout epoch / feed the opt-in host-clock bridge (off hot path).
        self.state.update_clocks();

        // Compute inter-frame duration
        let frame_duration = self
            .state
            .last_present_time
            .map(|prev| present_time.duration_since(prev));

        // Auto-detect refresh rate if needed
        if self.state.expected_frame_duration.is_none() {
            // First try the provider (e.g., GoogleDisplayTiming has driver info)
            if let Some(dur) = self.state.timing_provider.refresh_cycle_duration() {
                self.state.expected_frame_duration = Some(dur);
                info!(
                    "Refresh cycle duration from provider: {} us ({:.1} Hz)",
                    dur.as_micros(),
                    1_000_000.0 / dur.as_micros() as f64
                );
            } else if let Some(dur) = frame_duration {
                // Fall back to auto-detect from frame timings
                self.state.refresh_detect_samples.push(dur);
                if self.state.refresh_detect_samples.len() >= 10 {
                    let total: Duration = self.state.refresh_detect_samples.iter().copied().sum();
                    let avg = total / self.state.refresh_detect_samples.len() as u32;
                    self.state.expected_frame_duration = Some(avg);
                    info!(
                        "Auto-detected refresh rate: {:.1} Hz (frame duration: {} us)",
                        1_000_000.0 / avg.as_micros() as f64,
                        avg.as_micros()
                    );
                }
            }
        }

        let expected = self
            .state
            .expected_frame_duration
            .unwrap_or(Duration::from_micros(16_667)); // 60 Hz fallback

        // Missed frame detection
        let (missed, missed_count) = match frame_duration {
            Some(dur) => {
                let ratio = dur.as_micros() as f64 / expected.as_micros() as f64;
                if ratio > 1.5 {
                    (true, (ratio.round() as u32).saturating_sub(1))
                } else {
                    (false, 0)
                }
            }
            None => (false, 0),
        };

        // present_id is assigned by the EXT present path; 0 on the CPU-estimate path.
        let present_id: u64 = 0;

        let flip_info = FlipInfo {
            frame_number: self.state.frame_number,
            timing_source: self.state.timing_provider.source(),
            submit_time,
            present_time,
            present_id,
            target_time,
            on_target: true,
            missed,
            missed_count,
            skipped: false,
        };

        // Notify RecordingState of new flip
        if let Some(recording) = &mut self.state.recording {
            recording.on_flip(flip_info.clone());
        }

        // Update state for next frame
        self.state.last_present_time = Some(present_time);
        self.state.frame_number += 1;

        // Clear input event queue after the frame
        self.state.input.clear_events();

        Ok(flip_info)
    }

    /// Synchronous flip on the `VK_EXT_present_timing` backend: raw acquire → submit → present
    /// with a [`PresentChain`](crate::core::present_timing_ext::PresentChain) (present-id +
    /// scanout-timing request) attached to `vkQueuePresentKHR`, plus a
    /// `vkGetPastPresentationTimingEXT` feedback read.
    ///
    /// `FlipInfo.present_id` becomes the driver's real `VkPresentId2` value. `present_time` is
    /// still CPU fence time in B1 (B3 makes it a scanout-native timestamp). The CPU-estimate
    /// path stays on vulkano's present in [`flip()`](Self::flip).
    fn flip_ext(&mut self, target_time: Option<Timestamp>) -> Result<FlipInfo, VSEError> {
        use vulkano::VulkanObject;

        let clear_color = self.config.clear_color;
        let swapchain_handle = self.state.swapchain.swapchain().handle();
        let (dsw, dsh) = self.state.display_size;
        let win_size_arr = [dsw, dsh];

        // --- Acquire (raw, signals the slot's acquire semaphore) ---
        let (image_index, acquire_suboptimal, slot) = match self
            .state
            .present_engine
            .as_mut()
            .expect("flip_ext called without a present engine")
            .acquire_next(swapchain_handle)
        {
            Ok(r) => r,
            Err(ash::vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                if let Some(engine) = &mut self.state.present_engine {
                    engine.wait_idle();
                }
                self.state.recreate_swapchain(win_size_arr)?;
                let info = FlipInfo::skipped(self.state.frame_number);
                self.state.frame_number += 1;
                return Ok(info);
            }
            Err(e) => {
                return Err(SwapchainError::AcquireFailed(format!("{e:?}")).into());
            }
        };
        if acquire_suboptimal {
            self.state.swapchain.mark_needs_recreation();
        }

        // --- External frame source: release completed slots, take this frame's underlay ---
        if let Some(src) = self.state.external_source.as_mut() {
            src.pump_releases();
        }
        let ext_frame = self
            .state
            .external_source
            .as_mut()
            .and_then(|src| src.take_frame());
        let underlay = ext_frame.as_ref().map(|f| crate::drawing::renderer::ExternalUnderlay {
            image: f.image.clone(),
            readback: self.state.external_readback.take(),
        });

        // --- Render into the acquired image ---
        let image = self.state.swapchain.images()[image_index as usize].clone();
        let extent = self.state.swapchain.extent();
        let command_buffer =
            self.state
                .renderer
                .render_with_underlay(image, clear_color, extent, underlay.as_ref())?;

        // Hardware scheduling: express the target (scanout-domain µs) as an absolute scanout time
        // for `VkPresentTimingInfoEXT.targetTime`. Falls back to unscheduled when the scanout
        // epoch/domain isn't known yet (the very first flip, before `t=0` is established).
        let scheduled = target_time.and_then(|t| self.state.scheduled_target(t));

        // Software scanout-domain pacing: not every driver enforces `targetTime` (Intel/ANV/Mesa
        // 26.1 does not), so pace the present ourselves against the scanout clock. Harmless when the
        // driver *does* honor the hardware target above. Sync path only (buffered stays pipelined).
        if let Some(target) = target_time {
            self.state.note_scheduling_once();
            if let Some(refresh) = self.state.refresh_interval() {
                self.state.pace_to_scanout_target(target, refresh);
            }
        }

        let submit_time = self.state.clock.now();

        // --- Submit + raw present with the timing pNext chain ---
        let queue = self.state.queue.clone();
        let external_waits: Vec<_> = ext_frame.iter().filter_map(|f| f.wait.clone()).collect();
        let outcome = self
            .state
            .present_engine
            .as_mut()
            .expect("flip_ext called without a present engine")
            .submit_and_present(
                &queue,
                swapchain_handle,
                image_index,
                slot,
                command_buffer,
                scheduled,
                &external_waits,
            )
            .map_err(SwapchainError::PresentFailed)?;
        if outcome.suboptimal {
            self.state.swapchain.mark_needs_recreation();
        }
        // Release back-edge: the slot returns to the producer once this submit's
        // fence signals (pumped at the top of the next flip).
        if let (Some(f), Some(src)) = (ext_frame, self.state.external_source.as_mut()) {
            src.on_consumed(f.slot, outcome.fence.clone());
        }

        // Synchronous flip(): block on the render fence (GPU render done) before sampling — cheap,
        // and keeps the command buffer alive. The scanout-time wait below paces to the vblank.
        if let Some(engine) = &self.state.present_engine {
            engine.wait_frame(slot);
        }

        // Establish the scanout epoch on the first flip / feed the opt-in host-clock bridge (off
        // hot path). Must run before rebasing scanout feedback into scanout-domain time below.
        self.state.update_clocks();

        // Block until THIS frame has begun scanout, so its IMAGE_FIRST_PIXEL_OUT feedback record
        // has landed (feedback lags the present by ~1 vblank — that lag is why the sync path needs
        // present-wait2). Only legal on a present-wait2 swapchain, so gate on that; falls back to
        // CPU fence time when present-wait2 is unavailable or the wait times out.
        const SCANOUT_WAIT_NS: u64 = 250_000_000; // 250 ms safety cap (≫ one vblank)
        let waited = self.state.swapchain.present_wait2_enabled()
            && self
                .state
                .timing_provider
                .wait_for_present(outcome.present_id, SCANOUT_WAIT_NS);

        // Drain confirmed scanout records ONCE (destructive dequeue) and cache them: populates
        // `scanout_by_present_id` (present-id keyed) and `recent_scanouts` (for `scanout_feedback()`).
        let feedback = self.state.timing_provider.query_scanouts();
        self.state.ingest_scanout_feedback(feedback);
        if let Some(last) = self.state.recent_scanouts.last() {
            debug!(
                "scanout feedback: {} record(s); latest present_id={} first_pixel_out={:?} domain={}",
                self.state.recent_scanouts.len(),
                last.present_id,
                last.first_pixel_out_ns,
                last.time_domain
            );
        }

        // present_time is THIS frame's real hardware scanout time — the scientifically meaningful
        // "photons started" timestamp, in the scanout domain. Source order:
        //   1. the driver's `IMAGE_FIRST_PIXEL_OUT` feedback for this present, when the driver fills
        //      it (a true per-present scanout time);
        //   2. else — since `wait_for_present` just blocked until this frame began scanout — the
        //      calibrated present-stage-local clock sampled now (Intel/ANV/Mesa 26.1 stubs the
        //      feedback stage times to 0, so this is the real scanout source on that driver);
        //   3. else CPU fence-signal time (windowed with no scanout, no present-wait2, or before
        //      the scanout epoch is established).
        let scanout_present = self
            .state
            .take_scanout_for(outcome.present_id)
            .and_then(|fb| fb.first_pixel_out_ns)
            .and_then(|ns| self.state.scanout_present_time(ns))
            .or_else(|| waited.then(|| self.state.sample_scanout_now()).flatten());
        let present_time = scanout_present.unwrap_or_else(|| {
            self.state
                .timing_provider
                .record_present_time(&self.state.clock)
        });

        // on_target is only knowable against a real scanout time (same domain as the target). When
        // there is one, the scheduled present met its deadline iff scanout landed at/after target;
        // without one (windowed) we cannot verify, so report `true`.
        let on_target = match (target_time, scanout_present) {
            (Some(target), Some(sp)) => sp.as_micros() >= target.as_micros(),
            _ => true,
        };

        // --- Shared bottom half: refresh detect, missed-frame detection, FlipInfo assembly ---
        let frame_duration = self
            .state
            .last_present_time
            .map(|prev| present_time.duration_since(prev));

        if self.state.expected_frame_duration.is_none() {
            if let Some(dur) = self.state.timing_provider.refresh_cycle_duration() {
                self.state.expected_frame_duration = Some(dur);
                info!(
                    "Refresh cycle duration from provider: {} us ({:.1} Hz)",
                    dur.as_micros(),
                    1_000_000.0 / dur.as_micros() as f64
                );
            } else if let Some(dur) = frame_duration {
                self.state.refresh_detect_samples.push(dur);
                if self.state.refresh_detect_samples.len() >= 10 {
                    let total: Duration = self.state.refresh_detect_samples.iter().copied().sum();
                    let avg = total / self.state.refresh_detect_samples.len() as u32;
                    self.state.expected_frame_duration = Some(avg);
                }
            }
        }

        let expected = self
            .state
            .expected_frame_duration
            .unwrap_or(Duration::from_micros(16_667));

        let (missed, missed_count) = match frame_duration {
            Some(dur) => {
                let ratio = dur.as_micros() as f64 / expected.as_micros() as f64;
                if ratio > 1.5 {
                    (true, (ratio.round() as u32).saturating_sub(1))
                } else {
                    (false, 0)
                }
            }
            None => (false, 0),
        };

        let flip_info = FlipInfo {
            frame_number: self.state.frame_number,
            timing_source: self.state.timing_provider.source(),
            submit_time,
            present_time,
            present_id: outcome.present_id,
            target_time,
            on_target,
            missed,
            missed_count,
            skipped: false,
        };

        if let Some(recording) = &mut self.state.recording {
            recording.on_flip(flip_info.clone());
        }

        self.state.last_present_time = Some(present_time);
        self.state.frame_number += 1;
        self.state.input.clear_events();

        Ok(flip_info)
    }

    /// Submit the current frame to the GPU without blocking, attaching a typed payload.
    ///
    /// Only valid inside the [`FlipEvent::Render`] arm of [`VSEContext::run_buffered`].
    /// The `payload` is stored and delivered alongside the confirmed [`FlipInfo`] in
    /// the next [`FlipEvent::Presented`] for this frame.
    ///
    /// After this call returns, the GPU is processing frame `N` while the CPU is free
    /// to compute frame `N+1` on the next vblank.
    ///
    /// The `target_time` argument optionally schedules the present for a specific
    /// [`Timestamp`]. Pass `None` for immediate VSync-locked presentation.
    ///
    /// Call this method exactly **once** per `Render` event. Calling it multiple times
    /// or not at all results in queue desynchronisation.
    ///
    /// # Errors
    ///
    /// - [`VSEError::NotInBufferedMode`] if called from `run()` instead of `run_buffered()`.
    /// - [`VSEError::Swapchain`] if image acquisition or submission fails.
    pub fn flip_with_payload<T: std::any::Any + Send + 'static>(
        &mut self,
        target_time: Option<Timestamp>,
        payload: T,
    ) -> Result<(), VSEError> {
        if !self.state.in_buffered_mode {
            return Err(VSEError::NotInBufferedMode);
        }

        if self.state.minimized {
            // Skip silently — no fence, no payload stored; run_buffered() skips push.
            self.state.frame_number += 1;
            return Ok(());
        }

        // On the EXT backend, submit through the raw present engine (present-id + timing chain).
        if self.state.present_engine.is_some() {
            return self.flip_with_payload_ext(target_time, payload);
        }

        // Recreate swapchain if needed
        let (dsw, dsh) = self.state.display_size;
        let win_size_arr = [dsw, dsh];
        if self.state.swapchain.needs_recreation() {
            self.state.recreate_swapchain(win_size_arr)?;
        }

        // Acquire next image (natural backpressure from driver)
        let (image_index, _suboptimal, acquire_future) =
            match self.state.swapchain.acquire_next_image() {
                Ok(r) => r,
                Err(SwapchainError::OutOfDate) => {
                    self.state.recreate_swapchain(win_size_arr)?;
                    self.state.frame_number += 1;
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            };

        let image = self.state.swapchain.images()[image_index as usize].clone();
        let extent = self.state.swapchain.extent();

        let command_buffer = self
            .state
            .renderer
            .render(image, self.config.clear_color, extent)?;

        let future = acquire_future
            .then_execute(self.state.queue.clone(), command_buffer)
            .map_err(|e: vulkano::command_buffer::CommandBufferExecError| {
                FrameError::ExecutionFailed(e.to_string())
            })?;

        // Optional CPU spin-wait for scheduled present time
        if let Some(target) = target_time {
            self.state
                .timing_provider
                .wait_for_target(target, &self.state.clock);
        }

        let submit_time = self.state.clock.now();

        // Non-blocking submit — returns immediately, keeps fence alive
        let in_flight = self.state.swapchain.submit_nonblocking(
            self.state.queue.clone(),
            image_index,
            future,
        )?;

        let estimated_present = self.state.clock.now();

        // Establish the scanout epoch / feed the opt-in host-clock bridge (off hot path).
        self.state.update_clocks();

        // present_id is assigned by the EXT present path; 0 on the CPU-estimate path.
        let present_id: u64 = 0;

        let estimated_flip = FlipInfo {
            frame_number: self.state.frame_number,
            timing_source: self.state.timing_provider.source(),
            submit_time,
            present_time: estimated_present,
            present_id,
            target_time,
            on_target: true,
            missed: false,
            missed_count: 0,
            skipped: false,
        };

        // Store payload for run_buffered() to pick up after callback returns
        self.state.buffered_pending_payload = Some(Box::new(payload));

        // Store (estimated_flip, fence) — correlated with pending_frames by FIFO order
        self.state
            .buffered_in_flight
            .push_back((estimated_flip, in_flight));

        self.state.frame_number += 1;
        self.state.input.clear_events();

        Ok(())
    }

    /// Buffered (non-blocking) flip on the `VK_EXT_present_timing` backend.
    ///
    /// Mirrors [`flip_with_payload`](Self::flip_with_payload) but drives the raw acquire → submit →
    /// present engine: the present carries a real `VkPresentId2` + timing chain, and the frame's
    /// slot fence is wrapped as the in-flight future `run_buffered()` polls. Unlike
    /// [`flip_ext`](Self::flip_ext) it does **not** block on the fence — that is the point of the
    /// buffered pipeline. Scanout feedback is drained once here (the driver read is destructive)
    /// and cached for present-id-keyed confirmation.
    fn flip_with_payload_ext<T: std::any::Any + Send + 'static>(
        &mut self,
        target_time: Option<Timestamp>,
        payload: T,
    ) -> Result<(), VSEError> {
        use super::present_engine::EngineInFlight;
        use vulkano::VulkanObject;

        let clear_color = self.config.clear_color;
        let swapchain_handle = self.state.swapchain.swapchain().handle();
        let (dsw, dsh) = self.state.display_size;
        let win_size_arr = [dsw, dsh];

        if self.state.swapchain.needs_recreation() {
            if let Some(engine) = &mut self.state.present_engine {
                engine.wait_idle();
            }
            self.state.recreate_swapchain(win_size_arr)?;
        }

        // --- Acquire (raw) ---
        let (image_index, acquire_suboptimal, slot) = match self
            .state
            .present_engine
            .as_mut()
            .expect("flip_with_payload_ext called without a present engine")
            .acquire_next(swapchain_handle)
        {
            Ok(r) => r,
            Err(ash::vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                if let Some(engine) = &mut self.state.present_engine {
                    engine.wait_idle();
                }
                self.state.recreate_swapchain(win_size_arr)?;
                self.state.frame_number += 1;
                return Ok(());
            }
            Err(e) => return Err(SwapchainError::AcquireFailed(format!("{e:?}")).into()),
        };
        if acquire_suboptimal {
            self.state.swapchain.mark_needs_recreation();
        }

        // --- External frame source: release completed slots, take this frame's underlay ---
        if let Some(src) = self.state.external_source.as_mut() {
            src.pump_releases();
        }
        let ext_frame = self
            .state
            .external_source
            .as_mut()
            .and_then(|src| src.take_frame());
        let underlay = ext_frame.as_ref().map(|f| crate::drawing::renderer::ExternalUnderlay {
            image: f.image.clone(),
            readback: self.state.external_readback.take(),
        });

        // --- Render ---
        let image = self.state.swapchain.images()[image_index as usize].clone();
        let extent = self.state.swapchain.extent();
        let command_buffer =
            self.state
                .renderer
                .render_with_underlay(image, clear_color, extent, underlay.as_ref())?;

        // Hardware scheduling (no CPU spin): express the target as an absolute scanout time for
        // the driver. Falls back to unscheduled before the scanout epoch/domain is known.
        let scheduled = target_time.and_then(|t| self.state.scheduled_target(t));

        let submit_time = self.state.clock.now();

        // --- Submit + raw present (non-blocking) ---
        let queue = self.state.queue.clone();
        let external_waits: Vec<_> = ext_frame.iter().filter_map(|f| f.wait.clone()).collect();
        let outcome = self
            .state
            .present_engine
            .as_mut()
            .expect("flip_with_payload_ext called without a present engine")
            .submit_and_present(
                &queue,
                swapchain_handle,
                image_index,
                slot,
                command_buffer,
                scheduled,
                &external_waits,
            )
            .map_err(SwapchainError::PresentFailed)?;
        if outcome.suboptimal {
            self.state.swapchain.mark_needs_recreation();
        }
        // Release back-edge: the slot returns to the producer once this submit's
        // fence signals (pumped at the top of the next flip).
        if let (Some(f), Some(src)) = (&ext_frame, self.state.external_source.as_mut()) {
            src.on_consumed(f.slot, outcome.fence.clone());
        }

        let estimated_present = self.state.clock.now();
        self.state.update_clocks();

        // Drain confirmed scanout records once (destructive read) and cache them for
        // present-id-keyed confirmation in `build_confirmed_flip`.
        let feedback = self.state.timing_provider.query_scanouts();
        self.state.ingest_scanout_feedback(feedback);

        let in_flight: Box<dyn crate::core::buffered::InFlightFuture> =
            Box::new(EngineInFlight::new(outcome.fence));

        let estimated_flip = FlipInfo {
            frame_number: self.state.frame_number,
            timing_source: self.state.timing_provider.source(),
            submit_time,
            present_time: estimated_present,
            present_id: outcome.present_id,
            target_time,
            on_target: true,
            missed: false,
            missed_count: 0,
            skipped: false,
        };

        self.state.buffered_pending_payload = Some(Box::new(payload));
        self.state
            .buffered_in_flight
            .push_back((estimated_flip, in_flight));

        self.state.frame_number += 1;
        self.state.input.clear_events();

        Ok(())
    }

    /// Request a clean exit at the end of the current frame.
    ///
    /// Alias for [`request_exit()`](Self::request_exit).
    pub fn close(&mut self) {
        self.state.should_close = true;
    }

    /// Record per-frame experimental data merged with the most recent flip's timing.
    ///
    /// In synchronous `run()`: call after `flip()`. Uses the confirmed present time
    /// from the blocking fence wait.
    ///
    /// In `run_buffered()`: call inside `FlipEvent::Presented`. Uses the confirmed
    /// hardware scanout timestamp delivered to that arm — never an estimate.
    ///
    /// The data struct must implement `serde::Serialize`. Multiple calls per frame
    /// are allowed — each produces one row keyed to the same `frame_number`.
    ///
    /// # Errors
    ///
    /// - [`VSEError::NoSession`] if no session was attached to the builder.
    /// - [`VSEError::NoFlipPending`] if called before `flip()` in synchronous mode.
    /// - [`VSEError::NoConfirmedFlip`] if called in the `FlipEvent::Render` arm.
    pub fn record_frame<F: serde::Serialize>(&mut self, data: F) -> Result<(), VSEError> {
        // Buffered mode: use the confirmed flip set by run_buffered() before this callback.
        if self.state.in_buffered_mode {
            let flip = self
                .state
                .buffered_confirmed_flip
                .clone()
                .ok_or(VSEError::NoConfirmedFlip)?;

            let recording = self.state.recording.as_mut().ok_or(VSEError::NoSession)?;

            let payload =
                serde_json::to_vec(&data).map_err(|e| VSEError::DataRecording(e.to_string()))?;

            recording
                .session
                .send_frame(FrameMessage {
                    flip,
                    payload: Some(payload),
                    schema_name: std::any::type_name::<F>(),
                })
                .map_err(|e| VSEError::DataRecording(e.to_string()))?;

            self.state.buffered_record_called_this_presented = true;
            return Ok(());
        }

        // Synchronous mode: use pending_flip from the most recent flip().
        let recording = self.state.recording.as_mut().ok_or(VSEError::NoSession)?;
        let flip = recording
            .pending_flip
            .clone()
            .ok_or(VSEError::NoFlipPending)?;

        recording.last_claimed_frame = Some(flip.frame_number);

        let payload =
            serde_json::to_vec(&data).map_err(|e| VSEError::DataRecording(e.to_string()))?;

        recording
            .session
            .send_frame(FrameMessage {
                flip,
                payload: Some(payload),
                schema_name: std::any::type_name::<F>(),
            })
            .map_err(|e| VSEError::DataRecording(e.to_string()))?;

        Ok(())
    }

    /// Record a typed annotation at the current timestamp.
    ///
    /// `stream` is the table/group name in the output file (e.g. `"trial"`,
    /// `"subject_info"`, `"calibration"`). Any `serde::Serialize` type is accepted.
    pub fn record_annotation<A: serde::Serialize>(
        &mut self,
        stream: &str,
        data: A,
    ) -> Result<(), VSEError> {
        let recording = self.state.recording.as_mut().ok_or(VSEError::NoSession)?;
        let timestamp = self.state.clock.now();
        let payload =
            serde_json::to_vec(&data).map_err(|e| VSEError::DataRecording(e.to_string()))?;
        recording
            .session
            .send_annotation(crate::data::messages::AnnotationMessage {
                stream: stream.to_string(),
                timestamp,
                payload,
            })
            .map_err(|e| VSEError::DataRecording(e.to_string()))?;
        Ok(())
    }

    /// Record a raw key-value event at the current timestamp.
    ///
    /// Use for unstructured or one-off data. For structured, repeated data
    /// prefer [`Self::record_frame`] or [`Self::record_annotation`].
    pub fn record_event(&mut self, name: &str, value: &str) -> Result<(), VSEError> {
        let recording = self.state.recording.as_mut().ok_or(VSEError::NoSession)?;
        let timestamp = self.state.clock.now();
        recording
            .session
            .send_event(crate::data::messages::EventMessage {
                name: name.to_string(),
                timestamp,
                value: value.to_string(),
            })
            .map_err(|e| VSEError::DataRecording(e.to_string()))?;
        Ok(())
    }

    /// Check if the window should close
    pub fn should_close(&self) -> bool {
        self.state.should_close
    }

    /// Request a clean exit at the end of the current frame.
    ///
    /// Sets the internal close flag.  The loop will break after the current
    /// callback returns (including any pending `flip()`), allowing all Vulkan
    /// resources to be released cleanly.
    pub fn request_exit(&mut self) {
        self.state.should_close = true;
    }

    /// Set the clear color (RGBA, 0.0-1.0 range)
    pub fn set_clear_color(&mut self, r: f32, g: f32, b: f32, a: f32) {
        self.config.clear_color = [r, g, b, a];
    }

    /// Get the current clear color
    pub fn clear_color(&self) -> [f32; 4] {
        self.config.clear_color
    }

    /// Get the window dimensions in physical pixels.
    ///
    /// In fullscreen modes this returns the monitor's native resolution.
    pub fn window_size(&self) -> (u32, u32) {
        self.state.display_size
    }

    /// Get the device (for advanced users)
    pub fn device(&self) -> &Arc<vulkano::device::Device> {
        &self.state.device
    }

    /// Get the queue (for advanced users)
    pub fn queue(&self) -> &Arc<vulkano::device::Queue> {
        &self.state.queue
    }

    /// Get the swapchain manager (for advanced users)
    pub fn swapchain(&self) -> &SwapchainManager {
        &self.state.swapchain
    }

    /// Get the GPU name
    pub fn gpu_name(&self) -> &str {
        self.state.device_selector.device_name()
    }

    /// Get the active timing source.
    pub fn timing_source(&self) -> TimingSource {
        self.state.timing_provider.source()
    }

    /// Get the timing clock (for correlating with external events).
    pub fn clock(&self) -> &Clock {
        &self.state.clock
    }

    /// Get the current frame number (before the next flip).
    /// Attach an external-renderer frame source (see `core::external_frame`).
    ///
    /// Imports the producer's exported image ring + ready semaphores onto VSE's
    /// device. Subsequent flips consume queued external frames as a full-screen
    /// underlay beneath VSE's own draw commands; VSE remains sole present
    /// authority and `FlipInfo` is computed exactly as without a source.
    ///
    /// Requires the `ExtPresentTiming` backend (the CPU-estimate path has no
    /// seam for cross-device semaphore waits).
    ///
    /// `release_tx` is the consumer→producer slot-release back-edge: pass the
    /// sender half of [`vse_external_frame::release_channel`] and give the
    /// receiver to the producer.
    pub fn attach_external_frame_source(
        &mut self,
        desc: vse_external_frame::ExternalRingDesc,
        release_tx: vse_external_frame::SlotReleaseTx,
    ) -> Result<(), VSEError> {
        use crate::core::external_frame::{ExternalFrameError, ExternalFrameRing};
        if self.state.present_engine.is_none() {
            return Err(ExternalFrameError::Unsupported(
                "external frame sources require the ExtPresentTiming backend \
                 (CPU-estimate timing path active)"
                    .into(),
            )
            .into());
        }
        if self.state.external_source.is_some() {
            return Err(ExternalFrameError::Unsupported(
                "an external frame source is already attached".into(),
            )
            .into());
        }
        let ring =
            ExternalFrameRing::import(&self.state.device, &self.state.queue, desc, release_tx)?;
        tracing::info!(
            "external frame source attached: {} slots, {:?}, {:?}, {}x{}",
            ring.ring_len(),
            ring.format(),
            ring.sync(),
            ring.extent()[0],
            ring.extent()[1],
        );
        self.state.external_source = Some(ring);
        Ok(())
    }

    /// Queue the external frame in `slot` for consumption by the next flip.
    ///
    /// Call after the producer finishes rendering into `slot` (and signals its
    /// ready semaphore), before `flip`/`flip_with_payload`. Slots must be
    /// queued in the order the producer acquired them.
    pub fn queue_external_frame(
        &mut self,
        slot: vse_external_frame::SlotIndex,
    ) -> Result<(), VSEError> {
        let src = self.state.external_source.as_mut().ok_or_else(|| {
            crate::core::external_frame::ExternalFrameError::Unsupported(
                "no external frame source attached".into(),
            )
        })?;
        src.note_ready(slot)?;
        Ok(())
    }

    /// Arm a one-shot readback of the next consumed external frame into
    /// `buffer` (determinism-harness hook). The copy is recorded in the same
    /// command buffer as the underlay consumption; the buffer is safe to read
    /// once that flip is confirmed (fence signaled / `Presented` delivered).
    pub fn arm_external_readback(&mut self, buffer: vulkano::buffer::Subbuffer<[u8]>) {
        self.state.external_readback = Some(buffer);
    }

    pub fn frame_number(&self) -> u64 {
        self.state.frame_number
    }

    /// Sample the display's `PRESENT_STAGE_LOCAL` scanout clock against `CLOCK_MONOTONIC`.
    ///
    /// Returns `None` on the CPU-estimate path or before the present-stage time domain has been
    /// probed. Used to characterize the clock offset and relative drift that the present-timing
    /// calibration must correct. See `docs/clock-synchronization.md`.
    pub fn sample_present_calibration(&self) -> Option<crate::timing::CalibrationSample> {
        self.state.timing_provider.sample_present_calibration()
    }

    /// Read back confirmed per-present scanout timings from the driver's past-timing ring
    /// (`vkGetPastPresentationTimingEXT`).
    ///
    /// Each [`ScanoutFeedback`](crate::core::ScanoutFeedback) carries the correlating `present_id`
    /// (matching [`FlipInfo::present_id`]) and the `IMAGE_FIRST_PIXEL_OUT` scanout time in the
    /// driver's present-stage-local domain. Empty on the CPU-estimate path, and for a frame or two
    /// after a present while the driver has not yet recorded it. Rebasing these to a
    /// [`ScanoutTimestamp`](crate::timing::ScanoutTimestamp) is B3's job.
    ///
    /// Returns the records drained on the most recent `flip()`. The driver's read is *destructive*
    /// (each record is dequeued once), so `flip()` drains once per frame and caches the result
    /// here — this accessor never re-drains, and calling it repeatedly returns the same records.
    pub fn scanout_feedback(&self) -> Vec<crate::core::ScanoutFeedback> {
        self.state.recent_scanouts.clone()
    }

    /// Read the current scanout-clock time — VSE's primary experimental clock.
    ///
    /// Returns time since the session's scanout epoch (`t=0`, established on the first flip).
    /// `None` on the CPU-estimate path, or before the first flip has established the epoch.
    pub fn scanout_now(&self) -> Option<ScanoutTimestamp> {
        let clock = self.state.scanout_clock?;
        let sample = self.state.timing_provider.sample_present_calibration()?;
        Some(clock.rebase(sample.stage_ns))
    }

    /// Convert a host-clock [`Timestamp`] (e.g. a key-press or network-event time) into scanout
    /// time, using the opt-in host-clock bridge.
    ///
    /// Returns `None` unless the bridge is enabled ([`VSEContextBuilder::with_host_clock_bridge`]),
    /// warmed up, and the scanout epoch is established. This is the intended way to place
    /// host-originated events on the scanout timeline.
    pub fn host_to_scanout(&self, ts: Timestamp) -> Option<ScanoutTimestamp> {
        let clock = self.state.scanout_clock?;
        let bridge = self.state.host_bridge.as_ref()?;
        let mono_ns = self.state.clock.to_monotonic_nanos(ts)?;
        let stage_ns = bridge.host_to_scanout_ns(mono_ns)?;
        Some(clock.rebase(stage_ns))
    }

    /// Convert a scanout timestamp back into a host-clock [`Timestamp`], using the opt-in bridge.
    ///
    /// Inverse of [`host_to_scanout`](Self::host_to_scanout); same availability conditions.
    pub fn scanout_to_host(&self, ts: ScanoutTimestamp) -> Option<Timestamp> {
        let clock = self.state.scanout_clock?;
        let bridge = self.state.host_bridge.as_ref()?;
        let stage_ns = clock.epoch_stage_ns().saturating_add(ts.as_nanos());
        let mono_ns = bridge.scanout_to_host_ns(stage_ns)?;
        self.state.clock.from_monotonic_nanos(mono_ns)
    }

    /// The host-clock bridge's currently fitted relative drift, in ppm (diagnostic).
    ///
    /// `None` unless the bridge is enabled and warmed up.
    pub fn host_clock_bridge_drift_ppm(&self) -> Option<f64> {
        self.state.host_bridge.as_ref()?.drift_ppm()
    }

    // === Drawing primitives ===

    /// Draw a filled rectangle.
    ///
    /// Coordinates are in pixels with (0, 0) at the top-left of the window.
    pub fn draw_rect(&mut self, left: f32, top: f32, right: f32, bottom: f32, color: Color) {
        self.state.renderer.push(DrawCommand::Rect {
            left,
            top,
            right,
            bottom,
            color,
        });
    }

    /// Draw a filled circle.
    pub fn draw_circle(&mut self, cx: f32, cy: f32, radius: f32, color: Color) {
        let segments = default_circle_segments(radius);
        self.state.renderer.push(DrawCommand::Circle {
            cx,
            cy,
            radius,
            color,
            segments,
        });
    }

    /// Draw a line.
    pub fn draw_line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, width: f32, color: Color) {
        self.state.renderer.push(DrawCommand::Line {
            x1,
            y1,
            x2,
            y2,
            width,
            color,
        });
    }

    /// Draw a texture at the specified rectangle.
    pub fn draw_texture(
        &mut self,
        texture: TextureHandle,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    ) {
        self.state.renderer.push(DrawCommand::Texture {
            texture_id: texture.id,
            left,
            top,
            right,
            bottom,
        });
    }

    /// Set the clear color using a Color value.
    pub fn set_clear(&mut self, color: Color) {
        self.config.clear_color = color.to_array();
    }

    // === Texture management ===

    /// Load a texture from a file.
    pub fn load_image(&mut self, path: impl AsRef<Path>) -> Result<TextureHandle, VSEError> {
        Ok(self.state.renderer.load_image(path)?)
    }

    /// Create a texture from raw RGBA pixel data.
    pub fn load_texture_rgba(
        &mut self,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Result<TextureHandle, VSEError> {
        Ok(self.state.renderer.load_texture_rgba(width, height, data)?)
    }

    /// Create a Gabor patch texture from parameters.
    pub fn create_gabor(&mut self, params: &GaborParams) -> Result<TextureHandle, VSEError> {
        let pixels = params.generate();
        Ok(self
            .state
            .renderer
            .load_texture_rgba(params.size, params.size, &pixels)?)
    }

    /// Unload a texture and free its GPU resources.
    pub fn unload_texture(&mut self, handle: TextureHandle) {
        self.state.renderer.unload_texture(handle);
    }

    // === Advanced stimuli ===

    /// Draw a sinusoidal or square-wave grating.
    ///
    /// The grating fills the rectangle defined by (left, top, right, bottom)
    /// in pixel coordinates. Parameters control spatial frequency, orientation,
    /// phase, contrast, and waveform type.
    pub fn draw_grating(
        &mut self,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        params: &GratingParams,
    ) {
        self.state.renderer.push(DrawCommand::Grating {
            left,
            top,
            right,
            bottom,
            params: params.clone(),
        });
    }

    /// Draw a Gabor patch (grating windowed by a Gaussian envelope).
    ///
    /// Unlike `create_gabor()` which generates a CPU texture, this computes
    /// the Gabor mathematically on the GPU each frame, allowing real-time
    /// parameter animation.
    pub fn draw_gabor_shader(
        &mut self,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        params: &GaborParams,
    ) {
        self.state.renderer.push(DrawCommand::Gabor {
            left,
            top,
            right,
            bottom,
            params: params.clone(),
        });
    }

    /// Draw a noise pattern.
    ///
    /// Generates a noise texture on CPU from the given parameters and
    /// displays it in the specified rectangle. For animated noise, change
    /// `params.seed` each frame.
    pub fn draw_noise(
        &mut self,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        params: &NoiseParams,
    ) -> Result<(), VSEError> {
        let pixels = crate::drawing::noise::generate_noise(params);
        let handle = self
            .state
            .renderer
            .load_texture_rgba(params.width, params.height, &pixels)?;
        self.state.renderer.push(DrawCommand::Noise {
            left,
            top,
            right,
            bottom,
            texture_id: handle.id,
        });
        Ok(())
    }

    /// Draw filled circular dots at the specified positions.
    ///
    /// This is the rendering primitive for Random Dot Kinematograms.
    /// Positions are in pixel coordinates. Each dot is rendered as a
    /// filled circle with an anti-aliased edge.
    pub fn draw_dots(&mut self, positions: &[(f32, f32)], radius: f32, color: Color) {
        if positions.is_empty() {
            return;
        }
        self.state.renderer.push(DrawCommand::Dots {
            positions: positions.iter().map(|&(x, y)| [x, y]).collect(),
            radius,
            color,
        });
    }

    /// Capture a snapshot of the full host machine state.
    ///
    /// Returns a [`HostInfo`](crate::host::HostInfo) struct containing OS, CPU, memory, GPU,
    /// display, swapchain, pipeline config, build metadata, runtime
    /// environment, and EDID monitor data.
    ///
    /// This is an on-demand operation — call it when you need a snapshot.
    /// The EDID capture shells out to `xrandr`, which may take ~50ms.
    pub fn capture_host_info(&self) -> crate::host::HostInfo {
        crate::host::capture::capture_host_info(
            self.state.device_selector.physical_device(),
            &self.state.device,
            self.state.window.as_deref(),
            &self.state.swapchain,
            self.config,
            crate::host::capture::ObservedPresentTiming {
                scanout_feedback_populated: self.state.scanout_feedback_populated,
                // Enforcement is not auto-probed (it disrupts frames); `None` here. The
                // direct-display characterization example measures and reports it.
                absolute_scheduling_enforced: None,
                queue_global_priority: self.state.ext_features.map(|f| f.queue_priority),
            },
        )
    }

    /// Whether the driver was observed to actually populate `IMAGE_FIRST_PIXEL_OUT` in
    /// present-timing feedback, as opposed to merely advertising `VK_EXT_present_timing`.
    ///
    /// `Some(true)` — real per-present scanout timestamps; `Some(false)` — feedback correlates by
    /// present_id but carries zero-valued stage times (VSE falls back to sampling the calibrated
    /// scanout clock; measured on Intel/ANV/Mesa 26.1); `None` — not yet determined (too few flips,
    /// or the CPU-estimate backend). A guardrail against trusting an advertised-but-unimplemented
    /// feature. See `docs/clock-synchronization.md`.
    pub fn scanout_feedback_populated(&self) -> Option<bool> {
        self.state.scanout_feedback_populated
    }

    // === Input polling (frame-aligned) ===

    /// Returns `true` if the key is currently held down.
    pub fn key_pressed(&self, key: KeyCode) -> bool {
        self.state.input.keys_down.contains(&key)
    }

    /// Returns `true` if the key was pressed this frame (not held from previous frame).
    pub fn key_just_pressed(&self, key: KeyCode) -> bool {
        self.state.input.keys_just_pressed.contains(&key)
    }

    /// Returns `true` if the key was released this frame.
    pub fn key_just_released(&self, key: KeyCode) -> bool {
        self.state.input.keys_just_released.contains(&key)
    }

    /// Get the current mouse position in window-relative pixels.
    pub fn mouse_position(&self) -> (f64, f64) {
        self.state.input.mouse_position
    }

    /// Returns `true` if the mouse button is currently held down.
    pub fn mouse_button_pressed(&self, button: MouseButton) -> bool {
        self.state.input.buttons_down.contains(&button)
    }

    /// Returns `true` if the mouse button was pressed this frame.
    pub fn mouse_button_just_pressed(&self, button: MouseButton) -> bool {
        self.state.input.buttons_just_pressed.contains(&button)
    }

    // === Event queue (timing-precise) ===

    /// Get all input events since the last `flip()`.
    ///
    /// Each event carries a precise timestamp from the VSE `Clock`,
    /// suitable for reaction-time measurement relative to `FlipInfo` timestamps.
    pub fn input_events(&self) -> &[InputEvent] {
        &self.state.input.events
    }

    // === Cursor control ===

    /// Set whether the mouse cursor is visible.
    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.state.cursor_visible = visible;
        if let Some(w) = &self.state.window {
            w.set_cursor_visible(visible);
        }
    }

    /// Move the cursor to the specified position (logical pixels).
    pub fn set_cursor_position(&self, x: f64, y: f64) {
        if let Some(w) = &self.state.window {
            let _ = w.set_cursor_position(LogicalPosition::new(x, y));
        }
    }

    /// Returns whether the cursor is currently visible.
    pub fn cursor_visible(&self) -> bool {
        self.state.cursor_visible
    }

    // === Display backend detection ===

    /// Detect the display backend (windowing system) used for this session.
    ///
    /// Derived from the raw window handle type. Use this to warn users when
    /// running under X11/XWayland, which has higher timing jitter than native
    /// Wayland or direct display mode.
    ///
    /// # Example
    /// ```no_run
    /// # use vision_stimulus_engine::prelude::*;
    /// # fn example(vse: &mut RenderContext) {
    /// let backend = vse.display_backend();
    /// if backend.has_compositor() {
    ///     println!("Warning: frames pass through {}", backend.description());
    /// }
    /// # }
    /// ```
    pub fn display_backend(&self) -> DisplayBackend {
        // Direct display mode: no window, check the stored acquisition method
        if let Some(method) = self.state.acquired_display {
            return DisplayBackend::DirectDisplay { method };
        }

        // Compositor mode: detect from raw window handle
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Some(window) = &self.state.window {
            return match window.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Wayland(_)) => DisplayBackend::Wayland,
                Ok(RawWindowHandle::Xcb(_)) | Ok(RawWindowHandle::Xlib(_)) => DisplayBackend::X11,
                Ok(RawWindowHandle::Win32(_)) => DisplayBackend::Windows,
                Ok(RawWindowHandle::AppKit(_)) => DisplayBackend::MacOS,
                _ => DisplayBackend::Unknown,
            };
        }

        DisplayBackend::Unknown
    }

    // === Monitor & video mode queries ===

    /// Get information about all available monitors.
    ///
    /// Duplicates are filtered: some Wayland compositors advertise the same physical
    /// output via multiple `wl_output` globals. Monitors are considered identical if
    /// they share the same name, resolution, and desktop position.
    pub fn available_monitors(&self) -> Vec<MonitorInfo> {
        let window = match &self.state.window {
            Some(w) => w,
            None => return vec![],
        };
        let mut seen = std::collections::HashSet::new();
        window
            .available_monitors()
            .filter(|handle| {
                let pos = handle.position();
                let size = handle.size();
                let key = (handle.name(), size.width, size.height, pos.x, pos.y);
                seen.insert(key)
            })
            .enumerate()
            .map(|(i, handle)| monitor_handle_to_info(i, &handle))
            .collect()
    }

    /// Get information about the primary monitor, if available.
    pub fn primary_monitor(&self) -> Option<MonitorInfo> {
        self.state
            .window
            .as_ref()?
            .primary_monitor()
            .map(|handle| monitor_handle_to_info(0, &handle))
    }

    /// Get all video modes for a monitor by index.
    pub fn video_modes(&self, monitor_index: usize) -> Vec<VideoModeInfo> {
        let window = match &self.state.window {
            Some(w) => w,
            None => return vec![],
        };
        let monitors: Vec<_> = window.available_monitors().collect();
        monitors
            .get(monitor_index)
            .map(|handle| {
                handle
                    .video_modes()
                    .map(|m| VideoModeInfo {
                        width: m.size().width,
                        height: m.size().height,
                        refresh_rate_hz: m.refresh_rate_millihertz() as f64 / 1000.0,
                        bit_depth: m.bit_depth(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all video modes for the current monitor (the monitor the window is on).
    pub fn current_monitor_video_modes(&self) -> Vec<VideoModeInfo> {
        let window = match &self.state.window {
            Some(w) => w,
            None => return vec![],
        };
        window
            .current_monitor()
            .map(|handle| {
                handle
                    .video_modes()
                    .map(|m| VideoModeInfo {
                        width: m.size().width,
                        height: m.size().height,
                        refresh_rate_hz: m.refresh_rate_millihertz() as f64 / 1000.0,
                        bit_depth: m.bit_depth(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get the current window display mode.
    pub fn window_mode(&self) -> WindowMode {
        self.state.window_mode
    }

    /// Change the window display mode at runtime.
    ///
    /// Switches between windowed, borderless fullscreen, and exclusive fullscreen.
    /// Cursor visibility is automatically updated unless previously overridden
    /// via [`set_cursor_visible`](Self::set_cursor_visible).
    pub fn set_window_mode(&mut self, mode: WindowMode) {
        if mode == WindowMode::DirectDisplay {
            warn!("set_window_mode(DirectDisplay) has no effect — use WindowMode::DirectDisplay in the builder");
            return;
        }
        if let Some(w) = &self.state.window {
            let fullscreen = match mode {
                WindowMode::Windowed => None,
                WindowMode::DirectDisplay => unreachable!(),
                WindowMode::BorderlessFullscreen => {
                    Some(Fullscreen::Borderless(w.current_monitor()))
                }
                WindowMode::ExclusiveFullscreen => {
                    if let Some(monitor) = w.current_monitor() {
                        let best = monitor.video_modes().max_by(|a, b| {
                            let area_a = a.size().width * a.size().height;
                            let area_b = b.size().width * b.size().height;
                            area_a.cmp(&area_b).then(
                                a.refresh_rate_millihertz()
                                    .cmp(&b.refresh_rate_millihertz()),
                            )
                        });
                        match best {
                            Some(vm) => Some(Fullscreen::Exclusive(vm)),
                            None => Some(Fullscreen::Borderless(Some(monitor))),
                        }
                    } else {
                        Some(Fullscreen::Borderless(None))
                    }
                }
            };
            w.set_fullscreen(fullscreen);

            // Auto-update cursor visibility if not explicitly overridden by config
            if self.config.cursor_visible.is_none() {
                let visible = matches!(mode, WindowMode::Windowed);
                self.state.cursor_visible = visible;
                w.set_cursor_visible(visible);
            }
        } else {
            warn!("set_window_mode() has no effect in DirectDisplay mode");
        }
        self.state.window_mode = mode;
    }
}

/// Convert a winit MonitorHandle to our MonitorInfo type.
fn monitor_handle_to_info(index: usize, handle: &winit::monitor::MonitorHandle) -> MonitorInfo {
    let size = handle.size();
    let position = handle.position();
    let video_modes: Vec<VideoModeInfo> = handle
        .video_modes()
        .map(|m| VideoModeInfo {
            width: m.size().width,
            height: m.size().height,
            refresh_rate_hz: m.refresh_rate_millihertz() as f64 / 1000.0,
            bit_depth: m.bit_depth(),
        })
        .collect();

    // Get refresh rate from the highest-res, highest-refresh video mode
    let refresh_rate_hz = video_modes
        .iter()
        .map(|m| m.refresh_rate_hz)
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    MonitorInfo {
        name: handle.name(),
        index,
        width: size.width,
        height: size.height,
        refresh_rate_hz,
        scale_factor: handle.scale_factor(),
        position: (position.x, position.y),
        video_modes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recording_state_pending_flip_handoff() {
        use crate::data::{CsvDataWriter, ExperimentSession};
        use crate::timing::{FlipInfo, Timestamp, TimingSource};

        let dir = std::env::temp_dir().join("vse_pending_flip_test");
        let _ = std::fs::remove_dir_all(&dir);

        let session = ExperimentSession::builder()
            .with_writer(CsvDataWriter::new(&dir))
            .build()
            .unwrap();

        let mut state = RecordingState {
            session,
            pending_flip: None,
            last_claimed_frame: None,
        };

        let make_flip = |n: u64| FlipInfo {
            frame_number: n,
            timing_source: TimingSource::CpuEstimate,
            submit_time: Timestamp::from_micros(0),
            present_time: Timestamp::from_micros(16_667),
            present_id: 0,
            target_time: None,
            on_target: true,
            missed: false,
            missed_count: 0,
            skipped: false,
        };

        // Simulate flip(0) — no record_frame called
        state.on_flip(make_flip(0));
        // Simulate flip(1) — flip(0) was unclaimed, timing-only row sent
        state.on_flip(make_flip(1));

        // Drop sends Shutdown + flush
        drop(state.session);

        std::thread::sleep(std::time::Duration::from_millis(50));

        let frames = std::fs::read_to_string(dir.join("frames.csv")).unwrap();
        let lines: Vec<&str> = frames.lines().collect();
        // header + 1 timing-only row for frame 0
        assert!(
            lines.len() >= 2,
            "expected at least header + 1 row, got: {:?}",
            lines
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_record_frame_without_flip_returns_error() {
        let err = VSEError::NoFlipPending;
        assert!(err.to_string().contains("flip"));
    }

    #[test]
    #[ignore] // EventLoop::new() panics off main thread on Linux
    fn test_builder_with_session_compiles() {
        use crate::data::{CsvDataWriter, ExperimentSession};
        let session = ExperimentSession::builder()
            .with_writer(CsvDataWriter::new("/tmp/test_session"))
            .build()
            .unwrap();
        let _builder = VSEContext::builder()
            .with_window_size(800, 600)
            .with_session(session);
        // Just verifies it compiles — ignored at runtime
    }

    #[test]
    fn direct_display_unavailable_error_contains_tried_methods() {
        let msg = "Tried:\n  \u{2717} No-compositor: held by compositor\n  \u{2717} DRM acquire: permission denied";
        let err = VSEError::DirectDisplayUnavailable(msg.to_string());
        let display = format!("{}", err);
        assert!(display.contains("Tried:"));
        assert!(display.contains("Direct display"));
    }

    #[test]
    fn vse_error_variants_display() {
        let e = VSEError::NoConfirmedFlip;
        assert!(e.to_string().contains("Presented"));

        let e = VSEError::NotInBufferedMode;
        assert!(e.to_string().contains("run_buffered"));

        let e = VSEError::NotSupportedInBufferedMode;
        assert!(e.to_string().contains("flip()"));
    }
}
