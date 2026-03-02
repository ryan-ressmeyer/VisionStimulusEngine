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
    // Write to stderr so logs are captured when redirecting with > log 2>&1
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    eprintln!("VisionStimulusEngine - Direct Display Example");
    eprintln!("=============================================");
    eprintln!();
    eprintln!("Acquiring display... (this may fail if prerequisites are not met)");
    eprintln!("See docs/guides/display_backends.md for setup instructions.");
    eprintln!();

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
            eprintln!("Acquisition successful!");
            eprintln!("Backend: {}", backend.description());
            let (w, h) = vse.window_size();
            eprintln!("Display: {}x{}", w, h);
            eprintln!();
            eprintln!("Press Escape to exit.");
        }

        // Exit on Escape
        if vse.key_just_pressed(KeyCode::Escape) {
            eprintln!("Escape pressed — exiting.");
            vse.request_exit();
            return Ok(());
        }

        // Draw a moving white bar to visually confirm rendering
        let (w, h) = vse.window_size();
        let bar_y = ((frame_count % h as u64) as f32) - 4.0;
        vse.draw_rect(0.0, bar_y, w as f32, bar_y + 8.0, Color::WHITE);

        vse.clear()?;
        let _info = vse.flip(None)?;

        frame_count += 1;

        if frame_count % 600 == 0 {
            eprintln!("Frame {}", frame_count);
        }

        Ok(())
    })?;

    eprintln!("Clean shutdown.");
    Ok(())
}
