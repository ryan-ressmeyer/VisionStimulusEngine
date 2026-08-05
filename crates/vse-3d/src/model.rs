use std::path::{Path, PathBuf};

use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use vulkano::buffer::BufferContents;
use vulkano::pipeline::graphics::vertex_input::Vertex;

#[derive(Clone, Copy, Debug, Default, BufferContents, Vertex)]
#[repr(C)]
pub(crate) struct Vertex3D {
    #[format(R32G32B32_SFLOAT)]
    pub(crate) position: [f32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModelHandle {
    pub(crate) renderer_id: u64,
    pub(crate) id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bounds3D {
    pub min: Vec3,
    pub max: Vec3,
}

impl Bounds3D {
    pub fn center(self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn size(self) -> Vec3 {
        self.max - self.min
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PerspectiveCamera {
    pub eye: Vec3,
    pub target: Vec3,
    pub up: Vec3,
    pub vertical_fov_radians: f32,
    pub near: f32,
    pub far: f32,
}

impl Default for PerspectiveCamera {
    fn default() -> Self {
        Self {
            eye: Vec3::new(0.0, 0.0, 3.0),
            target: Vec3::ZERO,
            up: Vec3::Y,
            vertical_fov_radians: 45.0_f32.to_radians(),
            near: 0.01,
            far: 100.0,
        }
    }
}

impl PerspectiveCamera {
    pub fn validate(&self) -> Result<(), ModelError> {
        let finite = self.eye.is_finite()
            && self.target.is_finite()
            && self.up.is_finite()
            && self.vertical_fov_radians.is_finite()
            && self.near.is_finite()
            && self.far.is_finite();
        if !finite
            || self.near <= 0.0
            || self.far <= self.near
            || !(0.0..std::f32::consts::PI).contains(&self.vertical_fov_radians)
            || self.up.length_squared() <= f32::EPSILON
            || self.eye.distance_squared(self.target) <= f32::EPSILON
        {
            return Err(ModelError::InvalidCamera);
        }
        Ok(())
    }

    pub fn projection(&self, aspect_ratio: f32) -> Result<Mat4, ModelError> {
        self.validate()?;
        if !aspect_ratio.is_finite() || aspect_ratio <= 0.0 {
            return Err(ModelError::InvalidCamera);
        }
        let mut projection =
            Mat4::perspective_rh(self.vertical_fov_radians, aspect_ratio, self.near, self.far);
        projection.y_axis.y = -projection.y_axis.y;
        Ok(projection)
    }

    pub fn view_projection(&self, aspect_ratio: f32) -> Result<Mat4, ModelError> {
        Ok(self.projection(aspect_ratio)? * Mat4::look_at_rh(self.eye, self.target, self.up))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub source_path: PathBuf,
    pub source_sha256: String,
    pub generator: Option<String>,
    pub gltf_version: String,
    pub vertex_count: u64,
    pub index_count: u64,
    pub triangle_count: u64,
    pub primitive_count: usize,
    pub instance_count: usize,
    pub bounds: Bounds3D,
}

#[derive(Error, Debug)]
pub enum ModelError {
    #[error("failed to read model {path}: {reason}")]
    Read { path: PathBuf, reason: String },
    #[error("malformed or unsupported glTF: {0}")]
    InvalidGltf(String),
    #[error("mesh primitive is missing POSITION")]
    MissingPosition,
    #[error("only triangle-list glTF primitives are supported")]
    UnsupportedTopology,
    #[error("mesh index {index} is outside the {vertex_count} positions")]
    IndexOutOfBounds { index: u32, vertex_count: usize },
    #[error("model contains a non-finite position or transform")]
    NonFinite,
    #[error("model contains no drawable triangles")]
    EmptyModel,
    #[error("skins and morph targets are not supported")]
    UnsupportedDeformation,
    #[error("camera field of view, planes, or vectors are invalid")]
    InvalidCamera,
    #[error("model handle belongs to another vse-3d renderer")]
    ForeignHandle,
    #[error("unknown model handle: id={0}")]
    UnknownHandle(u64),
}

#[derive(Clone, Debug)]
pub(crate) struct DecodedPrimitive {
    pub vertices: Vec<Vertex3D>,
    pub indices: Vec<u32>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DecodedInstance {
    pub primitive_index: usize,
    pub local_transform: Mat4,
}

#[derive(Clone, Debug)]
pub(crate) struct DecodedModel {
    pub primitives: Vec<DecodedPrimitive>,
    pub instances: Vec<DecodedInstance>,
    pub info: ModelInfo,
}

#[cfg(test)]
pub(crate) fn normal_to_rgb(normal: Vec3) -> Vec3 {
    normal * 0.5 + Vec3::splat(0.5)
}

#[cfg(test)]
pub(crate) fn face_normal(a: Vec3, b: Vec3, c: Vec3) -> Option<Vec3> {
    (b - a).cross(c - a).try_normalize()
}

pub(crate) fn decode_model(path: &Path) -> Result<DecodedModel, ModelError> {
    let source = std::fs::read(path).map_err(|e| ModelError::Read {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    let sha = format!("{:x}", Sha256::digest(&source));
    let gltf = gltf::Gltf::open(path).map_err(|e| ModelError::InvalidGltf(e.to_string()))?;
    let base = path.parent();
    let buffers = gltf::import_buffers(&gltf.document, base, gltf.blob)
        .map_err(|e| ModelError::InvalidGltf(e.to_string()))?;
    let document = gltf.document;
    if document.skins().next().is_some() {
        return Err(ModelError::UnsupportedDeformation);
    }

    let mut primitives = Vec::new();
    let mut mesh_primitives: Vec<Vec<usize>> = vec![Vec::new(); document.meshes().len()];
    for mesh in document.meshes() {
        for primitive in mesh.primitives() {
            if primitive.mode() != gltf::mesh::Mode::Triangles {
                return Err(ModelError::UnsupportedTopology);
            }
            if primitive.morph_targets().next().is_some() {
                return Err(ModelError::UnsupportedDeformation);
            }
            let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()].0));
            let positions = reader.read_positions().ok_or(ModelError::MissingPosition)?;
            let vertices: Vec<_> = positions.map(|position| Vertex3D { position }).collect();
            if vertices.iter().any(|v| !Vec3::from(v.position).is_finite()) {
                return Err(ModelError::NonFinite);
            }
            let indices: Vec<u32> = reader
                .read_indices()
                .map(|values| values.into_u32().collect())
                .unwrap_or_else(|| (0..vertices.len() as u32).collect());
            if vertices.is_empty() || indices.is_empty() {
                return Err(ModelError::EmptyModel);
            }
            for &index in &indices {
                if index as usize >= vertices.len() {
                    return Err(ModelError::IndexOutOfBounds {
                        index,
                        vertex_count: vertices.len(),
                    });
                }
            }
            if indices.len() % 3 != 0 {
                return Err(ModelError::InvalidGltf(
                    "triangle index count is not divisible by three".into(),
                ));
            }
            let primitive_index = primitives.len();
            mesh_primitives[mesh.index()].push(primitive_index);
            primitives.push(DecodedPrimitive { vertices, indices });
        }
    }

    let scene = document
        .default_scene()
        .or_else(|| document.scenes().next())
        .ok_or(ModelError::EmptyModel)?;
    let mut instances = Vec::new();
    for node in scene.nodes() {
        visit_node(node, Mat4::IDENTITY, &mesh_primitives, &mut instances)?;
    }
    if primitives.is_empty() || instances.is_empty() {
        return Err(ModelError::EmptyModel);
    }

    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for instance in &instances {
        for vertex in &primitives[instance.primitive_index].vertices {
            let world = instance
                .local_transform
                .transform_point3(Vec3::from(vertex.position));
            if !world.is_finite() {
                return Err(ModelError::NonFinite);
            }
            min = min.min(world);
            max = max.max(world);
        }
    }
    let bounds = Bounds3D { min, max };
    let vertex_count = primitives.iter().map(|p| p.vertices.len() as u64).sum();
    let index_count = primitives.iter().map(|p| p.indices.len() as u64).sum();
    let asset = &document.as_json().asset;
    let info = ModelInfo {
        source_path: path.to_path_buf(),
        source_sha256: sha,
        generator: asset.generator.clone(),
        gltf_version: asset.version.clone(),
        vertex_count,
        index_count,
        triangle_count: index_count / 3,
        primitive_count: primitives.len(),
        instance_count: instances.len(),
        bounds,
    };
    Ok(DecodedModel {
        primitives,
        instances,
        info,
    })
}

fn visit_node(
    node: gltf::Node<'_>,
    parent: Mat4,
    mesh_primitives: &[Vec<usize>],
    instances: &mut Vec<DecodedInstance>,
) -> Result<(), ModelError> {
    if node.skin().is_some() || node.weights().is_some() {
        return Err(ModelError::UnsupportedDeformation);
    }
    let local = Mat4::from_cols_array_2d(&node.transform().matrix());
    let world = parent * local;
    if !world.is_finite() {
        return Err(ModelError::NonFinite);
    }
    if let Some(mesh) = node.mesh() {
        for &primitive_index in &mesh_primitives[mesh.index()] {
            instances.push(DecodedInstance {
                primitive_index,
                local_transform: world,
            });
        }
    }
    for child in node.children() {
        visit_node(child, world, mesh_primitives, instances)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{decode_model, face_normal, normal_to_rgb, ModelError, PerspectiveCamera};
    use glam::{Vec3, Vec4};
    use std::path::PathBuf;

    fn fixture_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("vse-3d-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn triangle_bytes(with_indices: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        if with_indices {
            bytes.extend_from_slice(
                &[0_u16, 1, 2]
                    .into_iter()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            );
        }
        bytes
    }

    fn write_glb(name: &str, indices: [u16; 3]) -> PathBuf {
        let dir = fixture_dir(name);
        let mut bin = triangle_bytes(false);
        for index in indices {
            bin.extend_from_slice(&index.to_le_bytes());
        }
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        let mut json = br#"{
          "asset":{"version":"2.0"},
          "buffers":[{"byteLength":42}],
          "bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":36},{"buffer":0,"byteOffset":36,"byteLength":6}],
          "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},{"bufferView":1,"componentType":5123,"count":3,"type":"SCALAR"}],
          "meshes":[{"primitives":[{"attributes":{"POSITION":0},"indices":1}]}],
          "nodes":[{"mesh":0}],"scenes":[{"nodes":[0]}],"scene":0
        }"#.to_vec();
        while json.len() % 4 != 0 {
            json.push(b' ');
        }
        let total = 12 + 8 + json.len() + 8 + bin.len();
        let mut glb = Vec::with_capacity(total);
        glb.extend_from_slice(&0x4654_6C67_u32.to_le_bytes());
        glb.extend_from_slice(&2_u32.to_le_bytes());
        glb.extend_from_slice(&(total as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x4E4F_534A_u32.to_le_bytes());
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x004E_4942_u32.to_le_bytes());
        glb.extend_from_slice(&bin);
        let path = dir.join("mesh.glb");
        std::fs::write(&path, glb).unwrap();
        path
    }

    fn write_external_gltf(name: &str, mode: u32) -> PathBuf {
        let dir = fixture_dir(name);
        std::fs::write(dir.join("mesh.bin"), triangle_bytes(false)).unwrap();
        let json = format!(
            r#"{{
          "asset": {{"version": "2.0", "generator": "VSE test"}},
          "buffers": [{{"uri": "mesh.bin", "byteLength": 36}}],
          "bufferViews": [{{"buffer": 0, "byteOffset": 0, "byteLength": 36}}],
          "accessors": [{{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [1,1,0]}}],
          "meshes": [{{"primitives": [{{"attributes": {{"POSITION": 0}}, "mode": {mode}}}]}}],
          "nodes": [{{"translation": [1,0,0], "children": [1]}}, {{"mesh": 0, "translation": [1,0,0]}}, {{"mesh": 0, "translation": [-1,0,0]}}],
          "scenes": [{{"nodes": [0,2]}}], "scene": 0
        }}"#
        );
        let path = dir.join("mesh.gltf");
        std::fs::write(&path, json).unwrap();
        path
    }

    #[test]
    fn cardinal_normals_map_to_rgb() {
        assert_eq!(normal_to_rgb(Vec3::X), Vec3::new(1.0, 0.5, 0.5));
        assert_eq!(normal_to_rgb(Vec3::NEG_X), Vec3::new(0.0, 0.5, 0.5));
        assert_eq!(normal_to_rgb(Vec3::Y), Vec3::new(0.5, 1.0, 0.5));
        assert_eq!(normal_to_rgb(Vec3::NEG_Y), Vec3::new(0.5, 0.0, 0.5));
        assert_eq!(normal_to_rgb(Vec3::Z), Vec3::new(0.5, 0.5, 1.0));
        assert_eq!(normal_to_rgb(Vec3::NEG_Z), Vec3::new(0.5, 0.5, 0.0));
    }

    #[test]
    fn counter_clockwise_xy_triangle_faces_positive_z() {
        let normal = face_normal(Vec3::ZERO, Vec3::X, Vec3::Y).unwrap();
        assert_eq!(normal, Vec3::Z);
    }

    #[test]
    fn camera_rejects_invalid_frustum() {
        assert!(PerspectiveCamera {
            near: 0.0,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(PerspectiveCamera {
            near: 0.1,
            far: 0.1,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(PerspectiveCamera {
            vertical_fov_radians: std::f32::consts::PI,
            ..Default::default()
        }
        .validate()
        .is_err());
    }

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
    fn external_gltf_generates_indices_and_preserves_repeated_instances() {
        let path = write_external_gltf("instances", 4);
        let decoded = decode_model(&path).unwrap();
        assert_eq!(decoded.primitives[0].indices, [0, 1, 2]);
        assert_eq!(decoded.instances.len(), 2);
        assert_eq!(decoded.info.generator.as_deref(), Some("VSE test"));
        assert_eq!(decoded.info.bounds.min, Vec3::new(-1.0, 0.0, 0.0));
        assert_eq!(decoded.info.bounds.max, Vec3::new(3.0, 1.0, 0.0));
        assert_eq!(decoded.info.triangle_count, 1);
        assert_eq!(decoded.info.source_sha256.len(), 64);
    }

    #[test]
    fn indexed_glb_is_decoded() {
        let decoded = decode_model(&write_glb("indexed-glb", [0, 1, 2])).unwrap();
        assert_eq!(decoded.primitives[0].indices, [0, 1, 2]);
        assert_eq!(decoded.info.triangle_count, 1);
    }

    #[test]
    fn out_of_range_index_is_rejected() {
        assert!(matches!(
            decode_model(&write_glb("bad-index", [0, 1, 3])),
            Err(ModelError::IndexOutOfBounds { index: 3, .. })
        ));
    }

    #[test]
    fn non_triangle_topology_is_rejected() {
        let path = write_external_gltf("lines", 1);
        assert!(matches!(
            decode_model(&path),
            Err(ModelError::UnsupportedTopology)
        ));
    }
}
