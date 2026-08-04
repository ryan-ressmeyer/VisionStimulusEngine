# Buffered Flips

`run_buffered()` pipelines CPU and GPU work across frames. It correlates each submitted payload with one confirmed presentation result and exposes the reaction delay to the experiment.

See [Choosing a Runtime](runtimes.md) before selecting a buffered API.

## Presentation pipeline

A synchronous frame follows this sequence:

```text
build → submit → wait → inspect timing → build next frame
```

Buffered presentation overlaps the work:

```text
CPU: build N → submit → build N+1 → submit → build N+2
GPU:           render N →           render N+1
```

The driver provides backpressure through swapchain image acquisition. VSE does not discard an already-submitted frame.

## Structured callbacks

The stateless runtime takes separate render and confirmation callbacks:

```rust,ignore
context.run_buffered(
    BufferedConfig::default(),
    |vse| {
        draw_stimulus(vse)?;
        Ok(BufferedFrame::new(FrameData { contrast: 1.0 }))
    },
    |confirmed, vse| {
        vse.record_frame(confirmed.payload)?;
        Ok(())
    },
)?;
```

The render callback returns one `BufferedFrame<T>`. VSE converts it to a `FrameRequest`, submits it through the active displayed backend, and stores its payload with the resulting `Submission`. The confirmation callback receives a `ConfirmedFrame<T>` containing:

- `flip_info`, with the timing and present identifier for that submission;
- `payload`, moved from the matching `BufferedFrame<T>`.

The payload type does not need `Serialize` unless user code passes it to `record_frame()`.

Use `run_buffered_with_state()` when the two callbacks share experiment state or GPU resources:

```rust,ignore
context.run_buffered_with_state(
    BufferedConfig::default(),
    |vse| {
        Ok(Experiment {
            pipeline: vse.register_pipeline(MyPipeline::new())?,
            contrast: 1.0,
        })
    },
    |experiment, vse| {
        draw_grating(vse, experiment.pipeline, experiment.contrast)?;
        Ok(BufferedFrame::new(FrameData {
            contrast: experiment.contrast,
        }))
    },
    |experiment, confirmed, vse| {
        vse.record_frame(confirmed.payload)?;
        if confirmed.flip_info.missed {
            experiment.contrast *= 0.9;
        }
        Ok(())
    },
)?;
```

The initializer runs after the GPU and final buffered swapchain exist but before frame zero.

## Target times

`BufferedFrame::new(payload)` presents at the next available vblank.

`BufferedFrame::at(target_time, payload)` requests a specific scanout-clock target. On the EXT path, VSE places the target in the present-timing chain. Buffered presentation leaves pacing to the pipelined driver queue.

## Confirmation and timing

On the `ExtPresentTiming` path, VSE assigns a `VK_KHR_present_id2` identifier to every successful present. It drains EXT feedback once after each buffered submission and caches records by present identifier. Confirmation uses the matching feedback record when the driver supplies `IMAGE_FIRST_PIXEL_OUT`.

If per-present scanout feedback is unavailable, buffered confirmation falls back to the timing provider's CPU observation. `FlipInfo::timing_source` and recorded host capabilities describe the active timing path.

On the `CpuEstimate` path, confirmation follows the GPU fence. This confirms rendering completion rather than physical scanout.

## Buffer depth

| `depth` | Minimum swapchain images | Additional reaction delay | Suggested use |
|---|---:|---:|---|
| `1` | 2 | One pipelined frame | Most experiments |
| `2` | 3 | Two pipelined frames | GPU-bound workloads after measurement |

`BufferedConfig::default()` uses depth one.

When confirmation arrives for frame N at depth one, frame N+1 has already been submitted. A state change made in that confirmation callback first affects frame N+2. Increasing depth gives the GPU more queued work and delays closed-loop reactions by another frame.

## Payload design

A payload should describe what the render callback submitted for one frame. Typical fields include:

- trial and condition identifiers;
- contrast, phase, position, or image identifier;
- external producer frame and slot identifiers;
- repeat or stale-frame decisions made before submission.

Persistent controllers, loaded resources, and adaptive state belong in the state returned to `run_buffered_with_state()`.

VSE stores each payload in the same FIFO entry as its submission metadata and completion object. Confirmation removes that entry as one unit, preserving payload order through normal operation and shutdown draining.

## Recording

Call `record_frame()` from the confirmation callback:

```rust,ignore
|confirmed, vse| {
    vse.record_frame(confirmed.payload)?;
    Ok(())
}
```

Before invoking the callback, VSE installs the confirmed `FlipInfo` as the active recording context. If the callback does not claim the frame with `record_frame()`, VSE writes one timing-only row when confirmation finishes.

This behavior also applies while draining the final submissions during clean shutdown.

## Shutdown

Calling `vse.close()` from the render callback still submits the `BufferedFrame` returned by that invocation. VSE then waits for every queued submission and invokes confirmation in FIFO order before `run_buffered()` returns.

Calling `close()` from a confirmation callback prevents another render invocation and drains submissions already in flight.

If a confirmation callback returns an error during draining, VSE continues retiring the queue and returns the first error afterward. A panic unwinds the event loop and does not guarantee delivery of remaining confirmation callbacks.

## External rendering

Queue an external frame before returning the corresponding `BufferedFrame`:

```rust,ignore
let ready = producer.render_frame(vse.frame_number())?;
vse.queue_external_frame(ready.slot)?;

Ok(BufferedFrame::new(ExternalFrameRecord {
    producer_frame: ready.frame_number,
    slot: ready.slot,
}))
```

The confirmation fence also marks matching readback copies and external-frame consumption as complete. See [External Rendering Timing](external_rendering_timing.md).

## Migrating from synchronous presentation

Synchronous:

```rust,ignore
context.run(|vse| {
    draw_stimulus(vse)?;
    let flip_info = vse.flip(None)?;
    vse.record_frame(FrameData { contrast: 1.0 })?;
    react_to_flip(&flip_info);
    Ok(())
})?;
```

Buffered:

```rust,ignore
context.run_buffered_with_state(
    BufferedConfig::default(),
    |_vse| Ok(ExperimentState::new()),
    |state, vse| {
        draw_stimulus(vse)?;
        Ok(BufferedFrame::new(state.frame_data()))
    },
    |state, confirmed, vse| {
        vse.record_frame(confirmed.payload)?;
        state.react_to_flip(&confirmed.flip_info);
        Ok(())
    },
)?;
```

The buffered version moves the reaction into the confirmation phase and makes the pipeline delay explicit. Keep the synchronous runtime when the next frame must incorporate the immediately preceding result.
