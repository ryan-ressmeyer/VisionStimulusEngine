//! Fullscreen & Input Handling Example
//!
//! Demonstrates:
//! - Borderless fullscreen mode
//! - Keyboard input (press Escape to exit)
//! - Mouse position tracking and click detection
//! - Monitor and video mode enumeration
//!
//! # Running
//!
//! ```bash
//! cargo run --example 07_fullscreen_input
//! ```

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    println!("VisionStimulusEngine - Fullscreen & Input Example");
    println!("==================================================");
    println!();
    println!("Press Escape to exit.");
    println!("Click the mouse to see position and button info.");
    println!();

    // Create a borderless fullscreen context
    let context = VSEContext::builder()
        .with_window_mode(WindowMode::BorderlessFullscreen)
        .with_monitor(MonitorSelection::Primary)
        .with_clear_color(0.2, 0.2, 0.2, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .build()?;

    let mut frame_count: u64 = 0;

    context.run(move |vse| {
        // Print monitor info on first frame
        if frame_count == 0 {
            println!("\nConnected monitors:");
            for monitor in vse.available_monitors() {
                println!(
                    "  [{}] {} - {}x{} @ {:.0} Hz (scale: {:.1}x)",
                    monitor.index,
                    monitor.name.as_deref().unwrap_or("Unknown"),
                    monitor.width,
                    monitor.height,
                    monitor.refresh_rate_hz.unwrap_or(0.0),
                    monitor.scale_factor,
                );
                println!("    Video modes:");
                let mut seen = std::collections::HashSet::new();
                for mode in &monitor.video_modes {
                    let key = (mode.width, mode.height, (mode.refresh_rate_hz * 10.0) as u32);
                    if seen.insert(key) {
                        println!(
                            "      {}x{} @ {:.1} Hz ({}-bit)",
                            mode.width, mode.height, mode.refresh_rate_hz, mode.bit_depth
                        );
                    }
                }
            }
            println!();
        }

        // Check for Escape key
        if vse.key_just_pressed(KeyCode::Escape) {
            println!("Escape pressed - exiting.");
            return Err(VSEError::EventLoop("User requested exit".into()));
        }

        // Report mouse clicks
        if vse.mouse_button_just_pressed(MouseButton::Left) {
            let (mx, my) = vse.mouse_position();
            println!("Left click at ({:.0}, {:.0})", mx, my);
        }
        if vse.mouse_button_just_pressed(MouseButton::Right) {
            let (mx, my) = vse.mouse_position();
            println!("Right click at ({:.0}, {:.0})", mx, my);
        }

        // Draw a small white square that follows the mouse
        let (mx, my) = vse.mouse_position();
        let size = 10.0;
        vse.draw_rect(
            mx as f32 - size,
            my as f32 - size,
            mx as f32 + size,
            my as f32 + size,
            Color::WHITE,
        );

        vse.clear()?;
        let _info = vse.flip(None)?;

        frame_count += 1;

        // Log FPS every 300 frames
        if frame_count % 300 == 0 {
            let (w, h) = vse.window_size();
            println!(
                "Frame {} | {}x{} | Mouse: ({:.0}, {:.0})",
                frame_count, w, h, mx, my
            );
        }

        Ok(())
    })?;

    println!("Clean shutdown complete!");
    Ok(())
}
