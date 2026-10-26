//! Mesh-editor mirror.
//!
//! Positions follow `mirrorH`, `mirrorV`, and `mirrorHV` in
//! `viewport/common/mesheditor/operations/base.d`. `mirrorV` uses
//! `mirrorOrigin.x` for the Y term. Path vertices are rewritten in place and
//! `"deform"` offsets on those indices use `mirrorDelta`. A grid is stored as
//! two axes, so the mirror is allowed only for every vertex: the mirrored
//! axis is rewritten by that formula and then reversed, and `"deform"` cells
//! are permuted onto the new vertex order. This command does not call
//! `rig.mirror`.

use crate::nodes::{self, find_node};
use crate::valid_id;
use crate::validate_model;
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Axis {
    X,
    Y,
    Both,
}

pub(crate) fn apply(model: &Value, request: &Value) -> Result<Value, String> {
    let node_id = required_node(request)?;
    let axis = required_axis(request)?;
    let origin = required_origin(request)?;
    let selected = required_indices(request)?;
    let node = find_node(model, node_id).ok_or_else(|| format!("node {node_id} was not found"))?;
    let kind = node.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "PathDeformer" => mirror_path(model, node_id, axis, origin, &selected),
        "GridDeformer" => mirror_grid(model, node_id, axis, origin, &selected),
        "Part" => mirror_part(model, node_id, axis, origin, &selected),
        _ => Err("mesh.mirror target is not a Path, Grid, or Part".into()),
    }
}

fn mirror_path(
    model: &Value,
    node_id: u32,
    axis: Axis,
    origin: [f64; 2],
    selected: &[usize],
) -> Result<Value, String> {
    if target_has_vertex_weights(model, node_id) {
        return Err("mesh.mirror cannot match D while vertexWeights are set".into());
    }
    let node = find_node(model, node_id).ok_or_else(|| format!("node {node_id} was not found"))?;
    let mut points = path_points(node)?;
    if selected.is_empty() {
        return Err("vertices must list at least one index".into());
    }
    let mut seen = vec![false; points.len()];
    for index in selected {
        if *index >= points.len() {
            return Err(format!("vertex index {index} is past the vertex list"));
        }
        if seen[*index] {
            return Err(format!("vertex index {index} is listed more than once"));
        }
        seen[*index] = true;
        let reflected = mirror_point(points[*index], axis, origin);
        if !reflected[0].is_finite() || !reflected[1].is_finite() {
            return Err("reflected vertex is not finite".into());
        }
        points[*index] = reflected;
    }
    let mut candidate = model.clone();
    {
        let node = nodes::find_node_mut(&mut candidate, node_id)
            .ok_or_else(|| format!("node {node_id} was not found"))?;
        let flat: Vec<f64> = points.iter().flat_map(|point| [point[0], point[1]]).collect();
        node.as_object_mut()
            .ok_or("node must be an object")?
            .insert("vertices".into(), json!(flat));
    }
    update_deform(candidate.as_object_mut().ok_or("model must be an object")?, node_id, points.len(), |offsets| {
        for index in selected {
            offsets[*index] = mirror_delta(offsets[*index], axis);
        }
        Ok(())
    })?;
    validate_model(&candidate)?;
    Ok(candidate)
}

fn mirror_part(
    model: &Value,
    node_id: u32,
    axis: Axis,
    origin: [f64; 2],
    selected: &[usize],
) -> Result<Value, String> {
    if target_has_vertex_weights(model, node_id) {
        return Err("mesh.mirror cannot match D while vertexWeights are set".into());
    }
    let node = find_node(model, node_id).ok_or_else(|| format!("node {node_id} was not found"))?;
    let Some(mut stored) = part_points(node)? else {
        return Err("Part mesh is not available".into());
    };
    if selected.is_empty() {
        return Err("vertices must list at least one index".into());
    }
    let mut seen = vec![false; stored.points.len()];
    for index in selected {
        if *index >= stored.points.len() {
            return Err(format!("vertex index {index} is past the vertex list"));
        }
        if seen[*index] {
            return Err(format!("vertex index {index} is listed more than once"));
        }
        seen[*index] = true;
        let reflected = mirror_point(stored.points[*index], axis, origin);
        if !reflected[0].is_finite() || !reflected[1].is_finite() {
            return Err("reflected vertex is not finite".into());
        }
        stored.points[*index] = reflected;
    }
    let mut candidate = model.clone();
    {
        let node = nodes::find_node_mut(&mut candidate, node_id)
            .ok_or_else(|| format!("node {node_id} was not found"))?;
        write_part_points(node, &stored)?;
    }
    update_deform(
        candidate.as_object_mut().ok_or("model must be an object")?,
        node_id,
        stored.points.len(),
        |offsets| {
            for index in selected {
                offsets[*index] = mirror_delta(offsets[*index], axis);
            }
            Ok(())
        },
    )?;
    validate_model(&candidate)?;
    Ok(candidate)
}

enum PartStore {
    MeshVerts,
    NestedVertices,
    FlatVertices,
}

struct PartPoints {
    points: Vec<[f64; 2]>,
    store: PartStore,
}

fn part_points(node: &Value) -> Result<Option<PartPoints>, String> {
    // Same precedence as render `load_part_arrays`: `mesh.verts` wins, and a
    // direct `vertices` array may be nested pairs or a flat x,y list.
    if let Some(verts) = node.get("mesh").and_then(|mesh| mesh.get("verts")) {
        if verts.is_null() {
            return Ok(None);
        }
        let points = flat_points(verts, "Part mesh.verts")?;
        if points.is_empty() {
            return Ok(None);
        }
        return Ok(Some(PartPoints { points, store: PartStore::MeshVerts }));
    }
    let Some(vertices) = node.get("vertices") else {
        return Ok(None);
    };
    if vertices.is_null() {
        return Ok(None);
    }
    let raw = vertices.as_array().ok_or("Part vertices must be an array")?;
    if raw.is_empty() {
        return Ok(None);
    }
    if raw.first().is_some_and(Value::is_array) {
        let points = nested_points(vertices)?;
        if points.is_empty() {
            return Ok(None);
        }
        return Ok(Some(PartPoints { points, store: PartStore::NestedVertices }));
    }
    let points = flat_points(vertices, "Part vertices")?;
    if points.is_empty() {
        return Ok(None);
    }
    Ok(Some(PartPoints { points, store: PartStore::FlatVertices }))
}

fn nested_points(value: &Value) -> Result<Vec<[f64; 2]>, String> {
    let raw = value.as_array().ok_or("Part vertices must be an array of pairs")?;
    raw.iter()
        .map(|pair| {
            let pair = pair.as_array().ok_or("Part vertices must be an array of pairs")?;
            if pair.len() < 2 {
                return Err("Part vertices must be an array of pairs".into());
            }
            let x = pair[0]
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or("Part vertices must be finite")?;
            let y = pair[1]
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or("Part vertices must be finite")?;
            Ok([x, y])
        })
        .collect()
}

fn flat_points(value: &Value, label: &str) -> Result<Vec<[f64; 2]>, String> {
    let raw = value
        .as_array()
        .ok_or_else(|| format!("{label} must be finite x,y pairs"))?;
    if raw.len() % 2 != 0 {
        return Err(format!("{label} must be finite x,y pairs"));
    }
    let mut points = Vec::with_capacity(raw.len() / 2);
    for pair in raw.chunks(2) {
        let x = pair[0]
            .as_f64()
            .filter(|number| number.is_finite())
            .ok_or_else(|| format!("{label} must be finite x,y pairs"))?;
        let y = pair[1]
            .as_f64()
            .filter(|number| number.is_finite())
            .ok_or_else(|| format!("{label} must be finite x,y pairs"))?;
        points.push([x, y]);
    }
    Ok(points)
}

fn write_part_points(node: &mut Value, stored: &PartPoints) -> Result<(), String> {
    let nested = node
        .get("vertices")
        .and_then(Value::as_array)
        .is_some_and(|vertices| vertices.first().is_some_and(Value::is_array));
    let flat_vertices = node
        .get("vertices")
        .and_then(Value::as_array)
        .is_some_and(|vertices| !vertices.is_empty() && vertices.first().is_some_and(|value| !value.is_array()));
    let has_mesh_verts = node
        .get("mesh")
        .and_then(|mesh| mesh.get("verts"))
        .is_some();
    let object = node.as_object_mut().ok_or("node must be an object")?;
    let flat: Vec<f64> = stored.points.iter().flat_map(|point| [point[0], point[1]]).collect();
    if nested {
        object.insert(
            "vertices".into(),
            Value::Array(stored.points.iter().copied().map(|pair| json!(pair)).collect()),
        );
    } else if flat_vertices {
        object.insert("vertices".into(), json!(flat.clone()));
    } else if matches!(stored.store, PartStore::NestedVertices) {
        object.insert(
            "vertices".into(),
            Value::Array(stored.points.iter().copied().map(|pair| json!(pair)).collect()),
        );
    } else if matches!(stored.store, PartStore::FlatVertices) {
        object.insert("vertices".into(), json!(flat.clone()));
    }
    if has_mesh_verts || matches!(stored.store, PartStore::MeshVerts) {
        let mesh = object
            .get_mut("mesh")
            .and_then(Value::as_object_mut)
            .ok_or("node must be an object")?;
        mesh.insert("verts".into(), json!(flat));
    }
    Ok(())
}

fn mirror_grid(
    model: &Value,
    node_id: u32,
    axis: Axis,
    origin: [f64; 2],
    selected: &[usize],
) -> Result<Value, String> {
    let node = find_node(model, node_id).ok_or_else(|| format!("node {node_id} was not found"))?;
    if has_depth_payload(node) {
        return Err("mesh.mirror cannot match D while depths are present".into());
    }
    if target_has_vertex_weights(model, node_id) {
        return Err("mesh.mirror cannot match D while vertexWeights are set".into());
    }
    let (mut xs, mut ys) = grid_axes(node)?;
    let count = xs.len().saturating_mul(ys.len());
    if !lists_every_index(selected, count) {
        return Err("mesh.mirror vertices must list each grid vertex once".into());
    }
    match axis {
        Axis::X => xs = mirror_axis(&xs, origin[0])?,
        Axis::Y => ys = mirror_axis(&ys, origin[0])?,
        Axis::Both => {
            xs = mirror_axis(&xs, origin[0])?;
            ys = mirror_axis(&ys, origin[1])?;
        }
    }
    let cols = xs.len();
    let rows = ys.len();
    let mut candidate = model.clone();
    {
        let node = nodes::find_node_mut(&mut candidate, node_id)
            .ok_or_else(|| format!("node {node_id} was not found"))?;
        let object = node.as_object_mut().ok_or("node must be an object")?;
        object.insert("grid_axis_x".into(), json!(xs));
        object.insert("grid_axis_y".into(), json!(ys));
    }
    update_deform(
        candidate.as_object_mut().ok_or("model must be an object")?,
        node_id,
        count,
        |offsets| {
            let previous = offsets.clone();
            for index in 0..count {
                let source = old_index_for_new(index, cols, rows, axis);
                offsets[index] = mirror_delta(previous[source], axis);
            }
            Ok(())
        },
    )?;
    validate_model(&candidate)?;
    Ok(candidate)
}

fn mirror_point(point: [f64; 2], axis: Axis, origin: [f64; 2]) -> [f64; 2] {
    match axis {
        Axis::X => mirror_h(point, origin),
        Axis::Y => mirror_v(point, origin),
        Axis::Both => mirror_hv(point, origin),
    }
}

fn mirror_h(point: [f64; 2], origin: [f64; 2]) -> [f64; 2] {
    [2.0 * origin[0] - point[0], point[1]]
}

fn mirror_v(point: [f64; 2], origin: [f64; 2]) -> [f64; 2] {
    // D `mirrorV` adds `mirrorOrigin.x` on Y, not `mirrorOrigin.y`.
    [point[0], 2.0 * origin[0] - point[1]]
}

fn mirror_hv(point: [f64; 2], origin: [f64; 2]) -> [f64; 2] {
    [2.0 * origin[0] - point[0], 2.0 * origin[1] - point[1]]
}

fn mirror_delta(delta: [f64; 2], axis: Axis) -> [f64; 2] {
    match axis {
        Axis::X => [-delta[0], delta[1]],
        Axis::Y => [delta[0], -delta[1]],
        Axis::Both => [-delta[0], -delta[1]],
    }
}

fn mirror_axis(values: &[f64], origin_component: f64) -> Result<Vec<f64>, String> {
    let mut mirrored: Vec<f64> = values
        .iter()
        .map(|value| 2.0 * origin_component - value)
        .collect();
    mirrored.reverse();
    if mirrored.iter().any(|value| !value.is_finite()) || !strictly_monotonic(&mirrored) {
        return Err("reflected grid axis is not strictly monotonic".into());
    }
    Ok(mirrored)
}

fn strictly_monotonic(values: &[f64]) -> bool {
    if values.len() < 2 {
        return false;
    }
    let increasing = values.windows(2).all(|pair| pair[0] < pair[1]);
    let decreasing = values.windows(2).all(|pair| pair[0] > pair[1]);
    increasing || decreasing
}

fn old_index_for_new(index: usize, cols: usize, rows: usize, axis: Axis) -> usize {
    let x = index % cols;
    let y = index / cols;
    let (old_x, old_y) = match axis {
        Axis::X => (cols - 1 - x, y),
        Axis::Y => (x, rows - 1 - y),
        Axis::Both => (cols - 1 - x, rows - 1 - y),
    };
    old_y * cols + old_x
}

fn lists_every_index(selected: &[usize], count: usize) -> bool {
    if count == 0 || selected.len() != count {
        return false;
    }
    let mut seen = vec![false; count];
    for index in selected {
        if *index >= count || seen[*index] {
            return false;
        }
        seen[*index] = true;
    }
    true
}

fn has_depth_payload(node: &Value) -> bool {
    ["depths", "depth-ops", "depth-op-base-depths"]
        .iter()
        .any(|key| node.get(*key).is_some_and(|value| !value.is_null()))
}

fn target_has_vertex_weights(model: &Value, target: u32) -> bool {
    let Some(root) = model.get("nodes") else {
        return false;
    };
    rig_has_weights(root, target)
}

fn rig_has_weights(node: &Value, target: u32) -> bool {
    if node.get("type").and_then(Value::as_str) == Some("DepthRigRoot") {
        if let Some(bindings) = node.get("bindings").and_then(Value::as_array) {
            for binding in bindings {
                let same = binding.get("target").and_then(Value::as_u64) == Some(u64::from(target));
                if same && binding.get("vertexWeights").is_some() {
                    return true;
                }
            }
        }
    }
    node.get("children")
        .and_then(Value::as_array)
        .is_some_and(|children| children.iter().any(|child| rig_has_weights(child, target)))
}

fn path_points(node: &Value) -> Result<Vec<[f64; 2]>, String> {
    let raw = node
        .get("vertices")
        .and_then(Value::as_array)
        .ok_or("path vertices must be an array")?;
    if raw.len() % 2 != 0 {
        return Err("vertices length must be even (x,y pairs)".into());
    }
    let mut points = Vec::with_capacity(raw.len() / 2);
    for pair in raw.chunks(2) {
        let x = pair[0]
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or("path vertices must be finite")?;
        let y = pair[1]
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or("path vertices must be finite")?;
        points.push([x, y]);
    }
    Ok(points)
}

fn grid_axes(node: &Value) -> Result<(Vec<f64>, Vec<f64>), String> {
    let xs = axis_numbers(node, "grid_axis_x")?;
    let ys = axis_numbers(node, "grid_axis_y")?;
    if xs.len() < 2 || ys.len() < 2 {
        return Err("target is not deformable".into());
    }
    Ok((xs, ys))
}

fn axis_numbers(node: &Value, key: &str) -> Result<Vec<f64>, String> {
    let raw = node
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{key} must be an array"))?;
    raw.iter()
        .map(|value| {
            value
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or_else(|| format!("{key} must contain finite numbers"))
        })
        .collect()
}

fn update_deform(
    model: &mut serde_json::Map<String, Value>,
    node_id: u32,
    vertex_count: usize,
    mut update: impl FnMut(&mut Vec<[f64; 2]>) -> Result<(), String>,
) -> Result<(), String> {
    let Some(parameters) = model.get_mut("param").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    update_parameter_list(parameters, node_id, vertex_count, &mut update)
}

fn update_parameter_list(
    list: &mut [Value],
    node_id: u32,
    vertex_count: usize,
    update: &mut impl FnMut(&mut Vec<[f64; 2]>) -> Result<(), String>,
) -> Result<(), String> {
    for parameter in list.iter_mut() {
        if parameter.get("groupUUID").is_some() {
            if let Some(children) = parameter.get_mut("children").and_then(Value::as_array_mut) {
                update_parameter_list(children, node_id, vertex_count, update)?;
            }
            continue;
        }
        let matches_node = parameter
            .get("bindings")
            .and_then(Value::as_array)
            .is_some_and(|bindings| {
                bindings.iter().any(|binding| {
                    binding.get("param_name").and_then(Value::as_str) == Some("deform")
                        && binding.get("node").and_then(Value::as_u64) == Some(u64::from(node_id))
                })
            });
        if !matches_node {
            continue;
        }
        let (x_count, y_count) = parameter_axis_counts(parameter, node_id)?;
        let Some(bindings) = parameter.get_mut("bindings").and_then(Value::as_array_mut) else {
            continue;
        };
        for binding in bindings {
            if binding.get("param_name").and_then(Value::as_str) != Some("deform") {
                continue;
            }
            if binding.get("node").and_then(Value::as_u64) != Some(u64::from(node_id)) {
                continue;
            }
            update_binding(binding, node_id, vertex_count, x_count, y_count, update)?;
        }
    }
    Ok(())
}

fn parameter_axis_counts(parameter: &Value, node_id: u32) -> Result<(usize, usize), String> {
    let malformed = format!("node {node_id} deform binding is malformed");
    let axes = parameter
        .get("axis_points")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed.clone())?;
    if axes.len() != 2 {
        return Err(malformed);
    }
    let x_count = axes
        .first()
        .and_then(Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| malformed.clone())?;
    let is_vec2 = parameter
        .get("is_vec2")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let y_count = if is_vec2 {
        axes.get(1)
            .and_then(Value::as_array)
            .map(Vec::len)
            .ok_or(malformed)?
    } else {
        1
    };
    if x_count == 0 || y_count == 0 {
        return Err(format!("node {node_id} deform binding is malformed"));
    }
    Ok((x_count, y_count))
}

fn update_binding(
    binding: &mut Value,
    node_id: u32,
    vertex_count: usize,
    x_count: usize,
    y_count: usize,
    update: &mut impl FnMut(&mut Vec<[f64; 2]>) -> Result<(), String>,
) -> Result<(), String> {
    validate_binding_grid(binding, node_id, x_count, y_count)?;
    let values = binding
        .get_mut("values")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| format!("node {node_id} deform binding is malformed"))?;
    for column in values.iter_mut() {
        let column = column
            .as_array_mut()
            .ok_or_else(|| format!("node {node_id} deform binding is malformed"))?;
        for cell in column.iter_mut() {
            if cell.is_null() {
                continue;
            }
            let Some(points) = cell.as_array() else {
                return Err(format!("node {node_id} deform binding is malformed"));
            };
            if points.is_empty() {
                continue;
            }
            if points.len() != vertex_count {
                return Err(format!("node {node_id} deform binding is malformed"));
            }
            let mut offsets = Vec::with_capacity(points.len());
            for point in points {
                let pair = point
                    .as_array()
                    .ok_or_else(|| format!("node {node_id} deform binding is malformed"))?;
                if pair.len() < 2 {
                    return Err(format!("node {node_id} deform binding is malformed"));
                }
                let x = pair[0]
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| format!("node {node_id} deform binding is malformed"))?;
                let y = pair[1]
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| format!("node {node_id} deform binding is malformed"))?;
                offsets.push([x, y]);
            }
            update(&mut offsets)?;
            *cell = Value::Array(offsets.iter().copied().map(|pair| json!(pair)).collect());
        }
    }
    Ok(())
}

fn validate_binding_grid(
    binding: &Value,
    node_id: u32,
    x_count: usize,
    y_count: usize,
) -> Result<(), String> {
    let malformed = format!("node {node_id} deform binding is malformed");
    let values = binding
        .get("values")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed.clone())?;
    let flags = binding
        .get("isSet")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed.clone())?;
    if values.len() != x_count || flags.len() != x_count {
        return Err(malformed);
    }
    for (column, flag_column) in values.iter().zip(flags.iter()) {
        let column = column.as_array().ok_or_else(|| malformed.clone())?;
        let flag_column = flag_column.as_array().ok_or_else(|| malformed.clone())?;
        if column.len() != y_count || flag_column.len() != y_count {
            return Err(malformed.clone());
        }
        for flag in flag_column {
            if !flag.is_boolean() && !flag.is_null() {
                return Err(malformed);
            }
        }
    }
    Ok(())
}

fn required_node(request: &Value) -> Result<u32, String> {
    request
        .get("node")
        .and_then(Value::as_u64)
        .and_then(valid_id)
        .ok_or_else(|| "node must be a positive resource UUID".to_string())
}

fn required_axis(request: &Value) -> Result<Axis, String> {
    match request.get("axis").and_then(Value::as_str) {
        Some("x") => Ok(Axis::X),
        Some("y") => Ok(Axis::Y),
        Some("both") => Ok(Axis::Both),
        _ => Err("mesh.mirror axis must be x, y, or both".into()),
    }
}

fn required_origin(request: &Value) -> Result<[f64; 2], String> {
    let raw = request
        .get("origin")
        .and_then(Value::as_array)
        .ok_or("origin must be a pair of finite numbers")?;
    if raw.len() != 2 {
        return Err("origin must be a pair of finite numbers".into());
    }
    let x = raw[0]
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or("origin must be a pair of finite numbers")?;
    let y = raw[1]
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or("origin must be a pair of finite numbers")?;
    Ok([x, y])
}

fn required_indices(request: &Value) -> Result<Vec<usize>, String> {
    let raw = request
        .get("vertices")
        .and_then(Value::as_array)
        .ok_or("vertices must be an array of indices")?;
    raw.iter()
        .map(|value| {
            value
                .as_u64()
                .and_then(|index| usize::try_from(index).ok())
                .ok_or_else(|| "vertex index must be an unsigned integer".to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(session: &mut crate::Session, request: &Value) -> Value {
        session.handle_request(request)
    }

    fn ok(response: &Value) -> Value {
        assert_eq!(response["succeeded"], true, "{response}");
        response["result"].clone()
    }

    #[test]
    fn mirror_v_uses_origin_x_and_deltas_match_d() {
        let origin = [2.0, 100.0];
        let point = [1.0, 4.0];
        assert_eq!(mirror_h(point, origin), [3.0, 4.0]);
        assert_eq!(mirror_v(point, origin), [1.0, 0.0]);
        assert_ne!(mirror_v(point, origin)[1], 2.0 * origin[1] - point[1]);
        assert_eq!(mirror_hv(point, origin), [3.0, 196.0]);
        assert_eq!(mirror_delta([3.0, 5.0], Axis::X), [-3.0, 5.0]);
        assert_eq!(mirror_delta([3.0, 5.0], Axis::Y), [3.0, -5.0]);
        assert_eq!(mirror_delta([3.0, 5.0], Axis::Both), [-3.0, -5.0]);
        assert_eq!(mirror_h(mirror_h(point, origin), origin), point);
        assert_eq!(mirror_v(mirror_v(point, origin), origin), point);
        assert_eq!(mirror_hv(mirror_hv(point, origin), origin), point);
    }

    #[test]
    fn path_mirror_matches_the_formula_and_a_second_mirror_restores_it() {
        let mut session = crate::Session::new();
        let created = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"PathDeformer","name":"Path"}),
        ));
        let path_id = created["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_vertices","nodes":[path_id],"vertices":[1.0, 4.0, 6.0, 1.0]}),
        ));
        let mut model = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"].clone();
        model["param"] = json!([{
            "uuid": 40, "name": "Deform", "is_vec2": false,
            "min": [0.0, 0.0], "max": [1.0, 0.0],
            "axis_points": [[0.0, 1.0], [0.0]],
            "bindings": [{
                "node": path_id, "param_name": "deform",
                "values": [[[[3.0, 5.0], [1.0, -2.0]]], [[[3.0, 5.0], [1.0, -2.0]]]],
                "isSet": [[true], [true]]
            }]
        }]);
        ok(&call(&mut session, &json!({"op":"new","model":model})));
        let rig = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let bone = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Bone","parents":[rig_id],
                "restHead":[1.0,2.0,3.0],"restTail":[1.0,8.0,3.0]
            }),
        ));
        let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
        let before = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let rested = ok(&call(&mut session, &json!({"op":"node.get","id":bone_id})));
        ok(&call(
            &mut session,
            &json!({"op":"mesh.mirror","node":path_id,"axis":"y","origin":[2.0, 100.0],"vertices":[0]}),
        ));
        let mirrored = ok(&call(&mut session, &json!({"op":"node.get","id":path_id})));
        assert_eq!(mirrored["vertices"], json!([1.0, 0.0, 6.0, 1.0]));
        let offsets = deform_cell(&mut session, path_id);
        assert_eq!(offsets, json!([[3.0, -5.0], [1.0, -2.0]]));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"node.get","id":bone_id})))["restHead"],
            rested["restHead"]
        );
        ok(&call(
            &mut session,
            &json!({"op":"mesh.mirror","node":path_id,"axis":"y","origin":[2.0, 100.0],"vertices":[0]}),
        ));
        let restored = ok(&call(&mut session, &json!({"op":"node.get","id":path_id})));
        assert_eq!(restored["vertices"], json!([1.0, 4.0, 6.0, 1.0]));
        assert_eq!(deform_cell(&mut session, path_id), json!([[3.0, 5.0], [1.0, -2.0]]));
        ok(&call(&mut session, &json!({"op":"undo"})));
        ok(&call(&mut session, &json!({"op":"undo"})));
        let undone = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(undone["model"], before["model"]);
        assert_eq!(deform_cell_from(&undone["model"], path_id), json!([[3.0, 5.0], [1.0, -2.0]]));
    }

    #[test]
    fn grid_mirror_requires_every_vertex_and_reverses_the_axis() {
        let mut session = crate::Session::new();
        let created = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid"}),
        ));
        let grid_id = created["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0, 1.0],"axisY":[0.0, 2.0]}),
        ));
        let mut model = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"].clone();
        model["param"] = json!([{
            "uuid": 41, "name": "Deform", "is_vec2": false,
            "min": [0.0, 0.0], "max": [1.0, 0.0],
            "axis_points": [[0.0, 1.0], [0.0]],
            "bindings": [{
                "node": grid_id, "param_name": "deform",
                "values": [[
                    [[1.0, 2.0], [3.0, 4.0], [5.0, 6.0], [7.0, 8.0]]
                ], [
                    [[1.0, 2.0], [3.0, 4.0], [5.0, 6.0], [7.0, 8.0]]
                ]],
                "isSet": [[true], [true]]
            }]
        }]);
        ok(&call(&mut session, &json!({"op":"new","model":model})));
        let held = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let partial = call(
            &mut session,
            &json!({"op":"mesh.mirror","node":grid_id,"axis":"x","origin":[0.0, 0.0],"vertices":[1]}),
        );
        assert_eq!(partial["succeeded"], false, "{partial}");
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"}))), held);
        ok(&call(
            &mut session,
            &json!({
                "op":"mesh.mirror","node":grid_id,"axis":"x","origin":[0.0, 0.0],
                "vertices":[0, 1, 2, 3]
            }),
        ));
        let grid = ok(&call(&mut session, &json!({"op":"node.get","id":grid_id})));
        assert_eq!(grid["grid_axis_x"], json!([-1.0, 0.0]));
        assert_eq!(grid["grid_axis_y"], json!([0.0, 2.0]));
        assert_eq!(
            deform_cell(&mut session, grid_id),
            json!([[-3.0, 4.0], [-1.0, 2.0], [-7.0, 8.0], [-5.0, 6.0]])
        );
        ok(&call(
            &mut session,
            &json!({
                "op":"mesh.mirror","node":grid_id,"axis":"x","origin":[0.0, 0.0],
                "vertices":[3, 2, 1, 0]
            }),
        ));
        let again = ok(&call(&mut session, &json!({"op":"node.get","id":grid_id})));
        assert_eq!(again["grid_axis_x"], json!([0.0, 1.0]));
        assert_eq!(again["grid_axis_y"], json!([0.0, 2.0]));
        assert_eq!(
            deform_cell(&mut session, grid_id),
            json!([[1.0, 2.0], [3.0, 4.0], [5.0, 6.0], [7.0, 8.0]])
        );
        ok(&call(&mut session, &json!({"op":"undo"})));
        let undone = ok(&call(&mut session, &json!({"op":"node.get","id":grid_id})));
        assert_eq!(undone["grid_axis_x"], json!([-1.0, 0.0]));
        assert_eq!(
            deform_cell(&mut session, grid_id),
            json!([[-3.0, 4.0], [-1.0, 2.0], [-7.0, 8.0], [-5.0, 6.0]])
        );
        ok(&call(&mut session, &json!({"op":"undo"})));
        let restored = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(restored["model"], held["model"]);
        assert_eq!(restored["canUndo"], false);
    }

    #[test]
    fn vertical_grid_mirror_uses_origin_x_and_both_uses_origin_y() {
        let mut session = crate::Session::new();
        let created = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid"}),
        ));
        let grid_id = created["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0, 3.0],"axisY":[0.0, 4.0]}),
        ));
        ok(&call(
            &mut session,
            &json!({
                "op":"mesh.mirror","node":grid_id,"axis":"y","origin":[1.0, 10.0],
                "vertices":[0, 1, 2, 3]
            }),
        ));
        let vertical = ok(&call(&mut session, &json!({"op":"node.get","id":grid_id})));
        assert_eq!(vertical["grid_axis_x"], json!([0.0, 3.0]));
        assert_eq!(vertical["grid_axis_y"], json!([-2.0, 2.0]));
        ok(&call(&mut session, &json!({"op":"undo"})));
        ok(&call(
            &mut session,
            &json!({
                "op":"mesh.mirror","node":grid_id,"axis":"both","origin":[1.0, 10.0],
                "vertices":[0, 1, 2, 3]
            }),
        ));
        let both = ok(&call(&mut session, &json!({"op":"node.get","id":grid_id})));
        assert_eq!(both["grid_axis_x"], json!([-1.0, 2.0]));
        assert_eq!(both["grid_axis_y"], json!([16.0, 20.0]));
    }

    #[test]
    fn part_vertices_follow_the_same_formula_as_a_path() {
        let mut session = crate::Session::new();
        let mut model = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"].clone();
        model["nodes"]["children"] = json!([{
            "uuid": 4, "name": "Card", "type": "Part", "enabled": true, "zsort": 0.0,
            "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
            "lockToRoot": false, "pinToMesh": false, "children": [],
            "vertices": [[1.0, 4.0], [6.0, 1.0]],
            "uvs": [[0.0, 0.0], [1.0, 0.0]],
            "indices": [0, 1, 0]
        }]);
        ok(&call(&mut session, &json!({"op":"new","model":model})));
        ok(&call(
            &mut session,
            &json!({"op":"mesh.mirror","node":4,"axis":"y","origin":[2.0, 100.0],"vertices":[0]}),
        ));
        let part = ok(&call(&mut session, &json!({"op":"node.get","id":4})));
        // Open stores a renderer mesh. The mirror updates that copy and the arrays,
        // so a later open does not rebuild the mesh from the pre-mirror vertices.
        assert_eq!(part["mesh"]["verts"], json!([1.0, 0.0, 6.0, 1.0]));
        assert_eq!(part["vertices"], json!([[1.0, 0.0], [6.0, 1.0]]));
        assert_eq!(part["uvs"], json!([[0.0, 0.0], [1.0, 0.0]]));
        assert_eq!(part["indices"], json!([0, 1, 0]));
        let mirrored = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"].clone();
        ok(&call(&mut session, &json!({"op":"new","model":mirrored})));
        let reopened = ok(&call(&mut session, &json!({"op":"node.get","id":4})));
        assert_eq!(reopened["mesh"]["verts"], json!([1.0, 0.0, 6.0, 1.0]));
        assert_eq!(reopened["vertices"], json!([[1.0, 0.0], [6.0, 1.0]]));

        let mut flat_only = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"].clone();
        flat_only["nodes"]["children"][0]
            .as_object_mut()
            .unwrap()
            .remove("mesh");
        flat_only["nodes"]["children"][0]["vertices"] = json!([1.0, 4.0, 6.0, 1.0]);
        ok(&call(&mut session, &json!({"op":"new","model":flat_only})));
        ok(&call(
            &mut session,
            &json!({"op":"mesh.mirror","node":4,"axis":"y","origin":[2.0, 100.0],"vertices":[0]}),
        ));
        let flat_part = ok(&call(&mut session, &json!({"op":"node.get","id":4})));
        assert_eq!(flat_part["vertices"], json!([1.0, 0.0, 6.0, 1.0]));

        let mut flat = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"].clone();
        flat["nodes"]["children"][0]["vertices"] = json!([1.0, 4.0, 6.0, 1.0]);
        flat["nodes"]["children"][0]["mesh"] = json!({
            "verts": [1.0, 4.0, 6.0, 1.0],
            "indices": [0, 1, 0],
            "origin": [0.0, 0.0]
        });
        ok(&call(&mut session, &json!({"op":"new","model":flat})));
        ok(&call(
            &mut session,
            &json!({"op":"mesh.mirror","node":4,"axis":"y","origin":[2.0, 100.0],"vertices":[0]}),
        ));
        let preferred = ok(&call(&mut session, &json!({"op":"node.get","id":4})));
        assert_eq!(preferred["mesh"]["verts"], json!([1.0, 0.0, 6.0, 1.0]));
        assert_eq!(preferred["vertices"], json!([1.0, 0.0, 6.0, 1.0]));
    }

    #[test]
    fn mirror_failures_leave_the_snapshot_unchanged() {
        let mut session = crate::Session::new();
        let path = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"PathDeformer","name":"Path"}),
        ));
        let path_id = path["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_vertices","nodes":[path_id],"vertices":[0.0, 1.0]}),
        ));
        let depth_grid = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Depths"}),
        ));
        let depth_id = depth_grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[depth_id],"axisX":[0.0, 1.0],"axisY":[0.0, 1.0]}),
        ));
        let flat_grid = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Flat"}),
        ));
        let flat_id = flat_grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[flat_id],"axisX":[0.0, 1.0],"axisY":[0.0, 1.0]}),
        ));
        let mut model = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"].clone();
        let part_id = insert_part(&mut model);
        stamp_depths(&mut model, depth_id);
        stamp_grid_axes(&mut model, flat_id, &[0.0, 0.0], &[0.0, 1.0]);
        ok(&call(&mut session, &json!({"op":"new","model":model})));
        let held = ok(&call(&mut session, &json!({"op":"snapshot"})));
        for request in [
            json!({"op":"mesh.mirror","node":999,"axis":"x","origin":[0.0, 0.0],"vertices":[0]}),
            json!({"op":"mesh.mirror","node":path_id,"axis":"z","origin":[0.0, 0.0],"vertices":[0]}),
            json!({"op":"mesh.mirror","node":path_id,"axis":"x","origin":[1.0, "no"],"vertices":[0]}),
            json!({"op":"mesh.mirror","node":path_id,"axis":"x","origin":[0.0, 0.0],"vertices":[4]}),
            json!({"op":"mesh.mirror","node":part_id,"axis":"x","origin":[0.0, 0.0],"vertices":[0]}),
            json!({"op":"mesh.mirror","node":depth_id,"axis":"x","origin":[0.0, 0.0],"vertices":[0, 1, 2, 3]}),
            json!({"op":"mesh.mirror","node":flat_id,"axis":"x","origin":[0.0, 0.0],"vertices":[0, 1, 2, 3]}),
        ] {
            let failed = call(&mut session, &request);
            assert_eq!(failed["succeeded"], false, "{failed}");
            assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"}))), held);
        }
    }

    #[test]
    fn overflowed_path_and_malformed_deform_leave_the_snapshot_unchanged() {
        let mut session = crate::Session::new();
        let created = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"PathDeformer","name":"Path"}),
        ));
        let path_id = created["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"deformer.define_vertices","nodes":[path_id],
                "vertices":[1.0e308, 0.0]
            }),
        ));
        let huge = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let overflowed = call(
            &mut session,
            &json!({
                "op":"mesh.mirror","node":path_id,"axis":"x",
                "origin":[1.0e308, 0.0],"vertices":[0]
            }),
        );
        assert_eq!(overflowed["succeeded"], false, "{overflowed}");
        assert!(overflowed["message"].as_str().unwrap().contains("not finite"));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"}))), huge);

        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_vertices","nodes":[path_id],"vertices":[1.0, 4.0]}),
        ));
        let mut model = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"].clone();
        model["param"] = json!([{
            "uuid": 42, "name": "Deform", "is_vec2": false,
            "min": [0.0, 0.0], "max": [1.0, 0.0],
            "axis_points": [[0.0, 1.0], [0.0]],
            "bindings": [{
                "node": path_id, "param_name": "deform",
                "values": [[[[1.0, 2.0]]]],
                "isSet": true
            }]
        }]);
        ok(&call(&mut session, &json!({"op":"new","model":model})));
        let held = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let malformed = call(
            &mut session,
            &json!({
                "op":"mesh.mirror","node":path_id,"axis":"y",
                "origin":[2.0, 0.0],"vertices":[0]
            }),
        );
        assert_eq!(malformed["succeeded"], false, "{malformed}");
        assert!(malformed["message"]
            .as_str()
            .unwrap()
            .contains("deform binding is malformed"));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"}))), held);

        let mut weighted = held["model"].clone();
        weighted["nodes"]["children"].as_array_mut().unwrap().push(json!({
            "uuid": 90, "name": "Rig", "type": "DepthRigRoot", "children": [],
            "bindings": [{
                "target": path_id,
                "sourceBoneUuids": [],
                "vertexWeights": [{"bone": 91, "values": [1.0]}]
            }]
        }));
        ok(&call(&mut session, &json!({"op":"new","model":weighted})));
        let weighted_held = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let weighted_mirror = call(
            &mut session,
            &json!({
                "op":"mesh.mirror","node":path_id,"axis":"x",
                "origin":[0.0, 0.0],"vertices":[0]
            }),
        );
        assert_eq!(weighted_mirror["succeeded"], false, "{weighted_mirror}");
        assert!(weighted_mirror["message"]
            .as_str()
            .unwrap()
            .contains("vertexWeights"));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"}))), weighted_held);
    }

    fn deform_cell(session: &mut crate::Session, node_id: u64) -> Value {
        let model = ok(&call(session, &json!({"op":"snapshot"})))["model"].clone();
        deform_cell_from(&model, node_id)
    }

    fn deform_cell_from(model: &Value, node_id: u64) -> Value {
        model["param"][0]["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["node"] == node_id && binding["param_name"] == "deform")
            .unwrap()["values"][0][0]
            .clone()
    }

    fn stamp_grid_axes(model: &mut Value, grid_id: u64, xs: &[f64], ys: &[f64]) {
        let node = find_child_mut(model.get_mut("nodes").unwrap(), grid_id).unwrap();
        node["grid_axis_x"] = json!(xs);
        node["grid_axis_y"] = json!(ys);
    }

    fn stamp_depths(model: &mut Value, grid_id: u64) {
        let node = find_child_mut(model.get_mut("nodes").unwrap(), grid_id).unwrap();
        node["depths"] = json!([1.0, 2.0, 3.0, 4.0]);
    }

    fn insert_part(model: &mut Value) -> u64 {
        let part_id = 80u64;
        model["nodes"]["children"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "uuid": part_id, "name": "Part", "type": "Part", "enabled": true, "zsort": 0.0,
                "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                "lockToRoot": false, "pinToMesh": false, "children": []
            }));
        part_id
    }

    fn find_child_mut(node: &mut Value, id: u64) -> Option<&mut Value> {
        if node.get("uuid").and_then(Value::as_u64) == Some(id) {
            return Some(node);
        }
        let children = node.get_mut("children")?.as_array_mut()?;
        for child in children {
            if let Some(found) = find_child_mut(child, id) {
                return Some(found);
            }
        }
        None
    }
}
