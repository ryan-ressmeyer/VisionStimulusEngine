//! Linux DirectDisplay render loop and VT restoration.

#![cfg(target_os = "linux")]

use tracing::{info, warn};

use super::config::VSEError;
use super::context::{RenderContext, VSEContext};
use super::input::AcquisitionMethod;
use super::state::{InputSource, RecordingState};

impl VSEContext {
    /// Run the direct display render loop (no winit).
    #[cfg(target_os = "linux")]
    pub(super) fn run_direct<F>(mut self, mut render_fn: F) -> Result<(), VSEError>
    where
        F: FnMut(&mut RenderContext) -> Result<(), VSEError> + 'static,
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let mut state = Self::initialize_direct(&self.config)?;
        state.recording = self.session.take().map(|session| RecordingState {
            session,
            pending_flip: None,
            last_claimed_frame: None,
        });
        let mut config = self.config;

        // Install a SIGINT (Ctrl+C) handler so the loop can exit cleanly,
        // running all Drop implementations and releasing the display surface
        // before the process terminates.  Without this, Ctrl+C kills the
        // process mid-flight, Vulkan never releases the display, and the
        // TTY is left with a blank screen.
        let quit_flag = Arc::new(AtomicBool::new(false));
        let quit_clone = quit_flag.clone();
        let _ = ctrlc::set_handler(move || {
            quit_clone.store(true, Ordering::SeqCst);
        });

        // Capture whether we need to restore the VT console on exit.
        // Only applies to bare-TTY acquisition paths (not Xlib).
        let restore_vt = matches!(
            state.acquired_display,
            Some(AcquisitionMethod::NoCompositor) | Some(AcquisitionMethod::DrmAcquire)
        );

        let loop_result: Option<VSEError> = loop {
            // Check both the SIGINT flag and any in-callback exit request.
            if quit_flag.load(Ordering::SeqCst) || state.should_close {
                info!("Direct display loop exiting");
                break None;
            }

            if let InputSource::Evdev(ref mut reader) = state.input_source {
                reader.poll(&mut state.input, &state.clock);
            }

            let mut render_ctx = RenderContext {
                state: &mut state,
                config: &mut config,
            };

            if let Err(e) = render_fn(&mut render_ctx) {
                warn!("Render error: {}", e);
                break Some(e);
            }

            // Clear per-frame input state AFTER the callback runs, mirroring
            // the winit path — poll() populates keys_just_pressed, so
            // begin_frame() must not run before the callback or those events
            // are erased before the user ever sees them.
            state.input.begin_frame();
        };

        // Flush final pending flip before dropping
        if let Some(recording) = &mut state.recording {
            recording.on_shutdown();
        }

        // Drop Vulkan state first so the display is released before we
        // attempt to restore the VT text mode.
        drop(state);

        if restore_vt {
            use std::os::unix::io::AsRawFd;

            // Restore the VT text console after Vulkan releases DRM.
            //
            // When drop(state) closes the Vulkan device the kernel transfers
            // DRM master back to fbcon asynchronously (~5–20 ms on i915).
            // Simply writing to /dev/tty is not enough: fbcon's GEM
            // framebuffer is not yet wired to the CRTC scanout plane, so
            // text writes update fbcon's virtual buffer but never appear.
            //
            // Correct sequence:
            //   1. Poll FBIO_WAITFORVSYNC on /dev/fb0 until fbcon has an
            //      active DRM CRTC (condition-based; no fixed sleep needed).
            //   2. FBIOBLANK(FB_BLANK_UNBLANK) on the same fd — triggers
            //      drm_client_modeset_commit(), which performs the atomic
            //      commit that wires fbcon's GEM buffer to the CRTC.
            //   3. Flush the TTY input queue so evdev-captured keystrokes
            //      (e.g. Escape) are not left for the shell to misread.
            //   4. KDSETMODE(KD_TEXT) + VT_ACTIVATE to re-initialise the VT.
            //   5. Write the ANSI clear sequence so the terminal content is
            //      refreshed cleanly.

            // _IOW('F', 0x20, u32)
            const FBIO_WAITFORVSYNC: libc::c_ulong = 0x40044620;
            // _IO('F', 0x11)
            const FBIOBLANK: libc::c_ulong = 0x4611;

            let poll_start = std::time::Instant::now();
            let poll_deadline = poll_start + std::time::Duration::from_secs(5);
            while std::time::Instant::now() < poll_deadline {
                if let Ok(fb) = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open("/dev/fb0")
                {
                    let arg: u32 = 0;
                    let r = unsafe { libc::ioctl(fb.as_raw_fd(), FBIO_WAITFORVSYNC, &arg) };
                    if r == 0 {
                        unsafe { libc::ioctl(fb.as_raw_fd(), FBIOBLANK, 0i32) };
                        info!(
                            "fbcon DRM handoff complete ({}ms)",
                            poll_start.elapsed().as_millis()
                        );
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            const KDSETMODE: libc::c_ulong = 0x4B3A;
            const KD_TEXT: libc::c_int = 0;
            const VT_GETSTATE: libc::c_ulong = 0x5603;
            const VT_ACTIVATE: libc::c_ulong = 0x5606;
            const VT_WAITACTIVE: libc::c_ulong = 0x5607;

            #[repr(C)]
            struct VtStat {
                v_active: libc::c_ushort,
                v_signal: libc::c_ushort,
                v_state: libc::c_ushort,
            }

            if let Ok(mut tty) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
            {
                let fd = tty.as_raw_fd();
                unsafe {
                    libc::tcflush(fd, libc::TCIFLUSH);
                    libc::ioctl(fd, KDSETMODE, KD_TEXT);
                    let mut vtstat = VtStat {
                        v_active: 0,
                        v_signal: 0,
                        v_state: 0,
                    };
                    if libc::ioctl(fd, VT_GETSTATE, &mut vtstat) == 0 {
                        let vt = vtstat.v_active as libc::c_int;
                        libc::ioctl(fd, VT_ACTIVATE, vt);
                        libc::ioctl(fd, VT_WAITACTIVE, vt);
                    }
                }
                use std::io::Write;
                let _ = tty.write_all(b"\x1b[H\x1b[2J");
                info!("VT text mode restored");
            }
        }

        info!("Direct display loop exited");
        match loop_result {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}
