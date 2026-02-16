# Host Logging Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a `capture_host_info()` method to `RenderContext` that returns a comprehensive `HostInfo` struct capturing the full hardware-driver-configuration state of the host machine.

**Architecture:** Monolithic `HostInfo` struct with nested sub-structs (`OsInfo`, `CpuInfo`, `MemoryInfo`, `GpuInfo`, `DisplayInfo`, `SwapchainInfo`, `PipelineConfig`, `BuildInfo`, `RuntimeEnv`, `EdidInfo`), all `#[derive(Debug, Clone, Serialize)]`. Single on-demand `capture_host_info()` method on `RenderContext`. External tools (xrandr, git) degrade gracefully with `tracing::warn!` messages.

**Tech Stack:** sysinfo (OS/CPU/memory), serde + serde_json (serialization), vulkano properties (GPU), winit (display), xrandr (EDID), build.rs (git hash + rustc version)

---

### Task 1: Add dependencies and build.rs

**Files:**
- Modify: `Cargo.toml`
- Create: `build.rs`

**Step 1: Add sysinfo and serde_json to Cargo.toml**

In `Cargo.toml`, add under `[dependencies]`:

```toml
# System information for host logging
sysinfo = "0.33"

# JSON serialization for host info export
serde_json = "1.0"
```

**Step 2: Create build.rs for git hash and rustc version**

Create `build.rs` at crate root:

```rust
use std::process::Command;

fn main() {
    // Capture git commit hash
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        });

    match git_hash {
        Some(hash) => println!("cargo:rustc-env=VSE_GIT_HASH={}", hash),
        None => {
            println!("cargo:warning=git not found — commit hash will be unavailable. Install git for full build metadata logging.");
            println!("cargo:rustc-env=VSE_GIT_HASH=");
        }
    }

    // Capture rustc version
    let rustc_version = Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=VSE_RUSTC_VERSION={}", rustc_version);

    // Re-run if git HEAD changes
    println!("cargo:rerun-if-changed=.git/HEAD");
}
```

**Step 3: Verify it compiles**

Run: `cargo check`
Expected: Success, no errors. May see the git warning if not in a repo.

**Step 4: Commit**

```bash
git add Cargo.toml build.rs
git commit -m "Add sysinfo, serde_json deps and build.rs for git hash capture"
```

---

### Task 2: Create host info struct definitions

**Files:**
- Create: `src/host/mod.rs`
- Create: `src/host/host_info.rs`
- Modify: `src/lib.rs`

**Step 1: Write tests for struct creation and serialization**

Add to `src/host/host_info.rs` at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_info_from_compile_time() {
        let build = BuildInfo::from_compile_time();
        assert!(!build.vse_version.is_empty());
        assert!(!build.rustc_version.is_empty());
        // build_profile should be "debug" in test mode
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
        assert!(json.contains("10DE")); // hex serialization or numeric
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib host`
Expected: FAIL — module doesn't exist yet.

**Step 3: Create the struct definitions**

Create `src/host/host_info.rs`:

```rust
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
            self.cpu.brand,
            self.cpu.physical_cores,
            self.cpu.logical_cores,
            self.cpu.frequency_mhz
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
            self.display
                .monitor_name
                .as_deref()
                .unwrap_or("unknown")
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
```

**Step 4: Create `src/host/mod.rs`**

```rust
//! Host machine information capture
//!
//! Provides comprehensive snapshots of the host system state
//! for reproducibility and audit trails in vision science experiments.

mod host_info;

pub use host_info::{
    BuildInfo, CpuInfo, DisplayInfo, EdidInfo, GpuInfo, HostInfo, MemoryInfo, OsInfo,
    PipelineConfig, RuntimeEnv, SwapchainInfo,
};
```

**Step 5: Register the host module in `src/lib.rs`**

Add `pub mod host;` after the existing module declarations, and add `HostInfo` to the prelude:

In `src/lib.rs`, change:

```rust
pub mod core;
pub mod drawing;
pub mod timing;
```

to:

```rust
pub mod core;
pub mod drawing;
pub mod host;
pub mod timing;
```

And in the prelude, add:

```rust
pub use crate::host::HostInfo;
```

**Step 6: Run tests**

Run: `cargo test --lib host`
Expected: PASS — struct creation, serialization, and `BuildInfo::from_compile_time()` all work.

**Step 7: Commit**

```bash
git add src/host/ src/lib.rs
git commit -m "Add HostInfo struct definitions with serde serialization"
```

---

### Task 3: Implement OS/CPU/memory capture using sysinfo

**Files:**
- Create: `src/host/capture.rs`
- Modify: `src/host/mod.rs`

**Step 1: Write tests for sysinfo-based capture**

Add to `src/host/capture.rs`:

```rust
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
        // display_server should be one of: x11, wayland, unknown
        assert!(
            env.display_server == "x11"
                || env.display_server == "wayland"
                || env.display_server == "unknown"
        );
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib host::capture`
Expected: FAIL — functions don't exist yet.

**Step 3: Implement the capture functions**

Create `src/host/capture.rs`:

```rust
//! Host information capture logic
//!
//! Collects system information from sysinfo, environment variables,
//! and other OS-level sources.

use sysinfo::System;

use super::host_info::{CpuInfo, MemoryInfo, OsInfo, RuntimeEnv};

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

    RuntimeEnv {
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

// Tests at the bottom of this file (see Step 1 above)
```

**Step 4: Export capture module from mod.rs**

In `src/host/mod.rs`, add:

```rust
pub(crate) mod capture;
```

**Step 5: Run tests**

Run: `cargo test --lib host::capture`
Expected: PASS — all capture functions work.

**Step 6: Commit**

```bash
git add src/host/capture.rs src/host/mod.rs
git commit -m "Implement OS, CPU, memory, and runtime capture via sysinfo"
```

---

### Task 4: Implement EDID parsing from xrandr

**Files:**
- Create: `src/host/edid.rs`
- Modify: `src/host/mod.rs`

**Step 1: Write tests for EDID parsing**

Add to `src/host/edid.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_edid_block_valid() {
        // Minimal valid EDID header (first 20 bytes of a real EDID, padded to 128 bytes)
        // Standard EDID header: 00 FF FF FF FF FF FF 00
        // Manufacturer ID at bytes 8-9: encoded 3-letter code
        // Product code at bytes 10-11
        // Serial at bytes 12-15
        // Week/year at bytes 16-17
        // Version at bytes 18-19
        let mut edid_bytes = vec![0u8; 128];
        // Header
        edid_bytes[0] = 0x00;
        edid_bytes[1] = 0xFF;
        edid_bytes[2] = 0xFF;
        edid_bytes[3] = 0xFF;
        edid_bytes[4] = 0xFF;
        edid_bytes[5] = 0xFF;
        edid_bytes[6] = 0xFF;
        edid_bytes[7] = 0x00;
        // Manufacturer: "DEL" (Dell) = 0x10AC
        edid_bytes[8] = 0x10;
        edid_bytes[9] = 0xAC;
        // Manufacture year: 2020 (byte 17 = year - 1990 = 30)
        edid_bytes[17] = 30;
        // Gamma: 2.2 = byte value 120 (gamma * 100 - 100)
        edid_bytes[23] = 120;

        let hex = edid_bytes
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>();

        let edid = parse_edid_bytes(&edid_bytes);
        assert!(edid.is_some());
        let edid = edid.unwrap();
        assert_eq!(edid.manufacturer.as_deref(), Some("DEL"));
        assert_eq!(edid.year, Some(2020));
        assert!((edid.gamma.unwrap() - 2.2).abs() < 0.01);
    }

    #[test]
    fn test_parse_edid_block_invalid_header() {
        let edid_bytes = vec![0u8; 128]; // All zeros, invalid header
        let edid = parse_edid_bytes(&edid_bytes);
        assert!(edid.is_none());
    }

    #[test]
    fn test_extract_edid_hex_from_xrandr() {
        let xrandr_output = r#"
Screen 0: minimum 8 x 8, current 3840 x 2160, maximum 32767 x 32767
DP-0 connected primary 3840x2160+0+0 (normal left inverted right x axis y axis) 600mm x 340mm
   3840x2160     60.00*+
        EDID:
                00ffffffffffff001e6d085b7c5b0000
                0b1e0104b53c22783aee95a3544c9926
                0f5054254b80714f81809500a9c0b300
                d1c0814001014dd000a0f0703e803020
                350055502100001a286800a0f0703e80
                0890350055502100001a000000fd0030
                901ee63c000a202020202020000000fc
                004c472048445220344b0a20200001e8
   1920x1080     60.00    59.94
"#;
        let hex = extract_edid_hex(xrandr_output);
        assert!(hex.is_some());
        let hex = hex.unwrap();
        assert!(hex.starts_with("00ffffffffffff00"));
    }

    #[test]
    fn test_extract_edid_hex_no_edid() {
        let xrandr_output = "DP-0 connected primary 3840x2160+0+0\n   3840x2160     60.00*+\n";
        let hex = extract_edid_hex(xrandr_output);
        assert!(hex.is_none());
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib host::edid`
Expected: FAIL — module doesn't exist.

**Step 3: Implement EDID parsing**

Create `src/host/edid.rs`:

```rust
//! EDID parsing from xrandr output
//!
//! Parses the Extended Display Identification Data (EDID) blob
//! from `xrandr --verbose` output. Falls back gracefully if
//! xrandr is unavailable.

use std::process::Command;
use tracing::warn;

use super::host_info::EdidInfo;

/// Attempt to capture EDID info by running xrandr
pub fn capture_edid() -> Option<EdidInfo> {
    let output = match Command::new("xrandr").arg("--verbose").output() {
        Ok(output) => output,
        Err(_) => {
            warn!(
                "xrandr not found — EDID monitor info unavailable. \
                 Install xrandr for full monitor logging."
            );
            return None;
        }
    };

    if !output.status.success() {
        warn!("xrandr returned non-zero exit code — EDID unavailable");
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let hex = extract_edid_hex(&stdout)?;
    let bytes = hex_to_bytes(&hex)?;
    let mut edid = parse_edid_bytes(&bytes)?;
    edid.raw_hex = hex;

    // Try to extract model name from descriptor blocks
    if edid.model.is_none() {
        edid.model = extract_descriptor_string(&bytes, 0xFC); // Monitor name tag
    }
    if edid.serial.is_none() {
        edid.serial = extract_descriptor_string(&bytes, 0xFF); // Serial number tag
    }

    Some(edid)
}

/// Extract the EDID hex block from xrandr --verbose output
fn extract_edid_hex(xrandr_output: &str) -> Option<String> {
    let mut in_edid = false;
    let mut hex = String::new();

    for line in xrandr_output.lines() {
        let trimmed = line.trim();
        if trimmed == "EDID:" {
            in_edid = true;
            continue;
        }
        if in_edid {
            // EDID hex lines are indented and contain only hex chars
            if trimmed.chars().all(|c| c.is_ascii_hexdigit()) && !trimmed.is_empty() {
                hex.push_str(trimmed);
            } else {
                break;
            }
        }
    }

    if hex.is_empty() {
        None
    } else {
        Some(hex)
    }
}

/// Convert hex string to bytes
fn hex_to_bytes(hex: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let mut chars = hex.chars();
    while let (Some(hi), Some(lo)) = (chars.next(), chars.next()) {
        let byte = u8::from_str_radix(&format!("{}{}", hi, lo), 16).ok()?;
        bytes.push(byte);
    }
    Some(bytes)
}

/// Parse raw EDID bytes into EdidInfo
fn parse_edid_bytes(bytes: &[u8]) -> Option<EdidInfo> {
    if bytes.len() < 128 {
        return None;
    }

    // Validate EDID header: 00 FF FF FF FF FF FF 00
    let header = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
    if bytes[..8] != header {
        return None;
    }

    // Manufacturer ID (bytes 8-9): 3 letters encoded in 2 bytes
    let mfg_raw = ((bytes[8] as u16) << 8) | (bytes[9] as u16);
    let c1 = ((mfg_raw >> 10) & 0x1F) as u8 + b'A' - 1;
    let c2 = ((mfg_raw >> 5) & 0x1F) as u8 + b'A' - 1;
    let c3 = (mfg_raw & 0x1F) as u8 + b'A' - 1;
    let manufacturer = if c1.is_ascii_uppercase() && c2.is_ascii_uppercase() && c3.is_ascii_uppercase() {
        Some(format!("{}{}{}", c1 as char, c2 as char, c3 as char))
    } else {
        None
    };

    // Year (byte 17): year - 1990
    let year = if bytes[17] > 0 {
        Some(1990 + bytes[17] as u16)
    } else {
        None
    };

    // Gamma (byte 23): (gamma * 100) - 100, so gamma = (value + 100) / 100
    let gamma = if bytes[23] != 0xFF {
        Some((bytes[23] as f32 + 100.0) / 100.0)
    } else {
        None
    };

    Some(EdidInfo {
        raw_hex: String::new(), // Filled in by caller
        manufacturer,
        model: None,  // Filled from descriptor blocks
        serial: None, // Filled from descriptor blocks
        year,
        gamma,
    })
}

/// Extract a string from EDID descriptor blocks (bytes 54-125)
/// Each descriptor is 18 bytes. Tag byte is at offset 3.
fn extract_descriptor_string(bytes: &[u8], tag: u8) -> Option<String> {
    if bytes.len() < 126 {
        return None;
    }

    for desc_start in (54..=90).step_by(18) {
        // Check if this is a "display descriptor" (first two bytes are 0x00 0x00)
        if bytes[desc_start] == 0x00
            && bytes[desc_start + 1] == 0x00
            && bytes[desc_start + 3] == tag
        {
            // String data is in bytes 5-17 of the descriptor
            let str_bytes = &bytes[desc_start + 5..desc_start + 18];
            let s: String = str_bytes
                .iter()
                .take_while(|&&b| b != 0x0A && b != 0x00) // Terminated by newline or null
                .map(|&b| b as char)
                .collect();
            let trimmed = s.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }

    None
}

// Tests at the bottom of this file (see Step 1 above)
```

**Step 4: Export edid module from mod.rs**

In `src/host/mod.rs`, add:

```rust
pub(crate) mod edid;
```

**Step 5: Run tests**

Run: `cargo test --lib host::edid`
Expected: PASS — hex extraction, EDID parsing, and manufacturer decoding all work.

**Step 6: Commit**

```bash
git add src/host/edid.rs src/host/mod.rs
git commit -m "Implement EDID parsing from xrandr --verbose output"
```

---

### Task 5: Wire up capture_host_info() on RenderContext

**Files:**
- Modify: `src/core/context.rs`
- Modify: `src/host/capture.rs`
- Modify: `src/host/mod.rs`

**Step 1: Write an integration test**

Add to `tests/core_tests.rs`:

```rust
/// Test that HostInfo struct is accessible from prelude
#[test]
fn test_host_info_in_prelude() {
    // HostInfo should be importable from prelude
    let build = vision_stimulus_engine::host::BuildInfo::from_compile_time();
    assert!(!build.vse_version.is_empty());
}
```

**Step 2: Run test to verify it passes (struct access only, no Vulkan)**

Run: `cargo test test_host_info_in_prelude`
Expected: PASS — the struct is already exported.

**Step 3: Add GPU, display, swapchain, and pipeline capture helpers to `capture.rs`**

Add these functions to `src/host/capture.rs`:

```rust
use std::sync::Arc;
use vulkano::device::physical::PhysicalDevice;
use winit::window::Window;

use super::host_info::{
    BuildInfo, DisplayInfo, EdidInfo, GpuInfo, HostInfo, PipelineConfig, SwapchainInfo,
};
use super::edid::capture_edid;
use crate::core::{GPUPreference, PresentMode, SwapchainManager};

/// Capture GPU info from Vulkan physical device properties
pub fn capture_gpu_info(physical_device: &PhysicalDevice) -> GpuInfo {
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
        timestamp_period: props.limits.timestamp_period,
        sub_pixel_precision_bits: props.limits.sub_pixel_precision_bits,
        max_image_dimension_2d: props.limits.max_image_dimension2_d,
    }
}

/// Capture display info from winit window
pub fn capture_display_info(window: &Window) -> DisplayInfo {
    let monitor = window.current_monitor();
    let scale_factor = window.scale_factor();
    let inner_size = window.inner_size();

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
pub fn capture_pipeline_config(config: &crate::core::context::VSEConfig) -> PipelineConfig {
    PipelineConfig {
        window_size: (config.window_width, config.window_height),
        clear_color: config.clear_color,
        gpu_preference: format!("{:?}", config.gpu_preference),
        present_mode: format!("{:?}", config.present_mode),
        expected_refresh_rate: config.expected_refresh_rate,
        flip_logging: config.flip_logging,
        flip_log_csv_path: config.flip_log_csv_path.as_ref().map(|p| p.display().to_string()),
    }
}

/// Assemble the complete HostInfo snapshot
pub fn capture_host_info(
    physical_device: &PhysicalDevice,
    window: &Window,
    swapchain_manager: &SwapchainManager,
    config: &crate::core::context::VSEConfig,
) -> HostInfo {
    let captured_at = {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        // Format as ISO 8601 without pulling in chrono
        let secs = now.as_secs();
        let days = secs / 86400;
        let time_of_day = secs % 86400;
        let hours = time_of_day / 3600;
        let minutes = (time_of_day % 3600) / 60;
        let seconds = time_of_day % 60;
        // Approximate date (good enough for logging, not calendar-precise)
        format!(
            "unix:{}  {:02}:{:02}:{:02} UTC",
            days, hours, minutes, seconds
        )
    };

    HostInfo {
        captured_at,
        os: capture_os_info(),
        cpu: capture_cpu_info(),
        memory: capture_memory_info(),
        gpu: capture_gpu_info(physical_device),
        display: capture_display_info(window),
        swapchain: capture_swapchain_info(swapchain_manager),
        pipeline: capture_pipeline_config(config),
        build: BuildInfo::from_compile_time(),
        runtime: capture_runtime_env(),
        edid: capture_edid(),
    }
}
```

Note: The `capture_pipeline_config` function needs `VSEConfig` to be accessible. Currently `VSEConfig` is `pub` in `context.rs`, but the `context` module is not re-exported by path. We need to either:
- Make `VSEConfig` part of the public API (add to `core/mod.rs` exports), or
- Pass the individual config fields as parameters

The cleaner approach is to add `VSEConfig` to the core module's public exports since it's already `pub struct`.

In `src/core/mod.rs`, add `VSEConfig` to the `pub use context::` line:

```rust
pub use context::{RenderContext, VSEConfig, VSEContext, VSEContextBuilder, VSEError};
```

**Step 4: Add `capture_host_info()` method to `RenderContext`**

In `src/core/context.rs`, add this method to `impl<'a> RenderContext<'a>`:

```rust
    /// Capture a snapshot of the full host machine state.
    ///
    /// Returns a [`HostInfo`] struct containing OS, CPU, memory, GPU,
    /// display, swapchain, pipeline config, build metadata, runtime
    /// environment, and EDID monitor data.
    ///
    /// This is an on-demand operation — call it when you need a snapshot.
    /// The EDID capture shells out to `xrandr`, which may take ~50ms.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use vision_stimulus_engine::prelude::*;
    /// # fn example(ctx: &mut RenderContext) -> Result<(), Box<dyn std::error::Error>> {
    /// let info = ctx.capture_host_info();
    /// println!("{}", info);  // Human-readable summary
    /// let json = serde_json::to_string_pretty(&info)?;
    /// std::fs::write("session_log.json", &json)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn capture_host_info(&self) -> crate::host::HostInfo {
        crate::host::capture::capture_host_info(
            self.state.device_selector.physical_device(),
            &self.state.window,
            &self.state.swapchain,
            self.config,
        )
    }
```

Add the necessary import at the top of `context.rs` — actually, since we're using full paths (`crate::host::...`), no additional imports are needed.

**Step 5: Run all tests and clippy**

Run: `cargo test`
Expected: PASS

Run: `cargo clippy --all-targets`
Expected: No warnings

Run: `cargo fmt`

**Step 6: Commit**

```bash
git add src/core/context.rs src/core/mod.rs src/host/
git commit -m "Wire up capture_host_info() on RenderContext"
```

---

### Task 6: Add host_info example

**Files:**
- Create: `examples/06_host_info.rs`
- Modify: `Cargo.toml`

**Step 1: Create the example**

Create `examples/06_host_info.rs`:

```rust
//! Example: Capture and display host machine information
//!
//! Demonstrates the capture_host_info() API for logging the full
//! host state to JSON for reproducibility audits.
//!
//! Run with: `cargo run --example 06_host_info`

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("Host Info Example")
        .build()?;

    context.run(|ctx| {
        // Capture host info on first frame
        if ctx.frame_number() == 0 {
            let info = ctx.capture_host_info();

            // Print human-readable summary
            println!("{}", info);

            // Save to JSON
            let json = serde_json::to_string_pretty(&info)
                .expect("Failed to serialize host info");
            std::fs::write("host_info.json", &json)
                .expect("Failed to write host_info.json");
            println!("\nSaved to host_info.json");
        }

        ctx.clear()?;
        ctx.flip(None)?;

        // Close after a few frames
        if ctx.frame_number() > 5 {
            return Err(VSEError::Window("Done".to_string()));
        }

        Ok(())
    })?;

    Ok(())
}
```

**Step 2: Add example to Cargo.toml**

Add after the last `[[example]]` entry:

```toml
[[example]]
name = "06_host_info"
path = "examples/06_host_info.rs"
```

**Step 3: Verify it compiles**

Run: `cargo check --example 06_host_info`
Expected: Success

**Step 4: Commit**

```bash
git add examples/06_host_info.rs Cargo.toml
git commit -m "Add host info capture example with JSON export"
```

---

### Task 7: Final validation

**Step 1: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

**Step 2: Run clippy**

Run: `cargo clippy --all-targets`
Expected: No warnings.

**Step 3: Run fmt**

Run: `cargo fmt`
Expected: No changes (already formatted).

**Step 4: Verify the example runs (manual)**

Run: `cargo run --example 06_host_info`
Expected: Prints host info summary, writes `host_info.json`.

**Step 5: Commit any final fixes, then tag**

```bash
git add -A
git commit -m "Host logging feature complete — Phase 4"
```

---

## Implementation Notes

### Potential Issues to Watch For

1. **`VSEConfig` visibility**: The `capture_pipeline_config` function needs access to `VSEConfig` fields. The struct is already `pub`, but we need to export it from `core/mod.rs`. This is done in Task 5 Step 3.

2. **`physical_device()` access on `DeviceSelector`**: Already has a public method returning `&Arc<PhysicalDevice>`. Good.

3. **Swapchain `image_color_space()`**: Need to verify this method exists on vulkano 0.35's `Swapchain` type. If not, we may need to store the color space in `SwapchainManager` during creation.

4. **`sysinfo` API version**: The API changed significantly between versions. The plan uses sysinfo 0.33 which uses `System::name()` as associated functions (not methods). Verify this compiles.

5. **EDID `physical_size_mm`**: The `DisplayInfo.physical_size_mm` from winit actually reports pixel dimensions, not physical mm. We may need to get physical size from EDID or xrandr instead. The plan uses `monitor.size()` which returns `PhysicalSize<u32>` in pixels. Consider renaming to `monitor_size_pixels` or getting actual mm from EDID.
