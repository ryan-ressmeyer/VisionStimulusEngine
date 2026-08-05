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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use thiserror::Error;
use vulkano::device::Device;
use vulkano::format::Format;
use vulkano::memory::allocator::StandardMemoryAllocator;

use super::renderer::FrameRecorder;

/// A boxed cause carried by [`PipelineError`], preserving the source chain.
type Cause = Box<dyn std::error::Error + Send + Sync>;

/// Errors surfaced while building or recording a user [`StimulusPipeline`].
#[derive(Error, Debug)]
pub enum PipelineError {
    /// The pipeline's [`build`](StimulusPipeline::build) returned an error
    /// (shader load, pipeline creation, buffer allocation, ...).
    ///
    /// Construct with [`PipelineError::build`] so the underlying error stays
    /// reachable through [`Error::source`](std::error::Error::source).
    #[error("pipeline build failed")]
    Build(#[source] Cause),

    /// A registered-draw payload's concrete type did not match the pipeline's
    /// [`Command`](StimulusPipeline::Command) type. This should be impossible
    /// through the typed [`RegisteredPipeline`] handle; it guards the
    /// type-erasure boundary.
    #[error("registered-draw payload type does not match the pipeline's Command type")]
    PayloadTypeMismatch,

    /// A [`RegisteredPipeline`] handle was used with a VSE context other than
    /// the one that issued it. Pipeline ids are per-context, so honoring such a
    /// handle could dispatch to an unrelated pipeline.
    #[error(
        "RegisteredPipeline handle belongs to a different VSE context; \
         register the pipeline on the context you are drawing with"
    )]
    ForeignHandle,

    /// The pipeline's [`record`](StimulusPipeline::record) returned an error
    /// while recording draws for this frame.
    ///
    /// Construct with [`PipelineError::record`] so the underlying error stays
    /// reachable through [`Error::source`](std::error::Error::source).
    #[error("pipeline record failed")]
    Record(#[source] Cause),
}

impl PipelineError {
    /// Wrap a build-time failure, keeping `cause` reachable as the error source.
    ///
    /// ```
    /// # use vision_stimulus_engine::drawing::PipelineError;
    /// # fn load_shader() -> Result<(), std::io::Error> { Ok(()) }
    /// load_shader().map_err(PipelineError::build).unwrap();
    /// ```
    pub fn build(cause: impl Into<Cause>) -> Self {
        Self::Build(cause.into())
    }

    /// Wrap a record-time failure, keeping `cause` reachable as the error source.
    pub fn record(cause: impl Into<Cause>) -> Self {
        Self::Record(cause.into())
    }
}

/// Identifies one renderer's pipeline registry.
///
/// Handed to every [`RegisteredPipeline`] a registry issues, so a handle can be
/// checked against the context it is used with. Without it, ids are just small
/// integers and a handle from another context would resolve to whichever
/// pipeline happened to occupy that slot — drawing the wrong stimulus silently
/// whenever the `Command` types happened to match.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct RegistryId(u64);

impl RegistryId {
    /// Allocate a process-unique registry id.
    pub(crate) fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// Context handed to [`StimulusPipeline::build`] to construct pipeline(s).
///
/// Fields are private and reached through accessor methods so the set can grow
/// without breaking existing `build` implementations. Holds the device, the
/// target color format, a supported depth format for advanced resource setup,
/// and the shared memory allocator for immutable buffers uploaded at build.
pub struct PipelineBuildCtx<'a> {
    pub(crate) device: &'a Arc<Device>,
    pub(crate) color_format: Format,
    pub(crate) depth_format: Format,
    pub(crate) memory_allocator: &'a Arc<StandardMemoryAllocator>,
}

impl<'a> PipelineBuildCtx<'a> {
    /// Construct a build context directly.
    ///
    /// VSE builds one for you during registration; this is for exercising a
    /// [`StimulusPipeline::build`] against your own device in a test, without
    /// standing up a full VSE session.
    pub fn new(
        device: &'a Arc<Device>,
        color_format: Format,
        depth_format: Format,
        memory_allocator: &'a Arc<StandardMemoryAllocator>,
    ) -> Self {
        Self {
            device,
            color_format,
            depth_format,
            memory_allocator,
        }
    }

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

    /// A depth-attachment format supported by the selected device. VSE's 2D
    /// pass has no depth attachment; this value is retained for compatibility
    /// and for advanced callers that prepare their own offscreen resources.
    pub fn depth_format(&self) -> Format {
        self.depth_format
    }

    /// VSE's shared device memory allocator, for any immutable vertex/index
    /// buffers the pipeline uploads once at build time.
    pub fn memory_allocator(&self) -> &Arc<StandardMemoryAllocator> {
        self.memory_allocator
    }
}

/// Per-frame context handed to [`StimulusPipeline::record`] and to a
/// [`draw_custom`] closure, alongside the raw [`FrameRecorder`].
///
/// Fields are private and reached through accessor methods so the set can grow
/// without breaking existing implementations.
///
/// [`draw_custom`]: crate::prelude::RenderContext::draw_custom
pub struct RecordCtx {
    pub(crate) viewport_extent: [u32; 2],
    pub(crate) memory_allocator: Arc<StandardMemoryAllocator>,
    pub(crate) device: Arc<Device>,
}

impl RecordCtx {
    /// VSE's Vulkan device.
    pub fn device(&self) -> &Arc<Device> {
        &self.device
    }

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

    /// The GPU state [`build`](Self::build) produces — typically the
    /// [`GraphicsPipeline`] plus any buffers created once alongside it.
    ///
    /// Keeping this separate from `Self` is what lets `record` receive fully
    /// built resources. A pipeline that stored its own `Option<Arc<..>>` would
    /// have to unwrap it on every frame to handle a state that cannot occur:
    /// registration builds before any draw can be enqueued.
    ///
    /// [`GraphicsPipeline`]: vulkano::pipeline::GraphicsPipeline
    type Resources: 'static;

    /// Build GPU resources once, at registration. Never on the presentation
    /// path. Called exactly once, by
    /// [`register_pipeline`](crate::prelude::RenderContext::register_pipeline).
    ///
    /// `self` carries whatever configuration the pipeline was constructed with;
    /// the returned `Resources` carry everything `record` needs on the GPU.
    fn build(&self, cx: &PipelineBuildCtx) -> Result<Self::Resources, PipelineError>;

    /// Record draws for this frame's queued commands into the active render
    /// pass, via the raw [`FrameRecorder`].
    ///
    /// `commands` holds every consecutive [`draw_with`] call made against this
    /// pipeline, in call order — a run of N consecutive draws arrives as one
    /// slice of N, so a pipeline can issue a single instanced draw for the run.
    /// A draw of a different pipeline (or any built-in `draw_*`) between two of
    /// your calls splits the run, preserving call-order compositing.
    ///
    /// `&mut self` and `&mut Self::Resources`, so a pipeline may cache across
    /// frames — a reused staging buffer, a memoized descriptor set — without
    /// reaching for interior mutability.
    ///
    /// The recorder runs inside VSE's already-active 2D pass with the viewport
    /// set; do not begin/end the pass or transition the target image.
    ///
    /// [`draw_with`]: crate::prelude::RenderContext::draw_with
    fn record(
        &mut self,
        gpu: &mut Self::Resources,
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
    registry: RegistryId,
    id: u64,
    _marker: PhantomData<fn(C)>,
}

impl<C> RegisteredPipeline<C> {
    pub(crate) fn new(registry: RegistryId, id: u64) -> Self {
        Self {
            registry,
            id,
            _marker: PhantomData,
        }
    }

    /// This pipeline's slot within its registry.
    pub(crate) fn id(self) -> u64 {
        self.id
    }

    /// Whether this handle was issued by `registry`. Checked before every
    /// dispatch so a handle from another VSE context is rejected rather than
    /// silently resolving to an unrelated pipeline.
    pub(crate) fn belongs_to(self, registry: RegistryId) -> bool {
        self.registry == registry
    }
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
    /// Record one run of consecutive draws for this pipeline. `payloads` are
    /// the run's type-erased commands, in call order.
    fn record_erased(
        &mut self,
        recorder: &mut FrameRecorder,
        cx: &RecordCtx,
        payloads: Vec<Box<dyn Any>>,
    ) -> Result<(), PipelineError>;
}

/// A registered pipeline paired with the resources its `build` produced.
///
/// Storing them together is what keeps `Resources` out of the user's own
/// struct, so `record` never has to unwrap a "not built yet" state.
pub(crate) struct RegisteredEntry<P: StimulusPipeline> {
    pub(crate) pipeline: P,
    pub(crate) resources: P::Resources,
}

impl<P: StimulusPipeline> ErasedStimulusPipeline for RegisteredEntry<P> {
    fn record_erased(
        &mut self,
        recorder: &mut FrameRecorder,
        cx: &RecordCtx,
        payloads: Vec<Box<dyn Any>>,
    ) -> Result<(), PipelineError> {
        let commands = downcast_payloads::<P::Command>(payloads)?;
        self.pipeline
            .record(&mut self.resources, recorder, cx, &commands)
    }
}

/// Recover a run's concrete `Command`s from their type-erased payloads.
///
/// This is the sole type-erasure boundary: the queue holds payloads as
/// `Box<dyn Any>` so it can stay homogeneous across pipelines, and each
/// dispatch downcasts the whole run back to the pipeline's `Command` before
/// calling the typed `record`. Commands are moved out of their boxes, so
/// `record` receives a contiguous slice it can hand straight to an instanced
/// draw.
///
/// A type mismatch (impossible through the typed handle) fails the entire run
/// with [`PipelineError::PayloadTypeMismatch`] rather than recording part of it.
fn downcast_payloads<C: 'static>(payloads: Vec<Box<dyn Any>>) -> Result<Vec<C>, PipelineError> {
    payloads
        .into_iter()
        .map(|payload| {
            payload
                .downcast::<C>()
                .map(|command| *command)
                .map_err(|_| PipelineError::PayloadTypeMismatch)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct MyCommand {
        phase: f32,
        tint: [f32; 4],
    }

    fn cmd(phase: f32) -> MyCommand {
        MyCommand {
            phase,
            tint: [1.0, 0.0, 0.5, 1.0],
        }
    }

    #[test]
    fn payloads_are_recovered_as_concrete_commands_in_call_order() {
        // A run of consecutive `draw_with` calls to one pipeline is handed to
        // `record` as a single slice. Order within the slice is the order the
        // draws were issued — a pipeline reading `commands[i]` is reading the
        // i-th call, not an arbitrary permutation.
        let payloads: Vec<Box<dyn Any>> =
            vec![Box::new(cmd(0.0)), Box::new(cmd(0.25)), Box::new(cmd(0.5))];

        let recovered =
            downcast_payloads::<MyCommand>(payloads).expect("correctly-typed payloads downcast");

        assert_eq!(recovered, vec![cmd(0.0), cmd(0.25), cmd(0.5)]);
    }

    #[test]
    fn a_single_mismatched_payload_rejects_the_whole_run() {
        // Type erasure is the one place a wrong-typed command could reach a
        // pipeline. It must fail loudly rather than record a partial run or
        // reinterpret bytes.
        let payloads: Vec<Box<dyn Any>> = vec![Box::new(cmd(0.0)), Box::new(99u64)];

        let err = downcast_payloads::<MyCommand>(payloads)
            .expect_err("a mismatched payload must not downcast");

        assert!(matches!(err, PipelineError::PayloadTypeMismatch));
    }

    #[test]
    fn an_empty_run_recovers_no_commands() {
        let recovered = downcast_payloads::<MyCommand>(Vec::new()).expect("an empty run is valid");
        assert!(recovered.is_empty());
    }

    // --- Handle provenance ---
    //
    // `RegisteredPipeline` ids are per-renderer. A handle from one VSE context
    // used against another would otherwise resolve to whatever pipeline happens
    // to hold that id — silently drawing the wrong stimulus when the `Command`
    // types coincide.

    #[test]
    fn a_handle_is_accepted_only_by_the_registry_that_issued_it() {
        let registry_a = RegistryId::next();
        let registry_b = RegistryId::next();
        assert_ne!(registry_a, registry_b, "each registry gets a fresh id");

        let handle: RegisteredPipeline<MyCommand> = RegisteredPipeline::new(registry_a, 0);

        assert!(handle.belongs_to(registry_a));
        assert!(
            !handle.belongs_to(registry_b),
            "a handle must not resolve against a different context's registry"
        );
    }

    #[test]
    fn handles_from_the_same_registry_are_distinguished_by_id() {
        let registry = RegistryId::next();
        let first: RegisteredPipeline<MyCommand> = RegisteredPipeline::new(registry, 0);
        let second: RegisteredPipeline<MyCommand> = RegisteredPipeline::new(registry, 1);

        assert!(first.belongs_to(registry));
        assert!(second.belongs_to(registry));
        assert_ne!(first.id(), second.id());
    }

    // --- Error source chain ---

    #[test]
    fn build_errors_keep_the_underlying_cause_reachable() {
        // A user's `build` wraps a vulkano error. Flattening it to a string
        // would drop the chain that tells them which shader or allocation
        // actually failed.
        let cause = std::io::Error::other("shader module load failed");
        let err = PipelineError::build(cause);

        let source = std::error::Error::source(&err).expect("the cause must stay reachable");
        assert_eq!(source.to_string(), "shader module load failed");
        assert!(err.to_string().contains("pipeline build failed"));
    }

    #[test]
    fn record_errors_keep_the_underlying_cause_reachable() {
        let cause = std::io::Error::other("descriptor set exhausted");
        let err = PipelineError::record(cause);

        let source = std::error::Error::source(&err).expect("the cause must stay reachable");
        assert_eq!(source.to_string(), "descriptor set exhausted");
        assert!(err.to_string().contains("pipeline record failed"));
    }
}
