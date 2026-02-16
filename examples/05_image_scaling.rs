//! Image Scaling Demo
//!
//! Loads an image from disk and displays it at three different scales.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 05_image_scaling
//! ```

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().init();

    let context = VSEContext::builder()
        .with_window_size(1024, 600)
        .with_title("VSE - Image Scaling")
        .with_clear_color(0.3, 0.3, 0.3, 1.0)
        .build()?;

    let mut texture: Option<TextureHandle> = None;

    context.run(move |vse| {
        // Load image on first frame
        if texture.is_none() {
            texture = Some(vse.load_image("assets/rustacean-flat-happy.png")?);
        }

        vse.clear()?;

        let (win_w, win_h) = vse.window_size();
        let cy = win_h as f32 / 2.0;
        let handle = texture.unwrap();

        // Three scales: 0.5x, 1x, 2x
        let base_size = 128.0;
        let scales = [0.5_f32, 1.0, 2.0];
        let section_w = win_w as f32 / scales.len() as f32;

        for (i, &scale) in scales.iter().enumerate() {
            let half = base_size * scale / 2.0;
            let cx = section_w * (i as f32 + 0.5);
            vse.draw_texture(handle, cx - half, cy - half, cx + half, cy + half);
        }

        vse.flip(None)?;
        Ok(())
    })?;

    Ok(())
}
