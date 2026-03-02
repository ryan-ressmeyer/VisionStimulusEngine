use crate::timing::{FlipInfo, Timestamp};
pub struct FrameMessage {
    pub flip: FlipInfo,
    pub payload: Option<Vec<u8>>,
    pub schema_name: &'static str,
}
pub struct AnnotationMessage {
    pub stream: String,
    pub timestamp: Timestamp,
    pub payload: Vec<u8>,
}
pub struct EventMessage {
    pub name: String,
    pub timestamp: Timestamp,
    pub value: String,
}
pub(crate) enum WriterMessage {
    Frame(FrameMessage),
    Annotation(AnnotationMessage),
    Event(EventMessage),
    Flush,
    Shutdown,
}
