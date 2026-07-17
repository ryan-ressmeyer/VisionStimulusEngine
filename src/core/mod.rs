//! Core Vulkan infrastructure
//!
//! This module contains the fundamental Vulkan initialization and
//! management code, abstracted for ease of use while maintaining
//! access to underlying Vulkan objects.

mod buffered;
mod config;
mod context;
mod device;
#[cfg(target_os = "linux")]
pub(crate) mod direct_display;
#[cfg(target_os = "linux")]
mod direct_loop;
#[cfg(target_os = "linux")]
pub(crate) mod evdev_input;
mod event_loop;
pub(crate) mod external_frame;
mod flip;
mod frame;
mod init;
pub(crate) mod input;
pub(crate) mod present_engine;
pub(crate) mod present_timing_ext;
mod render_context;
mod state;
mod swapchain;

// Public API exports
pub use buffered::{BufferedConfig, FlipEvent};
pub use config::{VSEConfig, VSEContextBuilder, VSEError};
pub use context::VSEContext;
pub use device::{DeviceError, DeviceSelector, GPUPreference};
pub use external_frame::{ExternalFrameError, ExternalFramePolicy, ExternalFrameRing};
pub use frame::{Frame, FrameError};
pub use input::{
    AcquisitionMethod, DisplayBackend, InputEvent, Key, KeyCode, MonitorInfo, MonitorSelection,
    MouseButton, NamedKey, PhysicalKey, VideoModeInfo, WindowMode,
};
pub use present_timing_ext::ScanoutFeedback;
pub use render_context::RenderContext;
pub use swapchain::{PresentMode, SwapchainConfig, SwapchainError, SwapchainManager};
