# Phase 3: Basic Drawing Primitives - Implementation Guide

## Overview

Phase 3 transforms VSE from a blank-screen timing validator into an actual stimulus presentation engine. This phase introduces the graphics pipeline infrastructure required to draw arbitrary shapes, load textures, and generate basic vision science stimuli. By the end, the "Calibration Square" milestone from the project plan will be achievable.

**Goal:** Draw simple primitives and load textures with the same timing precision established in Phase 2

**Success Criteria:**
- `draw_rect()`, `draw_circle()`, `draw_line()` produce correct visual output
- Textures load from files and render at specified positions
- Gabor patches generate from parameters and render correctly
- `Color` type provides ergonomic color management
- All drawing integrates cleanly with `flip()` timing infrastructure
- Calibration square example runs at 60 Hz with < 1ms jitter
- `cargo check`, `cargo test`, `cargo clippy --all-targets`, `cargo fmt` all pass clean

## What Changes From Phase 2

### Current Rendering Flow

```
clear() → no-op (just a marker)
flip()  → acquire_image → begin_clear() → end_rendering → execute → present → timing
```

Phase 2 can only clear the screen to a solid color. There is no way to draw anything.

### Phase 3 Rendering Flow

```
clear()                         → sets clear color (unchanged)
draw_rect(l, t, r, b, color)   → pushes DrawCommand::Rect to queue
draw_circle(cx, cy, r, color)  → pushes DrawCommand::Circle to queue
draw_line(x1, y1, x2, y2, ...) → pushes DrawCommand::Line to queue
draw_texture(handle, l, t, r, b) → pushes DrawCommand::Texture to queue
flip()                          → acquire_image → build command buffer
                                   (clear + bind pipeline + draw all queued commands)
                                   → execute → present → timing → clear queue
```

The fundamental change is that `flip()` now records draw commands from a queue into the command buffer between `begin_rendering()` and `end_rendering()`, rather than just clearing.

## New Module Structure

```
src/
  drawing/
    mod.rs              // Public exports
    color.rs            // Color type with named constants and conversions
    vertex.rs           // Vertex types (Vertex2D, TexturedVertex)
    primitives.rs       // Vertex generation for rects, circles, lines
    renderer.rs         // Renderer: pipelines, command buffer recording, draw queue
    texture.rs          // TextureHandle, load/create/manage GPU textures
    gabor.rs            // CPU-side Gabor patch generation

  shaders/
    flat_color.vert     // Vertex shader: pixel coords → NDC, color passthrough
    flat_color.frag     // Fragment shader: output interpolated vertex color
    textured.vert       // Vertex shader: pixel coords → NDC, UV passthrough
    textured.frag       // Fragment shader: sample texture
```

## Detailed Component Design

### 1. `src/drawing/color.rs` - Color Type

**Purpose:** Provide an ergonomic color type that matches how vision scientists think about color. Psychtoolbox uses `[R G B]` in 0-255 or 0.0-1.0 range; we use f32 0.0-1.0 to match Vulkan directly.

```rust
/// RGBA color in linear [0.0, 1.0] range.
///
/// All color values are in linear space. The swapchain uses an sRGB
/// format, so Vulkan applies the sRGB transfer function automatically
/// on write. You do not need to manually gamma-correct.
///
/// # Examples
///
/// ```
/// use vision_stimulus_engine::drawing::Color;
///
/// let red = Color::rgb(1.0, 0.0, 0.0);
/// let semi_transparent_blue = Color::rgba(0.0, 0.0, 1.0, 0.5);
/// let mid_grey = Color::grey(0.5);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}
```

**Constructors:**

```rust
impl Color {
    /// Create an opaque RGB color (alpha = 1.0).
    pub fn rgb(r: f32, g: f32, b: f32) -> Self;

    /// Create an RGBA color.
    pub fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self;

    /// Create a grey color (equal R, G, B; alpha = 1.0).
    pub fn grey(level: f32) -> Self;

    /// Create a color from 0-255 integer components.
    /// Useful for specifying colors from lookup tables.
    pub fn from_u8(r: u8, g: u8, b: u8) -> Self;

    /// Convert to [f32; 4] array for Vulkan.
    pub fn to_array(self) -> [f32; 4];

    // Named constants
    pub const WHITE: Self = Self { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
    pub const BLACK: Self = Self { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
    pub const RED: Self   = Self { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
    pub const GREEN: Self = Self { r: 0.0, g: 1.0, b: 0.0, a: 1.0 };
    pub const BLUE: Self  = Self { r: 0.0, g: 0.0, b: 1.0, a: 1.0 };
    pub const GREY: Self  = Self { r: 0.5, g: 0.5, b: 0.5, a: 1.0 };
}
```

**Design Decisions:**

- Linear f32 range, not sRGB. The swapchain format is sRGB, so Vulkan handles the gamma curve on write. Users specify linear values.
- `Color` is `Copy` because it's 16 bytes (4 floats) — cheap to pass by value.
- No HSL/LAB/DKL color spaces in Phase 3. These are useful for vision science but belong in a future `color_spaces` module.
- `set_clear_color()` on `RenderContext` should accept `Color` in addition to the existing `(f32, f32, f32, f32)` form.

### 2. `src/drawing/vertex.rs` - Vertex Types

**Purpose:** Define the vertex formats used by the graphics pipelines. vulkano requires specific derive macros for vertex input.

```rust
/// Vertex for flat-colored geometry (rectangles, circles, lines).
///
/// Position is in pixel coordinates (0,0 = top-left).
/// Color is per-vertex RGBA in linear [0.0, 1.0] range.
#[derive(Clone, Copy, Debug, Default, BufferContents, Vertex)]
#[repr(C)]
pub struct Vertex2D {
    #[format(R32G32_SFLOAT)]
    pub position: [f32; 2],

    #[format(R32G32B32A32_SFLOAT)]
    pub color: [f32; 4],
}

/// Vertex for textured geometry.
///
/// Position is in pixel coordinates.
/// UV coordinates are in [0.0, 1.0] range (0,0 = top-left of texture).
#[derive(Clone, Copy, Debug, Default, BufferContents, Vertex)]
#[repr(C)]
pub struct TexturedVertex {
    #[format(R32G32_SFLOAT)]
    pub position: [f32; 2],

    #[format(R32G32_SFLOAT)]
    pub uv: [f32; 2],
}
```

**Required vulkano imports:**

```rust
use vulkano::buffer::BufferContents;
use vulkano::pipeline::graphics::vertex_input::Vertex;
```

### 3. `src/shaders/` - GLSL Shaders

**Purpose:** Vertex and fragment shaders for the two pipeline types (flat-color and textured).

#### `shaders/flat_color.vert` - Flat Color Vertex Shader

```glsl
#version 460

layout(location = 0) in vec2 position;
layout(location = 1) in vec4 color;

layout(push_constant) uniform PushConstants {
    vec2 viewport_size;
} pc;

layout(location = 0) out vec4 v_color;

void main() {
    // Transform pixel coordinates to Vulkan NDC [-1, 1]
    // Pixel (0,0) = top-left → NDC (-1,-1) = top-left in Vulkan
    vec2 ndc = (position / pc.viewport_size) * 2.0 - 1.0;
    gl_Position = vec4(ndc, 0.0, 1.0);
    v_color = color;
}
```

**Coordinate transform explanation:**

Vulkan NDC: x ∈ [-1, +1] (left to right), y ∈ [-1, +1] (top to bottom).
Pixel coords: x ∈ [0, width] (left to right), y ∈ [0, height] (top to bottom).

The transform `ndc = (pixel / viewport) * 2.0 - 1.0` maps:
- pixel (0, 0) → NDC (-1, -1) → top-left
- pixel (width, height) → NDC (1, 1) → bottom-right

This matches screen coordinates (0,0 at top-left) which is what Psychtoolbox uses.

#### `shaders/flat_color.frag` - Flat Color Fragment Shader

```glsl
#version 460

layout(location = 0) in vec4 v_color;
layout(location = 0) out vec4 f_color;

void main() {
    f_color = v_color;
}
```

#### `shaders/textured.vert` - Textured Vertex Shader

```glsl
#version 460

layout(location = 0) in vec2 position;
layout(location = 1) in vec2 uv;

layout(push_constant) uniform PushConstants {
    vec2 viewport_size;
} pc;

layout(location = 0) out vec2 v_uv;

void main() {
    vec2 ndc = (position / pc.viewport_size) * 2.0 - 1.0;
    gl_Position = vec4(ndc, 0.0, 1.0);
    v_uv = uv;
}
```

#### `shaders/textured.frag` - Textured Fragment Shader

```glsl
#version 460

layout(set = 0, binding = 0) uniform sampler2D tex;

layout(location = 0) in vec2 v_uv;
layout(location = 0) out vec4 f_color;

void main() {
    f_color = texture(tex, v_uv);
}
```

#### Shader Compilation Strategy

Use `vulkano_shaders::shader!` macro to compile GLSL to SPIR-V at Rust compile time. This avoids runtime shader compilation and ensures shaders are always in sync with the Rust code.

```rust
// In renderer.rs or a dedicated shader module
mod flat_color_vs {
    vulkano_shaders::shader! {
        ty: "vertex",
        path: "src/shaders/flat_color.vert",
    }
}

mod flat_color_fs {
    vulkano_shaders::shader! {
        ty: "fragment",
        path: "src/shaders/flat_color.frag",
    }
}

mod textured_vs {
    vulkano_shaders::shader! {
        ty: "vertex",
        path: "src/shaders/textured.vert",
    }
}

mod textured_fs {
    vulkano_shaders::shader! {
        ty: "fragment",
        path: "src/shaders/textured.frag",
    }
}
```

The macro generates Rust types for push constants and descriptor set layouts automatically. For the flat_color vertex shader, it will generate:

```rust
// Auto-generated by vulkano_shaders
flat_color_vs::PushConstants {
    viewport_size: [f32; 2],
}
```

### 4. `src/drawing/primitives.rs` - Vertex Generation

**Purpose:** Convert high-level draw commands into vertex data for the GPU.

```rust
/// A queued draw command, processed during flip().
pub(crate) enum DrawCommand {
    /// Filled rectangle defined by corners.
    Rect {
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        color: Color,
    },

    /// Filled circle.
    Circle {
        cx: f32,
        cy: f32,
        radius: f32,
        color: Color,
        segments: u32,
    },

    /// Line with width.
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        width: f32,
        color: Color,
    },

    /// Textured quad.
    Texture {
        texture_id: u64,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    },
}
```

**Vertex generation functions:**

```rust
/// Generate 6 vertices (2 triangles) for a filled rectangle.
///
/// Vertex winding is counter-clockwise for each triangle:
///
/// ```text
///  (left,top) ---- (right,top)
///      |  \            |
///      |    \          |
///      |      \        |
///  (left,bot) --- (right,bot)
/// ```
pub(crate) fn rect_vertices(
    left: f32, top: f32, right: f32, bottom: f32, color: Color,
) -> [Vertex2D; 6];

/// Generate vertices for a filled circle as a triangle fan
/// (converted to triangle list for batching).
///
/// Creates `segments` triangles, each from center to two adjacent
/// perimeter points. Total vertices: segments * 3.
pub(crate) fn circle_vertices(
    cx: f32, cy: f32, radius: f32, color: Color, segments: u32,
) -> Vec<Vertex2D>;

/// Generate 6 vertices (2 triangles) for a thick line.
///
/// The line is rendered as a thin rectangle oriented along the
/// line direction, with width expanding perpendicular to the line.
///
/// ```text
///     ──────────────→  (direction)
///   ┌──────────────────┐
///   │     width/2      │  ← perpendicular expansion
///   │ (x1,y1)  (x2,y2) │
///   │     width/2      │
///   └──────────────────┘
/// ```
pub(crate) fn line_vertices(
    x1: f32, y1: f32, x2: f32, y2: f32, width: f32, color: Color,
) -> [Vertex2D; 6];

/// Generate 6 textured vertices for a quad.
///
/// UV mapping: (0,0) at top-left of texture, (1,1) at bottom-right.
pub(crate) fn textured_quad_vertices(
    left: f32, top: f32, right: f32, bottom: f32,
) -> [TexturedVertex; 6];
```

**Implementation Details:**

*Rectangle:* Two triangles. Trivial.

*Circle:* Triangle fan as individual triangles. For `n` segments, generate `n` triangles from center to perimeter:
```
for i in 0..segments:
    angle_a = 2π * i / segments
    angle_b = 2π * (i+1) / segments
    vertices.push(center)
    vertices.push(cx + r*cos(angle_a), cy + r*sin(angle_a))
    vertices.push(cx + r*cos(angle_b), cy + r*sin(angle_b))
```

Default segment count: `max(16, (radius * 2.0) as u32)` — more segments for larger circles. Cap at 256.

*Line:* Compute perpendicular offset vector, generate quad:
```
dir = normalize(p2 - p1)
perp = (-dir.y, dir.x) * width / 2
quad corners: p1 ± perp, p2 ± perp
```

### 5. `src/drawing/renderer.rs` - Renderer

**Purpose:** Central rendering coordinator. Manages graphics pipelines, processes the draw command queue, and records everything into a Vulkan command buffer.

```rust
/// The Renderer manages graphics pipelines and converts draw commands
/// into GPU command buffers.
///
/// Created once during VSEContext initialization. Holds cached pipelines
/// and allocators that persist for the lifetime of the context.
pub(crate) struct Renderer {
    device: Arc<Device>,
    queue: Arc<Queue>,
    command_buffer_allocator: Arc<StandardCommandBufferAllocator>,
    memory_allocator: Arc<StandardMemoryAllocator>,
    descriptor_set_allocator: Arc<StandardDescriptorSetAllocator>,

    // Cached pipelines (created lazily on first use)
    flat_color_pipeline: Arc<GraphicsPipeline>,
    textured_pipeline: Arc<GraphicsPipeline>,

    // Texture storage
    textures: HashMap<u64, TextureResources>,
    next_texture_id: u64,

    // Draw command queue (cleared after each flip)
    draw_commands: Vec<DrawCommand>,
}

/// GPU resources for a loaded texture.
struct TextureResources {
    image_view: Arc<ImageView>,
    sampler: Arc<Sampler>,
    descriptor_set: Arc<PersistentDescriptorSet>,
    width: u32,
    height: u32,
}
```

**Key Methods:**

```rust
impl Renderer {
    /// Create a new Renderer with compiled pipelines.
    ///
    /// This is called once during VSEContext::initialize().
    /// Pipeline creation is the most expensive part (~5-10ms).
    pub fn new(
        device: Arc<Device>,
        queue: Arc<Queue>,
        swapchain_format: Format,
    ) -> Result<Self, RendererError>;

    /// Push a draw command onto the queue.
    /// Commands are processed in order during render().
    pub fn push(&mut self, command: DrawCommand);

    /// Check if the draw queue is empty.
    pub fn has_commands(&self) -> bool;

    /// Render all queued commands into a command buffer.
    ///
    /// This method:
    /// 1. Creates a new command buffer
    /// 2. Begins dynamic rendering with the clear color
    /// 3. Batches all flat-color commands into a single vertex buffer + draw call
    /// 4. Records textured draw commands (one draw call per texture)
    /// 5. Ends rendering
    /// 6. Builds and returns the command buffer
    /// 7. Clears the draw queue
    ///
    /// If the draw queue is empty, only the clear operation is recorded.
    pub fn render(
        &mut self,
        target_image: Arc<Image>,
        clear_color: [f32; 4],
        viewport_extent: [u32; 2],
    ) -> Result<Arc<PrimaryAutoCommandBuffer>, RendererError>;

    /// Load a texture from raw RGBA pixel data.
    ///
    /// Returns a TextureHandle that can be used with DrawCommand::Texture.
    pub fn load_texture_rgba(
        &mut self,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Result<TextureHandle, RendererError>;

    /// Remove a texture and free its GPU resources.
    pub fn unload_texture(&mut self, handle: TextureHandle);
}
```

**Pipeline Creation (`Renderer::new`):**

Creating a Vulkan graphics pipeline requires:

1. **Shader stages:** Load compiled SPIR-V for vertex + fragment shaders
2. **Vertex input state:** Describe vertex attribute layout (`Vertex2D` or `TexturedVertex`)
3. **Input assembly:** Topology = `TriangleList` (all primitives decomposed into triangles)
4. **Viewport state:** Dynamic viewport (set per-frame based on swapchain extent)
5. **Rasterization state:** Default (fill mode, front face, no culling for 2D)
6. **Multisample state:** Default (no MSAA for Phase 3)
7. **Color blend state:** Alpha blending enabled for overlapping transparent primitives
8. **Dynamic state:** Viewport (changes on window resize)
9. **Subpass:** `BeginRendering` with the swapchain's color format (dynamic rendering, no render pass)

```rust
fn create_flat_color_pipeline(
    device: &Arc<Device>,
    swapchain_format: Format,
) -> Result<Arc<GraphicsPipeline>, RendererError> {
    let vs = flat_color_vs::load(device.clone())?;
    let fs = flat_color_fs::load(device.clone())?;

    let vs_entry = vs.entry_point("main").unwrap();
    let fs_entry = fs.entry_point("main").unwrap();

    let vertex_input_state = Vertex2D::per_vertex()
        .definition(&vs_entry.info().input_interface)?;

    let stages = [
        PipelineShaderStageCreateInfo::new(vs_entry),
        PipelineShaderStageCreateInfo::new(fs_entry),
    ];

    let layout = PipelineLayout::new(
        device.clone(),
        PipelineDescriptorSetLayoutCreateInfo::from_stages(&stages)
            .into_pipeline_layout_create_info(device.clone())?,
    )?;

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
            subpass: Some(PipelineSubpassType::BeginRendering(
                PipelineRenderingCreateInfo {
                    color_attachment_formats: vec![Some(swapchain_format)],
                    ..Default::default()
                },
            )),
            ..GraphicsPipelineCreateInfo::layout(layout),
        },
    )
}
```

**Render Method (`Renderer::render`):**

```rust
pub fn render(
    &mut self,
    target_image: Arc<Image>,
    clear_color: [f32; 4],
    viewport_extent: [u32; 2],
) -> Result<Arc<PrimaryAutoCommandBuffer>, RendererError> {
    let image_view = ImageView::new_default(target_image)?;

    let mut builder = AutoCommandBufferBuilder::primary(
        &*self.command_buffer_allocator,
        self.queue.queue_family_index(),
        CommandBufferUsage::OneTimeSubmit,
    )?;

    // Begin rendering with clear
    builder.begin_rendering(RenderingInfo {
        color_attachments: vec![Some(RenderingAttachmentInfo {
            load_op: AttachmentLoadOp::Clear,
            store_op: AttachmentStoreOp::Store,
            clear_value: Some(ClearValue::Float(clear_color)),
            ..RenderingAttachmentInfo::image_view(image_view)
        })],
        ..Default::default()
    })?;

    // Set viewport
    let viewport = Viewport {
        offset: [0.0, 0.0],
        extent: [viewport_extent[0] as f32, viewport_extent[1] as f32],
        depth_range: 0.0..=1.0,
    };
    builder.set_viewport(0, [viewport.clone()].into_iter().collect())?;

    // --- Flat-color draws ---
    let flat_vertices = self.generate_flat_color_vertices();
    if !flat_vertices.is_empty() {
        let vertex_buffer = Buffer::from_iter(
            &*self.memory_allocator,
            BufferCreateInfo {
                usage: BufferUsage::VERTEX_BUFFER,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                    | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                ..Default::default()
            },
            flat_vertices.into_iter(),
        )?;

        builder
            .bind_pipeline_graphics(self.flat_color_pipeline.clone())?
            .push_constants(
                self.flat_color_pipeline.layout().clone(),
                0,
                flat_color_vs::PushConstants {
                    viewport_size: [viewport_extent[0] as f32, viewport_extent[1] as f32],
                },
            )?
            .bind_vertex_buffers(0, vertex_buffer.clone())?
            .draw(vertex_buffer.len() as u32, 1, 0, 0)?;
    }

    // --- Textured draws ---
    // (Each texture requires its own descriptor set bind + draw call)
    for cmd in self.drain_texture_commands() {
        let resources = self.textures.get(&cmd.texture_id)
            .ok_or(RendererError::TextureNotFound(cmd.texture_id))?;

        let tex_vertices = textured_quad_vertices(cmd.left, cmd.top, cmd.right, cmd.bottom);
        let vertex_buffer = Buffer::from_iter(/* ... */)?;

        builder
            .bind_pipeline_graphics(self.textured_pipeline.clone())?
            .push_constants(/* viewport_size */)?
            .bind_descriptor_sets(
                PipelineBindPoint::Graphics,
                self.textured_pipeline.layout().clone(),
                0,
                resources.descriptor_set.clone(),
            )?
            .bind_vertex_buffers(0, vertex_buffer.clone())?
            .draw(6, 1, 0, 0)?;
    }

    builder.end_rendering()?;

    let command_buffer = builder.build()?;
    self.draw_commands.clear();

    Ok(command_buffer)
}
```

**Batching Strategy:**

All flat-color draw commands (rects, circles, lines) share the same pipeline and push constants. They can be batched into a single vertex buffer and drawn with one `draw()` call. This is important for performance: binding pipelines is expensive, draw calls have overhead.

Textured draws cannot be batched (each texture has a different descriptor set), so each gets its own draw call. For Phase 3 this is acceptable. Phase 4 could introduce texture atlasing if needed.

**Draw order:** Commands are rendered in the order they were queued. Later draws appear on top of earlier draws (painter's algorithm). This matches Psychtoolbox behavior.

### 6. `src/drawing/texture.rs` - Texture Management

**Purpose:** Load images, upload to GPU, and manage texture lifecycle.

```rust
/// Handle to a loaded texture.
///
/// This is a lightweight identifier. The actual GPU resources are
/// managed internally by the Renderer.
///
/// # Examples
///
/// ```no_run
/// # use vision_stimulus_engine::prelude::*;
/// # fn example(vse: &mut RenderContext) -> Result<(), VSEError> {
/// let texture = vse.load_image("stimulus.png")?;
/// vse.draw_texture(texture, 100.0, 100.0, 356.0, 356.0);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureHandle {
    pub(crate) id: u64,
    /// Width of the texture in pixels
    pub width: u32,
    /// Height of the texture in pixels
    pub height: u32,
}
```

**Loading Functions (on Renderer):**

```rust
impl Renderer {
    /// Load a texture from a file path.
    ///
    /// Supports PNG, JPEG, BMP, TIFF, and other formats supported
    /// by the `image` crate. The image is converted to RGBA8 and
    /// uploaded to GPU memory.
    pub fn load_image(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<TextureHandle, RendererError>;

    /// Create a texture from raw RGBA pixel data.
    ///
    /// Data must be `width * height * 4` bytes (RGBA, 8 bits per channel).
    /// This is the low-level equivalent of Psychtoolbox's `Screen('MakeTexture')`.
    pub fn load_texture_rgba(
        &mut self,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Result<TextureHandle, RendererError>;

    /// Remove a texture and free its GPU resources.
    pub fn unload_texture(&mut self, handle: TextureHandle);
}
```

**GPU Upload Process:**

1. Create a staging `Buffer` with `HOST_SEQUENTIAL_WRITE` for CPU-side pixel data
2. Create a `vulkano::image::Image` with `SAMPLED | TRANSFER_DST` usage, format `R8G8B8A8_SRGB`
3. Record a command buffer that copies from staging buffer to image
4. Execute the upload command buffer and wait for completion
5. Create `ImageView` and `Sampler` (nearest-neighbor by default, configurable later)
6. Create `PersistentDescriptorSet` binding the sampler + image view to set 0, binding 0
7. Store everything in `TextureResources`, return `TextureHandle`

**Sampler Configuration:**

```rust
Sampler::new(
    device.clone(),
    SamplerCreateInfo {
        mag_filter: Filter::Nearest,    // No interpolation (pixel-perfect)
        min_filter: Filter::Nearest,
        address_mode: [SamplerAddressMode::ClampToEdge; 3],
        ..Default::default()
    },
)
```

Vision science stimuli typically require `Nearest` filtering for pixel-accurate rendering. Linear filtering can be enabled as an option later.

### 7. `src/drawing/gabor.rs` - Gabor Patch Generation

**Purpose:** Generate Gabor patches (Gaussian-windowed sinusoidal gratings) as CPU-side pixel arrays, then upload as textures. This is the Phase 3 approach; Phase 4 will replace this with a real-time GPU fragment shader.

```rust
/// Parameters for a Gabor patch.
///
/// A Gabor patch is a sinusoidal grating multiplied by a Gaussian
/// envelope. It's the most commonly used stimulus in vision science
/// for studying spatial frequency processing.
///
/// # Mathematical Definition
///
/// ```text
/// gabor(x, y) = background + contrast * gaussian(x, y) * sin(carrier(x, y))
///
/// gaussian(x, y) = exp(-(x'² + y'²) / (2 * sigma²))
/// carrier(x, y) = 2π * frequency * x' + phase
///
/// where x' = x*cos(orientation) + y*sin(orientation)
///       y' = -x*sin(orientation) + y*cos(orientation)
///       (x, y) are relative to patch center
/// ```
pub struct GaborParams {
    /// Size of the patch in pixels (square texture).
    pub size: u32,

    /// Spatial frequency in cycles per pixel.
    ///
    /// For a 256px patch at 0.04 cycles/pixel → ~10 cycles across the patch.
    pub frequency: f32,

    /// Orientation of the grating in radians.
    ///
    /// 0 = vertical bars, π/2 = horizontal bars.
    pub orientation: f32,

    /// Phase of the sinusoidal carrier in radians.
    ///
    /// 0 = sine phase, π/2 = cosine phase.
    pub phase: f32,

    /// Standard deviation of the Gaussian envelope in pixels.
    ///
    /// Controls the size of the visible region. Typical: size / 6.
    pub sigma: f32,

    /// Contrast of the grating [0.0, 1.0].
    ///
    /// 1.0 = full contrast (luminance swings from background ± 0.5).
    pub contrast: f32,

    /// Mean luminance (background level) [0.0, 1.0].
    ///
    /// Typical: 0.5 (mid-grey).
    pub background: f32,
}

impl Default for GaborParams {
    fn default() -> Self {
        Self {
            size: 256,
            frequency: 0.04,
            orientation: 0.0,
            phase: 0.0,
            sigma: 256.0 / 6.0,
            contrast: 1.0,
            background: 0.5,
        }
    }
}
```

**Generation Function:**

```rust
impl GaborParams {
    /// Generate the Gabor patch as RGBA pixel data.
    ///
    /// Returns a `Vec<u8>` of length `size * size * 4` (RGBA8).
    /// This data can be uploaded as a texture via `load_texture_rgba()`.
    pub fn generate(&self) -> Vec<u8> {
        let mut pixels = Vec::with_capacity((self.size * self.size * 4) as usize);
        let center = self.size as f32 / 2.0;

        for y in 0..self.size {
            for x in 0..self.size {
                let dx = x as f32 - center;
                let dy = y as f32 - center;

                // Rotate to grating orientation
                let x_rot = dx * self.orientation.cos() + dy * self.orientation.sin();
                let y_rot = -dx * self.orientation.sin() + dy * self.orientation.cos();

                // Gaussian envelope
                let gaussian = (-(x_rot * x_rot + y_rot * y_rot)
                    / (2.0 * self.sigma * self.sigma))
                    .exp();

                // Sinusoidal carrier
                let carrier = (2.0 * std::f32::consts::PI * self.frequency * x_rot + self.phase)
                    .sin();

                // Combine: background + contrast * gaussian * carrier
                let luminance = self.background + self.contrast * 0.5 * gaussian * carrier;
                let luminance = luminance.clamp(0.0, 1.0);

                let byte = (luminance * 255.0) as u8;
                pixels.extend_from_slice(&[byte, byte, byte, 255]);
            }
        }

        pixels
    }
}
```

**Usage:**

```rust
let params = GaborParams {
    size: 256,
    frequency: 0.04,
    orientation: std::f32::consts::FRAC_PI_4, // 45 degrees
    phase: 0.0,
    sigma: 40.0,
    contrast: 1.0,
    background: 0.5,
};
let pixels = params.generate();
let texture = vse.load_texture_rgba(params.size, params.size, &pixels)?;
vse.draw_texture(texture, 100.0, 100.0, 356.0, 356.0);
```

**Why CPU-side for Phase 3?**

- Demonstrates the texture loading pipeline end-to-end
- Simple to implement and debug
- Good enough for static Gabor patches
- Phase 4 will add a GPU fragment shader for real-time parameter changes (orientation sweeps, contrast ramping, etc.)

### 8. Integration With Existing Code

#### Changes to `src/core/context.rs`

**Add Renderer to VSEState:**

```rust
struct VSEState {
    // ... existing fields ...
    renderer: Renderer,     // NEW: replaces frame_builder for draw commands
    frame_builder: FrameBuilder,  // KEEP: available for advanced users
}
```

**Initialize Renderer in `VSEContext::initialize()`:**

```rust
fn initialize(elwt: &EventLoopWindowTarget<()>, config: &VSEConfig) -> Result<VSEState, VSEError> {
    // ... existing window/device/swapchain creation ...

    let renderer = Renderer::new(
        device.clone(),
        queue.clone(),
        swapchain.format(),
    ).map_err(|e| VSEError::Renderer(e))?;

    Ok(VSEState {
        // ... existing fields ...
        renderer,
        frame_builder,  // keep for backward compat
    })
}
```

**Add draw methods to `RenderContext`:**

```rust
impl<'a> RenderContext<'a> {
    // === Existing methods (unchanged) ===
    pub fn clear(&mut self) -> Result<(), VSEError>;
    pub fn flip(&mut self) -> Result<FlipInfo, VSEError>;
    pub fn set_clear_color(&mut self, r: f32, g: f32, b: f32, a: f32);
    // ... etc ...

    // === NEW: Drawing primitives ===

    /// Draw a filled rectangle.
    ///
    /// Coordinates are in pixels with (0, 0) at the top-left of the window.
    /// Equivalent to Psychtoolbox's `Screen('FillRect', window, color, [l t r b])`.
    ///
    /// # Arguments
    ///
    /// * `left` - Left edge X coordinate
    /// * `top` - Top edge Y coordinate
    /// * `right` - Right edge X coordinate
    /// * `bottom` - Bottom edge Y coordinate
    /// * `color` - Fill color
    pub fn draw_rect(&mut self, left: f32, top: f32, right: f32, bottom: f32, color: Color);

    /// Draw a filled circle.
    ///
    /// Equivalent to Psychtoolbox's `Screen('FillOval')`.
    ///
    /// # Arguments
    ///
    /// * `cx` - Center X coordinate
    /// * `cy` - Center Y coordinate
    /// * `radius` - Circle radius in pixels
    /// * `color` - Fill color
    pub fn draw_circle(&mut self, cx: f32, cy: f32, radius: f32, color: Color);

    /// Draw a line.
    ///
    /// Equivalent to Psychtoolbox's `Screen('DrawLine')`.
    ///
    /// # Arguments
    ///
    /// * `x1`, `y1` - Start point
    /// * `x2`, `y2` - End point
    /// * `width` - Line width in pixels
    /// * `color` - Line color
    pub fn draw_line(
        &mut self,
        x1: f32, y1: f32, x2: f32, y2: f32,
        width: f32, color: Color,
    );

    /// Draw a texture at the specified rectangle.
    ///
    /// Equivalent to Psychtoolbox's `Screen('DrawTexture')`.
    ///
    /// # Arguments
    ///
    /// * `texture` - Handle from `load_image()` or `load_texture_rgba()`
    /// * `left`, `top`, `right`, `bottom` - Destination rectangle
    pub fn draw_texture(
        &mut self,
        texture: TextureHandle,
        left: f32, top: f32, right: f32, bottom: f32,
    );

    // === NEW: Set clear color with Color type ===

    /// Set the clear color using a Color value.
    pub fn set_clear(&mut self, color: Color);

    // === NEW: Texture management ===

    /// Load a texture from a file.
    ///
    /// Equivalent to Psychtoolbox's `Screen('MakeTexture')` from file.
    pub fn load_image(&mut self, path: impl AsRef<Path>) -> Result<TextureHandle, VSEError>;

    /// Create a texture from raw RGBA pixel data.
    ///
    /// Equivalent to Psychtoolbox's `Screen('MakeTexture')` from matrix.
    pub fn load_texture_rgba(
        &mut self,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Result<TextureHandle, VSEError>;

    /// Create a Gabor patch texture from parameters.
    pub fn create_gabor(&mut self, params: &GaborParams) -> Result<TextureHandle, VSEError>;

    /// Unload a texture and free its GPU resources.
    pub fn unload_texture(&mut self, handle: TextureHandle);
}
```

**Modify `flip()` to use Renderer:**

The key change is in `flip()`. Instead of using `frame_builder.begin_clear()`, use `renderer.render()`:

```rust
pub fn flip(&mut self) -> Result<FlipInfo, VSEError> {
    // ... existing minimized/swapchain recreation handling ...

    // Acquire next image
    let (image_index, _suboptimal, acquire_future) = /* ... same as before ... */;

    let image = self.state.swapchain.images()[image_index as usize].clone();
    let extent = self.state.swapchain.extent();

    // === CHANGED: Use Renderer instead of FrameBuilder ===
    let command_buffer = self.state.renderer.render(
        image,
        self.config.clear_color,
        extent,
    ).map_err(VSEError::Renderer)?;

    // Execute (same sync chain as before)
    let future = acquire_future
        .then_execute(self.state.queue.clone(), command_buffer)
        .map_err(|e| VSEError::Frame(FrameError::ExecutionFailed(e.to_string())))?;

    // --- TIMING: capture submit time ---
    let submit_time = self.state.clock.now();

    // Present (unchanged)
    match self.state.swapchain.present(self.state.queue.clone(), image_index, future) {
        // ... same error handling as Phase 2 ...
    }

    // --- TIMING: capture present complete time ---
    // ... rest of timing logic unchanged from Phase 2 ...
}
```

#### Changes to `src/core/mod.rs`

No changes needed. The draw methods are on `RenderContext`, which is already public.

#### Changes to `src/lib.rs`

```rust
pub mod core;
pub mod timing;
pub mod drawing;  // NEW

pub mod prelude {
    pub use crate::core::{
        DeviceSelector, Frame, GPUPreference, PresentMode, RenderContext,
        SwapchainConfig, SwapchainManager, VSEContext, VSEContextBuilder, VSEError,
    };
    pub use crate::timing::{FlipInfo, FlipLogger, TimingStats};

    // NEW: drawing types in prelude
    pub use crate::drawing::{Color, GaborParams, TextureHandle};
}
```

#### Changes to `Cargo.toml`

```toml
[dependencies]
# ... existing dependencies ...

# Shader compilation at build time
vulkano-shaders = "0.34"

# Image loading for textures
image = { version = "0.25", default-features = false, features = ["png", "jpeg"] }
```

**Why `image` 0.25?** vulkano 0.34 was released around the same era. Using 0.25 avoids potential compatibility issues. We only need PNG and JPEG support, so disable default features to minimize compile time.

#### Changes to `src/core/context.rs` - Error Types

Add a new error variant for renderer errors:

```rust
#[derive(Error, Debug)]
pub enum VSEError {
    // ... existing variants ...

    /// Renderer error
    #[error("Renderer error: {0}")]
    Renderer(#[from] RendererError),
}
```

### 9. `src/drawing/mod.rs` - Module Root

```rust
//! Drawing primitives and texture management
//!
//! This module provides functions for drawing shapes, loading textures,
//! and generating vision science stimuli.

mod color;
mod gabor;
mod primitives;
pub(crate) mod renderer;
mod texture;
mod vertex;

pub use color::Color;
pub use gabor::GaborParams;
pub use texture::TextureHandle;

// Re-export vertex types for advanced users
pub use vertex::{TexturedVertex, Vertex2D};
```

## Phase 3 Milestone Example: Calibration Square

### `examples/02_calibration_square.rs`

**Purpose:** The "Hello World" of vision science stimulus presentation. Demonstrates drawing primitives, timing, and the core usage pattern.

```rust
//! Phase 3 Milestone: The Calibration Square
//!
//! Displays a 100x100 pixel square that alternates between white and
//! black every 60 frames. Logs exact flip timestamps to CSV for
//! external validation with a photodiode or oscilloscope.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 02_calibration_square
//! ```
//!
//! # Validation
//!
//! 1. Run for at least 60 seconds
//! 2. Check timing_calibration_square.csv for frame timing
//! 3. Verify < 1ms jitter (std of frame durations)
//! 4. Verify 0 missed frames

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    let context = VSEContext::builder()
        .with_window_size(1920, 1080)
        .with_title("VSE - Calibration Square")
        .with_clear_color(0.5, 0.5, 0.5, 1.0) // Grey background
        .with_present_mode(PresentMode::Fifo)
        .with_flip_logging(true)
        .with_flip_log_csv("calibration_square.csv")
        .build()?;

    let mut frame_count = 0u64;
    let mut square_white = true;

    context.run(move |vse| {
        vse.clear()?;

        // Draw the calibration square (100x100, centered)
        let (w, h) = vse.window_size();
        let cx = w as f32 / 2.0;
        let cy = h as f32 / 2.0;
        let half = 50.0;

        let color = if square_white { Color::WHITE } else { Color::BLACK };
        vse.draw_rect(cx - half, cy - half, cx + half, cy + half, color);

        let info = vse.flip()?;

        // Toggle every 60 frames (~1 second at 60 Hz)
        frame_count += 1;
        if frame_count >= 60 {
            square_white = !square_white;
            frame_count = 0;
        }

        // Periodic timing report
        if info.frame_number % 600 == 0 && info.frame_number > 0 {
            vse.print_timing_report();
        }

        Ok(())
    })?;

    Ok(())
}
```

**Expected Behavior:**
- 1920x1080 window with grey background
- 100x100 white square centered, toggling to black every second
- Stable 60 FPS, < 1ms jitter
- CSV log generated on exit

### `examples/03_gabor_demo.rs` (Optional Bonus)

```rust
//! Gabor Patch Demo
//!
//! Displays a Gabor patch generated from parameters.

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().init();

    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("VSE - Gabor Patch")
        .with_clear_color(0.5, 0.5, 0.5, 1.0)
        .build()?;

    let mut gabor_handle: Option<TextureHandle> = None;

    context.run(move |vse| {
        // Create Gabor on first frame
        if gabor_handle.is_none() {
            let params = GaborParams {
                size: 256,
                frequency: 0.04,
                orientation: std::f32::consts::FRAC_PI_4,
                phase: 0.0,
                sigma: 40.0,
                contrast: 1.0,
                background: 0.5,
            };
            gabor_handle = Some(vse.create_gabor(&params)?);
        }

        vse.clear()?;

        // Draw centered
        let (w, h) = vse.window_size();
        let cx = w as f32 / 2.0;
        let cy = h as f32 / 2.0;
        let half = 128.0;

        vse.draw_texture(
            gabor_handle.unwrap(),
            cx - half, cy - half,
            cx + half, cy + half,
        );

        vse.flip()?;
        Ok(())
    })?;

    Ok(())
}
```

## Implementation Order

### Step 1: Add Dependencies to `Cargo.toml`

**Complexity:** Trivial.

1. Add `vulkano-shaders = "0.34"`
2. Add `image = { version = "0.25", default-features = false, features = ["png", "jpeg"] }`
3. `cargo check` should pass (new deps unused but available)

### Step 2: Create `src/drawing/color.rs`

**Complexity:** Simple. No Vulkan dependencies.

1. Define `Color` struct with `r`, `g`, `b`, `a` fields
2. Implement constructors: `rgb()`, `rgba()`, `grey()`, `from_u8()`, `to_array()`
3. Define named constants: `WHITE`, `BLACK`, `RED`, `GREEN`, `BLUE`, `GREY`
4. Write unit tests:
   - `test_color_rgb`: verify rgb() constructor
   - `test_color_grey`: verify grey() creates equal RGB
   - `test_color_from_u8`: verify 0-255 to 0.0-1.0 conversion
   - `test_color_to_array`: verify array conversion
   - `test_color_constants`: verify named constants have expected values

### Step 3: Create `src/drawing/vertex.rs`

**Complexity:** Simple. Requires vulkano derive macros.

1. Define `Vertex2D` with position and color fields
2. Define `TexturedVertex` with position and uv fields
3. Add `#[derive(BufferContents, Vertex)]` with format attributes
4. Write unit tests:
   - `test_vertex2d_size`: verify correct byte size (24 bytes)
   - `test_textured_vertex_size`: verify correct byte size (16 bytes)

### Step 4: Create shader files

**Complexity:** Moderate. Must get GLSL correct for vulkano shader macro.

1. Create `src/shaders/flat_color.vert`
2. Create `src/shaders/flat_color.frag`
3. Create `src/shaders/textured.vert`
4. Create `src/shaders/textured.frag`
5. Create shader loading modules with `vulkano_shaders::shader!` macros
6. Verify compilation with `cargo check`

**Important:** The `vulkano_shaders::shader!` macro will fail at compile time if the GLSL is invalid. This gives immediate feedback. The `path` parameter is relative to the Cargo.toml directory.

### Step 5: Create `src/drawing/primitives.rs`

**Complexity:** Moderate. Geometry math.

1. Define `DrawCommand` enum
2. Implement `rect_vertices()`: 6 vertices, 2 triangles
3. Implement `circle_vertices()`: n*3 vertices, n triangles
4. Implement `line_vertices()`: 6 vertices, 2 triangles (perpendicular offset)
5. Implement `textured_quad_vertices()`: 6 vertices with UV coords
6. Write unit tests:
   - `test_rect_vertex_count`: verify 6 vertices
   - `test_rect_vertex_positions`: verify correct corner positions
   - `test_circle_vertex_count`: verify segments * 3 vertices
   - `test_line_perpendicular`: verify line width is perpendicular to direction
   - `test_textured_quad_uvs`: verify UV coords are (0,0)→(1,1)

### Step 6: Create `src/drawing/renderer.rs`

**Complexity:** Most complex step. Vulkan pipeline creation.

1. Define `Renderer` struct with pipeline and allocator fields
2. Define `RendererError` error type
3. Implement `Renderer::new()`:
   - Create command buffer allocator
   - Create memory allocator
   - Create descriptor set allocator
   - Create flat-color pipeline
   - Create textured pipeline
4. Implement `Renderer::push()`: add command to queue
5. Implement `Renderer::render()`:
   - Generate flat-color vertices from queued commands
   - Create vertex buffer
   - Record command buffer (begin_rendering, bind pipeline, draw, end_rendering)
   - Handle textured commands separately
6. Write unit tests:
   - Pipeline creation tests require GPU — mark `#[ignore]`
   - Test vertex generation (already covered in primitives.rs tests)

### Step 7: Create `src/drawing/texture.rs`

**Complexity:** Moderate. GPU upload requires staging buffers.

1. Define `TextureHandle` struct
2. Implement `Renderer::load_image()`: load file with `image` crate, convert to RGBA, upload
3. Implement `Renderer::load_texture_rgba()`: upload raw pixel data
4. Implement GPU upload process (staging buffer → image copy)
5. Create descriptor set for each texture
6. Implement `Renderer::unload_texture()`: remove from map
7. Write unit tests:
   - `test_texture_handle_copy`: verify TextureHandle is Copy
   - GPU upload tests require GPU — mark `#[ignore]`

### Step 8: Create `src/drawing/gabor.rs`

**Complexity:** Moderate. Math-heavy but pure CPU.

1. Define `GaborParams` struct with all fields
2. Implement `GaborParams::default()`
3. Implement `GaborParams::generate()`: CPU pixel generation
4. Write unit tests:
   - `test_gabor_output_size`: verify output is size*size*4 bytes
   - `test_gabor_center_value`: verify center pixel equals background at phase=0
   - `test_gabor_symmetry`: verify horizontal symmetry for vertical grating (orientation=0)
   - `test_gabor_contrast_zero`: verify all pixels equal background at contrast=0
   - `test_gabor_edge_fadeout`: verify corner pixels ≈ background (Gaussian envelope)

### Step 9: Wire `src/drawing/mod.rs` and update `src/lib.rs`

**Complexity:** Trivial.

1. Create `src/drawing/mod.rs` with public re-exports
2. Add `pub mod drawing;` to `src/lib.rs`
3. Add `Color`, `GaborParams`, `TextureHandle` to prelude
4. `cargo check` should pass

### Step 10: Integrate Renderer into VSEContext

**Complexity:** Moderate. Touches context.rs.

1. Add `Renderer` to `VSEState`
2. Initialize `Renderer` in `VSEContext::initialize()`
3. Add `RendererError` variant to `VSEError`
4. Add draw methods to `RenderContext`:
   - `draw_rect()`, `draw_circle()`, `draw_line()`, `draw_texture()`
   - `load_image()`, `load_texture_rgba()`, `create_gabor()`
   - `set_clear()` (Color overload)
5. Modify `flip()` to use `Renderer::render()` instead of `FrameBuilder::begin_clear()`
6. Verify `00_clear_color` and `01_timing_validation` examples still compile and run
7. Run `cargo test`, `cargo clippy`, `cargo fmt`

### Step 11: Create calibration square example

**Complexity:** Simple. Uses the new API.

1. Write `examples/02_calibration_square.rs`
2. Add `[[example]]` entry to `Cargo.toml`
3. Run it and verify visual output
4. Verify timing CSV is correct

### Step 12: Create Gabor demo example (optional)

**Complexity:** Simple.

1. Write `examples/03_gabor_demo.rs`
2. Add `[[example]]` entry to `Cargo.toml`
3. Verify Gabor patch renders correctly

### Step 13: Final validation

1. `cargo check` — clean
2. `cargo test` — all pass
3. `cargo clippy --all-targets` — no warnings
4. `cargo fmt --check` — formatted
5. Run `02_calibration_square` in release mode for 10 minutes
6. Verify jitter < 1 ms standard deviation
7. Verify zero missed frames
8. Verify CSV format unchanged from Phase 2
9. Run `00_clear_color` and `01_timing_validation` — regression check

## Error Handling

### New Error Type

```rust
// In src/drawing/renderer.rs
use thiserror::Error;

#[derive(Error, Debug)]
pub enum RendererError {
    #[error("Failed to create graphics pipeline: {0}")]
    PipelineCreationFailed(String),

    #[error("Failed to create shader module: {0}")]
    ShaderLoadFailed(String),

    #[error("Failed to allocate buffer: {0}")]
    BufferAllocationFailed(String),

    #[error("Failed to record commands: {0}")]
    RecordingFailed(String),

    #[error("Failed to create texture: {0}")]
    TextureCreationFailed(String),

    #[error("Texture not found: id={0}")]
    TextureNotFound(u64),

    #[error("Failed to load image: {0}")]
    ImageLoadFailed(String),

    #[error("Failed to create descriptor set: {0}")]
    DescriptorSetFailed(String),
}
```

## Testing Strategy

### Unit Tests (Pure Logic, No GPU)

```rust
// drawing/color.rs
#[test] fn test_color_rgb()
#[test] fn test_color_rgba()
#[test] fn test_color_grey()
#[test] fn test_color_from_u8()
#[test] fn test_color_to_array()
#[test] fn test_color_constants()

// drawing/vertex.rs
#[test] fn test_vertex2d_default()
#[test] fn test_textured_vertex_default()

// drawing/primitives.rs
#[test] fn test_rect_vertex_count()
#[test] fn test_rect_vertex_positions()
#[test] fn test_rect_winding_order()
#[test] fn test_circle_vertex_count()
#[test] fn test_circle_center_vertices()
#[test] fn test_line_vertex_count()
#[test] fn test_line_perpendicular_offset()
#[test] fn test_line_zero_length()
#[test] fn test_textured_quad_uvs()

// drawing/gabor.rs
#[test] fn test_gabor_output_size()
#[test] fn test_gabor_center_at_background()
#[test] fn test_gabor_zero_contrast()
#[test] fn test_gabor_edge_fadeout()
#[test] fn test_gabor_orientation_symmetry()
#[test] fn test_gabor_default_params()
```

### Integration Tests (Require GPU, `#[ignore]`)

```rust
// In tests/drawing_tests.rs
#[test]
#[ignore] // Requires display + GPU
fn test_render_rect() {
    // Build VSEContext, draw a rect, flip, verify no errors
}

#[test]
#[ignore]
fn test_render_circle() { /* ... */ }

#[test]
#[ignore]
fn test_render_line() { /* ... */ }

#[test]
#[ignore]
fn test_render_texture() { /* ... */ }

#[test]
#[ignore]
fn test_render_gabor() { /* ... */ }
```

### Visual Validation (Manual)

```bash
# Run calibration square
cargo run --release --example 02_calibration_square

# Run Gabor demo
cargo run --release --example 03_gabor_demo
```

**Validation checklist:**
- [ ] Rectangle appears at correct position and size
- [ ] Rectangle color is correct
- [ ] Circle appears round (sufficient segments)
- [ ] Line appears at correct angle and width
- [ ] Gabor patch shows correct orientation and frequency
- [ ] Alpha blending works for overlapping primitives
- [ ] Window resize doesn't break rendering
- [ ] No visual artifacts or flickering

## Performance Considerations

### Vertex Buffer Allocation

Phase 3 creates a new vertex buffer every frame. This is acceptable for small numbers of primitives but will become a bottleneck with thousands of elements. Optimization strategies for future phases:

1. **Double-buffered vertex buffers:** Pre-allocate two large vertex buffers, alternate each frame
2. **Persistent mapped buffers:** Use `HOST_SEQUENTIAL_WRITE` for zero-copy upload
3. **Compute-generated vertices:** For particle systems (RDK), generate vertices on GPU

For Phase 3, per-frame allocation is fine. The bottleneck is vsync, not vertex allocation.

### Pipeline Binds

Flat-color primitives share one pipeline → one bind + one draw call (batched).
Each texture requires a pipeline bind + descriptor set bind + draw call.

For Phase 3 workloads (1-10 primitives), this is negligible. Phase 4 may introduce texture atlasing.

### Timing Impact

Draw command recording adds ~10-50 us per frame depending on complexity. This is well within the timing budget (16,667 us per frame at 60 Hz). The timing infrastructure from Phase 2 will automatically measure any added overhead.

## Dependencies Summary

### New Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `vulkano-shaders` | 0.34 | Compile-time GLSL → SPIR-V shader compilation |
| `image` | 0.25 | Image file loading (PNG, JPEG) for textures |

### Existing Dependencies (No Changes)

- vulkano 0.34
- winit 0.29 (with rwh_05)
- anyhow 1.0
- thiserror 1.0
- tracing 0.1
- tracing-subscriber 0.3
- serde 1.0
- csv 1.3

## Files Summary

| File | Change |
|------|--------|
| `Cargo.toml` | Add `vulkano-shaders`, `image` dependencies; add example entries |
| `src/lib.rs` | Add `pub mod drawing;`, expand prelude |
| `src/core/context.rs` | Add `Renderer` to `VSEState`, add draw methods to `RenderContext`, modify `flip()`, add `RendererError` to `VSEError` |
| `src/drawing/mod.rs` | **NEW** - module root |
| `src/drawing/color.rs` | **NEW** - Color type |
| `src/drawing/vertex.rs` | **NEW** - Vertex2D, TexturedVertex |
| `src/drawing/primitives.rs` | **NEW** - DrawCommand, vertex generation |
| `src/drawing/renderer.rs` | **NEW** - Renderer with pipeline management |
| `src/drawing/texture.rs` | **NEW** - TextureHandle, GPU upload |
| `src/drawing/gabor.rs` | **NEW** - Gabor patch CPU generation |
| `src/shaders/flat_color.vert` | **NEW** - flat color vertex shader |
| `src/shaders/flat_color.frag` | **NEW** - flat color fragment shader |
| `src/shaders/textured.vert` | **NEW** - textured vertex shader |
| `src/shaders/textured.frag` | **NEW** - textured fragment shader |
| `examples/02_calibration_square.rs` | **NEW** - calibration square milestone |
| `examples/03_gabor_demo.rs` | **NEW** - Gabor patch demo (optional) |

## Edge Cases and Considerations

### Empty Draw Queue

When no draw commands are queued, `flip()` renders only the clear color. This is backward-compatible with Phase 2 behavior.

### Degenerate Primitives

- Rectangle with `left >= right` or `top >= bottom`: Generate zero vertices, silently skip
- Circle with `radius <= 0`: Skip
- Line with zero length (same start and end point): Skip
- Line with `width <= 0`: Skip

### Window Resize During Rendering

The viewport is set from the current swapchain extent at the start of each `flip()`. Push constants use the same extent for coordinate transformation. Resize between `draw_rect()` and `flip()` is handled correctly because the extent is read at flip time.

### Texture Lifecycle

Textures persist until explicitly unloaded or the `Renderer` is dropped. Loading the same file twice creates two independent GPU textures (no deduplication). This is intentional — researchers may want to load the same image with different modifications.

### Color Space

The swapchain uses sRGB format (`B8G8R8A8_SRGB` or similar). Vulkan automatically applies the sRGB OETF when writing to sRGB images. This means:
- `Color::grey(0.5)` produces perceptually mid-grey on screen
- Input values are in linear space
- No manual gamma correction needed

For gamma-calibrated displays (common in vision science labs), a separate calibration module will be needed (future phase).

### Coordinate System

All coordinates are in pixels with (0, 0) at the top-left corner of the window. This matches:
- Psychtoolbox screen coordinates
- Vulkan viewport convention (after NDC transform)
- Standard screen coordinate convention

## Future Phase 3+ Extensions (Not In Scope Now)

1. **Outlined Primitives:** `frame_rect()`, outlined circles, dashed lines. Requires line-strip topology or geometry shader. Defer to Phase 4.

2. **Text Rendering:** Requires font rasterization (e.g., `rusttype` or `fontdue`). Important for fixation crosses and instructions. Defer to Phase 4.

3. **Anti-Aliasing:** MSAA or shader-based AA for smooth circle/line edges. Not critical for most vision science stimuli. Defer.

4. **Blend Modes:** Currently only alpha blending. Additive blending useful for plaid stimuli. Defer to Phase 4.

5. **Index Buffers:** Could reduce vertex count for rectangles from 6 to 4 vertices. Minor optimization, not needed for Phase 3 workloads.

6. **Texture Atlasing:** Batch multiple textures into one draw call. Needed for particle systems, not for Phase 3.

## Success Checklist

Phase 3 is complete when:

- [ ] `cargo build` succeeds without warnings
- [ ] `cargo test` passes all drawing unit tests
- [ ] `cargo clippy --all-targets` shows no warnings
- [ ] `cargo fmt --check` passes
- [ ] `examples/00_clear_color.rs` still runs correctly (regression)
- [ ] `examples/01_timing_validation.rs` still runs correctly (regression)
- [ ] `examples/02_calibration_square.rs` runs at 60 Hz with correct visuals
- [ ] Calibration square CSV timing shows < 1ms jitter
- [ ] Rectangles, circles, and lines render at correct positions and colors
- [ ] Textures load from PNG/JPEG and render correctly
- [ ] Gabor patches generate with correct parameters
- [ ] Alpha blending works for overlapping primitives
- [ ] Window resize works without crashes
- [ ] No memory leaks during extended runs
- [ ] All existing Phase 2 timing infrastructure works unchanged

## Resources

### Vulkano Graphics Pipeline
- [Vulkano Triangle Tutorial](https://vulkano.rs/guide/graphics-pipeline-creation) - Pipeline setup reference
- [vulkano-shaders Docs](https://docs.rs/vulkano-shaders/) - Shader macro usage

### Vulkan Graphics Pipeline
- [Vulkan Tutorial: Graphics Pipeline](https://vulkan-tutorial.com/Drawing_a_triangle/Graphics_pipeline_basics) - Conceptual reference
- [Dynamic Rendering (VK_KHR_dynamic_rendering)](https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/VK_KHR_dynamic_rendering.html) - We use this instead of render passes

### Vision Science Stimuli
- [Gabor Patch Mathematics](https://en.wikipedia.org/wiki/Gabor_filter) - Mathematical foundation
- [Psychtoolbox Screen Commands](http://psychtoolbox.org/docs/Screen) - API compatibility reference
