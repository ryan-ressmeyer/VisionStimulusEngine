# Window Modes, Input Handling & Monitor Selection — Design

**Date:** 2026-02-28
**Status:** Approved
**Approach:** Expand RenderContext (Approach A)

## Overview

Add fullscreen window modes, keyboard/mouse input handling, monitor selection, and cursor control to VSE. The design mirrors Psychtoolbox's polling model (KbCheck, GetMouse) while also providing a timestamped event queue for reaction-time measurement.

## New Types

### Window & Monitor

```rust
pub enum WindowMode {
    Windowed,
    BorderlessFullscreen,
    ExclusiveFullscreen, // Falls back to BorderlessFullscreen on Wayland
}

pub enum MonitorSelection {
    Primary,
    Index(usize),
    Name(String), // Substring match
}

pub struct MonitorInfo {
    pub name: Option<String>,
    pub index: usize,
    pub width: u32,
    pub height: u32,
    pub refresh_rate_hz: Option<f64>,
    pub scale_factor: f64,
    pub position: (i32, i32),
    // video_modes populated from winit's MonitorHandle::video_modes()
}

impl MonitorInfo {
    pub fn video_modes(&self) -> &[VideoModeInfo];
}

pub struct VideoModeInfo {
    pub width: u32,
    pub height: u32,
    pub refresh_rate_hz: f64,
    pub bit_depth: u16,
}
```

### Input

```rust
// Re-exports from winit
pub use winit::keyboard::KeyCode;
pub use winit::keyboard::Key;

pub enum MouseButton {
    Left,
    Right,
    Middle,
    Other(u16),
}

pub enum InputEvent {
    KeyDown {
        key_code: KeyCode,
        logical_key: Key,
        timestamp: Timestamp,
        repeat: bool,
    },
    KeyUp {
        key_code: KeyCode,
        logical_key: Key,
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
```

## Builder Additions

```rust
.with_window_mode(WindowMode::ExclusiveFullscreen) // default: Windowed
.with_monitor(MonitorSelection::Index(1))           // default: Primary
.with_cursor_visible(false)                          // override auto-hide
```

## RenderContext Additions

### Window & Monitor

```rust
vse.set_window_mode(WindowMode)
vse.window_mode() -> WindowMode
vse.available_monitors() -> Vec<MonitorInfo>
vse.primary_monitor() -> Option<MonitorInfo>
vse.video_modes(monitor_index: usize) -> Vec<VideoModeInfo>
vse.current_monitor_video_modes() -> Vec<VideoModeInfo>
```

### Keyboard Polling (frame-aligned)

```rust
vse.key_pressed(KeyCode) -> bool       // currently held
vse.key_just_pressed(KeyCode) -> bool  // pressed this frame
vse.key_just_released(KeyCode) -> bool // released this frame
```

### Mouse Polling (frame-aligned)

```rust
vse.mouse_position() -> (f64, f64)
vse.mouse_button_pressed(MouseButton) -> bool
vse.mouse_button_just_pressed(MouseButton) -> bool
```

### Event Queue (timing-precise)

```rust
vse.input_events() -> &[InputEvent]  // cleared on each flip()
```

### Cursor Control

```rust
vse.set_cursor_visible(bool)
vse.set_cursor_position(x: f64, y: f64)
vse.cursor_visible() -> bool
```

## Internal Implementation

### InputState (internal struct, lives in event loop)

Holds:
- `HashSet<KeyCode>` — currently pressed keys
- `HashSet<KeyCode>` — keys pressed this frame (cleared each frame)
- `HashSet<KeyCode>` — keys released this frame (cleared each frame)
- `(f64, f64)` — current mouse position
- `HashSet<MouseButton>` — currently pressed buttons
- `HashSet<MouseButton>` — buttons pressed this frame
- `Vec<InputEvent>` — event queue (cleared on flip())

### Event Loop Changes

Expand the `WindowEvent` match to capture:
- `KeyboardInput { event, .. }` — update key sets + push InputEvent
- `MouseInput { button, state, .. }` — update button sets + push InputEvent
- `CursorMoved { position, .. }` — update position + push InputEvent
- `MouseWheel { delta, .. }` — push InputEvent

### Window Creation Changes

- Use `WindowBuilder::with_fullscreen()` based on `WindowMode`:
  - `Windowed` → `None`
  - `BorderlessFullscreen` → `Some(Fullscreen::Borderless(monitor))`
  - `ExclusiveFullscreen` → `Some(Fullscreen::Exclusive(video_mode))`
- For exclusive: auto-select best VideoMode (match configured resolution, highest refresh rate)
- For monitor selection: enumerate `elwt.available_monitors()`, match by `MonitorSelection`

### Cursor Auto-Hide

- Fullscreen modes: cursor hidden by default
- Windowed mode: cursor visible by default
- `.with_cursor_visible(bool)` overrides the default

## Compositor Latency Notes

- **Windowed / BorderlessFullscreen**: rendering passes through the OS compositor (DWM on Windows, Mutter/KWin on Wayland/X11, Quartz on macOS), adding at least one frame of latency and potential jitter.
- **ExclusiveFullscreen**: swapchain presents directly to display scanout, bypassing compositor entirely. Essential for timing-critical neural recording experiments.
- **Wayland caveat**: does not support exclusive fullscreen; falls back to borderless. For timing-critical Linux experiments, X11 is recommended.
- **Recommendation**: use ExclusiveFullscreen for data collection, Windowed/BorderlessFullscreen for development.

## Documentation

New file: `docs/guides/window_modes_and_input.md`

Sections:
1. Window Modes — usage examples for each mode
2. Fullscreen & Compositor Latency — why exclusive matters for vision science
3. Monitor Selection — enumeration, video modes, dual-monitor lab setups
4. Polled Input — KbCheck/GetMouse-style examples
5. Event Queue — reaction time measurement with timestamps
6. Cursor Control — hiding, warping, auto-hide behavior
7. Key Reference — common KeyCode values for experiments
