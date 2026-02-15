//! Timing statistics computation from flip records.

use super::flip_info::FlipInfo;

/// Summary statistics computed from flip timing records.
///
/// These statistics answer the question: "Is my system delivering
/// frames reliably enough for my experiment?"
#[derive(Debug, Clone)]
pub struct TimingStats {
    /// Total number of frames
    pub total_frames: u64,

    /// Total duration of the recording (seconds)
    pub total_duration_secs: f64,

    /// Measured refresh rate (Hz)
    pub measured_refresh_rate: f64,

    /// Mean frame duration (microseconds)
    pub mean_frame_duration_us: f64,

    /// Standard deviation of frame duration (microseconds)
    pub std_frame_duration_us: f64,

    /// Minimum frame duration (microseconds)
    pub min_frame_duration_us: f64,

    /// Maximum frame duration (microseconds)
    pub max_frame_duration_us: f64,

    /// Median frame duration (microseconds)
    pub median_frame_duration_us: f64,

    /// Number of missed (dropped) frames
    pub missed_frames: u64,

    /// Percentage of frames that were missed
    pub missed_frame_pct: f64,

    /// Total number of frame slots lost to misses
    pub total_missed_slots: u64,
}

impl TimingStats {
    /// Compute statistics from a slice of FlipInfo records.
    ///
    /// Requires at least 2 records (need at least one inter-frame interval).
    /// Returns None if fewer than 2 records are provided.
    pub fn compute(records: &[FlipInfo]) -> Option<Self> {
        if records.len() < 2 {
            return None;
        }

        // Collect frame durations (skip records without duration, i.e. first frame)
        let durations_us: Vec<f64> = records
            .iter()
            .filter_map(|r| r.frame_duration.map(|d| d.as_micros() as f64))
            .collect();

        if durations_us.is_empty() {
            return None;
        }

        let n = durations_us.len() as f64;

        // Mean
        let sum: f64 = durations_us.iter().sum();
        let mean = sum / n;

        // Population standard deviation
        let variance: f64 = durations_us.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / n;
        let std_dev = variance.sqrt();

        // Min/Max
        let min = durations_us.iter().copied().fold(f64::INFINITY, f64::min);
        let max = durations_us
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);

        // Median
        let mut sorted = durations_us.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = if sorted.len() % 2 == 0 {
            let mid = sorted.len() / 2;
            (sorted[mid - 1] + sorted[mid]) / 2.0
        } else {
            sorted[sorted.len() / 2]
        };

        // Missed frames
        let missed_frames = records.iter().filter(|r| r.missed).count() as u64;
        let missed_frame_pct = missed_frames as f64 / records.len() as f64 * 100.0;
        let total_missed_slots: u64 = records.iter().map(|r| r.missed_count as u64).sum();

        // Total duration
        let first_time = records.first().unwrap().present_complete_time;
        let last_time = records.last().unwrap().present_complete_time;
        let total_duration = last_time.duration_since(first_time);
        let total_duration_secs = total_duration.as_secs_f64();

        // Measured refresh rate
        let measured_refresh_rate = if mean > 0.0 { 1_000_000.0 / mean } else { 0.0 };

        Some(Self {
            total_frames: records.len() as u64,
            total_duration_secs,
            measured_refresh_rate,
            mean_frame_duration_us: mean,
            std_frame_duration_us: std_dev,
            min_frame_duration_us: min,
            max_frame_duration_us: max,
            median_frame_duration_us: median,
            missed_frames,
            missed_frame_pct,
            total_missed_slots,
        })
    }

    /// Pretty-print the statistics to stdout.
    pub fn print_report(&self) {
        println!("=== VSE Timing Report ===");
        println!("Total frames:    {}", self.total_frames);
        println!("Duration:        {:.2} s", self.total_duration_secs);
        println!("Refresh rate:    {:.2} Hz", self.measured_refresh_rate);
        println!(
            "Frame duration:  {:.0} +/- {:.0} us (min: {:.0}, max: {:.0}, median: {:.0})",
            self.mean_frame_duration_us,
            self.std_frame_duration_us,
            self.min_frame_duration_us,
            self.max_frame_duration_us,
            self.median_frame_duration_us,
        );
        println!(
            "Missed frames:   {} / {} ({:.2}%)",
            self.missed_frames, self.total_frames, self.missed_frame_pct,
        );
        println!("=========================");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timing::clock::Timestamp;
    use std::time::Duration;

    fn make_records(durations_us: &[u64]) -> Vec<FlipInfo> {
        let expected = Duration::from_micros(16_667);
        let mut records = Vec::new();
        let mut time_us: u64 = 0;

        for (i, &dur) in durations_us.iter().enumerate() {
            let submit = Timestamp::from_micros(time_us);
            let present = Timestamp::from_micros(time_us + dur);

            let frame_duration = if i == 0 {
                None
            } else {
                Some(Duration::from_micros(dur))
            };

            let ratio = dur as f64 / expected.as_micros() as f64;
            let missed = i > 0 && ratio > 1.5;
            let missed_count = if missed {
                (ratio.round() as u32).saturating_sub(1)
            } else {
                0
            };

            records.push(FlipInfo {
                frame_number: i as u64,
                submit_time: submit,
                present_complete_time: present,
                frame_duration,
                expected_frame_duration: expected,
                missed,
                missed_count,
                skipped: false,
            });

            time_us += dur;
        }

        records
    }

    #[test]
    fn test_stats_insufficient_data() {
        assert!(TimingStats::compute(&[]).is_none());
        let records = make_records(&[16_667]);
        assert!(TimingStats::compute(&records).is_none());
    }

    #[test]
    fn test_stats_uniform_frames() {
        let records = make_records(&[16_667; 10]);
        let stats = TimingStats::compute(&records).unwrap();

        assert_eq!(stats.total_frames, 10);
        assert!((stats.mean_frame_duration_us - 16_667.0).abs() < 1.0);
        assert!(stats.std_frame_duration_us < 1.0); // essentially zero
        assert_eq!(stats.missed_frames, 0);
        assert!((stats.measured_refresh_rate - 60.0).abs() < 0.1);
    }

    #[test]
    fn test_stats_with_jitter() {
        // 21 durations: alternating 16_500 and 16_834
        // First frame has no duration, so 20 measured durations (10 each) -> mean = 16_667
        let durations: Vec<u64> = (0..21)
            .map(|i| if i % 2 == 0 { 16_500 } else { 16_834 })
            .collect();
        let records = make_records(&durations);
        let stats = TimingStats::compute(&records).unwrap();

        // Mean should be close to 16_667
        assert!((stats.mean_frame_duration_us - 16_667.0).abs() < 1.0);
        // Std should be exactly 167
        assert!((stats.std_frame_duration_us - 167.0).abs() < 1.0);
        assert_eq!(stats.missed_frames, 0);
    }

    #[test]
    fn test_stats_with_missed_frame() {
        // Normal frames with one doubled frame in the middle
        let mut durations = vec![16_667u64; 10];
        durations[5] = 33_334; // ~2x expected -> missed
        let records = make_records(&durations);
        let stats = TimingStats::compute(&records).unwrap();

        assert_eq!(stats.missed_frames, 1);
        assert_eq!(stats.total_missed_slots, 1);
    }

    #[test]
    fn test_stats_report_output() {
        let records = make_records(&[16_667; 5]);
        let stats = TimingStats::compute(&records).unwrap();
        // Just verify it doesn't panic
        stats.print_report();
    }
}
