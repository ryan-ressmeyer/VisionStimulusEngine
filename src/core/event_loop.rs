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

/// One-time setup, run after the GPU state exists and before the first frame.
///
/// Type-erased so the loop implementation is shared between
/// [`VSEContext::run`] and [`VSEContext::run_with_setup`]; the latter keeps the
/// caller's concrete return type at the public boundary.
pub(crate) type SetupFn = Box<dyn FnOnce(&mut RenderContext) -> Result<(), VSEError>>;

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
    pub fn run<F>(self, render_fn: F) -> Result<(), VSEError>
    where
        F: FnMut(&mut RenderContext) -> Result<(), VSEError> + 'static,
    {
        self.run_with_setup_boxed(None, render_fn)
    }

    /// Like [`run`](Self::run), but runs `setup` once before the first frame
    /// and threads its result into every frame.
    ///
    /// `setup` executes immediately after the GPU state exists and **before
    /// frame 0**, so expensive one-time work — compiling a
    /// [`StimulusPipeline`](crate::prelude::StimulusPipeline), loading textures
    /// or models — happens off the presentation path instead of inflating the
    /// first frame and registering as a missed deadline.
    ///
    /// Whatever `setup` returns (pipeline handles, texture handles, trial
    /// state) is handed to the render closure as `&mut T`.
    ///
    /// ```no_run
    /// use vision_stimulus_engine::prelude::*;
    /// # fn demo(context: VSEContext) -> Result<(), VSEError> {
    /// context.run_with_setup(
    ///     |vse| vse.load_image("stimulus.png"),
    ///     |vse, texture| {
    ///         vse.draw_texture(*texture, 0.0, 0.0, 256.0, 256.0);
    ///         vse.flip(None)?;
    ///         Ok(())
    ///     },
    /// )
    /// # }
    /// ```
    ///
    /// Registration is only possible here rather than before the loop because
    /// selecting a presentation-capable Vulkan device requires a surface, which
    /// does not exist until the window does.
    pub fn run_with_setup<S, T, F>(self, setup: S, mut render_fn: F) -> Result<(), VSEError>
    where
        S: FnOnce(&mut RenderContext) -> Result<T, VSEError> + 'static,
        T: 'static,
        F: FnMut(&mut RenderContext, &mut T) -> Result<(), VSEError> + 'static,
    {
        // The setup result is produced in one closure and consumed in another,
        // so it lands in a shared slot rather than being threaded through the
        // loop's type signature.
        let slot: Rc<RefCell<Option<T>>> = Rc::new(RefCell::new(None));
        let setup_slot = slot.clone();

        self.run_with_setup_boxed(
            Some(Box::new(move |ctx| {
                *setup_slot.borrow_mut() = Some(setup(ctx)?);
                Ok(())
            })),
            move |ctx| {
                let mut slot = slot.borrow_mut();
                let state = slot
                    .as_mut()
                    .expect("setup runs before the first frame, so the slot is filled");
                render_fn(ctx, state)
            },
        )
    }

    /// Shared implementation of [`run`](Self::run) and
    /// [`run_with_setup`](Self::run_with_setup). `setup`, when present, runs
    /// once after the GPU state is initialized and before the first frame.
    fn run_with_setup_boxed<F>(
        mut self,
        mut setup: Option<SetupFn>,
        mut render_fn: F,
    ) -> Result<(), VSEError>
    where
        F: FnMut(&mut RenderContext) -> Result<(), VSEError> + 'static,
    {
        // Branch for direct display mode (Linux only — no winit event loop)
        #[cfg(target_os = "linux")]
        if self.config.window_mode == WindowMode::DirectDisplay {
            return self.run_direct(setup.take(), render_fn);
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
                                // Run one-time setup BEFORE frame 0, so
                                // pipeline compilation and asset loading stay
                                // off the presentation path.
                                if let Some(setup) = setup.take() {
                                    let mut setup_ctx = RenderContext {
                                        state: &mut s,
                                        config: &mut config,
                                    };
                                    if let Err(e) = setup(&mut setup_ctx) {
                                        *error_clone.borrow_mut() = Some(e);
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
                                elwt.exit();
                            }
                            WindowEvent::Resized(new_size) => {
                                debug!("Window resized to {}x{}", new_size.width, new_size.height);
                                if new_size.width == 0 || new_size.height == 0 {
                                    s.target.present_expect_mut().minimized = true;
                                } else {
                                    let present = s.target.present_expect_mut();
                                    present.minimized = false;
                                    present.display_size = (new_size.width, new_size.height);
                                    present.swapchain.mark_needs_recreation();
                                }
                            }
                            WindowEvent::KeyboardInput { .. }
                            | WindowEvent::CursorMoved { .. }
                            | WindowEvent::MouseInput { .. }
                            | WindowEvent::MouseWheel { .. } => {
                                s.handle_winit_input(&window_event);
                            }
                            WindowEvent::RedrawRequested => {
                                if s.target.present_expect().minimized {
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
                            if let Some(w) = &s.target.present_expect().window {
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
    pub fn run_buffered<T, F>(self, config: BufferedConfig, callback: F) -> Result<(), VSEError>
    where
        T: std::any::Any + serde::Serialize + Send + 'static,
        F: FnMut(FlipEvent<T>, &mut RenderContext<'_>) -> Result<(), VSEError> + 'static,
    {
        self.run_buffered_boxed(config, None, callback)
    }

    /// Like [`run_buffered`](Self::run_buffered), but runs `setup` once before
    /// the first frame and threads its result into every event.
    ///
    /// The buffered counterpart of
    /// [`run_with_setup`](Self::run_with_setup), and it exists for the same
    /// reason: compiling a [`StimulusPipeline`](crate::prelude::StimulusPipeline)
    /// or loading assets inside the first `Render` event would inflate that
    /// frame past its deadline. `setup` runs after the GPU state and swapchain
    /// exist — including the buffered depth adjustment — and before frame 0.
    ///
    /// ```no_run
    /// use vision_stimulus_engine::prelude::*;
    /// # fn demo(context: VSEContext) -> Result<(), VSEError> {
    /// context.run_buffered_with_setup::<u32, _, _, _>(
    ///     BufferedConfig::default(),
    ///     |vse| vse.load_image("stimulus.png"),
    ///     |event, vse, texture| {
    ///         if let FlipEvent::Render = event {
    ///             vse.draw_texture(*texture, 0.0, 0.0, 256.0, 256.0);
    ///             vse.flip_with_payload(None, 0u32)?;
    ///         }
    ///         Ok(())
    ///     },
    /// )
    /// # }
    /// ```
    pub fn run_buffered_with_setup<T, S, U, F>(
        self,
        config: BufferedConfig,
        setup: S,
        mut callback: F,
    ) -> Result<(), VSEError>
    where
        T: std::any::Any + serde::Serialize + Send + 'static,
        S: FnOnce(&mut RenderContext) -> Result<U, VSEError> + 'static,
        U: 'static,
        F: FnMut(FlipEvent<T>, &mut RenderContext<'_>, &mut U) -> Result<(), VSEError> + 'static,
    {
        // Produced in one closure and consumed in another, so it lands in a
        // shared slot rather than in the loop's type signature — the same shape
        // `run_with_setup` uses.
        let slot: Rc<RefCell<Option<U>>> = Rc::new(RefCell::new(None));
        let setup_slot = slot.clone();

        self.run_buffered_boxed(
            config,
            Some(Box::new(move |ctx| {
                *setup_slot.borrow_mut() = Some(setup(ctx)?);
                Ok(())
            })),
            move |event, ctx| {
                let mut slot = slot.borrow_mut();
                let state = slot
                    .as_mut()
                    .expect("setup runs before the first frame, so the slot is filled");
                callback(event, ctx, state)
            },
        )
    }

    /// Shared implementation of [`run_buffered`](Self::run_buffered) and
    /// [`run_buffered_with_setup`](Self::run_buffered_with_setup).
    fn run_buffered_boxed<T, F>(
        mut self,
        config: BufferedConfig,
        mut setup: Option<SetupFn>,
        mut callback: F,
    ) -> Result<(), VSEError>
    where
        T: std::any::Any + serde::Serialize + Send + 'static,
        F: FnMut(FlipEvent<T>, &mut RenderContext<'_>) -> Result<(), VSEError> + 'static,
    {
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
                                s.target.present_expect_mut().in_buffered_mode = true;
                                let required = (config.depth + 1) as u32;
                                if let Err(e) = s
                                    .target
                                    .present_expect_mut()
                                    .swapchain
                                    .ensure_image_count(required)
                                {
                                    *error_clone.borrow_mut() = Some(e.into());
                                    elwt.exit();
                                    return;
                                }
                                // The raw present engine pipelines `depth + 1` frames, so its
                                // sync ring must have at least that many slots (+1 slack) or a
                                // slot's fence would be reset while its frame is still in flight.
                                let present = s.target.present_expect_mut();
                                if let Some(engine) = &mut present.present_engine {
                                    let slots = present.swapchain.images().len() + 1;
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
                                // One-time setup AFTER the swapchain depth is
                                // final and BEFORE frame 0, so pipeline
                                // compilation stays off the presentation path.
                                if let Some(setup) = setup.take() {
                                    let mut setup_ctx = RenderContext {
                                        state: &mut s,
                                        config: &mut vse_config,
                                    };
                                    if let Err(e) = setup(&mut setup_ctx) {
                                        *error_clone.borrow_mut() = Some(e);
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
                                    s.target.present_expect_mut().minimized = true;
                                } else {
                                    let present = s.target.present_expect_mut();
                                    present.minimized = false;
                                    present.display_size = (new_size.width, new_size.height);
                                    present.swapchain.mark_needs_recreation();
                                }
                            }
                            WindowEvent::KeyboardInput { .. }
                            | WindowEvent::CursorMoved { .. }
                            | WindowEvent::MouseInput { .. }
                            | WindowEvent::MouseWheel { .. } => {
                                s.handle_winit_input(&window_event);
                            }
                            WindowEvent::RedrawRequested => {
                                if s.target.present_expect().minimized {
                                    return;
                                }

                                // ── Phase 1: Check for confirmed presentation ──────────
                                let oldest_complete = s
                                    .target
                                    .present_expect()
                                    .buffered_pending_frames
                                    .front()
                                    .is_some_and(|frame| frame.completion.is_complete());

                                if oldest_complete {
                                    let pending = s
                                        .target
                                        .present_expect_mut()
                                        .buffered_pending_frames
                                        .pop_front()
                                        .expect("oldest buffered frame was just observed");
                                    if let Err(e) = Self::deliver_buffered_frame(
                                        s,
                                        &mut vse_config,
                                        pending,
                                        &mut callback,
                                    ) {
                                        *error_clone.borrow_mut() = Some(e);
                                        elwt.exit();
                                        return;
                                    }
                                }

                                // Early exit if callback requested close during Presented
                                if s.should_close {
                                    if let Err(e) =
                                        Self::drain_buffered(s, &mut vse_config, &mut callback)
                                    {
                                        *error_clone.borrow_mut() = Some(e);
                                    }
                                    if let Some(recording) = &mut s.recording {
                                        recording.on_shutdown();
                                    }
                                    s.target.present_expect_mut().in_buffered_mode = false;
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
                                }

                                s.input.begin_frame();

                                if s.should_close {
                                    if let Err(e) =
                                        Self::drain_buffered(s, &mut vse_config, &mut callback)
                                    {
                                        *error_clone.borrow_mut() = Some(e);
                                    }
                                    if let Some(recording) = &mut s.recording {
                                        recording.on_shutdown();
                                    }
                                    s.target.present_expect_mut().in_buffered_mode = false;
                                    elwt.exit();
                                }
                            }
                            _ => {}
                        }
                    }
                    Event::AboutToWait => {
                        if let Some(s) = &state {
                            if let Some(w) = &s.target.present_expect().window {
                                w.request_redraw();
                            }
                        }
                    }
                    Event::LoopExiting => {
                        if let Some(s) = &mut state {
                            s.target.present_expect_mut().in_buffered_mode = false;
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

    /// Confirm one submitted frame, dispatch its `Presented` event, and finalize recording.
    fn deliver_buffered_frame<T, F>(
        state: &mut VSEState,
        config: &mut VSEConfig,
        pending: crate::core::buffered::PendingFrame<Box<dyn std::any::Any + Send + 'static>>,
        callback: &mut F,
    ) -> Result<(), VSEError>
    where
        T: std::any::Any + serde::Serialize + Send + 'static,
        F: FnMut(FlipEvent<T>, &mut RenderContext<'_>) -> Result<(), VSEError>,
    {
        pending.completion.wait_blocking();
        let payload = *pending
            .payload
            .downcast::<T>()
            .expect("buffered payload type mismatch");
        let clock = &state.clock;
        let confirmed = state
            .target
            .present_expect_mut()
            .build_confirmed_flip(clock, pending.estimated_flip);
        state.target.present_expect_mut().buffered_confirmed_flip = Some(confirmed.clone());
        if let Some(recording) = &mut state.recording {
            recording.on_flip(confirmed.clone());
        }

        let callback_result = {
            let mut render_ctx = RenderContext { state, config };
            callback(
                FlipEvent::Presented {
                    flip_info: confirmed,
                    payload,
                },
                &mut render_ctx,
            )
        };

        state.target.present_expect_mut().buffered_confirmed_flip = None;
        let recording_result = state
            .recording
            .as_mut()
            .map(|recording| recording.finish_flip())
            .transpose()
            .map_err(|e| VSEError::DataRecording(e.to_string()));

        callback_result?;
        recording_result?;
        Ok(())
    }

    /// Drain all remaining in-flight frames and fire `Presented` events.
    ///
    /// Called on clean shutdown from within `run_buffered()`. All frames are
    /// retired even if a callback fails; the first error is returned afterward.
    fn drain_buffered<T, F>(
        state: &mut VSEState,
        config: &mut VSEConfig,
        callback: &mut F,
    ) -> Result<(), VSEError>
    where
        T: std::any::Any + serde::Serialize + Send + 'static,
        F: FnMut(FlipEvent<T>, &mut RenderContext<'_>) -> Result<(), VSEError>,
    {
        let mut first_error = None;
        while let Some(pending) = state
            .target
            .present_expect_mut()
            .buffered_pending_frames
            .pop_front()
        {
            if let Err(e) = Self::deliver_buffered_frame(state, config, pending, callback) {
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }

        first_error.map_or(Ok(()), Err)
    }
}
