//! Process-level initialization probe for base VSE versus `vse-3d` registration.
//!
//! Build once, then run the binary in a fresh process for each sample:
//!
//! ```bash
//! cargo build --release -p vse-3d --example init_probe
//! target/release/examples/init_probe base
//! target/release/examples/init_probe 3d
//! MESA_SHADER_CACHE_DISABLE=true target/release/examples/init_probe 3d
//! ```

use std::time::Instant;

use vision_stimulus_engine::prelude::*;
use vse_3d::{Vse3d, Vse3dConfig};

fn main() -> Result<(), VSEError> {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "base".into());
    let started = Instant::now();
    let mut context = HeadlessContext::builder(1920, 1080).build()?;
    let base_ready = started.elapsed();

    match mode.as_str() {
        "base" => {
            println!("base_ready_us={}", base_ready.as_micros());
        }
        "3d" => {
            context.run_headless_with_setup(
                |vse| Vse3d::register(vse, Vse3dConfig::default()),
                |_frame| Ok(()),
                |vse, renderer| {
                    println!(
                        "base_ready_us={} total_ready_us={} depth_format={} ring_len={}",
                        base_ready.as_micros(),
                        started.elapsed().as_micros(),
                        renderer.info().depth_format,
                        renderer.info().ring_len,
                    );
                    vse.request_exit();
                    Ok(())
                },
            )?;
        }
        other => {
            return Err(VSEError::EventLoop(format!(
                "unknown mode {other:?}; use base or 3d"
            )));
        }
    }

    Ok(())
}
