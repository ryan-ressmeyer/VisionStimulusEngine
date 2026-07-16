//! Deterministic demo scene: a cube translating around a circle with a point
//! light revolving overhead — moving specular/diffuse gradients make per-frame
//! 3D shading visually obvious.
//!
//! Determinism contract: every transform is a **pure function** of
//! [`ExternalFrameIndex`] (VSE's frame counter). No `Time`, no RNG, distinct
//! depths for all geometry.

use bevy::prelude::*;

use crate::ExternalFrameIndex;

/// Frames per full cube revolution (2 s at 120 Hz, 4 s at 60 Hz).
pub const CUBE_PERIOD_FRAMES: f64 = 240.0;

#[derive(Component)]
pub struct OrbitingCube;

#[derive(Component)]
pub struct OrbitingLight;

/// Cube transform at frame `n` (pure; also used by tests / analysis).
pub fn cube_transform(n: u64) -> Transform {
    let theta = (n as f64) * (std::f64::consts::TAU / CUBE_PERIOD_FRAMES);
    Transform::from_xyz((1.8 * theta.cos()) as f32, 0.6, (1.8 * theta.sin()) as f32)
        // Spin the cube on its own axis too, so specular highlights sweep its faces.
        .with_rotation(Quat::from_rotation_y((theta * 3.0) as f32))
}

/// Light transform at frame `n` (pure; revolves opposite the cube, overhead).
pub fn light_transform(n: u64) -> Transform {
    let phi = -(n as f64) * (std::f64::consts::TAU / (CUBE_PERIOD_FRAMES * 2.0));
    Transform::from_xyz((2.4 * phi.cos()) as f32, 2.8, (2.4 * phi.sin()) as f32)
}

/// Spawn the demo scene. `_camera` is already spawned by the producer; the
/// scene only adds geometry + light + the animation system.
pub fn build_demo_scene(app: &mut App, _camera: Entity) {
    let world = app.world_mut();

    let cube_mesh = world
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::new(1.0, 1.0, 1.0));
    let cube_mat = world
        .resource_mut::<Assets<StandardMaterial>>()
        .add(StandardMaterial {
            base_color: Color::srgb(0.85, 0.30, 0.20),
            perceptual_roughness: 0.25,
            metallic: 0.1,
            ..default()
        });
    let ground_mesh = world
        .resource_mut::<Assets<Mesh>>()
        .add(Plane3d::default().mesh().size(10.0, 10.0));
    let ground_mat = world
        .resource_mut::<Assets<StandardMaterial>>()
        .add(StandardMaterial {
            base_color: Color::srgb(0.35, 0.38, 0.42),
            perceptual_roughness: 0.9,
            ..default()
        });

    world.spawn((
        Mesh3d(cube_mesh),
        MeshMaterial3d(cube_mat),
        cube_transform(0),
        OrbitingCube,
    ));
    world.spawn((
        Mesh3d(ground_mesh),
        MeshMaterial3d(ground_mat),
        Transform::from_xyz(0.0, 0.0, 0.0),
    ));
    world.spawn((
        PointLight {
            intensity: 2_000_000.0,
            shadow_maps_enabled: false,
            ..default()
        },
        light_transform(0),
        OrbitingLight,
    ));

    app.add_systems(Update, animate);
}

/// The only mutation of scene state, driven exclusively by the frame index.
fn animate(
    frame: Res<ExternalFrameIndex>,
    mut cubes: Query<&mut Transform, (With<OrbitingCube>, Without<OrbitingLight>)>,
    mut lights: Query<&mut Transform, (With<OrbitingLight>, Without<OrbitingCube>)>,
) {
    for mut t in &mut cubes {
        *t = cube_transform(frame.0);
    }
    for mut t in &mut lights {
        *t = light_transform(frame.0);
    }
}
