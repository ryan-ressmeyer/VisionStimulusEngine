# Buffered Flips — Design Document

**Date:** 2026-03-02
**Status:** Approved
**Scope:** Add an explicit buffered flip variant (`run_buffered`) that pipelines CPU and GPU
work across frames, enables confirmed hardware scanout timestamps in data recording, and
supports closed-loop experiments with predictable, bounded timing latency.

---

## Problem Statement

The current `flip()` is synchronous and blocking: the CPU submits a frame to the GPU then
waits for the GPU fence to signal before proceeding. This means CPU and GPU are never
running in parallel:

```
CPU: [render N] [wait fence N] [render N+1] [wait fence N+1] ...
GPU:            [process N]                 [process N+1] ...
```

This wastes GPU pipeline capacity, limits the CPU time available to compute stimulus
parameters, and prevents scheduling presents to exact vblank targets via
`VkPresentTimesInfoGOOGLE`. All three matter for high-performance vision science
experiments.

---

## Goals

- Pipeline CPU and GPU work so both run in parallel across frames.
- Deliver confirmed, driver-reported hardware scanout timestamps to `record_frame()` calls.
- Support closed-loop experiments with explicit, predictable B-frame latency.
- Keep the synchronous `run()` API completely unchanged with zero overhead.
- Provide a VSE-managed typed payload queue so users never write ring-buffer boilerplate.
- Thorough documentation: rustdoc on all new public types and methods, a prose guide, and
  updated schema reference.

---

## Non-Goals

- Transparent buffering behind the existing `run()` / `flip()` API (rejected: hides latency
  from closed-loop users and allows estimated rather than confirmed timing in records).
- Buffer depth > 3 (triple-buffering covers all practical use cases; deeper queues increase
  memory and input latency with no throughput benefit at typical refresh rates).
- `VK_EXT_present_timing` integration (deferred; architecture is forward-compatible).

---

## Core Abstractions

### `BufferedConfig`

```rust
pub struct BufferedConfig {
    /// Number of frames to pipeline. Recommended: 1 (double-buffer) or 2 (triple-buffer).
    /// The swapchain image count is automatically set to `depth + 1`.
    pub depth: usize,
    /// Backpressure behavior when the pending-confirmation queue is full.
    /// Reuses the existing `OverflowBehavior` type.
    pub overflow: OverflowBehavior,
}

impl Default for BufferedConfig {
    fn default() -> Self {
        Self { depth: 1, overflow: OverflowBehavior::Block }
    }
}
```

### `FlipEvent<T>`

```rust
#[non_exhaustive]
pub enum FlipEvent<T> {
    /// Fired once per vblank to build and submit the next frame.
    /// Call `flip_with_payload()` before returning.
    Render,

    /// Fired when the GPU confirms frame N-depth was scanned out.
    /// `flip_info.present_time` is a confirmed hardware timestamp.
    /// `payload` is the value passed to `flip_with_payload()` at render time.
    /// Call `record_frame(payload)` here to record with confirmed timing.
    Presented {
        flip_info: FlipInfo,
        payload: T,
    },
}
```

`#[non_exhaustive]` allows future variants (`Skipped`, `Dropped`) without breaking existing
match arms. Users should include a `_ => Ok(())` catch-all.

### `VSEContext::run_buffered()`

```rust
impl VSEContext {
    /// Run the experiment loop in buffered (pipelined) mode.
    ///
    /// `T` is the per-frame payload type carried from `Render` to `Presented`.
    /// It must implement `serde::Serialize` (for data recording) and `Send + 'static`
    /// (for channel delivery). The type is inferred from the `flip_with_payload` call.
    pub fn run_buffered<T, F>(
        &mut self,
        config: BufferedConfig,
        callback: F,
    ) -> Result<(), VSEError>
    where
        T: serde::Serialize + Send + 'static,
        F: FnMut(FlipEvent<T>, &mut RenderContext<'_>) -> Result<(), VSEError>;
}
```

### `RenderContext::flip_with_payload()`

```rust
impl RenderContext<'_> {
    /// Submit the current frame to the GPU without blocking.
    ///
    /// `payload` is stored in the pending-confirmation queue and delivered alongside
    /// the confirmed `FlipInfo` in the next `FlipEvent::Presented` for this frame.
    /// Only valid inside `run_buffered()`. Returns `VSEError::NotInBufferedMode`
    /// if called from `run()`.
    pub fn flip_with_payload<T: serde::Serialize + Send + 'static>(
        &mut self,
        target_time: Option<Timestamp>,
        payload: T,
    ) -> Result<(), VSEError>;
}
```

---

## Typical Experiment Loop

```rust
#[derive(serde::Serialize)]
struct MyFrameData {
    stimulus_id: u32,
    contrast: f32,
    orientation_deg: f32,
}

let mut state = StimulusState::default();

context.run_buffered::<MyFrameData, _>(BufferedConfig::default(), |event, vse| {
    match event {
        FlipEvent::Render => {
            vse.clear()?;
            draw_stimulus(&state, vse)?;
            vse.flip_with_payload(None, MyFrameData {
                stimulus_id: state.current_id,
                contrast: state.contrast,
                orientation_deg: state.orientation,
            })?;
        }
        FlipEvent::Presented { flip_info, payload } => {
            // flip_info.present_time is a confirmed hardware scanout timestamp.
            // Use it for closed-loop stimulus adjustments.
            if flip_info.missed {
                state.adjust_for_missed_frame();
            }
            // record_frame() implicitly uses flip_info — no extra argument needed.
            vse.record_frame(payload)?;
        }
        _ => {}
    }
    Ok(())
})?;
```

---

## Internal Mechanics

### Payload ring buffer

`VSEState` gains a `VecDeque<PendingFrame<T>>` scoped to the lifetime of `run_buffered()`:

```rust
struct PendingFrame<T> {
    frame_number: u64,
    payload: T,
    estimated_flip: FlipInfo,  // available immediately; replaced by confirmed on delivery
}
```

When `flip_with_payload(payload)` is called in `Render`, VSE pushes a `PendingFrame` onto
the back. When a GPU confirmation arrives, VSE pops from the front (FIFO) and fires
`FlipEvent::Presented`. At `depth=1` this queue holds at most one frame; at `depth=2`, two.

### Non-blocking present and in-flight fence tracking

`swapchain.present()` today calls `future.wait(None)` synchronously. In buffered mode, the
fence signal future is stored rather than waited on:

```
flip_with_payload()  →  submit + signal fence
                         store FenceSignalFuture in in_flight_fences: VecDeque
                         return immediately

top of next loop iteration  →  poll oldest fence
                                if signaled: pop payload, build FlipInfo, fire Presented
                                if not yet:  Block (wait) or DropWithWarning per config
```

`FenceSignalFuture` must remain alive until explicitly waited on — dropping it triggers an
implicit wait. `in_flight_fences: VecDeque<FenceSignalFuture<...>>` in `VSEState` keeps
them alive in parallel with `pending_frames`.

### Swapchain image count

Buffer depth determines the required swapchain image count. `run_buffered()` recreates the
swapchain at entry if the current count is insufficient:

| `depth` | Swapchain images | Mode |
|---------|-----------------|------|
| 1 | 2 | Double-buffering |
| 2 | 3 | Triple-buffering |

### Confirmation via `vkGetPastPresentationTimingGOOGLE`

At the top of each loop iteration, before dispatching any event, VSE queries the driver for
past presentation timings. Confirmed `present_id` values are matched against
`pending_frames` by `frame_number`. When a match is found, the estimated `FlipInfo` is
replaced with a confirmed one before `FlipEvent::Presented` fires.

When `GoogleDisplayTiming` is unavailable (CPU timing path), the fence signal time is used
as `present_time` — identical accuracy to the synchronous path today, delivered one
iteration late.

### Event sequencing

```
Startup (first depth iterations):
  Render fires, Presented does not (queue filling up)

Steady state (every subsequent iteration):
  1. Query vkGetPastPresentationTimingGOOGLE
  2. Fire FlipEvent::Presented { flip_info (confirmed), payload } for frame N-depth
  3. Fire FlipEvent::Render for frame N

Shutdown (run_buffered() returning):
  Drain pending_frames: wait on each remaining fence in order,
  fire outstanding Presented events (confirmed timing if available, estimated otherwise),
  then flush ExperimentSession writer channel.
```

---

## ExperimentSession Integration

### `record_frame()` in the Presented arm

Before invoking the `Presented` callback, VSE stores the confirmed `FlipInfo` as
`buffered_confirmed_flip: Option<FlipInfo>` on `RenderContext`. `record_frame(payload)`
reads it there and sends a `FrameMessage` with the confirmed timing to the writer thread.
After the callback returns, the field is cleared.

- Calling `record_frame()` in the Presented arm: works, uses confirmed `FlipInfo`.
- Calling `record_frame()` multiple times in the Presented arm: valid — each call writes
  one row with the same confirmed `FlipInfo`.
- Calling `record_frame()` in the Render arm: returns `VSEError::NoConfirmedFlip`.

### Automatic timing-only rows

If `record_frame()` is not called in a `Presented` callback, VSE automatically enqueues a
timing-only `FrameMessage` with the confirmed `FlipInfo`. The complete timing history is
always present in the output file regardless of whether the user records per-frame data.

### `record_annotation()` and `record_event()`

Unchanged. Both use `Clock::now()` for their timestamp and are valid in either arm.

---

## API Compatibility

| Method | `run()` | `run_buffered()` |
|--------|---------|-----------------|
| `flip()` | works (synchronous, blocking) | `VSEError::NotSupportedInBufferedMode` |
| `flip_with_payload()` | `VSEError::NotInBufferedMode` | works (Render arm only) |
| `record_frame()` | works (after flip) | works (Presented arm only) |
| `record_annotation()` | works anywhere | works anywhere |
| `record_event()` | works anywhere | works anywhere |
| No session attached | record calls are no-ops | record calls are no-ops |

`run()` carries zero overhead from the buffered infrastructure — it never touches
`pending_frames`, `in_flight_fences`, or `buffered_confirmed_flip`.

---

## Documentation Deliverables

Documentation is a first-class deliverable shipped alongside the implementation.

### rustdoc

Every new public type, method, and enum variant gets a doc comment covering: purpose, usage
notes, error conditions, and a working code example. Special attention to:

- `FlipEvent`: what each variant means, when each fires, what is and isn't valid to call
  in each arm, and the `#[non_exhaustive]` catch-all requirement.
- `BufferedConfig`: how `depth` maps to swapchain image count, the latency vs. throughput
  tradeoff, and guidance on choosing `depth=1` vs `depth=2`.
- `flip_with_payload()`: the payload lifetime, what happens if called outside
  `run_buffered()`, and the relationship between payload and the `Presented` event.
- `record_frame()` in buffered context: why it must be called in the `Presented` arm,
  what `NoConfirmedFlip` means, and the automatic timing-only row fallback.
- `run_buffered()`: the full event sequencing (startup, steady state, shutdown), the
  closed-loop latency model (B frames), and when to prefer it over `run()`.

### `docs/guides/buffered_flips.md`

Prose walkthrough covering:

- Why buffered flips exist: CPU-GPU pipelining, confirmed scanout timestamps, scheduled
  presentation via `VkPresentTimesInfoGOOGLE`.
- The two-phase mental model: Render builds B frames ahead, Presented reacts to confirmed
  results.
- Choosing buffer depth: latency vs. throughput table, guidance for 60 Hz / 120 Hz / 240 Hz
  displays.
- The closed-loop latency contract: exactly B frames between stimulus submission and
  confirmed timing delivery, no hidden estimation.
- State management between Render and Presented: the typed payload queue, what to put in
  the payload vs. what to keep in external shared state.
- Full annotated example: a closed-loop contrast-adjustment experiment.
- Migration guide: how to port a synchronous `run()` experiment to `run_buffered()` and
  what changes in the process.
- Shutdown and panic behavior: what happens to in-flight frames on clean exit vs. panic.

### Updates to existing docs

- `docs/guides/data_recording.md`: update the "Future: Buffered Flip" section to link to
  the new guide now that the feature is implemented.
- `docs/guides/experiment_data_schema.md`: note that in buffered mode `present_time` is
  always a confirmed hardware timestamp, never an estimate.
