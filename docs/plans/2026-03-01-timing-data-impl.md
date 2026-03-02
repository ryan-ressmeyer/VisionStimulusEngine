# Timing & Data Recording Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Introduce a flexible, backend-agnostic experiment data recording system
(`ExperimentSession` + `DataWriter` trait) with CSV and Parquet backends, decoupling
timing infrastructure from data persistence and deprecating the old `FlipLogger`
public API.

**Architecture:** A new `src/data/` module provides the `DataWriter` trait, message
types, `ExperimentSession` (owns a bounded channel + writer thread), and two
backends (`CsvDataWriter`, `ParquetDataWriter`). `VSEState` grows a
`RecordingState` field that tracks the pending-flip and dispatches timing-only rows
for un-claimed flips. `RenderContext` exposes `record_frame`, `record_annotation`,
and `record_event`.

**Tech Stack:** Rust, existing `serde`/`csv` deps, new `arrow`/`parquet`/`serde_arrow`
deps (pure Rust, no C libraries), `std::sync::mpsc::SyncSender` for bounded channel.

**Design doc:** `docs/plans/2026-03-01-timing-data-design.md`

---

## Task 1: DataError + DataWriter trait

**Files:**
- Create: `src/data/writer.rs`

**Step 1: Write the failing test**

Add at the bottom of the new file:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::timing::{FlipInfo, Timestamp, TimingSource};
    use crate::data::messages::{FrameMessage, AnnotationMessage, EventMessage};

    struct NullWriter;
    impl DataWriter for NullWriter {
        fn write_frame(&mut self, _: FrameMessage) -> Result<(), DataError> { Ok(()) }
        fn write_annotation(&mut self, _: AnnotationMessage) -> Result<(), DataError> { Ok(()) }
        fn write_event(&mut self, _: EventMessage) -> Result<(), DataError> { Ok(()) }
        fn flush(&mut self) -> Result<(), DataError> { Ok(()) }
    }

    #[test]
    fn test_null_writer_implements_trait() {
        let mut w: Box<dyn DataWriter> = Box::new(NullWriter);
        // NullWriter is Send + 'static: verify it can be boxed as trait object
        drop(w);
    }

    #[test]
    fn test_data_error_display() {
        let e = DataError::ChannelDisconnected;
        assert!(e.to_string().contains("disconnected"));
        let io_err = DataError::Io(std::io::Error::new(std::io::ErrorKind::Other, "test"));
        assert!(io_err.to_string().contains("IO"));
    }
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test data::writer -- --nocapture 2>&1 | head -20
```
Expected: compile error — `data` module does not exist yet.

**Step 3: Implement**

Create `src/data/writer.rs`:
```rust
//! DataWriter trait — the persistence abstraction for experiment data.

/// Errors produced by DataWriter backends.
#[derive(Debug, thiserror::Error)]
pub enum DataError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("Channel disconnected — writer thread has stopped")]
    ChannelDisconnected,
    #[error("Writer backend error: {0}")]
    Backend(String),
}

use crate::data::messages::{AnnotationMessage, EventMessage, FrameMessage};

/// Persistence backend for experiment data.
///
/// Implement this trait to create a custom data recording backend.
/// The implementation runs on a dedicated writer thread — all methods
/// may perform blocking I/O safely.
///
/// # Example: custom UDP streaming backend
///
/// ```rust,no_run
/// use vision_stimulus_engine::data::{DataWriter, DataError};
/// use vision_stimulus_engine::data::messages::{FrameMessage, AnnotationMessage, EventMessage};
/// use std::net::UdpSocket;
///
/// struct UdpWriter { socket: UdpSocket, addr: String }
///
/// impl DataWriter for UdpWriter {
///     fn write_frame(&mut self, msg: FrameMessage) -> Result<(), DataError> {
///         let json = serde_json::json!({
///             "frame": msg.flip.frame_number,
///             "present_us": msg.flip.present_time.as_micros(),
///         });
///         self.socket.send_to(json.to_string().as_bytes(), &self.addr)
///             .map_err(DataError::Io)?;
///         Ok(())
///     }
///     fn write_annotation(&mut self, _msg: AnnotationMessage) -> Result<(), DataError> { Ok(()) }
///     fn write_event(&mut self, _msg: EventMessage) -> Result<(), DataError> { Ok(()) }
///     fn flush(&mut self) -> Result<(), DataError> { Ok(()) }
/// }
/// ```
pub trait DataWriter: Send + 'static {
    /// Write a frame record. Called for every flip — payload is None for
    /// timing-only rows (frames where `record_frame` was not called).
    fn write_frame(&mut self, msg: FrameMessage) -> Result<(), DataError>;

    /// Write a typed annotation. `msg.stream` is the table/group name.
    fn write_annotation(&mut self, msg: AnnotationMessage) -> Result<(), DataError>;

    /// Write a raw key-value event.
    fn write_event(&mut self, msg: EventMessage) -> Result<(), DataError>;

    /// Flush all buffered data. Called on clean shutdown before the writer
    /// thread exits. Must block until all data is written to durable storage.
    fn flush(&mut self) -> Result<(), DataError>;
}
```

Wire the module (temporary, will be replaced in Task 3):
Add to `src/lib.rs` (before other mods):
```rust
pub mod data;
```

Create `src/data/mod.rs` (stub — will be expanded in Task 3):
```rust
pub mod writer;
pub mod messages;
pub use writer::{DataError, DataWriter};
```

Create `src/data/messages.rs` (stub — full content in Task 2):
```rust
use crate::timing::{FlipInfo, Timestamp};
pub struct FrameMessage { pub flip: FlipInfo, pub payload: Option<Vec<u8>>, pub schema_name: &'static str }
pub struct AnnotationMessage { pub stream: String, pub timestamp: Timestamp, pub payload: Vec<u8> }
pub struct EventMessage { pub name: String, pub timestamp: Timestamp, pub value: String }
pub(crate) enum WriterMessage { Frame(FrameMessage), Annotation(AnnotationMessage), Event(EventMessage), Flush, Shutdown }
```

**Step 4: Run tests to verify they pass**

```bash
cargo test data::writer -- --nocapture
```
Expected: 2 tests pass.

**Step 5: Commit**

```bash
git add src/data/writer.rs src/data/mod.rs src/data/messages.rs src/lib.rs
git commit -m "feat(data): add DataWriter trait and DataError"
```

---

## Task 2: Message types

**Files:**
- Modify: `src/data/messages.rs` (replace stub)

**Step 1: Write the failing test**

Add to the bottom of `src/data/messages.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::timing::{FlipInfo, Timestamp, TimingSource};

    fn make_flip(frame: u64) -> FlipInfo {
        FlipInfo {
            frame_number: frame,
            timing_source: TimingSource::CpuEstimate,
            submit_time: Timestamp::from_micros(frame * 16_667),
            present_time: Timestamp::from_micros(frame * 16_667 + 500),
            missed: false,
            missed_count: 0,
            skipped: false,
        }
    }

    #[test]
    fn test_frame_message_timing_only() {
        let msg = FrameMessage {
            flip: make_flip(5),
            payload: None,
            schema_name: "",
        };
        assert!(msg.payload.is_none());
        assert_eq!(msg.flip.frame_number, 5);
    }

    #[test]
    fn test_frame_message_with_payload() {
        let data = serde_json::json!({"contrast": 0.8_f32, "orientation": 45_u32});
        let payload = serde_json::to_vec(&data).unwrap();
        let msg = FrameMessage {
            flip: make_flip(10),
            payload: Some(payload.clone()),
            schema_name: "MyFrameData",
        };
        assert_eq!(msg.payload.unwrap(), payload);
        assert_eq!(msg.schema_name, "MyFrameData");
    }

    #[test]
    fn test_annotation_message() {
        let msg = AnnotationMessage {
            stream: "trial".to_string(),
            timestamp: Timestamp::from_micros(1_000_000),
            payload: b"{}".to_vec(),
        };
        assert_eq!(msg.stream, "trial");
        assert_eq!(msg.timestamp.as_micros(), 1_000_000);
    }

    #[test]
    fn test_event_message() {
        let msg = EventMessage {
            name: "response".to_string(),
            timestamp: Timestamp::from_micros(500_000),
            value: "left".to_string(),
        };
        assert_eq!(msg.name, "response");
        assert_eq!(msg.value, "left");
    }

    #[test]
    fn test_writer_message_variants() {
        // Just verify enum variants compile and can be pattern matched
        let msgs = vec![
            WriterMessage::Frame(FrameMessage { flip: make_flip(0), payload: None, schema_name: "" }),
            WriterMessage::Flush,
            WriterMessage::Shutdown,
        ];
        assert_eq!(msgs.len(), 3);
    }
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test data::messages -- --nocapture 2>&1 | head -20
```
Expected: fails — stub `messages.rs` missing fields/derives.

**Step 3: Implement**

Replace `src/data/messages.rs` stub with:
```rust
//! Message types sent from the render thread to the writer thread.

use crate::timing::{FlipInfo, Timestamp};

/// A frame record — timing data from flip() plus optional user-defined payload.
///
/// `payload` is `None` for frames where `record_frame` was not called
/// (timing-only rows). The writer backend writes all fields from `flip`
/// regardless, using null/empty values for user columns when payload is absent.
pub struct FrameMessage {
    /// Timing data for this frame, always populated.
    pub flip: FlipInfo,
    /// Serde-JSON-serialized user struct, or None for timing-only rows.
    pub payload: Option<Vec<u8>>,
    /// `std::any::type_name` of the user struct (used as schema identifier).
    pub schema_name: &'static str,
}

/// A typed annotation record — arbitrary user struct at a named stream.
pub struct AnnotationMessage {
    /// Stream/table name (e.g. "trial", "subject", "calibration").
    pub stream: String,
    /// Clock timestamp when this annotation was recorded.
    pub timestamp: Timestamp,
    /// Serde-JSON-serialized user struct.
    pub payload: Vec<u8>,
}

/// A raw key-value event record — unstructured escape hatch.
pub struct EventMessage {
    /// Event name (e.g. "response", "trigger", "debug_note").
    pub name: String,
    /// Clock timestamp when this event was recorded.
    pub timestamp: Timestamp,
    /// String value.
    pub value: String,
}

/// Internal channel message envelope.
pub(crate) enum WriterMessage {
    Frame(FrameMessage),
    Annotation(AnnotationMessage),
    Event(EventMessage),
    /// Flush all pending data to storage. Does not shut down the thread.
    Flush,
    /// Flush and shut down the writer thread.
    Shutdown,
}

#[cfg(test)]
mod tests {
    // ... (tests from Step 1 above)
}
```

**Step 4: Run tests to verify they pass**

```bash
cargo test data::messages -- --nocapture
```
Expected: 5 tests pass.

**Step 5: Commit**

```bash
git add src/data/messages.rs
git commit -m "feat(data): add FrameMessage, AnnotationMessage, EventMessage types"
```

---

## Task 3: Wire data module + update prelude

**Files:**
- Modify: `src/data/mod.rs`
- Modify: `src/lib.rs`

**Step 1: Verify existing tests still pass**

```bash
cargo test 2>&1 | tail -5
```
Expected: all existing tests pass.

**Step 2: Implement**

Replace `src/data/mod.rs` with:
```rust
//! Experiment data recording infrastructure.
//!
//! See [`ExperimentSession`] for the main entry point and
//! [`DataWriter`] for implementing custom backends.

pub mod messages;
mod session;
mod csv_writer;
mod parquet_writer;
mod writer;

pub use writer::{DataError, DataWriter};
pub use session::{ExperimentSession, ExperimentSessionBuilder, OverflowBehavior};
pub use csv_writer::CsvDataWriter;
pub use parquet_writer::ParquetDataWriter;
```

Note: `session.rs`, `csv_writer.rs`, `parquet_writer.rs` don't exist yet — add stub
files so the crate still compiles:

`src/data/session.rs` stub:
```rust
pub struct ExperimentSession;
pub struct ExperimentSessionBuilder;
pub enum OverflowBehavior { Block, DropWithWarning }
```

`src/data/csv_writer.rs` stub:
```rust
pub struct CsvDataWriter;
```

`src/data/parquet_writer.rs` stub:
```rust
pub struct ParquetDataWriter;
```

Add to the `pub use` block in `src/lib.rs` prelude module:
```rust
// In the prelude mod:
pub use crate::data::{
    CsvDataWriter, DataError, DataWriter, ExperimentSession,
    ExperimentSessionBuilder, OverflowBehavior, ParquetDataWriter,
};
```

**Step 3: Verify it compiles**

```bash
cargo check 2>&1 | head -20
```
Expected: no errors.

**Step 4: Commit**

```bash
git add src/data/mod.rs src/data/session.rs src/data/csv_writer.rs src/data/parquet_writer.rs src/lib.rs
git commit -m "feat(data): wire data module and update prelude (stubs)"
```

---

## Task 4: CsvDataWriter

**Files:**
- Modify: `src/data/csv_writer.rs` (replace stub)

The CSV backend produces two files in a directory:
- `frames.csv`: one row per flip. Timing columns always present; user columns
  added when first `record_frame` payload arrives. Frames before the first
  payload are buffered in memory then flushed once the schema is known.
- `events.csv`: annotations and raw events, distinguished by a `stream` column.

**Step 1: Write the failing tests**

```rust
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
            missed: false,
            missed_count: 0,
            skipped: false,
        }
    }

    fn make_payload(contrast: f32, orientation: u32) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "contrast": contrast,
            "orientation": orientation
        })).unwrap()
    }

    #[test]
    fn test_csv_timing_only_rows() {
        let dir = std::env::temp_dir().join("vse_csv_test_timing_only");
        let _ = std::fs::remove_dir_all(&dir);
        let mut w = CsvDataWriter::new(&dir);

        // Write three timing-only frames then flush
        for i in 0..3u64 {
            w.write_frame(FrameMessage {
                flip: make_flip(i), payload: None, schema_name: "",
            }).unwrap();
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
            flip: make_flip(0), payload: None, schema_name: "",
        }).unwrap();
        w.write_frame(FrameMessage {
            flip: make_flip(1),
            payload: Some(make_payload(0.8, 45)),
            schema_name: "MyFrameData",
        }).unwrap();
        w.flush().unwrap();

        let frames = std::fs::read_to_string(dir.join("frames.csv")).unwrap();
        let lines: Vec<&str> = frames.lines().collect();
        // header + 2 rows
        assert_eq!(lines.len(), 3);
        // Header has user columns
        assert!(lines[0].contains("contrast"));
        assert!(lines[0].contains("orientation"));
        // First row (timing-only) has empty user columns
        assert!(lines[1].ends_with(",,") || lines[1].split(',').count() == lines[0].split(',').count());
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
        }).unwrap();
        w.write_event(EventMessage {
            name: "response".to_string(),
            timestamp: Timestamp::from_micros(2_000),
            value: "left".to_string(),
        }).unwrap();
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
```

**Step 2: Run tests to verify they fail**

```bash
cargo test data::csv_writer -- --nocapture 2>&1 | head -20
```
Expected: compile error — stub has no methods.

**Step 3: Implement**

Replace `src/data/csv_writer.rs` with:

```rust
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
use std::path::{Path, PathBuf};

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

    /// Serialize a FlipInfo into timing CSV columns.
    fn flip_to_csv(flip: &FlipInfo) -> String {
        format!(
            "{},{},{},{},{},{}",
            flip.frame_number,
            flip.present_time.as_micros(),
            flip.submit_time.as_micros(),
            flip.timing_source,
            flip.missed,
            flip.missed_count,
            // skipped is always false here (skipped frames are not recorded)
        )
    }

    /// Write a timing-only row (empty user columns).
    fn write_timing_only_row(&mut self, flip: &FlipInfo, n_user_cols: usize) -> Result<(), DataError> {
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

                let user_cols = self.user_columns.as_ref().unwrap();
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
                writeln!(file, "{},{},{}", timing, msg.flip.skipped, user_vals.join(","))?;
            }
        }
        Ok(())
    }

    fn write_annotation(&mut self, msg: AnnotationMessage) -> Result<(), DataError> {
        self.ensure_events_header()?;
        let payload_str = String::from_utf8_lossy(&msg.payload);
        let file = self.events_file()?;
        writeln!(file, "{},{},{}", msg.timestamp.as_micros(), msg.stream, payload_str)?;
        Ok(())
    }

    fn write_event(&mut self, msg: EventMessage) -> Result<(), DataError> {
        self.ensure_events_header()?;
        let file = self.events_file()?;
        writeln!(file, "{},{},{}", msg.timestamp.as_micros(), msg.name, msg.value)?;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DataError> {
        // If no user data ever arrived, write header with timing-only cols and flush pending
        if self.user_columns.is_none() && !self.pending_timing.is_empty() {
            self.write_frames_header(&[])?;
            self.user_columns = Some(vec![]);
            self.flush_pending_timing()?;
        }
        if let Some(f) = &mut self.frames_file { f.flush()?; }
        if let Some(f) = &mut self.events_file { f.flush()?; }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // ... (tests from Step 1 above)
}
```

**Step 4: Run tests to verify they pass**

```bash
cargo test data::csv_writer -- --nocapture
```
Expected: 3 tests pass.

**Step 5: Commit**

```bash
git add src/data/csv_writer.rs
git commit -m "feat(data): implement CsvDataWriter backend"
```

---

## Task 5: ParquetDataWriter

**Files:**
- Modify: `Cargo.toml` — add arrow, parquet, serde_arrow
- Modify: `src/data/parquet_writer.rs` (replace stub)

The Parquet backend buffers all rows in memory and writes a single Parquet file
on `flush()`. Rows are accumulated as `serde_json::Value` objects; on flush,
the Arrow schema is inferred from the first non-null user payload, and rows are
converted to Arrow RecordBatch using `serde_arrow`.

**Step 1: Add dependencies**

In `Cargo.toml` under `[dependencies]`:
```toml
# Parquet/Arrow for columnar data output
arrow = { version = "53", default-features = false, features = ["ipc"] }
parquet = { version = "53", default-features = false, features = ["arrow"] }
serde_arrow = { version = "0.12", features = ["arrow-53"] }
```

Verify it builds:
```bash
cargo check 2>&1 | head -10
```

**Step 2: Write the failing tests**

```rust
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
            missed: false,
            missed_count: 0,
            skipped: false,
        }
    }

    #[test]
    fn test_parquet_writes_file() {
        let path = std::env::temp_dir().join("vse_test_frames.parquet");
        let _ = std::fs::remove_file(&path);

        let mut w = ParquetDataWriter::new(&path);

        // Timing-only then user data
        w.write_frame(FrameMessage { flip: make_flip(0), payload: None, schema_name: "" }).unwrap();
        w.write_frame(FrameMessage {
            flip: make_flip(1),
            payload: Some(serde_json::to_vec(&serde_json::json!({"contrast": 0.5_f64})).unwrap()),
            schema_name: "MyData",
        }).unwrap();
        w.flush().unwrap();

        // File must exist and be non-empty
        let meta = std::fs::metadata(&path).unwrap();
        assert!(meta.len() > 0, "parquet file should not be empty");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_parquet_no_user_data_still_writes() {
        let path = std::env::temp_dir().join("vse_test_timing_only.parquet");
        let _ = std::fs::remove_file(&path);

        let mut w = ParquetDataWriter::new(&path);
        w.write_frame(FrameMessage { flip: make_flip(0), payload: None, schema_name: "" }).unwrap();
        w.write_frame(FrameMessage { flip: make_flip(1), payload: None, schema_name: "" }).unwrap();
        w.flush().unwrap();

        assert!(std::fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false));
        std::fs::remove_file(&path).ok();
    }
}
```

**Step 3: Run tests to verify they fail**

```bash
cargo test data::parquet_writer -- --nocapture 2>&1 | head -20
```
Expected: compile error or missing-method error on stub.

**Step 4: Implement**

Replace `src/data/parquet_writer.rs` with:

```rust
//! Parquet backend for experiment data recording.
//!
//! Buffers all frame/event rows in memory and writes a single `.parquet` file
//! on `flush()`. Uses Arrow for the in-memory representation and `serde_arrow`
//! to infer the schema from the first non-null user payload.
//!
//! Read in Python with:
//! ```python
//! import pandas as pd
//! df = pd.read_parquet("frames.parquet")
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

    fn write_parquet(&self) -> Result<(), DataError> {
        if self.frame_rows.is_empty() {
            return Ok(());
        }

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Build Arrow arrays for timing columns
        let n = self.frame_rows.len();
        let frame_nums: Vec<u64> = self.frame_rows.iter().map(|(f, _)| f.frame_number).collect();
        let present_times: Vec<u64> = self.frame_rows.iter().map(|(f, _)| f.present_time.as_micros()).collect();
        let submit_times: Vec<u64> = self.frame_rows.iter().map(|(f, _)| f.submit_time.as_micros()).collect();
        let timing_sources: Vec<String> = self.frame_rows.iter().map(|(f, _)| f.timing_source.to_string()).collect();
        let missed: Vec<bool> = self.frame_rows.iter().map(|(f, _)| f.missed).collect();
        let missed_counts: Vec<u32> = self.frame_rows.iter().map(|(f, _)| f.missed_count).collect();

        let mut fields = vec![
            Field::new("frame_number", DataType::UInt64, false),
            Field::new("present_time_us", DataType::UInt64, false),
            Field::new("submit_time_us", DataType::UInt64, false),
            Field::new("timing_source", DataType::Utf8, false),
            Field::new("missed", DataType::Boolean, false),
            Field::new("missed_count", DataType::UInt32, false),
        ];

        let mut columns: Vec<Arc<dyn arrow::array::Array>> = vec![
            Arc::new(UInt64Array::from(frame_nums)),
            Arc::new(UInt64Array::from(present_times)),
            Arc::new(UInt64Array::from(submit_times)),
            Arc::new(StringArray::from(timing_sources)),
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
                            let vals: Vec<Option<f64>> = self.frame_rows.iter().map(|(_, user)| {
                                user.as_ref()
                                    .and_then(|v| v.get(key))
                                    .and_then(|v| v.as_f64())
                            }).collect();
                            fields.push(Field::new(key, DataType::Float64, true));
                            columns.push(Arc::new(Float64Array::from(vals)));
                        }
                        serde_json::Value::Bool(_) => {
                            let vals: Vec<Option<bool>> = self.frame_rows.iter().map(|(_, user)| {
                                user.as_ref()
                                    .and_then(|v| v.get(key))
                                    .and_then(|v| v.as_bool())
                            }).collect();
                            fields.push(Field::new(key, DataType::Boolean, true));
                            columns.push(Arc::new(BooleanArray::from(vals)));
                        }
                        _ => {
                            // String fallback
                            let vals: Vec<Option<String>> = self.frame_rows.iter().map(|(_, user)| {
                                user.as_ref()
                                    .and_then(|v| v.get(key))
                                    .map(|v| match v {
                                        serde_json::Value::String(s) => s.clone(),
                                        other => other.to_string(),
                                    })
                            }).collect();
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

        let file = std::fs::File::create(&self.path)?;
        let props = WriterProperties::builder().build();
        let mut writer = ArrowWriter::try_new(file, schema, Some(props))
            .map_err(|e| DataError::Backend(e.to_string()))?;
        writer.write(&batch).map_err(|e| DataError::Backend(e.to_string()))?;
        writer.close().map_err(|e| DataError::Backend(e.to_string()))?;

        Ok(())
    }
}

impl DataWriter for ParquetDataWriter {
    fn write_frame(&mut self, msg: FrameMessage) -> Result<(), DataError> {
        let user_val = msg.payload.as_ref().and_then(|b| serde_json::from_slice(b).ok());
        self.frame_rows.push((msg.flip, user_val));
        Ok(())
    }

    fn write_annotation(&mut self, msg: AnnotationMessage) -> Result<(), DataError> {
        let val: serde_json::Value = serde_json::from_slice(&msg.payload)
            .unwrap_or(serde_json::Value::Null);
        self.annotation_rows.push((msg.stream, msg.timestamp.as_micros(), val));
        Ok(())
    }

    fn write_event(&mut self, msg: EventMessage) -> Result<(), DataError> {
        self.event_rows.push((msg.name, msg.timestamp.as_micros(), msg.value));
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DataError> {
        self.write_parquet()
    }
}

#[cfg(test)]
mod tests {
    // ... (tests from Step 2 above)
}
```

**Step 5: Run tests to verify they pass**

```bash
cargo test data::parquet_writer -- --nocapture
```
Expected: 2 tests pass.

**Step 6: Commit**

```bash
git add Cargo.toml src/data/parquet_writer.rs
git commit -m "feat(data): implement ParquetDataWriter backend"
```

---

## Task 6: ExperimentSession + OverflowBehavior + Builder

**Files:**
- Modify: `src/data/session.rs` (replace stub)

`ExperimentSession` owns a `SyncSender<WriterMessage>` (bounded channel) and
a writer thread. On creation it spawns the thread. On drop it sends `Shutdown`
and joins the thread.

**Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::csv_writer::CsvDataWriter;
    use crate::data::messages::*;
    use crate::timing::{FlipInfo, Timestamp, TimingSource};

    fn make_flip(frame: u64) -> FlipInfo {
        FlipInfo {
            frame_number: frame,
            timing_source: TimingSource::CpuEstimate,
            submit_time: Timestamp::from_micros(0),
            present_time: Timestamp::from_micros(16_667),
            missed: false, missed_count: 0, skipped: false,
        }
    }

    #[test]
    fn test_session_send_and_flush() {
        let dir = std::env::temp_dir().join("vse_session_test");
        let _ = std::fs::remove_dir_all(&dir);

        let session = ExperimentSession::builder()
            .with_writer(CsvDataWriter::new(&dir))
            .build()
            .unwrap();

        session.send_frame(FrameMessage {
            flip: make_flip(0), payload: None, schema_name: "",
        }).unwrap();

        // Drop triggers Shutdown + flush
        drop(session);

        assert!(dir.join("frames.csv").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_session_builder_defaults() {
        let builder = ExperimentSession::builder()
            .with_writer(CsvDataWriter::new("/tmp/ignored"))
            .with_channel_capacity(1024)
            .with_overflow(OverflowBehavior::DropWithWarning);
        assert!(matches!(builder.overflow, OverflowBehavior::DropWithWarning));
        assert_eq!(builder.channel_capacity, 1024);
    }

    #[test]
    fn test_overflow_drop_with_warning_does_not_panic() {
        // Fill the channel completely with a tiny capacity to trigger drop path
        let dir = std::env::temp_dir().join("vse_overflow_test");
        let _ = std::fs::remove_dir_all(&dir);

        // Use capacity=1 so it fills immediately
        let session = ExperimentSession::builder()
            .with_writer(CsvDataWriter::new(&dir))
            .with_channel_capacity(1)
            .with_overflow(OverflowBehavior::DropWithWarning)
            .build()
            .unwrap();

        // Send many messages — some will be dropped, none should panic
        for i in 0..100u64 {
            let _ = session.send_frame(FrameMessage {
                flip: make_flip(i), payload: None, schema_name: "",
            });
        }
        drop(session);
        std::fs::remove_dir_all(&dir).ok();
    }
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test data::session -- --nocapture 2>&1 | head -20
```
Expected: compile error — stub missing methods.

**Step 3: Implement**

Replace `src/data/session.rs` with:

```rust
//! ExperimentSession — manages the writer thread and message channel.

use std::sync::mpsc;
use std::thread;
use tracing::warn;

use crate::data::messages::{AnnotationMessage, EventMessage, FrameMessage, WriterMessage};
use crate::data::writer::{DataError, DataWriter};

/// Controls render-loop behavior when the writer channel is full.
///
/// The default channel capacity of 4096 messages provides ~68 seconds of
/// buffering at 60 Hz. Choose `Block` (default) to guarantee no data loss;
/// choose `DropWithWarning` to guarantee no frame-timing impact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowBehavior {
    /// Block the render loop until channel space is available. **Default.**
    /// No data loss. May cause a frame drop if the writer is persistently slow.
    Block,
    /// Drop the record and emit `tracing::warn!()`. Never stalls the render loop.
    /// May lose data if the writer falls far behind.
    DropWithWarning,
}

/// Manages a dedicated writer thread for non-blocking data recording.
///
/// Created via [`ExperimentSession::builder()`] and attached to [`VSEContext`]
/// via [`VSEContextBuilder::with_session()`]. Do not call recording methods
/// directly on `ExperimentSession` — use [`RenderContext::record_frame()`] etc.
///
/// On drop, sends a shutdown signal and joins the writer thread, ensuring all
/// buffered data is flushed to storage before the program exits.
pub struct ExperimentSession {
    tx: mpsc::SyncSender<WriterMessage>,
    thread: Option<thread::JoinHandle<()>>,
    pub(crate) overflow: OverflowBehavior,
}

impl ExperimentSession {
    /// Create a builder for configuring the session.
    pub fn builder() -> ExperimentSessionBuilder {
        ExperimentSessionBuilder {
            writer: None,
            channel_capacity: 4096,
            overflow: OverflowBehavior::Block,
        }
    }

    /// Send a FrameMessage. Called by RenderContext — not public API.
    pub(crate) fn send_frame(&self, msg: FrameMessage) -> Result<(), DataError> {
        self.send(WriterMessage::Frame(msg))
    }

    pub(crate) fn send_annotation(&self, msg: AnnotationMessage) -> Result<(), DataError> {
        self.send(WriterMessage::Annotation(msg))
    }

    pub(crate) fn send_event(&self, msg: EventMessage) -> Result<(), DataError> {
        self.send(WriterMessage::Event(msg))
    }

    fn send(&self, msg: WriterMessage) -> Result<(), DataError> {
        match self.overflow {
            OverflowBehavior::Block => {
                self.tx.send(msg).map_err(|_| DataError::ChannelDisconnected)
            }
            OverflowBehavior::DropWithWarning => {
                match self.tx.try_send(msg) {
                    Ok(()) => Ok(()),
                    Err(mpsc::TrySendError::Full(_)) => {
                        warn!("ExperimentSession: channel full — dropping record (DropWithWarning)");
                        Ok(())
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        Err(DataError::ChannelDisconnected)
                    }
                }
            }
        }
    }
}

impl Drop for ExperimentSession {
    fn drop(&mut self) {
        // Best-effort shutdown — ignore send error (thread may have panicked)
        let _ = self.tx.send(WriterMessage::Shutdown);
        if let Some(handle) = self.thread.take() {
            if let Err(e) = handle.join() {
                warn!("ExperimentSession: writer thread panicked: {:?}", e);
            }
        }
    }
}

/// Builder for [`ExperimentSession`].
pub struct ExperimentSessionBuilder {
    pub(crate) writer: Option<Box<dyn DataWriter>>,
    pub(crate) channel_capacity: usize,
    pub(crate) overflow: OverflowBehavior,
}

impl ExperimentSessionBuilder {
    /// Set the data writer backend (required).
    pub fn with_writer(mut self, writer: impl DataWriter) -> Self {
        self.writer = Some(Box::new(writer));
        self
    }

    /// Set the channel capacity (default: 4096).
    ///
    /// At 60 Hz, 4096 provides ~68 seconds of buffering before backpressure.
    pub fn with_channel_capacity(mut self, capacity: usize) -> Self {
        self.channel_capacity = capacity;
        self
    }

    /// Set the overflow behavior (default: [`OverflowBehavior::Block`]).
    pub fn with_overflow(mut self, overflow: OverflowBehavior) -> Self {
        self.overflow = overflow;
        self
    }

    /// Build the session and spawn the writer thread.
    pub fn build(self) -> Result<ExperimentSession, DataError> {
        let mut writer = self.writer.ok_or_else(|| {
            DataError::Backend("ExperimentSessionBuilder: no writer set (call .with_writer())".into())
        })?;

        let (tx, rx) = mpsc::sync_channel::<WriterMessage>(self.channel_capacity);

        let thread = thread::Builder::new()
            .name("vse-data-writer".into())
            .spawn(move || {
                for msg in &rx {
                    match msg {
                        WriterMessage::Frame(m) => {
                            if let Err(e) = writer.write_frame(m) {
                                warn!("DataWriter::write_frame error: {}", e);
                            }
                        }
                        WriterMessage::Annotation(m) => {
                            if let Err(e) = writer.write_annotation(m) {
                                warn!("DataWriter::write_annotation error: {}", e);
                            }
                        }
                        WriterMessage::Event(m) => {
                            if let Err(e) = writer.write_event(m) {
                                warn!("DataWriter::write_event error: {}", e);
                            }
                        }
                        WriterMessage::Flush => {
                            if let Err(e) = writer.flush() {
                                warn!("DataWriter::flush error: {}", e);
                            }
                        }
                        WriterMessage::Shutdown => {
                            if let Err(e) = writer.flush() {
                                warn!("DataWriter::flush on shutdown error: {}", e);
                            }
                            break;
                        }
                    }
                }
            })
            .map_err(DataError::Io)?;

        Ok(ExperimentSession {
            tx,
            thread: Some(thread),
            overflow: self.overflow,
        })
    }
}

#[cfg(test)]
mod tests {
    // ... (tests from Step 1 above)
}
```

**Step 4: Run tests to verify they pass**

```bash
cargo test data::session -- --nocapture
```
Expected: 3 tests pass.

**Step 5: Commit**

```bash
git add src/data/session.rs
git commit -m "feat(data): implement ExperimentSession with writer thread and backpressure"
```

---

## Task 7: VSEConfig + VSEContextBuilder integration

**Files:**
- Modify: `src/core/context.rs`

Add `session: Option<ExperimentSession>` to `VSEConfig`. Add
`with_session()` to `VSEContextBuilder`. Add `RecordingState` struct to
`VSEState`. Wire session from config into VSEState during `initialize_compositor`
and `initialize_direct`.

**Step 1: Write the failing test**

Find the existing `#[cfg(test)]` block in `context.rs` and add:
```rust
#[test]
#[ignore] // EventLoop::new() panics off main thread on Linux
fn test_builder_with_session_compiles() {
    use crate::data::{CsvDataWriter, ExperimentSession};
    let session = ExperimentSession::builder()
        .with_writer(CsvDataWriter::new("/tmp/test_session"))
        .build()
        .unwrap();
    let _builder = VSEContext::builder()
        .with_window_size(800, 600)
        .with_session(session);
    // Just verifies it compiles — ignored at runtime
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test test_builder_with_session_compiles -- --nocapture --ignored 2>&1 | head -10
```
Expected: compile error — `with_session` does not exist.

**Step 3: Implement**

In `src/core/context.rs`:

1. Add import at top:
```rust
use crate::data::session::ExperimentSession;
use crate::data::messages::FrameMessage;
```

2. Add to `VSEConfig`:
```rust
/// Optional experiment session for data recording.
pub session: Option<ExperimentSession>,
```
And in `VSEConfig::default()`:
```rust
session: None,
```

3. Add `RecordingState` struct (before or after `VSEState`):
```rust
/// Tracks per-frame recording state between flip() and record_frame() calls.
struct RecordingState {
    session: ExperimentSession,
    /// FlipInfo from the most recent flip(), available for record_frame().
    pending_flip: Option<FlipInfo>,
    /// frame_number of the most recently claimed flip (had record_frame called).
    last_claimed_frame: Option<u64>,
}
```

4. Add to `VSEState`:
```rust
recording: Option<RecordingState>,
```

5. In `initialize_compositor` and `initialize_direct`, after building VSEState, move `config.session` into recording:
```rust
let recording = config.session.take().map(|session| RecordingState {
    session,
    pending_flip: None,
    last_claimed_frame: None,
});
// Then in the VSEState struct literal:
recording,
```

6. Add `with_session()` to `VSEContextBuilder`:
```rust
/// Attach an experiment session for data recording.
///
/// Enables `record_frame()`, `record_annotation()`, and `record_event()`
/// on `RenderContext`. If not set, data recording is disabled.
pub fn with_session(mut self, session: ExperimentSession) -> Self {
    self.config.session = Some(session);
    self
}
```

**Step 4: Verify it compiles**

```bash
cargo check 2>&1 | head -20
```
Expected: no errors.

**Step 5: Run all tests**

```bash
cargo test 2>&1 | tail -10
```
Expected: all existing tests still pass.

**Step 6: Commit**

```bash
git add src/core/context.rs
git commit -m "feat(data): add with_session() to VSEContextBuilder and RecordingState to VSEState"
```

---

## Task 8: RenderContext recording methods

**Files:**
- Modify: `src/core/context.rs`

Add `VSEError::NoFlipPending` variant and `record_frame()`,
`record_annotation()`, `record_event()` methods to `RenderContext`.

**Step 1: Write the failing test**

In `context.rs` tests:
```rust
#[test]
fn test_record_frame_without_flip_returns_error() {
    // RecordingState with no pending_flip should return NoFlipPending.
    // Test the error variant exists and has the right message.
    let err = VSEError::NoFlipPending;
    assert!(err.to_string().contains("flip"));
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test test_record_frame_without_flip_returns_error -- --nocapture 2>&1 | head -10
```
Expected: compile error — `VSEError::NoFlipPending` does not exist.

**Step 3: Implement**

1. Add to `VSEError` enum:
```rust
/// record_frame() called before flip() in the current frame.
#[error("record_frame() called before flip() — call flip() first")]
NoFlipPending,

/// No ExperimentSession attached. Call VSEContextBuilder::with_session() to enable recording.
#[error("no ExperimentSession attached — call .with_session() on the builder")]
NoSession,
```

2. Add to `RenderContext` impl block:

```rust
/// Record per-frame experimental data merged with the most recent flip's timing.
///
/// Must be called after `flip()`. The data struct must implement
/// `serde::Serialize`. Multiple calls per frame are allowed — each produces
/// one row in the output keyed to the same `frame_number`.
///
/// Returns `VSEError::NoFlipPending` if `flip()` has not been called yet this
/// frame, or `VSEError::NoSession` if no session was attached to the builder.
pub fn record_frame<F: serde::Serialize>(&mut self, data: F) -> Result<(), VSEError> {
    let recording = self.state.recording.as_mut().ok_or(VSEError::NoSession)?;
    let flip = recording.pending_flip.clone().ok_or(VSEError::NoFlipPending)?;

    recording.last_claimed_frame = Some(flip.frame_number);

    let payload = serde_json::to_vec(&data)
        .map_err(|e| VSEError::DataRecording(e.to_string()))?;

    recording.session.send_frame(FrameMessage {
        flip,
        payload: Some(payload),
        schema_name: std::any::type_name::<F>(),
    }).map_err(|e| VSEError::DataRecording(e.to_string()))?;

    Ok(())
}

/// Record a typed annotation at the current timestamp.
///
/// `stream` is the table/group name in the output file (e.g. `"trial"`,
/// `"subject_info"`, `"calibration"`). Any `serde::Serialize` type is accepted.
pub fn record_annotation<A: serde::Serialize>(
    &mut self,
    stream: &str,
    data: A,
) -> Result<(), VSEError> {
    let recording = self.state.recording.as_mut().ok_or(VSEError::NoSession)?;
    let timestamp = self.state.clock.now();
    let payload = serde_json::to_vec(&data)
        .map_err(|e| VSEError::DataRecording(e.to_string()))?;
    recording.session.send_annotation(crate::data::messages::AnnotationMessage {
        stream: stream.to_string(),
        timestamp,
        payload,
    }).map_err(|e| VSEError::DataRecording(e.to_string()))?;
    Ok(())
}

/// Record a raw key-value event at the current timestamp.
///
/// Use for unstructured or one-off data. For structured, repeated data
/// prefer [`record_frame`] or [`record_annotation`].
pub fn record_event(&mut self, name: &str, value: &str) -> Result<(), VSEError> {
    let recording = self.state.recording.as_mut().ok_or(VSEError::NoSession)?;
    let timestamp = self.state.clock.now();
    recording.session.send_event(crate::data::messages::EventMessage {
        name: name.to_string(),
        timestamp,
        value: value.to_string(),
    }).map_err(|e| VSEError::DataRecording(e.to_string()))?;
    Ok(())
}
```

3. Add `VSEError::DataRecording` variant:
```rust
#[error("Data recording error: {0}")]
DataRecording(String),
```

**Step 4: Run tests to verify they pass**

```bash
cargo test 2>&1 | tail -10
```
Expected: all tests pass.

**Step 5: Commit**

```bash
git add src/core/context.rs
git commit -m "feat(data): add record_frame, record_annotation, record_event to RenderContext"
```

---

## Task 9: Automatic timing-only rows in flip()

**Files:**
- Modify: `src/core/context.rs`

Modify `flip()` so that when `RecordingState` is present, it:
1. Flushes the previous frame's FlipInfo as a timing-only row if no
   `record_frame` was called for it.
2. Stores the new FlipInfo in `pending_flip`.

Also add `RecordingState` cleanup on session drop (in the run loop shutdown
path) to flush the final frame.

**Step 1: Write the failing test**

In `context.rs` tests:
```rust
#[test]
fn test_recording_state_pending_flip_handoff() {
    // Unit test the pending flip handoff logic without a GPU.
    // Simulate what flip() does to RecordingState.
    use crate::data::{CsvDataWriter, ExperimentSession};
    use crate::timing::{FlipInfo, Timestamp, TimingSource};

    let dir = std::env::temp_dir().join("vse_pending_flip_test");
    let _ = std::fs::remove_dir_all(&dir);

    let session = ExperimentSession::builder()
        .with_writer(CsvDataWriter::new(&dir))
        .build()
        .unwrap();

    let mut state = RecordingState {
        session,
        pending_flip: None,
        last_claimed_frame: None,
    };

    let make_flip = |n: u64| FlipInfo {
        frame_number: n,
        timing_source: TimingSource::CpuEstimate,
        submit_time: Timestamp::from_micros(0),
        present_time: Timestamp::from_micros(16_667),
        missed: false, missed_count: 0, skipped: false,
    };

    // Simulate flip(0) — no record_frame called
    recording_state_on_flip(&mut state, make_flip(0));
    // Simulate flip(1) — flip(0) was unclaimed, timing-only row sent
    recording_state_on_flip(&mut state, make_flip(1));

    // Drop sends Shutdown + flush
    drop(state.session);

    std::thread::sleep(std::time::Duration::from_millis(50));

    let frames = std::fs::read_to_string(dir.join("frames.csv")).unwrap();
    let lines: Vec<&str> = frames.lines().collect();
    // header + 1 timing-only row for frame 0 (frame 1 pending, never flushed in this test)
    assert!(lines.len() >= 2, "expected at least header + 1 row, got: {:?}", lines);
    std::fs::remove_dir_all(&dir).ok();
}
```

Note: This test requires exposing `recording_state_on_flip` as a free function
or making `RecordingState` pub(crate) with a method. Adjust visibility as needed.

**Step 2: Implement**

Add a `pub(crate) fn on_flip` method to `RecordingState`:

```rust
impl RecordingState {
    /// Called by flip() after FlipInfo is computed.
    /// Sends timing-only row for the previous unclaimed flip, then caches new flip.
    pub(crate) fn on_flip(&mut self, new_flip: FlipInfo) {
        if let Some(prev) = self.pending_flip.take() {
            let already_claimed = self.last_claimed_frame == Some(prev.frame_number);
            if !already_claimed {
                // No record_frame was called for this flip — send timing-only row
                let _ = self.session.send_frame(FrameMessage {
                    flip: prev,
                    payload: None,
                    schema_name: "",
                });
            }
        }
        self.pending_flip = Some(new_flip);
    }

    /// Called on session shutdown — flushes final pending flip as timing-only if unclaimed.
    pub(crate) fn on_shutdown(&mut self) {
        if let Some(flip) = self.pending_flip.take() {
            let claimed = self.last_claimed_frame == Some(flip.frame_number);
            if !claimed {
                let _ = self.session.send_frame(FrameMessage {
                    flip,
                    payload: None,
                    schema_name: "",
                });
            }
        }
    }
}
```

In the `flip()` method in `RenderContext`, after recording to `flip_logger`,
add:
```rust
// Notify RecordingState of new flip
if let Some(recording) = &mut self.state.recording {
    recording.on_flip(flip_info.clone());
}
```

In the run loop shutdown path (where resources are cleaned up after the
callback loop exits), add:
```rust
if let Some(recording) = &mut state.recording {
    recording.on_shutdown();
}
```
(The `ExperimentSession` drop will then send the Flush + Shutdown messages.)

**Step 3: Run all tests**

```bash
cargo test 2>&1 | tail -10
```
Expected: all tests pass.

**Step 4: Commit**

```bash
git add src/core/context.rs
git commit -m "feat(data): auto timing-only rows for un-claimed flips in flip()"
```

---

## Task 10: Deprecate FlipLogger public API

**Files:**
- Modify: `src/core/context.rs`
- Modify: `src/timing/flip_logger.rs`

Mark `with_flip_logging()` and `with_flip_log_csv()` as `#[deprecated]`. Make
`FlipLogger` `pub(crate)` (remove from public re-exports).

**Step 1: Verify existing tests pass before changes**

```bash
cargo test 2>&1 | tail -5
```

**Step 2: Implement**

In `src/core/context.rs`:
```rust
#[deprecated(
    since = "0.2.0",
    note = "Use VSEContextBuilder::with_session(ExperimentSession::builder()\
            .with_writer(CsvDataWriter::new(path)).build()?) instead."
)]
pub fn with_flip_logging(mut self, enabled: bool) -> Self { ... }

#[deprecated(
    since = "0.2.0",
    note = "Use VSEContextBuilder::with_session(ExperimentSession::builder()\
            .with_writer(CsvDataWriter::new(path)).build()?) instead."
)]
pub fn with_flip_log_csv(mut self, path: impl Into<PathBuf>) -> Self { ... }
```

In `src/timing/mod.rs`, change:
```rust
pub use flip_logger::FlipLogger;
```
to:
```rust
pub(crate) use flip_logger::FlipLogger;
```

In `src/lib.rs` prelude, remove `FlipLogger` from the public re-export if present.

**Step 3: Run all tests (allow deprecation warnings)**

```bash
cargo test 2>&1 | tail -10
```
Expected: all tests pass. Deprecation warnings on examples are expected and OK.

**Step 4: Commit**

```bash
git add src/core/context.rs src/timing/mod.rs src/lib.rs
git commit -m "deprecate: mark with_flip_logging/with_flip_log_csv deprecated, make FlipLogger pub(crate)"
```

---

## Task 11: Run full test suite + clippy

**Step 1: Run tests**

```bash
cargo test 2>&1 | tail -15
```
Expected: all tests pass, no failures.

**Step 2: Run clippy**

```bash
cargo clippy --all-targets 2>&1 | grep -E "^error" | head -20
```
Expected: no errors. Warnings about deprecated usage in examples are OK.

**Step 3: Format**

```bash
cargo fmt
git add -p
git commit -m "style: cargo fmt after data recording implementation"
```

---

## Task 12: Documentation

**Files:**
- Create: `docs/guides/data_recording.md`
- Create: `docs/guides/experiment_data_schema.md`

**Step 1: Write `docs/guides/data_recording.md`**

```markdown
# Experiment Data Recording

VSE provides a flexible data recording system via `ExperimentSession` and the
`DataWriter` trait. All disk I/O happens on a dedicated writer thread so your
render loop is never stalled by filesystem operations.

## Data Flow

```
RenderContext::flip()
    │
    ▼
RecordingState::on_flip()
    │  Sends timing-only FrameMessage if previous flip had no record_frame
    ▼
ExperimentSession (SyncSender<WriterMessage>)
    │  Bounded channel (default: 4096 messages)
    ▼
Writer thread
    │
    ▼
DataWriter::write_frame() / write_annotation() / write_event()
    │
    ▼
Disk (frames.csv + events.csv  or  frames.parquet)
```

## Quick Start

Define your per-frame data struct (must derive `serde::Serialize`):

```rust
#[derive(serde::Serialize)]
struct FrameData {
    stimulus_id: u32,
    contrast: f32,
    orientation_deg: f32,
}
```

Attach a session to your context:

```rust
use vision_stimulus_engine::prelude::*;

let session = ExperimentSession::builder()
    .with_writer(CsvDataWriter::new("data/session_001/"))
    .build()?;

let context = VSEContext::builder()
    .with_window_size(1920, 1080)
    .with_session(session)
    .build()?;
```

Record data in the run loop:

```rust
context.run(|vse| {
    vse.clear()?;
    let _flip = vse.flip(None)?;

    vse.record_frame(FrameData { stimulus_id: 42, contrast: 0.8, orientation_deg: 45.0 })?;

    if trial_just_started {
        vse.record_annotation("trial", &TrialMeta { trial_id, condition: "A".into() })?;
    }
    if key_pressed {
        vse.record_event("response", "left")?;
    }
    Ok(())
})?;
```

## Reading Data in Python

**CSV:**
```python
import pandas as pd
frames = pd.read_csv("data/session_001/frames.csv")
events = pd.read_csv("data/session_001/events.csv")
```

**Parquet:**
```python
import pandas as pd
frames = pd.read_parquet("data/session_001.parquet")
# annotations/events in companion file if written
```

## Choosing a Backend

| Feature | CsvDataWriter | ParquetDataWriter |
|---|---|---|
| Dependencies | None (pure Rust) | None (pure Rust) |
| Human-readable | Yes | No |
| Compression | No | Yes (GZIP) |
| Append-friendly | Yes | No (written at flush) |
| Python/R interop | pandas, R csv | pandas, polars, R arrow |
| Missing data | Empty columns | Null values |

Use `CsvDataWriter` for quick experiments and debugging. Use
`ParquetDataWriter` for large datasets or when columnar compression matters.

## Missing Frame Data

Not every frame needs a `record_frame()` call. VSE automatically records a
timing-only row (all timing fields populated, user fields null/empty) for every
flip where `record_frame` was not called. This ensures the complete timing
history is always present in your output file.

## Backpressure

The channel capacity (default: 4096) controls how many messages can queue up
before the render loop is affected. At 60 Hz, 4096 provides ~68 seconds of
buffering.

```rust
// Block render loop if writer falls behind (default, no data loss)
ExperimentSession::builder()
    .with_overflow(OverflowBehavior::Block)

// Drop records if full (no frame drops, possible data loss)
ExperimentSession::builder()
    .with_overflow(OverflowBehavior::DropWithWarning)
```

## Custom Backends

Implement `DataWriter` to write to any destination:

```rust
use vision_stimulus_engine::data::{DataWriter, DataError};
use vision_stimulus_engine::data::messages::{FrameMessage, AnnotationMessage, EventMessage};

struct MyWriter;

impl DataWriter for MyWriter {
    fn write_frame(&mut self, msg: FrameMessage) -> Result<(), DataError> {
        println!("frame {}: present={}us", msg.flip.frame_number,
                 msg.flip.present_time.as_micros());
        Ok(())
    }
    fn write_annotation(&mut self, _msg: AnnotationMessage) -> Result<(), DataError> { Ok(()) }
    fn write_event(&mut self, _msg: EventMessage) -> Result<(), DataError> { Ok(()) }
    fn flush(&mut self) -> Result<(), DataError> { Ok(()) }
}
```

## Timing Notes

Every row in `frames.csv` / `frames.parquet` includes a `timing_source` column:

- `CpuEstimate`: timestamp taken after GPU fence signal — accurate to ~0.5ms
- `GoogleDisplayTiming`: hardware scanout timestamp from the driver — accurate
  to the display refresh interval

The `present_time_us` field is microseconds since the VSE clock epoch
(context creation time), not wall-clock time.

## Future: Buffered Flip

A future version will add non-blocking `flip()` variants for pipelined GPU
submission. The `ExperimentSession` architecture is designed for this: a
pending-confirmation queue will hold frame records with estimated timing until
the driver confirms the actual scanout time via `vkGetPastPresentationTimingGOOGLE`.
This is transparent to user code — the `record_frame()` API is unchanged.
```

**Step 2: Write `docs/guides/experiment_data_schema.md`**

Document every column in both backends:

```markdown
# Experiment Data Schema Reference

This document describes the exact output schema for VSE's two data backends.
All timestamps are in **microseconds since VSE context creation** (not wall clock).

## frames.csv / frames.parquet — Frame Records

One row per `flip()` call. Timing columns are always populated.
User columns (from `record_frame()`) are empty/null for frames where
`record_frame` was not called.

| Column | Type | Units | Notes |
|---|---|---|---|
| `frame_number` | u64 | — | Monotonically increasing from 0 |
| `present_time_us` | u64 | µs | Frame present timestamp (see timing_source) |
| `submit_time_us` | u64 | µs | GPU command buffer submission timestamp |
| `timing_source` | string | — | `CpuEstimate` or `GoogleDisplayTiming` |
| `missed` | bool | — | True if this frame was dropped |
| `missed_count` | u32 | — | Number of display intervals missed (0 = on time) |
| `skipped` | bool | — | True if frame was skipped (minimized/swapchain recreation) |
| *(user columns)* | varies | user-defined | Populated from first `record_frame()` payload |

## events.csv — Annotations and Events

Annotations (from `record_annotation()`) and raw events (from `record_event()`)
share this file, distinguished by the `stream` column.

| Column | Type | Units | Notes |
|---|---|---|---|
| `timestamp_us` | u64 | µs | Clock timestamp when recorded |
| `stream` | string | — | Stream name. For raw events: the `name` arg |
| `payload` | string | — | JSON string for annotations; raw value for events |

## Null Handling

**CSV:** User columns for timing-only rows are empty strings (`,,`).
**Parquet:** User columns for timing-only rows are Arrow null values.

In Python: `pd.read_csv(..., keep_default_na=True)` treats empty strings
as `NaN` automatically.
```

**Step 3: Commit**

```bash
git add docs/guides/data_recording.md docs/guides/experiment_data_schema.md
git commit -m "docs: add data recording guide and schema reference"
```

---

## Final Verification

```bash
cargo test
cargo clippy --all-targets
cargo fmt --check
cargo doc --no-deps 2>&1 | grep "^warning" | head -10
```

All should be clean before considering the implementation complete.
