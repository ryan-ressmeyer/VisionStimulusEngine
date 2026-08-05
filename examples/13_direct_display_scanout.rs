//! Example: Direct-display scanout timing verification (VK_EXT_present_timing, Subsystem B3)
//!
//! This is also VSE's entry point for direct-display mode generally: it acquires
//! the display with no compositor in the path and drives it end to end. (It
//! supersedes the earlier minimal "acquire + moving bar" direct-display demo.)
//!
//! The direct-display B3 characterization. VSE owns this swapchain path without a compositor.
//! Synchronous `flip()` waits for presentation-engine completion, then reads per-present feedback
//! or the qualified calibrated-clock fallback described in `docs/timing-conformance.md`. The run checks:
//!
//!   1. selected backend is `ExtPresentTiming`,
//!   2. `present_id` is non-zero and strictly monotonic,
//!   3. `present_time` is a real **scanout-domain** timestamp: monotonic, warmup deltas at the
//!      panel's refresh cadence, and tracking the calibrated scanout clock — median
//!      `|present_time − scanout_now|` well under one refresh (CPU-fallback present_time would sit
//!      a startup-offset ~10 ms away),
//!   4. **scheduling lands on target**: scheduled flips use a fixed anchor (`t0 + k·T`, the
//!      clock-model way); normal scheduled frames hold steady refresh cadence with `on_target`
//!      true, and periodic **deliberate multi-vblank gaps** actually land that many vblanks later
//!      (measured). VSE software-paces scheduled flips against the scanout clock, so this passes
//!      regardless of whether the display path holds `targetTime` on its own.
//!   5. **whether this display path holds `targetTime` requests** — a separate, appended phase
//!      runs with VSE's software pacing disabled. It requests deliberate multi-vblank gaps and
//!      measures where scanout lands. The verdict
//!      is recorded into `HostInfo.timing.absolute_scheduling_enforced`. Phases 1–4 cannot answer
//!      this: with pacing on, an on-target gap only proves VSE's own pacing loop works.
//!
//! Scanout source: `present_time` prefers the driver's per-present `IMAGE_FIRST_PIXEL_OUT` feedback
//! and falls back to sampling the calibrated `PRESENT_STAGE_LOCAL` clock after `wait_for_present`
//! when feedback is all-zero. If you see the fallback, check the swapchain present-timing opt-in and
//! whether the display was blanked before attributing the result to a driver — see
//! docs/timing-conformance.md#capability-and-conformance-evidence.
//!
//! It records every flip to `b3_direct_display/frames.csv` and prints a PASS/FAIL summary. It
//! **auto-terminates** after `[frames]` plus the characterization phase — no SIGINT (which bricks
//! the VT); Escape also exits.
//!
//! Run from a spare TTY:
//! ```bash
//! ./target/debug/examples/13_direct_display_scanout [frames] > /tmp/b3.txt 2>&1
//! ```

use vision_stimulus_engine::core::{absolute_scheduling_verdict, SchedulingTrial};
use vision_stimulus_engine::prelude::*;

/// Every Nth scheduled frame is a deliberate gap event.
const GAP_EVERY: u64 = 50;
/// A gap event schedules this many vblanks ahead (vs 1 for a normal scheduled frame).
const GAP_VBLANKS: u64 = 3;

/// Frames appended after the main run to characterize whether this display path holds
/// `VkPresentTimingInfoEXT.targetTime` requests, with VSE's software pacing disabled.
///
/// This must be its own phase: while VSE paces scheduled presents itself, a gap landing on target
/// only proves VSE's pacing loop works; the presentation path could ignore `targetTime` and produce
/// the same result. With pacing off, a path that does not hold a multi-vblank request presents at
/// the next opportunity, which is the discriminator.
const ENFORCE_FRAMES: u64 = 90;
/// Within the characterization phase, every Nth frame requests a multi-vblank gap.
const ENFORCE_GAP_EVERY: u64 = 6;

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

    // --- Absolute-scheduling characterization (appended phase, software pacing OFF) ---
    let mut enforce_started = false;
    let mut enforce_seq: u64 = 0;
    let mut trials: Vec<SchedulingTrial> = Vec::new();
    let enforce_start = total;
    let grand_total = total + ENFORCE_FRAMES;

    context.run(move |vse| {
        if source.is_none() {
            source = Some(vse.timing_source());
            eprintln!("Backend: {}", vse.display_backend().description());
            eprintln!("Selected backend: {}", vse.timing_source());
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

        // --- Phase switch: enter the pacing-off characterization phase once ---
        let enforcing = n >= enforce_start;
        if enforcing && !enforce_started {
            enforce_started = true;
            vse.set_software_present_pacing(false);
            // Re-anchor: targets from here are measured against where scanout actually is now.
            anchor_us = Some(last_present_us.unwrap_or(0));
            vblank_idx = 0;
            eprintln!(
                "\n--- absolute-scheduling characterization: software pacing OFF for \
                 {ENFORCE_FRAMES} frames (targetTime is now the only scheduler) ---"
            );
        }

        let scheduled = n >= warmup;
        let mut is_gap = false;

        if enforcing {
            // Same fixed-anchor scheme, but every ENFORCE_GAP_EVERY-th frame asks for a
            // GAP_VBLANKS-vblank jump with nothing but the hardware target to enforce it.
            let anchor = *anchor_us.get_or_insert(0);
            is_gap = enforce_seq > 0 && enforce_seq % ENFORCE_GAP_EVERY == 0;
            vblank_idx += if is_gap { GAP_VBLANKS } else { 1 };
            enforce_seq += 1;
            let target_us = anchor + vblank_idx * refresh_us - refresh_us / 2;
            let info = vse.flip(Some(Timestamp::from_micros(target_us)))?;

            let pt = info.present_time.as_micros();
            if let (Some(prev), true) = (last_present_us, is_gap) {
                // Round the measured gap to whole vblanks.
                let observed = (pt.saturating_sub(prev) + refresh_us / 2) / refresh_us;
                trials.push(SchedulingTrial {
                    requested_vblanks: GAP_VBLANKS,
                    observed_vblanks: observed,
                });
            }
            last_present_us = Some(pt);

            vse.record_frame(Row {
                idx: n,
                scheduled: true,
                gap_event: is_gap,
                present_id: info.present_id,
                on_target: info.on_target,
                missed: info.missed,
            })?;

            n += 1;
            if n >= grand_total {
                let verdict = absolute_scheduling_verdict(&trials);
                vse.record_absolute_scheduling_enforced(verdict);
                report_enforcement(&trials, verdict);
                vse.request_exit();
            }
            return Ok(());
        }

        // Compute the EXT target request for scheduled frames from the fixed anchor.
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
            // Do not exit here: the pacing-off characterization phase runs next.
        }
        Ok(())
    })?;

    eprintln!("Clean shutdown.");
    Ok(())
}

/// Report the display-path `targetTime` characterization.
///
/// This measurement separates path behavior from VSE's synchronous pacing because it runs with
/// software pacing disabled.
fn report_enforcement(trials: &[SchedulingTrial], verdict: Option<bool>) {
    println!("\n──────── Absolute scheduling (targetTime) enforcement ────────");
    println!("software pacing            : DISABLED for this phase");
    println!("gap trials                 : {}", trials.len());
    if !trials.is_empty() {
        let on_target = trials
            .iter()
            .filter(|t| t.observed_vblanks >= t.requested_vblanks)
            .count();
        println!(
            "landed at/after target     : {}/{}  (requested {GAP_VBLANKS} vblanks each)",
            on_target,
            trials.len()
        );
        let mut observed: Vec<u64> = trials.iter().map(|t| t.observed_vblanks).collect();
        observed.sort_unstable();
        println!("observed vblank gaps       : {observed:?}");
    }
    // Intermittent path behavior can look reliable, so report it separately from a clean result.
    let unstable = !trials.is_empty() && {
        let hit = trials
            .iter()
            .filter(|t| t.observed_vblanks >= t.requested_vblanks)
            .count();
        hit > 0 && hit < trials.len()
    };

    match verdict {
        Some(true) => println!(
            "VERDICT: this path held targetTime requests in the completed trials."
        ),
        Some(false) => println!(
            "VERDICT: this path did not hold targetTime requests — presents landed at the next opportunity.\n\
             Synchronous VSE pacing is re-enabled outside this phase."
        ),
        None => println!(
            "VERDICT: inconclusive — no multi-vblank trials completed; cannot discriminate."
        ),
    }
    if unstable {
        println!(
            "WARNING: enforcement was INCONSISTENT across trials (see the gaps above). Treat \
             targetTime behavior as unusable on this path and keep VSE's synchronous software \
             pacing on; re-run to confirm before recording any conclusion."
        );
    }
    println!("Recorded into HostInfo as timing.absolute_scheduling_enforced.");
    println!("──────────────────────────────────────────────────────────────\n");
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
    println!("selected backend       : {:?}", s.source);
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
        println!("PASS ✔  scanout-domain receipts tracked vblank cadence and software-paced requests landed at/after target");
    } else {
        println!("FAIL x  see fields above");
    }
    println!("CSV: b3_direct_display/frames.csv");
    println!();
}
