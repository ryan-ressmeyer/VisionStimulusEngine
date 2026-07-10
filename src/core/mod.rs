//! Core Vulkan infrastructure
//!
//! This module contains the fundamental Vulkan initialization and
//! management code, abstracted for ease of use while maintaining
//! access to underlying Vulkan objects.

mod buffered;
mod context;
mod device;
#[cfg(target_os = "linux")]
pub(crate) mod direct_display;
#[cfg(target_os = "linux")]
pub(crate) mod evdev_input;
mod frame;
pub(crate) mod input;
pub(crate) mod present_engine;
pub(crate) mod present_timing_ext;
mod swapchain;

// Public API exports
pub use buffered::{BufferedConfig, FlipEvent};
pub use context::{RenderContext, VSEConfig, VSEContext, VSEContextBuilder, VSEError};
pub use device::{DeviceError, DeviceSelector, GPUPreference};
pub use frame::{Frame, FrameError};
pub use input::{
    AcquisitionMethod, DisplayBackend, InputEvent, Key, KeyCode, MonitorInfo, MonitorSelection,
    MouseButton, NamedKey, PhysicalKey, VideoModeInfo, WindowMode,
};
pub use present_timing_ext::ScanoutFeedback;
pub use swapchain::{PresentMode, SwapchainConfig, SwapchainError, SwapchainManager};
