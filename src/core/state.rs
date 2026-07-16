//! Internal runtime state and timing helpers for VSEContext.

use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};
use winit::{
    event::{MouseScrollDelta, WindowEvent},
    keyboard::PhysicalKey,
    window::Window,
};

use super::config::VSEConfig;
use super::device::DeviceSelector;
use super::frame::FrameBuilder;
use super::input::{AcquisitionMethod, InputState, WindowMode};
use super::present_engine::{PresentEngine, ScheduledTarget};
use super::present_timing_ext::ScanoutFeedback;
use super::swapchain::{SwapchainError, SwapchainManager};
use crate::data::messages::FrameMessage;
use crate::data::ExperimentSession;
use crate::drawing::renderer::Renderer;
use crate::timing::{
    Clock, CpuTimingProvider, ExtPresentTimingProvider, FlipInfo, HostClockBridge, ScanoutClock,
    Timestamp, TimingProvider, TimingSource,
};

/// Source of input events for the current session.
pub(super) enum InputSource {
    /// Events from winit (compositor mode).
    Winit,
    /// Events from evdev (direct display mode, Linux only).
    #[cfg(target_os = "linux")]
    Evdev(crate::core::evdev_input::EvdevReader),
}

/// Tracks per-frame recording state between flip() and record_frame() calls.
pub(super) struct RecordingState {
    pub(super) session: ExperimentSession,
    /// FlipInfo from the most recent flip(), available for record_frame().
    pub(super) pending_flip: Option<FlipInfo>,
    /// frame_number of the most recently claimed flip (had record_frame called).
    pub(super) last_claimed_frame: Option<u64>,
}

impl RecordingState {
    /// Called by flip() after FlipInfo is computed.
    /// Sends timing-only row for the previous unclaimed flip, then caches new flip.
    pub(crate) fn on_flip(&mut self, new_flip: FlipInfo) {
        if let Some(prev) = self.pending_flip.take() {
            let already_claimed = self.last_claimed_frame == Some(prev.frame_number);
            if !already_claimed {
                let _ = self.session.send_frame(FrameMessage {
                    flip: prev,
                    payload: None,
                    schema_name: "",
                });
            }
        }
        self.pending_flip = Some(new_flip);
    }

    /// Called on session shutdown — flushes final pending flip as timing-only if unclaimed.
    pub(crate) fn on_shutdown(&mut self) {
        if let Some(flip) = self.pending_flip.take() {
            let claimed = self.last_claimed_frame == Some(flip.frame_number);
            if !claimed {
                let _ = self.session.send_frame(FrameMessage {
                    flip,
                    payload: None,
                    schema_name: "",
                });
            }
        }
    }
}

/// Internal state that requires an active window
pub(super) struct VSEState {
    pub(super) window: Option<Arc<Window>>, // None in DirectDisplay mode
    pub(super) device_selector: DeviceSelector,
    pub(super) device: Arc<vulkano::device::Device>,
    pub(super) queue: Arc<vulkano::device::Queue>,
    pub(super) swapchain: SwapchainManager,
    #[allow(dead_code)]
    pub(super) frame_builder: FrameBuilder,
    pub(super) renderer: Renderer,
    pub(super) should_close: bool,
    pub(super) minimized: bool,
    pub(super) input: InputState,
    pub(super) cursor_visible: bool,
    pub(super) window_mode: WindowMode,
    // Timing state
    pub(super) clock: Clock,
    pub(super) timing_provider: Box<dyn TimingProvider>,
    /// Raw acquire/submit/present engine for the EXT present-timing path. `None` on the
    /// CPU-estimate path (which keeps using vulkano's present).
    pub(super) present_engine: Option<PresentEngine>,
    /// Scanout-timing records drained from the driver on the most recent flip.
    /// `vkGetPastPresentationTimingEXT` dequeues records, so they are read exactly once per frame
    /// here and cached for `scanout_feedback()` to return without re-draining. Empty on the
    /// CPU-estimate path.
    pub(super) recent_scanouts: Vec<ScanoutFeedback>,
    /// Confirmed scanout records accumulated across flips, keyed by `present_id`. Feedback for a
    /// present arrives a frame or two after submission, so the buffered path stores records here
    /// and looks them up by the confirming frame's `present_id` (see `build_confirmed_flip`).
    /// Pruned on lookup to stay bounded (present ids are monotonic).
    pub(super) scanout_by_present_id: std::collections::HashMap<u64, ScanoutFeedback>,
    pub(super) frame_number: u64,
    pub(super) last_present_time: Option<Timestamp>,
    /// `IMAGE_FIRST_PIXEL_OUT` scanout time (present-stage-local ns) of the last frame confirmed
    /// with hardware feedback, for computing scanout-delta missed detection on the buffered path.
    /// `None` until the first confirmed scanout record arrives.
    pub(super) last_scanout_ns: Option<u64>,
    /// `present_id` paired with [`last_scanout_ns`]. A scanout delta is only trusted between
    /// *consecutive* present ids (`this == last + 1`); otherwise a lagged or dropped feedback
    /// record would inflate the delta into a false miss, so we fall back to the CPU delta.
    pub(super) last_scanout_present_id: Option<u64>,
    pub(super) expected_frame_duration: Option<Duration>,
    pub(super) refresh_detect_samples: Vec<Duration>,
    /// Scanout-clock epoch (present-stage-local `t=0`), established on the first flip under the
    /// EXT backend. `None` on the CPU-estimate path (no scanout clock available).
    pub(super) scanout_clock: Option<ScanoutClock>,
    // --- Driver-conformance observation (advertised present-timing features may be unimplemented) ---
    /// Whether the driver actually fills `IMAGE_FIRST_PIXEL_OUT` in feedback: `Some(true)` once a
    /// non-zero value is seen; `Some(false)` after enough feedback records arrive all-zero; `None`
    /// until determined. Recorded into `HostInfo` and drives a one-time guardrail warning.
    pub(super) scanout_feedback_populated: Option<bool>,
    /// Count of feedback records seen while `scanout_feedback_populated` is still undetermined.
    pub(super) scanout_feedback_probe_count: u32,
    /// One-time-warning latches for driver-conformance guardrails (feedback stubbed; scheduling
    /// software-paced), so the warnings fire once per session rather than every frame.
    pub(super) warned_feedback_stub: bool,
    pub(super) warned_sw_pacing: bool,
    /// Opt-in host↔scanout bridge (see `with_host_clock_bridge`). `None` unless requested and
    /// the EXT backend is active.
    pub(super) host_bridge: Option<HostClockBridge>,
    /// VSE-clock time of the last bridge sample, for rate-limiting sampling off the hot path.
    pub(super) last_bridge_sample_ts: Option<Timestamp>,
    pub(super) input_source: InputSource,
    /// Physical display dimensions (from window or VkDisplaySurfaceKHR).
    pub(super) display_size: (u32, u32),
    /// Which acquisition method succeeded, if in DirectDisplay mode.
    pub(super) acquired_display: Option<AcquisitionMethod>,
    /// Optional data recording session.
    pub(super) recording: Option<RecordingState>,

    // --- Buffered flip state (None/false when using synchronous run()) ---
    /// Transit slot: flip_with_payload() stores the payload here as a type-erased
    /// Box<dyn Any>. run_buffered() takes it out after the Render callback returns
    /// and downcasts it back to T. Always None outside the Render callback.
    pub(super) buffered_pending_payload: Option<Box<dyn std::any::Any + Send + 'static>>,

    /// The confirmed FlipInfo for the frame being delivered in a Presented callback.
    /// Set by run_buffered() before invoking the Presented arm; cleared after.
    /// record_frame() reads this field instead of pending_flip when Some.
    pub(super) buffered_confirmed_flip: Option<FlipInfo>,

    /// True while run_buffered() is executing. Guards flip_with_payload() and
    /// prevents flip() from being called in that context.
    pub(super) in_buffered_mode: bool,

    /// In-flight fences paired with estimated FlipInfo. Populated by flip_with_payload(),
    /// drained by run_buffered() when GPU confirmation arrives.
    /// VecDeque because we always drain from the front (FIFO confirmation order).
    pub(super) buffered_in_flight:
        std::collections::VecDeque<(FlipInfo, Box<dyn crate::core::buffered::InFlightFuture>)>,

    /// Tracks whether record_frame() was called during the current Presented callback.
    /// Reset to false before each Presented callback by run_buffered().
    pub(super) buffered_record_called_this_presented: bool,

    /// Present-timing sub-features enabled at device creation (`Some` on the EXT backend).
    /// Carries the queue global-priority outcome into host-info snapshots.
    pub(super) ext_features: Option<crate::core::present_timing_ext::EnabledPresentTimingFeatures>,

    /// Imported external-renderer frame ring (see `core::external_frame`).
    /// `None` unless a source was attached via `attach_external_frame_source`.
    pub(super) external_source: Option<crate::core::external_frame::ExternalFrameRing>,

    /// One-shot readback buffer for the next consumed external frame
    /// (determinism-harness hook, armed via `arm_external_readback`).
    pub(super) external_readback: Option<vulkano::buffer::Subbuffer<[u8]>>,
}

/// Select and construct the timing backend for a freshly created device + swapchain.
///
/// When the device was created with `VK_EXT_present_timing` (`ext_features` is `Some`), use
/// the hardware backend; otherwise (or if its function pointers fail to load) fall back
/// loudly to CPU estimation.
pub(super) fn build_timing_provider(
    device: &std::sync::Arc<vulkano::device::Device>,
    swapchain: &std::sync::Arc<vulkano::swapchain::Swapchain>,
    ext_features: Option<crate::core::present_timing_ext::EnabledPresentTimingFeatures>,
) -> Box<dyn TimingProvider> {
    if let Some(enabled) = ext_features {
        match unsafe { ExtPresentTimingProvider::new(device, swapchain, enabled) } {
            Some(p) => return Box::new(p),
            None => {
                warn!("VK_EXT_present_timing function pointers unavailable; using CPU estimation");
            }
        }
    }
    Box::new(CpuTimingProvider::new())
}

/// Build the opt-in host↔scanout bridge, if requested and supported.
///
/// The bridge needs the `VK_EXT_present_timing` backend's calibrated-timestamp sampler; on the
/// CPU-estimate path it cannot function, so the request is refused loudly rather than silently
/// producing a dead bridge.
pub(super) fn build_host_bridge(
    config: &VSEConfig,
    provider: &dyn TimingProvider,
) -> Option<HostClockBridge> {
    if !config.host_clock_bridge {
        return None;
    }
    if provider.source() == TimingSource::ExtPresentTiming {
        // 2 s window: measured offset stability plateaus by ~1-2 s (docs/clock-synchronization.md).
        Some(HostClockBridge::new(Duration::from_secs(2)))
    } else {
        warn!("host_clock_bridge requested but EXT present-timing backend is unavailable; bridge disabled");
        None
    }
}

/// Build the raw present engine for the EXT present-timing path.
///
/// Returns `None` on the CPU-estimate path (which keeps using vulkano's present) and, loudly,
/// if the EXT backend is active but the engine's function pointers / sync objects cannot be
/// created — in which case `flip()` degrades to the vulkano present with no present-id or
/// scanout feedback.
pub(super) fn build_present_engine(
    device: &Arc<vulkano::device::Device>,
    image_count: u32,
    provider: &dyn TimingProvider,
) -> Option<PresentEngine> {
    if provider.source() != TimingSource::ExtPresentTiming {
        return None;
    }
    match PresentEngine::new(device, image_count) {
        Some(e) => Some(e),
        None => {
            warn!(
                "EXT present-timing backend active but PresentEngine unavailable; \
                 falling back to vulkano present (no present-id / scanout feedback)"
            );
            None
        }
    }
}

pub(super) fn missed_frame_status(
    frame_duration: Option<Duration>,
    expected: Duration,
) -> (bool, u32) {
    match frame_duration {
        Some(dur) => {
            let ratio = dur.as_micros() as f64 / expected.as_micros() as f64;
            if ratio > 1.5 {
                (true, (ratio.round() as u32).saturating_sub(1))
            } else {
                (false, 0)
            }
        }
        None => (false, 0),
    }
}

fn update_refresh_auto_detection(
    expected_frame_duration: &mut Option<Duration>,
    refresh_detect_samples: &mut Vec<Duration>,
    provider_duration: Option<Duration>,
    frame_duration: Option<Duration>,
) -> Option<Duration> {
    if expected_frame_duration.is_some() {
        return None;
    }

    if let Some(dur) = provider_duration {
        *expected_frame_duration = Some(dur);
        return Some(dur);
    }

    if let Some(dur) = frame_duration {
        refresh_detect_samples.push(dur);
        if refresh_detect_samples.len() >= 10 {
            let total: Duration = refresh_detect_samples.iter().copied().sum();
            let avg = total / refresh_detect_samples.len() as u32;
            *expected_frame_duration = Some(avg);
            return Some(avg);
        }
    }

    None
}

impl VSEState {
    /// Recreate the swapchain from the current surface and notify the timing provider so it
    /// refreshes any cached swapchain handle (a retired handle is UB to query).
    pub(super) fn recreate_swapchain(&mut self, win_size: [u32; 2]) -> Result<(), SwapchainError> {
        self.swapchain.recreate_from_surface(win_size)?;
        self.timing_provider
            .on_swapchain_recreated(self.swapchain.swapchain());
        Ok(())
    }

    /// Per-flip clock maintenance: establish the scanout epoch on the first flip and, when the
    /// host-clock bridge is enabled, feed it a low-rate calibration sample. One calibrated read
    /// serves both. A no-op on the CPU-estimate path (the provider's sampler returns `None`).
    pub(super) fn update_clocks(&mut self) {
        let need_epoch = self.scanout_clock.is_none();
        let bridge_due = self.host_bridge.is_some() && self.bridge_sample_due();
        if !need_epoch && !bridge_due {
            return;
        }
        if let Some(sample) = self.timing_provider.sample_present_calibration() {
            if need_epoch {
                // First scanout reading is the session's scanout `t=0`.
                self.scanout_clock = Some(ScanoutClock::new(sample.stage_ns));
            }
            if bridge_due {
                self.last_bridge_sample_ts = Some(self.clock.now());
                if let Some(bridge) = &mut self.host_bridge {
                    bridge.push(sample);
                }
            }
        }
    }

    /// Whether enough time has elapsed to take another bridge sample (~10 Hz), keeping the
    /// calibrated-timestamp read off the presentation hot path.
    pub(super) fn bridge_sample_due(&self) -> bool {
        const RESAMPLE_US: u64 = 100_000; // 100 ms
        let now = self.clock.now().as_micros();
        self.last_bridge_sample_ts
            .map_or(true, |t| now.saturating_sub(t.as_micros()) >= RESAMPLE_US)
    }

    /// Record confirmed scanout feedback drained this frame: index each record by its
    /// `present_id` for later buffered confirmation, and keep the raw list for
    /// `scanout_feedback()`. The read is destructive, so this is called exactly once per flip.
    pub(super) fn ingest_scanout_feedback(&mut self, feedback: Vec<ScanoutFeedback>) {
        for fb in &feedback {
            self.scanout_by_present_id.insert(fb.present_id, *fb);
        }
        self.observe_feedback_conformance(&feedback);
        self.recent_scanouts = feedback;
    }

    /// Passively observe whether the driver actually populates `IMAGE_FIRST_PIXEL_OUT` in
    /// present-timing feedback (some drivers advertise `VK_EXT_present_timing` but return
    /// zero-valued stage timestamps). Latches `scanout_feedback_populated` and, on the first
    /// determination that it is stubbed, emits a one-time guardrail warning naming the workaround
    /// VSE is using. Cheap and automatic — no extra presents.
    pub(super) fn observe_feedback_conformance(&mut self, feedback: &[ScanoutFeedback]) {
        if self.scanout_feedback_populated == Some(true) {
            return;
        }
        if feedback
            .iter()
            .any(|f| f.first_pixel_out_ns.is_some_and(|v| v != 0))
        {
            self.scanout_feedback_populated = Some(true);
            return;
        }
        // All-zero so far. Once enough records have arrived all-zero, conclude the driver stubs the
        // stage timestamps (16 ≈ the driver's timing-ring depth — a full turnover with no real value).
        self.scanout_feedback_probe_count = self
            .scanout_feedback_probe_count
            .saturating_add(feedback.len() as u32);
        if self.scanout_feedback_populated.is_none() && self.scanout_feedback_probe_count >= 16 {
            self.scanout_feedback_populated = Some(false);
            if !self.warned_feedback_stub {
                self.warned_feedback_stub = true;
                warn!(
                    "VK_EXT_present_timing: the driver returns present-timing feedback that \
                     correlates by present_id but stubs the scanout stage timestamps \
                     (IMAGE_FIRST_PIXEL_OUT = 0) — advertised but not implemented (seen on \
                     Intel/ANV/Mesa 26.1). present_time is derived from the calibrated \
                     PRESENT_STAGE_LOCAL clock instead; scanout timing stays valid. See \
                     docs/clock-synchronization.md."
                );
            }
        }
    }

    /// Take the confirmed scanout record for `present_id`, pruning it and every older entry
    /// (present ids are monotonic, so records ≤ `present_id` are past and never looked up again).
    /// Keeps `scanout_by_present_id` bounded to the in-flight + feedback-lag window.
    pub(super) fn take_scanout_for(&mut self, present_id: u64) -> Option<ScanoutFeedback> {
        let found = self.scanout_by_present_id.remove(&present_id);
        self.scanout_by_present_id.retain(|&id, _| id > present_id);
        found
    }

    /// Rebase a driver `IMAGE_FIRST_PIXEL_OUT` scanout time (present-stage-local ns) into a
    /// scanout-domain [`Timestamp`] (µs since the session's scanout `t=0`). This is the value
    /// stored in `FlipInfo.present_time` under `ExtPresentTiming` — a hardware scanout time, not a
    /// host-clock time (see the clock model in `docs/clock-synchronization.md`). `None` before the
    /// scanout epoch is established, or when the driver reports no real scanout time.
    ///
    /// A real `IMAGE_FIRST_PIXEL_OUT` is an absolute present-stage-local value (~10¹³ ns), never
    /// zero. Windowed compositors that cannot observe true scanout report the stage with time `0`
    /// (seen on windowed Wayland / ANV); that is *not* a scanout time, so it yields `None` and the
    /// caller falls back to CPU fence time. Real values appear on the direct-display path.
    pub(super) fn scanout_present_time(&self, first_pixel_out_ns: u64) -> Option<Timestamp> {
        if first_pixel_out_ns == 0 {
            return None;
        }
        let clock = self.scanout_clock?;
        Some(Timestamp::from_micros(
            clock.rebase(first_pixel_out_ns).as_micros(),
        ))
    }

    /// Sample the present-stage-local scanout clock **now** and rebase it to a scanout-domain
    /// [`Timestamp`] (µs since `t=0`). `None` before the scanout epoch is established or on the
    /// CPU backend.
    ///
    /// Used by the synchronous `flip()` immediately after `wait_for_present` returns — i.e. right
    /// at this frame's scanout — to obtain a real scanout `present_time`. This is the fallback for
    /// the (measured) case where the driver stubs `vkGetPastPresentationTimingEXT`'s stage times
    /// to zero (Intel/ANV/Mesa 26.1): the feedback still correlates by `present_id` but carries no
    /// timestamp, whereas the calibrated `PRESENT_STAGE_LOCAL` clock reports real, vblank-cadence
    /// values. The sampled value is scanout-begin plus the calibrated-read latency (tens of µs —
    /// far below the display-panel latency floor that a photodiode measures anyway).
    pub(super) fn sample_scanout_now(&self) -> Option<Timestamp> {
        let clock = self.scanout_clock?;
        let sample = self.timing_provider.sample_present_calibration()?;
        Some(Timestamp::from_micros(
            clock.rebase(sample.stage_ns).as_micros(),
        ))
    }

    /// Convert a scheduling target (`target`, scanout-domain µs since `t=0`) into an absolute
    /// [`ScheduledTarget`] the driver can schedule against: absolute present-stage-local ns in the
    /// swapchain's `PRESENT_STAGE_LOCAL` domain. `None` until the scanout epoch and domain id are
    /// known (the first flip, before `t=0` is established) — the caller then presents unscheduled.
    pub(super) fn scheduled_target(&self, target: Timestamp) -> Option<ScheduledTarget> {
        let epoch = self.scanout_clock?.epoch_stage_ns();
        let time_domain_id = self.timing_provider.present_stage_domain_id()?;
        let target_time_ns = epoch.saturating_add(target.as_micros().saturating_mul(1_000));
        Some(ScheduledTarget {
            target_time_ns,
            time_domain_id,
        })
    }

    /// The display's refresh interval, if known (driver-reported, else the auto-detected estimate).
    pub(super) fn refresh_interval(&self) -> Option<Duration> {
        self.timing_provider
            .refresh_cycle_duration()
            .or(self.expected_frame_duration)
    }

    pub(super) fn update_refresh_detection(
        &mut self,
        frame_duration: Option<Duration>,
        log_provider: bool,
        log_average: bool,
    ) {
        let provider_duration = self.timing_provider.refresh_cycle_duration();
        let detected = update_refresh_auto_detection(
            &mut self.expected_frame_duration,
            &mut self.refresh_detect_samples,
            provider_duration,
            frame_duration,
        );

        if let Some(dur) = detected {
            if provider_duration.is_some() && log_provider {
                info!(
                    "Refresh cycle duration from provider: {} us ({:.1} Hz)",
                    dur.as_micros(),
                    1_000_000.0 / dur.as_micros() as f64
                );
            } else if provider_duration.is_none() && log_average {
                info!(
                    "Auto-detected refresh rate: {:.1} Hz (frame duration: {} us)",
                    1_000_000.0 / dur.as_micros() as f64,
                    dur.as_micros()
                );
            }
        }
    }

    pub(super) fn handle_winit_input(&mut self, event: &WindowEvent) {
        match event {
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(key_code) = event.physical_key {
                    self.input.handle_key(
                        key_code,
                        event.logical_key.clone(),
                        event.state,
                        self.clock.now(),
                    );
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.input
                    .handle_cursor_moved(position.x, position.y, self.clock.now());
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.input
                    .handle_mouse_button((*button).into(), *state, self.clock.now());
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (*x as f64, *y as f64),
                    MouseScrollDelta::PixelDelta(pos) => (pos.x, pos.y),
                };
                self.input.handle_mouse_wheel(dx, dy, self.clock.now());
            }
            _ => {}
        }
    }

    /// One-time guardrail note (per session) that scheduled presents are being software-paced,
    /// since hardware `targetTime` enforcement is driver-dependent and unverified at runtime.
    pub(super) fn note_scheduling_once(&mut self) {
        if !self.warned_sw_pacing {
            self.warned_sw_pacing = true;
            info!(
                "Scheduled present requested: VSE paces it against the scanout clock (software). \
                 Hardware targetTime enforcement is driver-dependent and NOT verified here — it is \
                 ignored on Intel/ANV/Mesa 26.1. Characterize your driver with \
                 `examples/13_direct_display_scanout` (reports absolute_scheduling_enforced)."
            );
        }
    }

    /// Software scanout-domain pacing for a scheduled flip (sync path only).
    ///
    /// Many drivers advertise `presentAtAbsoluteTime` but do **not** enforce
    /// `VkPresentTimingInfoEXT.targetTime` (measured: Intel/ANV/Mesa 26.1 ignores it), so VSE paces
    /// the present itself. FIFO quantizes every present to a vblank, so we need only issue the
    /// present within the target vblank's preceding refresh interval — sleep on the scanout clock
    /// until then. The wait is a scanout-domain *duration* (≈ real time to within the ~2 ppm
    /// scanout↔CPU drift), so no absolute cross-clock math enters the loop. Harmless on drivers that
    /// *do* honor `targetTime` — they would present at the same vblank; we just avoid submitting
    /// early. No-op when the scanout clock isn't established yet, or the target is already imminent.
    pub(super) fn pace_to_scanout_target(&self, target: Timestamp, refresh: Duration) {
        let Some(now) = self.sample_scanout_now() else {
            return;
        };
        // Aim to submit ~half a refresh before the target vblank: FIFO then shows it at that vblank,
        // robust to ±½-refresh sleep jitter.
        let present_at_us = target
            .as_micros()
            .saturating_sub(refresh.as_micros() as u64 / 2);
        let now_us = now.as_micros();
        if present_at_us > now_us {
            std::thread::sleep(Duration::from_micros(present_at_us - now_us));
        }
    }

    /// Build a confirmed `FlipInfo` from an estimated one captured at submit time.
    ///
    /// On the EXT backend, keys on the frame's `present_id`: if this frame's
    /// `IMAGE_FIRST_PIXEL_OUT` record has arrived (it was presented `depth+1` frames ago, so it
    /// normally has), `present_time` is the real hardware scanout time (rebased to the scanout
    /// epoch) and missed detection uses the scanout delta; otherwise it falls back to CPU fence
    /// time. `timing_source` records which domain the value is in.
    pub(super) fn build_confirmed_flip(&mut self, estimated: FlipInfo) -> FlipInfo {
        // Look up this frame's confirmed scanout record by present_id, pruning consumed/past
        // entries (present ids are monotonic, so anything ≤ this id is no longer needed).
        let scanout = self.take_scanout_for(estimated.present_id);

        // This frame's real scanout-begin time, if the driver reported one. Zero means "no real
        // scanout time" — filtered here so both present_time and missed detection below fall back
        // to the CPU clock rather than treating 0 as a timestamp.
        //
        // Note: unlike the synchronous `flip()` path — which blocks on `wait_for_present` and can
        // therefore sample the calibrated scanout clock at the frame's scanout — the buffered path
        // is pipelined and never blocks per frame, so it can only use the driver's per-present
        // feedback. Where that feedback is stubbed to 0 (Intel/ANV/Mesa 26.1), buffered
        // `present_time` falls back to CPU time; it becomes scanout-native automatically on drivers
        // that populate `IMAGE_FIRST_PIXEL_OUT`.
        let scanout_ns = scanout
            .and_then(|fb| fb.first_pixel_out_ns)
            .filter(|&ns| ns != 0);

        // present_time is the hardware scanout time for this frame when available, else CPU fence
        // time. `timing_source` (already ExtPresentTiming) plus a real scanout record is the domain
        // guard; there is no separate field (see the B3 schema decision).
        let confirmed_present = scanout_ns
            .and_then(|ns| self.scanout_present_time(ns))
            .unwrap_or_else(|| self.timing_provider.record_present_time(&self.clock));

        let cpu_frame_duration = self
            .last_present_time
            .map(|prev| confirmed_present.duration_since(prev));

        let expected = self
            .expected_frame_duration
            .unwrap_or(Duration::from_micros(16_667));

        // Auto-detect refresh rate using the same logic as flip().
        self.update_refresh_detection(cpu_frame_duration, false, false);

        // Missed detection prefers the real scanout delta (present-stage-local ns), but only
        // between *consecutive* present ids where both records are present — feedback for a
        // present lags its submission (at depth 1 it usually arrives after this frame's
        // confirmation), so a non-consecutive delta would span a gap and false-flag a miss.
        // Otherwise fall back to the CPU present-time delta.
        let frame_duration = match scanout_ns {
            Some(scanout_ns) => {
                let consecutive =
                    self.last_scanout_present_id == Some(estimated.present_id.wrapping_sub(1));
                let dur = match (self.last_scanout_ns, consecutive) {
                    (Some(prev), true) => {
                        Some(Duration::from_nanos(scanout_ns.saturating_sub(prev)))
                    }
                    _ => cpu_frame_duration,
                };
                self.last_scanout_ns = Some(scanout_ns);
                self.last_scanout_present_id = Some(estimated.present_id);
                dur
            }
            None => cpu_frame_duration,
        };

        let (missed, missed_count) = missed_frame_status(frame_duration, expected);

        // on_target: with a real scanout time (same domain as the target) the scheduled present
        // met its deadline iff scanout landed at/after target. Without one (windowed), keep the
        // estimate rather than claim a verification we cannot make.
        let on_target = match (estimated.target_time, scanout_ns) {
            (Some(target), Some(_)) => confirmed_present.as_micros() >= target.as_micros(),
            _ => estimated.on_target,
        };

        let flip = FlipInfo {
            frame_number: estimated.frame_number,
            timing_source: self.timing_provider.source(),
            submit_time: estimated.submit_time,
            present_time: confirmed_present,
            present_id: estimated.present_id,
            target_time: estimated.target_time,
            on_target,
            missed,
            missed_count,
            skipped: false,
        };

        self.last_present_time = Some(confirmed_present);

        flip
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context::VSEContext;
    use crate::core::VSEError;

    #[test]
    fn missed_frame_status_uses_strict_one_point_five_threshold() {
        let expected = Duration::from_micros(10_000);
        assert_eq!(missed_frame_status(None, expected), (false, 0));
        assert_eq!(
            missed_frame_status(Some(Duration::from_micros(15_000)), expected),
            (false, 0)
        );
        assert_eq!(
            missed_frame_status(Some(Duration::from_micros(15_001)), expected),
            (true, 1)
        );
        assert_eq!(
            missed_frame_status(Some(Duration::from_micros(35_000)), expected),
            (true, 3)
        );
    }

    #[test]
    fn refresh_auto_detection_prefers_provider_duration() {
        let mut samples = Vec::new();
        let mut expected = None;

        let detected = update_refresh_auto_detection(
            &mut expected,
            &mut samples,
            Some(Duration::from_micros(8_333)),
            Some(Duration::from_micros(16_667)),
        );

        assert_eq!(detected, Some(Duration::from_micros(8_333)));
        assert_eq!(expected, Some(Duration::from_micros(8_333)));
        assert!(samples.is_empty());
    }

    #[test]
    fn refresh_auto_detection_averages_ten_frame_samples() {
        let mut samples = Vec::new();
        let mut expected = None;

        for _ in 0..9 {
            assert_eq!(
                update_refresh_auto_detection(
                    &mut expected,
                    &mut samples,
                    None,
                    Some(Duration::from_micros(16_000)),
                ),
                None
            );
            assert!(expected.is_none());
        }

        let detected = update_refresh_auto_detection(
            &mut expected,
            &mut samples,
            None,
            Some(Duration::from_micros(18_000)),
        );

        assert_eq!(detected, Some(Duration::from_micros(16_200)));
        assert_eq!(expected, Some(Duration::from_micros(16_200)));
    }

    #[test]
    fn test_recording_state_pending_flip_handoff() {
        use crate::data::{CsvDataWriter, ExperimentSession};
        use crate::timing::{FlipInfo, Timestamp, TimingSource};

        let dir = std::env::temp_dir().join("vse_pending_flip_test");
        let _ = std::fs::remove_dir_all(&dir);

        let session = ExperimentSession::builder()
            .with_writer(CsvDataWriter::new(&dir))
            .build()
            .unwrap();

        let mut state = RecordingState {
            session,
            pending_flip: None,
            last_claimed_frame: None,
        };

        let make_flip = |n: u64| FlipInfo {
            frame_number: n,
            timing_source: TimingSource::CpuEstimate,
            submit_time: Timestamp::from_micros(0),
            present_time: Timestamp::from_micros(16_667),
            present_id: 0,
            target_time: None,
            on_target: true,
            missed: false,
            missed_count: 0,
            skipped: false,
        };

        // Simulate flip(0) — no record_frame called
        state.on_flip(make_flip(0));
        // Simulate flip(1) — flip(0) was unclaimed, timing-only row sent
        state.on_flip(make_flip(1));

        // Drop sends Shutdown + flush
        drop(state.session);

        std::thread::sleep(std::time::Duration::from_millis(50));

        let frames = std::fs::read_to_string(dir.join("frames.csv")).unwrap();
        let lines: Vec<&str> = frames.lines().collect();
        // header + 1 timing-only row for frame 0
        assert!(
            lines.len() >= 2,
            "expected at least header + 1 row, got: {:?}",
            lines
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_record_frame_without_flip_returns_error() {
        let err = VSEError::NoFlipPending;
        assert!(err.to_string().contains("flip"));
    }

    #[test]
    #[ignore] // EventLoop::new() panics off main thread on Linux
    fn test_builder_with_session_compiles() {
        use crate::data::{CsvDataWriter, ExperimentSession};
        let session = ExperimentSession::builder()
            .with_writer(CsvDataWriter::new("/tmp/test_session"))
            .build()
            .unwrap();
        let _builder = VSEContext::builder()
            .with_window_size(800, 600)
            .with_session(session);
        // Just verifies it compiles — ignored at runtime
    }

    #[test]
    fn direct_display_unavailable_error_contains_tried_methods() {
        let msg = "Tried:\n  \u{2717} No-compositor: held by compositor\n  \u{2717} DRM acquire: permission denied";
        let err = VSEError::DirectDisplayUnavailable(msg.to_string());
        let display = format!("{}", err);
        assert!(display.contains("Tried:"));
        assert!(display.contains("Direct display"));
    }

    #[test]
    fn vse_error_variants_display() {
        let e = VSEError::NoConfirmedFlip;
        assert!(e.to_string().contains("Presented"));

        let e = VSEError::NotInBufferedMode;
        assert!(e.to_string().contains("run_buffered"));

        let e = VSEError::NotSupportedInBufferedMode;
        assert!(e.to_string().contains("flip()"));
    }
}
