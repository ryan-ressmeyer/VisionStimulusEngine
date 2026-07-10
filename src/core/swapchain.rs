//! Swapchain creation and management
//!
//! This module handles Vulkan swapchain creation, image acquisition,
//! and presentation for double/triple buffering.

use std::sync::Arc;
use ash::vk;
use thiserror::Error;
use tracing::{debug, info, warn};
use vulkano::{
    device::Device,
    image::Image,
    swapchain::{
        self, ColorSpace, CompositeAlpha, PresentMode as VkPresentMode, Surface, Swapchain,
        SwapchainCreateInfo, SwapchainPresentInfo,
    },
    sync::GpuFuture,
    Validated, VulkanError, VulkanObject,
};

use crate::core::present_timing_ext as pt;

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

/// Image usage for VSE swapchain images: color attachment (rendered into) + transfer dst
/// (cleared / blitted). Shared verbatim by the vulkano and raw creation paths.
const SWAPCHAIN_IMAGE_USAGE: vulkano::image::ImageUsage = vulkano::image::ImageUsage::COLOR_ATTACHMENT
    .union(vulkano::image::ImageUsage::TRANSFER_DST);

/// Surface-dependent swapchain parameters resolved once and shared by the vulkano and raw
/// (present-wait2 opt-in) creation paths.
struct ResolvedParams {
    image_format: vulkano::format::Format,
    image_color_space: ColorSpace,
    image_extent: [u32; 2],
    image_count: u32,
    present_mode: VkPresentMode,
    composite_alpha: CompositeAlpha,
}

/// Build an `ash::khr::swapchain::Device` from vulkano's already-loaded device loader, for the raw
/// `vkCreateSwapchainKHR` path. Mirrors the loader pattern in `present_engine.rs`.
fn build_ash_swapchain_device(device: &Arc<Device>) -> ash::khr::swapchain::Device {
    let instance = device.instance();
    let get_dpa = instance.fns().v1_0.get_device_proc_addr;
    let dev_handle = device.handle();
    unsafe {
        let ash_instance = ash::Instance::load_with(
            |name| {
                std::mem::transmute(
                    instance
                        .library()
                        .get_instance_proc_addr(instance.handle(), name.as_ptr()),
                )
            },
            instance.handle(),
        );
        let ash_device = ash::Device::load_with(
            |name| std::mem::transmute(get_dpa(dev_handle, name.as_ptr())),
            dev_handle,
        );
        ash::khr::swapchain::Device::new(&ash_instance, &ash_device)
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
    /// When `Some`, swapchains are created through raw `vkCreateSwapchainKHR` with the
    /// present-id2 / present-wait2 opt-in flags set (see [`pt::SWAPCHAIN_CREATE_PRESENT_WAIT_2_BIT_KHR`]),
    /// then adopted via [`Swapchain::from_handle`]. vulkano's `Swapchain::new` predates Vulkan 1.4
    /// and cannot set these flags, so `vkWaitForPresent2KHR` would be UB (a driver crash) on a
    /// vulkano-created swapchain. `None` on the CPU-estimate path, which keeps the vulkano path.
    raw_present: Option<ash::khr::swapchain::Device>,
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
        Self::new_with_present_opt_in(device, surface, config, false)
    }

    /// Create a swapchain manager, optionally opting swapchains into present-id2 / present-wait2.
    ///
    /// When `present_opt_in` is `true` (the EXT present-timing backend with `presentWait2`
    /// enabled), every swapchain is created through a raw `vkCreateSwapchainKHR` with the
    /// [`SWAPCHAIN_CREATE_PRESENT_WAIT_2_BIT_KHR`](pt::SWAPCHAIN_CREATE_PRESENT_WAIT_2_BIT_KHR)
    /// opt-in so `vkWaitForPresent2KHR` is legal on it. Otherwise the vulkano path is used.
    pub fn new_with_present_opt_in(
        device: Arc<Device>,
        surface: Arc<Surface>,
        config: SwapchainConfig,
        present_opt_in: bool,
    ) -> Result<Self, SwapchainError> {
        let raw_present = if present_opt_in {
            Some(build_ash_swapchain_device(&device))
        } else {
            None
        };

        let (swapchain, images) =
            Self::create_swapchain(&device, &surface, &config, None, raw_present.as_ref())?;

        info!(
            "Swapchain created: {}x{}, {} images, {:?}, present_wait2_opt_in={}",
            config.width,
            config.height,
            images.len(),
            config.present_mode,
            raw_present.is_some(),
        );

        Ok(Self {
            device,
            surface,
            swapchain,
            images,
            config,
            needs_recreation: false,
            raw_present,
        })
    }

    /// Whether the current swapchain was created with the present-wait2 opt-in flag, i.e. whether
    /// `vkWaitForPresent2KHR` is legal on it. The synchronous `flip()` path must check this before
    /// calling `wait_for_present` — the call is UB (a driver crash) on a swapchain created without
    /// the flag.
    pub fn present_wait2_enabled(&self) -> bool {
        self.raw_present.is_some()
    }

    /// Resolve the surface-dependent swapchain parameters shared by the vulkano and raw paths
    /// (format, color space, extent, image count, present mode, composite alpha).
    fn resolve_params(
        device: &Arc<Device>,
        surface: &Arc<Surface>,
        config: &SwapchainConfig,
    ) -> Result<ResolvedParams, SwapchainError> {
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
        let (image_format, image_color_space) = surface_formats
            .iter()
            .find(|(format, _)| {
                format.numeric_format_color() == Some(vulkano::format::NumericFormat::SRGB)
            })
            .or_else(|| surface_formats.first())
            .cloned()
            .ok_or_else(|| SwapchainError::CreationFailed("No suitable format".into()))?;

        // Determine image count
        let min_image_count = surface_capabilities.min_image_count.max(config.image_count);
        let image_count = surface_capabilities
            .max_image_count
            .map(|max| min_image_count.min(max))
            .unwrap_or(min_image_count);

        // Determine extent
        let image_extent = surface_capabilities
            .current_extent
            .unwrap_or([config.width, config.height]);

        // Select present mode
        let present_mode = config.present_mode.select_with_fallback(&present_modes);

        let composite_alpha = surface_capabilities
            .supported_composite_alpha
            .into_iter()
            .next()
            .ok_or_else(|| SwapchainError::CreationFailed("No composite alpha mode".into()))?;

        Ok(ResolvedParams {
            image_format,
            image_color_space,
            image_extent,
            image_count,
            present_mode,
            composite_alpha,
        })
    }

    /// Create or recreate the swapchain, dispatching to the raw present-wait2 path when opted in.
    fn create_swapchain(
        device: &Arc<Device>,
        surface: &Arc<Surface>,
        config: &SwapchainConfig,
        old_swapchain: Option<&Arc<Swapchain>>,
        raw_present: Option<&ash::khr::swapchain::Device>,
    ) -> Result<(Arc<Swapchain>, Vec<Arc<Image>>), SwapchainError> {
        let params = Self::resolve_params(device, surface, config)?;

        debug!(
            "Creating swapchain: format={:?}, extent={:?}, images={}, present_mode={:?}, raw={}",
            params.image_format,
            params.image_extent,
            params.image_count,
            params.present_mode,
            raw_present.is_some(),
        );

        if let Some(raw) = raw_present {
            return Self::create_swapchain_raw(device, surface, old_swapchain, raw, &params);
        }

        let create_info = SwapchainCreateInfo {
            min_image_count: params.image_count,
            image_format: params.image_format,
            image_color_space: params.image_color_space,
            image_extent: params.image_extent,
            image_usage: SWAPCHAIN_IMAGE_USAGE,
            composite_alpha: params.composite_alpha,
            present_mode: params.present_mode,
            ..Default::default()
        };

        let result = if let Some(old) = old_swapchain {
            old.recreate(create_info)
        } else {
            Swapchain::new(device.clone(), surface.clone(), create_info)
        };

        result.map_err(|e| SwapchainError::CreationFailed(e.to_string()))
    }

    /// Create the swapchain through raw `vkCreateSwapchainKHR` with the present-id2 / present-wait2
    /// opt-in flags, then adopt the handle + its images into a vulkano [`Swapchain`] via
    /// [`Swapchain::from_handle`] (which takes ownership and destroys the handle on drop).
    ///
    /// The vulkano `SwapchainCreateInfo` passed to `from_handle` is metadata only (it is not
    /// re-validated against the handle), so its `flags` stay empty — the real 1.4 flags live only
    /// in the raw `VkSwapchainCreateInfoKHR` below, which vulkano cannot express.
    fn create_swapchain_raw(
        device: &Arc<Device>,
        surface: &Arc<Surface>,
        old_swapchain: Option<&Arc<Swapchain>>,
        raw: &ash::khr::swapchain::Device,
        params: &ResolvedParams,
    ) -> Result<(Arc<Swapchain>, Vec<Arc<Image>>), SwapchainError> {
        let create_flags = vk::SwapchainCreateFlagsKHR::from_raw(
            pt::SWAPCHAIN_CREATE_PRESENT_ID_2_BIT_KHR | pt::SWAPCHAIN_CREATE_PRESENT_WAIT_2_BIT_KHR,
        );
        let old_handle = old_swapchain
            .map(|s| s.handle())
            .unwrap_or_else(vk::SwapchainKHR::null);

        let raw_ci = vk::SwapchainCreateInfoKHR::default()
            .flags(create_flags)
            .surface(surface.handle())
            .min_image_count(params.image_count)
            .image_format(params.image_format.into())
            .image_color_space(params.image_color_space.into())
            .image_extent(vk::Extent2D {
                width: params.image_extent[0],
                height: params.image_extent[1],
            })
            .image_array_layers(1)
            .image_usage(SWAPCHAIN_IMAGE_USAGE.into())
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(vk::SurfaceTransformFlagsKHR::IDENTITY)
            .composite_alpha(params.composite_alpha.into())
            .present_mode(params.present_mode.into())
            .clipped(true)
            .old_swapchain(old_handle);

        // SAFETY: all handles belong to `device`; `raw_ci` outlives the call; the returned handle
        // is handed to vulkano below, which becomes its sole owner.
        let handle = unsafe { raw.create_swapchain(&raw_ci, None) }.map_err(|e| {
            SwapchainError::CreationFailed(format!("raw vkCreateSwapchainKHR failed: {e:?}"))
        })?;
        let image_handles = unsafe { raw.get_swapchain_images(handle) }.map_err(|e| {
            SwapchainError::CreationFailed(format!("vkGetSwapchainImagesKHR failed: {e:?}"))
        })?;

        // Metadata mirroring the raw create info so vulkano's image wrappers are correct.
        let vko_ci = SwapchainCreateInfo {
            min_image_count: params.image_count,
            image_format: params.image_format,
            image_color_space: params.image_color_space,
            image_extent: params.image_extent,
            image_usage: SWAPCHAIN_IMAGE_USAGE,
            composite_alpha: params.composite_alpha,
            present_mode: params.present_mode,
            ..Default::default()
        };

        // SAFETY: `handle` and `image_handles` were just created from `device`/`surface` with the
        // matching create info; vulkano takes ownership of both.
        unsafe {
            Swapchain::from_handle(device.clone(), handle, image_handles, surface.clone(), vko_ci)
        }
        .map_err(|e| SwapchainError::CreationFailed(format!("Swapchain::from_handle failed: {e}")))
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
            self.raw_present.as_ref(),
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

    /// Recreate the swapchain with current dimensions from surface.
    ///
    /// `window_size` is used as the fallback extent when the Vulkan surface
    /// does not report a `current_extent` (common on Wayland for new surfaces
    /// and immediately after going fullscreen). Pass `window.inner_size()`.
    pub fn recreate_from_surface(&mut self, window_size: [u32; 2]) -> Result<(), SwapchainError> {
        let surface_capabilities = self
            .device
            .physical_device()
            .surface_capabilities(&self.surface, Default::default())
            .map_err(|e| SwapchainError::CreationFailed(e.to_string()))?;

        let extent = surface_capabilities.current_extent.unwrap_or(window_size);

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

    /// Submit a frame to the GPU without blocking on fence completion.
    ///
    /// Returns a `Box<dyn InFlightFuture>` that must be kept alive until the frame
    /// is confirmed. Dropping it would trigger an implicit wait and defeat pipelining.
    ///
    /// Used exclusively by `run_buffered()`. For synchronous rendering use [`present()`].
    ///
    /// # Errors
    ///
    /// Returns `SwapchainError::PresentFailed` if submission fails.
    /// Returns `SwapchainError::OutOfDate` if the swapchain needs recreation.
    pub(crate) fn submit_nonblocking<F>(
        &mut self,
        queue: Arc<vulkano::device::Queue>,
        image_index: u32,
        wait_future: F,
    ) -> Result<Box<dyn crate::core::buffered::InFlightFuture>, SwapchainError>
    where
        F: GpuFuture + 'static,
    {
        use crate::core::buffered::InFlightFuture;
        use std::time::Duration;

        let present_info =
            SwapchainPresentInfo::swapchain_image_index(self.swapchain.clone(), image_index);

        let fence = wait_future
            .then_swapchain_present(queue, present_info)
            .then_signal_fence_and_flush()
            .map_err(|e| {
                if matches!(e, Validated::Error(VulkanError::OutOfDate)) {
                    self.needs_recreation = true;
                    SwapchainError::OutOfDate
                } else {
                    SwapchainError::PresentFailed(e.to_string())
                }
            })?;

        struct VulkanoFence<F: GpuFuture>(vulkano::sync::future::FenceSignalFuture<F>);

        impl<F: GpuFuture + 'static> InFlightFuture for VulkanoFence<F> {
            fn is_complete(&self) -> bool {
                self.0.wait(Some(Duration::ZERO)).is_ok()
            }
            fn wait_blocking(&self) {
                let _ = self.0.wait(None);
            }
        }

        Ok(Box::new(VulkanoFence(fence)))
    }

    /// Ensure the swapchain has at least `min_count` images.
    ///
    /// If the current swapchain already has enough images this is a no-op.
    /// Otherwise the swapchain is recreated with the requested count.
    ///
    /// Used by `run_buffered()` to match the swapchain image count to the
    /// pipeline depth.
    ///
    /// # Errors
    ///
    /// Returns `SwapchainError` if recreation fails.
    pub fn ensure_image_count(&mut self, min_count: u32) -> Result<(), SwapchainError> {
        if self.images.len() as u32 >= min_count {
            return Ok(());
        }
        let new_config = SwapchainConfig {
            image_count: min_count,
            ..self.config.clone()
        };
        self.recreate(new_config)
    }
}
