# Advanced Stimuli Design (Phase 4+5)

**Date:** 2026-02-16
**Status:** Approved

## Overview

This design covers GPU-accelerated gratings, Gabor patches, noise patterns, and dot rendering for Random Dot Kinematograms (RDK). It folds Phase 4 (shader-based stimuli) into Phase 5 (advanced stimuli) since Phase 5 depends on Phase 4's foundations.

## Architecture: Unified Shader Pipeline

Each stimulus type gets its own graphics pipeline with a dedicated fragment shader. Pipelines are compiled once at startup and switching between them during rendering is essentially free.

### New Pipelines

| Pipeline | Vertex Shader | Fragment Shader | Push Constants | Purpose |
|----------|--------------|----------------|----------------|---------|
| `grating` | `parametric.vert` | `grating.frag` | frequency, orientation, phase, contrast, background, wave_type, viewport_size, rect | Sine/square wave gratings |
| `gabor` | `parametric.vert` | `gabor.frag` | Same as grating + sigma | Gaussian-windowed grating |
| `dot` | `dot.vert` | `dot.frag` | dot_radius, dot_color, viewport_size | Instanced point rendering |

Existing `flat_color` and `textured` pipelines are unchanged. Noise reuses the `textured` pipeline since it's CPU-generated and uploaded as a texture.

### Why Separate Pipelines

GPUs execute pixels in lockstep groups (warps/wavefronts). Branching within a shader (e.g., `if stimulus_type == GRATING`) forces all pixels in a group to execute both branches. Separate pipelines run the exact right code with zero branching. The cost of creating pipelines is paid once at startup (~10-50ms each); binding a pipeline during rendering takes nanoseconds.

## Stimulus Details

### Gratings (GPU fragment shader)

Mathematical sine or square wave, computed per-pixel. Parameters passed via push constants (40 bytes):

- `viewport_size`: vec2
- `rect`: vec4 (left, top, right, bottom in pixels)
- `frequency`: float (cycles per pixel)
- `orientation`: float (radians, 0 = vertical)
- `phase`: float (radians)
- `contrast`: float (0.0-1.0)
- `background`: float (mean luminance)
- `wave_type`: uint (0=sine, 1=square)

Deterministic by definition — same parameters produce identical output on any hardware.

### Gabor (GPU fragment shader)

Same as grating with an additional Gaussian envelope. Push constants (44 bytes) add `sigma` to the grating set. Replaces the current CPU-side `GaborParams::generate()` for real-time use (the CPU version remains available for offline/texture-based workflows).

### Plaids

Two `draw_grating()` calls with additive blending. No separate pipeline needed.

### Noise (CPU-generated, texture upload)

CPU generation chosen over GPU for reproducibility:

- Same seed + same resolution = bit-exact identical output across machines
- Seed sequence can be logged for full experiment replay
- GPU shader PRNG can vary between vendor shader compilers

Generation functions:
- **White noise**: Seeded ChaCha8 RNG → uniform random values
- **Pink (1/f) noise**: Generate white noise, FFT, apply 1/f filter, inverse FFT (via `rustfft`)
- **Binary noise**: Seeded RNG → threshold at 0.5 → black or white

Upload cost: 512x512 RGBA at 60Hz = ~1MB/frame; 1920x1080 = ~8MB/frame. Both well within PCIe bandwidth.

### Dots / RDK

VSE provides an instanced dot rendering pipeline. RDK motion logic (coherence, direction, lifetime, random resets) lives in user code, not in VSE.

**Rationale**: RDK behavior is experiment logic. Different labs use different motion rules, lifetime strategies, and coherence algorithms. VSE provides the efficient rendering primitive; users compose the behavior.

API: `vse.draw_dots(&positions, dot_radius, dot_color)` where `positions: &[(f32, f32)]`.

## Parameter Structs

```rust
pub struct GratingParams {
    pub frequency: f32,
    pub orientation: f32,
    pub phase: f32,
    pub contrast: f32,
    pub background: f32,
    pub wave: WaveType,
}

pub enum WaveType { Sine, Square }

pub struct NoiseParams {
    pub noise_type: NoiseType,
    pub seed: u64,
    pub width: u32,
    pub height: u32,
    pub contrast: f32,
    pub background: f32,
}

pub enum NoiseType { White, Pink, Binary }
```

## API Surface (RenderContext methods)

```rust
vse.draw_grating(left, top, right, bottom, &grating_params);
vse.draw_gabor(left, top, right, bottom, &gabor_params);
vse.draw_noise(left, top, right, bottom, &noise_params);
vse.draw_dots(&positions, dot_radius, dot_color);
```

## Push Constant Layouts

All within Vulkan's guaranteed 128-byte minimum:

- **Grating**: 40 bytes (viewport_size + rect + 5 params + wave_type)
- **Gabor**: 44 bytes (grating + sigma)
- **Dot**: 28 bytes (viewport_size + dot_radius + dot_color)

No uniform buffers or descriptor sets needed for grating/gabor pipelines.

## File Layout

### New files

```
src/shaders/parametric.vert    — shared vertex shader (quad → UV + rect bounds)
src/shaders/grating.frag       — sine/square grating
src/shaders/gabor.frag         — grating × gaussian envelope
src/shaders/dot.vert           — instanced dot rendering
src/shaders/dot.frag           — circular dot with anti-aliased edge

src/drawing/stimuli.rs         — GratingParams, NoiseParams, WaveType, NoiseType
src/drawing/noise.rs           — CPU noise generation (white, pink, binary)
src/drawing/dot_buffer.rs      — DotInstance vertex type, instanced buffer helpers
```

### Modified files

- `src/drawing/renderer.rs` — New pipeline fields, create methods, render() extensions
- `src/drawing/primitives.rs` — New DrawCommand variants: Grating, Gabor, Noise, Dots
- `src/drawing/vertex.rs` — DotInstance vertex type
- `src/core/context.rs` — New draw methods on RenderContext
- `src/drawing/mod.rs`, `src/lib.rs` — Exports
- `Cargo.toml` — Add rand, rand_chacha, rustfft

### New example

- `examples/04_advanced_stimuli.rs` — Demonstrates all new stimulus types

### New documentation

- `docs/guides/pipelines.md` — In-depth pipeline guide for vision scientists

## Dependencies

| Crate | Purpose |
|-------|---------|
| `rand` | RNG traits |
| `rand_chacha` | Deterministic seeded PRNG for noise |
| `rustfft` | FFT for pink noise 1/f spectrum filtering |

## Testing Strategy

**Unit tests (no GPU):**
- Noise: deterministic output, correct dimensions, spectral properties of pink noise
- Param structs: defaults, enum coverage
- Dot vertex layout correctness
- Reproducibility: same seed → identical output; different seed → different output

**Visual validation (manual, GPU required):**
- `04_advanced_stimuli.rs` example
- Existing examples unchanged (regression)

**No GPU-dependent CI tests** — same constraint as prior phases.
