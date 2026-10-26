//! PSD depth import (slice D4).
//!
//! CPU sampling follows D `ngDepthSample*` and `ngBuildPsdDepthsFromPSD`:
//! layer paths, frontmost composite, white as front, invert, channel modes,
//! alpha threshold, and the documented convolutions. `zeroDepthIsMissing` is
//! on, matching the import builder. One successful edit commits once.
//! Path targets never receive a `depths` array. GPU compose and encodings this
//! parser cannot match fail closed. A changed grid is
//! returned so the session can run the same all-keypoints flush as `depth.set`.

use crate::depths::finite_depth_or_zero;
use crate::document::TextureBlob;
use crate::nodes::{find_node, find_node_mut};
use crate::psd;
use crate::validate_model;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

const OPS_KEY: &str = "depth-ops";
const BASE_KEY: &str = "depth-op-base-depths";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Convolution {
    Nearest,
    Box3x3,
    Box5x5,
    Gaussian3x3,
    Gaussian5x5,
    Median3x3,
    Frontmost3x3,
    Backmost3x3,
    BoxCustom,
    GaussianCustom,
    MedianCustom,
    FrontmostCustom,
    BackmostCustom,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Channel {
    AverageRgb,
    R,
    G,
    B,
    Luminance,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MissingPolicy {
    KeepExisting,
    SetZero,
    SetBack,
    SkipGrid,
}

#[derive(Clone)]
struct Settings {
    invert: bool,
    back_depth: f32,
    front_depth: f32,
    depth_scale: f32,
    alpha_threshold: f32,
    custom_radius: i32,
    convolution: Convolution,
    channel: Channel,
    missing: MissingPolicy,
    match_direct_grid_name: bool,
}

#[derive(Clone)]
struct LayerImage {
    path: String,
    name: String,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    opacity: f32,
    visible: bool,
    rgba: Vec<u8>,
    document_width: i32,
    document_height: i32,
    coverage: Vec<Coverage>,
    clipped: bool,
    retained_bytes: usize,
}

#[derive(Clone)]
struct Coverage {
    rgba: Vec<u8>,
    width: i32,
    height: i32,
    opacity: f32,
    to_local: Affine,
    mesh: Option<CoverageMesh>,
}

#[derive(Clone)]
struct CoverageMesh {
    vertices: Vec<[f32; 2]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<usize>,
}

#[derive(Default)]
struct Mapping {
    path: String,
    ignored: bool,
    manual: bool,
    name: String,
    status: &'static str,
    matched: bool,
    ambiguous: bool,
    target: Option<u32>,
    matched_node: Option<u32>,
}

struct NodeInfo {
    id: u32,
    kind: String,
    name: String,
    layer_path: Option<String>,
}

pub(crate) struct ImportedDepths {
    pub model: Value,
    pub report: Value,
    /// Grids whose depths or depth-ops changed. The caller marks these for
    /// the B8 all-keypoints flush. An unchanged grid is not marked.
    pub changed_grids: Vec<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TargetChoice {
    Auto,
    Ignore,
    Node(u32),
}

/// Per-layer dialog state (D `PsdDepthComposedLayer` edits and the mapping overrides).
#[derive(Clone)]
struct LayerOptions {
    target: TargetChoice,
    enabled: bool,
    /// `None` keeps the visibility stored in the PSD.
    visible: Option<bool>,
    depth_enabled: bool,
    invert: bool,
    depth_scale: f32,
    depth_offset: f32,
}

impl Default for LayerOptions {
    fn default() -> Self {
        Self {
            target: TargetChoice::Auto,
            enabled: true,
            visible: None,
            depth_enabled: true,
            invert: false,
            depth_scale: 1.0,
            depth_offset: 0.0,
        }
    }
}

struct Options {
    layers: HashMap<String, LayerOptions>,
    disabled_grids: Vec<u32>,
}

const LAYER_OPTION_KEYS: [&str; 8] =
    ["layerPath", "target", "enabled", "visible", "depthEnabled", "invert", "depthScale", "depthOffset"];

fn node_id(value: &Value, what: &str) -> Result<u32, String> {
    value
        .as_u64()
        .and_then(|id| u32::try_from(id).ok())
        .ok_or_else(|| format!("{what} must be a node id"))
}

fn options_from(request: &Value) -> Result<Options, String> {
    let mut layers = HashMap::new();
    if let Some(value) = request.get("layers") {
        for entry in value.as_array().ok_or("layers must be an array")? {
            let object = entry.as_object().ok_or("layers entries must be objects")?;
            if let Some(key) = object.keys().find(|key| !LAYER_OPTION_KEYS.contains(&key.as_str())) {
                return Err(format!("unknown layer option: {key}"));
            }
            let path = entry
                .get("layerPath")
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
                .ok_or("layers entries need a layerPath")?;
            let target = match entry.get("target") {
                None => TargetChoice::Auto,
                Some(Value::String(text)) if text == "auto" => TargetChoice::Auto,
                Some(Value::String(text)) if text == "ignore" => TargetChoice::Ignore,
                Some(value) => TargetChoice::Node(
                    node_id(value, "target").map_err(|_| "target must be \"auto\", \"ignore\" or a node id")?,
                ),
            };
            let visible = match entry.get("visible") {
                None => None,
                Some(_) => Some(bool_field(entry, "visible", true)?),
            };
            let options = LayerOptions {
                target,
                enabled: bool_field(entry, "enabled", true)?,
                visible,
                depth_enabled: bool_field(entry, "depthEnabled", true)?,
                invert: bool_field(entry, "invert", false)?,
                depth_scale: f32_field(entry, "depthScale", 1.0)?,
                depth_offset: f32_field(entry, "depthOffset", 0.0)?,
            };
            if layers.insert(path.to_owned(), options).is_some() {
                return Err(format!("duplicate options for layer {path}"));
            }
        }
    }
    let mut disabled_grids = Vec::new();
    if let Some(value) = request.get("disabledGrids") {
        for id in value.as_array().ok_or("disabledGrids must be an array")? {
            disabled_grids.push(node_id(id, "disabledGrids entries")?);
        }
    }
    Ok(Options { layers, disabled_grids })
}

/// One dialog row: the PSD layer, its mapping and the figures D shows in its layer table.
struct Row {
    mapping: Mapping,
    options: LayerOptions,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    opacity: f32,
    visible: bool,
    clipped: bool,
    masked_pixels: usize,
    zero_pixels: usize,
    range01: Option<f32>,
    sampled: usize,
    selected: usize,
}

/// D `computeLayerDepthStats`: accepted pixels, zero-depth pixels and the depth01 range.
fn layer_stats(layer: &LayerImage, settings: &Settings) -> (usize, usize, Option<f32>) {
    let (mut masked, mut zero) = (0, 0);
    let mut range: Option<(f32, f32)> = None;
    for pixel in layer.rgba.chunks_exact(4) {
        if (pixel[3] as f32 / 255.0) * layer.opacity <= settings.alpha_threshold {
            continue;
        }
        let depth01 = pixel_depth01(pixel, settings);
        masked += 1;
        if depth01 <= 0.0 {
            zero += 1;
        }
        range = Some(range.map_or((depth01, depth01), |(low, high)| (low.min(depth01), high.max(depth01))));
    }
    (masked, zero, range.map(|(low, high)| high - low))
}

struct Prepared {
    layer: LayerImage,
    options: LayerOptions,
    settings: Settings,
    row: usize,
}

impl Prepared {
    /// D `DepthDrawLayer.applyZTransform` on the sampled depth.
    fn value(&self, x: f32, y: f32) -> Option<f32> {
        let value = sample_layer(&self.layer, x, y, &self.settings)?;
        Some(value * self.options.depth_scale + self.options.depth_offset)
    }

    /// Whether the layer's alpha covers the nearest pixel, ignoring its depth value.
    fn covers(&self, x: f32, y: f32) -> bool {
        let (px, py) = (x.round() as i32, y.round() as i32);
        let layer = &self.layer;
        if px < 0 || py < 0 || px >= layer.width || py >= layer.height {
            return false;
        }
        let alpha = layer.rgba[(py as usize * layer.width as usize + px as usize) * 4 + 3];
        (alpha as f32 / 255.0) * layer.opacity > self.settings.alpha_threshold
    }
}

/// A layer with depth disabled follows the nearest enabled depth layer below it where it has pixels
/// (D `resolvedPixelAt`). ponytail: only layers of the same target are candidates.
fn resolved_value(layers: &[Prepared], index: usize, doc_x: f32, doc_y: f32) -> Option<f32> {
    let at = |layer: &Prepared| (doc_x - layer.layer.left as f32, doc_y - layer.layer.top as f32);
    let own = &layers[index];
    let (x, y) = at(own);
    if own.options.depth_enabled {
        return own.value(x, y);
    }
    if !own.covers(x, y) {
        return None;
    }
    layers[index + 1..]
        .iter()
        .filter(|below| below.options.depth_enabled)
        .find_map(|below| {
            let (x, y) = at(below);
            below.value(x, y)
        })
}

fn manual_mapping(nodes: &[NodeInfo], layer: &LayerImage, id: u32) -> Result<Mapping, String> {
    let mut mapping = Mapping {
        path: layer.path.clone(),
        name: layer.name.clone(),
        manual: true,
        ..Mapping::default()
    };
    match nodes.iter().find(|node| node.id == id).map(|node| node.kind.as_str()) {
        Some("GridDeformer") => {
            mapping.status = "Manual";
            mapping.matched = true;
            mapping.target = Some(id);
            mapping.matched_node = Some(id);
        }
        Some("PathDeformer") => return Err("PSD depth import does not assign depths to PathDeformer".into()),
        _ => mapping.status = "UnmatchedManualGrid",
    }
    Ok(mapping)
}

pub(crate) fn import_psd(
    model: &Value,
    geometry: &Value,
    textures: &[TextureBlob],
    request: &Value,
) -> Result<ImportedDepths, String> {
    let path = request
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .ok_or("Path not provided")?;
    let settings = settings_from(request)?;
    let options = options_from(request)?;
    let (width, height, mut layers) = read_source(path)?;
    let retained_bytes: usize = layers.iter().map(|layer| layer.retained_bytes).sum();
    let layer_count = layers.len();
    if let Some(missing) = options.layers.keys().find(|key| !layers.iter().any(|layer| &layer.path == *key)) {
        return Err(format!("PSD layer not found: {missing}"));
    }
    // File order is bottom to top. Front layers are sampled first so equal
    // depths keep the front layer, matching D's reverse iteration and `>`.
    layers.reverse();
    let nodes = collect_nodes(model);
    let mut rows: Vec<Row> = Vec::new();
    let mut matched: Vec<(Prepared, u32)> = Vec::new();
    for layer in layers {
        let layer_options = options.layers.get(&layer.path).cloned().unwrap_or_default();
        let mapping = match layer_options.target {
            TargetChoice::Ignore => Mapping {
                path: layer.path.clone(),
                name: layer.name.clone(),
                status: "Ignored",
                ignored: true,
                manual: true,
                ..Mapping::default()
            },
            TargetChoice::Node(id) => manual_mapping(&nodes, &layer, id)?,
            TargetChoice::Auto => match_layer(model, &nodes, &layer, &settings)?,
        };
        let visible = layer_options.visible.unwrap_or(layer.visible);
        let mut layer_settings = settings.clone();
        layer_settings.invert ^= layer_options.invert;
        let (masked_pixels, zero_pixels, range01) = layer_stats(&layer, &layer_settings);
        let usable = mapping.matched && !mapping.ambiguous && visible && layer_options.enabled;
        let (target, matched_node) = (mapping.target, mapping.matched_node);
        rows.push(Row {
            mapping,
            options: layer_options.clone(),
            left: layer.left,
            top: layer.top,
            width: layer.width,
            height: layer.height,
            opacity: layer.opacity,
            visible,
            clipped: layer.clipped,
            masked_pixels,
            zero_pixels,
            range01,
            sampled: 0,
            selected: 0,
        });
        if let (true, Some(target)) = (usable, target) {
            let mut layer = layer;
            attach_coverage(&mut layer, model, geometry, textures, target, matched_node)?;
            matched.push((
                Prepared { layer, options: layer_options, settings: layer_settings, row: rows.len() - 1 },
                target,
            ));
        }
    }

    let mut by_grid: Vec<(u32, Vec<Prepared>)> = Vec::new();
    for (layer, target) in matched {
        if let Some(entry) = by_grid.iter_mut().find(|entry| entry.0 == target) {
            entry.1.push(layer);
        } else {
            by_grid.push((target, vec![layer]));
        }
    }

    let mut candidate = model.clone();
    let mut applied = Vec::new();
    let mut grid_reports = Vec::new();
    for (grid_id, layers) in &by_grid {
        let grid =
            find_node(model, *grid_id).ok_or_else(|| format!("node {grid_id} was not found"))?;
        let grid_name = grid.get("name").and_then(Value::as_str).unwrap_or("").to_owned();
        let coverage_sources: usize = layers.iter().map(|layer| layer.layer.coverage.len()).sum();
        let points = grid_vertices(grid)?;
        let world = world_affine(geometry, *grid_id)?;
        let mut has = vec![false; points.len()];
        let mut best = vec![0.0f32; points.len()];
        let mut winner = vec![0usize; points.len()];
        let mut sampled = vec![0usize; layers.len()];
        for (index, point) in points.iter().enumerate() {
            let (wx, wy) = world.apply(point[0], point[1]);
            let document_x = wx + width as f32 / 2.0;
            let document_y = wy + height as f32 / 2.0;
            for layer in 0..layers.len() {
                if let Some(value) = resolved_value(layers, layer, document_x, document_y) {
                    sampled[layer] += 1;
                    if !has[index] || value > best[index] {
                        has[index] = true;
                        best[index] = value;
                        winner[index] = layer;
                    }
                }
            }
        }
        let skipped = options.disabled_grids.contains(grid_id)
            || (settings.missing == MissingPolicy::SkipGrid && has.iter().any(|present| !present));
        let mut report = json!({
            "target": grid_id,
            "name": grid_name,
            "enabled": !options.disabled_grids.contains(grid_id),
            "skipped": skipped,
            "coverageSources": coverage_sources,
            "sampledVertices": has.iter().filter(|present| **present).count(),
            "missingVertices": has.iter().filter(|present| !**present).count(),
            "minDepth": 0.0,
            "maxDepth": 0.0,
        });
        if !skipped {
            let found: Vec<f32> = (0..points.len())
                .filter(|index| has[*index])
                .map(|index| finite_depth_or_zero(f64::from(best[index])) as f32)
                .collect();
            if let (Some(low), Some(high)) = (
                found.iter().copied().reduce(f32::min),
                found.iter().copied().reduce(f32::max),
            ) {
                report["minDepth"] = json!(f64::from(low));
                report["maxDepth"] = json!(f64::from(high));
            }
        }
        grid_reports.push(report);
        for (layer, count) in layers.iter().zip(&sampled) {
            rows[layer.row].sampled = *count;
        }
        for index in (0..points.len()).filter(|index| has[*index]) {
            rows[layers[winner[index]].row].selected += 1;
        }
        if skipped {
            continue;
        }
        applied.push(*grid_id);
        let existing = existing_depths(grid, points.len());
        let nearest = (settings.missing == MissingPolicy::KeepExisting && has.iter().any(|present| !present))
            .then(|| NearestSampled::new(&points, &has, &best));
        let mut depths = Vec::with_capacity(points.len());
        for index in 0..points.len() {
            let sample = if has[index] {
                finite_depth_or_zero(f64::from(best[index])) as f32
            } else {
                match settings.missing {
                    MissingPolicy::KeepExisting => nearest
                        .as_ref()
                        .and_then(|nearest| nearest.get(index))
                        .unwrap_or(existing[index]),
                    MissingPolicy::SetZero => 0.0,
                    MissingPolicy::SetBack => {
                        finite_depth_or_zero(f64::from(settings.back_depth * settings.depth_scale))
                            as f32
                    }
                    MissingPolicy::SkipGrid => 0.0,
                }
            };
            depths.push(finite_depth_or_zero(f64::from(sample)) as f32);
        }
        let node = find_node_mut(&mut candidate, *grid_id)
            .ok_or_else(|| format!("node {grid_id} was not found"))?;
        let object = node.as_object_mut().ok_or("node must be an object")?;
        object.insert(
            "depths".into(),
            json!(depths.iter().copied().map(f64::from).collect::<Vec<_>>()),
        );
        object.remove(OPS_KEY);
        object.remove(BASE_KEY);
    }
    let reset_targets = reset_applied_source_settings(&mut candidate, &applied);
    validate_model(&candidate)?;
    let unmatched = rows
        .iter()
        .filter(|row| {
            matches!(row.mapping.status, "Unmatched" | "UnmatchedWithoutGrid" | "UnmatchedManualGrid")
                || row.mapping.ambiguous
        })
        .count();
    let ambiguous = rows.iter().filter(|row| row.mapping.ambiguous).count();
    let matched_layers = rows.iter().filter(|row| row.mapping.matched && !row.mapping.ambiguous).count();
    let mut changed_grids: Vec<u32> = by_grid
        .iter()
        .filter(|(id, _)| find_node(&candidate, *id) != find_node(model, *id))
        .map(|(id, _)| *id)
        .collect();
    for id in reset_targets {
        if !changed_grids.contains(&id) {
            changed_grids.push(id);
        }
    }
    let name_of = |id: Option<u32>| {
        id.and_then(|id| nodes.iter().find(|node| node.id == id)).map(|node| node.name.clone())
    };
    let mut diagnostics = Vec::new();
    if rows.is_empty() {
        diagnostics.push("No source layers were loaded from the selected depth source.".to_owned());
    }
    if !rows.is_empty() && by_grid.is_empty() {
        diagnostics.push(
            "No mapped targets were found. Remap a source layer to a GridDeformer or PathDeformer.".to_owned(),
        );
    }
    for row in &rows {
        let subject = &row.mapping.path;
        match row.mapping.status {
            "UnmatchedManualGrid" => {
                diagnostics.push(format!("Mapped target is missing for layer: {subject}"));
                diagnostics.push(format!("missing-manual-target: {subject}"));
            }
            "UnmatchedWithoutGrid" => {
                diagnostics.push(format!("Unsupported target type for layer: {subject}"));
                diagnostics.push(format!("unused-depth-layer: {subject}"));
            }
            "Unmatched" => diagnostics.push(format!("unused-depth-layer: {subject}")),
            "Ambiguous" => diagnostics.push(format!("ambiguous-target: {subject}")),
            _ => {}
        }
    }
    let targets: Vec<Value> = nodes
        .iter()
        .filter(|node| node.kind == "GridDeformer")
        .map(|node| json!({"id": node.id, "name": node.name, "type": node.kind}))
        .collect();
    let layer_reports: Vec<Value> = rows
        .iter()
        .map(|row| {
            let mapping = &row.mapping;
            json!({
                "layerPath": mapping.path,
                "layerName": mapping.name,
                "status": mapping.status,
                "matched": mapping.matched,
                "ambiguous": mapping.ambiguous,
                "ignored": mapping.ignored,
                "manual": mapping.manual,
                "problem": !mapping.matched || mapping.ambiguous || mapping.ignored,
                "target": mapping.target,
                "targetName": name_of(mapping.target),
                "matchedNode": mapping.matched_node,
                "matchedNodeName": name_of(mapping.matched_node),
                "left": row.left,
                "top": row.top,
                "width": row.width,
                "height": row.height,
                "opacity": f64::from(row.opacity),
                "visible": row.visible,
                "clipped": row.clipped,
                "enabled": row.options.enabled,
                "depthEnabled": row.options.depth_enabled,
                "invert": row.options.invert,
                "depthScale": f64::from(row.options.depth_scale),
                "depthOffset": f64::from(row.options.depth_offset),
                "sampledVertices": row.sampled,
                "selectedVertices": row.selected,
                "maskedPixels": row.masked_pixels,
                "zeroPixels": row.zero_pixels,
                "depthRange01": row.range01.map(f64::from),
            })
        })
        .collect();
    let report = json!({
        "document": {"width": width, "height": height},
        "changedGrids": changed_grids.len(),
        "matchedLayers": matched_layers,
        "unmatchedLayers": unmatched,
        "ambiguousLayers": ambiguous,
        "skippedGrids": grid_reports.iter().filter(|grid| grid["skipped"] == true).count(),
        "budget": {
            "retainedBytes": retained_bytes,
            "maxRetainedBytes": MAX_PSD_RETAINED_BYTES,
            "layerCount": layer_count,
        },
        "diagnostics": diagnostics,
        "targets": targets,
        "grids": grid_reports,
        "layers": layer_reports,
        "mappings": rows.iter().map(|row| json!({
            "layerPath": row.mapping.path,
            "layerName": row.mapping.name,
            "status": row.mapping.status,
            "matched": row.mapping.matched,
            "ambiguous": row.mapping.ambiguous,
            "target": row.mapping.target,
        })).collect::<Vec<_>>(),
    });
    Ok(ImportedDepths {
        model: candidate,
        report,
        changed_grids,
    })
}

/// `depth.import_psd.plan`: the same computation as the import without committing it.
pub(crate) fn plan_psd(
    model: &Value,
    geometry: &Value,
    textures: &[TextureBlob],
    request: &Value,
) -> Result<Value, String> {
    import_psd(model, geometry, textures, request).map(|planned| planned.report)
}

/// D resets `depthScale` to 1 and `depthOffset` to 0 on every applied target
/// before the dirty flush (`ngApplyPsdDepthImportResult`).
fn reset_applied_source_settings(model: &mut Value, applied: &[u32]) -> Vec<u32> {
    let mut changed = Vec::new();
    let Some(root) = model.get_mut("nodes") else {
        return changed;
    };
    reset_source_settings(root, applied, &mut changed);
    changed
}

fn reset_source_settings(node: &mut Value, applied: &[u32], changed: &mut Vec<u32>) {
    if node.get("type").and_then(Value::as_str) == Some("DepthRigRoot") {
        if let Some(bindings) = node.get_mut("bindings").and_then(Value::as_array_mut) {
            for binding in bindings {
                let Some(target) = binding
                    .get("target")
                    .and_then(Value::as_u64)
                    .and_then(|id| u32::try_from(id).ok())
                else {
                    continue;
                };
                if !applied.contains(&target) {
                    continue;
                }
                let Some(settings) = binding.get_mut("sourceSettings").and_then(Value::as_array_mut)
                else {
                    continue;
                };
                let mut reset = false;
                for setting in settings.iter_mut() {
                    let Some(object) = setting.as_object_mut() else {
                        continue;
                    };
                    let scale = object.get("depthScale").and_then(Value::as_f64).unwrap_or(1.0);
                    let offset = object.get("depthOffset").and_then(Value::as_f64).unwrap_or(0.0);
                    if scale == 1.0 && offset == 0.0 {
                        continue;
                    }
                    object.insert("depthScale".into(), json!(1.0));
                    object.insert("depthOffset".into(), json!(0.0));
                    reset = true;
                }
                if reset && !changed.contains(&target) {
                    changed.push(target);
                }
            }
        }
    }
    let child_count = node.get("children").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
    for index in 0..child_count {
        let Some(child) = node
            .get_mut("children")
            .and_then(Value::as_array_mut)
            .and_then(|children| children.get_mut(index))
        else {
            continue;
        };
        reset_source_settings(child, applied, changed);
    }
}

fn settings_from(request: &Value) -> Result<Settings, String> {
    for key in ["useGpuComposition", "colorSource", "repairContourBand", "smoothWavySurface"] {
        if request.get(key).is_some() {
            return Err(format!("unsupported PSD depth import option: {key}"));
        }
    }
    let back_depth = f32_field(request, "backDepth", -1.0)?;
    let front_depth = f32_field(request, "frontDepth", 1.0)?;
    let depth_scale = f32_field(request, "depthScale", 1.0)?;
    if depth_scale < 0.0 {
        return Err("Depth scale must be non-negative".into());
    }
    let alpha_threshold = f32_field(request, "alphaThreshold", 0.01)?;
    if !(0.0..=1.0).contains(&alpha_threshold) {
        return Err("Alpha threshold must be in [0, 1]".into());
    }
    let custom_radius = i32_field(request, "customRadius", 3)?;
    if !(1..=64).contains(&custom_radius) {
        return Err("Custom radius must be in [1, 64]".into());
    }
    Ok(Settings {
        invert: bool_field(request, "invert", false)?,
        back_depth,
        front_depth,
        depth_scale,
        alpha_threshold,
        custom_radius,
        convolution: convolution_field(request)?,
        channel: channel_field(request)?,
        missing: missing_field(request)?,
        match_direct_grid_name: bool_field(request, "matchDirectGridName", true)?,
    })
}

fn read_source(path: &str) -> Result<(i32, i32, Vec<LayerImage>), String> {
    let bytes = fs::read(path).map_err(|error| format!("could not read depth source: {error}"))?;
    let extension = Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "psd" => parse_psd(&bytes),
        "png" => parse_png(path, &bytes),
        _ => Err("unsupported depth source format".into()),
    }
}

fn parse_png(path: &str, bytes: &[u8]) -> Result<(i32, i32, Vec<LayerImage>), String> {
    let image = image::load_from_memory(bytes)
        .map_err(|error| format!("unsupported PNG depth source: {error}"))?;
    let rgba = image.to_rgba8();
    let width =
        i32::try_from(rgba.width()).map_err(|_| "unsupported PNG depth source".to_string())?;
    let height =
        i32::try_from(rgba.height()).map_err(|_| "unsupported PNG depth source".to_string())?;
    let name = Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("depth")
        .to_owned();
    Ok((
        width,
        height,
        vec![LayerImage {
            path: format!("/{name}"),
            name,
            left: 0,
            top: 0,
            width,
            height,
            opacity: 1.0,
            visible: true,
            rgba: rgba.into_raw(),
            document_width: width,
            document_height: height,
            coverage: Vec::new(),
            clipped: false,
            retained_bytes: 0,
        }],
    ))
}

const MAX_PSD_RETAINED_BYTES: usize = 256 * 1024 * 1024;
const PSD_DEPTH_RETAINED_BYTES_PER_PIXEL: usize = 40;
// D `PsdMaskExtractionBytesPerPixel`: a decoded mask plus its feather buffers.
const PSD_MASK_RETAINED_BYTES_PER_PIXEL: usize = 8;

/// Bytes D reserves while extracting one layer (`ngReservePsdLayerExtraction`).
fn retained_layer_bytes(layer: &psd::Layer) -> Result<usize, String> {
    let over = || "PSD Depth Map layers exceed the retained image memory budget".to_string();
    let rect = |width: u32, height: u32| (width as usize).checked_mul(height as usize).ok_or_else(over);
    let mut bytes = rect(layer.width(), layer.height())?
        .checked_mul(PSD_DEPTH_RETAINED_BYTES_PER_PIXEL)
        .ok_or_else(over)?;
    for channel in &layer.channels {
        let mask = match channel.id {
            psd::CHANNEL_LAYER_MASK => layer.layer_mask,
            psd::CHANNEL_LAYER_OR_VECTOR_MASK => layer.vector_mask.or(layer.layer_mask),
            _ => continue,
        };
        let (width, height) = match mask {
            Some(mask) if mask.right > mask.left && mask.bottom > mask.top => (
                (i64::from(mask.right) - i64::from(mask.left)) as u32,
                (i64::from(mask.bottom) - i64::from(mask.top)) as u32,
            ),
            _ => (layer.width(), layer.height()),
        };
        bytes = rect(width, height)?
            .checked_mul(PSD_MASK_RETAINED_BYTES_PER_PIXEL)
            .and_then(|mask_bytes| bytes.checked_add(mask_bytes))
            .ok_or_else(over)?;
    }
    Ok(bytes)
}

/// Parses a PSD into depth layers in file order (bottom to top).
///
/// Photoshop writes a group as `divider, children..., folder`, so group nesting and clipping bases are
/// resolved on the reversed list (top to bottom) for nesting and bottom to top for clipping. D's
/// `ngPsdLayerGroupStates` walks the raw file order forward instead, which only nests correctly for a
/// folder-first synthetic order; real files would put the layers above a group inside it.
fn parse_psd(bytes: &[u8]) -> Result<(i32, i32, Vec<LayerImage>), String> {
    let document = psd::parse(bytes, &psd::Limits::default())?;
    let width = i32::try_from(document.width).map_err(|_| "PSD document dimensions are too large")?;
    let height = i32::try_from(document.height).map_err(|_| "PSD document dimensions are too large")?;
    let count = document.layers.len();
    let top_down: Vec<psd::Layer> = document.layers.iter().rev().cloned().collect();
    let states = psd::group_states(&top_down);
    let mut layers = Vec::new();
    let mut clipping_info = Vec::new();
    let mut retained_bytes = 0usize;
    for (index, layer) in document.layers.iter().enumerate() {
        if !layer.has_pixel_data() {
            continue;
        }
        let state = &states[count - 1 - index];
        let layer_bytes = retained_layer_bytes(layer)?;
        retained_bytes = retained_bytes
            .checked_add(layer_bytes)
            .filter(|bytes| *bytes <= MAX_PSD_RETAINED_BYTES)
            .ok_or("PSD Depth Map layers exceed the retained image memory budget")?;
        let rgba = document.layer_rgba(index)?;
        layers.push(LayerImage {
            path: state.path.clone(),
            name: layer.name.clone(),
            left: layer.left,
            top: layer.top,
            width: layer.width() as i32,
            height: layer.height() as i32,
            opacity: state.opacity * f32::from(layer.opacity) / 255.0,
            visible: state.visible && layer.visible(),
            rgba,
            document_width: width,
            document_height: height,
            coverage: Vec::new(),
            clipped: layer.clipped,
            retained_bytes: layer_bytes,
        });
        clipping_info.push((state.path.clone(), layer.clipped, state.opacity));
    }
    // Bottom to top: a clipped layer takes the nearest unclipped layer below it in the same group.
    let mut clipping_bases: HashMap<String, ClippingBase> = HashMap::new();
    for index in 0..layers.len() {
        let (group_path, clipped, group_opacity) = &clipping_info[index];
        if *clipped {
            if let Some(base) = clipping_bases.get(group_path) {
                let layer = &mut layers[index];
                layer.visible &= base.visible;
                let base_local_opacity = if *group_opacity > 0.0 {
                    (base.opacity / *group_opacity).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                layer.opacity *= base_local_opacity;
                apply_clipping_base(layer, base);
            }
        } else {
            clipping_bases.insert(group_path.clone(), ClippingBase::from_layer(&layers[index]));
        }
    }
    assign_paths(&mut layers);
    Ok((width, height, layers))
}

struct ClippingBase {
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    visible: bool,
    opacity: f32,
    alpha: Vec<u8>,
}

impl ClippingBase {
    fn from_layer(layer: &LayerImage) -> Self {
        Self {
            left: layer.left,
            top: layer.top,
            width: layer.width,
            height: layer.height,
            visible: layer.visible,
            opacity: layer.opacity,
            alpha: layer.rgba.chunks_exact(4).map(|pixel| pixel[3]).collect(),
        }
    }
}

fn apply_clipping_base(layer: &mut LayerImage, base: &ClippingBase) {
    for (index, pixel) in layer.rgba.chunks_exact_mut(4).enumerate() {
        let x = i64::from(layer.left) + (index % layer.width as usize) as i64
            - i64::from(base.left);
        let y = i64::from(layer.top) + (index / layer.width as usize) as i64
            - i64::from(base.top);
        let base_alpha = if x >= 0 && y >= 0 && x < i64::from(base.width) && y < i64::from(base.height) {
            base.alpha[y as usize * base.width as usize + x as usize]
        } else {
            0
        };
        pixel[3] = ((u16::from(pixel[3]) * u16::from(base_alpha) + 127) / 255) as u8;
    }
}

fn assign_paths(layers: &mut [LayerImage]) {
    let mut seen: Vec<(String, usize)> = Vec::new();
    // D numbers duplicates while walking front-to-back, so the front layer keeps `/Name`.
    for layer in layers.iter_mut().rev() {
        let base = if layer.path.is_empty() {
            format!("/{}", layer.name)
        } else {
            format!("{}/{}", layer.path, layer.name)
        };
        let occurrence = if let Some(entry) = seen.iter_mut().find(|entry| entry.0 == base) {
            entry.1 += 1;
            entry.1
        } else {
            seen.push((base.clone(), 1));
            1
        };
        layer.path = if occurrence == 1 {
            base
        } else {
            format!("{base} [#{occurrence}]")
        };
    }
}

#[derive(Clone, Copy)]
struct Affine {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl Affine {
    fn identity() -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 0.0,
            e: 1.0,
            f: 0.0,
        }
    }

    fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.b * y + self.c,
            self.d * x + self.e * y + self.f,
        )
    }

    fn then(self, local: Self) -> Self {
        Self {
            a: self.a * local.a + self.b * local.d,
            b: self.a * local.b + self.b * local.e,
            c: self.a * local.c + self.b * local.f + self.c,
            d: self.d * local.a + self.e * local.d,
            e: self.d * local.b + self.e * local.e,
            f: self.d * local.c + self.e * local.f + self.f,
        }
    }

    fn inverse(self) -> Result<Self, String> {
        let det = self.a * self.e - self.b * self.d;
        if det.abs() < 1.0e-8 {
            return Err("PSD depth import cannot sample a degenerate part transform".into());
        }
        Ok(Self {
            a: self.e / det,
            b: -self.b / det,
            c: (self.b * self.f - self.e * self.c) / det,
            d: -self.d / det,
            e: self.a / det,
            f: (self.d * self.c - self.a * self.f) / det,
        })
    }
}

/// A node's world transform as the row-major 4x4 `depth_draw::document_points` takes (translation in column 3).
pub(crate) fn world_matrix(model: &Value, id: u32) -> Result<[[f64; 4]; 4], String> {
    let a = world_affine(model, id)?;
    let f = |v: f32| f64::from(v);
    Ok([
        [f(a.a), f(a.b), 0.0, f(a.c)],
        [f(a.d), f(a.e), 0.0, f(a.f)],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

fn world_affine(model: &Value, id: u32) -> Result<Affine, String> {
    let root = model.get("nodes").ok_or("model is missing nodes")?;
    let mut found = None;
    fn walk(
        node: &Value,
        target: u32,
        chain: Affine,
        found: &mut Option<Affine>,
    ) -> Result<(), String> {
        let local = local_affine(node)?;
        let chain = chain.then(local);
        let node_id = node
            .get("uuid")
            .and_then(Value::as_u64)
            .ok_or("node is missing uuid")?;
        if node_id == u64::from(target) {
            *found = Some(chain);
            return Ok(());
        }
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            for child in children {
                walk(child, target, chain, found)?;
                if found.is_some() {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
    walk(root, id, Affine::identity(), &mut found)?;
    found.ok_or_else(|| format!("node {id} was not found"))
}

fn local_affine(node: &Value) -> Result<Affine, String> {
    let Some(transform) = node.get("transform").and_then(Value::as_object) else {
        return Ok(Affine::identity());
    };
    let trans = numbers(transform.get("trans"), 3, "transform.trans")?;
    let rot = numbers(transform.get("rot"), 3, "transform.rot")?;
    let scale = numbers(transform.get("scale"), 2, "transform.scale")?;
    if rot[0].abs() > 1e-12 || rot[1].abs() > 1e-12 {
        return Err("PSD depth import cannot sample X/Y rotation".into());
    }
    let (sin, cos) = (rot[2] as f32).sin_cos();
    let sx = scale[0] as f32;
    let sy = scale[1] as f32;
    Ok(Affine {
        a: cos * sx,
        b: -sin * sy,
        c: trans[0] as f32,
        d: sin * sx,
        e: cos * sy,
        f: trans[1] as f32,
    })
}

fn grid_vertices(node: &Value) -> Result<Vec<[f32; 2]>, String> {
    let xs = axis(node, "grid_axis_x")?;
    let ys = axis(node, "grid_axis_y")?;
    if xs.len() < 2 || ys.len() < 2 {
        return Err("GridDeformer vertices are not a rectangular grid".into());
    }
    let mut points = Vec::with_capacity(xs.len() * ys.len());
    for y in &ys {
        for x in &xs {
            points.push([*x, *y]);
        }
    }
    Ok(points)
}

fn axis(node: &Value, key: &str) -> Result<Vec<f32>, String> {
    let values = node
        .get(key)
        .and_then(Value::as_array)
        .ok_or("GridDeformer vertices are not a rectangular grid")?;
    values
        .iter()
        .map(|value| {
            let number = value
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or("GridDeformer vertices are not a rectangular grid")?;
            let sample = number as f32;
            if sample.is_finite() {
                Ok(sample)
            } else {
                Err("GridDeformer vertices are not a rectangular grid".into())
            }
        })
        .collect()
}

/// D `applyPsdDepthMissingPolicy` (KeepExisting): the depth of the nearest vertex that was sampled, by squared
/// distance in the grid's own coordinates (the lowest index on ties); `None` when no vertex has a finite sample.
///
/// D scans every vertex for each missing one. This buckets the sampled vertices in a square-cell hash and searches
/// outwards ring by ring, which returns the same vertex and keeps a large grid linear.
struct NearestSampled<'a> {
    points: &'a [[f32; 2]],
    best: &'a [f32],
    cell: f32,
    origin: [f32; 2],
    rings: i64,
    buckets: HashMap<(i64, i64), Vec<usize>>,
}

impl<'a> NearestSampled<'a> {
    fn new(points: &'a [[f32; 2]], has: &[bool], best: &'a [f32]) -> Self {
        let sampled = |i: usize| has[i] && best[i].is_finite();
        // The cells cover the whole grid, so a missing vertex always starts inside them.
        let (mut low, mut high) = ([f32::MAX; 2], [f32::MIN; 2]);
        for point in points {
            for axis in 0..2 {
                low[axis] = low[axis].min(point[axis]);
                high[axis] = high[axis].max(point[axis]);
            }
        }
        let count = (0..points.len()).filter(|i| sampled(*i)).count();
        // A square cell holding about one sampled vertex on average (a line of points still gets cells).
        let span = [(high[0] - low[0]).max(0.0), (high[1] - low[1]).max(0.0)];
        let longest = span[0].max(span[1]);
        let cell = if count == 0 || !(longest > 0.0) || !longest.is_finite() {
            1.0
        } else if span[0] > 0.0 && span[1] > 0.0 {
            (span[0] * span[1] / count as f32).sqrt().max(longest / 4096.0)
        } else {
            (longest / count as f32).max(longest / 4096.0)
        };
        let mut nearest = Self { points, best, cell, origin: low, rings: 0, buckets: HashMap::new() };
        for (i, point) in points.iter().enumerate().filter(|(i, _)| sampled(*i)) {
            nearest.buckets.entry(nearest.cell_of(*point)).or_default().push(i);
        }
        nearest.rings = if longest.is_finite() { (longest / cell).ceil() as i64 + 2 } else { 2 };
        nearest
    }

    fn cell_of(&self, point: [f32; 2]) -> (i64, i64) {
        let at = |axis: usize| ((point[axis] - self.origin[axis]) / self.cell).floor() as i64;
        (at(0), at(1))
    }

    fn get(&self, index: usize) -> Option<f32> {
        if self.buckets.is_empty() {
            return None;
        }
        let from = self.points[index];
        let (cx, cy) = self.cell_of(from);
        let mut found: Option<(f32, usize)> = None;
        for ring in 0..=self.rings {
            let mut visit = |x: i64, y: i64| {
                for &other in self.buckets.get(&(x, y)).into_iter().flatten() {
                    let (dx, dy) = (self.points[other][0] - from[0], self.points[other][1] - from[1]);
                    let distance = dx * dx + dy * dy;
                    if found.is_none_or(|(shortest, first)| distance < shortest || (distance == shortest && other < first)) {
                        found = Some((distance, other));
                    }
                }
            };
            if ring == 0 {
                visit(cx, cy);
            } else {
                for x in cx - ring..=cx + ring {
                    visit(x, cy - ring);
                    visit(x, cy + ring);
                }
                for y in cy - ring + 1..cy + ring {
                    visit(cx - ring, y);
                    visit(cx + ring, y);
                }
            }
            // Everything in a farther ring is at least `ring * cell` away.
            let reach = ring as f32 * self.cell;
            if found.is_some_and(|(shortest, _)| shortest < reach * reach) {
                break;
            }
        }
        found.map(|(_, other)| self.best[other])
    }
}

fn existing_depths(node: &Value, count: usize) -> Vec<f32> {
    let Some(values) = node.get("depths").and_then(Value::as_array) else {
        return vec![0.0; count];
    };
    if values.len() != count {
        return vec![0.0; count];
    }
    values
        .iter()
        .map(|value| finite_depth_or_zero(value.as_f64().unwrap_or(0.0)) as f32)
        .collect()
}

fn collect_nodes(model: &Value) -> Vec<NodeInfo> {
    let mut nodes = Vec::new();
    if let Some(root) = model.get("nodes") {
        walk_nodes(root, &mut nodes);
    }
    nodes
}

fn walk_nodes(node: &Value, nodes: &mut Vec<NodeInfo>) {
    if let Some(id) = node
        .get("uuid")
        .and_then(Value::as_u64)
        .and_then(|id| u32::try_from(id).ok())
    {
        nodes.push(NodeInfo {
            id,
            kind: node
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("Node")
                .to_owned(),
            name: node
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            layer_path: node
                .get("layerPath")
                .and_then(Value::as_str)
                .map(str::to_owned),
        });
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            walk_nodes(child, nodes);
        }
    }
}

fn match_layer(
    model: &Value,
    nodes: &[NodeInfo],
    layer: &LayerImage,
    settings: &Settings,
) -> Result<Mapping, String> {
    let mut candidates: Vec<(u32, i32)> = Vec::new();
    for node in nodes.iter().filter(|node| node.kind == "Part") {
        let path = node.layer_path.clone().unwrap_or_else(|| {
            if node.name.is_empty() {
                String::new()
            } else {
                format!("/{}", node.name)
            }
        });
        if path.is_empty() {
            continue;
        }
        let priority = if path == layer.path {
            Some(0)
        } else if base_name(&path) == layer.name {
            Some(1)
        } else if node.name == layer.name {
            Some(2)
        } else if normalize_layer_name(&path) == normalize_layer_name(&layer.name)
            || normalize_layer_name(&node.name) == normalize_layer_name(&layer.name)
        {
            Some(3)
        } else {
            None
        };
        if let Some(priority) = priority {
            candidates.push((node.id, priority));
        }
    }
    if settings.match_direct_grid_name {
        for node in nodes {
            if matches!(node.kind.as_str(), "GridDeformer" | "PathDeformer")
                && node.name == layer.name
            {
                candidates.push((node.id, 5));
            }
        }
    }
    if candidates.is_empty() {
        return Ok(Mapping {
            path: layer.path.clone(),
            name: layer.name.clone(),
            status: "Unmatched",
            matched: false,
            ambiguous: false,
            target: None,
            matched_node: None,
            ..Mapping::default()
        });
    }
    let best = candidates
        .iter()
        .map(|candidate| candidate.1)
        .min()
        .unwrap_or(0);
    candidates.retain(|candidate| candidate.1 == best);
    let mut target_id = None;
    let mut ambiguous = false;
    let mut matched_node = None;
    for (id, _) in &candidates {
        let node = find_node(model, *id).ok_or_else(|| format!("node {id} was not found"))?;
        let Some(target) = containing_target(model, node) else {
            if matched_node.is_none() {
                matched_node = Some(*id);
            }
            continue;
        };
        let target_uuid = target
            .get("uuid")
            .and_then(Value::as_u64)
            .and_then(|id| u32::try_from(id).ok());
        let Some(target_uuid) = target_uuid else {
            continue;
        };
        if target.get("type").and_then(Value::as_str) == Some("PathDeformer") {
            return Err("PSD depth import does not assign depths to PathDeformer".into());
        }
        if let Some(previous) = target_id {
            if previous != target_uuid {
                ambiguous = true;
                target_id = None;
                break;
            }
        }
        target_id = Some(target_uuid);
        matched_node = Some(*id);
    }
    if ambiguous {
        return Ok(Mapping {
            path: layer.path.clone(),
            name: layer.name.clone(),
            status: "Ambiguous",
            matched: false,
            ambiguous: true,
            target: None,
            matched_node: None,
            ..Mapping::default()
        });
    }
    if target_id.is_none() {
        return Ok(Mapping {
            path: layer.path.clone(),
            name: layer.name.clone(),
            status: "UnmatchedWithoutGrid",
            matched: false,
            ambiguous: false,
            target: matched_node,
            matched_node,
            ..Mapping::default()
        });
    }
    Ok(Mapping {
        path: layer.path.clone(),
        name: layer.name.clone(),
        status: "Matched",
        matched: true,
        ambiguous: false,
        target: target_id,
        matched_node,
        ..Mapping::default()
    })
}

fn containing_target<'a>(model: &'a Value, node: &'a Value) -> Option<&'a Value> {
    parent_target(
        model.get("nodes")?,
        node.get("uuid").and_then(Value::as_u64)?,
    )
}

fn parent_target(root: &Value, id: u64) -> Option<&Value> {
    fn walk<'a>(node: &'a Value, id: u64, deformer: Option<&'a Value>) -> Option<&'a Value> {
        let kind = node.get("type").and_then(Value::as_str);
        let here = if matches!(kind, Some("GridDeformer" | "PathDeformer")) {
            Some(node)
        } else {
            deformer
        };
        if node.get("uuid").and_then(Value::as_u64) == Some(id) {
            return here;
        }
        node.get("children")
            .and_then(Value::as_array)
            .and_then(|children| children.iter().find_map(|child| walk(child, id, here)))
    }
    walk(root, id, None)
}

fn attach_coverage(
    layer: &mut LayerImage,
    model: &Value,
    geometry: &Value,
    textures: &[TextureBlob],
    grid_id: u32,
    matched_node: Option<u32>,
) -> Result<(), String> {
    let mut decoded = HashMap::new();
    let mut parts = Vec::new();
    if let Some(root) = model.get("nodes") {
        collect_parts(root, &mut parts);
    }
    let matched_is_part = matched_node
        .and_then(|id| parts.iter().find(|part| part.id == id))
        .is_some();
    for part in parts {
        if !part.active {
            continue;
        }
        if matched_is_part && matched_node != Some(part.id) {
            continue;
        }
        if !matched_is_part && part.target != Some(grid_id) {
            continue;
        }
        let Some(slot) = part.texture else {
            continue;
        };
        let world = world_affine(geometry, part.id)?;
        let to_local = world.inverse()?;
        let texture = decode_coverage_texture(textures, slot, &mut decoded)?;
        layer.coverage.push(Coverage {
            rgba: texture.rgba,
            width: texture.width,
            height: texture.height,
            opacity: part.opacity,
            to_local,
            mesh: part.mesh,
        });
    }
    Ok(())
}

struct DecodedCoverage {
    rgba: Vec<u8>,
    width: i32,
    height: i32,
}

fn decode_coverage_texture(
    textures: &[TextureBlob],
    slot: usize,
    decoded: &mut HashMap<usize, DecodedCoverage>,
) -> Result<DecodedCoverage, String> {
    if let Some(texture) = decoded.get(&slot) {
        return Ok(DecodedCoverage {
            rgba: texture.rgba.clone(),
            width: texture.width,
            height: texture.height,
        });
    }
    let blob = textures.get(slot).ok_or("PSD depth import cannot sample part coverage")?;
    let image = image::load_from_memory(&blob.data)
        .map_err(|_| "PSD depth import cannot sample part coverage".to_string())?;
    let rgba = image.to_rgba8();
    let width = i32::try_from(rgba.width()).map_err(|_| "PSD depth import cannot sample part coverage".to_string())?;
    let height = i32::try_from(rgba.height()).map_err(|_| "PSD depth import cannot sample part coverage".to_string())?;
    let texture = DecodedCoverage {
        rgba: rgba.into_raw(),
        width,
        height,
    };
    decoded.insert(slot, DecodedCoverage {
        rgba: texture.rgba.clone(),
        width,
        height,
    });
    Ok(texture)
}

struct PartCoverage {
    id: u32,
    active: bool,
    texture: Option<usize>,
    opacity: f32,
    target: Option<u32>,
    mesh: Option<CoverageMesh>,
}

fn collect_parts(node: &Value, parts: &mut Vec<PartCoverage>) {
    // Pointers are only used while `model` is borrowed for this call.
    let _ = node;
    fn walk(node: &Value, deformer: Option<u32>, parts: &mut Vec<PartCoverage>) {
        let kind = node.get("type").and_then(Value::as_str);
        let id = node.get("uuid").and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok());
        let here = if matches!(kind, Some("GridDeformer" | "PathDeformer")) {
            id
        } else {
            deformer
        };
        if kind == Some("Part") {
            if let Some(id) = id {
                let enabled = node.get("enabled").and_then(Value::as_bool).unwrap_or(true);
                let texture = node
                    .get("textures")
                    .and_then(Value::as_array)
                    .and_then(|values| values.first())
                    .and_then(Value::as_u64)
                    .and_then(|slot| usize::try_from(slot).ok());
                let opacity = node.get("opacity").and_then(Value::as_f64).unwrap_or(1.0) as f32;
                parts.push(PartCoverage {
                    id,
                    active: enabled,
                    texture,
                    opacity,
                    target: here,
                    mesh: mesh_of(node),
                });
            }
        }
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            for child in children {
                walk(child, here, parts);
            }
        }
    }
    walk(node, None, parts);
}

fn mesh_of(node: &Value) -> Option<CoverageMesh> {
    let mesh = node.get("mesh")?.as_object()?;
    let verts = pairs(mesh.get("verts"))?;
    let uvs = pairs(mesh.get("uvs"))?;
    if verts.len() != uvs.len() || verts.is_empty() {
        return None;
    }
    let origin = pairs(mesh.get("origin")).unwrap_or_else(|| vec![[0.0, 0.0]]);
    let origin = origin.first().copied().unwrap_or([0.0, 0.0]);
    let indices = mesh.get("indices")?.as_array()?;
    if indices.len() < 3 || indices.len() % 3 != 0 {
        return None;
    }
    let indices = indices
        .iter()
        .map(|value| value.as_u64().and_then(|index| usize::try_from(index).ok()))
        .collect::<Option<Vec<_>>>()?;
    Some(CoverageMesh {
        vertices: verts
            .into_iter()
            .map(|point| [point[0] - origin[0], point[1] - origin[1]])
            .collect(),
        uvs,
        indices,
    })
}

fn pairs(value: Option<&Value>) -> Option<Vec<[f32; 2]>> {
    let values = value?.as_array()?;
    if values.len() % 2 != 0 {
        return None;
    }
    let mut points = Vec::with_capacity(values.len() / 2);
    for pair in values.chunks(2) {
        let x = pair[0].as_f64()? as f32;
        let y = pair[1].as_f64()? as f32;
        points.push([x, y]);
    }
    Some(points)
}

fn coverage_alpha(layer: &LayerImage, x: i32, y: i32) -> f32 {
    if x < 0 || y < 0 || x >= layer.width || y >= layer.height {
        return 0.0;
    }
    let world_x = (layer.left + x) as f32 - layer.document_width as f32 / 2.0;
    let world_y = (layer.top + y) as f32 - layer.document_height as f32 / 2.0;
    let mut combined = 0.0f32;
    for source in &layer.coverage {
        let (local_x, local_y) = source.to_local.apply(world_x, world_y);
        let mut alpha = source_alpha(source, local_x, local_y);
        if alpha <= 0.0 {
            continue;
        }
        alpha = (alpha * source.opacity).clamp(0.0, 1.0);
        combined = 1.0 - (1.0 - combined.clamp(0.0, 1.0)) * (1.0 - alpha);
        if combined >= 1.0 {
            return 1.0;
        }
    }
    combined
}

fn source_alpha(source: &Coverage, local_x: f32, local_y: f32) -> f32 {
    if let Some(mesh) = &source.mesh {
        let point = [local_x, local_y];
        for triangle in mesh.indices.chunks(3) {
            let (i0, i1, i2) = (triangle[0], triangle[1], triangle[2]);
            if i0 >= mesh.vertices.len() || i1 >= mesh.vertices.len() || i2 >= mesh.vertices.len() {
                continue;
            }
            let Some((w0, w1, w2)) = barycentric(point, mesh.vertices[i0], mesh.vertices[i1], mesh.vertices[i2])
            else {
                continue;
            };
            let uv = [
                mesh.uvs[i0][0] * w0 + mesh.uvs[i1][0] * w1 + mesh.uvs[i2][0] * w2,
                mesh.uvs[i0][1] * w0 + mesh.uvs[i1][1] * w1 + mesh.uvs[i2][1] * w2,
            ];
            if uv[0] < 0.0 || uv[0] > 1.0 || uv[1] < 0.0 || uv[1] > 1.0 {
                return 0.0;
            }
            return alpha_at_uv(source, uv[0], uv[1]);
        }
        return 0.0;
    }
    let uv_x = local_x / source.width as f32 + 0.5;
    let uv_y = local_y / source.height as f32 + 0.5;
    if uv_x < 0.0 || uv_x > 1.0 || uv_y < 0.0 || uv_y > 1.0 {
        return 0.0;
    }
    alpha_at_uv(source, uv_x, uv_y)
}

fn alpha_at_uv(source: &Coverage, uv_x: f32, uv_y: f32) -> f32 {
    if source.width <= 0 || source.height <= 0 {
        return 0.0;
    }
    let px = (uv_x * source.width as f32).floor().clamp(0.0, (source.width - 1) as f32) as usize;
    let py = (uv_y * source.height as f32).floor().clamp(0.0, (source.height - 1) as f32) as usize;
    let index = (py * source.width as usize + px) * 4 + 3;
    source.rgba.get(index).copied().unwrap_or(0) as f32 / 255.0
}

fn barycentric(point: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> Option<(f32, f32, f32)> {
    let denom = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
    if denom == 0.0 {
        return None;
    }
    let w0 = ((b[1] - c[1]) * (point[0] - c[0]) + (c[0] - b[0]) * (point[1] - c[1])) / denom;
    let w1 = ((c[1] - a[1]) * (point[0] - c[0]) + (a[0] - c[0]) * (point[1] - c[1])) / denom;
    let w2 = 1.0 - w0 - w1;
    const EPSILON: f32 = -0.0001;
    (w0 >= EPSILON && w1 >= EPSILON && w2 >= EPSILON).then_some((w0, w1, w2))
}

fn sample_layer(layer: &LayerImage, x: f32, y: f32, settings: &Settings) -> Option<f32> {
    let cx = x.round() as i32;
    let cy = y.round() as i32;
    let radius = convolution_radius(settings.convolution, settings.custom_radius);
    if settings.convolution == Convolution::Nearest {
        return sample_pixel(layer, cx, cy, settings).map(|sample| sample.0);
    }
    if uses_weighted(settings.convolution) {
        let mut total = 0.0f32;
        let mut weight_total = 0.0f32;
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                let Some((value, alpha)) = sample_pixel(layer, cx + dx, cy + dy, settings) else {
                    continue;
                };
                let weight =
                    kernel_weight(settings.convolution, settings.custom_radius, dx, dy) * alpha;
                if weight <= 0.0 {
                    continue;
                }
                total += value * weight;
                weight_total += weight;
            }
        }
        return (weight_total > 0.0).then_some(total / weight_total);
    }
    let mut values = Vec::new();
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            if let Some((value, _)) = sample_pixel(layer, cx + dx, cy + dy, settings) {
                values.push(value);
            }
        }
    }
    if values.is_empty() {
        return None;
    }
    if uses_median(settings.convolution) {
        values.sort_by(f32::total_cmp);
        return Some(values[values.len() / 2]);
    }
    if uses_front(settings.convolution) {
        return values.into_iter().reduce(f32::max);
    }
    if uses_back(settings.convolution) {
        return values.into_iter().reduce(f32::min);
    }
    None
}

fn sample_pixel(layer: &LayerImage, x: i32, y: i32, settings: &Settings) -> Option<(f32, f32)> {
    if x < 0 || y < 0 || x >= layer.width || y >= layer.height {
        return None;
    }
    let index = (y as usize * layer.width as usize + x as usize) * 4;
    let pixel = layer.rgba.get(index..index + 4)?;
    let psd_alpha = (pixel[3] as f32 / 255.0) * layer.opacity;
    let alpha = if layer.coverage.is_empty() {
        psd_alpha
    } else {
        coverage_alpha(layer, x, y)
    };
    if alpha <= settings.alpha_threshold {
        return None;
    }
    let depth01 = pixel_depth01(pixel, settings);
    if depth01 <= 0.0 {
        return None;
    }
    let depth = (settings.back_depth + (settings.front_depth - settings.back_depth) * depth01)
        * settings.depth_scale;
    Some((depth, alpha))
}

fn pixel_depth01(pixel: &[u8], settings: &Settings) -> f32 {
    let red = pixel[0] as f32;
    let green = pixel[1] as f32;
    let blue = pixel[2] as f32;
    let mut depth01 = match settings.channel {
        Channel::AverageRgb => (red + green + blue) / (255.0 * 3.0),
        Channel::R => red / 255.0,
        Channel::G => green / 255.0,
        Channel::B => blue / 255.0,
        Channel::Luminance => (0.2126 * red + 0.7152 * green + 0.0722 * blue) / 255.0,
    };
    if settings.invert {
        depth01 = 1.0 - depth01;
    }
    depth01
}

fn convolution_radius(convolution: Convolution, custom: i32) -> i32 {
    match convolution {
        Convolution::Nearest => 0,
        Convolution::Box3x3
        | Convolution::Gaussian3x3
        | Convolution::Median3x3
        | Convolution::Frontmost3x3
        | Convolution::Backmost3x3 => 1,
        Convolution::Box5x5 | Convolution::Gaussian5x5 => 2,
        Convolution::BoxCustom
        | Convolution::GaussianCustom
        | Convolution::MedianCustom
        | Convolution::FrontmostCustom
        | Convolution::BackmostCustom => custom.max(1),
    }
}

fn kernel_weight(convolution: Convolution, custom: i32, dx: i32, dy: i32) -> f32 {
    match convolution {
        Convolution::Box3x3 | Convolution::Box5x5 | Convolution::BoxCustom => 1.0,
        Convolution::Gaussian3x3 => ((2 - dx.abs()) * (2 - dy.abs())) as f32,
        Convolution::Gaussian5x5 => (pascal5(dx) * pascal5(dy)) as f32,
        Convolution::GaussianCustom => {
            let radius = if custom > 0 { custom } else { 1 } as f32;
            let sigma = (radius / 2.0).max(1.0);
            let distance2 = (dx * dx + dy * dy) as f32;
            (-distance2 / (2.0 * sigma * sigma)).exp()
        }
        _ => 1.0,
    }
}

fn pascal5(d: i32) -> i32 {
    match d.abs() {
        0 => 6,
        1 => 4,
        2 => 1,
        _ => 0,
    }
}

fn uses_weighted(convolution: Convolution) -> bool {
    matches!(
        convolution,
        Convolution::Box3x3
            | Convolution::Box5x5
            | Convolution::Gaussian3x3
            | Convolution::Gaussian5x5
            | Convolution::BoxCustom
            | Convolution::GaussianCustom
    )
}

fn uses_median(convolution: Convolution) -> bool {
    matches!(
        convolution,
        Convolution::Median3x3 | Convolution::MedianCustom
    )
}

fn uses_front(convolution: Convolution) -> bool {
    matches!(
        convolution,
        Convolution::Frontmost3x3 | Convolution::FrontmostCustom
    )
}

fn uses_back(convolution: Convolution) -> bool {
    matches!(
        convolution,
        Convolution::Backmost3x3 | Convolution::BackmostCustom
    )
}

fn normalize_layer_name(name: &str) -> String {
    let stripped = strip_extension(base_name(name));
    let mut compact = String::new();
    let mut previous_separator = false;
    for ch in stripped.to_ascii_lowercase().chars() {
        let separator = ch == ' ' || ch == '_' || ch == '-';
        if separator {
            if !previous_separator && !compact.is_empty() {
                compact.push('-');
            }
            previous_separator = true;
            continue;
        }
        compact.push(ch);
        previous_separator = false;
    }
    if compact.ends_with('-') {
        compact.pop();
    }
    compact
}

fn strip_extension(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => stem,
        _ => name,
    }
}

fn base_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn bool_field(request: &Value, key: &str, fallback: bool) -> Result<bool, String> {
    match request.get(key) {
        None => Ok(fallback),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("{key} must be a boolean")),
    }
}

fn f32_field(request: &Value, key: &str, fallback: f32) -> Result<f32, String> {
    match request.get(key) {
        None => Ok(fallback),
        Some(value) => {
            let number = value
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or_else(|| format!("{key} must be finite"))?;
            let sample = number as f32;
            if sample.is_finite() {
                Ok(sample)
            } else {
                Err(format!("{key} must be finite"))
            }
        }
    }
}

fn i32_field(request: &Value, key: &str, fallback: i32) -> Result<i32, String> {
    match request.get(key) {
        None => Ok(fallback),
        Some(value) => value
            .as_i64()
            .and_then(|number| i32::try_from(number).ok())
            .or_else(|| value.as_u64().and_then(|number| i32::try_from(number).ok()))
            .or_else(|| {
                value.as_f64().and_then(|number| {
                    (number.is_finite() && number == number.trunc()).then_some(number as i32)
                })
            })
            .ok_or_else(|| format!("{key} must be an integer")),
    }
}

fn convolution_field(request: &Value) -> Result<Convolution, String> {
    let name = request
        .get("convolution")
        .and_then(Value::as_str)
        .unwrap_or("Gaussian3x3");
    Ok(match name {
        "Nearest" => Convolution::Nearest,
        "Box3x3" => Convolution::Box3x3,
        "Box5x5" => Convolution::Box5x5,
        "Gaussian3x3" => Convolution::Gaussian3x3,
        "Gaussian5x5" => Convolution::Gaussian5x5,
        "Median3x3" => Convolution::Median3x3,
        "Frontmost3x3" => Convolution::Frontmost3x3,
        "Backmost3x3" => Convolution::Backmost3x3,
        "BoxCustom" => Convolution::BoxCustom,
        "GaussianCustom" => Convolution::GaussianCustom,
        "MedianCustom" => Convolution::MedianCustom,
        "FrontmostCustom" => Convolution::FrontmostCustom,
        "BackmostCustom" => Convolution::BackmostCustom,
        _ => return Err(format!("unsupported convolution: {name}")),
    })
}

fn channel_field(request: &Value) -> Result<Channel, String> {
    let name = request
        .get("channel")
        .and_then(Value::as_str)
        .unwrap_or("AverageRGB");
    Ok(match name {
        "AverageRGB" => Channel::AverageRgb,
        "R" => Channel::R,
        "G" => Channel::G,
        "B" => Channel::B,
        "Luminance" => Channel::Luminance,
        _ => return Err(format!("unsupported channel: {name}")),
    })
}

fn missing_field(request: &Value) -> Result<MissingPolicy, String> {
    let name = request
        .get("missingPolicy")
        .and_then(Value::as_str)
        .unwrap_or("KeepExisting");
    Ok(match name {
        "KeepExisting" => MissingPolicy::KeepExisting,
        "SetZero" => MissingPolicy::SetZero,
        "SetBack" => MissingPolicy::SetBack,
        "SkipGrid" => MissingPolicy::SkipGrid,
        _ => return Err(format!("unsupported missing policy: {name}")),
    })
}

fn numbers(value: Option<&Value>, len: usize, name: &str) -> Result<Vec<f64>, String> {
    let Some(value) = value else {
        return Ok(if len == 2 {
            vec![1.0, 1.0]
        } else {
            vec![0.0; len]
        });
    };
    let values = value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array"))?;
    if values.len() < len {
        return Err(format!("{name} must contain {len} numbers"));
    }
    values
        .iter()
        .take(len)
        .map(|value| {
            value
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or_else(|| format!("{name} must be finite"))
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn encode_minimal_rgb_psd(width: i32, height: i32, layers: &[EncodedLayer]) -> Vec<u8> {
    let mut records = Vec::new();
    let mut pixels = Vec::new();
    for layer in layers {
        let (record, data) = encode_layer(layer);
        records.extend(record);
        pixels.extend(data);
    }
    let mut info = Vec::new();
    push_i16(&mut info, layers.len() as i16);
    info.extend(records);
    info.extend(pixels);
    if info.len() % 2 == 1 {
        info.push(0);
    }
    let mut section = Vec::new();
    push_u32(&mut section, info.len() as u32);
    section.extend(info);
    push_u32(&mut section, 0);
    let mut bytes = Vec::new();
    bytes.extend(b"8BPS");
    push_u16(&mut bytes, 1);
    bytes.extend([0; 6]);
    push_u16(&mut bytes, 3);
    push_u32(&mut bytes, height as u32);
    push_u32(&mut bytes, width as u32);
    push_u16(&mut bytes, 8);
    push_u16(&mut bytes, 3);
    push_u32(&mut bytes, 0);
    push_u32(&mut bytes, 0);
    push_u32(&mut bytes, section.len() as u32);
    bytes.extend(section);
    bytes
}

#[cfg(test)]
pub(crate) struct EncodedLayer {
    pub name: String,
    pub left: i32,
    pub top: i32,
    pub rgba: Vec<u8>,
    pub width: i32,
    pub height: i32,
    pub opacity: u8,
    pub visible: bool,
}

#[cfg(test)]
fn encode_layer(layer: &EncodedLayer) -> (Vec<u8>, Vec<u8>) {
    let mut record = Vec::new();
    push_i32(&mut record, layer.top);
    push_i32(&mut record, layer.left);
    push_i32(&mut record, layer.top + layer.height);
    push_i32(&mut record, layer.left + layer.width);
    push_u16(&mut record, 4);
    let channel_len = (2 + layer.width as u32 * layer.height as u32).to_be_bytes();
    for id in [0i16, 1, 2, -1] {
        record.extend(id.to_be_bytes());
        record.extend(channel_len);
    }
    record.extend(b"8BIM");
    record.extend(b"norm");
    record.push(layer.opacity);
    record.push(0);
    record.push(if layer.visible { 0 } else { 0x02 });
    record.push(0);
    let name = encode_pascal_name(&layer.name);
    let extra_len = 8 + name.len();
    push_u32(&mut record, extra_len as u32);
    push_u32(&mut record, 0);
    push_u32(&mut record, 0);
    record.extend(name);
    let mut data = Vec::new();
    let pixels = (layer.width * layer.height) as usize;
    for channel in 0..4 {
        data.extend(0u16.to_be_bytes());
        for index in 0..pixels {
            data.push(layer.rgba[index * 4 + channel]);
        }
    }
    (record, data)
}

#[cfg(test)]
fn encode_pascal_name(name: &str) -> Vec<u8> {
    encode_pascal_bytes(name.as_bytes())
}

#[cfg(test)]
fn encode_pascal_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![bytes.len() as u8];
    out.extend(bytes);
    while out.len() % 4 != 0 {
        out.push(0);
    }
    out
}

#[cfg(test)]
fn push_u16(buf: &mut Vec<u8>, value: u16) {
    buf.extend(value.to_be_bytes());
}
#[cfg(test)]
fn push_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend(value.to_be_bytes());
}
#[cfg(test)]
fn push_i16(buf: &mut Vec<u8>, value: i16) {
    buf.extend(value.to_be_bytes());
}
#[cfg(test)]
fn push_i32(buf: &mut Vec<u8>, value: i32) {
    buf.extend(value.to_be_bytes());
}

#[cfg(test)]
struct RawPsdRecord {
    name: String,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    channels: Vec<(i16, Vec<u8>)>,
    opacity: u8,
    clipping: u8,
    flags: u8,
    pascal_bytes: Option<Vec<u8>>,
    mask_data: Vec<u8>,
    extra_blocks: Vec<([u8; 4], Vec<u8>)>,
}

#[cfg(test)]
impl RawPsdRecord {
    fn pixel(name: &str, left: i32, top: i32, width: i32, height: i32, rgba: &[u8]) -> Self {
        let pixels = (width * height) as usize;
        assert_eq!(rgba.len(), pixels * 4);
        let mut channels = Vec::new();
        for channel in 0..4 {
            channels.push((
                if channel == 3 { -1 } else { channel as i16 },
                rgba.chunks_exact(4).map(|pixel| pixel[channel]).collect(),
            ));
        }
        Self {
            name: name.into(),
            left,
            top,
            width,
            height,
            channels,
            opacity: 255,
            clipping: 0,
            flags: 0,
            pascal_bytes: None,
            mask_data: Vec::new(),
            extra_blocks: Vec::new(),
        }
    }

    fn group(name: &str, kind: u32, opacity: u8, visible: bool) -> Self {
        let mut record = Self::pixel(name, 0, 0, 0, 0, &[]);
        record.opacity = opacity;
        record.flags = if visible { 0 } else { 0x02 };
        let mut section_type = Vec::new();
        push_u32(&mut section_type, kind);
        record.extra_blocks.push((*b"lsct", section_type));
        record
    }

    fn unicode_name(&mut self, name: &str) {
        let units: Vec<u16> = name.encode_utf16().collect();
        let mut block = Vec::new();
        push_u32(&mut block, units.len() as u32);
        for unit in units {
            push_u16(&mut block, unit);
        }
        self.extra_blocks.push((*b"luni", block));
    }
}

#[cfg(test)]
fn encode_raw_psd(width: i32, height: i32, records: &[RawPsdRecord]) -> Vec<u8> {
    let mut layer_records = Vec::new();
    let mut channel_data = Vec::new();
    for layer in records {
        let mut record = Vec::new();
        push_i32(&mut record, layer.top);
        push_i32(&mut record, layer.left);
        push_i32(&mut record, layer.top + layer.height);
        push_i32(&mut record, layer.left + layer.width);
        push_u16(&mut record, layer.channels.len() as u16);
        for (id, data) in &layer.channels {
            record.extend(id.to_be_bytes());
            push_u32(&mut record, (data.len() + 2) as u32);
            push_u16(&mut channel_data, 0);
            channel_data.extend(data);
        }
        record.extend(b"8BIMnorm");
        record.push(layer.opacity);
        record.push(layer.clipping);
        record.push(layer.flags);
        record.push(0);
        let mut extra = Vec::new();
        push_u32(&mut extra, layer.mask_data.len() as u32);
        extra.extend(&layer.mask_data);
        push_u32(&mut extra, 0);
        extra.extend(
            layer
                .pascal_bytes
                .as_deref()
                .map(encode_pascal_bytes)
                .unwrap_or_else(|| encode_pascal_name(&layer.name)),
        );
        for (key, data) in &layer.extra_blocks {
            extra.extend(b"8BIM");
            extra.extend(key);
            push_u32(&mut extra, data.len() as u32);
            extra.extend(data);
            if data.len() % 2 != 0 {
                extra.push(0);
            }
        }
        push_u32(&mut record, extra.len() as u32);
        record.extend(extra);
        layer_records.extend(record);
    }
    let mut info = Vec::new();
    push_i16(&mut info, records.len() as i16);
    info.extend(layer_records);
    info.extend(channel_data);
    if info.len() % 2 != 0 {
        info.push(0);
    }
    let mut section = Vec::new();
    push_u32(&mut section, info.len() as u32);
    section.extend(info);
    push_u32(&mut section, 0);
    let mut bytes = Vec::new();
    bytes.extend(b"8BPS");
    push_u16(&mut bytes, 1);
    bytes.extend([0; 6]);
    push_u16(&mut bytes, 3);
    push_u32(&mut bytes, height as u32);
    push_u32(&mut bytes, width as u32);
    push_u16(&mut bytes, 8);
    push_u16(&mut bytes, 3);
    push_u32(&mut bytes, 0);
    push_u32(&mut bytes, 0);
    push_u32(&mut bytes, section.len() as u32);
    bytes.extend(section);
    bytes
}

#[cfg(test)]
fn one_layer_mask(
    top: i32,
    left: i32,
    bottom: i32,
    right: i32,
    default_color: u8,
    flags: u8,
    parameters: &[u8],
) -> Vec<u8> {
    let mut data = Vec::new();
    for coordinate in [top, left, bottom, right] {
        push_i32(&mut data, coordinate);
    }
    data.push(default_color);
    data.push(flags);
    data.extend(parameters);
    data
}

#[cfg(test)]
fn two_layer_masks(bounds: (i32, i32, i32, i32), default_color: u8) -> Vec<u8> {
    let mut data = one_layer_mask(
        bounds.0,
        bounds.1,
        bounds.2,
        bounds.3,
        default_color,
        0x08,
        &[],
    );
    data.push(0);
    data.push(default_color);
    for coordinate in [bounds.0, bounds.1, bounds.2, bounds.3] {
        push_i32(&mut data, coordinate);
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;
    use serde_json::json;
    use std::fs;

    /// D's scan: every vertex, the lowest index on equal squared distances.
    fn scan(points: &[[f32; 2]], has: &[bool], best: &[f32], index: usize) -> Option<f32> {
        let mut nearest: Option<(f32, f32)> = None;
        for (other, point) in points.iter().enumerate() {
            if !has[other] || !best[other].is_finite() {
                continue;
            }
            let (dx, dy) = (point[0] - points[index][0], point[1] - points[index][1]);
            let distance = dx * dx + dy * dy;
            if nearest.is_some_and(|(_, shortest)| distance >= shortest) {
                continue;
            }
            nearest = Some((best[other], distance));
        }
        nearest.map(|(depth, _)| depth)
    }

    #[test]
    fn the_nearest_sampled_vertex_wins_ties_by_order_and_skips_non_finite_samples() {
        let points = [[0.0, 0.0], [2.0, 0.0], [1.0, 0.0], [1.0, 1.0]];
        let has = [true, true, false, true];
        // Vertex 2 is one unit from vertices 0, 1 and 3: the first one in order wins.
        assert_eq!(NearestSampled::new(&points, &has, &[0.25, 0.5, 0.0, 0.75]).get(2), Some(0.25));
        // A non-finite sample is not a candidate.
        assert_eq!(NearestSampled::new(&points, &has, &[f32::NAN, 0.5, 0.0, 0.75]).get(2), Some(0.5));
        // Nothing sampled: the caller falls back to the stored depth.
        assert_eq!(NearestSampled::new(&points, &[false; 4], &[0.0; 4]).get(2), None);
    }

    #[test]
    fn the_bucketed_search_returns_what_the_full_scan_returns() {
        // A lattice (many exact ties), a scatter, one line and one coincident cluster, each with a sparse sample set.
        let mut seed = 0x2545_F491u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32
        };
        let lattice: Vec<[f32; 2]> = (0..12).flat_map(|y| (0..9).map(move |x| [x as f32 * 3.0, y as f32 * 3.0])).collect();
        let scatter: Vec<[f32; 2]> = (0..120).map(|_| [next() * 500.0 - 250.0, next() * 40.0]).collect();
        let line: Vec<[f32; 2]> = (0..40).map(|i| [i as f32, 7.0]).collect();
        let cluster: Vec<[f32; 2]> = (0..30).map(|i| [if i % 2 == 0 { 1.0 } else { 1.5 }, 2.0]).collect();
        for points in [lattice, scatter, line, cluster] {
            for stride in [1usize, 3, 7, 29] {
                let has: Vec<bool> = (0..points.len()).map(|i| i % stride == 0).collect();
                let best: Vec<f32> = (0..points.len()).map(|i| i as f32 * 0.5).collect();
                let nearest = NearestSampled::new(&points, &has, &best);
                for index in 0..points.len() {
                    assert_eq!(nearest.get(index), scan(&points, &has, &best, index), "{} points, stride {stride}, vertex {index}", points.len());
                }
            }
        }
    }

    fn gray(value: u8, alpha: u8) -> [u8; 4] {
        [value, value, value, alpha]
    }

    fn raw_layer<'a>(layers: &'a [LayerImage], name: &str) -> &'a LayerImage {
        layers.iter().find(|layer| layer.name == name).unwrap()
    }

    fn grouped_clip_fixture() -> Vec<u8> {
        let mut base_pixels = Vec::new();
        for _ in 0..2 {
            base_pixels.extend(gray(64, 255));
            base_pixels.extend(gray(128, 128));
        }
        let mut base = RawPsdRecord::pixel("Base", 0, 0, 2, 2, &base_pixels);
        base.opacity = 128;
        let mut clip = RawPsdRecord::pixel("Clip", 1, 0, 2, 2, &vec![255; 16]);
        clip.clipping = 1;
        let mut unicode = RawPsdRecord::pixel("fallback", 0, 0, 1, 1, &[255, 255, 255, 255]);
        unicode.pascal_bytes = Some(vec![0xff]);
        unicode.unicode_name("前景");
        let mut hidden_clip =
            RawPsdRecord::pixel("HiddenClip", 0, 0, 1, 1, &[255, 255, 255, 255]);
        hidden_clip.clipping = 1;
        let mut hidden_base =
            RawPsdRecord::pixel("HiddenBase", 0, 0, 1, 1, &[255, 255, 255, 255]);
        hidden_base.opacity = 64;
        hidden_base.flags = 0x02;
        encode_raw_psd(
            3,
            2,
            // Real file order, bottom to top: a group is `divider, children, folder`.
            &[
                RawPsdRecord::group("</Layer set>", 3, 255, true),
                RawPsdRecord::group("</Layer set>", 3, 255, true),
                unicode,
                RawPsdRecord::group("Hidden", 1, 200, false),
                hidden_base,
                hidden_clip,
                RawPsdRecord::group("</Layer set>", 3, 255, true),
                base,
                clip,
                RawPsdRecord::group("Inner", 1, 128, true),
                RawPsdRecord::group("Outer", 2, 128, true),
            ],
        )
    }

    #[test]
    fn raw_psd_groups_unicode_names_visibility_opacity_and_clipping_match_d() {
        let bytes = grouped_clip_fixture();
        let (width, height, layers) = parse_psd(&bytes).unwrap();
        assert_eq!((width, height), (3, 2));
        assert_eq!(layers.len(), 5);
        let base = raw_layer(&layers, "Base");
        assert_eq!(base.path, "/Outer/Inner/Base");
        assert!(base.visible);
        let expected_opacity = (128.0f32 / 255.0) * (128.0f32 / 255.0) * (128.0f32 / 255.0);
        assert!((base.opacity - expected_opacity).abs() < 1e-7);
        let clip = raw_layer(&layers, "Clip");
        assert_eq!(clip.path, "/Outer/Inner/Clip");
        assert!(clip.visible);
        assert_eq!(
            clip.rgba.chunks_exact(4).map(|pixel| pixel[3]).collect::<Vec<_>>(),
            vec![128, 0, 128, 0]
        );
        assert!((clip.opacity - expected_opacity).abs() < 1e-7);
        let hidden_clip = raw_layer(&layers, "HiddenClip");
        let hidden_base = raw_layer(&layers, "HiddenBase");
        assert!(!hidden_base.visible);
        assert!(!hidden_clip.visible);
        assert!((hidden_clip.opacity - hidden_base.opacity).abs() < 1e-7);
        let unicode = raw_layer(&layers, "前景");
        assert_eq!(unicode.path, "/Outer/Hidden/前景");
        assert!(!unicode.visible);
        assert!((unicode.opacity - (128.0f32 / 255.0) * (200.0f32 / 255.0)).abs() < 1e-7);
    }

    #[test]
    fn raw_psd_layer_masks_apply_both_channels_flags_and_feather() {
        let mut both = RawPsdRecord::pixel("Both", 0, 0, 1, 1, &[10, 20, 30, 255]);
        both.mask_data = two_layer_masks((0, 0, 1, 1), 255);
        both.channels.push((-2, vec![128]));
        both.channels.push((-3, vec![128]));

        let mut relative = RawPsdRecord::pixel("Relative", 2, 0, 2, 1, &[64, 64, 64, 255, 128, 128, 128, 255]);
        relative.mask_data = one_layer_mask(0, 1, 1, 2, 0, 0x15, &[0x01, 128]);
        relative.channels.push((-2, vec![128]));

        let mut disabled = RawPsdRecord::pixel("Disabled", 0, 0, 1, 1, &[50, 50, 50, 255]);
        disabled.mask_data = one_layer_mask(0, 0, 1, 1, 0, 0x02, &[]);
        disabled.channels.push((-2, vec![0]));

        let mut feather = RawPsdRecord::pixel("Feather", 0, 0, 5, 5, &vec![255; 100]);
        let mut feather_parameters = vec![0x02];
        feather_parameters.extend(1.0f64.to_be_bytes());
        feather.mask_data = one_layer_mask(0, 0, 5, 5, 0, 0x10, &feather_parameters);
        let mut mask_pixels = vec![0; 25];
        mask_pixels[12] = 255;
        feather.channels.push((-2, mask_pixels));

        let bytes = encode_raw_psd(5, 5, &[both, relative, disabled, feather]);
        let (_, _, layers) = parse_psd(&bytes).unwrap();
        assert_eq!(raw_layer(&layers, "Both").rgba, vec![10, 20, 30, 64]);
        assert_eq!(
            raw_layer(&layers, "Relative")
                .rgba
                .chunks_exact(4)
                .map(|pixel| pixel[3])
                .collect::<Vec<_>>(),
            vec![255, 191]
        );
        assert_eq!(raw_layer(&layers, "Disabled").rgba[3], 255);
        assert_eq!(raw_layer(&layers, "Feather").rgba[12 * 4 + 3], 17);
    }

    #[test]
    fn grouped_masked_clipping_import_commits_once_and_undo_restores_model() {
        let dir = std::env::temp_dir().join(format!("vsoul-psd-groups-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("groups.psd");
        fs::write(&path, grouped_clip_fixture()).unwrap();

        let model = json!({
            "nodes": {
                "uuid": 1, "name": "Root", "type": "Node", "children": [{
                    "uuid": 4, "name": "Depth", "type": "GridDeformer",
                    "grid_axis_x": [-1.5, -0.5], "grid_axis_y": [-1.0, 0.0],
                    "depths": [9.0, 9.0, 9.0, 9.0],
                    "children": [{
                        "uuid": 5, "name": "Base", "type": "Part",
                        "layerPath": "/Outer/Inner/Base", "enabled": true, "opacity": 1.0,
                        "textures": [], "children": [],
                        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                    }, {
                        "uuid": 6, "name": "Clip", "type": "Part",
                        "layerPath": "/Outer/Inner/Clip", "enabled": true, "opacity": 1.0,
                        "textures": [], "children": [],
                        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                    }],
                }],
            },
            "param": [], "automation": [],
        });
        let mut session = Session::new();
        assert_eq!(
            session.handle_request(&json!({"op":"new", "model":model}))["succeeded"],
            true
        );
        let before = session.snapshot();
        let imported = session.handle_request(&json!({
            "op": "depth.import_psd",
            "path": path.to_string_lossy(),
            "convolution": "Nearest",
        }));
        assert_eq!(imported["succeeded"], true, "{imported}");
        assert_eq!(imported["result"]["changedGrids"], 1);
        let node = session.handle_request(&json!({"op":"node.get", "id":4}));
        let depths = &node["result"]["depths"];
        assert!((depths[0].as_f64().unwrap() - f64::from(-0.498_039_22f32)).abs() < 1e-7);
        assert_eq!(depths[1], json!(1.0));
        assert!((depths[2].as_f64().unwrap() - f64::from(-0.498_039_22f32)).abs() < 1e-7);
        assert_eq!(depths[3], json!(1.0));
        assert_eq!(
            session.handle_request(&json!({"op":"undo"}))["succeeded"],
            true
        );
        assert_eq!(session.snapshot()["model"], before["model"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn nearest_channels_and_frontmost_match_d_float() {
        let mut pixels = Vec::new();
        for index in 0..9 {
            pixels.extend(if index == 4 {
                gray(0, 255)
            } else {
                gray(255, 255)
            });
        }
        let layer = LayerImage {
            path: "/L".into(),
            name: "L".into(),
            left: 0,
            top: 0,
            width: 3,
            height: 3,
            opacity: 1.0,
            visible: true,
            rgba: pixels,
            document_width: 3,
            document_height: 3,
            coverage: Vec::new(),
            clipped: false,
            retained_bytes: 0,
        };
        let mut settings = Settings {
            invert: false,
            back_depth: -1.0,
            front_depth: 1.0,
            depth_scale: 1.0,
            alpha_threshold: 0.01,
            custom_radius: 3,
            convolution: Convolution::Nearest,
            channel: Channel::AverageRgb,
            missing: MissingPolicy::KeepExisting,
            match_direct_grid_name: true,
        };
        assert!(sample_layer(&layer, 1.0, 1.0, &settings).is_none());
        assert_eq!(sample_layer(&layer, 0.0, 0.0, &settings), Some(1.0));
        settings.invert = true;
        assert_eq!(sample_layer(&layer, 1.0, 1.0, &settings), Some(1.0));
        assert!(sample_layer(&layer, 0.0, 0.0, &settings).is_none());
        settings.invert = false;
        settings.channel = Channel::R;
        let red = LayerImage {
            rgba: vec![255, 0, 0, 255],
            width: 1,
            height: 1,
            ..layer.clone()
        };
        assert_eq!(sample_layer(&red, 0.0, 0.0, &settings), Some(1.0));
        settings.channel = Channel::AverageRgb;
        let average = sample_layer(&red, 0.0, 0.0, &settings).unwrap();
        let depth01 = 255.0f32 / (255.0 * 3.0);
        let expected = -1.0 + 2.0 * depth01;
        assert_eq!(average, expected);
    }

    #[test]
    fn psd_round_trip_and_import_are_one_undo() {
        let mut front = vec![0u8; 2 * 2 * 4];
        front[..4].copy_from_slice(&[255, 255, 255, 255]);
        let back = vec![128u8; 2 * 2 * 4];
        let bytes = encode_minimal_rgb_psd(
            2,
            2,
            &[
                EncodedLayer {
                    name: "Card".into(),
                    left: 0,
                    top: 0,
                    rgba: back,
                    width: 2,
                    height: 2,
                    opacity: 255,
                    visible: true,
                },
                EncodedLayer {
                    name: "Card".into(),
                    left: 0,
                    top: 0,
                    rgba: front,
                    width: 2,
                    height: 2,
                    opacity: 255,
                    visible: true,
                },
                EncodedLayer {
                    name: "Nope".into(),
                    left: 0,
                    top: 0,
                    rgba: vec![255u8; 16],
                    width: 2,
                    height: 2,
                    opacity: 255,
                    visible: true,
                },
            ],
        );
        let (width, height, layers) = parse_psd(&bytes).unwrap();
        assert_eq!((width, height), (2, 2));
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0].name, "Card");
        assert_eq!(layers[0].path, "/Card [#2]");
        assert_eq!(layers[1].path, "/Card");
        assert_eq!(layers[0].rgba[0], 128);
        assert_eq!(normalize_layer_name("/Group/Face.PNG"), "face");
        assert_eq!(normalize_layer_name("Face "), "face");
        assert_eq!(normalize_layer_name(".hidden"), ".hidden");
        assert_eq!(normalize_layer_name("a_b-"), "a-b");

        let dir = std::env::temp_dir().join(format!("vsoul-psd-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("card.psd");
        fs::write(&path, &bytes).unwrap();
        let mut session = Session::new();
        let model = json!({
            "nodes": {
                "uuid": 1, "name": "Root", "type": "Node", "children": [{
                    "uuid": 4, "name": "Card", "type": "GridDeformer",
                    "grid_axis_x": [-1.0, 0.0], "grid_axis_y": [-1.0, 0.0],
                    "depths": [9.0, 9.0, 9.0, 9.0],
                    "depth-ops": [{"type": "attached-point", "index": 0, "amount": 0.2}],
                    "depth-op-base-depths": [1.0, 1.0, 1.0, 1.0],
                }, {
                    "uuid": 5, "name": "Hair", "type": "PathDeformer", "vertices": [0.0, 0.0, 1.0, 0.0],
                }],
            },
            "param": [], "automation": [],
        });
        session.handle_request(&json!({"op":"new", "model": model}));
        let before = session.snapshot();
        let imported = session.handle_request(&json!({
            "op": "depth.import_psd",
            "path": path.to_string_lossy(),
            "convolution": "Nearest",
        }));
        assert_eq!(imported["succeeded"], true, "{imported}");
        assert_eq!(imported["result"]["unmatchedLayers"], 1);
        assert_eq!(imported["result"]["changedGrids"], 1);
        let node = session.handle_request(&json!({"op":"node.get", "id": 4}));
        let depths = &node["result"]["depths"];
        let gray = -1.0f32 + 2.0 * (128.0f32 / 255.0);
        assert_eq!(depths[0], json!(1.0));
        assert_eq!(depths[1], json!(f64::from(gray)));
        assert_eq!(depths[2], json!(f64::from(gray)));
        assert_eq!(depths[3], json!(f64::from(gray)));
        assert!(node["result"].get("depth-ops").is_none());
        assert!(node["result"].get("depth-op-base-depths").is_none());
        assert!(
            session.handle_request(&json!({"op":"node.get", "id": 5}))["result"]
                .get("depths")
                .is_none()
        );
        session.handle_request(&json!({"op":"undo"}));
        assert_eq!(session.snapshot()["model"], before["model"]);

        let blocked = session.snapshot();
        let missing = session.handle_request(&json!({"op":"depth.import_psd"}));
        assert_eq!(missing["succeeded"], false, "{missing}");
        assert_eq!(missing["message"], "Path not provided");
        assert_eq!(session.snapshot(), blocked);
        let bad = session.handle_request(&json!({
            "op": "depth.import_psd",
            "path": path.to_string_lossy(),
            "useGpuComposition": true,
        }));
        assert_eq!(bad["succeeded"], false, "{bad}");
        assert_eq!(session.snapshot(), blocked);
        let hair = encode_minimal_rgb_psd(
            2,
            2,
            &[EncodedLayer {
                name: "Hair".into(),
                left: 0,
                top: 0,
                rgba: vec![255u8; 16],
                width: 2,
                height: 2,
                opacity: 255,
                visible: true,
            }],
        );
        let hair_path = dir.join("hair.psd");
        fs::write(&hair_path, hair).unwrap();
        let path_target = session.handle_request(&json!({
            "op": "depth.import_psd",
            "path": hair_path.to_string_lossy(),
            "convolution": "Nearest",
        }));
        assert_eq!(path_target["succeeded"], false, "{path_target}");
        assert_eq!(
            path_target["message"],
            "PSD depth import does not assign depths to PathDeformer"
        );
        assert_eq!(session.snapshot(), blocked);
        fs::write(&path, b"not a psd").unwrap();
        let malformed = session.handle_request(&json!({
            "op": "depth.import_psd",
            "path": path.to_string_lossy(),
        }));
        assert_eq!(malformed["succeeded"], false, "{malformed}");
        assert_eq!(session.snapshot(), blocked);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn import_samples_the_evaluated_transform() {
        let dir = std::env::temp_dir().join(format!("vsoul-psd-pose-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut rgba = vec![0u8; 16];
        rgba[..4].copy_from_slice(&[255, 255, 255, 255]);
        rgba[4..8].copy_from_slice(&[128, 128, 128, 255]);
        let path = dir.join("shift.psd");
        fs::write(
            &path,
            encode_minimal_rgb_psd(2, 2, &[EncodedLayer {
                name: "Card".into(),
                left: 0,
                top: 0,
                rgba,
                width: 2,
                height: 2,
                opacity: 255,
                visible: true,
            }]),
        )
        .unwrap();
        let mut session = Session::new();
        session.handle_request(&json!({
            "op": "new",
            "model": {
                "nodes": {
                    "uuid": 1, "name": "Root", "type": "Node",
                    "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                    "children": [{
                        "uuid": 4, "name": "Card", "type": "GridDeformer",
                        "grid_axis_x": [-1.0, 0.0], "grid_axis_y": [-1.0, 0.0],
                        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                    }],
                },
                "param": [{
                    "uuid": 8, "is_vec2": false, "min": [0.0, 0.0], "max": [1.0, 0.0],
                    "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0]], "bindings": [],
                }],
                "automation": [],
            }
        }));
        let bound = session.handle_request(&json!({
            "op": "binding.set_trs", "parameter": 8, "nodes": [4], "key": [1, 0],
            "translation": [1.0, 0.0],
        }));
        assert_eq!(bound["succeeded"], true, "{bound}");
        session.handle_request(&json!({"op": "value.set", "id": 8, "value": [1.0, 0.0]}));
        let imported = session.handle_request(&json!({
            "op": "depth.import_psd",
            "path": path.to_string_lossy(),
            "convolution": "Nearest",
        }));
        assert_eq!(imported["succeeded"], true, "{imported}");
        let node = session.handle_request(&json!({"op":"node.get", "id": 4}));
        let gray = -1.0f32 + 2.0 * (128.0f32 / 255.0);
        assert_eq!(node["result"]["depths"][0], json!(f64::from(gray)));
        assert_eq!(node["result"]["transform"]["trans"][0], json!(0.0));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn part_coverage_rejects_depths_outside_the_artwork() {
        let rgba = vec![255u8; 2 * 2 * 4];
        let bytes = encode_minimal_rgb_psd(2, 2, &[EncodedLayer {
            name: "Card".into(),
            left: 0,
            top: 0,
            rgba,
            width: 2,
            height: 2,
            opacity: 255,
            visible: true,
        }]);
        let dir = std::env::temp_dir().join(format!("vsoul-psd-cover-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cover.psd");
        fs::write(&path, bytes).unwrap();
        let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 0]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let textures = vec![TextureBlob { tag: 0, data: png }];
        let model = json!({
            "nodes": {
                "uuid": 1, "name": "Root", "type": "Node", "children": [{
                    "uuid": 4, "name": "Card", "type": "GridDeformer",
                    "grid_axis_x": [-1.0, 0.0], "grid_axis_y": [-1.0, 0.0],
                    "children": [{
                        "uuid": 6, "name": "Art", "type": "Part", "enabled": true, "opacity": 1.0,
                        "textures": [0],
                        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                    }],
                }],
            },
            "param": [], "automation": [],
        });
        let imported = import_psd(
            &model,
            &model,
            &textures,
            &json!({"op":"depth.import_psd", "path": path.to_string_lossy(), "convolution": "Nearest"}),
        )
        .unwrap();
        assert_eq!(imported.report["changedGrids"], 1);
        assert_eq!(imported.changed_grids, vec![4]);
        assert_eq!(imported.model["nodes"]["children"][0]["depths"], json!([0.0, 0.0, 0.0, 0.0]));
        fs::remove_dir_all(dir).unwrap();
    }

    fn call(session: &mut Session, request: &Value) -> Value {
        session.handle_request(request)
    }

    fn ok(response: &Value) -> Value {
        assert_eq!(response["succeeded"], true, "{response}");
        response["result"].clone()
    }

    fn deform<'a>(parameter: &'a Value, node: u64) -> &'a Value {
        parameter["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == "deform" && binding["node"] == json!(node))
            .unwrap()
    }

    fn write_card_psd(dir: &std::path::Path) -> std::path::PathBuf {
        let mut front = vec![0u8; 16];
        front[..4].copy_from_slice(&[255, 255, 255, 255]);
        let path = dir.join("card.psd");
        fs::write(
            &path,
            encode_minimal_rgb_psd(
                2,
                2,
                &[EncodedLayer {
                    name: "Card".into(),
                    left: 0,
                    top: 0,
                    rgba: front,
                    width: 2,
                    height: 2,
                    opacity: 255,
                    visible: true,
                }],
            ),
        )
        .unwrap();
        path
    }

    #[test]
    fn import_with_a_driven_rig_refreshes_deform_in_one_undo() {
        let dir = std::env::temp_dir().join(format!("vsoul-psd-rig-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = write_card_psd(&dir);
        let mut session = Session::new();
        let rig_id = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ))["created"][0]["uuid"]
            .as_u64()
            .unwrap();
        let bone_id = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Bone","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ))["created"][0]["uuid"]
            .as_u64()
            .unwrap();
        let card_id = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Card"}),
        ))["created"][0]["uuid"]
            .as_u64()
            .unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[card_id],"axisX":[-1.0,0.0],"axisY":[-1.0,0.0]}),
        ));
        let hair_id = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"PathDeformer","name":"Hair","parents":[card_id]}),
        ))["created"][0]["uuid"]
            .as_u64()
            .unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_vertices","nodes":[hair_id],"vertices":[0.0,0.0,1.0,0.0]}),
        ));
        let other_id = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Other"}),
        ))["created"][0]["uuid"]
            .as_u64()
            .unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[other_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"depth.set","target":other_id,"depths":[9.0,9.0,9.0,9.0]}),
        ));
        for target in [card_id, hair_id, other_id] {
            ok(&call(
                &mut session,
                &json!({"op":"rig.add_source","root":rig_id,"target":target,"bone":bone_id}),
            ));
        }
        let parameter_id = ok(&call(
            &mut session,
            &json!({"op":"parameter.create","name":"Yaw","dimensions":1,"min":[-1.0,0.0],"max":[1.0,1.0]}),
        ))["uuid"]
            .as_u64()
            .unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[2,0],"rotation_y":1.5707963267948966
            }),
        ));
        ok(&call(
            &mut session,
            &json!({
                "op":"depth.ops.add","target":card_id,"index":-1,
                "operation":{"type":"attached-point","index":0,"amount":0.2}
            }),
        ));
        let before = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let imported = call(
            &mut session,
            &json!({"op":"depth.import_psd","path":path.to_string_lossy(),"convolution":"Nearest"}),
        );
        assert_eq!(imported["succeeded"], true, "{imported}");
        assert_eq!(imported["result"]["changedGrids"], 1);
        let card = ok(&call(&mut session, &json!({"op":"node.get","id":card_id})));
        assert_eq!(card["depths"][0], json!(1.0));
        assert!(card.get("depth-ops").is_none());
        assert!(card.get("depth-op-base-depths").is_none());
        let hair = ok(&call(&mut session, &json!({"op":"node.get","id":hair_id})));
        assert!(hair.get("depths").is_none());
        let parameter = ok(&call(&mut session, &json!({"op":"parameter.get","id":parameter_id})));
        for node in [card_id, hair_id] {
            let binding = deform(&parameter, node);
            assert_eq!(binding["isSet"][0][0], true, "{node}");
            assert_eq!(binding["isSet"][1][0], true, "{node}");
            assert_eq!(binding["isSet"][2][0], true, "{node}");
        }
        let sibling = deform(&parameter, other_id);
        assert_eq!(sibling["isSet"][0][0], false);
        assert_eq!(sibling["isSet"][2][0], true);
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"node.get","id":other_id})))["depths"],
            json!([9.0, 9.0, 9.0, 9.0])
        );
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"snapshot"})))["model"],
            before["model"]
        );

        let mut plain = Session::new();
        let plain_rig = ok(&call(
            &mut plain,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Plain"}),
        ))["created"][0]["uuid"]
            .as_u64()
            .unwrap();
        let plain_bone = ok(&call(
            &mut plain,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Bone","parents":[plain_rig],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ))["created"][0]["uuid"]
            .as_u64()
            .unwrap();
        let plain_card = ok(&call(
            &mut plain,
            &json!({"op":"node.create","className":"GridDeformer","name":"Card"}),
        ))["created"][0]["uuid"]
            .as_u64()
            .unwrap();
        ok(&call(
            &mut plain,
            &json!({"op":"deformer.define_grid","nodes":[plain_card],"axisX":[-1.0,0.0],"axisY":[-1.0,0.0]}),
        ));
        ok(&call(
            &mut plain,
            &json!({"op":"rig.add_source","root":plain_rig,"target":plain_card,"bone":plain_bone}),
        ));
        let idle = ok(&call(
            &mut plain,
            &json!({"op":"parameter.create","name":"Idle","dimensions":1,"min":[-1.0,0.0],"max":[1.0,1.0]}),
        ));
        let idle_id = idle["uuid"].as_u64().unwrap();
        ok(&call(
            &mut plain,
            &json!({
                "op":"depth.ops.add","target":plain_card,"index":-1,
                "operation":{"type":"attached-point","index":0,"amount":0.2}
            }),
        ));
        let plain_import = call(
            &mut plain,
            &json!({"op":"depth.import_psd","path":path.to_string_lossy(),"convolution":"Nearest"}),
        );
        assert_eq!(plain_import["succeeded"], true, "{plain_import}");
        let idle_after = ok(&call(&mut plain, &json!({"op":"parameter.get","id":idle_id})));
        assert!(idle_after["bindings"].as_array().unwrap().is_empty());
        assert!(ok(&call(&mut plain, &json!({"op":"node.get","id":plain_card}))).get("depth-ops").is_none());

        let wide: Vec<f64> = (0..1000).map(|index| index as f64).collect();
        let tall: Vec<f64> = (0..1001).map(|index| index as f64).collect();
        let heavy = json!({
            "nodes": {
                "uuid": 1, "type": "Node",
                "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                "children": [{
                    "uuid": 2, "type": "DepthRigRoot",
                    "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                    "bindings": [{"target": 4, "targetKind": "grid", "sourceBoneUuids": [3]}],
                    "children": [{
                        "uuid": 3, "type": "DepthBone", "children": [],
                        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]}
                    }]
                }, {
                    "uuid": 4, "name": "Card", "type": "GridDeformer",
                    "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                    "grid_axis_x": wide, "grid_axis_y": tall
                }]
            },
            "param": [{
                "uuid": 9, "is_vec2": false, "min": [0.0, 0.0], "max": [1.0, 0.0],
                "axis_points": [[0.0, 1.0], [0.0]],
                "bindings": [{"node": 3, "param_name": "transform.r.y", "values": [[0.0], [0.2]], "isSet": [[true], [true]]}]
            }],
            "automation": []
        });
        let mut blocked = Session::new();
        ok(&call(&mut blocked, &json!({"op":"new","model":heavy})));
        let held = ok(&call(&mut blocked, &json!({"op":"snapshot"})));
        let failed = call(
            &mut blocked,
            &json!({"op":"depth.import_psd","path":path.to_string_lossy(),"convolution":"Nearest"}),
        );
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(
            failed["message"].as_str().unwrap().contains("exceeds the maximum of 1000000"),
            "{failed}"
        );
        assert_eq!(ok(&call(&mut blocked, &json!({"op":"snapshot"}))), held);
        fs::remove_dir_all(dir).unwrap();
    }

    fn grid(session: &mut Session, name: &str) -> u64 {
        let id = ok(&call(
            session,
            &json!({"op":"node.create","className":"GridDeformer","name":name}),
        ))["created"][0]["uuid"]
            .as_u64()
            .unwrap();
        ok(&call(
            session,
            &json!({"op":"deformer.define_grid","nodes":[id],"axisX":[-1.0,0.0],"axisY":[-1.0,0.0]}),
        ));
        id
    }

    #[test]
    fn import_resets_source_depth_and_keeps_sibling_scopes() {
        let dir = std::env::temp_dir().join(format!("vsoul-psd-scope-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pair.psd");
        let white = vec![255u8; 16];
        fs::write(
            &path,
            encode_minimal_rgb_psd(
                2,
                2,
                &[
                    EncodedLayer {
                        name: "Card".into(),
                        left: 0,
                        top: 0,
                        rgba: white.clone(),
                        width: 2,
                        height: 2,
                        opacity: 255,
                        visible: true,
                    },
                    EncodedLayer {
                        name: "Other".into(),
                        left: 0,
                        top: 0,
                        rgba: white,
                        width: 2,
                        height: 2,
                        opacity: 255,
                        visible: true,
                    },
                ],
            ),
        )
        .unwrap();
        let mut session = Session::new();
        let rig_id = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ))["created"][0]["uuid"]
            .as_u64()
            .unwrap();
        let bone_id = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Bone","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ))["created"][0]["uuid"]
            .as_u64()
            .unwrap();
        let card_id = grid(&mut session, "Card");
        let other_id = grid(&mut session, "Other");
        let quiet_id = grid(&mut session, "Quiet");
        for target in [card_id, other_id, quiet_id] {
            ok(&call(
                &mut session,
                &json!({"op":"rig.add_source","root":rig_id,"target":target,"bone":bone_id}),
            ));
        }
        ok(&call(
            &mut session,
            &json!({
                "op":"rig.set_source_settings","root":rig_id,"target":card_id,"bone":bone_id,
                "settings":{"depthScale":2.0,"depthOffset":0.25}
            }),
        ));
        ok(&call(
            &mut session,
            &json!({
                "op":"rig.set_source_settings","root":rig_id,"target":quiet_id,"bone":bone_id,
                "settings":{"depthOffset":0.5}
            }),
        ));
        let parameter_id = ok(&call(
            &mut session,
            &json!({"op":"parameter.create","name":"Yaw","dimensions":1,"min":[-1.0,0.0],"max":[1.0,1.0]}),
        ))["uuid"]
            .as_u64()
            .unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[2,0],"rotation_y":1.5707963267948966
            }),
        ));
        let before = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"].clone();
        let imported = call(
            &mut session,
            &json!({"op":"depth.import_psd","path":path.to_string_lossy(),"convolution":"Nearest"}),
        );
        assert_eq!(imported["succeeded"], true, "{imported}");
        assert_eq!(imported["result"]["changedGrids"], 2);
        let card_sources = ok(&call(
            &mut session,
            &json!({"op":"rig.list_sources","root":rig_id,"target":card_id}),
        ));
        assert_eq!(card_sources["sources"][0]["depthScale"], json!(1.0));
        assert_eq!(card_sources["sources"][0]["depthOffset"], json!(0.0));
        let quiet_sources = ok(&call(
            &mut session,
            &json!({"op":"rig.list_sources","root":rig_id,"target":quiet_id}),
        ));
        assert_eq!(quiet_sources["sources"][0]["depthOffset"], json!(0.5));
        let parameter = ok(&call(&mut session, &json!({"op":"parameter.get","id":parameter_id})));
        for node in [card_id, other_id] {
            let binding = deform(&parameter, node);
            assert_eq!(binding["isSet"][0][0], true, "{node}");
        }
        assert_eq!(deform(&parameter, quiet_id)["isSet"][0][0], false);
        let reset_offsets = deform(&parameter, card_id)["values"].clone();
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"})))["model"], before);

        let mut kept = Session::new();
        ok(&call(&mut kept, &json!({"op":"new","model":before})));
        ok(&call(
            &mut kept,
            &json!({"op":"depth.set","target":card_id,"depths":[1.0,1.0,1.0,1.0]}),
        ));
        let kept_parameter = ok(&call(&mut kept, &json!({"op":"parameter.get","id":parameter_id})));
        let kept_sources = ok(&call(
            &mut kept,
            &json!({"op":"rig.list_sources","root":rig_id,"target":card_id}),
        ));
        assert_eq!(kept_sources["sources"][0]["depthOffset"], json!(0.25));
        assert_ne!(deform(&kept_parameter, card_id)["values"], reset_offsets);
        fs::remove_dir_all(dir).unwrap();
    }
}
