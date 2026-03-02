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
mod swapchain;

// Public API exports
pub use context::{RenderContext, VSEConfig, VSEContext, VSEContextBuilder, VSEError};
pub use buffered::{BufferedConfig, FlipEvent};
pub(crate) use buffered::{InFlightFuture, PendingFrame};
pub use device::{DeviceError, DeviceSelector, GPUPreference};
pub use frame::{Frame, FrameError};
pub use input::{
    AcquisitionMethod, DisplayBackend, InputEvent, Key, KeyCode, MonitorInfo, MonitorSelection,
    MouseButton, NamedKey, PhysicalKey, VideoModeInfo, WindowMode,
};
pub use swapchain::{PresentMode, SwapchainConfig, SwapchainError, SwapchainManager};

#[cfg(test)]
mod buffered_compile_test {
    use super::*;
    #[test]
    fn buffered_config_default() {
        let cfg = BufferedConfig::default();
        assert_eq!(cfg.depth, 1);
    }
    #[test]
    fn flip_event_pattern_match() {
        let event: FlipEvent<u32> = FlipEvent::Render;
        match event {
            FlipEvent::Render => {}
            FlipEvent::Presented { .. } => {}
            _ => {}
        }
    }
}
