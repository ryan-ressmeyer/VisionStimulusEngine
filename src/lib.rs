//! VisionStimulusEngine (VSE)
//!
//! A vision science stimulus presentation system built on Vulkan,
//! designed to request, measure, and record timing evidence for visual stimuli.
//!
//! # Quick Start
//!
//! ```no_run
//! use vision_stimulus_engine::prelude::*;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let context = VSEContext::builder()
//!     .with_window_size(800, 600)
//!     .with_title("My Experiment")
//!     .build()?;
//! # Ok(())
//! # }
//! ```

// Re-export core types for easy access
pub mod core;
pub mod data;
pub mod drawing;
pub mod host;
pub mod timing;

/// External-renderer handoff wire types (ring descriptors, slot state machine,
/// format negotiation), re-exported so consumers name one crate. See
/// `core::external_frame` for the VSE-side seam.
pub use vse_external_frame as external_frame;

/// Common types for writing experiments.
///
/// Advanced APIs remain available from their domain modules rather than this
/// curated namespace. For example:
///
/// ```
/// use vision_stimulus_engine::drawing::StimulusPipeline;
/// # fn accepts_pipeline<P: StimulusPipeline>() {}
/// ```
///
/// Old convenience aliases are intentionally unavailable:
///
/// ```compile_fail
/// use vision_stimulus_engine::prelude::RenderContext;
/// fn stop(ctx: &mut RenderContext<'_>) {
///     ctx.close();
/// }
/// ```
///
/// ```compile_fail
/// use vision_stimulus_engine::prelude::{Color, RenderContext};
/// fn set_background(ctx: &mut RenderContext<'_>) {
///     ctx.set_clear(Color::BLACK);
/// }
/// ```
///
/// Advanced APIs must use their canonical module paths:
///
/// ```compile_fail
/// use vision_stimulus_engine::prelude::StimulusPipeline;
/// ```
pub mod prelude {
    pub use crate::core::{
        BufferedConfig, BufferedFrame, GPUPreference, HeadlessContext, KeyCode, MonitorSelection,
        MouseButton, PresentMode, RenderContext, VSEContext, VSEError, WindowMode,
    };
    pub use crate::data::{CsvDataWriter, ExperimentSession, OverflowBehavior, ParquetDataWriter};
    pub use crate::drawing::{
        Color, GaborParams, GratingParams, NoiseParams, NoiseType, TextureHandle, WaveType,
    };
    pub use crate::host::HostInfo;
    pub use crate::timing::{FlipInfo, ScanoutTimestamp, Timestamp, TimingSource};
}
