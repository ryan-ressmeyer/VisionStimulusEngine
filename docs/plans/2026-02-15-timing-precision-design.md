# Timing Precision Upgrade — Design Document

**Date:** 2026-02-15
**Status:** Approved

## Goal

Upgrade VSE's timing infrastructure to support hardware-verified presentation timestamps and scheduled frame presentation, falling back gracefully when hardware support isn't available.

## Background

Phase 2 established CPU-based timing: `std::time::Instant` captures timestamps around the fence wait in `present()`. This gives microsecond-resolution CPU timestamps but doesn't reflect actual display scanout time — there's an unknown delay between fence signal and photons hitting the screen.

Vulkan provides two extensions that solve this:

- **VK_GOOGLE_display_timing** — available on Linux (Mesa) and Android. Reports actual display presentation times and refresh cycle duration. Allows scheduling presents at target times via `VkPresentTimeGOOGLE`.
- **VK_EXT_present_timing** — the new cross-platform standard (merged into Vulkan spec late 2025). Supersedes GOOGLE_display_timing with hardware scanout timestamps and driver-managed frame scheduling. Not yet available in Rust bindings (ash or vulkano) as of February 2026.

## Design

### TimingSource Enum

Stored in VSEState, written into every FlipInfo, exported in CSV:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TimingSource {
    /// VK_EXT_present_timing (future — not yet implemented)
    ExtPresentTiming,
    /// VK_GOOGLE_display_timing
    GoogleDisplayTiming,
    /// CPU estimation (std::time::Instant around fence wait)
    CpuEstimate,
}
```

### TimingProvider Trait

Abstracts the timing backend. Stored as `Box<dyn TimingProvider>` in VSEState:

```rust
pub trait TimingProvider {
    /// Which timing source this provider uses
    fn source(&self) -> TimingSource;

    /// Get the display refresh cycle duration (e.g., 16.6ms for 60Hz)
    fn refresh_cycle_duration(&self) -> Option<Duration>;

    /// Get the actual present time for a completed frame.
    /// Returns None if timing data isn't available yet.
    fn get_present_time(&self, present_id: u64) -> Option<Timestamp>;

    /// Submit a present with an optional target time.
    fn present(
        &self,
        queue: &Arc<Queue>,
        swapchain: ...,
        target_time: Option<Timestamp>,
    ) -> Result<Timestamp, ...>;
}
```

Three implementations:

1. **CpuTimingProvider** — current Phase 2 logic. `get_present_time()` returns `clock.now()` after fence signal. Scheduled presents use CPU spin-wait. Refresh rate auto-detected from first 10 frames.

2. **GoogleDisplayTimingProvider** — uses ash 0.38 function pointers loaded at device creation. Calls `vkGetPastPresentationTimingGOOGLE` for present times, `vkGetRefreshCycleDurationGOOGLE` for refresh rate, attaches `VkPresentTimeGOOGLE` for scheduled presents.

3. **ExtPresentTimingProvider** — stubbed. Not selectable at runtime. Exists as documentation of the intended future path.

### Detection at Init

During `VSEContext::initialize`:

1. Check physical device extensions for `VK_GOOGLE_display_timing`
2. If found → enable the extension, create `GoogleDisplayTimingProvider`
3. If not → create `CpuTimingProvider`
4. Log the selected provider at INFO level

### FlipInfo Refactor

Two timestamps plus timing source. No derived fields (frame_duration is computable from CSV):

```rust
pub struct FlipInfo {
    pub frame_number: u64,
    pub timing_source: TimingSource,
    pub submit_time: Timestamp,    // CPU instant before command buffer submission
    pub present_time: Timestamp,   // Meaning depends on timing_source
    pub missed: bool,
    pub missed_count: u32,
    pub skipped: bool,
}
```

`present_time` semantics by tier:
- **GoogleDisplayTiming** → driver-reported display presentation time
- **CpuEstimate** → CPU instant after fence signal

### Flip API

Single method with optional target time:

```rust
pub fn flip(&mut self, target_time: Option<Timestamp>) -> Result<FlipInfo, VSEError>
```

- `flip(None)` — present immediately (current behavior)
- `flip(Some(t))` — schedule present for time `t`
  - GoogleDisplayTiming: passed to `VkPresentTimeGOOGLE`
  - CpuEstimate: spin-wait until `t`, then submit. Warning logged on first call.

Breaking change: existing `flip()` calls become `flip(None)`.

### CSV Output

New format:

```
frame_number,timing_source,submit_time_us,present_time_us,missed,missed_count
```

### TimingStats

Derives inter-frame durations from consecutive `present_time` values in the log records, rather than reading a stored `frame_duration` field.

## Dependency Changes

- `vulkano`: 0.34 → 0.35 (brings ash 0.38 with GOOGLE_display_timing bindings)
- `vulkano-shaders`: 0.34 → 0.35
- Add `ash`: 0.38 as direct dependency for raw extension function calls

## Console Output

Startup:
```
INFO: Timing backend: GoogleDisplayTiming (VK_GOOGLE_display_timing)
INFO: Refresh cycle duration: 16666 us (60.0 Hz)
```
or:
```
WARN: VK_GOOGLE_display_timing not available. Using CPU estimation.
INFO: Refresh rate will be auto-detected from first 10 frames.
```

First scheduled flip without hardware support:
```
WARN: flip() called with target_time but VK_GOOGLE_display_timing is not available.
      Falling back to CPU spin-wait. Timing will be approximate.
```

## Future: VK_EXT_present_timing

VK_EXT_present_timing was merged into the Vulkan spec in late 2025. It provides:
- Hardware scanout timestamps (more precise than GOOGLE_display_timing)
- Driver-managed frame scheduling (CPU is freed immediately after submit)
- Cross-platform support (not just Linux/Android)

As of February 2026, no Rust Vulkan bindings support it:
- **ash**: Expected in 0.39+ (Vulkan 1.4 support in development)
- **vulkano**: Will need to update to ash 0.39+ first

The `TimingProvider` trait is designed so that `ExtPresentTimingProvider` can be added as a drop-in implementation when bindings become available. The detection logic at init will prefer it over GoogleDisplayTiming when present.

We intend to contribute to getting VK_EXT_present_timing support into vulkano when upstream ash support lands.
