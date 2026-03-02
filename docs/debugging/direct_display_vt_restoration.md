# Direct Display: VT Restoration After Exit — Solved

## Status
**Solved.** The program exits cleanly and the terminal is restored automatically.

## Environment
- Machine: ThinkPad (ThinkPad Extra Buttons device present)
- GPU: Intel UHD Graphics 620 (WHL GT2) — i915 driver
- Display: 2560×1440 @ 60 Hz
- Acquisition path: NoCompositor (`VK_KHR_display`)
- OS: Linux (TTY session — bare VT, no compositor)
- Kernel: 6.17.0-14-generic

## Root Cause

Two separate bugs, both now fixed:

### Bug 1: Black screen after exit

After Vulkan releases DRM, the kernel transfers DRM master back to fbcon
asynchronously (~5–20 ms on i915/drm-fbdev-generic). The VT layer (`VT_WAITACTIVE`)
becomes ready quickly, but fbcon's GEM framebuffer is **not yet wired to the CRTC
scanout plane**. Writing to `/dev/tty` at this point updates fbcon's virtual text
buffer, but the deferred dirty-copy never produces a visible result because the
CRTC is not scanning fbcon's buffer.

**Fix**: After `FBIO_WAITFORVSYNC` confirms the CRTC is active, call
`FBIOBLANK(FB_BLANK_UNBLANK)` on `/dev/fb0`. This triggers
`drm_client_modeset_commit()` which performs the DRM atomic commit that wires
fbcon's GEM buffer to the CRTC. Subsequent tty writes then produce visible output.

**What was tried and why it failed**:
- `\r\n` write to /dev/tty — too small a dirty region, but even `\x1b[H\x1b[2J` (full clear) failed for the same underlying reason: fbcon didn't have the CRTC yet
- 1000 ms fixed sleep — DRM handoff takes 2–5 s on this hardware before the scanout is usable without FBIOBLANK
- `VT_SETMODE`, `VT_ACTIVATE`, `KDSETMODE` — TTY-layer ioctls; they don't reach the DRM layer at all

**What confirmed the root cause**:
- `FBIO_WAITFORVSYNC` returned 0 in ~5–13 ms — fbcon had an active CRTC
- But writes to `/dev/tty` still didn't appear (7 ms write time, fbcon processed it)
- Adding `FBIOBLANK(0)` (~17 ms / one vsync to complete) immediately fixed the screen

### Bug 2: First character consumed after exit

After exit, the shell's readline consumed one extra character (users had to type
`cclear` instead of `clear`).

**Cause**: evdev reads key events from `/dev/input/event*` directly, bypassing the
VT TTY layer. The VT *also* buffers every keypress. When the program exits, the
Escape keypress used to quit is still in the TTY input buffer. The shell's readline
reads it, interprets the next character as an escape sequence (ESC + c = M-c =
capitalize-word), and discards both.

**Fix**: `tcflush(tty_fd, TCIFLUSH)` before exit clears the TTY input queue.

## Working Restoration Sequence (`src/core/context.rs`, `run_direct()`)

```
drop(state)                          // Vulkan releases DRM (~150 ms teardown)
  └─ poll /dev/fb0 FBIO_WAITFORVSYNC // wait for fbcon to have active CRTC
  └─ FBIOBLANK(FB_BLANK_UNBLANK)     // force drm_client_modeset_commit()
  └─ open /dev/tty
      └─ tcflush(TCIFLUSH)           // clear buffered keystrokes
      └─ KDSETMODE(KD_TEXT)          // belt-and-suspenders: text mode
      └─ VT_ACTIVATE + VT_WAITACTIVE // re-initialise VT
      └─ write("\x1b[H\x1b[2J")     // clear screen, push scanout update
```

Total time from Escape keypress to terminal restore: ~25–30 ms.

## Key ioctl Values
- `FBIO_WAITFORVSYNC` = `0x40044620` (`_IOW('F', 0x20, u32)`)
- `FBIOBLANK`         = `0x4611`     (`_IO('F', 0x11)`)
- `KDSETMODE`         = `0x4B3A`
- `VT_GETSTATE`       = `0x5603`
- `VT_ACTIVATE`       = `0x5606`
- `VT_WAITACTIVE`     = `0x5607`
