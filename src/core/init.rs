//! Window/compositor and direct-display initialization.

use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};
use vulkano::swapchain::Surface;
use winit::{
    dpi::PhysicalSize,
    event_loop::EventLoopWindowTarget,
    window::{Fullscreen, Window, WindowBuilder},
};

use super::config::{VSEConfig, VSEError};
use super::context::VSEContext;
use super::device::DeviceSelector;
use super::input::{InputState, MonitorSelection, WindowMode};
use super::present_timing_ext as pt;
use super::state::{
    build_host_bridge, build_present_engine, build_timing_provider, InputSource, PresentTarget,
    RenderTarget, VSEState,
};
use super::swapchain::{SwapchainConfig, SwapchainManager};
use crate::drawing::renderer::Renderer;
use crate::timing::{Clock, TimingProvider};

/// A presentation-capable surface plus the path-specific state needed to drive it.
///
/// Window and direct-display acquisition stay separate; everything after this
/// point must be initialized identically for both display paths.
struct DisplayEndpoint {
    device_selector: DeviceSelector,
    surface: Arc<Surface>,
    swapchain_extent: [u32; 2],
    display_size: (u32, u32),
    window: Option<Arc<Window>>,
    cursor_visible: bool,
    window_mode: WindowMode,
    input: DisplayInput,
    acquired_display: Option<super::input::AcquisitionMethod>,
}

enum DisplayInput {
    Winit,
    #[cfg(target_os = "linux")]
    Evdev,
}

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

        let (device_selector, surface) =
            DeviceSelector::with_surface(config.gpu_preference, window.clone())?;
        let swapchain_size = window.inner_size();

        Self::initialize_present_target(
            config,
            DisplayEndpoint {
                device_selector,
                surface,
                swapchain_extent: [swapchain_size.width, swapchain_size.height],
                display_size: (actual_size.width, actual_size.height),
                window: Some(window),
                cursor_visible,
                window_mode: config.window_mode,
                input: DisplayInput::Winit,
                acquired_display: None,
            },
        )
    }

    /// Initialize Vulkan state for direct display mode (no winit, no compositor).
    #[cfg(target_os = "linux")]
    pub(super) fn initialize_direct(config: &VSEConfig) -> Result<VSEState, VSEError> {
        use crate::core::direct_display::{acquire_display, default_acquisition_order};
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

        Self::initialize_present_target(
            config,
            DisplayEndpoint {
                device_selector,
                surface: direct_surface.surface,
                swapchain_extent: [direct_surface.width, direct_surface.height],
                display_size: (direct_surface.width, direct_surface.height),
                window: None,
                cursor_visible: false,
                window_mode: WindowMode::DirectDisplay,
                input: DisplayInput::Evdev,
                acquired_display: Some(direct_surface.method),
            },
        )
    }

    /// Initialize the Vulkan and runtime state shared by every displayed session.
    fn initialize_present_target(
        config: &VSEConfig,
        endpoint: DisplayEndpoint,
    ) -> Result<VSEState, VSEError> {
        let DisplayEndpoint {
            device_selector,
            surface,
            swapchain_extent,
            display_size,
            window,
            cursor_visible,
            window_mode,
            input,
            acquired_display,
        } = endpoint;

        let (device, queue, ext_features) = device_selector.create_device()?;
        let swapchain_config = SwapchainConfig {
            width: swapchain_extent[0],
            height: swapchain_extent[1],
            present_mode: config.present_mode,
            image_count: 2,
        };

        // Every present-family device opt-in needs its matching swapchain flag.
        let opt_ins = ext_features
            .map(|f| pt::SwapchainOptIns::from_features(&f))
            .unwrap_or_default();
        let swapchain =
            SwapchainManager::new_with_opt_ins(device.clone(), surface, swapchain_config, opt_ins)?;
        let renderer = Renderer::new(
            device.clone(),
            queue.clone(),
            swapchain.format(),
            swapchain.images().len(),
            swapchain.extent(),
            &config.pipeline_suite,
        )?;

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

        let input_source = match input {
            DisplayInput::Winit => {
                info!("Vulkan initialization complete");
                InputSource::Winit
            }
            #[cfg(target_os = "linux")]
            DisplayInput::Evdev => {
                use crate::core::evdev_input::EvdevReader;
                let reader = match EvdevReader::open() {
                    Ok(mut reader) => {
                        reader.set_display_size(display_size.0, display_size.1);
                        reader
                    }
                    Err(msg) => {
                        warn!("evdev input unavailable: {}", msg);
                        EvdevReader::empty()
                    }
                };
                info!("Direct display initialization complete");
                InputSource::Evdev(reader)
            }
        };

        Ok(VSEState {
            device_selector,
            device,
            queue,
            renderer,
            clock,
            frame_number: 0,
            should_close: false,
            input: InputState::new(),
            recording: None,
            target: RenderTarget::Present(Box::new(PresentTarget {
                window,
                swapchain,
                minimized: false,
                cursor_visible,
                window_mode,
                timing_provider,
                present_engine,
                recent_scanouts: Vec::new(),
                scanout_by_present_id: std::collections::HashMap::new(),
                last_present_time: None,
                last_scanout_ns: None,
                last_scanout_present_id: None,
                expected_frame_duration,
                refresh_detect_samples: Vec::with_capacity(10),
                scanout_clock: None,
                scanout_feedback_populated: None,
                absolute_scheduling_enforced: None,
                software_present_pacing: true,
                scanout_feedback_probe_count: 0,
                warned_feedback_stub: false,
                warned_sw_pacing: false,
                host_bridge,
                last_bridge_sample_ts: None,
                input_source,
                display_size,
                acquired_display,
                buffered_confirmed_flip: None,
                in_buffered_mode: false,
                ext_features,
                external_source: None,
                external_readback: None,
            })),
        })
    }
}
