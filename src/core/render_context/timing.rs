use super::*;

impl<'a> RenderContext<'a> {
    /// Total Tier 1 draws skipped because their registered pipeline had been
    /// removed before the frame was recorded. `0` for a healthy session.
    pub fn skipped_draw_count(&self) -> u64 {
        self.state.renderer.skipped_draw_total()
    }

    /// Display refresh interval reported by the timing backend or detected from flips.
    pub fn refresh_interval(&self) -> Option<std::time::Duration> {
        match &self.state.target {
            RenderTarget::Present(p) => p.refresh_interval(),
            // Headless: the nominal interval used to synthesize flip times.
            RenderTarget::Offscreen(o) => Some(o.frame_interval),
        }
    }

    /// Sample the display's `PRESENT_STAGE_LOCAL` scanout clock against `CLOCK_MONOTONIC`.
    ///
    /// Returns `None` on the CPU-estimate path or before the present-stage time domain has been
    /// probed. Used to characterize the clock offset and relative drift that the present-timing
    /// calibration must correct. See `docs/clock-synchronization.md`.
    pub fn sample_present_calibration(&self) -> Option<crate::timing::CalibrationSample> {
        self.state
            .target
            .present()?
            .timing_provider
            .sample_present_calibration()
    }

    /// Read back confirmed per-present scanout timings from the driver's past-timing ring
    /// (`vkGetPastPresentationTimingEXT`).
    ///
    /// Each [`ScanoutFeedback`](crate::core::ScanoutFeedback) carries the correlating `present_id`
    /// (matching [`FlipInfo::present_id`](crate::timing::FlipInfo::present_id)) and the
    /// `IMAGE_FIRST_PIXEL_OUT` scanout time in the
    /// driver's present-stage-local domain. Empty on the CPU-estimate path, and for a frame or two
    /// after a present while the driver has not yet recorded it. Rebasing these to a
    /// [`ScanoutTimestamp`](crate::timing::ScanoutTimestamp) is B3's job.
    ///
    /// Returns the records drained on the most recent `flip()`. The driver's read is *destructive*
    /// (each record is dequeued once), so `flip()` drains once per frame and caches the result
    /// here — this accessor never re-drains, and calling it repeatedly returns the same records.
    pub fn scanout_feedback(&self) -> Vec<crate::core::ScanoutFeedback> {
        self.state
            .target
            .present()
            .map(|p| p.recent_scanouts.clone())
            .unwrap_or_default()
    }

    /// Read the current scanout-clock time — VSE's primary experimental clock.
    ///
    /// Returns time since the session's scanout epoch (`t=0`, established on the first flip).
    /// `None` on the CPU-estimate path, or before the first flip has established the epoch.
    pub fn scanout_now(&self) -> Option<ScanoutTimestamp> {
        let present = self.state.target.present()?;
        let clock = present.scanout_clock?;
        let sample = present.timing_provider.sample_present_calibration()?;
        Some(clock.rebase(sample.stage_ns))
    }

    /// Convert a host-clock [`Timestamp`] (e.g. a key-press or network-event time) into scanout
    /// time, using the opt-in host-clock bridge.
    ///
    /// Returns `None` unless the bridge is enabled
    /// ([`VSEContextBuilder::with_host_clock_bridge`](crate::core::VSEContextBuilder::with_host_clock_bridge)),
    /// warmed up, and the scanout epoch is established. This is the intended way to place
    /// host-originated events on the scanout timeline.
    pub fn host_to_scanout(&self, ts: Timestamp) -> Option<ScanoutTimestamp> {
        let present = self.state.target.present()?;
        let clock = present.scanout_clock?;
        let bridge = present.host_bridge.as_ref()?;
        let mono_ns = self.state.clock.to_monotonic_nanos(ts)?;
        let stage_ns = bridge.host_to_scanout_ns(mono_ns)?;
        Some(clock.rebase(stage_ns))
    }

    /// Convert a scanout timestamp back into a host-clock [`Timestamp`], using the opt-in bridge.
    ///
    /// Inverse of [`host_to_scanout`](Self::host_to_scanout); same availability conditions.
    pub fn scanout_to_host(&self, ts: ScanoutTimestamp) -> Option<Timestamp> {
        let present = self.state.target.present()?;
        let clock = present.scanout_clock?;
        let bridge = present.host_bridge.as_ref()?;
        let stage_ns = clock.epoch_stage_ns().saturating_add(ts.as_nanos());
        let mono_ns = bridge.scanout_to_host_ns(stage_ns)?;
        self.state.clock.from_monotonic_nanos(mono_ns)
    }

    /// The host-clock bridge's currently fitted relative drift, in ppm (diagnostic).
    ///
    /// `None` unless the bridge is enabled and warmed up.
    pub fn host_clock_bridge_drift_ppm(&self) -> Option<f64> {
        self.state
            .target
            .present()?
            .host_bridge
            .as_ref()?
            .drift_ppm()
    }
    /// Whether the driver was observed to actually populate `IMAGE_FIRST_PIXEL_OUT` in
    /// present-timing feedback, as opposed to merely advertising `VK_EXT_present_timing`.
    ///
    /// `Some(true)` means at least one nonzero per-present scanout timestamp was observed.
    /// `Some(false)` means a full probe window contained only missing or zero stage values; this
    /// does not identify whether the cause was configuration, display state, or driver behavior.
    /// `None` means the observation is incomplete or unavailable. See
    /// `docs/timing-conformance.md`.
    pub fn scanout_feedback_populated(&self) -> Option<bool> {
        self.state.target.present()?.scanout_feedback_populated
    }

    /// Whether this display path was observed to hold absolute `targetTime` requests.
    ///
    /// `None` unless a characterization run called
    /// [`record_absolute_scheduling_enforced`](Self::record_absolute_scheduling_enforced) — the
    /// measurement deliberately drops frames, so it is never auto-run.
    pub fn absolute_scheduling_enforced(&self) -> Option<bool> {
        self.state.target.present()?.absolute_scheduling_enforced
    }

    /// **Driver characterization only.** Disable VSE's software pacing of scheduled presents, so
    /// `VkPresentTimingInfoEXT.targetTime` is the only thing that could hold a present back.
    ///
    /// Experiments must not call this: with pacing off, synchronous scheduled flips depend on the
    /// presentation path's handling of the target. It
    /// exists so [`record_absolute_scheduling_enforced`](Self::record_absolute_scheduling_enforced)
    /// can measure the hardware rather than measuring VSE's own pacing loop.
    pub fn set_software_present_pacing(&mut self, enabled: bool) {
        if let Some(p) = self.state.target.present_mut() {
            p.software_present_pacing = enabled;
        }
    }

    /// Record the verdict of an absolute-scheduling characterization run so it lands in the
    /// session's [`HostInfo`](crate::host::HostInfo).
    ///
    /// Build the verdict with
    /// [`absolute_scheduling_verdict`](crate::core::present_timing_ext::absolute_scheduling_verdict)
    /// over trials collected with software pacing disabled.
    pub fn record_absolute_scheduling_enforced(&mut self, enforced: Option<bool>) {
        if let Some(p) = self.state.target.present_mut() {
            p.absolute_scheduling_enforced = enforced;
        }
    }
}
