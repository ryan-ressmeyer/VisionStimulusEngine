//! Example: Capture and display host machine information
//!
//! Demonstrates the capture_host_info() API for logging the full
//! host state to JSON for reproducibility audits.
//!
//! Run with: `cargo run --example 06_host_info`

use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("Host Info Example")
        .build()?;

    context.run(|ctx| {
        ctx.clear()?;
        ctx.flip(None)?;

        // Capture host info AFTER a warm-up run, so the behaviorally-observed present-timing fields
        // (e.g. whether the driver actually fills IMAGE_FIRST_PIXEL_OUT) are populated, not just the
        // advertised capabilities. ~24 flips is a full turnover of the driver's timing ring.
        if ctx.frame_number() == 24 {
            let info = ctx.capture_host_info();

            // Print human-readable summary
            println!("{}", info);

            // Save to JSON
            let json = serde_json::to_string_pretty(&info).expect("Failed to serialize host info");
            std::fs::write("host_info.json", &json).expect("Failed to write host_info.json");
            println!("\nSaved to host_info.json");

            return Err(VSEError::Window("Done".to_string()));
        }

        Ok(())
    })?;

    Ok(())
}
