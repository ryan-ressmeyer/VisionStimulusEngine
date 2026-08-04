//! Winit event loops for synchronous and buffered presentation.

use std::cell::RefCell;
use std::rc::Rc;

use tracing::{debug, info, warn};
use winit::event::{Event, WindowEvent};
use winit::event_loop::ControlFlow;

use super::buffered::BufferedConfig;
use super::config::{DisplayedConfig, VSEError};
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
                                        config: &mut config.render,
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
                                    config: &mut config.render,
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

    /// Run a stateless buffered experiment.
    ///
    /// `render` builds one frame and returns its target and payload. VSE submits
    /// that frame after the callback returns. `confirm` later receives the same
    /// payload with confirmed presentation timing.
    pub fn run_buffered<T, R, C>(
        self,
        config: BufferedConfig,
        mut render: R,
        mut confirm: C,
    ) -> Result<(), VSEError>
    where
        T: 'static,
        R: FnMut(&mut RenderContext<'_>) -> Result<super::buffered::BufferedFrame<T>, VSEError>
            + 'static,
        C: FnMut(
                super::buffered::ConfirmedFrame<T>,
                &mut RenderContext<'_>,
            ) -> Result<(), VSEError>
            + 'static,
    {
        self.run_buffered_with_state(
            config,
            |_ctx| Ok(()),
            move |(), ctx| render(ctx),
            move |(), confirmed, ctx| confirm(confirmed, ctx),
        )
    }

    /// Run a buffered experiment with state shared by rendering and confirmation.
    ///
    /// `initialize` runs once after the GPU and final buffered swapchain exist,
    /// but before frame zero. Its result is passed to both frame callbacks.
    pub fn run_buffered_with_state<S, T, I, R, C>(
        mut self,
        config: BufferedConfig,
        initialize: I,
        mut render: R,
        mut confirm: C,
    ) -> Result<(), VSEError>
    where
        S: 'static,
        T: 'static,
        I: FnOnce(&mut RenderContext<'_>) -> Result<S, VSEError> + 'static,
        R: FnMut(
                &mut S,
                &mut RenderContext<'_>,
            ) -> Result<super::buffered::BufferedFrame<T>, VSEError>
            + 'static,
        C: FnMut(
                &mut S,
                super::buffered::ConfirmedFrame<T>,
                &mut RenderContext<'_>,
            ) -> Result<(), VSEError>
            + 'static,
    {
        #[cfg(target_os = "linux")]
        if self.config.window_mode == WindowMode::DirectDisplay {
            return Err(VSEError::EventLoop(
                "buffered presentation does not support DirectDisplay mode".into(),
            ));
        }

        let event_loop = self
            .event_loop
            .take()
            .ok_or_else(|| VSEError::EventLoop("Event loop already consumed".into()))?;

        let mut vse_config = self.config;
        let mut session = self.session;
        let mut state: Option<VSEState> = None;
        let mut experiment_state: Option<S> = None;
        let mut initialize = Some(initialize);
        let mut pending_frames =
            std::collections::VecDeque::<crate::core::buffered::PendingFrame<T>>::new();

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

                                let mut init_ctx = RenderContext {
                                    state: &mut s,
                                    config: &mut vse_config.render,
                                };
                                let init = initialize
                                    .take()
                                    .expect("buffered initialization runs exactly once");
                                match init(&mut init_ctx) {
                                    Ok(value) => experiment_state = Some(value),
                                    Err(e) => {
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
                                let experiment = experiment_state
                                    .as_mut()
                                    .expect("buffered state initialized before redraw");

                                let oldest_complete = pending_frames
                                    .front()
                                    .is_some_and(|frame| frame.submission.completion.is_complete());
                                if oldest_complete {
                                    let pending = pending_frames
                                        .pop_front()
                                        .expect("oldest buffered frame was just observed");
                                    if let Err(e) = Self::deliver_buffered_frame(
                                        s,
                                        &mut vse_config,
                                        experiment,
                                        pending,
                                        &mut confirm,
                                    ) {
                                        *error_clone.borrow_mut() = Some(e);
                                        if let Err(drain_error) = Self::drain_buffered(
                                            s,
                                            &mut vse_config,
                                            experiment,
                                            &mut pending_frames,
                                            &mut confirm,
                                        ) {
                                            if error_clone.borrow().is_none() {
                                                *error_clone.borrow_mut() = Some(drain_error);
                                            }
                                        }
                                        elwt.exit();
                                        return;
                                    }
                                }

                                if s.should_close {
                                    if let Err(e) = Self::drain_buffered(
                                        s,
                                        &mut vse_config,
                                        experiment,
                                        &mut pending_frames,
                                        &mut confirm,
                                    ) {
                                        *error_clone.borrow_mut() = Some(e);
                                    }
                                    if let Some(recording) = &mut s.recording {
                                        recording.on_shutdown();
                                    }
                                    s.target.present_expect_mut().in_buffered_mode = false;
                                    elwt.exit();
                                    return;
                                }

                                let frame = {
                                    let mut render_ctx = RenderContext {
                                        state: s,
                                        config: &mut vse_config.render,
                                    };
                                    match render(experiment, &mut render_ctx) {
                                        Ok(frame) => frame,
                                        Err(e) => {
                                            *error_clone.borrow_mut() = Some(e);
                                            if let Err(drain_error) = Self::drain_buffered(
                                                s,
                                                &mut vse_config,
                                                experiment,
                                                &mut pending_frames,
                                                &mut confirm,
                                            ) {
                                                if error_clone.borrow().is_none() {
                                                    *error_clone.borrow_mut() = Some(drain_error);
                                                }
                                            }
                                            elwt.exit();
                                            return;
                                        }
                                    }
                                };

                                let submit_result = {
                                    let mut render_ctx = RenderContext {
                                        state: s,
                                        config: &mut vse_config.render,
                                    };
                                    render_ctx.submit_buffered_frame(frame)
                                };
                                match submit_result {
                                    Ok(Some(pending)) => pending_frames.push_back(pending),
                                    Ok(None) => {}
                                    Err(e) => {
                                        *error_clone.borrow_mut() = Some(e);
                                        if let Err(drain_error) = Self::drain_buffered(
                                            s,
                                            &mut vse_config,
                                            experiment,
                                            &mut pending_frames,
                                            &mut confirm,
                                        ) {
                                            if error_clone.borrow().is_none() {
                                                *error_clone.borrow_mut() = Some(drain_error);
                                            }
                                        }
                                        elwt.exit();
                                        return;
                                    }
                                }

                                s.input.begin_frame();
                                if s.should_close {
                                    if let Err(e) = Self::drain_buffered(
                                        s,
                                        &mut vse_config,
                                        experiment,
                                        &mut pending_frames,
                                        &mut confirm,
                                    ) {
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
                        if let (Some(s), Some(experiment)) = (&mut state, experiment_state.as_mut())
                        {
                            if let Err(e) = Self::drain_buffered(
                                s,
                                &mut vse_config,
                                experiment,
                                &mut pending_frames,
                                &mut confirm,
                            ) {
                                if error_clone.borrow().is_none() {
                                    *error_clone.borrow_mut() = Some(e);
                                }
                            }
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

    fn deliver_buffered_frame<S, T, C>(
        state: &mut VSEState,
        config: &mut DisplayedConfig,
        experiment: &mut S,
        pending: crate::core::buffered::PendingFrame<T>,
        confirm: &mut C,
    ) -> Result<(), VSEError>
    where
        C: FnMut(
            &mut S,
            super::buffered::ConfirmedFrame<T>,
            &mut RenderContext<'_>,
        ) -> Result<(), VSEError>,
    {
        pending.submission.completion.wait_blocking();
        let clock = &state.clock;
        let confirmed = state
            .target
            .present_expect_mut()
            .build_confirmed_flip(clock, pending.submission.estimated_flip);
        state.target.present_expect_mut().buffered_confirmed_flip = Some(confirmed.clone());
        if let Some(recording) = &mut state.recording {
            recording.on_flip(confirmed.clone());
        }

        let callback_result = {
            let mut render_ctx = RenderContext {
                state,
                config: &mut config.render,
            };
            confirm(
                experiment,
                super::buffered::ConfirmedFrame {
                    flip_info: confirmed,
                    payload: pending.payload,
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

    fn drain_buffered<S, T, C>(
        state: &mut VSEState,
        config: &mut DisplayedConfig,
        experiment: &mut S,
        pending_frames: &mut std::collections::VecDeque<crate::core::buffered::PendingFrame<T>>,
        confirm: &mut C,
    ) -> Result<(), VSEError>
    where
        C: FnMut(
            &mut S,
            super::buffered::ConfirmedFrame<T>,
            &mut RenderContext<'_>,
        ) -> Result<(), VSEError>,
    {
        let mut first_error = None;
        while let Some(pending) = pending_frames.pop_front() {
            if let Err(e) =
                Self::deliver_buffered_frame(state, config, experiment, pending, confirm)
            {
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}
