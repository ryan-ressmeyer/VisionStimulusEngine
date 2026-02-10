//! Phase 1 Milestone: Clear Color Example
//!
//! This example demonstrates the basic VSE functionality:
//! - Opening a window with configurable dimensions
//! - Clearing the screen to a specified color
//! - Running at a stable frame rate with VSync
//! - Clean shutdown when the window is closed
//!
//! # Running
//!
//! ```bash
//! cargo run --example 00_clear_color
//! ```

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    println!("VisionStimulusEngine - Phase 1: Clear Color Example");
    println!("====================================================");
    println!();
    println!("This example opens a window and clears it to grey.");
    println!("Close the window to exit.");
    println!();

    // Create VSE context with custom settings
    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("VSE Phase 1 - Clear Color")
        .with_clear_color(0.5, 0.5, 0.5, 1.0) // Grey background
        .with_present_mode(PresentMode::Fifo) // VSync for stable timing
        .with_gpu_preference(GPUPreference::Discrete) // Prefer dedicated GPU
        .build()?;

    println!("Context created successfully!");
    println!();

    // Track frame count for logging
    let mut frame_count: u64 = 0;
    let start_time = std::time::Instant::now();

    // Run event loop (closure takes ownership of captured variables)
    context.run(move |vse| {
        // Clear screen with configured color
        vse.clear()?;

        // Present frame (waits for VSync)
        vse.flip()?;

        frame_count += 1;

        // Log every 60 frames (approximately once per second at 60 Hz)
        if frame_count % 60 == 0 {
            let elapsed = start_time.elapsed().as_secs_f64();
            let fps = frame_count as f64 / elapsed;
            println!(
                "Frame {}: {:.1} FPS | GPU: {}",
                frame_count,
                fps,
                vse.gpu_name()
            );
        }

        Ok(())
    })?;

    println!();
    println!("Clean shutdown complete!");

    Ok(())
}
