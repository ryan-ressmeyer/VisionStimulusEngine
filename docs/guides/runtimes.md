# Choosing a Runtime

VSE provides synchronous, buffered, and offscreen runtimes. Choose the smallest runtime that provides the timing and state coordination your experiment needs.

| Runtime | Presentation | State model | Use it for |
|---|---|---|---|
| `run()` | Synchronous | One render closure | Tutorials, diagnostics, and experiments that need the result of each flip before building the next frame |
| `run_with_setup()` | Synchronous | GPU-initialized setup value | Synchronous experiments with pipelines, textures, models, or device buffers |
| `run_buffered()` | Pipelined | Independent render and confirmation closures | Open-loop sequences whose confirmed results are recorded but do not affect rendering |
| `run_buffered_with_state()` | Pipelined | GPU-initialized state shared by both phases | Adaptive and closed-loop experiments |
| `run_headless()` | Offscreen | Render closure plus pixel sink | Regeneration, pixel tests, and record-path benchmarks |

## `run()`

`run()` presents one frame and waits for confirmation before the callback continues.

```rust,ignore
let mut contrast = 1.0;

context.run(move |vse| {
    draw_grating(vse, contrast)?;

    let flip = vse.flip(None)?;
    vse.record_frame(FrameData { contrast })?;

    if flip.missed {
        contrast *= 0.9;
    }
    Ok(())
})?;
```

The callback follows ordinary sequential control flow. Use this runtime when the experiment needs the result of frame N before it can build frame N+1. It is also the displayed runtime available for direct-display sessions.

## `run_with_setup()`

A displayed Vulkan device and swapchain do not exist until the windowing loop starts. `run_with_setup()` initializes GPU-dependent resources after those objects exist and before frame zero.

```rust,ignore
context.run_with_setup(
    |vse| {
        let pipeline = vse.register_pipeline(MyPipeline::new())?;
        let texture = vse.load_image("stimulus.png")?;
        Ok((pipeline, texture))
    },
    |vse, resources| {
        draw_stimulus(vse, resources)?;
        vse.flip(None)?;
        Ok(())
    },
)?;
```

Setup does not change the synchronous timing model. It keeps pipeline compilation and asset loading outside the presentation path.

## `run_buffered()`

`run_buffered()` separates frame construction from confirmation. The render callback returns one `BufferedFrame<T>`. VSE submits it after the callback returns. The confirmation callback later receives the same payload in a `ConfirmedFrame<T>`.

```rust,ignore
context.run_buffered(
    BufferedConfig::default(),
    move |vse| {
        let frame = sequence.next().expect("stimulus sequence exhausted");
        draw_stimulus(vse, &frame)?;

        if frame.is_final {
            vse.close();
        }
        Ok(BufferedFrame::new(frame))
    },
    move |confirmed, vse| {
        vse.record_frame(confirmed.payload)?;
        Ok(())
    },
)?;
```

Use this form when rendering and confirmation do not need the same mutable state. Common cases include fixed trial schedules, movie playback, and deterministic stimulus replay.

A scheduled frame uses the same payload path:

```rust,ignore
Ok(BufferedFrame::at(target_time, frame_data))
```

Every successful render callback produces exactly one submission. Do not call `flip()` inside a buffered render callback.

## `run_buffered_with_state()`

Most adaptive experiments need confirmation from an earlier frame to influence future rendering. `run_buffered_with_state()` initializes one state value and passes it to both callbacks.

```rust,ignore
struct Experiment {
    trial: u32,
    contrast: f32,
    pipeline: RegisteredPipeline,
}

context.run_buffered_with_state(
    BufferedConfig::default(),
    |vse| {
        Ok(Experiment {
            trial: 0,
            contrast: 1.0,
            pipeline: vse.register_pipeline(MyPipeline::new())?,
        })
    },
    |experiment, vse| {
        draw_grating(vse, experiment.pipeline, experiment.contrast)?;

        let payload = FrameData {
            trial: experiment.trial,
            contrast: experiment.contrast,
        };
        experiment.trial += 1;

        if experiment.trial == 300 {
            vse.close();
        }
        Ok(BufferedFrame::new(payload))
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

The initializer runs after the final buffered swapchain has been created and before frame zero. It replaces a separate buffered setup API. Use the returned state for experiment variables and GPU-dependent resources shared by rendering and confirmation.

With the default depth of one, frame N+1 has already been submitted when confirmation arrives for frame N. A change made in the confirmation callback first affects frame N+2. See [Buffered Flips](buffered_flips.md) for the full timing contract.

## `run_headless()`

Headless rendering uses the same drawing and `flip()` calls as synchronous displayed rendering, but sends completed pixels to a sink instead of a display.

```rust,ignore
let mut headless = HeadlessContext::builder(512, 512)
    .build()?;

headless.run_headless(
    |captured| {
        save_or_compare(captured.frame_number(), captured.bytes())?;
        Ok(())
    },
    |vse| {
        draw_stimulus(vse)?;
        vse.flip(None)?;
        Ok(())
    },
)?;
```

Headless timing is synthesized and marked `TimingSource::Offscreen`. It must not be interpreted as recorded display timing. See [Headless Rendering](headless.md).

## Runtime transitions in concrete experiments

### Add setup when frame zero starts missing

A prototype begins with `run()` and loads a texture during its first callback. Pipeline compilation and image upload make frame zero late. Move that work to `run_with_setup()` while leaving the synchronous render logic unchanged.

```rust,ignore
// Before
context.run(|vse| {
    let texture = vse.load_image("face.png")?;
    vse.draw_texture(texture, 0.0, 0.0, 512.0, 512.0);
    vse.flip(None)?;
    Ok(())
})?;

// After
context.run_with_setup(
    |vse| vse.load_image("face.png"),
    |vse, texture| {
        vse.draw_texture(*texture, 0.0, 0.0, 512.0, 512.0);
        vse.flip(None)?;
        Ok(())
    },
)?;
```

### Add buffering when stimulus generation can run ahead

A fixed image sequence does not use each flip result to select the next image. Move from `run()` to `run_buffered()` so CPU frame construction overlaps GPU work. Carry the image index and stimulus parameters in the payload for later recording.

```rust,ignore
context.run_buffered(
    BufferedConfig::default(),
    move |vse| {
        let stimulus = schedule.next().unwrap();
        draw_stimulus(vse, &stimulus)?;
        Ok(BufferedFrame::new(stimulus))
    },
    |confirmed, vse| {
        vse.record_frame(confirmed.payload)?;
        Ok(())
    },
)?;
```

Keep `run()` when frame N+1 cannot be chosen until frame N is confirmed. Buffering adds a known reaction delay.

### Add shared state when confirmation changes the stimulus

An open-loop contrast sequence starts with `run_buffered()`. The experiment later adds an adaptive rule that lowers contrast after a missed frame. Both phases now need the same contrast and trial state, so move them into `run_buffered_with_state()`.

```rust,ignore
context.run_buffered_with_state(
    BufferedConfig::default(),
    |_vse| Ok(ExperimentState::new()),
    |state, vse| {
        draw_grating(vse, state.contrast)?;
        Ok(BufferedFrame::new(state.current_record()))
    },
    |state, confirmed, vse| {
        vse.record_frame(confirmed.payload)?;
        state.update_from_flip(&confirmed.flip_info);
        Ok(())
    },
)?;
```

This transition also applies when both phases need the same external-frame bookkeeping, readback buffers, or trial controller.

### Move offscreen when display timing is no longer the measurement

A displayed experiment has finished collecting data. The next task is to regenerate every stimulus image and compare pixels across software versions. Build a headless context and use `run_headless()` instead of a displayed runtime. Keep the drawing code unchanged, and consume pixels in the sink.

Use a displayed runtime for measured scanout timing. Use headless rendering for regeneration and image-level tests.
