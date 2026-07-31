//! Text rendering — instructions, labels, and feedback.
//!
//! Consolidates PsychDemos `DrawSomeTextDemo`, `DrawFormattedTextDemo`, and
//! `FontDemo` using VSE's built-in 5×7 bitmap font (`draw_text` / `text_width`).
//! The font draws as filled rectangles through the flat-color pipeline, so text
//! needs no font asset and no texture upload — usable for on-screen instructions
//! and trial feedback in any experiment.
//!
//! Shows: a title and wrapped instruction block at several scales and colors,
//! centered text (via `text_width`), a live counter, and the full glyph sheet.
//!
//! Controls: Space increments the counter and cycles the feedback line;
//! Escape exits.
//!
//! # Running
//!
//! ```bash
//! cargo run --release --example 19_text_and_instructions
//! ```

use vision_stimulus_engine::prelude::*;

const INSTRUCTIONS: &[&str] = &[
    "This text is drawn with the built-in 5x7 bitmap font.",
    "Each glyph is filled rectangles - no font file, no texture.",
    "Use draw_text for instructions, labels, and trial feedback.",
    "Press space to advance the counter. Press escape to quit.",
];

const FEEDBACK: &[&str] = &["correct!", "too slow", "try again", "nice", "keep going"];

/// The printable glyphs the font covers, laid out as a sheet.
const GLYPH_SHEET: &[&str] = &[
    "ABCDEFGHIJKLM",
    "NOPQRSTUVWXYZ",
    "0123456789",
    ".,:;!?-+=/()'%",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("warn").init();

    let context = VSEContext::builder()
        .with_window_size(900, 680)
        .with_title("VSE - Text & Instructions")
        .with_clear_color(0.10, 0.10, 0.13, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .build()?;

    let mut counter: u32 = 0;

    context.run(move |vse| {
        if vse.key_just_pressed(KeyCode::Escape) {
            vse.request_exit();
            return Ok(());
        }
        if vse.key_just_pressed(KeyCode::Space) {
            counter += 1;
        }

        let (w, _h) = vse.window_size();
        let wf = w as f32;

        // Title, centered, large.
        let title = "VSE TEXT DEMO";
        let title_scale = 6.0;
        let tw = vse.text_width(title, title_scale);
        vse.draw_text(
            title,
            (wf - tw) / 2.0,
            30.0,
            title_scale,
            Color::rgb(0.9, 0.9, 0.4),
        );

        // Instruction block, left-aligned, medium.
        let mut y = 120.0;
        for line in INSTRUCTIONS {
            vse.draw_text(line, 40.0, y, 2.5, Color::grey(0.85));
            y += 30.0;
        }

        // Live counter + centered feedback line.
        let counter_str = format!("count: {counter}");
        vse.draw_text(&counter_str, 40.0, 260.0, 3.0, Color::rgb(0.4, 0.8, 1.0));

        let fb = FEEDBACK[(counter as usize) % FEEDBACK.len()];
        let fb_scale = 4.0;
        let fbw = vse.text_width(fb, fb_scale);
        vse.draw_text(
            fb,
            (wf - fbw) / 2.0,
            320.0,
            fb_scale,
            Color::rgb(0.5, 1.0, 0.6),
        );

        // Glyph sheet.
        vse.draw_text("GLYPH SHEET", 40.0, 420.0, 2.5, Color::grey(0.6));
        let mut gy = 460.0;
        for row in GLYPH_SHEET {
            vse.draw_text(row, 40.0, gy, 4.0, Color::WHITE);
            gy += 45.0;
        }

        vse.clear()?;
        vse.flip(None)?;
        Ok(())
    })?;

    Ok(())
}
