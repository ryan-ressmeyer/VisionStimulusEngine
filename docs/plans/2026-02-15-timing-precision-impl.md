# Timing Precision Upgrade Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Upgrade VSE timing to support hardware-verified presentation timestamps via VK_GOOGLE_display_timing, with a TimingProvider trait abstraction for future VK_EXT_present_timing support.

**Architecture:** A `TimingProvider` trait abstracts timing backends. At device creation, VSE probes for `VK_GOOGLE_display_timing` and instantiates the best available provider (`GoogleDisplayTimingProvider` or `CpuTimingProvider`). The provider is stored in `VSEState` and called during `flip()`. FlipInfo is simplified to 2 timestamps + a `TimingSource` enum.

**Tech Stack:** Rust, vulkano 0.35, ash 0.38, winit 0.29

**Design doc:** `docs/plans/2026-02-15-timing-precision-design.md`

---

### Task 1: Upgrade vulkano 0.34 → 0.35 and add ash 0.38

This is the prerequisite for everything else. Vulkano 0.35 uses ash 0.38 which has `VK_GOOGLE_display_timing` bindings.

**Files:**
- Modify: `Cargo.toml`
- Modify: any files that break due to API changes (expect `src/core/device.rs`, `src/core/swapchain.rs`, `src/core/context.rs`, `src/drawing/renderer.rs`, shader files)

**Step 1: Update Cargo.toml dependencies**

```toml
# Change:
vulkano = "0.34"
vulkano-shaders = "0.34"

# To:
vulkano = "0.35"
vulkano-shaders = "0.35"

# Add:
ash = "0.38"
```

**Step 2: Run cargo check and fix all compilation errors**

Run: `cargo check 2>&1`

Vulkano 0.34 → 0.35 breaking changes to watch for:
- Device creation API changes
- Shader macro output changes (`vulkano-shaders 0.35`)
- Swapchain API changes
- Image/buffer type changes
- Import path changes

Fix each error one at a time. Common patterns:
- Renamed types: check vulkano 0.35 changelog/docs
- Changed generics: update type parameters
- New required fields: add with sensible defaults

**Step 3: Run full test suite**

Run: `cargo test`
Expected: All existing tests pass

**Step 4: Run clippy and fmt**

Run: `cargo clippy --all-targets && cargo fmt --check`
Expected: Clean

**Step 5: Verify examples still run**

Run: `cargo run --example 00_clear_color` (visual check — window opens, grey background, stable FPS)

**Step 6: Commit**

```bash
git add -A
git commit -m "Upgrade vulkano 0.34 -> 0.35, add ash 0.38"
```

---

### Task 2: Add TimingSource enum and refactor FlipInfo

**Files:**
- Create: `src/timing/timing_source.rs`
- Modify: `src/timing/flip_info.rs`
- Modify: `src/timing/mod.rs`
- Modify: `src/lib.rs` (prelude)

**Step 1: Write test for TimingSource**

In `src/timing/timing_source.rs`:

```rust
//! Timing source classification for flip timing data.

/// Identifies which Vulkan extension (or fallback) provided the timing data.
///
/// This is written into every FlipInfo and CSV log so researchers always
/// know the precision tier of their timing data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum TimingSource {
    /// VK_EXT_present_timing — hardware scanout timestamps + scheduled presents.
    /// Not yet implemented (extension bindings not available in ash/vulkano).
    ExtPresentTiming,
    /// VK_GOOGLE_display_timing — driver-reported display timing.
    GoogleDisplayTiming,
    /// CPU estimation via std::time::Instant around fence wait.
    CpuEstimate,
}

impl std::fmt::Display for TimingSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TimingSource::ExtPresentTiming => write!(f, "ExtPresentTiming"),
            TimingSource::GoogleDisplayTiming => write!(f, "GoogleDisplayTiming"),
            TimingSource::CpuEstimate => write!(f, "CpuEstimate"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timing_source_display() {
        assert_eq!(TimingSource::CpuEstimate.to_string(), "CpuEstimate");
        assert_eq!(TimingSource::GoogleDisplayTiming.to_string(), "GoogleDisplayTiming");
        assert_eq!(TimingSource::ExtPresentTiming.to_string(), "ExtPresentTiming");
    }

    #[test]
    fn test_timing_source_equality() {
        assert_eq!(TimingSource::CpuEstimate, TimingSource::CpuEstimate);
        assert_ne!(TimingSource::CpuEstimate, TimingSource::GoogleDisplayTiming);
    }
}
```

**Step 2: Run test to verify it passes**

Run: `cargo test timing_source`
Expected: PASS

**Step 3: Refactor FlipInfo**

In `src/timing/flip_info.rs`, replace the struct with:

```rust
use super::clock::Timestamp;
use super::timing_source::TimingSource;

#[derive(Debug, Clone, serde::Serialize)]
pub struct FlipInfo {
    pub frame_number: u64,
    pub timing_source: TimingSource,
    pub submit_time: Timestamp,
    pub present_time: Timestamp,
    pub missed: bool,
    pub missed_count: u32,
    pub skipped: bool,
}

impl FlipInfo {
    pub fn skipped(frame_number: u64) -> Self {
        Self {
            frame_number,
            timing_source: TimingSource::CpuEstimate,
            submit_time: Timestamp::from_micros(0),
            present_time: Timestamp::from_micros(0),
            missed: false,
            missed_count: 0,
            skipped: true,
        }
    }
}
```

Remove `frame_duration`, `expected_frame_duration`, `frame_duration_us()`.

**Step 4: Update FlipInfo tests**

Update all tests in `flip_info.rs` to match the new struct fields. Remove tests for `frame_duration_us()`.

**Step 5: Update mod.rs and prelude**

In `src/timing/mod.rs`, add:
```rust
mod timing_source;
pub use timing_source::TimingSource;
```

In `src/lib.rs` prelude, add `TimingSource`:
```rust
pub use crate::timing::{FlipInfo, FlipLogger, TimingSource, TimingStats};
```

**Step 6: Run tests**

Run: `cargo test`
Expected: FlipInfo tests pass, other tests will fail (FlipLogger, TimingStats, context — those are fixed in later tasks)

**Step 7: Commit**

```bash
git add src/timing/timing_source.rs src/timing/flip_info.rs src/timing/mod.rs src/lib.rs
git commit -m "Add TimingSource enum and refactor FlipInfo to 2 timestamps"
```

---

### Task 3: Update FlipLogger and CSV output

**Files:**
- Modify: `src/timing/flip_logger.rs`

**Step 1: Update CSV header and export**

Change `export_csv` to write:
```
frame_number,timing_source,submit_time_us,present_time_us,missed,missed_count
```

Update the data row format to match. Remove `frame_duration_us` and `expected_frame_duration_us` columns. Add `timing_source` column using `Display` impl.

Rename `present_complete_time_us` → `present_time_us` in CSV header.

**Step 2: Update FlipLogger tests**

Fix `make_info` helper to use new FlipInfo fields. Update CSV header assertions. Update all test assertions that reference removed fields.

**Step 3: Run tests**

Run: `cargo test flip_logger`
Expected: All FlipLogger tests pass

**Step 4: Commit**

```bash
git add src/timing/flip_logger.rs
git commit -m "Update FlipLogger CSV output for new FlipInfo fields"
```

---

### Task 4: Update TimingStats to compute durations from consecutive records

**Files:**
- Modify: `src/timing/stats.rs`

**Step 1: Rewrite TimingStats::compute**

Instead of reading `r.frame_duration`, compute inter-frame durations from consecutive `present_time` values:

```rust
let durations_us: Vec<f64> = records
    .windows(2)
    .map(|pair| {
        pair[1].present_time.duration_since(pair[0].present_time).as_micros() as f64
    })
    .collect();
```

Also update `total_duration` to use `present_time` instead of `present_complete_time`.

Missed frame detection: use the expected frame duration from VSEState (passed as a parameter) or derive from median of durations. Simplest approach: add an `expected_frame_duration_us` parameter to `compute()`.

Alternatively, keep the current approach where missed is already stored in FlipInfo (computed at flip time). Then stats just reads `r.missed` and `r.missed_count` as before — no change needed there.

**Step 2: Update `make_records` test helper**

Update to use new FlipInfo fields. Compute `present_time` as cumulative sum of durations to make `windows(2)` math work correctly.

**Step 3: Run tests**

Run: `cargo test stats`
Expected: All TimingStats tests pass

**Step 4: Commit**

```bash
git add src/timing/stats.rs
git commit -m "Update TimingStats to compute durations from consecutive present_times"
```

---

### Task 5: Create TimingProvider trait and CpuTimingProvider

**Files:**
- Create: `src/timing/provider.rs`
- Modify: `src/timing/mod.rs`

**Step 1: Define the TimingProvider trait**

In `src/timing/provider.rs`:

```rust
//! Timing provider trait and implementations.

use std::sync::Arc;
use std::time::Duration;

use super::clock::{Clock, Timestamp};
use super::timing_source::TimingSource;

/// Result of a present operation from the timing provider.
pub struct PresentResult {
    /// The present time (meaning depends on TimingSource)
    pub present_time: Timestamp,
}

/// Abstracts timing backends for different Vulkan extensions.
pub trait TimingProvider {
    /// Which timing source this provider uses.
    fn source(&self) -> TimingSource;

    /// Get the display refresh cycle duration.
    /// Returns None if not yet known (e.g., still auto-detecting).
    fn refresh_cycle_duration(&self) -> Option<Duration>;

    /// Record the present time for the current frame.
    /// Called after the GPU fence signals.
    /// For CPU: returns clock.now().
    /// For GOOGLE: queries vkGetPastPresentationTimingGOOGLE.
    fn record_present_time(&self, clock: &Clock) -> Timestamp;

    /// Wait/schedule for a target present time.
    /// For CPU: spin-waits until target_time.
    /// For GOOGLE: target is passed to VkPresentTimeGOOGLE during present.
    /// Called before the present submission if target_time is Some.
    fn wait_for_target(&self, target_time: Timestamp, clock: &Clock);
}
```

**Step 2: Implement CpuTimingProvider**

```rust
/// CPU-based timing (fallback when no Vulkan timing extensions are available).
pub struct CpuTimingProvider {
    refresh_duration: std::cell::RefCell<Option<Duration>>,
}

impl CpuTimingProvider {
    pub fn new() -> Self {
        Self {
            refresh_duration: std::cell::RefCell::new(None),
        }
    }

    /// Set the auto-detected refresh duration.
    pub fn set_refresh_duration(&self, duration: Duration) {
        *self.refresh_duration.borrow_mut() = Some(duration);
    }
}

impl TimingProvider for CpuTimingProvider {
    fn source(&self) -> TimingSource {
        TimingSource::CpuEstimate
    }

    fn refresh_cycle_duration(&self) -> Option<Duration> {
        *self.refresh_duration.borrow()
    }

    fn record_present_time(&self, clock: &Clock) -> Timestamp {
        clock.now()
    }

    fn wait_for_target(&self, target_time: Timestamp, clock: &Clock) {
        while clock.now() < target_time {
            std::hint::spin_loop();
        }
    }
}
```

**Step 3: Write tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpu_provider_source() {
        let provider = CpuTimingProvider::new();
        assert_eq!(provider.source(), TimingSource::CpuEstimate);
    }

    #[test]
    fn test_cpu_provider_refresh_duration() {
        let provider = CpuTimingProvider::new();
        assert!(provider.refresh_cycle_duration().is_none());
        provider.set_refresh_duration(Duration::from_micros(16_667));
        assert_eq!(provider.refresh_cycle_duration(), Some(Duration::from_micros(16_667)));
    }

    #[test]
    fn test_cpu_provider_record_present_time() {
        let provider = CpuTimingProvider::new();
        let clock = Clock::new();
        let t = provider.record_present_time(&clock);
        assert!(t.as_micros() > 0 || t.as_micros() == 0); // just verify it returns
    }

    #[test]
    fn test_cpu_provider_spin_wait() {
        let provider = CpuTimingProvider::new();
        let clock = Clock::new();
        let target = Timestamp::from_micros(clock.now().as_micros() + 1_000); // 1ms in future
        provider.wait_for_target(target, &clock);
        assert!(clock.now() >= target);
    }
}
```

**Step 4: Update mod.rs**

```rust
mod provider;
pub use provider::{CpuTimingProvider, PresentResult, TimingProvider};
```

**Step 5: Run tests**

Run: `cargo test provider`
Expected: All pass

**Step 6: Commit**

```bash
git add src/timing/provider.rs src/timing/mod.rs
git commit -m "Add TimingProvider trait and CpuTimingProvider"
```

---

### Task 6: Implement GoogleDisplayTimingProvider

**Files:**
- Modify: `src/timing/provider.rs`
- Modify: `src/core/device.rs` (extension detection + enabling)

**Step 1: Add extension detection to DeviceSelector**

In `src/core/device.rs`, add a method to check if the physical device supports `VK_GOOGLE_display_timing`:

```rust
/// Check if the physical device supports VK_GOOGLE_display_timing
pub fn supports_google_display_timing(&self) -> bool {
    self.physical_device
        .supported_extensions()
        .google_display_timing
}
```

Modify `create_device()` to conditionally enable the extension:

```rust
let device_extensions = DeviceExtensions {
    khr_swapchain: true,
    khr_dynamic_rendering: true,
    google_display_timing: self.supports_google_display_timing(),
    ..DeviceExtensions::empty()
};
```

**Step 2: Implement GoogleDisplayTimingProvider**

In `src/timing/provider.rs`, add the implementation that uses ash 0.38's `GoogleDisplayTiming` extension loader:

```rust
use ash::vk;

/// Provider using VK_GOOGLE_display_timing extension.
pub struct GoogleDisplayTimingProvider {
    display_timing_fn: ash::extensions::google::DisplayTiming,
    swapchain_handle: vk::SwapchainKHR,
    device_handle: vk::Device,
}
```

Key methods:
- `refresh_cycle_duration()`: calls `get_refresh_cycle_duration_google()` via ash
- `record_present_time()`: calls `get_past_presentation_timing_google()` via ash, returns the `actualPresentTime` converted to a `Timestamp`
- `wait_for_target()`: stores target time to be attached during present (the actual attachment to pNext chain happens in swapchain present — coordinate with Task 7)

The ash function loading requires the raw `VkDevice` and `VkInstance` handles from vulkano. Access via:
```rust
let raw_device = vulkano_device.handle();
let raw_instance = vulkano_device.instance().handle();
```

Note: The exact ash 0.38 API for `GoogleDisplayTiming` may differ — consult `ash::extensions::google::DisplayTiming` docs during implementation.

**Step 3: Write tests**

GoogleDisplayTimingProvider tests will be `#[ignore]` since they require a GPU with the extension. Write a test that constructs the provider only if the extension is available:

```rust
#[test]
#[ignore] // Requires GPU with VK_GOOGLE_display_timing
fn test_google_provider_source() {
    // ... setup ...
    assert_eq!(provider.source(), TimingSource::GoogleDisplayTiming);
}
```

**Step 4: Run tests**

Run: `cargo test`
Expected: All non-ignored tests pass

**Step 5: Commit**

```bash
git add src/timing/provider.rs src/core/device.rs
git commit -m "Implement GoogleDisplayTimingProvider via ash 0.38"
```

---

### Task 7: Integrate TimingProvider into VSEState and flip()

**Files:**
- Modify: `src/core/context.rs`

This is the central integration task. The `flip()` method signature changes, VSEState gets a timing provider, and the present path uses the provider.

**Step 1: Add TimingProvider to VSEState**

```rust
struct VSEState {
    // ... existing fields ...
    timing_provider: Box<dyn TimingProvider>,
    flip_at_warned: bool, // Track if we've warned about CPU spin-wait
}
```

**Step 2: Initialize provider in VSEContext::initialize**

After device creation, detect the best timing provider:

```rust
let timing_provider: Box<dyn TimingProvider> = if device_selector.supports_google_display_timing() {
    info!("Timing backend: GoogleDisplayTiming (VK_GOOGLE_display_timing)");
    Box::new(GoogleDisplayTimingProvider::new(/* device handles */))
} else {
    warn!("VK_GOOGLE_display_timing not available. Using CPU estimation.");
    Box::new(CpuTimingProvider::new())
};
```

**Step 3: Change flip() signature**

```rust
pub fn flip(&mut self, target_time: Option<Timestamp>) -> Result<FlipInfo, VSEError>
```

**Step 4: Update flip() implementation**

Replace the timing capture section:

```rust
// If target time specified, wait/schedule
if let Some(target) = target_time {
    if self.state.timing_provider.source() == TimingSource::CpuEstimate && !self.state.flip_at_warned {
        warn!("flip() called with target_time but VK_GOOGLE_display_timing is not available. Falling back to CPU spin-wait. Timing will be approximate.");
        self.state.flip_at_warned = true;
    }
    self.state.timing_provider.wait_for_target(target, &self.state.clock);
}

let submit_time = self.state.clock.now();

// ... existing present logic ...

let present_time = self.state.timing_provider.record_present_time(&self.state.clock);
```

Build the new FlipInfo:

```rust
let flip_info = FlipInfo {
    frame_number: self.state.frame_number,
    timing_source: self.state.timing_provider.source(),
    submit_time,
    present_time,
    missed,
    missed_count,
    skipped: false,
};
```

**Step 5: Update missed frame detection**

The missed frame detection currently uses `frame_duration` which we removed from FlipInfo. Keep the computation internal to flip():

```rust
let frame_duration = self.state.last_present_time
    .map(|prev| present_time.duration_since(prev));

let expected = self.state.expected_frame_duration
    .unwrap_or(Duration::from_micros(16_667));

let (missed, missed_count) = match frame_duration {
    Some(dur) => {
        let ratio = dur.as_micros() as f64 / expected.as_micros() as f64;
        if ratio > 1.5 { (true, (ratio.round() as u32).saturating_sub(1)) }
        else { (false, 0) }
    }
    None => (false, 0),
};
```

**Step 6: Update auto-refresh-rate detection**

For GoogleDisplayTiming, use `refresh_cycle_duration()` instead of auto-detect:

```rust
if self.state.expected_frame_duration.is_none() {
    if let Some(dur) = self.state.timing_provider.refresh_cycle_duration() {
        self.state.expected_frame_duration = Some(dur);
        info!("Refresh cycle duration: {} us ({:.1} Hz)", dur.as_micros(), 1_000_000.0 / dur.as_micros() as f64);
    } else {
        // Fall back to auto-detect from frames (existing logic)
    }
}
```

**Step 7: Add timing_source() method to RenderContext**

```rust
/// Get the active timing source.
pub fn timing_source(&self) -> TimingSource {
    self.state.timing_provider.source()
}
```

**Step 8: Run tests**

Run: `cargo check` then `cargo test`
Expected: Compilation succeeds. Tests pass (context tests are `#[ignore]` so they won't run on CI).

**Step 9: Commit**

```bash
git add src/core/context.rs
git commit -m "Integrate TimingProvider into VSEState and flip()"
```

---

### Task 8: Update all examples for new flip() signature

**Files:**
- Modify: `examples/00_clear_color.rs`
- Modify: `examples/01_timing_validation.rs`
- Modify: `examples/02_calibration_square.rs`
- Modify: `examples/03_gabor_demo.rs`

**Step 1: Update all flip() calls**

In every example, change `vse.flip()?` to `vse.flip(None)?`.

**Step 2: Update example 01 missed frame logging**

Remove references to `info.frame_duration` and `info.expected_frame_duration` in `01_timing_validation.rs`. The missed frame log line can use `info.submit_time` and `info.present_time` instead:

```rust
if info.missed {
    println!(
        "*** MISSED FRAME {} (present_time: {:.2} ms, missed_count: {})",
        info.frame_number,
        info.present_time.as_millis_f64(),
        info.missed_count,
    );
}
```

**Step 3: Add timing source printout to examples**

In `01_timing_validation.rs`, print the timing source at startup (after first flip):

```rust
if info.frame_number == 0 {
    println!("Timing source: {}", vse.timing_source());
}
```

**Step 4: Run all examples**

Run: `cargo build --examples`
Expected: All compile. Run `cargo run --example 00_clear_color` for visual check.

**Step 5: Commit**

```bash
git add examples/
git commit -m "Update examples for new flip(Option<Timestamp>) signature"
```

---

### Task 9: Add flip_at example demonstrating scheduled presents

**Files:**
- Create: `examples/04_scheduled_flip.rs`
- Modify: `Cargo.toml` (add example entry)

**Step 1: Write the example**

```rust
//! Scheduled Flip Demo
//!
//! Demonstrates using flip(Some(target_time)) to schedule frame
//! presentation at specific times. Shows the difference between
//! immediate and scheduled presents.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 04_scheduled_flip
//! ```

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    println!("VSE - Scheduled Flip Demo");
    println!("=========================");

    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("VSE - Scheduled Flip")
        .with_clear_color(0.3, 0.3, 0.3, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .with_flip_logging(true)
        .with_flip_log_csv("scheduled_flip.csv")
        .build()?;

    let mut last_present = None;

    context.run(move |vse| {
        vse.clear()?;

        // After warmup, schedule each flip one refresh cycle after the last
        let target = last_present.map(|prev: Timestamp| {
            // Target: previous present time + ~16.667ms (60 Hz)
            Timestamp::from_micros(prev.as_micros() + 16_667)
        });

        let info = vse.flip(target)?;

        if info.frame_number == 0 {
            println!("Timing source: {}", vse.timing_source());
        }

        last_present = Some(info.present_time);

        if info.frame_number % 300 == 0 && info.frame_number > 0 {
            vse.print_timing_report();
        }

        Ok(())
    })?;

    Ok(())
}
```

**Step 2: Add to Cargo.toml**

```toml
[[example]]
name = "04_scheduled_flip"
path = "examples/04_scheduled_flip.rs"
```

**Step 3: Build and test**

Run: `cargo build --example 04_scheduled_flip`
Expected: Compiles successfully.

**Step 4: Commit**

```bash
git add examples/04_scheduled_flip.rs Cargo.toml
git commit -m "Add scheduled flip example demonstrating flip(target_time)"
```

---

### Task 10: Final verification and cleanup

**Files:**
- All modified files

**Step 1: Run full test suite**

Run: `cargo test`
Expected: All tests pass

**Step 2: Run clippy**

Run: `cargo clippy --all-targets`
Expected: No warnings

**Step 3: Run fmt**

Run: `cargo fmt --check`
Expected: No formatting issues

**Step 4: Build all examples**

Run: `cargo build --examples`
Expected: All compile

**Step 5: Run timing validation example**

Run: `cargo run --release --example 01_timing_validation`
Expected: Runs, reports timing source, shows stats, writes CSV

**Step 6: Verify CSV output format**

Check that `timing_validation.csv` has the new header:
```
frame_number,timing_source,submit_time_us,present_time_us,missed,missed_count
```

**Step 7: Commit any final fixes**

```bash
git add -A
git commit -m "Final cleanup: timing precision upgrade complete"
```
