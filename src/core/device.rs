//! GPU selection and Vulkan device initialization
//!
//! This module handles Vulkan instance creation, physical device enumeration,
//! and logical device creation with appropriate queue families.

use std::sync::Arc;
use thiserror::Error;
use tracing::{debug, info, warn};
use vulkano::{
    device::{
        physical::{PhysicalDevice, PhysicalDeviceType},
        Device, DeviceCreateInfo, DeviceExtensions, DeviceFeatures, Queue, QueueCreateInfo,
        QueueFlags,
    },
    instance::{Instance, InstanceCreateFlags, InstanceCreateInfo, InstanceExtensions},
    swapchain::Surface,
    Version, VulkanLibrary,
};
use winit::window::Window;

/// Errors that can occur during device selection and initialization
#[derive(Error, Debug)]
pub enum DeviceError {
    /// No suitable Vulkan device was found
    #[error("No suitable Vulkan device found")]
    NoDeviceFound,

    /// Failed to load the Vulkan library
    #[error("Failed to load Vulkan library: {0}")]
    LibraryLoadFailed(String),

    /// Failed to create Vulkan instance
    #[error("Failed to create Vulkan instance: {0}")]
    InstanceCreationFailed(String),

    /// Failed to create logical device
    #[error("Failed to create logical device: {0}")]
    DeviceCreationFailed(String),

    /// No suitable queue family found
    #[error("No suitable queue family found for graphics operations")]
    NoSuitableQueueFamily,

    /// Vulkan error
    #[error("Vulkan error: {0}")]
    VulkanError(#[from] vulkano::VulkanError),

    /// Validated Vulkan error
    #[error("Validated Vulkan error: {0}")]
    ValidatedVulkanError(#[from] vulkano::Validated<vulkano::VulkanError>),
}

/// Preference for GPU selection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GPUPreference {
    /// Prefer discrete GPU (dedicated graphics card) - best for performance
    #[default]
    Discrete,
    /// Prefer integrated GPU - lower power consumption
    Integrated,
    /// Use first available GPU
    Any,
}

/// Device selector handles Vulkan instance and physical device selection
///
/// This struct encapsulates the Vulkan instance creation and physical device
/// selection process, providing a clean interface for GPU initialization.
///
/// # Example
///
/// ```no_run
/// use vision_stimulus_engine::core::{DeviceSelector, GPUPreference};
///
/// let selector = DeviceSelector::new(GPUPreference::Discrete)?;
/// println!("Selected GPU: {}", selector.device_name());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct DeviceSelector {
    instance: Arc<Instance>,
    physical_device: Arc<PhysicalDevice>,
    graphics_queue_family_index: u32,
}

impl DeviceSelector {
    /// Create a new device selector with the specified GPU preference
    ///
    /// This initializes the Vulkan instance and selects a physical device
    /// based on the given preference.
    ///
    /// # Arguments
    ///
    /// * `preference` - The GPU preference for device selection
    ///
    /// # Errors
    ///
    /// Returns `DeviceError` if:
    /// - The Vulkan library cannot be loaded
    /// - No suitable GPU is found
    /// - Instance creation fails
    pub fn new(preference: GPUPreference) -> Result<Self, DeviceError> {
        let library =
            VulkanLibrary::new().map_err(|e| DeviceError::LibraryLoadFailed(e.to_string()))?;

        info!("Vulkan library loaded successfully");

        // Get required extensions for windowing
        let required_extensions = InstanceExtensions {
            // These are typically needed for windowing on most platforms
            ..InstanceExtensions::empty()
        };

        let instance = Instance::new(
            library,
            InstanceCreateInfo {
                flags: InstanceCreateFlags::ENUMERATE_PORTABILITY,
                enabled_extensions: required_extensions,
                ..Default::default()
            },
        )
        .map_err(|e| DeviceError::InstanceCreationFailed(e.to_string()))?;

        info!("Vulkan instance created");

        // Select physical device
        let (physical_device, queue_family_index) =
            Self::select_physical_device(&instance, preference)?;

        let device_name = physical_device.properties().device_name.clone();
        let device_type = physical_device.properties().device_type;
        info!("Selected GPU: {} ({:?})", device_name, device_type);

        Ok(Self {
            instance,
            physical_device,
            graphics_queue_family_index: queue_family_index,
        })
    }

    /// Create a device selector for direct display mode (no window/compositor).
    ///
    /// Enables VK_KHR_display and the acquisition extensions
    /// (VK_EXT_acquire_drm_display, VK_EXT_acquire_xlib_display).
    /// Returns the selector plus the Arc<Instance> needed by the direct display
    /// acquisition code.
    #[cfg(target_os = "linux")]
    pub fn with_direct_display(
        preference: GPUPreference,
    ) -> Result<(Self, Arc<Instance>), DeviceError> {
        let library =
            VulkanLibrary::new().map_err(|e| DeviceError::LibraryLoadFailed(e.to_string()))?;

        info!("Vulkan library loaded successfully");

        let requested = InstanceExtensions {
            khr_display: true,
            ext_acquire_drm_display: true,
            ext_acquire_xlib_display: true,
            // Declared dependency of the present-timing device-extension family
            // (VUID-vkCreateDevice-ppEnabledExtensionNames-01387).
            khr_get_surface_capabilities2: true,
            ..InstanceExtensions::empty()
        };

        // Mask out extensions not supported by this Vulkan installation.
        let supported = library.supported_extensions();
        let enabled_extensions = requested.intersection(supported);

        if !enabled_extensions.khr_display {
            return Err(DeviceError::InstanceCreationFailed(
                "VK_KHR_display not supported by this Vulkan installation".to_string(),
            ));
        }

        let instance = Instance::new(
            library,
            InstanceCreateInfo {
                flags: InstanceCreateFlags::ENUMERATE_PORTABILITY,
                enabled_extensions,
                ..Default::default()
            },
        )
        .map_err(|e| DeviceError::InstanceCreationFailed(e.to_string()))?;

        info!("Vulkan instance created with direct display extensions");

        let (physical_device, queue_family_index) =
            Self::select_physical_device(&instance, preference)?;

        let device_name = physical_device.properties().device_name.clone();
        let device_type = physical_device.properties().device_type;
        info!("Selected GPU: {} ({:?})", device_name, device_type);

        let selector = Self {
            instance: Arc::clone(&instance),
            physical_device,
            graphics_queue_family_index: queue_family_index,
        };

        Ok((selector, instance))
    }

    /// Create a new device selector with surface requirements
    ///
    /// This variant ensures the selected device can present to the given window.
    pub fn with_surface(
        preference: GPUPreference,
        window: Arc<Window>,
    ) -> Result<(Self, Arc<Surface>), DeviceError> {
        let library =
            VulkanLibrary::new().map_err(|e| DeviceError::LibraryLoadFailed(e.to_string()))?;

        info!("Vulkan library loaded successfully");

        // Get required extensions for windowing
        let mut required_extensions = Surface::required_extensions(window.as_ref())
            .map_err(|e| DeviceError::InstanceCreationFailed(e.to_string()))?;
        // Declared dependency of the present-timing device-extension family
        // (VK_EXT_present_timing / VK_KHR_present_id2 / VK_KHR_present_wait2);
        // VUID-vkCreateDevice-ppEnabledExtensionNames-01387 fires without it.
        required_extensions.khr_get_surface_capabilities2 =
            library.supported_extensions().khr_get_surface_capabilities2;

        let instance = Instance::new(
            library,
            InstanceCreateInfo {
                flags: InstanceCreateFlags::ENUMERATE_PORTABILITY,
                enabled_extensions: required_extensions,
                ..Default::default()
            },
        )
        .map_err(|e| DeviceError::InstanceCreationFailed(e.to_string()))?;

        info!("Vulkan instance created with surface extensions");

        // Create surface
        let surface = Surface::from_window(instance.clone(), window)
            .map_err(|e| DeviceError::InstanceCreationFailed(e.to_string()))?;

        // Select physical device that supports the surface
        let (physical_device, queue_family_index) =
            Self::select_physical_device_with_surface(&instance, &surface, preference)?;

        let device_name = physical_device.properties().device_name.clone();
        let device_type = physical_device.properties().device_type;
        info!("Selected GPU: {} ({:?})", device_name, device_type);

        Ok((
            Self {
                instance,
                physical_device,
                graphics_queue_family_index: queue_family_index,
            },
            surface,
        ))
    }

    /// Select a physical device based on preference
    fn select_physical_device(
        instance: &Arc<Instance>,
        preference: GPUPreference,
    ) -> Result<(Arc<PhysicalDevice>, u32), DeviceError> {
        let devices: Vec<_> = instance
            .enumerate_physical_devices()
            .map_err(DeviceError::VulkanError)?
            .collect();

        if devices.is_empty() {
            return Err(DeviceError::NoDeviceFound);
        }

        debug!("Found {} physical device(s)", devices.len());

        // Score and select the best device
        let mut best_device: Option<(Arc<PhysicalDevice>, u32, i32)> = None;

        for device in devices {
            let properties = device.properties();
            debug!(
                "Evaluating device: {} ({:?})",
                properties.device_name, properties.device_type
            );

            // Find a graphics queue family
            let queue_family_index = device
                .queue_family_properties()
                .iter()
                .enumerate()
                .find(|(_, props)| props.queue_flags.contains(QueueFlags::GRAPHICS))
                .map(|(index, _)| index as u32);

            let queue_family_index = match queue_family_index {
                Some(idx) => idx,
                None => {
                    debug!("Device has no graphics queue, skipping");
                    continue;
                }
            };

            let score = Self::score_device(&device, preference);

            if let Some((_, _, best_score)) = &best_device {
                if score > *best_score {
                    best_device = Some((device, queue_family_index, score));
                }
            } else {
                best_device = Some((device, queue_family_index, score));
            }
        }

        best_device
            .map(|(device, queue_idx, _)| (device, queue_idx))
            .ok_or(DeviceError::NoDeviceFound)
    }

    /// Select a physical device that supports the given surface
    fn select_physical_device_with_surface(
        instance: &Arc<Instance>,
        surface: &Arc<Surface>,
        preference: GPUPreference,
    ) -> Result<(Arc<PhysicalDevice>, u32), DeviceError> {
        let devices: Vec<_> = instance
            .enumerate_physical_devices()
            .map_err(DeviceError::VulkanError)?
            .collect();

        if devices.is_empty() {
            return Err(DeviceError::NoDeviceFound);
        }

        debug!("Found {} physical device(s)", devices.len());

        let mut best_device: Option<(Arc<PhysicalDevice>, u32, i32)> = None;

        for device in devices {
            let properties = device.properties();
            debug!(
                "Evaluating device: {} ({:?})",
                properties.device_name, properties.device_type
            );

            // Find a queue family that supports both graphics and presentation
            let queue_family_index = device
                .queue_family_properties()
                .iter()
                .enumerate()
                .find(|(index, props)| {
                    let supports_graphics = props.queue_flags.contains(QueueFlags::GRAPHICS);
                    let supports_surface = device
                        .surface_support(*index as u32, surface)
                        .unwrap_or(false);
                    supports_graphics && supports_surface
                })
                .map(|(index, _)| index as u32);

            let queue_family_index = match queue_family_index {
                Some(idx) => idx,
                None => {
                    debug!("Device has no suitable queue family, skipping");
                    continue;
                }
            };

            let score = Self::score_device(&device, preference);

            if let Some((_, _, best_score)) = &best_device {
                if score > *best_score {
                    best_device = Some((device, queue_family_index, score));
                }
            } else {
                best_device = Some((device, queue_family_index, score));
            }
        }

        best_device
            .map(|(device, queue_idx, _)| (device, queue_idx))
            .ok_or(DeviceError::NoDeviceFound)
    }

    /// Score a device based on preference and capabilities
    fn score_device(device: &PhysicalDevice, preference: GPUPreference) -> i32 {
        let properties = device.properties();
        let device_type = properties.device_type;

        let mut score = 0;

        // Base score by device type
        match device_type {
            PhysicalDeviceType::DiscreteGpu => score += 1000,
            PhysicalDeviceType::IntegratedGpu => score += 500,
            PhysicalDeviceType::VirtualGpu => score += 100,
            PhysicalDeviceType::Cpu => score += 10,
            PhysicalDeviceType::Other => score += 1,
            _ => {}
        }

        // Adjust based on preference
        match preference {
            GPUPreference::Discrete => {
                if device_type == PhysicalDeviceType::DiscreteGpu {
                    score += 500;
                }
            }
            GPUPreference::Integrated => {
                if device_type == PhysicalDeviceType::IntegratedGpu {
                    score += 500;
                }
            }
            GPUPreference::Any => {}
        }

        // Bonus for Vulkan 1.2+ support
        if properties.api_version >= Version::V1_2 {
            score += 100;
        }

        debug!("Device {} scored {}", properties.device_name, score);

        score
    }

    /// Get the Vulkan instance
    pub fn instance(&self) -> &Arc<Instance> {
        &self.instance
    }

    /// Get the selected physical device
    pub fn physical_device(&self) -> &Arc<PhysicalDevice> {
        &self.physical_device
    }

    /// Get the graphics queue family index
    pub fn graphics_queue_family_index(&self) -> u32 {
        self.graphics_queue_family_index
    }

    /// Get the name of the selected device
    pub fn device_name(&self) -> &str {
        &self.physical_device.properties().device_name
    }

    /// Create a logical device with the necessary queues.
    ///
    /// Prefers a device with the `VK_EXT_present_timing` family enabled (created raw and
    /// adopted, since vulkano 0.35 cannot express these Vulkan 1.4 extensions). Falls back
    /// to a standard vulkano device — with CPU-estimate timing — when the extension is
    /// unavailable or its device creation fails.
    ///
    /// # Returns
    ///
    /// The logical device, its graphics queue, and — when the EXT path was taken — the
    /// present-timing sub-features that were actually enabled.
    ///
    /// # Errors
    ///
    /// Returns `DeviceError` if even the standard device creation fails.
    pub fn create_device(
        &self,
    ) -> Result<
        (
            Arc<Device>,
            Arc<Queue>,
            Option<crate::core::present_timing_ext::EnabledPresentTimingFeatures>,
        ),
        DeviceError,
    > {
        // Prefer VK_EXT_present_timing when the device advertises the required family.
        let support = crate::core::present_timing_ext::probe_support(&self.physical_device);
        if support.is_usable() {
            match unsafe {
                crate::core::present_timing_ext::create_device_with_present_timing(
                    &self.physical_device,
                    self.graphics_queue_family_index,
                    support,
                )
            } {
                Ok((device, queue, enabled)) => {
                    info!(
                        "Timing backend: ExtPresentTiming (VK_EXT_present_timing; \
                         scheduling={}, present_wait2={}, queue_priority={}, \
                         external_handles={})",
                        enabled.present_at_absolute_time,
                        enabled.present_wait2,
                        enabled.queue_priority.label(),
                        enabled.external_handles
                    );
                    return Ok((device, queue, Some(enabled)));
                }
                Err(e) => {
                    warn!(
                        "VK_EXT_present_timing device creation failed ({e}); \
                         falling back to standard device with CPU-estimate timing"
                    );
                }
            }
        } else {
            info!("VK_EXT_present_timing not available; using CPU-estimate timing");
        }

        // Standard vulkano device (CPU-estimate timing path).
        // Enable calibrated timestamps when available so the timing-capabilities probe can
        // measure CPU<->GPU clock correlation even without present-timing.
        let supported = self.physical_device.supported_extensions();
        let device_extensions = DeviceExtensions {
            khr_swapchain: true,
            khr_dynamic_rendering: true,
            ext_calibrated_timestamps: supported.ext_calibrated_timestamps,
            ..DeviceExtensions::empty()
        };

        let features = DeviceFeatures {
            dynamic_rendering: true,
            ..DeviceFeatures::empty()
        };

        let (device, mut queues) = Device::new(
            self.physical_device.clone(),
            DeviceCreateInfo {
                queue_create_infos: vec![QueueCreateInfo {
                    queue_family_index: self.graphics_queue_family_index,
                    ..Default::default()
                }],
                enabled_extensions: device_extensions,
                enabled_features: features,
                ..Default::default()
            },
        )
        .map_err(|e| DeviceError::DeviceCreationFailed(e.to_string()))?;

        let queue = queues.next().ok_or(DeviceError::NoSuitableQueueFamily)?;

        info!("Logical device created successfully");

        Ok((device, queue, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gpu_preference_default() {
        let pref = GPUPreference::default();
        assert_eq!(pref, GPUPreference::Discrete);
    }
}
