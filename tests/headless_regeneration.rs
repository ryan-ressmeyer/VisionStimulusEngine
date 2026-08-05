//! Rebuilding a headless session from a recorded session's `HostInfo`.
//!
//! This is the motivating use case for headless rendering: after an experiment
//! has run, regenerate the stimuli it displayed from the metadata it recorded.

use std::cell::RefCell;

use vision_stimulus_engine::prelude::*;

/// The stimulus under regeneration. Deliberately exercises several pipelines,
/// so a suite mismatch between the recording and the regeneration would show up
/// as different pixels rather than as an identical blank frame.
fn draw_scene(vse: &mut RenderContext) -> Result<(), VSEError> {
    vse.draw_rect(4.0, 4.0, 40.0, 28.0, Color::GREEN);
    vse.draw_circle(24.0, 16.0, 9.0, Color::BLUE);
    vse.draw_dots(&[(8.0, 8.0), (40.0, 24.0)], 3.0, Color::WHITE);
    Ok(())
}

/// Render `draw_scene` in a headless context and return the frame's bytes.
fn render_scene(ctx: &mut vision_stimulus_engine::core::HeadlessContext) -> Vec<u8> {
    let bytes = RefCell::new(Vec::new());
    ctx.run_headless(
        |frame| {
            *bytes.borrow_mut() = frame.bytes().to_vec();
            Ok(())
        },
        |vse| {
            draw_scene(vse)?;
            vse.flip(None)?;
            vse.request_exit();
            Ok(())
        },
    )
    .expect("headless run");
    bytes.into_inner()
}

#[test]
fn legacy_pipeline_subselection_does_not_suppress_builtins_during_regeneration() {
    // --- The "recorded" session ---
    let mut original = HeadlessContext::builder(48, 32)
        .with_clear_color(0.25, 0.25, 0.25, 1.0)
        .build()
        .expect("headless context");
    let recorded_info = original.capture_host_info();
    let original_pixels = render_scene(&mut original);

    // Round-trip the metadata through JSON exactly as a real regeneration
    // would: the recording on disk is all a later analysis has.
    let json = serde_json::to_string(&recorded_info).expect("serialize host info");
    let mut recovered: HostInfo = serde_json::from_str(&json).expect("deserialize host info");
    recovered.pipeline.builtin_pipelines = vec!["FlatColor".into()];

    // --- The regeneration ---
    let mut regenerated = HeadlessContext::builder_from_host_info(&recovered)
        .expect("host info describes a renderable target")
        .with_clear_color(0.25, 0.25, 0.25, 1.0)
        .build()
        .expect("regenerated headless context");

    assert_eq!(
        regenerated.format(),
        original.format(),
        "the regeneration must render in the recorded color format"
    );
    assert_eq!(
        regenerated.extent(),
        [48, 32],
        "the regeneration must render at the recorded extent"
    );

    let regenerated_pixels = render_scene(&mut regenerated);
    // Guard against a vacuous pass: two blank frames would also compare equal.
    let distinct: std::collections::HashSet<&[u8]> = original_pixels.chunks_exact(4).collect();
    assert!(
        distinct.len() > 2,
        "the scene must render more than a flat field for this comparison to mean anything,          got {} distinct texel values",
        distinct.len()
    );
    assert_eq!(
        regenerated_pixels, original_pixels,
        "a session rebuilt from its own recorded metadata must reproduce its pixels"
    );
}

#[test]
fn recordings_without_legacy_pipeline_metadata_still_deserialize() {
    let original = HeadlessContext::builder(16, 16)
        .build()
        .expect("headless context");
    let mut json = serde_json::to_value(original.capture_host_info()).expect("serialize host info");
    json["pipeline"]
        .as_object_mut()
        .expect("pipeline metadata object")
        .remove("builtin_pipelines");

    let recovered: Result<HostInfo, _> = serde_json::from_value(json);
    assert!(
        recovered.is_ok(),
        "recordings predating builtin_pipelines must remain readable"
    );
}

#[test]
fn unknown_legacy_pipeline_names_do_not_block_regeneration() {
    let original = HeadlessContext::builder(16, 16)
        .build()
        .expect("headless context");
    let mut recorded_info = original.capture_host_info();
    recorded_info
        .pipeline
        .builtin_pipelines
        .push("FuturePipeline".into());

    assert!(
        HeadlessContext::builder_from_host_info(&recorded_info).is_ok(),
        "pipeline names are legacy metadata and must not control reconstruction"
    );
}

#[test]
fn host_info_from_a_headless_run_is_not_mistakable_for_a_presented_one() {
    let mut ctx = HeadlessContext::builder(16, 16)
        .build()
        .expect("headless context");
    let info = ctx.capture_host_info();

    assert!(
        info.swapchain.present_mode.contains("headless"),
        "a headless run has no presentation mode, and must say so: {:?}",
        info.swapchain.present_mode
    );
    assert_eq!(info.swapchain.image_count, 1);
    assert_eq!(info.swapchain.extent, [16, 16]);
    assert_eq!(info.pipeline.window_size, (16, 16));
    assert_eq!(info.pipeline.present_mode, "n/a (headless)");

    // And its flips are tagged as synthesized, never measured.
    let sources = RefCell::new(Vec::new());
    ctx.run_headless(
        |_frame| Ok(()),
        |vse| {
            let flip = vse.flip(None)?;
            sources.borrow_mut().push(flip.timing_source);
            vse.request_exit();
            Ok(())
        },
    )
    .expect("headless run");
    assert_eq!(sources.into_inner(), vec![TimingSource::Offscreen]);
}
