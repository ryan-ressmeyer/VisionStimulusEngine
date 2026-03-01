//! Direct Display Mode Example
//!
//! Demonstrates VSE's direct display mode, which bypasses the OS compositor
//! for sub-millisecond timing precision.
//!
//! # Setup
//!
//! See `docs/guides/display_backends.md` for prerequisites (video group,
//! TTY mode, or X11 session requirements).
//!
//! # Running
//!
//! ```bash
//! cargo run --example 08_direct_display
//! ```

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    println!("VisionStimulusEngine - Direct Display Example");
    println!("=============================================");
    println!();
    println!("Acquiring display... (this may fail if prerequisites are not met)");
    println!("See docs/guides/display_backends.md for setup instructions.");
    println!();

    let context = VSEContext::builder()
        .with_window_mode(WindowMode::DirectDisplay)
        .with_monitor(MonitorSelection::Primary)
        .with_clear_color(0.1, 0.1, 0.1, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .with_flip_logging(true)
        .build()?;

    let mut frame_count: u64 = 0;

    context.run(move |vse| {
        if frame_count == 0 {
            let backend = vse.display_backend();
            println!("Acquisition successful!");
            println!("Backend: {}", backend.description());
            let (w, h) = vse.window_size();
            println!("Display: {}x{}", w, h);
            println!();
            println!("Press Escape to exit.");
        }

        // Exit on Escape
        if vse.key_just_pressed(KeyCode::Escape) {
            println!("Escape pressed — exiting.");
            return Err(VSEError::EventLoop("User requested exit".into()));
        }

        // Draw a moving white bar to visually confirm rendering
        let (w, h) = vse.window_size();
        let bar_y = ((frame_count % h as u64) as f32) - 4.0;
        vse.draw_rect(0.0, bar_y, w as f32, bar_y + 8.0, Color::WHITE);

        vse.clear()?;
        let _info = vse.flip(None)?;

        frame_count += 1;

        if frame_count % 600 == 0 {
            println!("Frame {}", frame_count);
        }

        Ok(())
    })?;

    println!("Clean shutdown.");
    Ok(())
}
