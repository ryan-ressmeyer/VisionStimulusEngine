//! Raw FFI bindings for `VK_EXT_present_timing` (+ `VK_KHR_present_id2` /
//! `VK_KHR_present_wait2`).
//!
//! These extensions are Vulkan 1.4 (1.4.335) and postdate the `ash` 0.38 / vulkano 0.35
//! bindings the project pins, so neither crate exposes them. Rather than pull `ash` git
//! (which would compile a second, handle-incompatible `ash` into the tree), this module
//! hand-declares exactly the structs, enum values, and function pointers VSE needs, reusing
//! `ash` 0.38 for base handle types (`vk::Device`, `vk::SwapchainKHR`, …). Raw `u64` handles
//! minted here wrap cleanly into vulkano's `ash`-0.38 handle types for `Device::from_handle`
//! / `Swapchain::from_handle`.
//!
//! Layouts are transcribed verbatim from Vulkan-Headers `vulkan_core.h`
//! (`VK_EXT_PRESENT_TIMING_SPEC_VERSION == 3`). See
//! `docs/plans/2026-07-09-ext-present-timing-design.md` for the surrounding design.

#![allow(non_upper_case_globals)]
// This module is a complete FFI binding surface: some constants/structs are declared for
// correctness and forward use by the raw present path and are not all referenced yet.
#![allow(dead_code)]

use ash::vk;
use std::ffi::{c_void, CStr};
use std::os::raw::c_char;
use std::sync::Arc;
use tracing::warn;

// ─── Extension name strings ─────────────────────────────────────────────────

pub const VK_EXT_PRESENT_TIMING_EXTENSION_NAME: &[u8] = b"VK_EXT_present_timing\0";
pub const VK_KHR_PRESENT_ID_2_EXTENSION_NAME: &[u8] = b"VK_KHR_present_id2\0";
pub const VK_KHR_PRESENT_WAIT_2_EXTENSION_NAME: &[u8] = b"VK_KHR_present_wait2\0";

// ─── Structure type values (from the VkStructureType enum) ──────────────────

pub const STYPE_PHYSICAL_DEVICE_PRESENT_TIMING_FEATURES_EXT: vk::StructureType =
    vk::StructureType::from_raw(1000208000);
pub const STYPE_SWAPCHAIN_TIMING_PROPERTIES_EXT: vk::StructureType =
    vk::StructureType::from_raw(1000208001);
pub const STYPE_SWAPCHAIN_TIME_DOMAIN_PROPERTIES_EXT: vk::StructureType =
    vk::StructureType::from_raw(1000208002);
pub const STYPE_PRESENT_TIMINGS_INFO_EXT: vk::StructureType =
    vk::StructureType::from_raw(1000208003);
pub const STYPE_PRESENT_TIMING_INFO_EXT: vk::StructureType =
    vk::StructureType::from_raw(1000208004);
pub const STYPE_PAST_PRESENTATION_TIMING_INFO_EXT: vk::StructureType =
    vk::StructureType::from_raw(1000208005);
pub const STYPE_PAST_PRESENTATION_TIMING_PROPERTIES_EXT: vk::StructureType =
    vk::StructureType::from_raw(1000208006);
pub const STYPE_PAST_PRESENTATION_TIMING_EXT: vk::StructureType =
    vk::StructureType::from_raw(1000208007);
pub const STYPE_PRESENT_TIMING_SURFACE_CAPABILITIES_EXT: vk::StructureType =
    vk::StructureType::from_raw(1000208008);
pub const STYPE_SWAPCHAIN_CALIBRATED_TIMESTAMP_INFO_EXT: vk::StructureType =
    vk::StructureType::from_raw(1000208009);

pub const STYPE_PRESENT_ID_2_KHR: vk::StructureType = vk::StructureType::from_raw(1000479001);
pub const STYPE_PHYSICAL_DEVICE_PRESENT_ID_2_FEATURES_KHR: vk::StructureType =
    vk::StructureType::from_raw(1000479002);
pub const STYPE_PHYSICAL_DEVICE_PRESENT_WAIT_2_FEATURES_KHR: vk::StructureType =
    vk::StructureType::from_raw(1000480001);
pub const STYPE_PRESENT_WAIT_2_INFO_KHR: vk::StructureType =
    vk::StructureType::from_raw(1000480002);

// ─── Enum values ────────────────────────────────────────────────────────────

/// `VkResult` returned by present when the timing result queue is full.
pub const VK_ERROR_PRESENT_TIMING_QUEUE_FULL_EXT: vk::Result = vk::Result::from_raw(-1000208000);

/// `VkTimeDomainKHR` — the CPU-comparable domain VSE prefers. Value 1.
pub const TIME_DOMAIN_CLOCK_MONOTONIC: i32 = 1;
/// `VkTimeDomainKHR` — `VK_TIME_DOMAIN_PRESENT_STAGE_LOCAL_EXT`, the driver's opaque scanout
/// clock (the only present-timing domain guaranteed to exist). Bridged to `CLOCK_MONOTONIC`
/// via `vkGetCalibratedTimestampsKHR` + [`SwapchainCalibratedTimestampInfoEXT`].
pub const TIME_DOMAIN_PRESENT_STAGE_LOCAL: i32 = 1000208000;

// `VkPresentStageFlagBitsEXT`
pub const PRESENT_STAGE_QUEUE_OPERATIONS_END_BIT: u32 = 0x0000_0001;
pub const PRESENT_STAGE_REQUEST_DEQUEUED_BIT: u32 = 0x0000_0002;
/// First pixel of the image begins scanout — the "when photons start" timestamp for science.
pub const PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT_BIT: u32 = 0x0000_0004;
/// First pixel is visible on the panel (accounts for display latency).
pub const PRESENT_STAGE_IMAGE_FIRST_PIXEL_VISIBLE_BIT: u32 = 0x0000_0008;

// `VkPresentTimingInfoFlagBitsEXT`
pub const PRESENT_TIMING_INFO_PRESENT_AT_RELATIVE_TIME_BIT: u32 = 0x0000_0001;
pub const PRESENT_TIMING_INFO_PRESENT_AT_NEAREST_REFRESH_CYCLE_BIT: u32 = 0x0000_0002;

// ─── Feature structs (chained into VkDeviceCreateInfo.pNext) ────────────────

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PhysicalDevicePresentTimingFeaturesEXT {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub present_timing: vk::Bool32,
    pub present_at_absolute_time: vk::Bool32,
    pub present_at_relative_time: vk::Bool32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PhysicalDevicePresentId2FeaturesKHR {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub present_id2: vk::Bool32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PhysicalDevicePresentWait2FeaturesKHR {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub present_wait2: vk::Bool32,
}

// ─── Present scheduling (chained into VkPresentInfoKHR.pNext) ────────────────

/// `VkPresentId2KHR` — assigns monotonic present ids for correlation.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PresentId2KHR {
    pub s_type: vk::StructureType,
    pub p_next: *const c_void,
    pub swapchain_count: u32,
    pub p_present_ids: *const u64,
}

/// `VkPresentTimingInfoEXT` — per-swapchain scheduling request.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PresentTimingInfoEXT {
    pub s_type: vk::StructureType,
    pub p_next: *const c_void,
    pub flags: u32, // VkPresentTimingInfoFlagsEXT
    pub target_time: u64,
    pub time_domain_id: u64,
    pub present_stage_queries: u32, // VkPresentStageFlagsEXT
    pub target_time_domain_present_stage: u32, // VkPresentStageFlagsEXT
}

/// `VkPresentTimingsInfoEXT` — pNext head carrying the per-swapchain timing infos.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PresentTimingsInfoEXT {
    pub s_type: vk::StructureType,
    pub p_next: *const c_void,
    pub swapchain_count: u32,
    pub p_timing_infos: *const PresentTimingInfoEXT,
}

// ─── Feedback (vkGetPastPresentationTimingEXT) ──────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PresentStageTimeEXT {
    pub stage: u32, // VkPresentStageFlagsEXT
    pub time: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PastPresentationTimingEXT {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub present_id: u64,
    pub target_time: u64,
    pub present_stage_count: u32,
    pub p_present_stages: *mut PresentStageTimeEXT,
    pub time_domain: i32, // VkTimeDomainKHR
    pub time_domain_id: u64,
    pub report_complete: vk::Bool32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PastPresentationTimingInfoEXT {
    pub s_type: vk::StructureType,
    pub p_next: *const c_void,
    pub flags: u32, // VkPastPresentationTimingFlagsEXT
    pub swapchain: vk::SwapchainKHR,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PastPresentationTimingPropertiesEXT {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub timing_properties_counter: u64,
    pub time_domains_counter: u64,
    pub presentation_timing_count: u32,
    pub p_presentation_timings: *mut PastPresentationTimingEXT,
}

// ─── Time domain + refresh properties ───────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SwapchainTimeDomainPropertiesEXT {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub time_domain_count: u32,
    pub p_time_domains: *mut i32, // VkTimeDomainKHR*
    pub p_time_domain_ids: *mut u64,
}

/// `VkSwapchainCalibratedTimestampInfoEXT` — chained into a `VkCalibratedTimestampInfoKHR`
/// whose `timeDomain` is `PRESENT_STAGE_LOCAL`, so `vkGetCalibratedTimestampsKHR` samples a
/// swapchain's scanout clock alongside `CLOCK_MONOTONIC` and yields the offset that bridges
/// hardware scanout times onto the CPU clock. See `docs/clock-synchronization.md` §3.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SwapchainCalibratedTimestampInfoEXT {
    pub s_type: vk::StructureType,
    pub p_next: *const c_void,
    pub swapchain: vk::SwapchainKHR,
    pub present_stage: u32, // VkPresentStageFlagsEXT
    pub time_domain_id: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SwapchainTimingPropertiesEXT {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub refresh_duration: u64,
    pub refresh_interval: u64,
}

/// `VkPresentTimingSurfaceCapabilitiesEXT` — chained onto surface-capabilities queries to
/// learn which present stages the *current* surface/path can timestamp (compositor detection).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PresentTimingSurfaceCapabilitiesEXT {
    pub s_type: vk::StructureType,
    pub p_next: *mut c_void,
    pub present_timing_supported: vk::Bool32,
    pub present_at_absolute_time_supported: vk::Bool32,
    pub present_at_relative_time_supported: vk::Bool32,
    pub present_stage_queries: u32, // VkPresentStageFlagsEXT
}

// ─── present_wait2 ──────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PresentWait2InfoKHR {
    pub s_type: vk::StructureType,
    pub p_next: *const c_void,
    pub present_id: u64,
    pub timeout: u64,
}

// ─── Function pointer types ─────────────────────────────────────────────────

pub type PfnSetSwapchainPresentTimingQueueSize =
    unsafe extern "system" fn(vk::Device, vk::SwapchainKHR, u32) -> vk::Result;
pub type PfnGetSwapchainTimingProperties = unsafe extern "system" fn(
    vk::Device,
    vk::SwapchainKHR,
    *mut SwapchainTimingPropertiesEXT,
    *mut u64,
) -> vk::Result;
pub type PfnGetSwapchainTimeDomainProperties = unsafe extern "system" fn(
    vk::Device,
    vk::SwapchainKHR,
    *mut SwapchainTimeDomainPropertiesEXT,
    *mut u64,
) -> vk::Result;
pub type PfnGetPastPresentationTiming = unsafe extern "system" fn(
    vk::Device,
    *const PastPresentationTimingInfoEXT,
    *mut PastPresentationTimingPropertiesEXT,
) -> vk::Result;
pub type PfnWaitForPresent2 = unsafe extern "system" fn(
    vk::Device,
    vk::SwapchainKHR,
    *const PresentWait2InfoKHR,
) -> vk::Result;

/// Loaded device-level function pointers for the present-timing family.
#[derive(Clone)]
pub struct PresentTimingFns {
    pub set_queue_size: PfnSetSwapchainPresentTimingQueueSize,
    pub get_timing_properties: PfnGetSwapchainTimingProperties,
    pub get_time_domain_properties: PfnGetSwapchainTimeDomainProperties,
    pub get_past_presentation_timing: PfnGetPastPresentationTiming,
    /// `None` when `VK_KHR_present_wait2` was not enabled (present-timing can run without it).
    pub wait_for_present2: Option<PfnWaitForPresent2>,
}

impl PresentTimingFns {
    /// Load the function pointers from a logical device created with the extensions enabled.
    ///
    /// Returns `None` if any of the required `VK_EXT_present_timing` entry points is missing.
    ///
    /// # Safety
    ///
    /// The device must have been created with `VK_EXT_present_timing` enabled.
    pub unsafe fn load(device: &Arc<vulkano::device::Device>, present_wait2: bool) -> Option<Self> {
        use vulkano::VulkanObject;

        let handle = device.handle();
        let get_device_proc_addr = device.instance().fns().v1_0.get_device_proc_addr;
        let load = |name: &[u8]| -> Option<unsafe extern "system" fn()> {
            get_device_proc_addr(handle, name.as_ptr() as *const c_char)
        };

        Some(Self {
            set_queue_size: std::mem::transmute::<_, PfnSetSwapchainPresentTimingQueueSize>(load(
                b"vkSetSwapchainPresentTimingQueueSizeEXT\0",
            )?),
            get_timing_properties: std::mem::transmute::<_, PfnGetSwapchainTimingProperties>(load(
                b"vkGetSwapchainTimingPropertiesEXT\0",
            )?),
            get_time_domain_properties: std::mem::transmute::<_, PfnGetSwapchainTimeDomainProperties>(
                load(b"vkGetSwapchainTimeDomainPropertiesEXT\0")?,
            ),
            get_past_presentation_timing: std::mem::transmute::<_, PfnGetPastPresentationTiming>(
                load(b"vkGetPastPresentationTimingEXT\0")?,
            ),
            wait_for_present2: if present_wait2 {
                load(b"vkWaitForPresent2KHR\0")
                    .map(|f| std::mem::transmute::<_, PfnWaitForPresent2>(f))
            } else {
                None
            },
        })
    }
}

// ─── Capability probe ───────────────────────────────────────────────────────

/// Which present-timing-family extensions a physical device advertises.
#[derive(Debug, Clone, Copy, Default)]
pub struct PresentTimingSupport {
    pub present_timing: bool,
    pub present_id2: bool,
    pub present_wait2: bool,
}

impl PresentTimingSupport {
    /// The extensions are usable as VSE's primary timing path only if the core
    /// `VK_EXT_present_timing` and its required `VK_KHR_present_id2` are both present.
    pub fn is_usable(&self) -> bool {
        self.present_timing && self.present_id2
    }
}

/// Build a minimal `ash::Instance` backed by vulkano's already-loaded loader.
///
/// Reused for the capability probe and raw device creation.
fn build_ash_instance(instance: &Arc<vulkano::instance::Instance>) -> ash::Instance {
    use vulkano::VulkanObject;
    unsafe {
        ash::Instance::load_with(
            |name| {
                std::mem::transmute(
                    instance
                        .library()
                        .get_instance_proc_addr(instance.handle(), name.as_ptr()),
                )
            },
            instance.handle(),
        )
    }
}

/// Probe a physical device for the present-timing extension family.
///
/// vulkano's typed `supported_extensions()` silently drops names it does not know (it is
/// generated against Vulkan 1.3.281), so these 1.4 extensions must be discovered by raw
/// enumeration.
pub fn probe_support(
    physical_device: &Arc<vulkano::device::physical::PhysicalDevice>,
) -> PresentTimingSupport {
    use vulkano::VulkanObject;

    let ash_instance = build_ash_instance(physical_device.instance());
    let phys_handle = physical_device.handle();

    let props = match unsafe { ash_instance.enumerate_device_extension_properties(phys_handle) } {
        Ok(p) => p,
        Err(e) => {
            warn!(
                "enumerate_device_extension_properties failed during present-timing probe: {e:?}"
            );
            return PresentTimingSupport::default();
        }
    };

    let has = |name: &[u8]| -> bool {
        let want = CStr::from_bytes_with_nul(name).unwrap();
        props.iter().any(|p| {
            let have = unsafe { CStr::from_ptr(p.extension_name.as_ptr()) };
            have == want
        })
    };

    PresentTimingSupport {
        present_timing: has(VK_EXT_PRESENT_TIMING_EXTENSION_NAME),
        present_id2: has(VK_KHR_PRESENT_ID_2_EXTENSION_NAME),
        present_wait2: has(VK_KHR_PRESENT_WAIT_2_EXTENSION_NAME),
    }
}

// ─── Raw device creation + vulkano adoption ─────────────────────────────────

/// Which present-timing sub-features were actually enabled on the created device.
#[derive(Debug, Clone, Copy)]
pub struct EnabledPresentTimingFeatures {
    pub present_timing: bool,
    pub present_at_absolute_time: bool,
    pub present_id2: bool,
    pub present_wait2: bool,
}

/// Create a logical device with the present-timing extension family enabled, then adopt it
/// into a vulkano [`Device`](vulkano::device::Device) via `Device::from_handle`.
///
/// vulkano 0.35 cannot express these Vulkan 1.4 extensions/features, so the `VkDevice` is
/// created through raw `vkCreateDevice` (with the feature structs chained into `pNext`) and
/// handed to vulkano to own. Supported sub-features are discovered first via
/// `vkGetPhysicalDeviceFeatures2`, so we never request a feature the driver lacks.
///
/// # Safety
///
/// The physical device must advertise `VK_EXT_present_timing` and `VK_KHR_present_id2`
/// (see [`probe_support`]). The returned vulkano `Device` owns the handle.
// The feature-struct flags are read by the driver through the raw `pNext` pointer chain,
// which the borrow checker's dataflow cannot see — it reports the writes as dead.
#[allow(unused_assignments)]
pub unsafe fn create_device_with_present_timing(
    physical_device: &Arc<vulkano::device::physical::PhysicalDevice>,
    graphics_queue_family_index: u32,
    support: PresentTimingSupport,
) -> Result<
    (
        Arc<vulkano::device::Device>,
        Arc<vulkano::device::Queue>,
        EnabledPresentTimingFeatures,
    ),
    String,
> {
    use vulkano::device::{
        Device, DeviceCreateInfo as VkoDeviceCreateInfo, DeviceExtensions, DeviceFeatures,
        QueueCreateInfo,
    };
    use vulkano::VulkanObject;

    let instance = physical_device.instance();
    let ash_instance = build_ash_instance(instance);
    let phys = physical_device.handle();

    // Which extensions are advertised (dynamic_rendering may be core-promoted and hidden).
    let ext_props = ash_instance
        .enumerate_device_extension_properties(phys)
        .map_err(|e| format!("enumerate_device_extension_properties failed: {e:?}"))?;
    let has_ext = |name: &[u8]| -> bool {
        let want = CStr::from_bytes_with_nul(name).unwrap();
        ext_props
            .iter()
            .any(|p| CStr::from_ptr(p.extension_name.as_ptr()) == want)
    };
    let advertise_dynamic_rendering = has_ext(b"VK_KHR_dynamic_rendering\0");
    let advertise_calibrated = has_ext(b"VK_EXT_calibrated_timestamps\0");

    // --- Discover supported sub-features via vkGetPhysicalDeviceFeatures2 ---
    let mut dyn_render = ash::vk::PhysicalDeviceDynamicRenderingFeatures::default();
    let mut f_timing = PhysicalDevicePresentTimingFeaturesEXT {
        s_type: STYPE_PHYSICAL_DEVICE_PRESENT_TIMING_FEATURES_EXT,
        p_next: std::ptr::null_mut(),
        present_timing: 0,
        present_at_absolute_time: 0,
        present_at_relative_time: 0,
    };
    let mut f_id2 = PhysicalDevicePresentId2FeaturesKHR {
        s_type: STYPE_PHYSICAL_DEVICE_PRESENT_ID_2_FEATURES_KHR,
        p_next: std::ptr::null_mut(),
        present_id2: 0,
    };
    let mut f_wait2 = PhysicalDevicePresentWait2FeaturesKHR {
        s_type: STYPE_PHYSICAL_DEVICE_PRESENT_WAIT_2_FEATURES_KHR,
        p_next: std::ptr::null_mut(),
        present_wait2: 0,
    };

    // Chain: dyn_render -> f_timing -> f_id2 -> [f_wait2]
    if support.present_wait2 {
        f_id2.p_next = &mut f_wait2 as *mut _ as *mut c_void;
    }
    f_timing.p_next = &mut f_id2 as *mut _ as *mut c_void;
    dyn_render.p_next = &mut f_timing as *mut _ as *mut c_void;

    let mut features2 = ash::vk::PhysicalDeviceFeatures2::default();
    features2.p_next = &mut dyn_render as *mut _ as *mut c_void;
    ash_instance.get_physical_device_features2(phys, &mut features2);

    let enabled = EnabledPresentTimingFeatures {
        present_timing: f_timing.present_timing != 0,
        present_at_absolute_time: f_timing.present_at_absolute_time != 0,
        present_id2: f_id2.present_id2 != 0,
        present_wait2: support.present_wait2 && f_wait2.present_wait2 != 0,
    };

    if !enabled.present_timing || !enabled.present_id2 {
        return Err(
            "device advertises the extensions but not the presentTiming/presentId2 \
             features"
                .into(),
        );
    }

    // --- Set the enable flags on the same chain we hand to vkCreateDevice ---
    dyn_render.dynamic_rendering = ash::vk::TRUE;
    f_timing.present_timing = ash::vk::TRUE;
    f_timing.present_at_absolute_time = if enabled.present_at_absolute_time {
        ash::vk::TRUE
    } else {
        ash::vk::FALSE
    };
    f_timing.present_at_relative_time = ash::vk::FALSE;
    f_id2.present_id2 = ash::vk::TRUE;
    f_wait2.present_wait2 = if enabled.present_wait2 {
        ash::vk::TRUE
    } else {
        ash::vk::FALSE
    };

    // --- Extension name list ---
    let mut ext_names: Vec<*const c_char> = vec![b"VK_KHR_swapchain\0".as_ptr() as *const c_char];
    if advertise_dynamic_rendering {
        ext_names.push(b"VK_KHR_dynamic_rendering\0".as_ptr() as *const c_char);
    }
    ext_names.push(VK_EXT_PRESENT_TIMING_EXTENSION_NAME.as_ptr() as *const c_char);
    ext_names.push(VK_KHR_PRESENT_ID_2_EXTENSION_NAME.as_ptr() as *const c_char);
    if enabled.present_wait2 {
        ext_names.push(VK_KHR_PRESENT_WAIT_2_EXTENSION_NAME.as_ptr() as *const c_char);
    }
    if advertise_calibrated {
        ext_names.push(b"VK_EXT_calibrated_timestamps\0".as_ptr() as *const c_char);
    }

    // --- Queue + device create info ---
    let queue_priorities = [1.0f32];
    let queue_ci = ash::vk::DeviceQueueCreateInfo {
        queue_family_index: graphics_queue_family_index,
        queue_count: 1,
        p_queue_priorities: queue_priorities.as_ptr(),
        ..Default::default()
    };

    let mut device_ci = ash::vk::DeviceCreateInfo::default();
    device_ci.queue_create_info_count = 1;
    device_ci.p_queue_create_infos = &queue_ci;
    device_ci.enabled_extension_count = ext_names.len() as u32;
    device_ci.pp_enabled_extension_names = ext_names.as_ptr();
    device_ci.p_next = &dyn_render as *const _ as *const c_void;

    let ash_device = ash_instance
        .create_device(phys, &device_ci, None)
        .map_err(|e| format!("raw vkCreateDevice failed: {e:?}"))?;
    let device_handle = ash_device.handle();
    // vulkano takes ownership of the handle below; don't let ash's Drop destroy it.
    std::mem::forget(ash_device);

    // --- Adopt into vulkano. create_info must match what we actually enabled. ---
    let vko_ci = VkoDeviceCreateInfo {
        queue_create_infos: vec![QueueCreateInfo {
            queue_family_index: graphics_queue_family_index,
            ..Default::default()
        }],
        enabled_extensions: DeviceExtensions {
            khr_swapchain: true,
            khr_dynamic_rendering: advertise_dynamic_rendering,
            ext_calibrated_timestamps: advertise_calibrated,
            ..DeviceExtensions::empty()
        },
        enabled_features: DeviceFeatures {
            dynamic_rendering: true,
            ..DeviceFeatures::empty()
        },
        ..Default::default()
    };
    let (device, mut queues) = Device::from_handle(physical_device.clone(), device_handle, vko_ci);
    let queue = queues
        .next()
        .ok_or_else(|| "adopted device returned no queue".to_string())?;
    Ok((device, queue, enabled))
}

// ─── Layout guards ──────────────────────────────────────────────────────────
// Sizes/offsets must match the C ABI (`vulkan_core.h`, spec version 3) exactly — a mismatch
// silently corrupts timestamps. These assertions fail the build if the layout drifts.

const _: () = {
    use std::mem::size_of;
    // sType(4)+pad(4)+pNext(8) + 3x VkBool32(12) → padded to 32 on 64-bit.
    assert!(size_of::<PhysicalDevicePresentTimingFeaturesEXT>() == 32);
    // sType(4)/pad(4)/pNext(8)/presentId(8)/targetTime(8)/count(4)/pad(4)/ptr(8)/
    // domain(4)/pad(4)/id(8)/bool(4)/tailpad(4) = 72.
    assert!(size_of::<PastPresentationTimingEXT>() == 72);
    assert!(size_of::<PresentStageTimeEXT>() == 16);
    assert!(size_of::<PresentWait2InfoKHR>() == 32);
    // sType(4)/pad(4)/pNext(8)/swapchain(8)/presentStage(4)/pad(4)/timeDomainId(8) = 40.
    assert!(size_of::<SwapchainCalibratedTimestampInfoEXT>() == 40);
};
