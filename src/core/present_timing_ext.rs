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

// `VkSwapchainCreateFlagBitsKHR` — the present-id2 / present-wait2 per-swapchain opt-ins. A
// swapchain **must** be created with `PRESENT_WAIT_2_BIT` set in `VkSwapchainCreateInfoKHR.flags`
// for `vkWaitForPresent2KHR` to be legal on it (spec valid usage) — otherwise the call is UB and
// crashes inside the driver. `PRESENT_ID_2_BIT` is the matching opt-in for `VkPresentId2KHR`.
// vulkano 0.35's `SwapchainCreateFlags` predates Vulkan 1.4 and cannot express either, so the
// swapchain is created through raw `vkCreateSwapchainKHR` with these ORed into `flags`.
pub const SWAPCHAIN_CREATE_PRESENT_ID_2_BIT_KHR: u32 = 0x0000_0040;
pub const SWAPCHAIN_CREATE_PRESENT_WAIT_2_BIT_KHR: u32 = 0x0000_0080;

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

// ─── Present pNext chain builder (attached to vkQueuePresentKHR) ─────────────

/// Present stages requested on every timed present. `IMAGE_FIRST_PIXEL_OUT` is the
/// scientifically meaningful scanout-begin time; the earlier pipeline stages
/// (`QUEUE_OPERATIONS_END`, `REQUEST_DEQUEUED`) are requested too so the driver records *some*
/// timing even where the compositor cannot report true scanout (e.g. windowed Wayland), which
/// keeps the feedback path exercised off the direct-display TTY.
pub const REQUESTED_PRESENT_STAGES: u32 = PRESENT_STAGE_QUEUE_OPERATIONS_END_BIT
    | PRESENT_STAGE_REQUEST_DEQUEUED_BIT
    | PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT_BIT
    | PRESENT_STAGE_IMAGE_FIRST_PIXEL_VISIBLE_BIT;

/// Owns the `pNext` chain attached to `vkQueuePresentKHR` for a single frame:
/// a [`PresentId2KHR`] (frame correlation) followed by [`PresentTimingsInfoEXT`] →
/// [`PresentTimingInfoEXT`] (so the driver actually records scanout timing — the feedback
/// query returns **empty** without this).
///
/// The chain is self-referential (each struct's `p_next` / array pointers point into sibling
/// fields), so it **must stay pinned** — it is always heap-allocated (`Box`) and must outlive
/// the `queue_present` call. Moving it invalidates the interior pointers.
pub struct PresentChain {
    present_ids: [u64; 1],
    timing_infos: [PresentTimingInfoEXT; 1],
    timings: PresentTimingsInfoEXT,
    present_id2: PresentId2KHR,
}

impl PresentChain {
    /// Build an **unscheduled** present chain (`targetTime = 0`) requesting scanout timing for
    /// the `IMAGE_FIRST_PIXEL_OUT` and `IMAGE_FIRST_PIXEL_VISIBLE` stages, tagged with
    /// `present_id` for correlation. Presents at the next opportunity (VSync-locked).
    pub fn unscheduled(present_id: u64) -> Box<Self> {
        Self::build(present_id, 0, 0, 0)
    }

    /// Build a **scheduled** present chain: request that the frame's `IMAGE_FIRST_PIXEL_OUT`
    /// scanout hit at absolute time `target_time_ns` in the time domain `time_domain_id` (the
    /// swapchain's `PRESENT_STAGE_LOCAL` domain). `flags = 0` selects **absolute** scheduling
    /// (requires the `presentAtAbsoluteTime` feature); the driver presents at the first refresh
    /// cycle whose scanout is at or after the target. Still requests all scanout stages for
    /// feedback. Tagged with `present_id` for correlation.
    pub fn scheduled(present_id: u64, target_time_ns: u64, time_domain_id: u64) -> Box<Self> {
        Self::build(
            present_id,
            target_time_ns,
            time_domain_id,
            PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT_BIT,
        )
    }

    /// Shared constructor for [`unscheduled`](Self::unscheduled) / [`scheduled`](Self::scheduled):
    /// allocate the pinned chain and wire its interior pointers. `target_time_ns == 0` (with
    /// `target_stage == 0`) is the unscheduled case.
    fn build(
        present_id: u64,
        target_time_ns: u64,
        time_domain_id: u64,
        target_stage: u32,
    ) -> Box<Self> {
        let mut chain = Box::new(Self {
            present_ids: [present_id],
            timing_infos: [PresentTimingInfoEXT {
                s_type: STYPE_PRESENT_TIMING_INFO_EXT,
                p_next: std::ptr::null(),
                flags: 0, // absolute scheduling (or unscheduled when target_time == 0)
                target_time: target_time_ns,
                time_domain_id,
                present_stage_queries: REQUESTED_PRESENT_STAGES,
                target_time_domain_present_stage: target_stage,
            }],
            timings: PresentTimingsInfoEXT {
                s_type: STYPE_PRESENT_TIMINGS_INFO_EXT,
                p_next: std::ptr::null(),
                swapchain_count: 1,
                p_timing_infos: std::ptr::null(),
            },
            present_id2: PresentId2KHR {
                s_type: STYPE_PRESENT_ID_2_KHR,
                p_next: std::ptr::null(),
                swapchain_count: 1,
                p_present_ids: std::ptr::null(),
            },
        });

        // Wire the interior pointers now that the box has a stable heap address. Order:
        // VkPresentInfoKHR.pNext → present_id2 → timings → timing_infos[0].
        chain.timings.p_timing_infos = chain.timing_infos.as_ptr();
        chain.present_id2.p_present_ids = chain.present_ids.as_ptr();
        chain.present_id2.p_next = &chain.timings as *const _ as *const c_void;
        chain
    }

    /// Pointer to the chain head, for `VkPresentInfoKHR::p_next`.
    pub fn head(&self) -> *const c_void {
        &self.present_id2 as *const _ as *const c_void
    }

    /// The `VkPresentId2` value carried by this chain.
    pub fn present_id(&self) -> u64 {
        self.present_ids[0]
    }
}

// ─── Feedback parsing (vkGetPastPresentationTimingEXT results) ───────────────

/// Parsed scanout timing for one presented frame (one [`PastPresentationTimingEXT`] record).
///
/// Times are in the record's own `time_domain` (normally `PRESENT_STAGE_LOCAL`, driver-epoch
/// nanoseconds); rebasing to a [`ScanoutTimestamp`](crate::timing::ScanoutTimestamp) via the
/// session `ScanoutClock` is B3's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanoutFeedback {
    pub present_id: u64,
    pub target_time: u64,
    /// `IMAGE_FIRST_PIXEL_OUT` scanout-begin time — the "when photons start" timestamp.
    pub first_pixel_out_ns: Option<u64>,
    /// `IMAGE_FIRST_PIXEL_VISIBLE` time (accounts for display latency), if reported.
    pub first_pixel_visible_ns: Option<u64>,
    pub time_domain: i32,
    pub time_domain_id: u64,
    pub report_complete: bool,
}

/// Time of a specific present stage within a record's stage array, if present.
fn stage_time(stages: &[PresentStageTimeEXT], stage_bit: u32) -> Option<u64> {
    stages
        .iter()
        .find(|s| s.stage & stage_bit != 0)
        .map(|s| s.time)
}

/// Build a [`ScanoutFeedback`] from a record's scalar fields and its (already length-bounded)
/// stage slice. Pure over the slice — the unsafe pointer-following that produces `stages` lives
/// in the `vkGetPastPresentationTimingEXT` call site.
pub fn feedback_from_record(
    rec: &PastPresentationTimingEXT,
    stages: &[PresentStageTimeEXT],
) -> ScanoutFeedback {
    ScanoutFeedback {
        present_id: rec.present_id,
        target_time: rec.target_time,
        first_pixel_out_ns: stage_time(stages, PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT_BIT),
        first_pixel_visible_ns: stage_time(stages, PRESENT_STAGE_IMAGE_FIRST_PIXEL_VISIBLE_BIT),
        time_domain: rec.time_domain,
        time_domain_id: rec.time_domain_id,
        report_complete: rec.report_complete != 0,
    }
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

/// Outcome of requesting elevated queue global priority (`VK_KHR_global_priority`)
/// at device creation. Elevated priority lets the kernel GPU scheduler preempt
/// other contexts (e.g. an external renderer's device) so VSE's composite+present
/// submissions meet their deadline — the same mechanism VR compositors use.
///
/// Recorded (never assumed) per VSE's driver-conformance posture: a run's host
/// snapshot says what the driver actually granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuePriorityOutcome {
    /// `HIGH` global priority requested and device creation succeeded.
    HighGranted,
    /// The chain was attempted but `vkCreateDevice` failed with this result;
    /// the device was re-created without elevated priority.
    Denied(ash::vk::Result),
    /// `VK_KHR_global_priority` not advertised, or `HIGH` not offered for the
    /// graphics queue family.
    Unavailable,
    /// This code path does not attempt elevation (e.g. the non-EXT fallback
    /// device, which vulkano creates and cannot chain the struct).
    NotAttempted,
}

impl QueuePriorityOutcome {
    /// Stable string for host-info JSON snapshots.
    pub fn label(&self) -> String {
        match self {
            QueuePriorityOutcome::HighGranted => "high_granted".into(),
            QueuePriorityOutcome::Denied(e) => format!("denied({e:?})"),
            QueuePriorityOutcome::Unavailable => "unavailable".into(),
            QueuePriorityOutcome::NotAttempted => "not_attempted".into(),
        }
    }
}

/// Which present-timing sub-features were actually enabled on the created device.
#[derive(Debug, Clone, Copy)]
pub struct EnabledPresentTimingFeatures {
    pub present_timing: bool,
    pub present_at_absolute_time: bool,
    pub present_id2: bool,
    pub present_wait2: bool,
    /// Whether the queue was created at elevated global priority (see
    /// [`QueuePriorityOutcome`]).
    pub queue_priority: QueuePriorityOutcome,
    /// `VK_KHR_external_memory_fd` + `VK_KHR_external_semaphore_fd` enabled —
    /// required to import an external renderer's image ring (see
    /// `crate::core::external_frame`).
    pub external_handles: bool,
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
    // External-renderer handoff: importing another device's image ring + semaphores.
    // The base VK_KHR_external_memory / VK_KHR_external_semaphore are core since 1.1.
    let advertise_external_handles =
        has_ext(b"VK_KHR_external_memory_fd\0") && has_ext(b"VK_KHR_external_semaphore_fd\0");

    // Queue QoS: does the driver advertise global priority, and is HIGH offered for our
    // queue family? Checked up front so a denial is a driver decision, not a blind guess.
    let advertise_global_priority = has_ext(b"VK_KHR_global_priority\0");
    let high_priority_offered = advertise_global_priority && {
        let count =
            ash_instance.get_physical_device_queue_family_properties2_len(phys);
        let mut prio_props = ash::vk::QueueFamilyGlobalPriorityPropertiesKHR::default();
        let mut props2: Vec<ash::vk::QueueFamilyProperties2> =
            (0..count).map(|_| Default::default()).collect();
        if let Some(entry) = props2.get_mut(graphics_queue_family_index as usize) {
            entry.p_next = &mut prio_props as *mut _ as *mut c_void;
        }
        ash_instance.get_physical_device_queue_family_properties2(phys, &mut props2);
        prio_props.priorities[..prio_props.priority_count as usize]
            .contains(&ash::vk::QueueGlobalPriorityKHR::HIGH)
    };

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

    let mut enabled = EnabledPresentTimingFeatures {
        present_timing: f_timing.present_timing != 0,
        present_at_absolute_time: f_timing.present_at_absolute_time != 0,
        present_id2: f_id2.present_id2 != 0,
        present_wait2: support.present_wait2 && f_wait2.present_wait2 != 0,
        queue_priority: if high_priority_offered {
            // Provisional; downgraded to Denied if creation fails below.
            QueuePriorityOutcome::HighGranted
        } else {
            QueuePriorityOutcome::Unavailable
        },
        external_handles: advertise_external_handles,
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
    if advertise_external_handles {
        ext_names.push(b"VK_KHR_external_memory_fd\0".as_ptr() as *const c_char);
        ext_names.push(b"VK_KHR_external_semaphore_fd\0".as_ptr() as *const c_char);
    }
    if high_priority_offered {
        ext_names.push(b"VK_KHR_global_priority\0".as_ptr() as *const c_char);
    }

    // --- Queue + device create info ---
    let queue_priorities = [1.0f32];
    let queue_prio = ash::vk::DeviceQueueGlobalPriorityCreateInfoKHR::default()
        .global_priority(ash::vk::QueueGlobalPriorityKHR::HIGH);
    let mut queue_ci = ash::vk::DeviceQueueCreateInfo {
        queue_family_index: graphics_queue_family_index,
        queue_count: 1,
        p_queue_priorities: queue_priorities.as_ptr(),
        ..Default::default()
    };
    if high_priority_offered {
        queue_ci.p_next = &queue_prio as *const _ as *const c_void;
    }

    let mut device_ci = ash::vk::DeviceCreateInfo::default();
    device_ci.queue_create_info_count = 1;
    device_ci.p_queue_create_infos = &queue_ci;
    device_ci.enabled_extension_count = ext_names.len() as u32;
    device_ci.pp_enabled_extension_names = ext_names.as_ptr();
    device_ci.p_next = &dyn_render as *const _ as *const c_void;

    let ash_device = match ash_instance.create_device(phys, &device_ci, None) {
        Ok(d) => d,
        // A denial of elevated queue priority (typically ERROR_NOT_PERMITTED_KHR) must not
        // cost the timing backend: retry once without the chain and record the denial.
        Err(e) if high_priority_offered => {
            warn!(
                "queue global priority HIGH denied ({e:?}); retrying at default priority — \
                 present-deadline QoS reduced"
            );
            enabled.queue_priority = QueuePriorityOutcome::Denied(e);
            queue_ci.p_next = std::ptr::null();
            ext_names.retain(|p| {
                CStr::from_ptr(*p) != CStr::from_bytes_with_nul_unchecked(b"VK_KHR_global_priority\0")
            });
            device_ci.enabled_extension_count = ext_names.len() as u32;
            device_ci.pp_enabled_extension_names = ext_names.as_ptr();
            ash_instance
                .create_device(phys, &device_ci, None)
                .map_err(|e| format!("raw vkCreateDevice failed: {e:?}"))?
        }
        Err(e) => return Err(format!("raw vkCreateDevice failed: {e:?}")),
    };
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
            // Mirrored so vulkano's validated DeviceMemory::import / Semaphore::import_fd
            // accept the external-frame ring (crate::core::external_frame).
            khr_external_memory_fd: advertise_external_handles,
            khr_external_semaphore_fd: advertise_external_handles,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    #[test]
    fn queue_priority_outcome_labels_are_stable() {
        // These strings land in HostInfo JSON snapshots; changing them breaks
        // downstream analysis of a run's timing pedigree.
        assert_eq!(QueuePriorityOutcome::HighGranted.label(), "high_granted");
        assert_eq!(
            QueuePriorityOutcome::Denied(ash::vk::Result::ERROR_NOT_PERMITTED_KHR).label(),
            "denied(ERROR_NOT_PERMITTED_KHR)"
        );
        assert_eq!(QueuePriorityOutcome::Unavailable.label(), "unavailable");
        assert_eq!(QueuePriorityOutcome::NotAttempted.label(), "not_attempted");
    }

    #[test]
    fn swapchain_present_opt_in_flag_bits_match_spec() {
        // VkSwapchainCreateFlagBitsKHR (vulkan_core.h): getting these wrong silently drops the
        // present-wait2 opt-in and re-introduces the driver segfault, so pin the exact values.
        assert_eq!(SWAPCHAIN_CREATE_PRESENT_ID_2_BIT_KHR, 0x40);
        assert_eq!(SWAPCHAIN_CREATE_PRESENT_WAIT_2_BIT_KHR, 0x80);
    }

    #[test]
    fn present_chain_wires_pnext_id_and_stage_queries() {
        let chain = PresentChain::unscheduled(42);

        // Head is the PresentId2KHR, which chains to the PresentTimingsInfoEXT.
        assert_eq!(
            chain.head(),
            &chain.present_id2 as *const _ as *const c_void
        );
        assert_eq!(
            chain.present_id2.p_next,
            &chain.timings as *const _ as *const c_void
        );
        assert_eq!(chain.timings.p_next, ptr::null());

        // present_id2 points at the [present_id] slot and carries the id.
        assert_eq!(chain.present_id2.p_present_ids, chain.present_ids.as_ptr());
        assert_eq!(chain.present_id2.swapchain_count, 1);
        assert_eq!(chain.present_id(), 42);
        assert_eq!(unsafe { *chain.present_id2.p_present_ids }, 42);

        // timings points at the single per-swapchain timing info.
        assert_eq!(chain.timings.p_timing_infos, chain.timing_infos.as_ptr());
        assert_eq!(chain.timings.swapchain_count, 1);

        // The per-swapchain timing info requests both scanout stages, unscheduled.
        let ti = &chain.timing_infos[0];
        assert_eq!(ti.present_stage_queries, REQUESTED_PRESENT_STAGES);
        // The scanout-begin stage (the one that matters for science) must be requested.
        assert_ne!(
            ti.present_stage_queries & PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT_BIT,
            0
        );
        assert_eq!(ti.target_time, 0);
        assert_eq!(ti.flags, 0);

        // Correct sTypes throughout.
        assert_eq!(chain.present_id2.s_type, STYPE_PRESENT_ID_2_KHR);
        assert_eq!(chain.timings.s_type, STYPE_PRESENT_TIMINGS_INFO_EXT);
        assert_eq!(ti.s_type, STYPE_PRESENT_TIMING_INFO_EXT);
    }

    #[test]
    fn scheduled_present_chain_sets_target_time_domain_and_stage() {
        let target_ns: u64 = 29_714_123_456_789;
        let domain_id: u64 = 7;
        let chain = PresentChain::scheduled(99, target_ns, domain_id);

        let ti = &chain.timing_infos[0];
        // Absolute scheduling: flags stay 0 (no relative / nearest-refresh bits).
        assert_eq!(ti.flags, 0);
        assert_eq!(ti.target_time, target_ns);
        assert_eq!(ti.time_domain_id, domain_id);
        // The target refers to the scanout-begin stage — when photons should start.
        assert_eq!(
            ti.target_time_domain_present_stage,
            PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT_BIT
        );
        // Feedback is still requested for all stages, and the id still carried.
        assert_eq!(ti.present_stage_queries, REQUESTED_PRESENT_STAGES);
        assert_eq!(chain.present_id(), 99);
        // Interior pointers wired the same as the unscheduled chain.
        assert_eq!(chain.timings.p_timing_infos, chain.timing_infos.as_ptr());
        assert_eq!(chain.present_id2.p_present_ids, chain.present_ids.as_ptr());
    }

    #[test]
    fn unscheduled_present_chain_has_zero_target() {
        let chain = PresentChain::unscheduled(3);
        let ti = &chain.timing_infos[0];
        assert_eq!(ti.target_time, 0);
        assert_eq!(ti.time_domain_id, 0);
        assert_eq!(ti.target_time_domain_present_stage, 0);
    }

    #[test]
    fn present_chain_survives_being_boxed_and_moved() {
        // The Box must keep interior pointers valid even though `unscheduled` returns by move.
        let chain = PresentChain::unscheduled(7);
        // Pointer targets must land inside the heap allocation, not a stale stack frame.
        assert_eq!(chain.timings.p_timing_infos, chain.timing_infos.as_ptr());
        assert_eq!(chain.present_id2.p_present_ids, chain.present_ids.as_ptr());
    }

    fn record(present_id: u64, domain: i32, domain_id: u64) -> PastPresentationTimingEXT {
        PastPresentationTimingEXT {
            s_type: STYPE_PAST_PRESENTATION_TIMING_EXT,
            p_next: ptr::null_mut(),
            present_id,
            target_time: 0,
            present_stage_count: 0,
            p_present_stages: ptr::null_mut(),
            time_domain: domain,
            time_domain_id: domain_id,
            report_complete: 1,
        }
    }

    #[test]
    fn feedback_extracts_first_pixel_out_and_visible() {
        let rec = record(99, TIME_DOMAIN_PRESENT_STAGE_LOCAL, 5);
        let stages = [
            PresentStageTimeEXT {
                stage: PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT_BIT,
                time: 111_000,
            },
            PresentStageTimeEXT {
                stage: PRESENT_STAGE_IMAGE_FIRST_PIXEL_VISIBLE_BIT,
                time: 222_000,
            },
        ];
        let fb = feedback_from_record(&rec, &stages);
        assert_eq!(fb.present_id, 99);
        assert_eq!(fb.first_pixel_out_ns, Some(111_000));
        assert_eq!(fb.first_pixel_visible_ns, Some(222_000));
        assert_eq!(fb.time_domain, TIME_DOMAIN_PRESENT_STAGE_LOCAL);
        assert_eq!(fb.time_domain_id, 5);
        assert!(fb.report_complete);
    }

    #[test]
    fn feedback_absent_stage_is_none() {
        let rec = record(1, TIME_DOMAIN_PRESENT_STAGE_LOCAL, 0);
        // Only a queue-operations-end stage present: neither scanout stage reported yet.
        let stages = [PresentStageTimeEXT {
            stage: PRESENT_STAGE_QUEUE_OPERATIONS_END_BIT,
            time: 5,
        }];
        let fb = feedback_from_record(&rec, &stages);
        assert_eq!(fb.first_pixel_out_ns, None);
        assert_eq!(fb.first_pixel_visible_ns, None);
    }

    #[test]
    fn feedback_empty_stage_slice_is_none() {
        let rec = record(1, TIME_DOMAIN_PRESENT_STAGE_LOCAL, 0);
        let fb = feedback_from_record(&rec, &[]);
        assert_eq!(fb.first_pixel_out_ns, None);
        assert_eq!(fb.first_pixel_visible_ns, None);
    }
}
