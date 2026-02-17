/// Parameters for a sinusoidal or square-wave grating.
///
/// A grating is a repeating pattern of light and dark bars.
/// It is defined by spatial frequency, orientation, phase,
/// contrast, and mean luminance (background).
#[derive(Clone, Debug)]
pub struct GratingParams {
    /// Spatial frequency in cycles per pixel.
    pub frequency: f32,
    /// Orientation in radians (0 = vertical bars, PI/2 = horizontal).
    pub orientation: f32,
    /// Phase in radians.
    pub phase: f32,
    /// Contrast [0.0, 1.0].
    pub contrast: f32,
    /// Mean luminance [0.0, 1.0].
    pub background: f32,
    /// Waveform type.
    pub wave: WaveType,
}

/// Waveform type for gratings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaveType {
    /// Smooth sinusoidal grating.
    Sine,
    /// Hard-edged square wave grating.
    Square,
}

impl Default for GratingParams {
    fn default() -> Self {
        Self {
            frequency: 0.04,
            orientation: 0.0,
            phase: 0.0,
            contrast: 1.0,
            background: 0.5,
            wave: WaveType::Sine,
        }
    }
}

/// Parameters for CPU-generated noise textures.
///
/// Noise is generated deterministically from a seed, allowing
/// exact reproduction of stimuli across sessions and machines.
#[derive(Clone, Debug)]
pub struct NoiseParams {
    /// Type of noise.
    pub noise_type: NoiseType,
    /// Deterministic seed for the PRNG.
    pub seed: u64,
    /// Width of the generated texture in pixels.
    pub width: u32,
    /// Height of the generated texture in pixels.
    pub height: u32,
    /// Contrast [0.0, 1.0].
    pub contrast: f32,
    /// Mean luminance [0.0, 1.0].
    pub background: f32,
}

/// Type of noise pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoiseType {
    /// Uniform random luminance per pixel.
    White,
    /// 1/f power spectrum (natural image statistics).
    Pink,
    /// Each pixel is either black or white.
    Binary,
}

impl Default for NoiseParams {
    fn default() -> Self {
        Self {
            noise_type: NoiseType::White,
            seed: 0,
            width: 256,
            height: 256,
            contrast: 1.0,
            background: 0.5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grating_defaults() {
        let p = GratingParams::default();
        assert_eq!(p.frequency, 0.04);
        assert_eq!(p.orientation, 0.0);
        assert_eq!(p.phase, 0.0);
        assert_eq!(p.contrast, 1.0);
        assert_eq!(p.background, 0.5);
        assert_eq!(p.wave, WaveType::Sine);
    }

    #[test]
    fn test_noise_defaults() {
        let p = NoiseParams::default();
        assert_eq!(p.noise_type, NoiseType::White);
        assert_eq!(p.seed, 0);
        assert_eq!(p.width, 256);
        assert_eq!(p.height, 256);
        assert_eq!(p.contrast, 1.0);
        assert_eq!(p.background, 0.5);
    }

    #[test]
    fn test_wave_type_eq() {
        assert_eq!(WaveType::Sine, WaveType::Sine);
        assert_ne!(WaveType::Sine, WaveType::Square);
    }

    #[test]
    fn test_noise_type_eq() {
        assert_eq!(NoiseType::White, NoiseType::White);
        assert_ne!(NoiseType::White, NoiseType::Pink);
        assert_ne!(NoiseType::Pink, NoiseType::Binary);
    }
}
