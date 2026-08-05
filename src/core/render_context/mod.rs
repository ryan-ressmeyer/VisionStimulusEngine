//! Public rendering, drawing, input, recording, and query API.

use std::path::Path;
use std::sync::Arc;

use tracing::warn;
use winit::{dpi::LogicalPosition, window::Fullscreen};

use super::config::{RenderConfig, VSEError};
use super::input::{
    DisplayBackend, InputEvent, KeyCode, MonitorInfo, MouseButton, VideoModeInfo, WindowMode,
};
use super::state::{RenderTarget, VSEState};
use super::swapchain::SwapchainManager;
use crate::data::messages::FrameMessage;
use crate::drawing::primitives::{default_arc_segments, default_circle_segments, DrawCommand};
use crate::drawing::{
    Color, FrameRecorder, GaborParams, GratingParams, NoiseParams, RecordCtx, RegisteredPipeline,
    StimulusPipeline, TextureHandle,
};
use crate::timing::{Clock, ScanoutTimestamp, Timestamp, TimingSource};

mod drawing;
mod external;
mod host;
mod input;
mod pipeline;
mod recording;
mod timing;

pub(super) use host::capture_host_info;

/// Render context passed to the render callback
///
/// This provides access to rendering operations during the frame callback.
pub struct RenderContext<'a> {
    pub(super) state: &'a mut VSEState,
    pub(super) config: &'a mut RenderConfig,
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
}

impl<'a> RenderContext<'a> {
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
        match &self.state.target {
            RenderTarget::Present(p) => p.display_size,
            RenderTarget::Offscreen(o) => (o.extent[0], o.extent[1]),
        }
    }

    /// Get the device (for advanced users)
    pub fn device(&self) -> &Arc<vulkano::device::Device> {
        &self.state.device
    }

    /// Get the queue (for advanced users)
    pub fn queue(&self) -> &Arc<vulkano::device::Queue> {
        &self.state.queue
    }

    /// Get the swapchain manager (for advanced users).
    ///
    /// `None` in a headless session, which renders to an offscreen image and
    /// has no swapchain. For the color format alone — the usual reason to reach
    /// for this — prefer [`color_format`](Self::color_format), which answers in
    /// both modes.
    pub fn swapchain(&self) -> Option<&SwapchainManager> {
        self.state.target.present().map(|p| &p.swapchain)
    }

    /// The color format this session renders into: the negotiated swapchain
    /// format when presenting, the offscreen image's format when headless.
    ///
    /// This is the format a user pipeline must be built against, and the one a
    /// headless regeneration must match to reproduce a recorded session's bytes.
    pub fn color_format(&self) -> vulkano::format::Format {
        match &self.state.target {
            RenderTarget::Present(p) => p.swapchain.format(),
            RenderTarget::Offscreen(o) => o.format,
        }
    }

    /// Get the GPU name
    pub fn gpu_name(&self) -> &str {
        self.state.device_selector.device_name()
    }

    /// Get the selected session timing backend.
    ///
    /// An individual [`FlipInfo`](crate::timing::FlipInfo) can report `CpuEstimate` when this
    /// returns `ExtPresentTiming` if that frame lacked usable scanout evidence.
    pub fn timing_source(&self) -> TimingSource {
        match &self.state.target {
            RenderTarget::Present(p) => p.timing_provider.source(),
            RenderTarget::Offscreen(_) => TimingSource::Offscreen,
        }
    }

    /// Get VSE's host monotonic clock for host-originated events.
    ///
    /// Use the opt-in host-clock bridge before comparing these timestamps with scanout-domain
    /// `FlipInfo` values.
    pub fn clock(&self) -> &Clock {
        &self.state.clock
    }

    /// VSE's device memory allocator, for creating buffers on VSE's device
    /// (e.g. an external-frame readback target).
    pub fn memory_allocator(
        &self,
    ) -> std::sync::Arc<vulkano::memory::allocator::StandardMemoryAllocator> {
        self.state.renderer.memory_allocator()
    }

    pub fn frame_number(&self) -> u64 {
        self.state.frame_number
    }
}
