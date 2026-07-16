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
    format::{ClearValue, Format},
    image::{
        sampler::{Filter, Sampler, SamplerAddressMode, SamplerCreateInfo},
        view::ImageView,
        Image, ImageCreateInfo, ImageType, ImageUsage,
    },
    memory::allocator::{AllocationCreateInfo, MemoryTypeFilter, StandardMemoryAllocator},
    pipeline::{
        graphics::{
            color_blend::{AttachmentBlend, ColorBlendAttachmentState, ColorBlendState},
            input_assembly::{InputAssemblyState, PrimitiveTopology},
            multisample::MultisampleState,
            rasterization::RasterizationState,
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
    circle_vertices, dot_unit_quad_vertices, line_vertices, rect_vertices, textured_quad_vertices,
    DrawCommand,
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
use super::stimuli::WaveType;
use super::texture::TextureHandle;
use super::vertex::{DotInstance, TexturedVertex, Vertex2D};

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
}

/// GPU resources for a loaded texture.
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

    flat_color_pipeline: Arc<GraphicsPipeline>,
    textured_pipeline: Arc<GraphicsPipeline>,
    grating_pipeline: Arc<GraphicsPipeline>,
    gabor_pipeline: Arc<GraphicsPipeline>,
    dot_pipeline: Arc<GraphicsPipeline>,
    dot_quad_buffer: Subbuffer<[DotInstance]>,

    textures: HashMap<u64, TextureResources>,
    next_texture_id: u64,

    draw_commands: Vec<DrawCommand>,
    flat_vertex_scratch: Vec<Vertex2D>,
    dot_instance_scratch: Vec<DotInstance>,
}

impl Renderer {
    /// Create a new Renderer with compiled pipelines.
    pub fn new(
        device: Arc<Device>,
        queue: Arc<Queue>,
        swapchain_format: Format,
    ) -> Result<Self, RendererError> {
        let command_buffer_allocator: Arc<dyn CommandBufferAllocator> = Arc::new(
            StandardCommandBufferAllocator::new(device.clone(), Default::default()),
        );
        let memory_allocator = Arc::new(StandardMemoryAllocator::new_default(device.clone()));
        let descriptor_set_allocator: Arc<dyn DescriptorSetAllocator> = Arc::new(
            StandardDescriptorSetAllocator::new(device.clone(), Default::default()),
        );

        let flat_color_pipeline = Self::create_flat_color_pipeline(&device, swapchain_format)?;
        let textured_pipeline = Self::create_textured_pipeline(&device, swapchain_format)?;
        let grating_pipeline = Self::create_grating_pipeline(&device, swapchain_format)?;
        let gabor_pipeline = Self::create_gabor_pipeline(&device, swapchain_format)?;
        let dot_pipeline = Self::create_dot_pipeline(&device, swapchain_format)?;
        let dot_quad_buffer = Self::create_dot_quad_buffer(memory_allocator.clone())?;

        Ok(Self {
            device,
            queue,
            command_buffer_allocator,
            memory_allocator,
            descriptor_set_allocator,
            flat_color_pipeline,
            textured_pipeline,
            grating_pipeline,
            gabor_pipeline,
            dot_pipeline,
            dot_quad_buffer,
            textures: HashMap::new(),
            next_texture_id: 0,
            draw_commands: Vec::new(),
            flat_vertex_scratch: Vec::new(),
            dot_instance_scratch: Vec::new(),
        })
    }

    /// Push a draw command onto the queue.
    pub fn push(&mut self, command: DrawCommand) {
        self.draw_commands.push(command);
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
        clear_color: [f32; 4],
        viewport_extent: [u32; 2],
    ) -> Result<Arc<PrimaryAutoCommandBuffer>, RendererError> {
        self.render_with_underlay(target_image, clear_color, viewport_extent, None)
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

        // Begin rendering; the underlay (when present) is the background, so
        // load instead of clear.
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
            builder
                .bind_pipeline_graphics(self.flat_color_pipeline.clone())
                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                .push_constants(
                    self.flat_color_pipeline.layout().clone(),
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

            builder
                .bind_pipeline_graphics(self.textured_pipeline.clone())
                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                .push_constants(
                    self.textured_pipeline.layout().clone(),
                    0,
                    textured_vs::PushConstants {
                        viewport_size: [viewport_extent[0] as f32, viewport_extent[1] as f32],
                    },
                )
                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                .bind_descriptor_sets(
                    PipelineBindPoint::Graphics,
                    self.textured_pipeline.layout().clone(),
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
                wave_type,
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
                    match params.wave {
                        WaveType::Sine => 0u32,
                        WaveType::Square => 1u32,
                    },
                ),
                DrawCommand::Gabor {
                    left,
                    top,
                    right,
                    bottom,
                    params,
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
                    0u32,
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

            let pipeline = if is_grating {
                &self.grating_pipeline
            } else {
                &self.gabor_pipeline
            };

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
                        wave_type,
                    },
                )
                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                .bind_vertex_buffers(0, vertex_buffer)
                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
            unsafe {
                builder
                    .draw(6, 1, 0, 0)
                    .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;
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
            builder
                .bind_pipeline_graphics(self.dot_pipeline.clone())
                .map_err(|e| RendererError::RecordingFailed(e.to_string()))?
                .push_constants(
                    self.dot_pipeline.layout().clone(),
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
            .textured_pipeline
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

        Self::create_graphics_pipeline(device, swapchain_format, stages, vertex_input_state)
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

        Self::create_graphics_pipeline(device, swapchain_format, stages, vertex_input_state)
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

        Self::create_graphics_pipeline(device, swapchain_format, stages, vertex_input_state)
    }

    fn create_gabor_pipeline(
        device: &Arc<Device>,
        swapchain_format: Format,
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

        Self::create_graphics_pipeline(device, swapchain_format, stages, vertex_input_state)
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

        Self::create_graphics_pipeline(device, swapchain_format, stages, vertex_input_state)
    }
}
