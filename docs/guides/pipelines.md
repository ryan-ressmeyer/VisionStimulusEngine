# GPU Pipelines in VSE

## What is a GPU pipeline?

A GPU pipeline is a compiled recipe that turns vertex data into pixels. It consists of:

- **Shaders**: programs that run on the GPU. A *vertex shader* positions geometry in screen space; a *fragment shader* computes the color of each pixel.
- **Fixed-function state**: configuration for blending (how overlapping things combine), rasterization (how triangles become pixels), and multisampling.
- **Vertex format**: the layout of per-vertex data (position, color, UV coordinates).
- **Push constants**: a small block of parameters sent per draw call (viewport size, stimulus parameters).

**Cost model**: creating a pipeline is expensive (~10-50 ms, involves shader compilation). Binding a pipeline per frame is nearly free (~nanoseconds). VSE builds the pipelines you select once at startup, in `Renderer::new()` — see [Choosing which built-ins to build](#choosing-which-builtins-to-build).

## How VSE renders a frame

1. Your code calls `draw_*()` methods on `RenderContext` (`draw_rect()`, `draw_grating()`, `draw_dots()`, ...). Each pushes a command onto a per-frame queue.
2. `flip()` records the queued commands into one Vulkan command buffer and submits it.
3. Native 3D draws (`draw_model_normals`) record first, in a depth pass. The 2D draws then record **in the order you called them** (see [Draw order and batching](#draw-order-and-batching)).
4. Runs of adjacent draws that share a pipeline coalesce (see below); everything else records on its own.

Per-frame vertex data is suballocated from a pooled arena rather than a fresh Vulkan buffer per draw, so a steady-state frame creates no buffers at all. Arenas return to the pool once the GPU finishes with them.

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

`AdditiveGabor` and `SubtractiveGabor` must be selected together or not at all. They are the positive-add and negative-subtract passes of one draw, because a normalized swapchain format cannot carry signed modulation through a single pass. Selecting one alone would render half the signed modulation, so `build()` rejects it with `SuiteError::UnpairedGaborPass` before a session can start.

### What happens when a pipeline is missing

Two different failures, because they are knowable at two different times:

- **Setup-time calls fail loudly.** `load_image()`, `load_texture_rgba()`, and `draw_noise()` all create a texture, which needs the `Textured` pipeline's descriptor layout. Without it they return an error.
- **Draw-time calls are skipped and counted.** A queued `draw_*()` whose pipeline was not built renders nothing. VSE logs this at ERROR the first time it sees each kind, then stays quiet so a 60 Hz loop cannot flood the log.

Because the log goes quiet, check the count rather than the log:

```rust
if vse.skipped_draw_count() > 0 {
    for (pipeline, count) in vse.skipped_draws() {
        eprintln!("{pipeline:?}: {count} draws rendered nothing");
    }
}
```

A non-zero count means frames were presented without a stimulus the experiment asked for. Treat the affected data as suspect. The selected pipeline set is also recorded in `HostInfo.pipeline.builtin_pipelines`, so a session's configuration is recoverable from its metadata after the fact.

## Extending VSE with your own rendering

Two entry points let you render with pipelines VSE does not ship, at different levels of structure.

### Registering a stimulus pipeline (the structured path)

Implement `StimulusPipeline` to teach VSE a new stimulus. `build` creates your `GraphicsPipeline` once and returns it as the pipeline's `Resources`; `record` draws with it:

```rust
use vision_stimulus_engine::prelude::*;

struct CheckerParams { size: f32 }

/// Configuration, if any. GPU state does not live here.
struct CheckerPipeline;

impl StimulusPipeline for CheckerPipeline {
    type Command = CheckerParams;
    type Resources = Arc<GraphicsPipeline>;

    fn build(&self, cx: &PipelineBuildCtx) -> Result<Self::Resources, PipelineError> {
        // Build on cx.device(), with a color format matching cx.color_format().
        build_checker(cx).map_err(PipelineError::build)
    }

    fn record(
        &mut self,
        gpu: &mut Self::Resources,
        recorder: &mut FrameRecorder,
        cx: &RecordCtx,
        commands: &[Self::Command],
    ) -> Result<(), PipelineError> {
        // Bind `gpu`, push constants from `commands`, draw.
        // cx.device(), cx.viewport_extent(), cx.memory_allocator() are available.
        Ok(())
    }
}
```

Returning `Resources` from `build` rather than storing them in `self` is what keeps `record` free of an `Option` and an unwrap for a state that cannot happen. `record` takes `&mut self` and `&mut Self::Resources`, so a pipeline can cache a staging buffer or a memoized descriptor set across frames without interior mutability.

Wrap a failing vulkano call with `PipelineError::build` or `PipelineError::record` rather than stringifying it, which keeps the underlying error reachable through `Error::source`.

Register in the setup closure, then enqueue draws each frame:

```rust
context.run_with_setup(
    |vse| vse.register_pipeline(CheckerPipeline),
    move |vse, &mut checker| {
        vse.draw_with(checker, CheckerParams { size: 20.0 })?;
        vse.flip(None)?;
        Ok(())
    },
)?;
```

`register_pipeline` runs your `build` immediately, and compiling a pipeline costs tens of milliseconds. `run_with_setup` invokes its setup closure once after the GPU exists but before frame 0, which keeps that cost off the presentation path. Registering before the run loop is not possible at all: choosing a presentation-capable Vulkan device requires a surface, and a surface requires a window.

The returned handle is `Copy`, and its type parameter ties it to your `Command`, so `draw_with` accepts only matching parameters. A handle is also tagged with the context that issued it; using one against a different `VSEContext` returns `PipelineError::ForeignHandle` instead of resolving to whatever pipeline holds that slot.

Call `unregister_pipeline(handle)` when re-registering between trials. Without it, each registration leaks its `GraphicsPipeline` for the rest of the session.

A full working example is `examples/22_registered_pipeline.rs`.

### The raw record hook

When you want to record straight into VSE's frame without the trait, `draw_custom` hands your closure the same raw `FrameRecorder`:

```rust
vse.draw_custom(move |recorder: &mut FrameRecorder, cx: &RecordCtx| {
    // Bind a pipeline you built at setup, push constants, draw.
    // cx.viewport_extent() gives the framebuffer size.
});
```

The closure runs inside VSE's active 2D pass with the viewport already set, compositing at the point in call order where you queued it. It must not begin or end the render pass or transition the target image. Build pipelines and buffers in `run_with_setup`'s setup closure using `vse.device()`, `vse.swapchain().format()`, and `vse.memory_allocator()`. A full working example is `examples/21_custom_pipeline.rs`.

`draw_custom` and `StimulusPipeline::record` receive the same `RecordCtx`, so the two tiers see an identical view of the frame.

Both hooks expose vulkano's `AutoCommandBufferBuilder` (aliased as `FrameRecorder`) directly, so your code depends on the same vulkano version as VSE.

## Draw order and batching

VSE composites 2D draws in **call order**. A shape drawn after a texture lands on top of it.

Two kinds of adjacent draws merge into a single draw call:

- **Flat-color draws** (rectangles, circles, lines, arcs, and text, which expands to one rectangle per lit glyph pixel). A run of them becomes one vertex buffer and one draw.
- **Registered draws sharing one pipeline.** A run of N consecutive `draw_with` calls against the same handle reaches your `record` as one slice of N commands, which you can answer with a single instanced draw.

Everything else records on its own. Two consecutive `draw_texture` calls do *not* merge, and neither do two gratings; each carries its own descriptor set or push constants, which a shared draw call cannot express. They still share the vertex arena, so the per-draw cost is a bind and a draw, not an allocation.

Interleaving splits runs. A rectangle, then a texture, then another rectangle records two flat batches instead of one. Grouping draws that share a pipeline is faster; interleaving them is how you control layering. When the two conflict, layering wins — the planner never reorders your draws to improve batching.

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
