use super::*;

impl<'a> RenderContext<'a> {
    /// Record raw Vulkan draws into VSE's active render pass this frame
    /// (Tier 2 "raw record hook" — the low-level escape for advanced users; see
    /// `docs/design/pipeline-flexibility.md` §3).
    ///
    /// The closure is invoked once, during this frame's [`flip`](Self::flip),
    /// with a `&mut` [`FrameRecorder`] (the exact vulkano
    /// `AutoCommandBufferBuilder` VSE records into) and a
    /// [`RecordCtx`]. Queue as many as you like; each runs in the order
    /// it was queued. The queue is drained every frame, so a frame with no
    /// `draw_custom` call behaves exactly as before.
    ///
    /// # Where and how the closure runs — the contract
    ///
    /// - It executes **inside an already-active dynamic-rendering pass**, at
    ///   the point in call order where you queued it. A custom draw composites
    ///   over whatever was drawn before it and under whatever is drawn after —
    ///   the same rule as every built-in `draw_*`.
    /// - The color attachment is the swapchain image
    ///   (format = [`swapchain().format()`](SwapchainManager::format)); there is
    ///   **no depth attachment** in this 2D pass.
    /// - The **viewport is already set** to the full framebuffer
    ///   ([`RecordCtx::viewport_extent`]); a pipeline built with dynamic
    ///   viewport state needs no `set_viewport` of its own.
    ///
    /// The closure **must not** call `begin_rendering` / `end_rendering`, end the
    /// pass, or transition the target image's layout — VSE owns those. Do:
    /// bind a [`GraphicsPipeline`](vulkano::pipeline::GraphicsPipeline) you built
    /// once at setup (compatible with the swapchain color format, no depth), set
    /// push constants, bind vertex/index/instance buffers, and issue draws.
    ///
    /// Build pipelines and buffers **at setup / between trials, never here on the
    /// presentation path**, using the exposed [`device()`](Self::device),
    /// [`swapchain().format()`](SwapchainManager::format), and
    /// [`memory_allocator()`](Self::memory_allocator).
    ///
    /// Recording errors from the builder are the caller's to handle (the vulkano
    /// calls return `Result`); this hook does not surface them.
    pub fn draw_custom(&mut self, record: impl FnOnce(&mut FrameRecorder, &RecordCtx) + 'static) {
        self.state.renderer.push_custom(Box::new(record));
    }

    /// Register a user-defined Tier 1 [`StimulusPipeline`] and get a typed
    /// handle to enqueue draws with (see `docs/design/pipeline-flexibility.md`
    /// §3, Tier 1).
    ///
    /// Builds the pipeline **once**, here — call at setup or between trials,
    /// never on the presentation path. The pipeline's
    /// [`build`](StimulusPipeline::build) receives a
    /// [`PipelineBuildCtx`](crate::drawing::PipelineBuildCtx) carrying VSE's
    /// device, the swapchain color format, the depth format, and the memory
    /// allocator. The returned [`RegisteredPipeline`] is `Copy`; stash it and
    /// pass it to [`draw_with`](Self::draw_with) each frame.
    ///
    /// # Errors
    ///
    /// [`VSEError::Pipeline`] if the pipeline's `build` fails.
    pub fn register_pipeline<P: StimulusPipeline>(
        &mut self,
        pipeline: P,
    ) -> Result<RegisteredPipeline<P::Command>, VSEError> {
        let color_format = self.color_format();
        Ok(self.state.renderer.register(pipeline, color_format)?)
    }

    /// Drop a registered pipeline and release its GPU resources.
    ///
    /// Returns whether a pipeline was removed — `false` for an already-dropped
    /// handle, or one issued by a different VSE context. Call this when
    /// re-registering between trials; without it each registration leaks its
    /// `GraphicsPipeline` for the life of the session.
    pub fn unregister_pipeline<C: 'static>(&mut self, handle: RegisteredPipeline<C>) -> bool {
        self.state.renderer.unregister(handle)
    }

    /// Enqueue one draw for a registered Tier 1 pipeline.
    ///
    /// The `command` is this draw's parameters (the pipeline's
    /// [`Command`](StimulusPipeline::Command) type). It records in call order,
    /// interleaved with the built-in `draw_*` calls issued around it, when the
    /// pipeline's [`record`](StimulusPipeline::record) runs during this frame's
    /// [`flip`](Self::flip). The raw [`FrameRecorder`] is handed to `record` —
    /// the same low-level access as [`draw_custom`](Self::draw_custom).
    ///
    /// Consecutive `draw_with` calls against the same handle reach `record` as
    /// one command slice, so a pipeline can answer a run of draws with a single
    /// instanced draw. Any other draw between them splits the run, preserving
    /// call-order compositing.
    ///
    /// # Errors
    ///
    /// [`VSEError::Pipeline`] if `handle` was issued by a different VSE
    /// context; its pipeline ids mean nothing here.
    pub fn draw_with<C: 'static>(
        &mut self,
        handle: RegisteredPipeline<C>,
        command: C,
    ) -> Result<(), VSEError> {
        Ok(self.state.renderer.push_registered(handle, command)?)
    }
}
