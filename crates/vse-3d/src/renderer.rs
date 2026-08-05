use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use glam::Mat4;
use serde::{Deserialize, Serialize};
use vision_stimulus_engine::prelude::{RenderContext, VSEError};
use vulkano::buffer::{Buffer, BufferCreateInfo, BufferUsage, Subbuffer};
use vulkano::command_buffer::allocator::StandardCommandBufferAllocator;
use vulkano::command_buffer::{
    AutoCommandBufferBuilder, CommandBufferSubmitInfo, CommandBufferUsage,
    PrimaryAutoCommandBuffer, RenderingAttachmentInfo, RenderingInfo, SemaphoreSubmitInfo,
    SubmitInfo,
};
use vulkano::device::{
    Device, DeviceCreateInfo, DeviceExtensions, DeviceFeatures, Queue, QueueCreateInfo,
};
use vulkano::format::{ClearValue, Format, FormatFeatures};
use vulkano::image::sys::RawImage;
use vulkano::image::view::ImageView;
use vulkano::image::{Image, ImageCreateInfo, ImageType, ImageUsage};
use vulkano::memory::allocator::{AllocationCreateInfo, MemoryTypeFilter, StandardMemoryAllocator};
use vulkano::memory::{
    DedicatedAllocation, DeviceMemory, ExternalMemoryHandleType, ExternalMemoryHandleTypes,
    MemoryAllocateInfo, MemoryPropertyFlags, ResourceMemory,
};
use vulkano::pipeline::graphics::color_blend::{ColorBlendAttachmentState, ColorBlendState};
use vulkano::pipeline::graphics::depth_stencil::{DepthState, DepthStencilState};
use vulkano::pipeline::graphics::input_assembly::{InputAssemblyState, PrimitiveTopology};
use vulkano::pipeline::graphics::multisample::MultisampleState;
use vulkano::pipeline::graphics::rasterization::{CullMode, FrontFace, RasterizationState};
use vulkano::pipeline::graphics::subpass::PipelineRenderingCreateInfo;
use vulkano::pipeline::graphics::vertex_input::{Vertex as _, VertexDefinition};
use vulkano::pipeline::graphics::viewport::{Viewport, ViewportState};
use vulkano::pipeline::graphics::GraphicsPipelineCreateInfo;
use vulkano::pipeline::layout::PipelineDescriptorSetLayoutCreateInfo;
use vulkano::pipeline::{
    DynamicState, GraphicsPipeline, Pipeline, PipelineLayout, PipelineShaderStageCreateInfo,
};
use vulkano::render_pass::{AttachmentLoadOp, AttachmentStoreOp};
use vulkano::sync::semaphore::{
    ExternalSemaphoreHandleType, ExternalSemaphoreHandleTypes, Semaphore, SemaphoreCreateInfo,
};

use vse_external_frame::{
    release_channel, ExternalImageDesc, ExternalRingDesc, RingFormat, RingStateMachine,
    SlotReleaseRx, SyncKind,
};

use crate::model::{
    decode_model, DecodedInstance, ModelError, ModelHandle, ModelInfo, PerspectiveCamera, Vertex3D,
};

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

static NEXT_RENDERER_ID: AtomicU64 = AtomicU64::new(1);
const RING_USAGE: ImageUsage = ImageUsage::COLOR_ATTACHMENT
    .union(ImageUsage::TRANSFER_SRC)
    .union(ImageUsage::TRANSFER_DST);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vse3dConfig {
    pub ring_len: usize,
}

impl Default for Vse3dConfig {
    fn default() -> Self {
        Self { ring_len: 3 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vse3dInfo {
    pub crate_version: String,
    pub device_name: String,
    pub extent: [u32; 2],
    pub color_format: String,
    pub depth_format: String,
    pub ring_len: usize,
    pub pipelines: Vec<String>,
    pub models: Vec<ModelInfo>,
}

#[derive(Debug, thiserror::Error)]
pub enum Vse3dError {
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("VSE integration failed: {0}")]
    Vse(#[from] VSEError),
    #[error("Vulkan setup or rendering failed: {0}")]
    Vulkan(String),
    #[error("external-frame ring failed: {0}")]
    Ring(#[from] vse_external_frame::RingError),
    #[error("unsupported VSE target: {0}")]
    Unsupported(String),
    #[error("vse-3d renderer was used with a different VSE context")]
    ForeignContext,
}

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

struct DrawNormals {
    model_id: u64,
    model_transform: Mat4,
    view_projection: Mat4,
}

struct RingSlot {
    color_view: Arc<ImageView>,
    depth_view: Arc<ImageView>,
    ready: Arc<Semaphore>,
    last_command_buffer: Option<Arc<PrimaryAutoCommandBuffer>>,
}

pub struct Vse3d {
    id: u64,
    consumer_device: Arc<Device>,
    device: Arc<Device>,
    queue: Arc<Queue>,
    command_allocator: Arc<StandardCommandBufferAllocator>,
    memory_allocator: Arc<StandardMemoryAllocator>,
    pipeline: Arc<GraphicsPipeline>,
    depth_format: Format,
    extent: [u32; 2],
    color_format: Format,
    ring: Vec<RingSlot>,
    machine: RingStateMachine,
    release_rx: SlotReleaseRx,
    models: HashMap<u64, ModelResources>,
    next_model_id: u64,
    draws: Vec<DrawNormals>,
}

impl Vse3d {
    pub fn register(vse: &mut RenderContext<'_>, config: Vse3dConfig) -> Result<Self, VSEError> {
        Self::register_inner(vse, config).map_err(VSEError::extension)
    }

    fn register_inner(
        vse: &mut RenderContext<'_>,
        config: Vse3dConfig,
    ) -> Result<Self, Vse3dError> {
        if config.ring_len < 2 {
            return Err(Vse3dError::Unsupported(
                "external frame rings require at least two images".into(),
            ));
        }

        let extent = vse.window_size();
        let extent = [extent.0, extent.1];
        let color_format = vse.color_format();
        let ring_format = ring_format(color_format)?;
        let consumer_device = vse.device().clone();
        let physical = consumer_device.physical_device().clone();
        let supported = physical.supported_extensions();
        if !supported.khr_external_memory_fd || !supported.khr_external_semaphore_fd {
            return Err(Vse3dError::Unsupported(
                "GPU lacks VK_KHR_external_memory_fd or VK_KHR_external_semaphore_fd".into(),
            ));
        }

        let extensions = DeviceExtensions {
            khr_dynamic_rendering: true,
            khr_external_memory_fd: true,
            khr_external_semaphore_fd: true,
            ..DeviceExtensions::empty()
        };
        let features = DeviceFeatures {
            dynamic_rendering: true,
            ..DeviceFeatures::empty()
        };
        let queue_family_index = vse.queue().queue_family_index();
        let (device, mut queues) = Device::new(
            physical,
            DeviceCreateInfo {
                queue_create_infos: vec![QueueCreateInfo {
                    queue_family_index,
                    ..Default::default()
                }],
                enabled_extensions: extensions,
                enabled_features: features,
                ..Default::default()
            },
        )
        .map_err(vk_err)?;
        let queue = queues
            .next()
            .ok_or_else(|| Vse3dError::Vulkan("3D device returned no graphics queue".into()))?;

        let memory_allocator = Arc::new(StandardMemoryAllocator::new_default(device.clone()));
        let command_allocator = Arc::new(StandardCommandBufferAllocator::new(
            device.clone(),
            Default::default(),
        ));
        let depth_format = select_depth_format(&device)?;
        let pipeline = create_pipeline(&device, color_format, depth_format)?;

        let mut slots = Vec::with_capacity(config.ring_len);
        let mut image_descs = Vec::with_capacity(config.ring_len);
        let mut semaphore_fds = Vec::with_capacity(config.ring_len);
        for _ in 0..config.ring_len {
            let (image, image_desc) =
                create_exportable_image(device.clone(), color_format, ring_format, extent)?;
            let color_view = ImageView::new_default(image.clone()).map_err(vk_err)?;
            let depth = Image::new(
                memory_allocator.clone(),
                ImageCreateInfo {
                    image_type: ImageType::Dim2d,
                    format: depth_format,
                    extent: [extent[0], extent[1], 1],
                    usage: ImageUsage::DEPTH_STENCIL_ATTACHMENT,
                    ..Default::default()
                },
                AllocationCreateInfo {
                    memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
                    ..Default::default()
                },
            )
            .map_err(vk_err)?;
            let depth_view = ImageView::new_default(depth).map_err(vk_err)?;
            let ready = Semaphore::new(
                device.clone(),
                SemaphoreCreateInfo {
                    export_handle_types: ExternalSemaphoreHandleTypes::OPAQUE_FD,
                    ..Default::default()
                },
            )
            .map_err(vk_err)?;
            let fd = unsafe { ready.export_fd(ExternalSemaphoreHandleType::OpaqueFd) }
                .map_err(vk_err)?;
            semaphore_fds.push(fd.into());
            slots.push(RingSlot {
                color_view,
                depth_view,
                ready: Arc::new(ready),
                last_command_buffer: None,
            });
            image_descs.push(image_desc);
        }

        let (release_tx, release_rx) = release_channel();
        vse.attach_external_frame_source(
            ExternalRingDesc {
                images: image_descs,
                ready_semaphore_fds: semaphore_fds,
                timeline_semaphore_fd: None,
                sync: SyncKind::BinaryPerSlot,
            },
            release_tx,
        )?;

        Ok(Self {
            id: NEXT_RENDERER_ID.fetch_add(1, Ordering::Relaxed),
            consumer_device,
            device,
            queue,
            command_allocator,
            memory_allocator,
            pipeline,
            depth_format,
            extent,
            color_format,
            ring: slots,
            machine: RingStateMachine::new(config.ring_len, SyncKind::BinaryPerSlot)?,
            release_rx,
            models: HashMap::new(),
            next_model_id: 0,
            draws: Vec::with_capacity(16),
        })
    }

    pub fn load_model<P: AsRef<Path>>(&mut self, path: P) -> Result<ModelHandle, VSEError> {
        self.load_model_inner(path).map_err(VSEError::extension)
    }

    fn load_model_inner<P: AsRef<Path>>(&mut self, path: P) -> Result<ModelHandle, Vse3dError> {
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
            .map_err(vk_err)?;
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
            .map_err(vk_err)?;
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
        Ok(ModelHandle {
            renderer_id: self.id,
            id,
        })
    }

    pub fn model_info(&self, model: ModelHandle) -> Result<&ModelInfo, VSEError> {
        self.model_info_inner(model).map_err(VSEError::extension)
    }

    fn model_info_inner(&self, model: ModelHandle) -> Result<&ModelInfo, Vse3dError> {
        self.validate_handle(model)?;
        self.models
            .get(&model.id)
            .map(|model| &model.info)
            .ok_or(ModelError::UnknownHandle(model.id).into())
    }

    pub fn model_bounds(&self, model: ModelHandle) -> Result<crate::Bounds3D, VSEError> {
        Ok(self.model_info(model)?.bounds)
    }

    pub fn unload_model(&mut self, model: ModelHandle) -> Result<bool, VSEError> {
        self.unload_model_inner(model).map_err(VSEError::extension)
    }

    fn unload_model_inner(&mut self, model: ModelHandle) -> Result<bool, Vse3dError> {
        self.validate_handle(model)?;
        Ok(self.models.remove(&model.id).is_some())
    }

    pub fn draw_normals(
        &mut self,
        model: ModelHandle,
        model_transform: Mat4,
        camera: &PerspectiveCamera,
    ) -> Result<(), VSEError> {
        self.draw_normals_inner(model, model_transform, camera)
            .map_err(VSEError::extension)
    }

    fn draw_normals_inner(
        &mut self,
        model: ModelHandle,
        model_transform: Mat4,
        camera: &PerspectiveCamera,
    ) -> Result<(), Vse3dError> {
        if !model_transform.is_finite() {
            return Err(ModelError::NonFinite.into());
        }
        self.model_info_inner(model)?;
        let aspect = self.extent[0] as f32 / self.extent[1].max(1) as f32;
        let view_projection = camera.view_projection(aspect)?;
        self.draws.push(DrawNormals {
            model_id: model.id,
            model_transform,
            view_projection,
        });
        Ok(())
    }

    pub fn render_frame(&mut self, vse: &mut RenderContext<'_>) -> Result<(), VSEError> {
        self.render_frame_inner(vse).map_err(VSEError::extension)
    }

    fn render_frame_inner(&mut self, vse: &mut RenderContext<'_>) -> Result<(), Vse3dError> {
        if !Arc::ptr_eq(vse.device(), &self.consumer_device) {
            return Err(Vse3dError::ForeignContext);
        }
        let current_extent = vse.window_size();
        if [current_extent.0, current_extent.1] != self.extent {
            return Err(Vse3dError::Unsupported(format!(
                "VSE target resized from {}x{} to {}x{}; vse-3d rings have fixed session extent",
                self.extent[0], self.extent[1], current_extent.0, current_extent.1
            )));
        }
        if vse.color_format() != self.color_format {
            return Err(Vse3dError::Unsupported("VSE color format changed".into()));
        }

        for slot in self.release_rx.drain() {
            self.machine.release(slot)?;
            self.ring[slot.0].last_command_buffer = None;
        }
        let slot_index = self.machine.acquire_for_produce()?;
        let slot = &mut self.ring[slot_index.0];
        let mut builder = AutoCommandBufferBuilder::primary(
            self.command_allocator.clone(),
            self.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .map_err(vk_err)?;
        builder
            .begin_rendering(RenderingInfo {
                color_attachments: vec![Some(RenderingAttachmentInfo {
                    load_op: AttachmentLoadOp::Clear,
                    store_op: AttachmentStoreOp::Store,
                    clear_value: Some(ClearValue::Float(vse.clear_color())),
                    ..RenderingAttachmentInfo::image_view(slot.color_view.clone())
                })],
                depth_attachment: Some(RenderingAttachmentInfo {
                    load_op: AttachmentLoadOp::Clear,
                    store_op: AttachmentStoreOp::DontCare,
                    clear_value: Some(ClearValue::Depth(1.0)),
                    ..RenderingAttachmentInfo::image_view(slot.depth_view.clone())
                }),
                ..Default::default()
            })
            .map_err(vk_err)?
            .set_viewport(
                0,
                [Viewport {
                    offset: [0.0, 0.0],
                    extent: [self.extent[0] as f32, self.extent[1] as f32],
                    depth_range: 0.0..=1.0,
                }]
                .into_iter()
                .collect(),
            )
            .map_err(vk_err)?;

        for draw in &self.draws {
            let model = self
                .models
                .get(&draw.model_id)
                .ok_or(ModelError::UnknownHandle(draw.model_id))?;
            for instance in &model.instances {
                let primitive = &model.primitives[instance.primitive_index];
                let world = draw.model_transform * instance.local_transform;
                builder
                    .bind_pipeline_graphics(self.pipeline.clone())
                    .map_err(vk_err)?
                    .push_constants(
                        self.pipeline.layout().clone(),
                        0,
                        mesh_normals_vs::PushConstants {
                            model: world.to_cols_array_2d(),
                            view_projection: draw.view_projection.to_cols_array_2d(),
                        },
                    )
                    .map_err(vk_err)?
                    .bind_vertex_buffers(0, primitive.vertex_buffer.clone())
                    .map_err(vk_err)?
                    .bind_index_buffer(primitive.index_buffer.clone())
                    .map_err(vk_err)?;
                unsafe {
                    builder
                        .draw_indexed(primitive.index_count, 1, 0, 0, 0)
                        .map_err(vk_err)?;
                }
            }
        }
        builder.end_rendering().map_err(vk_err)?;
        let command_buffer = builder.build().map_err(vk_err)?;
        let submit = SubmitInfo {
            command_buffers: vec![CommandBufferSubmitInfo::new(command_buffer.clone())],
            signal_semaphores: vec![SemaphoreSubmitInfo::new(slot.ready.clone())],
            ..Default::default()
        };
        self.queue
            .with(|mut guard| unsafe { guard.submit(&[submit], None) })
            .map_err(vk_err)?;
        slot.last_command_buffer = Some(command_buffer);
        self.machine.mark_ready(slot_index)?;
        self.draws.clear();
        vse.queue_external_frame(slot_index)?;
        Ok(())
    }

    pub fn info(&self) -> Vse3dInfo {
        let mut models: Vec<_> = self
            .models
            .values()
            .map(|model| model.info.clone())
            .collect();
        models.sort_by(|a, b| a.source_path.cmp(&b.source_path));
        Vse3dInfo {
            crate_version: env!("CARGO_PKG_VERSION").to_string(),
            device_name: self
                .device
                .physical_device()
                .properties()
                .device_name
                .clone(),
            extent: self.extent,
            color_format: format!("{:?}", self.color_format),
            depth_format: format!("{:?}", self.depth_format),
            ring_len: self.ring.len(),
            pipelines: vec!["MeshNormals".into()],
            models,
        }
    }

    fn validate_handle(&self, model: ModelHandle) -> Result<(), ModelError> {
        if model.renderer_id != self.id {
            return Err(ModelError::ForeignHandle);
        }
        Ok(())
    }
}

fn ring_format(format: Format) -> Result<RingFormat, Vse3dError> {
    match format {
        Format::R8G8B8A8_SRGB => Ok(RingFormat::Rgba8UnormSrgb),
        Format::B8G8R8A8_SRGB => Ok(RingFormat::Bgra8UnormSrgb),
        Format::R16G16B16A16_SFLOAT => Ok(RingFormat::Rgba16Float),
        other => Err(Vse3dError::Unsupported(format!(
            "external-frame protocol does not represent VSE format {other:?}"
        ))),
    }
}

fn create_exportable_image(
    device: Arc<Device>,
    format: Format,
    ring_format: RingFormat,
    extent: [u32; 2],
) -> Result<(Arc<Image>, ExternalImageDesc), Vse3dError> {
    let raw = RawImage::new(
        device.clone(),
        ImageCreateInfo {
            image_type: ImageType::Dim2d,
            format,
            extent: [extent[0], extent[1], 1],
            usage: RING_USAGE,
            external_memory_handle_types: ExternalMemoryHandleTypes::OPAQUE_FD,
            ..Default::default()
        },
    )
    .map_err(vk_err)?;
    let requirements = raw.memory_requirements()[0];
    let memory_type_index = device
        .physical_device()
        .memory_properties()
        .memory_types
        .iter()
        .enumerate()
        .filter(|(index, _)| requirements.memory_type_bits & (1 << index) != 0)
        .max_by_key(|(_, ty)| {
            ty.property_flags
                .contains(MemoryPropertyFlags::DEVICE_LOCAL)
        })
        .map(|(index, _)| index as u32)
        .ok_or_else(|| Vse3dError::Vulkan("no compatible memory type for ring image".into()))?;
    let memory = DeviceMemory::allocate(
        device,
        MemoryAllocateInfo {
            allocation_size: requirements.layout.size(),
            memory_type_index,
            dedicated_allocation: Some(DedicatedAllocation::Image(&raw)),
            export_handle_types: ExternalMemoryHandleTypes::OPAQUE_FD,
            ..Default::default()
        },
    )
    .map_err(vk_err)?;
    let allocation_size = memory.allocation_size();
    let fd = memory
        .export_fd(ExternalMemoryHandleType::OpaqueFd)
        .map_err(vk_err)?;
    let image = raw
        .bind_memory([ResourceMemory::new_dedicated(memory)])
        .map_err(|(error, _, _)| vk_err(error))?;
    Ok((
        Arc::new(image),
        ExternalImageDesc {
            memory_fd: fd.into(),
            allocation_size,
            memory_type_index,
            format: ring_format,
            extent,
        },
    ))
}

fn select_depth_format(device: &Arc<Device>) -> Result<Format, Vse3dError> {
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
        .ok_or_else(|| Vse3dError::Unsupported("no supported depth attachment format".into()))
}

fn create_pipeline(
    device: &Arc<Device>,
    color_format: Format,
    depth_format: Format,
) -> Result<Arc<GraphicsPipeline>, Vse3dError> {
    let vs = mesh_normals_vs::load(device.clone()).map_err(vk_err)?;
    let fs = mesh_normals_fs::load(device.clone()).map_err(vk_err)?;
    let vs_entry = vs.entry_point("main").unwrap();
    let fs_entry = fs.entry_point("main").unwrap();
    let vertex_input = Vertex3D::per_vertex()
        .definition(&vs_entry)
        .map_err(vk_err)?;
    let stages = [
        PipelineShaderStageCreateInfo::new(vs_entry),
        PipelineShaderStageCreateInfo::new(fs_entry),
    ];
    let layout = PipelineLayout::new(
        device.clone(),
        PipelineDescriptorSetLayoutCreateInfo::from_stages(&stages)
            .into_pipeline_layout_create_info(device.clone())
            .map_err(vk_err)?,
    )
    .map_err(vk_err)?;
    GraphicsPipeline::new(
        device.clone(),
        None,
        GraphicsPipelineCreateInfo {
            stages: stages.into_iter().collect(),
            vertex_input_state: Some(vertex_input),
            input_assembly_state: Some(InputAssemblyState {
                topology: PrimitiveTopology::TriangleList,
                ..Default::default()
            }),
            viewport_state: Some(ViewportState::default()),
            rasterization_state: Some(RasterizationState {
                cull_mode: CullMode::Back,
                front_face: FrontFace::CounterClockwise,
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
                    color_attachment_formats: vec![Some(color_format)],
                    depth_attachment_format: Some(depth_format),
                    ..Default::default()
                }
                .into(),
            ),
            ..GraphicsPipelineCreateInfo::layout(layout)
        },
    )
    .map_err(vk_err)
}

fn vk_err(error: impl std::fmt::Display) -> Vse3dError {
    Vse3dError::Vulkan(error.to_string())
}
