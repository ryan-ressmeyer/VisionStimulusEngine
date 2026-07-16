//! VSEContext - Top-level VisionStimulusEngine environment
//!
//! This module provides the main entry point for VSE, managing all Vulkan
//! resources and providing a clean API for rendering operations.

use winit::event_loop::EventLoop;

pub use super::config::{VSEConfig, VSEContextBuilder, VSEError};
pub use super::render_context::RenderContext;

/// Main VisionStimulusEngine context
///
/// This is the primary interface for creating windows and managing
/// the rendering environment. Use the builder pattern to configure
/// the context before running.
///
/// # Example
///
/// ```no_run
/// use vision_stimulus_engine::prelude::*;
///
/// fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let context = VSEContext::builder()
///         .with_window_size(800, 600)
///         .with_title("VSE Example")
///         .with_clear_color(0.5, 0.5, 0.5, 1.0)
///         .build()?;
///
///     context.run(|vse| {
///         vse.clear()?;
///         let _info = vse.flip(None)?;
///         Ok(())
///     })?;
///
///     Ok(())
/// }
/// ```
pub struct VSEContext {
    pub(super) config: VSEConfig,
    pub(super) session: Option<crate::data::ExperimentSession>,
    pub(super) event_loop: Option<EventLoop<()>>,
}

impl VSEContext {
    /// Create a new VSE context with default settings
    ///
    /// For more control over initialization, use [`VSEContext::builder()`].
    ///
    /// # Errors
    ///
    /// Returns `VSEError` if initialization fails.
    pub fn new() -> Result<Self, VSEError> {
        Self::builder().build()
    }

    /// Create a builder for custom configuration
    pub fn builder() -> VSEContextBuilder {
        VSEContextBuilder::new()
    }
}
