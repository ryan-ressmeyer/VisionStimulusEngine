//! Configuration, builder, and public context errors.

use thiserror::Error;
use tracing::info;
use winit::event_loop::EventLoop;

use super::{
    device::{DeviceError, GPUPreference},
    frame::FrameError,
    input::{AcquisitionMethod, MonitorSelection, WindowMode},
    swapchain::{PresentMode, SwapchainError},
};
use crate::data::ExperimentSession;
use crate::drawing::renderer::RendererError;

use super::context::VSEContext;

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
