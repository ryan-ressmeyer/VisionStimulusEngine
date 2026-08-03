# GPU Pipelines in VSE

## What is a GPU pipeline?

A GPU pipeline is a compiled recipe that turns vertex data into pixels. It consists of:

- **Shaders**: programs that run on the GPU. A *vertex shader* positions geometry in screen space; a *fragment shader* computes the color of each pixel.
- **Fixed-function state**: configuration for blending (how overlapping things combine), rasterization (how triangles become pixels), and multisampling.
- **Vertex format**: the layout of per-vertex data (position, color, UV coordinates).
- **Push constants**: a small block of parameters sent per draw call (viewport size, stimulus parameters).

**Cost model**: creating a pipeline is expensive (~10-50 ms, involves shader compilation). Binding a pipeline per frame is nearly free (~nanoseconds). VSE builds all its pipelines once at startup, in `Renderer::new()`.

## How VSE renders a frame

1. Your code calls `draw_*()` methods on `RenderContext` (`draw_rect()`, `draw_grating()`, `draw_dots()`, ...). Each pushes a command onto a per-frame queue.
2. `flip()` records the queued commands into one Vulkan command buffer and submits it.
3. Native 3D draws (`draw_model_normals`) record first, in a depth pass. The 2D draws then record **in the order you called them** (see [Draw order and batching](#draw-order-and-batching)).
4. Consecutive flat-color draws (rectangles, circles, lines, arcs, text) coalesce into a single batched draw; every other primitive records on its own.

## Built-in pipelines

VSE ships eight built-in pipelines, identified by the `BuiltinPipeline` enum:

| `BuiltinPipeline` key | Draws | Shaders |
|---|---|---|
| `FlatColor` | Rectangles, circles, lines, arcs, text | `flat_color.vert/.frag` |
| `Textured` | Image and noise textures | `textured.vert/.frag` |
| `Grating` | Sinusoidal / square-wave gratings | `parametric.vert`, `grating.frag` |
| `Gabor` | Gaussian-windowed gratings | `parametric.vert`, `gabor.frag` |
| `AdditiveGabor` / `SubtractiveGabor` | The two-pass additive-Gabor accumulation (`draw_gabor_additive`) | `parametric.vert`, `gabor.frag` |
| `Dot` | Instanced circular dots (RDK) | `dot.vert/.frag` |
| `MeshNormals` | Native 3D geometric-normal meshes | `mesh_normals.vert/.frag` |

## Choosing which built-ins to build

By default VSE builds all eight. To build only the ones an experiment uses, pass a `PipelineSuite` to the builder:

```rust
use vision_stimulus_engine::prelude::*;

// Only flat shapes plus dots — skips compiling the grating/Gabor/texture pipelines.
let context = VSEContext::builder()
    .with_pipelines(PipelineSuite::minimal().with(BuiltinPipeline::Dot))
    .build()?;
```

`PipelineSuite::default()` selects all eight, `minimal()` selects `FlatColor` alone, and `empty()` selects none. Add or remove keys with `.with(key)` / `.without(key)`, and query with `.contains(key)`.

A `draw_*()` call whose pipeline was not built is skipped at render time, with a one-time warning naming the missing pipeline. Loading a texture requires `Textured` in the suite.

## Extending VSE with your own rendering

Two entry points let you render with pipelines VSE does not ship, at different levels of structure.

### Registering a stimulus pipeline (the structured path)

Implement `StimulusPipeline` to teach VSE a new stimulus. You build your own `GraphicsPipeline` once in `build`, and record draws for your parameters in `record`:

```rust
use vision_stimulus_engine::prelude::*;

struct CheckerParams { size: f32 }

struct CheckerPipeline { pipeline: Option<Arc<GraphicsPipeline>> }

impl StimulusPipeline for CheckerPipeline {
    type Command = CheckerParams;

    fn build(&mut self, cx: &PipelineBuildCtx) -> Result<(), PipelineError> {
        // Build a GraphicsPipeline on cx.device(), with a color format
        // matching cx.color_format(). Store it in self.
        self.pipeline = Some(build_checker(cx).map_err(|e| PipelineError::Build(e.to_string()))?);
        Ok(())
    }

    fn record(
        &self,
        recorder: &mut FrameRecorder,
        cx: &RecordCtx,
        commands: &[Self::Command],
    ) -> Result<(), PipelineError> {
        // Bind your pipeline, push constants from `commands`, draw.
        // cx.viewport_extent() and cx.memory_allocator() are available.
        Ok(())
    }
}
```

Register the pipeline once, then enqueue draws each frame:

```rust
let checker = vse.register_pipeline(CheckerPipeline { pipeline: None })?; // RegisteredPipeline<CheckerParams>
vse.draw_with(checker, CheckerParams { size: 20.0 });
```

`register_pipeline` calls your `build` immediately, so call it at setup or between trials, never on the presentation path. The returned handle is `Copy`; its type parameter ties it to your `Command`, so `draw_with` accepts only the matching parameters. Each `draw_with` records in call order, interleaved with the built-in draws around it.

A full working example is `examples/23_registered_pipeline.rs`.

### The raw record hook

When you want to record straight into VSE's frame without the trait, `draw_custom` hands your closure the same raw `FrameRecorder`:

```rust
vse.draw_custom(move |recorder: &mut FrameRecorder, ctx: &CustomFrameContext| {
    // Bind a pipeline you built at setup, push constants, draw.
    // ctx.viewport_extent gives the framebuffer size.
});
```

The closure runs inside VSE's active 2D pass with the viewport already set, compositing in call order. It must not begin or end the render pass or transition the target image. Build pipelines and buffers at setup using `vse.device()`, `vse.swapchain().format()`, and `vse.memory_allocator()`. A full working example is `examples/22_custom_pipeline.rs`.

Both hooks expose vulkano's `AutoCommandBufferBuilder` (aliased as `FrameRecorder`) directly, so your code depends on the same vulkano version as VSE.

## Draw order and batching

VSE composites 2D draws in **call order**: a shape drawn after a texture lands on top of it. Consecutive draws that use the same pipeline coalesce into one draw call. In particular, a run of consecutive flat-color draws (including text, which expands to one rectangle per lit glyph pixel) records as a single batched draw.

Interleaving pipelines breaks these runs. Drawing a rectangle, then a texture, then another rectangle records two separate flat batches instead of one. The dominant cost is the lost coalescing, not the pipeline bind itself, which is modest at typical stimulus counts. For throughput, group draws that share a pipeline. Batching and interleaving pull in opposite directions: group for speed, interleave for layering.

Native 3D (`draw_model_normals`) always renders before all 2D draws, in its own depth pass.

## Push constants

Push constants are the fastest way to send small amounts of data to shaders. Unlike uniform buffers, they need no GPU memory allocation; the data is embedded directly in the command buffer.

- **Size limit**: 128 bytes guaranteed by the Vulkan spec (most GPUs support more).
- **In GLSL**: `layout(push_constant) uniform PushConstants { ... }`.
- **In Rust**: `vulkano-shaders` generates a matching `PushConstants` struct from the GLSL layout.

## Shaders

The `vulkano_shaders::shader!` macro compiles GLSL to SPIR-V at build time and generates the matching `PushConstants` struct. It takes either a file path or inline source:

```rust
mod checker_fs {
    vulkano_shaders::shader! {
        ty: "fragment",
        path: "src/shaders/checker.frag",   // or: src: "#version 460 ..."
    }
}
```

Because it runs at build time, your experiment crate compiles its own shaders the same way VSE does. To load shaders VSE did not compile into the binary, build a `ShaderModule` from SPIR-V bytes at runtime and supply your own `PushConstants` type.
