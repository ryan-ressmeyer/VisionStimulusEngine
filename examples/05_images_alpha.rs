//! Images, scaling, and alpha compositing.
//!
//! Consolidates PsychDemos image scaling, `AlphaImageDemo`, and
//! `SimpleImageMixingDemo` / `ImageMixingTutorial`.
//!
//! Three columns:
//!   1. **Scaling** — one image drawn at 0.5x / 1x / 2x.
//!   2. **Alpha compositing** — a transparent RGBA overlay containing a
//!      translucent rectangle and circle is composited over the image.
//!   3. **Per-pixel image mixing** — a second texture whose alpha ramps from
//!      transparent (left) to opaque (right) is drawn over the image, so the
//!      two images cross-fade across space.
//!
//! Press Escape to exit.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 05_images_alpha
//! ```

use vision_stimulus_engine::prelude::*;

const GRAD: u32 = 256;

/// A second image to mix with: a smooth color gradient whose **alpha** ramps
/// left→right (0 → 1), so drawing it over another image reveals the underlying
/// image on the left and this gradient on the right.
fn gradient_texture() -> Vec<u8> {
    let mut data = vec![0u8; (GRAD * GRAD * 4) as usize];
    for y in 0..GRAD {
        for x in 0..GRAD {
            let fx = x as f32 / (GRAD - 1) as f32;
            let fy = y as f32 / (GRAD - 1) as f32;
            let i = ((y * GRAD + x) * 4) as usize;
            data[i] = (255.0 * (1.0 - fx)) as u8; // R
            data[i + 1] = (255.0 * fy) as u8; // G
            data[i + 2] = (255.0 * fx) as u8; // B
            data[i + 3] = (255.0 * fx) as u8; // alpha ramps with x
        }
    }
    data
}

/// A transparent overlay containing a red band and a cyan disc.
fn overlay_texture(size: u32) -> Vec<u8> {
    let mut data = vec![0u8; (size * size * 4) as usize];
    let center = size as f32 / 2.0;
    let radius = size as f32 * 0.25;
    for y in 0..size {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            if y < size / 2 && x >= size / 10 && x < size - size / 10 {
                data[i..i + 4].copy_from_slice(&[230, 26, 26, 140]);
            }
            let dx = x as f32 - center;
            let dy = y as f32 - size as f32 * 0.72;
            if dx * dx + dy * dy <= radius * radius {
                data[i..i + 4].copy_from_slice(&[26, 204, 230, 160]);
            }
        }
    }
    data
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let context = VSEContext::builder()
        .with_window_size(1040, 620)
        .with_title("VSE - Images & Alpha")
        .with_clear_color(0.2, 0.2, 0.22, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .build()?;

    let mut image: Option<TextureHandle> = None;
    let mut gradient: Option<TextureHandle> = None;
    let mut overlay: Option<TextureHandle> = None;

    context.run(move |vse| {
        if vse.key_just_pressed(KeyCode::Escape) {
            vse.request_exit();
            return Ok(());
        }

        // Load resources on the first frame.
        if image.is_none() {
            image = Some(vse.load_image("assets/rustacean-flat-happy.png")?);
            gradient = Some(vse.load_texture_rgba(GRAD, GRAD, &gradient_texture())?);
            overlay = Some(vse.load_texture_rgba(GRAD, GRAD, &overlay_texture(GRAD))?);
        }
        let img = image.unwrap();
        let grad = gradient.unwrap();
        let overlay = overlay.unwrap();

        // --- Column 1: scaling (0.5x, 1x, 2x), stacked vertically. ---
        let base = 96.0;
        for (i, scale) in [0.5_f32, 1.0, 2.0].iter().enumerate() {
            let half = base * scale / 2.0;
            let cx = 170.0;
            let cy = 130.0 + i as f32 * 190.0;
            vse.draw_texture(img, cx - half, cy - half, cx + half, cy + half);
        }

        // --- Column 2: alpha compositing over the image. ---
        // Both layers use the textured pipeline, which preserves submission order.
        let (bx, by, bs) = (370.0, 190.0, 240.0);
        vse.draw_texture(img, bx, by, bx + bs, by + bs);
        vse.draw_texture(overlay, bx, by, bx + bs, by + bs);

        // --- Column 3: per-pixel image mixing via the gradient's alpha ramp. ---
        let (mx, my, ms) = (700.0, 190.0, 240.0);
        vse.draw_texture(img, mx, my, mx + ms, my + ms); // underlying image
        vse.draw_texture(grad, mx, my, mx + ms, my + ms); // gradient over it

        vse.clear()?;
        vse.flip(None)?;
        Ok(())
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_texture_contains_transparent_and_colored_pixels() {
        let pixels = overlay_texture(32);
        let alpha = |x: u32, y: u32| pixels[((y * 32 + x) * 4 + 3) as usize];

        assert_eq!(alpha(0, 31), 0);
        assert!(alpha(4, 4) > 0);
        assert!(alpha(16, 23) > 0);
    }
}
