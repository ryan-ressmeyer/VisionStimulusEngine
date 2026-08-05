use super::*;

impl<'a> RenderContext<'a> {
    /// Attach an external-renderer frame source (see `core::external_frame`).
    ///
    /// Imports the producer's exported image ring + ready semaphores onto VSE's
    /// device. Subsequent flips consume queued external frames as a full-screen
    /// underlay beneath VSE's own draw commands; VSE remains sole present
    /// authority and `FlipInfo` is computed exactly as without a source.
    ///
    /// Displayed sessions require the EXT present engine because the CPU-estimate present path has
    /// no seam for cross-device semaphore waits. Headless sessions can attach a compatible ring
    /// through the offscreen submit path when the device supports the required external handles.
    ///
    /// `release_tx` is the consumer→producer slot-release back-edge: pass the
    /// sender half of [`vse_external_frame::release_channel`] and give the
    /// receiver to the producer.
    pub fn attach_external_frame_source(
        &mut self,
        desc: vse_external_frame::ExternalRingDesc,
        release_tx: vse_external_frame::SlotReleaseTx,
    ) -> Result<(), VSEError> {
        self.attach_external_frame_source_with_policy(
            desc,
            release_tx,
            crate::core::external_frame::ExternalFramePolicy::default(),
        )
    }

    /// Attach an external-renderer frame source with an explicit consumption
    /// policy. Use [`ExternalFramePolicy::LatestReadyHoldLast`](crate::core::ExternalFramePolicy::LatestReadyHoldLast) when VSE should
    /// repeat the last displayed external image instead of dropping to a clear
    /// underlay on frames where no new producer frame has been queued.
    pub fn attach_external_frame_source_with_policy(
        &mut self,
        desc: vse_external_frame::ExternalRingDesc,
        release_tx: vse_external_frame::SlotReleaseTx,
        policy: crate::core::external_frame::ExternalFramePolicy,
    ) -> Result<(), VSEError> {
        use crate::core::external_frame::{ExternalFrameError, ExternalFrameRing};
        if let Some(present) = self.state.target.present() {
            if present.present_engine.is_none() {
                return Err(ExternalFrameError::Unsupported(
                    "displayed external frame sources require the ExtPresentTiming backend \
                     (CPU-estimate timing path active)"
                        .into(),
                )
                .into());
            }
        }
        let occupied = match &self.state.target {
            RenderTarget::Present(present) => present.external_source.is_some(),
            RenderTarget::Offscreen(offscreen) => offscreen.external_source.is_some(),
        };
        if occupied {
            return Err(ExternalFrameError::Unsupported(
                "an external frame source is already attached".into(),
            )
            .into());
        }
        let ring = ExternalFrameRing::import_with_policy(
            &self.state.device,
            &self.state.queue,
            desc,
            release_tx,
            policy,
        )?;
        tracing::info!(
            "external frame source attached: {} slots, {:?}, {:?}, {:?}, {}x{}",
            ring.ring_len(),
            ring.format(),
            ring.sync(),
            policy,
            ring.extent()[0],
            ring.extent()[1],
        );
        match &mut self.state.target {
            RenderTarget::Present(present) => present.external_source = Some(ring),
            RenderTarget::Offscreen(offscreen) => offscreen.external_source = Some(Box::new(ring)),
        }
        Ok(())
    }

    /// Queue the external frame in `slot` for consumption by the next flip.
    ///
    /// Call after the producer finishes rendering into `slot` (and signals its
    /// ready semaphore), before `flip()` or returning a [`BufferedFrame`](crate::core::BufferedFrame).
    /// Slots must be
    /// queued in the order the producer acquired them.
    pub fn queue_external_frame(
        &mut self,
        slot: vse_external_frame::SlotIndex,
    ) -> Result<(), VSEError> {
        self.queue_external_frame_with_timeline_value(slot, None)
    }

    /// Queue an external frame and, for timeline-synchronized sources, the
    /// timeline semaphore value signaled by the producer for that frame. This
    /// method also accepts `None` for binary-per-slot and CPU-blocking sources,
    /// so callers can pass `ReadyFrame::timeline_value` without branching.
    pub fn queue_external_frame_with_timeline_value(
        &mut self,
        slot: vse_external_frame::SlotIndex,
        timeline_value: Option<u64>,
    ) -> Result<(), VSEError> {
        let src = match &mut self.state.target {
            RenderTarget::Present(present) => present.external_source.as_mut(),
            RenderTarget::Offscreen(offscreen) => offscreen.external_source.as_deref_mut(),
        }
        .ok_or_else(|| {
            crate::core::external_frame::ExternalFrameError::Unsupported(
                "no external frame source attached".into(),
            )
        })?;
        src.note_ready_with_value(slot, timeline_value)?;
        Ok(())
    }

    /// Arm a one-shot readback of the next consumed external frame into
    /// `buffer` (determinism-harness hook). The copy is recorded in the same
    /// command buffer as the underlay consumption; the buffer is safe to read
    /// once that flip is confirmed (fence signaled / `Presented` delivered).
    /// A no-op in a headless session, whose final offscreen readback already
    /// captures the consumed external frame and all VSE overlays.
    pub fn arm_external_readback(&mut self, buffer: vulkano::buffer::Subbuffer<[u8]>) {
        if let Some(present) = self.state.target.present_mut() {
            present.external_readback = Some(buffer);
        }
    }
}
