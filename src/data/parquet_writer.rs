//! Parquet backend for experiment data recording.
//!
//! Buffers all rows in memory and writes Parquet files on `flush()`.
//! The configured path is used for frame rows. Annotation and event rows are
//! written to sibling files derived from that path: `name.annotations.parquet`
//! and `name.events.parquet`. Empty streams are not written.
//!
//! Uses Arrow for the in-memory representation and type-specific frame payload
//! column arrays inferred from the first non-null user payload.
//!
//! Read in Python with:
//! ```python
//! import pandas as pd
//! frames = pd.read_parquet("session.parquet")
//! annotations = pd.read_parquet("session.annotations.parquet")
//! events = pd.read_parquet("session.events.parquet")
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use arrow::array::*;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;

use crate::data::messages::{AnnotationMessage, EventMessage, FrameMessage};
use crate::data::writer::{DataError, DataWriter};
use crate::timing::FlipInfo;

/// Parquet data recording backend.
///
/// All rows are buffered in memory until `flush()` is called (or the session
/// is dropped). For long experiments, call `vse.flush_data()` periodically.
///
/// The configured path stores frame rows. Annotation and event rows, when
/// present, are stored in sibling files named `*.annotations.parquet` and
/// `*.events.parquet`.
///
/// # Example
///
/// ```no_run
/// use vision_stimulus_engine::prelude::*;
///
/// let session = ExperimentSession::builder()
///     .with_writer(ParquetDataWriter::new("data/session_001.parquet"))
///     .build()
///     .unwrap();
/// ```
pub struct ParquetDataWriter {
    path: PathBuf,
    /// Buffered (flip, Option<user_json_value>) pairs.
    frame_rows: Vec<(FlipInfo, Option<serde_json::Value>)>,
    /// Buffered annotation rows: (stream, timestamp_us, payload_json).
    annotation_rows: Vec<(String, u64, serde_json::Value)>,
    /// Buffered event rows: (name, timestamp_us, value).
    event_rows: Vec<(String, u64, String)>,
}

impl ParquetDataWriter {
    /// Create a new Parquet writer. The file is created on `flush()`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            frame_rows: Vec::new(),
            annotation_rows: Vec::new(),
            event_rows: Vec::new(),
        }
    }

    fn stream_path(&self, stream: &str) -> PathBuf {
        let stem = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy())
            .unwrap_or_else(|| "frames".into());
        self.path.with_file_name(format!("{stem}.{stream}.parquet"))
    }

    fn write_batch(
        path: &std::path::Path,
        schema: Arc<Schema>,
        batch: &RecordBatch,
    ) -> Result<(), DataError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let file = std::fs::File::create(path)?;
        let props = WriterProperties::builder().build();
        let mut writer = ArrowWriter::try_new(file, schema, Some(props))
            .map_err(|e| DataError::Backend(e.to_string()))?;
        writer
            .write(batch)
            .map_err(|e| DataError::Backend(e.to_string()))?;
        writer
            .close()
            .map_err(|e| DataError::Backend(e.to_string()))?;
        Ok(())
    }

    fn write_frames_parquet(&self) -> Result<(), DataError> {
        if self.frame_rows.is_empty() {
            return Ok(());
        }

        // Build Arrow arrays for timing columns
        let frame_nums: Vec<u64> = self
            .frame_rows
            .iter()
            .map(|(f, _)| f.frame_number)
            .collect();
        let present_times: Vec<u64> = self
            .frame_rows
            .iter()
            .map(|(f, _)| f.present_time.as_micros())
            .collect();
        let submit_times: Vec<u64> = self
            .frame_rows
            .iter()
            .map(|(f, _)| f.submit_time.as_micros())
            .collect();
        let timing_sources: Vec<String> = self
            .frame_rows
            .iter()
            .map(|(f, _)| f.timing_source.to_string())
            .collect();
        let present_ids: Vec<u64> = self.frame_rows.iter().map(|(f, _)| f.present_id).collect();
        let target_times: Vec<Option<u64>> = self
            .frame_rows
            .iter()
            .map(|(f, _)| f.target_time.map(|t| t.as_micros()))
            .collect();
        let on_targets: Vec<bool> = self.frame_rows.iter().map(|(f, _)| f.on_target).collect();
        let missed: Vec<bool> = self.frame_rows.iter().map(|(f, _)| f.missed).collect();
        let missed_counts: Vec<u32> = self
            .frame_rows
            .iter()
            .map(|(f, _)| f.missed_count)
            .collect();

        let mut fields = vec![
            Field::new("frame_number", DataType::UInt64, false),
            Field::new("present_time_us", DataType::UInt64, false),
            Field::new("submit_time_us", DataType::UInt64, false),
            Field::new("timing_source", DataType::Utf8, false),
            Field::new("present_id", DataType::UInt64, false),
            // Nullable: absent for immediate (unscheduled) presents.
            Field::new("target_time_us", DataType::UInt64, true),
            Field::new("on_target", DataType::Boolean, false),
            Field::new("missed", DataType::Boolean, false),
            Field::new("missed_count", DataType::UInt32, false),
        ];

        let mut columns: Vec<Arc<dyn Array>> = vec![
            Arc::new(UInt64Array::from(frame_nums)),
            Arc::new(UInt64Array::from(present_times)),
            Arc::new(UInt64Array::from(submit_times)),
            Arc::new(StringArray::from(timing_sources)),
            Arc::new(UInt64Array::from(present_ids)),
            Arc::new(UInt64Array::from(target_times)),
            Arc::new(BooleanArray::from(on_targets)),
            Arc::new(BooleanArray::from(missed)),
            Arc::new(UInt32Array::from(missed_counts)),
        ];

        // Determine user columns from first non-null payload
        let first_user = self.frame_rows.iter().find_map(|(_, v)| v.as_ref());
        if let Some(first_val) = first_user {
            if let Some(obj) = first_val.as_object() {
                for (key, exemplar) in obj {
                    match exemplar {
                        serde_json::Value::Number(_) => {
                            let vals: Vec<Option<f64>> = self
                                .frame_rows
                                .iter()
                                .map(|(_, user)| {
                                    user.as_ref()
                                        .and_then(|v| v.get(key))
                                        .and_then(|v| v.as_f64())
                                })
                                .collect();
                            fields.push(Field::new(key, DataType::Float64, true));
                            columns.push(Arc::new(Float64Array::from(vals)));
                        }
                        serde_json::Value::Bool(_) => {
                            let vals: Vec<Option<bool>> = self
                                .frame_rows
                                .iter()
                                .map(|(_, user)| {
                                    user.as_ref()
                                        .and_then(|v| v.get(key))
                                        .and_then(|v| v.as_bool())
                                })
                                .collect();
                            fields.push(Field::new(key, DataType::Boolean, true));
                            columns.push(Arc::new(BooleanArray::from(vals)));
                        }
                        _ => {
                            // String fallback
                            let vals: Vec<Option<String>> = self
                                .frame_rows
                                .iter()
                                .map(|(_, user)| {
                                    user.as_ref().and_then(|v| v.get(key)).map(|v| match v {
                                        serde_json::Value::String(s) => s.clone(),
                                        other => other.to_string(),
                                    })
                                })
                                .collect();
                            fields.push(Field::new(key, DataType::Utf8, true));
                            columns.push(Arc::new(StringArray::from(vals)));
                        }
                    }
                }
            }
        }

        let schema = Arc::new(Schema::new(fields));
        let batch = RecordBatch::try_new(schema.clone(), columns)
            .map_err(|e| DataError::Backend(e.to_string()))?;

        Self::write_batch(&self.path, schema, &batch)
    }

    fn write_annotations_parquet(&self) -> Result<(), DataError> {
        if self.annotation_rows.is_empty() {
            return Ok(());
        }

        let streams: Vec<String> = self
            .annotation_rows
            .iter()
            .map(|(stream, _, _)| stream.clone())
            .collect();
        let timestamps: Vec<u64> = self
            .annotation_rows
            .iter()
            .map(|(_, timestamp_us, _)| *timestamp_us)
            .collect();
        let payloads: Vec<String> = self
            .annotation_rows
            .iter()
            .map(|(_, _, payload)| payload.to_string())
            .collect();

        let schema = Arc::new(Schema::new(vec![
            Field::new("timestamp_us", DataType::UInt64, false),
            Field::new("stream", DataType::Utf8, false),
            Field::new("payload_json", DataType::Utf8, false),
        ]));
        let columns: Vec<Arc<dyn Array>> = vec![
            Arc::new(UInt64Array::from(timestamps)),
            Arc::new(StringArray::from(streams)),
            Arc::new(StringArray::from(payloads)),
        ];
        let batch = RecordBatch::try_new(schema.clone(), columns)
            .map_err(|e| DataError::Backend(e.to_string()))?;
        Self::write_batch(&self.stream_path("annotations"), schema, &batch)
    }

    fn write_events_parquet(&self) -> Result<(), DataError> {
        if self.event_rows.is_empty() {
            return Ok(());
        }

        let names: Vec<String> = self
            .event_rows
            .iter()
            .map(|(name, _, _)| name.clone())
            .collect();
        let timestamps: Vec<u64> = self
            .event_rows
            .iter()
            .map(|(_, timestamp_us, _)| *timestamp_us)
            .collect();
        let values: Vec<String> = self
            .event_rows
            .iter()
            .map(|(_, _, value)| value.clone())
            .collect();

        let schema = Arc::new(Schema::new(vec![
            Field::new("timestamp_us", DataType::UInt64, false),
            Field::new("name", DataType::Utf8, false),
            Field::new("value", DataType::Utf8, false),
        ]));
        let columns: Vec<Arc<dyn Array>> = vec![
            Arc::new(UInt64Array::from(timestamps)),
            Arc::new(StringArray::from(names)),
            Arc::new(StringArray::from(values)),
        ];
        let batch = RecordBatch::try_new(schema.clone(), columns)
            .map_err(|e| DataError::Backend(e.to_string()))?;
        Self::write_batch(&self.stream_path("events"), schema, &batch)
    }

    fn write_parquet(&self) -> Result<(), DataError> {
        self.write_frames_parquet()?;
        self.write_annotations_parquet()?;
        self.write_events_parquet()
    }
}

impl DataWriter for ParquetDataWriter {
    fn write_frame(&mut self, msg: FrameMessage) -> Result<(), DataError> {
        let user_val = msg
            .payload
            .as_ref()
            .and_then(|b| serde_json::from_slice(b).ok());
        self.frame_rows.push((msg.flip, user_val));
        Ok(())
    }

    fn write_annotation(&mut self, msg: AnnotationMessage) -> Result<(), DataError> {
        let val: serde_json::Value =
            serde_json::from_slice(&msg.payload).unwrap_or(serde_json::Value::Null);
        self.annotation_rows
            .push((msg.stream, msg.timestamp.as_micros(), val));
        Ok(())
    }

    fn write_event(&mut self, msg: EventMessage) -> Result<(), DataError> {
        self.event_rows
            .push((msg.name, msg.timestamp.as_micros(), msg.value));
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DataError> {
        self.write_parquet()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::writer::DataWriter;
    use crate::timing::{FlipInfo, Timestamp, TimingSource};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

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

    fn unique_temp_path(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "vse_{name}_{}_{}.parquet",
            std::process::id(),
            nonce
        ))
    }

    fn read_single_batch(path: &std::path::Path) -> RecordBatch {
        let file = std::fs::File::open(path).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let mut reader = builder.build().unwrap();
        reader.next().unwrap().unwrap()
    }

    fn sibling_path(path: &std::path::Path, stream: &str) -> PathBuf {
        let stem = path.file_stem().unwrap().to_string_lossy();
        path.with_file_name(format!("{stem}.{stream}.parquet"))
    }

    #[test]
    fn test_parquet_writes_annotations_and_events_to_sibling_files() {
        let path = unique_temp_path("parquet_streams");
        let annotations_path = sibling_path(&path, "annotations");
        let events_path = sibling_path(&path, "events");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&annotations_path);
        let _ = std::fs::remove_file(&events_path);

        let mut w = ParquetDataWriter::new(&path);
        w.write_frame(FrameMessage {
            flip: make_flip(0),
            payload: None,
            schema_name: "",
        })
        .unwrap();
        w.write_annotation(AnnotationMessage {
            stream: "trial".to_string(),
            timestamp: Timestamp::from_micros(1_000),
            payload: serde_json::to_vec(&serde_json::json!({"note": "comma, quote \" newline\n"}))
                .unwrap(),
        })
        .unwrap();
        w.write_event(EventMessage {
            name: "response".to_string(),
            timestamp: Timestamp::from_micros(2_000),
            value: "left, \"quoted\"\nline".to_string(),
        })
        .unwrap();
        w.flush().unwrap();

        assert!(std::fs::metadata(&annotations_path)
            .map(|m| m.len() > 0)
            .unwrap_or(false));
        assert!(std::fs::metadata(&events_path)
            .map(|m| m.len() > 0)
            .unwrap_or(false));

        let annotations = read_single_batch(&annotations_path);
        assert_eq!(annotations.num_rows(), 1);
        let streams = annotations
            .column_by_name("stream")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let payloads = annotations
            .column_by_name("payload_json")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(streams.value(0), "trial");
        assert_eq!(
            payloads.value(0),
            "{\"note\":\"comma, quote \\\" newline\\n\"}"
        );

        let events = read_single_batch(&events_path);
        assert_eq!(events.num_rows(), 1);
        let names = events
            .column_by_name("name")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let values = events
            .column_by_name("value")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(names.value(0), "response");
        assert_eq!(values.value(0), "left, \"quoted\"\nline");

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&annotations_path).ok();
        std::fs::remove_file(&events_path).ok();
    }

    #[test]
    fn test_parquet_writes_annotation_and_event_only_sessions() {
        let path = unique_temp_path("parquet_streams_only");
        let annotations_path = sibling_path(&path, "annotations");
        let events_path = sibling_path(&path, "events");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&annotations_path);
        let _ = std::fs::remove_file(&events_path);

        let mut w = ParquetDataWriter::new(&path);
        w.write_annotation(AnnotationMessage {
            stream: "calibration".to_string(),
            timestamp: Timestamp::from_micros(3_000),
            payload: serde_json::to_vec(&serde_json::json!({"display": "test"})).unwrap(),
        })
        .unwrap();
        w.write_event(EventMessage {
            name: "debug".to_string(),
            timestamp: Timestamp::from_micros(4_000),
            value: "started".to_string(),
        })
        .unwrap();
        w.flush().unwrap();

        assert!(
            !path.exists(),
            "empty frame stream should not create frame file"
        );
        assert_eq!(read_single_batch(&annotations_path).num_rows(), 1);
        assert_eq!(read_single_batch(&events_path).num_rows(), 1);

        std::fs::remove_file(&annotations_path).ok();
        std::fs::remove_file(&events_path).ok();
    }

    #[test]
    fn test_parquet_writes_file() {
        let path = unique_temp_path("test_frames");
        let _ = std::fs::remove_file(&path);

        let mut w = ParquetDataWriter::new(&path);

        // Timing-only then user data
        w.write_frame(FrameMessage {
            flip: make_flip(0),
            payload: None,
            schema_name: "",
        })
        .unwrap();
        w.write_frame(FrameMessage {
            flip: make_flip(1),
            payload: Some(serde_json::to_vec(&serde_json::json!({"contrast": 0.5_f64})).unwrap()),
            schema_name: "MyData",
        })
        .unwrap();
        w.flush().unwrap();

        // File must exist and be non-empty
        let meta = std::fs::metadata(&path).unwrap();
        assert!(meta.len() > 0, "parquet file should not be empty");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_parquet_no_user_data_still_writes() {
        let path = unique_temp_path("test_timing_only");
        let _ = std::fs::remove_file(&path);

        let mut w = ParquetDataWriter::new(&path);
        w.write_frame(FrameMessage {
            flip: make_flip(0),
            payload: None,
            schema_name: "",
        })
        .unwrap();
        w.write_frame(FrameMessage {
            flip: make_flip(1),
            payload: None,
            schema_name: "",
        })
        .unwrap();
        w.flush().unwrap();

        assert!(std::fs::metadata(&path)
            .map(|m| m.len() > 0)
            .unwrap_or(false));
        std::fs::remove_file(&path).ok();
    }
}
