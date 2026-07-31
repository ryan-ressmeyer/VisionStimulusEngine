//! Host and display information — reproducibility metadata.
//!
//! Consolidates the host-info capture with the monitor/video-mode enumeration
//! that used to live in the fullscreen demo. It prints:
//!   * the display backend (and whether a compositor sits in the path),
//!   * every connected monitor with its resolution, refresh rate, scale, and
//!     available video modes, and
//!   * the full `HostInfo` snapshot (GPU, driver, OS, behaviorally-observed
//!     present-timing conformance), also written to `host_info.json`.
//!
//! Capturing this alongside experimental data is what makes a run auditable and
//! reproducible on other hardware. It runs a brief warm-up so the observed
//! present-timing fields are populated, then exits.
//!
//! # Running
//!
//! ```bash
//! cargo run --example 12_host_and_display_info
//! ```

use std::collections::HashSet;
use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("VSE - Host & Display Info")
        .build()?;

    let mut printed_displays = false;

    context.run(move |ctx| {
        // Enumerate displays once, up front.
        if !printed_displays {
            printed_displays = true;
            let backend = ctx.display_backend();
            println!("Display backend: {}", backend.description());
            if backend.has_compositor() {
                println!(
                    "  Note: frames pass through an OS compositor — timing jitter is possible. \
                     Direct-display mode bypasses this (see example 13)."
                );
            }
            println!("\nConnected monitors:");
            for monitor in ctx.available_monitors() {
                println!(
                    "  [{}] {} - {}x{} @ {:.0} Hz (scale {:.1}x)",
                    monitor.index,
                    monitor.name.as_deref().unwrap_or("Unknown"),
                    monitor.width,
                    monitor.height,
                    monitor.refresh_rate_hz.unwrap_or(0.0),
                    monitor.scale_factor,
                );
                let mut seen = HashSet::new();
                for mode in &monitor.video_modes {
                    let key = (
                        mode.width,
                        mode.height,
                        (mode.refresh_rate_hz * 10.0) as u32,
                    );
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

        ctx.clear()?;
        ctx.flip(None)?;

        // Capture host info AFTER a warm-up run, so behaviorally-observed
        // present-timing fields are populated, not just advertised capabilities.
        // ~24 flips is a full turnover of the driver's timing ring.
        if ctx.frame_number() == 24 {
            let info = ctx.capture_host_info();
            println!("{}", info);

            let json = serde_json::to_string_pretty(&info).expect("Failed to serialize host info");
            std::fs::write("host_info.json", &json).expect("Failed to write host_info.json");
            println!("\nSaved to host_info.json");

            return Err(VSEError::Window("Done".to_string()));
        }

        Ok(())
    })?;

    Ok(())
}
