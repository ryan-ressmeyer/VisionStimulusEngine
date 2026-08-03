use std::collections::HashMap;
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
    /// Build the full built-in suite from [`builtin_pipeline_descriptors`].
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

    /// Fetch a built-in pipeline by key. Panics if the key is absent, which for
    /// the always-complete built-in suite is a programming error.
    fn get(&self, key: BuiltinPipeline) -> &Arc<GraphicsPipeline> {
        self.pipelines
            .get(&key)
            .expect("built-in pipeline missing from registry")
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
    flat_vertex_scratch: Vec<Vertex2D>,
    dot_instance_scratch: Vec<DotInstance>,
}

impl Renderer {
    /// Create a new Renderer with compiled pipelines.
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
            dot_quad_buffer,
            depth_format,
            depth_views,
            textures: HashMap::new(),
            models: HashMap::new(),
            next_model_id: 0,
            next_texture_id: 0,
            draw_commands: Vec::new(),
            draw_commands_3d: Vec::with_capacity(16),
            flat_vertex_scratch: Vec::new(),
            dot_instance_scratch: Vec::new(),
        })
    }

    /// Push a draw command onto the queue.
    pub fn push(&mut self, command: DrawCommand) {
        self.draw_commands.push(command);
    }

    pub fn push_3d(&mut self, command: DrawCommand3D) {
        self.draw_commands_3d.push(command);
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
                    let mesh_normals_pipeline = self.pipelines.get(BuiltinPipeline::MeshNormals);
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

        // Generate flat-color vertices from queued commands
        self.fill_flat_color_vertices();
        if !self.flat_vertex_scratch.is_empty() {
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
            let flat_color_pipeline = self.pipelines.get(BuiltinPipeline::FlatColor);
            builder
                .bind_pipeline_graphics(flat_color_pipeline.clone())
                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                .push_constants(
                    flat_color_pipeline.layout().clone(),
                    0,
                    flat_color_vs::PushConstants {
                        viewport_size: [viewport_extent[0] as f32, viewport_extent[1] as f32],
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

        // Textured draws (Texture and Noise both use the textured pipeline)
        for cmd in &self.draw_commands {
            let (texture_id, left, top, right, bottom) = match cmd {
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
                } => (*texture_id, *left, *top, *right, *bottom),
                _ => continue,
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

            let textured_pipeline = self.pipelines.get(BuiltinPipeline::Textured);
            builder
                .bind_pipeline_graphics(textured_pipeline.clone())
                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                .push_constants(
                    textured_pipeline.layout().clone(),
                    0,
                    textured_vs::PushConstants {
                        viewport_size: [viewport_extent[0] as f32, viewport_extent[1] as f32],
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
        for cmd in &self.draw_commands {
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
                _ => continue,
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
                (self.pipelines.get(BuiltinPipeline::Grating), 0u32)
            } else if additive {
                (self.pipelines.get(BuiltinPipeline::AdditiveGabor), 1u32)
            } else {
                (self.pipelines.get(BuiltinPipeline::Gabor), 0u32)
            };
            // Normalized swapchain formats cannot reliably carry a negative
            // fragment source into ONE+ONE blending. Split signed modulation
            // into positive-add and negative-magnitude subtract passes.
            let second_pass =
                additive.then_some((self.pipelines.get(BuiltinPipeline::SubtractiveGabor), 2u32));

            for (pipeline, composite_mode) in std::iter::once(first_pass).chain(second_pass) {
                builder
                    .bind_pipeline_graphics(pipeline.clone())
                    .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                    .push_constants(
                        pipeline.layout().clone(),
                        0,
                        parametric_vs::PushConstants {
                            viewport_size: [viewport_extent[0] as f32, viewport_extent[1] as f32]
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
        for cmd in &self.draw_commands {
            let (positions, radius, color) = match cmd {
                DrawCommand::Dots {
                    positions,
                    radius,
                    color,
                } => (positions, *radius, *color),
                _ => continue,
            };

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

            let c = color.to_array();
            let dot_pipeline = self.pipelines.get(BuiltinPipeline::Dot);
            builder
                .bind_pipeline_graphics(dot_pipeline.clone())
                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                .push_constants(
                    dot_pipeline.layout().clone(),
                    0,
                    dot_vs::PushConstants {
                        viewport_size: [viewport_extent[0] as f32, viewport_extent[1] as f32],
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

        // Create descriptor set
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

    /// Remove a texture and free its GPU resources.
    pub fn unload_texture(&mut self, handle: TextureHandle) {
        self.textures.remove(&handle.id);
    }

    fn fill_flat_color_vertices(&mut self) {
        self.flat_vertex_scratch.clear();

        for cmd in &self.draw_commands {
            match cmd {
                DrawCommand::Rect {
                    left,
                    top,
                    right,
                    bottom,
                    color,
                } => {
                    if left >= right || top >= bottom {
                        continue;
                    }
                    self.flat_vertex_scratch
                        .extend_from_slice(&rect_vertices(*left, *top, *right, *bottom, *color));
                }
                DrawCommand::Circle {
                    cx,
                    cy,
                    radius,
                    color,
                    segments,
                } => {
                    if *radius <= 0.0 {
                        continue;
                    }
                    self.flat_vertex_scratch
                        .extend(circle_vertices(*cx, *cy, *radius, *color, *segments));
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
                        continue;
                    }
                    // Skip zero-length lines
                    let dx = x2 - x1;
                    let dy = y2 - y1;
                    if dx * dx + dy * dy < 1e-12 {
                        continue;
                    }
                    self.flat_vertex_scratch
                        .extend_from_slice(&line_vertices(*x1, *y1, *x2, *y2, *width, *color));
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
                    if *radius <= 0.0 || *thickness <= 0.0 || (end_angle - start_angle).abs() < 1e-6
                    {
                        continue;
                    }
                    self.flat_vertex_scratch.extend(arc_vertices(
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
}
