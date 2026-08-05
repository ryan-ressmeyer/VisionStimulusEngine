use super::*;

impl<'a> RenderContext<'a> {
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
        let Some(present) = self.state.target.present_mut() else {
            return;
        };
        present.cursor_visible = visible;
        if let Some(w) = &present.window {
            w.set_cursor_visible(visible);
        }
    }

    /// Move the cursor to the specified position (logical pixels).
    pub fn set_cursor_position(&self, x: f64, y: f64) {
        if let Some(w) = self.state.target.present().and_then(|p| p.window.as_ref()) {
            let _ = w.set_cursor_position(LogicalPosition::new(x, y));
        }
    }

    /// Returns whether the cursor is currently visible.
    pub fn cursor_visible(&self) -> bool {
        self.state
            .target
            .present()
            .is_some_and(|p| p.cursor_visible)
    }

    // === Display backend detection ===

    /// Detect the display backend (windowing system) used for this session.
    ///
    /// Derived from the raw window handle type. Use this to warn users when
    /// running under X11/XWayland. Backend names describe the presentation path; characterize
    /// timing on the active path rather than assigning a fixed jitter ranking here.
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
        let Some(present) = self.state.target.present() else {
            return DisplayBackend::Unknown;
        };
        if let Some(method) = present.acquired_display {
            return DisplayBackend::DirectDisplay { method };
        }

        // Compositor mode: detect from raw window handle
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Some(window) = &present.window {
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
        let window = match self.state.target.present().and_then(|p| p.window.as_ref()) {
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
            .target
            .present()?
            .window
            .as_ref()?
            .primary_monitor()
            .map(|handle| monitor_handle_to_info(0, &handle))
    }

    /// Get all video modes for a monitor by index.
    pub fn video_modes(&self, monitor_index: usize) -> Vec<VideoModeInfo> {
        let window = match self.state.target.present().and_then(|p| p.window.as_ref()) {
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
        let window = match self.state.target.present().and_then(|p| p.window.as_ref()) {
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
    /// The window display mode. Reports [`WindowMode::Windowed`] in a headless
    /// session, which has no window at all.
    pub fn window_mode(&self) -> WindowMode {
        self.state
            .target
            .present()
            .map(|p| p.window_mode)
            .unwrap_or_default()
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
        let Some(present) = self.state.target.present_mut() else {
            warn!("set_window_mode() has no effect in a headless session");
            return;
        };
        if let Some(w) = &present.window {
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
            if !present.cursor_visibility_overridden {
                let visible = matches!(mode, WindowMode::Windowed);
                present.cursor_visible = visible;
                w.set_cursor_visible(visible);
            }
        } else {
            warn!("set_window_mode() has no effect in DirectDisplay mode");
        }
        present.window_mode = mode;
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
