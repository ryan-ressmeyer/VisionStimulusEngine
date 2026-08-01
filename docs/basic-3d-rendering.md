# Basic native 3D rendering

**Status:** Approved for incremental implementation

**Scope:** A small deterministic 3D layer in VSE's native Vulkan renderer. The first deliverable loads static triangle meshes and colors each visible triangle from its geometric face normal. Bevy remains the path for materials, lighting, animation, and naturalistic scenes.

## Decision

Add native indexed-mesh rendering to the core VSE renderer. Use glTF 2.0 as the runtime asset format, `glam` for math, one depth attachment per swapchain image, and a dedicated normal-visualization pipeline. The first example displays one model at a time and switches models with the left and right arrow keys.

This is the standard shape of a small Vulkan mesh renderer:

1. Decode model data once.
2. Upload vertex and index buffers once.
3. Transform vertices with model, view, and projection matrices.
4. depth-test indexed triangle draws.
5. calculate one geometric normal per rasterized triangle and map its components to RGB.

The renderer does not need a scene graph, ECS, PBR materials, lights, or Bevy for this feature.

## Relationship to the Bevy path

VSE already separates scene rendering from presentation. `vse-bevy` renders a complete 3D frame on a separate Vulkan device, then hands it to VSE through the external-frame ring. Native 3D serves a different use case:

| Native VSE 3D | `vse-bevy` |
|---|---|
| controlled static geometry | complete scenes |
| small audited shader set | Bevy PBR and asset system |
| frame-index-driven transforms | frame-index-driven Bevy systems |
| no renderer handoff | external-image handoff |
| best for parametric and diagnostic stimuli | best for naturalistic stimuli |

Both paths retain VSE as the sole present authority. Native 3D records commands in the same command buffer as the existing 2D renderer, so it introduces no new queue or synchronization boundary.

## Scope

### Initial release

- static triangle meshes
- `.glb` and self-contained or local-resource `.gltf` files
- indexed and non-indexed glTF primitives
- glTF node transforms and repeated mesh instances
- perspective camera
- model transforms
- back-face culling
- depth testing
- flat geometric face-normal coloring
- immutable GPU buffers
- explicit model unload
- 2D overlays rendered after 3D
- deterministic, frame-index-driven animation in the example

### Non-goals

- materials and textures
- lighting and shadows
- skeletal or morph animation
- runtime asset streaming
- physics
- scene editing or ECS
- occlusion culling or LOD selection
- OBJ, PLY, STL, FBX, or USD loading at runtime
- stereoscopic cameras or VR presentation
- replacing `vse-bevy`

Authoring formats such as PLY and STL should be converted to GLB before an experiment. This keeps parsing and coordinate conversion outside the presentation process.

## Normal coloring

The default visualization encodes a world-space unit normal `n` as linear RGB:

```text
rgb = 0.5 * n + 0.5
```

The mapping is therefore:

| Normal | Linear RGB |
|---|---|
| +X | (1.0, 0.5, 0.5) |
| -X | (0.0, 0.5, 0.5) |
| +Y | (0.5, 1.0, 0.5) |
| -Y | (0.5, 0.0, 0.5) |
| +Z | (0.5, 0.5, 1.0) |
| -Z | (0.5, 0.5, 0.0) |

Colors are linear values, consistent with VSE's existing `Color` contract. An sRGB swapchain performs the final transfer encoding.

### Recommended shader implementation

Keep the source mesh indexed and pass world-space position from the vertex shader to the fragment shader. The fragment shader calculates the geometric normal from screen-space derivatives:

```glsl
vec3 dx = dFdx(world_position);
vec3 dy = dFdy(world_position);
vec3 normal = normalize(cross(dy, dx));
vec3 color = normal * 0.5 + 0.5;
```

A triangle's interpolated world position is planar, so its derivatives produce a constant geometric normal across the triangle. This has three useful properties:

- shared indexed vertices remain shared;
- source vertex normals are irrelevant;
- non-uniform model scaling still produces the geometric normal of the transformed surface.

Vulkan fragment coordinates increase downward. With VSE's positive-height viewport and projection-Y correction, `cross(dy, dx)` preserves a source counter-clockwise triangle's world-space geometric normal. A known-winding test triangle fixes this convention. Back faces are culled in the initial release. If two-sided drawing is added later, `gl_FrontFacing` must define whether back-face colors are inverted.

This derivative method is preferable to expanding every indexed triangle into three independent vertices. Expansion is a valid fallback, but it roughly triples position storage on smooth meshes. It is especially wasteful for dense scanned meshes. A geometry shader should not be introduced for this task; it adds a pipeline stage for work the fragment shader can perform directly.

### Future modes

A later API may add:

```rust
pub enum NormalColorSpace {
    Object,
    World,
    View,
}

pub enum NormalShading {
    Flat,
    Smooth,
}
```

Smooth mode would use imported or generated vertex normals and the inverse-transpose normal matrix. It is not part of the initial implementation.

## Coordinate conventions

VSE should adopt and document one 3D convention rather than converting silently at draw time.

- right-handed world coordinates
- `+Y` is up
- cameras look along local `-Z`
- distances are meters
- glTF node transforms are preserved
- Vulkan depth is `0..1`
- source front faces are counter-clockwise before the Vulkan projection-Y correction

Use `glam::Mat4::look_at_rh` and `glam::Mat4::perspective_rh`. The camera helper applies the Vulkan Y correction in one place. Because that correction reverses projected winding with VSE's positive-height viewport, the mesh pipeline must use the matching Vulkan front-face state. A unit test with a known counter-clockwise triangle fixes this convention and prevents later refactors from silently reversing culling or normal colors.

Do not normalize an imported model automatically. glTF defines meters, and silent normalization would destroy spatial calibration. Expose model bounds so examples can apply an explicit fit transform.

## Public API

The API should distinguish a loaded model from one GPU mesh because a glTF model may contain several mesh primitives and node instances.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModelHandle {
    id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds3D {
    pub min: glam::Vec3,
    pub max: glam::Vec3,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PerspectiveCamera {
    pub eye: glam::Vec3,
    pub target: glam::Vec3,
    pub up: glam::Vec3,
    pub vertical_fov_radians: f32,
    pub near: f32,
    pub far: f32,
}

impl PerspectiveCamera {
    pub fn view_projection(
        &self,
        aspect_ratio: f32,
    ) -> Result<glam::Mat4, ModelError>;
}

impl RenderContext<'_> {
    pub fn load_model(
        &mut self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<ModelHandle, VSEError>;

    pub fn model_info(&self, model: ModelHandle) -> Result<&ModelInfo, VSEError>;

    pub fn model_bounds(&self, model: ModelHandle) -> Result<Bounds3D, VSEError>;

    pub fn draw_model_normals(
        &mut self,
        model: ModelHandle,
        model_transform: glam::Mat4,
        camera: &PerspectiveCamera,
    ) -> Result<(), VSEError>;

    pub fn unload_model(&mut self, model: ModelHandle);
}
```

`draw_model_normals` queues a lightweight command. It must not allocate GPU memory, read files, or wait for the GPU. The renderer calculates the camera aspect ratio from the current swapchain extent.

A future general material API can add `draw_model` without changing the handle or loader contract. The normal-specific method makes the initial capability explicit instead of inventing a premature material abstraction.

### Errors

Add specific errors for:

- model file read failure
- malformed or unsupported glTF
- missing `POSITION`
- unsupported primitive topology
- index outside the position array
- non-finite position or transform
- empty model
- invalid camera planes or field of view
- unknown model handle
- GPU buffer upload failure
- depth-image creation failure

Unsupported features must fail or be reported explicitly. They must not produce a plausible but incorrect static mesh.

## Internal representation

```rust
struct ModelResources {
    primitives: Vec<MeshPrimitive>,
    instances: Vec<ModelInstance>,
    bounds: Bounds3D,
}

struct MeshPrimitive {
    vertex_buffer: Subbuffer<[Vertex3D]>,
    index_buffer: Subbuffer<[u32]>,
    index_count: u32,
}

struct ModelInstance {
    primitive_index: usize,
    local_transform: glam::Mat4,
}

#[repr(C)]
struct Vertex3D {
    position: [f32; 3],
}

enum DrawCommand3D {
    ModelNormals {
        model_id: u64,
        model_transform: glam::Mat4,
        view_projection: glam::Mat4,
    },
}
```

The loader converts all indices to `u32`. A primitive without indices receives a generated sequential index buffer. It accepts triangle-list primitives and rejects points, lines, strips, and fans in the initial release. Conversion tooling must triangulate source assets.

The glTF default scene is traversed in stable node order. Mesh buffers are uploaded once and reused when multiple nodes instance the same mesh. Node world transforms remain separate instance records. Bounds include every instance transform.

Materials, textures, imported normals, UVs, cameras, and lights are ignored because they cannot affect this pipeline. Skins and morph targets are rejected rather than rendered in a misleading bind state.

## GPU pipeline

### Vertex shader

Inputs:

- location 0: object-space position

Per-draw data:

- model matrix
- view-projection matrix

Outputs:

- `gl_Position = view_projection * model * vec4(position, 1)`
- world-space position for fragment derivatives

Two `mat4` values occupy 128 bytes, the Vulkan minimum guaranteed push-constant capacity. VSE may use exactly that push-constant block for the first implementation. If later draw parameters exceed it, move per-draw matrices to an aligned dynamic uniform buffer rather than increasing the requirement silently.

### Fragment shader

The fragment shader calculates and normalizes the geometric face normal, maps it to linear RGB, and writes alpha 1.0. It performs no lighting, texturing, blending, or temporal operations.

### Raster and depth state

- topology: triangle list
- polygon mode: fill
- back-face culling: enabled
- front face: matched to the documented projection convention
- blending: disabled
- depth test: enabled
- depth writes: enabled
- depth compare: `Less`
- depth clear: 1.0
- MSAA: off

Start with `D32_SFLOAT` when supported as a depth-stencil attachment and fall back to `D16_UNORM`. Stencil is unnecessary.

## Depth attachment lifetime

Allocate one depth image per swapchain image. A single shared depth image is unsafe when buffered presentation leaves several command buffers in flight.

Each depth image:

- matches the swapchain extent;
- uses optimal tiling;
- has `DEPTH_STENCIL_ATTACHMENT` usage;
- has one mip level and one layer;
- is cleared at the start of every 3D pass;
- has `StoreOp::DontCare` because no later pass samples it.

Swapchain recreation must recreate the depth-image set before another 3D frame is recorded. This adds a renderer resize/recreation hook to `VSEState::recreate_swapchain`; both windowed and direct-display initialization paths must create the same resources.

## Render ordering

VSE currently groups 2D commands by pipeline rather than preserving call order across every primitive type. Native 3D should define an equally explicit layer contract:

1. copy or blit an external underlay, if present;
2. render all native 3D commands with color and depth attachments;
3. render existing 2D commands with the color attachment only;
4. present through the existing timing path.

The 3D pass clears color when no external underlay exists and otherwise loads it. The 2D pass always loads color, so fixation marks, text, and photodiode patches remain visible above 3D content. Separate dynamic-rendering instances avoid making the existing 2D pipelines depth-compatible.

If a frame has no 3D commands, use the existing 2D path without allocating or clearing depth work.

## Loading and reproducibility

Loading is allowed before a timed trial and may block for file IO, CPU decode, upload, and a transfer fence. Drawing is allocation-free with respect to model resources.

For every loaded model, record or make available:

- source path
- SHA-256 of source bytes
- glTF generator and version strings, when present
- vertex, index, primitive, and instance counts
- bounds
- unsupported fields encountered

The initial implementation can expose this as `ModelInfo`; integration with session metadata can follow once the shape is stable. The example prints the information at startup.

VSE creates its Vulkan renderer only after the event loop resumes, so the example loads all four demo models during an explicit startup phase in the first render callback, before the first timed trial frame. Arrow-key switching then changes a handle only. No disk or GPU upload occurs on a key press.

## Demo

Add a numbered native example after the current curriculum:

```text
examples/20_mesh_normals_3d.rs
```

### Controls

- `Left Arrow`: previous model, wrapping at the start
- `Right Arrow`: next model, wrapping at the end
- left-button drag: arcball rotation offset
- `R`: clear the drag offset
- `Escape`: quit

The four entries, in order, are Bunny, Teapot, Suzanne, and Benchy.

### Motion

The object rotates clockwise around world `+Y` by a fixed angle per VSE frame index:

```text
auto_yaw(frame) = -TAU * frame / SPIN_PERIOD_FRAMES
```

Use a named constant such as `SPIN_PERIOD_FRAMES = 480`. The period is specified in frames, not host-clock seconds. This keeps the stimulus state reproducible and prevents host scheduling jitter from changing orientation. The corresponding duration in seconds depends on refresh rate and should be printed at startup when known.

Arcball drag updates a persistent quaternion from normalized cursor positions. Compose the drag orientation with the automatic yaw; automatic rotation continues while dragging and after release. Clamp or normalize every accumulated quaternion to prevent drift. Switching models retains the camera and drag orientation, while each model gets an explicit bounds-derived fit transform.

The background is neutral gray. Add a small VSE 2D overlay with model name, triangle count, controls, and the `X/Y/Z -> R/G/B` convention. The overlay also proves that existing 2D stimuli compose above native 3D.

## Demo assets and licensing

Do not add opaque copies of the four meshes without provenance and license records.

| Model | Recommended source | Redistribution plan |
|---|---|---|
| Bunny | Stanford 3D Scanning Repository | Do not include in the MIT crate/package. Stanford permits research use and free redistribution but prohibits commercial use or appearance in a product for sale without permission. Download during explicit demo-asset setup and retain attribution. |
| Teapot | freeglut teapot data | Generate a triangulated GLB and retain the source MIT/X-style notice. The University of Utah repository provides data but states no reuse license. |
| Suzanne | Khronos glTF Sample Assets Suzanne | Use the asset carrying its explicit CC0-1.0 declaration; convert or pack to GLB reproducibly. |
| Benchy | Official 3DBenchy | CC0-1.0; conversion from the official STL to GLB is permitted. Credit is appreciated. |

Create an asset manifest with source URL, source SHA-256, converted SHA-256, license, attribution, conversion command, scale, and orientation correction. Converted assets must preserve geometry; decimation is not part of the first demo unless the manifest names the exact method and parameters.

The asset preparation command should be separate from `cargo build`. Building VSE must not require network access, Blender, or acceptance of third-party terms. The example should report a clear setup command when an asset is absent.

## Performance requirements

The normal pipeline is intentionally small: one indexed draw per model primitive, one vertex position fetch, two matrix transforms, a depth test, and derivative-based fragment normal calculation.

Acceptance targets on the reference Intel Meteor Lake laptop at 800×600 in release mode:

- no model-resource allocation or file IO after the startup-loading phase;
- no validation-layer errors;
- the largest demo model remains interactive at display refresh;
- native 3D adds no missed presentations relative to a same-session blank/2D baseline under the same backend;
- model switching does not create a timing spike because all resources are resident;
- resize and swapchain recreation do not reuse an in-flight depth image.

Report CPU command-recording time and GPU frame time when timestamp-query support is added. Do not claim a fixed triangle budget until it is measured on the target hardware.

## Implementation milestones

### 1. Math and CPU asset model

- add `glam` and `gltf`
- define camera, bounds, handles, and errors
- decode a tiny checked-in GLB fixture
- test node traversal, generated indices, bounds, and rejection paths

Exit criterion: CPU tests establish coordinate, winding, and loader behavior without a Vulkan device.

### 2. Procedural GPU triangle

- add `Vertex3D`
- add mesh buffer upload and handle storage
- add normal shaders and pipeline
- add per-swapchain-image depth attachments
- render a known triangle and cube before enabling file loading in the example

Exit criterion: readback tests confirm axis-normal colors, culling, occlusion, and 2D overlay ordering.

### 3. glTF model drawing

- upload decoded primitives once
- preserve node instances
- queue `draw_model_normals`
- recreate depth resources with the swapchain
- add explicit unload

Exit criterion: a multi-primitive, multi-node fixture renders with correct transforms and bounds.

### 4. Four-object demo

- add deterministic asset preparation and manifest
- implement startup preload, fit transforms, arrow switching, arcball drag, and frame-index yaw
- add the 2D status overlay
- preload Bunny, Teapot, Suzanne, and Benchy before presentation begins

Exit criterion: every available model can be selected repeatedly without IO, upload, or pipeline creation during presentation.

### 5. Timing and determinism verification

- run validation layers
- compare repeated-frame readback hashes for identical frame index and interaction state
- run a blank/2D baseline and the largest model under identical display conditions
- record present statistics and any residual driver-specific differences

Exit criterion: pixel hashes match for repeated scene state on the same hardware/software stack, and presentation timing does not regress relative to baseline.

## Test matrix

### Unit tests

- normal-to-RGB mapping for all six cardinal axes
- known-winding triangle produces the expected normal
- camera rejects invalid near/far/FOV values
- GLB indexed primitive
- GLB non-indexed primitive
- nested node transforms and repeated mesh instances
- bounds under translated, rotated, and non-uniformly scaled nodes
- malformed indices and missing positions
- unsupported primitive modes, skins, and morph targets
- stable asset hashes and metadata

### GPU/readback tests

- front face visible and back face culled
- near triangle occludes far triangle regardless of submission order
- each cube face has one constant normal color
- non-uniform scale produces the transformed geometric normal
- 2D fixation mark appears above the mesh
- identical frame state hashes identically in two independent runs
- swapchain resize recreates all depth images

GPU tests should skip with an explicit reason when Vulkan or a required format is unavailable.

## Likely file changes

```text
Cargo.toml
src/drawing/mod.rs
src/drawing/model.rs          # public handles, bounds, CPU glTF decode
src/drawing/vertex.rs         # Vertex3D
src/drawing/primitives.rs     # DrawCommand3D or unified command enum
src/drawing/renderer.rs       # resources, upload, depth, 3D pass
src/shaders/mesh_normals.vert
src/shaders/mesh_normals.frag
src/core/render_context.rs    # public model API
src/core/state.rs             # swapchain-recreation hook
src/core/init.rs              # depth-resource initialization
src/core/flip.rs              # pass image index to renderer
src/lib.rs                    # prelude exports
examples/20_mesh_normals_3d.rs
examples/README.md
assets/3d/manifest.toml
assets/3d/README.md
```

Keep CPU decoding and transform tests out of `renderer.rs`. That file is already responsible for every 2D pipeline and command-recording path; a follow-up refactor may split 2D and 3D pipeline state behind one coordinating renderer, but the feature should not require a broad renderer rewrite first.

## Open decisions before implementation

1. Support `.glb` plus self-contained and local-resource `.gltf` files in the first release.
2. Confirm the derivative cross-product order and front-face state with a GPU readback test on the existing positive-height viewport.
3. Keep model provenance available through `ModelInfo` initially; session-schema integration can follow after the metadata shape is stable.

## References

- [glTF 2.0 specification](https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html)
- [Khronos Vulkan Guide: Depth](https://docs.vulkan.org/guide/latest/depth.html)
- [Khronos Vulkan sample: Dynamic rendering](https://docs.vulkan.org/samples/latest/samples/extensions/dynamic_rendering/README.html)
- [Stanford 3D Scanning Repository](https://graphics.stanford.edu/data/3Dscanrep/)
- [Official 3DBenchy license](https://www.3dbenchy.com/license/)
- [Khronos glTF Sample Assets](https://github.com/KhronosGroup/glTF-Sample-Assets)
- [freeglut project and license](https://freeglut.sourceforge.net/)
- [University of Utah model repository](https://www-old.cs.utah.edu/~dejohnso/models/teapot.html)
- [`docs/3d-vr-rendering-landscape.md`](3d-vr-rendering-landscape.md)
- [`docs/guides/external_rendering_timing.md`](guides/external_rendering_timing.md)
