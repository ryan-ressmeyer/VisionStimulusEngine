//! Tier 1 user-defined rendering pipelines.
//!
//! A user implements [`StimulusPipeline`] to build one-or-more graphics
//! pipelines once (at startup / between trials) and record draws for the
//! commands they enqueue with
//! [`RenderContext::draw_with`](crate::prelude::RenderContext::draw_with).
//! Registration returns a typed [`RegisteredPipeline`] handle; each `draw_with`
//! enqueues one ordered entry that interleaves with the built-in `draw_*` calls
//! (see `docs/design/pipeline-flexibility.md` §3, Tier 1).
//!
//! The registry stores pipelines type-erased (as [`ErasedStimulusPipeline`]) so
//! it is homogeneous, while `record` stays generic over the user's concrete
//! `Command` type. The only erasure boundary is the [`downcast_payload`] call
//! that recovers the concrete command from a `Box<dyn Any>` payload.

use std::any::Any;
use std::marker::PhantomData;
use std::sync::Arc;

use thiserror::Error;
use vulkano::device::Device;
use vulkano::format::Format;
use vulkano::memory::allocator::StandardMemoryAllocator;

use super::renderer::FrameRecorder;

/// Errors surfaced while building or recording a user [`StimulusPipeline`].
#[derive(Error, Debug)]
pub enum PipelineError {
    /// The pipeline's [`build`](StimulusPipeline::build) returned an error
    /// (shader load, pipeline creation, buffer allocation, ...).
    #[error("pipeline build failed: {0}")]
    Build(String),

    /// A registered-draw payload's concrete type did not match the pipeline's
    /// [`Command`](StimulusPipeline::Command) type. This should be impossible
    /// through the typed [`RegisteredPipeline`] handle; it guards the
    /// type-erasure boundary.
    #[error("registered-draw payload type does not match the pipeline's Command type")]
    PayloadTypeMismatch,

    /// The pipeline's [`record`](StimulusPipeline::record) returned an error
    /// while recording draws for this frame.
    #[error("pipeline record failed: {0}")]
    Record(String),
}

/// Context handed to [`StimulusPipeline::build`] to construct pipeline(s).
///
/// Fields are private and reached through accessor methods so the set can grow
/// without breaking existing `build` implementations. Holds the device, the
/// swapchain color format and depth format VSE renders into, and the shared
/// memory allocator for any immutable buffers the pipeline uploads at build.
pub struct PipelineBuildCtx<'a> {
    pub(crate) device: &'a Arc<Device>,
    pub(crate) color_format: Format,
    pub(crate) depth_format: Format,
    pub(crate) memory_allocator: &'a Arc<StandardMemoryAllocator>,
}

impl<'a> PipelineBuildCtx<'a> {
    /// VSE's Vulkan device; build your [`GraphicsPipeline`] on it.
    ///
    /// [`GraphicsPipeline`]: vulkano::pipeline::GraphicsPipeline
    pub fn device(&self) -> &Arc<Device> {
        self.device
    }

    /// The swapchain color-attachment format VSE's 2D pass renders into. Your
    /// pipeline's `PipelineRenderingCreateInfo` color format must match this.
    pub fn color_format(&self) -> Format {
        self.color_format
    }

    /// The depth-attachment format VSE uses for its native-3D pass. VSE's 2D
    /// pass has no depth attachment; this is exposed for pipelines that opt
    /// into a depth format.
    pub fn depth_format(&self) -> Format {
        self.depth_format
    }

    /// VSE's shared device memory allocator, for any immutable vertex/index
    /// buffers the pipeline uploads once at build time.
    pub fn memory_allocator(&self) -> &Arc<StandardMemoryAllocator> {
        self.memory_allocator
    }
}

/// Per-frame context handed to [`StimulusPipeline::record`] alongside the raw
/// [`FrameRecorder`].
///
/// Fields are private and reached through accessor methods so the set can grow
/// without breaking existing `record` implementations.
pub struct RecordCtx {
    pub(crate) viewport_extent: [u32; 2],
    pub(crate) memory_allocator: Arc<StandardMemoryAllocator>,
}

impl RecordCtx {
    /// The framebuffer extent (in pixels) for this frame. VSE has already set
    /// the dynamic viewport to cover this extent, so a pipeline using dynamic
    /// viewport state need not set it.
    pub fn viewport_extent(&self) -> [u32; 2] {
        self.viewport_extent
    }

    /// VSE's shared device memory allocator, for any per-frame vertex buffers
    /// the pipeline uploads while recording.
    pub fn memory_allocator(&self) -> &Arc<StandardMemoryAllocator> {
        &self.memory_allocator
    }
}

/// A user-implemented rendering pipeline (Tier 1).
///
/// Implement this to teach VSE a new stimulus: [`build`](Self::build) compiles
/// your own [`GraphicsPipeline`]s once (never on the presentation path), and
/// [`record`](Self::record) records draws for the commands you enqueued this
/// frame. Register an instance with
/// [`RenderContext::register_pipeline`](crate::prelude::RenderContext::register_pipeline)
/// and enqueue commands with
/// [`RenderContext::draw_with`](crate::prelude::RenderContext::draw_with).
///
/// [`GraphicsPipeline`]: vulkano::pipeline::GraphicsPipeline
pub trait StimulusPipeline: 'static {
    /// The user's per-draw parameter type, kept concrete inside `record`.
    type Command: 'static;

    /// Build pipeline(s) once, at startup or between trials. Never on the hot
    /// path. Called exactly once, during registration.
    fn build(&mut self, cx: &PipelineBuildCtx) -> Result<(), PipelineError>;

    /// Record draws for this frame's queued commands into the active render
    /// pass, via the raw [`FrameRecorder`].
    ///
    /// `commands` is the slice of commands enqueued for this pipeline (currently
    /// always length 1 per `draw_with`; the slice shape reserves room for a
    /// future consecutive-same-pipeline coalescing optimization). The recorder
    /// runs inside VSE's already-active 2D pass with the viewport set; do not
    /// begin/end the pass or transition the target image.
    fn record(
        &self,
        recorder: &mut FrameRecorder,
        cx: &RecordCtx,
        commands: &[Self::Command],
    ) -> Result<(), PipelineError>;
}

/// A typed handle to a registered [`StimulusPipeline`], returned by
/// [`RenderContext::register_pipeline`](crate::prelude::RenderContext::register_pipeline).
///
/// `Copy`, so it can be stashed and reused across frames. The `C` type parameter
/// ties the handle to the pipeline's [`Command`](StimulusPipeline::Command) type
/// so [`draw_with`](crate::prelude::RenderContext::draw_with) only accepts the
/// right payload.
pub struct RegisteredPipeline<C> {
    pub(crate) id: u64,
    pub(crate) _marker: PhantomData<fn(C)>,
}

// Manual `Copy`/`Clone` so the handle is `Copy` regardless of whether `C` is
// (the handle carries only an id; `C` appears solely in `PhantomData<fn(C)>`).
impl<C> Clone for RegisteredPipeline<C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C> Copy for RegisteredPipeline<C> {}

/// Type-erased view of a [`StimulusPipeline`] for the homogeneous registry.
///
/// The blanket impl recovers the concrete `Command` from the erased payload and
/// dispatches to the typed [`StimulusPipeline::record`], keeping the user's
/// params concrete inside `record` while the registry stores `Box<dyn
/// ErasedStimulusPipeline>`.
pub(crate) trait ErasedStimulusPipeline {
    fn record_erased(
        &self,
        recorder: &mut FrameRecorder,
        cx: &RecordCtx,
        payload: &dyn Any,
    ) -> Result<(), PipelineError>;
}

impl<P: StimulusPipeline> ErasedStimulusPipeline for P {
    fn record_erased(
        &self,
        recorder: &mut FrameRecorder,
        cx: &RecordCtx,
        payload: &dyn Any,
    ) -> Result<(), PipelineError> {
        let command = downcast_payload::<P::Command>(payload)?;
        self.record(recorder, cx, std::slice::from_ref(command))
    }
}

/// Recover a registered-draw's concrete `Command` from its type-erased payload.
///
/// This is the sole type-erasure boundary: the registry enqueues payloads as
/// `Box<dyn Any>`, and each dispatch downcasts back to the pipeline's `Command`
/// before calling the typed `record`. A type mismatch (impossible through the
/// typed handle) surfaces [`PipelineError::PayloadTypeMismatch`].
fn downcast_payload<C: 'static>(payload: &dyn Any) -> Result<&C, PipelineError> {
    payload
        .downcast_ref::<C>()
        .ok_or(PipelineError::PayloadTypeMismatch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downcast_payload_recovers_the_concrete_command() {
        // The registry stores payloads type-erased as `Box<dyn Any>`; the
        // erasure boundary must recover the pipeline's concrete `Command` so
        // `record` sees exactly the value that was enqueued.
        #[derive(Debug, PartialEq)]
        struct MyCommand {
            phase: f32,
            tint: [f32; 4],
        }
        let command = MyCommand {
            phase: 0.25,
            tint: [1.0, 0.0, 0.5, 1.0],
        };
        let payload: Box<dyn std::any::Any> = Box::new(MyCommand {
            phase: 0.25,
            tint: [1.0, 0.0, 0.5, 1.0],
        });
        let recovered = downcast_payload::<MyCommand>(&*payload)
            .expect("correctly-typed payload must downcast");
        assert_eq!(*recovered, command);
    }

    #[test]
    fn downcast_payload_rejects_a_mismatched_type() {
        // A payload whose concrete type differs from the pipeline's `Command`
        // must surface `PayloadTypeMismatch`, never a wrong-typed reference.
        #[derive(Debug)]
        struct Expected(#[allow(dead_code)] u32);
        let payload: Box<dyn std::any::Any> = Box::new(99u64);
        let err = downcast_payload::<Expected>(&*payload)
            .expect_err("mismatched payload must not downcast");
        assert!(matches!(err, PipelineError::PayloadTypeMismatch));
    }
}
