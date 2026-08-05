use glam::{Mat4, Vec3, Vec4};
use vision_stimulus_engine::prelude::{RenderContext, VSEError};
use vse_3d::{ModelHandle, PerspectiveCamera, Vse3d, Vse3dConfig, Vse3dInfo};

#[test]
fn camera_projection_uses_vulkan_zero_to_one_depth_and_y_flip() {
    let camera = PerspectiveCamera::default();
    let projection = camera.projection(1.0).unwrap();
    let near = projection * Vec4::new(0.0, 0.0, -camera.near, 1.0);
    let far = projection * Vec4::new(0.0, 0.0, -camera.far, 1.0);

    assert!(near.z.abs() < 1.0e-5, "near z={}", near.z);
    assert!((far.z / far.w - 1.0).abs() < 1.0e-5);
    assert!(projection.y_axis.y < 0.0);
}

#[test]
fn renderer_api_is_owned_and_explicit_per_frame() {
    fn exercise(
        vse: &mut RenderContext<'_>,
        renderer: &mut Vse3d,
        model: ModelHandle,
    ) -> Result<(), VSEError> {
        let camera = PerspectiveCamera {
            eye: Vec3::new(0.0, 0.0, 3.0),
            ..Default::default()
        };
        let _ = renderer.model_info(model)?;
        let _ = renderer.model_bounds(model)?;
        renderer.draw_normals(model, Mat4::IDENTITY, &camera)?;
        renderer.render_frame(vse)?;
        let _removed = renderer.unload_model(model)?;
        Ok(())
    }

    let _register = Vse3d::register;
    let _load = Vse3d::load_model::<&std::path::Path>;
    let _exercise = exercise;
}

#[test]
fn config_and_renderer_info_are_serializable() {
    let config = Vse3dConfig::default();
    assert_eq!(config.ring_len, 3);

    fn round_trip(info: &Vse3dInfo) {
        let json = serde_json::to_string(info).unwrap();
        let recovered: Vse3dInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered, *info);
    }

    let _round_trip = round_trip;
}
