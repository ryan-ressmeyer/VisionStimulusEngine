# Headless (offscreen) rendering

VSE can render with no window, swapchain, compositor, or display — into an
offscreen image that is copied back to host memory after every flip. It needs a
GPU, not a screen, so it runs over SSH and in CI.

It exists for **post-hoc reproducibility**: months after an experiment ran, you
still have its recorded metadata and its render closure, and you want the exact
frames the subject saw as pixels a model can consume.

The key property is that headless is not a parallel renderer. A headless flip
runs the *same* `Renderer` code path a windowed flip runs, records the same
command buffer, and composites in the same order. Only the destination and the
timing differ. A parallel implementation would reproduce a session's stimuli
only as well as the two implementations happened to agree.

## Quick start

```rust
use vision_stimulus_engine::prelude::*;

let mut headless = HeadlessContext::builder(512, 512)
    .with_clear_color(0.5, 0.5, 0.5, 1.0)
    .build()?;

let mut frame = 0u64;
headless.run_headless(
    // The sink: called once per flip, in order, with that frame's pixels.
    |captured| {
        println!("frame {} is {}x{}", captured.frame_number(), captured.width(), captured.height());
        Ok(())
    },
    // The render closure: the same signature `run` takes.
    |vse| {
        vse.draw_circle(256.0, 256.0, 40.0, Color::WHITE);
        vse.flip(None)?;
        frame += 1;
        if frame == 60 {
            vse.request_exit();
        }
        Ok(())
    },
)?;
```

`HeadlessContext::builder()` selects the offscreen builder, and `run_headless()` replaces a displayed runtime.
Everything between, including `draw_*`, `load_image`, `register_pipeline`,
`draw_with`, `draw_custom`, external-frame consumption, and `record_frame`, is
unchanged. An experiment's render closure can therefore run in both modes.

`run_headless_with_setup(setup, sink, render)` mirrors `run_with_setup`. Build
pipelines, attach external renderers, and load assets there rather than per frame.

A headless context can import a compatible external image ring used by a
displayed session when the selected GPU supports the required external-memory
and synchronization features. Its offscreen submission waits on the producer's
GPU semaphore, blits the external image, draws VSE overlays, and copies the
combined target to the readback buffer. This path remains offscreen and never
calls displayed submission or presentation code. The `vse-3d` crate uses this
path for 3D regeneration.

## What the sink receives

`CapturedFrame` borrows the frame for the duration of the call:

| Method | What it gives you |
|---|---|
| `frame_number()` | The flip that produced it |
| `width()` / `height()` | Extent in pixels |
| `format()` | The image format the bytes are in |
| `bytes()` | Raw image bytes, row-major, in `format()` |
| `pixel(x, y)` | One texel as RGBA8, BGRA normalized |
| `to_rgba8()` | The whole frame as RGBA8 — a copy |

`bytes()` is the *raw* buffer: for a `B8G8R8A8_*` target the channel order is
BGRA, and for an `_SRGB` target the values are sRGB-encoded, not linear. Hash
`bytes()` when comparing runs; use `to_rgba8()` when handing pixels to an image
library.

Copy anything you need to keep — the borrow ends when the sink returns.

## Regenerating a recorded session

Build the target from the recording rather than from arguments you retype:

```rust
let recovered: HostInfo = serde_json::from_str(&std::fs::read_to_string("host_info.json")?)?;

let mut regenerated = HeadlessContext::builder_from_host_info(&recovered)?
    .build()?;
```

That takes the color format and extent from `HostInfo.swapchain`. Both affect the pixels:

- **Format** — rendering `R8G8B8A8_UNORM` what was displayed as `B8G8R8A8_SRGB`
  gives different bytes for the same stimulus.
- **Extent** — a different size rasterizes different pixels, not the same
  picture scaled.

An unrecognized format is refused rather than approximated. Every context constructs all seven base 2D pipelines, so `HostInfo.pipeline.builtin_pipelines` is retained only for compatibility with older recordings and does not control regeneration. External renderers must be registered again by the experiment's regeneration code; their crate-owned metadata identifies their resources and configuration.

The clear color and refresh rate are **not** applied automatically — set them
yourself from `HostInfo.pipeline` if your stimulus depends on them.

`examples/23_headless_regeneration.rs` runs both halves of this workflow end to
end and verifies the frames match.

## Timing is synthesized, never measured

The normative interpretation of these fields is defined in
[Timing conformance](../timing-conformance.md#headless-runtime).

There is no display, so there is no scanout clock and no presentation to time.
A headless `FlipInfo` carries:

- `timing_source: TimingSource::Offscreen`
- `present_time = frame_number × frame_interval`, where the interval comes from
  `with_expected_refresh_rate` and defaults to 60 Hz
- `submit_time` from the host clock — real, but it measures how long
  regeneration took, not when anything was shown

`TimingSource::Offscreen` exists so this can never be confused with a recorded
session: if you record data during a headless run, every row is tagged as
regenerated. `scanout_now()`, `host_to_scanout()`, `sample_present_calibration()`,
and `scanout_feedback()` all return `None`/empty headless, and `flip(Some(t))`
accepts a target time but does not wait for it — pacing an offscreen render
against a wall clock would only make regeneration slower.

## What headless does not do

- **No input.** No window means no keyboard or mouse; `key_pressed` and friends
  are always false. An experiment closure that branches on input takes its
  no-input path, which is usually what you want when replaying — but check.
- **No buffered mode.** `run_buffered` pipelines against vblank; there is no
  vblank. Headless flips are synchronous by nature: the readback is only valid
  once the GPU is done.
- **No display timing for external frames.** External rings can supply pixels,
  but their headless consumption ends in offscreen readback and carries
  `TimingSource::Offscreen` like every other headless frame.
- **No monitors or window control.** `swapchain()` returns `None`,
  `available_monitors()` is empty, and `set_window_mode`/cursor calls are no-ops.
  Use `color_format()` — which answers in both modes — wherever you would have
  reached for `swapchain().format()`.

## Determinism: the honest boundary

Frames regenerate **byte-identically on the same machine and driver**. That is
what the test suite verifies and what the example checks.

Across GPU vendors or driver versions, they may not. Rasterization rules leave
room for implementation differences, floating-point evaluation in shaders is not
bit-specified, and drivers change. Compare hashes within a machine; treat a
cross-machine comparison as a similarity check, not an equality check.

Determinism also depends on the stimulus being a pure function of frame number.
A closure that reads a clock, an unseeded RNG, or live input produces a
different stimulus on every run — headless does not make it reproducible, it
just removes the display from the list of reasons it is not.

## Testing with headless

Headless is what makes pixel-level tests possible; `tests/headless_pixels.rs`
uses it to pin call-order compositing and registered-pipeline dispatch, and
`benches/frame_timing.rs` uses it to measure the record path. Both run without a
display. A test that needs a *window* still cannot run under the harness at all
— winit panics when an `EventLoop` is built off the main thread.
