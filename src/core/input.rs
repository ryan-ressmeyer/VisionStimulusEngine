//! Input handling, window modes, and monitor information types.

use crate::timing::Timestamp;
use std::collections::HashSet;
pub use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

/// How the window should be displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WindowMode {
    /// Standard resizable window (default).
    #[default]
    Windowed,
    /// Borderless window covering the entire monitor.
    /// The OS compositor remains active — adds latency.
    BorderlessFullscreen,
    /// Exclusive fullscreen — bypasses the OS compositor.
    /// Lowest latency, guaranteed vsync ownership.
    /// Falls back to `BorderlessFullscreen` on Wayland.
    ExclusiveFullscreen,
}

/// Which monitor to use for fullscreen modes.
#[derive(Debug, Clone, Default)]
pub enum MonitorSelection {
    /// Use the primary monitor (default).
    #[default]
    Primary,
    /// Select by index (0-based, from available monitors list).
    Index(usize),
    /// Select by name substring match (e.g., "ASUS" matches "ASUS VG279Q").
    Name(String),
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
