//! Host information data structures
//!
//! All structs derive Serialize for flexible output (JSON, TOML, etc.)
//! and Debug/Clone for inspection and copying.

use serde::Serialize;

/// Complete snapshot of host machine state at capture time.
///
/// Returned by [`RenderContext::capture_host_info()`].
/// Serialize to JSON, TOML, or any serde-supported format.
///
/// # Example
///
/// ```no_run
/// # use vision_stimulus_engine::prelude::*;
/// # fn example(ctx: &mut RenderContext) -> Result<(), Box<dyn std::error::Error>> {
/// let info = ctx.capture_host_info();
/// let json = serde_json::to_string_pretty(&info)?;
/// std::fs::write("session_log.json", &json)?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Serialize)]
pub struct HostInfo {
    /// ISO 8601 timestamp of when this snapshot was captured
    pub captured_at: String,
    /// Operating system information
    pub os: OsInfo,
    /// CPU information
    pub cpu: CpuInfo,
    /// Memory information
    pub memory: MemoryInfo,
    /// GPU/graphics hardware information
    pub gpu: GpuInfo,
    /// Display/monitor information
    pub display: DisplayInfo,
    /// Negotiated swapchain state
    pub swapchain: SwapchainInfo,
    /// User-configured pipeline settings
    pub pipeline: PipelineConfig,
    /// Build-time metadata
    pub build: BuildInfo,
    /// Runtime environment
    pub runtime: RuntimeEnv,
    /// EDID monitor data (None if xrandr unavailable)
    pub edid: Option<EdidInfo>,
}

/// Operating system information
#[derive(Debug, Clone, Serialize)]
pub struct OsInfo {
    pub name: String,
    pub version: String,
    pub kernel_version: String,
    pub hostname: String,
}

/// CPU information
#[derive(Debug, Clone, Serialize)]
pub struct CpuInfo {
    pub brand: String,
    pub physical_cores: usize,
    pub logical_cores: usize,
    pub frequency_mhz: u64,
}

/// Memory information (in bytes)
#[derive(Debug, Clone, Serialize)]
pub struct MemoryInfo {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
}

/// GPU hardware information from Vulkan physical device properties
#[derive(Debug, Clone, Serialize)]
pub struct GpuInfo {
    pub device_name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub device_type: String,
    pub driver_version: u32,
    pub api_version: String,
    /// Nanoseconds per GPU timestamp tick — needed to interpret GPU timestamp queries
    pub timestamp_period: f32,
    /// Sub-pixel precision bits — affects fine geometry alignment
    pub sub_pixel_precision_bits: u32,
    /// Maximum 2D image dimension — ensures textures fit without downscaling
    pub max_image_dimension_2d: u32,
}

/// Display/monitor information from winit
#[derive(Debug, Clone, Serialize)]
pub struct DisplayInfo {
    pub monitor_name: Option<String>,
    pub refresh_rate_millihertz: Option<u32>,
    pub scale_factor: f64,
    pub physical_size_mm: Option<(u32, u32)>,
    pub logical_size: (u32, u32),
}

/// Actually negotiated swapchain state (may differ from what was requested)
#[derive(Debug, Clone, Serialize)]
pub struct SwapchainInfo {
    pub image_format: String,
    pub color_space: String,
    pub present_mode: String,
    pub image_count: u32,
    pub extent: [u32; 2],
}

/// User-configured pipeline settings from the builder
#[derive(Debug, Clone, Serialize)]
pub struct PipelineConfig {
    pub window_size: (u32, u32),
    pub clear_color: [f32; 4],
    pub gpu_preference: String,
    pub present_mode: String,
    pub expected_refresh_rate: Option<f64>,
    pub flip_logging: bool,
    pub flip_log_csv_path: Option<String>,
}

/// Build-time metadata captured by build.rs
#[derive(Debug, Clone, Serialize)]
pub struct BuildInfo {
    pub vse_version: String,
    pub git_commit_hash: Option<String>,
    pub build_profile: String,
    pub rustc_version: String,
}

impl BuildInfo {
    /// Populate from compile-time environment variables set by build.rs
    pub fn from_compile_time() -> Self {
        let git_hash_raw = env!("VSE_GIT_HASH");
        let git_commit_hash = if git_hash_raw.is_empty() {
            None
        } else {
            Some(git_hash_raw.to_string())
        };

        Self {
            vse_version: env!("CARGO_PKG_VERSION").to_string(),
            git_commit_hash,
            build_profile: if cfg!(debug_assertions) {
                "debug".to_string()
            } else {
                "release".to_string()
            },
            rustc_version: env!("VSE_RUSTC_VERSION").to_string(),
        }
    }
}

/// Runtime environment information
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeEnv {
    pub display_server: String,
    pub env_display: Option<String>,
    pub env_wayland_display: Option<String>,
    pub env_vk_icd_filenames: Option<String>,
    pub env_vk_layer_path: Option<String>,
    pub process_nice_value: Option<i32>,
}

/// EDID monitor identification data parsed from xrandr
#[derive(Debug, Clone, Serialize)]
pub struct EdidInfo {
    pub raw_hex: String,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub year: Option<u16>,
    pub gamma: Option<f32>,
}

impl std::fmt::Display for HostInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "=== Host Info (captured {}) ===", self.captured_at)?;
        writeln!(f)?;
        writeln!(
            f,
            "OS: {} {} (kernel {})",
            self.os.name, self.os.version, self.os.kernel_version
        )?;
        writeln!(f, "Host: {}", self.os.hostname)?;
        writeln!(
            f,
            "CPU: {} ({} cores, {} threads, {} MHz)",
            self.cpu.brand, self.cpu.physical_cores, self.cpu.logical_cores, self.cpu.frequency_mhz
        )?;
        writeln!(
            f,
            "Memory: {:.1} GB / {:.1} GB",
            self.memory.used_bytes as f64 / 1_073_741_824.0,
            self.memory.total_bytes as f64 / 1_073_741_824.0
        )?;
        writeln!(f)?;
        writeln!(
            f,
            "GPU: {} ({}, driver v{})",
            self.gpu.device_name, self.gpu.device_type, self.gpu.driver_version
        )?;
        writeln!(f, "Vulkan API: {}", self.gpu.api_version)?;
        writeln!(
            f,
            "Timestamp period: {} ns, subpixel: {} bits, max 2D: {}",
            self.gpu.timestamp_period,
            self.gpu.sub_pixel_precision_bits,
            self.gpu.max_image_dimension_2d
        )?;
        writeln!(f)?;
        writeln!(
            f,
            "Monitor: {}",
            self.display.monitor_name.as_deref().unwrap_or("unknown")
        )?;
        if let Some(rate) = self.display.refresh_rate_millihertz {
            writeln!(f, "Refresh rate: {:.3} Hz", rate as f64 / 1000.0)?;
        }
        writeln!(f, "Scale factor: {}", self.display.scale_factor)?;
        writeln!(f)?;
        writeln!(
            f,
            "Swapchain: {} / {} / {} ({} images, {}x{})",
            self.swapchain.image_format,
            self.swapchain.color_space,
            self.swapchain.present_mode,
            self.swapchain.image_count,
            self.swapchain.extent[0],
            self.swapchain.extent[1]
        )?;
        writeln!(f)?;
        writeln!(f, "VSE: v{}", self.build.vse_version)?;
        if let Some(hash) = &self.build.git_commit_hash {
            writeln!(f, "Git: {}", hash)?;
        }
        writeln!(
            f,
            "Build: {} ({})",
            self.build.build_profile, self.build.rustc_version
        )?;
        writeln!(f, "Display server: {}", self.runtime.display_server)?;
        if let Some(edid) = &self.edid {
            writeln!(f)?;
            writeln!(
                f,
                "EDID: {} / {}",
                edid.manufacturer.as_deref().unwrap_or("?"),
                edid.model.as_deref().unwrap_or("?")
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_info_from_compile_time() {
        let build = BuildInfo::from_compile_time();
        assert!(!build.vse_version.is_empty());
        assert!(!build.rustc_version.is_empty());
        assert_eq!(build.build_profile, "debug");
    }

    #[test]
    fn test_host_info_serializes_to_json() {
        let build = BuildInfo::from_compile_time();
        let json = serde_json::to_string(&build).unwrap();
        assert!(json.contains("vse_version"));
        assert!(json.contains("rustc_version"));
    }

    #[test]
    fn test_os_info_serializes() {
        let os = OsInfo {
            name: "Linux".to_string(),
            version: "22.04".to_string(),
            kernel_version: "6.1.0".to_string(),
            hostname: "lab-pc".to_string(),
        };
        let json = serde_json::to_string(&os).unwrap();
        assert!(json.contains("Linux"));
        assert!(json.contains("lab-pc"));
    }

    #[test]
    fn test_gpu_info_serializes() {
        let gpu = GpuInfo {
            device_name: "Test GPU".to_string(),
            vendor_id: 0x10DE,
            device_id: 0x2684,
            device_type: "DiscreteGpu".to_string(),
            driver_version: 100,
            api_version: "1.3.0".to_string(),
            timestamp_period: 1.0,
            sub_pixel_precision_bits: 8,
            max_image_dimension_2d: 16384,
        };
        let json = serde_json::to_string(&gpu).unwrap();
        assert!(json.contains("Test GPU"));
    }
}
