//! Procedural Gaborium — a field of moving, linearly superimposed Gabors.
//!
//! This is a close Gabor-only port of Psychtoolbox's
//! `ProceduralGarboriumDemo`. Two hundred GPU-computed Gabors move along
//! independently evolving directions, rotate, shift phase, and pulse their
//! Gaussian aspect ratio. Overlaps use additive signed-modulation blending,
//! equivalent to Psychtoolbox's `GL_ONE, GL_ONE`.
//!
//! The random seed is fixed, so the initial field and animation are identical
//! on every run. Press Escape to exit.
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

const WIDTH: u32 = 1200;
const HEIGHT: u32 = 700;
const N_GABORS: usize = 200;
const SUPPORT: f32 = 65.0;
const BASE_SIGMA: f32 = 10.0;
const BASE_FREQUENCY: f32 = 0.05;
const STEP_PER_FRAME: f32 = 0.33;
const PHASE_STEP: f32 = 10.0 * PI / 180.0;
const ORIENTATION_JITTER: f32 = PI / 180.0;
// PTB contrast 10 * its default 1 / (sqrt(2*pi) * sigma) normalization,
// converted to VSE's contrast convention, which includes a factor of 0.5.
const CONTRAST: f32 = 2.0 / (2.506_628_3 * BASE_SIGMA) * 10.0;

/// One element of the Gaborium field.
struct FieldGabor {
    cx: f32,
    cy: f32,
    half: f32,
    frequency: f32,
    orientation: f32,
    phase: f32,
    sigma: f32,
    aspect_ratio: f32,
}

/// Deterministic standard-normal sample using the Box-Muller transform.
fn standard_normal(rng: &mut impl Rng) -> f32 {
    let u1 = rng.gen_range(f32::EPSILON..1.0);
    let u2 = rng.gen_range(0.0..1.0);
    (-2.0 * u1.ln()).sqrt() * (TAU * u2).cos()
}

fn advance_gabor(
    gabor: &mut FieldGabor,
    orientation_delta: f32,
    frame: u64,
    width: f32,
    height: f32,
) {
    gabor.orientation = (gabor.orientation + orientation_delta).rem_euclid(TAU);
    gabor.phase = (gabor.phase + PHASE_STEP).rem_euclid(TAU);
    gabor.aspect_ratio = 1.0 + 0.25 * (frame as f32 * 0.1).sin();

    // Screen y increases downward, hence the minus sign. This is the same
    // direction convention used by ProceduralGarboriumDemo.
    gabor.cx = (gabor.cx + STEP_PER_FRAME * gabor.orientation.cos()).rem_euclid(width);
    gabor.cy = (gabor.cy - STEP_PER_FRAME * gabor.orientation.sin()).rem_euclid(height);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let context = VSEContext::builder()
        .with_window_size(WIDTH, HEIGHT)
        .with_title("VSE - Procedural Gaborium")
        .with_clear_color(0.5, 0.5, 0.5, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .build()?;

    let mut rng = ChaCha8Rng::seed_from_u64(0xBEEF_F00D);
    let mut field: Vec<FieldGabor> = (0..N_GABORS)
        .map(|_| {
            // PTB uses 0.1 + 0.9 * randn. Taking the magnitude preserves its
            // size distribution without constructing inverted rectangles.
            let scale = (0.1 + 0.9 * standard_normal(&mut rng)).abs().max(0.05);
            FieldGabor {
                cx: rng.gen_range(0.0..WIDTH as f32),
                cy: rng.gen_range(0.0..HEIGHT as f32),
                half: SUPPORT * 0.5 * scale,
                // PTB evaluates the shader in unscaled 65 px texture space.
                // Compensate because VSE evaluates directly in destination pixels.
                frequency: BASE_FREQUENCY / scale,
                orientation: rng.gen_range(0.0..TAU),
                phase: PI,
                sigma: BASE_SIGMA * scale,
                aspect_ratio: 1.0,
            }
        })
        .collect();

    let mut frame = 0u64;

    context.run(move |vse| {
        if vse.key_just_pressed(KeyCode::Escape) {
            vse.request_exit();
            return Ok(());
        }

        for gabor in &field {
            let params = GaborParams {
                size: (gabor.half * 2.0).round().max(1.0) as u32,
                frequency: gabor.frequency,
                orientation: gabor.orientation,
                phase: gabor.phase,
                sigma: gabor.sigma,
                aspect_ratio: gabor.aspect_ratio,
                contrast: CONTRAST,
                background: 0.5,
            };
            vse.draw_gabor_additive(
                gabor.cx - gabor.half,
                gabor.cy - gabor.half,
                gabor.cx + gabor.half,
                gabor.cy + gabor.half,
                &params,
            );
        }

        vse.clear()?;
        vse.flip(None)?;

        for gabor in &mut field {
            advance_gabor(
                gabor,
                ORIENTATION_JITTER * standard_normal(&mut rng),
                frame,
                WIDTH as f32,
                HEIGHT as f32,
            );
        }
        frame += 1;
        Ok(())
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_gabor(cx: f32, cy: f32, orientation: f32) -> FieldGabor {
        FieldGabor {
            cx,
            cy,
            half: 32.5,
            frequency: 0.05,
            orientation,
            phase: PI,
            sigma: 10.0,
            aspect_ratio: 1.0,
        }
    }

    #[test]
    fn each_gabor_moves_along_its_own_orientation() {
        let mut rightward = test_gabor(100.0, 100.0, 0.0);
        let mut upward = test_gabor(100.0, 100.0, PI / 2.0);

        advance_gabor(&mut rightward, 0.0, 0, 1200.0, 700.0);
        advance_gabor(&mut upward, 0.0, 0, 1200.0, 700.0);

        assert!(rightward.cx > 100.0);
        assert!((rightward.cy - 100.0).abs() < 1e-5);
        assert!((upward.cx - 100.0).abs() < 1e-5);
        assert!(upward.cy < 100.0);
    }

    #[test]
    fn motion_wraps_and_animation_matches_psychtoolbox_steps() {
        let mut gabor = test_gabor(1199.9, 0.1, PI / 4.0);

        advance_gabor(&mut gabor, 0.0, 1, 1200.0, 700.0);

        assert!(gabor.cx < 1.0);
        assert!(gabor.cy > 699.0);
        let expected_phase = (PI + PHASE_STEP).rem_euclid(TAU);
        assert!((gabor.phase - expected_phase).abs() < 1e-6);
        assert!((gabor.aspect_ratio - (1.0 + 0.25 * 0.1f32.sin())).abs() < 1e-6);
    }

    #[test]
    fn destination_scaling_preserves_psychtoolbox_texture_space_parameters() {
        let scale = 2.0;
        let sigma = BASE_SIGMA * scale;
        let frequency = BASE_FREQUENCY / scale;

        assert_eq!(sigma, 20.0);
        assert_eq!(frequency, 0.025);
        assert!((CONTRAST * 0.5 - 1.0 / 2.506_628_3).abs() < 1e-6);
    }
}
