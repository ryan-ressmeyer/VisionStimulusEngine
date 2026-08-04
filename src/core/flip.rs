//! Frame presentation paths: synchronous, EXT present timing, and buffered flips.

use std::time::Duration;

use tracing::debug;

use vulkano::sync::GpuFuture;

use super::config::VSEError;
use super::frame::FrameError;
use super::render_context::RenderContext;
use super::state::missed_frame_status;
use super::swapchain::SwapchainError;
use crate::timing::{FlipInfo, Timestamp};

impl<'a> RenderContext<'a> {
    /// The live `VkSwapchainKHR` handle of the present target.
    ///
    /// Read this immediately before each use and **never cache it across a call that can
    /// recreate the swapchain**. [`recreate_swapchain`] replaces the `Arc<Swapchain>`,
    /// dropping the last reference and destroying the object, so a handle captured before a
    /// recreation dangles — passing it to `vkAcquireNextImageKHR` is a use-after-free.
    ///
    /// [`recreate_swapchain`]: super::state::VSEState::recreate_swapchain
    fn swapchain_handle(&mut self) -> ash::vk::SwapchainKHR {
        use vulkano::VulkanObject;
        self.state
            .target
            .present_expect_mut()
            .swapchain
            .swapchain()
            .handle()
    }

    /// Present the current frame to the screen
    ///
    /// Optionally accepts a target presentation time. When provided:
    /// - With `ExtPresentTiming`: schedules/paces against the scanout clock
    /// - With `CpuEstimate`: spin-waits until the target time
    ///
    /// Pass `None` for immediate presentation (VSync-locked).
    ///
    /// # Errors
    ///
    /// Returns `VSEError` if presentation fails.
    pub fn flip(&mut self, target_time: Option<Timestamp>) -> Result<FlipInfo, VSEError> {
        // Headless: no swapchain to acquire from and nothing to present to.
        if self.state.target.offscreen_mut().is_some() {
            return self.flip_offscreen(target_time);
        }

        if self.state.target.present_expect().minimized {
            let info = FlipInfo::skipped(self.state.frame_number);
            self.state.frame_number += 1;
            return Ok(info);
        }

        // Handle swapchain recreation if needed
        let (dsw, dsh) = self.state.target.present_expect_mut().display_size;
        let win_size_arr = [dsw, dsh];
        if self
            .state
            .target
            .present_expect_mut()
            .swapchain
            .needs_recreation()
        {
            // Drain in-flight raw presents before the swapchain (and its images) is retired.
            if let Some(engine) = &mut self.state.target.present_expect_mut().present_engine {
                engine.wait_idle();
            }
            self.state.recreate_swapchain(win_size_arr)?;
        }

        // On the EXT present-timing backend, take the raw acquire/submit/present path (attaches
        // present-id + timing pNext, reads scanout feedback). The CPU-estimate path below is
        // unchanged.
        if self
            .state
            .target
            .present_expect_mut()
            .present_engine
            .is_some()
        {
            return self.flip_ext(target_time);
        }

        // Acquire next image
        let (image_index, _suboptimal, acquire_future) = match self
            .state
            .target
            .present_expect_mut()
            .swapchain
            .acquire_next_image()
        {
            Ok(result) => result,
            Err(SwapchainError::OutOfDate) => {
                self.state.recreate_swapchain(win_size_arr)?;
                let info = FlipInfo::skipped(self.state.frame_number);
                self.state.frame_number += 1;
                return Ok(info);
            }
            Err(e) => return Err(e.into()),
        };

        // Get the image to render to
        let image =
            self.state.target.present_expect_mut().swapchain.images()[image_index as usize].clone();
        let extent = self.state.target.present_expect_mut().swapchain.extent();

        // Record and execute drawing commands via renderer
        let command_buffer = self.state.renderer.render(
            image,
            image_index as usize,
            self.config.clear_color,
            extent,
        )?;

        let future = acquire_future
            .then_execute(self.state.queue.clone(), command_buffer)
            .map_err(|e: vulkano::command_buffer::CommandBufferExecError| {
                FrameError::ExecutionFailed(e.to_string())
            })?;

        // If target time specified, wait/schedule
        if let Some(target) = target_time {
            self.state
                .target
                .present_expect_mut()
                .timing_provider
                .wait_for_target(target, &self.state.clock);
        }

        // --- TIMING: capture submit time ---
        let submit_time = self.state.clock.now();

        // Present (submits to GPU and waits for fence)
        match self.state.target.present_expect_mut().swapchain.present(
            self.state.queue.clone(),
            image_index,
            future,
        ) {
            Ok(()) => {}
            Err(SwapchainError::OutOfDate) => {
                // Will recreate on next frame
                let info = FlipInfo::skipped(self.state.frame_number);
                self.state.frame_number += 1;
                return Ok(info);
            }
            Err(e) => return Err(e.into()),
        }

        // --- TIMING: capture present time via provider ---
        let present_time = self
            .state
            .target
            .present_expect_mut()
            .timing_provider
            .record_present_time(&self.state.clock);

        // Establish the scanout epoch / feed the opt-in host-clock bridge (off hot path).
        self.state
            .target
            .present_expect_mut()
            .update_clocks(&self.state.clock);

        // Compute inter-frame duration
        let frame_duration = self
            .state
            .target
            .present_expect_mut()
            .last_present_time
            .map(|prev| present_time.duration_since(prev));

        self.state
            .target
            .present_expect_mut()
            .update_refresh_detection(frame_duration, true, true);

        let expected = self
            .state
            .target
            .present_expect_mut()
            .expected_frame_duration
            .unwrap_or(Duration::from_micros(16_667)); // 60 Hz fallback

        let (missed, missed_count) = missed_frame_status(frame_duration, expected);

        // present_id is assigned by the EXT present path; 0 on the CPU-estimate path.
        let present_id: u64 = 0;

        let flip_info = FlipInfo {
            frame_number: self.state.frame_number,
            timing_source: self
                .state
                .target
                .present_expect_mut()
                .timing_provider
                .source(),
            submit_time,
            present_time,
            present_id,
            target_time,
            on_target: true,
            missed,
            missed_count,
            skipped: false,
        };

        // Notify RecordingState of new flip
        if let Some(recording) = &mut self.state.recording {
            recording.on_flip(flip_info.clone());
        }

        // Update state for next frame
        self.state.target.present_expect_mut().last_present_time = Some(present_time);
        self.state.frame_number += 1;

        // Clear input event queue after the frame
        self.state.input.clear_events();

        Ok(flip_info)
    }

    /// Headless flip: render into the offscreen image, copy it back to host
    /// memory, and hand the pixels to the run loop's sink.
    ///
    /// Blocks on the fence — there is no vblank to pipeline against, and the
    /// readback is only valid once the GPU is done. `target_time` is accepted
    /// and recorded but never waited on: pacing an offscreen render against a
    /// wall clock would only slow regeneration down.
    ///
    /// The returned [`FlipInfo`] carries [`TimingSource::Offscreen`] and a
    /// synthesized `present_time` of `frame_number × frame_interval`.
    fn flip_offscreen(&mut self, target_time: Option<Timestamp>) -> Result<FlipInfo, VSEError> {
        use vulkano::sync::GpuFuture as _;

        let clear_color = self.config.clear_color;
        let submit_time = self.state.clock.now();

        let offscreen = self
            .state
            .target
            .offscreen_mut()
            .expect("flip_offscreen called on a presenting context");
        let image = offscreen.image.clone();
        let extent = offscreen.extent;
        let readback = offscreen.readback.clone();
        let frame_interval = offscreen.frame_interval;

        let command_buffer =
            self.state
                .renderer
                .render_to_offscreen(image, 0, clear_color, extent, &readback)?;

        let future = vulkano::sync::now(self.state.device.clone())
            .then_execute(self.state.queue.clone(), command_buffer)
            .map_err(|e: vulkano::command_buffer::CommandBufferExecError| {
                FrameError::ExecutionFailed(e.to_string())
            })?
            .then_signal_fence_and_flush()
            .map_err(|e| FrameError::ExecutionFailed(e.to_string()))?;
        future
            .wait(None)
            .map_err(|e| FrameError::ExecutionFailed(e.to_string()))?;

        let bytes = readback
            .read()
            .map_err(|e| FrameError::ExecutionFailed(format!("readback map failed: {e}")))?
            .to_vec();

        let frame_number = self.state.frame_number;
        // Synthesized, not measured: the k-th frame is nominally shown one
        // refresh interval after the (k-1)-th. See `TimingSource::Offscreen`.
        let present_time =
            Timestamp::from_micros(frame_number.saturating_mul(frame_interval.as_micros() as u64));

        let flip_info = FlipInfo {
            frame_number,
            timing_source: crate::timing::TimingSource::Offscreen,
            submit_time,
            present_time,
            present_id: 0,
            target_time,
            on_target: true,
            missed: false,
            missed_count: 0,
            skipped: false,
        };

        self.state
            .target
            .offscreen_mut()
            .expect("flip_offscreen called on a presenting context")
            .push_capture(frame_number, bytes);

        if let Some(recording) = &mut self.state.recording {
            recording.on_flip(flip_info.clone());
        }

        self.state.frame_number += 1;
        self.state.input.clear_events();

        Ok(flip_info)
    }

    /// Synchronous flip on the `VK_EXT_present_timing` backend: raw acquire → submit → present
    /// with a [`PresentChain`](crate::core::present_timing_ext::PresentChain) (present-id +
    /// scanout-timing request) attached to `vkQueuePresentKHR`, plus a
    /// `vkGetPastPresentationTimingEXT` feedback read.
    ///
    /// `FlipInfo.present_id` becomes the driver's real `VkPresentId2` value. `present_time` is
    /// still CPU fence time in B1 (B3 makes it a scanout-native timestamp). The CPU-estimate
    /// path stays on vulkano's present in [`flip()`](Self::flip).
    fn flip_ext(&mut self, target_time: Option<Timestamp>) -> Result<FlipInfo, VSEError> {
        let clear_color = self.config.clear_color;
        // Safe to hold across this whole function: unlike the buffered path there is no
        // pre-acquire recreation here — the only recreation is in the `OUT_OF_DATE` arm below,
        // which returns immediately without touching the handle again.
        let swapchain_handle = self.swapchain_handle();
        let (dsw, dsh) = self.state.target.present_expect_mut().display_size;
        let win_size_arr = [dsw, dsh];

        // --- Acquire (raw, signals the slot's acquire semaphore) ---
        let (image_index, acquire_suboptimal, slot) = match self
            .state
            .target
            .present_expect_mut()
            .present_engine
            .as_mut()
            .expect("flip_ext called without a present engine")
            .acquire_next(swapchain_handle)
        {
            Ok(r) => r,
            Err(ash::vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                if let Some(engine) = &mut self.state.target.present_expect_mut().present_engine {
                    engine.wait_idle();
                }
                self.state.recreate_swapchain(win_size_arr)?;
                let info = FlipInfo::skipped(self.state.frame_number);
                self.state.frame_number += 1;
                return Ok(info);
            }
            Err(e) => {
                return Err(SwapchainError::AcquireFailed(format!("{e:?}")).into());
            }
        };
        if acquire_suboptimal {
            self.state
                .target
                .present_expect_mut()
                .swapchain
                .mark_needs_recreation();
        }

        // --- External frame source: release completed slots, take this frame's underlay ---
        if let Some(src) = self
            .state
            .target
            .present_expect_mut()
            .external_source
            .as_mut()
        {
            src.pump_releases();
        }
        let ext_frames = self
            .state
            .target
            .present_expect_mut()
            .external_source
            .as_mut()
            .and_then(|src| src.take_frames());
        let underlay = ext_frames
            .as_ref()
            .map(|f| crate::drawing::renderer::ExternalUnderlay {
                image: f.image.clone(),
                readback: self
                    .state
                    .target
                    .present_expect_mut()
                    .external_readback
                    .take(),
            });

        // --- Render into the acquired image ---
        let image =
            self.state.target.present_expect_mut().swapchain.images()[image_index as usize].clone();
        let extent = self.state.target.present_expect_mut().swapchain.extent();
        let command_buffer = self.state.renderer.render_with_underlay(
            image,
            image_index as usize,
            clear_color,
            extent,
            underlay.as_ref(),
            None,
        )?;

        // Hardware scheduling: express the target (scanout-domain µs) as an absolute scanout time
        // for `VkPresentTimingInfoEXT.targetTime`. Falls back to unscheduled when the scanout
        // epoch/domain isn't known yet (the very first flip, before `t=0` is established).
        let scheduled =
            target_time.and_then(|t| self.state.target.present_expect_mut().scheduled_target(t));

        // Software scanout-domain pacing: a driver may not enforce `targetTime`, so pace the
        // present ourselves against the scanout clock. Harmless when the driver *does* honor the
        // hardware target above. Sync path only (buffered stays pipelined).
        //
        // `software_present_pacing` disables this for driver characterization only: with pacing on,
        // a scheduling measurement can only ever confirm VSE's own pacing, never the hardware.
        let pacing_enabled = self
            .state
            .target
            .present_expect_mut()
            .software_present_pacing;
        if let Some(target) = target_time.filter(|_| pacing_enabled) {
            self.state
                .target
                .present_expect_mut()
                .note_scheduling_once();
            if let Some(refresh) = self.state.target.present_expect_mut().refresh_interval() {
                self.state
                    .target
                    .present_expect_mut()
                    .pace_to_scanout_target(target, refresh);
            }
        }

        let submit_time = self.state.clock.now();

        // --- Submit + raw present with the timing pNext chain ---
        let queue = self.state.queue.clone();
        let external_waits: Vec<_> = ext_frames
            .as_ref()
            .map(|f| f.waits.clone())
            .unwrap_or_default();
        let outcome = self
            .state
            .target
            .present_expect_mut()
            .present_engine
            .as_mut()
            .expect("flip_ext called without a present engine")
            .submit_and_present(
                &queue,
                swapchain_handle,
                image_index,
                slot,
                command_buffer,
                scheduled,
                &external_waits,
            )
            .map_err(SwapchainError::PresentFailed)?;
        if outcome.suboptimal {
            self.state
                .target
                .present_expect_mut()
                .swapchain
                .mark_needs_recreation();
        }
        // Release back-edge: releasable slots return to the producer once this
        // submit's fence signals. In latched mode, the displayed slot remains
        // owned by VSE until a replacement submit succeeds.
        if let (Some(f), Some(src)) = (
            ext_frames,
            self.state
                .target
                .present_expect_mut()
                .external_source
                .as_mut(),
        ) {
            src.on_submitted(&f);
            src.on_consumed(&f.slots, outcome.fence.clone());
        }

        // Synchronous flip(): block on the render fence (GPU render done) before sampling — cheap,
        // and keeps the command buffer alive. The scanout-time wait below paces to the vblank.
        if let Some(engine) = &self.state.target.present_expect_mut().present_engine {
            engine.wait_frame(slot);
        }

        // Establish the scanout epoch on the first flip / feed the opt-in host-clock bridge (off
        // hot path). Must run before rebasing scanout feedback into scanout-domain time below.
        self.state
            .target
            .present_expect_mut()
            .update_clocks(&self.state.clock);

        // Block until THIS frame has begun scanout, so its IMAGE_FIRST_PIXEL_OUT feedback record
        // has landed (feedback lags the present by ~1 vblank — that lag is why the sync path needs
        // present-wait2). Only legal on a present-wait2 swapchain, so gate on that; falls back to
        // CPU fence time when present-wait2 is unavailable or the wait times out.
        const SCANOUT_WAIT_NS: u64 = 250_000_000; // 250 ms safety cap (≫ one vblank)
        let waited = self
            .state
            .target
            .present_expect_mut()
            .swapchain
            .present_wait2_enabled()
            && self
                .state
                .target
                .present_expect_mut()
                .timing_provider
                .wait_for_present(outcome.present_id, SCANOUT_WAIT_NS);

        // Drain confirmed scanout records ONCE (destructive dequeue) and cache them: populates
        // `scanout_by_present_id` (present-id keyed) and `recent_scanouts` (for `scanout_feedback()`).
        let feedback = self
            .state
            .target
            .present_expect_mut()
            .timing_provider
            .query_scanouts();
        self.state
            .target
            .present_expect_mut()
            .ingest_scanout_feedback(feedback);
        let present = self.state.target.present_expect();
        if let Some(last) = present.recent_scanouts.last() {
            debug!(
                "scanout feedback: {} record(s); latest present_id={} first_pixel_out={:?} domain={}",
                present.recent_scanouts.len(),
                last.present_id,
                last.first_pixel_out_ns,
                last.time_domain
            );
        }

        // present_time is THIS frame's real hardware scanout time — the scientifically meaningful
        // "photons started" timestamp, in the scanout domain. Source order:
        //   1. the driver's `IMAGE_FIRST_PIXEL_OUT` feedback for this present, when the driver fills
        //      it (a true per-present scanout time);
        //   2. else — since `wait_for_present` just blocked until this frame began scanout — the
        //      calibrated present-stage-local clock sampled now (Intel/ANV/Mesa 26.1 stubs the
        //      feedback stage times to 0, so this is the real scanout source on that driver);
        //   3. else CPU fence-signal time (windowed with no scanout, no present-wait2, or before
        //      the scanout epoch is established).
        let scanout_present = self
            .state
            .target
            .present_expect_mut()
            .take_scanout_for(outcome.present_id)
            .and_then(|fb| fb.first_pixel_out_ns)
            .and_then(|ns| {
                self.state
                    .target
                    .present_expect_mut()
                    .scanout_present_time(ns)
            })
            .or_else(|| {
                waited
                    .then(|| self.state.target.present_expect_mut().sample_scanout_now())
                    .flatten()
            });
        let present_time = scanout_present.unwrap_or_else(|| {
            self.state
                .target
                .present_expect_mut()
                .timing_provider
                .record_present_time(&self.state.clock)
        });

        // on_target is only knowable against a real scanout time (same domain as the target). When
        // there is one, the scheduled present met its deadline iff scanout landed at/after target;
        // without one (windowed) we cannot verify, so report `true`.
        let on_target = match (target_time, scanout_present) {
            (Some(target), Some(sp)) => sp.as_micros() >= target.as_micros(),
            _ => true,
        };

        // --- Shared bottom half: refresh detect, missed-frame detection, FlipInfo assembly ---
        let frame_duration = self
            .state
            .target
            .present_expect_mut()
            .last_present_time
            .map(|prev| present_time.duration_since(prev));

        self.state
            .target
            .present_expect_mut()
            .update_refresh_detection(frame_duration, true, false);

        let expected = self
            .state
            .target
            .present_expect_mut()
            .expected_frame_duration
            .unwrap_or(Duration::from_micros(16_667));

        let (missed, missed_count) = missed_frame_status(frame_duration, expected);

        let flip_info = FlipInfo {
            frame_number: self.state.frame_number,
            timing_source: self
                .state
                .target
                .present_expect_mut()
                .timing_provider
                .source(),
            submit_time,
            present_time,
            present_id: outcome.present_id,
            target_time,
            on_target,
            missed,
            missed_count,
            skipped: false,
        };

        if let Some(recording) = &mut self.state.recording {
            recording.on_flip(flip_info.clone());
        }

        self.state.target.present_expect_mut().last_present_time = Some(present_time);
        self.state.frame_number += 1;
        self.state.input.clear_events();

        Ok(flip_info)
    }

    /// Submit the current frame to the GPU without blocking, attaching a typed payload.
    ///
    /// Only valid inside the [`FlipEvent::Render`] arm of [`VSEContext::run_buffered`].
    /// The `payload` is stored and delivered alongside the confirmed [`FlipInfo`] in
    /// the next [`FlipEvent::Presented`] for this frame.
    ///
    /// After this call returns, the GPU is processing frame `N` while the CPU is free
    /// to compute frame `N+1` on the next vblank.
    ///
    /// The `target_time` argument optionally schedules the present for a specific
    /// [`Timestamp`]. Pass `None` for immediate VSync-locked presentation.
    ///
    /// Call this method exactly **once** per `Render` event. Calling it multiple times
    /// or not at all results in queue desynchronisation.
    ///
    /// # Errors
    ///
    /// - [`VSEError::NotInBufferedMode`] if called from `run()` instead of `run_buffered()`.
    /// - [`VSEError::Swapchain`] if image acquisition or submission fails.
    pub fn flip_with_payload<T: std::any::Any + Send + 'static>(
        &mut self,
        target_time: Option<Timestamp>,
        payload: T,
    ) -> Result<(), VSEError> {
        if !self.state.target.present_expect_mut().in_buffered_mode {
            return Err(VSEError::NotInBufferedMode);
        }

        if self.state.target.present_expect_mut().minimized {
            // Skip silently — no fence, no payload stored; run_buffered() skips push.
            self.state.frame_number += 1;
            return Ok(());
        }

        // On the EXT backend, submit through the raw present engine (present-id + timing chain).
        if self
            .state
            .target
            .present_expect_mut()
            .present_engine
            .is_some()
        {
            return self.flip_with_payload_ext(target_time, payload);
        }

        // Recreate swapchain if needed
        let (dsw, dsh) = self.state.target.present_expect_mut().display_size;
        let win_size_arr = [dsw, dsh];
        if self
            .state
            .target
            .present_expect_mut()
            .swapchain
            .needs_recreation()
        {
            self.state.recreate_swapchain(win_size_arr)?;
        }

        // Acquire next image (natural backpressure from driver)
        let (image_index, _suboptimal, acquire_future) = match self
            .state
            .target
            .present_expect_mut()
            .swapchain
            .acquire_next_image()
        {
            Ok(r) => r,
            Err(SwapchainError::OutOfDate) => {
                self.state.recreate_swapchain(win_size_arr)?;
                self.state.frame_number += 1;
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };

        let image =
            self.state.target.present_expect_mut().swapchain.images()[image_index as usize].clone();
        let extent = self.state.target.present_expect_mut().swapchain.extent();

        let command_buffer = self.state.renderer.render(
            image,
            image_index as usize,
            self.config.clear_color,
            extent,
        )?;

        let future = acquire_future
            .then_execute(self.state.queue.clone(), command_buffer)
            .map_err(|e: vulkano::command_buffer::CommandBufferExecError| {
                FrameError::ExecutionFailed(e.to_string())
            })?;

        // Optional CPU spin-wait for scheduled present time
        if let Some(target) = target_time {
            self.state
                .target
                .present_expect_mut()
                .timing_provider
                .wait_for_target(target, &self.state.clock);
        }

        let submit_time = self.state.clock.now();

        // Non-blocking submit — returns immediately, keeps fence alive
        let in_flight = self
            .state
            .target
            .present_expect_mut()
            .swapchain
            .submit_nonblocking(self.state.queue.clone(), image_index, future)?;

        let estimated_present = self.state.clock.now();

        // Establish the scanout epoch / feed the opt-in host-clock bridge (off hot path).
        self.state
            .target
            .present_expect_mut()
            .update_clocks(&self.state.clock);

        // present_id is assigned by the EXT present path; 0 on the CPU-estimate path.
        let present_id: u64 = 0;

        let estimated_flip = FlipInfo {
            frame_number: self.state.frame_number,
            timing_source: self
                .state
                .target
                .present_expect_mut()
                .timing_provider
                .source(),
            submit_time,
            present_time: estimated_present,
            present_id,
            target_time,
            on_target: true,
            missed: false,
            missed_count: 0,
            skipped: false,
        };

        self.state
            .target
            .present_expect_mut()
            .buffered_pending_frames
            .push_back(crate::core::buffered::PendingFrame {
                payload: Box::new(payload),
                estimated_flip,
                completion: in_flight,
            });

        self.state.frame_number += 1;
        self.state.input.clear_events();

        Ok(())
    }

    /// Buffered (non-blocking) flip on the `VK_EXT_present_timing` backend.
    ///
    /// Mirrors [`flip_with_payload`](Self::flip_with_payload) but drives the raw acquire → submit →
    /// present engine: the present carries a real `VkPresentId2` + timing chain, and the frame's
    /// slot fence is wrapped as the in-flight future `run_buffered()` polls. Unlike
    /// [`flip_ext`](Self::flip_ext) it does **not** block on the fence — that is the point of the
    /// buffered pipeline. Scanout feedback is drained once here (the driver read is destructive)
    /// and cached for present-id-keyed confirmation.
    fn flip_with_payload_ext<T: std::any::Any + Send + 'static>(
        &mut self,
        target_time: Option<Timestamp>,
        payload: T,
    ) -> Result<(), VSEError> {
        use super::present_engine::EngineInFlight;

        let clear_color = self.config.clear_color;
        let (dsw, dsh) = self.state.target.present_expect_mut().display_size;
        let win_size_arr = [dsw, dsh];

        if self
            .state
            .target
            .present_expect_mut()
            .swapchain
            .needs_recreation()
        {
            if let Some(engine) = &mut self.state.target.present_expect_mut().present_engine {
                engine.wait_idle();
            }
            self.state.recreate_swapchain(win_size_arr)?;
        }

        // --- Acquire (raw) ---
        // Read the handle *after* the recreation above, not before it: the old swapchain is
        // destroyed there, and this path — unlike the synchronous one — reaches the acquire
        // with the recreation already done. See `swapchain_handle`.
        let swapchain_handle = self.swapchain_handle();
        let (image_index, acquire_suboptimal, slot) = match self
            .state
            .target
            .present_expect_mut()
            .present_engine
            .as_mut()
            .expect("flip_with_payload_ext called without a present engine")
            .acquire_next(swapchain_handle)
        {
            Ok(r) => r,
            Err(ash::vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                if let Some(engine) = &mut self.state.target.present_expect_mut().present_engine {
                    engine.wait_idle();
                }
                self.state.recreate_swapchain(win_size_arr)?;
                self.state.frame_number += 1;
                return Ok(());
            }
            Err(e) => return Err(SwapchainError::AcquireFailed(format!("{e:?}")).into()),
        };
        if acquire_suboptimal {
            self.state
                .target
                .present_expect_mut()
                .swapchain
                .mark_needs_recreation();
        }

        // --- External frame source: release completed slots, take this frame's underlay ---
        if let Some(src) = self
            .state
            .target
            .present_expect_mut()
            .external_source
            .as_mut()
        {
            src.pump_releases();
        }
        let ext_frames = self
            .state
            .target
            .present_expect_mut()
            .external_source
            .as_mut()
            .and_then(|src| src.take_frames());
        let underlay = ext_frames
            .as_ref()
            .map(|f| crate::drawing::renderer::ExternalUnderlay {
                image: f.image.clone(),
                readback: self
                    .state
                    .target
                    .present_expect_mut()
                    .external_readback
                    .take(),
            });

        // --- Render ---
        let image =
            self.state.target.present_expect_mut().swapchain.images()[image_index as usize].clone();
        let extent = self.state.target.present_expect_mut().swapchain.extent();
        let command_buffer = self.state.renderer.render_with_underlay(
            image,
            image_index as usize,
            clear_color,
            extent,
            underlay.as_ref(),
            None,
        )?;

        // Hardware scheduling (no CPU spin): express the target as an absolute scanout time for
        // the driver. Falls back to unscheduled before the scanout epoch/domain is known.
        let scheduled =
            target_time.and_then(|t| self.state.target.present_expect_mut().scheduled_target(t));

        let submit_time = self.state.clock.now();

        // --- Submit + raw present (non-blocking) ---
        let queue = self.state.queue.clone();
        let external_waits: Vec<_> = ext_frames
            .as_ref()
            .map(|f| f.waits.clone())
            .unwrap_or_default();
        let outcome = self
            .state
            .target
            .present_expect_mut()
            .present_engine
            .as_mut()
            .expect("flip_with_payload_ext called without a present engine")
            .submit_and_present(
                &queue,
                // Deliberately the *same* handle the acquire used, not a fresh read: acquire and
                // present are a matched pair, and `image_index` indexes that swapchain's images.
                // A re-read here would paper over a mismatch rather than guard against one —
                // nothing between the acquire above and this submit can recreate the swapchain.
                swapchain_handle,
                image_index,
                slot,
                command_buffer,
                scheduled,
                &external_waits,
            )
            .map_err(SwapchainError::PresentFailed)?;
        if outcome.suboptimal {
            self.state
                .target
                .present_expect_mut()
                .swapchain
                .mark_needs_recreation();
        }
        // Release back-edge: releasable slots return to the producer once this
        // submit's fence signals. In latched mode, the displayed slot remains
        // owned by VSE until a replacement submit succeeds.
        if let (Some(f), Some(src)) = (
            &ext_frames,
            self.state
                .target
                .present_expect_mut()
                .external_source
                .as_mut(),
        ) {
            src.on_submitted(f);
            src.on_consumed(&f.slots, outcome.fence.clone());
        }

        let estimated_present = self.state.clock.now();
        self.state
            .target
            .present_expect_mut()
            .update_clocks(&self.state.clock);

        // Drain confirmed scanout records once (destructive read) and cache them for
        // present-id-keyed confirmation in `build_confirmed_flip`.
        let feedback = self
            .state
            .target
            .present_expect_mut()
            .timing_provider
            .query_scanouts();
        self.state
            .target
            .present_expect_mut()
            .ingest_scanout_feedback(feedback);

        let in_flight: Box<dyn crate::core::buffered::InFlightFuture> =
            Box::new(EngineInFlight::new(outcome.fence));

        let estimated_flip = FlipInfo {
            frame_number: self.state.frame_number,
            timing_source: self
                .state
                .target
                .present_expect_mut()
                .timing_provider
                .source(),
            submit_time,
            present_time: estimated_present,
            present_id: outcome.present_id,
            target_time,
            on_target: true,
            missed: false,
            missed_count: 0,
            skipped: false,
        };

        self.state
            .target
            .present_expect_mut()
            .buffered_pending_frames
            .push_back(crate::core::buffered::PendingFrame {
                payload: Box::new(payload),
                estimated_flip,
                completion: in_flight,
            });

        self.state.frame_number += 1;
        self.state.input.clear_events();

        Ok(())
    }
}
