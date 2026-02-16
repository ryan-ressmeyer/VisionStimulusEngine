//! Core Vulkan infrastructure
//!
//! This module contains the fundamental Vulkan initialization and
//! management code, abstracted for ease of use while maintaining
//! access to underlying Vulkan objects.

mod context;
mod device;
mod frame;
mod swapchain;

// Public API exports
pub use context::{RenderContext, VSEConfig, VSEContext, VSEContextBuilder, VSEError};
pub use device::{DeviceError, DeviceSelector, GPUPreference};
pub use frame::{Frame, FrameError};
pub use swapchain::{PresentMode, SwapchainConfig, SwapchainError, SwapchainManager};
