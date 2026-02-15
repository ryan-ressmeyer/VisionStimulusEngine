/// RGBA color in linear [0.0, 1.0] range.
///
/// All color values are in linear space. The swapchain uses an sRGB
/// format, so Vulkan applies the sRGB transfer function automatically
/// on write. You do not need to manually gamma-correct.
///
/// # Examples
///
/// ```
/// use vision_stimulus_engine::drawing::Color;
///
/// let red = Color::rgb(1.0, 0.0, 0.0);
/// let semi_transparent_blue = Color::rgba(0.0, 0.0, 1.0, 0.5);
/// let mid_grey = Color::grey(0.5);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const WHITE: Self = Self {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };
    pub const BLACK: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    pub const RED: Self = Self {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    pub const GREEN: Self = Self {
        r: 0.0,
        g: 1.0,
        b: 0.0,
        a: 1.0,
    };
    pub const BLUE: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 1.0,
        a: 1.0,
    };
    pub const GREY: Self = Self {
        r: 0.5,
        g: 0.5,
        b: 0.5,
        a: 1.0,
    };

    /// Create an opaque RGB color (alpha = 1.0).
    pub fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    /// Create an RGBA color.
    pub fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// Create a grey color (equal R, G, B; alpha = 1.0).
    pub fn grey(level: f32) -> Self {
        Self {
            r: level,
            g: level,
            b: level,
            a: 1.0,
        }
    }

    /// Create a color from 0-255 integer components.
    pub fn from_u8(r: u8, g: u8, b: u8) -> Self {
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: 1.0,
        }
    }

    /// Convert to [f32; 4] array for Vulkan.
    pub fn to_array(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_rgb() {
        let c = Color::rgb(0.1, 0.2, 0.3);
        assert_eq!(c.r, 0.1);
        assert_eq!(c.g, 0.2);
        assert_eq!(c.b, 0.3);
        assert_eq!(c.a, 1.0);
    }

    #[test]
    fn test_color_rgba() {
        let c = Color::rgba(0.1, 0.2, 0.3, 0.4);
        assert_eq!(c.a, 0.4);
    }

    #[test]
    fn test_color_grey() {
        let c = Color::grey(0.5);
        assert_eq!(c.r, 0.5);
        assert_eq!(c.g, 0.5);
        assert_eq!(c.b, 0.5);
        assert_eq!(c.a, 1.0);
    }

    #[test]
    fn test_color_from_u8() {
        let c = Color::from_u8(255, 0, 128);
        assert_eq!(c.r, 1.0);
        assert_eq!(c.g, 0.0);
        assert!((c.b - 128.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn test_color_to_array() {
        let c = Color::rgba(0.1, 0.2, 0.3, 0.4);
        assert_eq!(c.to_array(), [0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn test_color_constants() {
        assert_eq!(Color::WHITE.to_array(), [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(Color::BLACK.to_array(), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(Color::RED.to_array(), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(Color::GREEN.to_array(), [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(Color::BLUE.to_array(), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(Color::GREY.to_array(), [0.5, 0.5, 0.5, 1.0]);
    }
}
