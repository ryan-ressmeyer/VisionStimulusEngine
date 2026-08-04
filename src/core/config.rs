//! Configuration, builder, and public context errors.

use thiserror::Error;
use tracing::info;
use winit::event_loop::EventLoop;

use super::{
    device::{DeviceError, GPUPreference},
    input::{AcquisitionMethod, MonitorSelection, WindowMode},
    swapchain::{PresentMode, SwapchainError},
};
use crate::data::ExperimentSession;
use crate::drawing::renderer::RendererError;
use crate::drawing::{ModelError, PipelineError, PipelineSuite};

use super::context::VSEContext;

/// Errors that can occur while submitting or completing a frame.
#[derive(Error, Debug)]
pub enum FrameError {
    /// Failed to execute commands.
    #[error("Failed to execute commands: {0}")]
    ExecutionFailed(String),
}

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

    /// Native 3D model or camera error
    #[error("Model error: {0}")]
    Model(#[from] ModelError),

    /// User-registered Tier 1 pipeline error (build or record).
    #[error("Pipeline error: {0}")]
    Pipeline(#[from] PipelineError),

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

    /// `record_frame()` called from the render phase of a buffered loop.
    #[error(
        "record_frame() requires a confirmed buffered frame — \
             move it to the confirmation callback"
    )]
    NoConfirmedFlip,

    /// `flip()` called inside a structured buffered render callback.
    #[error("flip() is managed by run_buffered() — return BufferedFrame from the render callback")]
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
    /// Which built-in graphics pipelines to compile at startup
    /// (see [`VSEContextBuilder::with_pipelines`]). Defaults to the full suite.
    pub pipeline_suite: PipelineSuite,
    /// Offscreen render target, when the session is headless
    /// (see [`VSEContextBuilder::with_headless`]). `None` for a displayed session.
    pub headless: Option<HeadlessConfig>,
}

/// Offscreen target settings for a headless session.
///
/// For regeneration these must match the recorded session: a different color
/// format or extent produces different bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadlessConfig {
    pub width: u32,
    pub height: u32,
    pub format: vulkano::format::Format,
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
            pipeline_suite: PipelineSuite::default(),
            headless: None,
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

    /// Select which built-in graphics pipelines VSE compiles at startup.
    ///
    /// By default the full suite ([`PipelineSuite::default`]) is built, so every
    /// built-in `draw_*` works. Subselecting skips compiling unused pipelines;
    /// a draw whose pipeline was not built is skipped at render time with a
    /// one-time warning rather than causing an error.
    ///
    /// ```no_run
    /// use vision_stimulus_engine::prelude::*;
    ///
    /// let context = VSEContext::builder()
    ///     .with_pipelines(PipelineSuite::minimal().with(BuiltinPipeline::Dot))
    ///     .build()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn with_pipelines(mut self, suite: PipelineSuite) -> Self {
        self.config.pipeline_suite = suite;
        self
    }

    /// Render offscreen at `width` × `height` instead of to a display.
    ///
    /// The color format defaults to `B8G8R8A8_SRGB`, which is what the
    /// swapchain negotiates on most systems (it prefers an sRGB format). For
    /// regenerating a recorded session, prefer
    /// [`with_headless_from_host_info`](Self::with_headless_from_host_info),
    /// which takes format, extent, and pipeline suite from the recording rather
    /// than from your memory of it.
    ///
    /// Finish with [`build_headless`](Self::build_headless), not `build`.
    pub fn with_headless(mut self, width: u32, height: u32) -> Self {
        self.config.headless = Some(HeadlessConfig {
            width,
            height,
            format: vulkano::format::Format::B8G8R8A8_SRGB,
        });
        self
    }

    /// Configure a headless session to reproduce a recorded one, taking the
    /// color format, extent, and pipeline suite from its
    /// [`HostInfo`](crate::host::HostInfo).
    ///
    /// This is the intended entry point for post-hoc stimulus regeneration.
    /// Driving the target from the recording rather than from hand-passed
    /// parameters is the point: a format or suite that differs from the
    /// recorded session produces different pixels, and the difference is easy
    /// to introduce and hard to notice by eye.
    ///
    /// The clear color, refresh rate, and any other builder settings are yours
    /// to set — [`PipelineConfig`](crate::host::PipelineConfig) records the
    /// first two if you want them, but they are not applied automatically.
    ///
    /// Finish with [`build_headless`](Self::build_headless).
    ///
    /// # Errors
    ///
    /// [`VSEError::Window`] if the recorded color format is not one this
    /// version can name, and [`VSEError::Renderer`] if the recorded pipeline
    /// set contains a built-in this version does not have. Both mean the
    /// recording cannot be faithfully reproduced here, so neither is silently
    /// tolerated.
    pub fn with_headless_from_host_info(
        mut self,
        info: &crate::host::HostInfo,
    ) -> Result<Self, VSEError> {
        let format = parse_recorded_format(&info.swapchain.image_format).ok_or_else(|| {
            VSEError::Window(format!(
                "recorded color format {:?} is not one this VSE version can reconstruct; \
                 regenerating in a different format would not reproduce the recorded pixels",
                info.swapchain.image_format
            ))
        })?;

        let suite = PipelineSuite::from_key_names(&info.pipeline.builtin_pipelines)
            .map_err(RendererError::from)?;

        self.config.headless = Some(HeadlessConfig {
            width: info.swapchain.extent[0],
            height: info.swapchain.extent[1],
            format,
        });
        self.config.pipeline_suite = suite;
        Ok(self)
    }

    /// Override the offscreen color format set by
    /// [`with_headless`](Self::with_headless).
    ///
    /// Has no effect unless the session is headless.
    pub fn with_headless_format(mut self, format: vulkano::format::Format) -> Self {
        if let Some(headless) = &mut self.config.headless {
            headless.format = format;
        }
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
        // Reject an unrenderable pipeline suite here, at the earliest point it
        // is knowable. `Renderer::new` validates too, but it does not run until
        // the event loop starts — and a suite that renders a stimulus wrongly
        // must never get as far as a session that could record data.
        self.config
            .pipeline_suite
            .validate()
            .map_err(RendererError::from)?;

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

    /// Build a headless (offscreen) context.
    ///
    /// Unlike [`build`](Self::build), this constructs the GPU state
    /// immediately: with no surface to negotiate, nothing has to wait for a run
    /// loop. Requires [`with_headless`](Self::with_headless).
    ///
    /// # Errors
    ///
    /// [`VSEError::Window`] if the builder was not configured headless, plus
    /// the usual device and renderer errors.
    pub fn build_headless(self) -> Result<crate::core::HeadlessContext, VSEError> {
        self.config
            .pipeline_suite
            .validate()
            .map_err(RendererError::from)?;

        let headless = self.config.headless.ok_or_else(|| {
            VSEError::Window(
                "build_headless() requires .with_headless(width, height) on the builder".into(),
            )
        })?;

        info!(
            "Headless VSE context: {}x{}, {:?}",
            headless.width, headless.height, headless.format
        );

        crate::core::HeadlessContext::new(
            self.config,
            self.session,
            headless.format,
            [headless.width, headless.height],
        )
    }
}

impl Default for VSEContextBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Recover a [`Format`](vulkano::format::Format) from the `{:?}` name recorded
/// in [`SwapchainInfo::image_format`](crate::host::SwapchainInfo).
///
/// Covers the 8-bit and 10-bit color formats a swapchain actually negotiates,
/// plus the headless defaults. Deliberately a fixed table rather than a
/// wildcard: an unrecognized name must be an error the caller sees, not a
/// silent substitution that changes the regenerated pixels.
fn parse_recorded_format(name: &str) -> Option<vulkano::format::Format> {
    use vulkano::format::Format;
    Some(match name {
        "B8G8R8A8_SRGB" => Format::B8G8R8A8_SRGB,
        "B8G8R8A8_UNORM" => Format::B8G8R8A8_UNORM,
        "R8G8B8A8_SRGB" => Format::R8G8B8A8_SRGB,
        "R8G8B8A8_UNORM" => Format::R8G8B8A8_UNORM,
        "A2B10G10R10_UNORM_PACK32" => Format::A2B10G10R10_UNORM_PACK32,
        "A2R10G10B10_UNORM_PACK32" => Format::A2R10G10B10_UNORM_PACK32,
        "R16G16B16A16_SFLOAT" => Format::R16G16B16A16_SFLOAT,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawing::{BuiltinPipeline, SuiteError};

    // These build in DirectDisplay mode, which skips winit `EventLoop`
    // creation, so they run without a display server.

    #[test]
    fn building_with_a_half_configured_additive_gabor_suite_fails_immediately() {
        // The suite must be rejected at construction. Deferring to the first
        // draw would mean a session that records data while presenting a
        // stimulus at half its signed modulation.
        let result = VSEContext::builder()
            .with_window_mode(WindowMode::DirectDisplay)
            .with_pipelines(PipelineSuite::default().without(BuiltinPipeline::SubtractiveGabor))
            .build();

        // `VSEContext` is not `Debug`, so match rather than `expect_err`.
        let Err(err) = result else {
            panic!("an unpaired additive-Gabor pass must not reach a running session");
        };
        assert!(matches!(
            err,
            VSEError::Renderer(RendererError::Suite(SuiteError::UnpairedGaborPass))
        ));
    }

    #[test]
    fn building_with_a_valid_suite_succeeds() {
        let result = VSEContext::builder()
            .with_window_mode(WindowMode::DirectDisplay)
            .with_pipelines(PipelineSuite::minimal().with(BuiltinPipeline::Dot))
            .build();

        assert!(
            result.is_ok(),
            "a suite with neither additive-Gabor pass is valid"
        );
    }
}
