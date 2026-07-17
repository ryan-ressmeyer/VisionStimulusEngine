//! Public rendering, drawing, input, recording, and query API.

use std::path::Path;
use std::sync::Arc;

use tracing::warn;
use winit::{dpi::LogicalPosition, window::Fullscreen};

use super::config::{VSEConfig, VSEError};
use super::input::{
    DisplayBackend, InputEvent, KeyCode, MonitorInfo, MouseButton, VideoModeInfo, WindowMode,
};
use super::state::VSEState;
use super::swapchain::SwapchainManager;
use crate::data::messages::FrameMessage;
use crate::drawing::primitives::{default_circle_segments, DrawCommand};
use crate::drawing::{Color, GaborParams, GratingParams, NoiseParams, TextureHandle};
use crate::timing::{Clock, ScanoutTimestamp, Timestamp, TimingSource};

/// Render context passed to the render callback
///
/// This provides access to rendering operations during the frame callback.
pub struct RenderContext<'a> {
    pub(super) state: &'a mut VSEState,
    pub(super) config: &'a mut VSEConfig,
}

impl<'a> RenderContext<'a> {
    /// Clear the screen with the configured clear color
    ///
    /// This records a clear command to the current frame's command buffer.
    /// The actual clear operation happens during [`flip()`](Self::flip).
    ///
    /// # Errors
    ///
    /// Returns `VSEError::Frame` if command buffer recording fails.
    pub fn clear(&mut self) -> Result<(), VSEError> {
        // Clear is handled as part of the frame in flip()
        Ok(())
    }
}

impl<'a> RenderContext<'a> {
    /// Request a clean exit at the end of the current frame.
    ///
    /// Alias for [`request_exit()`](Self::request_exit).
    pub fn close(&mut self) {
        self.state.should_close = true;
    }

    /// Record per-frame experimental data merged with the most recent flip's timing.
    ///
    /// In synchronous `run()`: call after `flip()`. Uses the confirmed present time
    /// from the blocking fence wait.
    ///
    /// In `run_buffered()`: call inside `FlipEvent::Presented`. Uses the confirmed
    /// hardware scanout timestamp delivered to that arm — never an estimate.
    ///
    /// The data struct must implement `serde::Serialize`. Multiple calls per frame
    /// are allowed — each produces one row keyed to the same `frame_number`.
    ///
    /// # Errors
    ///
    /// - [`VSEError::NoSession`] if no session was attached to the builder.
    /// - [`VSEError::NoFlipPending`] if called before `flip()` in synchronous mode.
    /// - [`VSEError::NoConfirmedFlip`] if called in the `FlipEvent::Render` arm.
    pub fn record_frame<F: serde::Serialize>(&mut self, data: F) -> Result<(), VSEError> {
        // Buffered mode: use the confirmed flip set by run_buffered() before this callback.
        if self.state.in_buffered_mode {
            let flip = self
                .state
                .buffered_confirmed_flip
                .clone()
                .ok_or(VSEError::NoConfirmedFlip)?;

            let recording = self.state.recording.as_mut().ok_or(VSEError::NoSession)?;

            let payload =
                serde_json::to_vec(&data).map_err(|e| VSEError::DataRecording(e.to_string()))?;

            recording
                .session
                .send_frame(FrameMessage {
                    flip,
                    payload: Some(payload),
                    schema_name: std::any::type_name::<F>(),
                })
                .map_err(|e| VSEError::DataRecording(e.to_string()))?;

            self.state.buffered_record_called_this_presented = true;
            return Ok(());
        }

        // Synchronous mode: use pending_flip from the most recent flip().
        let recording = self.state.recording.as_mut().ok_or(VSEError::NoSession)?;
        let flip = recording
            .pending_flip
            .clone()
            .ok_or(VSEError::NoFlipPending)?;

        recording.last_claimed_frame = Some(flip.frame_number);

        let payload =
            serde_json::to_vec(&data).map_err(|e| VSEError::DataRecording(e.to_string()))?;

        recording
            .session
            .send_frame(FrameMessage {
                flip,
                payload: Some(payload),
                schema_name: std::any::type_name::<F>(),
            })
            .map_err(|e| VSEError::DataRecording(e.to_string()))?;

        Ok(())
    }

    /// Record a typed annotation at the current timestamp.
    ///
    /// `stream` is the table/group name in the output file (e.g. `"trial"`,
    /// `"subject_info"`, `"calibration"`). Any `serde::Serialize` type is accepted.
    pub fn record_annotation<A: serde::Serialize>(
        &mut self,
        stream: &str,
        data: A,
    ) -> Result<(), VSEError> {
        let recording = self.state.recording.as_mut().ok_or(VSEError::NoSession)?;
        let timestamp = self.state.clock.now();
        let payload =
            serde_json::to_vec(&data).map_err(|e| VSEError::DataRecording(e.to_string()))?;
        recording
            .session
            .send_annotation(crate::data::messages::AnnotationMessage {
                stream: stream.to_string(),
                timestamp,
                payload,
            })
            .map_err(|e| VSEError::DataRecording(e.to_string()))?;
        Ok(())
    }

    /// Record a raw key-value event at the current timestamp.
    ///
    /// Use for unstructured or one-off data. For structured, repeated data
    /// prefer [`Self::record_frame`] or [`Self::record_annotation`].
    pub fn record_event(&mut self, name: &str, value: &str) -> Result<(), VSEError> {
        let recording = self.state.recording.as_mut().ok_or(VSEError::NoSession)?;
        let timestamp = self.state.clock.now();
        recording
            .session
            .send_event(crate::data::messages::EventMessage {
                name: name.to_string(),
                timestamp,
                value: value.to_string(),
            })
            .map_err(|e| VSEError::DataRecording(e.to_string()))?;
        Ok(())
    }

    /// Check if the window should close
    pub fn should_close(&self) -> bool {
        self.state.should_close
    }

    /// Request a clean exit at the end of the current frame.
    ///
    /// Sets the internal close flag.  The loop will break after the current
    /// callback returns (including any pending `flip()`), allowing all Vulkan
    /// resources to be released cleanly.
    pub fn request_exit(&mut self) {
        self.state.should_close = true;
    }

    /// Set the clear color (RGBA, 0.0-1.0 range)
    pub fn set_clear_color(&mut self, r: f32, g: f32, b: f32, a: f32) {
        self.config.clear_color = [r, g, b, a];
    }

    /// Get the current clear color
    pub fn clear_color(&self) -> [f32; 4] {
        self.config.clear_color
    }

    /// Get the window dimensions in physical pixels.
    ///
    /// In fullscreen modes this returns the monitor's native resolution.
    pub fn window_size(&self) -> (u32, u32) {
        self.state.display_size
    }

    /// Get the device (for advanced users)
    pub fn device(&self) -> &Arc<vulkano::device::Device> {
        &self.state.device
    }

    /// Get the queue (for advanced users)
    pub fn queue(&self) -> &Arc<vulkano::device::Queue> {
        &self.state.queue
    }

    /// Get the swapchain manager (for advanced users)
    pub fn swapchain(&self) -> &SwapchainManager {
        &self.state.swapchain
    }

    /// Get the GPU name
    pub fn gpu_name(&self) -> &str {
        self.state.device_selector.device_name()
    }

    /// Get the active timing source.
    pub fn timing_source(&self) -> TimingSource {
        self.state.timing_provider.source()
    }

    /// Get the timing clock (for correlating with external events).
    pub fn clock(&self) -> &Clock {
        &self.state.clock
    }

    /// Get the current frame number (before the next flip).
    /// Attach an external-renderer frame source (see `core::external_frame`).
    ///
    /// Imports the producer's exported image ring + ready semaphores onto VSE's
    /// device. Subsequent flips consume queued external frames as a full-screen
    /// underlay beneath VSE's own draw commands; VSE remains sole present
    /// authority and `FlipInfo` is computed exactly as without a source.
    ///
    /// Requires the `ExtPresentTiming` backend (the CPU-estimate path has no
    /// seam for cross-device semaphore waits).
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
    /// policy. Use [`ExternalFramePolicy::LatestReadyHoldLast`] when VSE should
    /// repeat the last displayed external image instead of dropping to a clear
    /// underlay on frames where no new producer frame has been queued.
    pub fn attach_external_frame_source_with_policy(
        &mut self,
        desc: vse_external_frame::ExternalRingDesc,
        release_tx: vse_external_frame::SlotReleaseTx,
        policy: crate::core::external_frame::ExternalFramePolicy,
    ) -> Result<(), VSEError> {
        use crate::core::external_frame::{ExternalFrameError, ExternalFrameRing};
        if self.state.present_engine.is_none() {
            return Err(ExternalFrameError::Unsupported(
                "external frame sources require the ExtPresentTiming backend \
                 (CPU-estimate timing path active)"
                    .into(),
            )
            .into());
        }
        if self.state.external_source.is_some() {
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
        self.state.external_source = Some(ring);
        Ok(())
    }

    /// Queue the external frame in `slot` for consumption by the next flip.
    ///
    /// Call after the producer finishes rendering into `slot` (and signals its
    /// ready semaphore), before `flip`/`flip_with_payload`. Slots must be
    /// queued in the order the producer acquired them.
    pub fn queue_external_frame(
        &mut self,
        slot: vse_external_frame::SlotIndex,
    ) -> Result<(), VSEError> {
        let src = self.state.external_source.as_mut().ok_or_else(|| {
            crate::core::external_frame::ExternalFrameError::Unsupported(
                "no external frame source attached".into(),
            )
        })?;
        src.note_ready(slot)?;
        Ok(())
    }

    /// Arm a one-shot readback of the next consumed external frame into
    /// `buffer` (determinism-harness hook). The copy is recorded in the same
    /// command buffer as the underlay consumption; the buffer is safe to read
    /// once that flip is confirmed (fence signaled / `Presented` delivered).
    pub fn arm_external_readback(&mut self, buffer: vulkano::buffer::Subbuffer<[u8]>) {
        self.state.external_readback = Some(buffer);
    }

    /// VSE's device memory allocator, for creating buffers on VSE's device
    /// (e.g. an external-frame readback target).
    pub fn memory_allocator(
        &self,
    ) -> std::sync::Arc<vulkano::memory::allocator::StandardMemoryAllocator> {
        self.state.renderer.memory_allocator()
    }

    pub fn frame_number(&self) -> u64 {
        self.state.frame_number
    }

    /// Sample the display's `PRESENT_STAGE_LOCAL` scanout clock against `CLOCK_MONOTONIC`.
    ///
    /// Returns `None` on the CPU-estimate path or before the present-stage time domain has been
    /// probed. Used to characterize the clock offset and relative drift that the present-timing
    /// calibration must correct. See `docs/clock-synchronization.md`.
    pub fn sample_present_calibration(&self) -> Option<crate::timing::CalibrationSample> {
        self.state.timing_provider.sample_present_calibration()
    }

    /// Read back confirmed per-present scanout timings from the driver's past-timing ring
    /// (`vkGetPastPresentationTimingEXT`).
    ///
    /// Each [`ScanoutFeedback`](crate::core::ScanoutFeedback) carries the correlating `present_id`
    /// (matching [`FlipInfo::present_id`]) and the `IMAGE_FIRST_PIXEL_OUT` scanout time in the
    /// driver's present-stage-local domain. Empty on the CPU-estimate path, and for a frame or two
    /// after a present while the driver has not yet recorded it. Rebasing these to a
    /// [`ScanoutTimestamp`](crate::timing::ScanoutTimestamp) is B3's job.
    ///
    /// Returns the records drained on the most recent `flip()`. The driver's read is *destructive*
    /// (each record is dequeued once), so `flip()` drains once per frame and caches the result
    /// here — this accessor never re-drains, and calling it repeatedly returns the same records.
    pub fn scanout_feedback(&self) -> Vec<crate::core::ScanoutFeedback> {
        self.state.recent_scanouts.clone()
    }

    /// Read the current scanout-clock time — VSE's primary experimental clock.
    ///
    /// Returns time since the session's scanout epoch (`t=0`, established on the first flip).
    /// `None` on the CPU-estimate path, or before the first flip has established the epoch.
    pub fn scanout_now(&self) -> Option<ScanoutTimestamp> {
        let clock = self.state.scanout_clock?;
        let sample = self.state.timing_provider.sample_present_calibration()?;
        Some(clock.rebase(sample.stage_ns))
    }

    /// Convert a host-clock [`Timestamp`] (e.g. a key-press or network-event time) into scanout
    /// time, using the opt-in host-clock bridge.
    ///
    /// Returns `None` unless the bridge is enabled ([`VSEContextBuilder::with_host_clock_bridge`]),
    /// warmed up, and the scanout epoch is established. This is the intended way to place
    /// host-originated events on the scanout timeline.
    pub fn host_to_scanout(&self, ts: Timestamp) -> Option<ScanoutTimestamp> {
        let clock = self.state.scanout_clock?;
        let bridge = self.state.host_bridge.as_ref()?;
        let mono_ns = self.state.clock.to_monotonic_nanos(ts)?;
        let stage_ns = bridge.host_to_scanout_ns(mono_ns)?;
        Some(clock.rebase(stage_ns))
    }

    /// Convert a scanout timestamp back into a host-clock [`Timestamp`], using the opt-in bridge.
    ///
    /// Inverse of [`host_to_scanout`](Self::host_to_scanout); same availability conditions.
    pub fn scanout_to_host(&self, ts: ScanoutTimestamp) -> Option<Timestamp> {
        let clock = self.state.scanout_clock?;
        let bridge = self.state.host_bridge.as_ref()?;
        let stage_ns = clock.epoch_stage_ns().saturating_add(ts.as_nanos());
        let mono_ns = bridge.scanout_to_host_ns(stage_ns)?;
        self.state.clock.from_monotonic_nanos(mono_ns)
    }

    /// The host-clock bridge's currently fitted relative drift, in ppm (diagnostic).
    ///
    /// `None` unless the bridge is enabled and warmed up.
    pub fn host_clock_bridge_drift_ppm(&self) -> Option<f64> {
        self.state.host_bridge.as_ref()?.drift_ppm()
    }

    // === Drawing primitives ===

    /// Draw a filled rectangle.
    ///
    /// Coordinates are in pixels with (0, 0) at the top-left of the window.
    pub fn draw_rect(&mut self, left: f32, top: f32, right: f32, bottom: f32, color: Color) {
        self.state.renderer.push(DrawCommand::Rect {
            left,
            top,
            right,
            bottom,
            color,
        });
    }

    /// Draw a filled circle.
    pub fn draw_circle(&mut self, cx: f32, cy: f32, radius: f32, color: Color) {
        let segments = default_circle_segments(radius);
        self.state.renderer.push(DrawCommand::Circle {
            cx,
            cy,
            radius,
            color,
            segments,
        });
    }

    /// Draw a line.
    pub fn draw_line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, width: f32, color: Color) {
        self.state.renderer.push(DrawCommand::Line {
            x1,
            y1,
            x2,
            y2,
            width,
            color,
        });
    }

    /// Draw a texture at the specified rectangle.
    pub fn draw_texture(
        &mut self,
        texture: TextureHandle,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    ) {
        self.state.renderer.push(DrawCommand::Texture {
            texture_id: texture.id,
            left,
            top,
            right,
            bottom,
        });
    }

    /// Set the clear color using a Color value.
    pub fn set_clear(&mut self, color: Color) {
        self.config.clear_color = color.to_array();
    }

    // === Texture management ===

    /// Load a texture from a file.
    pub fn load_image(&mut self, path: impl AsRef<Path>) -> Result<TextureHandle, VSEError> {
        Ok(self.state.renderer.load_image(path)?)
    }

    /// Create a texture from raw RGBA pixel data.
    pub fn load_texture_rgba(
        &mut self,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Result<TextureHandle, VSEError> {
        Ok(self.state.renderer.load_texture_rgba(width, height, data)?)
    }

    /// Create a Gabor patch texture from parameters.
    pub fn create_gabor(&mut self, params: &GaborParams) -> Result<TextureHandle, VSEError> {
        let pixels = params.generate();
        Ok(self
            .state
            .renderer
            .load_texture_rgba(params.size, params.size, &pixels)?)
    }

    /// Unload a texture and free its GPU resources.
    pub fn unload_texture(&mut self, handle: TextureHandle) {
        self.state.renderer.unload_texture(handle);
    }

    // === Advanced stimuli ===

    /// Draw a sinusoidal or square-wave grating.
    ///
    /// The grating fills the rectangle defined by (left, top, right, bottom)
    /// in pixel coordinates. Parameters control spatial frequency, orientation,
    /// phase, contrast, and waveform type.
    pub fn draw_grating(
        &mut self,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        params: &GratingParams,
    ) {
        self.state.renderer.push(DrawCommand::Grating {
            left,
            top,
            right,
            bottom,
            params: params.clone(),
        });
    }

    /// Draw a Gabor patch (grating windowed by a Gaussian envelope).
    ///
    /// Unlike `create_gabor()` which generates a CPU texture, this computes
    /// the Gabor mathematically on the GPU each frame, allowing real-time
    /// parameter animation.
    pub fn draw_gabor_shader(
        &mut self,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        params: &GaborParams,
    ) {
        self.state.renderer.push(DrawCommand::Gabor {
            left,
            top,
            right,
            bottom,
            params: params.clone(),
        });
    }

    /// Draw a noise pattern.
    ///
    /// Generates a noise texture on CPU from the given parameters and
    /// displays it in the specified rectangle. For animated noise, change
    /// `params.seed` each frame.
    pub fn draw_noise(
        &mut self,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        params: &NoiseParams,
    ) -> Result<(), VSEError> {
        let pixels = crate::drawing::noise::generate_noise(params);
        let handle = self
            .state
            .renderer
            .load_texture_rgba(params.width, params.height, &pixels)?;
        self.state.renderer.push(DrawCommand::Noise {
            left,
            top,
            right,
            bottom,
            texture_id: handle.id,
        });
        Ok(())
    }

    /// Draw filled circular dots at the specified positions.
    ///
    /// This is the rendering primitive for Random Dot Kinematograms.
    /// Positions are in pixel coordinates. Each dot is rendered as a
    /// filled circle with an anti-aliased edge.
    pub fn draw_dots(&mut self, positions: &[(f32, f32)], radius: f32, color: Color) {
        if positions.is_empty() {
            return;
        }
        self.state.renderer.push(DrawCommand::Dots {
            positions: positions.iter().map(|&(x, y)| [x, y]).collect(),
            radius,
            color,
        });
    }

    /// Capture a snapshot of the full host machine state.
    ///
    /// Returns a [`HostInfo`](crate::host::HostInfo) struct containing OS, CPU, memory, GPU,
    /// display, swapchain, pipeline config, build metadata, runtime
    /// environment, and EDID monitor data.
    ///
    /// This is an on-demand operation — call it when you need a snapshot.
    /// The EDID capture shells out to `xrandr`, which may take ~50ms.
    pub fn capture_host_info(&self) -> crate::host::HostInfo {
        crate::host::capture::capture_host_info(
            self.state.device_selector.physical_device(),
            &self.state.device,
            self.state.window.as_deref(),
            &self.state.swapchain,
            self.config,
            crate::host::capture::ObservedPresentTiming {
                scanout_feedback_populated: self.state.scanout_feedback_populated,
                // Enforcement is not auto-probed (it disrupts frames); `None` here. The
                // direct-display characterization example measures and reports it.
                absolute_scheduling_enforced: None,
                queue_global_priority: self.state.ext_features.map(|f| f.queue_priority),
            },
        )
    }

    /// Whether the driver was observed to actually populate `IMAGE_FIRST_PIXEL_OUT` in
    /// present-timing feedback, as opposed to merely advertising `VK_EXT_present_timing`.
    ///
    /// `Some(true)` — real per-present scanout timestamps; `Some(false)` — feedback correlates by
    /// present_id but carries zero-valued stage times (VSE falls back to sampling the calibrated
    /// scanout clock; measured on Intel/ANV/Mesa 26.1); `None` — not yet determined (too few flips,
    /// or the CPU-estimate backend). A guardrail against trusting an advertised-but-unimplemented
    /// feature. See `docs/clock-synchronization.md`.
    pub fn scanout_feedback_populated(&self) -> Option<bool> {
        self.state.scanout_feedback_populated
    }

    // === Input polling (frame-aligned) ===

    /// Returns `true` if the key is currently held down.
    pub fn key_pressed(&self, key: KeyCode) -> bool {
        self.state.input.keys_down.contains(&key)
    }

    /// Returns `true` if the key was pressed this frame (not held from previous frame).
    pub fn key_just_pressed(&self, key: KeyCode) -> bool {
        self.state.input.keys_just_pressed.contains(&key)
    }

    /// Returns `true` if the key was released this frame.
    pub fn key_just_released(&self, key: KeyCode) -> bool {
        self.state.input.keys_just_released.contains(&key)
    }

    /// Get the current mouse position in window-relative pixels.
    pub fn mouse_position(&self) -> (f64, f64) {
        self.state.input.mouse_position
    }

    /// Returns `true` if the mouse button is currently held down.
    pub fn mouse_button_pressed(&self, button: MouseButton) -> bool {
        self.state.input.buttons_down.contains(&button)
    }

    /// Returns `true` if the mouse button was pressed this frame.
    pub fn mouse_button_just_pressed(&self, button: MouseButton) -> bool {
        self.state.input.buttons_just_pressed.contains(&button)
    }

    // === Event queue (timing-precise) ===

    /// Get all input events since the last `flip()`.
    ///
    /// Each event carries a precise timestamp from the VSE `Clock`,
    /// suitable for reaction-time measurement relative to `FlipInfo` timestamps.
    pub fn input_events(&self) -> &[InputEvent] {
        &self.state.input.events
    }

    // === Cursor control ===

    /// Set whether the mouse cursor is visible.
    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.state.cursor_visible = visible;
        if let Some(w) = &self.state.window {
            w.set_cursor_visible(visible);
        }
    }

    /// Move the cursor to the specified position (logical pixels).
    pub fn set_cursor_position(&self, x: f64, y: f64) {
        if let Some(w) = &self.state.window {
            let _ = w.set_cursor_position(LogicalPosition::new(x, y));
        }
    }

    /// Returns whether the cursor is currently visible.
    pub fn cursor_visible(&self) -> bool {
        self.state.cursor_visible
    }

    // === Display backend detection ===

    /// Detect the display backend (windowing system) used for this session.
    ///
    /// Derived from the raw window handle type. Use this to warn users when
    /// running under X11/XWayland, which has higher timing jitter than native
    /// Wayland or direct display mode.
    ///
    /// # Example
    /// ```no_run
    /// # use vision_stimulus_engine::prelude::*;
    /// # fn example(vse: &mut RenderContext) {
    /// let backend = vse.display_backend();
    /// if backend.has_compositor() {
    ///     println!("Warning: frames pass through {}", backend.description());
    /// }
    /// # }
    /// ```
    pub fn display_backend(&self) -> DisplayBackend {
        // Direct display mode: no window, check the stored acquisition method
        if let Some(method) = self.state.acquired_display {
            return DisplayBackend::DirectDisplay { method };
        }

        // Compositor mode: detect from raw window handle
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Some(window) = &self.state.window {
            return match window.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Wayland(_)) => DisplayBackend::Wayland,
                Ok(RawWindowHandle::Xcb(_)) | Ok(RawWindowHandle::Xlib(_)) => DisplayBackend::X11,
                Ok(RawWindowHandle::Win32(_)) => DisplayBackend::Windows,
                Ok(RawWindowHandle::AppKit(_)) => DisplayBackend::MacOS,
                _ => DisplayBackend::Unknown,
            };
        }

        DisplayBackend::Unknown
    }

    // === Monitor & video mode queries ===

    /// Get information about all available monitors.
    ///
    /// Duplicates are filtered: some Wayland compositors advertise the same physical
    /// output via multiple `wl_output` globals. Monitors are considered identical if
    /// they share the same name, resolution, and desktop position.
    pub fn available_monitors(&self) -> Vec<MonitorInfo> {
        let window = match &self.state.window {
            Some(w) => w,
            None => return vec![],
        };
        let mut seen = std::collections::HashSet::new();
        window
            .available_monitors()
            .filter(|handle| {
                let pos = handle.position();
                let size = handle.size();
                let key = (handle.name(), size.width, size.height, pos.x, pos.y);
                seen.insert(key)
            })
            .enumerate()
            .map(|(i, handle)| monitor_handle_to_info(i, &handle))
            .collect()
    }

    /// Get information about the primary monitor, if available.
    pub fn primary_monitor(&self) -> Option<MonitorInfo> {
        self.state
            .window
            .as_ref()?
            .primary_monitor()
            .map(|handle| monitor_handle_to_info(0, &handle))
    }

    /// Get all video modes for a monitor by index.
    pub fn video_modes(&self, monitor_index: usize) -> Vec<VideoModeInfo> {
        let window = match &self.state.window {
            Some(w) => w,
            None => return vec![],
        };
        let monitors: Vec<_> = window.available_monitors().collect();
        monitors
            .get(monitor_index)
            .map(|handle| {
                handle
                    .video_modes()
                    .map(|m| VideoModeInfo {
                        width: m.size().width,
                        height: m.size().height,
                        refresh_rate_hz: m.refresh_rate_millihertz() as f64 / 1000.0,
                        bit_depth: m.bit_depth(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all video modes for the current monitor (the monitor the window is on).
    pub fn current_monitor_video_modes(&self) -> Vec<VideoModeInfo> {
        let window = match &self.state.window {
            Some(w) => w,
            None => return vec![],
        };
        window
            .current_monitor()
            .map(|handle| {
                handle
                    .video_modes()
                    .map(|m| VideoModeInfo {
                        width: m.size().width,
                        height: m.size().height,
                        refresh_rate_hz: m.refresh_rate_millihertz() as f64 / 1000.0,
                        bit_depth: m.bit_depth(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get the current window display mode.
    pub fn window_mode(&self) -> WindowMode {
        self.state.window_mode
    }

    /// Change the window display mode at runtime.
    ///
    /// Switches between windowed, borderless fullscreen, and exclusive fullscreen.
    /// Cursor visibility is automatically updated unless previously overridden
    /// via [`set_cursor_visible`](Self::set_cursor_visible).
    pub fn set_window_mode(&mut self, mode: WindowMode) {
        if mode == WindowMode::DirectDisplay {
            warn!("set_window_mode(DirectDisplay) has no effect — use WindowMode::DirectDisplay in the builder");
            return;
        }
        if let Some(w) = &self.state.window {
            let fullscreen = match mode {
                WindowMode::Windowed => None,
                WindowMode::DirectDisplay => unreachable!(),
                WindowMode::BorderlessFullscreen => {
                    Some(Fullscreen::Borderless(w.current_monitor()))
                }
                WindowMode::ExclusiveFullscreen => {
                    if let Some(monitor) = w.current_monitor() {
                        let best = monitor.video_modes().max_by(|a, b| {
                            let area_a = a.size().width * a.size().height;
                            let area_b = b.size().width * b.size().height;
                            area_a.cmp(&area_b).then(
                                a.refresh_rate_millihertz()
                                    .cmp(&b.refresh_rate_millihertz()),
                            )
                        });
                        match best {
                            Some(vm) => Some(Fullscreen::Exclusive(vm)),
                            None => Some(Fullscreen::Borderless(Some(monitor))),
                        }
                    } else {
                        Some(Fullscreen::Borderless(None))
                    }
                }
            };
            w.set_fullscreen(fullscreen);

            // Auto-update cursor visibility if not explicitly overridden by config
            if self.config.cursor_visible.is_none() {
                let visible = matches!(mode, WindowMode::Windowed);
                self.state.cursor_visible = visible;
                w.set_cursor_visible(visible);
            }
        } else {
            warn!("set_window_mode() has no effect in DirectDisplay mode");
        }
        self.state.window_mode = mode;
    }
}

/// Convert a winit MonitorHandle to our MonitorInfo type.
fn monitor_handle_to_info(index: usize, handle: &winit::monitor::MonitorHandle) -> MonitorInfo {
    let size = handle.size();
    let position = handle.position();
    let video_modes: Vec<VideoModeInfo> = handle
        .video_modes()
        .map(|m| VideoModeInfo {
            width: m.size().width,
            height: m.size().height,
            refresh_rate_hz: m.refresh_rate_millihertz() as f64 / 1000.0,
            bit_depth: m.bit_depth(),
        })
        .collect();

    // Get refresh rate from the highest-res, highest-refresh video mode
    let refresh_rate_hz = video_modes
        .iter()
        .map(|m| m.refresh_rate_hz)
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    MonitorInfo {
        name: handle.name(),
        index,
        width: size.width,
        height: size.height,
        refresh_rate_hz,
        scale_factor: handle.scale_factor(),
        position: (position.x, position.y),
        video_modes,
    }
}
