//! Phase 2 Milestone: Timing Validation
//!
//! This example measures and reports frame timing precision.
//! Run for at least 60 seconds to get stable statistics.
//! Timing data is recorded to `timing_validation/frames.csv`.
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

    let session = ExperimentSession::builder()
        .with_writer(CsvDataWriter::new("timing_validation/"))
        .build()?;

    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("VSE Phase 2 - Timing Validation")
        .with_clear_color(0.3, 0.3, 0.3, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .with_session(session)
        .build()?;

    let target_frames: u64 = 60 * 60; // ~60 seconds at 60 Hz

    // Rolling window for computing Hz and jitter
    let mut recent_presents: std::collections::VecDeque<u64> = std::collections::VecDeque::new();
    let window = 60usize;
    let mut total_missed: u64 = 0;

    context.run(move |vse| {
        vse.clear()?;
        let info = vse.flip(None)?;

        // Print timing source on first frame
        if info.frame_number == 0 {
            println!("Timing source: {}", vse.timing_source());
        }

        recent_presents.push_back(info.present_time.as_micros());
        if recent_presents.len() > window {
            recent_presents.pop_front();
        }

        if info.missed {
            total_missed += 1;
            println!(
                "*** MISSED FRAME {} (present_time: {:.2} ms, missed_count: {})",
                info.frame_number,
                info.present_time.as_millis_f64(),
                info.missed_count,
            );
        }

        // Print periodic updates
        if info.frame_number % 300 == 0 && recent_presents.len() >= 2 {
            let intervals: Vec<u64> = recent_presents
                .iter()
                .zip(recent_presents.iter().skip(1))
                .map(|(a, b)| b - a)
                .collect();
            let mean_us = intervals.iter().sum::<u64>() as f64 / intervals.len() as f64;
            let hz = 1_000_000.0 / mean_us;
            let variance = intervals
                .iter()
                .map(|&x| (x as f64 - mean_us).powi(2))
                .sum::<f64>()
                / intervals.len() as f64;
            let std_us = variance.sqrt();
            println!(
                "Frame {:>6}: {:.1} Hz | jitter: {:.0} us | total missed: {}",
                info.frame_number, hz, std_us, total_missed,
            );
        }

        // Auto-stop after target frames
        if info.frame_number >= target_frames {
            println!();
            println!("Target frames reached. Timing data written to timing_validation/frames.csv");
            vse.request_exit();
        }

        Ok(())
    })?;

    println!();
    println!("Timing log written to: timing_validation/frames.csv");
    println!("Clean shutdown complete!");

    Ok(())
}
