//! Raw present engine for the `VK_EXT_present_timing` path.
//!
//! vulkano's presentation (`then_swapchain_present`) offers no hook to attach a `pNext` chain to
//! `vkQueuePresentKHR`, and the present-timing feedback query returns **empty** unless the present
//! carried `VkPresentTimingsInfoEXT`. So on the EXT backend VSE bypasses vulkano's present with a
//! hand-rolled acquire → submit → present loop:
//!
//! 1. `vkAcquireNextImageKHR` (ash) signals a VSE-owned binary [`Semaphore`].
//! 2. `QueueGuard::submit` runs the renderer's command buffer, waiting on the acquire semaphore at
//!    `COLOR_ATTACHMENT_OUTPUT` and signalling a render-finished [`Semaphore`] + a [`Fence`].
//! 3. Raw `vkQueuePresentKHR` (ash) presents the image, waiting on render-finished, with a
//!    [`PresentChain`] (`VkPresentId2KHR` + `VkPresentTimingsInfoEXT`) attached via `pNext`.
//!
//! Sync objects live in a small ring keyed by frame-in-flight (`image_count + 1` slots): a slot's
//! fence is waited before the slot is reused, which both retires the binary-semaphore reuse hazard
//! and keeps the previous frame's command buffer alive exactly long enough. The CPU-estimate path
//! keeps using vulkano's present unchanged; this engine is only built on the EXT backend.

use std::sync::Arc;

use ash::vk;
use vulkano::command_buffer::{CommandBufferSubmitInfo, PrimaryCommandBufferAbstract, SubmitInfo};
use vulkano::device::{Device, Queue};
use vulkano::sync::fence::{Fence, FenceCreateFlags, FenceCreateInfo};
use vulkano::sync::semaphore::{Semaphore, SemaphoreCreateInfo};
use vulkano::sync::PipelineStages;
use vulkano::VulkanObject;

use super::present_timing_ext::PresentChain;

/// Per-frame-in-flight synchronization set.
struct FrameSync {
    /// Signalled by `vkAcquireNextImageKHR`, waited by the render submit.
    acquire: Arc<Semaphore>,
    /// Signalled by the render submit, waited by `vkQueuePresentKHR`.
    render_finished: Arc<Semaphore>,
    /// Signalled by the render submit; waited before this slot is reused.
    fence: Arc<Fence>,
    /// The command buffer submitted with this slot, kept alive until the fence is next waited
    /// (the GPU may still be reading it until then).
    command_buffer: Option<Arc<dyn PrimaryCommandBufferAbstract>>,
}

/// Outcome of a raw present.
pub struct PresentOutcome {
    /// The `VkPresentId2` value assigned to this present.
    pub present_id: u64,
    /// Whether the swapchain reported itself suboptimal (needs recreation).
    pub suboptimal: bool,
}

/// Owns the raw acquire/submit/present machinery for the EXT present-timing path.
pub struct PresentEngine {
    swapchain_fns: ash::khr::swapchain::Device,
    ring: Vec<FrameSync>,
    /// Monotonic frame counter; selects the ring slot and seeds the present id.
    counter: u64,
}

impl PresentEngine {
    /// Build the engine for a device, sizing the sync ring to `image_count + 1` slots.
    ///
    /// Returns `None` if the swapchain function pointers or sync objects cannot be created.
    pub fn new(device: &Arc<Device>, image_count: u32) -> Option<Self> {
        let swapchain_fns = build_swapchain_device(device);

        let ring_size = (image_count as usize).saturating_add(1).max(2);
        let mut ring = Vec::with_capacity(ring_size);
        for _ in 0..ring_size {
            let acquire = Semaphore::new(device.clone(), SemaphoreCreateInfo::default()).ok()?;
            let render_finished =
                Semaphore::new(device.clone(), SemaphoreCreateInfo::default()).ok()?;
            // Created signalled so the first reuse-wait on each slot returns immediately.
            let fence = Fence::new(
                device.clone(),
                FenceCreateInfo {
                    flags: FenceCreateFlags::SIGNALED,
                    ..Default::default()
                },
            )
            .ok()?;
            ring.push(FrameSync {
                acquire: Arc::new(acquire),
                render_finished: Arc::new(render_finished),
                fence: Arc::new(fence),
                command_buffer: None,
            });
        }

        Some(Self {
            swapchain_fns,
            ring,
            counter: 0,
        })
    }

    /// The `VkPresentId2` value the next present will carry: the running count of successful
    /// acquires (`acquire_next` has already incremented `counter` for this frame). So the first
    /// present is id 1 and ids are non-zero, unique, and strictly increasing — zero is reserved
    /// for "no present id" (the CPU-estimate path).
    fn next_present_id(&self) -> u64 {
        self.counter
    }

    /// Acquire the next swapchain image for the current frame.
    ///
    /// Advances the frame counter, waits+resets the reused slot's fence (guaranteeing its previous
    /// submit finished and freeing its command buffer), then acquires into that slot's acquire
    /// semaphore. Returns the image index and the ring slot to pass to [`submit_and_present`].
    ///
    /// [`submit_and_present`]: Self::submit_and_present
    pub fn acquire_next(
        &mut self,
        swapchain: vk::SwapchainKHR,
    ) -> Result<(u32, bool, usize), vk::Result> {
        let slot = (self.counter % self.ring.len() as u64) as usize;

        // Wait for this slot's previous frame to finish before reusing its objects.
        let _ = self.ring[slot].fence.wait(None);
        // SAFETY: the fence is signalled (just waited), so resetting it is legal.
        unsafe {
            let _ = self.ring[slot].fence.reset();
        }
        // Drop the previous command buffer now the GPU is done with it.
        self.ring[slot].command_buffer = None;

        let acquire_handle = self.ring[slot].acquire.handle();
        // SAFETY: swapchain + semaphore belong to this device; timeout u64::MAX blocks until ready.
        let (image_index, suboptimal) = unsafe {
            self.swapchain_fns.acquire_next_image(
                swapchain,
                u64::MAX,
                acquire_handle,
                vk::Fence::null(),
            )?
        };

        self.counter += 1;
        Ok((image_index, suboptimal, slot))
    }

    /// Submit the rendered command buffer and present the image with the timing `pNext` chain.
    ///
    /// The submit waits on the slot's acquire semaphore at `COLOR_ATTACHMENT_OUTPUT` and signals
    /// the render-finished semaphore + the slot fence. The present waits on render-finished and
    /// attaches [`PresentChain::unscheduled`] (present-id + scanout timing request).
    pub fn submit_and_present(
        &mut self,
        queue: &Arc<Queue>,
        swapchain: vk::SwapchainKHR,
        image_index: u32,
        slot: usize,
        command_buffer: Arc<dyn PrimaryCommandBufferAbstract>,
    ) -> Result<PresentOutcome, String> {
        // --- Submit: wait acquire@COLOR_ATTACHMENT_OUTPUT, run cmd buf, signal render + fence ---
        let mut wait =
            vulkano::command_buffer::SemaphoreSubmitInfo::new(self.ring[slot].acquire.clone());
        wait.stages = PipelineStages::COLOR_ATTACHMENT_OUTPUT;

        let submit_info = SubmitInfo {
            wait_semaphores: vec![wait],
            command_buffers: vec![CommandBufferSubmitInfo::new(command_buffer.clone())],
            signal_semaphores: vec![vulkano::command_buffer::SemaphoreSubmitInfo::new(
                self.ring[slot].render_finished.clone(),
            )],
            ..Default::default()
        };

        let fence = self.ring[slot].fence.clone();
        queue
            .with(|mut guard| unsafe { guard.submit(&[submit_info], Some(&fence)) })
            .map_err(|e| format!("QueueGuard::submit failed: {e:?}"))?;

        // Keep the command buffer alive until this slot's fence is next waited.
        self.ring[slot].command_buffer = Some(command_buffer);

        // --- Present: wait render-finished, attach the timing pNext chain ---
        let present_id = self.next_present_id();
        let chain = PresentChain::unscheduled(present_id);

        let render_sem = self.ring[slot].render_finished.handle();
        let swapchains = [swapchain];
        let image_indices = [image_index];
        let wait_sems = [render_sem];

        let mut present_info = vk::PresentInfoKHR::default()
            .wait_semaphores(&wait_sems)
            .swapchains(&swapchains)
            .image_indices(&image_indices);
        present_info.p_next = chain.head();

        // SAFETY: all handles belong to this device/queue; `chain` outlives the call.
        let suboptimal = unsafe {
            self.swapchain_fns
                .queue_present(queue.handle(), &present_info)
                .map_err(|e| format!("vkQueuePresentKHR failed: {e:?}"))?
        };
        drop(chain);

        Ok(PresentOutcome {
            present_id,
            suboptimal,
        })
    }

    /// Block until the given slot's render submit has completed (its fence signalled).
    ///
    /// Used by the **synchronous** `flip()` path to keep it truly synchronous — capturing a
    /// present time only after the frame's GPU work finished, so inter-frame deltas track the
    /// vblank cadence rather than the free-running CPU loop. The buffered path does *not* call
    /// this (it confirms asynchronously via the fence instead).
    pub fn wait_frame(&self, slot: usize) {
        let _ = self.ring[slot].fence.wait(None);
    }

    /// Wait for every in-flight frame to finish. Called before swapchain recreation so no pending
    /// submit still references the retiring swapchain's images.
    pub fn wait_idle(&mut self) {
        for slot in &mut self.ring {
            let _ = slot.fence.wait(None);
            slot.command_buffer = None;
        }
    }
}

/// Build an `ash::khr::swapchain::Device` from vulkano's already-loaded device loader, mirroring
/// the loader pattern used elsewhere for raw extension entry points.
fn build_swapchain_device(device: &Arc<Device>) -> ash::khr::swapchain::Device {
    let instance = device.instance();
    let get_dpa = instance.fns().v1_0.get_device_proc_addr;
    let dev_handle = device.handle();
    unsafe {
        let ash_instance = ash::Instance::load_with(
            |name| {
                std::mem::transmute(
                    instance
                        .library()
                        .get_instance_proc_addr(instance.handle(), name.as_ptr()),
                )
            },
            instance.handle(),
        );
        let ash_device = ash::Device::load_with(
            |name| std::mem::transmute(get_dpa(dev_handle, name.as_ptr())),
            dev_handle,
        );
        ash::khr::swapchain::Device::new(&ash_instance, &ash_device)
    }
}
