# Host Logging Design

## Purpose

Capture a comprehensive snapshot of the host machine state for reproducible stimulus presentation and audit trails. Vision scientists need to know exactly what hardware, drivers, OS, and configuration produced their experimental data.

## Architecture

**Approach:** Monolithic `HostInfo` struct with nested sub-structs, all `#[derive(Debug, Clone, Serialize)]`. One on-demand `capture_host_info()` method on `RenderContext`. No auto-capture — the user decides when and how often to snapshot.

**Principle:** Host info capture never fails. Fields backed by optional external tools are `Option<T>`. Missing tools produce a `tracing::warn!` advising installation, but no errors.

## Data Model

```rust
pub struct HostInfo {
    pub captured_at: String,          // ISO 8601 timestamp
    pub os: OsInfo,
    pub cpu: CpuInfo,
    pub memory: MemoryInfo,
    pub gpu: GpuInfo,
    pub display: DisplayInfo,
    pub swapchain: SwapchainInfo,
    pub pipeline: PipelineConfig,
    pub build: BuildInfo,
    pub runtime: RuntimeEnv,
    pub edid: Option<EdidInfo>,       // None if xrandr unavailable
}
```

### Sub-structs

**OsInfo**: name, version, kernel_version, hostname

**CpuInfo**: brand, physical_cores, logical_cores, frequency_mhz

**MemoryInfo**: total_bytes, available_bytes, used_bytes

**GpuInfo**: device_name, vendor_id, device_id, device_type, driver_version, api_version, timestamp_period, sub_pixel_precision_bits, max_image_dimension_2d, and other reproducibility-critical Vulkan limits

**DisplayInfo**: monitor_name, refresh_rate_millihertz, scale_factor, physical_size_mm, logical_size

**SwapchainInfo**: image_format, color_space, present_mode, image_count, extent (the actually negotiated values, not just what was requested)

**PipelineConfig**: user-configured values from the builder — window_size, clear_color, gpu_preference, present_mode, expected_refresh_rate, flip_logging enabled, flip_log_csv path

**BuildInfo**: vse_version, git_commit_hash (Option — None if git unavailable at build time), build_profile (debug/release), rustc_version

**RuntimeEnv**: display_server (X11/Wayland), relevant env vars (DISPLAY, WAYLAND_DISPLAY, VK_ICD_FILENAMES, VK_LAYER_PATH), process_nice_value

**EdidInfo**: raw_hex, manufacturer, model, serial, year, gamma, chromaticity_coordinates

## API

```rust
// Capture a snapshot
let info: HostInfo = ctx.capture_host_info();

// Serialize to JSON
let json = serde_json::to_string_pretty(&info)?;
std::fs::write("session_log.json", &json)?;

// Human-readable summary via Display
println!("{}", info);

// Access specific fields
println!("GPU: {}", info.gpu.device_name);
```

- `capture_host_info()` on `RenderContext` — needs access to Vulkan device, swapchain, winit window
- Returns owned `HostInfo` — no lifetimes, user stores/serializes however they want
- `Display` impl for human-readable summary
- `Serialize` for JSON/TOML/any serde format

## Data Sources

| Sub-struct | Source |
|---|---|
| OsInfo | `sysinfo::System` |
| CpuInfo | `sysinfo::System` |
| MemoryInfo | `sysinfo::System` |
| GpuInfo | `vulkano::device::physical::PhysicalDevice::properties()` |
| DisplayInfo | winit monitor handle |
| SwapchainInfo | Internal `SwapchainManager` state |
| PipelineConfig | Internal `VSEConfig` |
| BuildInfo | Compile-time: `env!("CARGO_PKG_VERSION")`, `option_env!("GIT_HASH")`, `cfg!(debug_assertions)` |
| RuntimeEnv | `std::env::var()`, `/proc/self/stat` or `libc::getpriority()`, `XDG_SESSION_TYPE` |
| EdidInfo | `xrandr --verbose` → parse EDID hex blob |

## Dependencies

- **New:** `sysinfo` (OS/CPU/memory), `serde_json` (JSON serialization convenience)
- **Existing:** `serde` (already in project), `tracing` (already in project)
- **No new crate for EDID** — hand-written parser for the ~10 fields we need from the 128-byte EDID blob

## External Tool Handling

- **xrandr (runtime):** `tracing::warn!("xrandr not found — EDID monitor info unavailable. Install xrandr for full monitor logging.")` → `edid: None`
- **git (build-time):** `cargo:warning=git not found — commit hash will be unavailable. Install git for full build metadata logging.` → `git_commit_hash: None`

## Module Structure

```
src/host/
├── mod.rs          # Public exports
├── host_info.rs    # All struct definitions
├── capture.rs      # Collection logic (populate HostInfo from various sources)
└── edid.rs         # xrandr output parsing, EDID blob decoding
```

## Build Script

New `build.rs` at crate root:
- Runs `git rev-parse --short HEAD`
- Sets `GIT_HASH` env var for compile-time inclusion
- Emits `cargo:warning=` if git is unavailable
- Runs `rustc --version` and sets `RUSTC_VERSION` env var
