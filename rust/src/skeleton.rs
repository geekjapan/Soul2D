//! Standard depth skeleton placement, matching `ngAddStandardDepthSkeleton`.
//!
//! Points are fit to the parent node's combined bounds. When those bounds are
//! missing or smaller than `1e-4` on either axis, the 360×600 fallback is used.
//! A scale of zero, including a value that underflows to float zero, is treated
//! as one. Placement reads the evaluated pose and writes bones into the
//! canonical model. A walk that cannot match D fails before any node is created.

use crate::nodes::{self, BoneRest};
use crate::valid_id;
use serde_json::{json, Value};

const FALLBACK_BOUNDS: [f64; 4] = [-180.0, -300.0, 180.0, 300.0];
const BOUNDS_EPSILON: f64 = 1e-4;

type Matrix = [[f64; 3]; 3];

#[derive(Clone, Copy)]
struct Pose {
    matrix: Matrix,
    z: f64,
    scale: [f64; 2],
}

const BOUNDS_CONTAINERS: &[&str] = &[
    "Node",
    "Root",
    "Composite",
    "MeshGroup",
    "DynamicComposite",
    "Mask",
    "PathDeformer",
    "GridDeformer",
    "DepthRigRoot",
    "DepthBone",
];

/// Narrows `scale` the way the D command does: the argument is a float, and a
/// float zero (including f64 values that underflow to f32 zero) becomes one.
pub(crate) fn normalize_scale(scale: f64) -> Result<f64, String> {
    if !scale.is_finite() {
        return Err("scale must be finite".into());
    }
    let narrowed = scale as f32;
    if !narrowed.is_finite() {
        return Err("scale is outside the f32 range".into());
    }
    if narrowed == 0.0 {
        Ok(1.0)
    } else {
        Ok(f64::from(narrowed))
    }
}

pub(crate) fn add_standard_skeleton(
    model: &Value,
    posed: &Value,
    root_id: u32,
    scale: f64,
) -> Result<Value, String> {
    let scale = normalize_scale(scale)?;
    let root = nodes::find_node(model, root_id)
        .ok_or_else(|| format!("node {root_id} was not found"))?;
    if root.get("type").and_then(Value::as_str) != Some("DepthRigRoot") {
        return Err("Node is not a DepthRigRoot".into());
    }
    // Bounds and the rig inverse follow the live pose. Bones are written into
    // the canonical model, whose stored transforms stay unchanged.
    let bounds = parent_bounds(posed, root_id)?;
    let rig_pose = world_pose(posed, root_id)?;
    let inverse = invert(rig_pose.matrix)?;
    add_skeleton_with_rests(model, root_id, |name| {
        let (_, _, head, tail) = STANDARD_BONES.iter().find(|row| row.0 == name).expect("a standard bone");
        let point = |ratio: &(f32, f32)| local_point(bounds, scale, inverse, rig_pose.z, ratio.0, ratio.1);
        (point(head), point(tail))
    })
}

/// The standard skeleton in creation order: bone, parent ("" for the rig root), then head and tail as ratios of the
/// bounds `add_standard_skeleton` fits it into.
const STANDARD_BONES: [(&str, &str, (f32, f32), (f32, f32)); 19] = [
    ("Pelvis", "", (0.0, 0.58), (0.0, 0.47)),
    ("Spine", "Pelvis", (0.0, 0.47), (0.0, 0.34)),
    ("Chest", "Spine", (0.0, 0.34), (0.0, 0.22)),
    ("Neck", "Chest", (0.0, 0.22), (0.0, 0.16)),
    ("Head", "Neck", (0.0, 0.16), (0.0, 0.06)),
    ("Clavicle.L", "Chest", (0.0, 0.25), (-0.14, 0.25)),
    ("UpperArm.L", "Clavicle.L", (-0.14, 0.25), (-0.26, 0.42)),
    ("Forearm.L", "UpperArm.L", (-0.26, 0.42), (-0.31, 0.58)),
    ("Hand.L", "Forearm.L", (-0.31, 0.58), (-0.33, 0.64)),
    ("Clavicle.R", "Chest", (0.0, 0.25), (0.14, 0.25)),
    ("UpperArm.R", "Clavicle.R", (0.14, 0.25), (0.26, 0.42)),
    ("Forearm.R", "UpperArm.R", (0.26, 0.42), (0.31, 0.58)),
    ("Hand.R", "Forearm.R", (0.31, 0.58), (0.33, 0.64)),
    ("Thigh.L", "Pelvis", (-0.08, 0.58), (-0.12, 0.76)),
    ("Shin.L", "Thigh.L", (-0.12, 0.76), (-0.12, 0.94)),
    ("Foot.L", "Shin.L", (-0.12, 0.94), (-0.18, 0.98)),
    ("Thigh.R", "Pelvis", (0.08, 0.58), (0.12, 0.76)),
    ("Shin.R", "Thigh.R", (0.12, 0.76), (0.12, 0.94)),
    ("Foot.R", "Shin.R", (0.12, 0.94), (0.18, 0.98)),
];

/// Adds the 19 standard bones below the DepthRigRoot `root_id`, each with the rest head and tail `rest` gives for
/// its name, in rig-local coordinates.
pub(crate) fn add_skeleton_with_rests(
    model: &Value,
    root_id: u32,
    rest: impl Fn(&'static str) -> ([f64; 3], [f64; 3]),
) -> Result<Value, String> {
    let mut candidate = model.clone();
    let mut ids = std::collections::HashMap::<&str, u32>::new();
    for (name, parent, ..) in STANDARD_BONES {
        let parent = if parent.is_empty() { root_id } else { ids[parent] };
        let (rest_head, rest_tail) = rest(name);
        place(&mut candidate, &mut ids, parent, name, rest_head, rest_tail)?;
    }
    set_bool(&mut candidate, ids["Head"], "allowParentToTargets", false)?;
    set_bool(&mut candidate, ids["Foot.L"], "lockToRoot", true)?;
    set_bool(&mut candidate, ids["Foot.R"], "lockToRoot", true)?;
    Ok(candidate)
}

pub(crate) const STANDARD_BONE_NAMES: &[&str] = &[
    "Pelvis",
    "Spine",
    "Chest",
    "Neck",
    "Head",
    "Clavicle.L",
    "UpperArm.L",
    "Forearm.L",
    "Hand.L",
    "Clavicle.R",
    "UpperArm.R",
    "Forearm.R",
    "Hand.R",
    "Thigh.L",
    "Shin.L",
    "Foot.L",
    "Thigh.R",
    "Shin.R",
    "Foot.R",
];

pub(crate) fn fit_human(
    model: &Value,
    posed: &Value,
    root_id: u32,
    values: &std::collections::BTreeMap<u32, [f64; 2]>,
) -> Result<Value, String> {
    let root = nodes::find_node(model, root_id)
        .ok_or_else(|| format!("node {root_id} was not found"))?;
    if root.get("type").and_then(Value::as_str) != Some("DepthRigRoot") {
        return Err("Node is not a DepthRigRoot".into());
    }
    let bones = bones_by_name(model, root_id)?;
    for name in STANDARD_BONE_NAMES {
        if !bones.contains_key(*name) {
            return Err(format!("rig is missing bone {name}"));
        }
    }
    let mut candidate = model.clone();
    let mut placed = std::collections::BTreeMap::<u32, [f64; 3]>::new();
    for bone_id in crate::bone_deform::bone_order(model, root_id)? {
        let node = nodes::find_node(model, bone_id)
            .ok_or_else(|| format!("node {bone_id} was not found"))?;
        let name = bone_name(node);
        if !STANDARD_BONE_NAMES.contains(&name.as_str()) {
            continue;
        }
        let previous_head = vec3_or(node.get("restHead"), [0.0, 0.0, 0.0]);
        let previous_tail = vec3_or(node.get("restTail"), [0.0, 100.0, 0.0]);
        let (head, tail) = match named_bounds(posed, root_id, &name)? {
            Some(bounds) => segment_from_bounds(bounds, previous_head, previous_tail)?,
            None => (previous_head, previous_tail),
        };
        let parent = parent_of(model, bone_id)?;
        let parent_head = parent_rest_head(model, parent, &placed);
        let local = [
            head[0] - parent_head[0],
            head[1] - parent_head[1],
            head[2] - parent_head[2],
        ];
        let stored = nodes::find_node_mut(&mut candidate, bone_id)
            .ok_or_else(|| format!("node {bone_id} was not found"))?;
        let object = stored.as_object_mut().ok_or("node must be an object")?;
        object.insert("restHead".into(), json!(head));
        object.insert("restTail".into(), json!(tail));
        object
            .get_mut("transform")
            .and_then(Value::as_object_mut)
            .ok_or("node transform must be an object")?
            .insert("trans".into(), json!(local));
        placed.insert(bone_id, head);
    }
    let weight_pose = pose_for_weights(&candidate, values)?;
    let targets = binding_target_ids(&candidate, root_id);
    for target_id in targets {
        let rows = crate::bone_deform::gaussian_vertex_weights(
            &candidate,
            &weight_pose,
            root_id,
            target_id,
        )?;
        let weights = Value::Array(
            rows.into_iter()
                .map(|(bone, values)| json!({ "bone": bone, "values": values }))
                .collect(),
        );
        candidate = crate::bindings::set_vertex_weights(&candidate, root_id, target_id, &weights)?;
    }
    Ok(candidate)
}

fn parent_rest_head(
    model: &Value,
    parent: Option<u32>,
    placed: &std::collections::BTreeMap<u32, [f64; 3]>,
) -> [f64; 3] {
    let Some(parent_id) = parent else {
        return [0.0, 0.0, 0.0];
    };
    if let Some(head) = placed.get(&parent_id) {
        return *head;
    }
    let Some(node) = nodes::find_node(model, parent_id) else {
        return [0.0, 0.0, 0.0];
    };
    if node.get("type").and_then(Value::as_str) != Some("DepthBone") {
        return [0.0, 0.0, 0.0];
    }
    vec3_or(node.get("restHead"), [0.0, 0.0, 0.0])
}

fn pose_for_weights(
    model: &Value,
    values: &std::collections::BTreeMap<u32, [f64; 2]>,
) -> Result<Value, String> {
    let has_parameters = model
        .get("param")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty());
    if !has_parameters && values.is_empty() {
        return Ok(model.clone());
    }
    crate::evaluation::evaluate(model, values)
}

pub(crate) fn bones_by_name(model: &Value, root_id: u32) -> Result<std::collections::BTreeMap<String, u32>, String> {
    let mut names = std::collections::BTreeMap::new();
    for bone_id in crate::bone_deform::bone_order(model, root_id)? {
        let node = nodes::find_node(model, bone_id)
            .ok_or_else(|| format!("node {bone_id} was not found"))?;
        names.insert(bone_name(node), bone_id);
    }
    Ok(names)
}

fn bone_name(node: &Value) -> String {
    node.get("boneId")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .or_else(|| node.get("name").and_then(Value::as_str))
        .unwrap_or("")
        .to_owned()
}

fn named_bounds(model: &Value, root_id: u32, name: &str) -> Result<Option<[f64; 4]>, String> {
    let mut matched = Vec::new();
    if let Some(root) = model.get("nodes") {
        collect_named_geometry(root, name, &mut matched);
    }
    if matched.is_empty() {
        return Ok(None);
    }
    let mut combined: Option<[f64; 4]> = None;
    for node_id in matched {
        let bounds = rig_local_bounds(model, root_id, node_id)?;
        combined = Some(match combined {
            None => bounds,
            Some(current) => [
                current[0].min(bounds[0]),
                current[1].min(bounds[1]),
                current[2].max(bounds[2]),
                current[3].max(bounds[3]),
            ],
        });
    }
    Ok(combined)
}

fn collect_named_geometry(node: &Value, name: &str, matched: &mut Vec<u32>) {
    let kind = node.get("type").and_then(Value::as_str).unwrap_or("");
    let node_name = node.get("name").and_then(Value::as_str).unwrap_or("");
    if (kind == "Part" || kind == "GridDeformer") && node_name == name {
        if let Some(id) = node.get("uuid").and_then(Value::as_u64).and_then(valid_id) {
            matched.push(id);
        }
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            collect_named_geometry(child, name, matched);
        }
    }
}

fn rig_local_bounds(model: &Value, root_id: u32, node_id: u32) -> Result<[f64; 4], String> {
    let node = nodes::find_node(model, node_id)
        .ok_or_else(|| format!("node {node_id} was not found"))?;
    let kind = node.get("type").and_then(Value::as_str).unwrap_or("");
    let points = if kind == "Part" {
        let vertices = part_vertices(node).map_err(|_| "non-finite bounds".to_string())?;
        let deformation = part_deformation(node, vertices.len())
            .map_err(|_| "non-finite bounds".to_string())?;
        vertices
            .into_iter()
            .zip(deformation)
            .map(|(vertex, offset)| [vertex[0] + offset[0], vertex[1] + offset[1]])
            .collect::<Vec<_>>()
    } else {
        crate::bone_deform::target_vertices(node)?
    };
    let world = crate::bone_deform::world_matrix(model, node_id)?;
    let rig = crate::bone_deform::invert(crate::bone_deform::world_matrix(model, root_id)?)?;
    let mut bounds = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    let samples = if points.is_empty() {
        vec![[0.0, 0.0]]
    } else {
        points
    };
    for point in samples {
        let placed = crate::bone_deform::transform_point(world, [point[0], point[1], 0.0])?;
        let local = crate::bone_deform::transform_point(rig, placed)?;
        if !local[0].is_finite() || !local[1].is_finite() {
            return Err("non-finite bounds".into());
        }
        bounds[0] = bounds[0].min(local[0]);
        bounds[1] = bounds[1].min(local[1]);
        bounds[2] = bounds[2].max(local[0]);
        bounds[3] = bounds[3].max(local[1]);
    }
    if bounds.iter().any(|value| !value.is_finite()) {
        return Err("non-finite bounds".into());
    }
    Ok(bounds)
}

fn segment_from_bounds(
    bounds: [f64; 4],
    rest_head: [f64; 3],
    rest_tail: [f64; 3],
) -> Result<([f64; 3], [f64; 3]), String> {
    let mut direction = [rest_tail[0] - rest_head[0], rest_tail[1] - rest_head[1], 0.0];
    let length = (direction[0] * direction[0] + direction[1] * direction[1]).sqrt();
    if length > 1e-4 {
        direction[0] /= length;
        direction[1] /= length;
    } else {
        direction = [0.0, 1.0, 0.0];
    }
    let center = [(bounds[0] + bounds[2]) * 0.5, (bounds[1] + bounds[3]) * 0.5];
    let corners = [
        [bounds[0], bounds[1]],
        [bounds[2], bounds[1]],
        [bounds[0], bounds[3]],
        [bounds[2], bounds[3]],
    ];
    let mut min_projection = f64::INFINITY;
    let mut max_projection = f64::NEG_INFINITY;
    for corner in corners {
        let projection = (corner[0] - center[0]) * direction[0] + (corner[1] - center[1]) * direction[1];
        min_projection = min_projection.min(projection);
        max_projection = max_projection.max(projection);
    }
    let head = [
        center[0] + direction[0] * min_projection,
        center[1] + direction[1] * min_projection,
        rest_head[2],
    ];
    let tail = [
        center[0] + direction[0] * max_projection,
        center[1] + direction[1] * max_projection,
        rest_tail[2],
    ];
    if head.iter().chain(tail.iter()).any(|value| !value.is_finite()) {
        return Err("non-finite bounds".into());
    }
    Ok((head, tail))
}

fn binding_target_ids(model: &Value, root_id: u32) -> Vec<u32> {
    let Some(root) = nodes::find_node(model, root_id) else {
        return Vec::new();
    };
    let Some(bindings) = root.get("bindings").and_then(Value::as_array) else {
        return Vec::new();
    };
    bindings
        .iter()
        .filter_map(|binding| binding.get("target").and_then(Value::as_u64).and_then(valid_id))
        .collect()
}

fn vec3_or(value: Option<&Value>, fallback: [f64; 3]) -> [f64; 3] {
    let Some(value) = value.and_then(Value::as_array) else {
        return fallback;
    };
    let mut output = fallback;
    for (index, component) in value.iter().take(3).enumerate() {
        if let Some(number) = component.as_f64().filter(|number| number.is_finite()) {
            output[index] = number;
        }
    }
    output
}

fn place(
    model: &mut Value,
    ids: &mut std::collections::HashMap<&str, u32>,
    parent: u32,
    name: &'static str,
    rest_head: [f64; 3],
    rest_tail: [f64; 3],
) -> Result<(), String> {
    let rest = BoneRest {
        bone_id: Some(name.to_owned()),
        rest_head,
        rest_tail,
        rest_roll: 0.0,
    };
    let (next, created) =
        nodes::create_duplicate_named_nodes(model, &[parent], "DepthBone", None, None, Some(&rest))?;
    let id = created
        .first()
        .and_then(|node| node.get("uuid"))
        .and_then(Value::as_u64)
        .and_then(valid_id)
        .ok_or_else(|| format!("created bone {name} has no uuid"))?;
    *model = next;
    ids.insert(name, id);
    Ok(())
}

pub(crate) fn list_bones(model: &Value, root_id: u32) -> Result<Value, String> {
    let root = nodes::find_node(model, root_id)
        .ok_or_else(|| format!("node {root_id} was not found"))?;
    if root.get("type").and_then(Value::as_str) != Some("DepthRigRoot") {
        return Err("Node is not a DepthRigRoot".into());
    }
    let mut items = Vec::new();
    if let Some(children) = root.get("children").and_then(Value::as_array) {
        for child in children {
            collect_bones(child, root_id, &mut items)?;
        }
    }
    Ok(json!({ "items": items }))
}

fn collect_bones(node: &Value, parent: u32, items: &mut Vec<Value>) -> Result<(), String> {
    let id = node
        .get("uuid")
        .and_then(Value::as_u64)
        .and_then(valid_id)
        .ok_or("node.uuid must be an unsigned integer")?;
    if node.get("type").and_then(Value::as_str) == Some("DepthBone") {
        items.push(json!({
            "uuid": id,
            "name": node.get("name").cloned().unwrap_or(Value::Null),
            "boneId": node.get("boneId").cloned().unwrap_or(Value::Null),
            "restHead": node.get("restHead").cloned().unwrap_or(Value::Null),
            "restTail": node.get("restTail").cloned().unwrap_or(Value::Null),
            "restRoll": node.get("restRoll").cloned().unwrap_or(json!(0.0)),
            "allowParentToTargets": node.get("allowParentToTargets").cloned().unwrap_or(json!(true)),
            "parent": parent,
        }));
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            collect_bones(child, id, items)?;
        }
    }
    Ok(())
}

fn local_point(
    bounds: [f64; 4],
    scale: f64,
    inverse: Matrix,
    rig_z: f64,
    x_ratio: f32,
    y_ratio: f32,
) -> [f64; 3] {
    let bounds = bounds.map(|value| value as f32);
    let scale = scale as f32;
    let width = (bounds[2] - bounds[0]) * scale;
    let center_x = (bounds[0] + bounds[2]) * 0.5;
    let top = bounds[1];
    let world_x = center_x + x_ratio * width;
    let world_y = top + y_ratio * (bounds[3] - bounds[1]) * scale;
    let local = apply(inverse, [f64::from(world_x), f64::from(world_y), 1.0]);
    [local[0], local[1], -rig_z]
}

fn parent_bounds(model: &Value, root_id: u32) -> Result<[f64; 4], String> {
    let Some(parent_id) = parent_of(model, root_id)? else {
        return Ok(FALLBACK_BOUNDS);
    };
    let parent = nodes::find_node(model, parent_id)
        .ok_or_else(|| format!("node {parent_id} was not found"))?;
    let pose = world_pose(model, parent_id)?;
    let combined = combined_bounds(parent, pose)?;
    let size_x = combined[2] - combined[0];
    let size_y = combined[3] - combined[1];
    if size_x > BOUNDS_EPSILON && size_y > BOUNDS_EPSILON {
        Ok(combined)
    } else {
        Ok(FALLBACK_BOUNDS)
    }
}

fn bounds_mismatch() -> String {
    "cannot match D parent bounds for the standard skeleton".into()
}

fn combined_bounds(node: &Value, world: Pose) -> Result<[f64; 4], String> {
    let kind = node.get("type").and_then(Value::as_str).unwrap_or("");
    if kind != "Part" && kind != "Camera" && !BOUNDS_CONTAINERS.contains(&kind) {
        return Err(bounds_mismatch());
    }
    let children = node.get("children").and_then(Value::as_array);
    let child_count = children.map(|items| items.len()).unwrap_or(0);
    let mut has = false;
    let mut combined = [0.0; 4];
    if kind == "Part" {
        let vertices = part_vertices(node)?;
        let deformation = part_deformation(node, vertices.len())?;
        if !vertices.is_empty() || child_count == 0 {
            combined = if vertices.is_empty() {
                translation_bounds(world)
            } else {
                vertex_bounds(world, &vertices, &deformation)?
            };
            has = true;
        }
    } else if child_count == 0 {
        combined = if kind == "Camera" {
            camera_bounds(node, world)?
        } else {
            translation_bounds(world)
        };
        has = true;
    }
    if let Some(children) = children {
        for child in children {
            let child_world = compose(world, local_pose(child)?)?;
            let child_bounds = combined_bounds(child, child_world)?;
            if !has {
                combined = child_bounds;
                has = true;
            } else {
                combined[0] = combined[0].min(child_bounds[0]);
                combined[1] = combined[1].min(child_bounds[1]);
                combined[2] = combined[2].max(child_bounds[2]);
                combined[3] = combined[3].max(child_bounds[3]);
            }
        }
    }
    if !has || combined.iter().any(|value| !value.is_finite()) {
        return Err(bounds_mismatch());
    }
    Ok(combined)
}

fn part_vertices(node: &Value) -> Result<Vec<[f64; 2]>, String> {
    let Some(mesh) = node.get("mesh") else {
        return Ok(Vec::new());
    };
    let Some(raw) = mesh.get("verts").and_then(Value::as_array) else {
        return Err("cannot match D parent bounds for the standard skeleton".into());
    };
    if raw.len() % 2 != 0 {
        return Err("cannot match D parent bounds for the standard skeleton".into());
    }
    let mut vertices = Vec::with_capacity(raw.len() / 2);
    for pair in raw.chunks(2) {
        let x = pair[0]
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or("cannot match D parent bounds for the standard skeleton")?;
        let y = pair[1]
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or("cannot match D parent bounds for the standard skeleton")?;
        vertices.push([x, y]);
    }
    Ok(vertices)
}

fn part_deformation(node: &Value, vertex_count: usize) -> Result<Vec<[f64; 2]>, String> {
    let Some(raw) = node.get("deformation").and_then(Value::as_array) else {
        return Ok(vec![[0.0, 0.0]; vertex_count]);
    };
    if raw.is_empty() {
        return Ok(vec![[0.0, 0.0]; vertex_count]);
    }
    let mut offsets = Vec::with_capacity(vertex_count);
    if raw.first().is_some_and(Value::is_array) {
        if raw.len() != vertex_count {
            return Err(bounds_mismatch());
        }
        for entry in raw {
            let pair = entry.as_array().ok_or_else(bounds_mismatch)?;
            if pair.len() != 2 {
                return Err(bounds_mismatch());
            }
            offsets.push([
                pair[0].as_f64().filter(|value| value.is_finite()).ok_or_else(bounds_mismatch)?,
                pair[1].as_f64().filter(|value| value.is_finite()).ok_or_else(bounds_mismatch)?,
            ]);
        }
    } else {
        if raw.len() != vertex_count * 2 {
            return Err(bounds_mismatch());
        }
        for pair in raw.chunks(2) {
            offsets.push([
                pair[0].as_f64().filter(|value| value.is_finite()).ok_or_else(bounds_mismatch)?,
                pair[1].as_f64().filter(|value| value.is_finite()).ok_or_else(bounds_mismatch)?,
            ]);
        }
    }
    Ok(offsets)
}

fn vertex_bounds(world: Pose, vertices: &[[f64; 2]], deformation: &[[f64; 2]]) -> Result<[f64; 4], String> {
    let mut bounds = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for (vertex, offset) in vertices.iter().zip(deformation.iter()) {
        let point = apply(world.matrix, [vertex[0] + offset[0], vertex[1] + offset[1], 1.0]);
        if !point[0].is_finite() || !point[1].is_finite() {
            return Err(bounds_mismatch());
        }
        bounds[0] = bounds[0].min(point[0]);
        bounds[1] = bounds[1].min(point[1]);
        bounds[2] = bounds[2].max(point[0]);
        bounds[3] = bounds[3].max(point[1]);
    }
    Ok(bounds)
}

fn camera_bounds(node: &Value, world: Pose) -> Result<[f64; 4], String> {
    let viewport = node
        .get("viewport")
        .and_then(Value::as_array)
        .ok_or_else(bounds_mismatch)?;
    if viewport.len() != 2 {
        return Err(bounds_mismatch());
    }
    let width = viewport[0].as_f64().filter(|value| value.is_finite()).ok_or_else(bounds_mismatch)?;
    let height = viewport[1].as_f64().filter(|value| value.is_finite()).ok_or_else(bounds_mismatch)?;
    let origin = apply(world.matrix, [0.0, 0.0, 1.0]);
    let half_x = width * 0.5 * world.scale[0];
    let half_y = height * 0.5 * world.scale[1];
    Ok([origin[0] - half_x, origin[1] - half_y, origin[0] + half_x, origin[1] + half_y])
}

fn translation_bounds(world: Pose) -> [f64; 4] {
    let origin = apply(world.matrix, [0.0, 0.0, 1.0]);
    [origin[0], origin[1], origin[0], origin[1]]
}

fn world_pose(model: &Value, id: u32) -> Result<Pose, String> {
    let mut chain = Vec::new();
    let mut current = Some(id);
    while let Some(node_id) = current {
        chain.push(node_id);
        current = parent_of(model, node_id)?;
    }
    chain.reverse();
    let mut pose = Pose {
        matrix: identity(),
        z: 0.0,
        scale: [1.0, 1.0],
    };
    for node_id in chain {
        let node = nodes::find_node(model, node_id)
            .ok_or_else(|| format!("node {node_id} was not found"))?;
        pose = compose(pose, local_pose(node)?)?;
    }
    Ok(pose)
}

fn local_pose(node: &Value) -> Result<Pose, String> {
    if node.get("lockToRoot").and_then(Value::as_bool).unwrap_or(false) {
        return Err(bounds_mismatch());
    }
    let transform = node.get("transform");
    let translation = vec3(transform.and_then(|value| value.get("trans")))?;
    let rotation = vec3(transform.and_then(|value| value.get("rot")))?;
    let scale = vec2(transform.and_then(|value| value.get("scale")))?;
    if rotation[0].abs() > 1e-12 || rotation[1].abs() > 1e-12 {
        return Err(bounds_mismatch());
    }
    let (sin, cos) = rotation[2].sin_cos();
    Ok(Pose {
        matrix: [
            [cos * scale[0], -sin * scale[1], translation[0]],
            [sin * scale[0], cos * scale[1], translation[1]],
            [0.0, 0.0, 1.0],
        ],
        z: translation[2],
        scale,
    })
}

fn parent_of(model: &Value, id: u32) -> Result<Option<u32>, String> {
    fn walk(node: &Value, id: u32, parent: Option<u32>) -> Result<Option<Option<u32>>, String> {
        let node_id = node
            .get("uuid")
            .and_then(Value::as_u64)
            .and_then(valid_id)
            .ok_or("node.uuid must be an unsigned integer")?;
        if node_id == id {
            return Ok(Some(parent));
        }
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            for child in children {
                if let Some(found) = walk(child, id, Some(node_id))? {
                    return Ok(Some(found));
                }
            }
        }
        Ok(None)
    }
    let root = model.get("nodes").ok_or("model is missing nodes")?;
    walk(root, id, None)?.ok_or_else(|| format!("node {id} was not found"))
}

fn set_bool(model: &mut Value, id: u32, key: &str, value: bool) -> Result<(), String> {
    let node = nodes::find_node_mut(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    node.as_object_mut()
        .ok_or("node must be an object")?
        .insert(key.to_owned(), json!(value));
    Ok(())
}

fn vec3(value: Option<&Value>) -> Result<[f64; 3], String> {
    let Some(value) = value else {
        return Ok([0.0, 0.0, 0.0]);
    };
    let array = value
        .as_array()
        .ok_or("cannot match D parent bounds for the standard skeleton")?;
    let mut output = [0.0, 0.0, 0.0];
    if array.len() > 3 {
        return Err("cannot match D parent bounds for the standard skeleton".into());
    }
    for (index, component) in array.iter().enumerate() {
        output[index] = component
            .as_f64()
            .filter(|number| number.is_finite())
            .ok_or("cannot match D parent bounds for the standard skeleton")?;
    }
    Ok(output)
}

fn vec2(value: Option<&Value>) -> Result<[f64; 2], String> {
    let Some(value) = value else {
        return Ok([1.0, 1.0]);
    };
    let array = value
        .as_array()
        .ok_or("cannot match D parent bounds for the standard skeleton")?;
    if array.is_empty() || array.len() > 2 {
        return Err("cannot match D parent bounds for the standard skeleton".into());
    }
    let mut output = [1.0, 1.0];
    for (index, component) in array.iter().enumerate() {
        output[index] = component
            .as_f64()
            .filter(|number| number.is_finite())
            .ok_or("cannot match D parent bounds for the standard skeleton")?;
    }
    Ok(output)
}

fn compose(parent: Pose, local: Pose) -> Result<Pose, String> {
    Ok(Pose {
        matrix: multiply(parent.matrix, local.matrix),
        z: parent.z + local.z,
        scale: [parent.scale[0] * local.scale[0], parent.scale[1] * local.scale[1]],
    })
}

fn identity() -> Matrix {
    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
}

fn multiply(a: Matrix, b: Matrix) -> Matrix {
    let mut result = [[0.0; 3]; 3];
    for row in 0..3 {
        for column in 0..3 {
            for k in 0..3 {
                result[row][column] += a[row][k] * b[k][column];
            }
        }
    }
    result
}

fn apply(matrix: Matrix, vector: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|row| {
        matrix[row][0] * vector[0] + matrix[row][1] * vector[1] + matrix[row][2] * vector[2]
    })
}

fn invert(matrix: Matrix) -> Result<Matrix, String> {
    let det = matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
        - matrix[0][1] * (matrix[1][0] * matrix[2][2] - matrix[1][2] * matrix[2][0])
        + matrix[0][2] * (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]);
    if !det.is_finite() || det.abs() <= f64::EPSILON {
        return Err("cannot match D parent bounds for the standard skeleton".into());
    }
    let inv_det = 1.0 / det;
    Ok([
        [
            (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1]) * inv_det,
            (matrix[0][2] * matrix[2][1] - matrix[0][1] * matrix[2][2]) * inv_det,
            (matrix[0][1] * matrix[1][2] - matrix[0][2] * matrix[1][1]) * inv_det,
        ],
        [
            (matrix[1][2] * matrix[2][0] - matrix[1][0] * matrix[2][2]) * inv_det,
            (matrix[0][0] * matrix[2][2] - matrix[0][2] * matrix[2][0]) * inv_det,
            (matrix[0][2] * matrix[1][0] - matrix[0][0] * matrix[1][2]) * inv_det,
        ],
        [
            (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]) * inv_det,
            (matrix[0][1] * matrix[2][0] - matrix[0][0] * matrix[2][1]) * inv_det,
            (matrix[0][0] * matrix[1][1] - matrix[0][1] * matrix[1][0]) * inv_det,
        ],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_follows_the_d_float_argument() {
        assert_eq!(normalize_scale(0.0).unwrap(), 1.0);
        assert_eq!(normalize_scale(1e-50).unwrap(), 1.0);
        assert_eq!(normalize_scale(-1e-50).unwrap(), 1.0);
        assert!((normalize_scale(2.0).unwrap() - 2.0).abs() < 1e-6);
        assert!(normalize_scale(1e39).unwrap_err().contains("f32"));
        assert!(normalize_scale(f64::NAN).is_err());
    }

    #[test]
    fn fallback_pelvis_matches_the_d_ratio() {
        let head_y = f64::from(-300.0f32 + 0.58f32 * 600.0f32);
        let tail_y = f64::from(-300.0f32 + 0.47f32 * 600.0f32);
        let point = local_point(FALLBACK_BOUNDS, 1.0, identity(), 0.0, 0.0, 0.58);
        assert!((point[0]).abs() < 1e-4);
        assert!((point[1] - head_y).abs() < 1e-4, "{}", point[1]);
        assert!(point[2].abs() < 1e-6);
        let tail = local_point(FALLBACK_BOUNDS, 1.0, identity(), 0.0, 0.0, 0.47);
        assert!((tail[1] - tail_y).abs() < 1e-4);
    }

    #[test]
    fn fit_human_places_named_geometry_and_seeds_gaussian_weights() {
        let mut session = crate::Session::new();
        let rig = ok(&session.handle_request(&json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({"op":"rig.add_skeleton","root":rig_id})));
        let listed = ok(&session.handle_request(&json!({"op":"rig.list_bones","root":rig_id})));
        let head_id = listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|bone| bone["boneId"] == "Head")
            .unwrap()["uuid"]
            .as_u64()
            .unwrap();
        let pelvis_before = listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|bone| bone["boneId"] == "Pelvis")
            .unwrap()
            .clone();
        ok(&session.handle_request(&json!({"op":"node.rename","id":head_id,"name":"Head Bone"})));
        let grid = ok(&session.handle_request(&json!({"op":"node.create","className":"GridDeformer","name":"Grid"})));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({"op":"node.rename","id":grid_id,"name":"Head"})));
        ok(&session.handle_request(&json!({
            "op":"deformer.define_grid","nodes":[grid_id],"axisX":[10.0,30.0],"axisY":[20.0,40.0]
        })));
        ok(&session.handle_request(&json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":head_id})));
        let before = ok(&session.handle_request(&json!({"op":"snapshot"})));
        ok(&session.handle_request(&json!({"op":"rig.fit_human","root":rig_id})));
        let fitted = bone_rest(&mut session, rig_id, "Head");
        assert!((fitted.0[0] - 20.0).abs() < 1e-4, "{fitted:?}");
        assert!((fitted.1[0] - 20.0).abs() < 1e-4, "{fitted:?}");
        let mut ys = [fitted.0[1], fitted.1[1]];
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!((ys[0] - 20.0).abs() < 1e-4 && (ys[1] - 40.0).abs() < 1e-4, "{fitted:?}");
        let pelvis = bone_rest(&mut session, rig_id, "Pelvis");
        assert_eq!(json!(pelvis.0), pelvis_before["restHead"]);
        assert_eq!(json!(pelvis.1), pelvis_before["restTail"]);
        let weights = stored_weights(&mut session, rig_id, grid_id);
        assert_eq!(weights.as_array().unwrap().len(), 1);
        assert_eq!(weights[0], json!([1.0, 1.0, 1.0, 1.0]));
        let path = std::env::temp_dir().join(format!("vsoul-fit-human-{}-{}.inx", std::process::id(), rig_id));
        let _ = std::fs::remove_file(&path);
        ok(&session.handle_request(&json!({"op":"save","path":path.to_string_lossy(),"mark_saved":true})));
        let mut reloaded = crate::Session::new();
        ok(&reloaded.handle_request(&json!({"op":"open","path":path.to_string_lossy()})));
        let _ = std::fs::remove_file(&path);
        assert_eq!(stored_weights(&mut reloaded, rig_id, grid_id), weights);
        let revision = ok(&session.handle_request(&json!({"op":"snapshot"})))["revision"].clone();
        ok(&session.handle_request(&json!({"op":"rig.fit_human","root":rig_id})));
        let again = ok(&session.handle_request(&json!({"op":"snapshot"})));
        assert_eq!(again["revision"], revision);
        assert_eq!(again["canUndo"], before["canUndo"]);
        ok(&session.handle_request(&json!({"op":"undo"})));
        let restored = ok(&session.handle_request(&json!({"op":"snapshot"})));
        assert_eq!(restored["model"], before["model"]);
        assert!(stored_weights(&mut session, rig_id, grid_id).is_null());

        let mut bare = crate::Session::new();
        let bare_rig = ok(&bare.handle_request(&json!({"op":"node.create","className":"DepthRigRoot","name":"Bare"})));
        let bare_id = bare_rig["created"][0]["uuid"].as_u64().unwrap();
        ok(&bare.handle_request(&json!({"op":"rig.add_skeleton","root":bare_id})));
        let bare_bones = ok(&bare.handle_request(&json!({"op":"rig.list_bones","root":bare_id})));
        let bare_head = bare_bones["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|bone| bone["boneId"] == "Head")
            .unwrap()["uuid"]
            .as_u64()
            .unwrap();
        ok(&bare.handle_request(&json!({"op":"node.delete","nodes":[bare_head]})));
        let held = ok(&bare.handle_request(&json!({"op":"snapshot"})));
        let failed = bare.handle_request(&json!({"op":"rig.fit_human","root":bare_id}));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(failed["message"].as_str().unwrap().contains("Head"), "{failed}");
        assert_eq!(ok(&bare.handle_request(&json!({"op":"snapshot"}))), held);
    }

    #[test]
    fn non_finite_named_part_bounds_fail() {
        let model = json!({
            "nodes": {
                "uuid": 1,
                "type": "Root",
                "name": "Root",
                "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]},
                "children": [{
                    "uuid": 2,
                    "type": "DepthRigRoot",
                    "name": "Rig",
                    "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]},
                    "children": [{
                        "uuid": 3,
                        "type": "Part",
                        "name": "Head",
                        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]},
                        "mesh": {"verts": [0.0, null]}
                    }]
                }]
            }
        });
        let error = rig_local_bounds(&model, 2, 3).unwrap_err();
        assert!(error.contains("non-finite"), "{error}");
    }

    #[test]
    fn fit_human_keeps_custom_bone_pose() {
        let mut session = crate::Session::new();
        let rig = ok(&session.handle_request(&json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({"op":"rig.add_skeleton","root":rig_id})));
        let pelvis = bone_id(&mut session, rig_id, "Pelvis");
        let extra = ok(&session.handle_request(&json!({
            "op":"node.create","className":"DepthBone","name":"Prop","parents":[pelvis],"boneId":"Prop",
            "restHead":[4.0,5.0,0.0],"restTail":[4.0,15.0,0.0]
        })));
        let extra_id = extra["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"node.transform","id":extra_id,"translation":[40.0,50.0,3.0]
        })));
        let before = ok(&session.handle_request(&json!({"op":"node.get","id":extra_id})));
        ok(&session.handle_request(&json!({"op":"rig.fit_human","root":rig_id})));
        let after = ok(&session.handle_request(&json!({"op":"node.get","id":extra_id})));
        assert_eq!(after["restHead"], before["restHead"]);
        assert_eq!(after["restTail"], before["restTail"]);
        assert_eq!(after["transform"], before["transform"]);
    }

    #[test]
    fn fit_human_seeds_weights_from_the_evaluated_pose() {
        let mut session = crate::Session::new();
        let rig = ok(&session.handle_request(&json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({"op":"rig.add_skeleton","root":rig_id})));
        let head = bone_id(&mut session, rig_id, "Head");
        let chest = bone_id(&mut session, rig_id, "Chest");
        ok(&session.handle_request(&json!({"op":"node.rename","id":head,"name":"Head Bone"})));
        let named = ok(&session.handle_request(&json!({"op":"node.create","className":"GridDeformer","name":"Head"})));
        let named_id = named["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"deformer.define_grid","nodes":[named_id],"axisX":[10.0,30.0],"axisY":[20.0,40.0]
        })));
        let mesh = ok(&session.handle_request(&json!({"op":"node.create","className":"GridDeformer","name":"Mesh"})));
        let mesh_id = mesh["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"deformer.define_grid","nodes":[mesh_id],"axisX":[0.0,20.0],"axisY":[0.0,20.0]
        })));
        ok(&session.handle_request(&json!({"op":"rig.add_source","root":rig_id,"target":mesh_id,"bone":head})));
        ok(&session.handle_request(&json!({"op":"rig.add_source","root":rig_id,"target":mesh_id,"bone":chest})));
        let parameter = ok(&session.handle_request(&json!({"op":"parameter.create","dimensions":1,"name":"Slide"})));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"binding.set_trs","parameter":parameter_id,"nodes":[mesh_id],"key":[2,0],
            "translation":[300.0,0.0]
        })));
        ok(&session.handle_request(&json!({"op":"rig.fit_human","root":rig_id})));
        let at_rest = stored_weights(&mut session, rig_id, mesh_id);
        ok(&session.handle_request(&json!({"op":"undo"})));
        ok(&session.handle_request(&json!({"op":"value.set","id":parameter_id,"value":[1.0,0.0]})));
        ok(&session.handle_request(&json!({"op":"rig.fit_human","root":rig_id})));
        let posed = stored_weights(&mut session, rig_id, mesh_id);
        assert_ne!(posed, at_rest, "evaluated translation must change the seeded gaussian weights");
        assert_eq!(posed.as_array().unwrap().len(), 2);
    }

    fn bone_id(session: &mut crate::Session, root: u64, name: &str) -> u64 {
        let listed = ok(&session.handle_request(&json!({"op":"rig.list_bones","root":root})));
        listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|bone| bone["boneId"] == name)
            .unwrap()["uuid"]
            .as_u64()
            .unwrap()
    }

    fn bone_rest(session: &mut crate::Session, root: u64, name: &str) -> ([f64; 3], [f64; 3]) {
        let listed = ok(&session.handle_request(&json!({"op":"rig.list_bones","root":root})));
        let bone = listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|bone| bone["boneId"] == name)
            .unwrap();
        (vec3(&bone["restHead"]), vec3(&bone["restTail"]))
    }

    fn vec3(value: &Value) -> [f64; 3] {
        let items = value.as_array().unwrap();
        [
            items[0].as_f64().unwrap(),
            items[1].as_f64().unwrap(),
            items[2].as_f64().unwrap(),
        ]
    }

    fn stored_weights(session: &mut crate::Session, rig: u64, grid: u64) -> Value {
        let node = ok(&session.handle_request(&json!({"op":"node.get","id":rig})));
        node["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["target"] == grid)
            .and_then(|binding| binding.get("vertexWeights"))
            .cloned()
            .unwrap_or(Value::Null)
    }

    fn ok(response: &Value) -> Value {
        assert_eq!(response["succeeded"], true, "{response}");
        response["result"].clone()
    }
}
