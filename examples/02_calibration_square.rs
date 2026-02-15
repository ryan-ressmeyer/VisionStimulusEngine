//! Phase 3 Milestone: The Calibration Square
//!
//! Displays a 100x100 pixel square that alternates between white and
//! black every 60 frames. Logs exact flip timestamps to CSV for
//! external validation with a photodiode or oscilloscope.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 02_calibration_square
//! ```

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let context = VSEContext::builder()
        .with_window_size(1920, 1080)
        .with_title("VSE - Calibration Square")
        .with_clear_color(0.5, 0.5, 0.5, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .with_flip_logging(true)
        .with_flip_log_csv("calibration_square.csv")
        .build()?;

    let mut frame_count = 0u64;
    let mut square_white = true;

    context.run(move |vse| {
        vse.clear()?;

        // Draw the calibration square (100x100, centered)
        let (w, h) = vse.window_size();
        let cx = w as f32 / 2.0;
        let cy = h as f32 / 2.0;
        let half = 50.0;

        let color = if square_white {
            Color::WHITE
        } else {
            Color::BLACK
        };
        vse.draw_rect(cx - half, cy - half, cx + half, cy + half, color);

        let info = vse.flip()?;

        // Toggle every 60 frames (~1 second at 60 Hz)
        frame_count += 1;
        if frame_count >= 60 {
            square_white = !square_white;
            frame_count = 0;
        }

        // Periodic timing report
        if info.frame_number % 600 == 0 && info.frame_number > 0 {
            vse.print_timing_report();
        }

        Ok(())
    })?;

    Ok(())
}
