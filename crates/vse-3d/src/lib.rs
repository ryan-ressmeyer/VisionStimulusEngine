//! Deterministic scientific 3D rendering for VisionStimulusEngine.
//!
//! `vse-3d` owns its Vulkan renderer and produces complete offscreen color
//! frames. Base VSE imports those frames, composites its lightweight 2D
//! overlays, and remains the sole timing and presentation authority.

mod model;
mod renderer;

pub use model::{Bounds3D, ModelError, ModelHandle, ModelInfo, PerspectiveCamera};
pub use renderer::{Vse3d, Vse3dConfig, Vse3dError, Vse3dInfo};
