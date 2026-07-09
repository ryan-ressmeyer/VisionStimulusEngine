//! Example: Measure PRESENT_STAGE_LOCAL ↔ CLOCK_MONOTONIC offset and drift
//!
//! Samples the display's scanout clock against the host monotonic clock every frame via
//! `vkGetCalibratedTimestampsKHR` (see `VSEContext::sample_present_calibration`), then reports
//! the clock offset, its relative drift in ppm, and the per-read `maxDeviation` noise.
//!
//! This characterizes whether the present-timing calibration subsystem needs only a static
//! offset (drift below the read-noise floor) or a drift-rate model. It is a diagnostic, not a
//! stimulus: it runs a plain vsync loop for a fixed duration, then writes a CSV and a summary.
//!
//! Run with: `CARGO_INCREMENTAL=0 cargo run --example 09_present_clock_drift [seconds]`

use std::time::Instant;
use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let secs: f64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(60.0);

    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("Present Clock Drift")
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
            report(&samples);
            return Err(VSEError::Window("done".to_string()));
        }
        Ok(())
    })?;

    Ok(())
}

/// Print a summary and write the raw samples to `present_clock_samples.csv`.
fn report(samples: &[CalibrationSample]) {
    if samples.len() < 2 {
        eprintln!(
            "Only {} sample(s) collected — cannot fit drift.",
            samples.len()
        );
        return;
    }

    // Raw CSV for deeper offline analysis.
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

    // Offset = mono - stage; fit offset (y) vs elapsed monotonic time (x), both in ns,
    // rebased to the first sample so f64 keeps full precision.
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
    let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 }; // ns of offset per ns elapsed
    let intercept = mean_y - slope * mean_x;
    let drift_ppm = slope * 1e6;

    // Residual std after removing the fitted drift — the achievable offset stability.
    let mut ss_res = 0.0;
    for (x, y) in xs.iter().zip(ys.iter()) {
        let pred = intercept + slope * x;
        ss_res += (y - pred) * (y - pred);
    }
    let resid_std_ns = (ss_res / n).sqrt();

    let span_s = (xs[xs.len() - 1] - xs[0]) / 1e9;
    let offset_first = ys[0];
    let offset_last = ys[ys.len() - 1];

    // Read-noise (maxDeviation) distribution.
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
