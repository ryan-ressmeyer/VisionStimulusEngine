# VSE demo curriculum

A curated, numbered set of example experiments and demos. It draws on the
Psychtoolbox `PsychDemos` catalogue, consolidating ~60 of those demos into a
smaller teaching sequence and adding timing/reproducibility demos that
Psychtoolbox has no equivalent for. Content deliberately left out (movies,
audio, video capture, HDR, 2D stereo, gamma/color calibration) is catalogued in
[`../docs/ptb-deferred-content.md`](../docs/ptb-deferred-content.md).

Run any demo by its file basename:

```bash
cargo run --release --example 01_primitives_gallery
```

Most demos exit on **Escape**. The timing/diagnostic demos auto-terminate after
a fixed duration or frame count and print a summary.

## Fundamentals

| # | Demo | What it shows | Consolidates (PTB) |
|---|------|---------------|--------------------|
| 00 | `hello_flip` | Window, clear color, the flip loop | — |
| 01 | `primitives_gallery` | Rect, circle, line, arc, dots | ArcDemo, LinesDemo, DotDemo |
| 02 | `gratings_drift` | Drifting sine/square gratings, orientation, TF | GratingDemo, DriftDemo1–6, DriftWaitDemo |
| 03 | `gabor_field` | Moving procedural Gaborium with additive superposition | ProceduralGarboriumDemo |
| 04 | `noise_suite` | White/pink/binary, contrast-modulated, masked noise | FastNoise, FastMaskedNoise, ContrastModulatedNoise |
| 05 | `images_alpha` | Image scaling, alpha compositing, per-pixel mixing | AlphaImageDemo, SimpleImageMixingDemo |

## Timing & reproducibility (VSE's differentiator)

| # | Demo | What it shows |
|---|------|---------------|
| 06 | `timing_validation` | Frames land where they were scheduled |
| 07 | `scheduled_onset` | Scheduling a flip at a target scanout time |
| 08 | `reproducibility_hash` | Seed → identical pixels across runs (hashed) |
| 09 | `photodiode_sync` | Flashing patch + logged scanout onset times (DAQ alignment) |
| 10 | `present_timing_internals` | `VK_EXT_present_timing` internals: `drift` / `feedback` / `buffered` modes |
| 11 | `host_clock_bridge` | Scanout ↔ `CLOCK_MONOTONIC` calibration bridge |

## Display & host

| # | Demo | What it shows |
|---|------|---------------|
| 12 | `host_and_display_info` | Backend, monitors, video modes, `HostInfo` snapshot |
| 13 | `direct_display_scanout` | Compositor-free direct display + B3 scanout timing (auth. test) |

## Interaction & experiments

| # | Demo | What it shows | Consolidates (PTB) |
|---|------|---------------|--------------------|
| 14 | `input_and_rt` | Keyboard/mouse, mouse trace, reaction time | KbDemo, MouseTraceDemo1–3 |
| 15 | `gaze_contingent` | Mouse-as-gaze moving window | GazeContingentDemo |
| 16 | `rdk_motion` | Random-dot kinematogram with adjustable coherence | (RDK; no single PTB file) |
| 17 | `staircase_2afc` | 2-down-1-up staircase driving a 2AFC task + logging | MinExpEntStairDemo |
| 18 | `metacontrast_masking` | Full metacontrast-masking experiment (capstone) | PsychExampleExperiments |

## Text

| # | Demo | What it shows | Consolidates (PTB) |
|---|------|---------------|--------------------|
| 19 | `text_and_instructions` | Built-in bitmap font: instructions, labels, feedback | DrawSomeTextDemo, DrawFormattedTextDemo, FontDemo |

## 3D

3D / VR-ready rendering lives in the `vse-bevy` crate — a headless Bevy renderer
feeding frames to VSE's external-frame ring:

```bash
cargo run -p vse-bevy --release --example 01_bevy_ring_demo
```
