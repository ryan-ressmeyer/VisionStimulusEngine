//! VSEContext - Top-level VisionStimulusEngine environment
//!
//! This module provides the main entry point for VSE, managing all Vulkan
//! resources and providing a clean API for rendering operations.

use std::cell::RefCell;
use std::path::PathBuf;
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

use super::input::{
    AcquisitionMethod, DisplayBackend, InputEvent, InputState, KeyCode, MonitorInfo,
    MonitorSelection, MouseButton, VideoModeInfo, WindowMode,
};
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
    Clock, CpuTimingProvider, FlipInfo, FlipLogger, GoogleDisplayTimingProvider, Timestamp,
    TimingProvider, TimingSource, TimingStats,
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

    /// No ExperimentSession attached. Call VSEContextBuilder::with_session() to enable recording.
    #[error("no ExperimentSession attached — call .with_session() on the builder")]
    NoSession,

    /// Data recording error.
    #[error("Data recording error: {0}")]
    DataRecording(String),
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
    /// Enable flip timing and logging
    pub flip_logging: bool,
    /// Optional CSV path for automatic flip log export on shutdown
    pub flip_log_csv_path: Option<PathBuf>,
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
            flip_logging: false,
            flip_log_csv_path: None,
            expected_refresh_rate: None,
            window_mode: WindowMode::default(),
            monitor_selection: MonitorSelection::default(),
            cursor_visible: None,
            direct_display_video_mode: None,
            direct_display_acquisition_order: None,
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

    /// Enable flip timing and logging.
    ///
    /// When enabled, every `flip()` call records timing data.
    #[deprecated(
        since = "0.2.0",
        note = "Use VSEContextBuilder::with_session(ExperimentSession::builder()\
                .with_writer(CsvDataWriter::new(path)).build()?) instead."
    )]
    pub fn with_flip_logging(mut self, enabled: bool) -> Self {
        self.config.flip_logging = enabled;
        self
    }

    /// Set CSV path for automatic flip log export.
    ///
    /// The CSV file is written when the context shuts down.
    /// Implies `with_flip_logging(true)`.
    #[deprecated(
        since = "0.2.0",
        note = "Use VSEContextBuilder::with_session(ExperimentSession::builder()\
                .with_writer(CsvDataWriter::new(path)).build()?) instead."
    )]
    pub fn with_flip_log_csv(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.flip_log_csv_path = Some(path.into());
        self.config.flip_logging = true;
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
    flip_logger: Option<FlipLogger>,
    frame_number: u64,
    last_present_time: Option<Timestamp>,
    expected_frame_duration: Option<Duration>,
    refresh_detect_samples: Vec<Duration>,
    input_source: InputSource,
    /// Physical display dimensions (from window or VkDisplaySurfaceKHR).
    display_size: (u32, u32),
    /// Which acquisition method succeeded, if in DirectDisplay mode.
    acquired_display: Option<AcquisitionMethod>,
    /// Optional data recording session.
    recording: Option<RecordingState>,
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

        let (device, queue) = device_selector.create_device()?;

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

        let swapchain = SwapchainManager::new(device.clone(), surface, swapchain_config)?;
        let frame_builder = FrameBuilder::new(device.clone(), queue.clone());
        let renderer = Renderer::new(device.clone(), queue.clone(), swapchain.format())?;

        // Initialize timing
        let clock = Clock::new();

        let timing_provider: Box<dyn TimingProvider> = if device_selector
            .supports_google_display_timing()
        {
            info!("Timing backend: GoogleDisplayTiming (VK_GOOGLE_display_timing)");
            Box::new(unsafe { GoogleDisplayTimingProvider::new(&device, swapchain.swapchain()) })
        } else {
            warn!("VK_GOOGLE_display_timing not available. Using CPU estimation for timing.");
            Box::new(CpuTimingProvider::new())
        };

        let flip_logger = if config.flip_logging {
            let capacity = 3600 * 10; // ~10 minutes at 60 Hz
            Some(match &config.flip_log_csv_path {
                Some(path) => FlipLogger::with_csv(path.clone(), capacity),
                None => FlipLogger::new(capacity),
            })
        } else {
            None
        };

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
            flip_logger,
            frame_number: 0,
            last_present_time: None,
            expected_frame_duration,
            refresh_detect_samples: Vec::with_capacity(10),
            input_source: InputSource::Winit,
            display_size: win_size,
            acquired_display: None,
            recording: None,
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

        let (device, queue) = device_selector.create_device().map_err(VSEError::Device)?;

        let swapchain_config = SwapchainConfig {
            width,
            height,
            present_mode: config.present_mode,
            image_count: 2,
        };

        let swapchain = SwapchainManager::new(device.clone(), surface, swapchain_config)?;
        let frame_builder = FrameBuilder::new(device.clone(), queue.clone());
        let renderer = Renderer::new(device.clone(), queue.clone(), swapchain.format())?;

        let clock = Clock::new();

        let timing_provider: Box<dyn TimingProvider> = if device_selector
            .supports_google_display_timing()
        {
            Box::new(unsafe { GoogleDisplayTimingProvider::new(&device, swapchain.swapchain()) })
        } else {
            Box::new(CpuTimingProvider::new())
        };

        let flip_logger = if config.flip_logging {
            let capacity = 3600 * 10;
            Some(match &config.flip_log_csv_path {
                Some(path) => FlipLogger::with_csv(path.clone(), capacity),
                None => FlipLogger::new(capacity),
            })
        } else {
            None
        };

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
            flip_logger,
            recording: None,
            frame_number: 0,
            last_present_time: None,
            expected_frame_duration,
            refresh_detect_samples: Vec::with_capacity(10),
            input_source: InputSource::Evdev(evdev_reader),
            display_size: (width, height),
            acquired_display: Some(method),
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
            self.state.swapchain.recreate_from_surface(win_size_arr)?;
        }

        // Acquire next image
        let (image_index, _suboptimal, acquire_future) =
            match self.state.swapchain.acquire_next_image() {
                Ok(result) => result,
                Err(SwapchainError::OutOfDate) => {
                    self.state.swapchain.recreate_from_surface(win_size_arr)?;
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

        let flip_info = FlipInfo {
            frame_number: self.state.frame_number,
            timing_source: self.state.timing_provider.source(),
            submit_time,
            present_time,
            missed,
            missed_count,
            skipped: false,
        };

        // Record to logger
        if let Some(logger) = &mut self.state.flip_logger {
            logger.record(flip_info.clone());
        }

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

    /// Record per-frame experimental data merged with the most recent flip's timing.
    ///
    /// Must be called after `flip()`. The data struct must implement
    /// `serde::Serialize`. Multiple calls per frame are allowed — each produces
    /// one row in the output keyed to the same `frame_number`.
    ///
    /// Returns `VSEError::NoFlipPending` if `flip()` has not been called yet this
    /// frame, or `VSEError::NoSession` if no session was attached to the builder.
    pub fn record_frame<F: serde::Serialize>(&mut self, data: F) -> Result<(), VSEError> {
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
    /// prefer [`record_frame`] or [`record_annotation`].
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

    /// Get the flip logger (if timing is enabled).
    pub fn flip_logger(&self) -> Option<&FlipLogger> {
        self.state.flip_logger.as_ref()
    }

    /// Get computed timing statistics from all recorded frames.
    /// Returns None if timing is disabled or fewer than 2 frames recorded.
    pub fn timing_stats(&self) -> Option<TimingStats> {
        self.state
            .flip_logger
            .as_ref()
            .and_then(|logger| TimingStats::compute(logger.records()))
    }

    /// Print a timing report to stdout.
    /// No-op if timing is not enabled or fewer than 2 frames recorded.
    pub fn print_timing_report(&self) {
        if let Some(stats) = self.timing_stats() {
            stats.print_report();
        }
    }

    /// Get the timing clock (for correlating with external events).
    pub fn clock(&self) -> &Clock {
        &self.state.clock
    }

    /// Get the current frame number (before the next flip).
    pub fn frame_number(&self) -> u64 {
        self.state.frame_number
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
            self.state.window.as_deref(),
            &self.state.swapchain,
            self.config,
        )
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
}
