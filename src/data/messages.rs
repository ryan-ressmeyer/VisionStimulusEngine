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
#[allow(dead_code)] // Flush reserved for future flush_data() API
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
    use super::*;
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
            WriterMessage::Frame(FrameMessage {
                flip: make_flip(0),
                payload: None,
                schema_name: "",
            }),
            WriterMessage::Flush,
            WriterMessage::Shutdown,
        ];
        assert_eq!(msgs.len(), 3);
    }
}
