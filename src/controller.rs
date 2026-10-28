//! Controller poses and static glTF geometry shared by XR and the spectator.
use anyhow::{Context, Result, ensure};
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io::Cursor};

pub const LEFT: usize = 0;
pub const RIGHT: usize = 1;
const MAX_MODEL_BYTES: usize = 8 * 1024 * 1024;
const MAX_VERTICES: usize = 300_000;
const MAX_NODES: usize = 10_000;
const MAX_TEXTURE_BYTES: usize = 64 * 1024 * 1024;
const MAX_TEXTURES: usize = 8;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Pose {
    pub position: [f32; 3],
    pub orientation: [f32; 4],
}

impl Pose {
    pub fn valid(&self) -> bool {
        let q = Quat::from_array(self.orientation);
        self.position
            .iter()
            .all(|n| n.is_finite() && n.abs() < 10_000.0)
            && q.is_finite()
            && (0.5..=1.5).contains(&q.length())
    }

    pub fn transform(&self) -> Mat4 {
        Mat4::from_rotation_translation(
            Quat::from_array(self.orientation).normalize(),
            Vec3::from_array(self.position),
        )
    }

    pub fn from_transform(transform: Mat4) -> Option<Self> {
        if !transform.is_finite() {
            return None;
        }
        let (_, rotation, position) = transform.to_scale_rotation_translation();
        let pose = Self {
            position: position.to_array(),
            orientation: rotation.to_array(),
        };
        pose.valid().then_some(pose)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 3],
    pub uv: [f32; 2],
}

pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub struct Part {
    pub start: u32,
    pub count: u32,
    pub texture: Option<usize>,
}

pub struct Model {
    pub vertices: Vec<Vertex>,
    pub parts: Vec<Part>,
    pub textures: Vec<Texture>,
}

fn gltf_document(bytes: &[u8]) -> Result<gltf::Gltf> {
    let raw =
        gltf::Gltf::from_slice_without_validation(bytes).context("parsing controller glTF")?;
    let mut json = raw.document.into_json();
    // gltf-rs doesn't yet recognise KHR_texture_basisu. Map its image index
    // into the normal texture source, then retain all other validation.
    for texture in &mut json.textures {
        if let Some(basis) = texture
            .extensions
            .as_ref()
            .and_then(|ext| ext.others.get("KHR_texture_basisu"))
        {
            let source = basis
                .get("source")
                .and_then(|value| value.as_u64())
                .context("invalid Basis texture source")?;
            texture.source = gltf::json::Index::new(
                u32::try_from(source).context("Basis texture index too large")?,
            );
        }
    }
    json.extensions_required
        .retain(|name| name != "KHR_texture_basisu");
    let document = gltf::Document::from_json(json).context("validating controller glTF")?;
    Ok(gltf::Gltf {
        document,
        blob: raw.blob,
    })
}

fn decode_texture(encoded: &[u8], budget: usize) -> Result<Texture> {
    const KTX2_MAGIC: &[u8] = b"\xABKTX 20\xBB\r\n\x1A\n";
    if encoded.starts_with(KTX2_MAGIC) {
        let decoder = basisu::Transcoder::new(encoded)
            .map_err(|error| anyhow::anyhow!("invalid KTX2 texture: {error:?}"))?;
        let (width, height) = decoder.base_dimensions();
        ensure!(
            width > 0 && height > 0 && width <= 4096 && height <= 4096,
            "invalid controller texture dimensions"
        );
        let size = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .context("controller texture size overflow")?;
        ensure!(size <= budget, "controller textures exceed size limit");
        let rgba = decoder
            .transcode(0, basisu::TargetFormat::Rgba32, basisu::DecodeFlags::NONE)
            .map_err(|error| anyhow::anyhow!("KTX2 transcode failed: {error:?}"))?;
        ensure!(rgba.len() == size, "invalid KTX2 output size");
        Ok(Texture {
            width,
            height,
            rgba,
        })
    } else {
        let mut reader = image::ImageReader::new(Cursor::new(encoded)).with_guessed_format()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(4096);
        limits.max_image_height = Some(4096);
        limits.max_alloc = Some(budget as u64);
        reader.limits(limits);
        let image = reader
            .decode()
            .context("decoding controller texture")?
            .to_rgba8();
        let (width, height) = image.dimensions();
        let rgba = image.into_raw();
        ensure!(
            rgba.len() <= budget,
            "controller textures exceed size limit"
        );
        Ok(Texture {
            width,
            height,
            rgba,
        })
    }
}

/// Extract static, unskinned triangle geometry and base-color textures from a
/// GLB. Unsupported assets fall back to the bundled model; animations are not
/// yet driven by controller buttons.
pub fn parse_glb(bytes: &[u8]) -> Result<Model> {
    ensure!(
        bytes.len() <= MAX_MODEL_BYTES,
        "controller model exceeds size limit"
    );
    let document = gltf_document(bytes)?;
    let blob = document
        .blob
        .as_deref()
        .context("controller glTF has no embedded binary buffer")?;
    let scene = document
        .default_scene()
        .or_else(|| document.scenes().next())
        .context("controller glTF has no scene")?;
    let mut model = Model {
        vertices: Vec::new(),
        parts: Vec::new(),
        textures: Vec::new(),
    };
    let mut texture_indices = HashMap::new();
    let mut node_count = 0;
    for node in scene.nodes() {
        append_node(
            node,
            Mat4::IDENTITY,
            blob,
            &mut model,
            &mut texture_indices,
            &mut node_count,
            0,
        )?;
    }
    ensure!(
        !model.vertices.is_empty(),
        "controller glTF has no supported triangles"
    );
    Ok(model)
}

fn append_node(
    node: gltf::Node<'_>,
    parent: Mat4,
    blob: &[u8],
    model: &mut Model,
    texture_indices: &mut HashMap<usize, usize>,
    node_count: &mut usize,
    depth: usize,
) -> Result<()> {
    ensure!(depth < 64, "controller model node nesting exceeds limit");
    *node_count += 1;
    ensure!(
        *node_count <= MAX_NODES,
        "controller model has too many nodes"
    );
    let transform = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
    ensure!(
        transform.is_finite() && transform.determinant().abs() > 1e-8,
        "invalid controller node transform"
    );
    if let Some(mesh) = node.mesh() {
        for primitive in mesh.primitives() {
            if primitive.mode() != gltf::mesh::Mode::Triangles {
                continue;
            }
            let position_count = primitive
                .get(&gltf::Semantic::Positions)
                .context("controller model missing positions")?
                .count();
            ensure!(
                position_count <= MAX_VERTICES,
                "controller model has too many positions"
            );
            if let Some(indices) = primitive.indices() {
                ensure!(
                    indices.count() <= MAX_VERTICES.saturating_sub(model.vertices.len()),
                    "controller model has too many indices"
                );
            }
            let reader = primitive.reader(|buffer| match buffer.source() {
                gltf::buffer::Source::Bin => Some(blob),
                gltf::buffer::Source::Uri(_) => None,
            });
            let positions: Vec<_> = reader
                .read_positions()
                .context("controller model missing embedded positions")?
                .collect();
            ensure!(
                positions.len() <= MAX_VERTICES,
                "controller model has too many positions"
            );
            let normals: Option<Vec<_>> = reader.read_normals().map(|normals| normals.collect());
            if let Some(normals) = &normals {
                ensure!(
                    normals.len() == positions.len(),
                    "controller normal count mismatch"
                );
            }
            let indices: Vec<u32> = match reader.read_indices() {
                Some(indices) => indices.into_u32().collect(),
                None => (0..positions.len() as u32).collect(),
            };
            ensure!(
                indices.len().is_multiple_of(3)
                    && model.vertices.len() + indices.len() <= MAX_VERTICES,
                "invalid controller triangle count"
            );
            let material = primitive.material();
            let pbr = material.pbr_metallic_roughness();
            let factor = pbr.base_color_factor();
            let color = [factor[0], factor[1], factor[2]];
            ensure!(
                color.iter().all(|x| x.is_finite()),
                "invalid controller material"
            );
            let (texture, uvs) = if let Some(info) = pbr.base_color_texture() {
                let source = info.texture().source();
                let texture_index = if let Some(index) = texture_indices.get(&source.index()) {
                    *index
                } else {
                    ensure!(
                        model.textures.len() < MAX_TEXTURES,
                        "too many controller textures"
                    );
                    let view = match source.source() {
                        gltf::image::Source::View { view, .. } => view,
                        gltf::image::Source::Uri { .. } => {
                            anyhow::bail!("external controller texture is unsupported")
                        }
                    };
                    ensure!(
                        matches!(view.buffer().source(), gltf::buffer::Source::Bin),
                        "controller texture is not embedded"
                    );
                    let end = view
                        .offset()
                        .checked_add(view.length())
                        .context("controller texture length overflow")?;
                    let encoded = blob
                        .get(view.offset()..end)
                        .context("controller texture out of bounds")?;
                    let used: usize = model.textures.iter().map(|t| t.rgba.len()).sum();
                    let decoded = decode_texture(encoded, MAX_TEXTURE_BYTES - used)?;
                    let index = model.textures.len();
                    model.textures.push(decoded);
                    texture_indices.insert(source.index(), index);
                    index
                };
                let uvs: Vec<_> = reader
                    .read_tex_coords(info.tex_coord())
                    .context("controller texture missing UVs")?
                    .into_f32()
                    .collect();
                ensure!(
                    uvs.len() == positions.len() && uvs.iter().flatten().all(|v| v.is_finite()),
                    "invalid controller UVs"
                );
                (Some(texture_index), Some(uvs))
            } else {
                (None, None)
            };
            let start = model.vertices.len() as u32;
            let normal_matrix = transform.inverse().transpose();
            for triangle in indices.as_chunks::<3>().0 {
                let mut points = [Vec3::ZERO; 3];
                for (i, index) in triangle.iter().enumerate() {
                    let position = positions
                        .get(*index as usize)
                        .context("controller index out of bounds")?;
                    points[i] = transform.transform_point3(Vec3::from_array(*position));
                    ensure!(points[i].is_finite(), "non-finite controller vertex");
                }
                let face = (points[1] - points[0])
                    .cross(points[2] - points[0])
                    .normalize_or_zero();
                for (i, index) in triangle.iter().enumerate() {
                    let normal = normals
                        .as_ref()
                        .map(|n| {
                            normal_matrix
                                .transform_vector3(Vec3::from_array(n[*index as usize]))
                                .normalize_or_zero()
                        })
                        .unwrap_or(face);
                    ensure!(normal.is_finite(), "non-finite controller normal");
                    model.vertices.push(Vertex {
                        position: points[i].to_array(),
                        normal: normal.to_array(),
                        color,
                        uv: uvs.as_ref().map_or([0.0, 0.0], |uv| uv[*index as usize]),
                    });
                }
            }
            model.parts.push(Part {
                start,
                count: model.vertices.len() as u32 - start,
                texture,
            });
        }
    }
    for child in node.children() {
        append_node(
            child,
            transform,
            blob,
            model,
            texture_indices,
            node_count,
            depth + 1,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pose_round_trip_and_invalid_rotations() {
        let original =
            Mat4::from_rotation_translation(Quat::from_rotation_y(0.7), Vec3::new(1.0, 2.0, -3.0));
        let pose = Pose::from_transform(original).unwrap();
        assert!(pose.transform().abs_diff_eq(original, 1e-5));
        assert!(
            !Pose {
                position: [0.0; 3],
                orientation: [0.0; 4]
            }
            .valid()
        );
        assert!(
            !Pose {
                position: [f32::NAN, 0.0, 0.0],
                orientation: [0.0, 0.0, 0.0, 1.0]
            }
            .valid()
        );
    }

    #[test]
    fn rejects_invalid_and_oversized_models() {
        assert!(parse_glb(b"not a glb").is_err());
        assert!(parse_glb(&vec![0; MAX_MODEL_BYTES + 1]).is_err());
    }

    #[test]
    fn bundled_touch_plus_models_contain_reasonably_sized_geometry() {
        for bytes in [
            include_bytes!("../assets/controllers/quest-touch-plus-left.glb").as_slice(),
            include_bytes!("../assets/controllers/quest-touch-plus-right.glb").as_slice(),
        ] {
            let model = parse_glb(bytes).unwrap();
            let mesh = model.vertices;
            assert!(mesh.len() > 100 && mesh.len() < MAX_VERTICES);
            assert_eq!(model.textures.len(), 1);
            assert!(model.parts.iter().any(|part| part.texture.is_some()));
            for axis in 0..3 {
                let min = mesh
                    .iter()
                    .map(|v| v.position[axis])
                    .fold(f32::INFINITY, f32::min);
                let max = mesh
                    .iter()
                    .map(|v| v.position[axis])
                    .fold(f32::NEG_INFINITY, f32::max);
                assert!(max - min < 0.5, "controller geometry must be in metres");
            }
        }
    }

    #[test]
    fn transcodes_etc1s_and_uastc_ktx2_textures_with_bounded_output() {
        for bytes in [
            include_bytes!("../tests/fixtures/etc1s.ktx2").as_slice(),
            include_bytes!("../tests/fixtures/uastc.ktx2").as_slice(),
        ] {
            let texture = decode_texture(bytes, MAX_TEXTURE_BYTES).unwrap();
            assert!(texture.width > 0 && texture.height > 0);
            assert_eq!(
                texture.rgba.len(),
                texture.width as usize * texture.height as usize * 4
            );
            assert!(decode_texture(bytes, 1).is_err());
        }
    }

    #[test]
    fn reads_ktx2_basisu_texture_references_from_glb() {
        let texture = include_bytes!("../tests/fixtures/etc1s.ktx2");
        let mut bin = Vec::new();
        for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            bin.extend_from_slice(&value.to_le_bytes());
        }
        for value in [0.0_f32, 0.0, 1.0, 0.0, 0.0, 1.0] {
            bin.extend_from_slice(&value.to_le_bytes());
        }
        bin.extend_from_slice(texture);
        let json = serde_json::json!({
            "asset": {"version": "2.0"},
            "extensionsUsed": ["KHR_texture_basisu"], "extensionsRequired": ["KHR_texture_basisu"],
            "buffers": [{"byteLength": bin.len()}],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": 36},
                {"buffer": 0, "byteOffset": 36, "byteLength": 24},
                {"buffer": 0, "byteOffset": 60, "byteLength": texture.len()}
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [1,1,0]},
                {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC2"}
            ],
            "images": [{"bufferView": 2, "mimeType": "image/ktx2"}],
            "textures": [{"extensions": {"KHR_texture_basisu": {"source": 0}}}],
            "materials": [{"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "TEXCOORD_0": 1}, "material": 0}]}],
            "nodes": [{"mesh": 0}], "scenes": [{"nodes": [0]}], "scene": 0
        });
        let mut json = serde_json::to_vec(&json).unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let mut glb = Vec::new();
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2_u32.to_le_bytes());
        glb.extend_from_slice(&((12 + 8 + json.len() + 8 + bin.len()) as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(&bin);
        let model = parse_glb(&glb).unwrap();
        assert_eq!(model.vertices.len(), 3);
        assert_eq!(model.parts[0].texture, Some(0));
        assert_eq!(model.textures.len(), 1);
        assert_eq!(model.vertices[1].uv, [1.0, 0.0]);
    }

    #[test]
    fn loads_embedded_triangles_and_applies_node_transform() {
        let json = serde_json::json!({
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": 36}],
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 36}],
            "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [1,1,0]}],
            "materials": [{"pbrMetallicRoughness": {"baseColorFactor": [0.2,0.3,0.4,1.0]}}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "material": 0}]}],
            "nodes": [{"mesh": 0, "translation": [1,2,3]}],
            "scenes": [{"nodes": [0]}], "scene": 0
        });
        let mut json = serde_json::to_vec(&json).unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut bin = Vec::new();
        for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            bin.extend_from_slice(&value.to_le_bytes());
        }
        let mut glb = Vec::new();
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2_u32.to_le_bytes());
        glb.extend_from_slice(&((12 + 8 + json.len() + 8 + bin.len()) as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(&bin);
        let mesh = parse_glb(&glb).unwrap().vertices;
        assert_eq!(mesh.len(), 3);
        assert_eq!(mesh[0].position, [1.0, 2.0, 3.0]);
        assert_eq!(mesh[1].position, [2.0, 2.0, 3.0]);
        assert_eq!(mesh[0].normal, [0.0, 0.0, 1.0]);
        assert_eq!(mesh[0].color, [0.2, 0.3, 0.4]);
    }
}
