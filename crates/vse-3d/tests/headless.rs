use std::cell::RefCell;
use std::path::PathBuf;

use glam::{Mat4, Vec3};
use vision_stimulus_engine::prelude::*;
use vse_3d::{Bounds3D, ModelHandle, PerspectiveCamera, Vse3d, Vse3dConfig};

fn asset(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

fn fit_transform(bounds: Bounds3D) -> Mat4 {
    let extent = bounds.size().max_element();
    let scale = if extent > 0.0 { 1.8 / extent } else { 1.0 };
    Mat4::from_scale(Vec3::splat(scale)) * Mat4::from_translation(-bounds.center())
}

struct Setup {
    renderer: Vse3d,
    model: ModelHandle,
    fit: Mat4,
}

#[test]
fn headless_external_3d_renders_below_vse_2d_overlays() {
    let pixels = RefCell::new(Vec::new());
    let metadata = RefCell::new(None);
    let mut headless = HeadlessContext::builder(96, 96)
        .with_clear_color(1.0, 0.0, 0.0, 1.0)
        .build()
        .expect("headless VSE context");

    headless
        .run_headless_with_setup(
            |vse| {
                let mut renderer = Vse3d::register(vse, Vse3dConfig::default())?;
                let model = renderer.load_model(asset("assets/3d/models/bunny.glb"))?;
                let fit = fit_transform(renderer.model_bounds(model)?);
                *metadata.borrow_mut() = Some(renderer.info());
                Ok(Setup {
                    renderer,
                    model,
                    fit,
                })
            },
            |frame| {
                *pixels.borrow_mut() = frame.to_rgba8();
                Ok(())
            },
            |vse, setup| {
                setup.renderer.draw_normals(
                    setup.model,
                    setup.fit,
                    &PerspectiveCamera::default(),
                )?;
                setup.renderer.render_frame(vse)?;
                vse.draw_rect(0.0, 0.0, 16.0, 16.0, Color::BLUE);
                vse.flip(None)?;
                vse.request_exit();
                Ok(())
            },
        )
        .expect("headless external 3D run");

    let info = metadata
        .into_inner()
        .expect("setup captures vse-3d metadata");
    assert_eq!(info.extent, [96, 96]);
    assert_eq!(info.pipelines, ["MeshNormals"]);
    assert_eq!(info.models.len(), 1);
    assert_eq!(info.models[0].source_sha256.len(), 64);

    let pixels = pixels.into_inner();
    let at = |x: usize, y: usize| {
        let i = (y * 96 + x) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    assert_eq!(at(4, 4), [0, 0, 255, 255], "VSE overlay must be on top");
    assert_ne!(
        at(48, 48),
        [255, 0, 0, 255],
        "the centered model must replace the clear color"
    );
}
