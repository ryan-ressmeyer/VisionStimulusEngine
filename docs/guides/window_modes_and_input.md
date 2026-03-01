# Window Modes & Input Handling

This guide covers fullscreen rendering, monitor selection, keyboard/mouse input, and cursor control in VisionStimulusEngine.

## Window Modes

VSE supports three window modes via the builder:

```rust
use vision_stimulus_engine::prelude::*;

// Standard window (default)
let context = VSEContext::builder()
    .with_window_mode(WindowMode::Windowed)
    .with_window_size(800, 600)
    .build()?;

// Borderless fullscreen — good for development
let context = VSEContext::builder()
    .with_window_mode(WindowMode::BorderlessFullscreen)
    .build()?;

// Exclusive fullscreen — lowest latency, use for data collection
let context = VSEContext::builder()
    .with_window_mode(WindowMode::ExclusiveFullscreen)
    .with_window_size(1920, 1080) // resolution hint for video mode selection
    .build()?;
```

You can also switch modes at runtime:

```rust
context.run(move |vse| {
    // Toggle fullscreen with F11
    if vse.key_just_pressed(KeyCode::F11) {
        match vse.window_mode() {
            WindowMode::Windowed => vse.set_window_mode(WindowMode::BorderlessFullscreen),
            _ => vse.set_window_mode(WindowMode::Windowed),
        }
    }
    vse.clear()?;
    vse.flip(None)?;
    Ok(())
})?;
```

## Fullscreen & Compositor Latency

**This section is critical for understanding timing precision in vision science experiments.**

In **windowed** and **borderless fullscreen** modes, all rendering passes through the operating system's compositor:

- **Windows**: Desktop Window Manager (DWM)
- **Linux**: Mutter (GNOME), KWin (KDE), or other Wayland/X11 compositors
- **macOS**: Quartz Compositor

The compositor composites your application's framebuffer with other windows (desktop, taskbar, notifications) before presenting the final image to the display. This process adds **at least one frame of latency** (16.7ms at 60Hz, 6.9ms at 144Hz) and can introduce **variable jitter** when other applications compete for compositor time.

In **exclusive fullscreen** mode, your application's Vulkan swapchain presents **directly to the display's scanout buffer**, bypassing the compositor entirely. This provides:

- **Eliminated compositor latency** — no extra frame of delay
- **Deterministic timing** — no jitter from compositor scheduling
- **Guaranteed vsync ownership** — your application controls the display refresh

### Platform Notes

| Platform | Exclusive Fullscreen | Recommendation |
|----------|---------------------|----------------|
| Linux (X11) | Fully supported | Use for experiments |
| Linux (Wayland) | Not supported — falls back to borderless | Use X11 for timing-critical work |
| Windows | Fully supported | Use for experiments |
| macOS | Supported (with caveats) | Test timing on your hardware |

### Recommendation

- **Data collection / neural recording**: Always use `ExclusiveFullscreen`
- **Development and debugging**: Use `Windowed` or `BorderlessFullscreen`
- **Linux timing-critical experiments**: Run under X11, not Wayland

## Monitor Selection

Vision science rigs often have a dedicated stimulus display (e.g., a high-refresh CRT or LCD) plus a control monitor. VSE supports targeting a specific monitor:

```rust
// Use the primary monitor (default)
.with_monitor(MonitorSelection::Primary)

// Select by index (0-based)
.with_monitor(MonitorSelection::Index(1))

// Select by name (case-insensitive substring match)
.with_monitor(MonitorSelection::Name("ASUS"))
```

If the specified monitor is not found, VSE logs a warning and falls back to the primary monitor.

### Dual-Monitor Lab Setup Example

```rust
let context = VSEContext::builder()
    .with_window_mode(WindowMode::ExclusiveFullscreen)
    .with_monitor(MonitorSelection::Name("ViewPixx"))  // stimulus display
    .with_window_size(1920, 1080)
    .with_present_mode(PresentMode::Fifo)
    .build()?;
```

## Video Mode Enumeration

Query connected monitors and their supported video modes at runtime:

```rust
context.run(move |vse| {
    // List all monitors on first frame
    if vse.frame_number() == 0 {
        for monitor in vse.available_monitors() {
            println!("{}: {}x{} @ {:.0} Hz",
                monitor.name.as_deref().unwrap_or("Unknown"),
                monitor.width, monitor.height,
                monitor.refresh_rate_hz.unwrap_or(0.0));

            for mode in &monitor.video_modes {
                println!("  {}x{} @ {:.1} Hz ({}-bit)",
                    mode.width, mode.height, mode.refresh_rate_hz, mode.bit_depth);
            }
        }
    }

    vse.clear()?;
    vse.flip(None)?;
    Ok(())
})?;
```

### Finding a Specific Refresh Rate

```rust
// Find a monitor supporting 144Hz at 1920x1080
for monitor in vse.available_monitors() {
    let has_144hz = monitor.video_modes.iter().any(|m| {
        m.width == 1920 && m.height == 1080 && m.refresh_rate_hz >= 143.0
    });
    if has_144hz {
        println!("Monitor '{}' supports 144Hz at 1080p",
            monitor.name.as_deref().unwrap_or("Unknown"));
    }
}
```

You can also query video modes for the current monitor or a specific monitor by index:

```rust
let modes = vse.current_monitor_video_modes();
let modes = vse.video_modes(0); // monitor index 0
```

## Polled Input

VSE provides frame-aligned input polling, similar to Psychtoolbox's `KbCheck` and `GetMouse`. State is updated once per frame before your render callback runs.

### Keyboard

```rust
// Is a key currently held down?
if vse.key_pressed(KeyCode::Space) { /* held */ }

// Was a key pressed THIS frame (not held from previous)?
if vse.key_just_pressed(KeyCode::Space) { /* first frame of press */ }

// Was a key released THIS frame?
if vse.key_just_released(KeyCode::KeyA) { /* just let go */ }
```

### Mouse

```rust
// Current cursor position (window-relative pixels, (0,0) = top-left)
let (x, y) = vse.mouse_position();

// Is a mouse button held?
if vse.mouse_button_pressed(MouseButton::Left) { /* held */ }

// Was a button clicked THIS frame?
if vse.mouse_button_just_pressed(MouseButton::Left) { /* clicked */ }
```

### Escape to Quit

The most common pattern — exit when Escape is pressed:

```rust
context.run(move |vse| {
    if vse.key_just_pressed(KeyCode::Escape) {
        return Err(VSEError::EventLoop("User exit".into()));
    }

    vse.clear()?;
    vse.flip(None)?;
    Ok(())
})?;
```

## Event Queue (Timing-Precise Input)

For experiments requiring exact input timing (reaction time measurement), use the event queue instead of polling. Each event carries a `Timestamp` from the VSE `Clock`, making it directly comparable to `FlipInfo` timestamps.

```rust
// Show stimulus
vse.draw_rect(350.0, 250.0, 450.0, 350.0, Color::WHITE);
vse.clear()?;
let stimulus_flip = vse.flip(None)?;

// On a subsequent frame, check for responses
for event in vse.input_events() {
    if let InputEvent::KeyDown { key_code: KeyCode::Space, timestamp, .. } = event {
        let rt = timestamp.duration_since(stimulus_flip.present_time);
        println!("Reaction time: {:.1} ms", rt.as_secs_f64() * 1000.0);
    }
}
```

The event queue is automatically cleared on each `flip()` call, so events are always scoped to the interval between consecutive flips.

Available event types:

| Event | Fields |
|-------|--------|
| `InputEvent::KeyDown` | `key_code`, `logical_key`, `timestamp`, `repeat` |
| `InputEvent::KeyUp` | `key_code`, `logical_key`, `timestamp` |
| `InputEvent::MouseMove` | `x`, `y`, `timestamp` |
| `InputEvent::MouseDown` | `button`, `x`, `y`, `timestamp` |
| `InputEvent::MouseUp` | `button`, `x`, `y`, `timestamp` |
| `InputEvent::MouseWheel` | `delta_x`, `delta_y`, `timestamp` |

Each event includes both the physical `key_code` (layout-independent position on keyboard) and the `logical_key` (what character the key produces given the current layout).

## Cursor Control

```rust
// Hide the cursor
vse.set_cursor_visible(false);

// Show the cursor
vse.set_cursor_visible(true);

// Warp cursor to a position (window-relative pixels)
vse.set_cursor_position(400.0, 300.0);

// Check current visibility
if vse.cursor_visible() { /* visible */ }
```

### Auto-Hide Behavior

By default, the cursor is:
- **Hidden** in `BorderlessFullscreen` and `ExclusiveFullscreen` modes
- **Visible** in `Windowed` mode

Override this with the builder:

```rust
// Force cursor visible even in fullscreen
.with_cursor_visible(true)

// Force cursor hidden even in windowed mode
.with_cursor_visible(false)
```

## Key Reference

Common `KeyCode` values for vision science experiments:

| Key | KeyCode |
|-----|---------|
| Escape | `KeyCode::Escape` |
| Space | `KeyCode::Space` |
| Enter | `KeyCode::Enter` |
| Tab | `KeyCode::Tab` |
| Backspace | `KeyCode::Backspace` |
| Arrow Up | `KeyCode::ArrowUp` |
| Arrow Down | `KeyCode::ArrowDown` |
| Arrow Left | `KeyCode::ArrowLeft` |
| Arrow Right | `KeyCode::ArrowRight` |
| A-Z | `KeyCode::KeyA` ... `KeyCode::KeyZ` |
| 0-9 | `KeyCode::Digit0` ... `KeyCode::Digit9` |
| F1-F12 | `KeyCode::F1` ... `KeyCode::F12` |
| Left Shift | `KeyCode::ShiftLeft` |
| Right Shift | `KeyCode::ShiftRight` |
| Left Ctrl | `KeyCode::ControlLeft` |
| Left Alt | `KeyCode::AltLeft` |
| Numpad 0-9 | `KeyCode::Numpad0` ... `KeyCode::Numpad9` |

These are **physical key codes** (layout-independent). For layout-dependent input (e.g., detecting the character "a" regardless of where it is on the keyboard), use the `logical_key` field on `InputEvent`.
