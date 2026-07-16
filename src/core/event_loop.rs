//! Winit event loops for synchronous and buffered presentation.

use std::cell::RefCell;
use std::rc::Rc;

use tracing::{debug, info, warn};
use winit::event::{Event, WindowEvent};
use winit::event_loop::ControlFlow;

use super::buffered::{BufferedConfig, FlipEvent};
use super::config::{VSEConfig, VSEError};
use super::context::{RenderContext, VSEContext};
use super::input::WindowMode;
use super::state::{RecordingState, VSEState};
use super::swapchain::SwapchainError;

impl VSEContext {
    /// Run the main event loop
    ///
    /// This method takes ownership of the context and runs the event loop
    /// until the window is closed. The provided callback is called once
    /// per frame.
    ///
    /// # Arguments
    ///
    /// * `render_fn` - A callback that is called each frame for rendering
    ///
    /// # Errors
    ///
    /// Returns `VSEError` if an error occurs during rendering.
    pub fn run<F>(mut self, mut render_fn: F) -> Result<(), VSEError>
    where
        F: FnMut(&mut RenderContext) -> Result<(), VSEError> + 'static,
    {
        // Branch for direct display mode (Linux only — no winit event loop)
        #[cfg(target_os = "linux")]
        if self.config.window_mode == WindowMode::DirectDisplay {
            return self.run_direct(render_fn);
        }
        #[cfg(not(target_os = "linux"))]
        if self.config.window_mode == WindowMode::DirectDisplay {
            return Err(VSEError::DirectDisplayUnavailable(
                "Direct display mode is only supported on Linux".to_string(),
            ));
        }

        let event_loop = self
            .event_loop
            .take()
            .ok_or_else(|| VSEError::EventLoop("Event loop already consumed".into()))?;

        let mut config = self.config;
        let mut session = self.session;
        let mut state: Option<VSEState> = None;
        let error: Rc<RefCell<Option<VSEError>>> = Rc::new(RefCell::new(None));
        let error_clone = error.clone();

        event_loop
            .run(move |event, elwt| {
                elwt.set_control_flow(ControlFlow::Poll);

                match event {
                    Event::Resumed => {
                        if state.is_some() {
                            return;
                        }

                        match Self::initialize_compositor(elwt, &config) {
                            Ok(mut s) => {
                                s.recording = session.take().map(|sess| RecordingState {
                                    session: sess,
                                    pending_flip: None,
                                    last_claimed_frame: None,
                                });
                                state = Some(s);
                            }
                            Err(e) => {
                                *error_clone.borrow_mut() = Some(e);
                                elwt.exit();
                            }
                        }
                    }
                    Event::WindowEvent {
                        event: window_event,
                        ..
                    } => {
                        let s = match &mut state {
                            Some(s) => s,
                            None => return,
                        };

                        match window_event {
                            WindowEvent::CloseRequested => {
                                info!("Window close requested");
                                s.should_close = true;
                                elwt.exit();
                            }
                            WindowEvent::Resized(new_size) => {
                                debug!("Window resized to {}x{}", new_size.width, new_size.height);
                                if new_size.width == 0 || new_size.height == 0 {
                                    s.minimized = true;
                                } else {
                                    s.minimized = false;
                                    s.display_size = (new_size.width, new_size.height);
                                    s.swapchain.mark_needs_recreation();
                                }
                            }
                            WindowEvent::KeyboardInput { .. }
                            | WindowEvent::CursorMoved { .. }
                            | WindowEvent::MouseInput { .. }
                            | WindowEvent::MouseWheel { .. } => {
                                s.handle_winit_input(&window_event);
                            }
                            WindowEvent::RedrawRequested => {
                                if s.minimized {
                                    return;
                                }

                                let mut render_ctx = RenderContext {
                                    state: s,
                                    config: &mut config,
                                };

                                if let Err(e) = render_fn(&mut render_ctx) {
                                    warn!("Render error: {}", e);
                                    *error_clone.borrow_mut() = Some(e);
                                    elwt.exit();
                                }

                                // Clear per-frame input state AFTER the callback runs.
                                // KeyboardInput/MouseInput events arrive before RedrawRequested
                                // in the same event loop iteration, so begin_frame() must run
                                // after the callback — not before — or it would erase those events
                                // before the callback ever sees them.
                                s.input.begin_frame();

                                // Honor a callback's request_exit(): break the loop after this
                                // frame, mirroring the buffered and direct-display paths.
                                if s.should_close {
                                    elwt.exit();
                                }
                            }
                            _ => {}
                        }
                    }
                    Event::AboutToWait => {
                        if let Some(s) = &state {
                            if let Some(w) = &s.window {
                                w.request_redraw();
                            }
                        }
                    }
                    Event::LoopExiting => {
                        if let Some(s) = &mut state {
                            if let Some(recording) = &mut s.recording {
                                recording.on_shutdown();
                            }
                        }
                    }
                    _ => {}
                }
            })
            .map_err(|e| VSEError::EventLoop(e.to_string()))?;

        // Check if any error occurred during the event loop
        if let Some(err) = error.borrow_mut().take() {
            return Err(err);
        }

        info!("VSEContext shut down cleanly");
        Ok(())
    }

    /// Run the experiment loop in buffered (pipelined) mode.
    ///
    /// Unlike [`Self::run`], which blocks on every GPU fence, `run_buffered` pipelines CPU
    /// and GPU work across frames. The callback receives two alternating event variants:
    ///
    /// - [`FlipEvent::Render`]: build and submit frame `N` via
    ///   [`flip_with_payload()`](RenderContext::flip_with_payload). Fires every vblank.
    /// - [`FlipEvent::Presented`]: GPU has confirmed frame `N - depth` was scanned out.
    ///   `flip_info.present_time` is a confirmed timestamp. Call `record_frame(payload)?`
    ///   here to record data with accurate timing.
    ///
    /// During the first `config.depth` iterations only `Render` fires (queue warming up).
    /// On clean exit, all pending `Presented` events are drained before returning.
    ///
    /// # Closed-loop experiments
    ///
    /// The B-frame latency is explicit and predictable: when `Presented` fires for frame
    /// `N`, frame `N+1` has already been submitted. Stimulus updates in `Presented` take
    /// effect from frame `N+2` onward.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::{cell::RefCell, rc::Rc};
    /// use vision_stimulus_engine::prelude::*;
    ///
    /// #[derive(serde::Serialize)]
    /// struct FrameData { trial: u32, contrast: f32 }
    ///
    /// let context = VSEContext::builder().with_window_size(800, 600).build()?;
    ///
    /// let contrast = Rc::new(RefCell::new(1.0f32));
    /// let trial    = Rc::new(RefCell::new(0u32));
    /// let c = contrast.clone();
    /// let t = trial.clone();
    ///
    /// context.run_buffered::<FrameData, _>(BufferedConfig::default(), move |event, vse| {
    ///     match event {
    ///         FlipEvent::Render => {
    ///             vse.clear()?;
    ///             // draw stimulus …
    ///             let data = FrameData { trial: *t.borrow(), contrast: *c.borrow() };
    ///             vse.flip_with_payload(None, data)?;
    ///         }
    ///         FlipEvent::Presented { flip_info, payload } => {
    ///             // Confirmed hardware timing — safe to record
    ///             vse.record_frame(payload)?;
    ///             // Closed-loop: reduce contrast on missed frames
    ///             if flip_info.missed {
    ///                 *c.borrow_mut() *= 0.9;
    ///             }
    ///         }
    ///         _ => {}
    ///     }
    ///     Ok(())
    /// })?;
    ///
    /// # Ok::<(), VSEError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Propagates any `VSEError` returned by the callback, or returns
    /// `VSEError::EventLoop` if the underlying windowing system fails.
    pub fn run_buffered<T, F>(
        mut self,
        config: BufferedConfig,
        mut callback: F,
    ) -> Result<(), VSEError>
    where
        T: std::any::Any + serde::Serialize + Send + 'static,
        F: FnMut(FlipEvent<T>, &mut RenderContext<'_>) -> Result<(), VSEError> + 'static,
    {
        use crate::core::buffered::PendingFrame;
        use std::collections::VecDeque;

        // Branch for direct display mode
        #[cfg(target_os = "linux")]
        if self.config.window_mode == WindowMode::DirectDisplay {
            return Err(VSEError::EventLoop(
                "run_buffered() does not support DirectDisplay mode".into(),
            ));
        }

        let event_loop = self
            .event_loop
            .take()
            .ok_or_else(|| VSEError::EventLoop("Event loop already consumed".into()))?;

        let mut vse_config = self.config;
        let mut session = self.session;
        let mut state: Option<VSEState> = None;

        // pending_frames lives alongside in_flight fences; same FIFO order.
        let pending_frames: Rc<RefCell<VecDeque<PendingFrame<T>>>> =
            Rc::new(RefCell::new(VecDeque::with_capacity(config.depth + 1)));

        let error: Rc<RefCell<Option<VSEError>>> = Rc::new(RefCell::new(None));
        let error_clone = error.clone();

        event_loop
            .run(move |event, elwt| {
                elwt.set_control_flow(ControlFlow::Poll);

                match event {
                    Event::Resumed => {
                        if state.is_some() {
                            return;
                        }
                        match Self::initialize_compositor(elwt, &vse_config) {
                            Ok(mut s) => {
                                s.recording = session.take().map(|sess| RecordingState {
                                    session: sess,
                                    pending_flip: None,
                                    last_claimed_frame: None,
                                });
                                s.in_buffered_mode = true;
                                let required = (config.depth + 1) as u32;
                                if let Err(e) = s.swapchain.ensure_image_count(required) {
                                    *error_clone.borrow_mut() = Some(e.into());
                                    elwt.exit();
                                    return;
                                }
                                // The raw present engine pipelines `depth + 1` frames, so its
                                // sync ring must have at least that many slots (+1 slack) or a
                                // slot's fence would be reset while its frame is still in flight.
                                if let Some(engine) = &mut s.present_engine {
                                    let slots = s.swapchain.images().len() + 1;
                                    if !engine.ensure_ring(slots) {
                                        *error_clone.borrow_mut() = Some(VSEError::Swapchain(
                                            SwapchainError::CreationFailed(
                                                "failed to grow present engine sync ring".into(),
                                            ),
                                        ));
                                        elwt.exit();
                                        return;
                                    }
                                }
                                state = Some(s);
                            }
                            Err(e) => {
                                *error_clone.borrow_mut() = Some(e);
                                elwt.exit();
                            }
                        }
                    }
                    Event::WindowEvent {
                        event: window_event,
                        ..
                    } => {
                        let s = match &mut state {
                            Some(s) => s,
                            None => return,
                        };
                        match window_event {
                            WindowEvent::CloseRequested => {
                                info!("Window close requested");
                                s.should_close = true;
                                // Do NOT call elwt.exit() yet — let RedrawRequested drain.
                            }
                            WindowEvent::Resized(new_size) => {
                                debug!("Window resized to {}x{}", new_size.width, new_size.height);
                                if new_size.width == 0 || new_size.height == 0 {
                                    s.minimized = true;
                                } else {
                                    s.minimized = false;
                                    s.display_size = (new_size.width, new_size.height);
                                    s.swapchain.mark_needs_recreation();
                                }
                            }
                            WindowEvent::KeyboardInput { .. }
                            | WindowEvent::CursorMoved { .. }
                            | WindowEvent::MouseInput { .. }
                            | WindowEvent::MouseWheel { .. } => {
                                s.handle_winit_input(&window_event);
                            }
                            WindowEvent::RedrawRequested => {
                                if s.minimized {
                                    return;
                                }

                                // ── Phase 1: Check for confirmed presentation ──────────
                                let oldest_complete = s
                                    .buffered_in_flight
                                    .front()
                                    .map(|(_, fence)| fence.is_complete())
                                    .unwrap_or(false);

                                if oldest_complete {
                                    let (estimated_flip, fence) =
                                        s.buffered_in_flight.pop_front().unwrap();
                                    fence.wait_blocking();

                                    if let Some(pf) = pending_frames.borrow_mut().pop_front() {
                                        debug_assert_eq!(
                                            pf.frame_number, pf.estimated_flip.frame_number,
                                            "PendingFrame FIFO mismatch"
                                        );
                                        let confirmed = s.build_confirmed_flip(estimated_flip);
                                        s.buffered_confirmed_flip = Some(confirmed.clone());
                                        s.buffered_record_called_this_presented = false;

                                        let mut render_ctx = RenderContext {
                                            state: s,
                                            config: &mut vse_config,
                                        };
                                        if let Err(e) = callback(
                                            FlipEvent::Presented {
                                                flip_info: confirmed,
                                                payload: pf.payload,
                                            },
                                            &mut render_ctx,
                                        ) {
                                            *error_clone.borrow_mut() = Some(e);
                                            elwt.exit();
                                            return;
                                        }
                                        s.buffered_confirmed_flip = None;
                                    }
                                }

                                // Early exit if callback requested close during Presented
                                if s.should_close {
                                    Self::drain_buffered(
                                        s,
                                        &mut vse_config,
                                        &pending_frames,
                                        &mut callback,
                                    );
                                    if let Some(recording) = &mut s.recording {
                                        recording.on_shutdown();
                                    }
                                    s.in_buffered_mode = false;
                                    elwt.exit();
                                    return;
                                }

                                // ── Phase 2: Render ────────────────────────────────────
                                {
                                    let mut render_ctx = RenderContext {
                                        state: s,
                                        config: &mut vse_config,
                                    };
                                    if let Err(e) = callback(FlipEvent::Render, &mut render_ctx) {
                                        *error_clone.borrow_mut() = Some(e);
                                        elwt.exit();
                                        return;
                                    }

                                    // Pick up payload stored by flip_with_payload()
                                    if let Some(raw) = s.buffered_pending_payload.take() {
                                        let payload = *raw
                                            .downcast::<T>()
                                            .expect("buffered payload type mismatch");
                                        if let Some((estimated_flip, _)) =
                                            s.buffered_in_flight.back()
                                        {
                                            let ef = estimated_flip.clone();
                                            pending_frames.borrow_mut().push_back(PendingFrame {
                                                frame_number: ef.frame_number,
                                                payload,
                                                estimated_flip: ef,
                                            });
                                        }
                                    }
                                }

                                s.input.begin_frame();

                                if s.should_close {
                                    Self::drain_buffered(
                                        s,
                                        &mut vse_config,
                                        &pending_frames,
                                        &mut callback,
                                    );
                                    if let Some(recording) = &mut s.recording {
                                        recording.on_shutdown();
                                    }
                                    s.in_buffered_mode = false;
                                    elwt.exit();
                                }
                            }
                            _ => {}
                        }
                    }
                    Event::AboutToWait => {
                        if let Some(s) = &state {
                            if let Some(w) = &s.window {
                                w.request_redraw();
                            }
                        }
                    }
                    Event::LoopExiting => {
                        if let Some(s) = &mut state {
                            s.in_buffered_mode = false;
                            if let Some(recording) = &mut s.recording {
                                recording.on_shutdown();
                            }
                        }
                    }
                    _ => {}
                }
            })
            .map_err(|e| VSEError::EventLoop(e.to_string()))?;

        if let Some(err) = error.borrow_mut().take() {
            return Err(err);
        }

        info!("VSEContext (buffered) shut down cleanly");
        Ok(())
    }

    /// Drain all remaining in-flight fences and fire Presented events.
    ///
    /// Called on clean shutdown from within `run_buffered()`.
    fn drain_buffered<T, F>(
        state: &mut VSEState,
        config: &mut VSEConfig,
        pending_frames: &Rc<
            RefCell<std::collections::VecDeque<crate::core::buffered::PendingFrame<T>>>,
        >,
        callback: &mut F,
    ) where
        T: std::any::Any + serde::Serialize + Send + 'static,
        F: FnMut(FlipEvent<T>, &mut RenderContext<'_>) -> Result<(), VSEError>,
    {
        while let Some((estimated_flip, fence)) = state.buffered_in_flight.pop_front() {
            fence.wait_blocking();
            if let Some(pf) = pending_frames.borrow_mut().pop_front() {
                debug_assert_eq!(
                    pf.frame_number, pf.estimated_flip.frame_number,
                    "PendingFrame FIFO mismatch"
                );
                let confirmed = state.build_confirmed_flip(estimated_flip);
                state.buffered_confirmed_flip = Some(confirmed.clone());
                state.buffered_record_called_this_presented = false;
                let mut render_ctx = RenderContext { state, config };
                let _ = callback(
                    FlipEvent::Presented {
                        flip_info: confirmed,
                        payload: pf.payload,
                    },
                    &mut render_ctx,
                );
                state.buffered_confirmed_flip = None;
            }
        }
    }
}
