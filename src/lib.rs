//! VisionStimulusEngine (VSE)
//!
//! A vision science stimulus presentation system built on Vulkan,
//! designed for millisecond-accurate timing precision.
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

// Re-export commonly used types
pub mod prelude {
    pub use crate::core::{
        AcquisitionMethod, BufferedConfig, DeviceSelector, DisplayBackend, FlipEvent, Frame,
        GPUPreference, InputEvent, KeyCode, MonitorInfo, MonitorSelection, MouseButton, NamedKey,
        PresentMode, RenderContext, SwapchainConfig, SwapchainManager, VSEContext,
        VSEContextBuilder, VSEError, VideoModeInfo, WindowMode,
    };
    pub use crate::data::{
        CsvDataWriter, DataError, DataWriter, ExperimentSession, ExperimentSessionBuilder,
        OverflowBehavior, ParquetDataWriter,
    };
    pub use crate::drawing::{
        Color, GaborParams, GratingParams, NoiseParams, NoiseType, TextureHandle, WaveType,
    };
    pub use crate::host::HostInfo;
    pub use crate::timing::{
        CalibrationSample, FlipInfo, ScanoutTimestamp, Timestamp, TimingSource,
    };
}
