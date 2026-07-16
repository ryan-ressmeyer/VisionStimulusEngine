use super::color::Color;
use super::stimuli::GratingParams;
use super::vertex::{DotInstance, TexturedVertex, Vertex2D};
use crate::drawing::GaborParams;

/// A queued draw command, processed during flip().
pub(crate) enum DrawCommand {
    /// Filled rectangle defined by corners.
    Rect {
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        color: Color,
    },

    /// Filled circle.
    Circle {
        cx: f32,
        cy: f32,
        radius: f32,
        color: Color,
        segments: u32,
    },

    /// Line with width.
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        width: f32,
        color: Color,
    },

    /// Textured quad.
    Texture {
        texture_id: u64,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    },

    /// GPU-computed sinusoidal or square-wave grating.
    Grating {
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        params: GratingParams,
    },

    /// GPU-computed Gabor patch (grating x Gaussian envelope).
    Gabor {
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        params: GaborParams,
    },

    /// CPU-generated noise uploaded as texture.
    Noise {
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        texture_id: u64,
    },

    /// Instanced dot rendering.
    Dots {
        positions: Vec<[f32; 2]>,
        radius: f32,
        color: Color,
    },
}

/// Generate 6 vertices (2 triangles) for a filled rectangle.
pub(crate) fn rect_vertices(
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    color: Color,
) -> [Vertex2D; 6] {
    let c = color.to_array();
    [
        // Triangle 1: top-left, bottom-left, bottom-right
        Vertex2D {
            position: [left, top],
            color: c,
        },
        Vertex2D {
            position: [left, bottom],
            color: c,
        },
        Vertex2D {
            position: [right, bottom],
            color: c,
        },
        // Triangle 2: top-left, bottom-right, top-right
        Vertex2D {
            position: [left, top],
            color: c,
        },
        Vertex2D {
            position: [right, bottom],
            color: c,
        },
        Vertex2D {
            position: [right, top],
            color: c,
        },
    ]
}

/// Generate vertices for a filled circle as individual triangles.
///
/// Creates `segments` triangles from center to perimeter.
/// Total vertices: segments * 3.
pub(crate) fn circle_vertices(
    cx: f32,
    cy: f32,
    radius: f32,
    color: Color,
    segments: u32,
) -> Vec<Vertex2D> {
    let c = color.to_array();
    let mut vertices = Vec::with_capacity((segments * 3) as usize);

    for i in 0..segments {
        let angle_a = 2.0 * std::f32::consts::PI * i as f32 / segments as f32;
        let angle_b = 2.0 * std::f32::consts::PI * (i + 1) as f32 / segments as f32;

        vertices.push(Vertex2D {
            position: [cx, cy],
            color: c,
        });
        vertices.push(Vertex2D {
            position: [cx + radius * angle_a.cos(), cy + radius * angle_a.sin()],
            color: c,
        });
        vertices.push(Vertex2D {
            position: [cx + radius * angle_b.cos(), cy + radius * angle_b.sin()],
            color: c,
        });
    }

    vertices
}

/// Compute default segment count for a circle based on radius.
pub(crate) fn default_circle_segments(radius: f32) -> u32 {
    (16u32).max((radius * 2.0) as u32).min(256)
}

/// Generate 6 vertices (2 triangles) for a thick line.
///
/// The line is rendered as a thin rectangle oriented along the
/// line direction, with width expanding perpendicular to the line.
pub(crate) fn line_vertices(
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    width: f32,
    color: Color,
) -> [Vertex2D; 6] {
    let c = color.to_array();

    let dx = x2 - x1;
    let dy = y2 - y1;
    let len = (dx * dx + dy * dy).sqrt();

    // For zero-length lines, use a small dot
    let (px, py) = if len < 1e-6 {
        (width / 2.0, 0.0)
    } else {
        let nx = -dy / len;
        let ny = dx / len;
        (nx * width / 2.0, ny * width / 2.0)
    };

    let p0 = [x1 + px, y1 + py];
    let p1 = [x1 - px, y1 - py];
    let p2 = [x2 - px, y2 - py];
    let p3 = [x2 + px, y2 + py];

    [
        Vertex2D {
            position: p0,
            color: c,
        },
        Vertex2D {
            position: p1,
            color: c,
        },
        Vertex2D {
            position: p2,
            color: c,
        },
        Vertex2D {
            position: p0,
            color: c,
        },
        Vertex2D {
            position: p2,
            color: c,
        },
        Vertex2D {
            position: p3,
            color: c,
        },
    ]
}

/// Generate 6 textured vertices for a quad.
///
/// UV mapping: (0,0) at top-left of texture, (1,1) at bottom-right.
pub(crate) fn textured_quad_vertices(
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
) -> [TexturedVertex; 6] {
    [
        TexturedVertex {
            position: [left, top],
            uv: [0.0, 0.0],
        },
        TexturedVertex {
            position: [left, bottom],
            uv: [0.0, 1.0],
        },
        TexturedVertex {
            position: [right, bottom],
            uv: [1.0, 1.0],
        },
        TexturedVertex {
            position: [left, top],
            uv: [0.0, 0.0],
        },
        TexturedVertex {
            position: [right, bottom],
            uv: [1.0, 1.0],
        },
        TexturedVertex {
            position: [right, top],
            uv: [1.0, 0.0],
        },
    ]
}

/// Generate the static unit quad for instanced dot rendering.
pub(crate) fn dot_unit_quad_vertices() -> [DotInstance; 6] {
    [
        DotInstance {
            position: [-1.0, -1.0],
        },
        DotInstance {
            position: [-1.0, 1.0],
        },
        DotInstance {
            position: [1.0, 1.0],
        },
        DotInstance {
            position: [-1.0, -1.0],
        },
        DotInstance {
            position: [1.0, 1.0],
        },
        DotInstance {
            position: [1.0, -1.0],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rect_vertex_count() {
        let verts = rect_vertices(0.0, 0.0, 100.0, 100.0, Color::WHITE);
        assert_eq!(verts.len(), 6);
    }

    #[test]
    fn test_rect_vertex_positions() {
        let verts = rect_vertices(10.0, 20.0, 110.0, 120.0, Color::WHITE);
        // First triangle: top-left, bottom-left, bottom-right
        assert_eq!(verts[0].position, [10.0, 20.0]);
        assert_eq!(verts[1].position, [10.0, 120.0]);
        assert_eq!(verts[2].position, [110.0, 120.0]);
        // Second triangle: top-left, bottom-right, top-right
        assert_eq!(verts[3].position, [10.0, 20.0]);
        assert_eq!(verts[4].position, [110.0, 120.0]);
        assert_eq!(verts[5].position, [110.0, 20.0]);
    }

    #[test]
    fn test_circle_vertex_count() {
        let verts = circle_vertices(0.0, 0.0, 50.0, Color::RED, 32);
        assert_eq!(verts.len(), 32 * 3);
    }

    #[test]
    fn test_circle_center_vertices() {
        let verts = circle_vertices(100.0, 200.0, 50.0, Color::RED, 16);
        // Every 3rd vertex (starting at 0) should be the center
        for i in (0..verts.len()).step_by(3) {
            assert_eq!(verts[i].position, [100.0, 200.0]);
        }
    }

    #[test]
    fn test_line_vertex_count() {
        let verts = line_vertices(0.0, 0.0, 100.0, 100.0, 2.0, Color::WHITE);
        assert_eq!(verts.len(), 6);
    }

    #[test]
    fn test_line_perpendicular_offset() {
        // Horizontal line at y=50
        let verts = line_vertices(0.0, 50.0, 100.0, 50.0, 4.0, Color::WHITE);
        // Perpendicular to horizontal is vertical, so y offsets should be ±2
        // p0 = (x1+px, y1+py) where perp = (0, 1)*2 for rightward line
        assert!((verts[0].position[1] - 52.0).abs() < 1e-4);
        assert!((verts[1].position[1] - 48.0).abs() < 1e-4);
    }

    #[test]
    fn test_line_zero_length() {
        // Should not panic
        let verts = line_vertices(50.0, 50.0, 50.0, 50.0, 4.0, Color::WHITE);
        assert_eq!(verts.len(), 6);
    }

    #[test]
    fn test_textured_quad_uvs() {
        let verts = textured_quad_vertices(0.0, 0.0, 100.0, 100.0);
        // Check that UV coords span [0,0] to [1,1]
        assert_eq!(verts[0].uv, [0.0, 0.0]); // top-left
        assert_eq!(verts[1].uv, [0.0, 1.0]); // bottom-left
        assert_eq!(verts[2].uv, [1.0, 1.0]); // bottom-right
        assert_eq!(verts[5].uv, [1.0, 0.0]); // top-right
    }

    #[test]
    fn test_dot_unit_quad_vertices() {
        let verts = dot_unit_quad_vertices();
        assert_eq!(verts.len(), 6);
        assert_eq!(verts[0].position, [-1.0, -1.0]);
        assert_eq!(verts[1].position, [-1.0, 1.0]);
        assert_eq!(verts[2].position, [1.0, 1.0]);
        assert_eq!(verts[3].position, [-1.0, -1.0]);
        assert_eq!(verts[4].position, [1.0, 1.0]);
        assert_eq!(verts[5].position, [1.0, -1.0]);
    }

    #[test]
    fn test_default_circle_segments() {
        assert_eq!(default_circle_segments(5.0), 16); // min 16
        assert_eq!(default_circle_segments(50.0), 100);
        assert_eq!(default_circle_segments(200.0), 256); // max 256
    }
}
