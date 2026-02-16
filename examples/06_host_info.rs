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
        // Capture host info on first frame
        if ctx.frame_number() == 0 {
            let info = ctx.capture_host_info();

            // Print human-readable summary
            println!("{}", info);

            // Save to JSON
            let json = serde_json::to_string_pretty(&info).expect("Failed to serialize host info");
            std::fs::write("host_info.json", &json).expect("Failed to write host_info.json");
            println!("\nSaved to host_info.json");
        }

        ctx.clear()?;
        ctx.flip(None)?;

        // Close after a few frames
        if ctx.frame_number() > 5 {
            return Err(VSEError::Window("Done".to_string()));
        }

        Ok(())
    })?;

    Ok(())
}
