//! Headless regeneration — reproduce a session's stimuli with no display.
//!
//! Headless mode renders through the *same* [`Renderer`] path a windowed
//! session uses, into an offscreen image that is read back after every flip.
//! Its purpose is post-hoc reproducibility: months after an experiment ran, you
//! still have its recorded metadata and its render closure, and you want the
//! exact frames the subject saw — as pixels a model can consume.
//!
//! This example does both halves of that workflow:
//!
//! 1. **Record.** Run a short stimulus sequence and write the session's
//!    `HostInfo` to `host_info.json`. In a real experiment this is the metadata
//!    a *displayed* session recorded alongside its data.
//! 2. **Regenerate.** Rebuild the render target from that JSON with
//!    [`VSEContextBuilder::with_headless_from_host_info`] — color format,
//!    extent, and pipeline suite all come from the recording, not from
//!    hand-passed arguments — replay the same closure, and write each frame to
//!    a PNG plus a hash.
//!
//! Run it twice: the frame hashes are identical, because every stimulus is a
//! pure function of its frame number.
//!
//! # What headless does and does not promise
//!
//! Frames regenerate identically on the **same machine and driver**.
//! Rasterization is not bit-guaranteed across GPU vendors or driver versions,
//! so compare hashes within a machine, not across a lab.
//!
//! Flip timestamps are **synthesized**, not measured — there is no display and
//! no scanout clock. They are tagged [`TimingSource::Offscreen`] so a
//! regenerated data file can never be mistaken for a recorded one.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 23_headless_regeneration
//! # Then, to confirm determinism:
//! cargo run --release --example 23_headless_regeneration
//! ```
//!
//! No display, compositor, or window is needed — this runs over SSH.

use std::path::{Path, PathBuf};

use vision_stimulus_engine::core::CapturedFrame;
use vision_stimulus_engine::prelude::*;

const WIDTH: u32 = 256;
const HEIGHT: u32 = 256;
const FRAMES: u64 = 8;

/// FNV-1a 64-bit, matching `08_reproducibility_hash`.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The stimulus: a drifting grating with a fixation dot and a photodiode patch.
///
/// A pure function of `frame` — nothing here reads a clock, a random source, or
/// any state outside its arguments. That is what makes the sequence
/// regenerable at all; a closure that advanced on wall-clock time would produce
/// a different stimulus on every run, headless or not.
fn draw_frame(vse: &mut RenderContext, frame: u64) -> Result<(), VSEError> {
    let phase = frame as f32 * 0.25;
    vse.draw_grating(
        32.0,
        32.0,
        224.0,
        224.0,
        &GratingParams {
            frequency: 0.03,
            orientation: 0.4,
            phase,
            contrast: 0.9,
            ..Default::default()
        },
    );
    // Fixation dot, drawn after the grating so it composites on top.
    vse.draw_circle(128.0, 128.0, 5.0, Color::WHITE);
    // Photodiode patch: white on even frames, black on odd — the physical
    // signal that ties stimulus onset to an acquisition clock in a real
    // session, and a visible per-frame difference here.
    let patch = if frame % 2 == 0 {
        Color::WHITE
    } else {
        Color::BLACK
    };
    vse.draw_rect(0.0, 0.0, 24.0, 24.0, patch);
    Ok(())
}

/// Write one captured frame as an RGBA PNG.
fn write_png(frame: &CapturedFrame, dir: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = dir.join(format!("frame_{:03}.png", frame.frame_number()));
    let buffer = image::RgbaImage::from_raw(frame.width(), frame.height(), frame.to_rgba8())
        .ok_or("captured frame did not fill an RGBA buffer")?;
    buffer.save(&path)?;
    Ok(path)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from("headless_regeneration_out");
    std::fs::create_dir_all(&out_dir)?;
    let info_path = out_dir.join("host_info.json");

    // ---- 1. Record -------------------------------------------------------
    // Stands in for a displayed session: what matters is that it writes the
    // HostInfo describing exactly what it rendered into.
    println!("== recording ==");
    let mut recording = VSEContext::builder()
        .with_headless(WIDTH, HEIGHT)
        .with_clear_color(0.5, 0.5, 0.5, 1.0)
        .build_headless()?;

    let host_info = recording.capture_host_info();
    std::fs::write(&info_path, serde_json::to_string_pretty(&host_info)?)?;
    println!(
        "wrote {} — target {:?} {}x{}",
        info_path.display(),
        host_info.swapchain.image_format,
        host_info.swapchain.extent[0],
        host_info.swapchain.extent[1],
    );

    let mut recorded_hashes = Vec::new();
    let mut frame = 0u64;
    recording.run_headless(
        |captured| {
            recorded_hashes.push(fnv1a(captured.bytes()));
            Ok(())
        },
        |vse| {
            draw_frame(vse, frame)?;
            vse.flip(None)?;
            frame += 1;
            if frame == FRAMES {
                vse.request_exit();
            }
            Ok(())
        },
    )?;

    // ---- 2. Regenerate ---------------------------------------------------
    // Everything about the render target comes from the recording. Passing the
    // extent and format by hand instead is the easiest way to silently produce
    // pixels that are not the ones the subject saw.
    println!("\n== regenerating from {} ==", info_path.display());
    let recovered: HostInfo = serde_json::from_str(&std::fs::read_to_string(&info_path)?)?;

    let mut regenerated = VSEContext::builder()
        .with_headless_from_host_info(&recovered)?
        .with_clear_color(0.5, 0.5, 0.5, 1.0)
        .build_headless()?;

    let mut regenerated_hashes = Vec::new();
    let mut png_error = None;
    let mut frame = 0u64;
    regenerated.run_headless(
        |captured| {
            regenerated_hashes.push(fnv1a(captured.bytes()));
            match write_png(captured, &out_dir) {
                Ok(path) => println!(
                    "frame {:>3}  hash {:#018x}  -> {}",
                    captured.frame_number(),
                    fnv1a(captured.bytes()),
                    path.display()
                ),
                // The sink's error type is VSEError; carry the PNG failure out
                // rather than dressing it up as one.
                Err(e) => {
                    png_error = Some(e.to_string());
                    return Err(VSEError::DataRecording(
                        "failed to write a PNG; see the reported error".into(),
                    ));
                }
            }
            Ok(())
        },
        |vse| {
            draw_frame(vse, frame)?;
            vse.flip(None)?;
            frame += 1;
            if frame == FRAMES {
                vse.request_exit();
            }
            Ok(())
        },
    )?;
    if let Some(e) = png_error {
        return Err(e.into());
    }

    // ---- 3. Verify -------------------------------------------------------
    println!();
    if recorded_hashes == regenerated_hashes {
        println!(
            "OK  {} frames regenerated byte-identically from the recorded metadata",
            recorded_hashes.len()
        );
    } else {
        println!("FAIL  regenerated frames differ from the recorded ones:");
        for (i, (a, b)) in recorded_hashes
            .iter()
            .zip(regenerated_hashes.iter())
            .enumerate()
        {
            if a != b {
                println!("  frame {i}: recorded {a:#018x} != regenerated {b:#018x}");
            }
        }
        std::process::exit(1);
    }

    let distinct: std::collections::BTreeSet<_> = regenerated_hashes.iter().collect();
    if distinct.len() == 1 {
        println!("FAIL  every frame hashed identically — the stimulus is not animating");
        std::process::exit(1);
    }
    println!(
        "OK  {} of {} frames are distinct, so the sequence really is animating",
        distinct.len(),
        regenerated_hashes.len()
    );
    println!("\nRun again and compare the printed hashes — they must match exactly.");

    Ok(())
}
