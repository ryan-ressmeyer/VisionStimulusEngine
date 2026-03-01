# Direct Display Mode Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `WindowMode::DirectDisplay` that bypasses the OS compositor via `VK_KHR_display`, with a cascading acquisition probe (no-compositor → DRM → Xlib via libloading) and evdev keyboard/mouse input — identical rendering and input API to compositor modes.

**Architecture:** `VSEContext::run()` branches on `WindowMode::DirectDisplay` before calling `winit::EventLoop::run()`, instead invoking `run_direct()` which runs its own custom loop. Both paths call a shared `run_frame()` body and produce the same `RenderContext` passed to user closures. `VSEState.window` becomes `Option<Arc<Window>>` (None in direct mode); display dimensions and acquisition method are stored as new fields.

**Tech Stack:** vulkano 0.35, ash 0.38 (VK_KHR_display / VK_EXT_acquire_drm_display / VK_EXT_acquire_xlib_display), evdev 0.12, libloading 0.8 (runtime X11), winit 0.29 (compositor path only).

**Design doc:** `docs/plans/2026-02-28-direct-display-mode-design.md`

---

## Before You Start

- Run `cargo check` and `cargo clippy --all-targets` — must be clean before each task.
- All tests that create `EventLoop` or open a window must be `#[ignore]` (they panic on non-main threads on Linux). Hardware-dependent tests (actual display acquisition) are also `#[ignore]`.
- On ash 0.38, extension loaders live in `ash::khr::*` and `ash::ext::*` (e.g., `ash::khr::Display`, `ash::ext::AcquireDrmDisplay`). Verify exact paths against ash 0.38 docs if a module isn't found.
- vulkano's `Instance` exposes `.handle()` → `ash::vk::Instance`. `PhysicalDevice` exposes `.handle()` → `ash::vk::PhysicalDevice`. Use these for raw ash calls.

---

## Task 1: Add `AcquisitionMethod` and update `DisplayBackend`

**Files:**
- Modify: `src/core/input.rs`

**Step 1: Write the failing test**

At the bottom of `src/core/input.rs`, add:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquisition_method_has_compositor_flag() {
        // DirectDisplay variants should not claim has_compositor
        let backend = DisplayBackend::DirectDisplay {
            method: AcquisitionMethod::DrmAcquire,
        };
        assert!(!backend.has_compositor());
        assert!(DisplayBackend::Wayland.has_compositor());
        assert!(DisplayBackend::X11.has_compositor());
    }

    #[test]
    fn display_backend_direct_description() {
        let backend = DisplayBackend::DirectDisplay {
            method: AcquisitionMethod::NoCompositor,
        };
        let desc = backend.description();
        assert!(desc.contains("direct") || desc.contains("Direct"));
    }
}
```

**Step 2: Run to verify it fails**

```bash
cargo test --lib core::input::tests
```
Expected: compile error — `AcquisitionMethod` and `DisplayBackend::DirectDisplay` not defined yet.

**Step 3: Add `AcquisitionMethod` to `src/core/input.rs`**

After the existing `DisplayBackend` enum definition, add:

```rust
/// How VSE acquired exclusive access to the display in DirectDisplay mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AcquisitionMethod {
    /// No compositor was running — display was unclaimed (TTY / bare session).
    NoCompositor,
    /// Acquired via VK_EXT_acquire_drm_display (requires video group or root).
    DrmAcquire,
    /// Acquired via VK_EXT_acquire_xlib_display (requires DISPLAY env var).
    XlibAcquire,
}
```

**Step 4: Add `DirectDisplay` variant to `DisplayBackend`**

Change the existing `DisplayBackend` enum to add the new variant:

```rust
pub enum DisplayBackend {
    Wayland,
    X11,
    Windows,
    MacOS,
    DirectDisplay { method: AcquisitionMethod },  // ← add this
    Unknown,
}
```

Update `has_compositor()`:

```rust
pub fn has_compositor(&self) -> bool {
    matches!(
        self,
        DisplayBackend::Wayland
            | DisplayBackend::X11
            | DisplayBackend::Unknown
    )
    // DirectDisplay, Windows, MacOS return false
}
```

Update `description()`:

```rust
DisplayBackend::DirectDisplay { method } => match method {
    AcquisitionMethod::NoCompositor => {
        "Direct display — no compositor (TTY/bare session)"
    }
    AcquisitionMethod::DrmAcquire => {
        "Direct display — DRM acquire (VK_EXT_acquire_drm_display)"
    }
    AcquisitionMethod::XlibAcquire => {
        "Direct display — Xlib acquire (VK_EXT_acquire_xlib_display)"
    }
},
```

**Step 5: Run tests**

```bash
cargo test --lib core::input::tests
```
Expected: both tests PASS.

**Step 6: Commit**

```bash
git add src/core/input.rs
git commit -m "feat: add AcquisitionMethod enum and DisplayBackend::DirectDisplay variant"
```

---

## Task 2: Add new `VSEError` variants

**Files:**
- Modify: `src/core/context.rs`

**Step 1: Write the failing test**

In `src/core/context.rs`, find the existing test module (or add one) and add:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_display_unavailable_error_contains_tried_methods() {
        let msg = "Tried:\n  ✗ No-compositor: held by compositor\n  ✗ DRM acquire: permission denied";
        let err = VSEError::DirectDisplayUnavailable(msg.to_string());
        let display = format!("{}", err);
        assert!(display.contains("Tried:"));
        assert!(display.contains("Direct display"));
    }
}
```

**Step 2: Run to verify it fails**

```bash
cargo test --lib core::context::tests::direct_display_unavailable
```
Expected: compile error — `DirectDisplayUnavailable` not defined.

**Step 3: Add error variants**

In the `VSEError` enum in `src/core/context.rs`, add:

```rust
/// All acquisition methods were tried and failed.
/// The string contains a formatted diagnostic listing each failure reason.
#[error("Direct display mode unavailable: {0}")]
DirectDisplayUnavailable(String),

/// Acquisition succeeded but a subsequent setup step failed.
#[error("Direct display setup failed (acquired via {method:?}): {reason}")]
DirectDisplaySetupFailed {
    method: AcquisitionMethod,
    reason: String,
},
```

Also add `AcquisitionMethod` to the import from `super::input`:

```rust
use super::input::{
    AcquisitionMethod, DisplayBackend, InputEvent, InputState, KeyCode, MonitorInfo,
    MonitorSelection, MouseButton, VideoModeInfo, WindowMode,
};
```

**Step 4: Run tests**

```bash
cargo test --lib core::context::tests::direct_display_unavailable
```
Expected: PASS.

**Step 5: Commit**

```bash
git add src/core/context.rs
git commit -m "feat: add DirectDisplayUnavailable and DirectDisplaySetupFailed error variants"
```

---

## Task 3: Add Cargo dependencies

**Files:**
- Modify: `Cargo.toml`

**Step 1: Add Linux-only deps**

After the existing `[dependencies]` block, add:

```toml
[target.'cfg(target_os = "linux")'.dependencies]
# Direct input device access for direct display mode
evdev = "0.12"
# Runtime dynamic loading for optional X11 acquisition path
libloading = "0.8"
```

**Step 2: Verify it builds**

```bash
cargo check
```
Expected: clean.

**Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "feat: add evdev and libloading dependencies for direct display mode (Linux)"
```

---

## Task 4: Add `WindowMode::DirectDisplay` and builder config

**Files:**
- Modify: `src/core/input.rs` (the enum)
- Modify: `src/core/context.rs` (VSEConfig + builder)

**Step 1: Write the failing test**

In `src/core/input.rs` tests:

```rust
#[test]
fn window_mode_direct_display_is_distinct() {
    assert_ne!(WindowMode::DirectDisplay, WindowMode::BorderlessFullscreen);
    assert_ne!(WindowMode::DirectDisplay, WindowMode::ExclusiveFullscreen);
    assert_ne!(WindowMode::DirectDisplay, WindowMode::Windowed);
}
```

**Step 2: Run to verify it fails**

```bash
cargo test --lib core::input::tests::window_mode_direct_display
```
Expected: compile error — `DirectDisplay` variant not on `WindowMode`.

**Step 3: Add `DirectDisplay` to `WindowMode`**

In `src/core/input.rs`, extend `WindowMode`:

```rust
pub enum WindowMode {
    #[default]
    Windowed,
    BorderlessFullscreen,
    ExclusiveFullscreen,
    /// Bypass the OS compositor entirely via VK_KHR_display.
    ///
    /// Acquires exclusive access to the physical display using a cascading
    /// probe: (1) no-compositor TTY check, (2) VK_EXT_acquire_drm_display,
    /// (3) VK_EXT_acquire_xlib_display. Input is sourced from evdev.
    ///
    /// Linux only. See `docs/guides/display_backends.md` for setup.
    DirectDisplay,
}
```

**Step 4: Add VSEConfig fields for direct display overrides**

In the `VSEConfig` struct in `src/core/context.rs`, add two new optional fields:

```rust
/// Override video mode for DirectDisplay (width, height, refresh_hz).
/// Default: highest refresh rate at native resolution.
pub direct_display_video_mode: Option<(u32, u32, f64)>,

/// Override acquisition probe order for DirectDisplay mode.
/// Default: [NoCompositor, DrmAcquire, XlibAcquire].
pub direct_display_acquisition_order: Option<Vec<AcquisitionMethod>>,
```

In `VSEConfig::default()`, add:

```rust
direct_display_video_mode: None,
direct_display_acquisition_order: None,
```

**Step 5: Add builder methods**

In `VSEContextBuilder`, add:

```rust
/// Override the video mode selected in DirectDisplay mode.
///
/// Default: highest refresh rate at native resolution.
pub fn with_direct_display_video_mode(mut self, width: u32, height: u32, refresh_hz: f64) -> Self {
    self.config.direct_display_video_mode = Some((width, height, refresh_hz));
    self
}

/// Override the acquisition probe order for DirectDisplay mode.
///
/// Default: [NoCompositor, DrmAcquire, XlibAcquire].
/// Use this if you know your environment and want to skip failed probes.
pub fn with_acquisition_order(mut self, order: Vec<AcquisitionMethod>) -> Self {
    self.config.direct_display_acquisition_order = Some(order);
    self
}
```

**Step 6: Run tests**

```bash
cargo test --lib core::input::tests::window_mode_direct_display
cargo check
```
Expected: test PASSES, check clean.

**Step 7: Commit**

```bash
git add src/core/input.rs src/core/context.rs
git commit -m "feat: add WindowMode::DirectDisplay variant and builder config overrides"
```

---

## Task 5: Add `InputSource` and update `VSEState`

**Files:**
- Modify: `src/core/context.rs`

**Step 1: Add `InputSource` enum**

Near the top of `src/core/context.rs`, after the imports, add:

```rust
/// Source of input events for the current session.
enum InputSource {
    /// Events from winit (compositor mode).
    Winit,
    /// Events from evdev (direct display mode, Linux only).
    #[cfg(target_os = "linux")]
    Evdev(crate::core::evdev_input::EvdevReader),
}
```

**Step 2: Update `VSEState`**

Change `window: Arc<Window>` to `window: Option<Arc<Window>>` and add new fields:

```rust
struct VSEState {
    window: Option<Arc<Window>>,   // None in DirectDisplay mode
    // ... other existing fields unchanged ...
    input_source: InputSource,
    /// Physical display dimensions (from window or VkDisplaySurfaceKHR).
    display_size: (u32, u32),
    /// Which acquisition method succeeded, if in DirectDisplay mode.
    acquired_display: Option<AcquisitionMethod>,
}
```

**Step 3: Fix all `self.state.window.xxx()` call sites**

`window` is now `Option<Arc<Window>>`. Every place that calls `self.state.window.xxx()` must be updated to either:
- `self.state.window.as_ref().map(|w| w.xxx()).unwrap_or(default)` — for queries
- `if let Some(w) = &self.state.window { w.xxx(); }` — for side-effecting calls

Key methods to update in `RenderContext`:

- `window_size()`: return `self.state.display_size` (no longer reads from window).
- `display_backend()`: check `acquired_display` first; if `Some(method)`, return `DisplayBackend::DirectDisplay { method }`. Otherwise fall through to existing raw window handle detection.
- `available_monitors()`: wrap with `if let Some(w) = &self.state.window { ... } else { vec![] }` for now (direct-mode monitor query is in Task 11).
- `set_window_mode()`: wrap with `if let Some(w) = &self.state.window { ... } else { warn!("set_window_mode() has no effect in DirectDisplay mode"); }`.
- `set_cursor_visible()`: same pattern.
- `set_cursor_position()`: same pattern.

**Step 4: Fix `initialize()` → now `initialize_compositor()`**

Rename `initialize()` to `initialize_compositor()` and update the `Ok(VSEState { ... })` at the end to include the new fields:

```rust
window: Some(window),   // was: window
input_source: InputSource::Winit,
display_size: (win_size.width, win_size.height),
acquired_display: None,
```

Update the call site in `run()` from `Self::initialize(elwt, &config)` to `Self::initialize_compositor(elwt, &config)`.

**Step 5: Verify it compiles**

```bash
cargo check
```
Expected: clean (evdev_input module doesn't exist yet so the `#[cfg]` branch is inactive — that's fine).

**Step 6: Commit**

```bash
git add src/core/context.rs
git commit -m "refactor: make VSEState.window optional, add InputSource and display_size fields"
```

---

## Task 6: Create `EvdevReader` — device discovery

**Files:**
- Create: `src/core/evdev_input.rs`
- Modify: `src/core/mod.rs` (add `mod evdev_input`)

**Step 1: Write the failing test**

Create `src/core/evdev_input.rs` with just the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evdev_reader_new_returns_ok_or_no_devices() {
        // On CI / headless, open_input_devices may find nothing — that's fine.
        // On a real machine it should find at least one device.
        // We just verify it doesn't panic.
        let result = EvdevReader::open();
        // Either Ok (found devices) or Err (no readable devices)
        // — both are valid, just don't panic.
        let _ = result;
    }
}
```

**Step 2: Run to verify it fails**

```bash
cargo test --lib core::evdev_input::tests 2>&1 | head -20
```
Expected: compile error — `evdev_input` module not found, `EvdevReader` not defined.

**Step 3: Add module declaration**

In `src/core/mod.rs`, add:

```rust
#[cfg(target_os = "linux")]
pub(crate) mod evdev_input;
```

**Step 4: Implement `EvdevReader::open()`**

Fill in `src/core/evdev_input.rs`:

```rust
//! evdev-based input for direct display mode.
//!
//! Reads keyboard and mouse events directly from `/dev/input/event*`,
//! bypassing the window manager. Used when `WindowMode::DirectDisplay`
//! is active and no winit event loop is running.

use crate::core::input::{InputEvent, InputState, KeyCode, MouseButton};
use crate::timing::Timestamp;
use evdev::{Device, EventType, InputEventKind};
use tracing::{info, warn};

/// Reads input events from evdev devices for direct display mode.
pub struct EvdevReader {
    keyboards: Vec<Device>,
    pointers: Vec<Device>,
    /// Accumulated absolute mouse position (starts at display center).
    mouse_x: f64,
    mouse_y: f64,
    display_width: f64,
    display_height: f64,
}

impl EvdevReader {
    /// Scan `/dev/input/event*` and open all keyboard and pointer devices.
    ///
    /// Requires read access — user must be in the `input` group or running
    /// as root. Returns `Err` only if zero devices could be opened (which
    /// is a warning, not fatal — headless experiments need no input).
    pub fn open() -> Result<Self, String> {
        let mut keyboards = Vec::new();
        let mut pointers = Vec::new();

        for (_path, device) in evdev::enumerate() {
            let has_keys = device
                .supported_keys()
                .map_or(false, |k| k.iter().next().is_some());
            let has_rel = device
                .supported_relative_axes()
                .map_or(false, |a| a.iter().next().is_some());
            let has_abs = device
                .supported_absolute_axes()
                .map_or(false, |a| a.iter().next().is_some());

            if has_keys {
                info!("evdev: keyboard device: {:?}", device.name());
                keyboards.push(device);
            } else if has_rel || has_abs {
                info!("evdev: pointer device: {:?}", device.name());
                pointers.push(device);
            }
        }

        if keyboards.is_empty() && pointers.is_empty() {
            return Err(
                "No readable input devices found in /dev/input/. \
                 Try: sudo usermod -aG input $USER  (then re-login)"
                    .to_string(),
            );
        }

        Ok(Self {
            keyboards,
            pointers,
            mouse_x: 0.0,
            mouse_y: 0.0,
            display_width: 1920.0,
            display_height: 1080.0,
        })
    }

    /// Set display dimensions so mouse position can be clamped to bounds.
    pub fn set_display_size(&mut self, width: u32, height: u32) {
        self.display_width = width as f64;
        self.display_height = height as f64;
        // Start cursor at display center.
        self.mouse_x = self.display_width / 2.0;
        self.mouse_y = self.display_height / 2.0;
    }
}
```

**Step 5: Run test**

```bash
cargo test --lib core::evdev_input::tests
```
Expected: PASS (function exists and doesn't panic).

**Step 6: Commit**

```bash
git add src/core/evdev_input.rs src/core/mod.rs
git commit -m "feat: add EvdevReader device discovery for direct display input"
```

---

## Task 7: `EvdevReader` — event translation and poll loop

**Files:**
- Modify: `src/core/evdev_input.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn evdev_key_to_vse_keycode_escape() {
    // evdev Key::KEY_ESC maps to VSE KeyCode::Escape
    let mapped = evdev_key_to_keycode(evdev::Key::KEY_ESC);
    assert_eq!(mapped, Some(KeyCode::Escape));
}

#[test]
fn evdev_key_to_vse_keycode_space() {
    let mapped = evdev_key_to_keycode(evdev::Key::KEY_SPACE);
    assert_eq!(mapped, Some(KeyCode::Space));
}

#[test]
fn evdev_key_unknown_returns_none() {
    // A key with no mapping should return None gracefully.
    // KEY_RESERVED is 0 and should not map to anything.
    let mapped = evdev_key_to_keycode(evdev::Key::KEY_RESERVED);
    assert_eq!(mapped, None);
}
```

**Step 2: Run to verify they fail**

```bash
cargo test --lib core::evdev_input::tests::evdev_key_to
```
Expected: compile error — `evdev_key_to_keycode` not defined.

**Step 3: Implement key mapping and `poll()`**

Add to `src/core/evdev_input.rs`:

```rust
impl EvdevReader {
    /// Drain all pending evdev events and feed them into `InputState`.
    /// Call this at the top of each frame in place of winit event dispatch.
    pub fn poll(&mut self, input: &mut InputState, clock: &crate::timing::Clock) {
        // Poll keyboards
        for device in &mut self.keyboards {
            if let Ok(events) = device.fetch_events() {
                for ev in events {
                    self.handle_event(ev, input, clock);
                }
            }
        }
        // Poll pointers
        for device in &mut self.pointers {
            if let Ok(events) = device.fetch_events() {
                for ev in events {
                    self.handle_event(ev, input, clock);
                }
            }
        }
    }

    fn handle_event(
        &mut self,
        ev: evdev::InputEvent,
        input: &mut InputState,
        clock: &crate::timing::Clock,
    ) {
        let timestamp = clock.now();
        match ev.kind() {
            InputEventKind::Key(key) => {
                // Check if it's a mouse button
                if let Some(btn) = evdev_key_to_mouse_button(key) {
                    let (mx, my) = (self.mouse_x, self.mouse_y);
                    if ev.value() == 1 {
                        input.buttons_down.insert(btn);
                        input.buttons_just_pressed.insert(btn);
                        input.events.push(InputEvent::MouseDown {
                            button: btn,
                            x: mx,
                            y: my,
                            timestamp,
                        });
                    } else if ev.value() == 0 {
                        input.buttons_down.remove(&btn);
                        input.events.push(InputEvent::MouseUp {
                            button: btn,
                            x: mx,
                            y: my,
                            timestamp,
                        });
                    }
                } else if let Some(key_code) = evdev_key_to_keycode(key) {
                    let logical_key =
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Unidentified);
                    if ev.value() == 1 {
                        // press
                        let repeat = input.keys_down.contains(&key_code);
                        input.keys_down.insert(key_code);
                        if !repeat {
                            input.keys_just_pressed.insert(key_code);
                        }
                        input.events.push(InputEvent::KeyDown {
                            key_code,
                            logical_key,
                            timestamp,
                            repeat,
                        });
                    } else if ev.value() == 0 {
                        // release
                        input.keys_down.remove(&key_code);
                        input.keys_just_released.insert(key_code);
                        input.events.push(InputEvent::KeyUp {
                            key_code,
                            logical_key,
                            timestamp,
                        });
                    }
                }
            }
            InputEventKind::RelAxis(axis) => {
                use evdev::RelativeAxisType;
                match axis {
                    RelativeAxisType::REL_X => {
                        self.mouse_x =
                            (self.mouse_x + ev.value() as f64).clamp(0.0, self.display_width);
                    }
                    RelativeAxisType::REL_Y => {
                        self.mouse_y =
                            (self.mouse_y + ev.value() as f64).clamp(0.0, self.display_height);
                    }
                    _ => {}
                }
                input.mouse_position = (self.mouse_x, self.mouse_y);
            }
            InputEventKind::AbsAxis(axis) => {
                use evdev::AbsoluteAxisType;
                match axis {
                    AbsoluteAxisType::ABS_X => {
                        self.mouse_x = ev.value() as f64;
                        input.mouse_position.0 = self.mouse_x;
                    }
                    AbsoluteAxisType::ABS_Y => {
                        self.mouse_y = ev.value() as f64;
                        input.mouse_position.1 = self.mouse_y;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// Map an evdev Key to a VSE KeyCode. Returns None for unmapped keys.
pub(crate) fn evdev_key_to_keycode(key: evdev::Key) -> Option<KeyCode> {
    use evdev::Key as E;
    use winit::keyboard::KeyCode as V;
    Some(match key {
        E::KEY_ESC => V::Escape,
        E::KEY_SPACE => V::Space,
        E::KEY_ENTER => V::Enter,
        E::KEY_BACKSPACE => V::Backspace,
        E::KEY_TAB => V::Tab,
        E::KEY_UP => V::ArrowUp,
        E::KEY_DOWN => V::ArrowDown,
        E::KEY_LEFT => V::ArrowLeft,
        E::KEY_RIGHT => V::ArrowRight,
        E::KEY_A => V::KeyA,
        E::KEY_B => V::KeyB,
        E::KEY_C => V::KeyC,
        E::KEY_D => V::KeyD,
        E::KEY_E => V::KeyE,
        E::KEY_F => V::KeyF,
        E::KEY_G => V::KeyG,
        E::KEY_H => V::KeyH,
        E::KEY_I => V::KeyI,
        E::KEY_J => V::KeyJ,
        E::KEY_K => V::KeyK,
        E::KEY_L => V::KeyL,
        E::KEY_M => V::KeyM,
        E::KEY_N => V::KeyN,
        E::KEY_O => V::KeyO,
        E::KEY_P => V::KeyP,
        E::KEY_Q => V::KeyQ,
        E::KEY_R => V::KeyR,
        E::KEY_S => V::KeyS,
        E::KEY_T => V::KeyT,
        E::KEY_U => V::KeyU,
        E::KEY_V => V::KeyV,
        E::KEY_W => V::KeyW,
        E::KEY_X => V::KeyX,
        E::KEY_Y => V::KeyY,
        E::KEY_Z => V::KeyZ,
        E::KEY_1 => V::Digit1,
        E::KEY_2 => V::Digit2,
        E::KEY_3 => V::Digit3,
        E::KEY_4 => V::Digit4,
        E::KEY_5 => V::Digit5,
        E::KEY_6 => V::Digit6,
        E::KEY_7 => V::Digit7,
        E::KEY_8 => V::Digit8,
        E::KEY_9 => V::Digit9,
        E::KEY_0 => V::Digit0,
        E::KEY_F1 => V::F1,
        E::KEY_F2 => V::F2,
        E::KEY_F3 => V::F3,
        E::KEY_F4 => V::F4,
        E::KEY_F5 => V::F5,
        E::KEY_F6 => V::F6,
        E::KEY_F7 => V::F7,
        E::KEY_F8 => V::F8,
        E::KEY_F9 => V::F9,
        E::KEY_F10 => V::F10,
        E::KEY_F11 => V::F11,
        E::KEY_F12 => V::F12,
        E::KEY_LEFTSHIFT => V::ShiftLeft,
        E::KEY_RIGHTSHIFT => V::ShiftRight,
        E::KEY_LEFTCTRL => V::ControlLeft,
        E::KEY_RIGHTCTRL => V::ControlRight,
        E::KEY_LEFTALT => V::AltLeft,
        E::KEY_RIGHTALT => V::AltRight,
        _ => return None,
    })
}

/// Map an evdev Key to a VSE MouseButton. Returns None for non-button keys.
pub(crate) fn evdev_key_to_mouse_button(key: evdev::Key) -> Option<MouseButton> {
    use evdev::Key as E;
    Some(match key {
        E::BTN_LEFT => MouseButton::Left,
        E::BTN_RIGHT => MouseButton::Right,
        E::BTN_MIDDLE => MouseButton::Middle,
        _ => return None,
    })
}
```

**Step 4: Run tests**

```bash
cargo test --lib core::evdev_input::tests
```
Expected: all 3 tests PASS.

**Step 5: Commit**

```bash
git add src/core/evdev_input.rs
git commit -m "feat: implement EvdevReader event translation and poll loop"
```

---

## Task 8: Create `direct_display.rs` — Vulkan display infrastructure

**Files:**
- Create: `src/core/direct_display.rs`
- Modify: `src/core/mod.rs` (add mod)

**Step 1: Add module declaration**

In `src/core/mod.rs`:

```rust
#[cfg(target_os = "linux")]
mod direct_display;
#[cfg(target_os = "linux")]
pub(crate) use direct_display::DirectDisplaySurface;
```

**Step 2: Create `src/core/direct_display.rs` with the scaffold**

```rust
//! Direct display mode — VK_KHR_display surface acquisition.
//!
//! Creates a VkDisplaySurfaceKHR that bypasses the OS compositor, giving VSE
//! exclusive access to the physical display and direct vblank control.
//!
//! # Acquisition Cascade
//!
//! 1. `probe_no_compositor` — unclaimed display (TTY / bare session)
//! 2. `probe_drm_acquire`   — VK_EXT_acquire_drm_display
//! 3. `probe_xlib_acquire`  — VK_EXT_acquire_xlib_display (via libloading)
//!
//! See `docs/guides/display_backends.md` for user-facing setup instructions.

use crate::core::input::AcquisitionMethod;
use ash::vk;
use std::sync::Arc;
use tracing::info;
use vulkano::instance::Instance;
use vulkano::swapchain::Surface;

/// Result of a successful display acquisition.
pub(crate) struct DirectDisplaySurface {
    pub surface: Arc<Surface>,
    pub method: AcquisitionMethod,
    pub width: u32,
    pub height: u32,
    pub refresh_rate_hz: f64,
}

/// Error from a single probe attempt.
struct ProbeFailure {
    method: AcquisitionMethod,
    reason: String,
}
```

**Step 3: Write a unit test for error message formatting**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_unavailable_message_contains_all_methods() {
        let failures = vec![
            ProbeFailure {
                method: AcquisitionMethod::NoCompositor,
                reason: "display held by compositor".to_string(),
            },
            ProbeFailure {
                method: AcquisitionMethod::DrmAcquire,
                reason: "permission denied".to_string(),
            },
            ProbeFailure {
                method: AcquisitionMethod::XlibAcquire,
                reason: "libX11.so not found".to_string(),
            },
        ];
        let msg = format_unavailable_message("eDP-1", &failures);
        assert!(msg.contains("No-compositor"));
        assert!(msg.contains("DRM acquire"));
        assert!(msg.contains("Xlib acquire"));
        assert!(msg.contains("display_backends.md"));
    }
}
```

**Step 4: Implement `format_unavailable_message`**

```rust
fn format_unavailable_message(display_name: &str, failures: &[ProbeFailure]) -> String {
    let mut msg = format!(
        "Direct display mode unavailable on {}. Tried:\n",
        display_name
    );
    for f in failures {
        let label = match f.method {
            AcquisitionMethod::NoCompositor => "No-compositor",
            AcquisitionMethod::DrmAcquire => "DRM acquire  ",
            AcquisitionMethod::XlibAcquire => "Xlib acquire ",
        };
        msg.push_str(&format!("  \u{2717} {}: {}\n", label, f.reason));
    }
    msg.push_str("\nSee docs/guides/display_backends.md for setup instructions.");
    msg
}
```

**Step 5: Run the test**

```bash
cargo test --lib core::direct_display::tests
```
Expected: PASS.

**Step 6: Commit**

```bash
git add src/core/direct_display.rs src/core/mod.rs
git commit -m "feat: scaffold direct_display module with error message formatting"
```

---

## Task 9: `direct_display.rs` — `probe_no_compositor`

**Files:**
- Modify: `src/core/direct_display.rs`

**Step 1: Implement `probe_no_compositor`**

This probe enumerates displays via the raw ash `VK_KHR_display` loader and
attempts to create a `VkDisplaySurfaceKHR`. It succeeds when no compositor
holds DRM master.

```rust
/// Attempt to create a display surface with no compositor running.
///
/// Uses VK_KHR_display to enumerate physical displays and create a
/// VkDisplaySurfaceKHR directly. Succeeds only when the display is not
/// claimed by a running compositor.
fn probe_no_compositor(
    instance: &Arc<Instance>,
    physical_device: ash::vk::PhysicalDevice,
    target_name: Option<&str>,
    video_mode_override: Option<(u32, u32, f64)>,
) -> Result<DirectDisplaySurface, ProbeFailure> {
    // Load VK_KHR_display function pointers via ash.
    // ash 0.38: ash::khr::Display::new(entry, instance_raw)
    // instance.handle() gives ash::vk::Instance; we need ash Entry too.
    // vulkano doesn't expose Entry directly — construct ash::Entry manually:
    let entry = unsafe { ash::Entry::load() }.map_err(|e| ProbeFailure {
        method: AcquisitionMethod::NoCompositor,
        reason: format!("ash Entry load failed: {}", e),
    })?;
    let instance_raw = instance.handle();
    let khr_display = ash::khr::Display::new(&entry, &instance_raw);

    // Enumerate displays
    let displays = unsafe {
        khr_display
            .get_physical_device_display_properties(physical_device)
            .map_err(|e| ProbeFailure {
                method: AcquisitionMethod::NoCompositor,
                reason: format!("vkGetPhysicalDeviceDisplayPropertiesKHR failed: {:?}", e),
            })?
    };

    if displays.is_empty() {
        return Err(ProbeFailure {
            method: AcquisitionMethod::NoCompositor,
            reason: "no displays reported by VK_KHR_display".to_string(),
        });
    }

    // Select target display
    let display_props = select_display(&displays, target_name).ok_or_else(|| ProbeFailure {
        method: AcquisitionMethod::NoCompositor,
        reason: format!("no display matching {:?} found", target_name),
    })?;

    let display = display_props.display;
    let display_name = unsafe {
        std::ffi::CStr::from_ptr(display_props.display_name)
            .to_string_lossy()
            .into_owned()
    };

    // Enumerate and select video mode
    let modes = unsafe {
        khr_display
            .get_display_mode_properties(physical_device, display)
            .map_err(|e| ProbeFailure {
                method: AcquisitionMethod::NoCompositor,
                reason: format!("vkGetDisplayModePropertiesKHR failed: {:?}", e),
            })?
    };

    let mode_props =
        select_video_mode(&modes, video_mode_override).ok_or_else(|| ProbeFailure {
            method: AcquisitionMethod::NoCompositor,
            reason: "no video modes available for this display".to_string(),
        })?;

    let mode = mode_props.display_mode;
    let width = mode_props.parameters.visible_region.width;
    let height = mode_props.parameters.visible_region.height;
    let refresh_rate_hz = mode_props.parameters.refresh_rate as f64 / 1000.0;

    // Get display plane
    let planes = unsafe {
        khr_display
            .get_physical_device_display_plane_properties(physical_device)
            .map_err(|e| ProbeFailure {
                method: AcquisitionMethod::NoCompositor,
                reason: format!("vkGetPhysicalDeviceDisplayPlanePropertiesKHR: {:?}", e),
            })?
    };

    if planes.is_empty() {
        return Err(ProbeFailure {
            method: AcquisitionMethod::NoCompositor,
            reason: "no display planes available".to_string(),
        });
    }

    // Create VkDisplaySurfaceKHR
    let surface_info = vk::DisplaySurfaceCreateInfoKHR {
        display_mode: mode,
        plane_index: 0,
        plane_stack_index: planes[0].current_stack_index,
        transform: vk::SurfaceTransformFlagsKHR::IDENTITY,
        global_alpha: 1.0,
        alpha_mode: vk::DisplayPlaneAlphaFlagsKHR::OPAQUE,
        image_extent: vk::Extent2D { width, height },
        ..Default::default()
    };

    let surface_handle = unsafe {
        khr_display
            .create_display_plane_surface(&surface_info, None)
            .map_err(|e| ProbeFailure {
                method: AcquisitionMethod::NoCompositor,
                reason: format!(
                    "vkCreateDisplayPlaneSurfaceKHR failed: {:?} \
                     (compositor may be holding the display)",
                    e
                ),
            })?
    };

    // Wrap raw VkSurfaceKHR into a vulkano Surface.
    // vulkano 0.35: Surface::from_raw(instance, surface_khr)
    // Check vulkano 0.35 source for exact method name if this doesn't compile.
    let surface = unsafe {
        Arc::new(
            Surface::from_handle(Arc::clone(instance), surface_handle, ())
                .map_err(|e| ProbeFailure {
                    method: AcquisitionMethod::NoCompositor,
                    reason: format!("Surface::from_handle failed: {:?}", e),
                })?,
        )
    };

    info!(
        "Direct display: acquired {} via no-compositor path ({} x {} @ {:.1} Hz)",
        display_name, width, height, refresh_rate_hz
    );

    Ok(DirectDisplaySurface {
        surface,
        method: AcquisitionMethod::NoCompositor,
        width,
        height,
        refresh_rate_hz,
    })
}
```

Add helper functions:

```rust
/// Select the display matching `target_name` (substring), or the first display.
fn select_display<'a>(
    displays: &'a [vk::DisplayPropertiesKHR],
    target_name: Option<&str>,
) -> Option<&'a vk::DisplayPropertiesKHR> {
    if let Some(name) = target_name {
        let name_lower = name.to_lowercase();
        displays.iter().find(|d| {
            let n = unsafe { std::ffi::CStr::from_ptr(d.display_name) }
                .to_string_lossy()
                .to_lowercase();
            n.contains(&name_lower)
        })
    } else {
        displays.first()
    }
}

/// Select a video mode. Uses override if provided, otherwise highest refresh
/// rate at the largest resolution.
fn select_video_mode(
    modes: &[vk::DisplayModePropertiesKHR],
    override_: Option<(u32, u32, f64)>,
) -> Option<&vk::DisplayModePropertiesKHR> {
    if let Some((w, h, hz)) = override_ {
        let target_millihertz = (hz * 1000.0) as u32;
        modes.iter().find(|m| {
            m.parameters.visible_region.width == w
                && m.parameters.visible_region.height == h
                && (m.parameters.refresh_rate as i32 - target_millihertz as i32).abs() < 500
        })
    } else {
        modes.iter().max_by_key(|m| {
            let area = m.parameters.visible_region.width * m.parameters.visible_region.height;
            (area, m.parameters.refresh_rate)
        })
    }
}
```

**Step 2: Run existing tests**

```bash
cargo test --lib core::direct_display::tests
cargo check
```
Expected: existing test still PASSES, check clean.

> **Note on `Surface::from_handle`:** vulkano 0.35 may use a different constructor name. Check `vulkano::swapchain::Surface` in the vulkano 0.35 source or docs for the correct method to wrap a raw `ash::vk::SurfaceKHR`. Candidate names: `from_raw`, `from_handle`, `from_raw_surface`. Adjust as needed.

**Step 3: Commit**

```bash
git add src/core/direct_display.rs
git commit -m "feat: implement probe_no_compositor for direct display acquisition"
```

---

## Task 10: `direct_display.rs` — `probe_drm_acquire` and `probe_xlib_acquire`

**Files:**
- Modify: `src/core/direct_display.rs`

**Step 1: Implement `probe_drm_acquire`**

```rust
/// Acquire display via VK_EXT_acquire_drm_display.
///
/// Opens `/dev/dri/cardX` for the GPU and calls vkAcquireDrmDisplayEXT.
/// Requires the user to be in the `video` group or running as root.
fn probe_drm_acquire(
    instance: &Arc<Instance>,
    physical_device: ash::vk::PhysicalDevice,
    target_name: Option<&str>,
    video_mode_override: Option<(u32, u32, f64)>,
) -> Result<DirectDisplaySurface, ProbeFailure> {
    let entry = unsafe { ash::Entry::load() }.map_err(|e| ProbeFailure {
        method: AcquisitionMethod::DrmAcquire,
        reason: format!("ash Entry load failed: {}", e),
    })?;
    let instance_raw = instance.handle();

    // Check extension is available
    let ext_drm = ash::ext::AcquireDrmDisplay::new(&entry, &instance_raw);
    let khr_display = ash::khr::Display::new(&entry, &instance_raw);

    // Open DRM fd — /dev/dri/card0 for the first GPU.
    // TODO: select the correct DRI device based on the chosen GPU.
    let drm_path = "/dev/dri/card0";
    let drm_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(drm_path)
        .map_err(|e| ProbeFailure {
            method: AcquisitionMethod::DrmAcquire,
            reason: format!(
                "permission denied on {} — try: sudo usermod -aG video $USER (re-login required). OS error: {}",
                drm_path, e
            ),
        })?;

    use std::os::unix::io::AsRawFd;
    let drm_fd = drm_file.as_raw_fd();

    // Enumerate displays to find target
    let displays = unsafe {
        khr_display
            .get_physical_device_display_properties(physical_device)
            .map_err(|e| ProbeFailure {
                method: AcquisitionMethod::DrmAcquire,
                reason: format!("vkGetPhysicalDeviceDisplayPropertiesKHR: {:?}", e),
            })?
    };

    let display_props = select_display(&displays, target_name).ok_or_else(|| ProbeFailure {
        method: AcquisitionMethod::DrmAcquire,
        reason: "no matching display found".to_string(),
    })?;

    let display = display_props.display;

    // Acquire
    unsafe {
        ext_drm
            .acquire_drm_display(physical_device, drm_fd, display)
            .map_err(|e| ProbeFailure {
                method: AcquisitionMethod::DrmAcquire,
                reason: format!("vkAcquireDrmDisplayEXT failed: {:?}", e),
            })?;
    }

    info!("Direct display: DRM acquire succeeded on {}", drm_path);

    // Now create the surface using the same VK_KHR_display path
    let modes = unsafe {
        khr_display
            .get_display_mode_properties(physical_device, display)
            .map_err(|e| ProbeFailure {
                method: AcquisitionMethod::DrmAcquire,
                reason: format!("vkGetDisplayModePropertiesKHR: {:?}", e),
            })?
    };

    let mode_props =
        select_video_mode(&modes, video_mode_override).ok_or_else(|| ProbeFailure {
            method: AcquisitionMethod::DrmAcquire,
            reason: "no video modes available".to_string(),
        })?;

    let width = mode_props.parameters.visible_region.width;
    let height = mode_props.parameters.visible_region.height;
    let refresh_rate_hz = mode_props.parameters.refresh_rate as f64 / 1000.0;

    let surface_info = vk::DisplaySurfaceCreateInfoKHR {
        display_mode: mode_props.display_mode,
        plane_index: 0,
        plane_stack_index: 0,
        transform: vk::SurfaceTransformFlagsKHR::IDENTITY,
        global_alpha: 1.0,
        alpha_mode: vk::DisplayPlaneAlphaFlagsKHR::OPAQUE,
        image_extent: vk::Extent2D { width, height },
        ..Default::default()
    };

    let surface_handle = unsafe {
        khr_display
            .create_display_plane_surface(&surface_info, None)
            .map_err(|e| ProbeFailure {
                method: AcquisitionMethod::DrmAcquire,
                reason: format!("vkCreateDisplayPlaneSurfaceKHR: {:?}", e),
            })?
    };

    let surface = unsafe {
        Arc::new(
            Surface::from_handle(Arc::clone(instance), surface_handle, ())
                .map_err(|e| ProbeFailure {
                    method: AcquisitionMethod::DrmAcquire,
                    reason: format!("Surface::from_handle: {:?}", e),
                })?,
        )
    };

    Ok(DirectDisplaySurface {
        surface,
        method: AcquisitionMethod::DrmAcquire,
        width,
        height,
        refresh_rate_hz,
    })
}
```

**Step 2: Implement `probe_xlib_acquire`**

```rust
/// Acquire display via VK_EXT_acquire_xlib_display using libloading.
///
/// Dynamically loads libX11.so and libXrandr.so at runtime — no build-time
/// X11 headers required. Returns ProbeFailure if the libraries are absent.
fn probe_xlib_acquire(
    instance: &Arc<Instance>,
    physical_device: ash::vk::PhysicalDevice,
    target_name: Option<&str>,
    video_mode_override: Option<(u32, u32, f64)>,
) -> Result<DirectDisplaySurface, ProbeFailure> {
    let entry = unsafe { ash::Entry::load() }.map_err(|e| ProbeFailure {
        method: AcquisitionMethod::XlibAcquire,
        reason: format!("ash Entry load failed: {}", e),
    })?;
    let instance_raw = instance.handle();

    // Runtime load libX11
    let lib_x11 = unsafe { libloading::Library::new("libX11.so.6") }.map_err(|e| ProbeFailure {
        method: AcquisitionMethod::XlibAcquire,
        reason: format!("libX11.so.6 not found — X11 not installed: {}", e),
    })?;

    // XOpenDisplay(NULL) — connect to $DISPLAY
    type XOpenDisplayFn = unsafe extern "C" fn(*const std::ffi::c_char) -> *mut std::ffi::c_void;
    let x_open_display: libloading::Symbol<XOpenDisplayFn> = unsafe {
        lib_x11.get(b"XOpenDisplay\0").map_err(|e| ProbeFailure {
            method: AcquisitionMethod::XlibAcquire,
            reason: format!("XOpenDisplay symbol not found: {}", e),
        })?
    };

    let x_display = unsafe { x_open_display(std::ptr::null()) };
    if x_display.is_null() {
        return Err(ProbeFailure {
            method: AcquisitionMethod::XlibAcquire,
            reason: "XOpenDisplay returned NULL — DISPLAY env var not set or X server unavailable"
                .to_string(),
        });
    }

    // Enumerate VK_KHR_display displays
    let khr_display = ash::khr::Display::new(&entry, &instance_raw);
    let displays = unsafe {
        khr_display
            .get_physical_device_display_properties(physical_device)
            .map_err(|e| ProbeFailure {
                method: AcquisitionMethod::XlibAcquire,
                reason: format!("vkGetPhysicalDeviceDisplayPropertiesKHR: {:?}", e),
            })?
    };

    let display_props = select_display(&displays, target_name).ok_or_else(|| ProbeFailure {
        method: AcquisitionMethod::XlibAcquire,
        reason: "no matching display found".to_string(),
    })?;

    let display = display_props.display;

    // vkAcquireXlibDisplayEXT
    let ext_xlib = ash::ext::AcquireXlibDisplay::new(&entry, &instance_raw);
    unsafe {
        ext_xlib
            .acquire_xlib_display(physical_device, x_display as *mut _, display)
            .map_err(|e| ProbeFailure {
                method: AcquisitionMethod::XlibAcquire,
                reason: format!(
                    "vkAcquireXlibDisplayEXT failed — RandR output not found or X server denied: {:?}",
                    e
                ),
            })?;
    }

    info!("Direct display: Xlib acquire succeeded");

    // Create surface
    let modes = unsafe {
        khr_display
            .get_display_mode_properties(physical_device, display)
            .map_err(|e| ProbeFailure {
                method: AcquisitionMethod::XlibAcquire,
                reason: format!("vkGetDisplayModePropertiesKHR: {:?}", e),
            })?
    };

    let mode_props =
        select_video_mode(&modes, video_mode_override).ok_or_else(|| ProbeFailure {
            method: AcquisitionMethod::XlibAcquire,
            reason: "no video modes available".to_string(),
        })?;

    let width = mode_props.parameters.visible_region.width;
    let height = mode_props.parameters.visible_region.height;
    let refresh_rate_hz = mode_props.parameters.refresh_rate as f64 / 1000.0;

    let surface_info = vk::DisplaySurfaceCreateInfoKHR {
        display_mode: mode_props.display_mode,
        plane_index: 0,
        plane_stack_index: 0,
        transform: vk::SurfaceTransformFlagsKHR::IDENTITY,
        global_alpha: 1.0,
        alpha_mode: vk::DisplayPlaneAlphaFlagsKHR::OPAQUE,
        image_extent: vk::Extent2D { width, height },
        ..Default::default()
    };

    let surface_handle = unsafe {
        khr_display
            .create_display_plane_surface(&surface_info, None)
            .map_err(|e| ProbeFailure {
                method: AcquisitionMethod::XlibAcquire,
                reason: format!("vkCreateDisplayPlaneSurfaceKHR: {:?}", e),
            })?
    };

    let surface = unsafe {
        Arc::new(
            Surface::from_handle(Arc::clone(instance), surface_handle, ())
                .map_err(|e| ProbeFailure {
                    method: AcquisitionMethod::XlibAcquire,
                    reason: format!("Surface::from_handle: {:?}", e),
                })?,
        )
    };

    Ok(DirectDisplaySurface {
        surface,
        method: AcquisitionMethod::XlibAcquire,
        width,
        height,
        refresh_rate_hz,
    })
}
```

**Step 3: Verify**

```bash
cargo check
```
Expected: clean.

**Step 4: Commit**

```bash
git add src/core/direct_display.rs
git commit -m "feat: implement probe_drm_acquire and probe_xlib_acquire display probes"
```

---

## Task 11: `direct_display.rs` — cascade orchestrator

**Files:**
- Modify: `src/core/direct_display.rs`

**Step 1: Add `acquire_display` — the public entry point**

```rust
/// Run the acquisition cascade and return the first successful surface.
///
/// Probe order: NoCompositor → DrmAcquire → XlibAcquire (or custom order
/// from `acquisition_order`). All failures are collected; if every probe
/// fails, returns `VSEError::DirectDisplayUnavailable` with a detailed
/// diagnostic message.
pub(crate) fn acquire_display(
    instance: &Arc<Instance>,
    physical_device: ash::vk::PhysicalDevice,
    target_name: Option<&str>,
    video_mode_override: Option<(u32, u32, f64)>,
    acquisition_order: &[AcquisitionMethod],
) -> Result<DirectDisplaySurface, crate::core::context::VSEError> {
    let display_label = target_name.unwrap_or("primary display");
    info!("Attempting direct display mode on {}...", display_label);

    let mut failures = Vec::new();

    for (i, method) in acquisition_order.iter().enumerate() {
        info!(
            "Probe {}/{} ({:?})...",
            i + 1,
            acquisition_order.len(),
            method
        );
        let result = match method {
            AcquisitionMethod::NoCompositor => {
                probe_no_compositor(instance, physical_device, target_name, video_mode_override)
            }
            AcquisitionMethod::DrmAcquire => {
                probe_drm_acquire(instance, physical_device, target_name, video_mode_override)
            }
            AcquisitionMethod::XlibAcquire => {
                probe_xlib_acquire(instance, physical_device, target_name, video_mode_override)
            }
        };

        match result {
            Ok(surface) => {
                info!(
                    "Direct display mode active via {:?}: {}x{} @ {:.1} Hz",
                    surface.method, surface.width, surface.height, surface.refresh_rate_hz
                );
                return Ok(surface);
            }
            Err(f) => {
                info!("  Probe {:?} failed: {}", f.method, f.reason);
                failures.push(f);
            }
        }
    }

    let msg = format_unavailable_message(display_label, &failures);
    Err(crate::core::context::VSEError::DirectDisplayUnavailable(msg))
}

/// Default probe order.
pub(crate) fn default_acquisition_order() -> Vec<AcquisitionMethod> {
    vec![
        AcquisitionMethod::NoCompositor,
        AcquisitionMethod::DrmAcquire,
        AcquisitionMethod::XlibAcquire,
    ]
}
```

**Step 2: Verify**

```bash
cargo check
cargo test --lib core::direct_display::tests
```
Expected: clean and PASS.

**Step 3: Commit**

```bash
git add src/core/direct_display.rs
git commit -m "feat: add acquire_display cascade orchestrator for direct display mode"
```

---

## Task 12: Update `device.rs` — request display extensions

**Files:**
- Modify: `src/core/device.rs`

**Step 1: Add `with_direct_display_surface` constructor**

`DeviceSelector` needs a variant that enables `VK_KHR_display`,
`VK_EXT_acquire_drm_display`, and `VK_EXT_acquire_xlib_display` instance
extensions instead of the window surface extensions. Add to `device.rs`:

```rust
/// Create a device selector for direct display mode (no window/compositor).
///
/// Enables VK_KHR_display and acquisition extensions. Returns the selector
/// plus the raw ash Instance needed for direct display surface creation.
#[cfg(target_os = "linux")]
pub fn with_direct_display(
    preference: GPUPreference,
) -> Result<(Self, Arc<Instance>), DeviceError> {
    let library =
        VulkanLibrary::new().map_err(|e| DeviceError::LibraryLoadFailed(e.to_string()))?;

    info!("Vulkan library loaded successfully");

    let required_extensions = InstanceExtensions {
        khr_display: true,
        ext_acquire_drm_display: true,
        ext_acquire_xlib_display: true,
        ..InstanceExtensions::empty()
    };

    // Mask out extensions not supported on this instance
    let supported = InstanceExtensions::supported_by_core()
        .map_err(|e| DeviceError::InstanceCreationFailed(e.to_string()))?;
    let enabled_extensions = required_extensions.intersection(&supported);

    if !enabled_extensions.khr_display {
        return Err(DeviceError::InstanceCreationFailed(
            "VK_KHR_display not supported by this Vulkan installation".to_string(),
        ));
    }

    let instance = Instance::new(
        library,
        InstanceCreateInfo {
            flags: InstanceCreateFlags::ENUMERATE_PORTABILITY,
            enabled_extensions,
            ..Default::default()
        },
    )
    .map_err(|e| DeviceError::InstanceCreationFailed(e.to_string()))?;

    info!("Vulkan instance created with direct display extensions");

    let (physical_device, queue_family_index) =
        Self::select_physical_device(&instance, preference)?;

    let device_name = physical_device.properties().device_name.clone();
    let device_type = physical_device.properties().device_type;
    info!("Selected GPU: {} ({:?})", device_name, device_type);

    let selector = Self {
        instance: Arc::clone(&instance),
        physical_device,
        graphics_queue_family_index: queue_family_index,
    };

    Ok((selector, instance))
}
```

**Step 2: Verify**

```bash
cargo check
```
Expected: clean.

**Step 3: Commit**

```bash
git add src/core/device.rs
git commit -m "feat: add DeviceSelector::with_direct_display for VK_KHR_display instance setup"
```

---

## Task 13: Update `context.rs` — `initialize_direct` and `run_direct`

**Files:**
- Modify: `src/core/context.rs`

**Step 1: Add `initialize_direct`**

Add this function to `VSEContext` (below `initialize_compositor`):

```rust
/// Initialize Vulkan state for direct display mode (no winit, no compositor).
#[cfg(target_os = "linux")]
fn initialize_direct(config: &VSEConfig) -> Result<VSEState, VSEError> {
    use crate::core::direct_display::{acquire_display, default_acquisition_order};
    use crate::core::evdev_input::EvdevReader;

    // Resolve monitor name from selection (Name variant only; Index falls back to primary)
    let target_name = match &config.monitor_selection {
        MonitorSelection::Name(n) => Some(n.as_str()),
        _ => None,
    };

    // Create Vulkan instance with display extensions
    let (device_selector, instance) =
        DeviceSelector::with_direct_display(config.gpu_preference)
            .map_err(VSEError::Device)?;

    let phys_dev = device_selector.physical_device.handle();

    // Run acquisition cascade
    let order = config
        .direct_display_acquisition_order
        .clone()
        .unwrap_or_else(default_acquisition_order);

    let direct_surface = acquire_display(
        &instance,
        phys_dev,
        target_name,
        config.direct_display_video_mode,
        &order,
    )?;

    let (width, height) = (direct_surface.width, direct_surface.height);
    let method = direct_surface.method;
    let surface = direct_surface.surface;

    // Create logical device
    let (device, queue) = device_selector.create_device().map_err(VSEError::Device)?;

    let swapchain_config = SwapchainConfig {
        width,
        height,
        present_mode: config.present_mode,
        image_count: 2,
    };

    let swapchain = SwapchainManager::new(device.clone(), surface, swapchain_config)?;
    let frame_builder = FrameBuilder::new(device.clone(), queue.clone());
    let renderer = Renderer::new(device.clone(), queue.clone(), swapchain.format())?;

    let clock = Clock::new();

    // VK_GOOGLE_display_timing is less relevant in direct mode but keep if available
    let timing_provider: Box<dyn TimingProvider> =
        if device_selector.supports_google_display_timing() {
            Box::new(unsafe {
                GoogleDisplayTimingProvider::new(&device, swapchain.swapchain())
            })
        } else {
            Box::new(CpuTimingProvider::new())
        };

    let flip_logger = if config.flip_logging {
        let capacity = 3600 * 10;
        Some(match &config.flip_log_csv_path {
            Some(path) => FlipLogger::with_csv(path.clone(), capacity),
            None => FlipLogger::new(capacity),
        })
    } else {
        None
    };

    let expected_frame_duration = config
        .expected_refresh_rate
        .map(|hz| Duration::from_micros((1_000_000.0 / hz) as u64));

    // Open evdev input devices
    let evdev_reader = match EvdevReader::open() {
        Ok(mut r) => {
            r.set_display_size(width, height);
            r
        }
        Err(msg) => {
            warn!("evdev input unavailable: {}", msg);
            // Create a reader with no devices — input will simply be silent.
            EvdevReader::empty()
        }
    };

    info!("Direct display initialization complete");

    Ok(VSEState {
        window: None,
        device_selector,
        device,
        queue,
        swapchain,
        frame_builder,
        renderer,
        should_close: false,
        minimized: false,
        input: InputState::new(),
        cursor_visible: false,
        window_mode: WindowMode::DirectDisplay,
        clock,
        timing_provider,
        flip_logger,
        frame_number: 0,
        last_present_time: None,
        expected_frame_duration,
        refresh_detect_samples: Vec::with_capacity(10),
        input_source: InputSource::Evdev(evdev_reader),
        display_size: (width, height),
        acquired_display: Some(method),
    })
}
```

Also add `EvdevReader::empty()` to `evdev_input.rs`:

```rust
/// Create a reader with no devices. Input methods will return no events.
pub fn empty() -> Self {
    Self {
        keyboards: vec![],
        pointers: vec![],
        mouse_x: 0.0,
        mouse_y: 0.0,
        display_width: 1920.0,
        display_height: 1080.0,
    }
}
```

**Step 2: Add `run_direct`**

```rust
/// Run the direct display event loop (no winit).
#[cfg(target_os = "linux")]
fn run_direct<F>(self, mut render_fn: F) -> Result<(), VSEError>
where
    F: FnMut(&mut RenderContext) -> Result<(), VSEError> + 'static,
{
    let mut state = Self::initialize_direct(&self.config)?;
    let mut config = self.config;

    loop {
        // Poll evdev input
        if let InputSource::Evdev(ref mut reader) = state.input_source {
            reader.poll(&mut state.input, &state.clock);
        }
        state.input.begin_frame();

        // Call user render closure
        let mut render_ctx = RenderContext {
            state: &mut state,
            config: &mut config,
        };

        if let Err(e) = render_fn(&mut render_ctx) {
            warn!("Render error: {}", e);
            return Err(e);
        }

        if state.should_close {
            break;
        }
    }

    Ok(())
}
```

**Step 3: Update `run()` to branch for DirectDisplay**

At the top of `run()`, before the event_loop setup, add:

```rust
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

    // existing winit path below unchanged...
```

**Step 4: Update `display_backend()` to check `acquired_display`**

In `RenderContext::display_backend()`:

```rust
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
```

**Step 5: Verify**

```bash
cargo check
cargo clippy --all-targets
```
Expected: clean.

**Step 6: Commit**

```bash
git add src/core/context.rs src/core/evdev_input.rs
git commit -m "feat: add initialize_direct and run_direct for DirectDisplay mode"
```

---

## Task 14: Export new types

**Files:**
- Modify: `src/core/mod.rs`
- Modify: `src/lib.rs`

**Step 1: Export from `src/core/mod.rs`**

```rust
pub use input::{
    AcquisitionMethod, DisplayBackend, InputEvent, Key, KeyCode, MonitorInfo, MonitorSelection,
    MouseButton, NamedKey, PhysicalKey, VideoModeInfo, WindowMode,
};
```

**Step 2: Export from `src/lib.rs` prelude**

```rust
pub use crate::core::{
    AcquisitionMethod, DeviceSelector, DisplayBackend, Frame, GPUPreference, InputEvent, KeyCode,
    MonitorInfo, MonitorSelection, MouseButton, NamedKey, PresentMode, RenderContext,
    SwapchainConfig, SwapchainManager, VSEContext, VSEContextBuilder, VSEError, VideoModeInfo,
    WindowMode,
};
```

**Step 3: Verify**

```bash
cargo check
```
Expected: clean.

**Step 4: Commit**

```bash
git add src/core/mod.rs src/lib.rs
git commit -m "feat: export AcquisitionMethod from prelude"
```

---

## Task 15: Example `08_direct_display.rs`

**Files:**
- Create: `examples/08_direct_display.rs`
- Modify: `Cargo.toml` (add `[[example]]` entry)

**Step 1: Write the example**

```rust
//! Direct Display Mode Example
//!
//! Demonstrates VSE's direct display mode, which bypasses the OS compositor
//! for sub-millisecond timing precision.
//!
//! # Setup
//!
//! See `docs/guides/display_backends.md` for prerequisites (video group,
//! TTY mode, or X11 session requirements).
//!
//! # Running
//!
//! ```bash
//! cargo run --example 08_direct_display
//! ```

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    println!("VisionStimulusEngine - Direct Display Example");
    println!("=============================================");
    println!();
    println!("Acquiring display... (this may fail if prerequisites are not met)");
    println!("See docs/guides/display_backends.md for setup instructions.");
    println!();

    let context = VSEContext::builder()
        .with_window_mode(WindowMode::DirectDisplay)
        .with_monitor(MonitorSelection::Primary)
        .with_clear_color(0.1, 0.1, 0.1, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .with_flip_logging(true)
        .build()?;

    let mut frame_count: u64 = 0;

    context.run(move |vse| {
        if frame_count == 0 {
            let backend = vse.display_backend();
            println!("Acquisition successful!");
            println!("Backend: {}", backend.description());
            let (w, h) = vse.window_size();
            println!("Display: {}x{}", w, h);
            println!();
            println!("Press Escape to exit.");
        }

        // Exit on Escape
        if vse.key_just_pressed(KeyCode::Escape) {
            println!("Escape pressed — exiting.");
            return Err(VSEError::EventLoop("User requested exit".into()));
        }

        // Draw a moving white bar to visually confirm rendering
        let (w, h) = vse.window_size();
        let bar_y = ((frame_count % h as u64) as f32) - 4.0;
        vse.draw_rect(0.0, bar_y, w as f32, bar_y + 8.0, Color::WHITE);

        vse.clear()?;
        let _info = vse.flip(None)?;

        frame_count += 1;

        if frame_count % 600 == 0 {
            println!("Frame {}", frame_count);
        }

        Ok(())
    })?;

    println!("Clean shutdown.");
    Ok(())
}
```

**Step 2: Add to `Cargo.toml`**

```toml
[[example]]
name = "08_direct_display"
path = "examples/08_direct_display.rs"
```

**Step 3: Verify it compiles**

```bash
cargo build --example 08_direct_display
```
Expected: clean build (the example itself may not run without the prerequisites).

**Step 4: Commit**

```bash
git add examples/08_direct_display.rs Cargo.toml
git commit -m "feat: add direct display example (08_direct_display)"
```

---

## Task 16: Write `docs/guides/display_backends.md`

**Files:**
- Create: `docs/guides/display_backends.md`

**Step 1: Write the documentation**

```markdown
# Display Backends and Direct Display Mode

This guide explains how VSE interacts with the display stack on Linux,
what compositors are, and how to set up direct display mode for
timing-critical experiments.

---

## What Is a Compositor?

A compositor is the OS process responsible for managing all on-screen windows
and driving the physical display. When your application renders a frame, it
does not write directly to the screen — it hands a GPU buffer to the
compositor, which decides when and how to present it alongside other windows.

The compositor sits between your application and the display hardware:

```
Your App → GPU buffer → Compositor → Display Controller → Monitor
```

This design is great for general-purpose desktops (smooth window management,
effects, multi-app coordination) but introduces problems for timing-critical
experiments: the compositor schedules scanout independently of your
`flip()` call, adding jitter that can exceed 1 ms.

---

## Common Compositors by Environment

| Desktop Environment | Compositor | Notes |
|---|---|---|
| GNOME (Ubuntu default) | Mutter | Wayland-native; XWayland for X11 apps |
| KDE Plasma | KWin | Wayland or X11 mode |
| Ubuntu 22.04+ | Mutter + XWayland | Default session is Wayland |
| Ubuntu 20.04 | Mutter (X11 mode) | X11 session with compositor |
| Sway / Hyprland | sway / Hyprland | Tiling Wayland compositors |
| Headless / TTY | None | No compositor — ideal for experiments |

---

## VSE Display Backends

VSE reports the active backend via `vse.display_backend()`:

### `DisplayBackend::Wayland`

Your app is a native Wayland client. The compositor (Mutter, KWin) mediates
all frame presentation. `VK_GOOGLE_display_timing` gives feedback about actual
scanout times, but the compositor still controls scheduling. Typical jitter:
0.5–2 ms.

### `DisplayBackend::X11`

Your app is using the X11 protocol. On modern Ubuntu this means XWayland: an
X11 compatibility server running inside the Wayland compositor, adding an extra
hop. Timing jitter is higher than native Wayland.

### `DisplayBackend::DirectDisplay`

VSE has bypassed the compositor entirely via `VK_KHR_display`. Your GPU writes
directly to the display controller's scanout buffer. The compositor has no
involvement in frame delivery. Timing jitter is limited only by GPU and display
hardware. This is the recommended mode for EEG/MEG and neural recording
experiments.

---

## Direct Display Mode

### When to Use It

- Neural recording experiments where stimulus onset timing must be accurate
  to < 1 ms
- Experiments where `FlipLogger` data shows excessive jitter in compositor mode
- Labs with dedicated stimulus PCs separate from the experimenter's workstation

### How It Works

VSE uses the Vulkan `VK_KHR_display` extension to create a display surface
that talks directly to the display controller, bypassing the compositor. Three
acquisition methods are tried automatically:

1. **No-compositor** — If no compositor is running (TTY session), the display
   is unclaimed and VSE takes it directly. Simplest setup.

2. **DRM acquire** (`VK_EXT_acquire_drm_display`) — VSE opens the GPU's DRM
   device file and calls `vkAcquireDrmDisplayEXT`. Works in a Wayland session
   if the user has permission.

3. **Xlib acquire** (`VK_EXT_acquire_xlib_display`) — VSE connects to the
   X server via `libX11.so` (loaded at runtime) and calls
   `vkAcquireXlibDisplayEXT`. Works in X11/XWayland sessions.

---

## Setup Instructions

### Method 1: TTY Session (Simplest)

Press **Ctrl+Alt+F2** to switch to a TTY before running your experiment.
Log in, then run VSE. No compositor, no permissions required.

```bash
# From TTY:
cargo run --release --example 08_direct_display
```

Switch back to your desktop with **Ctrl+Alt+F1** (or F7 on some systems).

### Method 2: DRM Acquire (Wayland / Desktop Session)

Add your user to the `video` group, then re-login:

```bash
sudo usermod -aG video $USER
# Log out and log back in
```

Verify group membership:

```bash
groups | grep video
```

Then run VSE normally from your desktop session. The DRM acquire probe will
succeed automatically.

### Method 3: Xlib Acquire (X11 / XWayland Session)

If `DISPLAY` is set and you are in an X11 or XWayland session, VSE will
attempt Xlib acquire automatically. No additional setup required, but
it may require the video group permission as well depending on your compositor.

---

## Troubleshooting

**Error: "No unclaimed display found — a compositor may be holding DRM master"**
→ Try Method 1 (TTY) or Method 2 (video group + DRM acquire).

**Error: "permission denied on /dev/dri/card0"**
→ Run `sudo usermod -aG video $USER` and re-login.

**Error: "libX11.so.6 not found"**
→ Install X11: `sudo apt install libx11-6`

**Error: "XOpenDisplay returned NULL"**
→ `DISPLAY` env var is not set. Either set it (`export DISPLAY=:0`) or use
Method 1 or Method 2 instead.

**Error: "VK_KHR_display not supported"**
→ Your Vulkan driver does not support direct display. Update GPU drivers:
  - NVIDIA: install latest proprietary driver
  - AMD: update Mesa (`sudo apt upgrade`)
  - Intel: update Mesa (`sudo apt upgrade`)

---

## Input in Direct Display Mode

Without a window manager, keyboard and mouse events are sourced directly from
the Linux input subsystem (`/dev/input/event*`). VSE uses the `evdev` interface
and maps events to the same `key_just_pressed()` / `mouse_position()` API as
compositor modes.

**Permission requirement:** Your user must be in the `input` group:

```bash
sudo usermod -aG input $USER
# Log out and log back in
```

If no input devices can be opened, VSE logs a warning but continues — useful
for scripted experiments with no interactive input.

---

## Further Reading

- Vulkan specification: `VK_KHR_display` extension
- Vulkan specification: `VK_EXT_acquire_drm_display`
- Linux DRM/KMS documentation: https://www.kernel.org/doc/html/latest/gpu/drm-kms.html
- Psychtoolbox priority mode (conceptual reference):
  https://psychtoolbox.org/docs/Priority
```

**Step 2: Commit**

```bash
git add docs/guides/display_backends.md
git commit -m "docs: add display backends and direct display setup guide"
```

---

## Final Verification

```bash
cargo test
cargo clippy --all-targets
cargo fmt --check
cargo build --example 08_direct_display
```

All should be clean. The example itself requires hardware prerequisites to run
(`#[ignore]`-equivalent — run manually in a TTY or video-group session).
