//! DepthDraw session operations (`depth.draw.*`): the layer-composition half of D's depth viewport.
//!
//! The state is a [`dd::Session`] held by the editing `Session`, outside the undo history (D keeps it on the
//! viewport; only `ngApplyDepthDrawTargetResultWithSummary` pushes an action). `depth.draw.apply` is the one
//! operation that edits the document and it is ONE undo step. The manifest (layers, bindings, display, cleanup
//! recipes; never pixels) is written to the model key D uses, `depthDrawSessionManifest`, when the document is
//! saved, so save and reopen keep it. Pixels are not persisted: after a reopen `depth.draw.load_png` with the
//! same layer id attaches the pixels again and replays the recorded cleanup.

use crate::depth_draw as dd;
use crate::depth_ops;
use crate::depths::finite_depth_or_zero;
use crate::nodes::find_node;
use crate::{read_bounded_file, valid_id, write_atomic, Session};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const MANIFEST_KEY: &str = "depthDrawSessionManifest";
/// Largest PNG file `depth.draw.load_png` reads. D bounds the decoded pixels (256 MiB retained at 8 bytes per
/// pixel, enforced by `dd::load_png_layer`); this bounds the compressed bytes read before that check.
const MAX_PNG_BYTES: u64 = 256 * 1024 * 1024;
/// Largest manifest file `depth.draw.manifest.set` reads from `path`.
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const PNG_BYTES_PER_PIXEL: u64 = 8;

fn depth_manifest_target(alias: &Path, directory: &Path) -> Result<PathBuf, String> {
    let name = alias.file_name().ok_or("manifest must name a file")?;
    let parent = crate::export_ops::resolve_collection_path(alias.parent().unwrap_or(Path::new(".")))?;
    if parent.exists() {
        if !parent.is_dir() { return Err("manifest parent must be a directory".into()); }
    } else if !directory.starts_with(&parent) {
        return Err("manifest parent directory does not exist".into());
    }
    let target = parent.join(name);
    if target.exists() {
        fs::canonicalize(target).map_err(|error| format!("could not resolve manifest: {error}"))
    } else { Ok(target) }
}

fn depth_source_target(path: &Path, links: usize) -> Result<PathBuf, String> {
    use std::path::Component;
    if links > 40 { return Err("too many DepthDraw source symlinks".into()); }
    let absolute = if path.is_absolute() { path.to_owned() } else {
        std::env::current_dir().map_err(|error| error.to_string())?.join(path)
    };
    let mut resolved = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::Prefix(_) | Component::RootDir => resolved.push(part.as_os_str()),
            Component::CurDir => {},
            Component::ParentDir => { resolved.pop(); },
            Component::Normal(name) => {
                let next = resolved.join(name);
                resolved = match fs::canonicalize(&next) {
                    Ok(path) => path,
                    Err(_) => match fs::read_link(&next) {
                        Ok(link) => depth_source_target(&resolved.join(link), links + 1)?,
                        Err(_) => next,
                    },
                };
            },
        }
    }
    Ok(resolved)
}

fn depth_output_key(path: &Path) -> String {
    // Keep output names portable to case-insensitive filesystems, as collection does.
    fs::canonicalize(path).unwrap_or_else(|_| path.to_owned()).to_string_lossy().to_lowercase()
}

/// What an editing `Session` keeps for DepthDraw.
#[derive(Default)]
pub(crate) struct State {
    pub(crate) draw: dd::Session,
    /// The opened document carried a manifest this build cannot read; save leaves its text alone.
    unreadable: bool,
    /// The session changed since the last save (the document reads as modified).
    pub(crate) unsaved: bool,
    /// Counts every change, so a view can tell when to read the manifest again.
    pub(crate) revision: u64,
}

impl State {
    pub(crate) fn from_model(model: &Value) -> Self {
        let mut state = State::default();
        match model.get(MANIFEST_KEY) {
            None | Some(Value::Null) => {}
            Some(Value::String(text)) => match dd::session_from_persistent_json(text) {
                Ok(Some(draw)) => state.draw = draw,
                Ok(None) => {}
                Err(_) => state.unreadable = true,
            },
            Some(_) => state.unreadable = true,
        }
        state
    }

    fn has_content(&self) -> bool {
        !self.draw.layers.is_empty() || !self.draw.bindings.is_empty()
    }

    fn touch(&mut self) {
        self.unsaved = true;
        self.revision = self.revision.wrapping_add(1);
    }

    pub(crate) fn summary(&self) -> Value {
        json!({
            "revision": self.revision,
            "layers": self.draw.layers.len(),
            "bindings": self.draw.bindings.len(),
        })
    }
}

// ------------------------------------------------------------------------------------ request fields

fn opt_str<'a>(request: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match request.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn req_str<'a>(request: &'a Value, key: &str) -> Result<&'a str, String> {
    opt_str(request, key)?.ok_or_else(|| format!("{key} is required"))
}

fn opt_bool(request: &Value, key: &str) -> Result<Option<bool>, String> {
    match request.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(flag)) => Ok(Some(*flag)),
        Some(_) => Err(format!("{key} must be a boolean")),
    }
}

fn opt_f32(request: &Value, key: &str) -> Result<Option<f32>, String> {
    match request.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => match value.as_f64() {
            Some(number) if (number as f32).is_finite() => Ok(Some(number as f32)),
            _ => Err(format!("{key} must be a finite number")),
        },
    }
}

fn opt_i32(request: &Value, key: &str) -> Result<Option<i32>, String> {
    match request.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .and_then(|number| i32::try_from(number).ok())
            .map(Some)
            .ok_or_else(|| format!("{key} must be an integer")),
    }
}

fn opt_pair(request: &Value, key: &str) -> Result<Option<[f32; 2]>, String> {
    match request.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) if items.len() == 2 => {
            let number = |i: usize| match items[i].as_f64() {
                Some(value) if (value as f32).is_finite() => Ok(value as f32),
                _ => Err(format!("{key} must be [x, y] with finite numbers")),
            };
            Ok(Some([number(0)?, number(1)?]))
        }
        Some(_) => Err(format!("{key} must be [x, y]")),
    }
}

fn node_id(value: &Value, name: &str) -> Result<u32, String> {
    let raw = value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
        .ok_or_else(|| format!("{name} must be a node UUID"))?;
    valid_id(raw).ok_or_else(|| format!("{name} must be a nonzero u32 node UUID"))
}

fn num(value: f32) -> Value {
    json!(f64::from(value))
}

fn nums(values: &[f32]) -> Value {
    Value::Array(values.iter().map(|v| num(*v)).collect())
}

// ------------------------------------------------------------------------------------------- targets

/// A GridDeformer or PathDeformer as the composer sees it: vertices in the node's own space and the stored depths.
struct TargetInfo {
    points: Vec<[f32; 2]>,
    stored: Option<Vec<f32>>,
    is_grid: bool,
    has_ops: bool,
}

fn target_info(model: &Value, id: u32) -> Result<TargetInfo, String> {
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    let (points, is_grid) = match node.get("type").and_then(Value::as_str) {
        Some("GridDeformer") => (depth_ops::grid_points(node)?, true),
        Some("PathDeformer") => {
            let count = crate::depth_resample::path_vertex_count(node.get("vertices"))?;
            let flat = node["vertices"].as_array().ok_or("PathDeformer vertices must be an array")?;
            let at = |i: usize| flat[i].as_f64().unwrap_or(0.0) as f32;
            ((0..count).map(|i| [at(2 * i), at(2 * i + 1)]).collect(), false)
        }
        _ => return Err("target must be a GridDeformer or PathDeformer".into()),
    };
    let stored = match node.get("depths") {
        None | Some(Value::Null) => None,
        Some(Value::Array(values)) => Some(
            values
                .iter()
                .map(|value| value.as_f64().map(|n| finite_depth_or_zero(n) as f32).ok_or("depths must be an array of numbers"))
                .collect::<Result<Vec<f32>, _>>()?,
        ),
        Some(_) => return Err("depths must be an array of numbers".into()),
    };
    let has_ops = node.get("depth-ops").and_then(Value::as_array).is_some_and(|ops| !ops.is_empty());
    Ok(TargetInfo { points, stored, is_grid, has_ops })
}

fn is_depth_target(model: &Value, uuid: u64) -> bool {
    valid_id(uuid)
        .and_then(|id| find_node(model, id))
        .and_then(|node| node.get("type").and_then(Value::as_str))
        .is_some_and(|kind| matches!(kind, "GridDeformer" | "PathDeformer"))
}

fn node_name(model: &Value, uuid: u64) -> String {
    valid_id(uuid)
        .and_then(|id| find_node(model, id))
        .and_then(|node| node.get("name").and_then(Value::as_str))
        .unwrap_or("")
        .to_owned()
}

/// Every Part, and every depth-mapped deformer, as the auto-binder matches them (D `matchCandidates`).
fn bind_nodes(model: &Value) -> Vec<dd::BindNode> {
    fn walk(node: &Value, target: Option<(u64, String)>, out: &mut Vec<dd::BindNode>) {
        let kind = node.get("type").and_then(Value::as_str).unwrap_or("Node");
        let uuid = node.get("uuid").and_then(Value::as_u64).unwrap_or(0);
        let name = node.get("name").and_then(Value::as_str).unwrap_or("").to_owned();
        let direct = matches!(kind, "GridDeformer" | "PathDeformer");
        let here = if direct { Some((uuid, name.clone())) } else { target };
        if kind == "Part" || direct {
            out.push(dd::BindNode {
                uuid,
                name,
                layer_path: node.get("layerPath").and_then(Value::as_str).unwrap_or("").to_owned(),
                is_part: kind == "Part",
                direct_target: direct,
                target: here.clone(),
            });
        }
        for child in node.get("children").and_then(Value::as_array).into_iter().flatten() {
            walk(child, here.clone(), out);
        }
    }
    let mut out = Vec::new();
    if let Some(root) = model.get("nodes") {
        walk(root, None, &mut out);
    }
    out
}

// ---------------------------------------------------------------------------------------------- json

fn layer_pixels_json(draw: &dd::Session) -> Value {
    Value::Array(
        draw.layers
            .iter()
            .map(|l| {
                json!({"id": l.id, "hasDepthPixels": l.has_depth_pixels(), "hasNormalCoverage": l.has_normal_coverage()})
            })
            .collect(),
    )
}

fn validation_json(v: &dd::ManifestValidation) -> Value {
    let keys: Vec<Value> = v
        .duplicate_binding_keys
        .iter()
        .map(|key| {
            let (layer, grid) = key.split_once('\0').unwrap_or((key, ""));
            json!({"layerId": layer, "targetGridUuid": grid})
        })
        .collect();
    json!({
        "ok": v.ok(),
        "missingSourceLayerIds": v.missing_source_layer_ids,
        "missingTargetLayerIds": v.missing_target_layer_ids,
        "missingBindingLayerIds": v.missing_binding_layer_ids,
        "duplicateLayerIds": v.duplicate_layer_ids,
        "duplicateBindingKeys": keys,
        "missingSelectedLayerId": v.missing_selected_layer_id,
        "missingSelectedGridUuid": v.missing_selected_grid_uuid.to_string(),
    })
}

fn stats_json(s: &dd::LayerComposeStats) -> Value {
    json!({
        "layerId": s.layer_id, "sampledVertices": s.sampled_vertices, "missingVertices": s.missing_vertices,
        "winningVertices": s.winning_vertices, "contributedVertices": s.contributed_vertices,
        "additiveMerge": s.additive_merge, "hasDepthRange": s.has_depth_range,
        "minDepth": num(s.min_depth), "maxDepth": num(s.max_depth),
    })
}

fn apply_summary_json(s: &dd::ApplySummary) -> Value {
    json!({
        "succeeded": s.succeeded, "changedTargets": s.changed_targets, "changedVertices": s.changed_vertices,
        "layersApplied": s.layers_applied, "layersSkipped": s.layers_skipped, "missingSamples": s.missing_samples,
    })
}

fn row_json(r: &dd::LayerStackRow) -> Value {
    json!({
        "sourceIndex": r.source_index, "layerId": r.layer_id, "displayName": r.display_name,
        "layerPath": r.layer_path, "enabled": r.enabled, "visible": r.visible,
        "hasDepthPixels": r.has_depth_pixels, "hasNormalCoverage": r.has_normal_coverage,
        "hasBinding": r.has_binding, "hasAmbiguousBindings": r.has_ambiguous_bindings,
        "targetGridUuid": r.target_grid_uuid.to_string(), "targetDisplayName": r.target_display_name,
        "mergePolicy": r.merge_policy.name(), "sampledVertices": r.sampled_vertices,
        "missingVertices": r.missing_vertices, "winningVertices": r.winning_vertices,
        "hasDepthRange": r.has_depth_range, "minDepth": num(r.min_depth), "maxDepth": num(r.max_depth),
        "warnings": {
            "missingDepthPixels": r.warning_missing_depth_pixels, "missingTarget": r.warning_missing_target,
            "missingCoverage": r.warning_missing_coverage, "mostlyMissingSamples": r.warning_mostly_missing_samples,
        },
    })
}

fn compose_json(id: u32, result: &dd::ComposeResult, summary: &dd::ApplySummary) -> Value {
    let preview = dd::composite_preview_summary(result);
    json!({
        "target": id,
        "vertexCount": result.depths.len(),
        "sampledVertices": result.sampled_vertices,
        "missingVertices": result.missing_vertices,
        "winningVertices": preview.winning_vertices,
        "finalMissingVertices": preview.final_missing_vertices,
        "hasDepthRange": result.has_depth_range,
        "minDepth": num(result.min_depth),
        "maxDepth": num(result.max_depth),
        "depths": nums(&result.depths),
        "winningLayerIds": result.winning_layer_ids,
        "layerStats": result.layer_stats.iter().map(stats_json).collect::<Vec<_>>(),
        "apply": apply_summary_json(summary),
    })
}

// ------------------------------------------------------------------------------------------ the ops

impl Session {
    /// The model a file write must contain: the DepthDraw manifest under D's key (removed when there is none).
    pub(crate) fn model_for_save(&self, mut model: Value) -> Value {
        let state = &self.depth_draw;
        if !self.document.is_inx() {
            return model;
        }
        if let Some(object) = model.as_object_mut() {
            if state.has_content() {
                object.insert(MANIFEST_KEY.into(), Value::String(dd::session_to_persistent_json(&state.draw)));
            } else if !state.unreadable {
                object.remove(MANIFEST_KEY);
            }
        }
        model
    }

    fn document_target(&self, request: &Value) -> Result<u32, String> {
        match request.get("target") {
            None | Some(Value::Null) => {
                let selected = self.depth_draw.draw.selected_grid_uuid;
                valid_id(selected).ok_or_else(|| "target is required (no target is selected)".to_string())
            }
            Some(value) => node_id(value, "target"),
        }
    }

    fn layer_index_of(&self, request: &Value) -> Result<usize, String> {
        let id = req_str(request, "layer")?;
        self.depth_draw.draw.layer_index(id).ok_or_else(|| format!("layer {id:?} was not found"))
    }

    /// Compose one target against the current document (D `ngComposeDepthDrawTarget`).
    fn depth_draw_compose(&self, id: u32) -> Result<(dd::ComposeResult, TargetInfo), String> {
        let draw = &self.depth_draw.draw;
        self.compose_with(id, &draw.layers, &draw.bindings)
    }

    /// Compose `layers` and `bindings` on one target: base depths are the stored ones, vertices go through the
    /// node's posed world matrix and the session's document size.
    fn compose_with(
        &self,
        id: u32,
        layers: &[dd::Layer],
        bindings: &[dd::Binding],
    ) -> Result<(dd::ComposeResult, TargetInfo), String> {
        let info = target_info(&self.document.model, id)?;
        let posed = self.posed_model()?;
        let world = crate::psd_depth::world_matrix(&posed, id)?;
        let draw = &self.depth_draw.draw;
        let points = dd::document_points(&info.points, &world, draw.document_width.max(1), draw.document_height.max(1));
        let mut base = info.stored.clone().unwrap_or_default();
        base.resize(info.points.len(), 0.0);
        let target = dd::Target { grid_uuid: u64::from(id), document_points: &points, base_depths: &base };
        Ok((dd::compose_target(layers, bindings, &target)?, info))
    }

    fn compose_summary(&self, id: u32, result: &dd::ComposeResult, info: &TargetInfo) -> dd::ApplySummary {
        dd::summarize_apply(result, u64::from(id), info.points.len(), Some(info.stored.as_deref().unwrap_or(&[])))
    }

    /// The file a layer's `sourcePath` names: a relative one beside the open document when it is there (a
    /// collected copy names its images that way), else against the working directory, where
    /// `depth.draw.load_png` took it from.
    pub(crate) fn depth_source(&self, source: &str) -> PathBuf {
        let given = Path::new(source);
        let folder = self.file_path.as_deref().and_then(Path::parent).filter(|_| given.is_relative());
        match folder.map(|folder| folder.join(given)) {
            Some(beside) if beside.exists() => beside,
            _ => given.to_path_buf(),
        }
    }

    pub(crate) fn op_depth_draw_manifest_get(&self, _request: &Value) -> Result<Value, String> {
        let draw = &self.depth_draw.draw;
        let model = &self.document.model;
        let validation = dd::validate_manifest(
            draw,
            Some(&|uuid| is_depth_target(model, uuid)),
            &|path| self.depth_source(path).exists(),
        );
        Ok(json!({
            "manifest": dd::session_to_manifest(draw),
            "layers": layer_pixels_json(draw),
            "validation": validation_json(&validation),
            "retainedPngBytes": retained_png_bytes(draw),
            "unreadable": self.depth_draw.unreadable,
        }))
    }

    /// `depth.draw.manifest.set`: replace the session with a manifest given inline (`manifest`: object or
    /// the persistent JSON string; `null` clears) or read from `path`. Pixels are not part of a manifest, so
    /// every layer starts without pixels (see `depth.draw.load_png`).
    pub(crate) fn op_depth_draw_manifest_set(&mut self, request: &Value) -> Result<Value, String> {
        let base_dir: Option<PathBuf>;
        let next = if let Some(path) = opt_str(request, "path")? {
            if request.get("manifest").is_some_and(|value| !value.is_null()) {
                return Err("give either manifest or path, not both".into());
            }
            let bytes = read_bounded_file(path, MAX_MANIFEST_BYTES, "DepthDraw manifest")?;
            let text = String::from_utf8(bytes).map_err(|_| "DepthDraw manifest must be UTF-8 text")?;
            base_dir = Path::new(path).parent().map(Path::to_path_buf);
            dd::session_from_persistent_json(&text)?.unwrap_or_default()
        } else {
            base_dir = None;
            match request.get("manifest") {
                None => return Err("manifest or path is required".into()),
                Some(Value::Null) => dd::Session::new(),
                Some(Value::String(text)) => dd::session_from_persistent_json(text)?.unwrap_or_default(),
                Some(value) => dd::session_from_manifest(value)?,
            }
        };
        let model = &self.document.model;
        let validation = dd::validate_manifest(
            &next,
            Some(&|uuid| is_depth_target(model, uuid)),
            &|path| {
                let given = Path::new(path);
                match (&base_dir, given.is_absolute()) {
                    (Some(dir), false) => dir.join(given).exists(),
                    _ => given.exists(),
                }
            },
        );
        let mut next = next;
        next.mark_all_preview_dirty();
        self.depth_draw.draw = next;
        self.depth_draw.unreadable = false;
        self.depth_draw.touch();
        Ok(json!({
            "manifest": dd::session_to_manifest(&self.depth_draw.draw),
            "layers": layer_pixels_json(&self.depth_draw.draw),
            "validation": validation_json(&validation),
        }))
    }

    /// `depth.draw.load_png`: one PNG file becomes one layer whose depth pixels are its RGBA (D
    /// `ngLoadDepthDrawPngLayer`). A layer with the same id that has no pixels (it came from a manifest) gets
    /// the pixels and keeps its settings; the recorded cleanup is replayed on them.
    pub(crate) fn op_depth_draw_load_png(&mut self, request: &Value) -> Result<Value, String> {
        let path = req_str(request, "path")?;
        let id = opt_str(request, "id")?.unwrap_or("");
        let bytes = read_bounded_file(&self.depth_source(path).to_string_lossy(), MAX_PNG_BYTES, "PNG")?;
        let draw = &self.depth_draw.draw;
        let mut retained = retained_png_bytes(draw);
        let mut loaded = dd::load_png_layer(path, id, &bytes, &mut retained)?;
        let existing = draw.layer_index(&loaded.id);
        let (width, height) = (loaded.width, loaded.height);
        let layer_id = loaded.id.clone();
        match existing {
            Some(index) => {
                let old = &draw.layers[index];
                if old.has_depth_pixels() {
                    return Err(format!("layer {layer_id:?} already has pixels"));
                }
                if old.width != width || old.height != height {
                    return Err(format!(
                        "PNG is {width}x{height} but layer {layer_id:?} was saved as {}x{}",
                        old.width, old.height
                    ));
                }
                // The retained budget was checked against the layers that hold pixels; the old layer held none.
                let mut merged = old.clone();
                merged.rgba = std::mem::take(&mut loaded.rgba);
                merged.depth_pixels = std::mem::take(&mut loaded.depth_pixels);
                merged.alpha_mask.clear();
                merged.normal_coverage.clear();
                dd::replay_layer_cleanup_operations(&mut merged, index as i32)?;
                self.depth_draw.draw.layers[index] = merged;
            }
            None => self.depth_draw.draw.layers.push(loaded),
        }
        let draw = &mut self.depth_draw.draw;
        if draw.document_width <= 0 || draw.document_height <= 0 {
            // D's PNG loader has no document: the image itself is the canvas.
            draw.document_width = width;
            draw.document_height = height;
        }
        draw.mark_layer_preview_dirty(&layer_id);
        self.depth_draw.touch();
        let draw = &self.depth_draw.draw;
        let index = draw.layer_index(&layer_id).unwrap_or(0);
        Ok(json!({
            "layer": dd::session_to_manifest(draw)["layers"][index].clone(),
            "layers": draw.layers.len(),
            "documentWidth": draw.document_width,
            "documentHeight": draw.document_height,
            "retainedPngBytes": retained_png_bytes(draw),
        }))
    }

    /// `depth.draw.layer.update`: the session's four layer updaters. Fields of a group that are not given keep
    /// their value. Groups: `xyOffset`/`xyScale`; `backDepth`/`frontDepth`/`invert`/`zScale`/`zOffset`;
    /// `channel`/`convolution`/`customRadius`/`alphaThreshold`; `visible`/`enabled`.
    pub(crate) fn op_depth_draw_layer_update(&mut self, request: &Value) -> Result<Value, String> {
        let index = self.layer_index_of(request)?;
        // The settings only: a layer's pixels are not copied for a slider drag.
        let layer = {
            let l = &self.depth_draw.draw.layers[index];
            dd::Layer {
                id: l.id.clone(),
                xy_offset: l.xy_offset,
                xy_scale: l.xy_scale,
                back_depth: l.back_depth,
                front_depth: l.front_depth,
                invert: l.invert,
                z_scale: l.z_scale,
                z_offset: l.z_offset,
                channel: l.channel,
                convolution: l.convolution,
                custom_radius: l.custom_radius,
                alpha_threshold: l.alpha_threshold,
                visible: l.visible,
                enabled: l.enabled,
                ..dd::Layer::default()
            }
        };
        let offset = opt_pair(request, "xyOffset")?;
        let scale = opt_pair(request, "xyScale")?;
        let z = (
            opt_f32(request, "backDepth")?,
            opt_f32(request, "frontDepth")?,
            opt_bool(request, "invert")?,
            opt_f32(request, "zScale")?,
            opt_f32(request, "zOffset")?,
        );
        let channel = opt_str(request, "channel")?.map(dd::Channel::from_name).transpose()?;
        let convolution = opt_str(request, "convolution")?.map(dd::Convolution::from_name).transpose()?;
        let radius = opt_i32(request, "customRadius")?;
        let threshold = opt_f32(request, "alphaThreshold")?;
        let visible = opt_bool(request, "visible")?;
        let enabled = opt_bool(request, "enabled")?;
        let xy_given = offset.is_some() || scale.is_some();
        let z_given = z.0.is_some() || z.1.is_some() || z.2.is_some() || z.3.is_some() || z.4.is_some();
        let sampling_given = channel.is_some() || convolution.is_some() || radius.is_some() || threshold.is_some();
        let visibility_given = visible.is_some() || enabled.is_some();
        if !(xy_given || z_given || sampling_given || visibility_given) {
            return Err("no layer field was given".into());
        }
        let id = layer.id.clone();
        let draw = &mut self.depth_draw.draw;
        if xy_given {
            draw.update_layer_xy_transform(&id, offset.unwrap_or(layer.xy_offset), scale.unwrap_or(layer.xy_scale));
        }
        if z_given {
            draw.update_layer_z_transform(
                &id,
                z.0.unwrap_or(layer.back_depth),
                z.1.unwrap_or(layer.front_depth),
                z.2.unwrap_or(layer.invert),
                z.3.unwrap_or(layer.z_scale),
                z.4.unwrap_or(layer.z_offset),
            );
        }
        if sampling_given {
            draw.update_layer_sampling(
                &id,
                channel.unwrap_or(layer.channel),
                convolution.unwrap_or(layer.convolution),
                radius.unwrap_or(layer.custom_radius),
                threshold.unwrap_or(layer.alpha_threshold),
            );
        }
        if visibility_given {
            draw.update_layer_visibility(&id, visible.unwrap_or(layer.visible), enabled.unwrap_or(layer.enabled));
        }
        self.depth_draw.touch();
        let draw = &self.depth_draw.draw;
        Ok(json!({"layer": dd::session_to_manifest(draw)["layers"][index].clone()}))
    }

    /// `depth.draw.binding.update`: change, or create, the binding of one layer to one target. State fields are
    /// `enabled`, `order`, `mergePolicy`; sampling fields are `useNormalLayerAlpha` and `coverageThreshold`.
    pub(crate) fn op_depth_draw_binding_update(&mut self, request: &Value) -> Result<Value, String> {
        let layer = self.layer_index_of(request)?;
        let layer_id = self.depth_draw.draw.layers[layer].id.clone();
        let grid = node_id(request.get("target").ok_or("target is required")?, "target")?;
        let (enabled, order) = (opt_bool(request, "enabled")?, opt_i32(request, "order")?);
        let policy = opt_str(request, "mergePolicy")?.map(dd::MergePolicy::from_name).transpose()?;
        let (use_alpha, threshold) = (opt_bool(request, "useNormalLayerAlpha")?, opt_f32(request, "coverageThreshold")?);
        let grid64 = u64::from(grid);
        let draw = &mut self.depth_draw.draw;
        let found = draw.bindings.iter().position(|b| b.layer_id == layer_id && b.target_grid_uuid == grid64);
        if found.is_none() {
            if !is_depth_target(&self.document.model, grid64) {
                return Err("target must be a GridDeformer or PathDeformer".into());
            }
            draw.bindings.push(dd::Binding {
                layer_id: layer_id.clone(),
                target_node_uuid: grid64,
                target_grid_uuid: grid64,
                ..dd::Binding::default()
            });
        }
        let at = found.unwrap_or(draw.bindings.len() - 1);
        let current = draw.bindings[at].clone();
        if enabled.is_some() || order.is_some() || policy.is_some() || found.is_none() {
            draw.update_binding_state(
                &layer_id,
                grid64,
                enabled.unwrap_or(current.enabled),
                order.unwrap_or(current.order),
                policy.unwrap_or(current.merge_policy),
            );
        }
        if use_alpha.is_some() || threshold.is_some() {
            draw.update_binding_sampling(
                &layer_id,
                grid64,
                use_alpha.unwrap_or(current.use_normal_layer_alpha),
                threshold.unwrap_or(current.coverage_threshold),
            );
        }
        self.depth_draw.touch();
        let draw = &self.depth_draw.draw;
        Ok(json!({
            "created": found.is_none(),
            "binding": dd::session_to_manifest(draw)["bindings"][at].clone(),
        }))
    }

    /// `depth.draw.auto_bind`: match every layer to a Part (or, with `matchDirectGridName`, a depth-mapped
    /// deformer) by layer path, then base name, then name (D `ngDepthDrawAutoBindSession`).
    pub(crate) fn op_depth_draw_auto_bind(&mut self, request: &Value) -> Result<Value, String> {
        let direct = opt_bool(request, "matchDirectGridName")?.unwrap_or(true);
        let nodes = bind_nodes(&self.document.model);
        let model = &self.document.model;
        let results = dd::auto_bind_session(&mut self.depth_draw.draw, &nodes, direct, &|uuid| is_depth_target(model, uuid));
        self.depth_draw.draw.mark_all_preview_dirty();
        self.depth_draw.touch();
        let results: Vec<Value> = results
            .iter()
            .map(|r| {
                json!({
                    "layerId": r.layer_id, "layerPath": r.layer_path, "layerName": r.layer_name,
                    "matchedNodeName": r.matched_node_name, "targetGridName": r.target_grid_name,
                    "matchedNodeUuid": r.matched_node_uuid.to_string(),
                    "targetGridUuid": r.target_grid_uuid.to_string(),
                    "matched": r.matched, "ambiguous": r.ambiguous, "status": r.status,
                })
            })
            .collect();
        let matched = results.iter().filter(|r| r["matched"] == true).count();
        Ok(json!({
            "results": results, "matched": matched,
            "bindings": dd::session_to_manifest(&self.depth_draw.draw)["bindings"].clone(),
        }))
    }

    /// `depth.draw.compose` (read-only): the depths the bound layers give one target, with the winners, the
    /// per-layer statistics and what `depth.draw.apply` would change.
    pub(crate) fn op_depth_draw_compose(&self, request: &Value) -> Result<Value, String> {
        let id = self.document_target(request)?;
        let (result, info) = self.depth_draw_compose(id)?;
        let summary = self.compose_summary(id, &result, &info);
        Ok(compose_json(id, &result, &summary))
    }

    /// `depth.draw.apply`: write the composed depths to the target as ONE undo step (D
    /// `ngApplyDepthDrawTargetResultWithSummary`). A grid that has saved depth operations gets the composed
    /// depths as the new `depth-op-base-depths`, and its `depths` are the operations run on that base; any other
    /// target gets the composed depths.
    pub(crate) fn op_depth_draw_apply(&mut self, request: &Value) -> Result<Value, String> {
        let id = self.document_target(request)?;
        let (result, info) = self.depth_draw_compose(id)?;
        let summary = self.compose_summary(id, &result, &info);
        if !summary.succeeded {
            return Err("DepthDraw result does not match the target".into());
        }
        let stored = info.stored.clone().unwrap_or_default();
        let mut candidate = self.document.model.clone();
        let composed: Vec<f64> = result.depths.iter().map(|d| finite_depth_or_zero(f64::from(*d))).collect();
        let node = crate::nodes::find_node_mut(&mut candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
        let object = node.as_object_mut().ok_or("node must be an object")?;
        if info.is_grid && info.has_ops {
            object.insert(depth_ops::BASE_KEY.into(), json!(composed));
            candidate = depth_ops::apply_ops(&candidate, &json!({"target": id}))?;
        } else {
            object.insert("depths".into(), json!(composed));
        }
        let after = target_info(&candidate, id)?.stored.unwrap_or_default();
        let mut applied = summary;
        applied.changed_vertices = dd::count_changed_vertices(&stored, &after);
        applied.changed_targets = usize::from(applied.changed_vertices > 0);
        self.commit_targets(candidate, &[id])?;
        Ok(json!({"target": id, "apply": apply_summary_json(&applied), "depths": nums(&after)}))
    }

    /// `depth.draw.cleanup`: `kind` is `gap_fill` (alpha-depth gap fill with optional `rules`
    /// `[{x, y, w, h, lift, radius}]`) or `contour_repair` (`thickness`), on one layer's depth pixels.
    pub(crate) fn op_depth_draw_cleanup(&mut self, request: &Value) -> Result<Value, String> {
        let index = self.layer_index_of(request)?;
        let kind = req_str(request, "kind")?;
        if !self.depth_draw.draw.layers[index].has_depth_pixels() {
            return Err(format!("layer {:?} has no depth pixels (load its PNG first)", self.depth_draw.draw.layers[index].id));
        }
        let mut copy = self.depth_draw.draw.layers[index].clone();
        let id = copy.id.clone();
        let reply = match kind {
            "gap_fill" => {
                let rules = focused_rules(request, index)?;
                let s = dd::apply_layer_alpha_depth_gap_fill(&mut copy, index as i32, &rules)?;
                json!({
                    "kind": kind, "layerId": s.layer_id, "succeeded": s.succeeded, "changed": s.changed,
                    "detected": {"zero": s.detected.zero, "depression": s.detected.depression,
                        "cliff": s.detected.cliff, "focusedAdded": s.detected.focused_added, "total": s.detected.total},
                    "filled": s.filled.filled, "remaining": s.filled.remaining,
                })
            }
            "contour_repair" => {
                let thickness = opt_i32(request, "thickness")?.unwrap_or(2);
                let s = dd::repair_layer_contour_depth(&mut copy, thickness)?;
                json!({
                    "kind": kind, "layerId": s.layer_id, "succeeded": s.succeeded, "changed": s.changed,
                    "contourPixels": s.contour_pixels, "filledPixels": s.filled_pixels,
                })
            }
            other => return Err(format!("kind must be gap_fill or contour_repair, not {other:?}")),
        };
        let changed = reply["changed"] == true;
        self.depth_draw.draw.layers[index] = copy;
        if changed {
            self.depth_draw.draw.mark_layer_preview_dirty(&id);
            self.depth_draw.touch();
        }
        Ok(reply)
    }

    /// `depth.draw.fit_z`: fit one layer's z scale and offset so its range on `target` lies in a gap.
    /// `gap` is `adjacent` (between the layers before and after it, in layer order), `target` (the target's
    /// stored depth range), `layers` (between `backLayer` and `frontLayer`) or `values` (`back`, `front`).
    /// `margin` shrinks the gap on both sides. `apply: false` only reports the fit.
    pub(crate) fn op_depth_draw_fit_z(&mut self, request: &Value) -> Result<Value, String> {
        let index = self.layer_index_of(request)?;
        let id = self.document_target(request)?;
        let margin = opt_f32(request, "margin")?.unwrap_or(0.0);
        let apply = opt_bool(request, "apply")?.unwrap_or(true);
        let draw = &self.depth_draw.draw;
        let layer = draw.layers[index].clone();
        let grid = u64::from(id);
        let binding = draw
            .bindings
            .iter()
            .find(|b| b.applies_to(grid) && b.layer_id == layer.id)
            .ok_or_else(|| format!("layer {:?} is not bound to target {id}", layer.id))?
            .clone();
        // The raw range is what the layer samples before its own z transform.
        let mut raw_layer = layer.clone();
        (raw_layer.z_scale, raw_layer.z_offset, raw_layer.enabled, raw_layer.visible) = (1.0, 0.0, true, true);
        let (alone, info) = self.compose_with(id, std::slice::from_ref(&raw_layer), &[binding])?;
        let raw = alone
            .layer_stats
            .first()
            .map(|s| dd::DepthRange { valid: s.has_depth_range, min_depth: s.min_depth, max_depth: s.max_depth })
            .unwrap_or_default();
        let (all, _) = self.depth_draw_compose(id)?;
        let range_of = |layer_id: &str| {
            all.layer_stats
                .iter()
                .find(|s| s.layer_id == layer_id)
                .map(|s| dd::DepthRange { valid: s.has_depth_range, min_depth: s.min_depth, max_depth: s.max_depth })
                .unwrap_or_default()
        };
        let named = |key: &str| -> Result<dd::DepthRange, String> {
            let wanted = req_str(request, key)?;
            self.depth_draw.draw.layer_by_id(wanted).ok_or_else(|| format!("layer {wanted:?} was not found"))?;
            Ok(range_of(wanted))
        };
        let gap = match opt_str(request, "gap")?.unwrap_or("adjacent") {
            "adjacent" => {
                let ranges: Vec<dd::DepthRange> = self.depth_draw.draw.layers.iter().map(|l| range_of(&l.id)).collect();
                dd::gap_from_adjacent_ranges(&ranges, index)
            }
            "target" => dd::gap_from_target_range(dd::range_from_values(&info.stored.clone().unwrap_or_default())),
            "layers" => dd::gap_from_selected_layer_ranges(named("backLayer")?, named("frontLayer")?),
            "values" => {
                let back = opt_f32(request, "back")?.ok_or("back is required for gap values")?;
                let front = opt_f32(request, "front")?.ok_or("front is required for gap values")?;
                dd::FitZGap { valid: true, error: String::new(), back, front }
            }
            other => return Err(format!("gap must be adjacent, target, layers or values, not {other:?}")),
        };
        let fit = dd::fit_z_to_gap_checked(raw, &gap, margin);
        let diagnostics = dd::fit_z_diagnostics(raw, &gap, &fit);
        let applied = apply && fit.succeeded;
        if applied {
            let draw = &mut self.depth_draw.draw;
            let at = &draw.layers[index];
            draw.update_layer_z_transform(
                &layer.id,
                at.back_depth,
                at.front_depth,
                at.invert,
                fit.z_scale,
                fit.z_offset,
            );
            self.depth_draw.touch();
        }
        Ok(json!({
            "layerId": layer.id, "applied": applied, "succeeded": fit.succeeded, "error": diagnostics.error,
            "zScale": num(fit.z_scale), "zOffset": num(fit.z_offset),
            "targetMin": num(fit.target_min), "targetMax": num(fit.target_max),
            "rawRangeValid": diagnostics.raw_range_valid, "rawMin": num(diagnostics.raw_min),
            "rawMax": num(diagnostics.raw_max), "gapValid": diagnostics.gap_valid,
            "gapBack": num(diagnostics.gap_back), "gapFront": num(diagnostics.gap_front),
        }))
    }

    /// `depth.draw.export_png`: one grey depth PNG per layer that has pixels, named `NNN-id.png`, plus a
    /// manifest that points at them (D `ngExportDepthDrawPngSession`). Existing files are kept unless
    /// `overwrite` is true.
    pub(crate) fn op_depth_draw_export_png(&self, request: &Value) -> Result<Value, String> {
        self.execute_depth_png_export(self.prepare_depth_png_export(request)?)
    }

    pub(crate) fn prepare_depth_png_export(&self, request: &Value) -> Result<crate::export::DepthPngPlan, String> {
        use crate::export_ops::resolve_collection_path;
        let dir = Path::new(req_str(request, "dir")?);
        if dir.as_os_str().is_empty() { return Err("export directory must not be empty".into()); }
        let overwrite = opt_bool(request, "overwrite")?.unwrap_or(false);
        let manifest_path = match opt_str(request, "manifest")? {
            Some(path) => PathBuf::from(path),
            None => dir.join("manifest.json"),
        };
        let mut export = dd::export_png_session(&self.depth_draw.draw)?;
        let directory = resolve_collection_path(dir)?;
        let manifest_target = depth_manifest_target(&manifest_path, &directory)?;
        let manifest_dir = manifest_target.parent().unwrap_or(&directory);
        let mut targets = Vec::new();
        for (name, bytes) in std::mem::take(&mut export.files) {
            let file = directory.join(&name);
            let reference = match file.strip_prefix(manifest_dir) {
                Ok(relative) => relative.to_string_lossy().into_owned(),
                Err(_) => file.to_string_lossy().into_owned(),
            };
            for layer in export.manifest["layers"].as_array_mut().into_iter().flatten() {
                if layer["sourcePath"] == name.as_str() {
                    layer["sourcePath"] = json!(reference);
                }
            }
            targets.push((file, std::sync::Arc::<[u8]>::from(bytes)));
        }
        let layers: Vec<_> = targets.iter().map(|(path, _)| path.clone()).collect();
        targets.push((manifest_target.clone(), export.manifest.to_string().into_bytes().into()));
        let sources = self.depth_draw.draw.layers.iter().filter(|layer| !layer.source_path.is_empty())
            .map(|layer| {
                let alias = self.depth_source(&layer.source_path);
                Ok((layer.source_path.clone(), depth_source_target(&alias, 0)?, alias.exists()))
            }).collect::<Result<Vec<_>, String>>()?;
        let response = json!({
            "manifest": manifest_target.to_string_lossy(),
            "layers": layers.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>(),
            "exportedLayers": export.exported_layers,
            "skippedLayers": export.skipped_layers,
        });
        let plan = crate::export::DepthPngPlan {
            directory, manifest: manifest_target, layers, overwrite, directory_alias: dir.to_owned(),
            manifest_alias: manifest_path, depth_revision: self.depth_draw.revision, sources, outputs: targets, response,
        };
        self.validate_depth_png_export(&plan)?;
        Ok(plan)
    }

    fn validate_depth_png_export(&self, plan: &crate::export::DepthPngPlan) -> Result<(), String> {
        use crate::export_ops::resolve_collection_path;
        if self.depth_draw.revision != plan.depth_revision {
            return Err("DepthDraw changed after export preparation".into());
        }
        if resolve_collection_path(&plan.directory_alias)? != plan.directory
            || depth_manifest_target(&plan.manifest_alias, &plan.directory)? != plan.manifest {
            return Err("Depth PNG destination changed after preparation".into());
        }
        if plan.directory.exists() && !plan.directory.is_dir() {
            return Err("Depth PNG export directory must be a directory".into());
        }
        for (source, resolved, existed) in &plan.sources {
            let alias = self.depth_source(source);
            if depth_source_target(&alias, 0)? != *resolved || alias.exists() != *existed {
                return Err("Depth PNG source changed after preparation".into());
            }
        }
        let open = self.file_path.as_deref().map(depth_output_key);
        let mut seen = std::collections::BTreeSet::new();
        for (path, _) in &plan.outputs {
            let key = depth_output_key(path);
            if !seen.insert(key.clone()) { return Err("Depth PNG outputs resolve to the same file".into()); }
            if open.as_ref() == Some(&key) || plan.sources.iter().any(|(_, source, _)| depth_output_key(source) == key) {
                return Err(format!("depth.draw.export_png would overwrite its own input {}", path.display()));
            }
            match fs::symlink_metadata(path) {
                Ok(_) if !plan.overwrite => {
                    return Err(format!("export destination already exists: {}", path.display()));
                }
                Ok(meta) if !meta.is_file() => {
                    return Err(format!("export destination is not a regular file: {}", path.display()));
                }
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(format!("could not inspect export destination: {error}"));
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub(crate) fn execute_depth_png_export(&self, plan: crate::export::DepthPngPlan) -> Result<Value, String> {
        self.validate_depth_png_export(&plan)?;
        fs::create_dir_all(&plan.directory).map_err(|error| format!("could not create export directory: {error}"))?;
        self.validate_depth_png_export(&plan)?;
        let mut created: Vec<&PathBuf> = Vec::new();
        for (path, bytes) in &plan.outputs {
            let existed = path.exists();
            if let Err(error) = write_atomic(path, bytes, plan.overwrite) {
                created.iter().for_each(|file| drop(fs::remove_file(file)));
                return Err(error);
            }
            if !existed {
                created.push(path);
            }
        }
        Ok(plan.response)
    }

    /// `depth.draw.diagnostics` (read-only): the source counts, the layer table (with the composed per-layer
    /// statistics when a target is given or selected), the depth-space planes and gaps, and the manifest check.
    pub(crate) fn op_depth_draw_diagnostics(&self, request: &Value) -> Result<Value, String> {
        let explicit = !matches!(request.get("target"), None | Some(Value::Null));
        let target = if explicit {
            Some(self.document_target(request)?)
        } else {
            valid_id(self.depth_draw.draw.selected_grid_uuid)
        };
        let composed = match target {
            Some(id) if explicit => Some(self.depth_draw_compose(id)?.0),
            Some(id) => self.depth_draw_compose(id).ok().map(|(result, _)| result),
            None => None,
        };
        let draw = &self.depth_draw.draw;
        let model = &self.document.model;
        let name_of = |uuid: u64| node_name(model, uuid);
        let rows = dd::layer_stack_rows(draw, composed.as_ref(), Some(&name_of));
        let space = dd::depth_space_summary(draw, composed.as_ref());
        let source = dd::source_diagnostics(draw);
        let validation =
            dd::validate_manifest(draw, Some(&|uuid| is_depth_target(model, uuid)), &|p| self.depth_source(p).exists());
        Ok(json!({
            "target": target,
            "composed": composed.is_some(),
            "source": {
                "totalLayers": source.total_layers, "usableDepthLayers": source.usable_depth_layers,
                "disabledLayers": source.disabled_layers, "invisibleLayers": source.invisible_layers,
                "layersMissingDepthPixels": source.layers_missing_depth_pixels,
                "layersMissingTarget": source.layers_missing_target,
                "layersMissingCoverage": source.layers_missing_coverage,
                "layersWithAmbiguousBindings": source.layers_with_ambiguous_bindings,
                "totalBindings": source.total_bindings, "enabledBindings": source.enabled_bindings,
            },
            "rows": rows.iter().map(row_json).collect::<Vec<_>>(),
            "depthSpace": {
                "selectedGridUuid": space.selected_grid_uuid.to_string(),
                "selectedLayerId": space.selected_layer_id,
                "layers": space.layers.iter().map(|p| json!({
                    "layerId": p.layer_id, "displayName": p.display_name, "selected": p.selected,
                    "visible": p.visible, "enabled": p.enabled, "hasDepthRange": p.has_depth_range,
                    "minDepth": num(p.min_depth), "maxDepth": num(p.max_depth),
                    "representativeDepth": num(p.representative_depth),
                })).collect::<Vec<_>>(),
                "gaps": space.gaps.iter().map(|g| json!({
                    "backLayerId": g.back_layer_id, "frontLayerId": g.front_layer_id, "valid": g.valid,
                    "overlap": g.overlap, "backDepth": num(g.back_depth), "frontDepth": num(g.front_depth),
                })).collect::<Vec<_>>(),
            },
            "validation": validation_json(&validation),
        }))
    }

    /// `depth.draw.display`: set any of the nine display options (`showNormalImage`, `showRawDepth`,
    /// `showCoverage`, `showComposite`, `showLayerPlanes`, `showDepthRanges`, `showMissingVertices`,
    /// `showWinningLayer`, `useGpuPreview`).
    pub(crate) fn op_depth_draw_display(&mut self, request: &Value) -> Result<Value, String> {
        let mut next = self.depth_draw.draw.display;
        let fields: [(&str, &mut bool); 9] = [
            ("showNormalImage", &mut next.show_normal_image),
            ("showRawDepth", &mut next.show_raw_depth),
            ("showCoverage", &mut next.show_coverage),
            ("showComposite", &mut next.show_composite),
            ("showLayerPlanes", &mut next.show_layer_planes),
            ("showDepthRanges", &mut next.show_depth_ranges),
            ("showMissingVertices", &mut next.show_missing_vertices),
            ("showWinningLayer", &mut next.show_winning_layer),
            ("useGpuPreview", &mut next.use_gpu_preview),
        ];
        let mut given = false;
        for (key, slot) in fields {
            if let Some(flag) = opt_bool(request, key)? {
                *slot = flag;
                given = true;
            }
        }
        if !given {
            return Err("no display option was given".into());
        }
        let changed = self.depth_draw.draw.update_display_options(next);
        if changed {
            self.depth_draw.touch();
        }
        Ok(json!({"changed": changed, "display": dd::session_to_manifest(&self.depth_draw.draw)["display"].clone()}))
    }

    /// `depth.draw.select`: `layer` and/or `target` select; `point` `[x, y]` (document pixels) selects the
    /// topmost layer plane there; `clear: true` drops the selection.
    pub(crate) fn op_depth_draw_select(&mut self, request: &Value) -> Result<Value, String> {
        let layer = opt_str(request, "layer")?;
        let target = request.get("target").filter(|v| !v.is_null()).map(|v| node_id(v, "target")).transpose()?;
        let point = opt_pair(request, "point")?;
        let clear = opt_bool(request, "clear")?.unwrap_or(false);
        if layer.is_none() && target.is_none() && point.is_none() && !clear {
            return Err("nothing to select".into());
        }
        let draw = &mut self.depth_draw.draw;
        if let Some(id) = layer {
            if draw.layer_by_id(id).is_none() {
                return Err(format!("layer {id:?} was not found"));
            }
        }
        if let Some(id) = target {
            if !draw.has_target_grid(u64::from(id)) {
                return Err(format!("target {id} has no binding"));
            }
        }
        if clear {
            draw.clear_selection();
        }
        let mut hit = None;
        if let Some(point) = point {
            hit = Some(draw.select_layer_plane_at_document_point(point));
        }
        if let Some(id) = layer {
            draw.select_layer(id);
        }
        if let Some(id) = target {
            draw.select_target_grid(u64::from(id));
        }
        self.depth_draw.touch();
        let draw = &self.depth_draw.draw;
        Ok(json!({
            "selectedLayerId": draw.selected_layer_id,
            "selectedGridUuid": draw.selected_grid_uuid.to_string(),
            "hit": hit,
        }))
    }
}

fn retained_png_bytes(draw: &dd::Session) -> u64 {
    draw.layers
        .iter()
        .filter(|layer| layer.has_depth_pixels())
        .map(|layer| layer.width as u64 * layer.height as u64 * PNG_BYTES_PER_PIXEL)
        .sum()
}

fn focused_rules(request: &Value, layer_index: usize) -> Result<Vec<dd::FocusedRule>, String> {
    let list = match request.get("rules") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(list)) => list,
        Some(_) => return Err("rules must be an array".into()),
    };
    list.iter()
        .map(|entry| {
            if !entry.is_object() {
                return Err("each rule must be an object".to_string());
            }
            let int = |key: &str| opt_i32(entry, key)?.ok_or_else(|| format!("rule.{key} is required"));
            Ok(dd::FocusedRule {
                layer_index: layer_index as i32,
                x: int("x")?,
                y: int("y")?,
                w: int("w")?,
                h: int("h")?,
                lift: opt_i32(entry, "lift")?.unwrap_or(0),
                radius: opt_i32(entry, "radius")?.unwrap_or(0),
            })
        })
        .collect()
}
