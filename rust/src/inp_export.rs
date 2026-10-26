//! INP export (D `io/inpexport.d`, `ExportINPCommand`): a runtime puppet file from an editor model.
//!
//! Pure: model JSON and texture blobs in, file bytes out. The steps are D's `incINPExport`: strip the
//! editor-only rig nodes, prune disabled nodes, flatten, pack every Part texture into atlases, rewrite UVs
//! and texture slots, then write the same container an INX save uses (`TRNSRTS\0`, JSON, `TEX_SECT`,
//! optional `EXT_SECT`). The file differs from an INX only by this processing and the extension.

use crate::atlas::{build_atlases, AtlasPart, AtlasSettings, PartPlacement};
use crate::bone_deform::{mul_mat, projection_matrix, transform_vec, Mat4};
use crate::cv::WorkBudget;
use crate::model_import::{compact_slots, inx_bytes, Texture};
use crate::pixels::{PixelFormat, PixelImage};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::Cursor;

const MAX_RESOLUTION: u32 = crate::pixels::MAX_ATLAS_PAGE;
const MAX_SCALE: f32 = 16.0;
const MAX_LOOPS: u32 = 1000;
const MAX_SOURCE_PIXELS: u64 = 128 * 1024 * 1024;
const WORK_BUDGET: u64 = 1_000_000_000;
const TEXTURE_TGA: u8 = 1;
const NO_TEXTURE: u64 = u32::MAX as u64;
const EXTENSIONS: &[u8; 8] = b"EXT_SECT";

/// D `BlendMode` names accepted for the watermark.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WatermarkBlend {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    LinearDodge,
    AddGlow,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Subtract,
    Inverse,
    DestinationIn,
    ClipToLower,
    SliceFromLower,
}

impl WatermarkBlend {
    pub fn from_name(name: &str) -> Result<Self, String> {
        Ok(match name {
            "Normal" => Self::Normal,
            "Multiply" => Self::Multiply,
            "Screen" => Self::Screen,
            "Overlay" => Self::Overlay,
            "Darken" => Self::Darken,
            "Lighten" => Self::Lighten,
            "ColorDodge" => Self::ColorDodge,
            "LinearDodge" => Self::LinearDodge,
            "AddGlow" => Self::AddGlow,
            "ColorBurn" => Self::ColorBurn,
            "HardLight" => Self::HardLight,
            "SoftLight" => Self::SoftLight,
            "Difference" => Self::Difference,
            "Exclusion" => Self::Exclusion,
            "Subtract" => Self::Subtract,
            "Inverse" => Self::Inverse,
            "DestinationIn" => Self::DestinationIn,
            "ClipToLower" => Self::ClipToLower,
            "SliceFromLower" => Self::SliceFromLower,
            other => return Err(format!("unknown watermark blend mode {other:?}")),
        })
    }
}

/// D `decorateWatermark*`: tiled over the albedo atlas of every atlas, after packing.
#[derive(Clone, Debug)]
pub struct Watermark {
    /// The decoded file, straight alpha. It is premultiplied here like the D export window does on load.
    pub image: PixelImage,
    pub opacity: f32,
    pub blend: WatermarkBlend,
    /// How many times the image repeats across the atlas, per axis.
    pub loops: u32,
}

/// D `IncINPExportSettings` with the same defaults.
#[derive(Clone, Debug)]
pub struct InpExportOptions {
    pub atlas_resolution: u32,
    pub non_linear_scaling: bool,
    pub scale: f32,
    pub padding: i32,
    /// Drop disabled nodes (with their subtrees) from the output.
    pub prune_unused: bool,
    pub watermark: Option<Watermark>,
}

impl Default for InpExportOptions {
    fn default() -> Self {
        Self {
            atlas_resolution: 2048,
            non_linear_scaling: false,
            scale: 1.0,
            padding: 16,
            prune_unused: true,
            watermark: None,
        }
    }
}

impl InpExportOptions {
    /// The `ExportINPCommand` arguments: `atlasResolution`, `nonLinearScaling`, `scale`, `padding`,
    /// `optimizePruneUnused`. Absent or null keys keep the D defaults. The watermark is not a request
    /// field (the D command has none); the caller loads the image and sets `watermark`.
    pub fn from_request(request: &Value) -> Result<Self, String> {
        let mut options = Self::default();
        let present = |key: &str| request.get(key).filter(|value| !value.is_null());
        if let Some(value) = present("atlasResolution") {
            options.atlas_resolution = value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or("atlasResolution must be a positive integer")?;
        }
        if let Some(value) = present("nonLinearScaling") {
            options.non_linear_scaling = value.as_bool().ok_or("nonLinearScaling must be a boolean")?;
        }
        if let Some(value) = present("scale") {
            options.scale = value.as_f64().ok_or("scale must be a number")? as f32;
        }
        if let Some(value) = present("padding") {
            options.padding = value
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .ok_or("padding must be a 32-bit integer")?;
        }
        if let Some(value) = present("optimizePruneUnused") {
            options.prune_unused = value.as_bool().ok_or("optimizePruneUnused must be a boolean")?;
        }
        options.validate()?;
        Ok(options)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.atlas_resolution == 0 || self.atlas_resolution > MAX_RESOLUTION {
            return Err(format!("atlas resolution must be between 1 and {MAX_RESOLUTION}"));
        }
        if self.padding < 0 || self.padding as u32 > self.atlas_resolution {
            return Err("padding must be between 0 and the atlas resolution".into());
        }
        if !self.scale.is_finite() || self.scale <= 0.0 || self.scale > MAX_SCALE {
            return Err(format!("scale must be greater than 0 and at most {MAX_SCALE}"));
        }
        if let Some(watermark) = &self.watermark {
            let [width, height] = watermark.image.size();
            if watermark.image.format() != PixelFormat::Rgba || width == 0 || height == 0 {
                return Err("watermark must be a non-empty RGBA image".into());
            }
            if !(0.0..=1.0).contains(&watermark.opacity) {
                return Err("watermark opacity must be between 0 and 1".into());
            }
            if watermark.loops == 0 || watermark.loops > MAX_LOOPS {
                return Err(format!("watermark loops must be between 1 and {MAX_LOOPS}"));
            }
        }
        Ok(())
    }
}

/// What an export produced besides the file bytes.
#[derive(Clone, Debug)]
pub struct InpExport {
    pub bytes: Vec<u8>,
    /// Atlases that were packed (texture slots in the file can be fewer when pruning left one unused).
    pub atlas_count: usize,
    /// D `IncINPPreviewInfo.outputScale`: the uniform scale forced on linear packing; 1 means none.
    pub output_scale: f32,
    /// Nodes removed because they were disabled.
    pub pruned_nodes: usize,
    /// Parts left on their original textures because they have no usable UV area.
    pub skipped_parts: Vec<u32>,
    /// Grid/Path/MeshGroup deformers whose own deform bindings D bakes into their children and this
    /// port leaves as authored.
    pub unbaked_deformers: Vec<u32>,
}

pub fn export_inp(model: &Value, textures: &[Texture], options: &InpExportOptions) -> Result<Vec<u8>, String> {
    export_inp_report(model, textures, &[], options).map(|export| export.bytes)
}

/// `ext` is the `EXT_SECT` block of the source file, appended unchanged (empty for none).
pub fn export_inp_report(
    model: &Value,
    textures: &[Texture],
    ext: &[u8],
    options: &InpExportOptions,
) -> Result<InpExport, String> {
    options.validate()?;
    check_ext(ext)?;
    if model.get("nodes").and_then(Value::as_object).is_none() {
        return Err("model is missing nodes".into());
    }
    // D packs the original puppet and prunes the copy, so disabled Parts still take atlas space.
    let source = crate::nodes::model_without_rig_nodes(model)?;
    let mut candidates = Vec::new();
    let mut skipped_parts = Vec::new();
    let root = &source["nodes"];
    let root_local = local_matrix(root)?;
    collect_parts(root, None, root_local, textures.len(), &mut candidates, &mut skipped_parts)?;
    if candidates.is_empty() {
        return Err("no Part with an albedo texture to export".into());
    }

    let mut images = Vec::new();
    let mut decoded: Vec<Option<usize>> = vec![None; textures.len()];
    let mut pixel_total = 0u64;
    for candidate in &candidates {
        for slot in candidate.slots.iter().flatten() {
            if decoded[*slot].is_some() {
                continue;
            }
            let image = decode_texture(&textures[*slot], *slot)?;
            let [width, height] = image.size();
            pixel_total += u64::from(width) * u64::from(height);
            if pixel_total > MAX_SOURCE_PIXELS {
                return Err("the Part textures are too large to export".into());
            }
            images.push(image);
            decoded[*slot] = Some(images.len() - 1);
        }
    }
    let pins = crate::atlas::pins(model)?;
    let parts: Vec<AtlasPart> = candidates
        .iter()
        .map(|candidate| AtlasPart {
            textures: candidate.slots.map(|slot| slot.and_then(|slot| decoded[slot]).map(|index| &images[index])),
            uvs: &candidate.uvs,
            bounds: candidate.bounds,
            pin: pins.get(&candidate.id).copied(),
        })
        .collect();
    let settings = AtlasSettings {
        resolution: options.atlas_resolution,
        padding: options.padding,
        scale: options.scale,
        non_linear_scaling: options.non_linear_scaling,
    };
    let mut budget = WorkBudget::new(WORK_BUDGET);
    let mut build = build_atlases(&parts, &settings, &mut budget)?;
    drop(parts);

    // Atlas textures follow the originals in the slot list; compaction renumbers them like
    // D `populateTextureSlots`.
    let mut slots: Vec<Texture> = textures.to_vec();
    let mut atlas_slot: Vec<[Option<usize>; 3]> = Vec::new();
    for atlas in &mut build.atlases {
        if let (Some(watermark), Some(albedo)) = (&options.watermark, atlas.slots[0].as_mut()) {
            apply_watermark(albedo, watermark, &mut budget)?;
        }
        let mut indices = [None; 3];
        for (index, image) in atlas.slots.iter().enumerate() {
            if let Some(image) = image {
                slots.push(Texture { tag: TEXTURE_TGA, data: encode_tga(image, if index == 0 { 4 } else { 3 })? });
                indices[index] = Some(slots.len() - 1);
            }
        }
        atlas_slot.push(indices);
    }

    let mut out = source.clone();
    let mut removed = HashSet::new();
    if options.prune_unused {
        prune_disabled(&mut out["nodes"], &mut removed);
    }
    let pruned_nodes = removed.len();
    flatten(&mut out, &mut removed);
    prune_bindings(&mut out, &removed);

    let placements: HashMap<u32, usize> =
        candidates.iter().enumerate().map(|(index, candidate)| (candidate.id, index)).collect();
    rewrite_parts(&mut out["nodes"], &placements, &build.parts, &atlas_slot);
    let kept = compact_slots(&mut out, slots)?;
    let unbaked_deformers = unbaked_deformers(&out);

    let mut bytes = inx_bytes(&out, &kept)?;
    bytes.extend_from_slice(ext);
    Ok(InpExport {
        bytes,
        atlas_count: build.atlases.len(),
        output_scale: build.output_scale,
        pruned_nodes,
        skipped_parts,
        unbaked_deformers,
    })
}

// ---------------------------------------------------------------------------------------------
// Part collection
// ---------------------------------------------------------------------------------------------

struct Candidate {
    id: u32,
    /// Indices into the file's texture list: albedo, emissive, bump.
    slots: [Option<usize>; 3],
    uvs: Vec<[f32; 2]>,
    /// World `[left, top, right, bottom]` of the rest-pose vertices (D `Drawable.bounds`).
    bounds: [f32; 4],
}

fn local_matrix(node: &Value) -> Result<Mat4, String> {
    let scale = transform_vec(node, "scale", [1.0, 1.0, 1.0]);
    projection_matrix(
        transform_vec(node, "trans", [0.0, 0.0, 0.0]),
        transform_vec(node, "rot", [0.0, 0.0, 0.0]),
        [scale[0], scale[1]],
    )
}

fn node_id(node: &Value) -> Result<u32, String> {
    node.get("uuid")
        .and_then(Value::as_u64)
        .and_then(crate::valid_id)
        .ok_or_else(|| "node is missing a valid uuid".to_string())
}

fn collect_parts(
    node: &Value,
    parent: Option<Mat4>,
    root_local: Mat4,
    texture_count: usize,
    out: &mut Vec<Candidate>,
    skipped: &mut Vec<u32>,
) -> Result<(), String> {
    let local = local_matrix(node)?;
    let locked = node.get("lockToRoot").and_then(Value::as_bool) == Some(true);
    let world = match parent {
        None => local,
        Some(_) if locked => mul_mat(root_local, local),
        Some(parent) => mul_mat(parent, local),
    };
    if matches!(node.get("type").and_then(Value::as_str), Some("Part" | "AnimatedPart")) {
        if let Some(candidate) = candidate_of(node, world, texture_count, skipped)? {
            out.push(candidate);
        }
    }
    for child in node.get("children").and_then(Value::as_array).into_iter().flatten() {
        collect_parts(child, Some(world), root_local, texture_count, out, skipped)?;
    }
    Ok(())
}

fn candidate_of(
    node: &Value,
    world: Mat4,
    texture_count: usize,
    skipped: &mut Vec<u32>,
) -> Result<Option<Candidate>, String> {
    let id = node_id(node)?;
    let mut slots = [None; 3];
    for (position, entry) in node.get("textures").and_then(Value::as_array).into_iter().flatten().take(3).enumerate() {
        let Some(slot) = entry.as_u64().filter(|slot| *slot != NO_TEXTURE) else { continue };
        let slot = usize::try_from(slot)
            .ok()
            .filter(|slot| *slot < texture_count)
            .ok_or_else(|| format!("Part {id} uses texture slot {slot} that is not in the file"))?;
        slots[position] = Some(slot);
    }
    if slots[0].is_none() {
        return Ok(None);
    }
    let mesh = crate::deformers::parse_part_mesh(node).map_err(|error| format!("Part {id}: {error}"))?;
    let (mut min, mut max) = ([1.0f32, 1.0], [0.0f32, 0.0]);
    for uv in &mesh.uvs {
        for axis in 0..2 {
            let value = uv[axis] as f32;
            min[axis] = min[axis].min(value);
            max[axis] = max[axis].max(value);
        }
    }
    let finite = mesh.uvs.iter().flatten().chain(mesh.vertices.iter().flatten()).all(|value| value.is_finite());
    if !finite {
        return Err(format!("Part {id} has non-finite mesh data"));
    }
    if mesh.uvs.is_empty() || max[0] <= min[0] || max[1] <= min[1] {
        skipped.push(id);
        return Ok(None);
    }
    let mut bounds = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for [x, y] in &mesh.vertices {
        let wx = world[0][0] * x + world[0][1] * y + world[0][3];
        let wy = world[1][0] * x + world[1][1] * y + world[1][3];
        bounds = [bounds[0].min(wx), bounds[1].min(wy), bounds[2].max(wx), bounds[3].max(wy)];
    }
    if mesh.vertices.is_empty() {
        bounds = [world[0][3], world[1][3], world[0][3], world[1][3]];
    }
    Ok(Some(Candidate {
        id,
        slots,
        uvs: mesh.uvs.iter().map(|uv| [uv[0] as f32, uv[1] as f32]).collect(),
        bounds: bounds.map(|value| value as f32),
    }))
}

fn decode_texture(blob: &Texture, slot: usize) -> Result<PixelImage, String> {
    let format = match blob.tag {
        0 => image::ImageFormat::Png,
        1 => image::ImageFormat::Tga,
        tag => return Err(format!("texture slot {slot} has unsupported image tag {tag}")),
    };
    let mut reader = image::ImageReader::with_format(Cursor::new(blob.data.as_slice()), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|error| format!("could not decode texture slot {slot}: {error}"))?
        .into_rgba8();
    PixelImage::from_pixels(decoded.width(), decoded.height(), PixelFormat::Rgba, decoded.into_raw())
}

fn encode_tga(image: &PixelImage, channels: usize) -> Result<Vec<u8>, String> {
    let [width, height] = image.size();
    let (data, color) = if channels == 4 {
        (image.pixels().to_vec(), image::ExtendedColorType::Rgba8)
    } else {
        // D keeps emissive and bump atlases at three channels.
        (
            image.pixels().chunks_exact(4).flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]).collect(),
            image::ExtendedColorType::Rgb8,
        )
    };
    let mut out = Vec::new();
    image::codecs::tga::TgaEncoder::new(&mut out)
        .encode(&data, width, height, color)
        .map_err(|error| format!("could not encode atlas: {error}"))?;
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Tree editing
// ---------------------------------------------------------------------------------------------

fn uuids_of(node: &Value, into: &mut HashSet<u32>) {
    if let Some(id) = node.get("uuid").and_then(Value::as_u64).and_then(crate::valid_id) {
        into.insert(id);
    }
    for child in node.get("children").and_then(Value::as_array).into_iter().flatten() {
        uuids_of(child, into);
    }
}

fn remove_children(node: &mut Value, doomed: &dyn Fn(&Value) -> bool, removed: &mut HashSet<u32>) {
    let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) else { return };
    children.retain(|child| {
        let remove = doomed(child);
        if remove {
            uuids_of(child, removed);
        }
        !remove
    });
    for child in children {
        remove_children(child, doomed, removed);
    }
}

/// D `incINPExportOptimizePrune`: a disabled node leaves with its subtree; the root stays. D iterates a
/// slice it also mutates, so a disabled node right after a pruned sibling survives there; this removes
/// every disabled node.
fn prune_disabled(root: &mut Value, removed: &mut HashSet<u32>) {
    remove_children(root, &|node| node.get("enabled").and_then(Value::as_bool) == Some(false), removed);
}

/// D `incINPExportFlatten`: parameter groups dissolve into their children, animation lanes whose
/// parameter is gone are dropped, cameras leave the tree. Audio and image tracks are dropped too: they name files
/// on this machine, which a distributed file should not carry. Group tracks and the model's group go with them, and
/// so do the shy and freeze marks and the parameter bookmarks, which only serve the editor's timeline and its exports.
fn flatten(model: &mut Value, removed: &mut HashSet<u32>) {
    let mut flat = Vec::new();
    for parameter in model.get("param").and_then(Value::as_array).into_iter().flatten() {
        if parameter.get("groupUUID").is_some() {
            flat.extend(parameter.get("children").and_then(Value::as_array).cloned().unwrap_or_default());
        } else {
            flat.push(parameter.clone());
        }
    }
    let known: HashSet<u64> = flat.iter().filter_map(|parameter| parameter.get("uuid")?.as_u64()).collect();
    if model.get("param").is_some() {
        model["param"] = Value::Array(flat);
    }
    for animation in model.get_mut("animations").and_then(Value::as_object_mut).into_iter().flat_map(|map| map.values_mut()) {
        if let Some(lanes) = animation.get_mut("lanes").and_then(Value::as_array_mut) {
            lanes.retain(|lane| lane.get("uuid").and_then(Value::as_u64).is_some_and(|id| known.contains(&id)));
            for lane in lanes.iter_mut().filter_map(Value::as_object_mut) {
                lane.remove("shy");
                lane.remove("frozen");
            }
        }
        if let Some(animation) = animation.as_object_mut() {
            for key in ["audio", "images", "groups", "modelGroup", "shyActive", "freezeFrame", "bookmarks", "controllers", "forms", "formTime", "frameStep", "frameStepGroups"] {
                animation.remove(key);
            }
        }
    }
    remove_children(&mut model["nodes"], &|node| node.get("type").and_then(Value::as_str) == Some("Camera"), removed);
}

/// Bindings to removed nodes would be dropped when D reads the file back (`Parameter.finalize`).
fn prune_bindings(model: &mut Value, removed: &HashSet<u32>) {
    fn walk(parameters: &mut [Value], removed: &HashSet<u32>) {
        for parameter in parameters {
            if let Some(bindings) = parameter.get_mut("bindings").and_then(Value::as_array_mut) {
                bindings.retain(|binding| {
                    binding.get("node").and_then(Value::as_u64).and_then(crate::valid_id).is_none_or(|id| !removed.contains(&id))
                });
            }
            if let Some(children) = parameter.get_mut("children").and_then(Value::as_array_mut) {
                walk(children, removed);
            }
        }
    }
    if let Some(parameters) = model.get_mut("param").and_then(Value::as_array_mut) {
        walk(parameters, removed);
    }
}

fn unbaked_deformers(model: &Value) -> Vec<u32> {
    fn walk(node: &Value, model: &Value, out: &mut Vec<u32>) {
        if matches!(node.get("type").and_then(Value::as_str), Some("GridDeformer" | "PathDeformer" | "MeshGroup")) {
            if let Some(id) = node.get("uuid").and_then(Value::as_u64).and_then(crate::valid_id) {
                if crate::deformers::has_deform_binding(model, id) {
                    out.push(id);
                }
            }
        }
        for child in node.get("children").and_then(Value::as_array).into_iter().flatten() {
            walk(child, model, out);
        }
    }
    let mut out = Vec::new();
    walk(&model["nodes"], model, &mut out);
    out
}

/// D `incINPExportFinalizePacking` for the Parts that survived: new UVs and the three atlas texture
/// slots. A Part without an emissive/bump texture still points at the atlas slot when other Parts use it.
fn rewrite_parts(
    node: &mut Value,
    placements: &HashMap<u32, usize>,
    placed: &[PartPlacement],
    atlas_slot: &[[Option<usize>; 3]],
) {
    if matches!(node.get("type").and_then(Value::as_str), Some("Part" | "AnimatedPart")) {
        let id = node.get("uuid").and_then(Value::as_u64).and_then(crate::valid_id);
        if let Some(&index) = id.and_then(|id| placements.get(&id)) {
            let placement = &placed[index];
            set_uvs(node, &placement.uvs);
            let slots = atlas_slot[placement.atlas].map(|slot| slot.map_or(json!(NO_TEXTURE), |slot| json!(slot)));
            node["textures"] = Value::Array(slots.to_vec());
        }
    }
    for child in node.get_mut("children").and_then(Value::as_array_mut).into_iter().flatten() {
        rewrite_parts(child, placements, placed, atlas_slot);
    }
}

/// The shortest decimal that reads back as the same `f32`, like D's float serializer.
fn shortest(value: f32) -> f64 {
    format!("{value}").parse().unwrap_or(f64::from(value))
}

fn set_uvs(node: &mut Value, uvs: &[[f32; 2]]) {
    fn reshape(old: &Value, uvs: &[[f32; 2]]) -> Value {
        if old.as_array().and_then(|items| items.first()).is_some_and(Value::is_array) {
            Value::Array(uvs.iter().map(|uv| json!([shortest(uv[0]), shortest(uv[1])])).collect())
        } else {
            Value::Array(uvs.iter().flat_map(|uv| [json!(shortest(uv[0])), json!(shortest(uv[1]))]).collect())
        }
    }
    if let Some(old) = node.get("mesh").and_then(|mesh| mesh.get("uvs")) {
        let value = reshape(old, uvs);
        node["mesh"]["uvs"] = value;
    }
    if let Some(old) = node.get("uvs") {
        let value = reshape(old, uvs);
        node["uvs"] = value;
    }
}

// ---------------------------------------------------------------------------------------------
// Container
// ---------------------------------------------------------------------------------------------

/// `ext` must be empty or exactly one `EXT_SECT` block: count, then name and payload per section.
fn check_ext(ext: &[u8]) -> Result<(), String> {
    if ext.is_empty() {
        return Ok(());
    }
    let truncated = || "truncated extension section".to_string();
    if ext.len() < 12 || &ext[..8] != EXTENSIONS {
        return Err("extension data must start with EXT_SECT".into());
    }
    let take = |at: &mut usize| -> Result<usize, String> {
        let raw = ext.get(*at..*at + 4).ok_or_else(truncated)?;
        *at += 4;
        Ok(u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize)
    };
    let mut at = 8;
    let count = take(&mut at)?;
    for _ in 0..count {
        for _ in 0..2 {
            let length = take(&mut at)?;
            at = at.checked_add(length).filter(|end| *end <= ext.len()).ok_or_else(truncated)?;
        }
    }
    if at != ext.len() {
        return Err("extension data has trailing bytes".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Watermark (D `incINPExportDecorateWatermark`)
// ---------------------------------------------------------------------------------------------

#[derive(Copy, Clone)]
enum Equation {
    Add,
    ReverseSubtract,
    Max,
}

#[derive(Copy, Clone)]
enum Factor {
    Zero,
    One,
    DstColor,
    DstAlpha,
    SrcAlpha,
    OneMinusSrcColor,
    OneMinusSrcAlpha,
    OneMinusDstColor,
}

/// `[colour, alpha]` equation and (source, destination) factors: the GL state of D `oglSetLegacyBlendMode`.
/// The KHR advanced equations are not used: they depend on the GPU, and the legacy path is what D falls
/// back to (and always uses on macOS).
fn gl_state(blend: WatermarkBlend) -> ([Equation; 2], [(Factor, Factor); 2]) {
    use Equation::{Add, Max, ReverseSubtract};
    use Factor::*;
    let both = |source, destination| [(source, destination); 2];
    match blend {
        WatermarkBlend::Multiply => ([Add; 2], both(DstColor, OneMinusSrcAlpha)),
        WatermarkBlend::Screen => ([Add; 2], both(One, OneMinusSrcColor)),
        WatermarkBlend::Lighten => ([Max; 2], both(One, One)),
        WatermarkBlend::ColorDodge => ([Add; 2], both(DstColor, One)),
        WatermarkBlend::LinearDodge => ([Add; 2], [(One, OneMinusSrcColor), (One, OneMinusSrcAlpha)]),
        WatermarkBlend::AddGlow => ([Add; 2], [(One, One), (One, OneMinusSrcAlpha)]),
        WatermarkBlend::Subtract => ([ReverseSubtract, Add], both(OneMinusDstColor, One)),
        WatermarkBlend::Exclusion => ([Add; 2], [(OneMinusDstColor, OneMinusSrcColor), (One, One)]),
        WatermarkBlend::Inverse => ([Add; 2], both(OneMinusDstColor, OneMinusSrcAlpha)),
        WatermarkBlend::DestinationIn => ([Add; 2], both(Zero, SrcAlpha)),
        WatermarkBlend::ClipToLower => ([Add; 2], both(DstAlpha, OneMinusSrcAlpha)),
        WatermarkBlend::SliceFromLower => ([Add; 2], both(Zero, OneMinusSrcAlpha)),
        // Normal, and the modes only the advanced path knows (Overlay, Darken, ColorBurn, HardLight,
        // SoftLight, Difference).
        _ => ([Add; 2], both(One, OneMinusSrcAlpha)),
    }
}

fn factor(kind: Factor, source: [f32; 4], destination: [f32; 4], channel: usize) -> f32 {
    match kind {
        Factor::Zero => 0.0,
        Factor::One => 1.0,
        Factor::DstColor => destination[channel],
        Factor::DstAlpha => destination[3],
        Factor::SrcAlpha => source[3],
        Factor::OneMinusSrcColor => 1.0 - source[channel],
        Factor::OneMinusSrcAlpha => 1.0 - source[3],
        Factor::OneMinusDstColor => 1.0 - destination[channel],
    }
}

fn blend_pixel(blend: WatermarkBlend, source: [f32; 4], destination: [f32; 4]) -> [f32; 4] {
    let (equations, factors) = gl_state(blend);
    let mut out = [0.0f32; 4];
    for channel in 0..4 {
        let which = usize::from(channel == 3);
        let (source_factor, destination_factor) = factors[which];
        let weighted_source = source[channel] * factor(source_factor, source, destination, channel);
        let weighted_destination = destination[channel] * factor(destination_factor, source, destination, channel);
        out[channel] = match equations[which] {
            Equation::Add => weighted_source + weighted_destination,
            Equation::ReverseSubtract => weighted_destination - weighted_source,
            Equation::Max => source[channel].max(destination[channel]),
        }
        .clamp(0.0, 1.0);
    }
    out
}

/// Tiles the premultiplied watermark `loops` times over the whole atlas with repeat wrapping and bilinear
/// sampling, scaled by `opacity`, blended with the GL state of `blend`. ponytail: no mipmaps, so a heavily
/// minified watermark aliases slightly more than the GL sampler; add a box prefilter if that matters.
fn apply_watermark(atlas: &mut PixelImage, watermark: &Watermark, budget: &mut WorkBudget) -> Result<(), String> {
    let [atlas_w, atlas_h] = atlas.size();
    budget.charge(u64::from(atlas_w) * u64::from(atlas_h))?;
    let [mark_w, mark_h] = watermark.image.size();
    let texels = watermark.image.pixels();
    let texel = |x: i64, y: i64, channel: usize| {
        let x = x.rem_euclid(i64::from(mark_w)) as usize;
        let y = y.rem_euclid(i64::from(mark_h)) as usize;
        let alpha = f32::from(texels[(y * mark_w as usize + x) * 4 + 3]) / 255.0;
        let value = f32::from(texels[(y * mark_w as usize + x) * 4 + channel]) / 255.0;
        if channel == 3 { value } else { value * alpha }
    };
    let loops = watermark.loops as f32;
    for y in 0..atlas_h {
        let v = (y as f32 + 0.5) / atlas_h as f32 * loops;
        let ty = v * mark_h as f32 - 0.5;
        let (y0, fy) = (ty.floor(), ty - ty.floor());
        for x in 0..atlas_w {
            let u = (x as f32 + 0.5) / atlas_w as f32 * loops;
            let tx = u * mark_w as f32 - 0.5;
            let (x0, fx) = (tx.floor(), tx - tx.floor());
            let mut source = [0.0f32; 4];
            for (channel, slot) in source.iter_mut().enumerate() {
                let (xa, ya) = (x0 as i64, y0 as i64);
                let top = texel(xa, ya, channel) * (1.0 - fx) + texel(xa + 1, ya, channel) * fx;
                let bottom = texel(xa, ya + 1, channel) * (1.0 - fx) + texel(xa + 1, ya + 1, channel) * fx;
                *slot = (top * (1.0 - fy) + bottom * fy) * watermark.opacity;
            }
            let at = (y as usize * atlas_w as usize + x as usize) * 4;
            let pixels = atlas.pixels_mut();
            let destination = [0, 1, 2, 3].map(|channel| f32::from(pixels[at + channel]) / 255.0);
            let result = blend_pixel(watermark.blend, source, destination);
            for (channel, value) in result.iter().enumerate() {
                pixels[at + channel] = (value * 255.0 + 0.5).floor().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Ok(())
}
