//! Async Bevy latest-ready/hold-last verification demo.
//!
//! This example runs Bevy on a worker thread and keeps VSE's flip loop
//! nonblocking. Each VSE frame requests the newest desired Bevy frame, drains
//! whatever the worker has finished, queues those ready frames, and flips
//! immediately. `ExternalFramePolicy::LatestReadyHoldLast` displays the newest
//! queued frame when one exists and repeats the pinned frame otherwise.
//!
//! Run:
//! `CARGO_INCREMENTAL=0 cargo run -p vse-bevy --profile demo --example 03_async_latest_ready_demo -- [frames]`
//!
//! Optional timeline sync probe/path:
//! `VSE_BEVY_FORCE_TIMELINE=1 CARGO_INCREMENTAL=0 cargo run -p vse-bevy --profile demo --example 03_async_latest_ready_demo -- [frames]`
//!
//! Interpret the per-frame log as the external-stream behavior, not as a VSE
//! timing failure: repeated/pinned external frames mean VSE chose to flip on
//! time rather than wait for Bevy.

use std::cell::RefCell;
use std::rc::Rc;

use vision_stimulus_engine::prelude::*;
use vse_bevy::{
    scene::build_demo_scene, AsyncBevyProducer, ProducerConfig, ProducerError, ReadyFrame,
};
use vse_external_frame::{SlotIndex, SyncKind};

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
        .unwrap_or(300);

    let config = ProducerConfig {
        // Pinned-slot mode needs one held display slot plus room for producer
        // work and VSE in-flight submits. Four slots is the recommended
        // starting point for run_buffered(depth = 1).
        ring_len: 4,
        ..ProducerConfig::default()
    };
    let mut producer = AsyncBevyProducer::spawn(config, build_demo_scene)?;
    let sync_kind = producer.sync();
    let extent = producer.extent();
    let timeline_forced = std::env::var_os("VSE_BEVY_FORCE_TIMELINE").is_some();

    let context = VSEContext::builder()
        .with_window_size(extent[0], extent[1])
        .with_title("Async Bevy latest-ready hold-last")
        .build()?;

    let state = Rc::new(RefCell::new(DemoState::default()));
    let st = state.clone();
    let mut attached = false;

    context.run_buffered::<String, _>(BufferedConfig::default(), move |event, vse| {
        match event {
            FlipEvent::Render => {
                if !attached {
                    vse.attach_external_frame_source_with_policy(
                        producer.export_ring().map_err(box_producer_err)?,
                        producer.release_tx(),
                        ExternalFramePolicy::LatestReadyHoldLast,
                    )?;
                    attached = true;
                }

                let vse_frame = vse.frame_number();
                producer
                    .request_frame(vse_frame)
                    .map_err(box_producer_err)?;

                let mut ready = Vec::new();
                while let Some(frame) = producer.try_recv_ready().map_err(box_producer_err)? {
                    vse.queue_external_frame_with_timeline_value(frame.slot, frame.timeline_value)?;
                    ready.push(frame);
                }

                let log = st.borrow_mut().plan_frame_log(vse_frame, &ready);
                vse.flip_with_payload(None, log.describe())?;

                if vse_frame + 1 >= frames {
                    vse.close();
                }
            }
            FlipEvent::Presented { flip_info, payload } => {
                let mut s = st.borrow_mut();
                s.presented += 1;
                s.missed_total += flip_info.missed_count as u64;
                if s.timing_source.is_none() {
                    s.timing_source = Some(vse.timing_source());
                }
                println!(
                    "{} on_target={} missed_count={}",
                    payload, flip_info.on_target, flip_info.missed_count
                );
            }
            _ => {}
        }
        Ok(())
    })?;

    report(&state.borrow(), frames, sync_kind, timeline_forced);
    Ok(())
}

fn box_producer_err(e: ProducerError) -> VSEError {
    VSEError::EventLoop(format!("producer: {e}"))
}

#[derive(Default)]
struct DemoState {
    latched: Option<DisplayedExternalFrame>,
    timing_source: Option<TimingSource>,
    presented: u64,
    missed_total: u64,
    new_external: u64,
    repeats: u64,
    no_external: u64,
    stale_superseded: u64,
}

impl DemoState {
    fn plan_frame_log(&mut self, vse_frame: u64, ready: &[ReadyFrame]) -> FrameLog {
        let stale_superseded = ready.len().saturating_sub(1) as u64;
        self.stale_superseded += stale_superseded;

        let behavior = if let Some(frame) = ready.last().copied() {
            let displayed = DisplayedExternalFrame::from(frame);
            self.latched = Some(displayed);
            self.new_external += 1;
            ExternalBehavior::New
        } else if self.latched.is_some() {
            self.repeats += 1;
            ExternalBehavior::RepeatPinned
        } else {
            self.no_external += 1;
            ExternalBehavior::NoExternalYet
        };

        FrameLog {
            vse_frame,
            requested_producer_frame: vse_frame,
            displayed: self.latched,
            behavior,
            ready_drained: ready.len() as u64,
            stale_superseded,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct DisplayedExternalFrame {
    producer_frame: u64,
    slot: SlotIndex,
    timeline_value: Option<u64>,
}

impl From<ReadyFrame> for DisplayedExternalFrame {
    fn from(frame: ReadyFrame) -> Self {
        Self {
            producer_frame: frame.frame_number,
            slot: frame.slot,
            timeline_value: frame.timeline_value,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ExternalBehavior {
    New,
    RepeatPinned,
    NoExternalYet,
}

#[derive(Debug, Clone, Copy)]
struct FrameLog {
    vse_frame: u64,
    requested_producer_frame: u64,
    displayed: Option<DisplayedExternalFrame>,
    behavior: ExternalBehavior,
    ready_drained: u64,
    stale_superseded: u64,
}

impl FrameLog {
    fn describe(&self) -> String {
        let external = match self.displayed {
            Some(frame) => format!(
                "producer={} slot={} timeline={}",
                frame.producer_frame,
                frame.slot.0,
                frame
                    .timeline_value
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "n/a".to_string())
            ),
            None => "producer=none slot=n/a timeline=n/a".to_string(),
        };
        let behavior = match self.behavior {
            ExternalBehavior::New => "new external frame displayed",
            ExternalBehavior::RepeatPinned => "repeated/pinned frame",
            ExternalBehavior::NoExternalYet => "no external frame ready yet",
        };
        format!(
            "vse_frame={} requested={} behavior=\"{}\" ready_drained={} stale_superseded={} {}",
            self.vse_frame,
            self.requested_producer_frame,
            behavior,
            self.ready_drained,
            self.stale_superseded,
            external,
        )
    }
}

fn report(s: &DemoState, requested: u64, sync_kind: SyncKind, timeline_forced: bool) {
    println!("\n──── Async Bevy latest-ready hold-last ────");
    println!("timing source          : {:?}", s.timing_source);
    println!("frame sync             : {sync_kind:?}");
    println!("VSE_BEVY_FORCE_TIMELINE: {timeline_forced}");
    println!("requested / presented  : {requested} / {}", s.presented);
    println!("new external frames    : {}", s.new_external);
    println!("repeated pinned frames : {}", s.repeats);
    println!("initial blanks         : {}", s.no_external);
    println!("stale ready superseded : {}", s.stale_superseded);
    println!("missed (Σ count)       : {}", s.missed_total);
    println!("──────────────────────────────────────────");
    println!(
        "Repeats are expected when Bevy misses a VSE deadline; VSE kept flipping and reused the pinned slot."
    );
    if timeline_forced && sync_kind != SyncKind::Timeline {
        println!(
            "Timeline was requested but unavailable; the producer fell back to {sync_kind:?}."
        );
    }
    println!();
}
