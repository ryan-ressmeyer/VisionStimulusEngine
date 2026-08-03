//! Registered stimulus pipeline via the Tier 1 `StimulusPipeline` trait.
//!
//! Tier 1 is the teaching layer: you implement one trait
//! ([`StimulusPipeline`]) that builds your own `GraphicsPipeline` **once** in
//! `build`, and records draws for this frame's queued parameters in `record`.
//! You register it once to get a `Copy` handle
//! ([`RegisteredPipeline`]), then enqueue a draw each frame with
//! [`RenderContext::draw_with`]. Registered draws composite in **call order**,
//! interleaved with the built-in `draw_*` calls issued around them.
//!
//! This example registers a `RadialPipeline` that paints an animated,
//! soft-edged concentric-ring patch from push constants (a per-draw color +
//! phase). Each frame it draws, in call order: a built-in `draw_rect`
//! background stripe (BEHIND the patch), then the registered radial patch (its
//! soft alpha edge lets the stripe show through around it), then a built-in
//! `draw_circle` fixation dot (ON TOP of the patch) — demonstrating that a
//! Tier 1 draw interleaves with built-ins issued before and after it.
//!
//! `record` receives the **raw** [`FrameRecorder`] — the same low-level vulkano
//! `AutoCommandBufferBuilder` VSE records into (as in the Tier 2 `draw_custom`
//! hook). It runs INSIDE VSE's active 2D pass: the render target is the
//! swapchain image, the viewport is already set, and `record` must NOT
//! begin/end the pass. Build your pipeline in `build`, never in `record`.
//!
//! Registration is done lazily on the first frame here, because a
//! [`RenderContext`] (and thus the swapchain color format) is only available
//! inside the run loop. In a real experiment, register once at setup or between
//! trials, never on the presentation path.
//!
//! Press Escape to exit.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 23_registered_pipeline
//! ```

use std::sync::Arc;

use vision_stimulus_engine::prelude::*;
use vulkano::pipeline::{
    graphics::{
        color_blend::{AttachmentBlend, ColorBlendAttachmentState, ColorBlendState},
        input_assembly::InputAssemblyState,
        multisample::MultisampleState,
        rasterization::RasterizationState,
        subpass::PipelineRenderingCreateInfo,
        vertex_input::VertexInputState,
        viewport::ViewportState,
        GraphicsPipelineCreateInfo,
    },
    layout::PipelineDescriptorSetLayoutCreateInfo,
    DynamicState, GraphicsPipeline, Pipeline, PipelineLayout, PipelineShaderStageCreateInfo,
};

// Full-screen triangle from gl_VertexIndex — no vertex buffer needed. Visible
// uv spans [0, 1] across the framebuffer.
mod radial_vs {
    vulkano_shaders::shader! {
        ty: "vertex",
        src: r"
            #version 460

            layout(location = 0) out vec2 v_uv;

            void main() {
                v_uv = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2);
                gl_Position = vec4(v_uv * 2.0 - 1.0, 0.0, 1.0);
            }
        ",
    }
}

// Animated concentric rings with a soft radial alpha envelope. `phase` scrolls
// the rings inward; `tint` colors them. The envelope keeps the center opaque
// and fades the edge to transparent, so built-ins behind show through.
mod radial_fs {
    vulkano_shaders::shader! {
        ty: "fragment",
        src: r"
            #version 460

            layout(location = 0) in vec2 v_uv;
            layout(location = 0) out vec4 f_color;

            // vec4 first (offset 0), float after (offset 16): no interior padding.
            layout(push_constant) uniform PushConstants {
                vec4 tint;
                float phase;
            } pc;

            void main() {
                vec2 p = v_uv - vec2(0.5);
                float r = length(p);
                float rings = 0.5 + 0.5 * sin(40.0 * r - pc.phase);
                float envelope = smoothstep(0.5, 0.15, r);
                f_color = vec4(pc.tint.rgb * rings, pc.tint.a * envelope);
            }
        ",
    }
}

/// A Tier 1 stimulus pipeline: one `GraphicsPipeline`, built once, that paints
/// an animated radial patch from per-draw push constants.
struct RadialPipeline {
    pipeline: Option<Arc<GraphicsPipeline>>,
}

impl RadialPipeline {
    fn new() -> Self {
        Self { pipeline: None }
    }
}

/// Per-draw parameters for [`RadialPipeline`] — the trait's `Command` type.
#[derive(Clone, Copy)]
struct RadialParams {
    tint: [f32; 4],
    phase: f32,
}

impl StimulusPipeline for RadialPipeline {
    type Command = RadialParams;

    fn build(&mut self, cx: &PipelineBuildCtx) -> Result<(), PipelineError> {
        let device = cx.device().clone();

        let vs = radial_vs::load(device.clone())
            .map_err(|e| PipelineError::Build(e.to_string()))?
            .entry_point("main")
            .unwrap();
        let fs = radial_fs::load(device.clone())
            .map_err(|e| PipelineError::Build(e.to_string()))?
            .entry_point("main")
            .unwrap();

        let stages = [
            PipelineShaderStageCreateInfo::new(vs),
            PipelineShaderStageCreateInfo::new(fs),
        ];

        let layout = PipelineLayout::new(
            device.clone(),
            PipelineDescriptorSetLayoutCreateInfo::from_stages(&stages)
                .into_pipeline_layout_create_info(device.clone())
                .map_err(|e| PipelineError::Build(e.to_string()))?,
        )
        .map_err(|e| PipelineError::Build(e.to_string()))?;

        let pipeline = GraphicsPipeline::new(
            device.clone(),
            None,
            GraphicsPipelineCreateInfo {
                stages: stages.into_iter().collect(),
                // No vertex buffers: positions come from gl_VertexIndex.
                vertex_input_state: Some(VertexInputState::default()),
                input_assembly_state: Some(InputAssemblyState::default()),
                viewport_state: Some(ViewportState::default()),
                rasterization_state: Some(RasterizationState::default()),
                multisample_state: Some(MultisampleState::default()),
                // Alpha blend so the soft edge composites over prior draws.
                color_blend_state: Some(ColorBlendState::with_attachment_states(
                    1,
                    ColorBlendAttachmentState {
                        blend: Some(AttachmentBlend::alpha()),
                        ..Default::default()
                    },
                )),
                // VSE sets the viewport dynamically inside its pass; match that.
                dynamic_state: [DynamicState::Viewport].into_iter().collect(),
                // Must match VSE's 2D pass color format (no depth attachment).
                subpass: Some(
                    PipelineRenderingCreateInfo {
                        color_attachment_formats: vec![Some(cx.color_format())],
                        ..Default::default()
                    }
                    .into(),
                ),
                ..GraphicsPipelineCreateInfo::layout(layout)
            },
        )
        .map_err(|e| PipelineError::Build(e.to_string()))?;

        self.pipeline = Some(pipeline);
        Ok(())
    }

    fn record(
        &self,
        recorder: &mut FrameRecorder,
        _cx: &RecordCtx,
        commands: &[Self::Command],
    ) -> Result<(), PipelineError> {
        let pipeline = self
            .pipeline
            .as_ref()
            .ok_or_else(|| PipelineError::Record("pipeline not built".into()))?;

        // One ordered entry per `draw_with`, so the slice has exactly one param.
        // (The slice shape reserves room for future same-pipeline coalescing.)
        let params = commands[0];

        recorder
            .bind_pipeline_graphics(pipeline.clone())
            .map_err(|e| PipelineError::Record(e.to_string()))?
            .push_constants(
                pipeline.layout().clone(),
                0,
                radial_fs::PushConstants {
                    tint: params.tint,
                    phase: params.phase,
                },
            )
            .map_err(|e| PipelineError::Record(e.to_string()))?;
        // SAFETY: the pipeline takes no vertex/descriptor input; the full-screen
        // triangle's 3 vertices come from gl_VertexIndex.
        unsafe {
            recorder
                .draw(3, 1, 0, 0)
                .map_err(|e| PipelineError::Record(e.to_string()))?;
        }
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let context = VSEContext::builder()
        .with_window_size(900, 700)
        .with_title("VSE - Registered Pipeline (draw_with)")
        .with_clear_color(0.0, 0.0, 0.0, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .build()?;

    // Registered lazily on the first frame (see module docs). `RegisteredPipeline`
    // is `Copy`, so the handle is stashed and reused every frame.
    let mut handle: Option<RegisteredPipeline<RadialParams>> = None;
    let mut frame: u64 = 0;

    context.run(move |vse| {
        if vse.key_just_pressed(KeyCode::Escape) {
            vse.request_exit();
            return Ok(());
        }

        let handle = match handle {
            Some(h) => h,
            None => {
                let h = vse.register_pipeline(RadialPipeline::new())?;
                handle = Some(h);
                h
            }
        };

        let (w, h) = vse.window_size();
        let (w, h) = (w as f32, h as f32);
        let phase = frame as f32 * 0.15;

        // 1. Built-in BEHIND the registered draw: a background stripe.
        vse.draw_rect(0.0, h * 0.45, w, h * 0.55, Color::rgb(0.15, 0.15, 0.30));

        // 2. Registered Tier 1 draw: the animated radial patch. Its soft edge
        //    (alpha envelope) lets the stripe show through around it.
        vse.draw_with(
            handle,
            RadialParams {
                tint: [0.95, 0.75, 0.20, 1.0],
                phase,
            },
        );

        // 3. Built-in ON TOP of the registered draw: a fixation dot.
        vse.draw_circle(w * 0.5, h * 0.5, 6.0, Color::WHITE);

        vse.clear()?;
        vse.flip(None)?;
        frame += 1;
        Ok(())
    })?;

    Ok(())
}
