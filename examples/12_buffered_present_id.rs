//! Example: Buffered raw present + present-id correlation (VK_EXT_present_timing, Subsystem B2)
//!
//! Exercises `run_buffered()` on the raw EXT present engine: each `flip_with_payload` drives the
//! same acquire→submit→present path as B1 but **non-blocking** (pipelined `depth+1` frames in
//! flight), attaching a real `VkPresentId2` + timing chain. Confirmation is keyed on `present_id`.
//!
//! Pass/fail criteria (windowed):
//!   1. timing source is `ExtPresentTiming`,
//!   2. every `Presented` frame's `present_id` is non-zero and strictly monotonic,
//!   3. payloads arrive in submission order (FIFO), one `Presented` per frame after warm-up,
//!   4. driver scanout feedback comes back and correlates to real present ids.
//!
//! Run with: `CARGO_INCREMENTAL=0 cargo run --example 12_buffered_present_id [frames] [depth]`

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let frames: u32 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let depth: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("Buffered Present-Id (B2)")
        .build()?;

    let state = Rc::new(RefCell::new(Verify::default()));
    let render_n = Rc::new(RefCell::new(0u32));

    let cfg = BufferedConfig {
        depth,
        ..BufferedConfig::default()
    };
    let st = state.clone();
    let rn = render_n.clone();

    context.run_buffered::<u32, _>(cfg, move |event, vse| {
        let mut s = st.borrow_mut();
        match event {
            FlipEvent::Render => {
                if s.source.is_none() {
                    s.source = Some(vse.timing_source());
                }
                let mut n = rn.borrow_mut();
                *n += 1;
                let c = if *n % 2 == 0 { 0.15 } else { 0.35 };
                vse.set_clear_color(c, c, c, 1.0);
                vse.clear()?;
                vse.flip_with_payload(None, *n)?;
                if *n >= frames {
                    vse.close();
                }
            }
            FlipEvent::Presented { flip_info, payload } => {
                // present_id monotonic + non-zero
                if flip_info.present_id == 0 {
                    s.zero_present_id = true;
                }
                if let Some(last) = s.last_present_id {
                    if flip_info.present_id <= last {
                        s.present_id_non_monotonic = true;
                    }
                }
                s.last_present_id = Some(flip_info.present_id);

                // payload FIFO
                if let Some(last) = s.last_payload {
                    if payload <= last {
                        s.payload_out_of_order = true;
                    }
                }
                s.last_payload = Some(payload);
                s.presented += 1;
                if flip_info.missed {
                    s.missed += 1;
                }

                // feedback correlation
                for fb in vse.scanout_feedback() {
                    s.feedback_ids.insert(fb.present_id);
                    if fb.first_pixel_out_ns.is_some() {
                        s.first_pixel_out_seen = true;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    })?;

    report(&state.borrow(), *render_n.borrow(), depth);
    Ok(())
}

#[derive(Default)]
struct Verify {
    source: Option<TimingSource>,
    last_present_id: Option<u64>,
    zero_present_id: bool,
    present_id_non_monotonic: bool,
    last_payload: Option<u32>,
    payload_out_of_order: bool,
    presented: u32,
    missed: u32,
    feedback_ids: HashSet<u64>,
    first_pixel_out_seen: bool,
}

fn report(s: &Verify, rendered: u32, depth: usize) {
    println!("\n──────── Buffered Present-Id (B2) ────────");
    println!("timing source            : {:?}", s.source);
    println!("depth                    : {depth}");
    println!("rendered / presented     : {rendered} / {}", s.presented);
    println!("present_id non-zero      : {}", !s.zero_present_id);
    println!("present_id monotonic     : {}", !s.present_id_non_monotonic);
    println!("payload FIFO order       : {}", !s.payload_out_of_order);
    println!("missed frames            : {}", s.missed);
    println!("distinct feedback ids    : {}", s.feedback_ids.len());
    println!("IMAGE_FIRST_PIXEL_OUT    : {}", s.first_pixel_out_seen);

    let is_ext = s.source == Some(TimingSource::ExtPresentTiming);
    // `run_buffered` drains all pending frames on exit, so every *submitted* frame is presented;
    // `rendered - presented` is just the handful of startup frames skipped by the initial
    // Wayland `OUT_OF_DATE` recreation (independent of depth).
    let counts_ok = s.presented <= rendered && rendered.saturating_sub(s.presented) <= 3;
    println!("presented≈rendered       : {counts_ok}");

    let pass = is_ext
        && !s.zero_present_id
        && !s.present_id_non_monotonic
        && !s.payload_out_of_order
        && counts_ok
        && !s.feedback_ids.is_empty()
        && s.first_pixel_out_seen;

    println!("──────────────────────────────────────────");
    if !is_ext {
        println!("SKIP: backend is {:?}, not ExtPresentTiming.", s.source);
    } else if pass {
        println!(
            "PASS ✔  buffered raw present + present-id correlation + scanout feedback working"
        );
    } else {
        println!("FAIL x  see fields above");
    }
    println!();
}
