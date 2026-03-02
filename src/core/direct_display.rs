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
use vulkano::swapchain::{Surface, SurfaceApi};
use vulkano::VulkanObject;

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

// ─── Shared ash setup helper ────────────────────────────────────────────────

/// Construct an ash Entry + ash Instance backed by vulkano's already-loaded library.
fn make_ash_objects(instance: &Arc<Instance>) -> Result<(ash::Entry, ash::Instance), String> {
    // Re-open libvulkan via ash (it's already in memory from vulkano).
    let entry =
        unsafe { ash::Entry::load() }.map_err(|e| format!("ash::Entry::load failed: {}", e))?;

    // Construct an ash::Instance whose function pointers come from vulkano's loader.
    let vk_instance = instance.handle();
    let ash_instance = unsafe {
        ash::Instance::load_with(
            |name| {
                std::mem::transmute(
                    instance
                        .library()
                        .get_instance_proc_addr(vk_instance, name.as_ptr()),
                )
            },
            vk_instance,
        )
    };
    Ok((entry, ash_instance))
}

// ─── Helper: wrap raw VkSurfaceKHR ──────────────────────────────────────────

/// Wrap a raw `ash::vk::SurfaceKHR` into a `vulkano::swapchain::Surface`.
fn wrap_surface(instance: &Arc<Instance>, surface_handle: ash::vk::SurfaceKHR) -> Arc<Surface> {
    // Surface::from_handle returns Self directly (not Result).
    Arc::new(unsafe {
        Surface::from_handle(
            Arc::clone(instance),
            surface_handle,
            SurfaceApi::DisplayPlane,
            None,
        )
    })
}

// ─── Helper: select display and video mode ───────────────────────────────────

/// Select the display matching `target_name` (substring, case-insensitive),
/// or the first display if `target_name` is None.
fn select_display_index(
    displays: &[vk::DisplayPropertiesKHR],
    target_name: Option<&str>,
) -> Option<usize> {
    if let Some(name) = target_name {
        let name_lower = name.to_lowercase();
        displays.iter().position(|d| {
            let n = unsafe { std::ffi::CStr::from_ptr(d.display_name) }
                .to_string_lossy()
                .to_lowercase();
            n.contains(&name_lower)
        })
    } else {
        if displays.is_empty() {
            None
        } else {
            Some(0)
        }
    }
}

/// Select a video mode. Uses override if provided, otherwise highest refresh
/// rate at the largest resolution.
fn select_video_mode(
    modes: &[vk::DisplayModePropertiesKHR],
    override_: Option<(u32, u32, f64)>,
) -> Option<usize> {
    if let Some((w, h, hz)) = override_ {
        let target_millihertz = (hz * 1000.0) as u32;
        modes.iter().position(|m| {
            m.parameters.visible_region.width == w
                && m.parameters.visible_region.height == h
                && (m.parameters.refresh_rate as i32 - target_millihertz as i32).abs() < 500
        })
    } else {
        modes
            .iter()
            .enumerate()
            .max_by_key(|(_, m)| {
                let area = m.parameters.visible_region.width * m.parameters.visible_region.height;
                (area, m.parameters.refresh_rate)
            })
            .map(|(i, _)| i)
    }
}

// ─── Common display surface creation (after acquisition) ────────────────────

/// Create a VkDisplaySurfaceKHR and wrap it as a vulkano Surface.
/// Called by all probes after they've verified the display is available.
unsafe fn create_display_surface(
    khr_display: &ash::khr::display::Instance,
    instance: &Arc<Instance>,
    physical_device: ash::vk::PhysicalDevice,
    display: ash::vk::DisplayKHR,
    method: AcquisitionMethod,
    video_mode_override: Option<(u32, u32, f64)>,
) -> Result<DirectDisplaySurface, ProbeFailure> {
    let modes = khr_display
        .get_display_mode_properties(physical_device, display)
        .map_err(|e| ProbeFailure {
            method,
            reason: format!("vkGetDisplayModePropertiesKHR failed: {:?}", e),
        })?;

    let mode_idx = select_video_mode(&modes, video_mode_override).ok_or_else(|| ProbeFailure {
        method,
        reason: "no video modes available for this display".to_string(),
    })?;

    let mode_props = &modes[mode_idx];
    let mode = mode_props.display_mode;
    let width = mode_props.parameters.visible_region.width;
    let height = mode_props.parameters.visible_region.height;
    let refresh_rate_hz = mode_props.parameters.refresh_rate as f64 / 1000.0;

    let planes = khr_display
        .get_physical_device_display_plane_properties(physical_device)
        .map_err(|e| ProbeFailure {
            method,
            reason: format!("vkGetPhysicalDeviceDisplayPlanePropertiesKHR: {:?}", e),
        })?;

    if planes.is_empty() {
        return Err(ProbeFailure {
            method,
            reason: "no display planes available".to_string(),
        });
    }

    let surface_info = vk::DisplaySurfaceCreateInfoKHR::default()
        .display_mode(mode)
        .plane_index(0)
        .plane_stack_index(planes[0].current_stack_index)
        .transform(vk::SurfaceTransformFlagsKHR::IDENTITY)
        .global_alpha(1.0)
        .alpha_mode(vk::DisplayPlaneAlphaFlagsKHR::OPAQUE)
        .image_extent(vk::Extent2D { width, height });

    let surface_handle = khr_display
        .create_display_plane_surface(&surface_info, None)
        .map_err(|e| ProbeFailure {
            method,
            reason: format!(
                "vkCreateDisplayPlaneSurfaceKHR failed: {:?} \
                 (compositor may be holding the display)",
                e
            ),
        })?;

    let surface = wrap_surface(instance, surface_handle);

    Ok(DirectDisplaySurface {
        surface,
        method,
        width,
        height,
        refresh_rate_hz,
    })
}

// ─── Probe: No-compositor ────────────────────────────────────────────────────

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
    let (entry, ash_instance) = make_ash_objects(instance).map_err(|e| ProbeFailure {
        method: AcquisitionMethod::NoCompositor,
        reason: e,
    })?;

    let khr_display = ash::khr::display::Instance::new(&entry, &ash_instance);

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

    let display_idx = select_display_index(&displays, target_name).ok_or_else(|| ProbeFailure {
        method: AcquisitionMethod::NoCompositor,
        reason: format!("no display matching {:?} found", target_name),
    })?;

    let display_props = &displays[display_idx];
    let display = display_props.display;
    let display_name = unsafe {
        std::ffi::CStr::from_ptr(display_props.display_name)
            .to_string_lossy()
            .into_owned()
    };

    let result = unsafe {
        create_display_surface(
            &khr_display,
            instance,
            physical_device,
            display,
            AcquisitionMethod::NoCompositor,
            video_mode_override,
        )
    }?;

    info!(
        "Direct display: acquired {} via no-compositor path ({} x {} @ {:.1} Hz)",
        display_name, result.width, result.height, result.refresh_rate_hz
    );

    Ok(result)
}

// ─── Probe: DRM acquire ──────────────────────────────────────────────────────

/// Find the `/dev/dri/cardN` path that corresponds to a Vulkan physical device.
///
/// Uses `VK_EXT_physical_device_drm` when the device supports it: this
/// extension exposes the DRM primary minor number directly, so the correct card
/// is selected even when multiple GPUs are present.
///
/// Falls back to returning the first existing `/dev/dri/cardN` when the
/// extension is absent (reliable on single-GPU systems; may choose wrongly with
/// multiple GPUs if the extension is unavailable).
fn find_drm_card_path(
    ash_instance: &ash::Instance,
    physical_device: ash::vk::PhysicalDevice,
) -> Option<std::path::PathBuf> {
    // Check whether VK_EXT_physical_device_drm is supported by this device.
    let exts = unsafe {
        ash_instance
            .enumerate_device_extension_properties(physical_device)
            .unwrap_or_default()
    };
    let has_drm_ext = exts.iter().any(|e| {
        unsafe { std::ffi::CStr::from_ptr(e.extension_name.as_ptr()) }.to_bytes()
            == b"VK_EXT_physical_device_drm"
    });

    if has_drm_ext {
        let mut drm_props = ash::vk::PhysicalDeviceDrmPropertiesEXT::default();
        let mut props2 =
            ash::vk::PhysicalDeviceProperties2::default().push_next(&mut drm_props);
        unsafe {
            ash_instance.get_physical_device_properties2(physical_device, &mut props2);
        }
        if drm_props.has_primary == ash::vk::TRUE {
            let path =
                std::path::PathBuf::from(format!("/dev/dri/card{}", drm_props.primary_minor));
            if path.exists() {
                return Some(path);
            }
        }
    }

    // Fallback: return the first /dev/dri/cardN that exists.
    for i in 0..16u32 {
        let path = std::path::PathBuf::from(format!("/dev/dri/card{}", i));
        if path.exists() {
            return Some(path);
        }
    }

    None
}

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
    let (entry, ash_instance) = make_ash_objects(instance).map_err(|e| ProbeFailure {
        method: AcquisitionMethod::DrmAcquire,
        reason: e,
    })?;

    let khr_display = ash::khr::display::Instance::new(&entry, &ash_instance);
    let ext_drm = ash::ext::acquire_drm_display::Instance::new(&entry, &ash_instance);

    // Find the DRM card for this GPU. Uses VK_EXT_physical_device_drm when
    // available so the correct card is selected even with multiple GPUs present.
    let drm_path = find_drm_card_path(&ash_instance, physical_device).ok_or_else(|| {
        ProbeFailure {
            method: AcquisitionMethod::DrmAcquire,
            reason: "no DRM card found in /dev/dri/ (is the DRM driver loaded?)".to_string(),
        }
    })?;
    let drm_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&drm_path)
        .map_err(|e| ProbeFailure {
            method: AcquisitionMethod::DrmAcquire,
            reason: format!(
                "permission denied on {} — try: sudo usermod -aG video $USER \
                 (re-login required). OS error: {}",
                drm_path.display(),
                e
            ),
        })?;

    use std::os::unix::io::AsRawFd;
    let drm_fd = drm_file.as_raw_fd();

    let displays = unsafe {
        khr_display
            .get_physical_device_display_properties(physical_device)
            .map_err(|e| ProbeFailure {
                method: AcquisitionMethod::DrmAcquire,
                reason: format!("vkGetPhysicalDeviceDisplayPropertiesKHR: {:?}", e),
            })?
    };

    let display_idx = select_display_index(&displays, target_name).ok_or_else(|| ProbeFailure {
        method: AcquisitionMethod::DrmAcquire,
        reason: "no matching display found".to_string(),
    })?;

    let display = displays[display_idx].display;

    unsafe {
        ext_drm
            .acquire_drm_display(physical_device, drm_fd, display)
            .map_err(|e| ProbeFailure {
                method: AcquisitionMethod::DrmAcquire,
                reason: format!("vkAcquireDrmDisplayEXT failed: {:?}", e),
            })?;
    }

    info!("Direct display: DRM acquire succeeded on {}", drm_path.display());

    let result = unsafe {
        create_display_surface(
            &khr_display,
            instance,
            physical_device,
            display,
            AcquisitionMethod::DrmAcquire,
            video_mode_override,
        )
    }?;

    Ok(result)
}

// ─── Probe: Xlib acquire ─────────────────────────────────────────────────────

/// Acquire display via VK_EXT_acquire_xlib_display using libloading.
///
/// Dynamically loads libX11.so at runtime — no build-time X11 headers
/// required. Returns ProbeFailure if the libraries are absent.
fn probe_xlib_acquire(
    instance: &Arc<Instance>,
    physical_device: ash::vk::PhysicalDevice,
    target_name: Option<&str>,
    video_mode_override: Option<(u32, u32, f64)>,
) -> Result<DirectDisplaySurface, ProbeFailure> {
    let (entry, ash_instance) = make_ash_objects(instance).map_err(|e| ProbeFailure {
        method: AcquisitionMethod::XlibAcquire,
        reason: e,
    })?;

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

    let khr_display = ash::khr::display::Instance::new(&entry, &ash_instance);
    let ext_xlib = ash::ext::acquire_xlib_display::Instance::new(&entry, &ash_instance);

    let displays = unsafe {
        khr_display
            .get_physical_device_display_properties(physical_device)
            .map_err(|e| ProbeFailure {
                method: AcquisitionMethod::XlibAcquire,
                reason: format!("vkGetPhysicalDeviceDisplayPropertiesKHR: {:?}", e),
            })?
    };

    let display_idx = select_display_index(&displays, target_name).ok_or_else(|| ProbeFailure {
        method: AcquisitionMethod::XlibAcquire,
        reason: "no matching display found".to_string(),
    })?;

    let display = displays[display_idx].display;

    // ash 0.38: no wrapper method — call the function pointer directly.
    let result = unsafe {
        (ext_xlib.fp().acquire_xlib_display_ext)(physical_device, x_display as *mut _, display)
    };
    if result != ash::vk::Result::SUCCESS {
        return Err(ProbeFailure {
            method: AcquisitionMethod::XlibAcquire,
            reason: format!(
                "vkAcquireXlibDisplayEXT failed — RandR output not found or X server \
                 denied: {:?}",
                result
            ),
        });
    }

    info!("Direct display: Xlib acquire succeeded");

    let result = unsafe {
        create_display_surface(
            &khr_display,
            instance,
            physical_device,
            display,
            AcquisitionMethod::XlibAcquire,
            video_mode_override,
        )
    }?;

    Ok(result)
}

// ─── Error message formatting ────────────────────────────────────────────────

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

// ─── Public API: cascade orchestrator ────────────────────────────────────────

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
    Err(crate::core::context::VSEError::DirectDisplayUnavailable(
        msg,
    ))
}

/// Default probe order.
pub(crate) fn default_acquisition_order() -> Vec<AcquisitionMethod> {
    vec![
        AcquisitionMethod::NoCompositor,
        AcquisitionMethod::DrmAcquire,
        AcquisitionMethod::XlibAcquire,
    ]
}

// ─── Tests ───────────────────────────────────────────────────────────────────

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
