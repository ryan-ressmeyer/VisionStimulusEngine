# Display backends and direct display

VSE can present through a window system or acquire a physical display through `VK_KHR_display`. The normative distinction between these paths is defined in [Timing conformance](../timing-conformance.md#display-paths).

## Compositor-mediated backends

A normal Wayland or X11 application submits images to a presentation engine controlled by the window system. A compositor may combine the application image with other content before the display controller scans it out.

```text
VSE → swapchain presentation → compositor/window system → display controller → panel
```

`VK_EXT_present_timing` can still be available on a compositor-mediated surface. Its availability does not prove compositor bypass, target enforcement, or photon timing.

VSE reports the active backend through `vse.display_backend()`:

- `DisplayBackend::Wayland` identifies a native Wayland window;
- `DisplayBackend::X11` identifies an X11 or XWayland window;
- `DisplayBackend::DirectDisplay` identifies the `VK_KHR_display` path.

Do not assign a fixed jitter number to a backend name. Compositor version, fullscreen state, other windows, variable refresh, driver, and display configuration all affect measured behavior.

## Direct display

`WindowMode::DirectDisplay` asks VSE to acquire and present to a physical display without a compositor in its swapchain path:

```text
VSE → VK_KHR_display surface → display controller → panel
```

This path removes compositor mediation. It does not guarantee driver scheduling enforcement, an awake panel, scanout feedback, panel latency, or sub-millisecond photon timing. Characterize the complete recording path and use a photodiode when light onset matters.

Direct display is distinct from `WindowMode::ExclusiveFullscreen`. Exclusive fullscreen remains a window-system mode whose behavior is platform-dependent.

## Acquisition methods

VSE tries configured acquisition methods in order:

1. **No compositor.** A free display is acquired directly, commonly from a TTY.
2. **DRM acquire.** `VK_EXT_acquire_drm_display` attempts acquisition through the GPU's DRM device.
3. **Xlib acquire.** `VK_EXT_acquire_xlib_display` attempts acquisition through an X server.

The successful method is recorded in runtime state. Availability and permissions differ across machines.

## TTY workflow

Switch to a spare TTY, log in, and run the direct-display characterization example:

```bash
cargo run --release --example 13_direct_display_scanout
```

Return to the desktop with the appropriate virtual-terminal key for the machine. The example auto-terminates; avoid interrupting it in ways that bypass display restoration.

## Permissions

DRM acquisition may require access to `/dev/dri/card*`. Distribution policy commonly grants this through logind or the `video` group. Direct input uses `/dev/input/event*` and may require logind access or membership in the `input` group.

If no input devices can be opened, VSE logs a warning and can continue for scripted experiments.

## Troubleshooting

**No unclaimed display found**

The compositor or another process may own the display. Use a spare TTY or verify DRM acquisition permissions.

**Permission denied on `/dev/dri/card*`**

Check device ownership, active-session permissions, and local group policy. Re-login after changing groups.

**`libX11.so.6` not found**

Install the runtime X11 library if the Xlib acquisition method is required.

**`XOpenDisplay` returned `NULL`**

The Xlib method has no usable display connection. Use another acquisition method rather than inventing a `DISPLAY` value.

**`VK_KHR_display` unsupported**

The selected Vulkan driver does not expose the required direct-display extension. Use a supported driver or a characterized compositor-mediated path.

## Required characterization

Before collecting timing-critical data:

1. disable screen blanking and power-saving behavior on the stimulus display;
2. run examples 10–13 on the exact display path;
3. capture `HostInfo` after warm-up;
4. inspect advertised surface capabilities separately from observed feedback;
5. characterize unpaced target enforcement if the experiment depends on driver scheduling;
6. disable Vulkan validation layers for the recording run;
7. validate panel output with a photodiode.

Repeat this process after changing the GPU, driver, kernel, compositor, cable path, monitor, refresh mode, or acquisition method.
