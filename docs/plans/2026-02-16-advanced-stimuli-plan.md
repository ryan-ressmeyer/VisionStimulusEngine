# Advanced Stimuli Implementation Plan (Phase 4+5)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add GPU-accelerated gratings, Gabor patches, CPU-generated noise patterns, and instanced dot rendering to VSE.

**Architecture:** Each stimulus type gets its own graphics pipeline with a dedicated fragment shader, created once at startup. Gratings and Gabor patches are computed mathematically per-pixel in fragment shaders with parameters via push constants. Noise is generated on CPU with a seeded PRNG for bit-exact reproducibility and uploaded as a texture each frame. Dots use instanced rendering for efficient RDK support.

**Tech Stack:** Rust, vulkano 0.35, vulkano-shaders 0.35, GLSL 460, rand + rand_chacha (seeded RNG), rustfft (pink noise)

**Design doc:** `docs/plans/2026-02-16-advanced-stimuli-design.md`

---

## Task 1: Add Dependencies

**Files:**
- Modify: `Cargo.toml`

**Step 1: Add new dependencies to Cargo.toml**

Add under `[dependencies]`:

```toml
# Deterministic RNG for noise generation
rand = "0.8"
rand_chacha = "0.3"

# FFT for pink noise spectrum filtering
rustfft = "6.2"
```

**Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors

**Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "Add rand, rand_chacha, rustfft dependencies for advanced stimuli"
```

---

## Task 2: Parameter Structs (`stimuli.rs`)

**Files:**
- Create: `src/drawing/stimuli.rs`
- Modify: `src/drawing/mod.rs` (add module + re-exports)
- Modify: `src/lib.rs` (add to prelude)

**Step 1: Write the failing test**

Create `src/drawing/stimuli.rs` with tests first:

```rust
/// Parameters for a sinusoidal or square-wave grating.
///
/// A grating is a repeating pattern of light and dark bars.
/// It is defined by spatial frequency, orientation, phase,
/// contrast, and mean luminance (background).
#[derive(Clone, Debug)]
pub struct GratingParams {
    /// Spatial frequency in cycles per pixel.
    pub frequency: f32,
    /// Orientation in radians (0 = vertical bars, PI/2 = horizontal).
    pub orientation: f32,
    /// Phase in radians.
    pub phase: f32,
    /// Contrast [0.0, 1.0].
    pub contrast: f32,
    /// Mean luminance [0.0, 1.0].
    pub background: f32,
    /// Waveform type.
    pub wave: WaveType,
}

/// Waveform type for gratings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaveType {
    /// Smooth sinusoidal grating.
    Sine,
    /// Hard-edged square wave grating.
    Square,
}

impl Default for GratingParams {
    fn default() -> Self {
        Self {
            frequency: 0.04,
            orientation: 0.0,
            phase: 0.0,
            contrast: 1.0,
            background: 0.5,
            wave: WaveType::Sine,
        }
    }
}

/// Parameters for CPU-generated noise textures.
///
/// Noise is generated deterministically from a seed, allowing
/// exact reproduction of stimuli across sessions and machines.
#[derive(Clone, Debug)]
pub struct NoiseParams {
    /// Type of noise.
    pub noise_type: NoiseType,
    /// Deterministic seed for the PRNG.
    pub seed: u64,
    /// Width of the generated texture in pixels.
    pub width: u32,
    /// Height of the generated texture in pixels.
    pub height: u32,
    /// Contrast [0.0, 1.0].
    pub contrast: f32,
    /// Mean luminance [0.0, 1.0].
    pub background: f32,
}

/// Type of noise pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoiseType {
    /// Uniform random luminance per pixel.
    White,
    /// 1/f power spectrum (natural image statistics).
    Pink,
    /// Each pixel is either black or white.
    Binary,
}

impl Default for NoiseParams {
    fn default() -> Self {
        Self {
            noise_type: NoiseType::White,
            seed: 0,
            width: 256,
            height: 256,
            contrast: 1.0,
            background: 0.5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grating_defaults() {
        let p = GratingParams::default();
        assert_eq!(p.frequency, 0.04);
        assert_eq!(p.orientation, 0.0);
        assert_eq!(p.phase, 0.0);
        assert_eq!(p.contrast, 1.0);
        assert_eq!(p.background, 0.5);
        assert_eq!(p.wave, WaveType::Sine);
    }

    #[test]
    fn test_noise_defaults() {
        let p = NoiseParams::default();
        assert_eq!(p.noise_type, NoiseType::White);
        assert_eq!(p.seed, 0);
        assert_eq!(p.width, 256);
        assert_eq!(p.height, 256);
        assert_eq!(p.contrast, 1.0);
        assert_eq!(p.background, 0.5);
    }

    #[test]
    fn test_wave_type_eq() {
        assert_eq!(WaveType::Sine, WaveType::Sine);
        assert_ne!(WaveType::Sine, WaveType::Square);
    }

    #[test]
    fn test_noise_type_eq() {
        assert_eq!(NoiseType::White, NoiseType::White);
        assert_ne!(NoiseType::White, NoiseType::Pink);
        assert_ne!(NoiseType::Pink, NoiseType::Binary);
    }
}
```

**Step 2: Wire up the module**

In `src/drawing/mod.rs`, add:

```rust
mod stimuli;

pub use stimuli::{GratingParams, NoiseParams, NoiseType, WaveType};
```

In `src/lib.rs`, add to the prelude:

```rust
pub use crate::drawing::{GratingParams, NoiseParams, NoiseType, WaveType};
```

**Step 3: Run tests**

Run: `cargo test drawing::stimuli`
Expected: all 4 tests pass

**Step 4: Commit**

```bash
git add src/drawing/stimuli.rs src/drawing/mod.rs src/lib.rs
git commit -m "Add GratingParams, NoiseParams, WaveType, NoiseType param structs"
```

---

## Task 3: CPU Noise Generation (`noise.rs`)

**Files:**
- Create: `src/drawing/noise.rs`
- Modify: `src/drawing/mod.rs` (add module)

**Step 1: Write failing tests**

Create `src/drawing/noise.rs` with the test module first, then implement above:

```rust
use rand::Rng;
use rand_chacha::ChaCha8Rng;
use rand::SeedableRng;
use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

use super::stimuli::{NoiseParams, NoiseType};

/// Generate a noise texture as RGBA8 pixel data.
///
/// Returns `Vec<u8>` of length `width * height * 4`.
/// Output is deterministic for a given `NoiseParams`.
pub fn generate_noise(params: &NoiseParams) -> Vec<u8> {
    match params.noise_type {
        NoiseType::White => generate_white_noise(params),
        NoiseType::Pink => generate_pink_noise(params),
        NoiseType::Binary => generate_binary_noise(params),
    }
}

fn generate_white_noise(params: &NoiseParams) -> Vec<u8> {
    let mut rng = ChaCha8Rng::seed_from_u64(params.seed);
    let pixel_count = (params.width * params.height) as usize;
    let mut pixels = Vec::with_capacity(pixel_count * 4);

    for _ in 0..pixel_count {
        let noise_val: f32 = rng.gen::<f32>() - 0.5; // [-0.5, 0.5]
        let luminance = (params.background + params.contrast * noise_val).clamp(0.0, 1.0);
        let byte = (luminance * 255.0) as u8;
        pixels.extend_from_slice(&[byte, byte, byte, 255]);
    }

    pixels
}

fn generate_binary_noise(params: &NoiseParams) -> Vec<u8> {
    let mut rng = ChaCha8Rng::seed_from_u64(params.seed);
    let pixel_count = (params.width * params.height) as usize;
    let mut pixels = Vec::with_capacity(pixel_count * 4);

    let low = ((params.background - params.contrast * 0.5).clamp(0.0, 1.0) * 255.0) as u8;
    let high = ((params.background + params.contrast * 0.5).clamp(0.0, 1.0) * 255.0) as u8;

    for _ in 0..pixel_count {
        let byte = if rng.gen::<bool>() { high } else { low };
        pixels.extend_from_slice(&[byte, byte, byte, 255]);
    }

    pixels
}

fn generate_pink_noise(params: &NoiseParams) -> Vec<u8> {
    let w = params.width as usize;
    let h = params.height as usize;
    let pixel_count = w * h;

    // Generate white noise in spatial domain
    let mut rng = ChaCha8Rng::seed_from_u64(params.seed);
    let mut spatial: Vec<f32> = (0..pixel_count).map(|_| rng.gen::<f32>() - 0.5).collect();

    // Process rows: FFT, apply 1/f, inverse FFT
    let mut planner = FftPlanner::<f32>::new();

    // Apply 1/f filtering per row
    let fft_fwd = planner.plan_fft_forward(w);
    let fft_inv = planner.plan_fft_inverse(w);
    for row in 0..h {
        let start = row * w;
        let mut buffer: Vec<Complex<f32>> = spatial[start..start + w]
            .iter()
            .map(|&v| Complex::new(v, 0.0))
            .collect();
        fft_fwd.process(&mut buffer);
        for (i, c) in buffer.iter_mut().enumerate() {
            let freq = if i <= w / 2 { i } else { w - i };
            if freq == 0 {
                *c = Complex::new(0.0, 0.0); // Remove DC
            } else {
                *c /= (freq as f32).sqrt(); // 1/sqrt(f) amplitude = 1/f power
            }
        }
        fft_inv.process(&mut buffer);
        let norm = 1.0 / w as f32;
        for (i, c) in buffer.iter().enumerate() {
            spatial[start + i] = c.re * norm;
        }
    }

    // Apply 1/f filtering per column
    let fft_fwd_col = planner.plan_fft_forward(h);
    let fft_inv_col = planner.plan_fft_inverse(h);
    let mut col_buffer: Vec<Complex<f32>> = vec![Complex::new(0.0, 0.0); h];
    for col in 0..w {
        for row in 0..h {
            col_buffer[row] = Complex::new(spatial[row * w + col], 0.0);
        }
        fft_fwd_col.process(&mut col_buffer);
        for (i, c) in col_buffer.iter_mut().enumerate() {
            let freq = if i <= h / 2 { i } else { h - i };
            if freq == 0 {
                *c = Complex::new(0.0, 0.0);
            } else {
                *c /= (freq as f32).sqrt();
            }
        }
        fft_inv_col.process(&mut col_buffer);
        let norm = 1.0 / h as f32;
        for row in 0..h {
            spatial[row * w + col] = col_buffer[row].re * norm;
        }
    }

    // Normalize to [-0.5, 0.5] range
    let max_abs = spatial.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
    if max_abs > 0.0 {
        for v in spatial.iter_mut() {
            *v = (*v / max_abs) * 0.5;
        }
    }

    // Convert to RGBA
    let mut pixels = Vec::with_capacity(pixel_count * 4);
    for val in &spatial {
        let luminance = (params.background + params.contrast * val).clamp(0.0, 1.0);
        let byte = (luminance * 255.0) as u8;
        pixels.extend_from_slice(&[byte, byte, byte, 255]);
    }

    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_white_noise_dimensions() {
        let params = NoiseParams {
            width: 64,
            height: 32,
            ..Default::default()
        };
        let pixels = generate_noise(&params);
        assert_eq!(pixels.len(), 64 * 32 * 4);
    }

    #[test]
    fn test_white_noise_deterministic() {
        let params = NoiseParams {
            seed: 42,
            width: 64,
            height: 64,
            ..Default::default()
        };
        let a = generate_noise(&params);
        let b = generate_noise(&params);
        assert_eq!(a, b);
    }

    #[test]
    fn test_white_noise_different_seeds() {
        let a = generate_noise(&NoiseParams {
            seed: 1,
            width: 64,
            height: 64,
            ..Default::default()
        });
        let b = generate_noise(&NoiseParams {
            seed: 2,
            width: 64,
            height: 64,
            ..Default::default()
        });
        assert_ne!(a, b);
    }

    #[test]
    fn test_binary_noise_only_two_values() {
        let params = NoiseParams {
            noise_type: NoiseType::Binary,
            seed: 7,
            width: 32,
            height: 32,
            contrast: 1.0,
            background: 0.5,
        };
        let pixels = generate_noise(&params);
        let low = 0u8;   // (0.5 - 0.5).clamp(0,1) * 255 = 0
        let high = 255u8; // (0.5 + 0.5).clamp(0,1) * 255 = 255
        for chunk in pixels.chunks(4) {
            assert!(chunk[0] == low || chunk[0] == high,
                "Expected {} or {}, got {}", low, high, chunk[0]);
            assert_eq!(chunk[3], 255); // alpha
        }
    }

    #[test]
    fn test_binary_noise_deterministic() {
        let params = NoiseParams {
            noise_type: NoiseType::Binary,
            seed: 99,
            width: 32,
            height: 32,
            ..Default::default()
        };
        let a = generate_noise(&params);
        let b = generate_noise(&params);
        assert_eq!(a, b);
    }

    #[test]
    fn test_pink_noise_dimensions() {
        let params = NoiseParams {
            noise_type: NoiseType::Pink,
            seed: 0,
            width: 64,
            height: 64,
            ..Default::default()
        };
        let pixels = generate_noise(&params);
        assert_eq!(pixels.len(), 64 * 64 * 4);
    }

    #[test]
    fn test_pink_noise_deterministic() {
        let params = NoiseParams {
            noise_type: NoiseType::Pink,
            seed: 12,
            width: 64,
            height: 64,
            ..Default::default()
        };
        let a = generate_noise(&params);
        let b = generate_noise(&params);
        assert_eq!(a, b);
    }

    #[test]
    fn test_pink_noise_in_range() {
        let params = NoiseParams {
            noise_type: NoiseType::Pink,
            seed: 0,
            width: 64,
            height: 64,
            contrast: 1.0,
            background: 0.5,
        };
        let pixels = generate_noise(&params);
        // All RGB values should be in [0, 255], alpha always 255
        for chunk in pixels.chunks(4) {
            assert_eq!(chunk[3], 255);
        }
    }

    #[test]
    fn test_zero_contrast_is_flat() {
        let params = NoiseParams {
            noise_type: NoiseType::White,
            seed: 0,
            width: 32,
            height: 32,
            contrast: 0.0,
            background: 0.5,
        };
        let pixels = generate_noise(&params);
        let expected = (0.5 * 255.0) as u8;
        for chunk in pixels.chunks(4) {
            assert_eq!(chunk[0], expected);
        }
    }
}
```

**Step 2: Wire up the module**

In `src/drawing/mod.rs`, add:

```rust
pub(crate) mod noise;
```

**Step 3: Run tests**

Run: `cargo test drawing::noise`
Expected: all 9 tests pass

**Step 4: Commit**

```bash
git add src/drawing/noise.rs src/drawing/mod.rs
git commit -m "Add CPU noise generation: white, pink (FFT), and binary with seeded RNG"
```

---

## Task 4: DotInstance Vertex Type

**Files:**
- Modify: `src/drawing/vertex.rs` (add DotInstance)
- Modify: `src/drawing/mod.rs` (add re-export)

**Step 1: Add DotInstance to vertex.rs**

After the `TexturedVertex` definition, add:

```rust
/// Per-instance data for dot rendering.
///
/// Each instance represents one dot at a pixel position.
/// Used with instanced rendering for efficient RDK display.
#[derive(Clone, Copy, Debug, Default, BufferContents, Vertex)]
#[repr(C)]
pub struct DotInstance {
    #[format(R32G32_SFLOAT)]
    pub position: [f32; 2],
}
```

Add a test in the existing tests module:

```rust
#[test]
fn test_dot_instance_default() {
    let d = DotInstance::default();
    assert_eq!(d.position, [0.0, 0.0]);
}

#[test]
fn test_dot_instance_size() {
    assert_eq!(std::mem::size_of::<DotInstance>(), 8);
}
```

**Step 2: Add re-export in mod.rs**

In `src/drawing/mod.rs`, update the vertex re-export line:

```rust
pub use vertex::{DotInstance, TexturedVertex, Vertex2D};
```

**Step 3: Run tests**

Run: `cargo test drawing::vertex`
Expected: all 6 tests pass (4 existing + 2 new)

**Step 4: Commit**

```bash
git add src/drawing/vertex.rs src/drawing/mod.rs
git commit -m "Add DotInstance vertex type for instanced dot rendering"
```

---

## Task 5: New DrawCommand Variants

**Files:**
- Modify: `src/drawing/primitives.rs` (add Grating, Gabor, Noise, Dots variants)

**Step 1: Add new DrawCommand variants**

Add these variants to the `DrawCommand` enum in `src/drawing/primitives.rs`:

```rust
use super::color::Color;
use super::stimuli::{GratingParams, NoiseParams};

// ... existing variants ...

/// GPU-computed sinusoidal or square-wave grating.
Grating {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    params: GratingParams,
},

/// GPU-computed Gabor patch (grating x Gaussian envelope).
Gabor {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    params: crate::drawing::GaborParams,
},

/// CPU-generated noise uploaded as texture.
Noise {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    texture_id: u64,
},

/// Instanced dot rendering.
Dots {
    positions: Vec<[f32; 2]>,
    radius: f32,
    color: Color,
},
```

**Step 2: Run check**

Run: `cargo check`
Expected: compiles (no test needed for enum variants beyond what render() will exercise)

Note: You will need to add `_ => {}` or explicit match arms in `generate_flat_color_vertices()` for the new variants (they are handled separately, like `Texture`). Update the existing match in `generate_flat_color_vertices` to add:

```rust
DrawCommand::Grating { .. } => {}
DrawCommand::Gabor { .. } => {}
DrawCommand::Noise { .. } => {}
DrawCommand::Dots { .. } => {}
```

**Step 3: Commit**

```bash
git add src/drawing/primitives.rs
git commit -m "Add Grating, Gabor, Noise, Dots draw command variants"
```

---

## Task 6: Parametric Vertex Shader + Grating Fragment Shader

**Files:**
- Create: `src/shaders/parametric.vert`
- Create: `src/shaders/grating.frag`

**Step 1: Write the parametric vertex shader**

Create `src/shaders/parametric.vert`:

```glsl
#version 460

layout(location = 0) in vec2 position;
layout(location = 1) in vec2 uv;

layout(push_constant) uniform PushConstants {
    vec2 viewport_size;
    vec4 rect;          // left, top, right, bottom in pixels
    float frequency;
    float orientation;
    float phase;
    float contrast;
    float background;
    float sigma;        // used by gabor, ignored by grating
    uint wave_type;     // 0=sine, 1=square
} pc;

layout(location = 0) out vec2 v_uv;

void main() {
    vec2 ndc = (position / pc.viewport_size) * 2.0 - 1.0;
    gl_Position = vec4(ndc, 0.0, 1.0);
    v_uv = uv;
}
```

**Step 2: Write the grating fragment shader**

Create `src/shaders/grating.frag`:

```glsl
#version 460

layout(push_constant) uniform PushConstants {
    vec2 viewport_size;
    vec4 rect;
    float frequency;
    float orientation;
    float phase;
    float contrast;
    float background;
    float sigma;
    uint wave_type;
} pc;

layout(location = 0) in vec2 v_uv;
layout(location = 0) out vec4 f_color;

void main() {
    // Map UV to pixel offset from rect center
    vec2 rect_size = vec2(pc.rect.z - pc.rect.x, pc.rect.w - pc.rect.y);
    vec2 pixel = (v_uv - 0.5) * rect_size;

    // Rotate to grating orientation
    float cos_ori = cos(pc.orientation);
    float sin_ori = sin(pc.orientation);
    float x_rot = pixel.x * cos_ori + pixel.y * sin_ori;

    // Carrier
    float carrier = sin(6.2831853 * pc.frequency * x_rot + pc.phase);

    // Square wave: threshold the sine
    if (pc.wave_type == 1u) {
        carrier = carrier >= 0.0 ? 1.0 : -1.0;
    }

    float luminance = pc.background + pc.contrast * 0.5 * carrier;
    luminance = clamp(luminance, 0.0, 1.0);

    f_color = vec4(luminance, luminance, luminance, 1.0);
}
```

**Step 3: Verify shaders compile**

Run: `cargo check`
Expected: compiles (shaders are compiled by vulkano-shaders macro when referenced from renderer.rs, which we'll do in Task 8)

Note: This step may not trigger shader compilation yet since no Rust code references them. That's fine — compilation will be verified in Task 8.

**Step 4: Commit**

```bash
git add src/shaders/parametric.vert src/shaders/grating.frag
git commit -m "Add parametric vertex shader and grating fragment shader"
```

---

## Task 7: Gabor Fragment Shader + Dot Shaders

**Files:**
- Create: `src/shaders/gabor.frag`
- Create: `src/shaders/dot.vert`
- Create: `src/shaders/dot.frag`

**Step 1: Write the Gabor fragment shader**

Create `src/shaders/gabor.frag`:

```glsl
#version 460

layout(push_constant) uniform PushConstants {
    vec2 viewport_size;
    vec4 rect;
    float frequency;
    float orientation;
    float phase;
    float contrast;
    float background;
    float sigma;
    uint wave_type;
} pc;

layout(location = 0) in vec2 v_uv;
layout(location = 0) out vec4 f_color;

void main() {
    vec2 rect_size = vec2(pc.rect.z - pc.rect.x, pc.rect.w - pc.rect.y);
    vec2 pixel = (v_uv - 0.5) * rect_size;

    float cos_ori = cos(pc.orientation);
    float sin_ori = sin(pc.orientation);
    float x_rot = pixel.x * cos_ori + pixel.y * sin_ori;
    float y_rot = -pixel.x * sin_ori + pixel.y * cos_ori;

    // Gaussian envelope
    float gaussian = exp(-(x_rot * x_rot + y_rot * y_rot) / (2.0 * pc.sigma * pc.sigma));

    // Carrier
    float carrier = sin(6.2831853 * pc.frequency * x_rot + pc.phase);
    if (pc.wave_type == 1u) {
        carrier = carrier >= 0.0 ? 1.0 : -1.0;
    }

    float luminance = pc.background + pc.contrast * 0.5 * gaussian * carrier;
    luminance = clamp(luminance, 0.0, 1.0);

    f_color = vec4(luminance, luminance, luminance, 1.0);
}
```

**Step 2: Write the dot vertex shader**

Create `src/shaders/dot.vert`:

```glsl
#version 460

// Per-vertex: unit quad [-1, 1]
layout(location = 0) in vec2 quad_pos;

// Per-instance: dot center in pixel coords
layout(location = 1) in vec2 instance_pos;

layout(push_constant) uniform PushConstants {
    vec2 viewport_size;
    float dot_radius;
    float _pad;
    vec4 dot_color;
} pc;

layout(location = 0) out vec2 v_local;  // [-1, 1] within dot quad
layout(location = 1) out vec4 v_color;

void main() {
    // Scale unit quad to dot radius and offset to dot center
    vec2 pixel_pos = instance_pos + quad_pos * pc.dot_radius;
    vec2 ndc = (pixel_pos / pc.viewport_size) * 2.0 - 1.0;
    gl_Position = vec4(ndc, 0.0, 1.0);
    v_local = quad_pos;
    v_color = pc.dot_color;
}
```

**Step 3: Write the dot fragment shader**

Create `src/shaders/dot.frag`:

```glsl
#version 460

layout(location = 0) in vec2 v_local;
layout(location = 1) in vec4 v_color;
layout(location = 0) out vec4 f_color;

void main() {
    // Circular dot with anti-aliased edge
    float dist = length(v_local);
    if (dist > 1.0) {
        discard;
    }
    // Smooth edge over last 5% of radius
    float alpha = 1.0 - smoothstep(0.95, 1.0, dist);
    f_color = vec4(v_color.rgb, v_color.a * alpha);
}
```

**Step 4: Commit**

```bash
git add src/shaders/gabor.frag src/shaders/dot.vert src/shaders/dot.frag
git commit -m "Add Gabor fragment shader, dot vertex/fragment shaders"
```

---

## Task 8: Renderer Pipeline Creation

**Files:**
- Modify: `src/drawing/renderer.rs` (add shader modules, pipeline fields, create methods)

This is the largest task. It adds the three new pipelines to the Renderer struct.

**Step 1: Add shader module declarations**

After the existing `mod textured_fs` block (line ~77), add:

```rust
mod parametric_vs {
    vulkano_shaders::shader! {
        ty: "vertex",
        path: "src/shaders/parametric.vert",
    }
}

mod grating_fs {
    vulkano_shaders::shader! {
        ty: "fragment",
        path: "src/shaders/grating.frag",
    }
}

mod gabor_fs {
    vulkano_shaders::shader! {
        ty: "fragment",
        path: "src/shaders/gabor.frag",
    }
}

mod dot_vs {
    vulkano_shaders::shader! {
        ty: "vertex",
        path: "src/shaders/dot.vert",
    }
}

mod dot_fs {
    vulkano_shaders::shader! {
        ty: "fragment",
        path: "src/shaders/dot.frag",
    }
}
```

**Step 2: Add pipeline fields to Renderer struct**

Add to the `Renderer` struct:

```rust
grating_pipeline: Arc<GraphicsPipeline>,
gabor_pipeline: Arc<GraphicsPipeline>,
dot_pipeline: Arc<GraphicsPipeline>,
```

**Step 3: Add pipeline creation methods**

Add `create_grating_pipeline`, `create_gabor_pipeline`, and `create_dot_pipeline` methods following the same pattern as `create_flat_color_pipeline` and `create_textured_pipeline`.

Key differences:
- **Grating/Gabor**: Use `parametric_vs` + respective fragment shader. Use `TexturedVertex` (position + UV). Same blend/rasterization as existing pipelines.
- **Dot**: Use `dot_vs` + `dot_fs`. Two vertex bindings: binding 0 = `Vertex2D`-like quad vertices (per-vertex), binding 1 = `DotInstance` (per-instance). Uses alpha blending for anti-aliased edges.

For grating pipeline (gabor is identical except uses `gabor_fs`):

```rust
fn create_grating_pipeline(
    device: &Arc<Device>,
    swapchain_format: Format,
) -> Result<Arc<GraphicsPipeline>, RendererError> {
    let vs = parametric_vs::load(device.clone())
        .map_err(|e| RendererError::ShaderLoadFailed(e.to_string()))?;
    let fs = grating_fs::load(device.clone())
        .map_err(|e| RendererError::ShaderLoadFailed(e.to_string()))?;

    let vs_entry = vs.entry_point("main").unwrap();
    let fs_entry = fs.entry_point("main").unwrap();

    let vertex_input_state = TexturedVertex::per_vertex()
        .definition(&vs_entry)
        .map_err(|e| RendererError::PipelineCreationFailed(e.to_string()))?;

    let stages = [
        PipelineShaderStageCreateInfo::new(vs_entry),
        PipelineShaderStageCreateInfo::new(fs_entry),
    ];

    let layout = PipelineLayout::new(
        device.clone(),
        PipelineDescriptorSetLayoutCreateInfo::from_stages(&stages)
            .into_pipeline_layout_create_info(device.clone())
            .map_err(|e| RendererError::PipelineCreationFailed(e.to_string()))?,
    )
    .map_err(|e| RendererError::PipelineCreationFailed(e.to_string()))?;

    GraphicsPipeline::new(
        device.clone(),
        None,
        GraphicsPipelineCreateInfo {
            stages: stages.into_iter().collect(),
            vertex_input_state: Some(vertex_input_state),
            input_assembly_state: Some(InputAssemblyState {
                topology: PrimitiveTopology::TriangleList,
                ..Default::default()
            }),
            viewport_state: Some(ViewportState::default()),
            rasterization_state: Some(RasterizationState::default()),
            multisample_state: Some(MultisampleState::default()),
            color_blend_state: Some(ColorBlendState::with_attachment_states(
                1,
                ColorBlendAttachmentState {
                    blend: Some(AttachmentBlend::alpha()),
                    ..Default::default()
                },
            )),
            dynamic_state: [DynamicState::Viewport].into_iter().collect(),
            subpass: Some(
                PipelineRenderingCreateInfo {
                    color_attachment_formats: vec![Some(swapchain_format)],
                    ..Default::default()
                }
                .into(),
            ),
            ..GraphicsPipelineCreateInfo::layout(layout)
        },
    )
    .map_err(|e| RendererError::PipelineCreationFailed(e.to_string()))
}
```

For the dot pipeline, the key difference is the vertex input state uses two bindings. This requires manual vertex input definition using vulkano's `VertexInputState` API rather than the derive macro approach. The per-vertex binding (0) is a unit quad `[-1,1]` and the per-instance binding (1) is `DotInstance`. Consult `vulkano::pipeline::graphics::vertex_input` for `VertexInputBindingDescription` and `VertexInputAttributeDescription` to build the custom vertex state. Use `InputRate::Instance` for binding 1.

**Step 4: Update `Renderer::new()`**

Create all three new pipelines and store them:

```rust
let grating_pipeline = Self::create_grating_pipeline(&device, swapchain_format)?;
let gabor_pipeline = Self::create_gabor_pipeline(&device, swapchain_format)?;
let dot_pipeline = Self::create_dot_pipeline(&device, swapchain_format)?;
```

**Step 5: Verify compilation**

Run: `cargo check`
Expected: compiles. If shader compilation errors occur, fix GLSL syntax. Common issues: push constant layout alignment (std430), missing precision qualifiers.

**Step 6: Commit**

```bash
git add src/drawing/renderer.rs
git commit -m "Add grating, gabor, and dot graphics pipeline creation"
```

---

## Task 9: Renderer Draw Recording for Gratings and Gabor

**Files:**
- Modify: `src/drawing/renderer.rs` (extend `render()` method)

**Step 1: Add grating/gabor recording to render()**

After the textured draws section in `render()`, add a new section that processes `DrawCommand::Grating` and `DrawCommand::Gabor` commands. For each:

1. Generate a textured quad (same `textured_quad_vertices()` function)
2. Create a vertex buffer from the quad
3. Bind the grating or gabor pipeline
4. Push constants with all parameters:

```rust
// Grating push constants
parametric_vs::PushConstants {
    viewport_size: [viewport_extent[0] as f32, viewport_extent[1] as f32],
    rect: [left, top, right, bottom],
    frequency: params.frequency,
    orientation: params.orientation,
    phase: params.phase,
    contrast: params.contrast,
    background: params.background,
    sigma: 0.0,  // unused for grating
    wave_type: match params.wave {
        WaveType::Sine => 0,
        WaveType::Square => 1,
    },
}
```

For Gabor, same but use `gabor_pipeline` and set `sigma` from `params.sigma`.

**Step 2: Verify compilation**

Run: `cargo check`
Expected: compiles

**Step 3: Commit**

```bash
git add src/drawing/renderer.rs
git commit -m "Add grating and gabor draw command recording in renderer"
```

---

## Task 10: Renderer Draw Recording for Noise and Dots

**Files:**
- Modify: `src/drawing/renderer.rs` (extend `render()` method for noise and dots)

**Step 1: Add noise recording**

Noise commands use the existing `textured_pipeline` since they're CPU-generated textures. They are already handled by the `DrawCommand::Noise { texture_id, ... }` variant, which works exactly like `DrawCommand::Texture`. Add a section that processes them the same way as texture draws.

**Step 2: Add dots recording**

For `DrawCommand::Dots`:

1. Generate a unit quad (6 vertices for two triangles covering `[-1, 1]`):
```rust
let quad_vertices = [
    Vertex2D { position: [-1.0, -1.0], color: [0.0; 4] },
    Vertex2D { position: [-1.0,  1.0], color: [0.0; 4] },
    Vertex2D { position: [ 1.0,  1.0], color: [0.0; 4] },
    Vertex2D { position: [-1.0, -1.0], color: [0.0; 4] },
    Vertex2D { position: [ 1.0,  1.0], color: [0.0; 4] },
    Vertex2D { position: [ 1.0, -1.0], color: [0.0; 4] },
];
```
Note: We use `Vertex2D` here but the dot vertex shader only reads `location = 0` (the position). The color field is unused but present to satisfy the vertex buffer layout. Alternatively, define a minimal `QuadVertex` with just position — use whichever approach works cleanly with vulkano's vertex binding system.

2. Create instance buffer from `positions` (Vec of `DotInstance`)
3. Bind `dot_pipeline`
4. Push constants: `viewport_size`, `dot_radius`, padding, `dot_color`
5. Bind both vertex buffers (binding 0 = quad, binding 1 = instances)
6. Draw instanced: `draw(6, positions.len() as u32, 0, 0)`

**Step 3: Verify compilation**

Run: `cargo check`
Expected: compiles

**Step 4: Commit**

```bash
git add src/drawing/renderer.rs
git commit -m "Add noise and instanced dot draw command recording"
```

---

## Task 11: RenderContext API Methods

**Files:**
- Modify: `src/core/context.rs` (add draw methods)
- Modify: `src/drawing/renderer.rs` (add `generate_noise_texture` helper)

**Step 1: Add `draw_grating` and `draw_gabor` to RenderContext**

In `src/core/context.rs`, after the existing `draw_texture` method:

```rust
/// Draw a sinusoidal or square-wave grating.
///
/// The grating fills the rectangle defined by (left, top, right, bottom)
/// in pixel coordinates. Parameters control spatial frequency, orientation,
/// phase, contrast, and waveform type.
pub fn draw_grating(
    &mut self,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    params: &GratingParams,
) {
    self.state.renderer.push(DrawCommand::Grating {
        left,
        top,
        right,
        bottom,
        params: params.clone(),
    });
}

/// Draw a Gabor patch (grating windowed by a Gaussian envelope).
///
/// Uses the existing `GaborParams` struct. The patch fills the specified
/// rectangle. Unlike `create_gabor()` which generates a CPU texture,
/// this computes the Gabor mathematically on the GPU each frame,
/// allowing real-time parameter animation.
pub fn draw_gabor_shader(
    &mut self,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    params: &GaborParams,
) {
    self.state.renderer.push(DrawCommand::Gabor {
        left,
        top,
        right,
        bottom,
        params: params.clone(),
    });
}
```

**Step 2: Add `draw_noise` to RenderContext**

```rust
/// Draw a noise pattern.
///
/// Generates a noise texture on CPU from the given parameters and
/// displays it in the specified rectangle. The texture is generated
/// fresh each call — for animated noise, change `params.seed` each frame.
///
/// For static noise displayed over multiple frames, prefer generating
/// once with `load_texture_rgba()` and drawing with `draw_texture()`.
pub fn draw_noise(
    &mut self,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    params: &NoiseParams,
) -> Result<(), VSEError> {
    let pixels = crate::drawing::noise::generate_noise(params);
    let handle = self.state.renderer.load_texture_rgba(
        params.width, params.height, &pixels
    )?;
    self.state.renderer.push(DrawCommand::Noise {
        left,
        top,
        right,
        bottom,
        texture_id: handle.id,
    });
    Ok(())
}
```

Note: This creates a texture per-frame. For the first iteration this is simple and correct. If profiling shows the upload is a bottleneck, we can optimize later with a texture cache or ring buffer. The texture is cleaned up when the renderer is done with the frame.

**Step 3: Add `draw_dots` to RenderContext**

```rust
/// Draw filled circular dots at the specified positions.
///
/// This is the rendering primitive for Random Dot Kinematograms.
/// Positions are in pixel coordinates. Each dot is rendered as a
/// filled circle with an anti-aliased edge.
///
/// The motion logic (coherence, direction, lifetime) is managed
/// by the caller — VSE just renders the dots efficiently.
pub fn draw_dots(
    &mut self,
    positions: &[(f32, f32)],
    radius: f32,
    color: Color,
) {
    if positions.is_empty() {
        return;
    }
    self.state.renderer.push(DrawCommand::Dots {
        positions: positions.iter().map(|&(x, y)| [x, y]).collect(),
        radius,
        color,
    });
}
```

**Step 4: Add imports**

Add `GratingParams`, `NoiseParams` to the imports in `context.rs`.

**Step 5: Verify compilation**

Run: `cargo check`
Expected: compiles

**Step 6: Commit**

```bash
git add src/core/context.rs src/drawing/renderer.rs
git commit -m "Add draw_grating, draw_gabor_shader, draw_noise, draw_dots to RenderContext"
```

---

## Task 12: Update Exports

**Files:**
- Modify: `src/drawing/mod.rs`
- Modify: `src/lib.rs`

**Step 1: Update drawing/mod.rs exports**

Ensure all new public types are exported:

```rust
pub use stimuli::{GratingParams, NoiseParams, NoiseType, WaveType};
pub use vertex::{DotInstance, TexturedVertex, Vertex2D};
```

**Step 2: Update lib.rs prelude**

Add to the prelude:

```rust
pub use crate::drawing::{GratingParams, NoiseParams, NoiseType, WaveType};
```

**Step 3: Run full test suite**

Run: `cargo test`
Expected: all tests pass

Run: `cargo clippy --all-targets`
Expected: no warnings

Run: `cargo fmt --check`
Expected: no formatting issues

**Step 4: Commit**

```bash
git add src/drawing/mod.rs src/lib.rs
git commit -m "Export new stimulus types from drawing module and prelude"
```

---

## Task 13: Example — Advanced Stimuli Demo

**Files:**
- Create: `examples/05_advanced_stimuli.rs`
- Modify: `Cargo.toml` (add example entry)

**Step 1: Write the example**

Create `examples/05_advanced_stimuli.rs`:

```rust
//! Advanced Stimuli Demo
//!
//! Demonstrates GPU gratings, Gabor patches, noise patterns,
//! and dot rendering (RDK primitive).
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 05_advanced_stimuli
//! ```

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().init();

    let context = VSEContext::builder()
        .with_window_size(1200, 800)
        .with_title("VSE - Advanced Stimuli")
        .with_clear_color(0.5, 0.5, 0.5, 1.0)
        .build()?;

    let mut frame: u64 = 0;

    // RDK dot state: 200 random dots
    let mut dot_positions: Vec<(f32, f32)> = Vec::new();
    let mut dots_initialized = false;

    context.run(move |vse| {
        let (w, h) = vse.window_size();
        let wf = w as f32;
        let hf = h as f32;

        // Initialize dots on first frame
        if !dots_initialized {
            // Simple grid initialization
            for i in 0..200 {
                let x = 900.0 + (i % 20) as f32 * 14.0;
                let y = 50.0 + (i / 20) as f32 * 35.0;
                dot_positions.push((x, y));
            }
            dots_initialized = true;
        }

        vse.clear()?;

        // Quadrant 1 (top-left): Grating with drifting phase
        let grating_params = GratingParams {
            frequency: 0.02,
            orientation: std::f32::consts::FRAC_PI_4,
            phase: frame as f32 * 0.05,
            contrast: 0.8,
            background: 0.5,
            wave: WaveType::Sine,
        };
        vse.draw_grating(20.0, 20.0, 280.0, 280.0, &grating_params);

        // Quadrant 2 (top-center): Gabor with rotating orientation
        let gabor_params = GaborParams {
            size: 256,
            frequency: 0.03,
            orientation: frame as f32 * 0.02,
            phase: 0.0,
            sigma: 40.0,
            contrast: 1.0,
            background: 0.5,
        };
        vse.draw_gabor_shader(320.0, 20.0, 580.0, 280.0, &gabor_params);

        // Quadrant 3 (bottom-left): Animated white noise
        let noise_params = NoiseParams {
            noise_type: NoiseType::White,
            seed: frame,
            width: 128,
            height: 128,
            contrast: 0.8,
            background: 0.5,
        };
        vse.draw_noise(20.0, 320.0, 280.0, 580.0, &noise_params)?;

        // Quadrant 4 (bottom-center): Binary noise
        let binary_params = NoiseParams {
            noise_type: NoiseType::Binary,
            seed: frame / 10,  // changes every 10 frames
            width: 64,
            height: 64,
            contrast: 1.0,
            background: 0.5,
        };
        vse.draw_noise(320.0, 320.0, 580.0, 580.0, &binary_params)?;

        // Right side: Dots (simple rightward drift)
        for pos in dot_positions.iter_mut() {
            pos.0 += 1.5;
            if pos.0 > 1180.0 {
                pos.0 = 620.0;
            }
        }
        vse.draw_dots(&dot_positions, 4.0, Color::WHITE);

        // Square wave grating for variety
        let square_grating = GratingParams {
            frequency: 0.015,
            orientation: 0.0,
            phase: 0.0,
            contrast: 1.0,
            background: 0.5,
            wave: WaveType::Square,
        };
        vse.draw_grating(20.0, 620.0, 280.0, 780.0, &square_grating);

        // Plaid: two overlapping gratings
        let plaid1 = GratingParams {
            frequency: 0.03,
            orientation: std::f32::consts::FRAC_PI_4,
            phase: frame as f32 * 0.03,
            contrast: 0.4,
            background: 0.25,
            wave: WaveType::Sine,
        };
        let plaid2 = GratingParams {
            frequency: 0.03,
            orientation: -std::f32::consts::FRAC_PI_4,
            phase: frame as f32 * 0.03,
            contrast: 0.4,
            background: 0.25,
            wave: WaveType::Sine,
        };
        vse.draw_grating(320.0, 620.0, 580.0, 780.0, &plaid1);
        vse.draw_grating(320.0, 620.0, 580.0, 780.0, &plaid2);

        frame += 1;
        vse.flip(None)?;
        Ok(())
    })?;

    Ok(())
}
```

**Step 2: Add example to Cargo.toml**

```toml
[[example]]
name = "05_advanced_stimuli"
path = "examples/05_advanced_stimuli.rs"
```

**Step 3: Verify it compiles**

Run: `cargo check --example 05_advanced_stimuli`
Expected: compiles

**Step 4: Commit**

```bash
git add examples/05_advanced_stimuli.rs Cargo.toml
git commit -m "Add advanced stimuli demo example"
```

---

## Task 14: Pipeline Documentation

**Files:**
- Create: `docs/guides/pipelines.md`

**Step 1: Write the pipeline guide**

Write `docs/guides/pipelines.md` covering:

1. **What is a GPU Pipeline?** — Analogy of a compiled recipe. Components: shaders (vertex + fragment), fixed-function state (blending, depth), vertex format, push constants. Cost model: expensive to create once (~10-50ms), free to bind per-frame (~nanoseconds). Why separate pipelines beat uber-shaders (no GPU branch divergence).

2. **How VSE Manages Pipelines** — The `Renderer` struct holds all pipelines. Created in `Renderer::new()` at startup. Each `draw_*()` call pushes a `DrawCommand` to a queue. On `flip()`, `render()` iterates the queue, binds the right pipeline for each command, sets push constants, and records a draw call. All happens within a single Vulkan command buffer.

3. **Built-in Pipelines** — Table: flat_color (rects/circles/lines), textured (image textures), grating (sine/square gratings), gabor (Gaussian-windowed gratings), dot (instanced circular dots). For each: what it renders, what parameters it takes, which shader files implement it.

4. **Push Constants** — What they are (small block of data sent to shaders per-draw-call), why they're fast (no buffer allocation, stored in command buffer), size limit (128 bytes guaranteed). How VSE uses them to pass stimulus parameters.

5. **Writing Your Own Pipeline** — Step-by-step:
   a. Write vertex + fragment shaders in `src/shaders/`
   b. Add shader module declarations in `renderer.rs` with `vulkano_shaders::shader!` macro
   c. Define push constant struct (auto-generated by vulkano-shaders from GLSL layout)
   d. Write `create_*_pipeline()` method — walk through the boilerplate: load shaders, define vertex input, create stages, create layout, create pipeline
   e. Add `DrawCommand` variant in `primitives.rs`
   f. Add recording logic in `render()`
   g. Add `draw_*()` method on `RenderContext`
   h. Include a complete worked example: a checkerboard stimulus pipeline

**Step 2: Commit**

```bash
git add docs/guides/pipelines.md
git commit -m "Add pipeline documentation guide for vision scientists"
```

---

## Task 15: Final Verification

**Step 1: Run full test suite**

Run: `cargo test`
Expected: all tests pass

**Step 2: Run clippy**

Run: `cargo clippy --all-targets`
Expected: no warnings

**Step 3: Run formatter**

Run: `cargo fmt --check`
Expected: no formatting issues

**Step 4: Run all examples compile**

Run: `cargo check --examples`
Expected: all examples compile

**Step 5: Manual visual test**

Run: `cargo run --release --example 05_advanced_stimuli`
Expected: Window shows gratings (drifting sine, static square), Gabor (rotating), animated white noise, binary noise, drifting dots, and a plaid pattern. All should render without artifacts.

**Step 6: Final commit (if any fixes needed)**

```bash
git add -A
git commit -m "Final polish for Phase 4+5 advanced stimuli"
```
