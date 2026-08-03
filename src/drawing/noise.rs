use std::collections::{HashMap, VecDeque};

use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

use super::stimuli::{NoiseParams, NoiseType};

/// Identity of a generated noise texture.
///
/// Two draws share a texture only when they would produce byte-identical
/// pixels, so floats are compared by their bit patterns rather than by value.
/// That is the right test here: `generate_noise` is deterministic in its
/// parameters, and bit-exact identity is what reproducibility requires.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct NoiseKey {
    noise_type: NoiseType,
    seed: u64,
    width: u32,
    height: u32,
    contrast_bits: u32,
    background_bits: u32,
}

impl NoiseKey {
    pub(crate) fn of(params: &NoiseParams) -> Self {
        Self {
            noise_type: params.noise_type,
            seed: params.seed,
            width: params.width,
            height: params.height,
            contrast_bits: params.contrast.to_bits(),
            background_bits: params.background.to_bits(),
        }
    }
}

/// Bounded cache of uploaded noise textures, keyed by the parameters that
/// generated them.
///
/// Without it, every `draw_noise` call regenerated the pixels on the CPU,
/// allocated a GPU image, submitted a command buffer, and blocked on a fence —
/// per call, per frame, on the presentation path — while the previous texture
/// was never freed.
///
/// Caching alone would still leak for animated noise, where each update brings
/// a new seed and therefore a new key, so the cache is capacity-bounded and
/// evicts oldest-first. `insert` returns the texture ids it dropped; the caller
/// releases them once the frame that may reference them has been recorded.
pub(crate) struct NoiseTextureCache {
    entries: HashMap<NoiseKey, u64>,
    /// Insertion order, for oldest-first eviction.
    order: VecDeque<NoiseKey>,
    capacity: usize,
}

impl NoiseTextureCache {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// The texture already uploaded for these parameters, if any.
    pub(crate) fn get(&self, key: &NoiseKey) -> Option<u64> {
        self.entries.get(key).copied()
    }

    /// Record a newly uploaded texture, returning any texture ids evicted to
    /// stay within capacity. The caller must unload those.
    pub(crate) fn insert(&mut self, key: NoiseKey, texture_id: u64) -> Vec<u64> {
        if self.entries.insert(key, texture_id).is_none() {
            self.order.push_back(key);
        }

        let mut evicted = Vec::new();
        while self.entries.len() > self.capacity {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(id) = self.entries.remove(&oldest) {
                evicted.push(id);
            }
        }
        evicted
    }
}

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

    // --- Texture caching ---
    //
    // `draw_noise` used to build a fresh GPU texture on EVERY call: a CPU noise
    // generation, an image allocation, a command-buffer submit, and a BLOCKING
    // fence wait, all on the presentation path — and nothing ever freed them.
    // The cache removes the repeat work; the capacity bound stops animated
    // noise (a new seed per update) from leaking without limit.

    fn params(seed: u64, contrast: f32) -> NoiseParams {
        NoiseParams {
            noise_type: NoiseType::White,
            seed,
            width: 64,
            height: 64,
            contrast,
            background: 0.5,
        }
    }

    #[test]
    fn identical_parameters_reuse_the_uploaded_texture() {
        let mut cache = NoiseTextureCache::new(8);
        assert_eq!(cache.get(&NoiseKey::of(&params(1, 0.8))), None);

        cache.insert(NoiseKey::of(&params(1, 0.8)), 42);

        assert_eq!(
            cache.get(&NoiseKey::of(&params(1, 0.8))),
            Some(42),
            "a repeated draw must reuse the texture, not rebuild it"
        );
    }

    #[test]
    fn any_parameter_change_is_a_different_texture() {
        // Bit-exact identity: two draws share a texture only when they would
        // generate byte-identical pixels.
        let mut cache = NoiseTextureCache::new(8);
        cache.insert(NoiseKey::of(&params(1, 0.8)), 42);

        assert_eq!(
            cache.get(&NoiseKey::of(&params(2, 0.8))),
            None,
            "a new seed is new noise"
        );
        assert_eq!(
            cache.get(&NoiseKey::of(&params(1, 0.7999999))),
            None,
            "contrast differing in the last float bit is different noise"
        );

        let mut pink = params(1, 0.8);
        pink.noise_type = NoiseType::Pink;
        assert_eq!(cache.get(&NoiseKey::of(&pink)), None);

        let mut bigger = params(1, 0.8);
        bigger.width = 128;
        assert_eq!(cache.get(&NoiseKey::of(&bigger)), None);
    }

    #[test]
    fn animated_noise_stays_within_the_capacity_bound() {
        // The leak that motivated this: a distinct seed every update, forever.
        let mut cache = NoiseTextureCache::new(4);
        let mut freed = Vec::new();

        for seed in 0..100u64 {
            freed.extend(cache.insert(NoiseKey::of(&params(seed, 0.8)), seed));
        }

        // 100 inserted, 96 released, so exactly the 4 most recent are resident.
        assert_eq!(
            freed.len(),
            96,
            "every evicted texture is reported for release"
        );
        assert_eq!(
            freed.first().copied(),
            Some(0),
            "eviction is oldest-first, so the live seeds stay resident"
        );
        for seed in 96..100u64 {
            assert!(cache.get(&NoiseKey::of(&params(seed, 0.8))).is_some());
        }
    }

    #[test]
    fn unchanging_noise_never_evicts_anything() {
        // A static-noise experiment draws identical params every frame forever.
        // It must upload once and then evict nothing, no matter how long it runs.
        let mut cache = NoiseTextureCache::new(4);
        let mut uploads = 0;
        let mut freed = Vec::new();

        for _ in 0..100 {
            let key = NoiseKey::of(&params(7, 0.8));
            if cache.get(&key).is_none() {
                uploads += 1;
                freed.extend(cache.insert(key, 7));
            }
        }

        assert_eq!(uploads, 1, "the texture is generated and uploaded once");
        assert!(freed.is_empty(), "nothing is evicted");
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
