//! Gabor patches — a single animated Gabor and a field of them.
//!
//! Consolidates PsychDemos `ProceduralGaborDemo` and `GarboriumDemo`. On the
//! left, one large Gabor drifts in phase while its orientation rotates. On the
//! right, a "garborium" field of small Gabors with reproducible random
//! orientations, phases, and positions (seeded RNG → identical every run).
//!
//! All Gabors are computed on the GPU each frame (`draw_gabor_shader`), so
//! parameters animate in real time. Press Escape to exit.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 03_gabor_field
//! ```

use std::f32::consts::{PI, TAU};

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use vision_stimulus_engine::prelude::*;

/// One element of the garborium field.
struct FieldGabor {
    cx: f32,
    cy: f32,
    half: f32,
    frequency: f32,
    orientation: f32,
    phase0: f32,
    drift_hz: f32,
    sigma: f32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let context = VSEContext::builder()
        .with_window_size(1200, 700)
        .with_title("VSE - Gabor Field")
        .with_clear_color(0.5, 0.5, 0.5, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .build()?;

    // Reproducible field: fixed seed → identical layout on every run.
    let mut rng = ChaCha8Rng::seed_from_u64(0xBEEF_F00D);
    let field: Vec<FieldGabor> = (0..40)
        .map(|_| {
            let half = rng.gen_range(28.0..48.0);
            FieldGabor {
                cx: rng.gen_range(640.0..1160.0),
                cy: rng.gen_range(60.0..640.0),
                half,
                frequency: rng.gen_range(0.03..0.09),
                orientation: rng.gen_range(0.0..PI),
                phase0: rng.gen_range(0.0..TAU),
                drift_hz: rng.gen_range(-2.0..2.0),
                sigma: half * 0.5,
            }
        })
        .collect();

    let mut t0: Option<Timestamp> = None;

    context.run(move |vse| {
        if vse.key_just_pressed(KeyCode::Escape) {
            vse.request_exit();
            return Ok(());
        }

        let now = vse.clock().now();
        let start = *t0.get_or_insert(now);
        let t = (now.as_micros().saturating_sub(start.as_micros())) as f32 / 1e6;

        // --- Left: one large Gabor, drifting phase + rotating orientation. ---
        let big = GaborParams {
            size: 320,
            frequency: 0.03,
            orientation: (0.4 * t) % PI,
            phase: (TAU * 1.0 * t) % TAU,
            sigma: 70.0,
            contrast: 1.0,
            background: 0.5,
        };
        vse.draw_gabor_shader(60.0, 190.0, 60.0 + 320.0, 190.0 + 320.0, &big);

        // --- Right: the garborium field. ---
        for g in &field {
            let params = GaborParams {
                size: (g.half * 2.0) as u32,
                frequency: g.frequency,
                orientation: g.orientation,
                phase: (g.phase0 + TAU * g.drift_hz * t) % TAU,
                sigma: g.sigma,
                contrast: 0.9,
                background: 0.5,
            };
            vse.draw_gabor_shader(
                g.cx - g.half,
                g.cy - g.half,
                g.cx + g.half,
                g.cy + g.half,
                &params,
            );
        }

        vse.clear()?;
        vse.flip(None)?;
        Ok(())
    })?;

    Ok(())
}
