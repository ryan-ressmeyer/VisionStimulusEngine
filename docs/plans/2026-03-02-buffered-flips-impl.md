# Buffered Flips Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `run_buffered<T>()` — an explicit pipelined flip variant that pipelines CPU/GPU
work across frames, delivers confirmed hardware scanout timestamps to `record_frame()`, and
supports closed-loop experiments with B-frame latency.

**Architecture:** A new `FlipEvent<T>` event-enum dispatches two alternating phases per
vblank: `Render` (build and submit non-blocking) and `Presented` (react to confirmed timing,
record data). A `VecDeque<PendingFrame<T>>` local to `run_buffered()` carries typed payloads
from `Render` to `Presented` via a `Box<dyn Any>` transit slot in `VSEState`. In-flight GPU
fences are kept alive in a `VecDeque<Box<dyn InFlightFuture>>` to prevent implicit blocking
on drop; `vkGetPastPresentationTimingGOOGLE` confirms scanout times when available.

**Tech Stack:** Rust, vulkano 0.34, winit 0.29, serde, existing VSE timing/recording infra.

---

## Orientation

Key files you will touch in almost every task:
- **`src/core/context.rs`** — `VSEError` (line 44), `RecordingState` (line 330),
  `VSEState` (line 371), `RenderContext` (line 1131), `flip()` (line 1161).
- **`src/core/swapchain.rs`** — `SwapchainManager::present()` (line 387).
- **`src/core/mod.rs`** — public re-exports.
- **`src/lib.rs`** — prelude.
- **`src/timing/provider.rs`** — `TimingProvider` trait.

Run after every task: `cargo check && cargo test && cargo clippy --all-targets && cargo fmt`

---

## Task 1: Core types — `BufferedConfig`, `FlipEvent<T>`, `InFlightFuture`

**Files:**
- Create: `src/core/buffered.rs`
- Modify: `src/core/mod.rs`

**Step 1: Write the failing compile test**

Add to the bottom of `src/core/mod.rs` (temporarily, will be removed after Task 9):

```rust
#[cfg(test)]
mod buffered_compile_test {
    use super::*;
    #[test]
    fn buffered_config_default() {
        let cfg = BufferedConfig::default();
        assert_eq!(cfg.depth, 1);
    }
    #[test]
    fn flip_event_pattern_match() {
        // Verify the enum arms compile and the catch-all works
        let event: FlipEvent<u32> = FlipEvent::Render;
        match event {
            FlipEvent::Render => {}
            FlipEvent::Presented { .. } => {}
            _ => {}
        }
    }
}
```

**Step 2: Run to confirm it fails**

```bash
cargo test buffered_compile_test 2>&1 | head -20
```
Expected: `error[E0433]: failed to resolve: use of undeclared type BufferedConfig`

**Step 3: Create `src/core/buffered.rs`**

```rust
//! Buffered flip types — `BufferedConfig`, `FlipEvent<T>`, and internal fence abstraction.

use crate::data::OverflowBehavior;
use crate::timing::FlipInfo;

/// Configuration for [`VSEContext::run_buffered`].
///
/// # Example
/// ```
/// use vision_stimulus_engine::prelude::*;
/// let cfg = BufferedConfig { depth: 1, overflow: OverflowBehavior::Block };
/// ```
#[derive(Debug, Clone)]
pub struct BufferedConfig {
    /// Number of frames to pipeline ahead of confirmed GPU scanout.
    ///
    /// - `1` (double-buffering): CPU is one frame ahead of confirmed GPU output.
    ///   Swapchain image count is set to 2.
    /// - `2` (triple-buffering): CPU is two frames ahead. Swapchain image count is 3.
    ///
    /// Higher values increase GPU utilization but add closed-loop reaction latency.
    /// For most experiments `depth = 1` is the right choice.
    pub depth: usize,

    /// What to do when the pending-confirmation queue is full.
    ///
    /// - [`OverflowBehavior::Block`]: stall the render loop until space is available.
    ///   No data loss. Default.
    /// - [`OverflowBehavior::DropWithWarning`]: discard the oldest unconfirmed frame
    ///   and emit `tracing::warn!`. Never stalls; risk of data loss if writer falls behind.
    pub overflow: OverflowBehavior,
}

impl Default for BufferedConfig {
    fn default() -> Self {
        Self {
            depth: 1,
            overflow: OverflowBehavior::Block,
        }
    }
}

/// Events dispatched by [`VSEContext::run_buffered`].
///
/// The loop alternates between two phases each vblank:
///
/// 1. **`Presented`** (once the GPU confirms frame `N - depth` was scanned out)
/// 2. **`Render`** (build and submit frame `N`)
///
/// During startup (the first `depth` iterations), only `Render` fires — there are no
/// confirmed frames yet.
///
/// # Pattern matching
///
/// Because this enum is `#[non_exhaustive]`, always include a catch-all arm:
///
/// ```rust,ignore
/// match event {
///     FlipEvent::Render => { /* build frame */ }
///     FlipEvent::Presented { flip_info, payload } => { /* react + record */ }
///     _ => {}
/// }
/// ```
#[non_exhaustive]
pub enum FlipEvent<T> {
    /// Build and submit the next frame.
    ///
    /// Call [`RenderContext::flip_with_payload`] before returning from this arm.
    /// Drawing calls (`clear`, `draw_rect`, etc.) work normally here.
    /// `record_frame()` is **not** valid in this arm — call it in `Presented`.
    Render,

    /// A frame has been confirmed by the GPU/driver.
    ///
    /// `flip_info.present_time` is a confirmed hardware scanout timestamp when
    /// `GoogleDisplayTiming` is active; otherwise it is derived from the fence signal.
    ///
    /// `payload` is the value passed to `flip_with_payload()` when this frame was rendered.
    ///
    /// Call `vse.record_frame(payload)?` here to record with confirmed timing.
    /// Closed-loop stimulus adjustments based on `flip_info.missed` belong here.
    Presented {
        /// Confirmed timing for this frame.
        flip_info: FlipInfo,
        /// The per-frame data payload from the matching `Render` invocation.
        payload: T,
    },
}

/// Type-erased in-flight GPU future.
///
/// Keeps the `FenceSignalFuture` alive (dropping it would block) while allowing
/// non-blocking polling and deferred blocking drain on shutdown.
pub(crate) trait InFlightFuture {
    /// Returns `true` if the GPU fence has signaled (non-blocking poll).
    ///
    /// Implemented via `FenceSignalFuture::wait(Some(Duration::ZERO))`.
    fn is_complete(&self) -> bool;

    /// Block until the GPU fence signals. Used during shutdown drain.
    fn wait_blocking(&self);
}

/// A pending frame in the confirmation queue (local to `run_buffered()`).
pub(crate) struct PendingFrame<T> {
    pub frame_number: u64,
    pub payload: T,
    /// Best available timing at submit time. Replaced by confirmed timing in `Presented`.
    pub estimated_flip: FlipInfo,
}
```

**Step 4: Wire into `src/core/mod.rs`**

Add before the existing `pub use` lines:

```rust
mod buffered;
pub use buffered::{BufferedConfig, FlipEvent};
pub(crate) use buffered::{InFlightFuture, PendingFrame};
```

**Step 5: Run tests**

```bash
cargo test buffered_compile_test
```
Expected: both tests PASS.

**Step 6: Commit**

```bash
git add src/core/buffered.rs src/core/mod.rs
git commit -m "feat: add BufferedConfig, FlipEvent<T>, InFlightFuture types"
```

---

## Task 2: `VSEError` new variants

**Files:**
- Modify: `src/core/context.rs` (line 44, the `VSEError` enum)

**Step 1: Write failing test**

Add to the test block you added in Task 1 (or a new one inline in context.rs):

```rust
#[test]
fn vse_error_variants_display() {
    let e = VSEError::NoConfirmedFlip;
    assert!(e.to_string().contains("Presented"));

    let e = VSEError::NotInBufferedMode;
    assert!(e.to_string().contains("run_buffered"));

    let e = VSEError::NotSupportedInBufferedMode;
    assert!(e.to_string().contains("flip()"));
}
```

**Step 2: Run to confirm it fails**

```bash
cargo test vse_error_variants_display 2>&1 | head -10
```
Expected: compile error — variants do not exist yet.

**Step 3: Add variants to `VSEError` in `src/core/context.rs`**

Add after the existing `DataRecording` variant (around line 92):

```rust
/// `record_frame()` called in `FlipEvent::Render` arm of `run_buffered()`.
/// Move the `record_frame()` call to the `FlipEvent::Presented` arm.
#[error("record_frame() is only valid in the FlipEvent::Presented arm — \
         move it out of the Render arm")]
NoConfirmedFlip,

/// `flip_with_payload()` called outside of `run_buffered()`.
/// Use `flip()` in the standard `run()` loop instead.
#[error("flip_with_payload() requires run_buffered() — use flip() inside run()")]
NotInBufferedMode,

/// `flip()` called inside `run_buffered()`.
/// Replace with `flip_with_payload()` in the Render arm.
#[error("flip() is not supported in run_buffered() — use flip_with_payload() instead")]
NotSupportedInBufferedMode,
```

**Step 4: Run tests**

```bash
cargo test vse_error_variants_display
```
Expected: PASS.

**Step 5: Commit**

```bash
git add src/core/context.rs
git commit -m "feat: add VSEError variants for buffered flip mode"
```

---

## Task 3: `VSEState` fields for buffered mode

**Files:**
- Modify: `src/core/context.rs` (the `VSEState` struct, line 371)

**Step 1: Write failing test**

```rust
#[test]
fn vse_state_has_buffered_fields() {
    // Compile-time check: ensure the fields exist by using their names
    // This test lives in context.rs in a cfg(test) block
    let _: Option<Box<dyn std::any::Any + Send + 'static>> = None; // buffered_pending_payload type
    let _: Option<crate::timing::FlipInfo> = None;                  // buffered_confirmed_flip type
    let _: bool = false;                                            // in_buffered_mode type
}
```

This is a compile-check test only. Add it inside `context.rs` in an existing `#[cfg(test)]` block or create one.

**Step 2: Add fields to `VSEState`**

In `src/core/context.rs`, add to the `VSEState` struct after `recording`:

```rust
// --- Buffered flip state (None/false when using synchronous run()) ---

/// Transit slot: flip_with_payload() stores the payload here as a type-erased
/// Box<dyn Any>. run_buffered() takes it out after the Render callback returns
/// and downcasts it back to T. Always None outside the Render callback.
buffered_pending_payload: Option<Box<dyn std::any::Any + Send + 'static>>,

/// The confirmed FlipInfo for the frame being delivered in a Presented callback.
/// Set by run_buffered() before invoking the Presented arm; cleared after.
/// record_frame() reads this field instead of pending_flip when Some.
buffered_confirmed_flip: Option<FlipInfo>,

/// True while run_buffered() is executing. Guards flip_with_payload() and
/// prevents flip() from being called in that context.
in_buffered_mode: bool,
```

**Step 3: Initialize in `VSEState` construction**

Find the place where `VSEState` is constructed (search for `recording: None` in `context.rs`)
and add:

```rust
buffered_pending_payload: None,
buffered_confirmed_flip: None,
in_buffered_mode: false,
```

**Step 4: Compile check**

```bash
cargo check
```
Expected: clean.

**Step 5: Commit**

```bash
git add src/core/context.rs
git commit -m "feat: add buffered mode fields to VSEState"
```

---

## Task 4: Non-blocking present in `SwapchainManager`

**Files:**
- Modify: `src/core/swapchain.rs`

The existing `present()` calls `future.wait(None)` — this is the synchronous block. We need a
variant that returns immediately and hands back a `Box<dyn InFlightFuture>` to keep the fence
alive without blocking.

**Implementation note on vulkano 0.34 fences:**
`FenceSignalFuture::wait(&self, timeout: Option<Duration>)` — verify this signature in your
IDE before implementing `is_complete`. If it takes `&mut self`, change the trait method to
`&mut self`. `wait(Some(Duration::ZERO))` returns `Ok(())` if already signaled, `Err` on
timeout. This is the non-blocking poll.

**Step 1: Write failing compile test**

In `swapchain.rs` `#[cfg(test)]` block:

```rust
#[test]
fn swapchain_has_submit_nonblocking_signature() {
    // Just ensure the function signature compiles. Can't call it without GPU.
    let _: fn() = || {
        let _: &dyn Fn(
            Arc<vulkano::device::Queue>,
            u32,
            Box<dyn vulkano::sync::GpuFuture>,
        ) -> Result<Box<dyn crate::core::buffered::InFlightFuture>, crate::core::swapchain::SwapchainError> = &|_q, _i, _f| unimplemented!();
    };
}
```

This is a structural check — the real test is `cargo check` succeeding.

**Step 2: Add `submit_nonblocking` to `SwapchainManager`**

Add after the existing `present()` method (after line 414 in swapchain.rs):

```rust
/// Submit a frame to the GPU without blocking on fence completion.
///
/// Returns a `Box<dyn InFlightFuture>` that must be kept alive until the frame
/// is confirmed. Dropping it would trigger an implicit wait and defeat pipelining.
///
/// Used exclusively by `run_buffered()`. For synchronous rendering use `present()`.
///
/// # Errors
/// Returns `SwapchainError::PresentFailed` if submission fails.
/// Returns `SwapchainError::OutOfDate` if the swapchain needs recreation.
pub fn submit_nonblocking<F>(
    &mut self,
    queue: Arc<vulkano::device::Queue>,
    image_index: u32,
    wait_future: F,
) -> Result<Box<dyn crate::core::buffered::InFlightFuture>, SwapchainError>
where
    F: vulkano::sync::GpuFuture + 'static,
{
    use std::time::Duration;
    use crate::core::buffered::InFlightFuture;

    let present_info =
        SwapchainPresentInfo::swapchain_image_index(self.swapchain.clone(), image_index);

    let fence = wait_future
        .then_swapchain_present(queue, present_info)
        .then_signal_fence_and_flush()
        .map_err(|e| {
            if matches!(
                e,
                vulkano::Validated::Error(vulkano::VulkanError::OutOfDate)
            ) {
                self.needs_recreation = true;
                SwapchainError::OutOfDate
            } else {
                SwapchainError::PresentFailed(e.to_string())
            }
        })?;

    // Wrap in a concrete type that implements InFlightFuture without exposing
    // the complex generic parameter of FenceSignalFuture<F>.
    struct VulkanoFence<F: vulkano::sync::GpuFuture>(
        vulkano::sync::future::FenceSignalFuture<F>,
    );

    impl<F: vulkano::sync::GpuFuture + 'static> InFlightFuture for VulkanoFence<F> {
        fn is_complete(&self) -> bool {
            // wait(Some(Duration::ZERO)) returns Ok immediately if already signaled.
            // NOTE: if vulkano 0.34's wait() takes &mut self, change this to a Mutex.
            self.0.wait(Some(Duration::ZERO)).is_ok()
        }
        fn wait_blocking(&self) {
            let _ = self.0.wait(None);
        }
    }

    Ok(Box::new(VulkanoFence(fence)))
}
```

**Step 3: Compile check**

```bash
cargo check
```
Expected: clean. Fix any borrow/mut issues with `FenceSignalFuture::wait` signature here.

**Step 4: Commit**

```bash
git add src/core/swapchain.rs
git commit -m "feat: add SwapchainManager::submit_nonblocking for buffered flip"
```

---

## Task 5: `RenderContext::flip_with_payload()`

**Files:**
- Modify: `src/core/context.rs`

**Step 1: Write failing compile test**

In the `context.rs` `#[cfg(test)]` block:

```rust
#[test]
fn flip_with_payload_exists_on_render_context() {
    // Type-check only — can't call without GPU
    let _ = RenderContext::flip_with_payload::<u32>;
}
```

**Step 2: Run to confirm it fails**

```bash
cargo test flip_with_payload_exists 2>&1 | head -10
```

**Step 3: Implement `flip_with_payload()` on `RenderContext`**

Add after the existing `flip()` method in `impl<'a> RenderContext<'a>`:

```rust
/// Submit the current frame to the GPU without blocking, attaching a typed payload.
///
/// Only valid inside [`VSEContext::run_buffered`]. The `payload` is stored in VSE's
/// pending-confirmation queue and delivered alongside the confirmed [`FlipInfo`] in the
/// next [`FlipEvent::Presented`] for this frame.
///
/// After this call returns the GPU is working on frame `N` while the CPU is free to
/// start computing frame `N+1`.
///
/// # Errors
///
/// - [`VSEError::NotInBufferedMode`] if called from `run()` instead of `run_buffered()`.
/// - [`VSEError::Swapchain`] if image acquisition or submission fails.
pub fn flip_with_payload<T: std::any::Any + Send + 'static>(
    &mut self,
    target_time: Option<Timestamp>,
    payload: T,
) -> Result<(), VSEError> {
    if !self.state.in_buffered_mode {
        return Err(VSEError::NotInBufferedMode);
    }

    if self.state.minimized {
        // Store payload so run_buffered can pop it normally and fire Presented::skipped
        self.state.buffered_pending_payload = Some(Box::new(payload));
        self.state.frame_number += 1;
        return Ok(());
    }

    // Recreate swapchain if needed
    let (dsw, dsh) = self.state.display_size;
    let win_size_arr = [dsw, dsh];
    if self.state.swapchain.needs_recreation() {
        self.state.swapchain.recreate_from_surface(win_size_arr)?;
    }

    // Acquire next image (blocks if all swapchain images are in flight — natural backpressure)
    let (image_index, _suboptimal, acquire_future) =
        match self.state.swapchain.acquire_next_image() {
            Ok(r) => r,
            Err(SwapchainError::OutOfDate) => {
                self.state.swapchain.recreate_from_surface(win_size_arr)?;
                self.state.buffered_pending_payload = Some(Box::new(payload));
                self.state.frame_number += 1;
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };

    let image = self.state.swapchain.images()[image_index as usize].clone();
    let extent = self.state.swapchain.extent();

    let command_buffer = self
        .state
        .renderer
        .render(image, self.config.clear_color, extent)?;

    let future = acquire_future
        .then_execute(self.state.queue.clone(), command_buffer)
        .map_err(|e| FrameError::ExecutionFailed(e.to_string()))?;

    // Optional CPU spin-wait for scheduled present time
    if let Some(target) = target_time {
        self.state
            .timing_provider
            .wait_for_target(target, &self.state.clock);
    }

    let submit_time = self.state.clock.now();

    // Non-blocking submit — returns immediately, keeps fence alive
    let in_flight = self
        .state
        .swapchain
        .submit_nonblocking(self.state.queue.clone(), image_index, future)?;

    // Estimated present time (replaced by confirmed in Presented event)
    let estimated_present = self.state.clock.now();

    let estimated_flip = FlipInfo {
        frame_number: self.state.frame_number,
        timing_source: self.state.timing_provider.source(),
        submit_time,
        present_time: estimated_present,
        missed: false,       // unknown until confirmed
        missed_count: 0,     // unknown until confirmed
        skipped: false,
    };

    // Store payload for run_buffered() to pick up after callback returns
    self.state.buffered_pending_payload = Some(Box::new(payload));

    // Store estimated flip alongside the in-flight fence.
    // run_buffered() correlates these by push order (FIFO).
    self.state
        .buffered_in_flight
        .push_back((estimated_flip, in_flight));

    self.state.frame_number += 1;
    self.state.input.clear_events();

    Ok(())
}
```

**Note:** `buffered_in_flight` is a new field on `VSEState` added in this task. Add it now:

In `VSEState` struct (after `in_buffered_mode`):

```rust
/// In-flight fences paired with estimated FlipInfo. Populated by flip_with_payload(),
/// drained by run_buffered() when GPU confirmation arrives.
/// VecDeque because we always drain from the front (FIFO confirmation order).
buffered_in_flight: std::collections::VecDeque<(FlipInfo, Box<dyn crate::core::buffered::InFlightFuture>)>,
```

Initialize in VSEState construction:

```rust
buffered_in_flight: std::collections::VecDeque::new(),
```

**Step 4: Compile check**

```bash
cargo check
```

**Step 5: Commit**

```bash
git add src/core/context.rs
git commit -m "feat: implement flip_with_payload() on RenderContext"
```

---

## Task 6: `VSEContext::run_buffered()` event loop

**Files:**
- Modify: `src/core/context.rs`

This is the core of the feature. Add `run_buffered()` to `impl VSEContext`.

**Step 1: Add `#[ignore]` integration test skeleton**

Create `tests/buffered_flips.rs`:

```rust
//! Integration tests for run_buffered(). Require a display — marked #[ignore].

use vision_stimulus_engine::prelude::*;

/// Smoke test: run_buffered fires Render events and terminates cleanly.
#[test]
#[ignore = "requires display"]
fn run_buffered_fires_render_events() {
    let context = VSEContext::builder()
        .with_window_size(100, 100)
        .build()
        .expect("context build");

    let mut render_count = 0u32;
    let mut presented_count = 0u32;

    context.run_buffered::<u32, _>(BufferedConfig::default(), |event, vse| {
        match event {
            FlipEvent::Render => {
                render_count += 1;
                vse.clear()?;
                vse.flip_with_payload(None, render_count)?;
                if render_count >= 5 {
                    vse.close();
                }
            }
            FlipEvent::Presented { flip_info, payload } => {
                presented_count += 1;
                // payload is the render_count at the time of that Render
                assert!(payload >= 1 && payload <= 5);
                let _ = flip_info;
            }
            _ => {}
        }
        Ok(())
    }).expect("run_buffered");

    assert_eq!(render_count, 5);
    // With depth=1, first frame has no Presented; remaining 4 do
    assert_eq!(presented_count, 4);
}

/// Payload arrives in the correct order (FIFO).
#[test]
#[ignore = "requires display"]
fn run_buffered_payload_fifo_order() {
    let context = VSEContext::builder()
        .with_window_size(100, 100)
        .build()
        .expect("context build");

    let mut render_seq: Vec<u32> = Vec::new();
    let mut present_seq: Vec<u32> = Vec::new();
    let mut frame = 0u32;

    context.run_buffered::<u32, _>(BufferedConfig::default(), |event, vse| {
        match event {
            FlipEvent::Render => {
                frame += 1;
                render_seq.push(frame);
                vse.clear()?;
                vse.flip_with_payload(None, frame)?;
                if frame >= 10 { vse.close(); }
            }
            FlipEvent::Presented { payload, .. } => {
                present_seq.push(payload);
            }
            _ => {}
        }
        Ok(())
    }).expect("run_buffered");

    // Payloads must arrive in submission order
    for i in 1..present_seq.len() {
        assert!(present_seq[i] > present_seq[i - 1], "out of order: {:?}", present_seq);
    }
}
```

**Step 2: Run to confirm they are skipped (not failing)**

```bash
cargo test --test buffered_flips 2>&1 | head -10
```
Expected: tests are ignored (not run).

**Step 3: Implement `run_buffered()` on `VSEContext`**

Find `impl VSEContext` in `context.rs` and add (near `run()`):

```rust
/// Run the experiment loop in buffered (pipelined) mode.
///
/// Unlike [`run()`], which blocks on every GPU fence, `run_buffered` pipelines CPU
/// and GPU work across frames. The callback receives two alternating event variants:
///
/// - [`FlipEvent::Render`]: build and submit frame `N` via
///   [`flip_with_payload()`](RenderContext::flip_with_payload). This fires every vblank.
/// - [`FlipEvent::Presented`]: GPU has confirmed frame `N - depth` was scanned out.
///   `flip_info.present_time` is a confirmed timestamp. Call `record_frame(payload)?`
///   here to write data with accurate timing.
///
/// During the first `config.depth` iterations, only `Render` fires (queue is warming up).
/// On clean exit, all pending `Presented` events are drained before returning.
///
/// # Closed-loop experiments
///
/// The B-frame latency is explicit and predictable: when `Presented` fires for frame `N`,
/// frame `N+1` has already been submitted. You can update stimulus state in `Presented`
/// and it will take effect from frame `N+2` onward.
///
/// # Errors
///
/// Propagates any `VSEError` returned by the callback.
pub fn run_buffered<T, F>(
    mut self,
    config: BufferedConfig,
    mut callback: F,
) -> Result<(), VSEError>
where
    T: std::any::Any + serde::Serialize + Send + 'static,
    F: FnMut(FlipEvent<T>, &mut RenderContext<'_>) -> Result<(), VSEError>,
{
    use std::collections::VecDeque;
    use crate::core::buffered::PendingFrame;

    // pending_frames holds (frame_number, T) pairs for frames submitted but not yet
    // confirmed. Lives here (not in VSEState) because it is generic over T.
    let mut pending_frames: VecDeque<PendingFrame<T>> = VecDeque::with_capacity(config.depth + 1);

    // Build VSEState (same as run() — mirrors existing initialization pattern)
    let (mut state, mut vse_config) = self.build_state()?;

    // Ensure swapchain has enough images for the requested pipeline depth
    let required_images = (config.depth + 1) as u32;
    state.swapchain.ensure_image_count(required_images)?;

    state.in_buffered_mode = true;

    'outer: loop {
        // ── Phase 1: Check for confirmed presentation ──────────────────────────────
        //
        // Poll the oldest in-flight fence. When confirmed:
        //   - pop pending_frame (payload + estimated flip)
        //   - build confirmed FlipInfo (from GoogleDisplayTiming or fence signal time)
        //   - fire FlipEvent::Presented
        //
        // During startup (pending_frames is empty) this phase is skipped.

        if let Some(oldest) = state.buffered_in_flight.front() {
            if oldest.0.is_complete_or_wait(&state, &config) {
                // Pop the confirmed in-flight fence + estimated flip
                let (estimated_flip, fence) = state.buffered_in_flight.pop_front().unwrap();
                fence.wait_blocking(); // ensure complete (usually instant after is_complete)

                // Pop the matching payload (same FIFO order)
                if let Some(pf) = pending_frames.pop_front() {
                    debug_assert_eq!(pf.frame_number, estimated_flip.frame_number);

                    // Build confirmed FlipInfo
                    let confirmed_flip = state.build_confirmed_flip(estimated_flip);

                    // Set for record_frame() to read inside the Presented callback
                    state.buffered_confirmed_flip = Some(confirmed_flip.clone());

                    let auto_timing_only = {
                        let mut render_ctx = RenderContext { state: &mut state, config: &mut vse_config };
                        let had_record_call = std::cell::Cell::new(false);
                        // We need to detect if user calls record_frame() — see note below.
                        // For now, track via a flag set in record_frame_buffered().
                        let result = callback(
                            FlipEvent::Presented { flip_info: confirmed_flip.clone(), payload: pf.payload },
                            &mut render_ctx,
                        );
                        result?;
                        // Check if user called record_frame()
                        render_ctx.state.buffered_record_called_this_presented
                    };

                    state.buffered_confirmed_flip = None;
                    state.buffered_record_called_this_presented = false;

                    // Auto timing-only row if user skipped record_frame()
                    if !auto_timing_only {
                        if let Some(recording) = &mut state.recording {
                            let _ = recording.session.send_frame(crate::data::messages::FrameMessage {
                                flip: confirmed_flip,
                                payload: None,
                                schema_name: "",
                            });
                        }
                    }
                }
            }
        }

        // ── Phase 2: Handle window events (input, resize, close) ─────────────────
        //
        // Poll winit events without blocking (mirrors the non-blocking path in run())
        state.poll_window_events();
        if state.should_close {
            break 'outer;
        }

        // ── Phase 3: Fire Render event ────────────────────────────────────────────
        {
            let mut render_ctx = RenderContext { state: &mut state, config: &mut vse_config };
            callback(FlipEvent::Render, &mut render_ctx)?;

            // After Render callback: take payload stored by flip_with_payload()
            if let Some(raw_payload) = render_ctx.state.buffered_pending_payload.take() {
                let payload: T = *raw_payload
                    .downcast::<T>()
                    .expect("buffered payload type mismatch — internal VSE bug");

                // The latest entry in buffered_in_flight has the estimated_flip for this frame
                if let Some((estimated_flip, _)) = render_ctx.state.buffered_in_flight.back() {
                    pending_frames.push_back(PendingFrame {
                        frame_number: estimated_flip.frame_number,
                        payload,
                        estimated_flip: estimated_flip.clone(),
                    });
                }
            }
        }

        if state.should_close {
            break 'outer;
        }
    }

    // ── Shutdown: drain remaining pending frames ──────────────────────────────────
    state.in_buffered_mode = false;
    while let Some((estimated_flip, fence)) = state.buffered_in_flight.pop_front() {
        fence.wait_blocking();
        let confirmed_flip = state.build_confirmed_flip(estimated_flip);

        if let Some(pf) = pending_frames.pop_front() {
            state.buffered_confirmed_flip = Some(confirmed_flip.clone());
            let mut render_ctx = RenderContext { state: &mut state, config: &mut vse_config };
            let _ = callback(
                FlipEvent::Presented { flip_info: confirmed_flip.clone(), payload: pf.payload },
                &mut render_ctx,
            );
            let called = render_ctx.state.buffered_record_called_this_presented;
            state.buffered_confirmed_flip = None;
            state.buffered_record_called_this_presented = false;

            if !called {
                if let Some(recording) = &mut state.recording {
                    let _ = recording.session.send_frame(crate::data::messages::FrameMessage {
                        flip: confirmed_flip,
                        payload: None,
                        schema_name: "",
                    });
                }
            }
        }
    }

    Ok(())
}
```

**Additional helpers needed on `VSEState`** (add these as methods on `VSEState`):

```rust
impl VSEState {
    /// Build confirmed FlipInfo from an estimated one.
    /// For GoogleDisplayTiming: queries driver for actual scanout time.
    /// For CPU: uses fence signal time (already in estimated_flip.present_time).
    fn build_confirmed_flip(&self, estimated: FlipInfo) -> FlipInfo {
        let confirmed_present = self.timing_provider.record_present_time(&self.clock);
        let prev_time = self.last_present_time;
        let duration = prev_time.map(|p| confirmed_present.duration_since(p));
        let expected = self.expected_frame_duration;
        let missed = duration
            .zip(expected)
            .map(|(d, e)| d > e.mul_f32(1.5))
            .unwrap_or(false);
        let missed_count = duration
            .zip(expected)
            .map(|(d, e)| (d.as_secs_f64() / e.as_secs_f64()).round() as u32 - 1)
            .unwrap_or(0);
        FlipInfo {
            frame_number: estimated.frame_number,
            timing_source: self.timing_provider.source(),
            submit_time: estimated.submit_time,
            present_time: confirmed_present,
            missed,
            missed_count,
            skipped: false,
        }
    }

    /// Minimal winit event pump for the buffered loop (non-blocking).
    fn poll_window_events(&mut self) {
        // Mirror the non-blocking input/resize handling from run().
        // Specifically: handle WindowEvent::CloseRequested, Resized, KeyboardInput, etc.
        // This can delegate to the same helpers used in run().
        // Implementation: extract the inner event-handling logic from run() into a shared
        // private method `handle_window_event()` that both run() and poll_window_events() call.
    }
}
```

**Note on `poll_window_events`:** The current `run()` uses `event_loop.run()` which owns the
loop. For `run_buffered()`, you need a non-blocking event pump. In winit 0.29, use
`event_loop.run_return()` (from `EventLoopExtRunReturn`) with a `ControlFlow::Poll` to drain
events without blocking, or restructure using the same pattern as the existing synchronous
run. Research the winit 0.29 API for event polling without yielding control.

Also add to `VSEState`:

```rust
/// Tracks whether record_frame() was called during the current Presented callback.
/// Reset to false before each Presented callback by run_buffered().
buffered_record_called_this_presented: bool,
```

**Step 4: Compile check**

```bash
cargo check
```
Fix any type errors. This task has the most integration surface.

**Step 5: Run integration tests (expect ignore)**

```bash
cargo test --test buffered_flips
```
Expected: tests marked ignored, 0 failures.

**Step 6: Commit**

```bash
git add src/core/context.rs tests/buffered_flips.rs
git commit -m "feat: implement VSEContext::run_buffered() event loop"
```

---

## Task 7: `record_frame()` update for buffered confirmed flip

**Files:**
- Modify: `src/core/context.rs` (existing `record_frame()` on `RenderContext`)

Find `record_frame()` in `impl<'a> RenderContext<'a>`. It currently reads `recording.pending_flip`.

**Step 1: Write failing test**

Add to `tests/buffered_flips.rs`:

```rust
/// record_frame() in Presented gets confirmed FlipInfo, not estimated.
#[test]
#[ignore = "requires display"]
fn record_frame_in_presented_uses_confirmed_flip() {
    use std::sync::{Arc, Mutex};

    #[derive(serde::Serialize)]
    struct FrameData { val: u32 }

    struct CaptureWriter(Arc<Mutex<Vec<vision_stimulus_engine::timing::FlipInfo>>>);
    impl vision_stimulus_engine::data::DataWriter for CaptureWriter {
        fn write_frame(&mut self, msg: vision_stimulus_engine::data::messages::FrameMessage)
            -> Result<(), vision_stimulus_engine::data::DataError>
        {
            self.0.lock().unwrap().push(msg.flip.clone());
            Ok(())
        }
        fn write_annotation(&mut self, _: vision_stimulus_engine::data::messages::AnnotationMessage)
            -> Result<(), vision_stimulus_engine::data::DataError> { Ok(()) }
        fn write_event(&mut self, _: vision_stimulus_engine::data::messages::EventMessage)
            -> Result<(), vision_stimulus_engine::data::DataError> { Ok(()) }
        fn flush(&mut self) -> Result<(), vision_stimulus_engine::data::DataError> { Ok(()) }
    }

    let captured = Arc::new(Mutex::new(Vec::new()));
    let session = ExperimentSession::builder()
        .with_writer(CaptureWriter(captured.clone()))
        .build().unwrap();

    let context = VSEContext::builder()
        .with_window_size(100, 100)
        .with_session(session)
        .build().unwrap();

    let mut frame = 0u32;
    context.run_buffered::<FrameData, _>(BufferedConfig::default(), |event, vse| {
        match event {
            FlipEvent::Render => {
                frame += 1;
                vse.clear()?;
                vse.flip_with_payload(None, FrameData { val: frame })?;
                if frame >= 3 { vse.close(); }
            }
            FlipEvent::Presented { flip_info, payload } => {
                assert!(!flip_info.skipped);
                vse.record_frame(payload)?;
            }
            _ => {}
        }
        Ok(())
    }).unwrap();

    let flips = captured.lock().unwrap();
    assert!(!flips.is_empty(), "no frames recorded");
}
```

**Step 2: Modify `record_frame()` to support buffered context**

In `record_frame()` on `RenderContext`, add a check for `buffered_confirmed_flip` before
the existing `pending_flip` logic:

```rust
pub fn record_frame<F: serde::Serialize>(&mut self, data: F) -> Result<(), VSEError> {
    let recording = self.state.recording.as_mut().ok_or(VSEError::NoSession)?;

    // Buffered mode: use the confirmed flip set by run_buffered() before this callback.
    if self.state.in_buffered_mode {
        let flip = self.state.buffered_confirmed_flip
            .clone()
            .ok_or(VSEError::NoConfirmedFlip)?;

        let payload = serde_json::to_vec(&data)
            .map_err(|e| VSEError::DataRecording(e.to_string()))?;

        recording.session.send_frame(crate::data::messages::FrameMessage {
            flip,
            payload: Some(payload),
            schema_name: std::any::type_name::<F>(),
        }).map_err(|e| VSEError::DataRecording(e.to_string()))?;

        // Signal to run_buffered() that the user called record_frame() this Presented
        self.state.buffered_record_called_this_presented = true;
        return Ok(());
    }

    // Synchronous mode: existing logic unchanged below
    // ... (leave existing code here)
}
```

**Step 3: Compile check and run tests**

```bash
cargo check && cargo test
```

**Step 4: Commit**

```bash
git add src/core/context.rs tests/buffered_flips.rs
git commit -m "feat: update record_frame() to use confirmed flip in buffered mode"
```

---

## Task 8: `vkGetPastPresentationTimingGOOGLE` confirmation in buffered loop

**Files:**
- Modify: `src/timing/provider.rs`
- Modify: `src/core/context.rs` (`build_confirmed_flip` on `VSEState`)

**Context:** `TimingProvider::record_present_time()` already queries
`vkGetPastPresentationTimingGOOGLE` in `GoogleDisplayTimingProvider`. For buffered mode,
we need to query it for a specific `frame_number` rather than "most recent", since multiple
frames may be in flight.

**Step 1: Extend `TimingProvider` trait**

In `src/timing/provider.rs`, add an optional method:

```rust
pub trait TimingProvider {
    // ... existing methods ...

    /// Query confirmed scanout time for a specific frame number.
    /// Returns `None` if not available (e.g., CPU provider, or driver hasn't confirmed yet).
    /// Default implementation returns None (CPU path falls back to fence signal time).
    fn confirmed_present_time_for(
        &self,
        _frame_number: u64,
        _clock: &Clock,
    ) -> Option<Timestamp> {
        None
    }
}
```

**Step 2: Implement in `GoogleDisplayTimingProvider`**

In the `GoogleDisplayTimingProvider` impl block, override the new method to query
`vkGetPastPresentationTimingGOOGLE` for the given `present_id` (mapped from `frame_number`).
The provider already tracks `present_id` — extend this mapping to look up by frame.

**Step 3: Use in `build_confirmed_flip`**

In `VSEState::build_confirmed_flip()`:

```rust
fn build_confirmed_flip(&self, estimated: FlipInfo) -> FlipInfo {
    // Prefer hardware-confirmed scanout time when available
    let confirmed_present = self
        .timing_provider
        .confirmed_present_time_for(estimated.frame_number, &self.clock)
        .unwrap_or_else(|| self.clock.now()); // fence signal time (CPU path)

    // ... rest of missed-frame calculation as written in Task 6 ...
}
```

**Step 4: Compile check**

```bash
cargo check && cargo test
```

**Step 5: Commit**

```bash
git add src/timing/provider.rs src/core/context.rs
git commit -m "feat: integrate vkGetPastPresentationTimingGOOGLE into buffered confirmation"
```

---

## Task 9: Prelude exports

**Files:**
- Modify: `src/core/mod.rs`
- Modify: `src/lib.rs`

**Step 1: Add to `src/core/mod.rs`**

The `pub use buffered::{BufferedConfig, FlipEvent};` line was added in Task 1. Verify it is
present and add `Key` and `PhysicalKey` re-exports if they aren't already in the prelude.

**Step 2: Add to `src/lib.rs` prelude**

```rust
pub use crate::core::{
    // ... existing exports ...
    BufferedConfig, FlipEvent,
};
```

**Step 3: Verify**

```rust
// In a doc test or test file:
use vision_stimulus_engine::prelude::*;
let _ = BufferedConfig::default();
```

```bash
cargo check && cargo test
```

**Step 4: Remove temporary compile tests from `src/core/mod.rs`**

The `buffered_compile_test` module added in Task 1 should be removed or moved to an
appropriate `#[cfg(test)]` block in `buffered.rs`.

**Step 5: Commit**

```bash
git add src/core/mod.rs src/lib.rs
git commit -m "feat: export BufferedConfig and FlipEvent from prelude"
```

---

## Task 10: Rustdoc on all new public items

**Files:**
- Modify: `src/core/buffered.rs`
- Modify: `src/core/context.rs` (doc on `run_buffered`, `flip_with_payload`, new `VSEError` variants)

**Step 1: Check existing doc coverage**

```bash
cargo doc --no-deps 2>&1 | grep "warning: missing documentation"
```

**Step 2: Add doc comments**

Ensure every public item has:
- One-line summary
- Paragraph explaining purpose and when to use it
- `# Errors` section for fallible methods
- `# Example` with `no_run` code

Pay special attention to:

**`FlipEvent`**: Explain the two-phase model, when each variant fires, that `Render` fires
every vblank and `Presented` fires one `depth` iterations later. Include a complete example
showing the full `run_buffered` callback with both arms.

**`BufferedConfig::depth`**: Include a table mapping `depth` to swapchain image count and
recommend `depth = 1` for most experiments.

**`flip_with_payload()`**: Explain that the payload is delivered in the next `Presented`
event, and that calling this multiple times in one `Render` arm is an error.

**`run_buffered()`**: Full worked example showing stimulus state shared between arms,
`record_frame` in `Presented`, closed-loop adjustment on `flip_info.missed`.

**Step 3: Verify docs build cleanly**

```bash
cargo doc --no-deps 2>&1 | grep -c "warning"
```
Expected: 0 warnings.

**Step 4: Commit**

```bash
git add src/core/buffered.rs src/core/context.rs
git commit -m "docs: add rustdoc to all buffered flip public API"
```

---

## Task 11: `docs/guides/buffered_flips.md`

**Files:**
- Create: `docs/guides/buffered_flips.md`

Write the full prose guide. Cover every section listed in the design doc:

1. **Why buffered flips**: CPU-GPU pipelining, confirmed scanout timestamps, scheduled
   presentation. One paragraph each.

2. **The two-phase mental model**: ASCII diagram showing the frame timeline at depth=1.
   ```
   Frame N:   [Render] → submit → GPU processing
   Frame N+1: [Presented N] → react+record | [Render N+1] → submit
   Frame N+2: [Presented N+1] → react+record | [Render N+2] → submit
   ```

3. **Choosing buffer depth**: Table with depth, swapchain images, latency at 60/120/240 Hz,
   recommended use case.

4. **The closed-loop latency contract**: Exactly B frames between submit and confirmation.
   Explain why this is predictable (no hidden estimation) and how to reason about it.

5. **State management between arms**: What to put in the payload (stimulus params that
   need to be recorded) vs. what to keep in external `mut` state (persistent experiment
   state that both arms access). Example of each.

6. **Full annotated example**: A closed-loop contrast-adjustment experiment (~50 lines).

7. **Migration guide**: Side-by-side synchronous `run()` vs. `run_buffered()` for the
   same experiment. Highlight which lines move and why.

8. **Shutdown and panic behavior**: Clean shutdown drains pending frames; panic loses
   in-flight frames (same guarantee as synchronous mode).

**Step: Verify links are valid**

```bash
cargo doc --no-deps
```
Check any `[`links`]` in the guide point to real types.

**Step: Commit**

```bash
git add docs/guides/buffered_flips.md
git commit -m "docs: add buffered flips guide"
```

---

## Task 12: Update existing docs

**Files:**
- Modify: `docs/guides/data_recording.md` (line 160, "Future: Buffered Flip" section)
- Modify: `docs/guides/experiment_data_schema.md`

**Step 1: Update `data_recording.md`**

Replace the "Future: Buffered Flip" section with:

```markdown
## Buffered Flip

For pipelined GPU experiments, use `run_buffered()` instead of `run()`. In buffered mode,
`record_frame()` is called in the `FlipEvent::Presented` arm with **confirmed** hardware
timing — `present_time` is never an estimate. See the
[buffered flips guide](buffered_flips.md) for full details.
```

**Step 2: Update `experiment_data_schema.md`**

Add a note under the `present_time_us` column description:

```markdown
In `run_buffered()`, `present_time_us` is always a confirmed hardware scanout timestamp
(`GoogleDisplayTiming`) or fence-signal time (`CpuEstimate`). It is never an estimate.
In synchronous `run()`, it is also confirmed (derived from the blocking fence wait).
```

**Step 3: Commit**

```bash
git add docs/guides/data_recording.md docs/guides/experiment_data_schema.md
git commit -m "docs: update data recording and schema docs for buffered flip"
```

---

## Final Verification

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -W clippy::all
cargo doc --no-deps
```

All commands must complete with zero errors and zero warnings.
