//! Frame presentation paths: synchronous, EXT present timing, and buffered flips.

use std::time::Duration;

use tracing::debug;

use vulkano::sync::GpuFuture;

use super::config::{FrameError, VSEError};
use super::render_context::RenderContext;
use super::state::missed_frame_status;
use super::swapchain::SwapchainError;
use crate::timing::{FlipInfo, Timestamp};

/// Immutable metadata for one displayed-frame request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FrameRequest {
    pub(crate) frame_number: u64,
    pub(crate) target_time: Option<Timestamp>,
}

impl FrameRequest {
    fn new(frame_number: u64, target_time: Option<Timestamp>) -> Self {
        Self {
            frame_number,
            target_time,
        }
    }
}

/// Concrete backend that produced a displayed-frame submission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubmissionBackend {
    Vulkano,
    Ext,
}

/// How an EXT submission approaches a requested scanout target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PresentPacing {
    /// Pace in software before submitting, in addition to the hardware target.
    Software,
    /// Submit immediately and leave pacing to the pipelined driver queue.
    HardwareOnly,
}

/// A displayed frame whose GPU work has been submitted but may not yet be complete.
pub(crate) struct Submission {
    pub(crate) estimated_flip: FlipInfo,
    pub(crate) completion: Box<dyn crate::core::buffered::InFlightFuture>,
    pub(crate) backend: SubmissionBackend,
}

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

    /// Acquire, render, and submit one frame through vulkano without waiting for completion.
    fn submit_vulkano(&mut self, request: FrameRequest) -> Result<Option<Submission>, VSEError> {
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
                return Ok(None);
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

        if let Some(target) = request.target_time {
            self.state
                .target
                .present_expect_mut()
                .timing_provider
                .wait_for_target(target, &self.state.clock);
        }

        let submit_time = self.state.clock.now();
        let completion = self
            .state
            .target
            .present_expect_mut()
            .swapchain
            .submit_nonblocking(self.state.queue.clone(), image_index, future)?;
        let estimated_present = self.state.clock.now();

        Ok(Some(Submission {
            estimated_flip: FlipInfo {
                frame_number: request.frame_number,
                timing_source: self
                    .state
                    .target
                    .present_expect_mut()
                    .timing_provider
                    .source(),
                submit_time,
                present_time: estimated_present,
                present_id: 0,
                target_time: request.target_time,
                on_target: true,
                missed: false,
                missed_count: 0,
                skipped: false,
            },
            completion,
            backend: SubmissionBackend::Vulkano,
        }))
    }

    /// Acquire, render, and submit one frame through the raw EXT backend.
    fn submit_ext(
        &mut self,
        request: FrameRequest,
        pacing: PresentPacing,
    ) -> Result<Option<Submission>, VSEError> {
        use super::present_engine::EngineInFlight;

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

        // Read only after recreation and retain this exact handle through the matched present.
        let swapchain_handle = self.swapchain_handle();
        let (image_index, acquire_suboptimal, slot) = match self
            .state
            .target
            .present_expect_mut()
            .present_engine
            .as_mut()
            .expect("submit_ext called without a present engine")
            .acquire_next(swapchain_handle)
        {
            Ok(result) => result,
            Err(ash::vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                if let Some(engine) = &mut self.state.target.present_expect_mut().present_engine {
                    engine.wait_idle();
                }
                self.state.recreate_swapchain(win_size_arr)?;
                return Ok(None);
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

        if let Some(source) = self
            .state
            .target
            .present_expect_mut()
            .external_source
            .as_mut()
        {
            source.pump_releases();
        }
        let external_frames = self
            .state
            .target
            .present_expect_mut()
            .external_source
            .as_mut()
            .and_then(|source| source.take_frames());
        let underlay =
            external_frames
                .as_ref()
                .map(|frames| crate::drawing::renderer::ExternalUnderlay {
                    image: frames.image.clone(),
                    readback: self
                        .state
                        .target
                        .present_expect_mut()
                        .external_readback
                        .take(),
                });

        let image =
            self.state.target.present_expect_mut().swapchain.images()[image_index as usize].clone();
        let extent = self.state.target.present_expect_mut().swapchain.extent();
        let command_buffer = self.state.renderer.render_with_underlay(
            image,
            image_index as usize,
            self.config.clear_color,
            extent,
            underlay.as_ref(),
            None,
        )?;
        let scheduled = request.target_time.and_then(|target| {
            self.state
                .target
                .present_expect_mut()
                .scheduled_target(target)
        });

        if pacing == PresentPacing::Software {
            let pacing_enabled = self
                .state
                .target
                .present_expect_mut()
                .software_present_pacing;
            if let Some(target) = request.target_time.filter(|_| pacing_enabled) {
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
        }

        let submit_time = self.state.clock.now();
        let queue = self.state.queue.clone();
        let external_waits: Vec<_> = external_frames
            .as_ref()
            .map(|frames| frames.waits.clone())
            .unwrap_or_default();
        let outcome = self
            .state
            .target
            .present_expect_mut()
            .present_engine
            .as_mut()
            .expect("submit_ext called without a present engine")
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
        if let (Some(frames), Some(source)) = (
            external_frames,
            self.state
                .target
                .present_expect_mut()
                .external_source
                .as_mut(),
        ) {
            source.on_submitted(&frames);
            source.on_consumed(&frames.slots, outcome.fence.clone());
        }

        let estimated_present = self.state.clock.now();
        let present_id = outcome.present_id;
        Ok(Some(Submission {
            estimated_flip: FlipInfo {
                frame_number: request.frame_number,
                timing_source: self
                    .state
                    .target
                    .present_expect_mut()
                    .timing_provider
                    .source(),
                submit_time,
                present_time: estimated_present,
                present_id,
                target_time: request.target_time,
                on_target: true,
                missed: false,
                missed_count: 0,
                skipped: false,
            },
            completion: Box::new(EngineInFlight::new(outcome.fence)),
            backend: SubmissionBackend::Ext,
        }))
    }

    /// Drain EXT scanout records once and cache them for present-id lookup and diagnostics.
    fn drain_ext_feedback(&mut self) {
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
    }

    /// Confirm an EXT submission synchronously, including scanout wait where supported.
    fn confirm_ext(&mut self, submission: Submission) -> FlipInfo {
        debug_assert_eq!(submission.backend, SubmissionBackend::Ext);
        submission.completion.wait_blocking();
        self.state
            .target
            .present_expect_mut()
            .update_clocks(&self.state.clock);

        let estimated = submission.estimated_flip;
        const SCANOUT_WAIT_NS: u64 = 250_000_000;
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
                .wait_for_present(estimated.present_id, SCANOUT_WAIT_NS);
        self.drain_ext_feedback();

        let scanout_present = self
            .state
            .target
            .present_expect_mut()
            .take_scanout_for(estimated.present_id)
            .and_then(|feedback| feedback.first_pixel_out_ns)
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
        let on_target = match (estimated.target_time, scanout_present) {
            (Some(target), Some(scanout)) => scanout.as_micros() >= target.as_micros(),
            _ => true,
        };
        let frame_duration = self
            .state
            .target
            .present_expect_mut()
            .last_present_time
            .map(|previous| present_time.duration_since(previous));
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

        FlipInfo {
            frame_number: estimated.frame_number,
            timing_source: estimated.timing_source,
            submit_time: estimated.submit_time,
            present_time,
            present_id: estimated.present_id,
            target_time: estimated.target_time,
            on_target,
            missed,
            missed_count,
            skipped: false,
        }
    }

    /// Submit one displayed request through the active concrete backend.
    fn submit_displayed(
        &mut self,
        request: FrameRequest,
        ext_pacing: PresentPacing,
    ) -> Result<Option<Submission>, VSEError> {
        if self.state.target.present_expect().present_engine.is_some() {
            self.submit_ext(request, ext_pacing)
        } else {
            self.submit_vulkano(request)
        }
    }

    /// Perform submit-time maintenance needed before a deferred confirmation is queued.
    fn prepare_deferred_confirmation(&mut self, submission: &Submission) {
        self.state
            .target
            .present_expect_mut()
            .update_clocks(&self.state.clock);
        if submission.backend == SubmissionBackend::Ext {
            // The read is destructive: exactly one buffered call per successful EXT submit.
            self.drain_ext_feedback();
        }
    }

    /// Confirm a submission according to the backend that produced it.
    fn confirm_submission(&mut self, submission: Submission) -> FlipInfo {
        match submission.backend {
            SubmissionBackend::Vulkano => self.confirm_vulkano(submission),
            SubmissionBackend::Ext => self.confirm_ext(submission),
        }
    }

    /// Confirm a vulkano submission after its fence signals.
    fn confirm_vulkano(&mut self, submission: Submission) -> FlipInfo {
        debug_assert_eq!(submission.backend, SubmissionBackend::Vulkano);
        submission.completion.wait_blocking();
        self.state
            .target
            .present_expect_mut()
            .update_clocks(&self.state.clock);

        let present_time = self
            .state
            .target
            .present_expect_mut()
            .timing_provider
            .record_present_time(&self.state.clock);
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
            .unwrap_or(Duration::from_micros(16_667));
        let (missed, missed_count) = missed_frame_status(frame_duration, expected);
        let estimated = submission.estimated_flip;

        FlipInfo {
            frame_number: estimated.frame_number,
            timing_source: estimated.timing_source,
            submit_time: estimated.submit_time,
            present_time,
            present_id: estimated.present_id,
            target_time: estimated.target_time,
            on_target: estimated.on_target,
            missed,
            missed_count,
            skipped: false,
        }
    }

    /// Final bookkeeping shared by synchronous displayed backends.
    fn finish_synchronous_flip(&mut self, flip_info: FlipInfo) -> FlipInfo {
        if let Some(recording) = &mut self.state.recording {
            recording.on_flip(flip_info.clone());
        }
        self.state.target.present_expect_mut().last_present_time = Some(flip_info.present_time);
        self.state.frame_number += 1;
        self.state.input.clear_events();
        flip_info
    }

    /// Complete a skipped request. Skips advance numbering but do not clear input state.
    fn finish_skipped_request(&mut self, request: FrameRequest) -> FlipInfo {
        debug_assert_eq!(self.state.frame_number, request.frame_number);
        self.state.frame_number += 1;
        FlipInfo::skipped(request.frame_number)
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
        if self
            .state
            .target
            .present()
            .is_some_and(|present| present.in_buffered_mode)
        {
            return Err(VSEError::NotSupportedInBufferedMode);
        }

        // Headless: no swapchain to acquire from and nothing to present to.
        if self.state.target.offscreen_mut().is_some() {
            return self.flip_offscreen(target_time);
        }

        let request = FrameRequest::new(self.state.frame_number, target_time);
        if self.state.target.present_expect().minimized {
            return Ok(self.finish_skipped_request(request));
        }

        let submission = match self.submit_displayed(request, PresentPacing::Software) {
            Ok(Some(submission)) => submission,
            Ok(None) | Err(VSEError::Swapchain(SwapchainError::OutOfDate)) => {
                return Ok(self.finish_skipped_request(request));
            }
            Err(e) => return Err(e),
        };
        let confirmed = self.confirm_submission(submission);
        Ok(self.finish_synchronous_flip(confirmed))
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

    /// Submit one frame returned by a structured buffered render callback.
    pub(super) fn submit_buffered_frame<T>(
        &mut self,
        frame: crate::core::buffered::BufferedFrame<T>,
    ) -> Result<Option<crate::core::buffered::PendingFrame<T>>, VSEError> {
        debug_assert!(self.state.target.present_expect().in_buffered_mode);

        let request = FrameRequest::new(self.state.frame_number, frame.target_time);
        if self.state.target.present_expect().minimized {
            self.finish_skipped_request(request);
            return Ok(None);
        }

        let Some(submission) = self.submit_displayed(request, PresentPacing::HardwareOnly)? else {
            self.finish_skipped_request(request);
            return Ok(None);
        };

        self.prepare_deferred_confirmation(&submission);
        self.state.frame_number += 1;
        self.state.input.clear_events();
        Ok(Some(crate::core::buffered::PendingFrame {
            payload: frame.payload,
            submission,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::buffered::InFlightFuture;
    use crate::timing::TimingSource;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    struct TrackedCompletion(Arc<AtomicBool>);

    impl InFlightFuture for TrackedCompletion {
        fn is_complete(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }

        fn wait_blocking(&self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn estimated_flip(frame_number: u64, target_time: Option<Timestamp>) -> FlipInfo {
        FlipInfo {
            frame_number,
            timing_source: TimingSource::CpuEstimate,
            submit_time: Timestamp::from_micros(10),
            present_time: Timestamp::from_micros(20),
            present_id: 0,
            target_time,
            on_target: true,
            missed: false,
            missed_count: 0,
            skipped: false,
        }
    }

    #[test]
    fn frame_request_carries_number_and_target_through_submission() {
        let target = Timestamp::from_micros(50_000);
        let request = FrameRequest::new(7, Some(target));
        let submission = Submission {
            estimated_flip: estimated_flip(request.frame_number, request.target_time),
            completion: Box::new(TrackedCompletion(Arc::new(AtomicBool::new(false)))),
            backend: SubmissionBackend::Vulkano,
        };

        assert_eq!(submission.estimated_flip.frame_number, 7);
        assert_eq!(submission.estimated_flip.target_time, Some(target));
        assert_eq!(submission.backend, SubmissionBackend::Vulkano);
    }

    #[test]
    fn submission_owns_completion_until_confirmation() {
        let completed = Arc::new(AtomicBool::new(false));
        let submission = Submission {
            estimated_flip: estimated_flip(3, None),
            completion: Box::new(TrackedCompletion(completed.clone())),
            backend: SubmissionBackend::Ext,
        };

        assert!(!submission.completion.is_complete());
        submission.completion.wait_blocking();
        assert!(completed.load(Ordering::SeqCst));
    }
}
