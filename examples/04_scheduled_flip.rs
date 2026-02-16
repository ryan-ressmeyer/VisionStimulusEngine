//! Scheduled Flip Demo
//!
//! Demonstrates using flip(Some(target_time)) to schedule frame
//! presentation at specific times. Shows the difference between
//! immediate and scheduled presents.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 04_scheduled_flip
//! ```

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    println!("VSE - Scheduled Flip Demo");
    println!("=========================");

    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("VSE - Scheduled Flip")
        .with_clear_color(0.3, 0.3, 0.3, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .with_flip_logging(true)
        .with_flip_log_csv("scheduled_flip.csv")
        .build()?;

    let mut last_present = None;

    context.run(move |vse| {
        vse.clear()?;

        // After warmup, schedule each flip one refresh cycle after the last
        let target = last_present.map(|prev: Timestamp| {
            // Target: previous present time + ~16.667ms (60 Hz)
            Timestamp::from_micros(prev.as_micros() + 16_667)
        });

        let info = vse.flip(target)?;

        if info.frame_number == 0 {
            println!("Timing source: {}", vse.timing_source());
        }

        last_present = Some(info.present_time);

        if info.frame_number % 300 == 0 && info.frame_number > 0 {
            vse.print_timing_report();
        }

        Ok(())
    })?;

    Ok(())
}
