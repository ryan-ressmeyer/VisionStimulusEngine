use vulkano::buffer::BufferContents;
use vulkano::pipeline::graphics::vertex_input::Vertex;

/// Vertex for flat-colored geometry (rectangles, circles, lines).
///
/// Position is in pixel coordinates (0,0 = top-left).
/// Color is per-vertex RGBA in linear [0.0, 1.0] range.
#[derive(Clone, Copy, Debug, Default, BufferContents, Vertex)]
#[repr(C)]
pub struct Vertex2D {
    #[format(R32G32_SFLOAT)]
    pub position: [f32; 2],

    #[format(R32G32B32A32_SFLOAT)]
    pub color: [f32; 4],
}

/// Vertex for textured geometry.
///
/// Position is in pixel coordinates.
/// UV coordinates are in [0.0, 1.0] range (0,0 = top-left of texture).
#[derive(Clone, Copy, Debug, Default, BufferContents, Vertex)]
#[repr(C)]
pub struct TexturedVertex {
    #[format(R32G32_SFLOAT)]
    pub position: [f32; 2],

    #[format(R32G32_SFLOAT)]
    pub uv: [f32; 2],
}

/// Per-instance data for dot rendering.
///
/// Each instance represents one dot at a pixel position.
/// Used with instanced rendering for efficient RDK display.
#[derive(Clone, Copy, Debug, Default, BufferContents, Vertex)]
#[repr(C)]
pub struct DotInstance {
    #[format(R32G32_SFLOAT)]
    pub position: [f32; 2],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vertex2d_default() {
        let v = Vertex2D::default();
        assert_eq!(v.position, [0.0, 0.0]);
        assert_eq!(v.color, [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn test_textured_vertex_default() {
        let v = TexturedVertex::default();
        assert_eq!(v.position, [0.0, 0.0]);
        assert_eq!(v.uv, [0.0, 0.0]);
    }

    #[test]
    fn test_vertex2d_size() {
        assert_eq!(std::mem::size_of::<Vertex2D>(), 24);
    }

    #[test]
    fn test_textured_vertex_size() {
        assert_eq!(std::mem::size_of::<TexturedVertex>(), 16);
    }

    #[test]
    fn test_dot_instance_default() {
        let d = DotInstance::default();
        assert_eq!(d.position, [0.0, 0.0]);
    }

    #[test]
    fn test_dot_instance_size() {
        assert_eq!(std::mem::size_of::<DotInstance>(), 8);
    }
}
