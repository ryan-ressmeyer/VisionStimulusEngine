use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;

use vulkano::{
    buffer::Subbuffer,
    buffer::{Buffer, BufferCreateInfo, BufferUsage},
    command_buffer::{
        allocator::{CommandBufferAllocator, StandardCommandBufferAllocator},
        AutoCommandBufferBuilder, BlitImageInfo, CommandBufferUsage as CmdBufUsage,
        CopyBufferToImageInfo, CopyImageToBufferInfo, PrimaryAutoCommandBuffer,
        PrimaryCommandBufferAbstract, RenderingAttachmentInfo, RenderingInfo,
    },
    descriptor_set::{
        allocator::{DescriptorSetAllocator, StandardDescriptorSetAllocator},
        DescriptorSet, WriteDescriptorSet,
    },
    device::{Device, Queue},
    format::{ClearValue, Format, FormatFeatures},
    image::{
        sampler::{Filter, Sampler, SamplerAddressMode, SamplerCreateInfo},
        view::ImageView,
        Image, ImageCreateInfo, ImageType, ImageUsage,
    },
    memory::allocator::{AllocationCreateInfo, MemoryTypeFilter, StandardMemoryAllocator},
    pipeline::{
        graphics::{
            color_blend::{AttachmentBlend, ColorBlendAttachmentState, ColorBlendState},
            depth_stencil::{DepthState, DepthStencilState},
            input_assembly::{InputAssemblyState, PrimitiveTopology},
            multisample::MultisampleState,
            rasterization::{CullMode, FrontFace, RasterizationState},
            subpass::PipelineRenderingCreateInfo,
            vertex_input::{
                Vertex as VertexTrait, VertexDefinition, VertexInputAttributeDescription,
                VertexInputBindingDescription, VertexInputRate, VertexInputState,
            },
            viewport::{Viewport, ViewportState},
            GraphicsPipelineCreateInfo,
        },
        layout::PipelineDescriptorSetLayoutCreateInfo,
        DynamicState, GraphicsPipeline, Pipeline, PipelineBindPoint, PipelineLayout,
        PipelineShaderStageCreateInfo,
    },
    render_pass::{AttachmentLoadOp, AttachmentStoreOp},
    sync::GpuFuture,
};

use super::primitives::{
    arc_vertices, circle_vertices, dot_unit_quad_vertices, line_vertices, rect_vertices,
    textured_quad_vertices, DrawCommand, DrawCommand3D,
};

/// An external frame consumed as the background of one rendered frame
/// (see `core::external_frame`).
pub(crate) struct ExternalUnderlay {
    /// The imported external image to place under VSE's draw commands.
    pub image: Arc<Image>,
    /// When set, the external image is additionally copied into this
    /// host-visible buffer (determinism-harness readback).
    pub readback: Option<Subbuffer<[u8]>>,
}
use super::model::{decode_model, DecodedInstance, ModelError, ModelHandle, ModelInfo};
use super::stimuli::WaveType;
use super::texture::TextureHandle;
use super::vertex::{DotInstance, TexturedVertex, Vertex2D, Vertex3D};

const MESH_FRONT_FACE: FrontFace = FrontFace::CounterClockwise;

/// Whether a queued 2D draw uses the coalescing flat-color pipeline
/// (`Rect`/`Circle`/`Line`/`Arc`) or records on its own like every other
/// primitive. Used to plan call-order compositing without a Vulkan device.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum DrawKind {
    Flat,
    NonFlat,
}

/// One unit of the ordered 2D render plan produced by [`plan_render_segments`].
///
/// Either a run of consecutive flat-color commands coalesced into a single flat
/// draw, or a single non-flat command recorded on its own. In both cases the
/// indices refer to positions in the ordered `draw_commands` queue.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum RenderSegment {
    /// Consecutive flat-color commands `[start, end)`, coalesced into one draw.
    FlatRun { start: usize, end: usize },
    /// A single non-flat command at this index.
    Single(usize),
}

/// Classify a draw command as flat-color (coalescing) or not.
fn draw_kind(cmd: &DrawCommand) -> DrawKind {
    match cmd {
        DrawCommand::Rect { .. }
        | DrawCommand::Circle { .. }
        | DrawCommand::Line { .. }
        | DrawCommand::Arc { .. } => DrawKind::Flat,
        DrawCommand::Texture { .. }
        | DrawCommand::Noise { .. }
        | DrawCommand::Grating { .. }
        | DrawCommand::Gabor { .. }
        | DrawCommand::Dots { .. } => DrawKind::NonFlat,
    }
}

/// Partition ordered draw kinds into the call-order render plan: each maximal
/// run of consecutive `Flat` commands becomes one [`RenderSegment::FlatRun`],
/// and every `NonFlat` command becomes its own [`RenderSegment::Single`], with
/// call order preserved. Pure and device-free — the regression net for
/// call-order compositing (see `docs/design/pipeline-flexibility.md` §4-5).
fn plan_render_segments(kinds: &[DrawKind]) -> Vec<RenderSegment> {
    let mut segments = Vec::new();
    let mut run_start: Option<usize> = None;
    for (i, kind) in kinds.iter().enumerate() {
        match kind {
            DrawKind::Flat => {
                run_start.get_or_insert(i);
            }
            DrawKind::NonFlat => {
                if let Some(start) = run_start.take() {
                    segments.push(RenderSegment::FlatRun { start, end: i });
                }
                segments.push(RenderSegment::Single(i));
            }
        }
    }
    if let Some(start) = run_start.take() {
        segments.push(RenderSegment::FlatRun {
            start,
            end: kinds.len(),
        });
    }
    segments
}

fn additive_gabor_blend() -> AttachmentBlend {
    gabor_accumulation_blend(vulkano::pipeline::graphics::color_blend::BlendOp::Add)
}

fn subtractive_gabor_blend() -> AttachmentBlend {
    gabor_accumulation_blend(vulkano::pipeline::graphics::color_blend::BlendOp::ReverseSubtract)
}

fn gabor_accumulation_blend(
    color_blend_op: vulkano::pipeline::graphics::color_blend::BlendOp,
) -> AttachmentBlend {
    use vulkano::pipeline::graphics::color_blend::{BlendFactor, BlendOp};

    AttachmentBlend {
        src_color_blend_factor: BlendFactor::One,
        dst_color_blend_factor: BlendFactor::One,
        color_blend_op,
        src_alpha_blend_factor: BlendFactor::Zero,
        dst_alpha_blend_factor: BlendFactor::One,
        alpha_blend_op: BlendOp::Add,
    }
}

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

mod mesh_normals_vs {
    vulkano_shaders::shader! {
        ty: "vertex",
        path: "src/shaders/mesh_normals.vert",
    }
}

mod mesh_normals_fs {
    vulkano_shaders::shader! {
        ty: "fragment",
        path: "src/shaders/mesh_normals.frag",
    }
}

/// Key identifying one of VSE's built-in graphics pipelines.
///
/// The full suite is always constructed today; this key lets `render()` fetch
/// each pipeline by identity rather than by a named `Renderer` field, which is
/// the prerequisite for later suite-subselection and user-registered pipelines
/// (see `docs/design/pipeline-flexibility.md`).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum BuiltinPipeline {
    FlatColor,
    Textured,
    Grating,
    Gabor,
    AdditiveGabor,
    SubtractiveGabor,
    Dot,
    MeshNormals,
}

/// The set of built-in pipelines to build for a session.
///
/// Chosen on the builder via [`VSEContextBuilder::with_pipelines`]. Only the
/// selected pipelines are compiled at startup; a draw whose pipeline is absent
/// is skipped at render time (with a one-time warning) rather than panicking.
///
/// The default is the full suite (all eight built-ins), so existing code that
/// never calls `with_pipelines` is unaffected.
///
/// [`VSEContextBuilder::with_pipelines`]: crate::prelude::VSEContextBuilder::with_pipelines
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineSuite {
    enabled: HashSet<BuiltinPipeline>,
}

impl PipelineSuite {
    /// A suite containing no built-in pipelines.
    pub fn empty() -> Self {
        Self {
            enabled: HashSet::new(),
        }
    }

    /// A minimal suite: [`BuiltinPipeline::FlatColor`] only.
    pub fn minimal() -> Self {
        Self::empty().with(BuiltinPipeline::FlatColor)
    }

    /// Add a built-in pipeline to the suite (chainable).
    pub fn with(mut self, pipeline: BuiltinPipeline) -> Self {
        self.enabled.insert(pipeline);
        self
    }

    /// Remove a built-in pipeline from the suite (chainable).
    pub fn without(mut self, pipeline: BuiltinPipeline) -> Self {
        self.enabled.remove(&pipeline);
        self
    }

    /// Whether the suite contains the given built-in pipeline.
    pub fn contains(&self, pipeline: BuiltinPipeline) -> bool {
        self.enabled.contains(&pipeline)
    }

    /// Number of built-in pipelines in the suite.
    pub fn len(&self) -> usize {
        self.enabled.len()
    }

    /// Whether the suite is empty.
    pub fn is_empty(&self) -> bool {
        self.enabled.is_empty()
    }
}

impl Default for PipelineSuite {
    /// The full built-in suite (all eight pipelines) — today's default behavior.
    fn default() -> Self {
        Self {
            enabled: builtin_pipeline_descriptors()
                .into_iter()
                .map(|descriptor| descriptor.key)
                .collect(),
        }
    }
}

/// Builds a single built-in pipeline. The uniform `(device, swapchain_format,
/// depth_format)` signature lets every built-in — including the depth-using
/// mesh-normals pipeline — be constructed from one descriptor list.
type BuiltinPipelineBuilder =
    fn(&Arc<Device>, Format, Format) -> Result<Arc<GraphicsPipeline>, RendererError>;

/// A built-in pipeline's key paired with the code that builds it.
struct BuiltinPipelineDescriptor {
    key: BuiltinPipeline,
    build: BuiltinPipelineBuilder,
}

/// The list of built-in pipeline descriptors that make up the default suite.
///
/// This is device-free: it only names the pipelines and their builders, so the
/// registry's key set can be asserted without a Vulkan device.
fn builtin_pipeline_descriptors() -> [BuiltinPipelineDescriptor; 8] {
    [
        BuiltinPipelineDescriptor {
            key: BuiltinPipeline::FlatColor,
            build: |device, fmt, _depth| Renderer::create_flat_color_pipeline(device, fmt),
        },
        BuiltinPipelineDescriptor {
            key: BuiltinPipeline::Textured,
            build: |device, fmt, _depth| Renderer::create_textured_pipeline(device, fmt),
        },
        BuiltinPipelineDescriptor {
            key: BuiltinPipeline::Grating,
            build: |device, fmt, _depth| Renderer::create_grating_pipeline(device, fmt),
        },
        BuiltinPipelineDescriptor {
            key: BuiltinPipeline::Gabor,
            build: |device, fmt, _depth| {
                Renderer::create_gabor_pipeline(device, fmt, AttachmentBlend::alpha())
            },
        },
        BuiltinPipelineDescriptor {
            key: BuiltinPipeline::AdditiveGabor,
            build: |device, fmt, _depth| {
                Renderer::create_gabor_pipeline(device, fmt, additive_gabor_blend())
            },
        },
        BuiltinPipelineDescriptor {
            key: BuiltinPipeline::SubtractiveGabor,
            build: |device, fmt, _depth| {
                Renderer::create_gabor_pipeline(device, fmt, subtractive_gabor_blend())
            },
        },
        BuiltinPipelineDescriptor {
            key: BuiltinPipeline::Dot,
            build: |device, fmt, _depth| Renderer::create_dot_pipeline(device, fmt),
        },
        BuiltinPipelineDescriptor {
            key: BuiltinPipeline::MeshNormals,
            build: |device, fmt, depth| Renderer::create_mesh_normals_pipeline(device, fmt, depth),
        },
    ]
}

/// The built graphics pipelines, keyed by [`BuiltinPipeline`].
///
/// Owns the `Arc<GraphicsPipeline>`s that used to live in named `Renderer`
/// fields. Lookup happens once per pass (not per pixel), so a `HashMap` is fine.
pub(crate) struct Pipelines {
    pipelines: HashMap<BuiltinPipeline, Arc<GraphicsPipeline>>,
}

impl Pipelines {
    /// Build only the pipelines selected by `suite` from
    /// [`builtin_pipeline_descriptors`]. Descriptors whose key is not in the
    /// suite are skipped, so their shaders are never compiled.
    fn new(
        device: &Arc<Device>,
        swapchain_format: Format,
        depth_format: Format,
        suite: &PipelineSuite,
    ) -> Result<Self, RendererError> {
        let mut pipelines = HashMap::new();
        for descriptor in builtin_pipeline_descriptors() {
            if !suite.contains(descriptor.key) {
                continue;
            }
            let pipeline = (descriptor.build)(device, swapchain_format, depth_format)?;
            pipelines.insert(descriptor.key, pipeline);
        }
        Ok(Self { pipelines })
    }

    /// Fetch a built-in pipeline by key, or `None` if it is not in the active
    /// suite. The render path uses this to skip (and warn about) absent
    /// pipelines rather than panicking.
    fn get_opt(&self, key: BuiltinPipeline) -> Option<&Arc<GraphicsPipeline>> {
        self.pipelines.get(&key)
    }
}

/// Errors that can occur in the renderer.
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

    #[error(transparent)]
    Model(#[from] ModelError),

    #[error("Failed to create depth attachments: {0}")]
    DepthCreationFailed(String),
}

/// GPU resources for a loaded texture.
struct MeshPrimitiveResources {
    vertex_buffer: Subbuffer<[Vertex3D]>,
    index_buffer: Subbuffer<[u32]>,
    index_count: u32,
}

struct ModelResources {
    primitives: Vec<MeshPrimitiveResources>,
    instances: Vec<DecodedInstance>,
    info: ModelInfo,
}

struct TextureResources {
    #[allow(dead_code)]
    image_view: Arc<ImageView>,
    #[allow(dead_code)]
    sampler: Arc<Sampler>,
    descriptor_set: Arc<DescriptorSet>,
    #[allow(dead_code)]
    width: u32,
    #[allow(dead_code)]
    height: u32,
}

/// The concrete command-buffer builder VSE records every frame into.
///
/// This is the exact primary `AutoCommandBufferBuilder` used inside
/// [`Renderer::render_with_underlay`]; a [`RenderContext::draw_custom`] closure
/// records into a `&mut FrameRecorder`. Exposing the concrete vulkano type
/// intentionally couples VSE's public API to vulkano's version — the same
/// accepted tradeoff as the [`device()`]/[`queue()`]/[`swapchain()`] escape
/// hatches (see `docs/design/pipeline-flexibility.md` §3, Tier 2).
///
/// Most callers never need to name this: the closure parameter types are
/// inferred at the `draw_custom` call site. It is exported so helper functions
/// that take the builder can spell the type.
///
/// [`RenderContext::draw_custom`]: crate::prelude::RenderContext::draw_custom
/// [`device()`]: crate::prelude::RenderContext::device
/// [`queue()`]: crate::prelude::RenderContext::queue
/// [`swapchain()`]: crate::prelude::RenderContext::swapchain
pub type FrameRecorder = AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>;

/// Per-frame context handed to a [`RenderContext::draw_custom`] closure
/// alongside the [`FrameRecorder`].
///
/// A struct (not a bare tuple) so fields can be added later without breaking
/// existing custom-draw closures.
///
/// [`RenderContext::draw_custom`]: crate::prelude::RenderContext::draw_custom
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CustomFrameContext {
    /// The framebuffer extent (in pixels) VSE used for this frame's built-in
    /// passes. The dynamic viewport is already set to cover this extent when
    /// the closure runs, so custom pipelines using dynamic viewport state need
    /// not set it themselves.
    pub viewport_extent: [u32; 2],
}

/// A user-supplied custom draw recorded into the active 2D render pass
/// (Tier 2 raw hook). Boxed, single-shot, run once per frame. No `Send` bound:
/// custom draws are invoked single-threaded within `render`.
type CustomDraw = Box<dyn FnOnce(&mut FrameRecorder, &CustomFrameContext)>;

/// The Renderer manages graphics pipelines and converts draw commands
/// into GPU command buffers.
pub(crate) struct Renderer {
    device: Arc<Device>,
    queue: Arc<Queue>,
    command_buffer_allocator: Arc<dyn CommandBufferAllocator>,
    memory_allocator: Arc<StandardMemoryAllocator>,
    descriptor_set_allocator: Arc<dyn DescriptorSetAllocator>,

    pipelines: Pipelines,
    dot_quad_buffer: Subbuffer<[DotInstance]>,
    depth_format: Format,
    depth_views: Vec<Arc<ImageView>>,

    textures: HashMap<u64, TextureResources>,
    models: HashMap<u64, ModelResources>,
    next_model_id: u64,
    next_texture_id: u64,

    draw_commands: Vec<DrawCommand>,
    draw_commands_3d: Vec<DrawCommand3D>,
    /// User-supplied raw record hooks (Tier 2). Drained and run each frame
    /// inside the final 2D pass, on top of the built-in draws.
    custom_draws: Vec<CustomDraw>,
    flat_vertex_scratch: Vec<Vertex2D>,
    dot_instance_scratch: Vec<DotInstance>,

    /// Built-in pipeline kinds already warned about as absent from the suite.
    /// Interior-mutable so the warn-once check works inside the render loops,
    /// which borrow `self` immutably. Ensures we warn once per kind, not once
    /// per frame.
    warned_absent_pipelines: RefCell<HashSet<BuiltinPipeline>>,
}

impl Renderer {
    /// Create a new Renderer, compiling only the pipelines in `suite`.
    pub fn new(
        device: Arc<Device>,
        queue: Arc<Queue>,
        swapchain_format: Format,
        image_count: usize,
        extent: [u32; 2],
        suite: &PipelineSuite,
    ) -> Result<Self, RendererError> {
        let command_buffer_allocator: Arc<dyn CommandBufferAllocator> = Arc::new(
            StandardCommandBufferAllocator::new(device.clone(), Default::default()),
        );
        let memory_allocator = Arc::new(StandardMemoryAllocator::new_default(device.clone()));
        let descriptor_set_allocator: Arc<dyn DescriptorSetAllocator> = Arc::new(
            StandardDescriptorSetAllocator::new(device.clone(), Default::default()),
        );

        let dot_quad_buffer = Self::create_dot_quad_buffer(memory_allocator.clone())?;
        let depth_format = Self::select_depth_format(&device)?;
        let pipelines = Pipelines::new(&device, swapchain_format, depth_format, suite)?;
        let depth_views =
            Self::create_depth_views(memory_allocator.clone(), depth_format, image_count, extent)?;

        Ok(Self {
            device,
            queue,
            command_buffer_allocator,
            memory_allocator,
            descriptor_set_allocator,
            pipelines,
            dot_quad_buffer,
            depth_format,
            depth_views,
            textures: HashMap::new(),
            models: HashMap::new(),
            next_model_id: 0,
            next_texture_id: 0,
            draw_commands: Vec::new(),
            draw_commands_3d: Vec::with_capacity(16),
            custom_draws: Vec::new(),
            flat_vertex_scratch: Vec::new(),
            dot_instance_scratch: Vec::new(),
            warned_absent_pipelines: RefCell::new(HashSet::new()),
        })
    }

    /// Warn once (per pipeline kind) that a queued draw was skipped because its
    /// built-in pipeline is not in the active [`PipelineSuite`].
    fn warn_absent_pipeline(&self, key: BuiltinPipeline) {
        if self.warned_absent_pipelines.borrow_mut().insert(key) {
            tracing::warn!(
                "skipping draw: built-in pipeline {key:?} is not in the active PipelineSuite; \
                 add it with `.with_pipelines(PipelineSuite::default())` or \
                 `PipelineSuite::...with(BuiltinPipeline::{key:?})`"
            );
        }
    }

    /// Push a draw command onto the queue.
    pub fn push(&mut self, command: DrawCommand) {
        self.draw_commands.push(command);
    }

    pub fn push_3d(&mut self, command: DrawCommand3D) {
        self.draw_commands_3d.push(command);
    }

    /// Queue a user-supplied raw record hook (Tier 2). Runs once, inside the
    /// final 2D render pass, after all built-in 2D draws this frame.
    pub fn push_custom(&mut self, record: CustomDraw) {
        self.custom_draws.push(record);
    }

    pub fn load_model(&mut self, path: impl AsRef<Path>) -> Result<ModelHandle, RendererError> {
        let decoded = decode_model(path.as_ref())?;
        let mut primitives = Vec::with_capacity(decoded.primitives.len());
        for primitive in decoded.primitives {
            let vertex_buffer = Buffer::from_iter(
                self.memory_allocator.clone(),
                BufferCreateInfo {
                    usage: BufferUsage::VERTEX_BUFFER,
                    ..Default::default()
                },
                AllocationCreateInfo {
                    memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                        | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                    ..Default::default()
                },
                primitive.vertices,
            )
            .map_err(|e| RendererError::BufferAllocationFailed(e.to_string()))?;
            let index_count = primitive.indices.len() as u32;
            let index_buffer = Buffer::from_iter(
                self.memory_allocator.clone(),
                BufferCreateInfo {
                    usage: BufferUsage::INDEX_BUFFER,
                    ..Default::default()
                },
                AllocationCreateInfo {
                    memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                        | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                    ..Default::default()
                },
                primitive.indices,
            )
            .map_err(|e| RendererError::BufferAllocationFailed(e.to_string()))?;
            primitives.push(MeshPrimitiveResources {
                vertex_buffer,
                index_buffer,
                index_count,
            });
        }
        let id = self.next_model_id;
        self.next_model_id = self.next_model_id.wrapping_add(1);
        self.models.insert(
            id,
            ModelResources {
                primitives,
                instances: decoded.instances,
                info: decoded.info,
            },
        );
        Ok(ModelHandle { id })
    }

    pub fn model_info(&self, handle: ModelHandle) -> Result<&ModelInfo, ModelError> {
        self.models
            .get(&handle.id)
            .map(|model| &model.info)
            .ok_or(ModelError::UnknownHandle(handle.id))
    }

    pub fn unload_model(&mut self, handle: ModelHandle) {
        self.models.remove(&handle.id);
    }

    pub fn recreate_depth_attachments(
        &mut self,
        image_count: usize,
        extent: [u32; 2],
    ) -> Result<(), RendererError> {
        self.depth_views = Self::create_depth_views(
            self.memory_allocator.clone(),
            self.depth_format,
            image_count,
            extent,
        )?;
        Ok(())
    }

    /// The renderer's device memory allocator (for callers that need to create
    /// buffers on VSE's device, e.g. external-frame readbacks).
    pub(crate) fn memory_allocator(&self) -> Arc<StandardMemoryAllocator> {
        self.memory_allocator.clone()
    }

    /// Render all queued commands into a command buffer.
    pub fn render(
        &mut self,
        target_image: Arc<Image>,
        image_index: usize,
        clear_color: [f32; 4],
        viewport_extent: [u32; 2],
    ) -> Result<Arc<PrimaryAutoCommandBuffer>, RendererError> {
        self.render_with_underlay(
            target_image,
            image_index,
            clear_color,
            viewport_extent,
            None,
        )
    }

    /// Like [`render`](Self::render), but optionally consumes an external frame
    /// as an *underlay*: the external image is blitted into the target before
    /// VSE's queued draw commands, which then composite on top (fixation marks,
    /// photodiode patches, ...). The handoff mechanism (blit today, a sampled
    /// composite quad later) is an implementation detail of this function —
    /// consumers of the seam never depend on it.
    ///
    /// Everything records into one `AutoCommandBufferBuilder`, so vulkano
    /// inserts all image-layout transitions: swapchain `PresentSrc →
    /// TransferDst → ColorAttachment → PresentSrc`, external image
    /// `ColorAttachmentOptimal → TransferSrc → ColorAttachmentOptimal` (the
    /// layout contract in `core::external_frame`).
    pub fn render_with_underlay(
        &mut self,
        target_image: Arc<Image>,
        image_index: usize,
        clear_color: [f32; 4],
        viewport_extent: [u32; 2],
        underlay: Option<&ExternalUnderlay>,
    ) -> Result<Arc<PrimaryAutoCommandBuffer>, RendererError> {
        let image_view = ImageView::new_default(target_image.clone())
            .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;

        let mut builder = AutoCommandBufferBuilder::primary(
            self.command_buffer_allocator.clone(),
            self.queue.queue_family_index(),
            CmdBufUsage::OneTimeSubmit,
        )
        .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;

        if let Some(underlay) = underlay {
            // Full-image blit (not copy): handles RGBA↔BGRA channel reordering
            // and extent mismatches; Linear filtering only matters when scaling.
            builder
                .blit_image(BlitImageInfo {
                    filter: Filter::Linear,
                    ..BlitImageInfo::images(underlay.image.clone(), target_image.clone())
                })
                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
            if let Some(readback) = &underlay.readback {
                // Determinism-harness hook: capture the imported external image
                // exactly as consumed (through export/import + semaphore wait).
                builder
                    .copy_image_to_buffer(CopyImageToBufferInfo::image_buffer(
                        underlay.image.clone(),
                        readback.clone(),
                    ))
                    .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
            }
        }

        let viewport = Viewport {
            offset: [0.0, 0.0],
            extent: [viewport_extent[0] as f32, viewport_extent[1] as f32],
            depth_range: 0.0..=1.0,
        };

        if !self.draw_commands_3d.is_empty() {
            let depth_view = self.depth_views.get(image_index).cloned().ok_or_else(|| {
                RendererError::DepthCreationFailed(format!(
                    "no depth attachment for swapchain image {image_index}"
                ))
            })?;
            let (load_op, clear_value) = if underlay.is_some() {
                (AttachmentLoadOp::Load, None)
            } else {
                (
                    AttachmentLoadOp::Clear,
                    Some(ClearValue::Float(clear_color)),
                )
            };
            builder
                .begin_rendering(RenderingInfo {
                    color_attachments: vec![Some(RenderingAttachmentInfo {
                        load_op,
                        store_op: AttachmentStoreOp::Store,
                        clear_value,
                        ..RenderingAttachmentInfo::image_view(image_view.clone())
                    })],
                    depth_attachment: Some(RenderingAttachmentInfo {
                        load_op: AttachmentLoadOp::Clear,
                        store_op: AttachmentStoreOp::DontCare,
                        clear_value: Some(ClearValue::Depth(1.0)),
                        ..RenderingAttachmentInfo::image_view(depth_view)
                    }),
                    ..Default::default()
                })
                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
            builder
                .set_viewport(0, [viewport.clone()].into_iter().collect())
                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;

            for command in &self.draw_commands_3d {
                let DrawCommand3D::ModelNormals {
                    model_id,
                    model_transform,
                    view_projection,
                } = command;
                let model = self
                    .models
                    .get(model_id)
                    .ok_or(ModelError::UnknownHandle(*model_id))?;
                for instance in &model.instances {
                    let primitive = &model.primitives[instance.primitive_index];
                    let world = *model_transform * instance.local_transform;
                    let Some(mesh_normals_pipeline) =
                        self.pipelines.get_opt(BuiltinPipeline::MeshNormals)
                    else {
                        self.warn_absent_pipeline(BuiltinPipeline::MeshNormals);
                        continue;
                    };
                    builder
                        .bind_pipeline_graphics(mesh_normals_pipeline.clone())
                        .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                        .push_constants(
                            mesh_normals_pipeline.layout().clone(),
                            0,
                            mesh_normals_vs::PushConstants {
                                model: world.to_cols_array_2d(),
                                view_projection: view_projection.to_cols_array_2d(),
                            },
                        )
                        .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                        .bind_vertex_buffers(0, primitive.vertex_buffer.clone())
                        .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                        .bind_index_buffer(primitive.index_buffer.clone())
                        .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
                    unsafe {
                        builder
                            .draw_indexed(primitive.index_count, 1, 0, 0, 0)
                            .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
                    }
                }
            }
            builder
                .end_rendering()
                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
        }

        // Existing 2D commands always load over native 3D. With no 3D, retain
        // the original clear/underlay behavior.
        let (load_op, clear_value) = if underlay.is_some() || !self.draw_commands_3d.is_empty() {
            (AttachmentLoadOp::Load, None)
        } else {
            (
                AttachmentLoadOp::Clear,
                Some(ClearValue::Float(clear_color)),
            )
        };
        builder
            .begin_rendering(RenderingInfo {
                color_attachments: vec![Some(RenderingAttachmentInfo {
                    load_op,
                    store_op: AttachmentStoreOp::Store,
                    clear_value,
                    ..RenderingAttachmentInfo::image_view(image_view)
                })],
                ..Default::default()
            })
            .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;

        // Set viewport
        let viewport = Viewport {
            offset: [0.0, 0.0],
            extent: [viewport_extent[0] as f32, viewport_extent[1] as f32],
            depth_range: 0.0..=1.0,
        };
        builder
            .set_viewport(0, [viewport].into_iter().collect())
            .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;

        // Record the queued 2D commands in CALL ORDER. Consecutive flat-color
        // commands (Rect/Circle/Line/Arc) coalesce into a single flat draw;
        // every other primitive (Texture/Noise/Grating/Gabor/Dots) records on
        // its own. This replaces the former type-ordered passes (all flats,
        // then textures, then gratings/gabors, then dots) with call-order
        // compositing, while preserving the flat-color batch for each run of
        // consecutive flats (docs/design/pipeline-flexibility.md §4-5). The
        // per-command recording — buffers, push constants, blend/two-pass
        // selection, descriptor sets, dot instancing, degenerate-input guards,
        // and warn-once absent-pipeline gating — is byte-for-byte unchanged;
        // only the order and the flat-coalescing scope differ.
        let kinds: Vec<DrawKind> = self.draw_commands.iter().map(draw_kind).collect();
        for segment in plan_render_segments(&kinds) {
            match segment {
                RenderSegment::FlatRun { start, end } => {
                    self.fill_flat_run(start, end);
                    if self.flat_vertex_scratch.is_empty() {
                        continue;
                    }
                    let Some(flat_color_pipeline) =
                        self.pipelines.get_opt(BuiltinPipeline::FlatColor)
                    else {
                        self.warn_absent_pipeline(BuiltinPipeline::FlatColor);
                        continue;
                    };
                    let vertex_buffer = Buffer::from_iter(
                        self.memory_allocator.clone(),
                        BufferCreateInfo {
                            usage: BufferUsage::VERTEX_BUFFER,
                            ..Default::default()
                        },
                        AllocationCreateInfo {
                            memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                                | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                            ..Default::default()
                        },
                        self.flat_vertex_scratch.iter().copied(),
                    )
                    .map_err(|e| RendererError::BufferAllocationFailed(e.to_string()))?;

                    let vertex_count = vertex_buffer.len() as u32;
                    builder
                        .bind_pipeline_graphics(flat_color_pipeline.clone())
                        .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                        .push_constants(
                            flat_color_pipeline.layout().clone(),
                            0,
                            flat_color_vs::PushConstants {
                                viewport_size: [
                                    viewport_extent[0] as f32,
                                    viewport_extent[1] as f32,
                                ],
                            },
                        )
                        .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                        .bind_vertex_buffers(0, vertex_buffer)
                        .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
                    // SAFETY: vertex data matches the pipeline's vertex input state
                    unsafe {
                        builder
                            .draw(vertex_count, 1, 0, 0)
                            .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
                    }
                }
                RenderSegment::Single(i) => match &self.draw_commands[i] {
                    // Textured draws (Texture and Noise both use the textured pipeline)
                    DrawCommand::Texture {
                        texture_id,
                        left,
                        top,
                        right,
                        bottom,
                    }
                    | DrawCommand::Noise {
                        texture_id,
                        left,
                        top,
                        right,
                        bottom,
                    } => {
                        let (texture_id, left, top, right, bottom) =
                            (*texture_id, *left, *top, *right, *bottom);

                        let Some(textured_pipeline) =
                            self.pipelines.get_opt(BuiltinPipeline::Textured)
                        else {
                            self.warn_absent_pipeline(BuiltinPipeline::Textured);
                            continue;
                        };

                        let resources = self
                            .textures
                            .get(&texture_id)
                            .ok_or(RendererError::TextureNotFound(texture_id))?;

                        let tex_vertices = textured_quad_vertices(left, top, right, bottom);
                        let vertex_buffer = Buffer::from_iter(
                            self.memory_allocator.clone(),
                            BufferCreateInfo {
                                usage: BufferUsage::VERTEX_BUFFER,
                                ..Default::default()
                            },
                            AllocationCreateInfo {
                                memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                                    | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                                ..Default::default()
                            },
                            tex_vertices,
                        )
                        .map_err(|e| RendererError::BufferAllocationFailed(e.to_string()))?;

                        builder
                            .bind_pipeline_graphics(textured_pipeline.clone())
                            .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                            .push_constants(
                                textured_pipeline.layout().clone(),
                                0,
                                textured_vs::PushConstants {
                                    viewport_size: [
                                        viewport_extent[0] as f32,
                                        viewport_extent[1] as f32,
                                    ],
                                },
                            )
                            .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                            .bind_descriptor_sets(
                                PipelineBindPoint::Graphics,
                                textured_pipeline.layout().clone(),
                                0,
                                resources.descriptor_set.clone(),
                            )
                            .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                            .bind_vertex_buffers(0, vertex_buffer)
                            .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
                        // SAFETY: vertex/descriptor data matches the pipeline's input state
                        unsafe {
                            builder
                                .draw(6, 1, 0, 0)
                                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
                        }
                    }
                    // Grating and Gabor draws
                    cmd @ (DrawCommand::Grating { .. } | DrawCommand::Gabor { .. }) => {
                        let (
                            is_grating,
                            left,
                            top,
                            right,
                            bottom,
                            frequency,
                            orientation,
                            phase,
                            contrast,
                            background,
                            sigma,
                            aspect_ratio,
                            wave_type,
                            additive,
                        ) = match cmd {
                            DrawCommand::Grating {
                                left,
                                top,
                                right,
                                bottom,
                                params,
                            } => (
                                true,
                                *left,
                                *top,
                                *right,
                                *bottom,
                                params.frequency,
                                params.orientation,
                                params.phase,
                                params.contrast,
                                params.background,
                                0.0f32,
                                1.0f32,
                                match params.wave {
                                    WaveType::Sine => 0u32,
                                    WaveType::Square => 1u32,
                                },
                                false,
                            ),
                            DrawCommand::Gabor {
                                left,
                                top,
                                right,
                                bottom,
                                params,
                                additive,
                            } => (
                                false,
                                *left,
                                *top,
                                *right,
                                *bottom,
                                params.frequency,
                                params.orientation,
                                params.phase,
                                params.contrast,
                                params.background,
                                params.sigma,
                                params.aspect_ratio,
                                0u32,
                                *additive,
                            ),
                            _ => unreachable!("outer match guarantees Grating or Gabor"),
                        };
                        let quad = textured_quad_vertices(left, top, right, bottom);
                        let vertex_buffer = Buffer::from_iter(
                            self.memory_allocator.clone(),
                            BufferCreateInfo {
                                usage: BufferUsage::VERTEX_BUFFER,
                                ..Default::default()
                            },
                            AllocationCreateInfo {
                                memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                                    | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                                ..Default::default()
                            },
                            quad,
                        )
                        .map_err(|e| RendererError::BufferAllocationFailed(e.to_string()))?;

                        let first_pass = if is_grating {
                            let Some(pipeline) = self.pipelines.get_opt(BuiltinPipeline::Grating)
                            else {
                                self.warn_absent_pipeline(BuiltinPipeline::Grating);
                                continue;
                            };
                            (pipeline, 0u32)
                        } else if additive {
                            let Some(pipeline) =
                                self.pipelines.get_opt(BuiltinPipeline::AdditiveGabor)
                            else {
                                self.warn_absent_pipeline(BuiltinPipeline::AdditiveGabor);
                                continue;
                            };
                            (pipeline, 1u32)
                        } else {
                            let Some(pipeline) = self.pipelines.get_opt(BuiltinPipeline::Gabor)
                            else {
                                self.warn_absent_pipeline(BuiltinPipeline::Gabor);
                                continue;
                            };
                            (pipeline, 0u32)
                        };
                        // Normalized swapchain formats cannot reliably carry a negative
                        // fragment source into ONE+ONE blending. Split signed modulation
                        // into positive-add and negative-magnitude subtract passes. If the
                        // subtractive pipeline is absent, skip just the second pass.
                        let second_pass = if additive {
                            match self.pipelines.get_opt(BuiltinPipeline::SubtractiveGabor) {
                                Some(pipeline) => Some((pipeline, 2u32)),
                                None => {
                                    self.warn_absent_pipeline(BuiltinPipeline::SubtractiveGabor);
                                    None
                                }
                            }
                        } else {
                            None
                        };

                        for (pipeline, composite_mode) in
                            std::iter::once(first_pass).chain(second_pass)
                        {
                            builder
                                .bind_pipeline_graphics(pipeline.clone())
                                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                                .push_constants(
                                    pipeline.layout().clone(),
                                    0,
                                    parametric_vs::PushConstants {
                                        viewport_size: [
                                            viewport_extent[0] as f32,
                                            viewport_extent[1] as f32,
                                        ]
                                        .into(),
                                        rect: [left, top, right, bottom],
                                        frequency,
                                        orientation,
                                        phase,
                                        contrast,
                                        background,
                                        sigma,
                                        aspect_ratio,
                                        wave_type,
                                        composite_mode,
                                    },
                                )
                                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                                .bind_vertex_buffers(0, vertex_buffer.clone())
                                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
                            unsafe {
                                builder
                                    .draw(6, 1, 0, 0)
                                    .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
                            }
                        }
                    }
                    // Dot draws (instanced rendering)
                    DrawCommand::Dots {
                        positions,
                        radius,
                        color,
                    } => {
                        let (radius, color) = (*radius, *color);
                        if positions.is_empty() {
                            continue;
                        }

                        self.dot_instance_scratch.clear();
                        self.dot_instance_scratch.extend(
                            positions
                                .iter()
                                .copied()
                                .map(|position| DotInstance { position }),
                        );
                        let instance_count = self.dot_instance_scratch.len() as u32;

                        let instance_buffer = Buffer::from_iter(
                            self.memory_allocator.clone(),
                            BufferCreateInfo {
                                usage: BufferUsage::VERTEX_BUFFER,
                                ..Default::default()
                            },
                            AllocationCreateInfo {
                                memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                                    | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                                ..Default::default()
                            },
                            self.dot_instance_scratch.iter().copied(),
                        )
                        .map_err(|e| RendererError::BufferAllocationFailed(e.to_string()))?;

                        let Some(dot_pipeline) = self.pipelines.get_opt(BuiltinPipeline::Dot)
                        else {
                            self.warn_absent_pipeline(BuiltinPipeline::Dot);
                            continue;
                        };

                        let c = color.to_array();
                        builder
                            .bind_pipeline_graphics(dot_pipeline.clone())
                            .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                            .push_constants(
                                dot_pipeline.layout().clone(),
                                0,
                                dot_vs::PushConstants {
                                    viewport_size: [
                                        viewport_extent[0] as f32,
                                        viewport_extent[1] as f32,
                                    ],
                                    dot_radius: radius,
                                    _pad: 0.0,
                                    dot_color: c,
                                },
                            )
                            .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                            .bind_vertex_buffers(0, (self.dot_quad_buffer.clone(), instance_buffer))
                            .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
                        unsafe {
                            builder
                                .draw(6, instance_count, 0, 0)
                                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
                        }
                    }
                    // Flat-color commands are recorded only inside FlatRun
                    // segments; the planner never emits them as Single.
                    DrawCommand::Rect { .. }
                    | DrawCommand::Circle { .. }
                    | DrawCommand::Line { .. }
                    | DrawCommand::Arc { .. } => {
                        unreachable!("flat-color commands are recorded via FlatRun segments")
                    }
                },
            }
        }

        // User-recorded custom draws (Tier 2 raw hook). These run inside the
        // active 2D render pass, AFTER every built-in 2D pass (flat / textured /
        // grating-gabor / dots) and immediately BEFORE end_rendering(), so they
        // composite on top of all built-in draws. `mem::take` drains the queue
        // every frame exactly like `draw_commands`: with no custom draws the
        // loop is empty and this block is a no-op, identical to before.
        let custom_draws = std::mem::take(&mut self.custom_draws);
        if !custom_draws.is_empty() {
            let frame = CustomFrameContext { viewport_extent };
            for record in custom_draws {
                record(&mut builder, &frame);
            }
        }

        builder
            .end_rendering()
            .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;

        let command_buffer = builder
            .build()
            .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;

        self.draw_commands.clear();
        self.draw_commands_3d.clear();

        Ok(command_buffer)
    }

    /// Load a texture from a file path.
    pub fn load_image(&mut self, path: impl AsRef<Path>) -> Result<TextureHandle, RendererError> {
        let img = image::open(path.as_ref())
            .map_err(|e| RendererError::ImageLoadFailed(e.to_string()))?;
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        self.load_texture_rgba(width, height, &rgba)
    }

    /// Create a texture from raw RGBA pixel data.
    pub fn load_texture_rgba(
        &mut self,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Result<TextureHandle, RendererError> {
        // Create staging buffer
        let staging_buffer = Buffer::from_iter(
            self.memory_allocator.clone(),
            BufferCreateInfo {
                usage: BufferUsage::TRANSFER_SRC,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_HOST
                    | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                ..Default::default()
            },
            data.iter().copied(),
        )
        .map_err(|e| RendererError::TextureCreationFailed(e.to_string()))?;

        // Create GPU image
        let image = Image::new(
            self.memory_allocator.clone(),
            ImageCreateInfo {
                image_type: ImageType::Dim2d,
                format: Format::R8G8B8A8_SRGB,
                extent: [width, height, 1],
                usage: ImageUsage::SAMPLED | ImageUsage::TRANSFER_DST,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
                ..Default::default()
            },
        )
        .map_err(|e| RendererError::TextureCreationFailed(e.to_string()))?;

        // Upload via command buffer
        let mut upload_builder = AutoCommandBufferBuilder::primary(
            self.command_buffer_allocator.clone(),
            self.queue.queue_family_index(),
            CmdBufUsage::OneTimeSubmit,
        )
        .map_err(|e| RendererError::TextureCreationFailed(e.to_string()))?;

        upload_builder
            .copy_buffer_to_image(CopyBufferToImageInfo::buffer_image(
                staging_buffer,
                image.clone(),
            ))
            .map_err(|e| RendererError::TextureCreationFailed(e.to_string()))?;

        let upload_cmd = upload_builder
            .build()
            .map_err(|e| RendererError::TextureCreationFailed(e.to_string()))?;

        // Execute upload and wait
        let future = upload_cmd
            .execute(self.queue.clone())
            .map_err(|e| RendererError::TextureCreationFailed(e.to_string()))?;
        let fence = future
            .then_signal_fence_and_flush()
            .map_err(|e| RendererError::TextureCreationFailed(e.to_string()))?;
        fence
            .wait(None)
            .map_err(|e| RendererError::TextureCreationFailed(e.to_string()))?;

        // Create image view
        let image_view = ImageView::new_default(image)
            .map_err(|e| RendererError::TextureCreationFailed(e.to_string()))?;

        // Create sampler
        let sampler = Sampler::new(
            self.device.clone(),
            SamplerCreateInfo {
                mag_filter: Filter::Nearest,
                min_filter: Filter::Nearest,
                address_mode: [SamplerAddressMode::ClampToEdge; 3],
                ..Default::default()
            },
        )
        .map_err(|e| RendererError::TextureCreationFailed(e.to_string()))?;

        // Create descriptor set. The layout comes from the textured pipeline, so
        // loading a texture requires it to be in the active suite.
        let layout = self
            .pipelines
            .get_opt(BuiltinPipeline::Textured)
            .ok_or_else(|| {
                RendererError::DescriptorSetFailed(
                    "textured pipeline not in active PipelineSuite — add BuiltinPipeline::Textured \
                     to load textures"
                        .to_string(),
                )
            })?
            .layout()
            .set_layouts()
            .first()
            .ok_or_else(|| {
                RendererError::DescriptorSetFailed("No descriptor set layout".to_string())
            })?;

        let descriptor_set = DescriptorSet::new(
            self.descriptor_set_allocator.clone(),
            layout.clone(),
            [WriteDescriptorSet::image_view_sampler(
                0,
                image_view.clone(),
                sampler.clone(),
            )],
            [],
        )
        .map_err(|e| RendererError::DescriptorSetFailed(e.to_string()))?;

        let id = self.next_texture_id;
        self.next_texture_id += 1;

        self.textures.insert(
            id,
            TextureResources {
                image_view,
                sampler,
                descriptor_set,
                width,
                height,
            },
        );

        Ok(TextureHandle { id, width, height })
    }

    /// Remove a texture and free its GPU resources.
    pub fn unload_texture(&mut self, handle: TextureHandle) {
        self.textures.remove(&handle.id);
    }

    /// Coalesce the flat-color commands in `draw_commands[start..end]` (a run of
    /// consecutive flats, per the call-order plan) into `flat_vertex_scratch`,
    /// ready for a single flat-color draw. The scratch buffer is cleared first,
    /// so each run flushes independently.
    fn fill_flat_run(&mut self, start: usize, end: usize) {
        self.flat_vertex_scratch.clear();
        for cmd in &self.draw_commands[start..end] {
            Self::append_flat_command_vertices(&mut self.flat_vertex_scratch, cmd);
        }
    }

    /// Append one flat-color command's triangles to `scratch`, preserving the
    /// degenerate-input guards (empty rect, non-positive radius/width/thickness,
    /// zero-length line, vanishing arc span). Non-flat commands contribute
    /// nothing; the call-order plan never routes them here.
    fn append_flat_command_vertices(scratch: &mut Vec<Vertex2D>, cmd: &DrawCommand) {
        match cmd {
            DrawCommand::Rect {
                left,
                top,
                right,
                bottom,
                color,
            } => {
                if left >= right || top >= bottom {
                    return;
                }
                scratch.extend_from_slice(&rect_vertices(*left, *top, *right, *bottom, *color));
            }
            DrawCommand::Circle {
                cx,
                cy,
                radius,
                color,
                segments,
            } => {
                if *radius <= 0.0 {
                    return;
                }
                scratch.extend(circle_vertices(*cx, *cy, *radius, *color, *segments));
            }
            DrawCommand::Line {
                x1,
                y1,
                x2,
                y2,
                width,
                color,
            } => {
                if *width <= 0.0 {
                    return;
                }
                // Skip zero-length lines
                let dx = x2 - x1;
                let dy = y2 - y1;
                if dx * dx + dy * dy < 1e-12 {
                    return;
                }
                scratch.extend_from_slice(&line_vertices(*x1, *y1, *x2, *y2, *width, *color));
            }
            DrawCommand::Arc {
                cx,
                cy,
                radius,
                start_angle,
                end_angle,
                thickness,
                color,
                segments,
            } => {
                if *radius <= 0.0 || *thickness <= 0.0 || (end_angle - start_angle).abs() < 1e-6 {
                    return;
                }
                scratch.extend(arc_vertices(
                    *cx,
                    *cy,
                    *radius,
                    *start_angle,
                    *end_angle,
                    *thickness,
                    *color,
                    *segments,
                ));
            }
            DrawCommand::Texture { .. } => {}
            DrawCommand::Grating { .. } => {}
            DrawCommand::Gabor { .. } => {}
            DrawCommand::Noise { .. } => {}
            DrawCommand::Dots { .. } => {}
        }
    }

    fn create_dot_quad_buffer(
        memory_allocator: Arc<StandardMemoryAllocator>,
    ) -> Result<Subbuffer<[DotInstance]>, RendererError> {
        Buffer::from_iter(
            memory_allocator,
            BufferCreateInfo {
                usage: BufferUsage::VERTEX_BUFFER,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                    | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                ..Default::default()
            },
            dot_unit_quad_vertices(),
        )
        .map_err(|e| RendererError::BufferAllocationFailed(e.to_string()))
    }

    fn create_graphics_pipeline(
        device: &Arc<Device>,
        swapchain_format: Format,
        stages: [PipelineShaderStageCreateInfo; 2],
        vertex_input_state: VertexInputState,
        blend: AttachmentBlend,
    ) -> Result<Arc<GraphicsPipeline>, RendererError> {
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
                        blend: Some(blend),
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

    fn create_flat_color_pipeline(
        device: &Arc<Device>,
        swapchain_format: Format,
    ) -> Result<Arc<GraphicsPipeline>, RendererError> {
        let vs = flat_color_vs::load(device.clone())
            .map_err(|e| RendererError::ShaderLoadFailed(e.to_string()))?;
        let fs = flat_color_fs::load(device.clone())
            .map_err(|e| RendererError::ShaderLoadFailed(e.to_string()))?;

        let vs_entry = vs.entry_point("main").unwrap();
        let fs_entry = fs.entry_point("main").unwrap();

        let vertex_input_state = Vertex2D::per_vertex()
            .definition(&vs_entry)
            .map_err(|e| RendererError::PipelineCreationFailed(e.to_string()))?;

        let stages = [
            PipelineShaderStageCreateInfo::new(vs_entry),
            PipelineShaderStageCreateInfo::new(fs_entry),
        ];

        Self::create_graphics_pipeline(
            device,
            swapchain_format,
            stages,
            vertex_input_state,
            AttachmentBlend::alpha(),
        )
    }

    fn create_textured_pipeline(
        device: &Arc<Device>,
        swapchain_format: Format,
    ) -> Result<Arc<GraphicsPipeline>, RendererError> {
        let vs = textured_vs::load(device.clone())
            .map_err(|e| RendererError::ShaderLoadFailed(e.to_string()))?;
        let fs = textured_fs::load(device.clone())
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

        Self::create_graphics_pipeline(
            device,
            swapchain_format,
            stages,
            vertex_input_state,
            AttachmentBlend::alpha(),
        )
    }

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

        Self::create_graphics_pipeline(
            device,
            swapchain_format,
            stages,
            vertex_input_state,
            AttachmentBlend::alpha(),
        )
    }

    fn create_gabor_pipeline(
        device: &Arc<Device>,
        swapchain_format: Format,
        blend: AttachmentBlend,
    ) -> Result<Arc<GraphicsPipeline>, RendererError> {
        let vs = parametric_vs::load(device.clone())
            .map_err(|e| RendererError::ShaderLoadFailed(e.to_string()))?;
        let fs = gabor_fs::load(device.clone())
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

        Self::create_graphics_pipeline(device, swapchain_format, stages, vertex_input_state, blend)
    }

    fn select_depth_format(device: &Arc<Device>) -> Result<Format, RendererError> {
        [Format::D32_SFLOAT, Format::D16_UNORM]
            .into_iter()
            .find(|&format| {
                device
                    .physical_device()
                    .format_properties(format)
                    .is_ok_and(|properties| {
                        properties
                            .optimal_tiling_features
                            .contains(FormatFeatures::DEPTH_STENCIL_ATTACHMENT)
                    })
            })
            .ok_or_else(|| {
                RendererError::DepthCreationFailed(
                    "neither D32_SFLOAT nor D16_UNORM supports depth attachments".into(),
                )
            })
    }

    fn create_depth_views(
        memory_allocator: Arc<StandardMemoryAllocator>,
        format: Format,
        image_count: usize,
        extent: [u32; 2],
    ) -> Result<Vec<Arc<ImageView>>, RendererError> {
        (0..image_count)
            .map(|_| {
                let image = Image::new(
                    memory_allocator.clone(),
                    ImageCreateInfo {
                        image_type: ImageType::Dim2d,
                        format,
                        extent: [extent[0], extent[1], 1],
                        usage: ImageUsage::DEPTH_STENCIL_ATTACHMENT,
                        ..Default::default()
                    },
                    AllocationCreateInfo {
                        memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
                        ..Default::default()
                    },
                )
                .map_err(|e| RendererError::DepthCreationFailed(e.to_string()))?;
                ImageView::new_default(image)
                    .map_err(|e| RendererError::DepthCreationFailed(e.to_string()))
            })
            .collect()
    }

    fn create_mesh_normals_pipeline(
        device: &Arc<Device>,
        swapchain_format: Format,
        depth_format: Format,
    ) -> Result<Arc<GraphicsPipeline>, RendererError> {
        let vs = mesh_normals_vs::load(device.clone())
            .map_err(|e| RendererError::ShaderLoadFailed(e.to_string()))?;
        let fs = mesh_normals_fs::load(device.clone())
            .map_err(|e| RendererError::ShaderLoadFailed(e.to_string()))?;
        let vs_entry = vs.entry_point("main").unwrap();
        let fs_entry = fs.entry_point("main").unwrap();
        let vertex_input_state = Vertex3D::per_vertex()
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
                rasterization_state: Some(RasterizationState {
                    cull_mode: CullMode::Back,
                    front_face: MESH_FRONT_FACE,
                    ..Default::default()
                }),
                multisample_state: Some(MultisampleState::default()),
                depth_stencil_state: Some(DepthStencilState {
                    depth: Some(DepthState::simple()),
                    ..Default::default()
                }),
                color_blend_state: Some(ColorBlendState::with_attachment_states(
                    1,
                    ColorBlendAttachmentState {
                        blend: None,
                        ..Default::default()
                    },
                )),
                dynamic_state: [DynamicState::Viewport].into_iter().collect(),
                subpass: Some(
                    PipelineRenderingCreateInfo {
                        color_attachment_formats: vec![Some(swapchain_format)],
                        depth_attachment_format: Some(depth_format),
                        ..Default::default()
                    }
                    .into(),
                ),
                ..GraphicsPipelineCreateInfo::layout(layout)
            },
        )
        .map_err(|e| RendererError::PipelineCreationFailed(e.to_string()))
    }

    fn create_dot_pipeline(
        device: &Arc<Device>,
        swapchain_format: Format,
    ) -> Result<Arc<GraphicsPipeline>, RendererError> {
        let vs = dot_vs::load(device.clone())
            .map_err(|e| RendererError::ShaderLoadFailed(e.to_string()))?;
        let fs = dot_fs::load(device.clone())
            .map_err(|e| RendererError::ShaderLoadFailed(e.to_string()))?;

        let vs_entry = vs.entry_point("main").unwrap();
        let fs_entry = fs.entry_point("main").unwrap();

        // Manual vertex input: binding 0 = per-vertex quad, binding 1 = per-instance dot position
        let mut vertex_input_state = VertexInputState::default();
        vertex_input_state.bindings.insert(
            0,
            VertexInputBindingDescription {
                stride: 8,
                input_rate: VertexInputRate::Vertex,
                ..Default::default()
            },
        );
        vertex_input_state.bindings.insert(
            1,
            VertexInputBindingDescription {
                stride: 8,
                input_rate: VertexInputRate::Instance { divisor: 1 },
                ..Default::default()
            },
        );
        vertex_input_state.attributes.insert(
            0,
            VertexInputAttributeDescription {
                binding: 0,
                format: Format::R32G32_SFLOAT,
                offset: 0,
                ..Default::default()
            },
        );
        vertex_input_state.attributes.insert(
            1,
            VertexInputAttributeDescription {
                binding: 1,
                format: Format::R32G32_SFLOAT,
                offset: 0,
                ..Default::default()
            },
        );

        let stages = [
            PipelineShaderStageCreateInfo::new(vs_entry),
            PipelineShaderStageCreateInfo::new(fs_entry),
        ];

        Self::create_graphics_pipeline(
            device,
            swapchain_format,
            stages,
            vertex_input_state,
            AttachmentBlend::alpha(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vulkano::pipeline::graphics::color_blend::{BlendFactor, BlendOp};

    #[test]
    fn gltf_counter_clockwise_faces_are_front_facing() {
        assert_eq!(MESH_FRONT_FACE, FrontFace::CounterClockwise);
    }

    #[test]
    fn builtin_descriptors_cover_exactly_the_eight_builtin_pipelines() {
        use std::collections::HashSet;

        let keys: HashSet<BuiltinPipeline> = builtin_pipeline_descriptors()
            .into_iter()
            .map(|descriptor| descriptor.key)
            .collect();

        let expected: HashSet<BuiltinPipeline> = [
            BuiltinPipeline::FlatColor,
            BuiltinPipeline::Textured,
            BuiltinPipeline::Grating,
            BuiltinPipeline::Gabor,
            BuiltinPipeline::AdditiveGabor,
            BuiltinPipeline::SubtractiveGabor,
            BuiltinPipeline::Dot,
            BuiltinPipeline::MeshNormals,
        ]
        .into_iter()
        .collect();

        // Exactly the eight built-ins, no duplicates (array len already asserts 8).
        assert_eq!(keys.len(), 8, "descriptor keys must be unique");
        assert_eq!(keys, expected);
    }

    #[test]
    fn pipeline_suite_default_contains_all_eight_builtins() {
        let suite = PipelineSuite::default();
        for descriptor in builtin_pipeline_descriptors() {
            assert!(
                suite.contains(descriptor.key),
                "default suite must contain {:?}",
                descriptor.key
            );
        }
        // And exactly eight, no more.
        assert_eq!(suite.len(), 8);
    }

    #[test]
    fn pipeline_suite_minimal_contains_only_flat_color() {
        let suite = PipelineSuite::minimal();
        assert!(suite.contains(BuiltinPipeline::FlatColor));
        assert_eq!(suite.len(), 1);
        assert!(!suite.contains(BuiltinPipeline::Textured));
    }

    #[test]
    fn pipeline_suite_empty_contains_none() {
        let suite = PipelineSuite::empty();
        assert_eq!(suite.len(), 0);
        for descriptor in builtin_pipeline_descriptors() {
            assert!(!suite.contains(descriptor.key));
        }
    }

    #[test]
    fn pipeline_suite_with_and_without_add_and_remove_keys() {
        let suite = PipelineSuite::empty().with(BuiltinPipeline::Dot);
        assert!(suite.contains(BuiltinPipeline::Dot));
        assert!(!suite.contains(BuiltinPipeline::Gabor));

        let suite = suite.with(BuiltinPipeline::Gabor);
        assert!(suite.contains(BuiltinPipeline::Gabor));

        let suite = suite.without(BuiltinPipeline::Dot);
        assert!(!suite.contains(BuiltinPipeline::Dot));
        assert!(suite.contains(BuiltinPipeline::Gabor));
    }

    #[test]
    fn custom_frame_context_carries_viewport_extent() {
        // Device-free: the context is a plain value struct built from the
        // frame's viewport extent. The pixel behavior of `draw_custom` needs a
        // Vulkan device + display and is verified visually via
        // `examples/22_custom_pipeline.rs`.
        let frame = CustomFrameContext {
            viewport_extent: [1920, 1080],
        };
        assert_eq!(frame.viewport_extent, [1920, 1080]);
        // Copy semantics: the struct is cheap to hand to each closure.
        let copy = frame;
        assert_eq!(copy, frame);
    }

    #[test]
    fn subtractive_gabor_blend_subtracts_magnitude_and_preserves_destination_alpha() {
        let blend = subtractive_gabor_blend();

        assert_eq!(blend.src_color_blend_factor, BlendFactor::One);
        assert_eq!(blend.dst_color_blend_factor, BlendFactor::One);
        assert_eq!(blend.color_blend_op, BlendOp::ReverseSubtract);
        assert_eq!(blend.src_alpha_blend_factor, BlendFactor::Zero);
        assert_eq!(blend.dst_alpha_blend_factor, BlendFactor::One);
        assert_eq!(blend.alpha_blend_op, BlendOp::Add);
    }

    #[test]
    fn additive_gabor_blend_adds_color_and_preserves_destination_alpha() {
        let blend = additive_gabor_blend();

        assert_eq!(blend.src_color_blend_factor, BlendFactor::One);
        assert_eq!(blend.dst_color_blend_factor, BlendFactor::One);
        assert_eq!(blend.color_blend_op, BlendOp::Add);
        assert_eq!(blend.src_alpha_blend_factor, BlendFactor::Zero);
        assert_eq!(blend.dst_alpha_blend_factor, BlendFactor::One);
        assert_eq!(blend.alpha_blend_op, BlendOp::Add);
    }

    // --- Call-order render planning (device-free) ---
    //
    // These lock the ordering + flat-coalescing-scope decision that
    // `render_with_underlay` consumes. Pixel output needs a display, so this
    // pure plan is the regression net for call-order compositing.

    use super::super::color::Color;

    #[test]
    fn plan_empty_input_yields_no_segments() {
        assert_eq!(plan_render_segments(&[]), vec![]);
    }

    #[test]
    fn plan_all_flat_coalesces_into_one_run() {
        let kinds = [DrawKind::Flat, DrawKind::Flat, DrawKind::Flat];
        assert_eq!(
            plan_render_segments(&kinds),
            vec![RenderSegment::FlatRun { start: 0, end: 3 }]
        );
    }

    #[test]
    fn plan_all_non_flat_yields_single_segments_in_order() {
        let kinds = [DrawKind::NonFlat, DrawKind::NonFlat, DrawKind::NonFlat];
        assert_eq!(
            plan_render_segments(&kinds),
            vec![
                RenderSegment::Single(0),
                RenderSegment::Single(1),
                RenderSegment::Single(2),
            ]
        );
    }

    #[test]
    fn plan_flat_then_texture_then_flat_yields_three_ordered_segments() {
        // The load-bearing behavior change: a flat, a non-flat, then a flat
        // must produce three segments in call order (not "all flats first").
        let kinds = [DrawKind::Flat, DrawKind::NonFlat, DrawKind::Flat];
        assert_eq!(
            plan_render_segments(&kinds),
            vec![
                RenderSegment::FlatRun { start: 0, end: 1 },
                RenderSegment::Single(1),
                RenderSegment::FlatRun { start: 2, end: 3 },
            ]
        );
    }

    #[test]
    fn plan_coalesces_only_consecutive_flats() {
        // F F N F F N F -> run[0,2), single(2), run[3,5), single(5), run[6,7)
        let kinds = [
            DrawKind::Flat,
            DrawKind::Flat,
            DrawKind::NonFlat,
            DrawKind::Flat,
            DrawKind::Flat,
            DrawKind::NonFlat,
            DrawKind::Flat,
        ];
        assert_eq!(
            plan_render_segments(&kinds),
            vec![
                RenderSegment::FlatRun { start: 0, end: 2 },
                RenderSegment::Single(2),
                RenderSegment::FlatRun { start: 3, end: 5 },
                RenderSegment::Single(5),
                RenderSegment::FlatRun { start: 6, end: 7 },
            ]
        );
    }

    #[test]
    fn plan_leading_nonflat_and_trailing_flat_run_are_kept() {
        let kinds = [DrawKind::NonFlat, DrawKind::Flat, DrawKind::Flat];
        assert_eq!(
            plan_render_segments(&kinds),
            vec![
                RenderSegment::Single(0),
                RenderSegment::FlatRun { start: 1, end: 3 },
            ]
        );
    }

    #[test]
    fn draw_kind_classifies_flat_and_non_flat_commands() {
        // Flat-color family (coalesces).
        let flats = [
            DrawCommand::Rect {
                left: 0.0,
                top: 0.0,
                right: 1.0,
                bottom: 1.0,
                color: Color::WHITE,
            },
            DrawCommand::Circle {
                cx: 0.0,
                cy: 0.0,
                radius: 1.0,
                color: Color::WHITE,
                segments: 16,
            },
            DrawCommand::Line {
                x1: 0.0,
                y1: 0.0,
                x2: 1.0,
                y2: 1.0,
                width: 1.0,
                color: Color::WHITE,
            },
            DrawCommand::Arc {
                cx: 0.0,
                cy: 0.0,
                radius: 1.0,
                start_angle: 0.0,
                end_angle: 1.0,
                thickness: 1.0,
                color: Color::WHITE,
                segments: 16,
            },
        ];
        for cmd in &flats {
            assert_eq!(draw_kind(cmd), DrawKind::Flat);
        }

        // Everything else records on its own.
        let non_flats = [
            DrawCommand::Texture {
                texture_id: 0,
                left: 0.0,
                top: 0.0,
                right: 1.0,
                bottom: 1.0,
            },
            DrawCommand::Noise {
                texture_id: 0,
                left: 0.0,
                top: 0.0,
                right: 1.0,
                bottom: 1.0,
            },
            DrawCommand::Dots {
                positions: vec![],
                radius: 1.0,
                color: Color::WHITE,
            },
        ];
        for cmd in &non_flats {
            assert_eq!(draw_kind(cmd), DrawKind::NonFlat);
        }
    }
}
