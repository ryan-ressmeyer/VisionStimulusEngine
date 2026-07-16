# Host Configuration Logging

## Why Capture the Host Configuration?

Vision science experiments depend on precise, repeatable stimulus presentation. A finding that holds on one machine may not replicate on another if the hardware, driver, or OS differs in ways that affect timing or rendering. Capturing the full host configuration at experiment time serves three purposes:

1. **Reproducibility.** When publishing results or sharing data, a complete machine snapshot lets another lab verify whether their setup matches yours or diagnose why their results diverge. Differences in GPU driver version, display refresh rate, or present mode can shift frame timing by milliseconds — enough to matter for neural response measurements.

2. **Debugging.** When timing anomalies appear (missed frames, variable flip durations), the host log is the first place to look. Knowing the exact GPU, driver version, swapchain configuration, and OS kernel narrows the search space immediately.

3. **Audit trails.** Multi-session experiments that span weeks or months need a record of whether the setup changed between sessions. A JSON log per session makes this trivially diffable.

## What VSE Captures

A single call to `capture_host_info()` returns a `HostInfo` struct containing:

| Section | Contents | Source |
|---------|----------|--------|
| **OS** | Name, version, kernel version, hostname | `sysinfo` crate |
| **CPU** | Brand, physical/logical core count, frequency | `sysinfo` crate |
| **Memory** | Total, available, and used bytes | `sysinfo` crate |
| **GPU** | Device name, vendor/device IDs, device type, driver version, Vulkan API version, timestamp period, sub-pixel precision, max 2D image dimension | Vulkan physical device properties |
| **Display** | Monitor name, refresh rate (millihertz), scale factor, physical size, logical size | `winit` window/monitor info |
| **Swapchain** | Image format, color space, present mode, image count, extent | Negotiated Vulkan swapchain state |
| **Timing** | Present-timing extension support, present-id2, present-wait2, calibrated timestamp domains, measured CPU↔GPU deviation, observed scanout-feedback and scheduling behavior, queue-priority outcome | Vulkan capability probes and runtime observations |
| **Pipeline** | Window size, clear color, GPU preference, present mode, expected refresh rate, flip logging settings | VSE builder configuration |
| **Build** | VSE version, git commit hash, build profile (debug/release), rustc version | Compile-time `build.rs` + `env!()` |
| **Runtime** | Username, display server (X11/Wayland), relevant environment variables (`DISPLAY`, `WAYLAND_DISPLAY`, `VK_ICD_FILENAMES`, `VK_LAYER_PATH`), process nice value | Environment variables, `/proc/self/stat` |
| **EDID** | Raw hex, manufacturer code, model name, serial number, manufacture year, gamma | `xrandr --verbose` (optional, graceful fallback) |

The **Swapchain** section is particularly important: it records the *actually negotiated* state, which may differ from what was requested. For example, you may request `Mailbox` present mode but the driver may fall back to `Fifo`.

## Usage

### Basic: Capture and Save to JSON

```rust
use vision_stimulus_engine::prelude::*;

context.run(|ctx| {
    if ctx.frame_number() == 0 {
        let info = ctx.capture_host_info();

        // Human-readable summary to stdout
        println!("{}", info);

        // Machine-readable JSON to file
        let json = serde_json::to_string_pretty(&info)?;
        std::fs::write("session_host_info.json", &json)?;
    }

    ctx.clear()?;
    ctx.flip(None)?;
    Ok(())
})?;
```

### Naming Convention for Multi-Session Experiments

A practical pattern is to include the date and subject ID in the filename:

```rust
let info = ctx.capture_host_info();
let json = serde_json::to_string_pretty(&info)?;

let filename = format!(
    "host_{}_{}.json",
    info.captured_at.replace([':', ' '], "_"),
    "subject_01"
);
std::fs::write(&filename, &json)?;
```

### Accessing Individual Fields

All fields on `HostInfo` and its nested structs are public. You can inspect specific values without serializing:

```rust
let info = ctx.capture_host_info();

// Check GPU timestamp resolution
println!("Timestamp period: {} ns", info.gpu.timestamp_period);

// Verify present mode
println!("Negotiated present mode: {}", info.swapchain.present_mode);

// Check monitor refresh rate
if let Some(rate) = info.display.refresh_rate_millihertz {
    println!("Refresh rate: {:.2} Hz", rate as f64 / 1000.0);
}

// Check whether advertised present-timing features worked in this run
if let Some(timing) = &info.timing {
    println!("EXT present timing available: {}", timing.present_timing);
    println!("Scanout feedback populated: {:?}", timing.scanout_feedback_populated);
    println!("Absolute scheduling enforced: {:?}", timing.absolute_scheduling_enforced);
}
```

### Running the Example

```bash
cargo run --example 06_host_info
```

This opens a window, captures host info on the first frame, prints the summary, writes `host_info.json`, and exits after a few frames.

## Graceful Degradation

Some data sources may not be available on every machine:

- **EDID data** requires `xrandr` to be installed. If it is missing, `edid` will be `None` and a `tracing::warn!` message is emitted. No error is raised.
- **Git commit hash** requires `git` to be available at build time. If unavailable, `git_commit_hash` will be `None` and a build warning is printed.
- **Process nice value** is only available on Linux (read from `/proc/self/stat`). On other platforms it will be `None`.

The rest of the fields (OS, CPU, memory, GPU, display, swapchain, pipeline, build profile, rustc version) are always populated.

## Output Format

`HostInfo` derives `serde::Serialize`, so you can serialize it to any serde-supported format. JSON is the most common choice, but TOML, YAML, or MessagePack all work if you add the appropriate serde crate.

The `Display` trait implementation on `HostInfo` provides a compact human-readable summary suitable for printing to the terminal or embedding in log files.
