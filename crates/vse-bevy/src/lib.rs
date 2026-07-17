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
//!
//! Use [`BevyProducer`] when VSE should render a Bevy frame synchronously before
//! each flip. Use [`AsyncBevyProducer`] with VSE's `LatestReadyHoldLast` external
//! frame policy when VSE should flip on time and display the newest Bevy frame
//! that has completed by the deadline.

mod ring_alloc;
pub mod scene;

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use bevy::app::PluginsState;
use bevy::camera::{ManualTextureViewHandle, RenderTarget};
use bevy::prelude::*;
use bevy::render::renderer::{RenderDevice, RenderQueue};
use bevy::render::settings::RenderCreation;
use bevy::render::texture::{ManualTextureView, ManualTextureViews};
use bevy::render::RenderPlugin;
use bevy::window::ExitCondition;

use vse_external_frame::{
    release_channel, ExternalRingDesc, RingError, RingFormat, RingStateMachine, SlotIndex,
    SlotReleaseRx, SlotReleaseTx, SyncKind,
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
    #[error("async producer worker stopped")]
    WorkerStopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadyFrame {
    pub frame_number: u64,
    pub slot: SlotIndex,
    pub timeline_value: Option<u64>,
}

enum AsyncProducerCommand {
    Render(u64),
    Stop,
}

struct AsyncProducerClient {
    request_tx: mpsc::Sender<AsyncProducerCommand>,
    ready_rx: mpsc::Receiver<Result<ReadyFrame, ProducerError>>,
}

struct AsyncProducerWorker {
    request_rx: mpsc::Receiver<AsyncProducerCommand>,
    ready_tx: mpsc::Sender<Result<ReadyFrame, ProducerError>>,
}

struct AsyncProducerChannels;

impl AsyncProducerChannels {
    fn pair() -> (AsyncProducerClient, AsyncProducerWorker) {
        let (request_tx, request_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        (
            AsyncProducerClient {
                request_tx,
                ready_rx,
            },
            AsyncProducerWorker {
                request_rx,
                ready_tx,
            },
        )
    }
}

impl AsyncProducerClient {
    fn request_frame(&self, frame_number: u64) -> Result<(), ProducerError> {
        self.request_tx
            .send(AsyncProducerCommand::Render(frame_number))
            .map_err(|_| ProducerError::WorkerStopped)
    }

    fn try_recv_ready(&self) -> Result<Option<ReadyFrame>, ProducerError> {
        match self.ready_rx.try_recv() {
            Ok(Ok(frame)) => Ok(Some(frame)),
            Ok(Err(e)) => Err(e),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(ProducerError::WorkerStopped),
        }
    }
}

fn is_retryable_async_backpressure(error: &ProducerError) -> bool {
    matches!(error, ProducerError::Ring(RingError::NoFreeSlot))
}

impl AsyncProducerWorker {
    fn try_recv_request(&self) -> Result<Option<u64>, ProducerError> {
        match self.request_rx.try_recv() {
            Ok(AsyncProducerCommand::Render(frame_number)) => Ok(Some(frame_number)),
            Ok(AsyncProducerCommand::Stop) => Ok(None),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(ProducerError::WorkerStopped),
        }
    }

    fn recv_latest_request(&self) -> Result<Option<u64>, ProducerError> {
        let mut latest = match self.request_rx.recv() {
            Ok(AsyncProducerCommand::Render(frame_number)) => frame_number,
            Ok(AsyncProducerCommand::Stop) => return Ok(None),
            Err(_) => return Err(ProducerError::WorkerStopped),
        };
        while let Some(frame_number) = self.try_recv_request()? {
            latest = frame_number;
        }
        Ok(Some(latest))
    }

    #[cfg(test)]
    fn send_ready(&self, frame: ReadyFrame) -> Result<(), ProducerError> {
        self.ready_tx
            .send(Ok(frame))
            .map_err(|_| ProducerError::WorkerStopped)
    }
}

struct AsyncProducerInit {
    export_desc: ExternalRingDesc,
    sync: SyncKind,
    extent: [u32; 2],
}

/// A Bevy external-frame producer running on its own worker thread.
///
/// `request_frame()` queues work and returns immediately. Completed frames are
/// retrieved with `try_recv_ready()` and then passed to VSE via
/// `RenderContext::queue_external_frame`.
pub struct AsyncBevyProducer {
    client: AsyncProducerClient,
    release_tx: SlotReleaseTx,
    export_desc: Option<ExternalRingDesc>,
    sync: SyncKind,
    extent: [u32; 2],
    worker: Option<thread::JoinHandle<()>>,
}

impl AsyncBevyProducer {
    pub fn spawn(
        config: ProducerConfig,
        build_scene: impl FnOnce(&mut App, Entity) + Send + 'static,
    ) -> Result<Self, ProducerError> {
        let (release_tx, release_rx) = release_channel();
        let (client, worker_channels) = AsyncProducerChannels::pair();
        let (init_tx, init_rx) = mpsc::sync_channel(1);

        let worker = thread::spawn(move || {
            let mut producer = match BevyProducer::new(config, build_scene) {
                Ok(producer) => producer,
                Err(e) => {
                    let _ = init_tx.send(Err(e));
                    return;
                }
            };
            producer.set_release_rx(release_rx);

            let export_desc = match producer.export_ring() {
                Ok(desc) => desc,
                Err(e) => {
                    let _ = init_tx.send(Err(e));
                    return;
                }
            };
            let init = AsyncProducerInit {
                export_desc,
                sync: producer.sync(),
                extent: producer.extent(),
            };
            if init_tx.send(Ok(init)).is_err() {
                return;
            }

            'worker: while let Ok(Some(mut frame_number)) = worker_channels.recv_latest_request() {
                loop {
                    match producer.render_ready_frame(frame_number) {
                        Ok(frame) => {
                            if worker_channels.ready_tx.send(Ok(frame)).is_err() {
                                break 'worker;
                            }
                            break;
                        }
                        Err(e) if is_retryable_async_backpressure(&e) => {
                            // The async producer can temporarily outrun VSE's
                            // release back-edge. That is backpressure, not a
                            // fatal producer error: wait briefly, coalesce any
                            // newer requests, and retry after releases have had
                            // a chance to arrive.
                            thread::sleep(Duration::from_millis(1));
                            loop {
                                match worker_channels.request_rx.try_recv() {
                                    Ok(AsyncProducerCommand::Render(newer)) => {
                                        frame_number = newer;
                                    }
                                    Ok(AsyncProducerCommand::Stop) => break 'worker,
                                    Err(mpsc::TryRecvError::Empty) => break,
                                    Err(mpsc::TryRecvError::Disconnected) => break 'worker,
                                }
                            }
                        }
                        Err(e) => {
                            let _ = worker_channels.ready_tx.send(Err(e));
                            break 'worker;
                        }
                    }
                }
            }
        });

        let init = init_rx.recv().map_err(|_| ProducerError::WorkerStopped)??;

        Ok(Self {
            client,
            release_tx,
            export_desc: Some(init.export_desc),
            sync: init.sync,
            extent: init.extent,
            worker: Some(worker),
        })
    }

    pub fn export_ring(&mut self) -> Result<ExternalRingDesc, ProducerError> {
        self.export_desc
            .take()
            .ok_or(ProducerError::AlreadyExported)
    }

    pub fn release_tx(&self) -> SlotReleaseTx {
        self.release_tx.clone()
    }

    pub fn sync(&self) -> SyncKind {
        self.sync
    }

    pub fn extent(&self) -> [u32; 2] {
        self.extent
    }

    pub fn request_frame(&self, frame_number: u64) -> Result<(), ProducerError> {
        self.client.request_frame(frame_number)
    }

    pub fn try_recv_ready(&self) -> Result<Option<ReadyFrame>, ProducerError> {
        self.client.try_recv_ready()
    }
}

impl Drop for AsyncBevyProducer {
    fn drop(&mut self) {
        let _ = self.client.request_tx.send(AsyncProducerCommand::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
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
    timeline_counter: u64,
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
            timeline_semaphore_fd: ring
                .timeline_semaphore_fd
                .as_ref()
                .map(|fd| fd.try_clone().expect("dup timeline semaphore fd")),
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
            timeline_counter: 0,
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
        self.render_ready_frame(frame_number)
            .map(|frame| frame.slot)
    }

    /// Render one frame and return the full ready-frame notification,
    /// including a timeline value when the ring uses timeline sync.
    pub fn render_ready_frame(&mut self, frame_number: u64) -> Result<ReadyFrame, ProducerError> {
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
        let mut timeline_value = None;
        match (
            self.ring.sync,
            self.ring.slots[slot.0].semaphore,
            self.ring.timeline_semaphore,
        ) {
            (SyncKind::BinaryPerSlot, Some(semaphore), _) => {
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
            (SyncKind::Timeline, _, Some(semaphore)) => {
                self.timeline_counter += 1;
                let value = self.timeline_counter;
                // SAFETY: semaphore was created on this queue's device; the hal
                // guard is dropped before the submit that consumes the signal.
                unsafe {
                    let hal_queue = queue
                        .as_hal::<wgpu::hal::api::Vulkan>()
                        .expect("wgpu is running on Vulkan");
                    hal_queue.add_signal_semaphore(semaphore, Some(value));
                }
                queue.submit([]);
                timeline_value = Some(value);
            }
            _ => {
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
        Ok(ReadyFrame {
            frame_number,
            slot,
            timeline_value,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn async_channels_deliver_requests_and_ready_frames_without_blocking() {
        let (client, worker) = AsyncProducerChannels::pair();

        client.request_frame(7).unwrap();
        assert_eq!(worker.try_recv_request().unwrap(), Some(7));
        assert_eq!(worker.try_recv_request().unwrap(), None);

        worker
            .send_ready(ReadyFrame {
                frame_number: 7,
                slot: SlotIndex(2),
                timeline_value: Some(11),
            })
            .unwrap();
        assert_eq!(
            client.try_recv_ready().unwrap(),
            Some(ReadyFrame {
                frame_number: 7,
                slot: SlotIndex(2),
                timeline_value: Some(11),
            })
        );
        assert_eq!(client.try_recv_ready().unwrap(), None);
    }

    #[test]
    fn async_worker_coalesces_pending_requests_to_newest_frame() {
        let (client, worker) = AsyncProducerChannels::pair();

        client.request_frame(1).unwrap();
        client.request_frame(2).unwrap();
        client.request_frame(3).unwrap();

        assert_eq!(worker.recv_latest_request().unwrap(), Some(3));
        assert_eq!(worker.try_recv_request().unwrap(), None);
    }

    #[test]
    fn async_worker_treats_no_free_slot_as_retryable_backpressure() {
        assert!(is_retryable_async_backpressure(&ProducerError::Ring(
            RingError::NoFreeSlot
        )));
        assert!(!is_retryable_async_backpressure(&ProducerError::Ring(
            RingError::TooSmall { len: 1 }
        )));
        assert!(!is_retryable_async_backpressure(
            &ProducerError::WorkerStopped
        ));
    }
}
