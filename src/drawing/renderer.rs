use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;

use vulkano::{
    buffer::{Buffer, BufferCreateInfo, BufferUsage},
    command_buffer::{
        allocator::{CommandBufferAllocator, StandardCommandBufferAllocator},
        AutoCommandBufferBuilder, CommandBufferUsage as CmdBufUsage, CopyBufferToImageInfo,
        PrimaryAutoCommandBuffer, PrimaryCommandBufferAbstract, RenderingAttachmentInfo,
        RenderingInfo,
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
            vertex_input::{Vertex as VertexTrait, VertexDefinition},
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
    circle_vertices, line_vertices, rect_vertices, textured_quad_vertices, DrawCommand,
};
use super::texture::TextureHandle;
use super::vertex::{TexturedVertex, Vertex2D};

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

    textures: HashMap<u64, TextureResources>,
    next_texture_id: u64,

    draw_commands: Vec<DrawCommand>,
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

        Ok(Self {
            device,
            queue,
            command_buffer_allocator,
            memory_allocator,
            descriptor_set_allocator,
            flat_color_pipeline,
            textured_pipeline,
            textures: HashMap::new(),
            next_texture_id: 0,
            draw_commands: Vec::new(),
        })
    }

    /// Push a draw command onto the queue.
    pub fn push(&mut self, command: DrawCommand) {
        self.draw_commands.push(command);
    }

    /// Render all queued commands into a command buffer.
    pub fn render(
        &mut self,
        target_image: Arc<Image>,
        clear_color: [f32; 4],
        viewport_extent: [u32; 2],
    ) -> Result<Arc<PrimaryAutoCommandBuffer>, RendererError> {
        let image_view = ImageView::new_default(target_image)
            .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;

        let mut builder = AutoCommandBufferBuilder::primary(
            self.command_buffer_allocator.clone(),
            self.queue.queue_family_index(),
            CmdBufUsage::OneTimeSubmit,
        )
        .map_err(|e| RendererError::RecordingFailed(e.to_string()))?;

        // Begin rendering with clear
        builder
            .begin_rendering(RenderingInfo {
                color_attachments: vec![Some(RenderingAttachmentInfo {
                    load_op: AttachmentLoadOp::Clear,
                    store_op: AttachmentStoreOp::Store,
                    clear_value: Some(ClearValue::Float(clear_color)),
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
        let flat_vertices = self.generate_flat_color_vertices();
        if !flat_vertices.is_empty() {
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
                flat_vertices.into_iter(),
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

        // Textured draws
        let texture_commands: Vec<_> = self
            .draw_commands
            .iter()
            .filter_map(|cmd| match cmd {
                DrawCommand::Texture {
                    texture_id,
                    left,
                    top,
                    right,
                    bottom,
                } => Some((*texture_id, *left, *top, *right, *bottom)),
                _ => None,
            })
            .collect();

        for (texture_id, left, top, right, bottom) in texture_commands {
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
                tex_vertices.into_iter(),
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

    fn generate_flat_color_vertices(&self) -> Vec<Vertex2D> {
        let mut vertices = Vec::new();

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
                    vertices
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
                    vertices.extend(circle_vertices(*cx, *cy, *radius, *color, *segments));
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
                    vertices.extend_from_slice(&line_vertices(*x1, *y1, *x2, *y2, *width, *color));
                }
                DrawCommand::Texture { .. } => {
                    // Handled separately
                }
            }
        }

        vertices
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
}
