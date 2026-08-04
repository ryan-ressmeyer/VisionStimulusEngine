//! Present-timing internals — the low-level VK_EXT_present_timing machinery.
//!
//! Consolidates three former standalone diagnostics into one example, selected
//! by a mode argument (each builds its own context; a single context owns one
//! present loop, so the modes cannot share one run). None of these has a
//! Psychtoolbox equivalent — they exercise VSE's scanout-clock timing stack and
//! record what the driver *actually* does (see CLAUDE.md "Driver conformance").
//!
//! * `drift` — sample PRESENT_STAGE_LOCAL ↔ CLOCK_MONOTONIC every frame and fit
//!   the offset + relative drift (informs the calibration bridge). Optional arg:
//!   seconds (default 60).
//! * `feedback` — raw `vkQueuePresentKHR` path with `VkPresentId2` + scanout
//!   feedback via `vkGetPastPresentationTimingEXT` (Subsystem B1). Optional arg:
//!   frames (default 200).
//! * `buffered` — the same raw path pipelined through `run_buffered()`,
//!   confirming present-id/payload correlation (Subsystem B2). Optional args:
//!   frames (default 200), depth (default 1).
//!
//! # Running
//!
//! ```bash
//! cargo run --example 10_present_timing_internals            # drift, 60 s
//! cargo run --example 10_present_timing_internals feedback   # B1, 200 frames
//! cargo run --example 10_present_timing_internals buffered 200 2
//! ```

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Instant;
use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "drift".to_string());
    match mode.as_str() {
        "drift" => {
            let secs = std::env::args()
                .nth(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or(60.0);
            mode_drift(secs)
        }
        "feedback" => {
            let frames = std::env::args()
                .nth(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or(200);
            mode_feedback(frames)
        }
        "buffered" => {
            let frames = std::env::args()
                .nth(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or(200);
            let depth = std::env::args()
                .nth(3)
                .and_then(|s| s.parse().ok())
                .unwrap_or(1);
            mode_buffered(frames, depth)
        }
        other => {
            eprintln!("unknown mode {other:?}; expected one of: drift | feedback | buffered");
            Ok(())
        }
    }
}

/// Accumulated `IMAGE_FIRST_PIXEL_OUT` evidence across a run's feedback records.
///
/// Two facts that are easy to conflate and must be reported apart:
///
/// * **reported** — the driver put an `IMAGE_FIRST_PIXEL_OUT` entry in the record's stage array.
/// * **populated** — that entry carries a *nonzero* timestamp.
///
/// A driver that advertises `VK_EXT_present_timing` but stubs the stage clocks reports the stage
/// with `time == 0`, so a plain `Option::is_some()` test says "true" on exactly the driver the
/// check exists to catch. `VSEState::observe_feedback_conformance` uses the nonzero test; this
/// mirrors it so the example and the library cannot disagree.
#[derive(Default)]
struct FirstPixelOut {
    /// Records whose stage array contained `IMAGE_FIRST_PIXEL_OUT` at all.
    reported: u64,
    /// Records whose `IMAGE_FIRST_PIXEL_OUT` timestamp was nonzero.
    populated: u64,
    /// First nonzero timestamp observed — the proof value.
    sample_ns: Option<u64>,
}

impl FirstPixelOut {
    fn observe(&mut self, first_pixel_out_ns: Option<u64>) {
        let Some(ns) = first_pixel_out_ns else { return };
        self.reported += 1;
        if ns != 0 {
            self.populated += 1;
            self.sample_ns.get_or_insert(ns);
        }
    }

    /// The experiment's actual quantity: did the driver ever give us a real scanout time?
    fn is_populated(&self) -> bool {
        self.populated > 0
    }

    fn print(&self, records: u64) {
        println!(
            "IMAGE_FIRST_PIXEL_OUT    : stage reported {}/{} records, timestamps populated {}/{}",
            self.reported, records, self.populated, records
        );
        match self.sample_ns {
            Some(ns) => println!("  first nonzero sample   : {ns} ns (present-stage-local)"),
            None if self.reported > 0 => println!(
                "  → driver STUBS the stage clock (every reported time == 0): advertised, \
                 not implemented"
            ),
            None => println!("  → driver never reported the stage at all"),
        }
    }
}

// ───────────────────────────── drift ─────────────────────────────

fn mode_drift(secs: f64) -> Result<(), Box<dyn std::error::Error>> {
    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("Present Timing Internals — drift")
        .build()?;

    let mut samples: Vec<CalibrationSample> = Vec::new();
    let start = Instant::now();
    let mut warned = false;

    context.run(move |ctx| {
        match ctx.sample_present_calibration() {
            Some(s) => samples.push(s),
            None if !warned => {
                warned = true;
                eprintln!(
                    "sample_present_calibration() returned None — present-stage calibration \
                     unavailable on this path (source={:?}). Nothing to measure.",
                    ctx.timing_source()
                );
            }
            None => {}
        }

        ctx.clear()?;
        ctx.flip(None)?;

        if start.elapsed().as_secs_f64() >= secs {
            report_drift(&samples);
            return Err(VSEError::Window("done".to_string()));
        }
        Ok(())
    })?;

    Ok(())
}

/// Print a summary and write the raw samples to `present_clock_samples.csv`.
fn report_drift(samples: &[CalibrationSample]) {
    if samples.len() < 2 {
        eprintln!(
            "Only {} sample(s) collected — cannot fit drift.",
            samples.len()
        );
        return;
    }

    let mut csv = String::from("index,stage_ns,mono_ns,offset_ns,max_dev_ns\n");
    for (i, s) in samples.iter().enumerate() {
        let offset = s.mono_ns as i128 - s.stage_ns as i128;
        csv.push_str(&format!(
            "{},{},{},{},{}\n",
            i, s.stage_ns, s.mono_ns, offset, s.max_deviation_ns
        ));
    }
    let path = "present_clock_samples.csv";
    if let Err(e) = std::fs::write(path, &csv) {
        eprintln!("failed to write {path}: {e}");
    }

    let t0 = samples[0].mono_ns as f64;
    let xs: Vec<f64> = samples.iter().map(|s| s.mono_ns as f64 - t0).collect();
    let ys: Vec<f64> = samples
        .iter()
        .map(|s| s.mono_ns as i128 as f64 - s.stage_ns as i128 as f64)
        .collect();

    let n = xs.len() as f64;
    let mean_x = xs.iter().sum::<f64>() / n;
    let mean_y = ys.iter().sum::<f64>() / n;
    let mut sxx = 0.0;
    let mut sxy = 0.0;
    for (x, y) in xs.iter().zip(ys.iter()) {
        sxx += (x - mean_x) * (x - mean_x);
        sxy += (x - mean_x) * (y - mean_y);
    }
    let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    let intercept = mean_y - slope * mean_x;
    let drift_ppm = slope * 1e6;

    let mut ss_res = 0.0;
    for (x, y) in xs.iter().zip(ys.iter()) {
        let pred = intercept + slope * x;
        ss_res += (y - pred) * (y - pred);
    }
    let resid_std_ns = (ss_res / n).sqrt();

    let span_s = (xs[xs.len() - 1] - xs[0]) / 1e9;
    let offset_first = ys[0];
    let offset_last = ys[ys.len() - 1];

    let mut devs: Vec<u64> = samples.iter().map(|s| s.max_deviation_ns).collect();
    devs.sort_unstable();
    let dev_min = devs[0];
    let dev_med = devs[devs.len() / 2];
    let dev_p95 = devs[(devs.len() as f64 * 0.95) as usize];
    let dev_max = devs[devs.len() - 1];

    println!("\n=== PRESENT_STAGE_LOCAL ↔ CLOCK_MONOTONIC ===");
    println!(
        "samples:            {}  over {:.1} s",
        samples.len(),
        span_s
    );
    println!("mean offset:        {:.3} ms  (mono - stage)", mean_y / 1e6);
    println!(
        "offset first→last:  {:.1} µs → {:.1} µs  (Δ {:.1} µs)",
        offset_first / 1e3,
        offset_last / 1e3,
        (offset_last - offset_first) / 1e3
    );
    println!("relative drift:     {drift_ppm:.3} ppm");
    println!(
        "  → over 30 min that is {:.2} ms of accumulated error if left uncorrected",
        drift_ppm * 1e-6 * 1800.0 * 1e3
    );
    println!(
        "residual std:       {:.2} µs  (offset stability after removing drift)",
        resid_std_ns / 1e3
    );
    println!(
        "read noise maxDev:  min {} µs / median {} µs / p95 {} µs / max {} µs",
        dev_min / 1000,
        dev_med / 1000,
        dev_p95 / 1000,
        dev_max / 1000
    );
    println!("raw samples:        {path}");
}

// ─────────────────────────── feedback (B1) ───────────────────────────

fn mode_feedback(frames: u64) -> Result<(), Box<dyn std::error::Error>> {
    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("Present Timing Internals — raw feedback (B1)")
        .build()?;

    let mut source: Option<TimingSource> = None;
    let mut present_ids: Vec<u64> = Vec::new();
    let mut monotonic_ok = true;
    let mut feedback_present_ids: HashSet<u64> = HashSet::new();
    let mut feedback_records: u64 = 0;
    let mut first_pixel_out = FirstPixelOut::default();
    let mut last_present_us: Option<u64> = None;
    let mut present_time_monotonic = true;
    let mut present_deltas_us: Vec<u64> = Vec::new();
    let mut n: u64 = 0;

    context.run(move |ctx| {
        if source.is_none() {
            source = Some(ctx.timing_source());
        }

        let c = if n % 2 == 0 { 0.15 } else { 0.35 };
        ctx.set_clear_color(c, c, c, 1.0);
        ctx.clear()?;
        let flip = ctx.flip(None)?;

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

        let pt = flip.present_time.as_micros();
        if let Some(prev) = last_present_us {
            if pt < prev {
                present_time_monotonic = false;
            } else {
                present_deltas_us.push(pt - prev);
            }
        }
        last_present_us = Some(pt);

        for fb in ctx.scanout_feedback() {
            feedback_records += 1;
            feedback_present_ids.insert(fb.present_id);
            first_pixel_out.observe(fb.first_pixel_out_ns);
        }

        n += 1;
        if n >= frames {
            report_feedback(
                source,
                &present_ids,
                monotonic_ok,
                feedback_records,
                &feedback_present_ids,
                &first_pixel_out,
            );
            present_deltas_us.sort_unstable();
            let med = present_deltas_us
                .get(present_deltas_us.len() / 2)
                .copied()
                .unwrap_or(0);
            let near_refresh = present_deltas_us
                .iter()
                .filter(|&&d| (13_000..=20_000).contains(&d))
                .count();
            println!("present_time monotonic   : {present_time_monotonic}");
            println!(
                "present_time median dt   : {:.2} ms  ({}/{} deltas within 13-20 ms)",
                med as f64 / 1000.0,
                near_refresh,
                present_deltas_us.len()
            );
            println!();
            return Err(VSEError::Window("done".to_string()));
        }
        Ok(())
    })?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn report_feedback(
    source: Option<TimingSource>,
    present_ids: &[u64],
    monotonic_ok: bool,
    feedback_records: u64,
    feedback_present_ids: &HashSet<u64>,
    first_pixel_out: &FirstPixelOut,
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
    first_pixel_out.print(feedback_records);

    let is_ext = source == Some(TimingSource::ExtPresentTiming);
    // PASS covers the present-id/feedback *machinery*, which works independently of whether the
    // driver populates the stage clock; a stubbed clock is called out separately below so the two
    // are never conflated into one verdict.
    let pass = is_ext && nonzero && monotonic_ok && feedback_records > 0;

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
        println!("PASS ✔  raw present + present-id + feedback correlation all working");
        if !first_pixel_out.is_populated() {
            println!(
                "        …but scanout stage timestamps are stubbed — present_time falls back to \
                 the calibrated PRESENT_STAGE_LOCAL clock"
            );
        }
    } else {
        println!("FAIL x  see fields above");
    }
    println!();
}

// ─────────────────────────── buffered (B2) ───────────────────────────

fn mode_buffered(frames: u32, depth: usize) -> Result<(), Box<dyn std::error::Error>> {
    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("Present Timing Internals — buffered (B2)")
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
                if flip_info.present_id == 0 {
                    s.zero_present_id = true;
                }
                if let Some(last) = s.last_present_id {
                    if flip_info.present_id <= last {
                        s.present_id_non_monotonic = true;
                    }
                }
                s.last_present_id = Some(flip_info.present_id);

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

                for fb in vse.scanout_feedback() {
                    s.feedback_records += 1;
                    s.feedback_ids.insert(fb.present_id);
                    s.first_pixel_out.observe(fb.first_pixel_out_ns);
                }
            }
            _ => {}
        }
        Ok(())
    })?;

    report_buffered(&state.borrow(), *render_n.borrow(), depth);
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
    feedback_records: u64,
    first_pixel_out: FirstPixelOut,
}

fn report_buffered(s: &Verify, rendered: u32, depth: usize) {
    println!("\n──────── Buffered Present-Id (B2) ────────");
    println!("timing source            : {:?}", s.source);
    println!("depth                    : {depth}");
    println!("rendered / presented     : {rendered} / {}", s.presented);
    println!("present_id non-zero      : {}", !s.zero_present_id);
    println!("present_id monotonic     : {}", !s.present_id_non_monotonic);
    println!("payload FIFO order       : {}", !s.payload_out_of_order);
    println!("missed frames            : {}", s.missed);
    println!("distinct feedback ids    : {}", s.feedback_ids.len());
    s.first_pixel_out.print(s.feedback_records);

    let is_ext = s.source == Some(TimingSource::ExtPresentTiming);
    let counts_ok = s.presented <= rendered && rendered.saturating_sub(s.presented) <= 3;
    println!("presented≈rendered       : {counts_ok}");

    let pass = is_ext
        && !s.zero_present_id
        && !s.present_id_non_monotonic
        && !s.payload_out_of_order
        && counts_ok
        && !s.feedback_ids.is_empty();

    println!("──────────────────────────────────────────");
    if !is_ext {
        println!("SKIP: backend is {:?}, not ExtPresentTiming.", s.source);
    } else if pass {
        println!("PASS ✔  buffered raw present + present-id correlation + feedback working");
        if !s.first_pixel_out.is_populated() {
            println!("        …but scanout stage timestamps are stubbed (see above)");
        }
    } else {
        println!("FAIL x  see fields above");
    }
    println!();
}
