//! Frame abstraction for per-frame rendering state
//!
//! This module provides a clean abstraction for managing per-frame
//! rendering state, including command buffer recording and synchronization.

use std::sync::Arc;
use thiserror::Error;
use vulkano::{
    command_buffer::{
        allocator::{CommandBufferAllocator, StandardCommandBufferAllocator},
        AutoCommandBufferBuilder, CommandBufferUsage, PrimaryAutoCommandBuffer,
        RenderingAttachmentInfo, RenderingInfo,
    },
    device::{Device, Queue},
    format::ClearValue,
    image::{view::ImageView, Image},
    memory::allocator::StandardMemoryAllocator,
    render_pass::{AttachmentLoadOp, AttachmentStoreOp},
    swapchain::SwapchainAcquireFuture,
    sync::GpuFuture,
};

/// Errors that can occur during frame operations
#[derive(Error, Debug)]
pub enum FrameError {
    /// Failed to allocate command buffer
    #[error("Failed to allocate command buffer: {0}")]
    CommandBufferAllocationFailed(String),

    /// Failed to record commands
    #[error("Failed to record commands: {0}")]
    RecordingFailed(String),

    /// Failed to execute commands
    #[error("Failed to execute commands: {0}")]
    ExecutionFailed(String),

    /// Failed to create image view
    #[error("Failed to create image view: {0}")]
    ImageViewFailed(String),
}

/// Represents a single frame of rendering
///
/// A Frame encapsulates all the state needed for a single frame,
/// including the command buffer and the image being rendered to.
pub struct Frame {
    /// The index of the swapchain image
    pub image_index: u32,
    /// The command buffer for this frame
    command_buffer: Arc<PrimaryAutoCommandBuffer>,
    /// The acquire future for synchronization
    acquire_future: SwapchainAcquireFuture,
}

impl Frame {
    /// Get the image index
    pub fn image_index(&self) -> u32 {
        self.image_index
    }

    /// Get the command buffer and acquire future
    pub fn command_buffer(self) -> (Arc<PrimaryAutoCommandBuffer>, SwapchainAcquireFuture) {
        (self.command_buffer, self.acquire_future)
    }
}

/// Frame builder for recording commands
///
/// The FrameBuilder manages command buffer allocation and provides
/// methods for recording rendering commands.
#[allow(dead_code)]
pub struct FrameBuilder {
    device: Arc<Device>,
    queue: Arc<Queue>,
    command_buffer_allocator: Arc<dyn CommandBufferAllocator>,
    memory_allocator: Arc<StandardMemoryAllocator>,
}

#[allow(dead_code)]
impl FrameBuilder {
    /// Create a new frame builder
    ///
    /// # Arguments
    ///
    /// * `device` - The Vulkan logical device
    /// * `queue` - The graphics queue
    pub fn new(device: Arc<Device>, queue: Arc<Queue>) -> Self {
        let command_buffer_allocator = Arc::new(StandardCommandBufferAllocator::new(
            device.clone(),
            Default::default(),
        ));

        let memory_allocator = Arc::new(StandardMemoryAllocator::new_default(device.clone()));

        Self {
            device,
            queue,
            command_buffer_allocator,
            memory_allocator,
        }
    }

    /// Get the device
    pub fn device(&self) -> &Arc<Device> {
        &self.device
    }

    /// Get the queue
    pub fn queue(&self) -> &Arc<Queue> {
        &self.queue
    }

    /// Get the memory allocator
    pub fn memory_allocator(&self) -> &Arc<StandardMemoryAllocator> {
        &self.memory_allocator
    }

    /// Begin recording a new frame with a clear color
    ///
    /// This creates a command buffer that clears the target image with
    /// the specified color.
    ///
    /// # Arguments
    ///
    /// * `image` - The swapchain image to render to
    /// * `image_index` - The index of the swapchain image
    /// * `acquire_future` - The future from swapchain image acquisition
    /// * `clear_color` - The RGBA clear color (values 0.0-1.0)
    ///
    /// # Errors
    ///
    /// Returns `FrameError` if command buffer creation or recording fails.
    pub fn begin_clear(
        &self,
        image: Arc<Image>,
        image_index: u32,
        acquire_future: SwapchainAcquireFuture,
        clear_color: [f32; 4],
    ) -> Result<Frame, FrameError> {
        // Create image view
        let image_view = ImageView::new_default(image.clone())
            .map_err(|e| FrameError::ImageViewFailed(e.to_string()))?;

        // Create command buffer builder
        let mut builder = AutoCommandBufferBuilder::primary(
            self.command_buffer_allocator.clone(),
            self.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .map_err(|e| FrameError::CommandBufferAllocationFailed(e.to_string()))?;

        // Begin dynamic rendering with clear
        builder
            .begin_rendering(RenderingInfo {
                color_attachments: vec![Some(RenderingAttachmentInfo {
                    load_op: AttachmentLoadOp::Clear,
                    store_op: AttachmentStoreOp::Store,
                    clear_value: Some(ClearValue::Float(clear_color)),
                    ..RenderingAttachmentInfo::image_view(image_view)
                })],
                ..Default::default()
            })
            .map_err(|e| FrameError::RecordingFailed(e.to_string()))?;

        // End rendering
        builder
            .end_rendering()
            .map_err(|e| FrameError::RecordingFailed(e.to_string()))?;

        // Build command buffer
        let command_buffer = builder
            .build()
            .map_err(|e| FrameError::RecordingFailed(e.to_string()))?;

        Ok(Frame {
            image_index,
            command_buffer,
            acquire_future,
        })
    }

    /// Execute a frame and wait for completion
    ///
    /// # Arguments
    ///
    /// * `frame` - The frame to execute
    ///
    /// # Returns
    ///
    /// A future that can be used for presentation.
    pub fn execute(&self, frame: Frame) -> Result<impl GpuFuture, FrameError> {
        let (command_buffer, acquire_future) = frame.command_buffer();

        let future = acquire_future
            .then_execute(self.queue.clone(), command_buffer)
            .map_err(|e| FrameError::ExecutionFailed(e.to_string()))?;

        Ok(future)
    }
}
