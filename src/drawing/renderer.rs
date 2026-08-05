use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;

use vulkano::{
    buffer::allocator::{SubbufferAllocator, SubbufferAllocatorCreateInfo},
    buffer::BufferContents,
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

use super::pipeline::{
    ErasedStimulusPipeline, PipelineBuildCtx, PipelineError, RecordCtx, RegisteredEntry,
    RegisteredPipeline, RegistryId, StimulusPipeline,
};
use super::primitives::{
    arc_vertices, circle_vertices, dot_unit_quad_vertices, line_vertices, rect_vertices,
    textured_quad_vertices, CustomDrawFn, DrawCommand, DrawCommand3D,
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
use super::noise::{NoiseKey, NoiseTextureCache};
use super::stimuli::WaveType;
use super::texture::TextureHandle;
use super::vertex::{DotInstance, TexturedVertex, Vertex2D, Vertex3D};

const MESH_FRONT_FACE: FrontFace = FrontFace::CounterClockwise;

/// How many distinct noise textures stay resident.
///
/// Sized for animated noise: several panels updating independently keep a few
/// frames of history live without the cache growing without bound. Static noise
/// needs one entry per distinct stimulus and hits forever.
const NOISE_CACHE_CAPACITY: usize = 32;

/// What a queued 2D draw coalesces with, if anything.
///
/// Two draws merge into one segment only when their kinds are equal AND
/// adjacent, which is what keeps call-order compositing intact: anything drawn
/// between them splits the run. Used to plan without a Vulkan device.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum DrawKind {
    /// `Rect`/`Circle`/`Line`/`Arc` — share the flat-color pipeline, so a run
    /// of them becomes one vertex buffer and one draw.
    Flat,
    /// A Tier 1 registered draw, tagged with its pipeline id. Consecutive draws
    /// to the same pipeline reach `record` as one command slice.
    Registered(u64),
    /// Everything else records on its own: each carries per-draw push constants
    /// or descriptor sets that cannot merge into a neighbor's draw call.
    Other,
}

/// One unit of the ordered 2D render plan produced by [`plan_render_segments`].
///
/// Indices refer to positions in the ordered `draw_commands` queue, and the
/// segments partition those indices exactly — every command is recorded once.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum RenderSegment {
    /// Consecutive flat-color commands `[start, end)`, coalesced into one draw.
    FlatRun { start: usize, end: usize },
    /// Consecutive registered draws `[start, end)` for pipeline `id`, handed to
    /// its `record` as a single command slice.
    RegisteredRun { id: u64, start: usize, end: usize },
    /// A command that records on its own.
    Single(usize),
}

/// Classify a draw command by what it can coalesce with.
fn draw_kind(cmd: &DrawCommand) -> DrawKind {
    match cmd {
        DrawCommand::Rect { .. }
        | DrawCommand::Circle { .. }
        | DrawCommand::Line { .. }
        | DrawCommand::Arc { .. } => DrawKind::Flat,
        DrawCommand::Registered { id, .. } => DrawKind::Registered(*id),
        DrawCommand::Texture { .. }
        | DrawCommand::Noise { .. }
        | DrawCommand::Grating { .. }
        | DrawCommand::Gabor { .. }
        | DrawCommand::Dots { .. }
        | DrawCommand::Custom(_) => DrawKind::Other,
    }
}

/// Partition ordered draw kinds into the call-order render plan.
///
/// Each maximal run of consecutive equal coalescing kinds becomes one segment —
/// a [`FlatRun`](RenderSegment::FlatRun) for flat-color commands, a
/// [`RegisteredRun`](RenderSegment::RegisteredRun) for draws sharing one user
/// pipeline — and every other command becomes its own
/// [`Single`](RenderSegment::Single). Call order is preserved throughout, so a
/// draw issued between two coalescable ones splits them rather than being
/// reordered around: layering outranks batching.
///
/// Pure and device-free: this is the regression net for call-order compositing
/// (see `docs/design/pipeline-flexibility.md` §4-5).
fn plan_render_segments(kinds: &[DrawKind]) -> Vec<RenderSegment> {
    /// Close an open run of `kind` spanning `[start, end)`.
    fn flush(segments: &mut Vec<RenderSegment>, kind: DrawKind, start: usize, end: usize) {
        match kind {
            DrawKind::Flat => segments.push(RenderSegment::FlatRun { start, end }),
            DrawKind::Registered(id) => {
                segments.push(RenderSegment::RegisteredRun { id, start, end })
            }
            // `Other` is emitted as a `Single` directly and never opens a run.
            DrawKind::Other => unreachable!("non-coalescing draws never open a run"),
        }
    }

    let mut segments = Vec::new();
    let mut open: Option<(DrawKind, usize)> = None;

    for (i, &kind) in kinds.iter().enumerate() {
        // An open run continues only while the kind is unchanged.
        if let Some((open_kind, start)) = open {
            if open_kind != kind {
                flush(&mut segments, open_kind, start, i);
                open = None;
            }
        }
        match kind {
            DrawKind::Other => segments.push(RenderSegment::Single(i)),
            _ => {
                open.get_or_insert((kind, i));
            }
        }
    }

    if let Some((kind, start)) = open {
        flush(&mut segments, kind, start, kinds.len());
    }
    segments
}

/// Move a frame's queued draws into the reusable recording scratch.
///
/// Both allocations survive: `queue` is drained (not replaced), and `scratch` is
/// cleared and refilled in place. This runs once per frame on the presentation
/// path, so handing either allocation back to the allocator would mean growing
/// it again from zero on the very next frame.
///
/// The scratch holds `Option<DrawCommand>` because recording moves commands out
/// — a `Custom` hook is `FnOnce` and must be owned to be called.
fn take_queue_into(scratch: &mut Vec<Option<DrawCommand>>, queue: &mut Vec<DrawCommand>) {
    scratch.clear();
    scratch.extend(queue.drain(..).map(Some));
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

/// Internal key identifying one of VSE's always-available built-in pipelines.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum BuiltinPipeline {
    FlatColor,
    Textured,
    Grating,
    Gabor,
    AdditiveGabor,
    SubtractiveGabor,
    Dot,
    MeshNormals,
}

impl BuiltinPipeline {
    /// Stable legacy metadata name for this built-in.
    fn name(self) -> &'static str {
        match self {
            Self::FlatColor => "FlatColor",
            Self::Textured => "Textured",
            Self::Grating => "Grating",
            Self::Gabor => "Gabor",
            Self::AdditiveGabor => "AdditiveGabor",
            Self::SubtractiveGabor => "SubtractiveGabor",
            Self::Dot => "Dot",
            Self::MeshNormals => "MeshNormals",
        }
    }
}

/// Stable, sorted names retained in `HostInfo` for serialized compatibility.
pub(crate) fn builtin_pipeline_names() -> Vec<&'static str> {
    let mut names: Vec<_> = builtin_pipeline_descriptors()
        .into_iter()
        .map(|descriptor| descriptor.key.name())
        .collect();
    names.sort_unstable();
    names
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

/// The complete list of built-in pipelines constructed for every context.
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

/// Tally of Tier 1 draws whose registered pipeline is no longer present.
#[derive(Default, Debug)]
pub(crate) struct SkippedDraws {
    registered: HashMap<u64, u64>,
}

impl SkippedDraws {
    /// Tally a skipped Tier 1 registered draw. Returns `true` only on the first
    /// skip for this pipeline id.
    fn record_registered(&mut self, id: u64) -> bool {
        let count = self.registered.entry(id).or_insert(0);
        *count += 1;
        *count == 1
    }

    pub(crate) fn total(&self) -> u64 {
        self.registered.values().sum()
    }
}

/// The built graphics pipelines, keyed by [`BuiltinPipeline`].
///
/// Owns the `Arc<GraphicsPipeline>`s that used to live in named `Renderer`
/// fields. Lookup happens once per pass (not per pixel), so a `HashMap` is fine.
pub(crate) struct Pipelines {
    pipelines: HashMap<BuiltinPipeline, Arc<GraphicsPipeline>>,
}

impl Pipelines {
    fn new(
        device: &Arc<Device>,
        swapchain_format: Format,
        depth_format: Format,
    ) -> Result<Self, RendererError> {
        let mut pipelines = HashMap::new();
        for descriptor in builtin_pipeline_descriptors() {
            let pipeline = (descriptor.build)(device, swapchain_format, depth_format)?;
            pipelines.insert(descriptor.key, pipeline);
        }
        Ok(Self { pipelines })
    }

    fn get(&self, key: BuiltinPipeline) -> &Arc<GraphicsPipeline> {
        self.pipelines
            .get(&key)
            .expect("every built-in pipeline is constructed at initialization")
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

/// The Renderer manages graphics pipelines and converts draw commands
/// into GPU command buffers.
pub(crate) struct Renderer {
    device: Arc<Device>,
    queue: Arc<Queue>,
    command_buffer_allocator: Arc<dyn CommandBufferAllocator>,
    memory_allocator: Arc<StandardMemoryAllocator>,
    descriptor_set_allocator: Arc<dyn DescriptorSetAllocator>,

    pipelines: Pipelines,
    /// Arena allocator for per-frame streaming vertex data.
    ///
    /// Every 2D draw uploads a small vertex or instance buffer that lives for
    /// exactly one frame. Creating a fresh `Buffer` for each meant a
    /// `vkCreateBuffer` + `vkBindBufferMemory` + deferred destroy per draw, per
    /// frame, on the presentation path. This suballocates them all from a
    /// pooled arena instead: in steady state no Vulkan buffer is created at all.
    ///
    /// Arenas return to the pool once every subbuffer cut from them is dropped,
    /// and the command buffer holds its subbuffers until the GPU is done — so
    /// reuse is already gated on frame completion, with no manual
    /// frames-in-flight bookkeeping.
    ///
    /// Streaming only. Data that outlives a frame (the dot unit quad, mesh
    /// vertex/index buffers) stays in its own device-local `Buffer`.
    vertex_allocator: SubbufferAllocator,
    dot_quad_buffer: Subbuffer<[DotInstance]>,
    depth_format: Format,
    depth_views: Vec<Arc<ImageView>>,

    textures: HashMap<u64, TextureResources>,
    /// Noise textures keyed by the parameters that generated them, so a
    /// repeated `draw_noise` reuses its upload instead of regenerating pixels
    /// and blocking on a fence every frame. Bounded, because animated noise
    /// brings a new seed (and so a new key) on every update.
    noise_cache: NoiseTextureCache,
    /// Textures evicted from `noise_cache` this frame, released only after the
    /// command buffer is built. Freeing them earlier could drop a texture that
    /// an already-queued `DrawCommand::Noise` still needs to record.
    pending_noise_unloads: Vec<u64>,
    models: HashMap<u64, ModelResources>,
    next_model_id: u64,
    next_texture_id: u64,

    draw_commands: Vec<DrawCommand>,
    draw_commands_3d: Vec<DrawCommand3D>,
    flat_vertex_scratch: Vec<Vertex2D>,
    dot_instance_scratch: Vec<DotInstance>,
    /// Reusable per-frame recording buffers. Held on `self` so their
    /// allocations survive between frames instead of being rebuilt on the
    /// presentation path; swapped out for the duration of `render` so the
    /// built-in draw arms can still borrow `self` mutably.
    command_scratch: Vec<Option<DrawCommand>>,
    kind_scratch: Vec<DrawKind>,

    /// Registered draws whose pipeline was removed before recording.
    skipped_draws: RefCell<SkippedDraws>,

    /// User-registered Tier 1 pipelines, keyed by the id handed back in a
    /// [`RegisteredPipeline`](super::pipeline::RegisteredPipeline). Type-erased
    /// so the registry is homogeneous; each entry recovers its concrete
    /// `Command` when recording (see `super::pipeline`).
    registered: HashMap<u64, Box<dyn ErasedStimulusPipeline>>,
    /// Next id to assign on [`register`](Self::register).
    next_registered_id: u64,
    /// Identifies this registry, so a [`RegisteredPipeline`] handle issued
    /// elsewhere is rejected instead of resolving to an unrelated pipeline.
    ///
    /// [`RegisteredPipeline`]: super::pipeline::RegisteredPipeline
    registry_id: RegistryId,
}

impl Renderer {
    /// Create a renderer with the complete built-in pipeline set.
    pub fn new(
        device: Arc<Device>,
        queue: Arc<Queue>,
        swapchain_format: Format,
        image_count: usize,
        extent: [u32; 2],
    ) -> Result<Self, RendererError> {
        let command_buffer_allocator: Arc<dyn CommandBufferAllocator> = Arc::new(
            StandardCommandBufferAllocator::new(device.clone(), Default::default()),
        );
        let memory_allocator = Arc::new(StandardMemoryAllocator::new_default(device.clone()));
        let descriptor_set_allocator: Arc<dyn DescriptorSetAllocator> = Arc::new(
            StandardDescriptorSetAllocator::new(device.clone(), Default::default()),
        );

        let vertex_allocator = SubbufferAllocator::new(
            memory_allocator.clone(),
            SubbufferAllocatorCreateInfo {
                buffer_usage: BufferUsage::VERTEX_BUFFER,
                memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                    | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                ..Default::default()
            },
        );

        let dot_quad_buffer = Self::create_dot_quad_buffer(memory_allocator.clone())?;
        let depth_format = Self::select_depth_format(&device)?;
        let pipelines = Pipelines::new(&device, swapchain_format, depth_format)?;
        let depth_views =
            Self::create_depth_views(memory_allocator.clone(), depth_format, image_count, extent)?;

        Ok(Self {
            device,
            queue,
            command_buffer_allocator,
            memory_allocator,
            descriptor_set_allocator,
            pipelines,
            vertex_allocator,
            dot_quad_buffer,
            depth_format,
            depth_views,
            textures: HashMap::new(),
            noise_cache: NoiseTextureCache::new(NOISE_CACHE_CAPACITY),
            pending_noise_unloads: Vec::new(),
            models: HashMap::new(),
            next_model_id: 0,
            next_texture_id: 0,
            draw_commands: Vec::new(),
            draw_commands_3d: Vec::with_capacity(16),
            flat_vertex_scratch: Vec::new(),
            dot_instance_scratch: Vec::new(),
            command_scratch: Vec::new(),
            kind_scratch: Vec::new(),
            skipped_draws: RefCell::new(SkippedDraws::default()),
            registered: HashMap::new(),
            next_registered_id: 0,
            registry_id: RegistryId::next(),
        })
    }

    /// Register a user Tier 1 [`StimulusPipeline`]: build it once (via
    /// [`build`](StimulusPipeline::build)) and store it type-erased under a
    /// fresh id, which the caller wraps in a
    /// [`RegisteredPipeline`](super::pipeline::RegisteredPipeline).
    ///
    /// `color_format` is the swapchain color format (the renderer does not store
    /// it); it and `self.depth_format` are handed to the pipeline's
    /// [`PipelineBuildCtx`] so it can create pipelines matching VSE's passes.
    pub fn register<P: StimulusPipeline>(
        &mut self,
        pipeline: P,
        color_format: Format,
    ) -> Result<RegisteredPipeline<P::Command>, PipelineError> {
        let cx = PipelineBuildCtx {
            device: &self.device,
            color_format,
            depth_format: self.depth_format,
            memory_allocator: &self.memory_allocator,
        };
        let resources = pipeline.build(&cx)?;
        let id = self.next_registered_id;
        self.next_registered_id = self.next_registered_id.wrapping_add(1);
        self.registered.insert(
            id,
            Box::new(RegisteredEntry {
                pipeline,
                resources,
            }),
        );
        Ok(RegisteredPipeline::new(self.registry_id, id))
    }

    /// Drop a registered pipeline and its GPU resources.
    ///
    /// Registering per trial without this leaks a `GraphicsPipeline` per
    /// registration for the life of the session. Returns whether a pipeline was
    /// actually removed.
    pub fn unregister<C: 'static>(&mut self, handle: RegisteredPipeline<C>) -> bool {
        if !handle.belongs_to(self.registry_id) {
            return false;
        }
        self.registered.remove(&handle.id()).is_some()
    }

    /// Enqueue a Tier 1 registered draw: pushes a [`DrawCommand::Registered`]
    /// carrying the type-erased `Command` payload, so it composites in call
    /// order interleaved with the built-in draws queued around it.
    ///
    /// # Errors
    ///
    /// [`PipelineError::ForeignHandle`] if the handle came from another VSE
    /// context, whose ids mean nothing here.
    pub fn push_registered<C: 'static>(
        &mut self,
        handle: RegisteredPipeline<C>,
        command: C,
    ) -> Result<(), PipelineError> {
        if !handle.belongs_to(self.registry_id) {
            return Err(PipelineError::ForeignHandle);
        }
        self.draw_commands.push(DrawCommand::Registered {
            id: handle.id(),
            payload: Box::new(command),
        });
        Ok(())
    }

    /// Tally a registered draw skipped because no pipeline is registered under
    /// its id, logging on the first occurrence. Defensive: a valid handle
    /// always resolves.
    fn warn_absent_registered(&self, id: u64) {
        if self.skipped_draws.borrow_mut().record_registered(id) {
            tracing::error!(
                "skipping registered draw: no pipeline registered under id {id} \
                 (was register_pipeline called on this VSE context?). \
                 This and any further skips are counted by skipped_draw_count()"
            );
        }
    }

    /// Total registered draws skipped this session.
    pub(crate) fn skipped_draw_total(&self) -> u64 {
        self.skipped_draws.borrow().total()
    }

    /// Push a draw command onto the queue.
    pub fn push(&mut self, command: DrawCommand) {
        self.draw_commands.push(command);
    }

    pub fn push_3d(&mut self, command: DrawCommand3D) {
        self.draw_commands_3d.push(command);
    }

    /// Queue a user-supplied raw record hook (Tier 2). Recorded on its own in
    /// call order via [`DrawCommand::Custom`], so it composites interleaved with
    /// the built-in draws queued around it rather than always running last.
    pub fn push_custom(&mut self, record: CustomDrawFn) {
        self.draw_commands.push(DrawCommand::Custom(record));
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

    /// Suballocate a per-frame vertex/instance buffer from the arena and fill
    /// it with `data`.
    ///
    /// Replaces a per-draw `Buffer::from_iter`. Generic over the vertex type,
    /// so one arena serves flat vertices, textured quads, and dot instances
    /// alike. `data` must be non-empty — a zero-length draw is skipped by its
    /// caller's degenerate-input guard before reaching here.
    fn upload_vertices<T>(&self, data: &[T]) -> Result<Subbuffer<[T]>, RendererError>
    where
        T: BufferContents + Copy,
    {
        debug_assert!(!data.is_empty(), "callers guard against empty vertex data");
        let buffer = self
            .vertex_allocator
            .allocate_slice::<T>(data.len() as u64)
            .map_err(|e| RendererError::BufferAllocationFailed(e.to_string()))?;
        buffer
            .write()
            .map_err(|e| RendererError::BufferAllocationFailed(e.to_string()))?
            .copy_from_slice(data);
        Ok(buffer)
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
            None,
        )
    }

    /// Like [`render`](Self::render), but appends a copy of the finished image
    /// into `readback` — the headless (offscreen) path.
    ///
    /// The copy is recorded *after* every draw, in the same command buffer, so
    /// it cannot perturb what was rendered: the drawing commands recorded here
    /// are the same ones a windowed session records. That identity is the whole
    /// point of reusing this function rather than writing a parallel one.
    pub fn render_to_offscreen(
        &mut self,
        target_image: Arc<Image>,
        image_index: usize,
        clear_color: [f32; 4],
        viewport_extent: [u32; 2],
        readback: &Subbuffer<[u8]>,
    ) -> Result<Arc<PrimaryAutoCommandBuffer>, RendererError> {
        self.render_with_underlay(
            target_image,
            image_index,
            clear_color,
            viewport_extent,
            None,
            Some(readback),
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
        readback: Option<&Subbuffer<[u8]>>,
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

        let mesh_normals_pipeline = self.pipelines.get(BuiltinPipeline::MeshNormals).clone();
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
        // every other primitive (Texture/Noise/Grating/Gabor/Dots) — and each
        // Tier 2 Custom hook — records on its own. This replaces the former
        // type-ordered passes (all flats, then textures, then gratings/gabors,
        // then dots, then custom draws last) with call-order compositing, while
        // preserving the flat-color batch for each run of consecutive flats
        // (docs/design/pipeline-flexibility.md §4-5). The per-command recording
        // — buffers, push constants, blend/two-pass selection, descriptor sets,
        // dot instancing, and degenerate-input guards — is unchanged; only the order and the
        // flat-coalescing scope differ.
        //
        // Ownership: a `Custom` command holds a `FnOnce` that must be MOVED to be
        // called, so the queue is drained into `Vec<Option<DrawCommand>>` and
        // each recorded command is `.take()`n out. Every index belongs to
        // exactly one segment, so each is taken at most once. An empty queue
        // yields an empty plan and records nothing — a perfect no-op.
        //
        // The scratch buffers are owned by `self` and swapped out for the
        // duration of the frame: taking them here releases the `self` borrow so
        // the built-in arms below can use `&mut self`, and returning them at the
        // end preserves their capacity for the next frame.
        let mut commands = std::mem::take(&mut self.command_scratch);
        take_queue_into(&mut commands, &mut self.draw_commands);
        let mut kinds = std::mem::take(&mut self.kind_scratch);
        kinds.clear();
        kinds.extend(
            commands
                .iter()
                .map(|c| draw_kind(c.as_ref().expect("the queue was just filled"))),
        );
        // Built once for the frame and handed to every Tier 1 registered draw
        // and every Tier 2 custom hook. Holds cloned `Arc`s (not `self`
        // borrows), so it does not conflict with the mutable `self` uses in the
        // built-in arms below.
        let record_ctx = RecordCtx {
            viewport_extent,
            memory_allocator: self.memory_allocator.clone(),
            device: self.device.clone(),
        };
        for segment in plan_render_segments(&kinds) {
            match segment {
                RenderSegment::FlatRun { start, end } => {
                    self.fill_flat_run(&commands, start, end);
                    if self.flat_vertex_scratch.is_empty() {
                        continue;
                    }
                    let flat_color_pipeline = self.pipelines.get(BuiltinPipeline::FlatColor);
                    let vertex_buffer = self.upload_vertices(&self.flat_vertex_scratch)?;

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
                RenderSegment::RegisteredRun { id, start, end } => {
                    // Move the run's payloads out (each belongs to exactly one
                    // segment) and hand the whole run to the pipeline, so N
                    // consecutive `draw_with` calls become one `record` — and
                    // can become one instanced draw.
                    let payloads: Vec<Box<dyn std::any::Any>> = commands[start..end]
                        .iter_mut()
                        .map(|slot| {
                            let cmd = slot.take().expect("each command is recorded exactly once");
                            match cmd {
                                DrawCommand::Registered { payload, .. } => payload,
                                _ => unreachable!("a RegisteredRun holds only registered draws"),
                            }
                        })
                        .collect();

                    // `self.registered` is a distinct field and `record_ctx`
                    // owns cloned `Arc`s rather than borrowing `self`, so the
                    // mutable pipeline borrow coexists with `builder`.
                    match self.registered.get_mut(&id) {
                        Some(pipeline) => {
                            pipeline
                                .record_erased(&mut builder, &record_ctx, payloads)
                                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
                        }
                        // Warn-once needs `&self` while `registered` is
                        // mutably borrowed above, so it happens after.
                        None => self.warn_absent_registered(id),
                    }
                }
                RenderSegment::Single(i) => {
                    // Take ownership so a Custom closure (FnOnce) can be moved
                    // out and invoked; each index belongs to exactly one segment.
                    let cmd = commands[i]
                        .take()
                        .expect("each command is recorded exactly once");
                    // Custom draws move their closure out and run it in place;
                    // registered (Tier 1) draws dispatch to their user pipeline;
                    // every built-in records by reference exactly as before.
                    if let DrawCommand::Custom(record) = cmd {
                        record(&mut builder, &record_ctx);
                    } else {
                        match &cmd {
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

                                let textured_pipeline =
                                    self.pipelines.get(BuiltinPipeline::Textured);

                                let resources = self
                                    .textures
                                    .get(&texture_id)
                                    .ok_or(RendererError::TextureNotFound(texture_id))?;

                                let tex_vertices = textured_quad_vertices(left, top, right, bottom);
                                let vertex_buffer = self.upload_vertices(&tex_vertices)?;

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
                                    builder.draw(6, 1, 0, 0).map_err(|e| {
                                        RendererError::RecordingFailed(e.to_string())
                                    })?;
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
                                let vertex_buffer = self.upload_vertices(&quad)?;

                                let first_pass = if is_grating {
                                    (self.pipelines.get(BuiltinPipeline::Grating), 0u32)
                                } else if additive {
                                    (self.pipelines.get(BuiltinPipeline::AdditiveGabor), 1u32)
                                } else {
                                    (self.pipelines.get(BuiltinPipeline::Gabor), 0u32)
                                };
                                // Normalized swapchain formats cannot reliably carry a negative
                                // fragment source into ONE+ONE blending. Split signed modulation
                                // into positive-add and negative-magnitude subtract passes.
                                let second_pass = additive.then(|| {
                                    (self.pipelines.get(BuiltinPipeline::SubtractiveGabor), 2u32)
                                });

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
                                        .map_err(|e| {
                                            RendererError::RecordingFailed(e.to_string())
                                        })?;
                                    unsafe {
                                        builder.draw(6, 1, 0, 0).map_err(|e| {
                                            RendererError::RecordingFailed(e.to_string())
                                        })?;
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

                                let instance_buffer =
                                    self.upload_vertices(&self.dot_instance_scratch)?;

                                let dot_pipeline = self.pipelines.get(BuiltinPipeline::Dot);

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
                                    .bind_vertex_buffers(
                                        0,
                                        (self.dot_quad_buffer.clone(), instance_buffer),
                                    )
                                    .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
                                unsafe {
                                    builder.draw(6, instance_count, 0, 0).map_err(|e| {
                                        RendererError::RecordingFailed(e.to_string())
                                    })?;
                                }
                            }
                            // Flat-color commands are recorded only inside FlatRun
                            // segments; the planner never emits them as Single.
                            DrawCommand::Rect { .. }
                            | DrawCommand::Circle { .. }
                            | DrawCommand::Line { .. }
                            | DrawCommand::Arc { .. } => {
                                unreachable!(
                                    "flat-color commands are recorded via FlatRun segments"
                                )
                            }
                            // Custom draws are handled by the `if let` above.
                            DrawCommand::Custom(_) => {
                                unreachable!("custom draws are moved out and run above")
                            }
                            // Registered draws are recorded via RegisteredRun
                            // segments; the planner never emits them as Single.
                            DrawCommand::Registered { .. } => {
                                unreachable!("registered draws are recorded via RegisteredRun")
                            }
                        }
                    }
                }
            }
        }

        builder
            .end_rendering()
            .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;

        // Headless readback: every draw is already recorded, so this copy sees
        // the finished frame and cannot alter it.
        if let Some(readback) = readback {
            builder
                .copy_image_to_buffer(CopyImageToBufferInfo::image_buffer(
                    target_image.clone(),
                    readback.clone(),
                ))
                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
        }

        let command_buffer = builder
            .build()
            .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;

        // Hand the scratch allocations back for the next frame. Every command
        // has been recorded, so the slots hold only `None`.
        commands.clear();
        self.command_scratch = commands;
        self.kind_scratch = kinds;

        // Safe to release evicted noise textures now: the command buffer holds
        // its own references to everything it recorded.
        for texture_id in std::mem::take(&mut self.pending_noise_unloads) {
            self.textures.remove(&texture_id);
        }

        // `self.draw_commands` was drained into the scratch above.
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

        // Create a descriptor set using the always-available textured pipeline.
        let layout = self
            .pipelines
            .get(BuiltinPipeline::Textured)
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

    /// The cached noise texture for `params`, if one is already uploaded.
    ///
    /// Checked before generating pixels, so a cache hit skips the CPU noise
    /// generation as well as the GPU upload.
    pub fn cached_noise_texture(&self, params: &crate::drawing::NoiseParams) -> Option<u64> {
        self.noise_cache.get(&NoiseKey::of(params))
    }

    /// Upload a freshly generated noise texture and cache it under `params`.
    ///
    /// Any texture evicted to stay within the cache bound is queued for release
    /// at the end of the current frame.
    pub fn insert_noise_texture(
        &mut self,
        params: &crate::drawing::NoiseParams,
        pixels: &[u8],
    ) -> Result<u64, RendererError> {
        let handle = self.load_texture_rgba(params.width, params.height, pixels)?;
        let evicted = self.noise_cache.insert(NoiseKey::of(params), handle.id);
        self.pending_noise_unloads.extend(evicted);
        Ok(handle.id)
    }

    /// Remove a texture and free its GPU resources.
    pub fn unload_texture(&mut self, handle: TextureHandle) {
        self.textures.remove(&handle.id);
    }

    /// Coalesce the flat-color commands in `commands[start..end]` (a run of
    /// consecutive flats, per the call-order plan) into `flat_vertex_scratch`,
    /// ready for a single flat-color draw. The scratch buffer is cleared first,
    /// so each run flushes independently. Flats are only read, never moved, so
    /// each slot in the run is still `Some`.
    fn fill_flat_run(&mut self, commands: &[Option<DrawCommand>], start: usize, end: usize) {
        // Split out so the coalescing logic can be exercised without a device.
        let mut scratch = std::mem::take(&mut self.flat_vertex_scratch);
        Self::fill_flat_run_into(&mut scratch, commands, start, end);
        self.flat_vertex_scratch = scratch;
    }

    /// Device-free core of [`fill_flat_run`](Self::fill_flat_run): coalesce
    /// `commands[start..end]` into `scratch`, which is cleared first so each run
    /// flushes independently of the last.
    fn fill_flat_run_into(
        scratch: &mut Vec<Vertex2D>,
        commands: &[Option<DrawCommand>],
        start: usize,
        end: usize,
    ) {
        scratch.clear();
        for cmd in &commands[start..end] {
            let cmd = cmd
                .as_ref()
                .expect("flat commands are read in place, never taken");
            Self::append_flat_command_vertices(scratch, cmd);
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
            DrawCommand::Custom(_) => {}
            DrawCommand::Registered { .. } => {}
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

    // --- Degenerate flat-color inputs ---
    //
    // A stimulus parameter can legitimately sweep to zero (a contracting
    // aperture, a staircase converging on nothing). Those must emit no
    // geometry rather than NaN-filled or back-facing triangles.

    #[test]
    fn degenerate_flat_shapes_emit_no_geometry() {
        let degenerate = [
            // Inverted and empty rectangles.
            DrawCommand::Rect {
                left: 10.0,
                top: 0.0,
                right: 10.0,
                bottom: 5.0,
                color: Color::WHITE,
            },
            DrawCommand::Rect {
                left: 20.0,
                top: 5.0,
                right: 10.0,
                bottom: 0.0,
                color: Color::WHITE,
            },
            // Zero and negative radius.
            DrawCommand::Circle {
                cx: 0.0,
                cy: 0.0,
                radius: 0.0,
                color: Color::WHITE,
                segments: 32,
            },
            DrawCommand::Circle {
                cx: 0.0,
                cy: 0.0,
                radius: -3.0,
                color: Color::WHITE,
                segments: 32,
            },
            // Zero width, and zero length.
            DrawCommand::Line {
                x1: 0.0,
                y1: 0.0,
                x2: 10.0,
                y2: 10.0,
                width: 0.0,
                color: Color::WHITE,
            },
            DrawCommand::Line {
                x1: 4.0,
                y1: 4.0,
                x2: 4.0,
                y2: 4.0,
                width: 2.0,
                color: Color::WHITE,
            },
            // Zero thickness, and a vanishing angular span.
            DrawCommand::Arc {
                cx: 0.0,
                cy: 0.0,
                radius: 5.0,
                start_angle: 0.0,
                end_angle: 1.0,
                thickness: 0.0,
                color: Color::WHITE,
                segments: 32,
            },
            DrawCommand::Arc {
                cx: 0.0,
                cy: 0.0,
                radius: 5.0,
                start_angle: 1.0,
                end_angle: 1.0,
                thickness: 2.0,
                color: Color::WHITE,
                segments: 32,
            },
        ];

        for cmd in &degenerate {
            let mut scratch = Vec::new();
            Renderer::append_flat_command_vertices(&mut scratch, cmd);
            assert!(
                scratch.is_empty(),
                "a degenerate shape must contribute no vertices"
            );
        }
    }

    #[test]
    fn a_valid_flat_shape_still_emits_geometry() {
        // Guards the guards: if the degenerate test above passed because
        // nothing ever emits vertices, this fails.
        let mut scratch = Vec::new();
        Renderer::append_flat_command_vertices(&mut scratch, &a_rect());
        assert_eq!(scratch.len(), 6, "a rectangle is two triangles");
    }

    #[test]
    fn each_flat_run_starts_from_an_empty_scratch() {
        // Runs flush independently. If the scratch leaked between them, the
        // second run would redraw the first run's geometry on top of whatever
        // was composited in between.
        let commands: Vec<Option<DrawCommand>> = (0..4).map(|_| Some(a_rect())).collect::<Vec<_>>();

        let mut scratch = Vec::new();
        Renderer::fill_flat_run_into(&mut scratch, &commands, 0, 3);
        assert_eq!(scratch.len(), 18, "three rectangles");

        Renderer::fill_flat_run_into(&mut scratch, &commands, 3, 4);
        assert_eq!(
            scratch.len(),
            6,
            "the second run holds only its own geometry"
        );
    }

    // --- Per-frame queue recycling ---
    //
    // The draw queue is rebuilt every frame on the presentation path. Handing
    // its allocation back to the allocator each frame means re-growing it from
    // zero on the next one, so both the queue and the scratch it drains into
    // must keep their capacity.

    #[test]
    fn draining_the_queue_empties_it_without_surrendering_its_capacity() {
        let mut queue: Vec<DrawCommand> = (0..64).map(|_| a_rect()).collect();
        let capacity_before = queue.capacity();
        let mut scratch: Vec<Option<DrawCommand>> = Vec::new();

        take_queue_into(&mut scratch, &mut queue);

        assert!(queue.is_empty(), "the frame's queue is consumed");
        assert_eq!(
            queue.capacity(),
            capacity_before,
            "the queue keeps its allocation for the next frame"
        );
        assert_eq!(scratch.len(), 64);
    }

    #[test]
    fn the_scratch_is_reused_across_frames_rather_than_reallocated() {
        let mut scratch: Vec<Option<DrawCommand>> = Vec::new();

        let mut frame_one: Vec<DrawCommand> = (0..64).map(|_| a_rect()).collect();
        take_queue_into(&mut scratch, &mut frame_one);
        let capacity_after_first_frame = scratch.capacity();

        // A later, smaller frame must reuse the existing allocation.
        let mut frame_two: Vec<DrawCommand> = (0..8).map(|_| a_rect()).collect();
        take_queue_into(&mut scratch, &mut frame_two);

        assert_eq!(
            scratch.len(),
            8,
            "stale commands from the last frame are gone"
        );
        assert_eq!(
            scratch.capacity(),
            capacity_after_first_frame,
            "the scratch keeps its allocation instead of reallocating each frame"
        );
    }

    fn a_rect() -> DrawCommand {
        DrawCommand::Rect {
            left: 0.0,
            top: 0.0,
            right: 1.0,
            bottom: 1.0,
            color: Color::WHITE,
        }
    }

    #[test]
    fn skipped_draws_counts_registered_pipelines_after_the_warning() {
        let mut skipped = SkippedDraws::default();

        assert!(skipped.record_registered(7));
        assert!(!skipped.record_registered(7));
        assert!(skipped.record_registered(9));

        assert_eq!(skipped.total(), 3);
    }

    #[test]
    fn a_session_that_skipped_nothing_reports_zero() {
        // The healthy case has to be unambiguous: this is what an experimenter
        // checks to confirm no frame was presented missing a stimulus.
        let skipped = SkippedDraws::default();
        assert_eq!(skipped.total(), 0);
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
        let kinds = [DrawKind::Other, DrawKind::Other, DrawKind::Other];
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
        let kinds = [DrawKind::Flat, DrawKind::Other, DrawKind::Flat];
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
            DrawKind::Other,
            DrawKind::Flat,
            DrawKind::Flat,
            DrawKind::Other,
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
        let kinds = [DrawKind::Other, DrawKind::Flat, DrawKind::Flat];
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
            assert_eq!(draw_kind(cmd), DrawKind::Other);
        }
    }

    #[test]
    fn draw_kind_classifies_custom_as_non_flat() {
        // A Tier 2 custom draw records on its own, so it must never coalesce
        // into a flat run — it is NonFlat like every other self-recording draw.
        let cmd = DrawCommand::Custom(Box::new(|_, _| {}));
        assert_eq!(draw_kind(&cmd), DrawKind::Other);
    }

    #[test]
    fn draw_kind_tags_a_registered_draw_with_its_pipeline_id() {
        // A registered draw carries its pipeline identity so consecutive draws
        // to the SAME pipeline can coalesce, while draws to different pipelines
        // stay separate.
        let cmd = DrawCommand::Registered {
            id: 3,
            payload: Box::new(7u32),
        };
        assert_eq!(draw_kind(&cmd), DrawKind::Registered(3));
    }

    // --- Registered-run coalescing ---
    //
    // `StimulusPipeline::record` takes a SLICE of commands. That slice is only
    // honest if consecutive draws to one pipeline actually arrive together, so
    // a pipeline can answer a run of N draws with one instanced draw.

    #[test]
    fn consecutive_draws_to_one_pipeline_coalesce_into_a_single_run() {
        let kinds = [
            DrawKind::Registered(1),
            DrawKind::Registered(1),
            DrawKind::Registered(1),
        ];
        assert_eq!(
            plan_render_segments(&kinds),
            vec![RenderSegment::RegisteredRun {
                id: 1,
                start: 0,
                end: 3
            }]
        );
    }

    #[test]
    fn adjacent_draws_to_different_pipelines_do_not_coalesce() {
        // Two pipelines back to back are still two records — merging them would
        // hand one pipeline another's commands.
        let kinds = [DrawKind::Registered(1), DrawKind::Registered(2)];
        assert_eq!(
            plan_render_segments(&kinds),
            vec![
                RenderSegment::RegisteredRun {
                    id: 1,
                    start: 0,
                    end: 1
                },
                RenderSegment::RegisteredRun {
                    id: 2,
                    start: 1,
                    end: 2
                },
            ]
        );
    }

    #[test]
    fn a_builtin_draw_between_registered_draws_splits_the_run() {
        // Call-order compositing is the invariant that outranks batching: a
        // built-in drawn between two registered draws must land between them on
        // screen, so the run has to split.
        let kinds = [
            DrawKind::Registered(1),
            DrawKind::Flat,
            DrawKind::Registered(1),
        ];
        assert_eq!(
            plan_render_segments(&kinds),
            vec![
                RenderSegment::RegisteredRun {
                    id: 1,
                    start: 0,
                    end: 1
                },
                RenderSegment::FlatRun { start: 1, end: 2 },
                RenderSegment::RegisteredRun {
                    id: 1,
                    start: 2,
                    end: 3
                },
            ]
        );
    }

    #[test]
    fn registered_runs_interleave_with_flat_runs_and_singles_in_call_order() {
        // R1 R1 F F T R2 R2 F  ->  run(R1,0..2) run(F,2..4) single(4) run(R2,5..7) run(F,7..8)
        let kinds = [
            DrawKind::Registered(1),
            DrawKind::Registered(1),
            DrawKind::Flat,
            DrawKind::Flat,
            DrawKind::Other,
            DrawKind::Registered(2),
            DrawKind::Registered(2),
            DrawKind::Flat,
        ];
        assert_eq!(
            plan_render_segments(&kinds),
            vec![
                RenderSegment::RegisteredRun {
                    id: 1,
                    start: 0,
                    end: 2
                },
                RenderSegment::FlatRun { start: 2, end: 4 },
                RenderSegment::Single(4),
                RenderSegment::RegisteredRun {
                    id: 2,
                    start: 5,
                    end: 7
                },
                RenderSegment::FlatRun { start: 7, end: 8 },
            ]
        );
    }

    #[test]
    fn every_index_belongs_to_exactly_one_segment() {
        // The render loop moves each command out of the queue exactly once
        // (a Custom hook is FnOnce), so the plan must partition the indices.
        let kinds = [
            DrawKind::Flat,
            DrawKind::Registered(1),
            DrawKind::Registered(1),
            DrawKind::Other,
            DrawKind::Flat,
            DrawKind::Flat,
            DrawKind::Registered(2),
        ];

        let mut covered: Vec<usize> = Vec::new();
        for segment in plan_render_segments(&kinds) {
            match segment {
                RenderSegment::FlatRun { start, end }
                | RenderSegment::RegisteredRun { start, end, .. } => covered.extend(start..end),
                RenderSegment::Single(i) => covered.push(i),
            }
        }

        assert_eq!(
            covered,
            (0..kinds.len()).collect::<Vec<_>>(),
            "the plan must cover every index exactly once, in order"
        );
    }
}
