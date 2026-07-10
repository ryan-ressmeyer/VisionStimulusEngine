//! Example: Raw present + scanout feedback (VK_EXT_present_timing, Subsystem B1)
//!
//! Verifies the raw `vkQueuePresentKHR` path that attaches `VkPresentId2KHR` +
//! `VkPresentTimingsInfoEXT`, then reads back per-present scanout timing via
//! `vkGetPastPresentationTimingEXT`. On the EXT backend, every `flip()` now:
//!   - carries a real `VkPresentId2` (surfaced as `FlipInfo.present_id`), and
//!   - lets the driver record scanout timing, which `scanout_feedback()` reads back.
//!
//! Windowed pass/fail criteria (compositor-mediated — the scanout *values* are only
//! fidelity-checked on the direct-display path in B3):
//!   1. timing source is `ExtPresentTiming` (else the raw path is inactive),
//!   2. `present_id` is non-zero and strictly monotonic across flips,
//!   3. `scanout_feedback()` returns at least one record with an `IMAGE_FIRST_PIXEL_OUT`
//!      time, whose `present_id` matches a real flip.
//!
//! Run with: `CARGO_INCREMENTAL=0 cargo run --example 11_raw_present_feedback [frames]`
//! Under validation layers:
//!   `VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation CARGO_INCREMENTAL=0 \
//!    cargo run --example 11_raw_present_feedback`

use std::collections::HashSet;
use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let frames: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);

    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("Raw Present + Scanout Feedback (B1)")
        .build()?;

    // Collected verification state.
    let mut source: Option<TimingSource> = None;
    let mut present_ids: Vec<u64> = Vec::new();
    let mut monotonic_ok = true;
    let mut feedback_present_ids: HashSet<u64> = HashSet::new();
    let mut feedback_records: u64 = 0;
    let mut first_pixel_out_seen = false;
    let mut n: u64 = 0;

    context.run(move |ctx| {
        if source.is_none() {
            source = Some(ctx.timing_source());
        }

        // Alternate the background so there is real scanout to time.
        let c = if n % 2 == 0 { 0.15 } else { 0.35 };
        ctx.set_clear_color(c, c, c, 1.0);
        ctx.clear()?;
        let flip = ctx.flip(None)?;

        // present_id must be non-zero and strictly increasing on the EXT path.
        if let Some(&last) = present_ids.last() {
            if flip.present_id <= last {
                monotonic_ok = false;
                eprintln!(
                    "present_id not monotonic: frame {} id {} followed {}",
                    flip.frame_number, flip.present_id, last
                );
            }
        }
        present_ids.push(flip.present_id);

        // Confirmed scanout records from the driver.
        for fb in ctx.scanout_feedback() {
            feedback_records += 1;
            feedback_present_ids.insert(fb.present_id);
            if fb.first_pixel_out_ns.is_some() {
                first_pixel_out_seen = true;
            }
        }

        n += 1;
        if n >= frames {
            report(
                source,
                &present_ids,
                monotonic_ok,
                feedback_records,
                &feedback_present_ids,
                first_pixel_out_seen,
            );
            return Err(VSEError::Window("done".to_string()));
        }
        Ok(())
    })?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn report(
    source: Option<TimingSource>,
    present_ids: &[u64],
    monotonic_ok: bool,
    feedback_records: u64,
    feedback_present_ids: &HashSet<u64>,
    first_pixel_out_seen: bool,
) {
    println!("\n──────── Raw Present + Feedback (B1) ────────");
    println!("timing source           : {source:?}");
    println!("flips                    : {}", present_ids.len());
    println!(
        "present_id range         : {}..={}",
        present_ids.first().copied().unwrap_or(0),
        present_ids.last().copied().unwrap_or(0)
    );
    let nonzero = present_ids.iter().all(|&id| id != 0);
    println!("present_id all non-zero  : {nonzero}");
    println!("present_id monotonic     : {monotonic_ok}");
    println!("feedback records read    : {feedback_records}");
    println!("distinct feedback ids    : {}", feedback_present_ids.len());
    println!("IMAGE_FIRST_PIXEL_OUT    : {first_pixel_out_seen}");

    let is_ext = source == Some(TimingSource::ExtPresentTiming);
    let pass = is_ext && nonzero && monotonic_ok && feedback_records > 0 && first_pixel_out_seen;

    // A correlation sanity check: at least one feedback id should be a real flip id.
    let flip_id_set: HashSet<u64> = present_ids.iter().copied().collect();
    let correlated = feedback_present_ids
        .iter()
        .any(|id| flip_id_set.contains(id));
    println!("feedback correlates flip : {correlated}");

    println!("────────────────────────────────────────────");
    if !is_ext {
        println!(
            "SKIP: backend is {source:?}, not ExtPresentTiming — the raw present path is \
             inactive on this machine/driver."
        );
    } else if pass && correlated {
        println!("PASS ✔  raw present + present-id + scanout feedback all working");
    } else {
        println!("FAIL x  see fields above");
    }
    println!();
}
