//! Example: Direct-display scanout timing verification (VK_EXT_present_timing, Subsystem B3)
//!
//! The authoritative B3 test. On the direct-display path VSE owns the display (no compositor), so
//! the synchronous `flip()` blocks on `vkWaitForPresent2KHR` until the frame begins scanout and
//! reads its real scanout time. This example verifies, end to end:
//!
//!   1. timing source is `ExtPresentTiming`,
//!   2. `present_id` is non-zero and strictly monotonic,
//!   3. `present_time` is a real **scanout-domain** timestamp: monotonic, warmup deltas at the
//!      panel's refresh cadence, and tracking the calibrated scanout clock — median
//!      `|present_time − scanout_now|` well under one refresh (CPU-fallback present_time would sit
//!      a startup-offset ~10 ms away),
//!   4. **scheduling lands on target**: scheduled flips use a fixed anchor (`t0 + k·T`, the
//!      clock-model way); normal scheduled frames hold steady refresh cadence with `on_target`
//!      true, and periodic **deliberate multi-vblank gaps** actually land that many vblanks later
//!      (measured). VSE software-paces scheduled flips against the scanout clock, so this passes
//!      whether or not the driver enforces `targetTime`. (Whether the *hardware* honors `targetTime`
//!      — `absolute_scheduling_enforced` — is a separate driver-conformance fact: measured false on
//!      Intel/ANV/Mesa 26.1; see docs/clock-synchronization.md §6.)
//!
//! Scanout source: measured on Intel/ANV/Mesa 26.1, `vkGetPastPresentationTimingEXT` returns
//! *complete* records that correlate by `present_id` but stub the stage timestamps
//! (`IMAGE_FIRST_PIXEL_OUT` = 0). VSE therefore samples the calibrated `PRESENT_STAGE_LOCAL` clock
//! right after `wait_for_present` for `present_time`; on a driver that fills the feedback stage
//! times, that true per-present value is preferred automatically.
//!
//! It records every flip to `b3_direct_display/frames.csv` and prints a PASS/FAIL summary. It
//! **auto-terminates** after `[frames]` (default 640) — no SIGINT (which bricks the VT); Escape
//! also exits.
//!
//! Run from a spare TTY:
//! ```bash
//! ./target/debug/examples/13_direct_display_scanout [frames] > /tmp/b3.txt 2>&1
//! ```

use vision_stimulus_engine::prelude::*;

/// Every Nth scheduled frame is a deliberate gap event.
const GAP_EVERY: u64 = 50;
/// A gap event schedules this many vblanks ahead (vs 1 for a normal scheduled frame).
const GAP_VBLANKS: u64 = 3;

#[derive(serde::Serialize)]
struct Row {
    idx: u64,
    scheduled: bool,
    gap_event: bool,
    present_id: u64,
    on_target: bool,
    missed: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    let total: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(640);
    let warmup: u64 = (total / 5).clamp(30, 120);

    let session = ExperimentSession::builder()
        .with_writer(CsvDataWriter::new("b3_direct_display/"))
        .build()?;

    let context = VSEContext::builder()
        .with_window_mode(WindowMode::DirectDisplay)
        .with_monitor(MonitorSelection::Primary)
        .with_present_mode(PresentMode::Fifo)
        .with_clear_color(0.1, 0.1, 0.1, 1.0)
        .with_session(session)
        .build()?;

    // Verification state.
    let mut source: Option<TimingSource> = None;
    let mut n: u64 = 0;
    let mut present_ids: Vec<u64> = Vec::new();
    let mut pid_monotonic = true;
    let mut last_present_us: Option<u64> = None;
    let mut present_time_monotonic = true;
    let mut warmup_deltas_us: Vec<u64> = Vec::new();
    let mut sched_deltas_us: Vec<u64> = Vec::new(); // normal scheduled frames only (excl. gaps)
    let mut gap_measured_us: Vec<u64> = Vec::new(); // measured Δpresent_time at gap events
    let mut scanout_domain_offsets_us: Vec<u64> = Vec::new();
    let mut refresh_us: u64 = 16_667; // refined from warmup median
    let mut scheduled_frames: u64 = 0;
    let mut on_target_true: u64 = 0;

    // Fixed scheduling anchor (scanout-domain µs) + running target vblank index (clock-model
    // `t0 + k·T`): set when the scheduled phase begins.
    let mut anchor_us: Option<u64> = None;
    let mut vblank_idx: u64 = 0;
    let mut sched_seq: u64 = 0;

    context.run(move |vse| {
        if source.is_none() {
            source = Some(vse.timing_source());
            eprintln!("Backend: {}", vse.display_backend().description());
            eprintln!("Timing source: {}", vse.timing_source());
            eprintln!("Press Escape to exit early; auto-exits after {total} frames.");
        }
        if vse.key_just_pressed(KeyCode::Escape) {
            eprintln!("Escape — exiting early at frame {n}.");
            vse.request_exit();
            return Ok(());
        }

        let c = if n % 2 == 0 { 0.2 } else { 0.45 };
        vse.set_clear_color(c, c, c, 1.0);
        vse.clear()?;

        let scheduled = n >= warmup;
        let mut is_gap = false;

        // Compute the hardware target for scheduled frames from the fixed anchor.
        let target = if scheduled {
            let anchor = *anchor_us.get_or_insert_with(|| last_present_us.unwrap_or(0));
            is_gap = sched_seq > 0 && sched_seq % GAP_EVERY == 0;
            vblank_idx += if is_gap { GAP_VBLANKS } else { 1 };
            sched_seq += 1;
            scheduled_frames += 1;
            // Aim mid-interval before the target vblank so ±jitter still hits the intended cycle.
            let target_us = anchor + vblank_idx * refresh_us - refresh_us / 2;
            Some(Timestamp::from_micros(target_us))
        } else {
            None
        };

        let info = vse.flip(target)?;

        // present_id: non-zero, strictly monotonic.
        if let Some(&last) = present_ids.last() {
            if info.present_id <= last {
                pid_monotonic = false;
            }
        }
        present_ids.push(info.present_id);

        // present_time: monotonic + inter-flip deltas (gap deltas tracked separately).
        let pt = info.present_time.as_micros();
        if let Some(prev) = last_present_us {
            if pt < prev {
                present_time_monotonic = false;
            } else {
                let d = pt - prev;
                if !scheduled {
                    warmup_deltas_us.push(d);
                } else if is_gap {
                    gap_measured_us.push(d);
                } else {
                    sched_deltas_us.push(d);
                }
            }
        }
        last_present_us = Some(pt);

        // present_time must be scanout-domain: close to the scanout clock read now.
        if let Some(s_now) = vse.scanout_now() {
            scanout_domain_offsets_us.push(pt.abs_diff(s_now.as_micros()));
        }

        if scheduled && info.on_target {
            on_target_true += 1;
        }

        // Lock the refresh estimate from the warmup median just before scheduling starts.
        if n + 1 == warmup && !warmup_deltas_us.is_empty() {
            let mut d = warmup_deltas_us.clone();
            d.sort_unstable();
            let med = d[d.len() / 2];
            if (8_000..=40_000).contains(&med) {
                refresh_us = med;
            }
            eprintln!(
                "Measured refresh interval: {:.3} ms",
                refresh_us as f64 / 1000.0
            );
        }

        vse.record_frame(Row {
            idx: n,
            scheduled,
            gap_event: is_gap,
            present_id: info.present_id,
            on_target: info.on_target,
            missed: info.missed,
        })?;

        n += 1;
        if n >= total {
            report(Summary {
                source,
                present_ids: &present_ids,
                pid_monotonic,
                present_time_monotonic,
                warmup_deltas_us: &warmup_deltas_us,
                sched_deltas_us: &sched_deltas_us,
                gap_measured_us: &gap_measured_us,
                scanout_domain_offsets_us: &scanout_domain_offsets_us,
                refresh_us,
                scheduled_frames,
                on_target_true,
            });
            vse.request_exit();
        }
        Ok(())
    })?;

    eprintln!("Clean shutdown.");
    Ok(())
}

struct Summary<'a> {
    source: Option<TimingSource>,
    present_ids: &'a [u64],
    pid_monotonic: bool,
    present_time_monotonic: bool,
    warmup_deltas_us: &'a [u64],
    sched_deltas_us: &'a [u64],
    gap_measured_us: &'a [u64],
    scanout_domain_offsets_us: &'a [u64],
    refresh_us: u64,
    scheduled_frames: u64,
    on_target_true: u64,
}

fn median(v: &[u64]) -> u64 {
    if v.is_empty() {
        return 0;
    }
    let mut d = v.to_vec();
    d.sort_unstable();
    d[d.len() / 2]
}

fn report(s: Summary) {
    let refresh = s.refresh_us;
    let warmup_med = median(s.warmup_deltas_us);
    let sched_med = median(s.sched_deltas_us);
    let gap_med = median(s.gap_measured_us);
    let scanout_offset_med = median(s.scanout_domain_offsets_us);

    // Normal scheduled cadence within ±10% of one refresh.
    let cadence_ok = (refresh * 9 / 10..=refresh * 11 / 10).contains(&sched_med);
    // Gaps land ~GAP_VBLANKS refreshes later (band [GAP-0.5, GAP+0.5]·refresh) — i.e. the scheduled
    // target was actually hit (here via VSE's scanout-domain software pacing), not free-run cadence.
    let expected_gap = GAP_VBLANKS * refresh;
    let gap_lo = expected_gap - refresh / 2;
    let gap_hi = expected_gap + refresh / 2;
    let scheduling_lands = !s.gap_measured_us.is_empty() && (gap_lo..=gap_hi).contains(&gap_med);
    // present_time is scanout-domain iff it tracks the scanout clock (median |Δ| ≪ one refresh).
    let scanout_domain_ok =
        !s.scanout_domain_offsets_us.is_empty() && scanout_offset_med < refresh / 2;

    let nonzero = s.present_ids.iter().all(|&id| id != 0);
    let is_ext = s.source == Some(TimingSource::ExtPresentTiming);
    let on_target_ok = s.scheduled_frames > 0 && s.on_target_true == s.scheduled_frames;

    println!("\n──────── Direct-Display Scanout Timing (B3) ────────");
    println!("timing source          : {:?}", s.source);
    println!("flips                  : {}", s.present_ids.len());
    println!(
        "present_id range       : {}..={}",
        s.present_ids.first().copied().unwrap_or(0),
        s.present_ids.last().copied().unwrap_or(0)
    );
    println!("present_id non-zero    : {nonzero}");
    println!("present_id monotonic   : {}", s.pid_monotonic);
    println!("present_time monotonic : {}", s.present_time_monotonic);
    println!(
        "present_time scanout   : {scanout_domain_ok}  (median |pt−scanout_now| = {:.3} ms)",
        scanout_offset_med as f64 / 1000.0
    );
    println!("refresh interval       : {:.3} ms", refresh as f64 / 1000.0);
    println!(
        "warmup median dt       : {:.3} ms  ({} samples)",
        warmup_med as f64 / 1000.0,
        s.warmup_deltas_us.len()
    );
    println!(
        "scheduled median dt    : {:.3} ms  ({} samples)  cadence_ok={cadence_ok}",
        sched_med as f64 / 1000.0,
        s.sched_deltas_us.len()
    );
    println!(
        "scheduled on_target    : {}/{}",
        s.on_target_true, s.scheduled_frames
    );
    println!(
        "gap events             : {} × ~{} vblanks → measured {:.3} ms (expected {:.3} ms)  lands={scheduling_lands}",
        s.gap_measured_us.len(),
        GAP_VBLANKS,
        gap_med as f64 / 1000.0,
        expected_gap as f64 / 1000.0,
    );
    println!("────────────────────────────────────────────────────");

    let pass = is_ext
        && nonzero
        && s.pid_monotonic
        && s.present_time_monotonic
        && scanout_domain_ok
        && cadence_ok
        && on_target_ok
        && scheduling_lands;
    if !is_ext {
        println!("SKIP: backend is {:?}, not ExtPresentTiming.", s.source);
    } else if pass {
        println!("PASS ✔  scanout-domain present_time at vblank cadence + scheduled flips land on target (software-paced)");
    } else {
        println!("FAIL x  see fields above");
    }
    println!("CSV: b3_direct_display/frames.csv");
    println!();
}
