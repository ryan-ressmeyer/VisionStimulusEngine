//! Window/compositor and direct-display initialization.

use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};
use winit::{
    dpi::PhysicalSize,
    event_loop::EventLoopWindowTarget,
    window::{Fullscreen, WindowBuilder},
};

use super::config::{VSEConfig, VSEError};
use super::context::VSEContext;
use super::device::DeviceSelector;
use super::frame::FrameBuilder;
use super::input::{InputState, MonitorSelection, WindowMode};
use super::state::{
    build_host_bridge, build_present_engine, build_timing_provider, InputSource, VSEState,
};
use super::swapchain::{SwapchainConfig, SwapchainManager};
use crate::drawing::renderer::Renderer;
use crate::timing::{Clock, TimingProvider};

impl VSEContext {
    /// Initialize Vulkan state from an event loop window target
    pub(super) fn initialize_compositor(
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
    pub(super) fn initialize_direct(config: &VSEConfig) -> Result<VSEState, VSEError> {
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
}
