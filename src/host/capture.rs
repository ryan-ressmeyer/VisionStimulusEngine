//! Host information capture logic
//!
//! Collects system information from sysinfo, environment variables,
//! and other OS-level sources.

use std::sync::Arc;
use sysinfo::System;
use vulkano::device::physical::PhysicalDevice;
use winit::window::Window;

use super::edid::capture_edid;
use super::host_info::{
    BuildInfo, CpuInfo, DisplayInfo, GpuInfo, HostInfo, MemoryInfo, OsInfo, PipelineConfig,
    RuntimeEnv, SwapchainInfo, TimingCapabilities,
};
use crate::core::{SwapchainManager, VSEConfig};
use vulkano::device::Device;

/// Capture operating system information
pub fn capture_os_info() -> OsInfo {
    OsInfo {
        name: System::name().unwrap_or_else(|| "unknown".to_string()),
        version: System::os_version().unwrap_or_else(|| "unknown".to_string()),
        kernel_version: System::kernel_version().unwrap_or_else(|| "unknown".to_string()),
        hostname: System::host_name().unwrap_or_else(|| "unknown".to_string()),
    }
}

/// Capture CPU information
pub fn capture_cpu_info() -> CpuInfo {
    let sys = System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_cpu(sysinfo::CpuRefreshKind::everything()),
    );
    let cpus = sys.cpus();
    let brand = cpus
        .first()
        .map(|c| c.brand().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let frequency = cpus.first().map(|c| c.frequency()).unwrap_or(0);

    CpuInfo {
        brand,
        physical_cores: sys.physical_core_count().unwrap_or(0),
        logical_cores: cpus.len(),
        frequency_mhz: frequency,
    }
}

/// Capture memory information
pub fn capture_memory_info() -> MemoryInfo {
    let sys = System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_memory(sysinfo::MemoryRefreshKind::everything()),
    );

    MemoryInfo {
        total_bytes: sys.total_memory(),
        available_bytes: sys.available_memory(),
        used_bytes: sys.used_memory(),
    }
}

/// Capture runtime environment information
pub fn capture_runtime_env() -> RuntimeEnv {
    let display_server = if std::env::var("WAYLAND_DISPLAY").is_ok() {
        "wayland".to_string()
    } else if std::env::var("DISPLAY").is_ok() {
        "x11".to_string()
    } else {
        "unknown".to_string()
    };

    let nice_value = get_process_nice_value();

    let username = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string());

    RuntimeEnv {
        username,
        display_server,
        env_display: std::env::var("DISPLAY").ok(),
        env_wayland_display: std::env::var("WAYLAND_DISPLAY").ok(),
        env_vk_icd_filenames: std::env::var("VK_ICD_FILENAMES").ok(),
        env_vk_layer_path: std::env::var("VK_LAYER_PATH").ok(),
        process_nice_value: nice_value,
    }
}

/// Get the process nice value on Linux
fn get_process_nice_value() -> Option<i32> {
    #[cfg(target_os = "linux")]
    {
        // Read from /proc/self/stat — field 19 is the nice value
        std::fs::read_to_string("/proc/self/stat")
            .ok()
            .and_then(|stat| {
                // Fields are space-separated, but field 2 (comm) may contain spaces
                // and is wrapped in parens. Find the closing paren first.
                let after_comm = stat.find(')')?.checked_add(2)?;
                let fields: Vec<&str> = stat[after_comm..].split_whitespace().collect();
                // After comm: state(0), ppid(1), pgrp(2), session(3), tty(4),
                // tpgid(5), flags(6), minflt(7), cminflt(8), majflt(9),
                // cmajflt(10), utime(11), stime(12), cutime(13), cstime(14),
                // priority(15), nice(16)
                fields.get(16)?.parse::<i32>().ok()
            })
    }

    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Capture GPU info from Vulkan physical device properties
pub fn capture_gpu_info(physical_device: &Arc<PhysicalDevice>) -> GpuInfo {
    let props = physical_device.properties();
    GpuInfo {
        device_name: props.device_name.clone(),
        vendor_id: props.vendor_id,
        device_id: props.device_id,
        device_type: format!("{:?}", props.device_type),
        driver_version: props.driver_version,
        api_version: format!(
            "{}.{}.{}",
            props.api_version.major, props.api_version.minor, props.api_version.patch
        ),
        timestamp_period: props.timestamp_period,
        sub_pixel_precision_bits: props.sub_pixel_precision_bits,
        max_image_dimension_2d: props.max_image_dimension2_d,
    }
}

/// Capture display info from winit window
pub fn capture_display_info(window: Option<&Window>) -> DisplayInfo {
    if let Some(w) = window {
        let monitor = w.current_monitor();
        let scale_factor = w.scale_factor();
        let inner_size = w.inner_size();
        DisplayInfo {
            monitor_name: monitor.as_ref().and_then(|m| m.name()),
            refresh_rate_millihertz: monitor.as_ref().and_then(|m| m.refresh_rate_millihertz()),
            scale_factor,
            physical_size_mm: monitor.as_ref().map(|m| {
                let size = m.size();
                (size.width, size.height)
            }),
            logical_size: (inner_size.width, inner_size.height),
        }
    } else {
        DisplayInfo {
            monitor_name: None,
            refresh_rate_millihertz: None,
            scale_factor: 1.0,
            physical_size_mm: None,
            logical_size: (0, 0),
        }
    }
}

/// Capture negotiated swapchain state
pub fn capture_swapchain_info(swapchain_manager: &SwapchainManager) -> SwapchainInfo {
    let swapchain = swapchain_manager.swapchain();
    SwapchainInfo {
        image_format: format!("{:?}", swapchain.image_format()),
        color_space: format!("{:?}", swapchain.image_color_space()),
        present_mode: format!("{:?}", swapchain_manager.config().present_mode),
        image_count: swapchain_manager.images().len() as u32,
        extent: swapchain_manager.extent(),
    }
}

/// Capture user-configured pipeline settings
pub fn capture_pipeline_config(config: &VSEConfig) -> PipelineConfig {
    PipelineConfig {
        window_size: (config.window_width, config.window_height),
        clear_color: config.clear_color,
        gpu_preference: format!("{:?}", config.gpu_preference),
        present_mode: format!("{:?}", config.present_mode),
        expected_refresh_rate: config.expected_refresh_rate,
    }
}

/// Assemble the complete HostInfo snapshot
/// Human-readable name for a calibrateable `VkTimeDomainKHR`/`EXT`.
fn time_domain_name(d: ash::vk::TimeDomainEXT) -> String {
    match d.as_raw() {
        0 => "Device".to_string(),
        1 => "ClockMonotonic".to_string(),
        2 => "ClockMonotonicRaw".to_string(),
        3 => "QueryPerformanceCounter".to_string(),
        // VK_EXT_present_timing additions:
        1_000_208_000 => "PresentStageLocal".to_string(),
        1_000_208_001 => "SwapchainLocal".to_string(),
        other => format!("Unknown({other})"),
    }
}

/// Probe present-timing + clock-synchronization capabilities, measuring the CPU↔GPU
/// calibrated-timestamp deviation when possible.
///
/// The measurement (`cpu_gpu_max_deviation_ns`) is the driver-reported bound on how tightly
/// the GPU `Device` clock and the host `CLOCK_MONOTONIC` clock could be sampled together —
/// the dominant error when converting GPU/present timestamps to the CPU timeline. See
/// `docs/clock-synchronization.md`.
pub fn capture_timing_capabilities(device: &Arc<Device>) -> TimingCapabilities {
    use vulkano::VulkanObject;

    let physical_device = device.physical_device();
    let support = crate::core::present_timing_ext::probe_support(physical_device);
    let supported = physical_device.supported_extensions();
    let calibrated_supported =
        supported.ext_calibrated_timestamps || supported.khr_calibrated_timestamps;

    let mut caps = TimingCapabilities {
        present_timing: support.present_timing,
        present_id2: support.present_id2,
        present_wait2: support.present_wait2,
        calibrated_timestamps: calibrated_supported,
        calibrateable_time_domains: Vec::new(),
        cpu_gpu_max_deviation_ns: None,
        // Behaviorally-observed fields: measured at runtime, not from the physical-device probe.
        scanout_feedback_populated: None,
        absolute_scheduling_enforced: None,
        queue_global_priority: None,
    };

    if !calibrated_supported {
        return caps;
    }

    let instance = physical_device.instance();
    let entry = match unsafe { ash::Entry::load() } {
        Ok(e) => e,
        Err(_) => return caps,
    };
    let ash_instance = unsafe {
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
    };
    let ct_instance = ash::ext::calibrated_timestamps::Instance::new(&entry, &ash_instance);

    let domains = match unsafe {
        ct_instance.get_physical_device_calibrateable_time_domains(physical_device.handle())
    } {
        Ok(d) => d,
        Err(_) => return caps,
    };
    caps.calibrateable_time_domains = domains.iter().map(|d| time_domain_name(*d)).collect();

    // Measure Device↔CLOCK_MONOTONIC deviation if both are calibrateable and the extension
    // is enabled on this device.
    let enabled = device.enabled_extensions();
    let ext_on = enabled.ext_calibrated_timestamps || enabled.khr_calibrated_timestamps;
    let has_device = domains.contains(&ash::vk::TimeDomainEXT::DEVICE);
    let has_mono = domains.contains(&ash::vk::TimeDomainEXT::CLOCK_MONOTONIC);
    if ext_on && has_device && has_mono {
        let get_dpa = instance.fns().v1_0.get_device_proc_addr;
        let dev_handle = device.handle();
        let ash_device = unsafe {
            ash::Device::load_with(
                |name| std::mem::transmute(get_dpa(dev_handle, name.as_ptr())),
                dev_handle,
            )
        };
        let ct_device = ash::ext::calibrated_timestamps::Device::new(&ash_instance, &ash_device);
        let infos = [
            ash::vk::CalibratedTimestampInfoEXT::default()
                .time_domain(ash::vk::TimeDomainEXT::DEVICE),
            ash::vk::CalibratedTimestampInfoEXT::default()
                .time_domain(ash::vk::TimeDomainEXT::CLOCK_MONOTONIC),
        ];
        // maxDeviation is a per-read upper bound sensitive to scheduling; sample a few times
        // and keep the best (tightest) read as the representative clock-sync quality.
        let mut best: Option<u64> = None;
        for _ in 0..8 {
            if let Ok((_timestamps, max_deviation)) =
                unsafe { ct_device.get_calibrated_timestamps(&infos) }
            {
                best = Some(best.map_or(max_deviation, |b| b.min(max_deviation)));
            }
        }
        caps.cpu_gpu_max_deviation_ns = best;
    }

    caps
}

/// Behaviorally-observed present-timing driver conformance, gathered at runtime (advertised
/// support can outrun implementation on brand-new extensions). Threaded into the [`HostInfo`]
/// snapshot so a run records what the driver *actually did*, not just what it claimed.
#[derive(Debug, Clone, Copy, Default)]
pub struct ObservedPresentTiming {
    /// Whether feedback records carried a non-zero `IMAGE_FIRST_PIXEL_OUT` (see field docs).
    pub scanout_feedback_populated: Option<bool>,
    /// Whether absolute `targetTime` scheduling was observed to be enforced (see field docs).
    pub absolute_scheduling_enforced: Option<bool>,
    /// Queue global-priority outcome from device creation (see field docs on
    /// [`TimingCapabilities::queue_global_priority`]).
    pub queue_global_priority: Option<crate::core::present_timing_ext::QueuePriorityOutcome>,
}

pub fn capture_host_info(
    physical_device: &Arc<PhysicalDevice>,
    device: &Arc<Device>,
    window: Option<&Window>,
    swapchain_manager: &SwapchainManager,
    config: &VSEConfig,
    observed: ObservedPresentTiming,
) -> HostInfo {
    let captured_at = {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        // Format as ISO 8601 without pulling in chrono
        let secs = now.as_secs();
        let time_of_day = secs % 86400;
        let hours = time_of_day / 3600;
        let minutes = (time_of_day % 3600) / 60;
        let seconds = time_of_day % 60;
        format!(
            "unix:{}  {:02}:{:02}:{:02} UTC",
            secs, hours, minutes, seconds
        )
    };

    let mut timing = capture_timing_capabilities(device);
    // Overlay behaviorally-observed conformance onto the advertised capabilities.
    timing.scanout_feedback_populated = observed.scanout_feedback_populated;
    timing.absolute_scheduling_enforced = observed.absolute_scheduling_enforced;
    timing.queue_global_priority = observed.queue_global_priority.map(|o| o.label());

    HostInfo {
        captured_at,
        os: capture_os_info(),
        cpu: capture_cpu_info(),
        memory: capture_memory_info(),
        gpu: capture_gpu_info(physical_device),
        timing,
        display: capture_display_info(window),
        swapchain: capture_swapchain_info(swapchain_manager),
        pipeline: capture_pipeline_config(config),
        build: BuildInfo::from_compile_time(),
        runtime: capture_runtime_env(),
        edid: capture_edid(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capture_os_info() {
        let os = capture_os_info();
        assert!(!os.name.is_empty() || os.name == "unknown");
        assert!(!os.hostname.is_empty() || os.hostname == "unknown");
    }

    #[test]
    fn test_capture_cpu_info() {
        let cpu = capture_cpu_info();
        assert!(cpu.logical_cores > 0);
        assert!(!cpu.brand.is_empty() || cpu.brand == "unknown");
    }

    #[test]
    fn test_capture_memory_info() {
        let mem = capture_memory_info();
        assert!(mem.total_bytes > 0);
        assert!(mem.used_bytes <= mem.total_bytes);
    }

    #[test]
    fn test_capture_runtime_env() {
        let env = capture_runtime_env();
        assert!(
            env.display_server == "x11"
                || env.display_server == "wayland"
                || env.display_server == "unknown"
        );
    }
}
