//! CSV backend for experiment data recording.
//!
//! Produces two files in the configured output directory:
//! - `frames.csv`  — one row per flip; timing columns always present
//! - `events.csv`  — annotations and raw events
//!
//! Frame schema is inferred from the first `record_frame` payload.
//! Timing-only rows arriving before the first payload are buffered in
//! memory and flushed once the schema is known.

use std::io::Write;
use std::path::PathBuf;

use crate::data::messages::{AnnotationMessage, EventMessage, FrameMessage};
use crate::data::writer::{DataError, DataWriter};
use crate::timing::FlipInfo;

/// CSV data recording backend.
///
/// Output directory is created if it does not exist.
///
/// # Example
///
/// ```no_run
/// use vision_stimulus_engine::prelude::*;
///
/// let session = ExperimentSession::builder()
///     .with_writer(CsvDataWriter::new("data/session_001/"))
///     .build()
///     .unwrap();
/// ```
pub struct CsvDataWriter {
    output_dir: PathBuf,
    /// Pending timing-only rows buffered before schema is known.
    pending_timing: Vec<FlipInfo>,
    /// CSV column names derived from the first user payload (excluding timing cols).
    user_columns: Option<Vec<String>>,
    /// Writer for frames.csv, opened lazily on first flush/write.
    frames_file: Option<std::fs::File>,
    /// Writer for events.csv, opened lazily on first event.
    events_file: Option<std::fs::File>,
    events_header_written: bool,
}

/// Fixed timing column names written to every row of frames.csv.
const TIMING_COLUMNS: &[&str] = &[
    "frame_number",
    "present_time_us",
    "submit_time_us",
    "timing_source",
    "present_id",
    "target_time_us",
    "on_target",
    "missed",
    "missed_count",
    "skipped",
];

impl CsvDataWriter {
    /// Create a new CSV writer. The output directory is created on first write.
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
            pending_timing: Vec::new(),
            user_columns: None,
            frames_file: None,
            events_file: None,
            events_header_written: false,
        }
    }

    fn ensure_output_dir(&self) -> Result<(), DataError> {
        std::fs::create_dir_all(&self.output_dir)?;
        Ok(())
    }

    fn frames_file(&mut self) -> Result<&mut std::fs::File, DataError> {
        if self.frames_file.is_none() {
            self.ensure_output_dir()?;
            let path = self.output_dir.join("frames.csv");
            self.frames_file = Some(std::fs::File::create(path)?);
        }
        Ok(self.frames_file.as_mut().unwrap())
    }

    fn events_file(&mut self) -> Result<&mut std::fs::File, DataError> {
        if self.events_file.is_none() {
            self.ensure_output_dir()?;
            let path = self.output_dir.join("events.csv");
            self.events_file = Some(std::fs::File::create(path)?);
        }
        Ok(self.events_file.as_mut().unwrap())
    }

    /// Write the frames.csv header once the user column schema is known.
    fn write_frames_header(&mut self, user_cols: &[String]) -> Result<(), DataError> {
        let file = self.frames_file()?;
        let header = TIMING_COLUMNS
            .iter()
            .map(|s| *s)
            .chain(user_cols.iter().map(|s| s.as_str()))
            .collect::<Vec<_>>()
            .join(",");
        writeln!(file, "{}", header)?;
        Ok(())
    }

    /// Serialize a FlipInfo into timing CSV columns (without skipped).
    fn flip_to_csv(flip: &FlipInfo) -> String {
        // Empty cell for an unscheduled (immediate) present.
        let target = flip
            .target_time
            .map(|t| t.as_micros().to_string())
            .unwrap_or_default();
        format!(
            "{},{},{},{},{},{},{},{},{}",
            flip.frame_number,
            flip.present_time.as_micros(),
            flip.submit_time.as_micros(),
            flip.timing_source,
            flip.present_id,
            target,
            flip.on_target,
            flip.missed,
            flip.missed_count,
        )
    }

    /// Write a timing-only row (empty user columns).
    fn write_timing_only_row(
        &mut self,
        flip: &FlipInfo,
        n_user_cols: usize,
    ) -> Result<(), DataError> {
        let file = self.frames_file()?;
        let timing = Self::flip_to_csv(flip);
        let empties = ",".repeat(n_user_cols);
        writeln!(file, "{},{}{}", timing, flip.skipped, empties)?;
        Ok(())
    }

    /// Flush all buffered timing-only rows now that we know the schema.
    fn flush_pending_timing(&mut self) -> Result<(), DataError> {
        let pending = std::mem::take(&mut self.pending_timing);
        let n_user_cols = self.user_columns.as_ref().map(|c| c.len()).unwrap_or(0);
        for flip in &pending {
            self.write_timing_only_row(flip, n_user_cols)?;
        }
        Ok(())
    }

    fn ensure_events_header(&mut self) -> Result<(), DataError> {
        if !self.events_header_written {
            let file = self.events_file()?;
            writeln!(file, "timestamp_us,stream,payload")?;
            self.events_header_written = true;
        }
        Ok(())
    }
}

impl DataWriter for CsvDataWriter {
    fn write_frame(&mut self, msg: FrameMessage) -> Result<(), DataError> {
        match msg.payload {
            None => {
                if self.user_columns.is_none() {
                    // Schema not known yet — buffer
                    self.pending_timing.push(msg.flip);
                } else {
                    let n = self.user_columns.as_ref().unwrap().len();
                    self.write_timing_only_row(&msg.flip, n)?;
                }
            }
            Some(ref payload_bytes) => {
                // Parse user data to learn column names on first call
                let value: serde_json::Value = serde_json::from_slice(payload_bytes)
                    .map_err(|e| DataError::Serialization(e.to_string()))?;
                let obj = value.as_object().ok_or_else(|| {
                    DataError::Serialization("frame data must serialize to a JSON object".into())
                })?;

                if self.user_columns.is_none() {
                    let cols: Vec<String> = obj.keys().cloned().collect();
                    self.write_frames_header(&cols)?;
                    self.user_columns = Some(cols);
                    self.flush_pending_timing()?;
                }

                let user_cols: Vec<String> = self.user_columns.as_ref().unwrap().clone();
                let timing = Self::flip_to_csv(&msg.flip);
                let file = self.frames_file()?;
                let user_vals: Vec<String> = user_cols
                    .iter()
                    .map(|col| {
                        obj.get(col)
                            .map(|v| match v {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .unwrap_or_default()
                    })
                    .collect();
                writeln!(
                    file,
                    "{},{},{}",
                    timing,
                    msg.flip.skipped,
                    user_vals.join(",")
                )?;
            }
        }
        Ok(())
    }

    fn write_annotation(&mut self, msg: AnnotationMessage) -> Result<(), DataError> {
        self.ensure_events_header()?;
        let payload_str = String::from_utf8_lossy(&msg.payload);
        let file = self.events_file()?;
        writeln!(
            file,
            "{},{},{}",
            msg.timestamp.as_micros(),
            msg.stream,
            payload_str
        )?;
        Ok(())
    }

    fn write_event(&mut self, msg: EventMessage) -> Result<(), DataError> {
        self.ensure_events_header()?;
        let file = self.events_file()?;
        writeln!(
            file,
            "{},{},{}",
            msg.timestamp.as_micros(),
            msg.name,
            msg.value
        )?;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DataError> {
        // If no user data ever arrived, write header with timing-only cols and flush pending
        if self.user_columns.is_none() && !self.pending_timing.is_empty() {
            self.write_frames_header(&[])?;
            self.user_columns = Some(vec![]);
            self.flush_pending_timing()?;
        }
        if let Some(f) = &mut self.frames_file {
            f.flush()?;
        }
        if let Some(f) = &mut self.events_file {
            f.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::messages::*;
    use crate::data::writer::DataWriter;
    use crate::timing::{FlipInfo, Timestamp, TimingSource};

    fn make_flip(frame: u64) -> FlipInfo {
        FlipInfo {
            frame_number: frame,
            timing_source: TimingSource::CpuEstimate,
            submit_time: Timestamp::from_micros(frame * 16_667),
            present_time: Timestamp::from_micros(frame * 16_667 + 500),
            present_id: 0,
            target_time: None,
            on_target: true,
            missed: false,
            missed_count: 0,
            skipped: false,
        }
    }

    fn make_payload(contrast: f32, orientation: u32) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "contrast": contrast,
            "orientation": orientation
        }))
        .unwrap()
    }

    #[test]
    fn test_csv_timing_only_rows() {
        let dir = std::env::temp_dir().join("vse_csv_test_timing_only");
        let _ = std::fs::remove_dir_all(&dir);
        let mut w = CsvDataWriter::new(&dir);

        // Write three timing-only frames then flush
        for i in 0..3u64 {
            w.write_frame(FrameMessage {
                flip: make_flip(i),
                payload: None,
                schema_name: "",
            })
            .unwrap();
        }
        w.flush().unwrap();

        let frames = std::fs::read_to_string(dir.join("frames.csv")).unwrap();
        let lines: Vec<&str> = frames.lines().collect();
        // header + 3 rows
        assert_eq!(lines.len(), 4);
        assert!(lines[0].contains("frame_number"));
        assert!(lines[0].contains("present_time_us"));
        assert!(lines[1].starts_with("0,"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_csv_user_data_merged() {
        let dir = std::env::temp_dir().join("vse_csv_test_user_data");
        let _ = std::fs::remove_dir_all(&dir);
        let mut w = CsvDataWriter::new(&dir);

        // One timing-only then one with user data
        w.write_frame(FrameMessage {
            flip: make_flip(0),
            payload: None,
            schema_name: "",
        })
        .unwrap();
        w.write_frame(FrameMessage {
            flip: make_flip(1),
            payload: Some(make_payload(0.8, 45)),
            schema_name: "MyFrameData",
        })
        .unwrap();
        w.flush().unwrap();

        let frames = std::fs::read_to_string(dir.join("frames.csv")).unwrap();
        let lines: Vec<&str> = frames.lines().collect();
        // header + 2 rows
        assert_eq!(lines.len(), 3);
        // Header has user columns
        assert!(lines[0].contains("contrast"));
        assert!(lines[0].contains("orientation"));
        // First row (timing-only) has empty user columns
        assert!(
            lines[1].ends_with(",,") || lines[1].split(',').count() == lines[0].split(',').count()
        );
        // Second row has user values
        assert!(lines[2].contains("0.8") || lines[2].contains("45"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_csv_events_and_annotations() {
        let dir = std::env::temp_dir().join("vse_csv_test_events");
        let _ = std::fs::remove_dir_all(&dir);
        let mut w = CsvDataWriter::new(&dir);

        w.write_annotation(AnnotationMessage {
            stream: "trial".to_string(),
            timestamp: Timestamp::from_micros(1_000),
            payload: serde_json::to_vec(&serde_json::json!({"trial_id": 1})).unwrap(),
        })
        .unwrap();
        w.write_event(EventMessage {
            name: "response".to_string(),
            timestamp: Timestamp::from_micros(2_000),
            value: "left".to_string(),
        })
        .unwrap();
        w.flush().unwrap();

        let events = std::fs::read_to_string(dir.join("events.csv")).unwrap();
        let lines: Vec<&str> = events.lines().collect();
        assert_eq!(lines.len(), 3); // header + 2
        assert!(lines[0].contains("timestamp_us"));
        assert!(lines[0].contains("stream"));
        assert!(lines[0].contains("payload"));
        assert!(lines[1].contains("trial"));
        assert!(lines[2].contains("response"));
        assert!(lines[2].contains("left"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
