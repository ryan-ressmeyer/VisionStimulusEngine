//! Custom pipeline via the Tier 2 raw record hook (`draw_custom`).
//!
//! Advanced users can bring their own `GraphicsPipeline` and record draws
//! straight into VSE's active render pass. This example builds ONE custom
//! pipeline whose fragment shader paints an animated checkerboard over a
//! full-screen triangle, then renders it every frame with
//! [`RenderContext::draw_custom`]. VSE's built-in `draw_*` calls still work
//! normally; any that were issued this frame composite UNDERNEATH the custom
//! draw, since custom draws record last in the pass.
//!
//! The custom draw runs INSIDE VSE's 2D pass: the render target is the
//! swapchain image, the viewport is already set to the whole framebuffer, and
//! the closure must NOT begin/end the pass itself. See the `draw_custom`
//! rustdoc for the full contract.
//!
//! The pipeline is built lazily on the first frame — the only point at which a
//! [`RenderContext`] (and thus `device()` / `swapchain().format()`) is
//! available in this run-loop shape. In a real experiment, build it once at
//! setup or between trials, never every frame.
//!
//! Press Escape to exit.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 21_custom_pipeline
//! ```

use std::sync::Arc;

use vision_stimulus_engine::prelude::*;
use vulkano::pipeline::{
    graphics::{
        color_blend::{ColorBlendAttachmentState, ColorBlendState},
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

// Full-screen triangle generated from gl_VertexIndex — no vertex buffer needed.
mod checker_vs {
    vulkano_shaders::shader! {
        ty: "vertex",
        src: r"
            #version 460

            layout(location = 0) out vec2 v_uv;

            void main() {
                // Oversized triangle covering the screen; visible uv is [0, 1].
                v_uv = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2);
                gl_Position = vec4(v_uv * 2.0 - 1.0, 0.0, 1.0);
            }
        ",
    }
}

// Animated checkerboard; `phase` scrolls the pattern each frame.
mod checker_fs {
    vulkano_shaders::shader! {
        ty: "fragment",
        src: r"
            #version 460

            layout(location = 0) in vec2 v_uv;
            layout(location = 0) out vec4 f_color;

            layout(push_constant) uniform PushConstants {
                float phase;
            } pc;

            void main() {
                vec2 cell = floor(v_uv * 10.0 + vec2(pc.phase));
                float checker = mod(cell.x + cell.y, 2.0);
                vec3 color = mix(vec3(0.10, 0.12, 0.35), vec3(0.95, 0.75, 0.20), checker);
                f_color = vec4(color, 1.0);
            }
        ",
    }
}

/// Build the custom checkerboard pipeline once. Compatible with VSE's 2D pass:
/// the swapchain color format, no depth attachment, dynamic viewport (VSE sets
/// the viewport for us).
fn build_checker_pipeline(vse: &RenderContext) -> Arc<GraphicsPipeline> {
    let device = vse.device().clone();

    let vs = checker_vs::load(device.clone())
        .unwrap()
        .entry_point("main")
        .unwrap();
    let fs = checker_fs::load(device.clone())
        .unwrap()
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
            .unwrap(),
    )
    .unwrap();

    GraphicsPipeline::new(
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
            color_blend_state: Some(ColorBlendState::with_attachment_states(
                1,
                ColorBlendAttachmentState::default(),
            )),
            // VSE sets the viewport dynamically inside its pass; match that.
            dynamic_state: [DynamicState::Viewport].into_iter().collect(),
            subpass: Some(
                PipelineRenderingCreateInfo {
                    color_attachment_formats: vec![Some(vse.swapchain().format())],
                    ..Default::default()
                }
                .into(),
            ),
            ..GraphicsPipelineCreateInfo::layout(layout)
        },
    )
    .unwrap()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let context = VSEContext::builder()
        .with_window_size(900, 700)
        .with_title("VSE - Custom Pipeline (draw_custom)")
        .with_clear_color(0.0, 0.0, 0.0, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .build()?;

    // Built lazily on the first frame (see module docs).
    let mut pipeline: Option<Arc<GraphicsPipeline>> = None;
    let mut frame: u64 = 0;

    context.run(move |vse| {
        if vse.key_just_pressed(KeyCode::Escape) {
            vse.request_exit();
            return Ok(());
        }

        let pipeline = pipeline
            .get_or_insert_with(|| build_checker_pipeline(vse))
            .clone();

        let phase = frame as f32 * 0.03;

        // Record the custom pipeline into VSE's active 2D pass. Composites on
        // top of any built-in draws issued this frame.
        vse.draw_custom(move |builder, _frame| {
            builder
                .bind_pipeline_graphics(pipeline.clone())
                .unwrap()
                .push_constants(
                    pipeline.layout().clone(),
                    0,
                    checker_fs::PushConstants { phase },
                )
                .unwrap();
            // SAFETY: the pipeline takes no vertex/descriptor input; the
            // full-screen triangle's 3 vertices come from gl_VertexIndex.
            unsafe {
                builder.draw(3, 1, 0, 0).unwrap();
            }
        });

        vse.clear()?;
        vse.flip(None)?;
        frame += 1;
        Ok(())
    })?;

    Ok(())
}
