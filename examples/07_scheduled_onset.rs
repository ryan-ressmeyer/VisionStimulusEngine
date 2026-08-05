//! Scheduled Flip Demo
//!
//! Demonstrates using flip(Some(target_time)) to schedule frame
//! presentation at specific times. Shows the difference between
//! immediate and scheduled presents.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 07_scheduled_onset
//! ```

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    println!("VSE - Scheduled Flip Demo");
    println!("=========================");

    let session = ExperimentSession::builder()
        .with_writer(CsvDataWriter::new("scheduled_flip/"))
        .build()?;

    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("VSE - Scheduled Flip")
        .with_clear_color(0.3, 0.3, 0.3, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .with_session(session)
        .build()?;

    let mut last_scanout_present = None;

    context.run(move |vse| {
        vse.clear()?;

        // After warmup, schedule each flip one refresh cycle after the last
        let target = last_scanout_present.map(|prev: Timestamp| {
            // Request one nominal 60 Hz interval after the last scanout-domain receipt.
            Timestamp::from_micros(prev.as_micros() + 16_667)
        });

        let info = vse.flip(target)?;

        if info.frame_number == 0 {
            println!("Selected backend: {}", vse.timing_source());
            println!("First receipt source: {}", info.timing_source);
        }

        // Never feed a CPU-domain fallback back into the scanout-domain target API.
        last_scanout_present =
            (info.timing_source == TimingSource::ExtPresentTiming).then_some(info.present_time);

        Ok(())
    })?;

    Ok(())
}
