//! Export of an INX document as layered planes in glTF 2.0 (`export.gltf`, issue #245).
//!
//! Each drawn Part becomes one mesh at its rest shape, placed along Z in painter order, with its texture as an
//! embedded PNG. DepthBones become nodes, and a key of a one-axis parameter becomes a morph target. What glTF has no
//! counterpart for is listed in the loss report of `doc/3d-conversion-loss-map.md`. Coordinates follow
//! `doc/3d-conversion-conventions.md`.

use crate::bone_deform::{self, Mat4};
use crate::document::TextureBlob;
use crate::gltf_rig::{label, shown, Report, GLTF_ASSET_KEY, VRM_META_KEY};
use crate::{evaluation, render, scene_deform, skeleton};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

/// Buffer bytes one export may write. A `.gltf` holds them as base64, a third larger.
const MAX_BUFFER_BYTES: usize = 256 * 1024 * 1024;
/// Parameter keys that are evaluated for morph targets.
const MAX_TARGET_KEYS: usize = 64;
const PUPPET_META_KEY: &str = "io.github.geekjapan.soul2d.PuppetMeta";
const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;

pub(crate) struct Export {
    pub(crate) bytes: Vec<u8>,
    /// The loss report of `doc/3d-conversion-loss-map.md`, without the fields the session adds.
    pub(crate) report: Value,
    /// The document keeps rights data, which the caller has to acknowledge.
    pub(crate) rights: bool,
}

/// The one buffer of the file, with the views and accessors into it.
#[derive(Default)]
struct Buffer {
    bytes: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
}

impl Buffer {
    fn view(&mut self, bytes: &[u8], target: Option<u32>) -> Result<usize, String> {
        if bytes.len() > MAX_BUFFER_BYTES - self.bytes.len() {
            return Err(format!("the export is larger than {} MiB", MAX_BUFFER_BYTES >> 20));
        }
        let mut view = json!({ "buffer": 0, "byteOffset": self.bytes.len(), "byteLength": bytes.len() });
        if let Some(target) = target {
            view["target"] = json!(target);
        }
        self.bytes.extend_from_slice(bytes);
        // Every view starts on a multiple of four, which every component type here may start on.
        self.bytes.resize(self.bytes.len().div_ceil(4) * 4, 0);
        self.views.push(view);
        Ok(self.views.len() - 1)
    }

    /// An accessor of `rows`, which is not empty, as f32 vectors with the bounds glTF requires of positions. A bound
    /// is written as the exact value of its f32, so a reader that compares in f64 finds it in the data.
    fn vectors<const N: usize>(&mut self, rows: &[[f32; N]]) -> Result<usize, String> {
        if rows.iter().flatten().any(|number| !number.is_finite()) {
            return Err("a Part has a number outside the range of the file".into());
        }
        let bytes: Vec<u8> = rows.iter().flatten().flat_map(|number| number.to_le_bytes()).collect();
        let view = self.view(&bytes, Some(ARRAY_BUFFER))?;
        let bound = |pick: fn(f32, f32) -> f32| -> Vec<f64> {
            (0..N).map(|axis| f64::from(rows.iter().map(|row| row[axis]).fold(rows[0][axis], pick))).collect()
        };
        self.accessors.push(json!({
            "bufferView": view, "componentType": 5126, "count": rows.len(), "type": format!("VEC{N}"),
            "min": bound(f32::min), "max": bound(f32::max),
        }));
        Ok(self.accessors.len() - 1)
    }
}

fn list(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

fn pair(value: &Value) -> [f64; 2] {
    [value[0].as_f64().unwrap_or(0.0), value[1].as_f64().unwrap_or(0.0)]
}

fn name(node: &Value) -> &str {
    node["name"].as_str().unwrap_or("")
}

fn id_of(item: &Value) -> Option<u32> {
    item["uuid"].as_u64().and_then(|id| u32::try_from(id).ok())
}

fn walk<'a>(node: &'a Value, out: &mut Vec<&'a Value>) {
    out.push(node);
    for child in list(&node["children"]) {
        walk(child, out);
    }
}

fn leaves<'a>(items: &'a Value, out: &mut Vec<&'a Value>) {
    for item in list(items) {
        if item.get("groupUUID").is_some() {
            leaves(&item["children"], out);
        } else {
            out.push(item);
        }
    }
}

/// The Part draws of the model at the parameter `values`, in painter order, with vertices in canvas units.
fn draws(model: &Value, textures: &[TextureBlob], values: &BTreeMap<u32, [f64; 2]>) -> Result<Vec<Value>, String> {
    let evaluated = evaluation::evaluate(model, values)?;
    let deformations = evaluation::raster_deformations(model, values)?;
    let root = model.get("nodes").ok_or("model.nodes is missing")?;
    let mut still = |_: u32, _: &Value, _: Mat4, _: f64, _: &[[f64; 2]]| Ok(None);
    let scene = scene_deform::evaluate_with_path_update(&evaluated, root, &deformations, &mut still)?;
    render::scene_draws_for_scene(&evaluated, &scene, textures)
}

/// The texture in `slot` as a PNG with straight alpha. Soul2D stores colour multiplied by alpha, and glTF does not.
/// A fully transparent texel keeps the colour it is stored with, which a reader filters toward.
fn straight_png(textures: &[TextureBlob], slot: u64) -> Result<Vec<u8>, String> {
    let at = usize::try_from(slot).map_err(|_| format!("texture slot {slot} is missing"))?;
    let blob = textures.get(at).ok_or_else(|| format!("texture slot {slot} is missing"))?;
    let mut texture = render::decode_stored_texture(blob, at)?;
    for pixel in texture.rgba.chunks_exact_mut(4).filter(|pixel| pixel[3] != 0 && pixel[3] != 255) {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
        }
    }
    let image = image::RgbaImage::from_raw(texture.width, texture.height, texture.rgba)
        .ok_or_else(|| format!("texture slot {slot} does not hold its pixels"))?;
    let mut png = Cursor::new(Vec::new());
    image
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|error| format!("could not encode texture slot {slot}: {error}"))?;
    Ok(png.into_inner())
}

/// The document as glTF 2.0: a binary `.glb`, or a `.gltf` whose buffer is a data URI. `units` is the number of
/// canvas units for one metre, and `kept` the extension entries of the document.
pub(crate) fn export(
    model: &Value,
    textures: &[TextureBlob],
    kept: &[(Vec<u8>, Vec<u8>)],
    units: f64,
    binary: bool,
) -> Result<Export, String> {
    let mut parameters = Vec::new();
    leaves(&model["param"], &mut parameters);
    let defaults: BTreeMap<u32, [f64; 2]> =
        parameters.iter().filter_map(|parameter| Some((id_of(parameter)?, pair(&parameter["defaults"])))).collect();
    let root = model.get("nodes").ok_or("model.nodes is missing")?;
    render::packet_scene_limits(root)?;
    let rest = draws(model, textures, &defaults)?;
    let mut all = Vec::new();
    walk(root, &mut all);
    let node_of: BTreeMap<u64, &Value> = all.iter().filter_map(|node| Some((node["uuid"].as_u64()?, *node))).collect();
    // The rig frame has y down and glTF has y up. Both have z toward the viewer.
    let metres = |point: [f64; 3]| -> Result<[f32; 3], String> {
        let point = [point[0] / units, -point[1] / units, point[2] / units].map(|number| number as f32);
        let finite = point.iter().all(|number| number.is_finite());
        finite.then_some(point).ok_or_else(|| "unitsPerMetre puts a point outside the range of the file".into())
    };

    let mut report = Report::default();
    let mut buffer = Buffer::default();
    let (mut nodes, mut meshes, mut materials, mut images) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut image_of = BTreeMap::<u64, usize>::new();
    let mut drawn = BTreeSet::new();
    // Part id, to its mesh and its rest vertices in canvas units.
    let mut written = BTreeMap::<u64, (usize, Vec<[f64; 2]>)>::new();
    for draw in &rest {
        let id = draw["id"].as_u64().unwrap_or(0);
        drawn.insert(id);
        let part = node_of.get(&id).map_or("", |node| name(node));
        let vertices: Vec<[f64; 2]> = list(&draw["vertices"]).iter().map(pair).collect();
        let corners = list(&draw["indices"]).iter().map(|index| {
            let index = index.as_u64().and_then(|index| u16::try_from(index).ok());
            index.filter(|index| usize::from(*index) < vertices.len()).ok_or("a Part has a mesh index without a vertex")
        });
        let corners = corners.collect::<Result<Vec<u16>, _>>()?;
        let uvs: Vec<[f32; 2]> = list(&draw["uvs"]).iter().map(|uv| pair(uv).map(|n| n as f32)).collect();
        let (Some(slot), true) = (draw["textures"][0].as_u64(), corners.len() >= 3 && uvs.len() == vertices.len())
        else {
            report.add("part.mesh", part, "dropped", "it has no texture or no triangle");
            continue;
        };
        // An index is written in 16 bits, where 65535 restarts a primitive and names no vertex.
        if vertices.len() > usize::from(u16::MAX) {
            return Err(format!("the Part {} has more than {} vertices", label(part, 64), u16::MAX));
        }
        let positions = vertices.iter().map(|at| metres([at[0], at[1], written.len() as f64]));
        let positions = positions.collect::<Result<Vec<[f32; 3]>, _>>()?;
        // glTF takes the front of a triangle from its winding, and the y axis is turned, so every triangle is
        // written counter-clockwise as seen from +Z.
        let mut indices = Vec::with_capacity(corners.len() * 2);
        for triangle in corners.chunks_exact(3) {
            let [a, b, c] = [0, 1, 2].map(|corner| positions[usize::from(triangle[corner])].map(f64::from));
            let area = (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]);
            let order = if area < 0.0 { [0, 2, 1] } else { [0, 1, 2] };
            indices.extend(order.iter().flat_map(|corner| triangle[*corner].to_le_bytes()));
        }
        let view = buffer.view(&indices, Some(ELEMENT_ARRAY_BUFFER))?;
        buffer.accessors.push(json!({
            "bufferView": view, "componentType": 5123, "count": indices.len() / 2, "type": "SCALAR",
        }));
        let index = buffer.accessors.len() - 1;
        let (position, uv) = (buffer.vectors(&positions)?, buffer.vectors(&uvs)?);
        let image = match image_of.get(&slot) {
            Some(image) => *image,
            None => {
                let view = buffer.view(&straight_png(textures, slot)?, None)?;
                images.push(json!({ "bufferView": view, "mimeType": "image/png" }));
                let reason = "stored with premultiplied alpha, written with straight alpha";
                report.add("part.texture", &format!("texture {slot}"), "approximated", reason);
                image_of.insert(slot, images.len() - 1);
                images.len() - 1
            }
        };

        let material = &draw["material"];
        let number = |value: &Value, default: f64| value.as_f64().unwrap_or(default);
        let tint: [f64; 3] = std::array::from_fn(|channel| number(&material["tint"][channel], 1.0));
        let opacity = number(&material["opacity"], 1.0);
        // The factor multiplies the decoded texel, and Soul2D multiplies the encoded one, so the tint is decoded
        // with the sRGB transfer function. The opacity is linear in both.
        let decoded = |encoded: f64| {
            let encoded = encoded.clamp(0.0, 1.0);
            if encoded <= 0.04045 { encoded / 12.92 } else { ((encoded + 0.055) / 1.055).powf(2.4) }
        };
        let factor = [decoded(tint[0]), decoded(tint[1]), decoded(tint[2]), opacity.clamp(0.0, 1.0)];
        if tint.iter().chain([&opacity]).any(|part| *part != 1.0) {
            let reason = "the base colour factor of an unlit material, in linear colour and cut to the range 0 to 1";
            report.add("part.tint", part, "approximated", reason);
        }
        if (0..3).any(|channel| number(&material["screenTint"][channel], 0.0) != 0.0) {
            report.add("part.screenTint", part, "dropped", "glTF has no screen tint");
        }
        if material["blendMode"].as_str().is_some_and(|mode| mode != "Normal") {
            report.add("part.blendMode", part, "dropped", "glTF has no blend modes");
        }
        if !list(&draw["masks"]).is_empty() {
            report.add("mask", part, "dropped", "glTF has no masks");
        }
        materials.push(json!({
            "name": part,
            "pbrMetallicRoughness": {
                "baseColorFactor": factor, "baseColorTexture": { "index": image },
                "metallicFactor": 0.0, "roughnessFactor": 1.0,
            },
            "alphaMode": "BLEND", "doubleSided": true,
            "extensions": { "KHR_materials_unlit": {} },
        }));
        let attributes = json!({ "POSITION": position, "TEXCOORD_0": uv });
        let primitive = json!({ "attributes": attributes, "indices": index, "material": materials.len() - 1 });
        meshes.push(json!({ "name": part, "primitives": [primitive] }));
        nodes.push(json!({ "name": part, "mesh": meshes.len() - 1 }));
        written.insert(id, (meshes.len() - 1, vertices));
        report.add("part.mesh", part, "converted", "");
    }
    if written.is_empty() {
        return Err("the document draws no textured Part to export".into());
    }
    let layered = "Z is the place among the written Parts, one canvas unit a layer; a 3D viewer sorts by its own rule";
    report.add("part.drawOrder", "", "approximated", layered);

    // A key of a one-axis parameter is a morph target of every Part it moves.
    let mut target_names = BTreeMap::<usize, Vec<String>>::new();
    let mut keys_left = MAX_TARGET_KEYS;
    for parameter in &parameters {
        let Some(id) = id_of(parameter) else { continue };
        if parameter["is_vec2"].as_bool().unwrap_or(false) {
            let reason = "a two-axis parameter has no morph counterpart";
            report.add("parameter.keyform", name(parameter), "dropped", reason);
            continue;
        }
        let default = defaults[&id];
        let keys = evaluation::parameter_key_values(parameter)?;
        for key in keys[0].iter().copied().filter(|key| (key - default[0]).abs() > 1e-9) {
            // The name is cut before the key, which tells the targets of one parameter apart.
            let source = label(&format!("{}={key}", label(name(parameter), 32)), 64);
            if keys_left == 0 {
                let reason = format!("over the limit of {MAX_TARGET_KEYS} parameter keys");
                report.add("parameter.keyform", &source, "dropped", &reason);
                continue;
            }
            keys_left -= 1;
            let mut values = defaults.clone();
            values.insert(id, [key, default[1]]);
            let mut moved = false;
            for draw in draws(model, textures, &values)? {
                let Some((mesh, rest)) = draw["id"].as_u64().and_then(|id| written.get(&id)) else { continue };
                let posed = list(&draw["vertices"]);
                if posed.len() != rest.len() {
                    return Err("a parameter key changes the vertex count of a Part".into());
                }
                let offset = |(rest, posed): (&[f64; 2], &Value)| [pair(posed)[0] - rest[0], pair(posed)[1] - rest[1]];
                let offsets: Vec<[f64; 2]> = rest.iter().zip(posed).map(offset).collect();
                if !offsets.iter().flatten().any(|offset| offset.abs() > 1e-6) {
                    continue;
                }
                let deltas = offsets.iter().map(|offset| metres([offset[0], offset[1], 0.0]));
                let accessor = buffer.vectors(&deltas.collect::<Result<Vec<[f32; 3]>, _>>()?)?;
                let targets = &mut meshes[*mesh]["primitives"][0]["targets"];
                if targets.is_null() {
                    *targets = json!([]);
                }
                if let Some(targets) = targets.as_array_mut() {
                    targets.push(json!({ "POSITION": accessor }));
                }
                target_names.entry(*mesh).or_default().push(source.clone());
                moved = true;
            }
            if moved {
                let reason = "a morph target of each Part the key moves; a reader blends targets linearly";
                report.add("parameter.keyform", &source, "approximated", reason);
            } else {
                report.add("parameter.keyform", &source, "dropped", "the key moves no vertex of a written Part");
            }
        }
    }
    for (mesh, names) in target_names {
        meshes[mesh]["weights"] = json!(vec![0.0; names.len()]);
        meshes[mesh]["extras"] = json!({ "targetNames": names });
    }

    // A DepthBone is a node at its rest head, the one `rig.list_bones` answers, whatever the parameters pose. Nothing
    // is skinned to it: see `rig.vertexWeights` below.
    // ponytail: every root lists its bones with a walk of the whole tree. Index the tree once if a document with
    // hundreds of rigs has to be exported.
    let mut scene_nodes: Vec<usize> = (0..nodes.len()).collect();
    for root in all.iter().filter(|node| node["type"] == "DepthRigRoot") {
        let Some(root_id) = id_of(root) else { continue };
        let bones = skeleton::list_bones(model, root_id)?;
        if list(&bones["items"]).is_empty() {
            continue;
        }
        let world = bone_deform::world_matrix(model, root_id)?;
        let origin = bone_deform::transform_point(world, [0.0; 3])?;
        scene_nodes.push(nodes.len());
        nodes.push(json!({ "name": name(root), "translation": metres(origin)? }));
        let top = (nodes.len() - 1, origin);
        // The list has a bone after its parent.
        let mut placed = BTreeMap::new();
        for bone in list(&bones["items"]) {
            let head: [f64; 3] = std::array::from_fn(|axis| bone["restHead"][axis].as_f64().unwrap_or(0.0));
            let here = bone_deform::transform_point(world, head)?;
            let above = bone["parent"].as_u64().and_then(|parent| placed.get(&parent)).copied();
            let (parent, above) = above.unwrap_or(top);
            let bone_id = bone["boneId"].as_str().filter(|id| !id.is_empty()).unwrap_or_else(|| name(bone));
            let offset = metres([here[0] - above[0], here[1] - above[1], here[2] - above[2]])?;
            nodes.push(json!({ "name": bone_id, "translation": offset }));
            let at = nodes.len() - 1;
            let children = &mut nodes[parent]["children"];
            if children.is_null() {
                *children = json!([]);
            }
            if let Some(children) = children.as_array_mut() {
                children.push(json!(at));
            }
            placed.insert(bone["uuid"].as_u64().unwrap_or(0), (at, here));
            report.add("rig.bone", bone_id, "converted", "");
        }
        let reason = "the tail, the roll, the constraints and the orientation of a bone are not written";
        report.add("skeleton.restPose", name(root), "approximated", reason);
        for binding in list(&root["bindings"]) {
            let target = binding["target"].as_u64().and_then(|id| node_of.get(&id)).map_or("", |node| name(node));
            let reason = "no skin is written: deformer weights are not resampled and Part weights are not written yet";
            report.add("rig.vertexWeights", target, "dropped", reason);
        }
    }

    for node in &all {
        let (feature, outcome, reason) = match node["type"].as_str().unwrap_or("") {
            "Composite" | "DynamicComposite" => ("composite", "dropped", "its Parts are written without it"),
            "Mask" => ("mask", "dropped", "glTF has no masks"),
            "SimplePhysics" => ("physics", "dropped", "glTF has no counterpart"),
            "MeshGroup" | "GridDeformer" | "PathDeformer" => {
                ("deformer", "approximated", "its rest shape is baked into the Part meshes")
            }
            "Part" if !node["uuid"].as_u64().is_some_and(|id| drawn.contains(&id)) => {
                ("part.mesh", "dropped", "it is not drawn")
            }
            _ => continue,
        };
        report.add(feature, name(node), outcome, reason);
    }
    for animation in model["animations"].as_object().into_iter().flatten().map(|(animation, _)| animation) {
        report.add("animation", animation, "dropped", "motion is not exported");
    }

    // Puppet metadata and kept rights data go to `asset`. Rights data is never dropped.
    let fields = ["name", "artist", "rigger", "contact", "licenseURL", "copyright", "reference"];
    let stated = |field: &&str| {
        let text = model["meta"][*field].as_str().filter(|text| !text.is_empty())?;
        Some(((*field).to_owned(), json!(text)))
    };
    let meta: Map<String, Value> = fields.iter().filter_map(stated).collect();
    let mut copyright = meta.get("copyright").cloned();
    let mut extras = Map::new();
    if !meta.is_empty() {
        extras.insert(PUPPET_META_KEY.into(), Value::Object(meta));
        report.add("metadata", "", "converted", "");
    }
    let (mut rights, mut cut) = (Map::new(), false);
    for (key, payload) in kept {
        let key = String::from_utf8_lossy(key).into_owned();
        if key != VRM_META_KEY && key != GLTF_ASSET_KEY {
            report.add("extdata", &key, "dropped", "not written to glTF");
            continue;
        }
        let stated: Value =
            serde_json::from_slice(payload).map_err(|_| format!("the kept rights data under {key} is not JSON"))?;
        if key == GLTF_ASSET_KEY && stated["copyright"].is_string() {
            copyright = Some(stated["copyright"].clone());
        }
        if rights.contains_key(&key) {
            return Err(format!("the document keeps two entries under {key}"));
        }
        let view = shown(&stated);
        cut |= view != stated;
        report.add("rights", &key, "kept", &format!("asset.extras.{key}"));
        rights.insert(key.clone(), view);
        extras.insert(key, stated);
    }

    let mut asset = json!({ "version": "2.0", "generator": concat!("Soul2D ", env!("CARGO_PKG_VERSION")) });
    if let Some(copyright) = copyright {
        asset["copyright"] = copyright;
    }
    if !extras.is_empty() {
        asset["extras"] = Value::Object(extras);
    }
    let length = buffer.bytes.len();
    let textures: Vec<Value> = (0..images.len()).map(|image| json!({ "source": image, "sampler": 0 })).collect();
    let mut gltf = json!({
        "asset": asset,
        "extensionsUsed": ["KHR_materials_unlit"],
        "scene": 0,
        "scenes": [{ "nodes": scene_nodes }],
        "nodes": nodes,
        "meshes": meshes,
        "materials": materials,
        "textures": textures,
        "images": images,
        "samplers": [{ "magFilter": 9729, "minFilter": 9729, "wrapS": 33071, "wrapT": 33071 }],
        "accessors": buffer.accessors,
        "bufferViews": buffer.views,
        "buffers": [{ "byteLength": length }],
    });
    let encode = |gltf: &Value| serde_json::to_vec(gltf).map_err(|error| error.to_string());
    let bytes = if binary {
        let mut text = encode(&gltf)?;
        text.resize(text.len().div_ceil(4) * 4, b' ');
        let size = |bytes: usize| {
            u32::try_from(bytes).map(u32::to_le_bytes).map_err(|_| "the export is too large for a binary glTF")
        };
        let total = size(28 + text.len() + length)?;
        let (json_size, bin_size) = (size(text.len())?, size(length)?);
        [b"glTF".as_slice(), &2u32.to_le_bytes(), &total, &json_size, b"JSON", &text, &bin_size, b"BIN\0", &buffer.bytes]
            .concat()
    } else {
        let data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &buffer.bytes);
        gltf["buffers"][0]["uri"] = json!(format!("data:application/octet-stream;base64,{data}"));
        encode(&gltf)?
    };

    let has_rights = !rights.is_empty();
    let mut report = report.fields(cut);
    report["direction"] = json!("soul2d-to-gltf");
    report["target"] = json!({ "format": "glTF 2.0", "convention": "gltf2" });
    report["rights"] = if has_rights { Value::Object(rights) } else { Value::Null };
    Ok(Export { bytes, report, rights: has_rights })
}
