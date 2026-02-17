//! Advanced Stimuli Demo
//!
//! Demonstrates GPU gratings, Gabor patches, noise patterns,
//! and dot rendering (RDK primitive).
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 05_advanced_stimuli
//! ```

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().init();

    let context = VSEContext::builder()
        .with_window_size(1200, 800)
        .with_title("VSE - Advanced Stimuli")
        .with_clear_color(0.5, 0.5, 0.5, 1.0)
        .build()?;

    let mut frame: u64 = 0;

    // RDK dot state: 200 random dots
    let mut dot_positions: Vec<(f32, f32)> = Vec::new();
    let mut dots_initialized = false;

    context.run(move |vse| {
        // Initialize dots on first frame
        if !dots_initialized {
            for i in 0..200 {
                let x = 900.0 + (i % 20) as f32 * 14.0;
                let y = 50.0 + (i / 20) as f32 * 35.0;
                dot_positions.push((x, y));
            }
            dots_initialized = true;
        }

        vse.clear()?;

        // Quadrant 1 (top-left): Grating with drifting phase
        let grating_params = GratingParams {
            frequency: 0.02,
            orientation: std::f32::consts::FRAC_PI_4,
            phase: frame as f32 * 0.05,
            contrast: 0.8,
            background: 0.5,
            wave: WaveType::Sine,
        };
        vse.draw_grating(20.0, 20.0, 280.0, 280.0, &grating_params);

        // Quadrant 2 (top-center): Gabor with rotating orientation
        let gabor_params = GaborParams {
            size: 256,
            frequency: 0.03,
            orientation: frame as f32 * 0.02,
            phase: 0.0,
            sigma: 40.0,
            contrast: 1.0,
            background: 0.5,
        };
        vse.draw_gabor_shader(320.0, 20.0, 580.0, 280.0, &gabor_params);

        // Quadrant 3 (bottom-left): Animated white noise
        let noise_params = NoiseParams {
            noise_type: NoiseType::White,
            seed: frame,
            width: 128,
            height: 128,
            contrast: 0.8,
            background: 0.5,
        };
        vse.draw_noise(20.0, 320.0, 280.0, 580.0, &noise_params)?;

        // Quadrant 4 (bottom-center): Binary noise
        let binary_params = NoiseParams {
            noise_type: NoiseType::Binary,
            seed: frame / 10, // changes every 10 frames
            width: 64,
            height: 64,
            contrast: 1.0,
            background: 0.5,
        };
        vse.draw_noise(320.0, 320.0, 580.0, 580.0, &binary_params)?;

        // Right side: Dots (simple rightward drift)
        for pos in dot_positions.iter_mut() {
            pos.0 += 1.5;
            if pos.0 > 1180.0 {
                pos.0 = 620.0;
            }
        }
        vse.draw_dots(&dot_positions, 4.0, Color::WHITE);

        // Square wave grating
        let square_grating = GratingParams {
            frequency: 0.015,
            orientation: 0.0,
            phase: 0.0,
            contrast: 1.0,
            background: 0.5,
            wave: WaveType::Square,
        };
        vse.draw_grating(20.0, 620.0, 280.0, 780.0, &square_grating);

        // Plaid: two overlapping gratings
        let plaid1 = GratingParams {
            frequency: 0.03,
            orientation: std::f32::consts::FRAC_PI_4,
            phase: frame as f32 * 0.03,
            contrast: 0.4,
            background: 0.25,
            wave: WaveType::Sine,
        };
        let plaid2 = GratingParams {
            frequency: 0.03,
            orientation: -std::f32::consts::FRAC_PI_4,
            phase: frame as f32 * 0.03,
            contrast: 0.4,
            background: 0.25,
            wave: WaveType::Sine,
        };
        vse.draw_grating(320.0, 620.0, 580.0, 780.0, &plaid1);
        vse.draw_grating(320.0, 620.0, 580.0, 780.0, &plaid2);

        frame += 1;
        vse.flip(None)?;
        Ok(())
    })?;

    Ok(())
}
