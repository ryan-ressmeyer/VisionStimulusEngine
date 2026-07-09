//! Opt-in host-clock bridge: `CLOCK_MONOTONIC` ↔ present-stage-local (scanout) clock.
//!
//! The scanout clock is VSE's primary experimental clock; this bridge exists only to place
//! host-originated events (key presses, network) into scanout time, or expose an optional
//! host-clock value for a scanout timestamp. It is **off the presentation hot path**.
//!
//! It models the offset between the two clocks as a line `offset(mono) = a + b·(mono − ref)`
//! where `offset = mono_ns − stage_ns`, fitted over a sliding window of paired samples. Two
//! properties measured on real hardware (see `docs/clock-synchronization.md`) drive the design:
//!
//! - **Relative drift is real (~2 ppm) and stable**, so the fit carries a slope `b`, not just an
//!   offset — otherwise a fixed offset accrues ~3.5 ms over a 30-min session.
//! - **Read noise is one-sided** (jitter can only make a read appear *later*, inflating the
//!   measured offset), so the true offset is the *lower envelope* of the samples. The fit takes
//!   the **minimum offset per time-bin** and regresses through those minima; a mean would be
//!   biased high.

use std::collections::VecDeque;
use std::time::Duration;

use super::provider::CalibrationSample;

/// A fitted line `offset(mono) = a_offset_ns + b·(mono − ref_mono_ns)`, `offset = mono − stage`.
///
/// `ref_mono_ns` and `a_offset_ns` are kept exact (integer) so the ~10¹³-ns magnitudes never
/// touch lossy float math; only the tiny drift term `b·Δ` is evaluated in `f64`.
#[derive(Debug, Clone, Copy)]
struct BridgeFit {
    ref_mono_ns: u64,
    a_offset_ns: i64,
    b: f64,
}

impl BridgeFit {
    /// Modeled offset (`mono − stage`) at an absolute monotonic time, in ns.
    fn offset_at(&self, mono_ns: u64) -> i64 {
        let delta = mono_ns as i64 - self.ref_mono_ns as i64;
        self.a_offset_ns + (self.b * delta as f64).round() as i64
    }
}

/// Sliding-window linear bridge between the host and scanout clocks.
pub struct HostClockBridge {
    window: VecDeque<CalibrationSample>,
    window_ns: u64,
    fit: Option<BridgeFit>,
}

impl HostClockBridge {
    /// Number of time-bins the sliding window is divided into for the lower-envelope fit.
    const NBINS: usize = 20;

    /// Create a bridge that retains samples spanning `window`.
    pub fn new(window: Duration) -> Self {
        Self {
            window: VecDeque::new(),
            window_ns: window.as_nanos() as u64,
            fit: None,
        }
    }

    /// Add a paired sample, evict samples older than the window, and refit.
    ///
    /// Samples must be pushed in monotonic (non-decreasing `mono_ns`) order.
    pub fn push(&mut self, sample: CalibrationSample) {
        self.window.push_back(sample);
        let newest = sample.mono_ns;
        let cutoff = newest.saturating_sub(self.window_ns);
        while let Some(front) = self.window.front() {
            if front.mono_ns < cutoff {
                self.window.pop_front();
            } else {
                break;
            }
        }
        self.refit();
    }

    /// Whether a usable fit is available.
    pub fn is_ready(&self) -> bool {
        self.fit.is_some()
    }

    /// Number of samples currently in the window.
    pub fn sample_count(&self) -> usize {
        self.window.len()
    }

    /// The fitted relative drift in parts per million, if ready.
    pub fn drift_ppm(&self) -> Option<f64> {
        self.fit.map(|f| f.b * 1e6)
    }

    /// Convert an absolute host `CLOCK_MONOTONIC` nanosecond value to an absolute
    /// present-stage-local (scanout) nanosecond value.
    pub fn host_to_scanout_ns(&self, mono_ns: u64) -> Option<u64> {
        let fit = self.fit?;
        let stage = mono_ns as i128 - fit.offset_at(mono_ns) as i128;
        Some(stage.max(0) as u64)
    }

    /// Convert an absolute present-stage-local (scanout) nanosecond value to an absolute host
    /// `CLOCK_MONOTONIC` nanosecond value.
    pub fn scanout_to_host_ns(&self, stage_ns: u64) -> Option<u64> {
        let fit = self.fit?;
        // Solve mono − offset(mono) = stage. offset varies with mono only through the tiny b·Δ
        // term, so one fixed-point step (seeded with the stage-time offset) converges to well
        // under a nanosecond.
        let seed_mono = stage_ns as i128 + fit.a_offset_ns as i128;
        let offset = fit.offset_at(seed_mono.max(0) as u64) as i128;
        let mono = stage_ns as i128 + offset;
        Some(mono.max(0) as u64)
    }

    /// Refit the line through the per-bin minimum offsets (lower envelope).
    fn refit(&mut self) {
        let n = self.window.len();
        if n < 2 {
            self.fit = None;
            return;
        }
        let ref_mono = self.window.front().unwrap().mono_ns;
        let mono_min = ref_mono;
        let mono_max = self.window.back().unwrap().mono_ns;
        let span = mono_max.saturating_sub(mono_min);

        let offset_of = |s: &CalibrationSample| s.mono_ns as i128 - s.stage_ns as i128;

        // Per-bin minimum offset and the mono time at which it occurred.
        let nbins = Self::NBINS.min(n);
        let mut bin_min: Vec<Option<(u64, i128)>> = vec![None; nbins];
        for s in &self.window {
            let idx = if span == 0 {
                0
            } else {
                (((s.mono_ns - mono_min) as u128 * nbins as u128) / span as u128)
                    .min(nbins as u128 - 1) as usize
            };
            let off = offset_of(s);
            match &mut bin_min[idx] {
                Some((_, best)) if *best <= off => {}
                slot => *slot = Some((s.mono_ns, off)),
            }
        }
        let points: Vec<(u64, i128)> = bin_min.into_iter().flatten().collect();

        // Reference offset keeps the regression's y-values small.
        let off0 = points.iter().map(|(_, o)| *o).min().unwrap();

        if points.len() < 2 {
            // Not enough spread to fit a slope — fall back to a static lower-envelope offset.
            self.fit = Some(BridgeFit {
                ref_mono_ns: ref_mono,
                a_offset_ns: off0 as i64,
                b: 0.0,
            });
            return;
        }

        // Least squares of (x = mono − ref, y = offset − off0), both small enough for f64.
        let np = points.len() as f64;
        let xs: Vec<f64> = points.iter().map(|(m, _)| (*m - ref_mono) as f64).collect();
        let ys: Vec<f64> = points.iter().map(|(_, o)| (*o - off0) as f64).collect();
        let mean_x = xs.iter().sum::<f64>() / np;
        let mean_y = ys.iter().sum::<f64>() / np;
        let mut sxx = 0.0;
        let mut sxy = 0.0;
        for (x, y) in xs.iter().zip(ys.iter()) {
            sxx += (x - mean_x) * (x - mean_x);
            sxy += (x - mean_x) * (y - mean_y);
        }
        let b = if sxx > 0.0 { sxy / sxx } else { 0.0 };
        // Fitted offset at x = 0 (mono = ref_mono).
        let y_at_ref = mean_y - b * mean_x;
        let a_offset_ns = off0 + y_at_ref.round() as i128;

        self.fit = Some(BridgeFit {
            ref_mono_ns: ref_mono,
            a_offset_ns: a_offset_ns as i64,
            b,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build synthetic samples: true offset a0 + drift, plus one-sided (offset-inflating) noise
    /// on all but every third sample. `noise_at(i)` returns the extra ns added to the offset.
    fn synth(
        a0_ns: i64,
        drift_ppm: f64,
        mono_start: u64,
        dt_ns: u64,
        count: usize,
        noise_at: impl Fn(usize) -> i64,
    ) -> Vec<CalibrationSample> {
        let b = drift_ppm * 1e-6;
        (0..count)
            .map(|i| {
                let mono = mono_start + i as u64 * dt_ns;
                let true_off = a0_ns + (b * (mono - mono_start) as f64).round() as i64;
                let meas_off = true_off + noise_at(i);
                let stage = mono as i128 - meas_off as i128;
                CalibrationSample {
                    stage_ns: stage as u64,
                    mono_ns: mono,
                    max_deviation_ns: noise_at(i).max(1) as u64,
                }
            })
            .collect()
    }

    // Clean every 3rd sample, big one-sided spikes otherwise.
    fn one_sided_noise(i: usize) -> i64 {
        if i % 3 == 0 {
            0
        } else {
            20_000 + (i % 7) as i64 * 5_000
        }
    }

    const A0: i64 = 29_714_000_000_000; // ~2.97e13 ns offset, like real hardware
                                        // Uptime must exceed the offset for stage = mono − offset to stay positive (~11 h here).
    const MONO_START: u64 = 40_000_000_000_000;

    #[test]
    fn recovers_offset_and_drift_from_lower_envelope() {
        let mut b = HostClockBridge::new(Duration::from_secs(3));
        for s in synth(A0, 2.0, MONO_START, 10_000_000, 200, one_sided_noise) {
            b.push(s);
        }
        assert!(b.is_ready());
        // Drift recovered near 2 ppm.
        let ppm = b.drift_ppm().unwrap();
        assert!(
            (ppm - 2.0).abs() < 0.3,
            "drift {ppm} ppm not within 0.3 of 2.0"
        );
        // Offset at the epoch recovered near A0 (within 1 us despite 20-50 us spikes).
        let stage_at_start = b.host_to_scanout_ns(MONO_START).unwrap();
        let recovered_offset = MONO_START as i64 - stage_at_start as i64;
        assert!(
            (recovered_offset - A0).abs() < 1_000,
            "offset err {} ns exceeds 1 us",
            recovered_offset - A0
        );
    }

    #[test]
    fn round_trips_host_scanout_host() {
        let mut b = HostClockBridge::new(Duration::from_secs(3));
        for s in synth(A0, 2.0, MONO_START, 10_000_000, 200, one_sided_noise) {
            b.push(s);
        }
        let mono = MONO_START + 1_234_567_890;
        let stage = b.host_to_scanout_ns(mono).unwrap();
        let back = b.scanout_to_host_ns(stage).unwrap();
        assert!(
            (back as i64 - mono as i64).abs() < 10,
            "round trip off by {} ns",
            back as i64 - mono as i64
        );
    }

    #[test]
    fn lower_envelope_beats_mean_under_one_sided_noise() {
        let samples = synth(A0, 2.0, MONO_START, 10_000_000, 200, one_sided_noise);
        let mut b = HostClockBridge::new(Duration::from_secs(3));
        for s in &samples {
            b.push(*s);
        }
        let bridge_off = MONO_START as i64 - b.host_to_scanout_ns(MONO_START).unwrap() as i64;
        // Naive mean offset over all samples — biased high by the one-sided spikes.
        let mean_off: i64 = (samples
            .iter()
            .map(|s| s.mono_ns as i128 - s.stage_ns as i128)
            .sum::<i128>()
            / samples.len() as i128) as i64;
        let bridge_err = (bridge_off - A0).abs();
        let mean_err = (mean_off - A0).abs();
        assert!(
            bridge_err < mean_err,
            "lower-envelope err {bridge_err} should beat mean err {mean_err}"
        );
        assert!(
            mean_err > 5_000,
            "control: mean should be biased >5us, was {mean_err}"
        );
    }

    #[test]
    fn not_ready_with_fewer_than_two_samples() {
        let mut b = HostClockBridge::new(Duration::from_secs(3));
        assert!(!b.is_ready());
        b.push(CalibrationSample {
            stage_ns: 100,
            mono_ns: A0 as u64,
            max_deviation_ns: 1,
        });
        assert!(!b.is_ready());
        assert!(b.host_to_scanout_ns(A0 as u64).is_none());
    }

    #[test]
    fn evicts_samples_beyond_window() {
        let mut b = HostClockBridge::new(Duration::from_secs(1));
        // 300 samples at 10ms = 3s span; only ~last 1s should remain.
        for s in synth(A0, 2.0, MONO_START, 10_000_000, 300, |_| 0) {
            b.push(s);
        }
        assert!(
            b.sample_count() <= 102,
            "window not bounded: {}",
            b.sample_count()
        );
        assert!(
            b.sample_count() >= 98,
            "window too small: {}",
            b.sample_count()
        );
    }
}
