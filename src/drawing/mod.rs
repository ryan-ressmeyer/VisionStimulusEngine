//! Drawing primitives and texture management
//!
//! This module provides functions for drawing shapes, loading textures,
//! and generating vision science stimuli.

mod color;
mod gabor;
pub(crate) mod primitives;
pub(crate) mod renderer;
mod texture;
mod vertex;

pub use color::Color;
pub use gabor::GaborParams;
pub use texture::TextureHandle;

pub use vertex::{TexturedVertex, Vertex2D};
