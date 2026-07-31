//! Photodiode sync patch — tie stimulus onset to the acquisition clock.
//!
//! This is VSE's native answer to the trigger-box demos in Psychtoolbox
//! (`PsychRTBoxDemo`, `ReceivingTriggerFromSerialPortDemo`, …) and subsumes the
//! old calibration-square example. See `docs/clock-synchronization.md`.
//!
//! The canonical way to align stimulus onset to an ephys/DAQ clock is *physical*:
//! put a **photodiode on a screen patch** and feed its output into the
//! acquisition box's ADC. VSE's job is only to make onsets deterministic and
//! **known in the scanout clock**, which the photodiode then ties to acquisition
//! time. This demo does exactly that:
//!   * a high-contrast patch in a screen corner toggles black↔white on a fixed
//!     schedule (put the photodiode here),
//!   * a central stimulus changes in lock-step with the patch, and
//!   * every onset's `present_time` (a scanout-clock timestamp) is logged to CSV.
//!
//! No host-clock math is on the critical path: the recorded onset time is the
//! native scanout timestamp from `flip()`. Press Escape to exit.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 09_photodiode_sync
//! ```

use serde::Serialize;
use vision_stimulus_engine::prelude::*;

/// Toggle the patch/stimulus every this many frames (~0.5 s at 60 Hz).
const TOGGLE_FRAMES: u64 = 30;
/// Photodiode patch size in pixels (make it comfortably larger than the sensor).
const PATCH: f32 = 120.0;

#[derive(Serialize)]
struct OnsetRecord {
    onset_index: u64,
    frame_number: u64,
    patch_white: bool,
    /// Scanout-clock timestamp of this onset (microseconds).
    present_time_us: u64,
    timing_source: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    println!("Photodiode sync demo. Place the photodiode over the flashing corner patch.");
    println!("Onset scanout timestamps are logged to photodiode_sync/. Escape to exit.\n");

    let session = ExperimentSession::builder()
        .with_writer(CsvDataWriter::new("photodiode_sync/"))
        .build()?;

    let context = VSEContext::builder()
        .with_window_size(1280, 720)
        .with_title("VSE - Photodiode Sync")
        .with_clear_color(0.5, 0.5, 0.5, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .with_session(session)
        .build()?;

    let mut frame: u64 = 0;
    let mut onset_index: u64 = 0;
    // Track state transitions so we log one row per *onset*, not per frame.
    let mut last_logged_state: Option<bool> = None;

    context.run(move |vse| {
        if vse.key_just_pressed(KeyCode::Escape) {
            vse.request_exit();
            return Ok(());
        }

        // State is a pure function of the frame index → deterministic schedule.
        let patch_white = (frame / TOGGLE_FRAMES) % 2 == 0;

        let (w, h) = vse.window_size();

        // Photodiode patch: bottom-left corner, full black/white toggle.
        let patch_color = if patch_white {
            Color::WHITE
        } else {
            Color::BLACK
        };
        vse.draw_rect(0.0, h as f32 - PATCH, PATCH, h as f32, patch_color);

        // Central stimulus in lock-step: a grating that flips contrast polarity.
        let cx = w as f32 / 2.0;
        let cy = h as f32 / 2.0;
        let params = GratingParams {
            frequency: 0.03,
            orientation: 0.0,
            phase: if patch_white {
                0.0
            } else {
                std::f32::consts::PI
            },
            contrast: 0.9,
            background: 0.5,
            wave: WaveType::Square,
        };
        vse.draw_grating(cx - 200.0, cy - 200.0, cx + 200.0, cy + 200.0, &params);

        vse.clear()?;
        let info = vse.flip(None)?;

        // Log exactly one row at each onset (state change), keyed to the scanout
        // present time of the frame that first showed the new state.
        if last_logged_state != Some(patch_white) {
            vse.record_frame(OnsetRecord {
                onset_index,
                frame_number: info.frame_number,
                patch_white,
                present_time_us: info.present_time.as_micros(),
                timing_source: vse.timing_source().to_string(),
            })?;
            last_logged_state = Some(patch_white);
            onset_index += 1;
        }

        frame += 1;
        Ok(())
    })?;

    Ok(())
}
