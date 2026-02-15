# Phase 2: Timing Infrastructure - Implementation Guide

## Overview

Phase 2 builds the timing subsystem that makes VSE a credible tool for vision science. Without sub-millisecond timing precision and the ability to measure and validate that precision, VSE is just another rendering engine. This phase transforms it into an instrument.

**Goal:** Achieve millisecond-accurate timing with full logging and validation

**Success Criteria:**
- `flip()` returns a `FlipInfo` struct with high-resolution timestamps
- Every frame's timing is recorded and queryable
- Flip log can be exported to CSV for post-hoc analysis
- Timing statistics (mean, std, min, max, missed frames) computed on demand
- Frame duration jitter is < 1ms standard deviation at 60 Hz (FIFO mode)
- Missed frames are detected and reported
- Builder API allows enabling/disabling timing features
- `cargo check`, `cargo test`, `cargo clippy --all-targets`, `cargo fmt` all pass clean

## What Changes From Phase 1

### Current `flip()` Signature

```rust
// Phase 1: fire-and-forget
pub fn flip(&mut self) -> Result<(), VSEError> { ... }
```

### Phase 2 `flip()` Signature

```rust
// Phase 2: returns timing information
pub fn flip(&mut self) -> Result<FlipInfo, VSEError> { ... }
```

This is the single most important API change. Every call to `flip()` now produces a receipt containing exactly when that frame was presented. Vision scientists need this data to correlate stimulus events with neural recordings.

## New Module Structure

```
src/
  timing/
    mod.rs            // Public exports
    clock.rs          // High-resolution clock abstraction
    flip_info.rs      // FlipInfo struct and related types
    flip_logger.rs    // Per-frame timing log with CSV export
    stats.rs          // Timing statistics computation
```

## Detailed Component Design

### 1. `src/timing/clock.rs` - High-Resolution Clock

**Purpose:** Provide a consistent, monotonic time source for all timing measurements. Wraps `std::time::Instant` with microsecond-resolution helpers and a fixed reference point.

```rust
use std::time::Instant;

/// High-resolution monotonic clock for timing measurements.
///
/// All timestamps in VSE are relative to the clock's creation time
/// (typically when VSEContext is built). This avoids the ambiguity of
/// wall-clock times and ensures monotonicity.
pub struct Clock {
    /// The epoch (time zero) for this clock
    epoch: Instant,
}

/// A timestamp relative to the clock's epoch, in microseconds.
///
/// Stored as u64 microseconds. At 1 MHz resolution this gives
/// ~584,942 years before overflow - sufficient for any experiment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(u64);
```

**Key Methods:**

```rust
impl Clock {
    /// Create a new clock. The current instant becomes time zero.
    pub fn new() -> Self;

    /// Get the current timestamp (microseconds since epoch).
    pub fn now(&self) -> Timestamp;

    /// Get the underlying epoch for interop with std::time.
    pub fn epoch(&self) -> Instant;
}

impl Timestamp {
    /// Get raw microseconds value.
    pub fn as_micros(&self) -> u64;

    /// Convert to seconds (f64).
    pub fn as_secs_f64(&self) -> f64;

    /// Convert to milliseconds (f64).
    pub fn as_millis_f64(&self) -> f64;

    /// Duration between two timestamps.
    pub fn duration_since(&self, earlier: Timestamp) -> Duration;
}
```

**Why a wrapper instead of raw `Instant`?**
- Forces a common epoch across all subsystems
- Microsecond `u64` representation is serializable (for CSV/JSON) unlike `Instant`
- Clear units prevent confusion (is it seconds? nanoseconds?)
- `Timestamp` implements `serde::Serialize` for logging

### 2. `src/timing/flip_info.rs` - FlipInfo Struct

**Purpose:** The receipt returned by every `flip()` call. Contains everything a vision scientist needs to validate that a stimulus appeared when expected.

```rust
/// Information about a single frame flip (presentation).
///
/// Returned by `RenderContext::flip()`. Contains timestamps that let
/// you verify timing precision and correlate with external recordings.
///
/// # Timing Model
///
/// ```text
/// CPU timeline:
///   [submit_time]----[present_complete_time]
///         |                    |
///         v                    v
///   Command buffer       Fence signals
///   submitted to GPU     (GPU done, frame
///                         queued for display)
/// ```
///
/// `present_complete_time` is the closest CPU-side approximation of
/// when the frame reaches the display. True scanout time requires
/// hardware support (photodiode or VK_GOOGLE_display_timing).
#[derive(Debug, Clone)]
pub struct FlipInfo {
    /// Monotonically increasing frame number (0-indexed from first flip)
    pub frame_number: u64,

    /// Timestamp just before command buffer submission
    pub submit_time: Timestamp,

    /// Timestamp after fence signal (GPU finished, frame queued for display)
    pub present_complete_time: Timestamp,

    /// Duration of this frame (time since previous flip's present_complete_time)
    /// None for the very first frame.
    pub frame_duration: Option<Duration>,

    /// Expected frame duration based on display refresh rate.
    /// Used to detect missed frames.
    pub expected_frame_duration: Duration,

    /// Whether this frame was likely missed (frame_duration > 1.5 * expected)
    pub missed: bool,

    /// Number of frames missed (0 = on time, 1 = one frame late, etc.)
    /// Computed as: round(frame_duration / expected_frame_duration) - 1
    pub missed_count: u32,
}
```

**Design Decisions:**

- `frame_number` starts at 0 and increments monotonically. Never resets.
- `submit_time` is captured via `clock.now()` immediately before `queue.submit()`. This is the last CPU-side action before the GPU takes over.
- `present_complete_time` is captured via `clock.now()` immediately after `future.wait(None)` returns (the fence signals). This is the earliest CPU-side indication that the GPU has finished and the frame has been queued for scanout.
- `missed` uses a 1.5x threshold: if `frame_duration > 1.5 * expected_frame_duration`, the frame is considered missed. This threshold is standard in vision science tools (Psychtoolbox uses a similar heuristic).
- `missed_count` gives a more precise count for cases where multiple frames were dropped (e.g., a long stall causes a 3-frame skip).

### 3. `src/timing/flip_logger.rs` - Flip Logger

**Purpose:** Record every `FlipInfo` for the entire session. Provide CSV export and in-memory query access.

```rust
/// Records timing information for every frame flip.
///
/// The FlipLogger stores all FlipInfo records in memory during the
/// session and can export them to CSV for post-hoc analysis.
///
/// # Memory Usage
///
/// Each FlipInfo record is ~64 bytes. At 60 Hz:
/// - 1 minute  = 3,600 records  = ~225 KB
/// - 1 hour    = 216,000 records = ~13 MB
/// - 10 hours  = 2,160,000 records = ~130 MB
///
/// For very long sessions, consider periodic CSV flushes (future feature).
pub struct FlipLogger {
    /// All recorded flip records
    records: Vec<FlipInfo>,

    /// Optional file path for CSV export on drop/close
    csv_path: Option<PathBuf>,

    /// Pre-allocated capacity hint
    capacity: usize,
}
```

**Key Methods:**

```rust
impl FlipLogger {
    /// Create a new flip logger.
    ///
    /// # Arguments
    /// * `capacity` - Pre-allocate storage for this many frames.
    ///   At 60 Hz, use 3600 * expected_minutes for zero reallocation.
    pub fn new(capacity: usize) -> Self;

    /// Create a logger that will write CSV on close.
    pub fn with_csv(path: impl Into<PathBuf>, capacity: usize) -> Self;

    /// Record a flip.
    pub fn record(&mut self, info: FlipInfo);

    /// Get all records.
    pub fn records(&self) -> &[FlipInfo];

    /// Get the most recent record.
    pub fn last(&self) -> Option<&FlipInfo>;

    /// Total number of recorded flips.
    pub fn frame_count(&self) -> u64;

    /// Number of missed frames.
    pub fn missed_frame_count(&self) -> u64;

    /// Export all records to CSV.
    ///
    /// CSV columns:
    /// frame_number, submit_time_us, present_complete_time_us,
    /// frame_duration_us, expected_frame_duration_us, missed, missed_count
    pub fn export_csv(&self, path: impl AsRef<Path>) -> Result<(), std::io::Error>;

    /// Flush to CSV if a path was configured, then clear records.
    /// Useful for very long sessions to bound memory usage.
    pub fn flush(&mut self) -> Result<(), std::io::Error>;
}

impl Drop for FlipLogger {
    /// Automatically export CSV on drop if a path was configured.
    fn drop(&mut self) {
        if let Some(path) = &self.csv_path {
            if let Err(e) = self.export_csv(path) {
                eprintln!("Warning: failed to write flip log CSV: {}", e);
            }
        }
    }
}
```

**CSV Format:**

```csv
frame_number,submit_time_us,present_complete_time_us,frame_duration_us,expected_frame_duration_us,missed,missed_count
0,1023,17845,,16667,false,0
1,17901,34512,16667,16667,false,0
2,34570,51178,16666,16667,false,0
3,51235,84523,33345,16667,true,1
...
```

- All times in microseconds (integer) for maximum precision in CSV
- `frame_duration_us` is empty for the first frame (no prior reference)
- This format is directly loadable by pandas, R, MATLAB, etc.

### 4. `src/timing/stats.rs` - Timing Statistics

**Purpose:** Compute summary statistics from flip records. These are the numbers a vision scientist checks to validate their setup before running an experiment.

```rust
/// Summary statistics computed from flip timing records.
///
/// These statistics answer the question: "Is my system delivering
/// frames reliably enough for my experiment?"
#[derive(Debug, Clone)]
pub struct TimingStats {
    /// Total number of frames
    pub total_frames: u64,

    /// Total duration of the recording (seconds)
    pub total_duration_secs: f64,

    /// Measured refresh rate (Hz)
    pub measured_refresh_rate: f64,

    /// Mean frame duration (microseconds)
    pub mean_frame_duration_us: f64,

    /// Standard deviation of frame duration (microseconds)
    pub std_frame_duration_us: f64,

    /// Minimum frame duration (microseconds)
    pub min_frame_duration_us: f64,

    /// Maximum frame duration (microseconds)
    pub max_frame_duration_us: f64,

    /// Median frame duration (microseconds)
    pub median_frame_duration_us: f64,

    /// Number of missed (dropped) frames
    pub missed_frames: u64,

    /// Percentage of frames that were missed
    pub missed_frame_pct: f64,

    /// Total number of frame slots lost to misses
    /// (e.g., one double-miss counts as 2)
    pub total_missed_slots: u64,
}
```

**Key Methods:**

```rust
impl TimingStats {
    /// Compute statistics from a slice of FlipInfo records.
    ///
    /// Requires at least 2 records (need at least one inter-frame interval).
    /// Returns None if fewer than 2 records are provided.
    pub fn compute(records: &[FlipInfo]) -> Option<Self>;

    /// Pretty-print the statistics to stdout.
    ///
    /// Output format:
    /// ```text
    /// === VSE Timing Report ===
    /// Total frames:    3600
    /// Duration:        60.00 s
    /// Refresh rate:    60.00 Hz
    /// Frame duration:  16667 +/- 42 us (min: 16580, max: 16790, median: 16665)
    /// Missed frames:   0 / 3600 (0.00%)
    /// =========================
    /// ```
    pub fn print_report(&self);
}
```

**Implementation Notes:**
- Standard deviation uses the population formula (not sample), since we have every frame
- Median requires sorting a copy of durations; this is computed on-demand, not maintained live
- `measured_refresh_rate` = `1_000_000.0 / mean_frame_duration_us`

### 5. `src/timing/mod.rs` - Module Root

```rust
//! Timing and synchronization infrastructure
//!
//! This module provides high-resolution timing measurement,
//! per-frame flip logging, and timing statistics for validating
//! stimulus presentation precision.

mod clock;
mod flip_info;
mod flip_logger;
mod stats;

pub use clock::{Clock, Timestamp};
pub use flip_info::FlipInfo;
pub use flip_logger::FlipLogger;
pub use stats::TimingStats;
```

## Integration With Existing Code

### Changes to `src/lib.rs`

Add the new timing module and re-export key types in the prelude:

```rust
pub mod core;
pub mod timing;  // NEW

pub mod prelude {
    pub use crate::core::{
        DeviceSelector, Frame, GPUPreference, PresentMode, RenderContext,
        SwapchainConfig, SwapchainManager, VSEContext, VSEContextBuilder, VSEError,
    };
    // NEW: timing types in prelude
    pub use crate::timing::{FlipInfo, FlipLogger, TimingStats};
}
```

### Changes to `src/core/context.rs`

This is where the bulk of integration happens. The key changes:

#### 1. Add timing fields to `VSEConfig`

```rust
pub struct VSEConfig {
    // ... existing fields ...

    /// Enable flip timing (adds small overhead per frame)
    pub flip_logging: bool,

    /// Optional CSV path for automatic flip log export on shutdown
    pub flip_log_csv_path: Option<PathBuf>,

    /// Expected refresh rate in Hz (used for missed frame detection).
    /// If None, auto-detected from first few frames.
    pub expected_refresh_rate: Option<f64>,
}
```

#### 2. Add builder methods

```rust
impl VSEContextBuilder {
    /// Enable flip timing and logging.
    ///
    /// When enabled, every `flip()` call records timing data.
    /// This adds ~1-2 us of overhead per frame (negligible).
    pub fn with_flip_logging(mut self, enabled: bool) -> Self;

    /// Set CSV path for automatic flip log export.
    ///
    /// The CSV file is written when the context shuts down.
    /// Implies `with_flip_logging(true)`.
    pub fn with_flip_log_csv(mut self, path: impl Into<PathBuf>) -> Self;

    /// Set expected refresh rate for missed frame detection.
    ///
    /// If not set, the refresh rate is auto-detected from the
    /// first 10 frames. Set this explicitly if you know your
    /// display's refresh rate for immediate missed-frame detection.
    pub fn with_expected_refresh_rate(mut self, hz: f64) -> Self;
}
```

#### 3. Add timing state to `VSEState`

```rust
struct VSEState {
    // ... existing fields ...

    /// High-resolution clock (epoch = context creation time)
    clock: Clock,

    /// Flip logger (if timing enabled)
    flip_logger: Option<FlipLogger>,

    /// Frame counter
    frame_number: u64,

    /// Timestamp of previous flip's present_complete (for inter-frame duration)
    last_present_time: Option<Timestamp>,

    /// Expected frame duration in microseconds.
    /// Starts as None if not configured; auto-detected from first frames.
    expected_frame_duration: Option<Duration>,

    /// Accumulator for auto-detecting refresh rate from first N frames
    refresh_detect_samples: Vec<Duration>,
}
```

#### 4. Modify `RenderContext::flip()` return type

The `flip()` method changes from `Result<(), VSEError>` to `Result<FlipInfo, VSEError>`.

**Timing measurement points inside `flip()`:**

```
1. submit_time = clock.now()          // right before queue submit
2. ... GPU executes ...
3. ... fence.wait(None) ...           // blocks until GPU done
4. present_complete_time = clock.now() // right after fence signals
5. Compute frame_duration, missed detection
6. Record to FlipLogger
7. Return FlipInfo
```

The critical change to the existing `flip()` body in `context.rs`:

```rust
pub fn flip(&mut self) -> Result<FlipInfo, VSEError> {
    if self.state.minimized {
        // Return a synthetic FlipInfo for minimized frames
        // (no actual presentation happened)
        return Ok(FlipInfo::skipped(self.state.frame_number));
    }

    // Handle swapchain recreation if needed
    // ... (same as Phase 1) ...

    // Acquire next image
    // ... (same as Phase 1) ...

    // Record and execute clear command
    // ... (same as Phase 1) ...

    // --- TIMING: capture submit time ---
    let submit_time = self.state.clock.now();

    // Execute and present (this includes fence wait)
    let future = self.state.frame_builder.execute(frame)?;
    match self.state.swapchain.present(..., future) { ... }

    // --- TIMING: capture present complete time ---
    let present_complete_time = self.state.clock.now();

    // Compute inter-frame duration
    let frame_duration = self.state.last_present_time
        .map(|prev| present_complete_time.duration_since(prev));

    // Auto-detect refresh rate if needed
    if self.state.expected_frame_duration.is_none() {
        if let Some(dur) = frame_duration {
            self.state.refresh_detect_samples.push(dur);
            if self.state.refresh_detect_samples.len() >= 10 {
                let avg = /* mean of samples */;
                self.state.expected_frame_duration = Some(avg);
            }
        }
    }

    let expected = self.state.expected_frame_duration
        .unwrap_or(Duration::from_micros(16_667)); // 60 Hz fallback

    // Missed frame detection
    let (missed, missed_count) = match frame_duration {
        Some(dur) => {
            let ratio = dur.as_micros() as f64 / expected.as_micros() as f64;
            if ratio > 1.5 {
                (true, (ratio.round() as u32).saturating_sub(1))
            } else {
                (false, 0)
            }
        }
        None => (false, 0),
    };

    let flip_info = FlipInfo {
        frame_number: self.state.frame_number,
        submit_time,
        present_complete_time,
        frame_duration,
        expected_frame_duration: expected,
        missed,
        missed_count,
    };

    // Record to logger
    if let Some(logger) = &mut self.state.flip_logger {
        logger.record(flip_info.clone());
    }

    // Update state for next frame
    self.state.last_present_time = Some(present_complete_time);
    self.state.frame_number += 1;

    Ok(flip_info)
}
```

#### 5. Add timing access methods to `RenderContext`

```rust
impl<'a> RenderContext<'a> {
    /// Get the flip logger (if timing is enabled).
    pub fn flip_logger(&self) -> Option<&FlipLogger>;

    /// Get computed timing statistics from all recorded frames.
    /// Returns None if fewer than 2 frames have been recorded.
    pub fn timing_stats(&self) -> Option<TimingStats>;

    /// Print a timing report to stdout.
    /// No-op if timing is not enabled or fewer than 2 frames recorded.
    pub fn print_timing_report(&self);

    /// Get the timing clock (for correlating with external events).
    pub fn clock(&self) -> &Clock;

    /// Get the current frame number (before the next flip).
    pub fn frame_number(&self) -> u64;
}
```

### Changes to `src/core/swapchain.rs`

The `present()` method currently does `future.wait(None).ok()` internally. For timing, we need the fence wait to happen in a place where we can measure it. Two options:

**Option A (Preferred): Move fence wait into `flip()`**

Change `SwapchainManager::present()` to return the flushed future instead of waiting internally. The caller (`flip()`) then does the wait and captures timing around it.

```rust
// Before (Phase 1):
pub fn present<F>(&mut self, queue, image_index, wait_future) -> Result<(), SwapchainError>
{
    let result = wait_future
        .then_swapchain_present(queue, present_info)
        .then_signal_fence_and_flush();
    match result {
        Ok(future) => {
            future.wait(None).ok();  // wait is INSIDE present()
            Ok(())
        }
        ...
    }
}

// After (Phase 2):
pub fn present<F>(&mut self, queue, image_index, wait_future)
    -> Result<Box<dyn GpuFuture>, SwapchainError>
{
    let result = wait_future
        .then_swapchain_present(queue, present_info)
        .then_signal_fence_and_flush();
    match result {
        Ok(future) => Ok(Box::new(future)),  // return future, let caller wait
        ...
    }
}
```

Then in `flip()`:

```rust
let submit_time = self.state.clock.now();
let future = self.state.frame_builder.execute(frame)?;
let fence_future = self.state.swapchain.present(queue, image_index, future)?;
fence_future.wait(None).ok();
let present_complete_time = self.state.clock.now();
```

This is cleaner because timing measurement is fully contained in `flip()`.

**Option B: Keep wait inside `present()`, pass clock in**

Less clean but avoids changing the `present()` signature. Not recommended.

**Recommendation: Option A.** It's a small refactor and gives `flip()` full control over timing measurement placement.

**User Directive:** Implement Option A for cleaner timing integration.

### Changes to `Cargo.toml`

Add the `csv` and `serde` dependencies needed for flip log export:

```toml
[dependencies]
# ... existing dependencies ...

# Serialization for timing logs
serde = { version = "1.0", features = ["derive"] }
csv = "1.3"
```

These are lightweight and already listed in the ProjectPlan.md as planned dependencies.

## New Example: `examples/01_timing_validation.rs`

**Purpose:** Demonstrate timing infrastructure and validate presentation precision.

```rust
//! Phase 2 Milestone: Timing Validation
//!
//! This example measures and reports frame timing precision.
//! Run for at least 60 seconds to get stable statistics.
//!
//! # Running
//!
//! ```bash
//! cargo run --example 01_timing_validation
//! cargo run --release --example 01_timing_validation  # for production timing
//! ```

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    println!("VSE Phase 2 - Timing Validation");
    println!("================================");
    println!("Running for 60 seconds. Close window or wait for auto-stop.");
    println!();

    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("VSE Phase 2 - Timing Validation")
        .with_clear_color(0.3, 0.3, 0.3, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .with_flip_logging(true)
        .with_flip_log_csv("timing_validation.csv")
        .build()?;

    let target_frames: u64 = 60 * 60; // ~60 seconds at 60 Hz

    context.run(move |vse| {
        vse.clear()?;
        let info = vse.flip()?;

        // Print periodic updates
        if info.frame_number % 300 == 0 && info.frame_number > 0 {
            if let Some(stats) = vse.timing_stats() {
                println!(
                    "Frame {:>6}: {:.1} Hz | jitter: {:.0} us | missed: {}",
                    info.frame_number,
                    stats.measured_refresh_rate,
                    stats.std_frame_duration_us,
                    stats.missed_frames,
                );
            }
        }

        // Log missed frames immediately
        if info.missed {
            println!(
                "*** MISSED FRAME {} (duration: {:.2} ms, expected: {:.2} ms, missed_count: {})",
                info.frame_number,
                info.frame_duration.map(|d| d.as_micros() as f64 / 1000.0).unwrap_or(0.0),
                info.expected_frame_duration.as_micros() as f64 / 1000.0,
                info.missed_count,
            );
        }

        // Auto-stop after target frames
        if info.frame_number >= target_frames {
            vse.print_timing_report();
            // Signal close (need to add request_close() method)
        }

        Ok(())
    })?;

    println!();
    println!("Timing log written to: timing_validation.csv");
    println!("Clean shutdown complete!");

    Ok(())
}
```

## Implementation Order

### Step 1: Create `src/timing/clock.rs`

**Estimated complexity:** Simple. No Vulkan dependencies.

1. Implement `Clock` struct wrapping `Instant`
2. Implement `Timestamp` struct wrapping `u64` microseconds
3. Add `serde::Serialize` derive to `Timestamp`
4. Write unit tests:
   - `test_clock_monotonicity`: two successive `now()` calls produce increasing timestamps
   - `test_timestamp_conversions`: verify `as_secs_f64()`, `as_millis_f64()`, `as_micros()`
   - `test_timestamp_duration`: verify `duration_since()` arithmetic

### Step 2: Create `src/timing/flip_info.rs`

**Estimated complexity:** Simple. Pure data struct.

1. Define `FlipInfo` struct with all fields
2. Add `FlipInfo::skipped()` constructor for minimized window frames
3. Derive `Debug, Clone, serde::Serialize`
4. Write unit tests:
   - `test_flip_info_skipped`: verify skipped frame has correct defaults
   - `test_missed_frame_detection`: verify the 1.5x threshold logic (test as a standalone function)

### Step 3: Create `src/timing/flip_logger.rs`

**Estimated complexity:** Moderate. File I/O for CSV export.

1. Implement `FlipLogger` with `Vec<FlipInfo>` storage
2. Implement `record()`, `records()`, `last()`, `frame_count()`, `missed_frame_count()`
3. Implement `export_csv()` using the `csv` crate
4. Implement `flush()` for long sessions
5. Implement `Drop` for auto-export
6. Write unit tests:
   - `test_logger_record_and_retrieve`: record several FlipInfos and verify
   - `test_logger_missed_count`: record mix of normal and missed, verify count
   - `test_csv_export`: export to tempfile, read back, verify contents
   - `test_csv_format`: verify column headers and data formatting

### Step 4: Create `src/timing/stats.rs`

**Estimated complexity:** Moderate. Statistics math.

1. Implement `TimingStats::compute()` from `&[FlipInfo]`
2. Implement `print_report()` formatted output
3. Write unit tests:
   - `test_stats_uniform_frames`: synthetic data at exactly 16667 us => std ~0
   - `test_stats_with_jitter`: synthetic data with known jitter => verify mean/std
   - `test_stats_with_missed`: include a doubled frame => verify missed_frames = 1
   - `test_stats_insufficient_data`: < 2 records returns None
   - `test_stats_report_output`: verify `print_report()` doesn't panic

### Step 5: Wire `src/timing/mod.rs` and update `src/lib.rs`

**Estimated complexity:** Trivial.

1. Create mod.rs with public re-exports
2. Add `pub mod timing;` to lib.rs
3. Add timing types to prelude
4. `cargo check` should pass

### Step 6: Add dependencies to `Cargo.toml`

**Estimated complexity:** Trivial.

1. Add `serde = { version = "1.0", features = ["derive"] }`
2. Add `csv = "1.3"`
3. `cargo check` should pass

### Step 7: Refactor `SwapchainManager::present()` (Option A)

**Estimated complexity:** Moderate. Must not break existing behavior.

1. Change return type to return the fenced future instead of waiting internally
2. Update `RenderContext::flip()` to call `future.wait(None)` itself
3. Verify existing `00_clear_color` example still compiles and runs
4. Note: the `flip()` return type hasn't changed yet at this step - that's Step 8

### Step 8: Integrate timing into `RenderContext::flip()`

**Estimated complexity:** Most complex step. Touches context.rs heavily.

1. Add `Clock`, `FlipLogger`, timing state to `VSEState`
2. Add builder methods to `VSEContextBuilder`
3. Update `VSEConfig` with timing fields
4. Initialize timing state in `VSEContext::initialize()`
5. Modify `RenderContext::flip()`:
   - Change return type to `Result<FlipInfo, VSEError>`
   - Add timing measurement points
   - Add refresh rate auto-detection
   - Add missed frame detection
   - Record to FlipLogger
6. Add accessor methods to `RenderContext` (`flip_logger()`, `timing_stats()`, etc.)
7. Update `00_clear_color` example for new `flip()` signature (just add `let _info =`)
8. Run `cargo test`, `cargo clippy`, `cargo fmt`

### Step 9: Create `examples/01_timing_validation.rs`

**Estimated complexity:** Simple. Uses the new API.

1. Write the timing validation example
2. Run it for 60 seconds and verify output
3. Verify CSV file is generated with correct format
4. Load CSV in a data analysis tool to sanity-check

### Step 10: Final validation

1. `cargo check` - clean
2. `cargo test` - all pass
3. `cargo clippy --all-targets` - no warnings
4. `cargo fmt --check` - formatted
5. Run `01_timing_validation` in release mode for 10 minutes
6. Verify jitter < 1 ms standard deviation
7. Verify zero missed frames (on a quiet system with FIFO mode)
8. Verify CSV loads correctly in external tools

## Testing Strategy

### Unit Tests (Pure Logic, No GPU Needed)

These tests run in `cargo test` without any display or GPU:

```rust
// timing/clock.rs
#[test] fn test_clock_monotonicity()
#[test] fn test_timestamp_conversions()
#[test] fn test_timestamp_duration()
#[test] fn test_timestamp_zero()
#[test] fn test_timestamp_ordering()

// timing/flip_info.rs
#[test] fn test_flip_info_clone()
#[test] fn test_flip_info_skipped()

// timing/flip_logger.rs
#[test] fn test_logger_empty()
#[test] fn test_logger_record_and_retrieve()
#[test] fn test_logger_missed_count()
#[test] fn test_csv_export()
#[test] fn test_csv_column_headers()
#[test] fn test_logger_flush()

// timing/stats.rs
#[test] fn test_stats_insufficient_data()
#[test] fn test_stats_uniform_frames()
#[test] fn test_stats_with_jitter()
#[test] fn test_stats_with_missed_frame()
#[test] fn test_stats_print_report()
```

### Integration Tests (Require GPU, Manual)

```bash
# Run timing validation example
cargo run --example 01_timing_validation

# Run in release mode for production timing
cargo run --release --example 01_timing_validation
```

### Benchmark (Optional, If Time Permits)

Add to `benches/frame_timing.rs`:

```rust
// Benchmark the overhead of timing measurement itself
fn bench_clock_now(c: &mut Criterion) {
    let clock = Clock::new();
    c.bench_function("clock_now", |b| {
        b.iter(|| clock.now())
    });
}

// Benchmark FlipLogger record
fn bench_logger_record(c: &mut Criterion) {
    let mut logger = FlipLogger::new(10_000);
    let info = /* synthetic FlipInfo */;
    c.bench_function("logger_record", |b| {
        b.iter(|| logger.record(info.clone()))
    });
}
```

Goal: `clock.now()` < 100 ns, `logger.record()` < 500 ns.

## Edge Cases and Considerations

### Minimized Window

When the window is minimized, no presentation happens. `flip()` should return a `FlipInfo` with a special marker (the `FlipInfo::skipped()` constructor). Skipped frames should NOT be recorded in the FlipLogger and should NOT affect timing statistics.

### Swapchain Recreation

When the swapchain is recreated (window resize, out-of-date), the frame may be skipped. This should be handled similarly to minimized: return a skipped FlipInfo, don't record.

### First Frame

The first frame has no prior reference for `frame_duration`. This is explicitly `None` in `FlipInfo`. The stats computation ignores frames without duration.

### Refresh Rate Auto-Detection

The first 10 frames are used to estimate the display refresh rate if not explicitly configured. During these 10 frames, missed frame detection uses the 60 Hz fallback (16,667 us). After auto-detection, the measured rate is used.

This is a pragmatic choice. The alternative (querying the display mode via Vulkan) is possible but platform-dependent and not always accurate. Measuring actual frame intervals is more reliable.

### Very Long Sessions

At 60 Hz, 10 hours = ~2.16 million records = ~130 MB. This is acceptable for most experiments. For multi-day recordings, the `FlipLogger::flush()` method can periodically write to CSV and clear the in-memory buffer. This is a future optimization; for Phase 2, in-memory storage is sufficient.

### Thread Safety

`Clock` is `Send + Sync` (wraps `Instant`, which is). `FlipLogger` is single-threaded (used only in the render callback). No synchronization needed for Phase 2.

## Future Phase 2+ Extensions (Not In Scope Now)

These are noted for awareness but explicitly deferred:

1. **GPU Timestamp Queries** (`vkCmdWriteTimestamp`): Would give true GPU-side timing. Requires query pool management. Defer to Phase 2b or Phase 5.

2. **`VK_GOOGLE_display_timing`**: Extension that reports actual scanout timestamps. Not widely supported. Defer indefinitely; rely on photodiode validation instead.

3. **Real-Time Thread Priority**: `sched_setscheduler(SCHED_FIFO)` on Linux for the render thread. Significant OS-level concern. Defer to Phase 5 (Hardware Integration).

4. **Spin-Wait for Critical Timing**: Replace `sleep()` with spin-wait for the final microseconds before a critical flip. Useful for stimulus onset timing but increases CPU usage. Defer to Phase 5.

5. **Multi-Monitor Support**: Different monitors at different refresh rates. Complex swapchain management. Defer to Phase 7.

## Dependency Summary

### New Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `serde` | 1.0 (features: derive) | Serialize FlipInfo for CSV |
| `csv` | 1.3 | CSV file export |

### Existing Dependencies (No Changes)

- vulkano 0.34
- winit 0.29
- anyhow 1.0
- thiserror 1.0
- tracing 0.1
- tracing-subscriber 0.3

## Files Modified (Summary)

| File | Change |
|------|--------|
| `Cargo.toml` | Add `serde`, `csv` dependencies |
| `src/lib.rs` | Add `pub mod timing;`, expand prelude |
| `src/core/mod.rs` | No change |
| `src/core/context.rs` | Add timing fields, modify `flip()` return type, add builder methods, add RenderContext accessors |
| `src/core/swapchain.rs` | Refactor `present()` to return future instead of waiting |
| `src/timing/mod.rs` | **NEW** - module root |
| `src/timing/clock.rs` | **NEW** - high-resolution clock |
| `src/timing/flip_info.rs` | **NEW** - FlipInfo struct |
| `src/timing/flip_logger.rs` | **NEW** - flip logging with CSV export |
| `src/timing/stats.rs` | **NEW** - timing statistics |
| `examples/00_clear_color.rs` | Update for new `flip()` return type |
| `examples/01_timing_validation.rs` | **NEW** - timing validation example |

## Success Checklist

Phase 2 is complete when:

- [ ] `cargo build` succeeds without warnings
- [ ] `cargo test` passes all timing unit tests
- [ ] `cargo clippy --all-targets` shows no warnings
- [ ] `cargo fmt --check` passes
- [ ] `examples/00_clear_color.rs` still runs correctly
- [ ] `examples/01_timing_validation.rs` runs for 60 seconds
- [ ] CSV file is generated with correct format and headers
- [ ] CSV loads correctly in pandas/R/MATLAB
- [ ] Frame duration jitter < 1 ms std dev at 60 Hz (release mode, FIFO, quiet system)
- [ ] Missed frames are detected and reported
- [ ] `print_timing_report()` produces clear, readable output
- [ ] Auto-refresh-rate detection converges within 10 frames
- [ ] Window resize doesn't corrupt timing statistics
- [ ] Minimized window doesn't produce spurious missed frame reports
