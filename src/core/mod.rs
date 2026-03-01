//! Core Vulkan infrastructure
//!
//! This module contains the fundamental Vulkan initialization and
//! management code, abstracted for ease of use while maintaining
//! access to underlying Vulkan objects.

mod context;
mod device;
mod frame;
pub(crate) mod input;
mod swapchain;
#[cfg(target_os = "linux")]
pub(crate) mod evdev_input;
#[cfg(target_os = "linux")]
pub(crate) mod direct_display;

// Public API exports
pub use context::{RenderContext, VSEConfig, VSEContext, VSEContextBuilder, VSEError};
pub use device::{DeviceError, DeviceSelector, GPUPreference};
pub use frame::{Frame, FrameError};
pub use input::{
    AcquisitionMethod, DisplayBackend, InputEvent, Key, KeyCode, MonitorInfo, MonitorSelection,
    MouseButton, NamedKey, PhysicalKey, VideoModeInfo, WindowMode,
};
pub use swapchain::{PresentMode, SwapchainConfig, SwapchainError, SwapchainManager};
