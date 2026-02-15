//! Gabor Patch Demo
//!
//! Displays a Gabor patch generated from parameters.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 03_gabor_demo
//! ```

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().init();

    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("VSE - Gabor Patch")
        .with_clear_color(0.5, 0.5, 0.5, 1.0)
        .build()?;

    let mut gabor_handle: Option<TextureHandle> = None;

    context.run(move |vse| {
        // Create Gabor on first frame
        if gabor_handle.is_none() {
            let params = GaborParams {
                size: 256,
                frequency: 0.04,
                orientation: std::f32::consts::FRAC_PI_4,
                phase: 0.0,
                sigma: 40.0,
                contrast: 1.0,
                background: 0.5,
            };
            gabor_handle = Some(vse.create_gabor(&params)?);
        }

        vse.clear()?;

        // Draw centered
        let (w, h) = vse.window_size();
        let cx = w as f32 / 2.0;
        let cy = h as f32 / 2.0;
        let half = 128.0;

        vse.draw_texture(
            gabor_handle.unwrap(),
            cx - half,
            cy - half,
            cx + half,
            cy + half,
        );

        vse.flip()?;
        Ok(())
    })?;

    Ok(())
}
