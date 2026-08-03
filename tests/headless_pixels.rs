//! Pixel-level regression tests for headless (offscreen) rendering.
//!
//! These are the first tests in the repo that assert on rendered *pixels*
//! rather than on the plan that produces them. They need a Vulkan device but
//! no display, compositor, or window.

use std::cell::RefCell;
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

/// Render one frame headless and return its pixels as RGBA8, row-major.
fn render_one_frame(
    width: u32,
    height: u32,
    clear: Color,
    draw: impl FnMut(&mut RenderContext) -> Result<(), VSEError>,
) -> Vec<[u8; 4]> {
    let pixels = RefCell::new(Vec::new());
    let mut draw = draw;

    let mut ctx = VSEContext::builder()
        .with_headless(width, height)
        .with_clear_color(clear.r, clear.g, clear.b, clear.a)
        .build_headless()
        .expect("headless context creation requires a Vulkan device but no display");

    ctx.run_headless(
        |frame| {
            let mut out = Vec::with_capacity((frame.width() * frame.height()) as usize);
            for y in 0..frame.height() {
                for x in 0..frame.width() {
                    out.push(frame.pixel(x, y));
                }
            }
            *pixels.borrow_mut() = out;
            Ok(())
        },
        |vse| {
            draw(vse)?;
            vse.flip(None)?;
            vse.request_exit();
            Ok(())
        },
    )
    .expect("headless run");

    pixels.into_inner()
}

/// Index a row-major RGBA8 frame.
fn at(pixels: &[[u8; 4]], width: u32, x: u32, y: u32) -> [u8; 4] {
    pixels[(y * width + x) as usize]
}

#[test]
fn a_rect_covers_the_clear_color_where_it_is_drawn_and_nowhere_else() {
    let width = 64;
    let height = 64;

    let pixels = render_one_frame(width, height, Color::RED, |vse| {
        vse.draw_rect(0.0, 0.0, 32.0, 32.0, Color::BLUE);
        Ok(())
    });

    assert_eq!(
        pixels.len(),
        (width * height) as usize,
        "the sink must receive every pixel of the frame"
    );
    assert_eq!(
        at(&pixels, width, 10, 10),
        [0, 0, 255, 255],
        "inside the rect the frame must show the rect's colour"
    );
    assert_eq!(
        at(&pixels, width, 50, 50),
        [255, 0, 0, 255],
        "outside the rect the frame must still show the clear colour"
    );
}

#[test]
fn a_rect_drawn_after_a_texture_composites_on_top_of_it() {
    // Call-order compositing across *different* pipelines: the flat-color and
    // textured pipelines are separate, so an implementation that grouped draws
    // by type — as VSE did before call-order compositing landed — would draw
    // the texture last and bury the rect.
    let width = 64;
    let height = 64;
    let green: Vec<u8> = std::iter::repeat([0u8, 255, 0, 255])
        .take((32 * 32) as usize)
        .flatten()
        .collect();

    let pixels = render_one_frame(width, height, Color::BLACK, move |vse| {
        let texture = vse.load_texture_rgba(32, 32, &green)?;
        vse.draw_texture(texture, 0.0, 0.0, 32.0, 32.0);
        vse.draw_rect(0.0, 0.0, 16.0, 16.0, Color::BLUE);
        Ok(())
    });

    assert_eq!(
        at(&pixels, width, 8, 8),
        [0, 0, 255, 255],
        "the rect was drawn after the texture, so it must be on top of it"
    );
    assert_eq!(
        at(&pixels, width, 24, 24),
        [0, 255, 0, 255],
        "where the rect does not reach, the texture must still be visible"
    );
    assert_eq!(
        at(&pixels, width, 48, 48),
        [0, 0, 0, 255],
        "outside both draws the clear colour must survive"
    );
}

// --- A minimal Tier 1 pipeline, to pin registered-draw interleaving ---------

mod solid_vs {
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

mod solid_fs {
    vulkano_shaders::shader! {
        ty: "fragment",
        src: r"
            #version 460
            layout(location = 0) in vec2 v_uv;
            layout(location = 0) out vec4 f_color;
            layout(push_constant) uniform PushConstants {
                vec4 color;
                vec4 rect;   // uv-space min.xy, max.xy
            } pc;
            void main() {
                if (v_uv.x < pc.rect.x || v_uv.x > pc.rect.z ||
                    v_uv.y < pc.rect.y || v_uv.y > pc.rect.w) {
                    discard;
                }
                f_color = pc.color;
            }
        ",
    }
}

/// Paints an opaque axis-aligned rectangle in uv space. Deliberately trivial:
/// this test is about *where in the frame* a registered draw lands, not about
/// what it can draw.
struct SolidPipeline;

#[derive(Clone, Copy)]
struct SolidParams {
    color: [f32; 4],
    rect: [f32; 4],
}

impl StimulusPipeline for SolidPipeline {
    type Command = SolidParams;
    type Resources = Arc<GraphicsPipeline>;

    fn build(&self, cx: &PipelineBuildCtx) -> Result<Self::Resources, PipelineError> {
        let device = cx.device().clone();
        let vs = solid_vs::load(device.clone())
            .map_err(PipelineError::build)?
            .entry_point("main")
            .unwrap();
        let fs = solid_fs::load(device.clone())
            .map_err(PipelineError::build)?
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
                .map_err(PipelineError::build)?,
        )
        .map_err(PipelineError::build)?;

        GraphicsPipeline::new(
            device.clone(),
            None,
            GraphicsPipelineCreateInfo {
                stages: stages.into_iter().collect(),
                vertex_input_state: Some(VertexInputState::default()),
                input_assembly_state: Some(InputAssemblyState::default()),
                viewport_state: Some(ViewportState::default()),
                rasterization_state: Some(RasterizationState::default()),
                multisample_state: Some(MultisampleState::default()),
                color_blend_state: Some(ColorBlendState::with_attachment_states(
                    1,
                    ColorBlendAttachmentState::default(),
                )),
                dynamic_state: [DynamicState::Viewport].into_iter().collect(),
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
        .map_err(PipelineError::build)
    }

    fn record(
        &mut self,
        pipeline: &mut Self::Resources,
        recorder: &mut FrameRecorder,
        _cx: &RecordCtx,
        commands: &[Self::Command],
    ) -> Result<(), PipelineError> {
        for params in commands {
            recorder
                .bind_pipeline_graphics(pipeline.clone())
                .map_err(PipelineError::record)?
                .push_constants(
                    pipeline.layout().clone(),
                    0,
                    solid_fs::PushConstants {
                        color: params.color,
                        rect: params.rect,
                    },
                )
                .map_err(PipelineError::record)?;
            // SAFETY: three vertices, no vertex buffer — positions come from
            // gl_VertexIndex, exactly as the pipeline's vertex input declares.
            unsafe {
                recorder.draw(3, 1, 0, 0).map_err(PipelineError::record)?;
            }
        }
        Ok(())
    }
}

#[test]
fn a_registered_draw_composites_between_the_builtins_queued_around_it() {
    // Also the first test to exercise `record_erased`: the type-erased dispatch
    // from a `DrawCommand::Registered` payload back to the concrete pipeline.
    let width = 64;
    let height = 64;
    let pixels = RefCell::new(Vec::new());

    let mut ctx = VSEContext::builder()
        .with_headless(width, height)
        .with_clear_color(0.0, 0.0, 0.0, 1.0)
        .build_headless()
        .expect("headless context");

    ctx.run_headless_with_setup(
        |vse| vse.register_pipeline(SolidPipeline),
        |frame| {
            let mut out = Vec::with_capacity((frame.width() * frame.height()) as usize);
            for y in 0..frame.height() {
                for x in 0..frame.width() {
                    out.push(frame.pixel(x, y));
                }
            }
            *pixels.borrow_mut() = out;
            Ok(())
        },
        |vse, handle| {
            // Below: a green band across the top half.
            vse.draw_rect(0.0, 0.0, 64.0, 32.0, Color::GREEN);
            // Middle: the registered draw, covering the centre quarter.
            vse.draw_with(
                *handle,
                SolidParams {
                    color: [1.0, 0.0, 0.0, 1.0],
                    rect: [0.25, 0.25, 0.75, 0.75],
                },
            )?;
            // Above: a blue square in the top-left corner.
            vse.draw_rect(0.0, 0.0, 8.0, 8.0, Color::BLUE);
            vse.flip(None)?;
            vse.request_exit();
            Ok(())
        },
    )
    .expect("headless run");

    let pixels = pixels.into_inner();
    assert_eq!(
        at(&pixels, width, 32, 32),
        [255, 0, 0, 255],
        "the registered draw must cover the built-in rect queued before it"
    );
    assert_eq!(
        at(&pixels, width, 4, 4),
        [0, 0, 255, 255],
        "the built-in rect queued after the registered draw must cover it"
    );
    assert_eq!(
        at(&pixels, width, 40, 8),
        [0, 255, 0, 255],
        "outside the registered rect the earlier built-in band must survive"
    );
    assert_eq!(
        at(&pixels, width, 8, 56),
        [0, 0, 0, 255],
        "outside every draw the clear colour must survive"
    );
}
