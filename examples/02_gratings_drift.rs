//! Drifting gratings — spatial frequency, orientation, waveform, and drift.
//!
//! Consolidates PsychDemos `GratingDemo`, `DriftDemo1`–`DriftDemo6`, and
//! `DriftWaitDemo`: a panel of gratings that differ in spatial frequency,
//! orientation, and waveform, all drifting by advancing phase each frame.
//!
//! Drift is driven by VSE's host monotonic clock rather than frame count, so
//! phase advances with elapsed time even if a frame interval is missed. This
//! is an animation policy, not a scanout-timing measurement. Press Escape to exit.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 02_gratings_drift
//! ```

use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU};
use vision_stimulus_engine::prelude::*;

/// One panel: a grating whose phase drifts at `temporal_hz` cycles/second.
struct Panel {
    rect: (f32, f32, f32, f32),
    frequency: f32,
    orientation: f32,
    temporal_hz: f32,
    wave: WaveType,
    /// If set, the orientation itself rotates at this rate (rad/s).
    rotate_rad_s: Option<f32>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let context = VSEContext::builder()
        .with_window_size(1200, 620)
        .with_title("VSE - Drifting Gratings")
        .with_clear_color(0.5, 0.5, 0.5, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .build()?;

    // 2 rows x 3 columns of panels, each 340x260 with margins.
    let panels = vec![
        Panel {
            rect: (30.0, 30.0, 370.0, 290.0),
            frequency: 0.02,
            orientation: 0.0,
            temporal_hz: 1.0,
            wave: WaveType::Sine,
            rotate_rad_s: None,
        },
        Panel {
            rect: (400.0, 30.0, 740.0, 290.0),
            frequency: 0.04,
            orientation: FRAC_PI_2,
            temporal_hz: 2.0,
            wave: WaveType::Sine,
            rotate_rad_s: None,
        },
        Panel {
            rect: (770.0, 30.0, 1110.0, 290.0),
            frequency: 0.02,
            orientation: FRAC_PI_4,
            temporal_hz: 1.0,
            wave: WaveType::Square,
            rotate_rad_s: None,
        },
        Panel {
            rect: (30.0, 320.0, 370.0, 580.0),
            frequency: 0.08,
            orientation: 0.0,
            temporal_hz: 4.0,
            wave: WaveType::Sine,
            rotate_rad_s: None,
        },
        Panel {
            rect: (400.0, 320.0, 740.0, 580.0),
            frequency: 0.03,
            orientation: 0.0,
            temporal_hz: 0.0,
            wave: WaveType::Sine,
            rotate_rad_s: Some(0.6),
        },
        Panel {
            rect: (770.0, 320.0, 1110.0, 580.0),
            frequency: 0.03,
            orientation: FRAC_PI_4,
            temporal_hz: -1.5,
            wave: WaveType::Square,
            rotate_rad_s: None,
        },
    ];

    // Animation timebase: seconds since the first frame, from a single clock
    // domain (`clock().now()`) so temporal frequency stays correct across
    // dropped frames without doing any cross-clock math.
    let mut t0: Option<Timestamp> = None;

    context.run(move |vse| {
        if vse.key_just_pressed(KeyCode::Escape) {
            vse.request_exit();
            return Ok(());
        }

        let now = vse.clock().now();
        let start = *t0.get_or_insert(now);
        let t = (now.as_micros().saturating_sub(start.as_micros())) as f32 / 1e6;

        for p in &panels {
            // Phase advances by 2*pi*temporal_hz per second.
            let phase = (TAU * p.temporal_hz * t) % TAU;
            let orientation = p.orientation + p.rotate_rad_s.map_or(0.0, |r| r * t) % PI;
            let params = GratingParams {
                frequency: p.frequency,
                orientation,
                phase,
                contrast: 0.9,
                background: 0.5,
                wave: p.wave,
            };
            let (l, top, r, b) = p.rect;
            vse.draw_grating(l, top, r, b, &params);
        }

        vse.clear()?;
        vse.flip(None)?;
        Ok(())
    })?;

    Ok(())
}
