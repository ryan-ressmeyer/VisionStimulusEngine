//! Primitives gallery — every 2D drawing primitive on one screen.
//!
//! Consolidates PsychDemos `ArcDemo`, `LinesDemo`, and `DotDemo`: filled
//! rectangles, filled circles, thick lines, stroked arcs, and instanced dots,
//! each animated so it is obvious they render live.
//!
//! Press Escape to exit.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 01_primitives_gallery
//! ```

use std::f32::consts::{PI, TAU};
use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let context = VSEContext::builder()
        .with_window_size(1000, 700)
        .with_title("VSE - Primitives Gallery")
        .with_clear_color(0.15, 0.15, 0.18, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .build()?;

    // A fixed cloud of dots that drifts rightward and wraps.
    let mut dots: Vec<(f32, f32)> = (0..150)
        .map(|i| {
            let col = (i % 15) as f32;
            let row = (i / 15) as f32;
            (700.0 + col * 18.0, 420.0 + row * 22.0)
        })
        .collect();

    let mut frame: u64 = 0;

    context.run(move |vse| {
        if vse.key_just_pressed(KeyCode::Escape) {
            vse.request_exit();
            return Ok(());
        }

        let t = frame as f32 / 60.0;

        // --- Rectangles (top-left): a row of filled squares, pulsing size. ---
        for i in 0..4 {
            let cx = 90.0 + i as f32 * 90.0;
            let cy = 90.0;
            let half = 25.0 + 12.0 * (t + i as f32 * 0.5).sin();
            let shade = 0.4 + 0.15 * i as f32;
            vse.draw_rect(
                cx - half,
                cy - half,
                cx + half,
                cy + half,
                Color::rgb(shade, 0.5, 0.9 - shade),
            );
        }

        // --- Circles (top-right): concentric filled circles. ---
        let (ccx, ccy) = (760.0, 100.0);
        for i in (0..5).rev() {
            let r = 20.0 + i as f32 * 16.0;
            let g = 0.3 + 0.14 * i as f32;
            vse.draw_circle(ccx, ccy, r, Color::rgb(0.9 - g, g, 0.3));
        }

        // --- Lines (middle-left): a rotating spoke fan from a hub. ---
        let (hx, hy) = (150.0, 320.0);
        for i in 0..8 {
            let a = t * 0.6 + i as f32 * (TAU / 8.0);
            let len = 90.0;
            vse.draw_line(
                hx,
                hy,
                hx + len * a.cos(),
                hy + len * a.sin(),
                3.0,
                Color::rgb(0.9, 0.9, 0.4),
            );
        }

        // --- Arcs (middle-center): nested rings + a sweeping arc. ---
        let (acx, acy) = (480.0, 320.0);
        // Two full stroked rings (start..start+2*PI).
        vse.draw_arc(acx, acy, 60.0, 0.0, TAU, 6.0, Color::rgb(0.3, 0.7, 0.9));
        vse.draw_arc(acx, acy, 95.0, 0.0, TAU, 3.0, Color::rgb(0.2, 0.4, 0.6));
        // A bright arc segment that sweeps around the ring.
        let sweep_start = (t * 1.2) % TAU;
        vse.draw_arc(
            acx,
            acy,
            80.0,
            sweep_start,
            sweep_start + PI / 2.0,
            10.0,
            Color::rgb(1.0, 0.6, 0.2),
        );

        // --- Dots (bottom-right): rightward-drifting dot cloud (RDK primitive). ---
        for d in dots.iter_mut() {
            d.0 += 1.2;
            if d.0 > 970.0 {
                d.0 = 690.0;
            }
        }
        vse.draw_dots(&dots, 3.5, Color::WHITE);

        vse.clear()?;
        vse.flip(None)?;
        frame += 1;
        Ok(())
    })?;

    Ok(())
}
