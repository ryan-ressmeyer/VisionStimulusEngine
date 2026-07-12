//! Bevy → VSE external-frame ring demo (landscape doc §5.4, Topology 2).
//!
//! A headless Bevy app renders a deterministic PBR scene (cube translating
//! around a circle, point light revolving overhead) on **its own wgpu Vulkan
//! device** into a 3-slot ring of exported images. VSE imports the ring,
//! blits each frame under its own draw commands, and presents with the full
//! EXT present-timing machinery — the renderer never touches the swapchain.
//!
//! Pass/fail criteria (windowed):
//!   1. timing source is `ExtPresentTiming`,
//!   2. all requested frames rendered + presented (minus the usual ≤3 startup
//!      skips), animation driven purely by VSE's frame counter,
//!   3. zero (or explicitly-reported) missed frames over the run,
//!   4. sync mode + queue-priority outcome reported from the host snapshot.
//!
//! Run with:
//! `CARGO_INCREMENTAL=0 cargo run -p vse-bevy --release --example 01_bevy_ring_demo [frames]`

use std::cell::RefCell;
use std::rc::Rc;

use vision_stimulus_engine::prelude::*;
use vse_bevy::{scene::build_demo_scene, BevyProducer, ProducerConfig};
use vse_external_frame::release_channel;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let frames: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);

    // Producer first: Bevy initializes its own wgpu device, allocates and
    // exports the ring, compiles every pipeline (warm-up frame).
    let mut producer = BevyProducer::new(ProducerConfig::default(), build_demo_scene)?;
    let (release_tx, release_rx) = release_channel();
    producer.set_release_rx(release_rx);
    let sync_kind = producer.sync();

    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("Bevy external-frame ring")
        .build()?;

    let state = Rc::new(RefCell::new(Verify::default()));
    let st = state.clone();
    let mut attached = false;

    context.run_buffered::<u64, _>(BufferedConfig::default(), move |event, vse| {
        match event {
            FlipEvent::Render => {
                if !attached {
                    // Device exists only inside the callback (after Resumed).
                    vse.attach_external_frame_source(
                        producer.export_ring().map_err(box_err)?,
                        release_tx.clone(),
                    )?;
                    attached = true;
                }
                let n = vse.frame_number();
                let slot = producer.render_frame(n).map_err(box_err)?;
                vse.queue_external_frame(slot)?;
                vse.flip_with_payload(None, n)?;
                if n + 1 >= frames {
                    vse.close();
                }
            }
            FlipEvent::Presented { flip_info, .. } => {
                let mut s = st.borrow_mut();
                if s.source.is_none() {
                    s.source = Some(vse.timing_source());
                    let caps = vse.capture_host_info().timing;
                    s.queue_priority = caps.queue_global_priority;
                }
                s.presented += 1;
                s.missed_total += flip_info.missed_count as u64;
                if flip_info.on_target {
                    s.on_target += 1;
                }
                if let Some(last) = s.last_present {
                    let dt_us = flip_info.present_time.as_micros() as i64
                        - last.as_micros() as i64;
                    if s.dt_count == 0 {
                        s.min_dt_us = dt_us;
                        s.max_dt_us = dt_us;
                    } else {
                        s.min_dt_us = s.min_dt_us.min(dt_us);
                        s.max_dt_us = s.max_dt_us.max(dt_us);
                    }
                    s.sum_dt_us += dt_us;
                    s.dt_count += 1;
                }
                s.last_present = Some(flip_info.present_time);
            }
            _ => {}
        }
        Ok(())
    })?;

    report(&state.borrow(), frames, sync_kind);
    Ok(())
}

fn box_err(e: vse_bevy::ProducerError) -> VSEError {
    VSEError::EventLoop(format!("producer: {e}"))
}

#[derive(Default)]
struct Verify {
    source: Option<TimingSource>,
    queue_priority: Option<String>,
    presented: u64,
    missed_total: u64,
    on_target: u64,
    last_present: Option<Timestamp>,
    min_dt_us: i64,
    max_dt_us: i64,
    sum_dt_us: i64,
    dt_count: u64,
}

fn report(s: &Verify, requested: u64, sync_kind: vse_external_frame::SyncKind) {
    println!("\n──────── Bevy external-frame ring ────────");
    println!("timing source        : {:?}", s.source);
    println!("frame sync           : {sync_kind:?}");
    println!(
        "queue priority       : {}",
        s.queue_priority.as_deref().unwrap_or("unknown")
    );
    println!("requested / presented: {requested} / {}", s.presented);
    println!("missed (Σ count)     : {}", s.missed_total);
    println!("on_target            : {}/{}", s.on_target, s.presented);
    if s.dt_count > 0 {
        println!(
            "inter-present dt     : mean {:.2} ms, min {:.2} ms, max {:.2} ms",
            s.sum_dt_us as f64 / s.dt_count as f64 / 1000.0,
            s.min_dt_us as f64 / 1000.0,
            s.max_dt_us as f64 / 1000.0,
        );
    }

    let is_ext = s.source == Some(TimingSource::ExtPresentTiming);
    let counts_ok = s.presented <= requested && requested.saturating_sub(s.presented) <= 3;
    println!("──────────────────────────────────────────");
    if !is_ext {
        println!("SKIP: backend is {:?}, not ExtPresentTiming.", s.source);
    } else if counts_ok && s.missed_total == 0 {
        println!("PASS ✔  sustained refresh, zero missed frames, VSE sole present authority");
    } else if counts_ok {
        println!(
            "PASS (with misses)  {} missed frame(s) over {} — see stats above",
            s.missed_total, s.presented
        );
    } else {
        println!("FAIL x  presented count off: {} of {requested}", s.presented);
    }
    println!();
}
