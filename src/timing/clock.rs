//! High-resolution monotonic clock for timing measurements.

use std::time::{Duration, Instant};

/// High-resolution monotonic clock for timing measurements.
///
/// All timestamps in VSE are relative to the clock's creation time
/// (typically when VSEContext is built). This avoids the ambiguity of
/// wall-clock times and ensures monotonicity.
///
/// # Cross-clock calibration
///
/// For `VK_EXT_present_timing` we must both *emit* scheduled target times and *interpret*
/// hardware scanout timestamps in the driver's chosen time domain. When that domain is
/// `VK_TIME_DOMAIN_CLOCK_MONOTONIC_KHR` (the common case on Linux), the clock captures an
/// absolute `CLOCK_MONOTONIC` reading at its epoch so VSE `Timestamp`s convert directly to
/// and from absolute monotonic nanoseconds — putting `submit_time`, hardware `present_time`,
/// scheduled targets, and any `CLOCK_MONOTONIC`-based external recording hardware in one
/// comparable domain. See [`Clock::to_monotonic_nanos`] / [`Clock::from_monotonic_nanos`].
pub struct Clock {
    epoch: Instant,
    /// Absolute `CLOCK_MONOTONIC` nanoseconds at `epoch`, if readable on this platform.
    epoch_mono_ns: Option<u64>,
}

impl Clock {
    /// Create a new clock. The current instant becomes time zero.
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
            epoch_mono_ns: read_monotonic_nanos(),
        }
    }

    /// Get the current timestamp (microseconds since epoch).
    pub fn now(&self) -> Timestamp {
        let elapsed = self.epoch.elapsed();
        Timestamp(elapsed.as_micros() as u64)
    }

    /// Get the underlying epoch for interop with std::time.
    pub fn epoch(&self) -> Instant {
        self.epoch
    }

    /// Whether this clock could be anchored to `CLOCK_MONOTONIC` (Linux).
    ///
    /// When `false`, the `CLOCK_MONOTONIC` time domain cannot be used for present-timing
    /// scheduling/readback and the caller must fall back to another domain.
    pub fn has_monotonic_anchor(&self) -> bool {
        self.epoch_mono_ns.is_some()
    }

    /// Convert a VSE [`Timestamp`] to an absolute `CLOCK_MONOTONIC` value in nanoseconds.
    ///
    /// Used to emit `VkPresentTimingInfoEXT::targetTime` for a scheduled present.
    /// Returns `None` if the clock has no monotonic anchor (non-Linux).
    pub fn to_monotonic_nanos(&self, ts: Timestamp) -> Option<u64> {
        self.epoch_mono_ns
            .map(|e| e.saturating_add(ts.as_micros().saturating_mul(1_000)))
    }

    /// Convert an absolute `CLOCK_MONOTONIC` nanosecond value (e.g. a hardware scanout
    /// timestamp reported by the driver) into a VSE [`Timestamp`].
    ///
    /// Returns `None` if the clock has no monotonic anchor (non-Linux). Saturates to zero
    /// for times at or before the clock epoch.
    pub fn from_monotonic_nanos(&self, mono_ns: u64) -> Option<Timestamp> {
        self.epoch_mono_ns
            .map(|e| Timestamp::from_micros(mono_ns.saturating_sub(e) / 1_000))
    }
}

/// Read the current `CLOCK_MONOTONIC` value in nanoseconds, if available on this platform.
///
/// Rust's `Instant` uses `CLOCK_MONOTONIC` on Linux but does not expose the raw value, so we
/// read it directly. Non-Linux platforms return `None` (the monotonic domain is not used
/// there; Windows would use QPC via calibrated timestamps instead).
#[cfg(target_os = "linux")]
fn read_monotonic_nanos() -> Option<u64> {
    // SAFETY: `ts` is a valid, properly-aligned `timespec` we hand to `clock_gettime`.
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    if rc == 0 {
        Some((ts.tv_sec as u64).saturating_mul(1_000_000_000) + ts.tv_nsec as u64)
    } else {
        None
    }
}

#[cfg(not(target_os = "linux"))]
fn read_monotonic_nanos() -> Option<u64> {
    None
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

/// A timestamp relative to the clock's epoch, in microseconds.
///
/// Stored as u64 microseconds. At 1 MHz resolution this gives
/// ~584,942 years before overflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct Timestamp(u64);

impl Timestamp {
    /// Create a timestamp from raw microseconds.
    pub fn from_micros(us: u64) -> Self {
        Self(us)
    }

    /// Get raw microseconds value.
    pub fn as_micros(&self) -> u64 {
        self.0
    }

    /// Convert to seconds (f64).
    pub fn as_secs_f64(&self) -> f64 {
        self.0 as f64 / 1_000_000.0
    }

    /// Convert to milliseconds (f64).
    pub fn as_millis_f64(&self) -> f64 {
        self.0 as f64 / 1_000.0
    }

    /// Duration between two timestamps.
    pub fn duration_since(&self, earlier: Timestamp) -> Duration {
        Duration::from_micros(self.0.saturating_sub(earlier.0))
    }
}

/// A timestamp in the **scanout clock** — VSE's primary experimental clock — in nanoseconds
/// since the session's scanout epoch (see [`ScanoutClock`]).
///
/// This is a distinct type from [`Timestamp`] (the host `CLOCK_MONOTONIC`-anchored clock) so the
/// two domains cannot be accidentally compared or subtracted. Display timing lives entirely in
/// this domain; converting to/from the host clock is the opt-in job of the host-clock bridge.
/// Stored in nanoseconds because the scanout clock is nanosecond-native and the drift correction
/// needs sub-microsecond headroom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct ScanoutTimestamp(u64);

impl ScanoutTimestamp {
    /// Create a scanout timestamp from nanoseconds since the scanout epoch.
    pub fn from_nanos(ns: u64) -> Self {
        Self(ns)
    }

    /// Nanoseconds since the scanout epoch.
    pub fn as_nanos(&self) -> u64 {
        self.0
    }

    /// Whole microseconds since the scanout epoch.
    pub fn as_micros(&self) -> u64 {
        self.0 / 1_000
    }

    /// Seconds since the scanout epoch (f64).
    pub fn as_secs_f64(&self) -> f64 {
        self.0 as f64 / 1_000_000_000.0
    }

    /// Duration between two scanout timestamps (saturating).
    pub fn duration_since(&self, earlier: ScanoutTimestamp) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }
}

/// The scanout clock's session epoch: an absolute present-stage-local nanosecond reading captured
/// at session start, against which all scanout timestamps are referenced (`t=0`).
///
/// Absolute present-stage-local values are large (~10¹³ ns) and driver-epoch-relative; rebasing to
/// a session zero keeps [`ScanoutTimestamp`] values small and meaningful ("time since experiment
/// start"), mirroring how [`Clock`] anchors the host clock.
#[derive(Debug, Clone, Copy)]
pub struct ScanoutClock {
    epoch_stage_ns: u64,
}

impl ScanoutClock {
    /// Anchor the scanout clock at an absolute present-stage-local nanosecond reading.
    pub fn new(epoch_stage_ns: u64) -> Self {
        Self { epoch_stage_ns }
    }

    /// The absolute present-stage-local nanosecond value this clock is anchored to.
    pub fn epoch_stage_ns(&self) -> u64 {
        self.epoch_stage_ns
    }

    /// Rebase an absolute present-stage-local nanosecond reading to a [`ScanoutTimestamp`]
    /// (time since the scanout epoch). Saturates to zero for readings at or before the epoch.
    pub fn rebase(&self, stage_ns: u64) -> ScanoutTimestamp {
        ScanoutTimestamp(stage_ns.saturating_sub(self.epoch_stage_ns))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clock_monotonicity() {
        let clock = Clock::new();
        let t1 = clock.now();
        // Small busy-wait to ensure time passes
        std::thread::sleep(Duration::from_micros(100));
        let t2 = clock.now();
        assert!(t2 > t1, "timestamps must be monotonically increasing");
    }

    #[test]
    fn test_timestamp_conversions() {
        let ts = Timestamp::from_micros(1_500_000); // 1.5 seconds
        assert_eq!(ts.as_micros(), 1_500_000);
        assert!((ts.as_secs_f64() - 1.5).abs() < 1e-9);
        assert!((ts.as_millis_f64() - 1500.0).abs() < 1e-6);
    }

    #[test]
    fn test_timestamp_duration() {
        let t1 = Timestamp::from_micros(1_000);
        let t2 = Timestamp::from_micros(5_000);
        let dur = t2.duration_since(t1);
        assert_eq!(dur, Duration::from_micros(4_000));
    }

    #[test]
    fn test_timestamp_zero() {
        let ts = Timestamp::from_micros(0);
        assert_eq!(ts.as_micros(), 0);
        assert_eq!(ts.as_secs_f64(), 0.0);
    }

    #[test]
    fn test_timestamp_ordering() {
        let t1 = Timestamp::from_micros(100);
        let t2 = Timestamp::from_micros(200);
        let t3 = Timestamp::from_micros(100);
        assert!(t1 < t2);
        assert!(t2 > t1);
        assert_eq!(t1, t3);
    }

    #[test]
    fn test_duration_since_saturates() {
        let t1 = Timestamp::from_micros(500);
        let t2 = Timestamp::from_micros(100);
        // earlier > self should saturate to zero, not panic
        let dur = t2.duration_since(t1);
        assert_eq!(dur, Duration::from_micros(0));
    }

    #[test]
    fn test_scanout_timestamp_conversions() {
        let ts = ScanoutTimestamp::from_nanos(1_500_000_000); // 1.5 s
        assert_eq!(ts.as_nanos(), 1_500_000_000);
        assert_eq!(ts.as_micros(), 1_500_000);
        assert!((ts.as_secs_f64() - 1.5).abs() < 1e-9);
    }

    #[test]
    fn test_scanout_timestamp_ordering_and_duration() {
        let a = ScanoutTimestamp::from_nanos(1_000);
        let b = ScanoutTimestamp::from_nanos(5_000);
        assert!(a < b);
        assert_eq!(b.duration_since(a), Duration::from_nanos(4_000));
        // saturating: earlier > self must not panic
        assert_eq!(a.duration_since(b), Duration::from_nanos(0));
    }

    #[test]
    fn test_scanout_clock_rebases_to_epoch() {
        // Scanout clock anchored at an absolute present-stage-local ns value.
        let clock = ScanoutClock::new(29_714_000_000_000);
        // A later absolute stage reading rebases to time-since-epoch.
        let ts = clock.rebase(29_714_000_016_667);
        assert_eq!(ts.as_nanos(), 16_667);
        // Epoch itself is zero.
        assert_eq!(clock.rebase(29_714_000_000_000).as_nanos(), 0);
        // Before the epoch saturates to zero rather than underflowing.
        assert_eq!(clock.rebase(29_714_000_000_000 - 100).as_nanos(), 0);
    }
}
