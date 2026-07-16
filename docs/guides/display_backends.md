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
experiments because the compositor schedules scanout independently of your
`flip()` call.

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
all frame presentation. `VK_EXT_present_timing` can still provide hardware-anchored
feedback for the compositor-presented frame, but the compositor remains in the path.
Typical jitter: 0.5–2 ms.

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
