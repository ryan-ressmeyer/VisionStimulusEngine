# Direct Display Mode — Design Document

**Date:** 2026-02-28
**Status:** Approved
**Feature:** `WindowMode::DirectDisplay`

---

## Motivation

VSE currently routes all frames through the OS compositor (Mutter on GNOME,
KWin on KDE, XWayland on X11 sessions). The compositor schedules scanout
independently of the application's `flip()` call, introducing timing jitter
that is unacceptable for neural recording experiments requiring sub-millisecond
stimulus onset accuracy.

Direct display mode bypasses the compositor entirely by creating a
`VkDisplaySurfaceKHR` (via `VK_KHR_display`) instead of a compositor-mediated
window surface. The GPU writes directly to the display controller's scanout
buffer. This is the same model used by Psychtoolbox's priority mode and custom
C stimulus engines in neurophysiology labs.

---

## Goals

- Provide sub-millisecond timing precision by eliminating the compositor from
  the frame delivery path.
- Require zero new mandatory configuration — `WindowMode::DirectDisplay` is the
  only required change to existing experiment code.
- Automatically probe all available acquisition methods and select the first
  that succeeds, with clear diagnostic logging and error messages at every step.
- Maintain the identical rendering and input API as compositor modes — user
  closure code is unchanged.
- Ship comprehensive documentation (`docs/guides/display_backends.md`) covering
  compositors, display backends, and direct display setup for non-expert users.

---

## Non-Goals

- Windows and macOS support (Linux only for this feature).
- Wayland compositor negotiation / `wp_drm_lease` protocol (future work).
- Variable refresh rate (VRR/FreeSync/G-Sync) support in direct mode (future).

---

## Architecture

### Approach: Converging Init Paths, Shared Run Core

The existing `initialize()` function splits into two named variants. Both
produce the same `VSEState` struct. The render loop body is extracted into a
shared `run_frame()` function called from both the winit event loop and the
direct display custom loop.

```
VSEContext::run()
    │
    ├─ WindowMode::DirectDisplay
    │       └─ initialize_direct()
    │               ├─ probe_no_compositor()      ← try 1
    │               ├─ probe_drm_acquire()         ← try 2
    │               ├─ probe_xlib_acquire()        ← try 3 (via libloading)
    │               └─ VSEError::DirectDisplayUnavailable
    │               │
    │               └─ VkDisplaySurfaceKHR + EvdevReader
    │                       → VSEState { input_source: InputSource::Evdev(..) }
    │
    └─ compositor modes (current path)
            └─ initialize_compositor()
                    └─ winit window + VkWaylandSurface / VkXcbSurface
                            → VSEState { input_source: InputSource::Winit }
    │
    └─ both converge ──→ run_frame(&mut state, &closure)
                              ├─ poll input (winit OR evdev)
                              ├─ call user closure with RenderContext
                              ├─ clear + flip
                              └─ timing / flip logging
```

### Files Changed

| File | Change |
|---|---|
| `src/core/context.rs` | Split `initialize()` → `initialize_compositor()` + `initialize_direct()`; extract `run_frame()` |
| `src/core/direct_display.rs` | **New.** Acquisition cascade, `VkDisplaySurfaceKHR` creation, video mode selection |
| `src/core/evdev_input.rs` | **New.** evdev reader, event mapping to VSE `InputEvent` |
| `src/core/input.rs` | Add `AcquisitionMethod`; update `DisplayBackend` with `DirectDisplay` variant |
| `src/core/device.rs` | Request direct-display instance extensions conditionally |
| `src/core/swapchain.rs` | Accept `VkDisplaySurfaceKHR` as an alternative surface source |
| `docs/guides/display_backends.md` | **New.** Compositor explainer, backend comparison, direct display setup |

---

## Acquisition Cascade

Probes run in order. First success wins. All failure reasons are collected and
included in the error message if all probes fail.

### Probe 1: No-Compositor

Attempts `vkGetPhysicalDeviceDisplayPropertiesKHR` and tries to create a
`VkDisplaySurfaceKHR` directly. Succeeds when no compositor holds DRM master
on the target display — typically when VSE is running in a TTY or on a
dedicated bare-metal stimulus PC.

**Common scenario:** Ctrl+Alt+F2 to TTY before running the experiment.

**Failure reason:** `"display is held by a compositor (WAYLAND_DISPLAY or DISPLAY is set)"`

### Probe 2: DRM Acquire (`VK_EXT_acquire_drm_display`)

Opens `/dev/dri/cardX` for the selected GPU, calls `drmSetMaster()`, then
`vkAcquireDrmDisplayEXT`. Requires the user to be in the `video` group or
running as root.

**Common scenario:** Dedicated stimulus GPU in a multi-GPU system; or a lab
machine with a `udev` rule granting video group DRM master access.

**Failure reason:** `"permission denied on /dev/dri/card0 — try: sudo usermod -aG video $USER (re-login required)"`

### Probe 3: Xlib Acquire (`VK_EXT_acquire_xlib_display`)

Uses `libloading` to open `libX11.so` and `libXrandr.so` at runtime (no
build-time X11 headers required). Calls `XOpenDisplay`, enumerates RandR
outputs to find the target monitor, then `vkAcquireXlibDisplayEXT`. Requires
`DISPLAY` to be set.

**Common scenario:** X11 or XWayland session; the user wants to monopolise one
monitor for the experiment while keeping the desktop on another.

**Failure reason (lib not found):** `"libX11.so not found — X11 not installed"`
**Failure reason (output not found):** `"RandR output not found for this monitor"`

### All Probes Failed

```
Direct display mode unavailable on eDP-1. Tried:
  ✗ No-compositor: display is held by a compositor (WAYLAND_DISPLAY is set)
  ✗ DRM acquire:   permission denied on /dev/dri/card0
                   Fix: sudo usermod -aG video $USER  (then re-login)
  ✗ Xlib acquire:  libX11.so loaded but RandR output not found for this monitor

See docs/guides/display_backends.md for setup instructions.
```

---

## New Types

### `AcquisitionMethod` (public, `src/core/input.rs`)

```rust
pub enum AcquisitionMethod {
    NoCompositor,  // TTY / bare session
    DrmAcquire,    // VK_EXT_acquire_drm_display
    XlibAcquire,   // VK_EXT_acquire_xlib_display (via libloading)
}
```

### `DisplayBackend` updated

```rust
pub enum DisplayBackend {
    Wayland,
    X11,
    Windows,
    MacOS,
    DirectDisplay { method: AcquisitionMethod },  // new
    Unknown,
}
```

### `InputSource` (internal, `src/core/context.rs`)

```rust
enum InputSource {
    Winit,
    Evdev(EvdevReader),
}
```

### `VSEState` addition

```rust
struct VSEState {
    // ... existing fields ...
    input_source: InputSource,  // new
}
```

---

## Evdev Input Handling

`src/core/evdev_input.rs` scans `/dev/input/event*` at init time, opening all
devices that expose keyboard or pointer capabilities. Requires the `input`
group or root access. If no readable devices are found, `initialize_direct()`
logs a warning but does not fail — headless/scripted experiments need no input.

**Event mapping:**

| evdev event | VSE InputEvent |
|---|---|
| `EV_KEY` key codes | `KeyboardInput { key, state }` |
| `EV_KEY` BTN_LEFT / RIGHT / MIDDLE | `MouseButton { button, state }` |
| `EV_REL` REL_X / REL_Y | Accumulated into absolute position (starts at display center, clamped to bounds) |
| `EV_ABS` ABS_X / ABS_Y | Direct absolute position (touchscreen / tablet) |

The `InputState` machinery (`HashSet` press tracking, `begin_frame()`,
`clear_events()`) is reused unchanged. `EvdevReader::poll()` is called at the
top of `run_frame()` in place of winit event dispatch.

**Mouse position:** In compositor mode, the OS provides absolute coordinates.
In direct display mode, `EV_REL` deltas are accumulated from display center.
`mouse_position()` returns the same `(f64, f64)` type regardless.

`EvdevReader` releases all device file handles on drop. No explicit user cleanup.

---

## Builder API

### Required (only change needed)

```rust
VSEContext::builder()
    .with_window_mode(WindowMode::DirectDisplay)
    .build()?
```

### Optional Overrides

```rust
// Select a specific video mode (default: highest refresh at native resolution)
.with_direct_display_video_mode(width: u32, height: u32, refresh_hz: f64)

// Override probe order (default: [NoCompositor, DrmAcquire, XlibAcquire])
.with_acquisition_order(methods: &[AcquisitionMethod])
```

All existing builder methods (`with_clear_color`, `with_present_mode`,
`with_flip_logging`, `with_monitor`) work identically in direct display mode.
`with_cursor_visible` is ignored with a logged warning (no cursor in direct
display mode).

### Startup Logging

```
INFO  Attempting direct display mode on eDP-1...
INFO  Probe 1/3 (no-compositor): display claimed by compositor — skipping
INFO  Probe 2/3 (DRM acquire): success — display acquired via /dev/dri/card0
INFO  Direct display mode active. Video mode: 2560x1440 @ 60 Hz
INFO  Input: /dev/input/event3 (keyboard), /dev/input/event5 (mouse)
```

---

## Error Handling

Two new `VSEError` variants:

```rust
/// All acquisition methods were tried and failed.
DirectDisplayUnavailable(String),

/// Acquisition succeeded but a subsequent setup step failed
/// (e.g., unsupported video mode, swapchain creation failure).
DirectDisplaySetupFailed { method: AcquisitionMethod, reason: String },
```

**Non-Linux:** `WindowMode::DirectDisplay` returns
`DirectDisplayUnavailable("Direct display mode is only supported on Linux")`
at `build()` time, before any Vulkan init.

**Partial success:** If acquisition succeeds but video mode selection fails,
`DirectDisplaySetupFailed` is returned so the user knows acquisition worked
and only the configuration needs adjustment.

---

## New Dependencies

```toml
[target.'cfg(target_os = "linux")'.dependencies]
evdev = "0.12"
libloading = "0.8"   # for runtime X11 dlopen (likely already transitive)
```

No feature flags. No build-time X11 headers. The xlib probe is always
attempted on Linux; if `libX11.so` is absent, the probe cleanly fails and the
cascade continues.

---

## Documentation Deliverable

`docs/guides/display_backends.md` — written alongside the code, covers:

- What a compositor is and what it does to frame timing
- Common compositors by OS / desktop environment (Mutter/GNOME, KWin/KDE,
  Weston, sway, Mutter+XWayland on Ubuntu)
- How VSE interacts with each backend (Wayland, X11/XWayland, DirectDisplay)
- What direct display mode is and when to use it
- Step-by-step setup for each acquisition method (TTY, DRM, Xlib)
- Troubleshooting checklist

---

## Success Criteria

- `WindowMode::DirectDisplay` works on a TTY with no compositor.
- `WindowMode::DirectDisplay` works in a Wayland session on a machine with the
  user in the `video` group (DRM acquire path).
- `WindowMode::DirectDisplay` works in an X11/XWayland session (Xlib path).
- All three failure paths produce actionable error messages.
- `vse.display_backend()` returns `DisplayBackend::DirectDisplay { method }`.
- `key_just_pressed()`, `mouse_position()` work identically to compositor mode.
- Flip timing jitter in direct display mode is measurably lower than in
  compositor mode (validated via `FlipLogger` CSV output).
- `docs/guides/display_backends.md` is complete and accurate.
