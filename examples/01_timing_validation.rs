//! Phase 2 Milestone: Timing Validation
//!
//! This example measures and reports frame timing precision.
//! Run for at least 60 seconds to get stable statistics.
//!
//! # Running
//!
//! ```bash
//! cargo run --example 01_timing_validation
//! cargo run --release --example 01_timing_validation  # for production timing
//! ```

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    println!("VSE Phase 2 - Timing Validation");
    println!("================================");
    println!("Running for ~60 seconds. Close window or wait for auto-stop.");
    println!();

    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("VSE Phase 2 - Timing Validation")
        .with_clear_color(0.3, 0.3, 0.3, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .with_flip_logging(true)
        .with_flip_log_csv("timing_validation.csv")
        .build()?;

    let target_frames: u64 = 60 * 60; // ~60 seconds at 60 Hz

    context.run(move |vse| {
        vse.clear()?;
        let info = vse.flip()?;

        // Print periodic updates
        if info.frame_number % 300 == 0 && info.frame_number > 0 {
            if let Some(stats) = vse.timing_stats() {
                println!(
                    "Frame {:>6}: {:.1} Hz | jitter: {:.0} us | missed: {}",
                    info.frame_number,
                    stats.measured_refresh_rate,
                    stats.std_frame_duration_us,
                    stats.missed_frames,
                );
            }
        }

        // Log missed frames immediately
        if info.missed {
            println!(
                "*** MISSED FRAME {} (present_time: {:.2} ms, missed_count: {})",
                info.frame_number,
                info.present_time.as_millis_f64(),
                info.missed_count,
            );
        }

        // Auto-stop after target frames
        if info.frame_number >= target_frames {
            vse.print_timing_report();
        }

        Ok(())
    })?;

    println!();
    println!("Timing log written to: timing_validation.csv");
    println!("Clean shutdown complete!");

    Ok(())
}
