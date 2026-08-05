# Controlled 3D rendering with `vse-3d`

Base VSE owns lightweight 2D stimuli, timing, recording, compositing, and presentation. The `vse-3d` workspace crate owns controlled scientific 3D rendering. `vse-bevy` remains a separate option for naturalistic scenes and Bevy's asset and PBR systems.

The initial `vse-3d` release preserves the former mesh-normal renderer:

- static glTF 2.0 triangle meshes;
- indexed and generated-index primitives;
- glTF node transforms and repeated mesh instances;
- perspective cameras and model transforms;
- back-face culling and depth testing;
- geometric face-normal colors;
- resident model buffers and explicit unload;
- source hashes and model provenance.

Materials, lighting, animation, scene graphs, and runtime asset streaming remain outside this release.

## Renderer boundary

`vse-3d` creates a second Vulkan logical device on the same physical GPU. It renders into a ring of exportable color images and owns its depth images, model buffers, command buffers, and graphics pipelines. Base VSE imports the color ring through `vse-external-frame`, blits each completed 3D frame into its target, draws its 2D overlays, and presents.

This boundary keeps depth and future multipass work out of base VSE. VSE remains the only code that acquires swapchain images, submits presentation work, queries scanout feedback, and constructs `FlipInfo`.

The initial integration requires Linux Vulkan external-memory and external-semaphore file descriptors. Displayed sessions also require VSE's EXT present backend, which owns the cross-device semaphore waits in the displayed submit. Headless sessions wait through their separate offscreen submit. Ordinary base-VSE rendering remains available without those extensions.

## Migration from the former core API

Audit 7b removed `load_model`, `model_info`, `model_bounds`, `draw_model_normals`, and `unload_model` from `RenderContext`. It also removed `ModelHandle`, `ModelInfo`, `ModelError`, `Bounds3D`, `PerspectiveCamera`, and `Vertex3D` from base VSE exports. Import the corresponding model and camera types from `vse_3d` and route model operations through the owned `Vse3d` renderer.

`HostInfo.pipeline.builtin_pipelines` now lists the seven base 2D pipelines. Recordings that contain the legacy `MeshNormals` name remain readable because regeneration already treats this field as compatibility metadata rather than pipeline configuration.

## Setup and frame flow

Create and attach the renderer once after the VSE GPU exists:

```rust,ignore
use vision_stimulus_engine::prelude::*;
use vse_3d::{PerspectiveCamera, Vse3d, Vse3dConfig};

context.run_with_setup(
    |vse| {
        let mut renderer = Vse3d::register(vse, Vse3dConfig::default())?;
        let model = renderer.load_model("model.glb")?;
        Ok((renderer, model))
    },
    |vse, (renderer, model)| {
        renderer.draw_normals(*model, transform, &PerspectiveCamera::default())?;
        renderer.render_frame(vse)?;

        // Base-VSE draws are overlays above the completed 3D frame.
        vse.draw_text("fixation", 16.0, 16.0, 2.0, Color::WHITE);
        vse.flip(None)?;
        Ok(())
    },
)?;
```

`draw_normals` validates parameters and queues a small command. It performs no file I/O, pipeline creation, GPU allocation, or GPU wait. `render_frame` records and submits one complete 3D frame on the producer device, signals the selected ring slot, and queues that slot for the next VSE flip. The GPU semaphore wait occurs in VSE's submission. Displayed code does not wait for the producer on the CPU.

The initial API is frame-locked. Scene state `n` produces the external image consumed by VSE frame `n`. A slow 3D renderer can therefore cause a measured missed presentation rather than silently repeating a prior scene state.

`Vse3d` can live in the state returned by `run_buffered_with_state`. Set `Vse3dConfig::ring_len` to at least `BufferedConfig::depth + 2`; the default three-slot ring matches buffered depth one. Slot exhaustion returns an error rather than waiting or reallocating during frame production.

## Render ordering

A displayed or headless frame records these operations:

1. wait for the selected external slot on the GPU;
2. blit the completed `vse-3d` color image into the VSE target;
3. render base-VSE 2D commands in call order;
4. present the displayed target or copy the headless target to its readback buffer.

The 3D renderer clears its own color and depth targets. Base VSE loads the imported color before drawing overlays, so fixation marks, text, and photodiode patches remain visible above 3D content.

## Headless regeneration

`Vse3d::register` also works inside `HeadlessContext::run_headless_with_setup`. Headless VSE imports and waits on the same external ring, composites the same overlays, and reads back the combined frame. It stays in the offscreen submission path and reports `TimingSource::Offscreen`.

```rust,ignore
headless.run_headless_with_setup(
    |vse| {
        let mut renderer = Vse3d::register(vse, Vse3dConfig::default())?;
        let model = renderer.load_model("model.glb")?;
        Ok((renderer, model))
    },
    |frame| save(frame),
    |vse, (renderer, model)| {
        renderer.draw_normals(*model, transform, &camera)?;
        renderer.render_frame(vse)?;
        vse.flip(None)?;
        Ok(())
    },
)?;
```

## Coordinates and shading

`vse-3d` uses right-handed world coordinates, `+Y` up, cameras looking along local `-Z`, and Vulkan depth in `0..1`. `PerspectiveCamera` applies the Vulkan projection-Y correction. Source glTF front faces are counter-clockwise.

The fragment shader derives a world-space geometric normal and maps it to linear RGB:

```text
rgb = 0.5 * normal + 0.5
```

The loader preserves glTF units and node transforms. It does not normalize imported models. Use `ModelInfo::bounds` to construct an explicit fit transform when a demonstration should fill the viewport.

## Resource lifetime and resize

`Vse3d` owns models and GPU resources. `ModelHandle` is tagged with the renderer that issued it; a handle cannot resolve to a coincident numeric ID in another renderer. Removing a model invalidates that handle.

The external ring has a fixed session extent. `render_frame` returns an error if a displayed target changes size after registration. It does not stretch pixels or allocate a replacement ring during presentation. Dynamic ring replacement can be added later as an explicit between-trials operation.

## Metadata

`Vse3d::info()` returns a serializable `Vse3dInfo` containing:

- crate and GPU identity;
- color and depth formats;
- extent and ring length;
- active 3D pipeline names;
- loaded `ModelInfo` records, including source path and SHA-256.

Experiments should record this snapshot with their session metadata. Base `HostInfo` describes VSE's host, target, timing, and base pipelines; it does not contain a 3D-specific field.

## Performance and the future direct seam

The external boundary avoids a CPU copy, but VSE still performs one full-frame GPU blit into the presentable target. This costs memory bandwidth. Separate logical devices isolate complex renderer submissions from VSE's presentation queue and permit future queue-priority control.

A seven-process initialization probe at 1920×1080 on Intel Meteor Lake/ANV measured a median base-VSE initialization of 61.2 ms with a warm shader cache. Registering `vse-3d` added 51.3 ms. With `MESA_SHADER_CACHE_DISABLE=true`, the corresponding medians were 74.1 ms for base VSE and 54.9 ms additional for `vse-3d`. The selected depth format was `D32_SFLOAT` and the ring contained three images. Process RSS did not track GPU allocations consistently and is not reported as GPU memory.

At that extent, three RGBA8 color images contain 23.7 MiB of texels and three D32 depth images contain another 23.7 MiB, before Vulkan allocation granularity and driver metadata. Base VSE allocates neither set. The imported consumer images alias the producer allocations rather than duplicating their pixel storage.

Reproduce the process-level timing measurement with `crates/vse-3d/examples/init_probe.rs`. Build once, invoke the binary in fresh processes, and set `MESA_SHADER_CACHE_DISABLE=true` for cold-cache samples.

A later advanced direct-underlay API may let a registered renderer record into VSE's acquired target before the 2D overlay pass. That API must preserve VSE's exclusive ownership of acquisition, submission, timing, and presentation. It will remain separate from `StimulusPipeline`, which records only inside the existing color-only 2D pass.

The direct seam should not ship until an end-to-end benchmark compares it with external frames at the target resolutions and refresh rates. The comparison must include GPU time, missed presentations, throughput, and frame latency.

## Example

Prepare the models described in `assets/3d/README.md`, then run:

```bash
cargo run --release -p vse-3d --example mesh_normals
```
