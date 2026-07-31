//! Noise suite — white, pink, binary, contrast-modulated, and masked noise.
//!
//! Consolidates PsychDemos `FastNoiseDemo`, `FastMaskedNoiseDemo`, and
//! `ContrastModulatedNoiseThe*StyleDemo`.
//!
//! The first three panels use VSE's built-in `draw_noise` (white / pink /
//! binary). The last two are generated as RGBA textures on the CPU to show
//! two things `draw_noise` does not do out of the box:
//!   * **contrast-modulated** ("second-order") noise: a carrier multiplied by a
//!     low spatial-frequency contrast envelope, and
//!   * **aperture-masked** noise: a Gaussian aperture written into the texture's
//!     alpha channel, so the noise composites onto the grey background through
//!     VSE's alpha blending — the noise fades out toward the patch edge.
//!
//! All randomness is seeded from the frame index, so the sequence is
//! reproducible. Press Escape to exit.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 04_noise_suite
//! ```

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use vision_stimulus_engine::prelude::*;

const TEX: u32 = 200;
const UPDATE_EVERY: u64 = 6;

fn should_refresh_custom_textures(frame: u64) -> bool {
    frame % UPDATE_EVERY == 0
}

/// Generate a contrast-modulated noise texture (RGBA, opaque).
///
/// `mean + envelope(x,y) * carrier`, where the carrier is zero-mean white noise
/// and the envelope is a low-frequency sinusoid in [0, 1] — so contrast, not
/// luminance, carries the pattern.
fn contrast_modulated(seed: u64) -> Vec<u8> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut data = vec![0u8; (TEX * TEX * 4) as usize];
    let env_freq = 3.0; // cycles across the patch
    for y in 0..TEX {
        for x in 0..TEX {
            let fx = x as f32 / TEX as f32;
            let envelope = 0.5 * (1.0 + (std::f32::consts::TAU * env_freq * fx).sin());
            let carrier: f32 = rng.gen_range(-1.0..1.0);
            let v = (0.5 + 0.5 * envelope * carrier).clamp(0.0, 1.0);
            let b = (v * 255.0) as u8;
            let i = ((y * TEX + x) * 4) as usize;
            data[i] = b;
            data[i + 1] = b;
            data[i + 2] = b;
            data[i + 3] = 255;
        }
    }
    data
}

/// Generate white noise windowed by a Gaussian aperture in the alpha channel.
fn masked_noise(seed: u64) -> Vec<u8> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut data = vec![0u8; (TEX * TEX * 4) as usize];
    let c = TEX as f32 / 2.0;
    let sigma = TEX as f32 / 5.0;
    for y in 0..TEX {
        for x in 0..TEX {
            let v: f32 = rng.gen_range(0.0..1.0);
            let b = (v * 255.0) as u8;
            let dx = x as f32 - c;
            let dy = y as f32 - c;
            let g = (-(dx * dx + dy * dy) / (2.0 * sigma * sigma)).exp();
            let i = ((y * TEX + x) * 4) as usize;
            data[i] = b;
            data[i + 1] = b;
            data[i + 2] = b;
            data[i + 3] = (g * 255.0) as u8;
        }
    }
    data
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let context = VSEContext::builder()
        .with_window_size(1100, 560)
        .with_title("VSE - Noise Suite")
        .with_clear_color(0.5, 0.5, 0.5, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .build()?;

    // Regenerate custom textures every few frames so they animate without
    // uploading identical pixel buffers on intervening frames.
    let mut frame: u64 = 0;
    let mut cm_tex: Option<TextureHandle> = None;
    let mut mk_tex: Option<TextureHandle> = None;

    context.run(move |vse| {
        if vse.key_just_pressed(KeyCode::Escape) {
            vse.request_exit();
            return Ok(());
        }

        let seed = frame / UPDATE_EVERY;

        // Row of five 200px panels with 20px gutters.
        let y0 = 120.0;
        let xs = [20.0, 240.0, 460.0, 680.0, 900.0];
        let s = TEX as f32;

        // Panel 0: white noise.
        vse.draw_noise(
            xs[0],
            y0,
            xs[0] + s,
            y0 + s,
            &NoiseParams {
                noise_type: NoiseType::White,
                seed,
                width: TEX,
                height: TEX,
                contrast: 0.9,
                background: 0.5,
            },
        )?;
        // Panel 1: pink (1/f) noise.
        vse.draw_noise(
            xs[1],
            y0,
            xs[1] + s,
            y0 + s,
            &NoiseParams {
                noise_type: NoiseType::Pink,
                seed,
                width: TEX,
                height: TEX,
                contrast: 0.9,
                background: 0.5,
            },
        )?;
        // Panel 2: binary noise.
        vse.draw_noise(
            xs[2],
            y0,
            xs[2] + s,
            y0 + s,
            &NoiseParams {
                noise_type: NoiseType::Binary,
                seed,
                width: TEX,
                height: TEX,
                contrast: 1.0,
                background: 0.5,
            },
        )?;

        if should_refresh_custom_textures(frame) {
            if let Some(old) = cm_tex.take() {
                vse.unload_texture(old);
            }
            if let Some(old) = mk_tex.take() {
                vse.unload_texture(old);
            }
            cm_tex = Some(vse.load_texture_rgba(TEX, TEX, &contrast_modulated(seed))?);
            mk_tex = Some(vse.load_texture_rgba(TEX, TEX, &masked_noise(seed))?);
        }

        // Panel 3: contrast-modulated (custom RGBA).
        vse.draw_texture(cm_tex.unwrap(), xs[3], y0, xs[3] + s, y0 + s);

        // Panel 4: Gaussian-aperture masked white noise (alpha channel).
        vse.draw_texture(mk_tex.unwrap(), xs[4], y0, xs[4] + s, y0 + s);

        vse.clear()?;
        vse.flip(None)?;

        frame += 1;
        Ok(())
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_textures_refresh_only_at_the_requested_cadence() {
        assert!(should_refresh_custom_textures(0));
        assert!(!should_refresh_custom_textures(1));
        assert!(!should_refresh_custom_textures(5));
        assert!(should_refresh_custom_textures(6));
    }
}
