//! evdev-based input for direct display mode.
//!
//! Reads keyboard and mouse events directly from `/dev/input/event*`,
//! bypassing the window manager. Used when `WindowMode::DirectDisplay`
//! is active and no winit event loop is running.

use crate::core::input::{InputState, KeyCode, MouseButton};
use evdev::{Device, InputEventKind};
use std::os::unix::io::AsRawFd;
use tracing::info;

/// Set a device fd to non-blocking mode so `fetch_events()` returns
/// immediately with whatever is in the kernel buffer instead of sleeping
/// until an event arrives.  Without this, the render loop blocks on the
/// first device that has no pending events (e.g. the headphone jack
/// button device), preventing any frames from being presented.
fn set_nonblocking(device: &Device) {
    unsafe {
        let fd = device.as_raw_fd();
        let flags = libc::fcntl(fd, libc::F_GETFL, 0);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
}

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
                set_nonblocking(&device);
                keyboards.push(device);
            } else if has_rel || has_abs {
                info!("evdev: pointer device: {:?}", device.name());
                set_nonblocking(&device);
                pointers.push(device);
            }
        }

        if keyboards.is_empty() && pointers.is_empty() {
            return Err("No readable input devices found in /dev/input/. \
                 Try: sudo usermod -aG input $USER  (then re-login)"
                .to_string());
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

    /// Set display dimensions so mouse position can be clamped to bounds.
    pub fn set_display_size(&mut self, width: u32, height: u32) {
        self.display_width = width as f64;
        self.display_height = height as f64;
        self.mouse_x = self.display_width / 2.0;
        self.mouse_y = self.display_height / 2.0;
    }

    /// Drain all pending evdev events and feed them into `InputState`.
    pub fn poll(&mut self, input: &mut InputState, clock: &crate::timing::Clock) {
        // Collect events first to avoid overlapping mutable borrows on self.
        let mut all_events: Vec<evdev::InputEvent> = Vec::new();
        for device in &mut self.keyboards {
            if let Ok(iter) = device.fetch_events() {
                all_events.extend(iter);
            }
        }
        for device in &mut self.pointers {
            if let Ok(iter) = device.fetch_events() {
                all_events.extend(iter);
            }
        }
        for ev in all_events {
            self.handle_event(ev, input, clock);
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
                if let Some(btn) = evdev_key_to_mouse_button(key) {
                    let (mx, my) = (self.mouse_x, self.mouse_y);
                    if ev.value() == 1 {
                        input.buttons_down.insert(btn);
                        input.buttons_just_pressed.insert(btn);
                        input
                            .events
                            .push(crate::core::input::InputEvent::MouseDown {
                                button: btn,
                                x: mx,
                                y: my,
                                timestamp,
                            });
                    } else if ev.value() == 0 {
                        input.buttons_down.remove(&btn);
                        input.events.push(crate::core::input::InputEvent::MouseUp {
                            button: btn,
                            x: mx,
                            y: my,
                            timestamp,
                        });
                    }
                } else if let Some(key_code) = evdev_key_to_keycode(key) {
                    let logical_key = winit::keyboard::Key::Unidentified(
                        winit::keyboard::NativeKey::Unidentified,
                    );
                    if ev.value() == 1 {
                        let repeat = input.keys_down.contains(&key_code);
                        input.keys_down.insert(key_code);
                        if !repeat {
                            input.keys_just_pressed.insert(key_code);
                        }
                        input.events.push(crate::core::input::InputEvent::KeyDown {
                            key_code,
                            logical_key,
                            timestamp,
                            repeat,
                        });
                    } else if ev.value() == 0 {
                        input.keys_down.remove(&key_code);
                        input.keys_just_released.insert(key_code);
                        input.events.push(crate::core::input::InputEvent::KeyUp {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evdev_reader_new_returns_ok_or_no_devices() {
        let result = EvdevReader::open();
        let _ = result;
    }

    #[test]
    fn evdev_key_to_vse_keycode_escape() {
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
        let mapped = evdev_key_to_keycode(evdev::Key::KEY_RESERVED);
        assert_eq!(mapped, None);
    }
}
