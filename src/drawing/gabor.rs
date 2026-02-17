/// Parameters for a Gabor patch.
///
/// A Gabor patch is a sinusoidal grating multiplied by a Gaussian
/// envelope. It's the most commonly used stimulus in vision science
/// for studying spatial frequency processing.
///
/// # Mathematical Definition
///
/// ```text
/// gabor(x, y) = background + contrast * gaussian(x, y) * sin(carrier(x, y))
///
/// gaussian(x, y) = exp(-(x'^2 + y'^2) / (2 * sigma^2))
/// carrier(x, y) = 2*pi * frequency * x' + phase
///
/// where x' = x*cos(orientation) + y*sin(orientation)
///       y' = -x*sin(orientation) + y*cos(orientation)
///       (x, y) are relative to patch center
/// ```
#[derive(Clone)]
pub struct GaborParams {
    /// Size of the patch in pixels (square texture).
    pub size: u32,

    /// Spatial frequency in cycles per pixel.
    pub frequency: f32,

    /// Orientation of the grating in radians.
    /// 0 = vertical bars, pi/2 = horizontal bars.
    pub orientation: f32,

    /// Phase of the sinusoidal carrier in radians.
    pub phase: f32,

    /// Standard deviation of the Gaussian envelope in pixels.
    pub sigma: f32,

    /// Contrast of the grating [0.0, 1.0].
    pub contrast: f32,

    /// Mean luminance (background level) [0.0, 1.0].
    pub background: f32,
}

impl Default for GaborParams {
    fn default() -> Self {
        Self {
            size: 256,
            frequency: 0.04,
            orientation: 0.0,
            phase: 0.0,
            sigma: 256.0 / 6.0,
            contrast: 1.0,
            background: 0.5,
        }
    }
}

impl GaborParams {
    /// Generate the Gabor patch as RGBA pixel data.
    ///
    /// Returns a `Vec<u8>` of length `size * size * 4` (RGBA8).
    pub fn generate(&self) -> Vec<u8> {
        let mut pixels = Vec::with_capacity((self.size * self.size * 4) as usize);
        let center = self.size as f32 / 2.0;
        let cos_ori = self.orientation.cos();
        let sin_ori = self.orientation.sin();

        for y in 0..self.size {
            for x in 0..self.size {
                let dx = x as f32 - center;
                let dy = y as f32 - center;

                // Rotate to grating orientation
                let x_rot = dx * cos_ori + dy * sin_ori;
                let y_rot = -dx * sin_ori + dy * cos_ori;

                // Gaussian envelope
                let gaussian =
                    (-(x_rot * x_rot + y_rot * y_rot) / (2.0 * self.sigma * self.sigma)).exp();

                // Sinusoidal carrier
                let carrier =
                    (2.0 * std::f32::consts::PI * self.frequency * x_rot + self.phase).sin();

                // Combine
                let luminance =
                    (self.background + self.contrast * 0.5 * gaussian * carrier).clamp(0.0, 1.0);

                let byte = (luminance * 255.0) as u8;
                pixels.extend_from_slice(&[byte, byte, byte, 255]);
            }
        }

        pixels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gabor_output_size() {
        let params = GaborParams {
            size: 64,
            ..Default::default()
        };
        let pixels = params.generate();
        assert_eq!(pixels.len(), 64 * 64 * 4);
    }

    #[test]
    fn test_gabor_zero_contrast() {
        let params = GaborParams {
            size: 64,
            contrast: 0.0,
            background: 0.5,
            ..Default::default()
        };
        let pixels = params.generate();
        let expected_byte = (0.5 * 255.0) as u8;
        for chunk in pixels.chunks(4) {
            assert_eq!(chunk[0], expected_byte);
            assert_eq!(chunk[1], expected_byte);
            assert_eq!(chunk[2], expected_byte);
            assert_eq!(chunk[3], 255);
        }
    }

    #[test]
    fn test_gabor_center_at_background() {
        // At center with phase=0, sin(0)=0, so luminance = background
        let params = GaborParams {
            size: 64,
            phase: 0.0,
            background: 0.5,
            ..Default::default()
        };
        let pixels = params.generate();
        // Center pixel at (32, 32) → index = (32 * 64 + 32) * 4
        let idx = (32 * 64 + 32) * 4;
        let expected = (0.5 * 255.0) as u8;
        // Should be approximately background (sin(0) = 0)
        assert!((pixels[idx] as i32 - expected as i32).unsigned_abs() <= 1);
    }

    #[test]
    fn test_gabor_edge_fadeout() {
        let params = GaborParams {
            size: 128,
            sigma: 10.0, // Small sigma = strong fadeout
            background: 0.5,
            ..Default::default()
        };
        let pixels = params.generate();
        // Corner pixel (0, 0) should be very close to background
        let expected = (0.5 * 255.0) as u8;
        assert!((pixels[0] as i32 - expected as i32).unsigned_abs() <= 1);
    }

    #[test]
    fn test_gabor_default_params() {
        let params = GaborParams::default();
        assert_eq!(params.size, 256);
        assert_eq!(params.frequency, 0.04);
        assert_eq!(params.orientation, 0.0);
        assert_eq!(params.phase, 0.0);
        assert_eq!(params.contrast, 1.0);
        assert_eq!(params.background, 0.5);
    }

    #[test]
    fn test_gabor_orientation_symmetry() {
        // Vertical grating (orientation=0) should be symmetric across y-axis
        let params = GaborParams {
            size: 64,
            orientation: 0.0,
            phase: 0.0,
            ..Default::default()
        };
        let pixels = params.generate();
        let center_y = 32;
        // Check symmetry: pixel at (center-dx, center_y) == pixel at (center+dx, center_y)
        for dx in 1..20 {
            let idx_left = (center_y * 64 + (32 - dx)) * 4;
            let idx_right = (center_y * 64 + (32 + dx)) * 4;
            // sin is odd, so values should be reflections: one above, one below background
            // Their average should be close to background
            let avg = (pixels[idx_left] as f32 + pixels[idx_right] as f32) / 2.0;
            let expected = 0.5 * 255.0;
            assert!(
                (avg - expected).abs() < 3.0,
                "Symmetry failed at dx={dx}: left={}, right={}, avg={avg}, expected={expected}",
                pixels[idx_left],
                pixels[idx_right]
            );
        }
    }
}
