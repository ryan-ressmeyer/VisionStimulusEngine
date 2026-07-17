//! Consumer side of the external-renderer handoff seam.
//!
//! An external renderer (e.g. `vse-bevy`) renders on **its own Vulkan device**
//! into a ring of images allocated with exportable memory; this module imports
//! that ring onto VSE's device (`VK_KHR_external_memory_fd`, OPAQUE_FD) plus
//! the per-slot ready semaphores (`VK_KHR_external_semaphore_fd`). VSE remains
//! the sole present authority: the external image is consumed as an *underlay*
//! blitted into the swapchain image before VSE's own draw commands, and the
//! renderer never touches the swapchain, present timing, or `FlipInfo`.
//!
//! Synchronization topology per frame:
//! - **Forward edge (GPU):** the producer signals the slot's exported binary
//!   semaphore when its render is complete; VSE's submit waits on it at the
//!   transfer stage (see `PresentEngine::submit_and_present`).
//! - **Release back-edge (CPU, off the critical path):** when the fence of the
//!   submit that sampled a slot signals, the slot is released to the producer
//!   over an mpsc channel ([`SlotReleaseTx`]). Only then may the producer
//!   re-signal that slot's binary semaphore (the reuse invariant encoded in
//!   [`RingStateMachine`]).
//!
//! Layout contract (documented, not negotiated, for the OPAQUE_FD PoC): images
//! are created with `COLOR_ATTACHMENT | TRANSFER_SRC | TRANSFER_DST` usage on
//! **both** devices, which makes vulkano's assumed steady-state layout
//! `ColorAttachmentOptimal` — the same layout wgpu leaves a render-attachment
//! texture in after the producer's final pass, and the layout VSE's command
//! buffer returns the image to after blitting from it. Explicit
//! `VK_QUEUE_FAMILY_EXTERNAL` ownership transfers are deliberately skipped:
//! same driver, same physical device, queue family 0 on both sides; the
//! determinism harness is the behavioral check. The dmabuf + explicit
//! DRM-format-modifier upgrade (cross-process / cross-vendor) is where real
//! ownership transfer gets added.

use std::collections::VecDeque;
use std::sync::Arc;

use tracing::warn;
use vulkano::command_buffer::allocator::StandardCommandBufferAllocator;
use vulkano::command_buffer::{
    AutoCommandBufferBuilder, ClearColorImageInfo, CommandBufferUsage, PrimaryCommandBufferAbstract,
};
use vulkano::device::{Device, Queue};
use vulkano::image::sys::RawImage;
use vulkano::image::{Image, ImageCreateInfo, ImageType, ImageUsage};
use vulkano::memory::{
    DedicatedAllocation, DeviceMemory, ExternalMemoryHandleType, ExternalMemoryHandleTypes,
    MemoryAllocateInfo, MemoryImportInfo, ResourceMemory,
};
use vulkano::sync::fence::Fence;
use vulkano::sync::semaphore::{
    ExternalSemaphoreHandleType, ImportSemaphoreFdInfo, Semaphore, SemaphoreCreateInfo,
    SemaphoreType,
};
use vulkano::sync::GpuFuture;

use vse_external_frame::{
    ExternalRingDesc, RingError, RingFormat, RingStateMachine, SlotIndex, SlotReleaseTx, SyncKind,
};

/// Image usage on both sides of the boundary. Must byte-match the exporter's
/// usage (defined memory aliasing), and — transfer bits stripped — must reduce
/// to `COLOR_ATTACHMENT` so vulkano's assumed layout is `ColorAttachmentOptimal`
/// (see the module docs).
pub(crate) const RING_IMAGE_USAGE: ImageUsage = ImageUsage::COLOR_ATTACHMENT
    .union(ImageUsage::TRANSFER_SRC)
    .union(ImageUsage::TRANSFER_DST);

#[derive(Debug, thiserror::Error)]
pub enum ExternalFrameError {
    #[error("external frame source unsupported: {0}")]
    Unsupported(String),
    #[error("external ring import failed: {0}")]
    ImportFailed(String),
    #[error("external ring descriptor invalid: {0}")]
    InvalidDesc(String),
    #[error("ring state machine: {0}")]
    Ring(#[from] RingError),
}

fn vk_format(format: RingFormat) -> vulkano::format::Format {
    match format {
        RingFormat::Rgba8UnormSrgb => vulkano::format::Format::R8G8B8A8_SRGB,
        RingFormat::Bgra8UnormSrgb => vulkano::format::Format::B8G8R8A8_SRGB,
        RingFormat::Rgba16Float => vulkano::format::Format::R16G16B16A16_SFLOAT,
    }
}

/// The external frame(s) consumed by one flip.
///
/// Normally one slot; more when previous flips were skipped (swapchain
/// recreation) and ready frames accumulated. Only the **newest** image is
/// shown, but every slot's semaphore is waited by the consuming submit — a
/// signaled binary semaphore must be waited before its slot can be reused, so
/// skipped frames' signals ride along on the next successful submit.
pub(crate) struct ConsumableFrames {
    /// Newest ready image (the one to display).
    pub image: Arc<Image>,
    /// All semaphores the consuming submit must wait on (empty under
    /// [`SyncKind::CpuBlocking`]), with timeline values if any.
    pub waits: Vec<(Arc<Semaphore>, Option<u64>)>,
    /// Consumed slots that should be released together on this flip's fence.
    /// In latched mode this excludes the newly displayed slot, which remains
    /// owned by VSE until a replacement frame is submitted.
    pub slots: Vec<SlotIndex>,
    /// Slot that becomes the held image after this flip submit succeeds.
    pub latch_after_submit: Option<SlotIndex>,
}

/// How VSE chooses external-renderer frames at flip time.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ExternalFramePolicy {
    /// Consume ready external frames for this flip and release every consumed
    /// slot after the flip's submit fence signals.
    #[default]
    FrameLocked,
    /// Display the newest ready external frame when one exists. If no new
    /// frame is ready, keep displaying the last consumed slot and do not
    /// release it to the producer.
    LatestReadyHoldLast,
}

#[derive(Debug, Default)]
struct ExternalFrameLatch {
    slot: Option<SlotIndex>,
}

impl ExternalFrameLatch {
    fn note_latched(&mut self, slot: SlotIndex) {
        self.slot = Some(slot);
    }

    fn plan(&self, policy: ExternalFramePolicy, ready_slots: Vec<SlotIndex>) -> ExternalFramePlan {
        match policy {
            ExternalFramePolicy::FrameLocked => ExternalFramePlan {
                display_slot: ready_slots.last().copied(),
                wait_slots: ready_slots.clone(),
                release_after_submit: ready_slots,
                new_latch: None,
            },
            ExternalFramePolicy::LatestReadyHoldLast => match ready_slots.last().copied() {
                Some(newest) => {
                    let wait_slots = ready_slots.clone();
                    let mut release_after_submit = ready_slots;
                    let _ = release_after_submit.pop();
                    if let Some(previous) = self.slot {
                        release_after_submit.push(previous);
                    }
                    ExternalFramePlan {
                        display_slot: Some(newest),
                        wait_slots,
                        release_after_submit,
                        new_latch: Some(newest),
                    }
                }
                None => ExternalFramePlan {
                    display_slot: self.slot,
                    wait_slots: Vec::new(),
                    release_after_submit: Vec::new(),
                    new_latch: self.slot,
                },
            },
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ExternalFramePlan {
    display_slot: Option<SlotIndex>,
    wait_slots: Vec<SlotIndex>,
    release_after_submit: Vec<SlotIndex>,
    new_latch: Option<SlotIndex>,
}

/// The imported external image ring: VSE-side consumer state.
pub struct ExternalFrameRing {
    images: Vec<Arc<Image>>,
    ready_sems: Vec<Arc<Semaphore>>,
    timeline_sem: Option<Arc<Semaphore>>,
    ready_values: Vec<Option<u64>>,
    machine: RingStateMachine,
    release_tx: SlotReleaseTx,
    /// Consumed slots awaiting their sampling submit's fence (release back-edge).
    in_flight: VecDeque<(Arc<Fence>, SlotIndex)>,
    sync: SyncKind,
    format: RingFormat,
    extent: [u32; 2],
    policy: ExternalFramePolicy,
    latch: ExternalFrameLatch,
}

impl ExternalFrameRing {
    /// Import the producer's ring onto VSE's device.
    ///
    /// Fails loudly on any mismatch — a silently wrong import corrupts stimuli.
    /// The one-time init pass clears every imported image so its first real use
    /// never goes through vulkano's `oldLayout = Undefined` (content-discarding)
    /// first-use path, and leaves each image in `ColorAttachmentOptimal`.
    pub fn import(
        device: &Arc<Device>,
        queue: &Arc<Queue>,
        desc: ExternalRingDesc,
        release_tx: SlotReleaseTx,
    ) -> Result<Self, ExternalFrameError> {
        Self::import_with_policy(
            device,
            queue,
            desc,
            release_tx,
            ExternalFramePolicy::default(),
        )
    }

    pub fn import_with_policy(
        device: &Arc<Device>,
        queue: &Arc<Queue>,
        mut desc: ExternalRingDesc,
        release_tx: SlotReleaseTx,
        policy: ExternalFramePolicy,
    ) -> Result<Self, ExternalFrameError> {
        let enabled = device.enabled_extensions();
        if !enabled.khr_external_memory_fd {
            return Err(ExternalFrameError::Unsupported(
                "device created without VK_KHR_external_memory_fd (EXT present-timing \
                 backend with external-handle support required)"
                    .into(),
            ));
        }
        let ring_len = desc.images.len();
        if ring_len < 2 {
            return Err(ExternalFrameError::InvalidDesc(format!(
                "ring of {ring_len} image(s); need at least 2"
            )));
        }
        desc.validate_sync_shape(ring_len)
            .map_err(ExternalFrameError::InvalidDesc)?;
        match desc.sync {
            SyncKind::BinaryPerSlot => {
                if !enabled.khr_external_semaphore_fd {
                    return Err(ExternalFrameError::Unsupported(
                        "device created without VK_KHR_external_semaphore_fd".into(),
                    ));
                }
            }
            SyncKind::CpuBlocking => {
                warn!(
                    "external frame source using CpuBlocking sync (semaphore export \
                     unavailable on the producer) — producer CPU-stalls per frame"
                );
            }
            SyncKind::Timeline => {
                if !device.enabled_features().timeline_semaphore {
                    return Err(ExternalFrameError::Unsupported(
                        "device created without timeline_semaphore feature".into(),
                    ));
                }
                if !enabled.khr_external_semaphore_fd {
                    return Err(ExternalFrameError::Unsupported(
                        "device created without VK_KHR_external_semaphore_fd".into(),
                    ));
                }
            }
        }

        let format = desc
            .images
            .first()
            .map(|i| i.format)
            .expect("ring_len checked above");
        let extent = desc.images[0].extent;
        if desc
            .images
            .iter()
            .any(|i| i.format != format || i.extent != extent)
        {
            return Err(ExternalFrameError::InvalidDesc(
                "ring images differ in format or extent".into(),
            ));
        }

        // --- Import each image: exportable-image reconstruction + memory import ---
        let mut images = Vec::with_capacity(ring_len);
        for (i, img_desc) in desc.images.into_iter().enumerate() {
            let raw = RawImage::new(
                device.clone(),
                ImageCreateInfo {
                    image_type: ImageType::Dim2d,
                    format: vk_format(img_desc.format),
                    extent: [img_desc.extent[0], img_desc.extent[1], 1],
                    usage: RING_IMAGE_USAGE,
                    external_memory_handle_types: ExternalMemoryHandleTypes::OPAQUE_FD,
                    ..Default::default()
                },
            )
            .map_err(|e| ExternalFrameError::ImportFailed(format!("image {i}: {e}")))?;

            let reqs = &raw.memory_requirements()[0];
            if reqs.layout.size() > img_desc.allocation_size {
                return Err(ExternalFrameError::InvalidDesc(format!(
                    "image {i}: importer needs {} bytes, exporter allocated {}",
                    reqs.layout.size(),
                    img_desc.allocation_size
                )));
            }
            if reqs.memory_type_bits & (1 << img_desc.memory_type_index) == 0 {
                return Err(ExternalFrameError::InvalidDesc(format!(
                    "image {i}: exporter memory type {} not in importer's mask {:#x} \
                     (same-physical-device OPAQUE_FD contract violated?)",
                    img_desc.memory_type_index, reqs.memory_type_bits
                )));
            }

            // SAFETY: `memory_fd` is a valid OPAQUE_FD exported by the producer's
            // Vulkan driver on the same physical device; allocation size, memory
            // type, and dedicated-allocation shape match the exporter's (the
            // OPAQUE_FD import contract). Ownership of the fd passes to Vulkan.
            let memory = unsafe {
                DeviceMemory::import(
                    device.clone(),
                    MemoryAllocateInfo {
                        allocation_size: img_desc.allocation_size,
                        memory_type_index: img_desc.memory_type_index,
                        dedicated_allocation: Some(DedicatedAllocation::Image(&raw)),
                        ..Default::default()
                    },
                    MemoryImportInfo::Fd {
                        handle_type: ExternalMemoryHandleType::OpaqueFd,
                        file: std::fs::File::from(img_desc.memory_fd),
                    },
                )
            }
            .map_err(|e| ExternalFrameError::ImportFailed(format!("image {i} memory: {e}")))?;

            let image = raw
                .bind_memory([ResourceMemory::new_dedicated(memory)])
                .map_err(|(e, _, _)| {
                    ExternalFrameError::ImportFailed(format!("image {i} bind: {e}"))
                })?;
            images.push(Arc::new(image));
        }

        // --- Import ready semaphore(s) ---
        let mut ready_sems = Vec::new();
        if desc.sync == SyncKind::BinaryPerSlot {
            for (i, fd) in desc.ready_semaphore_fds.into_iter().enumerate() {
                let sem = Semaphore::new(device.clone(), Default::default())
                    .map_err(|e| ExternalFrameError::ImportFailed(format!("semaphore {i}: {e}")))?;
                // SAFETY: the fd is a valid OPAQUE_FD exported from a binary
                // semaphore on the same driver; permanent import (no flags).
                // Ownership of the fd passes to Vulkan.
                let mut import_info =
                    ImportSemaphoreFdInfo::handle_type(ExternalSemaphoreHandleType::OpaqueFd);
                import_info.file = Some(std::fs::File::from(fd));
                unsafe { sem.import_fd(import_info) }.map_err(|e| {
                    ExternalFrameError::ImportFailed(format!("semaphore {i} import: {e}"))
                })?;
                ready_sems.push(Arc::new(sem));
            }
        }
        let timeline_sem = if desc.sync == SyncKind::Timeline {
            let fd = desc
                .timeline_semaphore_fd
                .take()
                .expect("validate_sync_shape checked timeline fd");
            let sem = Semaphore::new(
                device.clone(),
                SemaphoreCreateInfo {
                    semaphore_type: SemaphoreType::Timeline,
                    ..Default::default()
                },
            )
            .map_err(|e| ExternalFrameError::ImportFailed(format!("timeline semaphore: {e}")))?;
            let mut import_info =
                ImportSemaphoreFdInfo::handle_type(ExternalSemaphoreHandleType::OpaqueFd);
            import_info.file = Some(std::fs::File::from(fd));
            unsafe { sem.import_fd(import_info) }.map_err(|e| {
                ExternalFrameError::ImportFailed(format!("timeline semaphore import: {e}"))
            })?;
            Some(Arc::new(sem))
        } else {
            None
        };

        // --- One-time layout init: clear every image (see doc comment) ---
        let cb_alloc = Arc::new(StandardCommandBufferAllocator::new(
            device.clone(),
            Default::default(),
        ));
        let mut builder = AutoCommandBufferBuilder::primary(
            cb_alloc,
            queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .map_err(|e| ExternalFrameError::ImportFailed(format!("init cb: {e}")))?;
        for image in &images {
            builder
                .clear_color_image(ClearColorImageInfo::image(image.clone()))
                .map_err(|e| ExternalFrameError::ImportFailed(format!("init clear: {e}")))?;
        }
        let cb = builder
            .build()
            .map_err(|e| ExternalFrameError::ImportFailed(format!("init build: {e}")))?;
        cb.execute(queue.clone())
            .map_err(|e| ExternalFrameError::ImportFailed(format!("init exec: {e}")))?
            .then_signal_fence_and_flush()
            .map_err(|e| ExternalFrameError::ImportFailed(format!("init flush: {e}")))?
            .wait(None)
            .map_err(|e| ExternalFrameError::ImportFailed(format!("init wait: {e}")))?;

        Ok(Self {
            machine: RingStateMachine::new(ring_len, desc.sync)?,
            ready_values: vec![None; ring_len],
            images,
            ready_sems,
            timeline_sem,
            release_tx,
            in_flight: VecDeque::new(),
            sync: desc.sync,
            format,
            extent,
            policy,
            latch: ExternalFrameLatch::default(),
        })
    }

    pub fn ring_len(&self) -> usize {
        self.images.len()
    }

    pub fn format(&self) -> RingFormat {
        self.format
    }

    pub fn extent(&self) -> [u32; 2] {
        self.extent
    }

    pub fn sync(&self) -> SyncKind {
        self.sync
    }

    /// The producer finished rendering `slot` (consumer-side mirror of the
    /// producer's `mark_ready`): `Producing → Ready` here, entered via
    /// `RenderContext::queue_external_frame`.
    pub(crate) fn note_ready_with_value(
        &mut self,
        slot: SlotIndex,
        timeline_value: Option<u64>,
    ) -> Result<(), ExternalFrameError> {
        match self.sync {
            SyncKind::Timeline if timeline_value.is_none() => {
                return Err(ExternalFrameError::InvalidDesc(
                    "Timeline external frame queued without a timeline value".into(),
                ));
            }
            SyncKind::BinaryPerSlot | SyncKind::CpuBlocking if timeline_value.is_some() => {
                return Err(ExternalFrameError::InvalidDesc(
                    "timeline value supplied for non-timeline external frame source".into(),
                ));
            }
            _ => {}
        }
        // Mirror the producer's actual slot choice on this side. Async
        // producers can receive release back-edge messages at different times
        // than the consumer mirror, so lowest-free FIFO acquisition is not a
        // safe cross-thread assumption.
        self.machine.acquire_specific_for_produce(slot)?;
        self.ready_values[slot.0] = timeline_value;
        self.machine.mark_ready(slot)?;
        Ok(())
    }

    /// Take the external image to use for this flip (see [`ConsumableFrames`]).
    pub(crate) fn take_frames(&mut self) -> Option<ConsumableFrames> {
        let ready_slots = self.machine.take_all_ready();
        let plan = self.latch.plan(self.policy, ready_slots);
        let display_slot = plan.display_slot?;
        let waits = match self.sync {
            SyncKind::BinaryPerSlot => plan
                .wait_slots
                .iter()
                .map(|slot| (self.ready_sems[slot.0].clone(), None))
                .collect(),
            SyncKind::Timeline => {
                let value = self.ready_values[display_slot.0]
                    .expect("timeline ready frame must carry a value");
                vec![(
                    self.timeline_sem
                        .as_ref()
                        .expect("timeline sync imports a semaphore")
                        .clone(),
                    Some(value),
                )]
            }
            SyncKind::CpuBlocking => Vec::new(),
        };
        Some(ConsumableFrames {
            image: self.images[display_slot.0].clone(),
            waits,
            slots: plan.release_after_submit,
            latch_after_submit: plan.new_latch,
        })
    }

    /// Commit latch state after the flip submit that uses `frames` succeeds.
    pub(crate) fn on_submitted(&mut self, frames: &ConsumableFrames) {
        if let Some(slot) = frames.latch_after_submit {
            self.latch.note_latched(slot);
        }
    }

    /// The flip seam consumed `slots` in a submit guarded by `fence`; when
    /// that fence signals, the blit has executed (and with it every semaphore
    /// wait), so the slots become re-signalable and are released.
    pub(crate) fn on_consumed(&mut self, slots: &[SlotIndex], fence: Arc<Fence>) {
        for slot in slots {
            self.in_flight.push_back((fence.clone(), *slot));
        }
    }

    /// Poll in-flight fences and send completed slots back to the producer.
    /// Cheap (non-blocking `is_signaled`), called once per flip, off the
    /// critical path.
    pub(crate) fn pump_releases(&mut self) {
        while let Some((fence, slot)) = self.in_flight.front() {
            match fence.is_signaled() {
                Ok(true) => {
                    let slot = *slot;
                    self.in_flight.pop_front();
                    self.ready_values[slot.0] = None;
                    if let Err(e) = self.machine.release(slot) {
                        warn!("external frame slot release bookkeeping failed: {e}");
                    }
                    self.release_tx.send(slot);
                }
                Ok(false) => break,
                Err(e) => {
                    warn!("external frame fence poll failed: {e}");
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vse_external_frame::SlotIndex;

    #[test]
    fn latest_ready_hold_last_repeats_latched_slot_when_no_new_frame_is_ready() {
        let mut latch = ExternalFrameLatch::default();
        latch.note_latched(SlotIndex(1));

        let decision = latch.plan(ExternalFramePolicy::LatestReadyHoldLast, Vec::new());

        assert_eq!(decision.display_slot, Some(SlotIndex(1)));
        assert!(decision.wait_slots.is_empty());
        assert!(decision.release_after_submit.is_empty());
        assert_eq!(decision.new_latch, Some(SlotIndex(1)));
    }

    #[test]
    fn latest_ready_hold_last_replaces_latch_without_releasing_new_slot() {
        let mut latch = ExternalFrameLatch::default();
        latch.note_latched(SlotIndex(1));

        let decision = latch.plan(
            ExternalFramePolicy::LatestReadyHoldLast,
            vec![SlotIndex(2), SlotIndex(3)],
        );

        assert_eq!(decision.display_slot, Some(SlotIndex(3)));
        assert_eq!(decision.wait_slots, vec![SlotIndex(2), SlotIndex(3)]);
        assert_eq!(
            decision.release_after_submit,
            vec![SlotIndex(2), SlotIndex(1)]
        );
        assert_eq!(decision.new_latch, Some(SlotIndex(3)));
    }
}
