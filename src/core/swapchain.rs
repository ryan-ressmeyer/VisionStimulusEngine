//! Swapchain creation and management
//!
//! This module handles Vulkan swapchain creation, image acquisition,
//! and presentation for double/triple buffering.

use std::sync::Arc;
use thiserror::Error;
use tracing::{debug, info, warn};
use vulkano::{
    device::Device,
    image::Image,
    swapchain::{
        self, PresentMode as VkPresentMode, Surface, Swapchain, SwapchainCreateInfo,
        SwapchainPresentInfo,
    },
    sync::GpuFuture,
    Validated, VulkanError,
};

/// Errors that can occur during swapchain operations
#[derive(Error, Debug)]
pub enum SwapchainError {
    /// Failed to create swapchain
    #[error("Failed to create swapchain: {0}")]
    CreationFailed(String),

    /// Swapchain is out of date and needs recreation
    #[error("Swapchain is out of date")]
    OutOfDate,

    /// Swapchain is suboptimal
    #[error("Swapchain is suboptimal")]
    Suboptimal,

    /// Failed to acquire next image
    #[error("Failed to acquire next image: {0}")]
    AcquireFailed(String),

    /// Failed to present image
    #[error("Failed to present image: {0}")]
    PresentFailed(String),

    /// Vulkan error
    #[error("Vulkan error: {0}")]
    VulkanError(#[from] VulkanError),
}

/// Presentation mode (affects timing behavior)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PresentMode {
    /// VSync - wait for vertical blank (best for timing precision)
    ///
    /// This mode ensures frames are presented at the display's refresh rate,
    /// providing the most predictable timing for vision science experiments.
    #[default]
    Fifo,

    /// No VSync - immediate presentation (may tear)
    ///
    /// Frames are presented immediately without waiting for vertical blank.
    /// This can cause tearing but has lowest latency.
    Immediate,

    /// Mailbox - low latency with no tearing
    ///
    /// Similar to VSync but if the application is running faster than the
    /// display refresh rate, newer frames replace older queued frames.
    Mailbox,
}

impl PresentMode {
    /// Convert to Vulkan present mode
    fn to_vulkan(self) -> VkPresentMode {
        match self {
            PresentMode::Fifo => VkPresentMode::Fifo,
            PresentMode::Immediate => VkPresentMode::Immediate,
            PresentMode::Mailbox => VkPresentMode::Mailbox,
        }
    }

    /// Check if a present mode is supported and return a fallback if not
    fn select_with_fallback(self, supported: &[VkPresentMode]) -> VkPresentMode {
        let preferred = self.to_vulkan();
        if supported.contains(&preferred) {
            preferred
        } else {
            warn!(
                "Requested present mode {:?} not supported, falling back to FIFO",
                self
            );
            VkPresentMode::Fifo // FIFO is always supported
        }
    }
}

/// Swapchain configuration options
#[derive(Debug, Clone)]
pub struct SwapchainConfig {
    /// Width of swapchain images
    pub width: u32,
    /// Height of swapchain images
    pub height: u32,
    /// Presentation mode
    pub present_mode: PresentMode,
    /// Number of swapchain images (2 for double buffering, 3 for triple)
    pub image_count: u32,
}

impl Default for SwapchainConfig {
    fn default() -> Self {
        Self {
            width: 800,
            height: 600,
            present_mode: PresentMode::Fifo,
            image_count: 2, // Double buffering
        }
    }
}

/// Manages swapchain and associated resources
///
/// The SwapchainManager handles the lifecycle of the Vulkan swapchain,
/// including creation, image acquisition, and recreation when needed
/// (e.g., after window resize).
pub struct SwapchainManager {
    device: Arc<Device>,
    surface: Arc<Surface>,
    swapchain: Arc<Swapchain>,
    images: Vec<Arc<Image>>,
    config: SwapchainConfig,
    needs_recreation: bool,
}

impl SwapchainManager {
    /// Create a new swapchain manager
    ///
    /// # Arguments
    ///
    /// * `device` - The Vulkan logical device
    /// * `surface` - The window surface to present to
    /// * `config` - Swapchain configuration options
    ///
    /// # Errors
    ///
    /// Returns `SwapchainError` if swapchain creation fails.
    pub fn new(
        device: Arc<Device>,
        surface: Arc<Surface>,
        config: SwapchainConfig,
    ) -> Result<Self, SwapchainError> {
        let (swapchain, images) = Self::create_swapchain(&device, &surface, &config, None)?;

        info!(
            "Swapchain created: {}x{}, {} images, {:?}",
            config.width,
            config.height,
            images.len(),
            config.present_mode
        );

        Ok(Self {
            device,
            surface,
            swapchain,
            images,
            config,
            needs_recreation: false,
        })
    }

    /// Create or recreate the swapchain
    fn create_swapchain(
        device: &Arc<Device>,
        surface: &Arc<Surface>,
        config: &SwapchainConfig,
        old_swapchain: Option<&Arc<Swapchain>>,
    ) -> Result<(Arc<Swapchain>, Vec<Arc<Image>>), SwapchainError> {
        let surface_capabilities = device
            .physical_device()
            .surface_capabilities(surface, Default::default())
            .map_err(|e| SwapchainError::CreationFailed(e.to_string()))?;

        let surface_formats = device
            .physical_device()
            .surface_formats(surface, Default::default())
            .map_err(|e| SwapchainError::CreationFailed(e.to_string()))?;

        let present_modes = device
            .physical_device()
            .surface_present_modes(surface, Default::default())
            .map_err(|e| SwapchainError::CreationFailed(e.to_string()))?;

        // Choose format (prefer sRGB for correct color)
        let (image_format, _color_space) = surface_formats
            .iter()
            .find(|(format, _)| {
                format.numeric_format_color() == Some(vulkano::format::NumericFormat::SRGB)
            })
            .or_else(|| surface_formats.first())
            .cloned()
            .ok_or_else(|| SwapchainError::CreationFailed("No suitable format".into()))?;

        // Determine image count
        let min_image_count = surface_capabilities.min_image_count.max(config.image_count);
        let max_image_count = surface_capabilities
            .max_image_count
            .map(|max| min_image_count.min(max))
            .unwrap_or(min_image_count);

        // Determine extent
        let extent = surface_capabilities
            .current_extent
            .unwrap_or([config.width, config.height]);

        // Select present mode
        let present_mode = config.present_mode.select_with_fallback(&present_modes);

        debug!(
            "Creating swapchain: format={:?}, extent={:?}, images={}, present_mode={:?}",
            image_format, extent, max_image_count, present_mode
        );

        let create_info = SwapchainCreateInfo {
            min_image_count: max_image_count,
            image_format,
            image_extent: extent,
            image_usage: vulkano::image::ImageUsage::COLOR_ATTACHMENT
                | vulkano::image::ImageUsage::TRANSFER_DST,
            composite_alpha: surface_capabilities
                .supported_composite_alpha
                .into_iter()
                .next()
                .ok_or_else(|| SwapchainError::CreationFailed("No composite alpha mode".into()))?,
            present_mode,
            ..Default::default()
        };

        let result = if let Some(old) = old_swapchain {
            old.recreate(create_info)
        } else {
            Swapchain::new(device.clone(), surface.clone(), create_info)
        };

        result.map_err(|e| SwapchainError::CreationFailed(e.to_string()))
    }

    /// Get the swapchain images
    pub fn images(&self) -> &[Arc<Image>] {
        &self.images
    }

    /// Get the current swapchain
    pub fn swapchain(&self) -> &Arc<Swapchain> {
        &self.swapchain
    }

    /// Get the current configuration
    pub fn config(&self) -> &SwapchainConfig {
        &self.config
    }

    /// Get the swapchain image extent
    pub fn extent(&self) -> [u32; 2] {
        self.swapchain.image_extent()
    }

    /// Get the swapchain image format
    pub fn format(&self) -> vulkano::format::Format {
        self.swapchain.image_format()
    }

    /// Check if swapchain needs recreation
    pub fn needs_recreation(&self) -> bool {
        self.needs_recreation
    }

    /// Mark swapchain as needing recreation
    pub fn mark_needs_recreation(&mut self) {
        self.needs_recreation = true;
    }

    /// Acquire the next image for rendering
    ///
    /// # Returns
    ///
    /// A tuple containing:
    /// - The image index
    /// - Whether the swapchain is suboptimal
    /// - A future that completes when the image is available
    ///
    /// # Errors
    ///
    /// Returns `SwapchainError::OutOfDate` if the swapchain needs recreation.
    pub fn acquire_next_image(
        &mut self,
    ) -> Result<(u32, bool, swapchain::SwapchainAcquireFuture), SwapchainError> {
        let (image_index, suboptimal, acquire_future) =
            match swapchain::acquire_next_image(self.swapchain.clone(), None) {
                Ok(result) => result,
                Err(Validated::Error(VulkanError::OutOfDate)) => {
                    self.needs_recreation = true;
                    return Err(SwapchainError::OutOfDate);
                }
                Err(e) => return Err(SwapchainError::AcquireFailed(e.to_string())),
            };

        if suboptimal {
            self.needs_recreation = true;
        }

        Ok((image_index, suboptimal, acquire_future))
    }

    /// Recreate the swapchain with new dimensions
    ///
    /// This should be called after a window resize or when `needs_recreation` is true.
    ///
    /// # Arguments
    ///
    /// * `new_config` - The new swapchain configuration
    ///
    /// # Errors
    ///
    /// Returns `SwapchainError` if swapchain recreation fails.
    pub fn recreate(&mut self, new_config: SwapchainConfig) -> Result<(), SwapchainError> {
        let (new_swapchain, new_images) = Self::create_swapchain(
            &self.device,
            &self.surface,
            &new_config,
            Some(&self.swapchain),
        )?;

        self.swapchain = new_swapchain;
        self.images = new_images;
        self.config = new_config;
        self.needs_recreation = false;

        info!(
            "Swapchain recreated: {}x{}, {} images",
            self.config.width,
            self.config.height,
            self.images.len()
        );

        Ok(())
    }

    /// Recreate the swapchain with current dimensions from surface
    pub fn recreate_from_surface(&mut self) -> Result<(), SwapchainError> {
        let surface_capabilities = self
            .device
            .physical_device()
            .surface_capabilities(&self.surface, Default::default())
            .map_err(|e| SwapchainError::CreationFailed(e.to_string()))?;

        let extent = surface_capabilities
            .current_extent
            .unwrap_or([self.config.width, self.config.height]);

        // Don't recreate if dimensions are zero (window minimized)
        if extent[0] == 0 || extent[1] == 0 {
            return Ok(());
        }

        let new_config = SwapchainConfig {
            width: extent[0],
            height: extent[1],
            ..self.config.clone()
        };

        self.recreate(new_config)
    }

    /// Present an image to the screen
    ///
    /// # Arguments
    ///
    /// * `queue` - The queue to present on
    /// * `image_index` - The index of the image to present
    /// * `wait_future` - A future to wait on before presenting
    ///
    /// # Errors
    ///
    /// Returns `SwapchainError` if presentation fails.
    pub fn present<F>(
        &mut self,
        queue: Arc<vulkano::device::Queue>,
        image_index: u32,
        wait_future: F,
    ) -> Result<(), SwapchainError>
    where
        F: GpuFuture + 'static,
    {
        let present_info =
            SwapchainPresentInfo::swapchain_image_index(self.swapchain.clone(), image_index);

        let result = wait_future
            .then_swapchain_present(queue, present_info)
            .then_signal_fence_and_flush();

        match result {
            Ok(future) => {
                future.wait(None).ok();
                Ok(())
            }
            Err(Validated::Error(VulkanError::OutOfDate)) => {
                self.needs_recreation = true;
                Err(SwapchainError::OutOfDate)
            }
            Err(e) => Err(SwapchainError::PresentFailed(e.to_string())),
        }
    }
}
