# Buffered Flips

`run_buffered()` is VSE's pipelined rendering mode. It pipelines CPU and GPU work across
frames, delivers **confirmed hardware scanout timestamps** to your data recorder, and provides
a predictable closed-loop latency contract for neural recording experiments.

---

## Why buffered flips?

### CPU–GPU pipelining

In the synchronous `run()` loop, each frame follows this sequence:

```
CPU: build commands → submit → wait for GPU → record data → build commands → …
GPU:                  render → idle          → render → …
```

The GPU idles while the CPU waits for the fence. With `run_buffered()`, the CPU submits frame
`N` non-blocking, so it is free to handle input and prepare state for frame `N+1` while the
GPU is still rendering frame `N`:

```
CPU: build N → submit → build N+1 → submit → build N+2 → …
GPU:           render N →           render N+1 →          …
```

This is standard double-buffering. The driver's swapchain image management provides natural
backpressure — `acquire_next_image()` blocks if all images are in use, keeping the pipeline
at the configured depth.

### Confirmed scanout timestamps

The synchronous `run()` path records `present_time` after the fence signals. The fence signals
when the GPU finishes *executing* the command buffer, not necessarily when the display hardware
*scans out* the frame.

In `run_buffered()`, `FlipEvent::Presented` fires after fence completion and, when
`VK_GOOGLE_display_timing` is available, attempts to retrieve the actual hardware scanout time
via `vkGetPastPresentationTimingGOOGLE`. The resulting `FlipInfo::present_time` is therefore
as close to a ground-truth scanout timestamp as the driver will provide.

### Scheduled presentation

`flip_with_payload(Some(target_time), payload)` spin-waits until `target_time` before
submitting. Paired with `VK_GOOGLE_display_timing` (future: `VkPresentTimesInfoGOOGLE`),
this enables sub-millisecond control over which vblank a stimulus appears on.

---

## The two-phase mental model

At every vblank, `run_buffered()` fires two events in order:

```
vblank N:   [Presented N-1]  ← react to confirmed timing, record data
            [Render N]       ← build stimulus, call flip_with_payload()

vblank N+1: [Presented N]    ← react + record
            [Render N+1]     ← build + submit

vblank N+2: [Presented N+1]  ← react + record
            [Render N+2]     ← build + submit
```

During the first `depth` vblanks (the warm-up period), only `Render` fires — there are no
confirmed frames yet.

On clean exit (when `vse.close()` is called), all pending `Presented` events are drained
synchronously before `run_buffered()` returns.

---

## Choosing buffer depth

| `depth` | Swapchain images | Extra latency at 60 Hz | Extra latency at 120 Hz | Recommended for |
|---------|-----------------|------------------------|-------------------------|-----------------|
| `1`     | 2               | 0 frames               | 0 frames                | Most experiments (**default**) |
| `2`     | 3               | 1 frame (~16 ms)       | 1 frame (~8 ms)         | High GPU utilization / VR |

`depth = 1` is the right choice for closed-loop neural recording. It gives you one frame of
pipelining with the minimum possible confirmed-timing latency.

`depth = 2` may improve frame-rate stability on very GPU-bound workloads (complex shaders,
high-resolution textures) at the cost of one additional frame of closed-loop latency.

Configure depth via [`BufferedConfig`](../../src/core/buffered.rs):

```rust
use vision_stimulus_engine::prelude::*;

let cfg = BufferedConfig { depth: 1, overflow: OverflowBehavior::Block };
```

---

## The closed-loop latency contract

When `FlipEvent::Presented` fires for frame `N`:

- Frame `N` has been confirmed by the GPU (fence signaled).
- Frame `N+1` has **already been submitted** to the GPU.
- Stimulus changes you make in the `Presented` arm take effect from frame `N+2` onward.

The B-frame latency is therefore exactly `depth` frames — one frame with the default
`depth = 1`. This is explicit and predictable. There is no hidden estimation or jitter: you
always know exactly how many vblanks separate a stimulus decision from its display.

**Example at 60 Hz, depth = 1:**

| Time    | Event                          | Latency |
|---------|-------------------------------|---------|
| T=0 ms  | Render(N): decide contrast    | —       |
| T=16 ms | Presented(N-1): observe outcome, update contrast | 16 ms from last decision |
| T=16 ms | Render(N+1): apply new contrast | — |
| T=32 ms | Presented(N): contrast hits display | 32 ms from decision |

---

## State management between arms

### What goes in the payload

The payload `T` carries the **per-frame stimulus parameters you need to record** — the values
that describe exactly what was shown on the display for frame `N`. Put anything that:

- Must be correlated with `flip_info.present_time` in your data file
- Changes frame-by-frame based on your experiment design
- Is determined at render time and needs to be confirmed at presentation time

```rust
#[derive(serde::Serialize)]
struct FrameData {
    trial:    u32,
    contrast: f32,
    phase:    f32,
    grating_sf: f32,
}
```

### What stays in external state

Persistent experiment state that both arms access lives in your closure captures (via
`Rc<RefCell<...>>`). Put here:

- The current trial counter
- Adaptive algorithm state (e.g. staircase threshold estimate)
- Anything that needs to persist across frames without being recorded each frame

```rust
let trial   = Rc::new(RefCell::new(0u32));
let contrast = Rc::new(RefCell::new(1.0f32));
```

---

## Full annotated example

A closed-loop contrast-tracking experiment that reduces stimulus contrast whenever the GPU
misses a frame, and records confirmed timing for each frame:

```rust
use std::{cell::RefCell, rc::Rc};
use vision_stimulus_engine::prelude::*;

#[derive(serde::Serialize)]
struct FrameData {
    trial:    u32,
    contrast: f32,
}

fn run_experiment(context: VSEContext, session: ExperimentSession) -> Result<(), VSEError> {
    // Persistent state shared between Render and Presented arms
    let trial     = Rc::new(RefCell::new(0u32));
    let contrast  = Rc::new(RefCell::new(1.0f32));
    let max_frames = 300u32; // 5 seconds at 60 Hz

    let trial_c    = trial.clone();
    let contrast_c = contrast.clone();

    let context = VSEContext::builder()
        .with_window_size(1920, 1080)
        .with_session(session)
        .build()?;

    context.run_buffered::<FrameData, _>(BufferedConfig::default(), move |event, vse| {
        match event {
            FlipEvent::Render => {
                let t = *trial_c.borrow();
                let c = *contrast_c.borrow();

                vse.clear()?;
                // draw Gabor patch at current contrast …

                // Submit and attach the frame's parameters as payload
                vse.flip_with_payload(None, FrameData { trial: t, contrast: c })?;

                *trial_c.borrow_mut() += 1;
                if t >= max_frames {
                    vse.close();
                }
            }

            FlipEvent::Presented { flip_info, payload } => {
                // Confirmed hardware timing — record with accurate present_time
                vse.record_frame(payload)?;

                // Closed-loop: reduce contrast on missed frames
                if flip_info.missed {
                    tracing::warn!(
                        "Frame {} missed ({} skipped). Reducing contrast.",
                        flip_info.frame_number,
                        flip_info.missed_count,
                    );
                    *contrast_c.borrow_mut() *= 0.9;
                }
            }

            _ => {}
        }
        Ok(())
    })?;

    Ok(())
}
```

---

## Migration guide: `run()` → `run_buffered()`

| Synchronous `run()`                           | Buffered `run_buffered()`                      |
|----------------------------------------------|------------------------------------------------|
| `vse.flip(None)?`                            | `vse.flip_with_payload(None, data)?` in `Render` arm |
| `vse.record_frame(data)?`                    | `vse.record_frame(payload)?` in `Presented` arm |
| `flip_info` returned by `flip()`             | `flip_info` delivered in `FlipEvent::Presented` |
| No explicit payload                          | `T: serde::Serialize + Send + 'static`         |
| Single callback `FnMut(&mut RenderContext)`  | `FnMut(FlipEvent<T>, &mut RenderContext)`       |

**Synchronous:**

```rust
context.run(|vse| {
    vse.clear()?;
    let info = vse.flip(None)?;
    if info.frame_number < 300 {
        vse.record_frame(MyData { contrast: 1.0 })?;
    } else {
        vse.close();
    }
    Ok(())
})?;
```

**Buffered equivalent:**

```rust
let frame = Rc::new(RefCell::new(0u64));
let fr = frame.clone();

context.run_buffered::<MyData, _>(BufferedConfig::default(), move |event, vse| {
    match event {
        FlipEvent::Render => {
            vse.clear()?;
            *fr.borrow_mut() += 1;
            vse.flip_with_payload(None, MyData { contrast: 1.0 })?;
            if *fr.borrow() >= 300 { vse.close(); }
        }
        FlipEvent::Presented { payload, .. } => {
            vse.record_frame(payload)?;
        }
        _ => {}
    }
    Ok(())
})?;
```

The key differences:
- `flip()` → `flip_with_payload()` in `Render`
- `record_frame()` moves to `Presented`, where `flip_info` carries confirmed timing
- Per-frame state (e.g. frame counter) lives in `Rc<RefCell<...>>` captures

---

## Shutdown and panic behavior

**Clean shutdown** (`vse.close()` or window close): `run_buffered()` drains all pending
in-flight fences synchronously, firing `Presented` for every submitted frame before
returning. Data for all submitted frames is guaranteed to reach your writer.

**Panic**: If your callback panics, Rust's unwinding will drop all in-flight
`FenceSignalFuture`s. Vulkano's `Drop` impl for `FenceSignalFuture` blocks until the fence
signals, so the GPU is always quiesced cleanly. However, any pending `Presented` callbacks
will not fire — data for the in-flight frames at the time of the panic is lost. This is the
same guarantee as synchronous `run()`.
