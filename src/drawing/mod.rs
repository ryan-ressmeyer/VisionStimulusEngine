//! Drawing primitives and texture management
//!
//! This module provides functions for drawing shapes, loading textures,
//! and generating vision science stimuli.

mod color;
pub(crate) mod font;
mod gabor;
mod model;
pub(crate) mod noise;
pub(crate) mod primitives;
pub(crate) mod renderer;
mod stimuli;
mod texture;
mod vertex;

pub use color::Color;
pub use gabor::GaborParams;
pub use model::{Bounds3D, ModelError, ModelHandle, ModelInfo, PerspectiveCamera};
pub use stimuli::{GratingParams, NoiseParams, NoiseType, WaveType};
pub use texture::TextureHandle;

pub use vertex::{DotInstance, TexturedVertex, Vertex2D, Vertex3D};
