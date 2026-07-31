//! Gaze-contingent moving window (mouse as gaze proxy).
//!
//! Replicates PsychDemos `GazeContingentDemo` / `GazeContingentTutorial` using
//! the mouse in place of an eye tracker. A detailed "scene" is hidden behind a
//! full-field mask; a soft-edged window follows the cursor and reveals the scene
//! only where you are looking — the classic McConkie–Rayner moving-window
//! paradigm.
//!
//! Implementation note: VSE's renderer draws all textures before gratings/dots,
//! and textures composite in submission order, so both layers here are textures
//! — the scene (drawn first) and a large occluder sprite with a transparent
//! Gaussian hole (drawn second, centered on the cursor). Alpha blending does the
//! rest. Press Escape to exit.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 15_gaze_contingent
//! ```

use vision_stimulus_engine::prelude::*;

const SCENE_W: u32 = 1000;
const SCENE_H: u32 = 700;
const OCC: u32 = 2048; // occluder sprite side; large enough to cover the screen for any cursor pos

/// A high-detail "scene": a fine checkerboard with a color gradient, opaque.
fn scene_texture() -> Vec<u8> {
    let mut data = vec![0u8; (SCENE_W * SCENE_H * 4) as usize];
    let check = 20u32;
    for y in 0..SCENE_H {
        for x in 0..SCENE_W {
            let on = ((x / check) + (y / check)) % 2 == 0;
            let i = ((y * SCENE_W + x) * 4) as usize;
            let fx = x as f32 / SCENE_W as f32;
            let fy = y as f32 / SCENE_H as f32;
            if on {
                data[i] = (40.0 + 200.0 * fx) as u8;
                data[i + 1] = (40.0 + 200.0 * fy) as u8;
                data[i + 2] = 220;
            } else {
                data[i] = 20;
                data[i + 1] = 20;
                data[i + 2] = 30;
            }
            data[i + 3] = 255;
        }
    }
    data
}

/// A full occluder tile: opaque neutral mask everywhere, with a soft transparent
/// hole in the center (alpha 0 inside `r_in`, ramping to 1 by `r_out`).
fn occluder_texture() -> Vec<u8> {
    let mut data = vec![0u8; (OCC * OCC * 4) as usize];
    let c = OCC as f32 / 2.0;
    let r_in = 90.0;
    let r_out = 150.0;
    for y in 0..OCC {
        for x in 0..OCC {
            let dx = x as f32 - c;
            let dy = y as f32 - c;
            let r = (dx * dx + dy * dy).sqrt();
            let alpha = ((r - r_in) / (r_out - r_in)).clamp(0.0, 1.0);
            let i = ((y * OCC + x) * 4) as usize;
            data[i] = 128;
            data[i + 1] = 128;
            data[i + 2] = 128;
            data[i + 3] = (alpha * 255.0) as u8;
        }
    }
    data
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("warn").init();

    println!("Gaze-contingent window: move the mouse to reveal the hidden scene.");
    println!("Escape to exit.\n");

    let context = VSEContext::builder()
        .with_window_size(SCENE_W, SCENE_H)
        .with_title("VSE - Gaze-Contingent Window")
        .with_clear_color(0.5, 0.5, 0.5, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .build()?;

    let mut scene: Option<TextureHandle> = None;
    let mut occluder: Option<TextureHandle> = None;

    context.run(move |vse| {
        if vse.key_just_pressed(KeyCode::Escape) {
            vse.request_exit();
            return Ok(());
        }

        if scene.is_none() {
            scene = Some(vse.load_texture_rgba(SCENE_W, SCENE_H, &scene_texture())?);
            occluder = Some(vse.load_texture_rgba(OCC, OCC, &occluder_texture())?);
        }

        // Layer 1: the full-field scene.
        vse.draw_texture(scene.unwrap(), 0.0, 0.0, SCENE_W as f32, SCENE_H as f32);

        // Layer 2: the occluder, centered on the cursor. Its transparent hole
        // reveals the scene beneath at the gaze point.
        let (mx, my) = vse.mouse_position();
        let half = OCC as f32 / 2.0;
        vse.draw_texture(
            occluder.unwrap(),
            mx as f32 - half,
            my as f32 - half,
            mx as f32 + half,
            my as f32 + half,
        );

        vse.clear()?;
        vse.flip(None)?;
        Ok(())
    })?;

    Ok(())
}
