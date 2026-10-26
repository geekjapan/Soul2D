//! GridDeformer depth operations (slice D2).
//!
//! Op edits replace `depth-ops` and, when the list goes from empty to
//! non-empty, snapshot the current `depths` into `depth-op-base-depths`.
//! Clearing the list removes that base. `ApplyDepthOps` writes only `depths`,
//! using the same order as D `ngComputeDepthsFromOps`: all rings, then
//! attached points, then planes. One undo restores that depths write. Grid
//! resampling lives in `depth_resample`. PSD import and Bone refresh stay
//! outside this slice.

use crate::depths::finite_depth_or_zero;
use crate::nodes::{find_node, find_node_mut};
use crate::{valid_id, validate_model};
use serde_json::{json, Map, Value};

const OPS_KEY: &str = "depth-ops";
pub(crate) const BASE_KEY: &str = "depth-op-base-depths";

#[derive(Clone, Debug)]
struct Ring {
    p0: [f32; 2],
    p1: [f32; 2],
    amount: f32,
    width: f32,
    hardness: f32,
    p0_angle: f32,
    p1_angle: f32,
}

#[derive(Clone, Debug)]
struct Plane {
    center: [f32; 2],
    radius_x: f32,
    radius_y: f32,
    angle: f32,
    target_depth: f32,
    flatten_strength: f32,
}

#[derive(Clone, Debug)]
enum Op {
    Attached { index: usize, amount: f32 },
    Ring(Ring),
    Plane(Plane),
}

struct RingSample {
    cross: f32,
    z: f32,
    normal_slope: f32,
    width: f32,
    hardness: f32,
    count: usize,
}

pub(crate) fn list_ops(model: &Value, request: &Value) -> Result<Value, String> {
    let id = target_id(request)?;
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    require_grid_ops(node)?;
    Ok(ops_payload(id, node)?)
}

pub(crate) fn set_ops(model: &Value, request: &Value) -> Result<Value, String> {
    let id = target_id(request)?;
    let raw = request
        .get("operations")
        .ok_or("operations must be an array")?;
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    require_grid_ops(node)?;
    let vertices = grid_vertex_count(node)?;
    let inputs = raw.as_array().ok_or("operations must be an array")?;
    let ops = inputs
        .iter()
        .map(canonicalize_input)
        .collect::<Result<Vec<_>, _>>()?;
    validate_ops(&parsed_from_raw(&ops)?, vertices)?;
    let mut candidate = model.clone();
    {
        let node =
            find_node_mut(&mut candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
        write_ops(node, &ops)?;
    }
    validate_model(&candidate)?;
    Ok(candidate)
}

pub(crate) fn add_op(model: &Value, request: &Value) -> Result<Value, String> {
    let id = target_id(request)?;
    let raw = request
        .get("operation")
        .ok_or("operation must be an object")?;
    let index = signed_index(request, "index")?;
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    require_grid_ops(node)?;
    let vertices = grid_vertex_count(node)?;
    let op = canonicalize_input(raw)?;
    let mut ops = read_raw_ops(node)?;
    if index < 0 || index as usize >= ops.len() {
        ops.push(op);
    } else {
        ops.insert(index as usize, op);
    }
    validate_ops(&parsed_from_raw(&ops)?, vertices)?;
    let mut candidate = model.clone();
    {
        let node =
            find_node_mut(&mut candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
        write_ops(node, &ops)?;
    }
    validate_model(&candidate)?;
    Ok(candidate)
}

pub(crate) fn update_op(model: &Value, request: &Value) -> Result<Value, String> {
    let id = target_id(request)?;
    let raw = request
        .get("operation")
        .ok_or("operation must be an object")?;
    let index = signed_index(request, "index")?;
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    require_grid_ops(node)?;
    let vertices = grid_vertex_count(node)?;
    let op = canonicalize_input(raw)?;
    let mut ops = read_raw_ops(node)?;
    if index < 0 || index as usize >= ops.len() {
        return Err("operation index out of range".into());
    }
    ops[index as usize] = op;
    validate_ops(&parsed_from_raw(&ops)?, vertices)?;
    let mut candidate = model.clone();
    {
        let node =
            find_node_mut(&mut candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
        write_ops(node, &ops)?;
    }
    validate_model(&candidate)?;
    Ok(candidate)
}

pub(crate) fn remove_op(model: &Value, request: &Value) -> Result<Value, String> {
    let id = target_id(request)?;
    let index = signed_index(request, "index")?;
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    require_grid_ops(node)?;
    let mut ops = read_raw_ops(node)?;
    if index < 0 || index as usize >= ops.len() {
        return Err("operation index out of range".into());
    }
    ops.remove(index as usize);
    let mut candidate = model.clone();
    {
        let node =
            find_node_mut(&mut candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
        write_ops(node, &ops)?;
    }
    validate_model(&candidate)?;
    Ok(candidate)
}

pub(crate) fn move_op(model: &Value, request: &Value) -> Result<Value, String> {
    let id = target_id(request)?;
    let from = signed_index(request, "fromIndex")?;
    let to = signed_index(request, "toIndex")?;
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    require_grid_ops(node)?;
    let mut ops = read_raw_ops(node)?;
    if from < 0 || from as usize >= ops.len() {
        return Err("fromIndex out of range".into());
    }
    if to < 0 || to as usize >= ops.len() {
        return Err("toIndex out of range".into());
    }
    let op = ops.remove(from as usize);
    let dest = (to as usize).min(ops.len());
    ops.insert(dest, op);
    let mut candidate = model.clone();
    {
        let node =
            find_node_mut(&mut candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
        write_ops(node, &ops)?;
    }
    validate_model(&candidate)?;
    Ok(candidate)
}

pub(crate) fn clear_ops(model: &Value, request: &Value) -> Result<Value, String> {
    let id = target_id(request)?;
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    require_grid_ops(node)?;
    let mut candidate = model.clone();
    {
        let node =
            find_node_mut(&mut candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
        write_ops(node, &[])?;
    }
    validate_model(&candidate)?;
    Ok(candidate)
}

pub(crate) fn apply_ops(model: &Value, request: &Value) -> Result<Value, String> {
    let id = target_id(request)?;
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    // ApplyDepthOps checks GridDeformer before the depth-operation interface.
    require_grid(node)?;
    let vertices = grid_points(node)?;
    let ops = read_ops(node)?;
    validate_ops(&ops, vertices.len())?;
    let base = read_depth_array(node, BASE_KEY)?;
    let depths = compute_depths(&vertices, &ops, base.as_deref());
    let mut candidate = model.clone();
    {
        let node =
            find_node_mut(&mut candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
        node.as_object_mut()
            .ok_or("node must be an object")?
            .insert("depths".into(), json!(depths));
    }
    validate_model(&candidate)?;
    Ok(candidate)
}

/// `depth.edit.apply`: D `DepthMeshEditor.applyToTargets` for one GridDeformer. The steps D pushes as one
/// action group are chained on one candidate, so the caller commits a single undo step. `depth_draw::apply_plan`
/// chooses the steps; the returned name says which one ran.
pub(crate) fn edit_apply(model: &Value, request: &Value) -> Result<(Value, &'static str), String> {
    use crate::depth_draw::{apply_plan, ApplyPlan};
    let id = target_id(request)?;
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    require_grid_ops(node)?;
    let direct = match request.get("directEdits") {
        None | Some(Value::Null) => false,
        Some(value) => value.as_bool().ok_or("directEdits must be a boolean")?,
    };
    let incoming = request
        .get("operations")
        .and_then(Value::as_array)
        .ok_or("operations must be an array")?;
    let strip = |mut op: Value| {
        if let Some(object) = op.as_object_mut() {
            object.remove("indexInList");
        }
        op
    };
    let next = incoming
        .iter()
        .map(|op| canonicalize_input(op).map(strip))
        .collect::<Result<Vec<_>, _>>()?;
    let stored = read_raw_ops(node)?;
    let stored_canonical = stored
        .iter()
        .map(|op| canonicalize_input(op).map(strip))
        .collect::<Result<Vec<_>, _>>()
        .ok();
    let changed = next != stored && stored_canonical.as_ref() != Some(&next);
    let plan = apply_plan(!stored.is_empty(), changed, direct);
    let candidate = match plan {
        ApplyPlan::Nothing => return Ok((model.clone(), "Nothing")),
        ApplyPlan::SetDepths => (crate::depths::set_depths(model, request)?, "SetDepths"),
        ApplyPlan::ClearOpsThenSetDepths => {
            let cleared = clear_ops(model, request)?;
            (crate::depths::set_depths(&cleared, request)?, "ClearOpsThenSetDepths")
        }
        ApplyPlan::SetOpsThenApply => {
            let replaced = set_ops(model, request)?;
            (apply_ops(&replaced, request)?, "SetOpsThenApply")
        }
    };
    Ok(candidate)
}

fn compute_depths(vertices: &[[f32; 2]], ops: &[Op], base: Option<&[f32]>) -> Vec<f64> {
    let mut depths = if base.is_some_and(|values| values.len() == vertices.len()) {
        base.unwrap().to_vec()
    } else {
        vec![0.0; vertices.len()]
    };
    let rings: Vec<&Ring> = ops
        .iter()
        .filter_map(|op| match op {
            Op::Ring(ring) => Some(ring),
            _ => None,
        })
        .collect();
    apply_rings(vertices, &mut depths, &rings);
    for op in ops {
        if let Op::Attached { index, amount } = op {
            depths[*index] = clamp_depth(depths[*index] + amount);
        }
    }
    for op in ops {
        if let Op::Plane(plane) = op {
            apply_plane(vertices, &mut depths, plane);
        }
    }
    depths
        .into_iter()
        .map(|value| f64::from(clamp_depth(value)))
        .collect()
}

fn require_grid(node: &Value) -> Result<(), String> {
    let kind = node.get("type").and_then(Value::as_str).unwrap_or("Node");
    if kind != "GridDeformer" {
        return Err("target must be a GridDeformer".into());
    }
    Ok(())
}

fn require_grid_ops(node: &Value) -> Result<(), String> {
    require_grid(node).map_err(|_| "target must support depth operations".to_string())
}

fn grid_vertex_count(node: &Value) -> Result<usize, String> {
    Ok(grid_points(node)?.len())
}

pub(crate) fn grid_points(node: &Value) -> Result<Vec<[f32; 2]>, String> {
    let xs = axis_numbers(node, "grid_axis_x")?;
    let ys = axis_numbers(node, "grid_axis_y")?;
    if xs.len() < 2 || ys.len() < 2 {
        return Err("GridDeformer vertices are not a rectangular grid".into());
    }
    let mut points = Vec::with_capacity(xs.len().saturating_mul(ys.len()));
    for y in &ys {
        for x in &xs {
            points.push([*x, *y]);
        }
    }
    Ok(points)
}

fn axis_numbers(node: &Value, key: &str) -> Result<Vec<f32>, String> {
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
                .ok_or_else(|| "GridDeformer vertices are not a rectangular grid".to_string())?;
            let sample = number as f32;
            if sample.is_finite() {
                Ok(sample)
            } else {
                Err("GridDeformer vertices are not a rectangular grid".into())
            }
        })
        .collect()
}

fn read_ops(node: &Value) -> Result<Vec<Op>, String> {
    parsed_from_raw(&read_raw_ops(node)?)
}

fn read_raw_ops(node: &Value) -> Result<Vec<Value>, String> {
    match node.get(OPS_KEY) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => Ok(values.clone()),
        Some(_) => Err("depth-ops must be an array".into()),
    }
}

fn parsed_from_raw(ops: &[Value]) -> Result<Vec<Op>, String> {
    ops.iter().map(parse_op).collect()
}

fn canonicalize_input(value: &Value) -> Result<Value, String> {
    let op = parse_op(value)?;
    let mut stored = op_to_json(&op);
    if let (Some(source), Some(dest)) = (value.as_object(), stored.as_object_mut()) {
        for (key, field) in source {
            if !dest.contains_key(key) {
                dest.insert(key.clone(), field.clone());
            }
        }
    }
    Ok(stored)
}

fn read_depth_array(node: &Value, key: &str) -> Result<Option<Vec<f32>>, String> {
    match node.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(values)) => {
            let mut depths = Vec::with_capacity(values.len());
            for entry in values {
                let Some(number) = entry.as_f64() else {
                    return Err(format!("{key} must be an array of numbers"));
                };
                depths.push(finite_depth_or_zero(number) as f32);
            }
            Ok(Some(depths))
        }
        Some(_) => Err(format!("{key} must be an array of numbers")),
    }
}

fn write_ops(node: &mut Value, ops: &[Value]) -> Result<(), String> {
    // D replaces the base whenever the saved list goes from empty to non-empty,
    // including when a leftover depth-op-base-depths array is still present.
    let previous_empty = read_raw_ops(node)?.is_empty();
    let refresh_base = !ops.is_empty() && previous_empty;
    let snapshot = if refresh_base {
        read_depth_array(node, "depths")?
    } else {
        None
    };
    let object = node.as_object_mut().ok_or("node must be an object")?;
    if ops.is_empty() {
        object.remove(OPS_KEY);
        object.remove(BASE_KEY);
        return Ok(());
    }
    if refresh_base {
        match snapshot {
            Some(depths) if !depths.is_empty() => {
                let stored: Vec<f64> = depths.iter().copied().map(f64::from).collect();
                object.insert(BASE_KEY.into(), json!(stored));
            }
            _ => {
                object.remove(BASE_KEY);
            }
        }
    }
    object.insert(OPS_KEY.into(), Value::Array(ops.to_vec()));
    Ok(())
}

fn ops_payload(id: u32, node: &Value) -> Result<Value, String> {
    let stored = match node.get(OPS_KEY) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(values)) => values.clone(),
        Some(_) => return Err("depth-ops must be an array".into()),
    };
    let operations = stored
        .into_iter()
        .enumerate()
        .map(|(index, mut op)| {
            if let Some(object) = op.as_object_mut() {
                object.insert("indexInList".into(), json!(index));
            }
            op
        })
        .collect::<Vec<_>>();
    let count = i64::try_from(operations.len()).unwrap_or(i64::MAX);
    Ok(json!({ "target": id, "operations": operations, "count": count }))
}

pub(crate) fn ensure_known_depth_op(value: &Value) -> Result<(), String> {
    let type_name = value
        .as_object()
        .and_then(|object| object.get("type"))
        .and_then(Value::as_str)
        .ok_or("operation.type is required")?;
    match type_name {
        "attached-point" | "AttachedPoint" | "ring" | "Ring" | "plane" | "Plane" => {
            parse_op(value).map(|_| ())
        }
        _ => Ok(()),
    }
}

fn parse_op(value: &Value) -> Result<Op, String> {
    let object = value.as_object().ok_or("operation must be an object")?;
    let type_name = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or("operation.type is required")?;
    match type_name {
        "attached-point" | "AttachedPoint" => Ok(Op::Attached {
            index: usize_field(object, "index", 0)?,
            amount: f32_field(object, "amount", 0.0)?,
        }),
        "ring" | "Ring" => Ok(Op::Ring(Ring {
            p0: vec2_field(object, "p0")?,
            p1: vec2_field(object, "p1")?,
            amount: f32_field(object, "amount", 0.0)?,
            width: f32_field(object, "width", 1.0)?.max(0.5),
            hardness: f32_field(object, "hardness", 1.0)?.max(0.1),
            p0_angle: f32_field(object, "p0Angle", 180.0)?,
            p1_angle: f32_field(object, "p1Angle", 0.0)?,
        })),
        "plane" | "Plane" => {
            let target = if object
                .get("targetDepth")
                .is_some_and(|value| !value.is_null())
            {
                f32_field(object, "targetDepth", 0.0)?
            } else {
                f32_field(object, "amount", 0.0)?
            };
            Ok(Op::Plane(Plane {
                center: vec2_field(object, "center")?,
                radius_x: f32_field(object, "radiusX", 1.0)?.max(1.0),
                radius_y: f32_field(object, "radiusY", 1.0)?.max(1.0),
                angle: f32_field(object, "angle", 0.0)?,
                target_depth: target,
                flatten_strength: f32_field(object, "flattenStrength", 1.0)?,
            }))
        }
        other => Err(format!("unknown depth operation type: {other}")),
    }
}

fn validate_ops(ops: &[Op], vertices: usize) -> Result<(), String> {
    for op in ops {
        if let Op::Attached { index, .. } = op {
            if *index >= vertices {
                return Err("attached-point index must be less than vertices length".into());
            }
        }
    }
    Ok(())
}

fn op_to_json(op: &Op) -> Value {
    match op {
        Op::Attached { index, amount } => json!({
            "type": "attached-point",
            "index": index,
            "amount": f64::from(*amount),
        }),
        Op::Ring(ring) => json!({
            "type": "ring",
            "p0": [f64::from(ring.p0[0]), f64::from(ring.p0[1])],
            "p1": [f64::from(ring.p1[0]), f64::from(ring.p1[1])],
            "amount": f64::from(ring.amount),
            "width": f64::from(ring.width),
            "hardness": f64::from(ring.hardness),
            "p0Angle": f64::from(ring.p0_angle),
            "p1Angle": f64::from(ring.p1_angle),
        }),
        Op::Plane(plane) => json!({
            "type": "plane",
            "center": [f64::from(plane.center[0]), f64::from(plane.center[1])],
            "radiusX": f64::from(plane.radius_x),
            "radiusY": f64::from(plane.radius_y),
            "angle": f64::from(plane.angle),
            "targetDepth": f64::from(plane.target_depth),
            "flattenStrength": f64::from(plane.flatten_strength),
        }),
    }
}

fn apply_rings(vertices: &[[f32; 2]], depths: &mut [f32], rings: &[&Ring]) {
    let mut horizontal = Vec::new();
    let mut vertical = Vec::new();
    for ring in rings {
        if (ring.p1[0] - ring.p0[0]).abs() >= (ring.p1[1] - ring.p0[1]).abs() {
            horizontal.push(*ring);
        } else {
            vertical.push(*ring);
        }
    }
    apply_ring_family(vertices, depths, &horizontal, true);
    apply_ring_family(vertices, depths, &vertical, false);
}

fn apply_ring_family(vertices: &[[f32; 2]], depths: &mut [f32], rings: &[&Ring], axis_x: bool) {
    let mut ordered = rings.to_vec();
    ordered.sort_by(|left, right| ring_cross(left, axis_x).total_cmp(&ring_cross(right, axis_x)));
    for (index, point) in vertices.iter().enumerate() {
        let mut samples = Vec::new();
        for op in &ordered {
            if let Some(sample) = ring_sample_at(op, *point, axis_x) {
                samples.push(sample);
            }
        }
        if samples.is_empty() {
            continue;
        }
        samples.sort_by(|left, right| left.cross.total_cmp(&right.cross));
        let candidates = combine_ring_samples(samples);
        if candidates.is_empty() {
            continue;
        }
        let cross = if axis_x { point[1] } else { point[0] };
        let mut before = None;
        let mut after = None;
        for (sample_index, sample) in candidates.iter().enumerate() {
            if sample.cross <= cross {
                before = Some(sample_index);
            }
            if after.is_none() && sample.cross >= cross {
                after = Some(sample_index);
            }
        }
        let value = match (before, after) {
            (Some(before), Some(after)) if before != after => {
                let left = &candidates[before];
                let right = &candidates[after];
                let span = (right.cross - left.cross).max(0.000001);
                let t = ((cross - left.cross) / span).clamp(0.0, 1.0);
                hermite(
                    left.z,
                    left.normal_slope,
                    right.z,
                    right.normal_slope,
                    span,
                    t,
                )
            }
            _ => {
                let nearest = before.or(after).unwrap_or(0);
                nearest_falloff(&candidates[nearest], cross)
            }
        };
        if value.abs() > 0.000001 {
            depths[index] = clamp_depth(depths[index] + value);
        }
    }
}

fn ring_cross(op: &Ring, axis_x: bool) -> f32 {
    if axis_x {
        (op.p0[1] + op.p1[1]) * 0.5
    } else {
        (op.p0[0] + op.p1[0]) * 0.5
    }
}

fn ring_sample_at(op: &Ring, point: [f32; 2], axis_x: bool) -> Option<RingSample> {
    let along = if axis_x { point[0] } else { point[1] };
    let a_along = if axis_x { op.p0[0] } else { op.p0[1] };
    let b_along = if axis_x { op.p1[0] } else { op.p1[1] };
    if along < a_along.min(b_along) || along > a_along.max(b_along) {
        return None;
    }
    let span = b_along - a_along;
    if span.abs() < 0.000001 {
        return None;
    }
    let ratio = ((along - a_along) / span).clamp(0.0, 1.0);
    let angle0 = op.p0_angle * std::f32::consts::PI / 180.0;
    let angle1 = op.p1_angle * std::f32::consts::PI / 180.0;
    let angle = angle0 + (angle1 - angle0) * ratio;
    let tangent_slope = op.amount * angle.cos() * (angle1 - angle0) / span;
    Some(RingSample {
        cross: ring_cross(op, axis_x),
        z: op.amount * angle.sin(),
        normal_slope: -tangent_slope,
        width: op.width.max(1.0),
        hardness: op.hardness.max(0.1),
        count: 1,
    })
}

fn combine_ring_samples(samples: Vec<RingSample>) -> Vec<RingSample> {
    let mut groups = Vec::new();
    for sample in samples {
        if let Some(group) = groups
            .iter_mut()
            .find(|group: &&mut RingSample| (group.cross - sample.cross).abs() <= 0.0001)
        {
            let count = group.count as f32;
            group.z += sample.z;
            group.normal_slope += sample.normal_slope;
            group.width = group.width.max(sample.width);
            group.hardness = (group.hardness * count + sample.hardness) / (count + 1.0);
            group.count += 1;
        } else {
            groups.push(sample);
        }
    }
    groups
}

fn hermite(z0: f32, m0: f32, z1: f32, m1: f32, span: f32, t: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    (2.0 * t3 - 3.0 * t2 + 1.0) * z0
        + (t3 - 2.0 * t2 + t) * m0 * span
        + (-2.0 * t3 + 3.0 * t2) * z1
        + (t3 - t2) * m1 * span
}

fn nearest_falloff(nearest: &RingSample, cross: f32) -> f32 {
    let distance = (cross - nearest.cross).abs();
    let falloff = (-((distance / nearest.width.max(1.0)).powf(nearest.hardness)) * 2.2).exp();
    nearest.z * falloff
}

fn apply_plane(vertices: &[[f32; 2]], depths: &mut [f32], plane: &Plane) {
    let snapshot = depths.to_vec();
    let mut ring_sum = 0.0;
    let mut ring_count = 0usize;
    let mut inside_sum = 0.0;
    let mut inside_count = 0usize;
    for (index, point) in vertices.iter().enumerate() {
        let norm = ellipse_norm(*point, plane);
        if norm <= 1.0 {
            inside_sum += snapshot[index];
            inside_count += 1;
        } else if norm <= 1.35 {
            ring_sum += snapshot[index];
            ring_count += 1;
        }
    }
    if inside_count == 0 {
        return;
    }
    let mut target = if ring_count > 0 {
        ring_sum / ring_count as f32
    } else {
        inside_sum / inside_count as f32
    };
    if plane.target_depth != 0.0 {
        target = plane.target_depth;
    }
    for (index, point) in vertices.iter().enumerate() {
        let norm = ellipse_norm(*point, plane);
        if norm > 1.0 {
            continue;
        }
        let center_weight = 1.0 - norm.clamp(0.0, 1.0);
        let strength = plane.flatten_strength.clamp(0.0, 1.0) * (0.45 + center_weight * 0.55);
        let value = snapshot[index] * (1.0 - strength) + target * strength;
        depths[index] = clamp_depth(value);
    }
}

fn ellipse_norm(point: [f32; 2], plane: &Plane) -> f32 {
    let angle = -plane.angle * std::f32::consts::PI / 180.0;
    let dx = point[0] - plane.center[0];
    let dy = point[1] - plane.center[1];
    let x = angle.cos() * dx - angle.sin() * dy;
    let y = angle.sin() * dx + angle.cos() * dy;
    let radius_x = plane.radius_x.max(1.0);
    let radius_y = plane.radius_y.max(1.0);
    ((x / radius_x).powi(2) + (y / radius_y).powi(2)).sqrt()
}

fn clamp_depth(value: f32) -> f32 {
    if !value.is_finite() {
        return 0.0;
    }
    (value * 1000.0).round() / 1000.0
}

fn f32_field(object: &Map<String, Value>, key: &str, fallback: f32) -> Result<f32, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(fallback),
        Some(value) => {
            let number = value
                .as_f64()
                .ok_or_else(|| format!("{key} must be a number"))?;
            if !number.is_finite() {
                return Err(format!("{key} must be finite"));
            }
            let sample = number as f32;
            if sample.is_finite() {
                Ok(sample)
            } else {
                Err(format!("{key} must be finite"))
            }
        }
    }
}

fn usize_field(object: &Map<String, Value>, key: &str, fallback: usize) -> Result<usize, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(fallback),
        Some(value) => non_negative_index(value, key),
    }
}

pub(crate) fn non_negative_index(value: &Value, key: &str) -> Result<usize, String> {
    if let Some(number) = value.as_u64() {
        return usize::try_from(number).map_err(|_| format!("{key} must be >= 0"));
    }
    if let Some(number) = value.as_i64() {
        if number < 0 {
            return Err(format!("{key} must be >= 0"));
        }
        return usize::try_from(number).map_err(|_| format!("{key} must be >= 0"));
    }
    if let Some(number) = value.as_f64() {
        if number >= 0.0 && number.is_finite() && number == number.trunc() {
            return Ok(number as usize);
        }
    }
    Err(format!("{key} must be a non-negative integer"))
}

fn vec2_field(object: &Map<String, Value>, key: &str) -> Result<[f32; 2], String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok([0.0, 0.0]),
        Some(value) => {
            let values = value
                .as_array()
                .filter(|values| values.len() == 2)
                .ok_or_else(|| format!("{key} must be [x, y]"))?;
            Ok([
                f32_number(&values[0], &format!("{key}[0]"))?,
                f32_number(&values[1], &format!("{key}[1]"))?,
            ])
        }
    }
}

fn f32_number(value: &Value, name: &str) -> Result<f32, String> {
    let number = value
        .as_f64()
        .ok_or_else(|| format!("{name} must be a number"))?;
    if !number.is_finite() {
        return Err(format!("{name} must be finite"));
    }
    let sample = number as f32;
    if sample.is_finite() {
        Ok(sample)
    } else {
        Err(format!("{name} must be finite"))
    }
}

fn signed_index(request: &Value, key: &str) -> Result<i64, String> {
    let value = request
        .get(key)
        .ok_or_else(|| format!("{key} must be an integer"))?;
    if let Some(number) = value.as_i64() {
        return Ok(number);
    }
    if let Some(number) = value.as_u64() {
        return i64::try_from(number).map_err(|_| format!("{key} must be an integer"));
    }
    if let Some(number) = value.as_f64() {
        if number.is_finite() && number == number.trunc() {
            return Ok(number as i64);
        }
    }
    Err(format!("{key} must be an integer"))
}

fn target_id(request: &Value) -> Result<u32, String> {
    if let Some(value) = request.get("target").or_else(|| request.get("id")) {
        return parse_id(value, "target");
    }
    if let Some(nodes) = request.get("nodes").or_else(|| request.get("ids")) {
        let nodes = nodes
            .as_array()
            .ok_or("nodes must be an array of node UUIDs")?;
        if nodes.len() != 1 {
            return Err("exactly one depth target is required".into());
        }
        return parse_id(&nodes[0], "target");
    }
    Err("target must be a node UUID".into())
}

fn parse_id(value: &Value, name: &str) -> Result<u32, String> {
    let raw = if let Some(number) = value.as_u64() {
        number
    } else if let Some(text) = value.as_str() {
        text.parse::<u64>()
            .map_err(|_| format!("{name} must be a node UUID"))?
    } else {
        return Err(format!("{name} must be a node UUID"));
    };
    valid_id(raw).ok_or_else(|| format!("{name} must be a node UUID"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Vec<[f32; 2]> {
        vec![[-1.0, -1.0], [1.0, -1.0], [-1.0, 1.0], [1.0, 1.0]]
    }

    #[test]
    fn attached_point_uses_base_then_rounds_like_d() {
        let ops = [Op::Attached {
            index: 0,
            amount: 0.3334,
        }];
        let base = [0.1, 0.0, 0.0, 0.0];
        let depths = compute_depths(&grid(), &ops, Some(&base));
        assert_eq!(depths[0], f64::from(0.433f32));
        assert_eq!(&depths[1..], &[0.0, 0.0, 0.0]);
    }

    #[test]
    fn rings_run_before_attached_points() {
        let ring = Op::Ring(Ring {
            p0: [-1.0, 0.0],
            p1: [1.0, 0.0],
            amount: 1.0,
            width: 1.0,
            hardness: 1.0,
            p0_angle: 90.0,
            p1_angle: 90.0,
        });
        let attached = Op::Attached {
            index: 0,
            amount: 0.5,
        };
        let depths = compute_depths(&grid(), &[attached, ring], None);
        assert_eq!(depths[0], f64::from(0.611f32));
        assert_eq!(depths[1], f64::from(0.111f32));
    }
}
