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

use super::input::{InputEvent, InputState, MonitorInfo, MonitorSelection, MouseButton, VideoModeInfo, WindowMode};
use super::{
    device::{DeviceError, DeviceSelector, GPUPreference},
    frame::{FrameBuilder, FrameError},
    swapchain::{PresentMode, SwapchainConfig, SwapchainError, SwapchainManager},
};
use vulkano::sync::GpuFuture;

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
#[derive(Debug, Clone)]
pub struct VSEContextBuilder {
    config: VSEConfig,
}

impl VSEContextBuilder {
    /// Create a new builder with default settings
    pub fn new() -> Self {
        Self {
            config: VSEConfig::default(),
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
    pub fn with_flip_logging(mut self, enabled: bool) -> Self {
        self.config.flip_logging = enabled;
        self
    }

    /// Set CSV path for automatic flip log export.
    ///
    /// The CSV file is written when the context shuts down.
    /// Implies `with_flip_logging(true)`.
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

    /// Build the VSEContext
    ///
    /// This creates the event loop but does not yet create the window.
    /// The window is created when `run()` is called.
    ///
    /// # Errors
    ///
    /// Returns `VSEError` if initialization fails.
    pub fn build(self) -> Result<VSEContext, VSEError> {
        let event_loop = EventLoop::new().map_err(|e| VSEError::EventLoop(e.to_string()))?;

        info!(
            "VSEContext created with config: {}x{}, {:?}",
            self.config.window_width, self.config.window_height, self.config.present_mode
        );

        Ok(VSEContext {
            config: self.config,
            event_loop: Some(event_loop),
        })
    }
}

impl Default for VSEContextBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Internal state that requires an active window
struct VSEState {
    window: Arc<Window>,
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
    fn initialize(
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
            WindowMode::Windowed => None,
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
                        .max_by(|a, b| a.refresh_rate_millihertz().cmp(&b.refresh_rate_millihertz()))
                        .or_else(|| {
                            // Fall back to native resolution (highest refresh rate)
                            modes.iter().max_by(|a, b| {
                                let area_a = a.size().width * a.size().height;
                                let area_b = b.size().width * b.size().height;
                                area_a
                                    .cmp(&area_b)
                                    .then(a.refresh_rate_millihertz().cmp(&b.refresh_rate_millihertz()))
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

        info!(
            "Window created: {}x{} mode={:?} cursor_visible={}",
            config.window_width, config.window_height, config.window_mode, cursor_visible
        );

        // Initialize Vulkan
        let (device_selector, surface) =
            DeviceSelector::with_surface(config.gpu_preference, window.clone())?;

        let (device, queue) = device_selector.create_device()?;

        let swapchain_config = SwapchainConfig {
            width: config.window_width,
            height: config.window_height,
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

        Ok(VSEState {
            window,
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
        })
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
        let event_loop = self
            .event_loop
            .take()
            .ok_or_else(|| VSEError::EventLoop("Event loop already consumed".into()))?;

        let mut config = self.config;
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

                        match Self::initialize(elwt, &config) {
                            Ok(s) => {
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
                            WindowEvent::MouseInput { state: btn_state, button, .. } => {
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

                                s.input.begin_frame();

                                let mut render_ctx = RenderContext {
                                    state: s,
                                    config: &mut config,
                                };

                                if let Err(e) = render_fn(&mut render_ctx) {
                                    warn!("Render error: {}", e);
                                    *error_clone.borrow_mut() = Some(e);
                                    elwt.exit();
                                }
                            }
                            _ => {}
                        }
                    }
                    Event::AboutToWait => {
                        if let Some(s) = &state {
                            s.window.request_redraw();
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
        if self.state.swapchain.needs_recreation() {
            self.state.swapchain.recreate_from_surface()?;
        }

        // Acquire next image
        let (image_index, _suboptimal, acquire_future) =
            match self.state.swapchain.acquire_next_image() {
                Ok(result) => result,
                Err(SwapchainError::OutOfDate) => {
                    self.state.swapchain.recreate_from_surface()?;
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

        // Update state for next frame
        self.state.last_present_time = Some(present_time);
        self.state.frame_number += 1;

        // Clear input event queue after the frame
        self.state.input.clear_events();

        Ok(flip_info)
    }

    /// Check if the window should close
    pub fn should_close(&self) -> bool {
        self.state.should_close
    }

    /// Set the clear color (RGBA, 0.0-1.0 range)
    pub fn set_clear_color(&mut self, r: f32, g: f32, b: f32, a: f32) {
        self.config.clear_color = [r, g, b, a];
    }

    /// Get the current clear color
    pub fn clear_color(&self) -> [f32; 4] {
        self.config.clear_color
    }

    /// Get the window dimensions
    pub fn window_size(&self) -> (u32, u32) {
        let extent = self.state.swapchain.extent();
        (extent[0], extent[1])
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
            &self.state.window,
            &self.state.swapchain,
            self.config,
        )
    }
}
