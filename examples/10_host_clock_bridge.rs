//! Example: Verify the opt-in host↔scanout clock bridge end-to-end on real hardware.
//!
//! Enables the bridge, lets it warm up, then each frame cross-checks three things:
//!   1. `host_to_scanout(now)` (bridge-predicted) vs. `scanout_now()` (directly measured) —
//!      they should agree to within the offset read-noise plus the µs gap between the two reads.
//!   2. `host → scanout → host` round-trips back to the original host timestamp.
//!   3. `scanout_now()` advances at real (wall-clock) rate, to within the fitted drift.
//!
//! Run with: `CARGO_INCREMENTAL=0 cargo run --example 10_host_clock_bridge [seconds]`

use std::time::Instant;
use vision_stimulus_engine::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let secs: f64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(15.0);

    let context = VSEContext::builder()
        .with_window_size(800, 600)
        .with_title("Host Clock Bridge")
        .with_host_clock_bridge()
        .build()?;

    // (cross_check_ns, roundtrip_us, wall_s, scanout_ns)
    let mut cross: Vec<f64> = Vec::new();
    let mut roundtrip: Vec<f64> = Vec::new();
    let mut advance: Vec<(f64, u64)> = Vec::new();
    let start = Instant::now();
    let mut warned = false;

    context.run(move |ctx| {
        let wall = start.elapsed().as_secs_f64();

        // Only measure once the bridge has fitted (skip warm-up).
        if ctx.host_clock_bridge_drift_ppm().is_some() {
            // Bracket the direct scanout read with host timestamps so the read-latency gap
            // cancels: the true host time of `s_direct` is the midpoint of before/after.
            let h_before = ctx.clock().now();
            let s_direct = ctx.scanout_now();
            let h_after = ctx.clock().now();
            let h_mid = Timestamp::from_micros((h_before.as_micros() + h_after.as_micros()) / 2);
            if let (Some(s_bridge), Some(s_direct)) = (ctx.host_to_scanout(h_mid), s_direct) {
                cross.push(s_direct.as_nanos() as f64 - s_bridge.as_nanos() as f64);
                advance.push((wall, s_direct.as_nanos()));
                if let Some(h2) = ctx.scanout_to_host(s_bridge) {
                    roundtrip.push(h2.as_micros() as f64 - h_mid.as_micros() as f64);
                }
            }
        } else if wall > 5.0 && !warned {
            warned = true;
            eprintln!(
                "bridge never warmed up (source={:?}) — cannot verify.",
                ctx.timing_source()
            );
        }

        ctx.clear()?;
        ctx.flip(None)?;

        if wall >= secs {
            report(
                ctx.host_clock_bridge_drift_ppm(),
                &cross,
                &roundtrip,
                &advance,
            );
            return Err(VSEError::Window("done".to_string()));
        }
        Ok(())
    })?;

    Ok(())
}

fn report(drift_ppm: Option<f64>, cross: &[f64], roundtrip: &[f64], advance: &[(f64, u64)]) {
    println!("\n=== Host ↔ Scanout bridge verification ===");
    let Some(ppm) = drift_ppm else {
        println!("bridge unavailable — nothing verified.");
        return;
    };
    if cross.len() < 2 {
        println!("insufficient samples ({}) — nothing verified.", cross.len());
        return;
    }
    // Drift over a 2 s sliding window is an intentionally *instantaneous* estimate and is noisy
    // by several ppm (a short window pins the offset tightly but the slope loosely — see the
    // drift measurement in docs/clock-synchronization.md). Conversion accuracy is offset-driven,
    // so this noise barely affects the numbers below.
    println!("fitted drift:        {ppm:.3} ppm  (2 s-window instantaneous estimate; noisy)");

    // Robust |·| percentiles — the read noise is one-sided with a long tail, so median/p95
    // characterize it better than max.
    let robust = |v: &[f64]| -> (f64, f64) {
        let mut a: Vec<f64> = v.iter().map(|x| x.abs()).collect();
        a.sort_by(|x, y| x.partial_cmp(y).unwrap());
        let med = a[a.len() / 2];
        let p95 = a[((a.len() as f64 * 0.95) as usize).min(a.len() - 1)];
        (med, p95)
    };
    let (cx_med, cx_p95) = robust(cross);
    println!(
        "bridge vs direct:    median |{:.1}| µs, p95 |{:.1}| µs  ({} samples, gap-corrected)",
        cx_med / 1e3,
        cx_p95 / 1e3,
        cross.len()
    );
    let (rt_med, rt_p95) = robust(roundtrip);
    println!("host→scanout→host:   median |{rt_med:.2}| µs, p95 |{rt_p95:.2}| µs");

    // Scanout advance vs wall time: slope should be ~1.0 (ns of scanout per s of wall / 1e9).
    let n = advance.len() as f64;
    let mx = advance.iter().map(|(w, _)| *w).sum::<f64>() / n;
    let s0 = advance[0].1;
    let my = advance.iter().map(|(_, s)| (*s - s0) as f64).sum::<f64>() / n;
    let (mut sxx, mut sxy) = (0.0, 0.0);
    for (w, s) in advance {
        sxx += (w - mx) * (w - mx);
        sxy += (w - mx) * ((*s - s0) as f64 - my);
    }
    let rate = if sxx > 0.0 { (sxy / sxx) / 1e9 } else { 0.0 };
    println!("scanout advance:     {rate:.6} s scanout / s wall  (expect ≈ 1.0)");

    // The single-read `s_direct` carries one-sided read noise, so bridge-vs-direct scatter at
    // the read-noise scale (~tens of µs, p95 ~100 µs) is expected and healthy. The strong proofs
    // are the exact round-trip and the ~1.0 scanout rate.
    let ok = cx_p95 < 150_000.0 && rt_p95 < 50.0 && (rate - 1.0).abs() < 1e-4;
    println!(
        "\n{}",
        if ok {
            "PASS"
        } else {
            "CHECK — see values above"
        }
    );
}
