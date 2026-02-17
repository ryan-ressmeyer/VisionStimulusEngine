# GPU Pipelines in VSE

## What is a GPU Pipeline?

A GPU pipeline is like a compiled recipe that tells the graphics card exactly how to turn vertex data into pixels on screen. It consists of:

- **Shaders**: Programs that run on the GPU. A *vertex shader* positions geometry in screen space, and a *fragment shader* computes the color of each pixel.
- **Fixed-function state**: Configuration for blending (how overlapping things combine), rasterization (how triangles become pixels), and multisampling.
- **Vertex format**: The layout of per-vertex data (position, color, UV coordinates, etc.).
- **Push constants**: A small block of parameters sent per-draw-call (viewport size, stimulus parameters).

**Cost model**: Creating a pipeline is expensive (~10-50ms, involves shader compilation). Binding a pipeline per-frame is nearly free (~nanoseconds). VSE creates all pipelines once at startup in `Renderer::new()`.

## How VSE Manages Pipelines

The `Renderer` struct (in `src/drawing/renderer.rs`) owns all pipelines. The rendering flow each frame:

1. Your code calls `draw_*()` methods on `RenderContext` (e.g., `draw_grating()`, `draw_dots()`).
2. Each call pushes a `DrawCommand` variant onto an internal queue.
3. When you call `flip()`, the `render()` method iterates the queue:
   - Groups flat-color draws into a single batch.
   - Records each textured/parametric/dot draw individually.
   - Binds the appropriate pipeline, sets push constants, and issues a draw call.
4. All commands are recorded into a single Vulkan command buffer and submitted to the GPU.

## Built-in Pipelines

| Pipeline | Draws | Parameters | Shaders |
|----------|-------|------------|---------|
| **flat_color** | Rectangles, circles, lines | viewport_size | `flat_color.vert`, `flat_color.frag` |
| **textured** | Image textures, noise textures | viewport_size, sampler | `textured.vert`, `textured.frag` |
| **grating** | Sinusoidal/square-wave gratings | frequency, orientation, phase, contrast, background, wave_type | `parametric.vert`, `grating.frag` |
| **gabor** | Gaussian-windowed gratings | frequency, orientation, phase, contrast, background, sigma | `parametric.vert`, `gabor.frag` |
| **dot** | Instanced circular dots (RDK) | viewport_size, dot_radius, dot_color | `dot.vert`, `dot.frag` |

## Push Constants

Push constants are the fastest way to send small amounts of data to shaders. Unlike uniform buffers, they require no GPU memory allocation — the data is embedded directly in the command buffer.

- **Size limit**: 128 bytes guaranteed by the Vulkan spec (most GPUs support more).
- **Usage**: VSE uses push constants for all per-draw-call parameters (viewport size, grating frequency, dot color, etc.).
- **In GLSL**: Declared as `layout(push_constant) uniform PushConstants { ... }`.
- **In Rust**: vulkano-shaders auto-generates a matching `PushConstants` struct from the GLSL layout.

## Writing Your Own Pipeline

To add a custom stimulus pipeline:

### 1. Write shaders

Create vertex and fragment shaders in `src/shaders/`. Example for a checkerboard:

```glsl
// src/shaders/checker.frag
#version 460

layout(push_constant) uniform PushConstants {
    vec2 viewport_size;
    vec4 rect;
    float check_size;  // pixels per square
} pc;

layout(location = 0) in vec2 v_uv;
layout(location = 0) out vec4 f_color;

void main() {
    vec2 rect_size = vec2(pc.rect.z - pc.rect.x, pc.rect.w - pc.rect.y);
    vec2 pixel = v_uv * rect_size;
    float checker = mod(floor(pixel.x / pc.check_size) + floor(pixel.y / pc.check_size), 2.0);
    f_color = vec4(vec3(checker), 1.0);
}
```

### 2. Add shader module declarations

In `src/drawing/renderer.rs`:

```rust
mod checker_fs {
    vulkano_shaders::shader! {
        ty: "fragment",
        path: "src/shaders/checker.frag",
    }
}
```

The `vulkano_shaders::shader!` macro compiles GLSL at build time and generates Rust types including a `PushConstants` struct matching your GLSL layout.

### 3. Write `create_*_pipeline()` method

Follow the pattern of existing methods like `create_grating_pipeline()`:

1. Load vertex and fragment shader modules
2. Get entry points
3. Define vertex input state (use `TexturedVertex::per_vertex()` for standard quad-based stimuli)
4. Create pipeline stages and layout
5. Create the `GraphicsPipeline` with standard settings

### 4. Add `DrawCommand` variant

In `src/drawing/primitives.rs`:

```rust
Checker {
    left: f32, top: f32, right: f32, bottom: f32,
    check_size: f32,
},
```

Add an empty match arm in `generate_flat_color_vertices()` for the new variant.

### 5. Add recording logic in `render()`

Extract matching commands, create vertex buffers, bind pipeline, push constants, draw.

### 6. Add `draw_*()` method on `RenderContext`

In `src/core/context.rs`, add the public API method that pushes the command.

### 7. Add to prelude

Export any new parameter types from `src/drawing/mod.rs` and `src/lib.rs` prelude.
