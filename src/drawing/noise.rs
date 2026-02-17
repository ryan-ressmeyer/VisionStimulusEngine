use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

use super::stimuli::{NoiseParams, NoiseType};

/// Generate a noise texture as RGBA8 pixel data.
///
/// Returns `Vec<u8>` of length `width * height * 4`.
/// Output is deterministic for a given `NoiseParams`.
pub fn generate_noise(params: &NoiseParams) -> Vec<u8> {
    match params.noise_type {
        NoiseType::White => generate_white_noise(params),
        NoiseType::Pink => generate_pink_noise(params),
        NoiseType::Binary => generate_binary_noise(params),
    }
}

fn generate_white_noise(params: &NoiseParams) -> Vec<u8> {
    let mut rng = ChaCha8Rng::seed_from_u64(params.seed);
    let pixel_count = (params.width * params.height) as usize;
    let mut pixels = Vec::with_capacity(pixel_count * 4);

    for _ in 0..pixel_count {
        let noise_val: f32 = rng.gen::<f32>() - 0.5; // [-0.5, 0.5]
        let luminance = (params.background + params.contrast * noise_val).clamp(0.0, 1.0);
        let byte = (luminance * 255.0) as u8;
        pixels.extend_from_slice(&[byte, byte, byte, 255]);
    }

    pixels
}

fn generate_binary_noise(params: &NoiseParams) -> Vec<u8> {
    let mut rng = ChaCha8Rng::seed_from_u64(params.seed);
    let pixel_count = (params.width * params.height) as usize;
    let mut pixels = Vec::with_capacity(pixel_count * 4);

    let low = ((params.background - params.contrast * 0.5).clamp(0.0, 1.0) * 255.0) as u8;
    let high = ((params.background + params.contrast * 0.5).clamp(0.0, 1.0) * 255.0) as u8;

    for _ in 0..pixel_count {
        let byte = if rng.gen::<bool>() { high } else { low };
        pixels.extend_from_slice(&[byte, byte, byte, 255]);
    }

    pixels
}

fn generate_pink_noise(params: &NoiseParams) -> Vec<u8> {
    let w = params.width as usize;
    let h = params.height as usize;
    let pixel_count = w * h;

    // Generate white noise in spatial domain
    let mut rng = ChaCha8Rng::seed_from_u64(params.seed);
    let mut spatial: Vec<f32> = (0..pixel_count).map(|_| rng.gen::<f32>() - 0.5).collect();

    // Process rows: FFT, apply 1/f, inverse FFT
    let mut planner = FftPlanner::<f32>::new();

    // Apply 1/f filtering per row
    let fft_fwd = planner.plan_fft_forward(w);
    let fft_inv = planner.plan_fft_inverse(w);
    for row in 0..h {
        let start = row * w;
        let mut buffer: Vec<Complex<f32>> = spatial[start..start + w]
            .iter()
            .map(|&v| Complex::new(v, 0.0))
            .collect();
        fft_fwd.process(&mut buffer);
        for (i, c) in buffer.iter_mut().enumerate() {
            let freq = if i <= w / 2 { i } else { w - i };
            if freq == 0 {
                *c = Complex::new(0.0, 0.0); // Remove DC
            } else {
                *c /= (freq as f32).sqrt(); // 1/sqrt(f) amplitude = 1/f power
            }
        }
        fft_inv.process(&mut buffer);
        let norm = 1.0 / w as f32;
        for (i, c) in buffer.iter().enumerate() {
            spatial[start + i] = c.re * norm;
        }
    }

    // Apply 1/f filtering per column
    let fft_fwd_col = planner.plan_fft_forward(h);
    let fft_inv_col = planner.plan_fft_inverse(h);
    let mut col_buffer: Vec<Complex<f32>> = vec![Complex::new(0.0, 0.0); h];
    for col in 0..w {
        for row in 0..h {
            col_buffer[row] = Complex::new(spatial[row * w + col], 0.0);
        }
        fft_fwd_col.process(&mut col_buffer);
        for (i, c) in col_buffer.iter_mut().enumerate() {
            let freq = if i <= h / 2 { i } else { h - i };
            if freq == 0 {
                *c = Complex::new(0.0, 0.0);
            } else {
                *c /= (freq as f32).sqrt();
            }
        }
        fft_inv_col.process(&mut col_buffer);
        let norm = 1.0 / h as f32;
        for row in 0..h {
            spatial[row * w + col] = col_buffer[row].re * norm;
        }
    }

    // Normalize to [-0.5, 0.5] range
    let max_abs = spatial.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
    if max_abs > 0.0 {
        for v in spatial.iter_mut() {
            *v = (*v / max_abs) * 0.5;
        }
    }

    // Convert to RGBA
    let mut pixels = Vec::with_capacity(pixel_count * 4);
    for val in &spatial {
        let luminance = (params.background + params.contrast * val).clamp(0.0, 1.0);
        let byte = (luminance * 255.0) as u8;
        pixels.extend_from_slice(&[byte, byte, byte, 255]);
    }

    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_white_noise_dimensions() {
        let params = NoiseParams {
            width: 64,
            height: 32,
            ..Default::default()
        };
        let pixels = generate_noise(&params);
        assert_eq!(pixels.len(), 64 * 32 * 4);
    }

    #[test]
    fn test_white_noise_deterministic() {
        let params = NoiseParams {
            seed: 42,
            width: 64,
            height: 64,
            ..Default::default()
        };
        let a = generate_noise(&params);
        let b = generate_noise(&params);
        assert_eq!(a, b);
    }

    #[test]
    fn test_white_noise_different_seeds() {
        let a = generate_noise(&NoiseParams {
            seed: 1,
            width: 64,
            height: 64,
            ..Default::default()
        });
        let b = generate_noise(&NoiseParams {
            seed: 2,
            width: 64,
            height: 64,
            ..Default::default()
        });
        assert_ne!(a, b);
    }

    #[test]
    fn test_binary_noise_only_two_values() {
        let params = NoiseParams {
            noise_type: NoiseType::Binary,
            seed: 7,
            width: 32,
            height: 32,
            contrast: 1.0,
            background: 0.5,
        };
        let pixels = generate_noise(&params);
        let low = 0u8; // (0.5 - 0.5).clamp(0,1) * 255 = 0
        let high = 255u8; // (0.5 + 0.5).clamp(0,1) * 255 = 255
        for chunk in pixels.chunks(4) {
            assert!(
                chunk[0] == low || chunk[0] == high,
                "Expected {} or {}, got {}",
                low,
                high,
                chunk[0]
            );
            assert_eq!(chunk[3], 255); // alpha
        }
    }

    #[test]
    fn test_binary_noise_deterministic() {
        let params = NoiseParams {
            noise_type: NoiseType::Binary,
            seed: 99,
            width: 32,
            height: 32,
            ..Default::default()
        };
        let a = generate_noise(&params);
        let b = generate_noise(&params);
        assert_eq!(a, b);
    }

    #[test]
    fn test_pink_noise_dimensions() {
        let params = NoiseParams {
            noise_type: NoiseType::Pink,
            seed: 0,
            width: 64,
            height: 64,
            ..Default::default()
        };
        let pixels = generate_noise(&params);
        assert_eq!(pixels.len(), 64 * 64 * 4);
    }

    #[test]
    fn test_pink_noise_deterministic() {
        let params = NoiseParams {
            noise_type: NoiseType::Pink,
            seed: 12,
            width: 64,
            height: 64,
            ..Default::default()
        };
        let a = generate_noise(&params);
        let b = generate_noise(&params);
        assert_eq!(a, b);
    }

    #[test]
    fn test_pink_noise_in_range() {
        let params = NoiseParams {
            noise_type: NoiseType::Pink,
            seed: 0,
            width: 64,
            height: 64,
            contrast: 1.0,
            background: 0.5,
        };
        let pixels = generate_noise(&params);
        // All RGB values should be in [0, 255], alpha always 255
        for chunk in pixels.chunks(4) {
            assert_eq!(chunk[3], 255);
        }
    }

    #[test]
    fn test_zero_contrast_is_flat() {
        let params = NoiseParams {
            noise_type: NoiseType::White,
            seed: 0,
            width: 32,
            height: 32,
            contrast: 0.0,
            background: 0.5,
        };
        let pixels = generate_noise(&params);
        let expected = (0.5 * 255.0) as u8;
        for chunk in pixels.chunks(4) {
            assert_eq!(chunk[0], expected);
        }
    }
}
