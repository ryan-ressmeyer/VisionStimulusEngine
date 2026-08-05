//! Configuration, builders, and public context errors.

use thiserror::Error;
use tracing::info;
use vulkano::format::Format;
use winit::event_loop::EventLoop;

use super::{
    device::{DeviceError, GPUPreference},
    input::{AcquisitionMethod, MonitorSelection, WindowMode},
    swapchain::{PresentMode, SwapchainError},
};
use crate::data::ExperimentSession;
use crate::drawing::renderer::RendererError;
use crate::drawing::PipelineError;

use super::context::VSEContext;

/// Errors that can occur while submitting or completing a frame.
#[derive(Error, Debug)]
pub enum FrameError {
    /// Failed to execute commands.
    #[error("Failed to execute commands: {0}")]
    ExecutionFailed(String),
}

/// Errors that can occur in VSEContext.
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

    /// User-registered Tier 1 pipeline error (build or record).
    #[error("Pipeline error: {0}")]
    Pipeline(#[from] PipelineError),

    /// Error reported by a separately versioned extension crate.
    #[error("Extension error: {0}")]
    Extension(#[source] Box<dyn std::error::Error + Send + Sync>),

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

    /// No ExperimentSession attached. Call the builder's `with_session()` to enable recording.
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

impl VSEError {
    /// Wrap an error from a separately versioned extension crate while
    /// preserving its source chain.
    pub fn extension(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Extension(Box::new(error))
    }
}

/// Settings shared by displayed and offscreen rendering.
#[derive(Debug, Clone)]
pub(crate) struct RenderConfig {
    pub(crate) gpu_preference: GPUPreference,
    pub(crate) clear_color: [f32; 4],
    pub(crate) expected_refresh_rate: Option<f64>,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            gpu_preference: GPUPreference::Discrete,
            clear_color: [0.0, 0.0, 0.0, 1.0],
            expected_refresh_rate: None,
        }
    }
}

/// Configuration meaningful only for a displayed context.
#[derive(Debug, Clone)]
pub(crate) struct DisplayedConfig {
    pub(crate) render: RenderConfig,
    pub(crate) window_width: u32,
    pub(crate) window_height: u32,
    pub(crate) window_title: String,
    pub(crate) present_mode: PresentMode,
    pub(crate) window_mode: WindowMode,
    pub(crate) monitor_selection: MonitorSelection,
    pub(crate) cursor_visible: Option<bool>,
    pub(crate) direct_display_video_mode: Option<(u32, u32, f64)>,
    pub(crate) direct_display_acquisition_order: Option<Vec<AcquisitionMethod>>,
    pub(crate) host_clock_bridge: bool,
}

impl Default for DisplayedConfig {
    fn default() -> Self {
        Self {
            render: RenderConfig::default(),
            window_width: 800,
            window_height: 600,
            window_title: "VisionStimulusEngine".to_string(),
            present_mode: PresentMode::Fifo,
            window_mode: WindowMode::default(),
            monitor_selection: MonitorSelection::default(),
            cursor_visible: None,
            direct_display_video_mode: None,
            direct_display_acquisition_order: None,
            host_clock_bridge: false,
        }
    }
}

/// Configuration meaningful only for an offscreen context.
#[derive(Debug, Clone)]
pub(crate) struct HeadlessConfig {
    pub(crate) render: RenderConfig,
    pub(crate) extent: [u32; 2],
    pub(crate) format: Format,
}

/// Builder for a displayed [`VSEContext`].
///
/// Window, presentation, monitor, and direct-display options live here. For
/// offscreen rendering, start from [`HeadlessContext::builder`](crate::core::HeadlessContext::builder).
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
///
/// Headless target options are intentionally unavailable here:
///
/// ```compile_fail
/// use vision_stimulus_engine::prelude::*;
/// let _ = VSEContext::builder().with_headless(512, 512);
/// ```
#[derive(Debug)]
pub struct VSEContextBuilder {
    config: DisplayedConfig,
    session: Option<ExperimentSession>,
}

impl VSEContextBuilder {
    /// Create a displayed builder with default settings.
    pub fn new() -> Self {
        Self {
            config: DisplayedConfig::default(),
            session: None,
        }
    }

    pub fn with_window_size(mut self, width: u32, height: u32) -> Self {
        self.config.window_width = width;
        self.config.window_height = height;
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.config.window_title = title.into();
        self
    }

    pub fn with_gpu_preference(mut self, preference: GPUPreference) -> Self {
        self.config.render.gpu_preference = preference;
        self
    }

    /// Set presentation mode (`Fifo` is recommended for VSE timing workflows).
    pub fn with_present_mode(mut self, mode: PresentMode) -> Self {
        self.config.present_mode = mode;
        self
    }

    pub fn with_clear_color(mut self, r: f32, g: f32, b: f32, a: f32) -> Self {
        self.config.render.clear_color = [r, g, b, a];
        self
    }

    /// Set the expected refresh rate used for missed-frame detection.
    pub fn with_expected_refresh_rate(mut self, hz: f64) -> Self {
        self.config.render.expected_refresh_rate = Some(hz);
        self
    }

    /// Enable the opt-in host↔scanout clock bridge.
    pub fn with_host_clock_bridge(mut self) -> Self {
        self.config.host_clock_bridge = true;
        self
    }

    pub fn with_window_mode(mut self, mode: WindowMode) -> Self {
        self.config.window_mode = mode;
        self
    }

    pub fn with_monitor(mut self, selection: MonitorSelection) -> Self {
        self.config.monitor_selection = selection;
        self
    }

    pub fn with_cursor_visible(mut self, visible: bool) -> Self {
        self.config.cursor_visible = Some(visible);
        self
    }

    pub fn with_direct_display_video_mode(
        mut self,
        width: u32,
        height: u32,
        refresh_hz: f64,
    ) -> Self {
        self.config.direct_display_video_mode = Some((width, height, refresh_hz));
        self
    }

    pub fn with_acquisition_order(mut self, order: Vec<AcquisitionMethod>) -> Self {
        self.config.direct_display_acquisition_order = Some(order);
        self
    }

    pub fn with_session(mut self, session: ExperimentSession) -> Self {
        self.session = Some(session);
        self
    }

    /// Build a displayed context.
    ///
    /// This creates the event loop but defers window and Vulkan initialization
    /// until a displayed runtime starts.
    pub fn build(self) -> Result<VSEContext, VSEError> {
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

/// Builder for a [`HeadlessContext`](crate::core::HeadlessContext).
///
/// It exposes only settings meaningful without a window or presentation
/// engine. Create one through [`HeadlessContext::builder`](crate::core::HeadlessContext::builder)
/// or [`HeadlessContext::builder_from_host_info`](crate::core::HeadlessContext::builder_from_host_info).
///
/// Display presentation options are intentionally unavailable:
///
/// ```compile_fail
/// use vision_stimulus_engine::prelude::*;
/// let _ = HeadlessContext::builder(512, 512).with_present_mode(PresentMode::Fifo);
/// ```
#[derive(Debug)]
pub struct HeadlessContextBuilder {
    config: HeadlessConfig,
    session: Option<ExperimentSession>,
}

impl HeadlessContextBuilder {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        Self {
            config: HeadlessConfig {
                render: RenderConfig::default(),
                extent: [width, height],
                format: Format::B8G8R8A8_SRGB,
            },
            session: None,
        }
    }

    pub(crate) fn from_host_info(info: &crate::host::HostInfo) -> Result<Self, VSEError> {
        let format = parse_recorded_format(&info.swapchain.image_format).ok_or_else(|| {
            VSEError::Window(format!(
                "recorded color format {:?} is not one this VSE version can reconstruct; \
                 regenerating in a different format would not reproduce the recorded pixels",
                info.swapchain.image_format
            ))
        })?;
        Ok(Self {
            config: HeadlessConfig {
                render: RenderConfig::default(),
                extent: info.swapchain.extent,
                format,
            },
            session: None,
        })
    }

    pub fn with_format(mut self, format: Format) -> Self {
        self.config.format = format;
        self
    }

    pub fn with_gpu_preference(mut self, preference: GPUPreference) -> Self {
        self.config.render.gpu_preference = preference;
        self
    }

    pub fn with_clear_color(mut self, r: f32, g: f32, b: f32, a: f32) -> Self {
        self.config.render.clear_color = [r, g, b, a];
        self
    }

    /// Set the nominal refresh used to synthesize offscreen frame timestamps.
    pub fn with_expected_refresh_rate(mut self, hz: f64) -> Self {
        self.config.render.expected_refresh_rate = Some(hz);
        self
    }

    pub fn with_session(mut self, session: ExperimentSession) -> Self {
        self.session = Some(session);
        self
    }

    /// Eagerly build the GPU and offscreen target.
    pub fn build(self) -> Result<crate::core::HeadlessContext, VSEError> {
        info!(
            "Headless VSE context: {}x{}, {:?}",
            self.config.extent[0], self.config.extent[1], self.config.format
        );

        crate::core::HeadlessContext::new(self.config, self.session)
    }
}

/// Recover a [`Format`] from the debug name recorded in `HostInfo`.
fn parse_recorded_format(name: &str) -> Option<Format> {
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
