//! Pixel-level regression tests for headless (offscreen) rendering.
//!
//! These are the first tests in the repo that assert on rendered *pixels*
//! rather than on the plan that produces them. They need a Vulkan device but
//! no display, compositor, or window.

use std::cell::RefCell;

use vision_stimulus_engine::prelude::*;

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
