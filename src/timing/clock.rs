//! High-resolution monotonic clock for timing measurements.

use std::time::{Duration, Instant};

/// High-resolution monotonic clock for timing measurements.
///
/// All timestamps in VSE are relative to the clock's creation time
/// (typically when VSEContext is built). This avoids the ambiguity of
/// wall-clock times and ensures monotonicity.
pub struct Clock {
    epoch: Instant,
}

impl Clock {
    /// Create a new clock. The current instant becomes time zero.
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
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
}
