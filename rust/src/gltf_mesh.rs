//! The meshes of a glTF 2.0 file as Parts seen from the front: the mesh import prototype of issue #244.
//!
//! Each triangle primitive gives one Part. A vertex keeps its x and y, its z becomes a depth, and its skin weights
//! become weights on the standard bones. What a flat picture cannot hold is counted in the loss report, and
//! `doc/3d-mesh-import-prototype.md` has the measured result. The file is untrusted.

use crate::document::TextureBlob;
use crate::gltf_rig::{self, index, list, named, Affine, Report, Skeleton};
use crate::mesh_edit::MAX_VERTICES;
use base64::Engine;
use serde_json::Value;
use std::array::from_fn;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

/// The whole file: its JSON, its binary chunk and what its data URIs hold. Its images together hold no more.
pub(crate) const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
// ponytail: fixed budgets for one file, so that a small file which names the same data many times cannot ask for
// unbounded work. They count what is read, a dropped primitive too. Raise them when a real model needs more.
const MAX_PRIMITIVES: usize = 256;
/// The pixels of the images that are decoded: eight of the largest texture.
const MAX_FILE_PIXELS: usize = 8 * crate::render::MAX_TEXTURE_PIXELS as usize;
const MAX_FILE_VERTICES: usize = 4 * MAX_VERTICES;
const MAX_FILE_CORNERS: usize = 3 * 262_144;
const BIN_CHUNK: u32 = 0x004E_4942;
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const UNDECODED: &str = "its image does not decode within the limits of a texture";

/// One primitive as a Part.
pub(crate) struct Mesh {
    pub(crate) name: String,
    /// In rig axes and source units: x to the right, y down, z toward the viewer.
    pub(crate) vertices: Vec<[f64; 3]>,
    pub(crate) uvs: Vec<[f64; 2]>,
    /// The triangles that are drawn, the farthest first.
    pub(crate) indices: Vec<u32>,
    /// Index into `Meshes::images`.
    pub(crate) image: usize,
    /// The base colour factor, linear, with the alpha that is drawn.
    pub(crate) factor: [f64; 4],
    /// The weight of each vertex on each standard bone that moves one. Empty without a usable skin.
    pub(crate) weights: BTreeMap<&'static str, Vec<f64>>,
}

pub(crate) struct Meshes {
    pub(crate) parts: Vec<Mesh>,
    /// PNG files.
    pub(crate) images: Vec<Vec<u8>>,
    pub(crate) report: Report,
}

/// The JSON of a glTF file and the binary chunk of a `.glb`.
pub(crate) fn read_file(file: impl Read) -> Result<(Vec<u8>, Option<Vec<u8>>), String> {
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read glTF file: {error}"))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err("glTF file is too large".into());
    }
    let word = |at: usize| bytes.get(at..at + 4).map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]));
    let (json, bin) = if bytes.starts_with(b"glTF") {
        let json = match (word(4), word(12), word(16)) {
            (Some(2), Some(length), Some(gltf_rig::JSON_CHUNK)) => bytes.get(20..20 + length as usize),
            _ => return Err("binary glTF must be version 2 and start with a JSON chunk".into()),
        };
        let json = json.ok_or("binary glTF JSON chunk is cut short")?;
        // A chunk is padded to four bytes, so the next one starts where this one ends.
        let next = 20 + json.len();
        let bin = match (word(next), word(next + 4)) {
            (Some(length), Some(BIN_CHUNK)) => {
                Some(bytes.get(next + 8..next + 8 + length as usize).ok_or("binary glTF BIN chunk is cut short")?)
            }
            _ => None,
        };
        (json, bin.map(<[u8]>::to_vec))
    } else {
        (bytes.as_slice(), None)
    };
    if json.len() > gltf_rig::MAX_JSON_BYTES {
        return Err("glTF JSON is too large".into());
    }
    Ok((json.to_vec(), bin))
}

/// What the file has cost so far, and the images it has already shown.
#[derive(Default)]
struct Spent {
    primitives: usize,
    vertices: usize,
    corners: usize,
    image_bytes: usize,
    image_pixels: usize,
    /// For each image that was read: its index into `Meshes::images`, or why its primitives are dropped.
    slots: BTreeMap<usize, Result<usize, String>>,
}

/// Adds `cost` to `spent` and refuses the file when that is more than `most`.
fn spend(spent: &mut usize, cost: usize, most: usize, what: &str) -> Result<(), String> {
    *spent = spent.saturating_add(cost);
    if *spent > most {
        return Err(format!("the file has more than {most} {what} to read"));
    }
    Ok(())
}

/// What a `data:` URI holds.
fn embedded(uri: &Value) -> Result<Vec<u8>, String> {
    // ponytail: a file beside the glTF is not read. ADR 0003 has the rule for a relative path; add it with the
    // first real file that needs one.
    let data = uri.as_str().and_then(|uri| uri.strip_prefix("data:")).and_then(|rest| rest.split_once(";base64,"));
    let (_, data) = data.ok_or("a buffer or image outside the glTF file is not read")?;
    let decoded = base64::engine::general_purpose::STANDARD.decode(data);
    decoded.map_err(|_| "a data URI of the glTF file is not base64".into())
}

fn present(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !value.is_null())
}

/// A byte count or offset of the file. One that is not stated is 0.
fn size(value: Option<&Value>) -> Result<usize, String> {
    let refusal = "a count or offset of the glTF file is not a whole number";
    let Some(value) = present(value) else { return Ok(0) };
    value.as_u64().and_then(|size| usize::try_from(size).ok()).ok_or(refusal.into())
}

fn buffers(root: &Value, mut bin: Option<Vec<u8>>) -> Result<Vec<Vec<u8>>, String> {
    let mut total = 0;
    let mut out = Vec::new();
    for (at, buffer) in list(&root["buffers"]).iter().enumerate() {
        let mut bytes = match present(buffer.get("uri")) {
            Some(uri) => embedded(uri)?,
            None => bin.take().filter(|_| at == 0).ok_or("a buffer of the glTF file has no data")?,
        };
        total += bytes.len();
        if total > MAX_FILE_BYTES {
            return Err("the buffers of the glTF file are too large".into());
        }
        let length = size(buffer.get("byteLength"))?;
        if length > bytes.len() {
            return Err("a buffer is shorter than its byteLength".into());
        }
        bytes.truncate(length);
        out.push(bytes);
    }
    Ok(out)
}

/// The bytes of buffer view `at`, which lie inside its buffer, and its stride.
fn view<'a>(root: &Value, buffers: &'a [Vec<u8>], at: &Value) -> Result<(&'a [u8], usize), String> {
    let views = list(&root["bufferViews"]);
    let view = &views[index(at, views.len()).ok_or("the glTF file names a buffer view that does not exist")?];
    let buffer = index(&view["buffer"], buffers.len()).ok_or("a buffer view names a buffer that does not exist")?;
    let (offset, length) = (size(view.get("byteOffset"))?, size(view.get("byteLength"))?);
    let bytes = offset.checked_add(length).and_then(|end| buffers[buffer].get(offset..end));
    Ok((bytes.ok_or("a buffer view is outside its buffer")?, size(view.get("byteStride"))?))
}

/// An accessor whose every element lies inside its buffer view.
struct Access<'a> {
    bytes: &'a [u8],
    stride: usize,
    count: usize,
    component: u64,
    normalized: bool,
}

fn component_size(component: u64) -> usize {
    match component {
        5121 => 1,
        5123 => 2,
        _ => 4,
    }
}

/// Accessor `at`, which must have the type `kind` and one of the component types `components`: unsigned byte
/// 5121, unsigned short 5123, unsigned int 5125 or float 5126.
fn access<'a>(
    root: &Value,
    buffers: &'a [Vec<u8>],
    at: &Value,
    kind: &str,
    components: &[u64],
) -> Result<Access<'a>, String> {
    let accessors = list(&root["accessors"]);
    let accessor = &accessors[index(at, accessors.len()).ok_or("a primitive names an accessor that does not exist")?];
    let component = accessor["componentType"].as_u64().unwrap_or(0);
    if accessor["type"] != kind || !components.contains(&component) {
        return Err(format!("an accessor is not a {kind} of a component type that is read"));
    }
    if present(accessor.get("sparse")).is_some() {
        return Err("sparse accessors are not read".into());
    }
    let width = match kind {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" => 4,
        _ => 16,
    };
    let element = width * component_size(component);
    let (bytes, stride) = view(root, buffers, &accessor["bufferView"])?;
    let stride = if stride == 0 { element } else { stride };
    let (offset, count) = (size(accessor.get("byteOffset"))?, size(accessor.get("count"))?);
    // The last element ends inside the view.
    let last = count.saturating_sub(1).checked_mul(stride).and_then(|last| last.checked_add(element));
    let end = last.and_then(|last| last.checked_add(offset));
    if stride < element || count > 0 && end.map_or(true, |end| end > bytes.len()) {
        return Err("an accessor is outside its buffer view".into());
    }
    let bytes: &[u8] = if count == 0 { &[] } else { &bytes[offset..] };
    Ok(Access { bytes, stride, count, component, normalized: accessor["normalized"] == true })
}

impl Access<'_> {
    /// Component `part` of element `item`. A normalised integer is a fraction of its largest value.
    fn number(&self, item: usize, part: usize) -> f64 {
        let at = item * self.stride + part * component_size(self.component);
        let b = &self.bytes[at..];
        let (value, most) = match self.component {
            5121 => (f64::from(b[0]), 255.0),
            5123 => (f64::from(u16::from_le_bytes([b[0], b[1]])), 65_535.0),
            5125 => (f64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])), 1.0),
            _ => (f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])), 1.0),
        };
        if self.normalized {
            value / most
        } else {
            value
        }
    }

    /// Element `item`, which must be below `count`.
    fn row<const N: usize>(&self, item: usize) -> Result<[f64; N], String> {
        let row: [f64; N] = from_fn(|part| self.number(item, part));
        if row.iter().all(|number| number.is_finite()) {
            Ok(row)
        } else {
            Err("an accessor holds a number that is not finite".into())
        }
    }

    fn rows<const N: usize>(&self) -> Result<Vec<[f64; N]>, String> {
        (0..self.count).map(|item| self.row(item)).collect()
    }
}

/// The standard bone that moves each node: its own, or that of the nearest node above it that has one.
fn standard_bones(nodes: &[Value], skeleton: &Skeleton) -> Vec<Option<&'static str>> {
    let mut parent = vec![None; nodes.len()];
    for (at, node) in nodes.iter().enumerate() {
        for child in list(&node["children"]).iter().filter_map(|child| index(child, nodes.len())) {
            parent[child] = Some(at);
        }
    }
    let mut bones: Vec<Option<Option<&'static str>>> = vec![None; nodes.len()];
    for start in 0..nodes.len() {
        // The graph is a forest (`world_transforms`), so the walk up ends, and no node is walked twice.
        let (mut path, mut at, mut found) = (Vec::new(), Some(start), None);
        while let Some(node) = at {
            path.push(node);
            if let Some(known) = bones[node].or_else(|| skeleton.joints.get(&node).map(|bone| Some(*bone))) {
                found = known;
                break;
            }
            at = parent[node];
        }
        for node in path {
            bones[node] = Some(found);
        }
    }
    bones.into_iter().map(Option::flatten).collect()
}

/// The PNG of image `at`, or why its primitives are dropped. An error refuses the file.
fn png(root: &Value, buffers: &[Vec<u8>], at: usize, spent: &mut Spent) -> Result<Result<Vec<u8>, String>, String> {
    let image = &list(&root["images"])[at];
    let bytes = match present(image.get("uri")) {
        // An image beside the file is a common layout, so it is listed and the other primitives are imported.
        Some(uri) => match embedded(uri) {
            Ok(bytes) => bytes,
            Err(reason) => return Ok(Err(reason)),
        },
        None => view(root, buffers, &image["bufferView"])?.0.to_vec(),
    };
    spend(&mut spent.image_bytes, bytes.len(), MAX_FILE_BYTES, "bytes of images")?;
    // ponytail: PNG only. `jpeg.rs` decodes JPEG; route it through there when a real file needs it.
    if !bytes.starts_with(PNG_SIGNATURE) {
        return Ok(Err("its image is not a PNG".into()));
    }
    // The header is read before the picture: a PNG of a few bytes may state the largest texture, and many images
    // may name it.
    let field = |at: usize| bytes.get(at..at + 4).map(|b| u64::from(u32::from_be_bytes([b[0], b[1], b[2], b[3]])));
    let pixels = field(16).zip(field(20)).filter(|_| bytes.get(12..16) == Some(b"IHDR")).map(|(w, h)| w * h);
    let Some(pixels) = pixels else {
        return Ok(Err(UNDECODED.into()));
    };
    if !(1..=crate::render::MAX_TEXTURE_PIXELS).contains(&pixels) {
        return Ok(Err("its image is empty or larger than a texture can be".into()));
    }
    spend(&mut spent.image_pixels, pixels as usize, MAX_FILE_PIXELS, "pixels of images")?;
    let blob = TextureBlob { tag: 0, data: bytes };
    if crate::render::decode_stored_texture(&blob, 0).is_err() {
        return Ok(Err(UNDECODED.into()));
    }
    Ok(Ok(blob.data))
}

struct Source<'a> {
    root: &'a Value,
    buffers: &'a [Vec<u8>],
    nodes: &'a [Value],
    /// The standard bone that moves each node.
    bones: Vec<Option<&'static str>>,
    signs: [f64; 3],
    /// The nodes that are a standard bone themselves.
    standard: &'a BTreeMap<usize, &'static str>,
}

/// The skin of a mesh node: its joints, and where its bind pose puts a vertex.
struct Skin<'a> {
    joints: &'a [Value],
    bind: Affine,
}

impl<'a> Source<'a> {
    /// Where `skin` puts a vertex of its meshes: the node of its first joint times the inverse bind matrix of that
    /// joint. glTF blends this over the joints of a vertex. At the bind pose every joint gives the same place, and
    /// a skin whose joints differ is listed.
    fn bind(&self, skin: &Value, name: &str, world: &[Affine], report: &mut Report) -> Result<Affine, String> {
        let joints = list(&skin["joints"]);
        let inverse = present(skin.get("inverseBindMatrices"))
            .map(|at| access(self.root, self.buffers, at, "MAT4", &[5126]))
            .transpose()?;
        if inverse.as_ref().is_some_and(|inverse| inverse.count < joints.len()) {
            return Err("a skin has fewer inverse bind matrices than joints".into());
        }
        let (mut first, mut posed) = (None::<Affine>, false);
        for (at, joint) in joints.iter().enumerate() {
            let node = index(joint, self.nodes.len()).ok_or("a skin names a joint that does not exist")?;
            // Without inverse bind matrices each of them is the identity.
            let place = match &inverse {
                Some(inverse) => world[node].then(&Affine::from_matrix(inverse.row(at)?))?,
                None => world[node],
            };
            // The joints of a model are as far apart as the model is large.
            let first = first.get_or_insert(place);
            posed |= !first.near(&place, first.size().max(world[node].size()));
        }
        if posed {
            let reason = "the nodes of the skin are not at its bind pose, and its meshes are placed by its first joint";
            report.add("mesh.geometry", name, "approximated", reason);
        }
        Ok(first.unwrap_or(Affine::IDENTITY))
    }

    /// Primitive `primitive` of a mesh as a Part, or `None` with the reason in `out.report`. An error refuses the
    /// file.
    fn part(
        &self,
        primitive: &Value,
        name: &str,
        place: &Affine,
        skin: Option<&Skin>,
        spent: &mut Spent,
        out: &mut Meshes,
    ) -> Result<Option<Mesh>, String> {
        let (root, buffers) = (self.root, self.buffers);
        let drop = |report: &mut Report, reason: &str| -> Result<Option<Mesh>, String> {
            report.add("mesh.geometry", name, "dropped", reason);
            Ok(None)
        };
        let attributes = &primitive["attributes"];
        // glTF defines far fewer, and each one that is not read becomes a report item.
        if attributes.as_object().is_some_and(|attributes| attributes.len() > 64) {
            return Err("a primitive has more than 64 attributes".into());
        }
        if present(primitive.get("mode")).is_some_and(|mode| mode.as_u64() != Some(4)) {
            return drop(&mut out.report, "only triangle lists are read");
        }
        let (Some(position), Some(uv)) = (present(attributes.get("POSITION")), present(attributes.get("TEXCOORD_0")))
        else {
            return drop(&mut out.report, "it has no positions or no texture coordinates");
        };
        let position = access(root, buffers, position, "VEC3", &[5126])?;
        if position.count > MAX_VERTICES {
            return drop(&mut out.report, "it has more than 65535 vertices");
        }
        spend(&mut spent.vertices, position.count, MAX_FILE_VERTICES, "vertices")?;
        let counted = |other: Access<'a>| -> Result<Access<'a>, String> {
            if other.count == position.count {
                Ok(other)
            } else {
                Err("an attribute has another count than the positions".into())
            }
        };
        let uv = counted(access(root, buffers, uv, "VEC2", &[5126, 5121, 5123])?)?.rows::<2>()?;

        let materials = list(&root["materials"]);
        let material = present(primitive.get("material"))
            .map(|at| index(at, materials.len()).ok_or("a primitive names a material that does not exist"))
            .transpose()?
            .map(|at| &materials[at]);
        let colour = material.map(|material| &material["pbrMetallicRoughness"]);
        let Some(texture) = colour.and_then(|colour| present(colour.get("baseColorTexture"))) else {
            return drop(&mut out.report, "it has no base colour texture");
        };
        if present(texture.get("texCoord")).is_some_and(|set| set.as_u64() != Some(0)) {
            return drop(&mut out.report, "its base colour texture uses another set of texture coordinates");
        }
        // `KHR_texture_transform` is an extension of a texture reference and moves the texture coordinates.
        if present(texture.get("extensions")).is_some() {
            return drop(&mut out.report, "its base colour texture has an extension, which is not read");
        }
        let textures = list(&root["textures"]);
        let texture = index(&texture["index"], textures.len()).ok_or("a material names a texture that does not exist")?;
        let Some(image) = index(&textures[texture]["source"], list(&root["images"]).len()) else {
            return drop(&mut out.report, "its texture has no image that is read");
        };
        let mut factor = match colour.and_then(|colour| present(colour.get("baseColorFactor"))) {
            None => [1.0; 4],
            Some(factor) => {
                // The crate keeps the text of a number, which a file can make as long as it likes.
                let part = |part: &Value| match part {
                    Value::Number(number) if number.as_str().len() <= 32 => number.as_f64(),
                    _ => None,
                };
                let stated = match list(factor) {
                    [r, g, b, a] => [part(r), part(g), part(b), part(a)],
                    _ => [None; 4],
                };
                let stated = stated.map(|part| part.filter(|part| (0.0..=1.0).contains(part)));
                let [Some(r), Some(g), Some(b), Some(a)] = stated else {
                    return Err("a base colour factor must hold 4 numbers from 0 to 1".into());
                };
                [r, g, b, a]
            }
        };
        // The alpha of the factor counts for a blended or masked material only. A material is opaque by default.
        if !material.is_some_and(|material| matches!(material["alphaMode"].as_str(), Some("BLEND" | "MASK"))) {
            factor[3] = 1.0;
        }

        // A node that mirrors its mesh shows the other side of each triangle as the front.
        let mirrored = place.mirrors();
        // A skinned vertex is placed by its skin and not by the node of its mesh.
        let place = skin.map_or(place, |skin| &skin.bind);
        let all: Vec<[f64; 3]> = position
            .rows::<3>()?
            .into_iter()
            .map(|point| place.apply(point))
            .map(|point| from_fn(|axis| self.signs[axis] * point[axis]))
            .collect();
        if !all.iter().flatten().all(|number| number.is_finite()) {
            return Err("a mesh vertex is outside the finite range".into());
        }
        let corners: Vec<usize> = match present(primitive.get("indices")) {
            None => {
                spend(&mut spent.corners, all.len(), MAX_FILE_CORNERS, "triangle corners")?;
                (0..all.len()).collect()
            }
            Some(at) => {
                let indices = access(root, buffers, at, "SCALAR", &[5121, 5123, 5125])?;
                spend(&mut spent.corners, indices.count, MAX_FILE_CORNERS, "triangle corners")?;
                (0..indices.count).map(|item| indices.number(item, 0) as usize).collect()
            }
        };
        if corners.len() % 3 != 0 || corners.iter().any(|corner| *corner >= all.len()) {
            return Err("a triangle list is cut short or names a vertex that does not exist".into());
        }
        let double = material.is_some_and(|material| material["doubleSided"] == true);
        // With y down, a triangle that shows its front to the viewer turns the other way round than in the file.
        let mut faces: Vec<(&[usize], f64)> = corners
            .chunks_exact(3)
            .filter(|face| {
                let [a, b, c] = [all[face[0]], all[face[1]], all[face[2]]];
                let turn = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
                let turn = if mirrored { -turn } else { turn };
                turn < 0.0 || double && turn > 0.0
            })
            .map(|face| (face, face.iter().map(|corner| all[*corner][2]).sum::<f64>()))
            .collect();
        let (total, drawn) = (corners.len() / 3, faces.len());
        if drawn == 0 {
            return drop(&mut out.report, "no triangle of it shows its front");
        }
        if !spent.slots.contains_key(&image) {
            let read = png(root, buffers, image, spent)?.map(|bytes| {
                out.images.push(bytes);
                let source = named(&list(&root["images"])[image], "image", image);
                out.report.add("mesh.texture", &source, "converted", "");
                out.images.len() - 1
            });
            spent.slots.insert(image, read);
        }
        let slot = match &spent.slots[&image] {
            Ok(slot) => *slot,
            Err(reason) => return drop(&mut out.report, reason),
        };
        // The painter draws in this order, so a nearer triangle covers a farther one.
        faces.sort_by(|a, b| a.1.total_cmp(&b.1));
        let mut renumbered = vec![u32::MAX; all.len()];
        let mut kept = Vec::new();
        let mut indices = Vec::with_capacity(drawn * 3);
        for corner in faces.iter().flat_map(|(face, _)| face.iter()) {
            if renumbered[*corner] == u32::MAX {
                renumbered[*corner] = kept.len() as u32;
                kept.push(*corner);
            }
            indices.push(renumbered[*corner]);
        }

        let mut weights = BTreeMap::<&'static str, Vec<f64>>::new();
        if let Some(skin) = skin {
            let sets = (present(attributes.get("JOINTS_0")), present(attributes.get("WEIGHTS_0")));
            let outcome = match sets {
                (Some(joints), Some(influence)) => {
                    let joints = counted(access(root, buffers, joints, "VEC4", &[5121, 5123])?)?;
                    let influence = counted(access(root, buffers, influence, "VEC4", &[5126, 5121, 5123])?)?;
                    self.weights(skin, &kept, &joints, &influence, &mut weights)?
                }
                _ => ("dropped", "its mesh has a skin and no JOINTS_0 and WEIGHTS_0".to_owned()),
            };
            out.report.add("mesh.skinWeights", name, outcome.0, &outcome.1);
        }

        // A Part draws nothing outside its texture, where the sampler of the file repeats or clamps it.
        if kept.iter().any(|old| uv[*old].iter().any(|part| !(0.0..=1.0).contains(part))) {
            let reason = "its texture coordinates leave the texture, and the sampler of the file is not read";
            out.report.add("mesh.uv", name, "approximated", reason);
        }
        let used = ["POSITION", "TEXCOORD_0", "JOINTS_0", "WEIGHTS_0"];
        let unused = attributes.as_object().into_iter().flatten().filter(|(name, _)| !used.contains(&name.as_str()));
        for (attribute, _) in unused {
            let source = format!("{name} {}", gltf_rig::label(attribute, 64));
            out.report.add("mesh.attribute", &source, "dropped", "a flat Part has no use for it");
        }
        // One item for a primitive, however many targets it has.
        let targets = list(&primitive["targets"]).len();
        let reason = format!("the prototype reads no morph target, and it has {targets}");
        out.report.add_times(targets, "morph.target", name, "dropped", &reason);
        let hidden = format!(
            "seen from the front with z as depth: {} of {total} triangles turn away or stand on edge and are not \
             drawn, the others are drawn far to near",
            total - drawn
        );
        out.report.add("mesh.geometry", name, "approximated", &hidden);
        Ok(Some(Mesh {
            name: name.to_owned(),
            vertices: kept.iter().map(|old| all[*old]).collect(),
            uvs: kept.iter().map(|old| uv[*old]).collect(),
            indices,
            image: slot,
            factor,
            weights,
        }))
    }

    /// Fills `weights` with the weight of each kept vertex on the standard bones and answers the outcome for the
    /// report. A joint that is no standard bone gives its weight to the standard bone above it, and the weight on a
    /// joint without one is left out.
    fn weights(
        &self,
        skin: &Skin,
        kept: &[usize],
        joints: &Access,
        influence: &Access,
        weights: &mut BTreeMap<&'static str, Vec<f64>>,
    ) -> Result<(&'static str, String), String> {
        let (mut moved, mut lost, mut bare) = (false, false, false);
        for (vertex, old) in kept.iter().enumerate() {
            let mut total = 0.0;
            for set in 0..4 {
                let weight = influence.number(*old, set);
                if !weight.is_finite() || weight < 0.0 {
                    return Err("a skin weight is not a finite number from 0 up".into());
                }
                if weight == 0.0 {
                    continue;
                }
                let joint = present(skin.joints.get(joints.number(*old, set) as usize));
                let node = joint.and_then(|joint| index(joint, self.nodes.len()));
                let node = node.ok_or("a vertex or a skin names a joint that does not exist")?;
                let Some(bone) = self.bones[node] else {
                    lost = true;
                    continue;
                };
                moved |= !self.standard.contains_key(&node);
                weights.entry(bone).or_insert_with(|| vec![0.0; kept.len()])[vertex] += weight;
                total += weight;
            }
            bare |= total == 0.0;
            if total > 0.0 {
                weights.values_mut().for_each(|row| row[vertex] /= total);
            }
        }
        if bare {
            weights.clear();
            return Ok(("dropped", "a vertex has no weight on a standard bone or on a bone below one".into()));
        }
        let mut notes = Vec::new();
        if moved {
            notes.push("the weight on a bone that is no standard bone goes to the standard bone above it");
        }
        if lost {
            notes.push("the weight on a bone without a standard bone above it is left out");
        }
        Ok((if notes.is_empty() { "converted" } else { "approximated" }, notes.join("; ")))
    }
}

/// The meshes of the glTF JSON `text`, whose humanoid is `skeleton`, and the loss report of the import.
pub(crate) fn meshes(text: &[u8], bin: Option<Vec<u8>>, skeleton: &Skeleton) -> Result<Meshes, String> {
    let root: Value = serde_json::from_slice(text).map_err(|error| format!("glTF JSON is not valid: {error}"))?;
    let buffers = buffers(&root, bin)?;
    let nodes = list(&root["nodes"]);
    let world = gltf_rig::world_transforms(nodes)?;
    let source = Source {
        root: &root,
        buffers: &buffers,
        nodes,
        bones: standard_bones(nodes, skeleton),
        signs: skeleton.convention.signs(),
        standard: &skeleton.joints,
    };
    let (meshes, skins) = (list(&root["meshes"]), list(&root["skins"]));
    let mut out = Meshes { parts: Vec::new(), images: Vec::new(), report: Report::default() };
    let mut spent = Spent::default();
    let mut binds = BTreeMap::new();
    let mut materials = BTreeSet::new();
    for (at, node) in nodes.iter().enumerate() {
        let Some(mesh) = present(node.get("mesh")) else {
            continue;
        };
        let mesh_at = index(mesh, meshes.len()).ok_or("a node names a mesh that does not exist")?;
        let skin = match present(node.get("skin")) {
            None => None,
            Some(skin) => {
                let skin_at = index(skin, skins.len()).ok_or("a node names a skin that does not exist")?;
                let skin = &skins[skin_at];
                // Many mesh nodes may name one skin, which is read once.
                let bind = match binds.get(&skin_at) {
                    Some(bind) => *bind,
                    None => {
                        let name = named(skin, "skin", skin_at);
                        *binds.entry(skin_at).or_insert(source.bind(skin, &name, &world, &mut out.report)?)
                    }
                };
                Some(Skin { joints: list(&skin["joints"]), bind })
            }
        };
        let primitives = list(&meshes[mesh_at]["primitives"]);
        // Cut short, so that a report item still shows the primitive and the attribute after the name.
        let mesh_name = gltf_rig::label(&named(&meshes[mesh_at], "mesh", mesh_at), 48);
        for (number, primitive) in primitives.iter().enumerate() {
            let name = if primitives.len() == 1 { mesh_name.clone() } else { format!("{mesh_name}.{number}") };
            spend(&mut spent.primitives, 1, MAX_PRIMITIVES, "primitives")?;
            let Some(part) = source.part(primitive, &name, &world[at], skin.as_ref(), &mut spent, &mut out)? else {
                continue;
            };
            if let Some(material) = present(primitive.get("material")).and_then(Value::as_u64) {
                materials.insert(material as usize);
            }
            out.parts.push(part);
        }
    }
    for material in materials {
        let name = named(&list(&root["materials"])[material], "material", material);
        let reason = "the base colour texture and factor are kept; shading is dropped, and alpha is always blended";
        out.report.add("material", &name, "approximated", reason);
    }
    Ok(out)
}
