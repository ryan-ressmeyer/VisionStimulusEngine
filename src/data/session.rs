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
#[derive(Debug)]
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
            OverflowBehavior::Block => self
                .tx
                .send(msg)
                .map_err(|_| DataError::ChannelDisconnected),
            OverflowBehavior::DropWithWarning => match self.tx.try_send(msg) {
                Ok(()) => Ok(()),
                Err(mpsc::TrySendError::Full(_)) => {
                    warn!("ExperimentSession: channel full — dropping record (DropWithWarning)");
                    Ok(())
                }
                Err(mpsc::TrySendError::Disconnected(_)) => Err(DataError::ChannelDisconnected),
            },
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
            DataError::Backend(
                "ExperimentSessionBuilder: no writer set (call .with_writer())".into(),
            )
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
            missed: false,
            missed_count: 0,
            skipped: false,
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

        session
            .send_frame(FrameMessage {
                flip: make_flip(0),
                payload: None,
                schema_name: "",
            })
            .unwrap();

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
        assert!(matches!(
            builder.overflow,
            OverflowBehavior::DropWithWarning
        ));
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
                flip: make_flip(i),
                payload: None,
                schema_name: "",
            });
        }
        drop(session);
        std::fs::remove_dir_all(&dir).ok();
    }
}
