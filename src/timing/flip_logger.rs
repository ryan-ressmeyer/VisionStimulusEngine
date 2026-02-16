//! FlipLogger - records timing information for every frame flip.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::flip_info::FlipInfo;

/// Records timing information for every frame flip.
///
/// The FlipLogger stores all FlipInfo records in memory during the
/// session and can export them to CSV for post-hoc analysis.
///
/// # Memory Usage
///
/// Each FlipInfo record is ~64 bytes. At 60 Hz:
/// - 1 minute  = 3,600 records  = ~225 KB
/// - 1 hour    = 216,000 records = ~13 MB
pub struct FlipLogger {
    records: Vec<FlipInfo>,
    csv_path: Option<PathBuf>,
}

impl FlipLogger {
    /// Create a new flip logger.
    ///
    /// # Arguments
    /// * `capacity` - Pre-allocate storage for this many frames.
    pub fn new(capacity: usize) -> Self {
        Self {
            records: Vec::with_capacity(capacity),
            csv_path: None,
        }
    }

    /// Create a logger that will write CSV on close/drop.
    pub fn with_csv(path: impl Into<PathBuf>, capacity: usize) -> Self {
        Self {
            records: Vec::with_capacity(capacity),
            csv_path: Some(path.into()),
        }
    }

    /// Record a flip. Skipped frames are not recorded.
    pub fn record(&mut self, info: FlipInfo) {
        if !info.skipped {
            self.records.push(info);
        }
    }

    /// Get all records.
    pub fn records(&self) -> &[FlipInfo] {
        &self.records
    }

    /// Get the most recent record.
    pub fn last(&self) -> Option<&FlipInfo> {
        self.records.last()
    }

    /// Total number of recorded flips.
    pub fn frame_count(&self) -> u64 {
        self.records.len() as u64
    }

    /// Number of missed frames.
    pub fn missed_frame_count(&self) -> u64 {
        self.records.iter().filter(|r| r.missed).count() as u64
    }

    /// Export all records to CSV.
    ///
    /// CSV columns:
    /// frame_number, timing_source, submit_time_us, present_time_us, missed, missed_count
    pub fn export_csv(&self, path: impl AsRef<Path>) -> Result<(), std::io::Error> {
        let mut file = std::fs::File::create(path)?;

        // Header
        writeln!(
            file,
            "frame_number,timing_source,submit_time_us,present_time_us,missed,missed_count"
        )?;

        // Data rows
        for record in &self.records {
            writeln!(
                file,
                "{},{},{},{},{},{}",
                record.frame_number,
                record.timing_source,
                record.submit_time.as_micros(),
                record.present_time.as_micros(),
                record.missed,
                record.missed_count,
            )?;
        }

        Ok(())
    }

    /// Flush to CSV if a path was configured, then clear records.
    pub fn flush(&mut self) -> Result<(), std::io::Error> {
        if let Some(path) = &self.csv_path {
            self.export_csv(path.clone())?;
        }
        self.records.clear();
        Ok(())
    }
}

impl Drop for FlipLogger {
    fn drop(&mut self) {
        if let Some(path) = &self.csv_path {
            if let Err(e) = self.export_csv(path) {
                eprintln!("Warning: failed to write flip log CSV: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timing::clock::Timestamp;
    use crate::timing::timing_source::TimingSource;

    fn make_info(frame_number: u64, missed: bool) -> FlipInfo {
        FlipInfo {
            frame_number,
            timing_source: TimingSource::CpuEstimate,
            submit_time: Timestamp::from_micros(frame_number * 16_000),
            present_time: Timestamp::from_micros(frame_number * 16_000 + 16_667),
            missed,
            missed_count: if missed { 1 } else { 0 },
            skipped: false,
        }
    }

    #[test]
    fn test_logger_empty() {
        let logger = FlipLogger::new(100);
        assert_eq!(logger.frame_count(), 0);
        assert_eq!(logger.missed_frame_count(), 0);
        assert!(logger.last().is_none());
        assert!(logger.records().is_empty());
    }

    #[test]
    fn test_logger_record_and_retrieve() {
        let mut logger = FlipLogger::new(100);
        logger.record(make_info(0, false));
        logger.record(make_info(1, false));
        logger.record(make_info(2, false));

        assert_eq!(logger.frame_count(), 3);
        assert_eq!(logger.records().len(), 3);
        assert_eq!(logger.last().unwrap().frame_number, 2);
    }

    #[test]
    fn test_logger_skipped_not_recorded() {
        let mut logger = FlipLogger::new(100);
        logger.record(make_info(0, false));
        logger.record(FlipInfo::skipped(1));
        logger.record(make_info(2, false));

        assert_eq!(logger.frame_count(), 2);
    }

    #[test]
    fn test_logger_missed_count() {
        let mut logger = FlipLogger::new(100);
        logger.record(make_info(0, false));
        logger.record(make_info(1, false));
        logger.record(make_info(2, true));
        logger.record(make_info(3, false));

        assert_eq!(logger.missed_frame_count(), 1);
    }

    #[test]
    fn test_csv_export() {
        let mut logger = FlipLogger::new(100);
        logger.record(make_info(0, false));
        logger.record(make_info(1, false));
        logger.record(make_info(2, true));

        let dir = std::env::temp_dir().join("vse_test_csv_export.csv");
        logger.export_csv(&dir).unwrap();

        let contents = std::fs::read_to_string(&dir).unwrap();
        std::fs::remove_file(&dir).ok();

        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 4); // header + 3 data rows

        // Check header
        assert_eq!(
            lines[0],
            "frame_number,timing_source,submit_time_us,present_time_us,missed,missed_count"
        );

        // First data row
        assert!(lines[1].starts_with("0,CpuEstimate,"));
        assert!(lines[1].ends_with(",false,0"));

        // Third data row: missed = true
        assert!(lines[3].contains("true,1"));
    }

    #[test]
    fn test_csv_column_headers() {
        let logger = FlipLogger::new(0);
        let dir = std::env::temp_dir().join("vse_test_csv_headers.csv");
        logger.export_csv(&dir).unwrap();

        let contents = std::fs::read_to_string(&dir).unwrap();
        std::fs::remove_file(&dir).ok();

        let header = contents.lines().next().unwrap();
        assert!(header.contains("frame_number"));
        assert!(header.contains("timing_source"));
        assert!(header.contains("submit_time_us"));
        assert!(header.contains("present_time_us"));
        assert!(header.contains("missed"));
        assert!(header.contains("missed_count"));
    }

    #[test]
    fn test_logger_flush() {
        let dir = std::env::temp_dir().join("vse_test_flush.csv");
        let mut logger = FlipLogger::with_csv(dir.clone(), 100);
        logger.record(make_info(0, false));
        logger.record(make_info(1, false));

        logger.flush().unwrap();

        // Records should be cleared
        assert_eq!(logger.frame_count(), 0);

        // CSV should exist with data
        let contents = std::fs::read_to_string(&dir).unwrap();
        std::fs::remove_file(&dir).ok();
        assert!(contents.lines().count() >= 3); // header + 2 rows
    }
}
