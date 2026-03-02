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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::messages::{AnnotationMessage, EventMessage, FrameMessage};
    use crate::timing::{FlipInfo, Timestamp, TimingSource};

    struct NullWriter;
    impl DataWriter for NullWriter {
        fn write_frame(&mut self, _: FrameMessage) -> Result<(), DataError> {
            Ok(())
        }
        fn write_annotation(&mut self, _: AnnotationMessage) -> Result<(), DataError> {
            Ok(())
        }
        fn write_event(&mut self, _: EventMessage) -> Result<(), DataError> {
            Ok(())
        }
        fn flush(&mut self) -> Result<(), DataError> {
            Ok(())
        }
    }

    #[test]
    fn test_null_writer_implements_trait() {
        let w: Box<dyn DataWriter> = Box::new(NullWriter);
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
