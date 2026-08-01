//! Native indexed-mesh rendering with geometric face-normal colors.
//!
//! Prepare the four models described in `assets/3d/README.md`, then run:
//!
//! ```bash
//! cargo run --release --example 20_mesh_normals_3d
//! ```

use std::path::Path;

use glam::{Mat4, Quat, Vec2, Vec3};
use vision_stimulus_engine::prelude::*;

const SPIN_PERIOD_FRAMES: f32 = 480.0;
const MODEL_FILES: [(&str, &str); 4] = [
    ("Bunny", "assets/3d/models/bunny.glb"),
    ("Teapot", "assets/3d/models/teapot.glb"),
    ("Suzanne", "assets/3d/models/suzanne.glb"),
    ("Benchy", "assets/3d/models/benchy.glb"),
];

fn missing_asset_message(path: &str) -> String {
    format!(
        "missing native 3D demo asset: {path}\nPrepare the licensed demo models with:\n  uv run assets/3d/prepare.py"
    )
}

struct DemoModel {
    name: &'static str,
    handle: ModelHandle,
    fit: Mat4,
    status: String,
}

fn arcball_point(position: (f64, f64), size: (u32, u32)) -> Vec3 {
    let scale = size.0.min(size.1).max(1) as f32;
    let p = Vec2::new(
        (2.0 * position.0 as f32 - size.0 as f32) / scale,
        (size.1 as f32 - 2.0 * position.1 as f32) / scale,
    );
    let length_squared = p.length_squared();
    if length_squared <= 1.0 {
        Vec3::new(p.x, p.y, (1.0 - length_squared).sqrt())
    } else {
        Vec3::new(p.x, p.y, 0.0).normalize()
    }
}

fn fit_transform(bounds: Bounds3D) -> Mat4 {
    let extent = bounds.size().max_element();
    let scale = if extent > 0.0 { 1.8 / extent } else { 1.0 };
    Mat4::from_scale(Vec3::splat(scale)) * Mat4::from_translation(-bounds.center())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("warn").init();

    let context = VSEContext::builder()
        .with_window_size(900, 700)
        .with_title("VSE - Native 3D Mesh Normals")
        .with_clear_color(0.18, 0.18, 0.18, 1.0)
        .with_present_mode(PresentMode::Fifo)
        .build()?;

    let mut models: Option<Vec<DemoModel>> = None;
    let mut selected = 0_usize;
    let mut drag = Quat::IDENTITY;
    let mut previous_arcball: Option<Vec3> = None;
    let mut printed_spin_duration = false;
    let camera = PerspectiveCamera::default();

    context.run(move |vse| {
        if models.is_none() {
            if let Some((_, missing)) = MODEL_FILES
                .iter()
                .find(|(_, file)| !Path::new(file).is_file())
            {
                eprintln!("{}", missing_asset_message(missing));
                vse.request_exit();
                return Ok(());
            }

            let mut loaded = Vec::with_capacity(MODEL_FILES.len());
            for &(name, file) in &MODEL_FILES {
                let handle = vse.load_model(file)?;
                let info = vse.model_info(handle)?.clone();
                println!(
                    "{name}: {} triangles, {} primitives, {} instances, sha256 {}",
                    info.triangle_count,
                    info.primitive_count,
                    info.instance_count,
                    info.source_sha256
                );
                loaded.push(DemoModel {
                    name,
                    handle,
                    fit: fit_transform(info.bounds),
                    status: format!("{name} - {} triangles", info.triangle_count),
                });
            }
            println!("Loaded all models. Switching uses resident handles only.");
            models = Some(loaded);
        }
        let models = models.as_ref().expect("models initialized above");
        if !printed_spin_duration {
            if let Some(refresh) = vse.refresh_interval() {
                println!(
                    "Automatic rotation period: {SPIN_PERIOD_FRAMES:.0} frames ({:.2} s at {:.2} Hz)",
                    refresh.as_secs_f32() * SPIN_PERIOD_FRAMES,
                    1.0 / refresh.as_secs_f64()
                );
                printed_spin_duration = true;
            }
        }

        if vse.key_just_pressed(KeyCode::Escape) {
            vse.request_exit();
            return Ok(());
        }
        if vse.key_just_pressed(KeyCode::ArrowLeft) {
            selected = selected.checked_sub(1).unwrap_or(models.len() - 1);
        }
        if vse.key_just_pressed(KeyCode::ArrowRight) {
            selected = (selected + 1) % models.len();
        }
        if vse.key_just_pressed(KeyCode::KeyR) {
            drag = Quat::IDENTITY;
        }

        let current_arcball = arcball_point(vse.mouse_position(), vse.window_size());
        if vse.mouse_button_just_pressed(MouseButton::Left) {
            previous_arcball = Some(current_arcball);
        } else if vse.mouse_button_pressed(MouseButton::Left) {
            if let Some(previous) = previous_arcball {
                let delta = Quat::from_rotation_arc(previous, current_arcball);
                drag = (delta * drag).normalize();
            }
            previous_arcball = Some(current_arcball);
        } else {
            previous_arcball = None;
        }

        let model = &models[selected];
        let auto_yaw = -std::f32::consts::TAU * vse.frame_number() as f32 / SPIN_PERIOD_FRAMES;
        let transform = Mat4::from_quat(drag * Quat::from_rotation_y(auto_yaw)) * model.fit;
        vse.draw_model_normals(model.handle, transform, &camera)?;

        vse.draw_text(&model.status, 16.0, 16.0, 2.0, Color::WHITE);
        vse.draw_text(
            "LEFT/RIGHT MODEL  DRAG ROTATE  R RESET  ESC EXIT",
            16.0,
            42.0,
            1.5,
            Color::grey(0.85),
        );
        vse.draw_text(
            "FACE NORMAL: X/Y/Z -> R/G/B",
            16.0,
            62.0,
            1.5,
            Color::grey(0.85),
        );
        vse.draw_text(model.name, 16.0, 82.0, 1.5, Color::grey(0.7));

        vse.clear()?;
        vse.flip(None)?;
        Ok(())
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::missing_asset_message;

    #[test]
    fn missing_asset_message_names_the_setup_command_once() {
        let message = missing_asset_message("assets/3d/models/bunny.glb");
        assert!(message.contains("uv run assets/3d/prepare.py"));
        assert_eq!(message.matches("bunny.glb").count(), 1);
    }
}
