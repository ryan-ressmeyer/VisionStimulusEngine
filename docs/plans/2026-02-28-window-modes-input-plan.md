# Window Modes, Input Handling & Monitor Selection — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add fullscreen window modes (exclusive + borderless), keyboard/mouse input (polled + event queue), monitor selection, cursor control, and video mode enumeration to VSE.

**Architecture:** All new capabilities are added directly to the existing `VSEContextBuilder` (for configuration) and `RenderContext` (for runtime access). Input state is captured internally by expanding the event loop's `WindowEvent` match arms into a new `InputState` struct. New public types (`WindowMode`, `MonitorSelection`, `MonitorInfo`, `VideoModeInfo`, `MouseButton`, `InputEvent`) live in a new `src/core/input.rs` module.

**Tech Stack:** winit 0.29 (existing dep — `Fullscreen`, `MonitorHandle`, `VideoMode`, `KeyEvent`, `MouseButton`, cursor APIs), existing Vulkan/Timestamp infrastructure.

---

### Task 1: Add InputState struct and InputEvent types

**Files:**
- Create: `src/core/input.rs`
- Modify: `src/core/mod.rs:7-16`

**Step 1: Write the failing test**

Add to `tests/core_tests.rs`:

```rust
use vision_stimulus_engine::core::{
    InputEvent, MonitorInfo, MonitorSelection, MouseButton, VideoModeInfo, WindowMode,
};

#[test]
fn test_window_mode_default() {
    let mode = WindowMode::default();
    assert!(matches!(mode, WindowMode::Windowed));
}

#[test]
fn test_monitor_selection_default() {
    let sel = MonitorSelection::default();
    assert!(matches!(sel, MonitorSelection::Primary));
}

#[test]
fn test_mouse_button_variants() {
    let left = MouseButton::Left;
    let right = MouseButton::Right;
    let middle = MouseButton::Middle;
    let other = MouseButton::Other(4);
    // Ensure they're distinct via Debug
    assert_ne!(format!("{:?}", left), format!("{:?}", right));
    assert_ne!(format!("{:?}", middle), format!("{:?}", other));
}

#[test]
fn test_video_mode_info_fields() {
    let mode = VideoModeInfo {
        width: 1920,
        height: 1080,
        refresh_rate_hz: 144.0,
        bit_depth: 32,
    };
    assert_eq!(mode.width, 1920);
    assert_eq!(mode.refresh_rate_hz, 144.0);
}

#[test]
fn test_monitor_info_fields() {
    let info = MonitorInfo {
        name: Some("Test Monitor".into()),
        index: 0,
        width: 2560,
        height: 1440,
        refresh_rate_hz: Some(165.0),
        scale_factor: 1.0,
        position: (0, 0),
        video_modes: vec![],
    };
    assert_eq!(info.name.as_deref(), Some("Test Monitor"));
    assert_eq!(info.width, 2560);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test test_window_mode_default test_monitor_selection_default test_mouse_button_variants test_video_mode_info_fields test_monitor_info_fields -- --no-capture`
Expected: FAIL — types don't exist yet.

**Step 3: Create `src/core/input.rs` with all public types**

```rust
//! Input handling, window modes, and monitor information types.

use crate::timing::Timestamp;
use std::collections::HashSet;
pub use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

/// How the window should be displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowMode {
    /// Standard resizable window (default).
    Windowed,
    /// Borderless window covering the entire monitor.
    /// The OS compositor remains active — adds latency.
    BorderlessFullscreen,
    /// Exclusive fullscreen — bypasses the OS compositor.
    /// Lowest latency, guaranteed vsync ownership.
    /// Falls back to `BorderlessFullscreen` on Wayland.
    ExclusiveFullscreen,
}

impl Default for WindowMode {
    fn default() -> Self {
        Self::Windowed
    }
}

/// Which monitor to use for fullscreen modes.
#[derive(Debug, Clone)]
pub enum MonitorSelection {
    /// Use the primary monitor (default).
    Primary,
    /// Select by index (0-based, from available monitors list).
    Index(usize),
    /// Select by name substring match (e.g., "ASUS" matches "ASUS VG279Q").
    Name(String),
}

impl Default for MonitorSelection {
    fn default() -> Self {
        Self::Primary
    }
}

/// A supported video mode for a monitor.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoModeInfo {
    pub width: u32,
    pub height: u32,
    pub refresh_rate_hz: f64,
    pub bit_depth: u16,
}

/// Information about a connected monitor.
#[derive(Debug, Clone)]
pub struct MonitorInfo {
    pub name: Option<String>,
    pub index: usize,
    pub width: u32,
    pub height: u32,
    pub refresh_rate_hz: Option<f64>,
    pub scale_factor: f64,
    pub position: (i32, i32),
    pub video_modes: Vec<VideoModeInfo>,
}

/// Mouse button identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Other(u16),
}

impl From<winit::event::MouseButton> for MouseButton {
    fn from(btn: winit::event::MouseButton) -> Self {
        match btn {
            winit::event::MouseButton::Left => MouseButton::Left,
            winit::event::MouseButton::Right => MouseButton::Right,
            winit::event::MouseButton::Middle => MouseButton::Middle,
            winit::event::MouseButton::Other(id) => MouseButton::Other(id),
            _ => MouseButton::Other(0),
        }
    }
}

/// An input event with a timestamp for precise timing measurement.
///
/// Events are collected between `flip()` calls and accessible via
/// `RenderContext::input_events()`. Timestamps use the VSE `Clock`,
/// making them directly comparable to `FlipInfo` timestamps for
/// reaction time computation.
#[derive(Debug, Clone)]
pub enum InputEvent {
    KeyDown {
        key_code: KeyCode,
        logical_key: Key<'static>,
        timestamp: Timestamp,
        repeat: bool,
    },
    KeyUp {
        key_code: KeyCode,
        logical_key: Key<'static>,
        timestamp: Timestamp,
    },
    MouseMove {
        x: f64,
        y: f64,
        timestamp: Timestamp,
    },
    MouseDown {
        button: MouseButton,
        x: f64,
        y: f64,
        timestamp: Timestamp,
    },
    MouseUp {
        button: MouseButton,
        x: f64,
        y: f64,
        timestamp: Timestamp,
    },
    MouseWheel {
        delta_x: f64,
        delta_y: f64,
        timestamp: Timestamp,
    },
}

/// Internal input state tracker.
///
/// Captures all input events from the winit event loop and provides
/// both polled (frame-aligned) and event-queue access patterns.
pub(crate) struct InputState {
    /// Keys currently held down.
    pub(crate) keys_down: HashSet<KeyCode>,
    /// Keys pressed this frame (cleared each frame).
    pub(crate) keys_just_pressed: HashSet<KeyCode>,
    /// Keys released this frame (cleared each frame).
    pub(crate) keys_just_released: HashSet<KeyCode>,
    /// Current mouse position (window-relative pixels).
    pub(crate) mouse_position: (f64, f64),
    /// Mouse buttons currently held down.
    pub(crate) buttons_down: HashSet<MouseButton>,
    /// Mouse buttons pressed this frame (cleared each frame).
    pub(crate) buttons_just_pressed: HashSet<MouseButton>,
    /// Event queue — all events since last flip().
    pub(crate) events: Vec<InputEvent>,
}

impl InputState {
    pub(crate) fn new() -> Self {
        Self {
            keys_down: HashSet::new(),
            keys_just_pressed: HashSet::new(),
            keys_just_released: HashSet::new(),
            mouse_position: (0.0, 0.0),
            buttons_down: HashSet::new(),
            buttons_just_pressed: HashSet::new(),
            events: Vec::new(),
        }
    }

    /// Clear per-frame state. Called at the start of each frame
    /// (before processing new events for that frame).
    pub(crate) fn begin_frame(&mut self) {
        self.keys_just_pressed.clear();
        self.keys_just_released.clear();
        self.buttons_just_pressed.clear();
    }

    /// Clear the event queue. Called on flip().
    pub(crate) fn clear_events(&mut self) {
        self.events.clear();
    }
}
```

**Step 4: Register the module in `src/core/mod.rs`**

Add `mod input;` and export the public types:

```rust
mod input;

pub use input::{
    InputEvent, Key, KeyCode, MonitorInfo, MonitorSelection, MouseButton, NamedKey, PhysicalKey,
    VideoModeInfo, WindowMode,
};
```

**Step 5: Run tests to verify they pass**

Run: `cargo test test_window_mode_default test_monitor_selection_default test_mouse_button_variants test_video_mode_info_fields test_monitor_info_fields`
Expected: PASS

**Step 6: Commit**

```bash
git add src/core/input.rs src/core/mod.rs tests/core_tests.rs
git commit -m "feat: add input types, window mode, and monitor selection enums"
```

---

### Task 2: Add window mode and monitor config to builder

**Files:**
- Modify: `src/core/context.rs:66-103` (VSEConfig)
- Modify: `src/core/context.rs:124-197` (VSEContextBuilder)

**Step 1: Write the failing test**

Add to `tests/core_tests.rs`:

```rust
use vision_stimulus_engine::core::{WindowMode, MonitorSelection};

#[test]
fn test_builder_with_window_mode() {
    let _builder = VSEContext::builder()
        .with_window_mode(WindowMode::ExclusiveFullscreen);
}

#[test]
fn test_builder_with_monitor() {
    let _builder = VSEContext::builder()
        .with_monitor(MonitorSelection::Index(1));
}

#[test]
fn test_builder_with_cursor_visible() {
    let _builder = VSEContext::builder()
        .with_cursor_visible(false);
}

#[test]
fn test_builder_fullscreen_chain() {
    let _builder = VSEContext::builder()
        .with_window_mode(WindowMode::BorderlessFullscreen)
        .with_monitor(MonitorSelection::Name("ASUS".into()))
        .with_cursor_visible(false)
        .with_window_size(1920, 1080);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test test_builder_with_window_mode test_builder_with_monitor test_builder_with_cursor_visible test_builder_fullscreen_chain`
Expected: FAIL — methods don't exist.

**Step 3: Add fields to VSEConfig**

In `src/core/context.rs`, add to the `VSEConfig` struct (after line 86):

```rust
    /// Window display mode (windowed, borderless fullscreen, exclusive fullscreen).
    pub window_mode: WindowMode,
    /// Which monitor to use for fullscreen modes.
    pub monitor_selection: MonitorSelection,
    /// Cursor visibility override. None = auto (hidden in fullscreen, visible in windowed).
    pub cursor_visible: Option<bool>,
```

Update `Default for VSEConfig` to include:

```rust
    window_mode: WindowMode::default(),
    monitor_selection: MonitorSelection::default(),
    cursor_visible: None,
```

Add the necessary import at the top of context.rs:

```rust
use super::input::{InputState, MonitorInfo, MonitorSelection, VideoModeInfo, WindowMode};
```

**Step 4: Add builder methods to VSEContextBuilder**

In `src/core/context.rs`, add after the `with_expected_refresh_rate` method (after line 197):

```rust
    /// Set window display mode.
    ///
    /// - `Windowed`: Standard resizable window (default)
    /// - `BorderlessFullscreen`: Borderless window covering monitor (compositor active)
    /// - `ExclusiveFullscreen`: True fullscreen bypassing compositor (lowest latency)
    pub fn with_window_mode(mut self, mode: WindowMode) -> Self {
        self.config.window_mode = mode;
        self
    }

    /// Select which monitor to use for fullscreen modes.
    ///
    /// Ignored when `window_mode` is `Windowed`.
    pub fn with_monitor(mut self, selection: MonitorSelection) -> Self {
        self.config.monitor_selection = selection;
        self
    }

    /// Override automatic cursor visibility.
    ///
    /// By default, the cursor is hidden in fullscreen modes and visible
    /// in windowed mode. Use this to override that behavior.
    pub fn with_cursor_visible(mut self, visible: bool) -> Self {
        self.config.cursor_visible = Some(visible);
        self
    }
```

**Step 5: Run tests to verify they pass**

Run: `cargo test test_builder_with_window_mode test_builder_with_monitor test_builder_with_cursor_visible test_builder_fullscreen_chain`
Expected: PASS

**Step 6: Run full test suite + clippy**

Run: `cargo test && cargo clippy --all-targets`
Expected: PASS (no regressions)

**Step 7: Commit**

```bash
git add src/core/context.rs
git commit -m "feat: add window mode, monitor selection, and cursor config to builder"
```

---

### Task 3: Wire fullscreen and monitor selection into window creation

**Files:**
- Modify: `src/core/context.rs:300-381` (initialize method)

**Step 1: Implement monitor resolution and fullscreen in `initialize()`**

Replace the `WindowBuilder` section in `initialize()` (lines 304-308) with:

```rust
        // Resolve target monitor for fullscreen modes
        let target_monitor = match config.window_mode {
            WindowMode::Windowed => None,
            _ => {
                let monitor = match &config.monitor_selection {
                    MonitorSelection::Primary => elwt.primary_monitor(),
                    MonitorSelection::Index(idx) => {
                        elwt.available_monitors().nth(*idx)
                    }
                    MonitorSelection::Name(name) => {
                        elwt.available_monitors().find(|m| {
                            m.name()
                                .map(|n| n.to_lowercase().contains(&name.to_lowercase()))
                                .unwrap_or(false)
                        })
                    }
                };
                // Fall back to primary if specified monitor not found
                Some(monitor.or_else(|| elwt.primary_monitor()).ok_or_else(|| {
                    VSEError::Window("No monitors available".into())
                })?)
            }
        };

        // Build fullscreen setting
        let fullscreen = match config.window_mode {
            WindowMode::Windowed => None,
            WindowMode::BorderlessFullscreen => {
                Some(winit::window::Fullscreen::Borderless(target_monitor))
            }
            WindowMode::ExclusiveFullscreen => {
                if let Some(ref monitor) = target_monitor {
                    // Find best video mode: match configured resolution, highest refresh rate
                    let best_mode = monitor
                        .video_modes()
                        .filter(|m| {
                            m.size().width == config.window_width
                                && m.size().height == config.window_height
                        })
                        .max_by_key(|m| m.refresh_rate_millihertz())
                        .or_else(|| {
                            // Fall back to native resolution, highest refresh
                            monitor
                                .video_modes()
                                .max_by_key(|m| m.refresh_rate_millihertz())
                        });

                    match best_mode {
                        Some(mode) => Some(winit::window::Fullscreen::Exclusive(mode)),
                        None => {
                            warn!("No video modes available, falling back to borderless fullscreen");
                            Some(winit::window::Fullscreen::Borderless(target_monitor))
                        }
                    }
                } else {
                    Some(winit::window::Fullscreen::Borderless(None))
                }
            }
        };

        let window = WindowBuilder::new()
            .with_title(&config.window_title)
            .with_inner_size(PhysicalSize::new(config.window_width, config.window_height))
            .with_fullscreen(fullscreen)
            .build(elwt)
            .map_err(|e| VSEError::Window(e.to_string()))?;

        let window = Arc::new(window);

        // Apply cursor visibility
        let cursor_visible = config.cursor_visible.unwrap_or_else(|| {
            matches!(config.window_mode, WindowMode::Windowed)
        });
        window.set_cursor_visible(cursor_visible);
```

**Step 2: Run full test suite + clippy**

Run: `cargo test && cargo clippy --all-targets`
Expected: PASS

**Step 3: Commit**

```bash
git add src/core/context.rs
git commit -m "feat: wire fullscreen mode and monitor selection into window creation"
```

---

### Task 4: Wire InputState into event loop and RenderContext

**Files:**
- Modify: `src/core/context.rs:229-248` (VSEState)
- Modify: `src/core/context.rs:410-480` (event loop)
- Modify: `src/core/context.rs:496-499` (RenderContext)
- Modify: `src/core/context.rs:526-669` (flip method)

**Step 1: Add InputState to VSEState**

Add to `VSEState` struct (after line 247):

```rust
    input: InputState,
```

Initialize in the `Ok(VSEState { ... })` block (after line 379):

```rust
    input: InputState::new(),
```

**Step 2: Expand event loop to capture input events**

In the event loop's `WindowEvent` match (around line 470, replacing the `_ => {}` catch-all), add new arms before the catch-all:

```rust
                            WindowEvent::KeyboardInput { event, .. } => {
                                let now = s.clock.now();
                                let key_code = match event.physical_key {
                                    winit::keyboard::PhysicalKey::Code(code) => code,
                                    _ => return,
                                };
                                match event.state {
                                    winit::event::ElementState::Pressed => {
                                        if !event.repeat {
                                            s.input.keys_just_pressed.insert(key_code);
                                        }
                                        s.input.keys_down.insert(key_code);
                                        s.input.events.push(InputEvent::KeyDown {
                                            key_code,
                                            logical_key: event.logical_key.to_owned(),
                                            timestamp: now,
                                            repeat: event.repeat,
                                        });
                                    }
                                    winit::event::ElementState::Released => {
                                        s.input.keys_down.remove(&key_code);
                                        s.input.keys_just_released.insert(key_code);
                                        s.input.events.push(InputEvent::KeyUp {
                                            key_code,
                                            logical_key: event.logical_key.to_owned(),
                                            timestamp: now,
                                        });
                                    }
                                }
                            }
                            WindowEvent::CursorMoved { position, .. } => {
                                let now = s.clock.now();
                                s.input.mouse_position = (position.x, position.y);
                                s.input.events.push(InputEvent::MouseMove {
                                    x: position.x,
                                    y: position.y,
                                    timestamp: now,
                                });
                            }
                            WindowEvent::MouseInput { state: btn_state, button, .. } => {
                                let now = s.clock.now();
                                let btn = MouseButton::from(button);
                                let (mx, my) = s.input.mouse_position;
                                match btn_state {
                                    winit::event::ElementState::Pressed => {
                                        s.input.buttons_down.insert(btn);
                                        s.input.buttons_just_pressed.insert(btn);
                                        s.input.events.push(InputEvent::MouseDown {
                                            button: btn,
                                            x: mx,
                                            y: my,
                                            timestamp: now,
                                        });
                                    }
                                    winit::event::ElementState::Released => {
                                        s.input.buttons_down.remove(&btn);
                                        s.input.events.push(InputEvent::MouseUp {
                                            button: btn,
                                            x: mx,
                                            y: my,
                                            timestamp: now,
                                        });
                                    }
                                }
                            }
                            WindowEvent::MouseWheel { delta, .. } => {
                                let now = s.clock.now();
                                let (dx, dy) = match delta {
                                    winit::event::MouseScrollDelta::LineDelta(x, y) => {
                                        (x as f64, y as f64)
                                    }
                                    winit::event::MouseScrollDelta::PixelDelta(pos) => {
                                        (pos.x, pos.y)
                                    }
                                };
                                s.input.events.push(InputEvent::MouseWheel {
                                    delta_x: dx,
                                    delta_y: dy,
                                    timestamp: now,
                                });
                            }
```

**Step 3: Clear per-frame input at the start of RedrawRequested**

In the `WindowEvent::RedrawRequested` arm, add before the `RenderContext` creation (before line 459):

```rust
                                s.input.begin_frame();
```

**Step 4: Clear event queue in flip()**

In the `flip()` method, add at the very end (before the `Ok(flip_info)` return, after line 666):

```rust
        // Clear input event queue for next frame
        self.state.input.clear_events();
```

**Step 5: Add import for InputEvent and MouseButton at top of context.rs**

Update the import from `super::input` to also include `InputEvent` and `MouseButton`:

```rust
use super::input::{InputEvent, InputState, MonitorInfo, MonitorSelection, MouseButton, VideoModeInfo, WindowMode};
```

**Step 6: Run full test suite + clippy**

Run: `cargo test && cargo clippy --all-targets`
Expected: PASS

**Step 7: Commit**

```bash
git add src/core/context.rs
git commit -m "feat: capture keyboard and mouse events in event loop"
```

---

### Task 5: Add polled input and event query methods to RenderContext

**Files:**
- Modify: `src/core/context.rs` (RenderContext impl block, after line 947)

**Step 1: Write the failing test**

Add to `tests/core_tests.rs`:

```rust
use vision_stimulus_engine::core::{InputEvent, KeyCode, MouseButton};

// These test that the API exists and compiles.
// Actual input behavior requires a running window (tested via examples).

#[test]
fn test_keycode_reexport() {
    let _key = KeyCode::Escape;
    let _space = KeyCode::Space;
    let _a = KeyCode::KeyA;
}

#[test]
fn test_mouse_button_equality() {
    assert_eq!(MouseButton::Left, MouseButton::Left);
    assert_ne!(MouseButton::Left, MouseButton::Right);
    assert_eq!(MouseButton::Other(5), MouseButton::Other(5));
    assert_ne!(MouseButton::Other(5), MouseButton::Other(6));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test test_keycode_reexport test_mouse_button_equality`
Expected: FAIL — `KeyCode` not exported from prelude yet.

**Step 3: Add input methods to RenderContext**

In `src/core/context.rs`, add to the `RenderContext` impl block (after the `capture_host_info` method):

```rust
    // === Input polling (frame-aligned) ===

    /// Check if a key is currently held down.
    ///
    /// Returns `true` if the key was pressed and has not been released.
    /// This is the equivalent of Psychtoolbox's `KbCheck`.
    pub fn key_pressed(&self, key: KeyCode) -> bool {
        self.state.input.keys_down.contains(&key)
    }

    /// Check if a key was pressed this frame (not held from a previous frame).
    ///
    /// Returns `true` only on the frame the key was first pressed.
    /// Useful for one-shot actions like "press Space to continue".
    pub fn key_just_pressed(&self, key: KeyCode) -> bool {
        self.state.input.keys_just_pressed.contains(&key)
    }

    /// Check if a key was released this frame.
    pub fn key_just_released(&self, key: KeyCode) -> bool {
        self.state.input.keys_just_released.contains(&key)
    }

    /// Get the current mouse cursor position in window-relative pixel coordinates.
    ///
    /// Returns `(x, y)` where (0, 0) is the top-left of the window.
    /// This is the equivalent of Psychtoolbox's `GetMouse`.
    pub fn mouse_position(&self) -> (f64, f64) {
        self.state.input.mouse_position
    }

    /// Check if a mouse button is currently held down.
    pub fn mouse_button_pressed(&self, button: MouseButton) -> bool {
        self.state.input.buttons_down.contains(&button)
    }

    /// Check if a mouse button was pressed this frame.
    pub fn mouse_button_just_pressed(&self, button: MouseButton) -> bool {
        self.state.input.buttons_just_pressed.contains(&button)
    }

    // === Event queue (timing-precise) ===

    /// Get all input events that occurred since the last `flip()` call.
    ///
    /// Each event carries a `Timestamp` from the VSE `Clock`, making it
    /// directly comparable to `FlipInfo` timestamps for reaction time
    /// computation.
    ///
    /// The queue is automatically cleared on each `flip()` call.
    pub fn input_events(&self) -> &[InputEvent] {
        &self.state.input.events
    }

    // === Cursor control ===

    /// Show or hide the mouse cursor.
    pub fn set_cursor_visible(&self, visible: bool) {
        self.state.window.set_cursor_visible(visible);
    }

    /// Set the mouse cursor position (warp the cursor).
    ///
    /// Coordinates are in window-relative pixels.
    pub fn set_cursor_position(&self, x: f64, y: f64) {
        use winit::dpi::LogicalPosition;
        let _ = self.state.window.set_cursor_position(LogicalPosition::new(x, y));
    }

    /// Check whether the cursor is currently visible.
    ///
    /// Note: Tracks the last value set via `set_cursor_visible()` or
    /// the auto-hide default, not the actual OS cursor state.
    pub fn cursor_visible(&self) -> bool {
        // We'll need to track this in VSEState — see step below
        self.state.cursor_visible
    }
```

**Step 4: Add `cursor_visible` field to VSEState**

Add to `VSEState` struct:

```rust
    cursor_visible: bool,
```

Initialize in the `Ok(VSEState { ... })` block, using the resolved cursor_visible value computed during window creation.

**Step 5: Update set_cursor_visible to also track state**

Modify `set_cursor_visible` to:

```rust
    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.state.cursor_visible = visible;
        self.state.window.set_cursor_visible(visible);
    }
```

(Change `&self` to `&mut self`.)

**Step 6: Add KeyCode and MouseButton imports to use statements at top of context.rs**

Update the import:

```rust
use super::input::{InputEvent, InputState, KeyCode, MonitorInfo, MonitorSelection, MouseButton, VideoModeInfo, WindowMode};
```

**Step 7: Run tests to verify they pass**

Run: `cargo test test_keycode_reexport test_mouse_button_equality`
Expected: PASS

**Step 8: Run full test suite + clippy**

Run: `cargo test && cargo clippy --all-targets`
Expected: PASS

**Step 9: Commit**

```bash
git add src/core/context.rs tests/core_tests.rs
git commit -m "feat: add polled input, event queue, and cursor control to RenderContext"
```

---

### Task 6: Add monitor and video mode query methods to RenderContext

**Files:**
- Modify: `src/core/context.rs` (RenderContext impl block)

**Step 1: Add monitor query methods to RenderContext**

```rust
    // === Monitor & video mode queries ===

    /// Get a list of all connected monitors.
    pub fn available_monitors(&self) -> Vec<MonitorInfo> {
        self.state
            .window
            .available_monitors()
            .enumerate()
            .map(|(index, handle)| {
                let size = handle.size();
                let video_modes = handle
                    .video_modes()
                    .map(|vm| VideoModeInfo {
                        width: vm.size().width,
                        height: vm.size().height,
                        refresh_rate_hz: vm.refresh_rate_millihertz() as f64 / 1000.0,
                        bit_depth: vm.bit_depth(),
                    })
                    .collect();
                MonitorInfo {
                    name: handle.name(),
                    index,
                    width: size.width,
                    height: size.height,
                    refresh_rate_hz: handle
                        .refresh_rate_millihertz()
                        .map(|mhz| mhz as f64 / 1000.0),
                    scale_factor: handle.scale_factor(),
                    position: {
                        let pos = handle.position();
                        (pos.x, pos.y)
                    },
                    video_modes,
                }
            })
            .collect()
    }

    /// Get the primary monitor, if one is designated by the OS.
    pub fn primary_monitor(&self) -> Option<MonitorInfo> {
        let handle = self.state.window.primary_monitor()?;
        let size = handle.size();
        let video_modes = handle
            .video_modes()
            .map(|vm| VideoModeInfo {
                width: vm.size().width,
                height: vm.size().height,
                refresh_rate_hz: vm.refresh_rate_millihertz() as f64 / 1000.0,
                bit_depth: vm.bit_depth(),
            })
            .collect();
        Some(MonitorInfo {
            name: handle.name(),
            index: 0,
            width: size.width,
            height: size.height,
            refresh_rate_hz: handle
                .refresh_rate_millihertz()
                .map(|mhz| mhz as f64 / 1000.0),
            scale_factor: handle.scale_factor(),
            position: {
                let pos = handle.position();
                (pos.x, pos.y)
            },
            video_modes,
        })
    }

    /// Get video modes supported by the monitor at the given index.
    pub fn video_modes(&self, monitor_index: usize) -> Vec<VideoModeInfo> {
        self.state
            .window
            .available_monitors()
            .nth(monitor_index)
            .map(|handle| {
                handle
                    .video_modes()
                    .map(|vm| VideoModeInfo {
                        width: vm.size().width,
                        height: vm.size().height,
                        refresh_rate_hz: vm.refresh_rate_millihertz() as f64 / 1000.0,
                        bit_depth: vm.bit_depth(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get video modes for the monitor the window is currently on.
    pub fn current_monitor_video_modes(&self) -> Vec<VideoModeInfo> {
        self.state
            .window
            .current_monitor()
            .map(|handle| {
                handle
                    .video_modes()
                    .map(|vm| VideoModeInfo {
                        width: vm.size().width,
                        height: vm.size().height,
                        refresh_rate_hz: vm.refresh_rate_millihertz() as f64 / 1000.0,
                        bit_depth: vm.bit_depth(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get the current window mode.
    pub fn window_mode(&self) -> WindowMode {
        self.state.window_mode
    }

    /// Change the window mode at runtime.
    ///
    /// Switches between windowed, borderless fullscreen, and exclusive fullscreen.
    pub fn set_window_mode(&mut self, mode: WindowMode) {
        let fullscreen = match mode {
            WindowMode::Windowed => None,
            WindowMode::BorderlessFullscreen => {
                Some(winit::window::Fullscreen::Borderless(None))
            }
            WindowMode::ExclusiveFullscreen => {
                // Use current monitor's best video mode
                if let Some(monitor) = self.state.window.current_monitor() {
                    let best_mode = monitor
                        .video_modes()
                        .max_by_key(|m| m.refresh_rate_millihertz());
                    match best_mode {
                        Some(mode) => Some(winit::window::Fullscreen::Exclusive(mode)),
                        None => Some(winit::window::Fullscreen::Borderless(None)),
                    }
                } else {
                    Some(winit::window::Fullscreen::Borderless(None))
                }
            }
        };
        self.state.window.set_fullscreen(fullscreen);
        self.state.window_mode = mode;

        // Auto-update cursor visibility
        if self.config.cursor_visible.is_none() {
            let visible = matches!(mode, WindowMode::Windowed);
            self.state.cursor_visible = visible;
            self.state.window.set_cursor_visible(visible);
        }
    }
```

**Step 2: Add `window_mode` field to VSEState**

```rust
    window_mode: WindowMode,
```

Initialize from `config.window_mode` in the `Ok(VSEState { ... })` block.

**Step 3: Run full test suite + clippy**

Run: `cargo test && cargo clippy --all-targets`
Expected: PASS

**Step 4: Commit**

```bash
git add src/core/context.rs
git commit -m "feat: add monitor queries, video modes, and runtime window mode switching"
```

---

### Task 7: Update prelude and public exports

**Files:**
- Modify: `src/lib.rs:27-37` (prelude)
- Modify: `src/core/mod.rs:12-16` (exports)

**Step 1: Update `src/core/mod.rs` exports**

Add the new input types to the public exports:

```rust
pub use input::{
    InputEvent, Key, KeyCode, MonitorInfo, MonitorSelection, MouseButton, NamedKey, PhysicalKey,
    VideoModeInfo, WindowMode,
};
```

**Step 2: Update `src/lib.rs` prelude**

Add the most commonly used input types to the prelude:

```rust
pub mod prelude {
    pub use crate::core::{
        DeviceSelector, Frame, GPUPreference, InputEvent, KeyCode, MonitorInfo,
        MonitorSelection, MouseButton, NamedKey, PresentMode, RenderContext,
        SwapchainConfig, SwapchainManager, VSEContext, VSEContextBuilder, VSEError,
        VideoModeInfo, WindowMode,
    };
    pub use crate::drawing::{
        Color, GaborParams, GratingParams, NoiseParams, NoiseType, TextureHandle, WaveType,
    };
    pub use crate::host::HostInfo;
    pub use crate::timing::{FlipInfo, FlipLogger, Timestamp, TimingSource, TimingStats};
}
```

**Step 3: Run full test suite + clippy + fmt**

Run: `cargo test && cargo clippy --all-targets && cargo fmt --check`
Expected: PASS

**Step 4: Commit**

```bash
git add src/lib.rs src/core/mod.rs
git commit -m "feat: export input and window types in prelude"
```

---

### Task 8: Create fullscreen + input example

**Files:**
- Create: `examples/07_fullscreen_input.rs`
- Modify: `Cargo.toml` (add example entry)

**Step 1: Write the example**

```rust
//! Fullscreen & Input Handling Example
//!
//! Demonstrates:
//! - Borderless fullscreen mode
//! - Keyboard input (press Escape to exit)
//! - Mouse position tracking and click detection
//! - Monitor and video mode enumeration
//!
//! # Running
//!
//! ```bash
//! cargo run --example 07_fullscreen_input
//! ```

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    println!("VisionStimulusEngine - Fullscreen & Input Example");
    println!("==================================================");
    println!();
    println!("Press Escape to exit.");
    println!("Click the mouse to see position and button info.");
    println!();

    // Create a borderless fullscreen context
    let context = VSEContext::builder()
        .with_window_mode(WindowMode::BorderlessFullscreen)
        .with_monitor(MonitorSelection::Primary)
        .with_clear_color(0.2, 0.2, 0.2, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .build()?;

    let mut frame_count: u64 = 0;

    context.run(move |vse| {
        // Print monitor info on first frame
        if frame_count == 0 {
            println!("\nConnected monitors:");
            for monitor in vse.available_monitors() {
                println!(
                    "  [{}] {} — {}x{} @ {:.0} Hz (scale: {:.1}x)",
                    monitor.index,
                    monitor.name.as_deref().unwrap_or("Unknown"),
                    monitor.width,
                    monitor.height,
                    monitor.refresh_rate_hz.unwrap_or(0.0),
                    monitor.scale_factor,
                );
                println!("    Video modes:");
                // Show unique resolution/refresh combos
                let mut seen = std::collections::HashSet::new();
                for mode in &monitor.video_modes {
                    let key = (mode.width, mode.height, (mode.refresh_rate_hz * 10.0) as u32);
                    if seen.insert(key) {
                        println!(
                            "      {}x{} @ {:.1} Hz ({}-bit)",
                            mode.width, mode.height, mode.refresh_rate_hz, mode.bit_depth
                        );
                    }
                }
            }
            println!();
        }

        // Check for Escape key
        if vse.key_just_pressed(KeyCode::Escape) {
            println!("Escape pressed — exiting.");
            return Err(VSEError::EventLoop("User requested exit".into()));
        }

        // Report mouse clicks
        if vse.mouse_button_just_pressed(MouseButton::Left) {
            let (mx, my) = vse.mouse_position();
            println!("Left click at ({:.0}, {:.0})", mx, my);
        }
        if vse.mouse_button_just_pressed(MouseButton::Right) {
            let (mx, my) = vse.mouse_position();
            println!("Right click at ({:.0}, {:.0})", mx, my);
        }

        // Draw a small white square that follows the mouse
        let (mx, my) = vse.mouse_position();
        let size = 10.0;
        vse.draw_rect(
            mx as f32 - size,
            my as f32 - size,
            mx as f32 + size,
            my as f32 + size,
            Color::WHITE,
        );

        vse.clear()?;
        let _info = vse.flip(None)?;

        frame_count += 1;

        // Log FPS every 60 frames
        if frame_count % 300 == 0 {
            let (w, h) = vse.window_size();
            println!("Frame {} | {}x{} | Mouse: ({:.0}, {:.0})", frame_count, w, h, mx, my);
        }

        Ok(())
    })?;

    println!("Clean shutdown complete!");
    Ok(())
}
```

**Step 2: Add example entry to Cargo.toml**

Add after the last `[[example]]` entry:

```toml
[[example]]
name = "07_fullscreen_input"
path = "examples/07_fullscreen_input.rs"
```

**Step 3: Verify it compiles**

Run: `cargo build --example 07_fullscreen_input`
Expected: BUILD SUCCESS

**Step 4: Commit**

```bash
git add examples/07_fullscreen_input.rs Cargo.toml
git commit -m "feat: add fullscreen and input handling example"
```

---

### Task 9: Write documentation guide

**Files:**
- Create: `docs/guides/window_modes_and_input.md`

**Step 1: Write the guide**

Create `docs/guides/window_modes_and_input.md` with the following sections:

1. **Window Modes** — code examples for `Windowed`, `BorderlessFullscreen`, `ExclusiveFullscreen`
2. **Fullscreen & Compositor Latency** — explain why exclusive fullscreen matters:
   - In windowed and borderless modes, the OS compositor (DWM on Windows, Mutter/KWin on Linux, Quartz on macOS) composites your framebuffer with other windows before scanout. This adds at least one frame of latency (16.7ms at 60Hz) and can introduce variable jitter.
   - Exclusive fullscreen bypasses the compositor — your swapchain presents directly to the display's scanout buffer. This eliminates compositor latency and provides the most deterministic timing for neural recording experiments.
   - Wayland does not support exclusive fullscreen; VSE falls back to borderless. For timing-critical experiments on Linux, X11 is recommended.
   - Recommendation: use `ExclusiveFullscreen` for data collection, `Windowed` or `BorderlessFullscreen` during development.
3. **Monitor Selection** — `MonitorSelection::Primary`, `Index`, `Name`, with dual-monitor lab example
4. **Video Mode Enumeration** — querying `available_monitors()`, `video_modes()`, finding specific refresh rates
5. **Polled Input** — `key_pressed()`, `key_just_pressed()`, `mouse_position()`, `mouse_button_pressed()` with escape-to-quit and click examples
6. **Event Queue** — `input_events()` with reaction time measurement example:
   ```rust
   // Measure reaction time from stimulus onset to key press
   let flip_info = vse.flip(None)?;
   // ... next frame ...
   for event in vse.input_events() {
       if let InputEvent::KeyDown { key_code: KeyCode::Space, timestamp, .. } = event {
           let rt = timestamp.duration_since(flip_info.present_time);
           println!("Reaction time: {:.1} ms", rt.as_secs_f64() * 1000.0);
       }
   }
   ```
7. **Cursor Control** — `set_cursor_visible()`, `set_cursor_position()`, auto-hide behavior
8. **Key Reference Table** — common `KeyCode` values: Escape, Space, Enter, ArrowUp/Down/Left/Right, KeyA-KeyZ, Digit0-Digit9, F1-F12

**Step 2: Verify the guide renders well**

Read through to ensure accuracy and completeness.

**Step 3: Commit**

```bash
git add docs/guides/window_modes_and_input.md
git commit -m "docs: add window modes and input handling guide"
```

---

### Task 10: Final verification

**Step 1: Run all quality checks**

```bash
cargo fmt
cargo clippy --all-targets
cargo test
cargo build --examples
```

Expected: All pass clean.

**Step 2: Verify no regressions in existing examples**

```bash
cargo build --example 00_clear_color
cargo build --example 01_timing_validation
cargo build --example 02_calibration_square
cargo build --example 03_gabor_demo
cargo build --example 04_scheduled_flip
cargo build --example 05_advanced_stimuli
cargo build --example 05_image_scaling
cargo build --example 06_host_info
```

Expected: All compile successfully.

**Step 3: Final commit if any formatting changes**

```bash
git add -A && git commit -m "chore: final formatting pass"
```
