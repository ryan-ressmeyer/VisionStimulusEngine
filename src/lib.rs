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

// Re-export commonly used types
pub mod prelude {
    pub use crate::core::{
        DeviceSelector, Frame, GPUPreference, PresentMode, RenderContext, SwapchainConfig,
        SwapchainManager, VSEContext, VSEContextBuilder, VSEError,
    };
}
