//! CMO3 to INX conversion (issue #146), the lossy mode the owner approved on 2026-10-20. The default pose of the
//! native scene becomes the INX node tree, and every native parameter becomes a one-axis INX parameter whose keys
//! hold the offsets the pose at that key moves each ArtMesh vertex by. Deformers, Glue and blend shapes are baked
//! into those offsets, so the file keeps the look of every single-parameter key and loses the editable deformer
//! graph; `Converted::report` names what the file no longer carries. Keyed ArtMesh draw order and opacity become
//! zSort and opacity bindings of the same parameter, and a CAN3 Part opacity curve becomes a 0..1 parameter whose
//! opacity bindings reach every node the Part fades.

use crate::model_import::Texture;
use crate::native_scene::NativeScene;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct Converted {
    pub model: Value,
    pub textures: Vec<Texture>,
    pub report: Value,
    /// The INX parameter of each native parameter ID, for the CAN3 lanes.
    pub parameter_uuids: BTreeMap<String, u32>,
    /// The default value of each native parameter, the base a CAN3 scene is sampled over.
    pub defaults: BTreeMap<String, f64>,
    /// The first UUID no node or converted parameter uses, for the parameters the CAN3 Part lanes add.
    pub next_id: u32,
}

/// A rendered node that a parameter key can move: its mesh vertices (empty for a Composite), draw order and opacity.
struct Posed {
    vertices: Vec<f64>,
    zsort: f64,
    opacity: f64,
}

/// Two normalized axis points this close are one key.
const KEY_EPSILON: f64 = 1e-9;

pub(crate) fn to_inx(scene: &NativeScene) -> Result<Converted, String> {
    let parameters = scene.parameters();
    let mut defaults = BTreeMap::new();
    for parameter in parameters {
        defaults.insert(parameter_id(parameter)?, number(parameter, "default")?);
    }
    let (base, mut warnings) = scene.render_model_with_parts(&defaults, &BTreeMap::new())?;
    let mut nodes = base["nodes"].clone();
    complete_node(&mut nodes, "Root")?;
    let base_parts = posed_parts(&nodes)?;
    let mut next_id = fresh_id(&nodes)?;

    let sources = scene.source()["sources"].as_array().ok_or("native sources are missing")?;
    let keys = parameter_keys(sources)?;
    let mut inx_parameters = Vec::with_capacity(parameters.len());
    let mut parameter_uuids = BTreeMap::new();
    let mut deform_bindings = 0usize;
    // Per node, the (parameter index, absolute opacity at every point) of each parameter that moves its opacity.
    let mut opacity_changes: BTreeMap<u32, Vec<(usize, Vec<f64>)>> = BTreeMap::new();
    for parameter in parameters {
        let id = parameter_id(parameter)?;
        let (minimum, maximum) = (number(parameter, "minimum")?, number(parameter, "maximum")?);
        let default = defaults[&id];
        let span = maximum - minimum;
        let normalize = |value: f64| ((value - minimum) / span).clamp(0.0, 1.0);
        let mut points = vec![0.0, 1.0];
        if span > 0.0 {
            points.push(normalize(default));
            points.extend(keys.get(&id).into_iter().flatten().map(|key| normalize(*key)));
        } else {
            warnings.push(format!("parameter {id:?} has an empty range; its keys are not converted"));
        }
        points.sort_by(f64::total_cmp);
        points.dedup_by(|later, earlier| (*later - *earlier).abs() < KEY_EPSILON);

        let mut cells: BTreeMap<u32, Vec<Vec<f64>>> = BTreeMap::new();
        let mut moved: BTreeMap<u32, bool> = BTreeMap::new();
        let mut zsorts: BTreeMap<u32, Vec<f64>> = BTreeMap::new();
        let mut opacities: BTreeMap<u32, Vec<f64>> = BTreeMap::new();
        if span > 0.0 {
            for point in &points {
                let mut values = defaults.clone();
                values.insert(id.clone(), minimum + point * span);
                let (model, _) = scene.render_model_with_parts(&values, &BTreeMap::new())?;
                let parts = posed_parts(&model["nodes"])?;
                for (node, posed) in &parts {
                    let Some(rest) = base_parts.get(node) else {
                        return Err(format!("node {node} is drawn at a key of {id:?} but not at the default pose"));
                    };
                    if rest.vertices.len() != posed.vertices.len() {
                        return Err(format!("node {node} changes its vertex count along {id:?}"));
                    }
                    let offsets: Vec<f64> = posed.vertices.iter().zip(&rest.vertices).map(|(at, rest)| at - rest).collect();
                    if offsets.iter().any(|offset| offset.abs() > KEY_EPSILON) {
                        moved.insert(*node, true);
                    }
                    zsorts.entry(*node).or_default().push(posed.zsort - rest.zsort);
                    opacities.entry(*node).or_default().push(posed.opacity);
                    cells.entry(*node).or_default().push(offsets);
                }
            }
        }
        let mut bindings = Vec::new();
        for (node, columns) in cells {
            if !moved.contains_key(&node) {
                continue;
            }
            deform_bindings += 1;
            let values: Vec<Value> = columns
                .iter()
                .map(|offsets| json!([offsets.chunks(2).map(|pair| json!([pair[0], pair[1]])).collect::<Vec<_>>()]))
                .collect();
            bindings.push(json!({
                "node": node, "param_name": "deform", "interpolate_mode": "Linear",
                "values": values, "isSet": vec![json!([true]); columns.len()],
            }));
        }
        for (node, offsets) in zsorts {
            if offsets.iter().any(|offset| offset.abs() > KEY_EPSILON) {
                bindings.push(value_binding(node, "zSort", &offsets));
            }
        }
        for (node, column) in opacities {
            if column.iter().any(|opacity| (opacity - base_parts[&node].opacity).abs() > KEY_EPSILON) {
                opacity_changes.entry(node).or_default().push((inx_parameters.len(), column));
            }
        }
        let name = parameter["name"].as_str().filter(|name| !name.is_empty()).unwrap_or(&id);
        parameter_uuids.insert(id.clone(), next_id);
        inx_parameters.push(json!({
            "uuid": next_id,
            "name": name,
            "is_vec2": false,
            "min": [minimum, 0.0],
            "max": [maximum, 0.0],
            "defaults": [default, 0.0],
            "axis_points": [points, [0.0]],
            "repeat": parameter["repeat"] == true,
            "merge_mode": "Additive",
            "bindings": bindings,
        }));
        next_id += 1;
    }

    let mut model = json!({
        "meta": {"name": scene.source()["name"].as_str().unwrap_or(""), "version": "1.0-alpha"},
        "physics": {},
        "nodes": nodes,
        "param": inx_parameters,
        "automation": [],
        "animations": {},
    });
    // The opacity of a node is its tree opacity times every opacity binding, so the tree value and the binding
    // columns are chosen to give the rendered opacity at the default pose again.
    let mut opacity_key_changes = 0usize;
    for (node, changes) in opacity_changes {
        let rest = base_parts[&node].opacity;
        let scale = if changes.len() == 1 {
            crate::nodes::find_node_mut(&mut model, node).ok_or("render node is missing")?["opacity"] = json!(1.0);
            1.0
        } else if rest > KEY_EPSILON {
            rest
        } else {
            opacity_key_changes += changes.len();
            continue;
        };
        for (index, column) in changes {
            let column: Vec<f64> = column.iter().map(|opacity| opacity / scale).collect();
            let bindings = model["param"][index]["bindings"].as_array_mut().ok_or("INX bindings are not an array")?;
            bindings.push(value_binding(node, "opacity", &column));
        }
    }
    let textures = scene
        .textures()
        .iter()
        .map(|blob| Texture { tag: blob.tag, data: blob.data.clone() })
        .collect();
    let report = report(sources, &model, parameters.len(), deform_bindings, opacity_key_changes, &warnings)?;
    Ok(Converted { model, textures, report, parameter_uuids, defaults, next_id })
}

/// One CAN3 scene sampled at every frame: the parameter columns and, per Part GUID, the opacity columns
/// (NaN where no curve writes the Part).
struct Clip {
    name: String,
    guid: String,
    frames: i64,
    fps: f64,
    columns: BTreeMap<String, Vec<f64>>,
    part_columns: BTreeMap<String, Vec<f64>>,
}

/// Adds one INX animation per scene of a CAN3 that drives the converted model: the scene is sampled at every
/// frame with the CAN3 player, so the lanes are frame-exact Linear keyframes and the Bezier handles are baked.
/// A Part with opacity curves becomes a 0..1 parameter (see `part_opacity_parameters`) that the scene drives with
/// one lane; a Part that fades no node cannot be a parameter and is counted as not converted.
/// Returns the animation report.
pub(crate) fn add_can3_animations(
    model: &mut Value,
    animation: &Value,
    model_source: &Value,
    resource_guid: Option<&str>,
    scene: &NativeScene,
    converted: &Converted,
) -> Result<Value, String> {
    let resource = crate::native_playback::select_resource(animation, resource_guid)?;
    let scenes = animation["scenes"].as_array().ok_or("animation scenes are missing")?;
    let mut skipped = Vec::new();
    let mut clips = Vec::new();
    let mut part_guids = BTreeSet::new();
    for clip in scenes {
        let guid = clip["guid"].as_str().ok_or("animation scene has no GUID")?;
        let name = clip["name"].as_str().filter(|name| !name.is_empty()).unwrap_or(guid);
        let frames = clip["durationFrames"].as_i64().ok_or("animation durationFrames must be an integer")?;
        let fps = clip["fps"].as_f64().filter(|fps| *fps > 0.0).ok_or("animation FPS must be positive")?;
        let drives_model = clip["tracks"]
            .as_array()
            .is_some_and(|tracks| tracks.iter().any(|track| track["resourceGuid"].as_str() == Some(resource.as_str())));
        if frames < 1 || frames > crate::animation::MAX_ANIMATION_LENGTH || !drives_model {
            skipped.push(json!({"scene": name, "frames": frames, "drivesModel": drives_model}));
            continue;
        }
        let mut columns: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        let mut part_columns: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for frame in 0..frames {
            let sampled =
                crate::native_playback::sample(animation, model_source, &resource, guid, frame as f64, &converted.defaults)?;
            for (id, value) in &sampled.values {
                columns.entry(id.clone()).or_insert_with(|| vec![f64::NAN; frames as usize])[frame as usize] = *value;
            }
            for (part, value) in &sampled.part_opacities {
                let column = part_columns.entry(part.clone()).or_insert_with(|| vec![f64::NAN; frames as usize]);
                column[frame as usize] = *value;
            }
        }
        part_guids.extend(part_columns.keys().cloned());
        clips.push(Clip { name: name.to_owned(), guid: guid.to_owned(), frames, fps, columns, part_columns });
    }
    let part_parameters = part_opacity_parameters(model, scene, converted, &part_guids)?;
    let animations = model
        .as_object_mut()
        .ok_or("INX model is not an object")?
        .entry("animations")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("INX animations are not an object")?;
    let (mut lanes_written, mut keyframes_written) = (0usize, 0usize);
    let (mut part_lanes, mut part_not_converted) = (0usize, 0usize);
    let mut unknown_parameters = BTreeMap::new();
    for clip in clips {
        let mut lanes = Vec::new();
        for (id, values) in &clip.columns {
            let Some(uuid) = converted.parameter_uuids.get(id) else {
                *unknown_parameters.entry(id.clone()).or_insert(0usize) += 1;
                continue;
            };
            lanes.extend(lane(*uuid, values, converted.defaults.get(id).copied().unwrap_or(0.0)));
        }
        let parameter_lanes = lanes.len();
        for (part, values) in &clip.part_columns {
            let Some((uuid, part_rest)) = part_parameters.get(part) else {
                part_not_converted += 1;
                continue;
            };
            let filled: Vec<f64> =
                values.iter().map(|value| if value.is_nan() { *part_rest } else { *value }).collect();
            lanes.extend(lane(*uuid, &filled, *part_rest));
        }
        part_lanes += lanes.len() - parameter_lanes;
        keyframes_written += lanes.iter().map(|lane| lane["keyframes"].as_array().map_or(0, Vec::len)).sum::<usize>();
        lanes_written += lanes.len();
        let mut key = clip.name.clone();
        if animations.contains_key(&key) {
            key = format!("{} ({})", clip.name, clip.guid);
        }
        animations.insert(key, json!({
            "length": clip.frames, "timestep": (1.0 / clip.fps) as f32, "additive": false, "animationWeight": 1.0,
            "leadIn": -1, "leadOut": -1, "lanes": lanes,
        }));
    }
    Ok(json!({
        "scenes": scenes.len() - skipped.len(),
        "lanes": lanes_written,
        "keyframes": keyframes_written,
        "partOpacityParameters": part_parameters.len(),
        "partOpacityLanes": part_lanes,
        "skippedScenes": skipped,
        "notConverted": {"partOpacityLanes": part_not_converted, "curvesOfUnknownParameters": unknown_parameters},
        "bezierHandles": "baked into frame-exact Linear keyframes",
    }))
}

/// A Forced Linear lane over `values`; `None` when the lane never leaves `default`. Only the frames where the
/// value changes (and the first and the last) are written.
fn lane(uuid: u32, values: &[f64], default: f64) -> Option<Value> {
    if values.iter().all(|value| value.is_nan() || (*value - default).abs() < KEY_EPSILON) {
        return None;
    }
    let keyframes: Vec<Value> = values
        .iter()
        .enumerate()
        .filter(|(frame, value)| {
            let differs = |other: Option<&f64>| other.is_some_and(|other| (*other - **value).abs() > KEY_EPSILON);
            !value.is_nan()
                && (*frame == 0
                    || *frame + 1 == values.len()
                    || differs(values.get(frame.wrapping_sub(1)))
                    || differs(values.get(frame + 1)))
        })
        .map(|(frame, value)| json!({"frame": frame, "value": value, "tension": 0.5}))
        .collect();
    Some(json!({"uuid": uuid, "target": 0, "interpolation": "Linear", "merge_mode": "Forced", "keyframes": keyframes}))
}

/// A one-channel Linear binding with one value per axis point.
fn value_binding(node: u32, channel: &str, values: &[f64]) -> Value {
    json!({
        "node": node, "param_name": channel, "interpolate_mode": "Linear",
        "values": values.iter().map(|value| json!([value])).collect::<Vec<_>>(),
        "isSet": vec![json!([true]); values.len()],
    })
}

/// Every binding of every parameter of `model`.
fn all_bindings(model: &Value) -> impl Iterator<Item = &Value> {
    model["param"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|parameter| parameter["bindings"].as_array().into_iter().flatten())
}

fn count_bindings(model: &Value, channel: &str) -> usize {
    all_bindings(model).filter(|binding| binding["param_name"] == channel).count()
}

/// True when some parameter already binds the opacity of `node`.
fn has_opacity_binding(model: &Value, node: u32) -> bool {
    all_bindings(model)
        .any(|binding| binding["param_name"] == "opacity" && binding["node"].as_u64() == Some(u64::from(node)))
}

/// Adds one 0..1 parameter per Part GUID of `parts` that fades at least one drawn node, and returns its UUID and
/// default by GUID. The parameter value is the Part opacity that the CAN3 player gives the Part: the player
/// replaces the Part's form opacity with it, so a node under the Part draws at `value * k`, where `k` is the node
/// opacity with the Part at 1. That is linear in the value, so the axis points are 0 and 1 and the opacity
/// binding values are `0` and a factor, exact at every value. The default is the Part's own form opacity at the
/// default pose, `o_default / k`.
///
/// Every pass keeps one invariant: the tree opacity times every opacity binding at the defaults is the node
/// opacity at the default pose (`o_default`). A node without an opacity binding gets the tree opacity `k` and the
/// factor 1; a node with one keeps its tree opacity and gets the factor `k / o_default`, which is 1 at the
/// parameter default. A Part is left out (so the caller counts its lanes as not converted) when no node it fades is
/// drawn at the default pose, or when a node with an opacity binding is drawn at opacity 0 there. The nodes are
/// decided before any is changed, and each parameter is added before the next Part is looked at, so the scan for
/// existing bindings sees the parameters of earlier (for instance ancestor) Parts.
fn part_opacity_parameters(
    model: &mut Value,
    scene: &NativeScene,
    converted: &Converted,
    parts: &BTreeSet<String>,
) -> Result<BTreeMap<String, (u32, f64)>, String> {
    let mut made = BTreeMap::new();
    if parts.is_empty() {
        return Ok(made);
    }
    let render = |overrides: &BTreeMap<String, f64>| -> Result<BTreeMap<u32, Posed>, String> {
        let (rendered, _) = scene.render_model_with_parts(&converted.defaults, overrides)?;
        posed_parts(&rendered["nodes"])
    };
    let rest = render(&BTreeMap::new())?;
    let mut next_id = converted.next_id;
    for guid in parts {
        let full = render(&BTreeMap::from([(guid.clone(), 1.0)]))?;
        let hidden = render(&BTreeMap::from([(guid.clone(), 0.0)]))?;
        // (node, opacity with the Part at 1, opacity at the default pose) of every node the Part fades.
        let mut affected = Vec::new();
        for (node, at_one) in &full {
            let (Some(at_rest), Some(at_zero)) = (rest.get(node), hidden.get(node)) else {
                return Err(format!("node {node} is drawn with a part opacity but not at the default pose"));
            };
            if at_one.opacity > at_zero.opacity + KEY_EPSILON {
                affected.push((*node, at_one.opacity, at_rest.opacity));
            }
        }
        let Some(&(_, first_full, first_rest)) = affected.first() else { continue };
        let part_rest = (first_rest / first_full).clamp(0.0, 1.0);
        let plan: Option<Vec<(u32, f64, Option<f64>)>> = affected
            .iter()
            .map(|&(node, at_one, at_rest)| {
                if has_opacity_binding(model, node) {
                    (at_rest > KEY_EPSILON).then(|| (node, at_one / at_rest, None))
                } else {
                    Some((node, 1.0, Some(at_one)))
                }
            })
            .collect();
        let Some(plan) = plan else { continue };
        let mut bindings = Vec::new();
        for (node, factor, rebase) in plan {
            if let Some(opacity) = rebase {
                crate::nodes::find_node_mut(model, node).ok_or("render node is missing")?["opacity"] = json!(opacity);
            }
            bindings.push(value_binding(node, "opacity", &[0.0, factor]));
        }
        let source = scene.source()["sources"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|source| source["guid"].as_str() == Some(guid.as_str()));
        let name = source
            .and_then(|source| {
                ["name", "id"].iter().find_map(|key| source[*key].as_str().filter(|name| !name.is_empty()))
            })
            .unwrap_or(guid);
        model["param"].as_array_mut().ok_or("INX parameters are not an array")?.push(json!({
            "uuid": next_id,
            "name": format!("{name} opacity"),
            "is_vec2": false,
            "min": [0.0, 0.0],
            "max": [1.0, 0.0],
            "defaults": [part_rest, 0.0],
            "axis_points": [[0.0, 1.0], [0.0]],
            "repeat": false,
            "merge_mode": "Additive",
            "bindings": bindings,
        }));
        made.insert(guid.clone(), (next_id, part_rest));
        next_id += 1;
    }
    Ok(made)
}

/// What the INX keeps and what it lost, so a partial conversion is never reported as a plain success.
fn report(
    sources: &[Value],
    model: &Value,
    parameters: usize,
    deform_bindings: usize,
    opacity_key_changes: usize,
    warnings: &[String],
) -> Result<Value, String> {
    let count = |kind: &str| sources.iter().filter(|source| source["data"]["type"] == kind).count();
    let on_several_parameters = sources
        .iter()
        .filter(|source| source["grid"]["axes"].as_array().is_some_and(|axes| axes.len() > 1))
        .count();
    let with_morph_targets = sources
        .iter()
        .filter(|source| source["morphTargets"].as_array().is_some_and(|targets| !targets.is_empty()))
        .count();
    let mut blend_modes: BTreeMap<String, usize> = BTreeMap::new();
    let mut nodes = 0usize;
    count_nodes(&model["nodes"], &mut nodes, &mut blend_modes);
    Ok(json!({
        "nodes": nodes,
        "parameters": parameters,
        "deformBindings": deform_bindings,
        "opacityBindings": count_bindings(model, "opacity"),
        "zSortBindings": count_bindings(model, "zSort"),
        "baked": {
            "warpDeformers": count("Warp"),
            "rotationDeformers": count("Rotation"),
            "glues": count("Glue"),
            "sourcesWithBlendShapes": with_morph_targets,
            "sourcesOnSeveralParameters": on_several_parameters,
        },
        "notConverted": {
            "opacityKeyChanges": opacity_key_changes,
            "physics": true,
            "blendShapeConstraints": with_morph_targets > 0,
            "can3Animations": true,
        },
        "blendModes": blend_modes,
        "warnings": warnings,
    }))
}

fn count_nodes(node: &Value, nodes: &mut usize, blend_modes: &mut BTreeMap<String, usize>) {
    *nodes += 1;
    if let Some(mode) = node["blend_mode"].as_str() {
        *blend_modes.entry(mode.to_owned()).or_default() += 1;
    }
    for child in node["children"].as_array().into_iter().flatten() {
        count_nodes(child, nodes, blend_modes);
    }
}

/// The keys of every parameter, from the keyform grids of every source (a deformer's keys move its meshes).
fn parameter_keys(sources: &[Value]) -> Result<BTreeMap<String, Vec<f64>>, String> {
    let mut keys: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for source in sources {
        for axis in source["grid"]["axes"].as_array().into_iter().flatten() {
            let id = axis["parameterId"].as_str().ok_or("a keyform grid axis names no parameter")?;
            let list = keys.entry(id.to_owned()).or_default();
            for key in axis["keys"].as_array().ok_or("a keyform grid axis has no keys")? {
                list.push(key.as_f64().filter(|key| key.is_finite()).ok_or("a keyform grid key is not finite")?);
            }
        }
    }
    Ok(keys)
}

/// Adds the fields `model_import` writes for every INX node and the render model leaves out.
fn complete_node(node: &mut Value, fallback_name: &str) -> Result<(), String> {
    let kind = node["type"].as_str().unwrap_or("").to_owned();
    let object = node.as_object_mut().ok_or("render node is not an object")?;
    if object.get("name").and_then(Value::as_str).is_none() {
        object.insert("name".into(), json!(fallback_name));
    }
    object.entry("lockToRoot").or_insert(json!(false));
    object.entry("pinToMesh").or_insert(json!(false));
    if kind == "Part" {
        object.entry("mask_threshold").or_insert(json!(0.5));
        object.entry("emissionStrength").or_insert(json!(1.0));
        // The renderer's copies of the mesh; the INX mesh carries them.
        for render_only in ["vertices", "uvs", "indices"] {
            object.remove(render_only);
        }
    }
    object.entry("children").or_insert(json!([]));
    for child in object["children"].as_array_mut().into_iter().flatten() {
        complete_node(child, "")?;
    }
    Ok(())
}

fn posed_parts(node: &Value) -> Result<BTreeMap<u32, Posed>, String> {
    fn visit(node: &Value, out: &mut BTreeMap<u32, Posed>) -> Result<(), String> {
        if node["type"] == "Part" || node["type"] == "Composite" {
            let id = node["uuid"].as_u64().and_then(crate::valid_id).ok_or("render node has no valid uuid")?;
            let vertices = if node["type"] == "Part" {
                node["mesh"]["verts"]
                    .as_array()
                    .ok_or("render Part has no mesh vertices")?
                    .iter()
                    .map(|value| value.as_f64().ok_or("render vertex is not a number".to_owned()))
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                Vec::new()
            };
            let posed = Posed {
                vertices,
                zsort: node["zsort"].as_f64().unwrap_or(0.0),
                opacity: node["opacity"].as_f64().unwrap_or(1.0),
            };
            if out.insert(id, posed).is_some() {
                return Err(format!("render node {id} is drawn twice"));
            }
        }
        for child in node["children"].as_array().into_iter().flatten() {
            visit(child, out)?;
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    visit(node, &mut out)?;
    Ok(out)
}

fn fresh_id(nodes: &Value) -> Result<u32, String> {
    fn visit(node: &Value, highest: &mut u32) {
        if let Some(id) = node["uuid"].as_u64().and_then(crate::valid_id) {
            *highest = (*highest).max(id);
        }
        for child in node["children"].as_array().into_iter().flatten() {
            visit(child, highest);
        }
    }
    let mut highest = 1;
    visit(nodes, &mut highest);
    highest.checked_add(1).ok_or_else(|| "no unused resource UUID remains".to_owned())
}

fn parameter_id(parameter: &Value) -> Result<String, String> {
    parameter["id"].as_str().map(str::to_owned).ok_or_else(|| "a native parameter has no id".to_owned())
}

fn number(parameter: &Value, field: &str) -> Result<f64, String> {
    parameter[field]
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| format!("native parameter {field} is missing"))
}
