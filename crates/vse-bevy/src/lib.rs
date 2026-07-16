//! Headless Bevy producer for VSE's external-renderer handoff seam.
//!
//! Bevy renders on **its own stock wgpu device** (landscape doc §5.4,
//! Topology 2) into a ring of exportable images; only image memory FDs and
//! semaphore FDs cross to VSE, which imports the ring
//! (`vision_stimulus_engine::core::external_frame`) and remains sole present
//! authority. Nothing here touches a swapchain, present timing, or `FlipInfo`.
//!
//! Determinism contract (enforced by construction):
//! - headless: no winit, no window, no compositor involvement;
//! - `PipelinedRenderingPlugin` disabled — `app.update()` submits the frame's
//!   GPU work before returning;
//! - `synchronous_pipeline_compilation` — no frames silently skipped while
//!   shaders compile in the background;
//! - all animation is a pure function of [`ExternalFrameIndex`] (VSE's frame
//!   counter); Bevy's `Time` is never read by scene systems.

mod ring_alloc;
pub mod scene;

use bevy::app::PluginsState;
use bevy::camera::{ManualTextureViewHandle, RenderTarget};
use bevy::prelude::*;
use bevy::render::renderer::{RenderDevice, RenderQueue};
use bevy::render::settings::RenderCreation;
use bevy::render::texture::{ManualTextureView, ManualTextureViews};
use bevy::render::RenderPlugin;
use bevy::window::ExitCondition;

use vse_external_frame::{
    ExternalRingDesc, RingError, RingFormat, RingStateMachine, SlotIndex, SlotReleaseRx, SyncKind,
};

use ring_alloc::ExportedRing;

#[derive(Debug, thiserror::Error)]
pub enum ProducerError {
    #[error("producer setup failed: {0}")]
    Setup(String),
    #[error("ring state machine: {0}")]
    Ring(#[from] RingError),
    #[error("ring already exported")]
    AlreadyExported,
}

/// The only clock scene systems may read: VSE's frame counter, written by
/// [`BevyProducer::render_frame`] before each `app.update()`.
#[derive(Resource, Default, Clone, Copy)]
pub struct ExternalFrameIndex(pub u64);

pub struct ProducerConfig {
    pub extent: [u32; 2],
    pub format: RingFormat,
    pub ring_len: usize,
}

impl Default for ProducerConfig {
    fn default() -> Self {
        Self {
            extent: [800, 600],
            format: RingFormat::Rgba8UnormSrgb,
            ring_len: 3,
        }
    }
}

/// Headless Bevy app rendering into the exported image ring, driven one frame
/// at a time by VSE's loop.
pub struct BevyProducer {
    app: App,
    machine: RingStateMachine,
    release_rx: Option<SlotReleaseRx>,
    ring: ExportedRing,
    handles: Vec<ManualTextureViewHandle>,
    camera: Entity,
    /// Ring descriptor, present until `export_ring` moves it out.
    export_desc: Option<ExternalRingDesc>,
    extent: [u32; 2],
}

impl BevyProducer {
    /// Build the headless app, allocate + export the image ring, and spawn the
    /// scene via `build_scene` (which receives the camera entity to configure).
    pub fn new(
        config: ProducerConfig,
        build_scene: impl FnOnce(&mut App, Entity),
    ) -> Result<Self, ProducerError> {
        let mut app = App::new();

        // wgpu 29 never requests VK_KHR_external_semaphore_fd at vkCreateDevice
        // (docs/upstream-watch.md item 2), which leaves vkGetSemaphoreFdKHR
        // unresolvable on a conformant loader and forces the CpuBlocking
        // fallback. Bevy's raw_vulkan_init hook lets us append the extension to
        // wgpu's own list at device creation. Must be inserted before
        // add_plugins: RenderPlugin::build reads this resource.
        let mut raw_vulkan =
            bevy::render::renderer::raw_vulkan_init::RawVulkanInitSettings::default();
        // SAFETY: the callback only appends an extension the physical device
        // reports support for; nothing is removed or disabled.
        unsafe {
            raw_vulkan.add_create_device_callback(|args, adapter, _| {
                let name = ash::khr::external_semaphore_fd::NAME;
                if adapter
                    .physical_device_capabilities()
                    .supports_extension(name)
                    && !args.extensions.contains(&name)
                {
                    args.extensions.push(name);
                }
            });
        }
        app.insert_resource(raw_vulkan);

        app.add_plugins(
            DefaultPlugins
                .set(bevy::window::WindowPlugin {
                    primary_window: None,
                    exit_condition: ExitCondition::DontExit,
                    ..default()
                })
                .set(RenderPlugin {
                    render_creation: RenderCreation::default(),
                    synchronous_pipeline_compilation: true,
                    ..default()
                })
                .disable::<bevy::render::pipelined_rendering::PipelinedRenderingPlugin>()
                // VSE owns the process's tracing subscriber.
                .disable::<bevy::log::LogPlugin>(),
        );

        // Drive plugin construction to completion (RenderPlugin initializes
        // wgpu asynchronously), then unpack RenderDevice/RenderQueue into the
        // main world.
        while app.plugins_state() == PluginsState::Adding {
            bevy::tasks::tick_global_task_pools_on_main_thread();
        }
        app.finish();
        app.cleanup();

        // --- Allocate + export the ring on wgpu's raw device ---
        let ring = {
            let device = app.world().resource::<RenderDevice>().wgpu_device().clone();
            ring_alloc::allocate_ring(&device, config.format, config.extent, config.ring_len)?
        };

        // Register each slot as a manual render target.
        let mut handles = Vec::with_capacity(config.ring_len);
        {
            let views: Vec<_> = ring
                .slots
                .iter()
                .map(|slot| {
                    let view = slot
                        .texture
                        .create_view(&wgpu::TextureViewDescriptor::default());
                    ManualTextureView {
                        texture_view: view.into(),
                        size: UVec2::new(config.extent[0], config.extent[1]),
                        view_format: ring_alloc::wgpu_format(config.format),
                    }
                })
                .collect();
            let mut manual = app.world_mut().resource_mut::<ManualTextureViews>();
            for (i, view) in views.into_iter().enumerate() {
                let handle = ManualTextureViewHandle(i as u32);
                manual.insert(handle, view);
                handles.push(handle);
            }
        }

        let export_desc = ExternalRingDesc {
            images: ring
                .image_descs
                .iter()
                .map(|d| vse_external_frame::ExternalImageDesc {
                    memory_fd: d.memory_fd.try_clone().expect("dup ring memory fd"),
                    allocation_size: d.allocation_size,
                    memory_type_index: d.memory_type_index,
                    format: d.format,
                    extent: d.extent,
                })
                .collect(),
            ready_semaphore_fds: ring
                .semaphore_fds
                .iter()
                .map(|fd| fd.try_clone().expect("dup ring semaphore fd"))
                .collect(),
            sync: ring.sync,
        };

        app.insert_resource(ExternalFrameIndex(0));

        // Camera: renders into the ring (retargeted to the acquired slot each
        // frame). Msaa/Tonemapping are part of the determinism contract.
        let camera = app
            .world_mut()
            .spawn((
                Camera3d::default(),
                Msaa::Off,
                bevy::core_pipeline::tonemapping::Tonemapping::None,
                RenderTarget::TextureView(handles[0]),
                Transform::from_xyz(0.0, 3.5, 6.0).looking_at(Vec3::ZERO, Vec3::Y),
            ))
            .id();

        build_scene(&mut app, camera);

        // Warm-up: compile every pipeline the scene needs before frame 0 so
        // pipeline-compilation cost never lands inside a timed trial. Renders
        // into slot 0, which is discarded (VSE clears the ring at import).
        app.update();

        Ok(Self {
            app,
            machine: RingStateMachine::new(config.ring_len, ring.sync)?,
            release_rx: None,
            ring,
            handles,
            camera,
            export_desc: Some(export_desc),
            extent: config.extent,
        })
    }

    /// The frame-sync mode the probe selected ([`SyncKind::BinaryPerSlot`] on
    /// a conformant driver; [`SyncKind::CpuBlocking`] fallback otherwise).
    pub fn sync(&self) -> SyncKind {
        self.ring.sync
    }

    pub fn extent(&self) -> [u32; 2] {
        self.extent
    }

    /// Hand the ring descriptor to the consumer (once).
    pub fn export_ring(&mut self) -> Result<ExternalRingDesc, ProducerError> {
        self.export_desc
            .take()
            .ok_or(ProducerError::AlreadyExported)
    }

    /// Receiver for the consumer's slot-release back-edge.
    pub fn set_release_rx(&mut self, rx: SlotReleaseRx) {
        self.release_rx = Some(rx);
    }

    /// Render one frame into a ring slot and return it.
    ///
    /// `frame_number` is VSE's frame counter; scene animation is a pure
    /// function of it. The returned slot must be handed to
    /// `RenderContext::queue_external_frame` before the corresponding flip.
    pub fn render_frame(&mut self, frame_number: u64) -> Result<SlotIndex, ProducerError> {
        // Drain the release back-edge.
        if let Some(rx) = &self.release_rx {
            for slot in rx.drain() {
                self.machine.release(slot)?;
            }
        }

        // A failure here is a pipelining bug (ring too small for the depth),
        // not a wait condition — fail loudly per the seam's design.
        let slot = self.machine.acquire_for_produce()?;

        self.app
            .world_mut()
            .insert_resource(ExternalFrameIndex(frame_number));
        self.app
            .world_mut()
            .entity_mut(self.camera)
            .insert(RenderTarget::TextureView(self.handles[slot.0]));

        // Extract + render synchronously (PipelinedRenderingPlugin disabled).
        self.app.update();

        // Signal the slot's exported semaphore behind everything this frame
        // submitted: a queue signal op's first synchronization scope covers all
        // prior commands on the queue in submission order, so an empty submit
        // carrying the signal is correct even if Bevy submitted multiple times.
        let queue = self.app.world().resource::<RenderQueue>();
        match self.ring.slots[slot.0].semaphore {
            Some(semaphore) => {
                // SAFETY: semaphore was created on this queue's device; the hal
                // guard is dropped before the submit that consumes the signal.
                unsafe {
                    let hal_queue = queue
                        .as_hal::<wgpu::hal::api::Vulkan>()
                        .expect("wgpu is running on Vulkan");
                    hal_queue.add_signal_semaphore(semaphore, None);
                }
                queue.submit([]);
            }
            None => {
                // CpuBlocking fallback: stall until this frame's GPU work is
                // done, so the frame is complete before VSE ever samples it.
                self.app
                    .world()
                    .resource::<RenderDevice>()
                    .wgpu_device()
                    .poll(wgpu::PollType::wait_indefinitely())
                    .map_err(|e| ProducerError::Setup(format!("device poll: {e}")))?;
            }
        }

        self.machine.mark_ready(slot)?;
        // Hand-off: the slot now belongs to the consumer until it comes back
        // over the release channel (Ready → Consuming on this side's mirror).
        let handed = self.machine.take_ready().expect("slot marked ready above");
        debug_assert_eq!(handed, slot, "producer hand-off must be FIFO");
        Ok(slot)
    }
}
