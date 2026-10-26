use crate::depth_resample;
use crate::depths::finite_depth_or_zero;
use crate::nodes::{self, find_node};
use crate::{finite_number, validate_model};
use serde_json::{json, Map, Value};
use std::collections::HashSet;

const MAX_DEFORM_OUTPUT_VALUES: usize = 1_000_000;
const AXIS_TOLERANCE: f64 = 1e-4;
const DEFAULT_AXIS: [f64; 2] = [-0.5, 0.5];
const PENDULUM_DAMPING: f64 = 1.0;
const PENDULUM_RESTORE: f64 = 300.0;
const PENDULUM_GRAVITY: f64 = 9.8;
const PENDULUM_INPUT_SCALE: f64 = 0.01;
const PENDULUM_PROPAGATE_SCALE: f64 = 0.2;

const DEFORMER_KEYS: &[&str] = &[
    "grid_axis_x",
    "grid_axis_y",
    "formation",
    "dynamic",
    "curve_type",
    "vertices",
    "dynamic_deformation",
    "translate_children",
    "physics",
    "physics_only",
    "mesh",
    "uvs",
    "indices",
];

pub fn stamp_new_node(node: &mut Value, class_name: &str) {
    let Some(object) = node.as_object_mut() else {
        return;
    };
    match class_name {
        "GridDeformer" => stamp_grid(object, DEFAULT_AXIS.to_vec(), DEFAULT_AXIS.to_vec(), false),
        "PathDeformer" => stamp_path(object, Vec::new(), "Spline", false, None),
        "MeshGroup" => {
            object.insert("dynamic_deformation".into(), json!(false));
            object.insert("translate_children".into(), json!(true));
            object.insert("mesh".into(), empty_mesh());
        }
        _ => {}
    }
}

pub fn apply_conversion(node: &mut Value, from_type: &str, to_type: &str) -> Result<(), String> {
    let grid_dynamic = node
        .get("dynamic")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mesh_dynamic = node
        .get("dynamic_deformation")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let translate_children = node
        .get("translate_children")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let path_vertices = flat_vertices(node.get("vertices"));
    let grid_vertices = grid_vertices_from_node(node);
    let mesh_vertices = mesh_flat_vertices(node);
    let existing_mesh = node.get("mesh").cloned();
    let part_mesh = if matches!(from_type, "Part" | "Mask" | "DynamicComposite") {
        Some(parse_part_mesh(node)?)
    } else {
        None
    };

    let object = node.as_object_mut().ok_or("node must be an object")?;
    for key in DEFORMER_KEYS {
        object.remove(*key);
    }

    match to_type {
        "GridDeformer" => {
            let source = if from_type == "GridDeformer" {
                grid_vertices
            } else if from_type == "PathDeformer" {
                path_vertices
            } else if mesh_vertices.is_some() {
                mesh_vertices
            } else {
                None
            };
            let (xs, ys) = source
                .and_then(|points| adopt_exact_grid(&points))
                .unwrap_or_else(|| (DEFAULT_AXIS.to_vec(), DEFAULT_AXIS.to_vec()));
            stamp_grid(object, xs, ys, false);
        }
        "PathDeformer" => {
            let vertices = if from_type == "PathDeformer" {
                path_vertices.unwrap_or_default()
            } else if from_type == "GridDeformer" {
                grid_vertices.unwrap_or_default()
            } else {
                mesh_vertices.unwrap_or_default()
            };
            stamp_path(object, vertices, "Spline", false, None);
        }
        "MeshGroup" => {
            let dynamic = if from_type == "GridDeformer" {
                grid_dynamic
            } else if from_type == "MeshGroup" {
                mesh_dynamic
            } else {
                false
            };
            let translate = if from_type == "MeshGroup" {
                translate_children
            } else {
                true
            };
            object.insert("dynamic_deformation".into(), json!(dynamic));
            object.insert("translate_children".into(), json!(translate));
            let mesh = if from_type == "GridDeformer" {
                grid_vertices
                    .filter(|points| !points.is_empty())
                    .map(|points| mesh_from_grid_points(&points))
                    .transpose()?
                    .unwrap_or_else(empty_mesh)
            } else if from_type == "MeshGroup" {
                existing_mesh.unwrap_or_else(empty_mesh)
            } else {
                empty_mesh()
            };
            object.insert("mesh".into(), mesh);
        }
        "Part" => {
            let mesh = part_mesh.unwrap_or_else(PartMesh::empty);
            write_part_mesh(object, &mesh, existing_mesh);
        }
        _ => {}
    }
    if is_depth_mapped_type(from_type) && is_depth_mapped_type(to_type) {
        let count = conversion_vertex_count(object, to_type)?;
        resize_converted_depths(object, count)?;
    }
    Ok(())
}

fn is_depth_mapped_type(node_type: &str) -> bool {
    matches!(node_type, "GridDeformer" | "PathDeformer")
}

fn conversion_vertex_count(object: &Map<String, Value>, node_type: &str) -> Result<usize, String> {
    match node_type {
        "PathDeformer" => depth_resample::path_vertex_count(object.get("vertices")),
        "GridDeformer" => {
            let xs = object
                .get("grid_axis_x")
                .and_then(Value::as_array)
                .ok_or("GridDeformer vertices are not a rectangular grid")?;
            let ys = object
                .get("grid_axis_y")
                .and_then(Value::as_array)
                .ok_or("GridDeformer vertices are not a rectangular grid")?;
            xs.len()
                .checked_mul(ys.len())
                .ok_or_else(|| "GridDeformer vertices are not a rectangular grid".to_string())
        }
        _ => Err("target must be a GridDeformer or PathDeformer".into()),
    }
}

fn resize_converted_depths(
    object: &mut Map<String, Value>,
    vertex_count: usize,
) -> Result<(), String> {
    let Some(raw) = object.get("depths") else {
        return Ok(());
    };
    if raw.is_null() {
        return Ok(());
    }
    let values = raw.as_array().ok_or("depths must be an array of numbers")?;
    let mut depths = Vec::with_capacity(values.len());
    for value in values {
        let number = value
            .as_f64()
            .filter(|number| number.is_finite())
            .ok_or("depths must be an array of numbers")?;
        depths.push(finite_depth_or_zero(number));
    }
    depths.truncate(vertex_count);
    depths.resize(vertex_count, 0.0);
    object.insert("depths".into(), json!(depths));
    Ok(())
}

#[derive(Clone)]
pub struct PartMesh {
    pub vertices: Vec<[f64; 2]>,
    pub uvs: Vec<[f64; 2]>,
    pub indices: Vec<u64>,
    pub origin: [f64; 2],
}

impl PartMesh {
    fn empty() -> Self {
        Self {
            vertices: Vec::new(),
            uvs: Vec::new(),
            indices: Vec::new(),
            origin: [0.0, 0.0],
        }
    }
}

pub fn parse_part_mesh(node: &Value) -> Result<PartMesh, String> {
    let has_top_level = node.get("vertices").is_some()
        || node.get("uvs").is_some()
        || node.get("indices").is_some();
    if let Some(mesh) = node.get("mesh") {
        if !mesh.is_object() {
            return Err("mesh must be an object".into());
        }
        if !has_top_level {
            return part_from_renderer_mesh(mesh);
        }
        let origin = mesh_origin(Some(mesh))?;
        let vertices = pair_array(node.get("vertices"), "vertices")?;
        let uvs = pair_array(node.get("uvs"), "uvs")?;
        if uvs.len() != vertices.len() {
            return Err("uv count must match the vertex count".into());
        }
        let indices = index_array(node.get("indices"), vertices.len())?;
        return Ok(PartMesh {
            vertices,
            uvs,
            indices,
            origin,
        });
    }
    if !has_top_level {
        return Ok(PartMesh::empty());
    }
    let vertices = pair_array(node.get("vertices"), "vertices")?;
    let uvs = pair_array(node.get("uvs"), "uvs")?;
    if uvs.len() != vertices.len() {
        return Err("uv count must match the vertex count".into());
    }
    let indices = index_array(node.get("indices"), vertices.len())?;
    Ok(PartMesh {
        vertices,
        uvs,
        indices,
        origin: [0.0, 0.0],
    })
}

pub fn ensure_part_arrays(model: &mut Value) {
    fn walk(node: &mut Value) {
        if node.get("type").and_then(Value::as_str) == Some("Part") {
            let blank_arrays = ["vertices", "uvs", "indices"].iter().all(|key| match node.get(*key) {
                None => true,
                Some(Value::Array(items)) => items.is_empty(),
                Some(_) => false,
            });
            let has_mesh = node.get("mesh").is_some();
            let mesh_has_verts = node
                .get("mesh")
                .and_then(|mesh| mesh.get("verts"))
                .and_then(Value::as_array)
                .is_some_and(|verts| !verts.is_empty());
            if blank_arrays && mesh_has_verts {
                if let Some(mesh) = node.get("mesh") {
                    if mesh.get("verts").is_some() {
                        if let Ok(parsed) = part_from_renderer_mesh(mesh) {
                            if let Some(object) = node.as_object_mut() {
                                object
                                    .entry("vertices".to_owned())
                                    .or_insert_with(|| pairs_json(&parsed.vertices));
                                object
                                    .entry("uvs".to_owned())
                                    .or_insert_with(|| pairs_json(&parsed.uvs));
                                object
                                    .entry("indices".to_owned())
                                    .or_insert_with(|| json!(parsed.indices));
                                if let Some(mesh_object) =
                                    object.get_mut("mesh").and_then(Value::as_object_mut)
                                {
                                    mesh_object
                                        .entry("uvs".to_owned())
                                        .or_insert_with(|| json!(flat_pairs(&parsed.uvs)));
                                    mesh_object
                                        .entry("indices".to_owned())
                                        .or_insert_with(|| json!(parsed.indices));
                                }
                            }
                        }
                    }
                }
            } else if blank_arrays && !has_mesh {
                if let Some(object) = node.as_object_mut() {
                    write_part_mesh(object, &PartMesh::empty(), None);
                }
            } else if !has_mesh {
                if let Ok(mesh) = parse_part_mesh(node) {
                    if let Some(object) = node.as_object_mut() {
                        object.insert("mesh".into(), renderer_mesh(&mesh));
                    }
                }
            } else if let Ok(parsed) = parse_part_mesh(node) {
                let mesh_disagrees = node.get("mesh").is_some_and(|mesh| {
                    !same_flat(mesh.get("verts"), &parsed.vertices)
                        || !same_flat(mesh.get("uvs"), &parsed.uvs)
                        || mesh.get("indices") != Some(&json!(parsed.indices))
                });
                if mesh_disagrees {
                    if let Some(mesh_object) = node.get_mut("mesh").and_then(Value::as_object_mut) {
                        mesh_object.insert("verts".into(), json!(flat_pairs(&parsed.vertices)));
                        mesh_object.insert("uvs".into(), json!(flat_pairs(&parsed.uvs)));
                        mesh_object.insert("indices".into(), json!(parsed.indices));
                        mesh_object
                            .entry("origin".to_owned())
                            .or_insert_with(|| json!(parsed.origin));
                    }
                }
            }
        }
        if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
            for child in children {
                walk(child);
            }
        }
    }
    if let Some(nodes) = model.get_mut("nodes") {
        walk(nodes);
    }
}

fn write_part_mesh(object: &mut Map<String, Value>, mesh: &PartMesh, retained: Option<Value>) {
    object.insert("vertices".into(), pairs_json(&mesh.vertices));
    object.insert("uvs".into(), pairs_json(&mesh.uvs));
    object.insert("indices".into(), json!(mesh.indices));
    let mut mesh_object = match retained {
        Some(Value::Object(fields)) => fields,
        _ => Map::new(),
    };
    mesh_object.insert("verts".into(), json!(flat_pairs(&mesh.vertices)));
    mesh_object.insert("uvs".into(), json!(flat_pairs(&mesh.uvs)));
    mesh_object.insert("indices".into(), json!(mesh.indices));
    mesh_object.insert("origin".into(), json!(mesh.origin));
    object.insert("mesh".into(), Value::Object(mesh_object));
}

fn renderer_mesh(mesh: &PartMesh) -> Value {
    json!({
        "verts": flat_pairs(&mesh.vertices),
        "uvs": flat_pairs(&mesh.uvs),
        "indices": mesh.indices,
        "origin": mesh.origin,
    })
}

pub(crate) fn mesh_origin(mesh: Option<&Value>) -> Result<[f64; 2], String> {
    let Some(origin) = mesh.and_then(|mesh| mesh.get("origin")) else {
        return Ok([0.0, 0.0]);
    };
    let values = origin
        .as_array()
        .ok_or("mesh.origin must be an array")?;
    if values.len() != 2 {
        return Err("mesh.origin has an invalid component count".into());
    }
    let x = values[0]
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or("mesh.origin must be finite")?;
    let y = values[1]
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or("mesh.origin must be finite")?;
    Ok([x, y])
}

fn same_flat(existing: Option<&Value>, points: &[[f64; 2]]) -> bool {
    let Some(values) = existing.and_then(Value::as_array) else {
        return false;
    };
    if values.len() != points.len() * 2 {
        return false;
    }
    values.chunks(2).zip(points).all(|(pair, point)| {
        pair.first().and_then(Value::as_f64) == Some(point[0])
            && pair.get(1).and_then(Value::as_f64) == Some(point[1])
    })
}

fn flat_pairs(points: &[[f64; 2]]) -> Vec<f64> {
    let mut flat = Vec::with_capacity(points.len() * 2);
    for point in points {
        flat.push(point[0]);
        flat.push(point[1]);
    }
    flat
}

fn pairs_json(points: &[[f64; 2]]) -> Value {
    Value::Array(points.iter().map(|point| json!([point[0], point[1]])).collect())
}

fn part_from_renderer_mesh(mesh: &Value) -> Result<PartMesh, String> {
    let vertices = flat_to_pairs(mesh.get("verts"), "mesh.verts")?;
    let uvs = if mesh.get("uvs").is_some() {
        flat_to_pairs(mesh.get("uvs"), "mesh.uvs")?
    } else {
        vertices.clone()
    };
    if uvs.len() != vertices.len() {
        return Err("uv count must match the vertex count".into());
    }
    let indices = if mesh.get("indices").is_some() {
        index_array(mesh.get("indices"), vertices.len())?
    } else {
        Vec::new()
    };
    Ok(PartMesh {
        vertices,
        uvs,
        indices,
        origin: mesh_origin(Some(mesh))?,
    })
}

fn flat_to_pairs(value: Option<&Value>, name: &str) -> Result<Vec<[f64; 2]>, String> {
    let Some(value) = value else {
        return Err(format!("{name} must be an array"));
    };
    let items = value.as_array().ok_or_else(|| format!("{name} must be an array"))?;
    if items.len() % 2 != 0 {
        return Err(format!("{name} length must be even"));
    }
    let mut points = Vec::with_capacity(items.len() / 2);
    for pair in items.chunks(2) {
        let x = pair[0].as_f64().filter(|n| n.is_finite()).ok_or("vertex must be finite")?;
        let y = pair[1].as_f64().filter(|n| n.is_finite()).ok_or("vertex must be finite")?;
        points.push([x, y]);
    }
    Ok(points)
}

fn pair_array(value: Option<&Value>, name: &str) -> Result<Vec<[f64; 2]>, String> {
    let Some(value) = value else {
        return Err(format!("{name} must be an array"));
    };
    let items = value.as_array().ok_or_else(|| format!("{name} must be an array"))?;
    let mut points = Vec::with_capacity(items.len());
    for item in items {
        let pair = item.as_array().ok_or_else(|| format!("{name} entries must be [x, y]"))?;
        if pair.len() != 2 {
            return Err(format!("{name} entries must be [x, y]"));
        }
        let x = pair[0].as_f64().filter(|n| n.is_finite()).ok_or("vertex must be finite")?;
        let y = pair[1].as_f64().filter(|n| n.is_finite()).ok_or("vertex must be finite")?;
        points.push([x, y]);
    }
    Ok(points)
}

fn index_array(value: Option<&Value>, vertex_count: usize) -> Result<Vec<u64>, String> {
    let Some(value) = value else {
        return Err("indices must be an array".into());
    };
    let items = value.as_array().ok_or("indices must be an array")?;
    if items.len() % 3 != 0 {
        return Err("indices length must be a multiple of 3".into());
    }
    let mut indices = Vec::with_capacity(items.len());
    for item in items {
        let index = item.as_u64().ok_or("index past the vertex list")?;
        if index > u64::from(u16::MAX) {
            return Err("index must be a 16-bit value".into());
        }
        if index as usize >= vertex_count {
            return Err("index past the vertex list".into());
        }
        indices.push(index);
    }
    Ok(indices)
}

pub fn define_vertices(
    model: &Value,
    node_ids: &[u32],
    vertices: &[f64],
) -> Result<(Value, Vec<u32>), String> {
    let points = parse_control_points(vertices)?;
    let targets = deformable_targets(model, node_ids)?;
    for id in &targets {
        reject_locked_geometry(model, *id, true)?;
        let node = find_node(model, *id).ok_or_else(|| format!("node {id} was not found"))?;
        let node_type = node_type(node);
        if node_type == "GridDeformer" && !count_grid(&points) {
            return Err("GridDeformer vertices must form a complete rectangular grid".into());
        }
        if node_type == "GridDeformer" {
            reject_unresampleable_grid_depths(model, *id)?;
        } else if node_type == "PathDeformer" {
            reject_unresampleable_path_depths(model, *id)?;
        }
        crate::bindings::reject_vertex_weight_resize(model, *id, points.len())?;
    }
    let mut candidate = model.clone();
    let mut pending = Vec::new();
    for id in targets.iter().copied() {
        let refresh = crate::dirty::refreshes_bound_target(model, id);
        let node = nodes::find_node_mut(&mut candidate, id)
            .ok_or_else(|| format!("node {id} was not found"))?;
        let node_type = node_type(node).to_owned();
        let old_points = node_geometry_points(node);
        if node_type == "GridDeformer" {
            let (xs, ys) = adopt_exact_grid(&points)
                .unwrap_or_else(|| (DEFAULT_AXIS.to_vec(), DEFAULT_AXIS.to_vec()));
            let old_vertices = capture_grid_depth_vertices(node)?;
            let dynamic = node
                .get("dynamic")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let object = node.as_object_mut().ok_or("node must be an object")?;
            stamp_grid(object, xs, ys, dynamic);
            rebuffer_captured_depths(node, old_vertices)?;
        } else {
            let old_depth_vertex_count = capture_path_depth_vertex_count(node)?;
            let dynamic = node
                .get("dynamic_deformation")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let physics = node.get("physics").cloned();
            let curve = node
                .get("curve_type")
                .and_then(Value::as_str)
                .unwrap_or("Spline")
                .to_owned();
            let object = node.as_object_mut().ok_or("node must be an object")?;
            stamp_path(object, points.clone(), &curve, dynamic, physics);
            rebuffer_captured_path_depths(node, old_depth_vertex_count)?;
        }
        if !refresh {
            let node = nodes::find_node(&candidate, id)
                .ok_or_else(|| format!("node {id} was not found"))?;
            pending.push((id, node_type, old_points, node_geometry_points(node)));
        }
    }
    for (id, node_type, old_points, new_points) in pending {
        resize_undriven_deform(&mut candidate, id, &node_type, &old_points, &new_points)?;
    }
    validate_model(&candidate)?;
    Ok((candidate, targets))
}

pub fn define_grid(
    model: &Value,
    node_ids: &[u32],
    axis_x: &[f64],
    axis_y: &[f64],
) -> Result<(Value, Vec<u32>), String> {
    if axis_x.len() < 2 {
        return Err("axisX must contain at least 2 values".into());
    }
    if axis_y.len() < 2 {
        return Err("axisY must contain at least 2 values".into());
    }
    if axis_x
        .iter()
        .chain(axis_y.iter())
        .any(|value| !value.is_finite())
    {
        return Err("grid axes must contain finite values".into());
    }
    let targets = grid_targets(model, node_ids)?;
    for id in &targets {
        reject_locked_geometry(model, *id, true)?;
        reject_unresampleable_grid_depths(model, *id)?;
    }
    let xs = normalize_axis(axis_x);
    let ys = normalize_axis(axis_y);
    let (xs, ys) = if xs.len() >= 2 && ys.len() >= 2 {
        (xs, ys)
    } else {
        (DEFAULT_AXIS.to_vec(), DEFAULT_AXIS.to_vec())
    };
    let new_count = xs.len() * ys.len();
    for id in &targets {
        crate::bindings::reject_vertex_weight_resize(model, *id, new_count)?;
    }
    let mut candidate = model.clone();
    let mut pending = Vec::new();
    for id in targets.iter().copied() {
        let refresh = crate::dirty::refreshes_bound_target(model, id);
        let node = nodes::find_node_mut(&mut candidate, id)
            .ok_or_else(|| format!("node {id} was not found"))?;
        let old_points = node_geometry_points(node);
        let old_vertices = capture_grid_depth_vertices(node)?;
        let dynamic = node
            .get("dynamic")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let object = node.as_object_mut().ok_or("node must be an object")?;
        stamp_grid(object, xs.clone(), ys.clone(), dynamic);
        rebuffer_captured_depths(node, old_vertices)?;
        if !refresh {
            let node = nodes::find_node(&candidate, id)
                .ok_or_else(|| format!("node {id} was not found"))?;
            pending.push((id, old_points, node_geometry_points(node)));
        }
    }
    for (id, old_points, new_points) in pending {
        resize_undriven_deform(
            &mut candidate,
            id,
            "GridDeformer",
            &old_points,
            &new_points,
        )?;
    }
    validate_model(&candidate)?;
    Ok((candidate, targets))
}

/// Adds a grid line outside each side of GridDeformer `id` that `reach` (min x, min y, max x, max y in the grid's
/// space) passes by more than the axis tolerance, and gives each added line the keyed offsets of the old edge it
/// continues. The old area moves as before, and so does a child outside it, which was clamped to that edge.
pub(crate) fn extend_grid(model: &Value, id: u32, reach: [f64; 4]) -> Result<Option<Value>, String> {
    let node = nodes::find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    if node.get("type").and_then(Value::as_str) != Some("GridDeformer") {
        return Err(format!("node {id} is not a GridDeformer"));
    }
    let mut axes = [grid_axis(node, "grid_axis_x")?, grid_axis(node, "grid_axis_y")?];
    let old = [axes[0].len(), axes[1].len()];
    let mut inner = [0, 0];
    for (side, (low, high)) in [(reach[0], reach[2]), (reach[1], reach[3])].into_iter().enumerate() {
        let axis = &mut axes[side];
        if high > axis[axis.len() - 1] + AXIS_TOLERANCE {
            axis.push(high);
        }
        if low < axis[0] - AXIS_TOLERANCE {
            axis.insert(0, low);
            inner[side] = 1;
        }
    }
    if axes[0].len() == old[0] && axes[1].len() == old[1] {
        return Ok(None);
    }
    let (mut candidate, _) = define_grid(model, &[id], &axes[0], &axes[1])?;
    let columns = axes[0].len();
    let count = columns * axes[1].len();
    let source = |index: usize| {
        let column = (index % columns).clamp(inner[0], inner[0] + old[0] - 1);
        (index / columns).clamp(inner[1], inner[1] + old[1] - 1) * columns + column
    };
    visit_deform_bindings(&mut candidate, id, |x_axis, y_axis, repeat, binding| {
        let mut cells = parse_binding_cells(binding, id, x_axis, y_axis)?;
        if !cells.is_set.iter().flatten().any(|set| *set) {
            return Ok(());
        }
        for (x, column) in cells.offsets.iter_mut().enumerate() {
            for (y, offsets) in column.iter_mut().enumerate() {
                if cells.is_set[x][y] && offsets.len() == count {
                    *offsets = (0..count).map(|index| offsets[source(index)]).collect();
                }
            }
        }
        fill_unset_offsets(&mut cells.offsets, &cells.is_set, x_axis, y_axis, repeat)?;
        write_binding_cells(binding, &cells)
    })?;
    validate_model(&candidate)?;
    Ok(Some(candidate))
}

/// Adds the keyed deformation of GridDeformer `id` to the deform bindings of `children`, key by key, on the same
/// parameters (issue #167). A grid that is not dynamic moves a child point by a bilinear mix of its offsets, which is
/// linear in them, and every interpolation mode is linear in the key values, so with the same mode on both bindings
/// the children reproduce the grid's motion, inside the grid and where a point is clamped to its edge.
pub(crate) fn transfer_grid_to_children(
    model: &Value,
    id: u32,
    children: &[crate::scene_deform::GridChild],
) -> Result<Value, String> {
    let node = nodes::find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    if node.get("type").and_then(Value::as_str) != Some("GridDeformer") {
        return Err(format!("node {id} is not a GridDeformer"));
    }
    if node.get("dynamic").and_then(Value::as_bool).unwrap_or(false) {
        return Err(format!("GridDeformer {id} is dynamic, so its motion depends on each child's own offsets"));
    }
    if crate::dirty::refreshes_bound_target(model, id) {
        return Err(format!("GridDeformer {id} is driven by a rig"));
    }
    let (axis_x, axis_y) = (grid_axis(node, "grid_axis_x")?, grid_axis(node, "grid_axis_y")?);
    let rest = node_geometry_points(node);
    let mut candidate = model.clone();
    let parameters = candidate.get_mut("param").and_then(Value::as_array_mut).map(std::mem::take).unwrap_or_default();
    let mut moved = Vec::new();
    for mut parameter in parameters {
        transfer_parameter(&mut parameter, id, &axis_x, &axis_y, &rest, children)?;
        moved.push(parameter);
    }
    if let Some(slot) = candidate.get_mut("param") {
        *slot = Value::Array(moved);
    }
    validate_model(&candidate)?;
    Ok(candidate)
}

fn transfer_parameter(
    parameter: &mut Value,
    id: u32,
    axis_x: &[f64],
    axis_y: &[f64],
    rest: &[[f64; 2]],
    children: &[crate::scene_deform::GridChild],
) -> Result<(), String> {
    for group in parameter.get_mut("children").and_then(Value::as_array_mut).into_iter().flatten() {
        transfer_parameter(group, id, axis_x, axis_y, rest, children)?;
    }
    let bindings = parameter.get("bindings").and_then(Value::as_array).cloned().unwrap_or_default();
    if let Some(other) = bindings.iter().find(|binding| {
        binding.get("node").and_then(Value::as_u64) == Some(u64::from(id)) && !deform_binding_matches(binding, id)
    }) {
        return Err(format!("GridDeformer {id} has a {} binding; only deform bindings can move to the children",
            other.get("param_name").and_then(Value::as_str).unwrap_or("?")));
    }
    // A parameter-driven transform moves where the grid samples a child, which offsets per key cannot carry.
    if let Some(child) = children.iter().find(|child| bindings.iter().any(|binding| {
        binding.get("node").and_then(Value::as_u64) == Some(u64::from(child.id))
            && binding.get("param_name").and_then(Value::as_str).is_some_and(|name| name.starts_with("transform"))
    })) {
        return Err(format!("child {} has a transform binding", child.id));
    }
    let Some(grid_binding) = bindings.iter().find(|binding| deform_binding_matches(binding, id)).cloned() else {
        return Ok(());
    };
    let grid_binding = &grid_binding;
    let (x_axis, y_axis) = parameter_axes(parameter, id)?;
    let mode = |binding: &Value| {
        binding.get("interpolate_mode").and_then(Value::as_str).unwrap_or("Linear").to_ascii_lowercase()
    };
    // Evaluation reads the offsets stored in every cell, set or not, so the stored ones are added as they are.
    let grid = parse_binding_cells(grid_binding, id, &x_axis, &y_axis)?;
    let mut bindings = bindings;
    for child in children {
        let index = match bindings.iter().position(|binding| deform_binding_matches(binding, child.id)) {
            Some(index) if mode(&bindings[index]) != mode(grid_binding) => {
                return Err(format!("child {} interpolates this parameter {} and the grid {}", child.id,
                    mode(&bindings[index]), mode(grid_binding)));
            }
            Some(index) => index,
            None => {
                let points = child.displacement(axis_x, axis_y, rest, rest)?.len();
                let interpolation = grid_binding.get("interpolate_mode").cloned().unwrap_or_else(|| json!("Linear"));
                let mut binding = json!({"node": child.id, "param_name": "deform", "interpolate_mode": interpolation});
                write_binding_cells(&mut binding, &BindingCells {
                    offsets: vec![vec![vec![[0.0, 0.0]; points]; y_axis.len()]; x_axis.len()],
                    is_set: vec![vec![false; y_axis.len()]; x_axis.len()],
                })?;
                bindings.push(binding);
                bindings.len() - 1
            }
        };
        let mut cells = parse_binding_cells(&bindings[index], child.id, &x_axis, &y_axis)?;
        // Each cell below holds the exact sum. A later edit refills unset keys from the set ones, so unless the two
        // bindings set the same keys, every key is set.
        let keyed = cells.is_set.iter().flatten().any(|set| *set);
        let is_set = if !keyed || cells.is_set == grid.is_set {
            grid.is_set.clone()
        } else {
            vec![vec![true; y_axis.len()]; x_axis.len()]
        };
        for x in 0..x_axis.len() {
            for y in 0..y_axis.len() {
                let deformed: Vec<[f64; 2]> = rest.iter().zip(&grid.offsets[x][y])
                    .map(|(point, offset)| [point[0] + offset[0], point[1] + offset[1]]).collect();
                let added = child.displacement(axis_x, axis_y, rest, &deformed)?;
                if added.len() != cells.offsets[x][y].len() {
                    return Err(format!("child {} deform binding does not match its points", child.id));
                }
                for (offset, extra) in cells.offsets[x][y].iter_mut().zip(added) {
                    *offset = [offset[0] + extra[0], offset[1] + extra[1]];
                }
            }
        }
        cells.is_set = is_set;
        write_binding_cells(&mut bindings[index], &cells)?;
    }
    if let Some(object) = parameter.as_object_mut() {
        object.insert("bindings".into(), Value::Array(bindings));
    }
    Ok(())
}

/// Warp Bezier handles as an edit-time lattice (issue #160). `controls` is a row-major (3·bx+1) × (3·by+1) grid of
/// cubic Bezier control points in the GridDeformer's space for bx × by patches spanning its axes. Each grid point
/// moves to the patch surface at its normalised axis position; the result is the offsets from the rest grid.
pub(crate) fn bezier_offsets(node: &Value, divisions: [usize; 2], controls: &[[f64; 2]]) -> Result<Vec<[f64; 2]>, String> {
    let (axis_x, axis_y) = (grid_axis(node, "grid_axis_x")?, grid_axis(node, "grid_axis_y")?);
    let [bx, by] = divisions;
    if !(1..=16).contains(&bx) || !(1..=16).contains(&by) {
        return Err("divisions must be 1 to 16 patches per axis".into());
    }
    let columns = 3 * bx + 1;
    if controls.len() != columns * (3 * by + 1) || controls.iter().flatten().any(|value| !value.is_finite()) {
        return Err(format!("controls must hold {} finite points", columns * (3 * by + 1)));
    }
    let bernstein = |t: f64| [(1.0 - t).powi(3), 3.0 * t * (1.0 - t).powi(2), 3.0 * t * t * (1.0 - t), t.powi(3)];
    let patch = |position: f64, count: usize| {
        let scaled = (position * count as f64).clamp(0.0, count as f64);
        let index = (scaled.floor() as usize).min(count - 1);
        (index * 3, bernstein(scaled - index as f64))
    };
    let normal = |axis: &[f64], value: f64| (value - axis[0]) / (axis[axis.len() - 1] - axis[0]);
    let mut offsets = Vec::with_capacity(axis_x.len() * axis_y.len());
    for y in &axis_y {
        let (row, wy) = patch(normal(&axis_y, *y), by);
        for x in &axis_x {
            let (column, wx) = patch(normal(&axis_x, *x), bx);
            let mut point = [0.0, 0.0];
            for (j, weight_y) in wy.iter().enumerate() {
                for (i, weight_x) in wx.iter().enumerate() {
                    let control = controls[(row + j) * columns + column + i];
                    point[0] += weight_x * weight_y * control[0];
                    point[1] += weight_x * weight_y * control[1];
                }
            }
            offsets.push([point[0] - x, point[1] - y]);
        }
    }
    Ok(offsets)
}

/// The control lattice for `bezier_offsets` read from the grid as `offsets` deform it: each control point is the
/// deformed grid at its normalised position, bilinear within a cell. An undeformed grid gives evenly spaced
/// controls, which reproduce it exactly.
pub(crate) fn bezier_controls(node: &Value, divisions: [usize; 2], offsets: &[[f64; 2]]) -> Result<Vec<[f64; 2]>, String> {
    let (axis_x, axis_y) = (grid_axis(node, "grid_axis_x")?, grid_axis(node, "grid_axis_y")?);
    let [bx, by] = divisions;
    if !(1..=16).contains(&bx) || !(1..=16).contains(&by) {
        return Err("divisions must be 1 to 16 patches per axis".into());
    }
    if offsets.len() != axis_x.len() * axis_y.len() {
        return Err("the offsets do not match the grid".into());
    }
    let at = |x: usize, y: usize| {
        let offset = offsets[y * axis_x.len() + x];
        [axis_x[x] + offset[0], axis_y[y] + offset[1]]
    };
    let locate = |axis: &[f64], position: f64| {
        let value = axis[0] + position * (axis[axis.len() - 1] - axis[0]);
        let cell = axis.windows(2).position(|pair| value <= pair[1]).unwrap_or(axis.len() - 2);
        (cell, ((value - axis[cell]) / (axis[cell + 1] - axis[cell])).clamp(0.0, 1.0))
    };
    let mut controls = Vec::with_capacity((3 * bx + 1) * (3 * by + 1));
    for j in 0..=3 * by {
        let (cy, v) = locate(&axis_y, j as f64 / (3 * by) as f64);
        for i in 0..=3 * bx {
            let (cx, u) = locate(&axis_x, i as f64 / (3 * bx) as f64);
            let corner = |dx: usize, dy: usize| at(cx + dx, cy + dy);
            let mut point = [0.0, 0.0];
            for (weight, corner) in [((1.0 - u) * (1.0 - v), corner(0, 0)), (u * (1.0 - v), corner(1, 0)),
                ((1.0 - u) * v, corner(0, 1)), (u * v, corner(1, 1))] {
                point[0] += weight * corner[0];
                point[1] += weight * corner[1];
            }
            controls.push(point);
        }
    }
    Ok(controls)
}

/// The fixed humanoid deformer tree of automatic deformer generation (issue #163), parents before children. The
/// manual shows the tree only as a picture, so these roles are inferred, not compared.
pub(crate) const HUMANOID_ROLES: &[(&str, Option<&str>)] = &[
    ("Body", None),
    ("Arm L", Some("Body")),
    ("Arm R", Some("Body")),
    ("Neck", Some("Body")),
    ("Head", Some("Body")),
    ("Face", Some("Head")),
    ("Eye L", Some("Face")),
    ("Eye R", Some("Face")),
    ("Brow L", Some("Face")),
    ("Brow R", Some("Face")),
    ("Mouth", Some("Face")),
    ("Nose", Some("Face")),
    ("Front Hair", Some("Head")),
    ("Side Hair L", Some("Head")),
    ("Side Hair R", Some("Head")),
    ("Back Hair", Some("Head")),
];

/// Builds the humanoid GridDeformer tree under `parent`: role `r` becomes GridDeformer "Warp r" holding the meshes
/// `roles[r]` and the grids of its generated child roles, sized to the rest bounds of every mesh under it plus
/// `margin` (a fraction of the larger extent) with `divisions` cells per axis. A role with nothing under it is not
/// created and is returned in the second list, the manual's "[Not generated]". Returns the model and, per created
/// grid, its role and UUID.
pub(crate) fn generate_humanoid(
    model: &Value,
    parent: u32,
    roles: &std::collections::BTreeMap<String, Vec<u32>>,
    margin: f64,
    divisions: usize,
) -> Result<(Value, Vec<Value>, Vec<&'static str>), String> {
    if !margin.is_finite() || !(0.0..=1.0).contains(&margin) || !(1..=16).contains(&divisions) {
        return Err("margin must be 0 to 1 and divisions 1 to 16".into());
    }
    let mut seen = HashSet::new();
    for (role, ids) in roles {
        if !HUMANOID_ROLES.iter().any(|(name, _)| name == role) {
            return Err(format!("unknown role {}; use one of {}", crate::mcp::echoed([role.as_str()]),
                HUMANOID_ROLES.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ")));
        }
        for id in ids {
            let node = find_node(model, *id).ok_or_else(|| format!("node {id} was not found"))?;
            if !matches!(node["type"].as_str(), Some("Part" | "Mask" | "MeshGroup")) {
                return Err(format!("node {id} is not a mesh"));
            }
            if !seen.insert(*id) {
                return Err(format!("node {id} is given to two roles"));
            }
        }
    }
    if seen.is_empty() {
        return Err("give at least one mesh to a role".into());
    }
    let has_meshes = |role: &str| roles.get(role).is_some_and(|ids| !ids.is_empty());
    let mut generated: HashSet<&str> = HashSet::new();
    for (role, _) in HUMANOID_ROLES.iter().rev() {
        let has_children = HUMANOID_ROLES.iter().any(|(child, up)| *up == Some(*role) && generated.contains(child));
        if has_meshes(role) || has_children {
            generated.insert(role);
        }
    }
    let mut candidate = model.clone();
    let mut grids: Vec<(&str, u32)> = Vec::new();
    for (role, up) in HUMANOID_ROLES.iter().filter(|(role, _)| generated.contains(role)) {
        let parent = up.and_then(|up| grids.iter().find(|(name, _)| *name == up)).map_or(parent, |(_, id)| *id);
        let (next, created) =
            nodes::create_nodes(&candidate, &[parent], "GridDeformer", None, Some(&format!("Warp {role}")), None)?;
        let id = created.first().and_then(|node| node["uuid"].as_u64()).and_then(|id| u32::try_from(id).ok())
            .ok_or("no GridDeformer was created")?;
        candidate = next;
        if let Some(ids) = roles.get(*role).filter(|ids| !ids.is_empty()) {
            candidate = nodes::move_nodes(&candidate, ids, id, None)?;
        }
        grids.push((role, id));
    }
    for (_, id) in grids.iter().rev() {
        let grid = find_node(&candidate, *id).ok_or("a created GridDeformer was not found")?;
        let inverse = crate::bone_deform::invert(crate::bone_deform::world_matrix(&candidate, *id)?)?;
        let mut bounds = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        let mut meshes = Vec::new();
        collect_meshes(grid, &mut meshes);
        for mesh in meshes {
            let mesh_id = mesh["uuid"].as_u64().and_then(|id| u32::try_from(id).ok()).ok_or("a mesh has no UUID")?;
            let to_grid = crate::bone_deform::mul_mat(inverse, crate::bone_deform::world_matrix(&candidate, mesh_id)?);
            for point in crate::scene_deform::geometry_points(mesh, mesh_id)? {
                let [x, y, _] = crate::bone_deform::transform_point(to_grid, [point[0], point[1], 0.0])?;
                bounds = [bounds[0].min(x), bounds[1].min(y), bounds[2].max(x), bounds[3].max(y)];
            }
        }
        if !bounds.iter().all(|value| value.is_finite()) {
            return Err("a role's meshes have no vertices".into());
        }
        let pad = (margin * (bounds[2] - bounds[0]).max(bounds[3] - bounds[1])).max(1.0);
        let axis = |low: f64, high: f64| {
            (0..=divisions).map(|i| low - pad + (high - low + 2.0 * pad) * i as f64 / divisions as f64).collect::<Vec<_>>()
        };
        candidate = define_grid(&candidate, &[*id], &axis(bounds[0], bounds[2]), &axis(bounds[1], bounds[3]))?.0;
    }
    let not_generated = HUMANOID_ROLES.iter().map(|(role, _)| *role).filter(|role| !generated.contains(role)).collect();
    let created = grids.iter().map(|(role, id)| json!({"role": role, "node": id})).collect();
    Ok((candidate, created, not_generated))
}

fn collect_meshes<'a>(node: &'a Value, found: &mut Vec<&'a Value>) {
    for child in node.get("children").and_then(Value::as_array).into_iter().flatten() {
        if matches!(child["type"].as_str(), Some("Part" | "Mask" | "MeshGroup")) {
            found.push(child);
        }
        collect_meshes(child, found);
    }
}

/// The grid edge a sway hangs from (issue #181).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Fulcrum {
    Top,
    Bottom,
    Left,
    Right,
}

/// Sway settings in the manual's terms. `horizontal` is across the fulcrum axis and `vertical` along it, in the
/// grid's units; `None` takes the inferred default.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Sway {
    pub fulcrum: Fulcrum,
    pub horizontal: Option<f64>,
    pub vertical: f64,
    pub softness: f64,
    pub zoom: f64,
    pub flip: bool,
}

/// The offsets of GridDeformer `node` at the low and high key of a three-key sway (the middle key is the rest
/// grid), and the horizontal magnitude used. The manual gives no formula, so the rule is inferred, not compared:
/// a point at normalised distance `d` from the fulcrum edge has weight `w = d^(1 + softness)`; it moves
/// `±horizontal·w` across the axis, `vertical·w` toward the fulcrum, and `zoom·w` times its distance from the
/// grid's centre line across the axis. The default horizontal magnitude is 15 % of the grid's length along the axis.
pub(crate) fn sway_offsets(node: &Value, sway: &Sway) -> Result<(f64, [Vec<[f64; 2]>; 2]), String> {
    let (axis_x, axis_y) = (grid_axis(node, "grid_axis_x")?, grid_axis(node, "grid_axis_y")?);
    if [sway.vertical, sway.softness, sway.zoom].iter().chain(sway.horizontal.as_ref()).any(|v| !v.is_finite())
        || sway.softness < 0.0
    {
        return Err("sway settings must be finite and softness at least 0".into());
    }
    let span = |axis: &[f64]| (axis[0], axis[axis.len() - 1] - axis[0]);
    let ((x0, width), (y0, height)) = (span(&axis_x), span(&axis_y));
    let vertical_axis = matches!(sway.fulcrum, Fulcrum::Top | Fulcrum::Bottom);
    let (length, across_center) = if vertical_axis { (height, x0 + width / 2.0) } else { (width, y0 + height / 2.0) };
    let horizontal = sway.horizontal.unwrap_or(0.15 * length);
    let mut keys = [Vec::new(), Vec::new()];
    for y in &axis_y {
        for x in &axis_x {
            let (u, v) = ((x - x0) / width, (y - y0) / height);
            let (distance, toward, across) = match sway.fulcrum {
                Fulcrum::Top => (v, [0.0, -1.0], [1.0, 0.0]),
                Fulcrum::Bottom => (1.0 - v, [0.0, 1.0], [1.0, 0.0]),
                Fulcrum::Left => (u, [-1.0, 0.0], [0.0, 1.0]),
                Fulcrum::Right => (1.0 - u, [1.0, 0.0], [0.0, 1.0]),
            };
            let weight = distance.clamp(0.0, 1.0).powf(1.0 + sway.softness);
            let spread = sway.zoom * weight * (if vertical_axis { x } else { y } - across_center);
            for (side, key) in [-1.0, 1.0].into_iter().zip(keys.iter_mut()) {
                let swing = if sway.flip { -side } else { side } * horizontal * weight + spread;
                let lift = sway.vertical * weight;
                key.push([across[0] * swing + toward[0] * lift, across[1] * swing + toward[1] * lift]);
            }
        }
    }
    Ok((horizontal, keys))
}

/// Low and high key offsets that turn a face grid around its vertical (`horizontal` true) or horizontal axis
/// (issue #181): the outline stays, the inside bulges toward the turn like a sphere. The high key turns right or
/// up, unless `flip`. `amount` is the centre's shift as a fraction of the half width or height, below 0.5 so
/// the grid keeps its order.
pub(crate) fn turn_offsets(node: &Value, horizontal: bool, amount: f64, flip: bool) -> Result<[Vec<[f64; 2]>; 2], String> {
    if !(0.0..0.5).contains(&amount) {
        return Err("amount must be at least 0 and below 0.5".into());
    }
    let (axis_x, axis_y) = (grid_axis(node, "grid_axis_x")?, grid_axis(node, "grid_axis_y")?);
    let normal = |axis: &[f64], at: f64| {
        let (low, high) = (axis[0], axis[axis.len() - 1]);
        (2.0 * (at - low) / (high - low) - 1.0, (high - low) / 2.0)
    };
    let direction = if flip { -1.0 } else { 1.0 };
    let mut keys = [Vec::new(), Vec::new()];
    for y in &axis_y {
        for x in &axis_x {
            let ((u, half_width), (v, half_height)) = (normal(&axis_x, *x), normal(&axis_y, *y));
            let (along, across, half) = if horizontal { (u, v, half_width) } else { (v, u, -half_height) };
            let shift = direction * amount * half * (1.0 - along * along) * (1.0 - across * across).max(0.0).sqrt();
            for (side, key) in [-1.0, 1.0].into_iter().zip(keys.iter_mut()) {
                key.push(if horizontal { [side * shift, 0.0] } else { [0.0, side * shift] });
            }
        }
    }
    Ok(keys)
}

fn grid_axis(node: &Value, key: &str) -> Result<Vec<f64>, String> {
    node.get(key)
        .and_then(Value::as_array)
        .and_then(|values| values.iter().map(Value::as_f64).collect::<Option<Vec<f64>>>())
        .filter(|values| values.len() >= 2)
        .ok_or_else(|| format!("{key} must hold at least 2 numbers"))
}

pub fn define_mesh(
    model: &Value,
    node_ids: &[u32],
    request: &Value,
) -> Result<(Value, Vec<u32>), String> {
    let vertices = required_number_array(request, "vertices")?;
    let indices = required_index_array(request, "indices")?;
    parse_control_points(&vertices)?;
    if indices.is_empty() {
        return Err("Vertices or indices empty".into());
    }
    if indices.len() % 3 != 0 {
        return Err("indices length must be a multiple of 3 (triangles)".into());
    }
    let point_count = vertices.len() / 2;
    if indices.iter().any(|index| *index as usize >= point_count) {
        return Err("index out of range in indices array".into());
    }
    let targets = mesh_group_targets(model, node_ids)?;
    for id in &targets {
        reject_locked_geometry(model, *id, false)?;
    }
    let uvs = request
        .get("uvs")
        .map(|value| required_numbers(value, "uvs"))
        .transpose()?;
    if let Some(uvs) = &uvs {
        if uvs.len() != vertices.len() {
            return Err("uvs length must match vertices length".into());
        }
        if uvs.iter().any(|value| !value.is_finite()) {
            return Err("uvs must contain finite values".into());
        }
    }
    let origin = request
        .get("origin")
        .map(|value| parse_pair(value, "origin"))
        .transpose()?;
    let mut candidate = model.clone();
    for id in targets.iter().copied() {
        let node = nodes::find_node_mut(&mut candidate, id)
            .ok_or_else(|| format!("node {id} was not found"))?;
        let mut mesh = match node.get("mesh") {
            None => Map::new(),
            Some(Value::Object(fields)) => fields.clone(),
            Some(_) => return Err(format!("node {id} mesh must be an object")),
        };
        if uvs.is_none() {
            if let Some(existing_uvs) = mesh.get("uvs") {
                let existing_uvs = required_numbers(existing_uvs, "mesh.uvs")?;
                if !existing_uvs.is_empty() && existing_uvs.len() != vertices.len() {
                    return Err(format!("node {id} mesh.uvs length must match vertices length"));
                }
            }
        }
        mesh.insert("verts".into(), json!(vertices));
        mesh.insert("indices".into(), json!(indices));
        if let Some(uvs) = &uvs {
            mesh.insert("uvs".into(), json!(uvs));
        }
        if let Some(origin) = origin {
            mesh.insert("origin".into(), json!(origin));
        }
        node.as_object_mut()
            .ok_or("node must be an object")?
            .insert("mesh".into(), Value::Object(mesh));
    }
    validate_model(&candidate)?;
    Ok((candidate, targets))
}

pub(crate) fn set_deform(model: &mut Value, request: &Value) -> Result<Vec<u32>, String> {
    let parameter_id = request
        .get("parameter")
        .and_then(Value::as_u64)
        .and_then(|id| u32::try_from(id).ok())
        .ok_or("request.parameter must be an unsigned 32-bit id")?;
    let target_ids = request
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or("request.nodes must be an array")?
        .iter()
        .map(|id| {
            id.as_u64()
                .and_then(|id| u32::try_from(id).ok())
                .ok_or_else(|| "request.nodes must contain unsigned 32-bit ids".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if target_ids.is_empty() {
        return Err("request.nodes must not be empty".into());
    }
    let mut unique_ids = HashSet::new();
    if target_ids.iter().any(|id| !unique_ids.insert(*id)) {
        return Err("request.nodes contains a duplicate id".into());
    }
    let key = deform_key_index_pair(request.get("key"))?;
    let values = required_number_array(request, "values")?
        .into_iter()
        .map(|value| {
            if value < -f64::from(f32::MAX) || value > f64::from(f32::MAX) {
                return Err("values must contain finite f32 values".into());
            }
            Ok(f64::from(value as f32))
        })
        .collect::<Result<Vec<_>, String>>()?;
    if values.len() % 2 != 0 {
        return Err("values length must be even (x,y pairs)".into());
    }
    let offsets = values
        .chunks_exact(2)
        .map(|pair| [pair[0], pair[1]])
        .collect::<Vec<_>>();

    let parameters = model
        .get("param")
        .and_then(Value::as_array)
        .ok_or("model.param must be an array")?;
    let mut leaves = Vec::new();
    collect_deform_parameters(parameters, &mut leaves)?;
    let mut parameter_ids = HashSet::new();
    let mut selected_parameter = None;
    for parameter in leaves {
        let id = parameter
            .get("uuid")
            .and_then(Value::as_u64)
            .and_then(|id| u32::try_from(id).ok())
            .ok_or("parameter.uuid must be an unsigned 32-bit id")?;
        if !parameter_ids.insert(id) {
            return Err(format!("duplicate parameter id {id}"));
        }
        if id == parameter_id {
            selected_parameter = Some(parameter);
        }
    }
    let parameter = selected_parameter
        .ok_or_else(|| format!("request refers to unknown leaf parameter {parameter_id}"))?;
    let (x_axis, y_axis, is_vec2) = deform_key_axes(parameter)?;
    if key[0] >= x_axis.len() || key[1] >= y_axis.len() || (!is_vec2 && key[1] != 0) {
        return Err("request.key must identify an existing parameter keypoint index".into());
    }

    let target_nodes = target_ids
        .iter()
        .map(|id| {
            let node = find_node(model, *id)
                .ok_or_else(|| format!("request targets missing node {id}"))?;
            if crate::dirty::refreshes_bound_target(model, *id) {
                return Err(format!("node {id} is driven by a rig and cannot accept a manual deform key"));
            }
            Ok((*id, node))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut remaining_values = MAX_DEFORM_OUTPUT_VALUES;
    for _ in &target_nodes {
        consume_deform_output_budget(
            &mut remaining_values,
            x_axis.len(),
            y_axis.len(),
            offsets.len(),
        )?;
    }

    let mut point_counts = Vec::with_capacity(target_nodes.len());
    for (id, node) in target_nodes {
        if node_type(node) == "GridDeformer" {
            let x_count = node
                .get("grid_axis_x")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("node {id} has no deform geometry"))?
                .len();
            let y_count = node
                .get("grid_axis_y")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("node {id} has no deform geometry"))?
                .len();
            let grid_point_count = x_count
                .checked_mul(y_count)
                .ok_or_else(|| format!("node {id} grid geometry point count overflow"))?;
            if offsets.len() != grid_point_count {
                return Err(format!("values must contain exactly {grid_point_count} deform offset pairs for node {id}"));
            }
        }
        let point_count = node_geometry_points(node).len();
        if point_count == 0 {
            return Err(format!("node {id} has no deform geometry"));
        }
        if offsets.len() != point_count {
            return Err(format!("values must contain exactly {point_count} deform offset pairs for node {id}"));
        }
        point_counts.push(point_count);
    }

    let mut candidate = model.clone();
    let parameters = candidate
        .get_mut("param")
        .and_then(Value::as_array_mut)
        .ok_or("model.param must be an array")?;
    let parameter = find_deform_parameter_mut(parameters, parameter_id)
        .ok_or_else(|| format!("request refers to unknown leaf parameter {parameter_id}"))?;
    let repeat = crate::evaluation::parameter_repeats(parameter);
    let bindings = parameter
        .as_object_mut()
        .ok_or("parameter must be an object")?
        .entry("bindings".to_owned())
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or("parameter.bindings must be an array")?;

    for (id, point_count) in target_ids.iter().copied().zip(point_counts) {
        let matches = bindings
            .iter()
            .enumerate()
            .filter(|(_, binding)| deform_binding_matches(binding, id))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            return Err(format!("node {id} has duplicate deform bindings"));
        }
        let binding_index = if let Some(index) = matches.first() {
            *index
        } else {
            let cells = BindingCells {
                offsets: vec![vec![vec![[0.0, 0.0]; point_count]; y_axis.len()]; x_axis.len()],
                is_set: vec![vec![false; y_axis.len()]; x_axis.len()],
            };
            let mut binding = json!({
                "node": id,
                "param_name": "deform",
                "interpolate_mode": "Linear",
            });
            write_binding_cells(&mut binding, &cells)?;
            bindings.push(binding);
            bindings.len() - 1
        };
        let binding = &mut bindings[binding_index];
        if let Some(mode) = binding.get("interpolate_mode") {
            let mode = mode
                .as_str()
                .ok_or_else(|| format!("node {id} deform interpolation mode must be a string"))?;
            if !matches!(mode.to_ascii_lowercase().as_str(), "linear" | "nearest" | "cubic") {
                return Err(format!("node {id} deform interpolation {mode:?} is unsupported"));
            }
        }
        update_deform_key(
            binding,
            id,
            key,
            &offsets,
            &x_axis,
            &y_axis,
            false,
            UnsetFillOverflow::Error,
            repeat,
        )?;
    }
    *model = candidate;
    Ok(target_ids)
}

fn consume_deform_output_budget(
    remaining: &mut usize,
    x_count: usize,
    y_count: usize,
    point_count: usize,
) -> Result<(), String> {
    let components = point_count
        .checked_mul(2)
        .ok_or("deform binding exceeds the output size limit")?;
    let count = x_count
        .checked_mul(y_count)
        .and_then(|cells| cells.checked_mul(components))
        .ok_or("deform binding exceeds the output size limit")?;
    *remaining = remaining
        .checked_sub(count)
        .ok_or("deform binding exceeds the output size limit")?;
    Ok(())
}

fn check_deform_output_budget(
    x_count: usize,
    y_count: usize,
    point_count: usize,
) -> Result<(), String> {
    let mut remaining = MAX_DEFORM_OUTPUT_VALUES;
    consume_deform_output_budget(&mut remaining, x_count, y_count, point_count)
}

fn collect_deform_parameters<'a>(parameters: &'a [Value], output: &mut Vec<&'a Value>) -> Result<(), String> {
    for parameter in parameters {
        if parameter.get("groupUUID").is_some() {
            let children = parameter
                .get("children")
                .and_then(Value::as_array)
                .ok_or("parameter group children must be an array")?;
            collect_deform_parameters(children, output)?;
        } else {
            output.push(parameter);
        }
    }
    Ok(())
}

fn find_deform_parameter_mut(parameters: &mut [Value], id: u32) -> Option<&mut Value> {
    for parameter in parameters {
        if parameter.get("groupUUID").is_some() {
            if let Some(children) = parameter.get_mut("children").and_then(Value::as_array_mut) {
                if let Some(found) = find_deform_parameter_mut(children, id) {
                    return Some(found);
                }
            }
        } else if parameter.get("uuid").and_then(Value::as_u64) == Some(u64::from(id)) {
            return Some(parameter);
        }
    }
    None
}

fn deform_key_index_pair(value: Option<&Value>) -> Result<[usize; 2], String> {
    let pair = value
        .and_then(Value::as_array)
        .filter(|pair| pair.len() == 2)
        .ok_or("request.key must be a two-element array of axis indexes")?;
    let parse = |value: &Value| {
        value
            .as_u64()
            .and_then(|index| usize::try_from(index).ok())
            .ok_or_else(|| "request.key entries must be unsigned axis indexes".to_string())
    };
    Ok([parse(&pair[0])?, parse(&pair[1])?])
}

pub(crate) fn deform_key_axes(parameter: &Value) -> Result<(Vec<f64>, Vec<f64>, bool), String> {
    let axes = parameter
        .get("axis_points")
        .and_then(Value::as_array)
        .filter(|axes| axes.len() == 2)
        .ok_or("parameter.axis_points must contain two arrays")?;
    let parse_axis = |value: &Value, name: &str, min_len: usize| -> Result<Vec<f64>, String> {
        let values = value
            .as_array()
            .ok_or_else(|| format!("{name} must be an array"))?
            .iter()
            .map(|value| finite_number(value, name))
            .collect::<Result<Vec<_>, _>>()?;
        if values.len() < min_len {
            return Err(format!("{name} must have axis points"));
        }
        if values.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(format!("{name} must be strictly increasing"));
        }
        Ok(values)
    };
    let x_axis = parse_axis(&axes[0], "parameter.axis_points[0]", 2)?;
    let raw_y_axis = parse_axis(&axes[1], "parameter.axis_points[1]", 1)?;
    let is_vec2 = parameter
        .get("is_vec2")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if x_axis.len() > 256
        || raw_y_axis.len() > 256
        || x_axis.len().saturating_mul(raw_y_axis.len()) > 256 * 256
    {
        return Err("parameter axis grid exceeds the supported size".into());
    }
    if is_vec2 && raw_y_axis.len() < 2 {
        return Err("2D parameter needs at least two Y axis points".into());
    }
    let y_axis = if is_vec2 { raw_y_axis } else { vec![0.0] };
    Ok((x_axis, y_axis, is_vec2))
}

pub fn set_property(
    model: &Value,
    node_ids: &[u32],
    property: &str,
    value: &Value,
) -> Result<Value, String> {
    if node_ids.is_empty() {
        return Err("at least one node UUID is required".into());
    }
    let mut targets = Vec::new();
    for id in node_ids {
        let node = find_node(model, *id).ok_or_else(|| format!("node {id} was not found"))?;
        if property_matches(node_type(node), property) {
            targets.push(*id);
        }
    }
    if targets.is_empty() {
        return Err(format!("no nodes accept deformer property {property}"));
    }
    let mut candidate = model.clone();
    for id in targets {
        let node = nodes::find_node_mut(&mut candidate, id)
            .ok_or_else(|| format!("node {id} was not found"))?;
        apply_property(node, property, value)?;
    }
    validate_model(&candidate)?;
    Ok(candidate)
}

pub fn has_blocked_deformer_payload(node: &Value, to_type: &str) -> bool {
    let preserves_depths = is_depth_mapped_type(node_type(node)) && is_depth_mapped_type(to_type);
    blocked_payload(node, true, !preserves_depths)
        || (preserves_depths
            && (node.get("depth-ops").is_some() || node.get("depth-op-base-depths").is_some()))
}

pub fn has_blocked_part_convert_payload(node: &Value) -> bool {
    // A Drawable texture list is copied onto the Part. Deform, depth, weld,
    // and mask-binding payloads stay outside this conversion.
    blocked_payload(node, false, true)
}

fn has_blocked_vertex_edit_payload(node: &Value, rebuffer_depths: bool) -> bool {
    let kind = node_type(node);
    let can_rebuffer_depths = rebuffer_depths && is_depth_mapped_type(kind);
    blocked_payload(node, true, !can_rebuffer_depths)
        || (can_rebuffer_depths
            && kind == "PathDeformer"
            && (node.get("depth-ops").is_some() || node.get("depth-op-base-depths").is_some()))
}

fn blocked_payload(node: &Value, block_textures: bool, block_depths: bool) -> bool {
    (block_textures && nonempty_array(node.get("textures")))
        || node.get("deformation").is_some()
        || (block_depths
            && (node.get("depths").is_some()
                || node.get("depth-ops").is_some()
                || node.get("depth-op-base-depths").is_some()))
        || nonempty_array(node.get("weldedLinks"))
        || nonempty_array(node.get("masks"))
        || nonempty_array(node.get("masked_by"))
}

pub fn reject_deform_bindings(model: &Value, id: u32) -> Result<(), String> {
    if deform_binding_targets(model, id) {
        return Err(format!(
            "node {id} has a deform parameter binding and is outside Rust Core mode"
        ));
    }
    Ok(())
}

fn deform_binding_targets(model: &Value, id: u32) -> bool {
    fn walk(list: &[Value], id: u32) -> bool {
        list.iter().any(|parameter| {
            parameter
                .get("bindings")
                .and_then(Value::as_array)
                .is_some_and(|bindings| {
                    bindings.iter().any(|binding| {
                        binding.get("param_name").and_then(Value::as_str) == Some("deform")
                            && binding.get("node").and_then(Value::as_u64) == Some(u64::from(id))
                    })
                })
                || parameter
                    .get("children")
                    .and_then(Value::as_array)
                    .is_some_and(|children| walk(children, id))
        })
    }
    model
        .get("param")
        .and_then(Value::as_array)
        .is_some_and(|list| walk(list, id))
}

fn reject_locked_geometry(model: &Value, id: u32, allow_deform_binding: bool) -> Result<(), String> {
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    if has_blocked_vertex_edit_payload(node, true) {
        return Err(format!(
            "node {id} deform geometry is outside Rust Core mode"
        ));
    }
    if !allow_deform_binding {
        reject_deform_bindings(model, id)?;
    }
    Ok(())
}

pub(crate) fn has_deform_binding(model: &Value, id: u32) -> bool {
    deform_binding_targets(model, id)
}

pub(crate) fn node_geometry_points(node: &Value) -> Vec<[f64; 2]> {
    let flat = match node_type(node) {
        "GridDeformer" => grid_vertices_from_node(node),
        "PathDeformer" => flat_vertices(node.get("vertices")),
        "MeshGroup" | "Part" | "Composite" | "DynamicComposite" | "Mask" => {
            mesh_flat_vertices(node)
        }
        _ => None,
    };
    flat.map(|values| pair_points(&values)).unwrap_or_default()
}

pub(crate) fn resize_undriven_deform(
    model: &mut Value,
    id: u32,
    node_type: &str,
    old_points: &[[f64; 2]],
    new_points: &[[f64; 2]],
) -> Result<(), String> {
    if !has_deform_binding(model, id) {
        return Ok(());
    }
    visit_deform_bindings(model, id, |x_axis, y_axis, repeat, binding| {
        resize_binding(binding, id, node_type, old_points, new_points, x_axis, y_axis, repeat)
    })
}

pub(crate) fn retarget_undriven_convert(
    model: &mut Value,
    id: u32,
    to_type: &str,
    old_points: &[[f64; 2]],
    new_points: &[[f64; 2]],
) -> Result<(), String> {
    if !has_deform_binding(model, id) {
        return Ok(());
    }
    visit_deform_bindings(model, id, |x_axis, y_axis, repeat, binding| {
        if to_type == "GridDeformer" {
            remap_binding_to_grid(binding, id, old_points, new_points, x_axis, y_axis, repeat)
        } else {
            // setTarget keeps the row. A non-grid target does not remap offsets.
            parse_binding_cells(binding, id, x_axis, y_axis).map(|_| ())
        }
    })
}

fn visit_deform_bindings(
    model: &mut Value,
    id: u32,
    mut visit: impl FnMut(&[f64], &[f64], bool, &mut Value) -> Result<(), String>,
) -> Result<(), String> {
    let Some(parameters) = model.get_mut("param").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    visit_parameter_list(parameters, id, &mut visit)
}

fn visit_parameter_list(
    parameters: &mut [Value],
    id: u32,
    visit: &mut impl FnMut(&[f64], &[f64], bool, &mut Value) -> Result<(), String>,
) -> Result<(), String> {
    for parameter in parameters.iter_mut() {
        let child_count = parameter
            .get("children")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        let matched = {
            let bindings = parameter.get("bindings").and_then(Value::as_array);
            bindings
                .map(|bindings| {
                    bindings
                        .iter()
                        .enumerate()
                        .filter_map(|(index, binding)| {
                            deform_binding_matches(binding, id).then_some(index)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        if !matched.is_empty() {
            let (x_axis, y_axis) = parameter_axes(parameter, id)?;
            let repeat = crate::evaluation::parameter_repeats(parameter);
            for index in matched {
                visit(&x_axis, &y_axis, repeat, binding_at_mut(parameter, index)?)?;
            }
        }
        if child_count == 0 {
            continue;
        }
        let children = parameter
            .get_mut("children")
            .and_then(Value::as_array_mut)
            .ok_or("parameter children must be an array")?;
        visit_parameter_list(children, id, visit)?;
    }
    Ok(())
}

fn binding_at_mut(parameter: &mut Value, index: usize) -> Result<&mut Value, String> {
    parameter
        .get_mut("bindings")
        .and_then(Value::as_array_mut)
        .and_then(|bindings| bindings.get_mut(index))
        .ok_or_else(|| "deform binding is malformed".to_string())
}

fn deform_binding_matches(binding: &Value, id: u32) -> bool {
    binding.get("param_name").and_then(Value::as_str) == Some("deform")
        && binding.get("node").and_then(Value::as_u64) == Some(u64::from(id))
}

fn parameter_axes(parameter: &Value, id: u32) -> Result<(Vec<f64>, Vec<f64>), String> {
    let axes = parameter
        .get("axis_points")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed(id))?;
    let x_axis = axes
        .first()
        .ok_or_else(|| malformed(id))
        .and_then(|axis| finite_axis(axis, id))?;
    if x_axis.is_empty() {
        return Err(malformed(id));
    }
    let is_vec2 = parameter
        .get("is_vec2")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let y_axis = if is_vec2 {
        finite_axis(axes.get(1).ok_or_else(|| malformed(id))?, id)?
    } else {
        vec![0.0]
    };
    if y_axis.is_empty() {
        return Err(malformed(id));
    }
    Ok((x_axis, y_axis))
}

fn finite_axis(axis: &Value, id: u32) -> Result<Vec<f64>, String> {
    let values = axis.as_array().ok_or_else(|| malformed(id))?;
    values
        .iter()
        .map(|value| {
            value
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or_else(|| malformed(id))
        })
        .collect()
}

fn resize_binding(
    binding: &mut Value,
    id: u32,
    node_type: &str,
    old_points: &[[f64; 2]],
    new_points: &[[f64; 2]],
    x_axis: &[f64],
    y_axis: &[f64],
    repeat: bool,
) -> Result<(), String> {
    let mut cells = parse_binding_cells(binding, id, x_axis, y_axis)?;
    if node_type == "PathDeformer" && cells.is_set.iter().flatten().any(|set| *set) {
        return Err(format!("node {id} path deform transfer cannot match D"));
    }
    let new_len = new_points.len();
    let any_set = cells.is_set.iter().flatten().any(|set| *set);
    for x in 0..cells.offsets.len() {
        for y in 0..cells.offsets[x].len() {
            if node_type == "GridDeformer" && cells.is_set[x][y] {
                cells.offsets[x][y] =
                    transfer_grid_offsets(old_points, new_points, &cells.offsets[x][y]);
            } else if any_set {
                cells.offsets[x][y] = resize_offsets(&cells.offsets[x][y], new_len);
            } else {
                cells.offsets[x][y] = vec![[0.0, 0.0]; new_len];
            }
        }
    }
    if any_set {
        fill_unset_offsets(&mut cells.offsets, &cells.is_set, x_axis, y_axis, repeat)?;
    }
    write_binding_cells(binding, &cells)
}

fn remap_binding_to_grid(
    binding: &mut Value,
    id: u32,
    old_points: &[[f64; 2]],
    new_points: &[[f64; 2]],
    x_axis: &[f64],
    y_axis: &[f64],
    repeat: bool,
) -> Result<(), String> {
    let mut cells = parse_binding_cells(binding, id, x_axis, y_axis)?;
    let remap = build_position_remap(old_points, new_points);
    if remap.is_empty() {
        return Ok(());
    }
    for x in 0..cells.offsets.len() {
        for y in 0..cells.offsets[x].len() {
            if cells.offsets[x][y].len() != remap.len() {
                cells.offsets[x][y] = vec![[0.0, 0.0]; remap.len()];
                cells.is_set[x][y] = false;
            } else {
                let mut reordered = vec![[0.0, 0.0]; remap.len()];
                for (old_index, new_index) in remap.iter().copied().enumerate() {
                    reordered[new_index] = cells.offsets[x][y][old_index];
                }
                cells.offsets[x][y] = reordered;
            }
        }
    }
    fill_unset_offsets(&mut cells.offsets, &cells.is_set, x_axis, y_axis, repeat)?;
    write_binding_cells(binding, &cells)
}

struct BindingCells {
    offsets: Vec<Vec<Vec<[f64; 2]>>>,
    is_set: Vec<Vec<bool>>,
}

#[derive(Clone, Copy)]
pub(crate) enum UnsetFillOverflow {
    Error,
    Defer,
}

fn parse_binding_cells(
    binding: &Value,
    id: u32,
    x_axis: &[f64],
    y_axis: &[f64],
) -> Result<BindingCells, String> {
    let values = binding
        .get("values")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed(id))?;
    let flags = binding
        .get("isSet")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed(id))?;
    if values.len() != x_axis.len() || flags.len() != x_axis.len() {
        return Err(malformed(id));
    }
    let mut offsets = Vec::with_capacity(values.len());
    let mut is_set = Vec::with_capacity(flags.len());
    for (column, flag_column) in values.iter().zip(flags.iter()) {
        let column = column.as_array().ok_or_else(|| malformed(id))?;
        let flag_column = flag_column.as_array().ok_or_else(|| malformed(id))?;
        if column.len() != y_axis.len() || flag_column.len() != y_axis.len() {
            return Err(malformed(id));
        }
        let mut offset_column = Vec::with_capacity(column.len());
        let mut set_column = Vec::with_capacity(flag_column.len());
        for (cell, flag) in column.iter().zip(flag_column.iter()) {
            offset_column.push(parse_offsets(cell, id)?);
            set_column.push(match flag {
                Value::Bool(set) => *set,
                Value::Null => false,
                _ => return Err(malformed(id)),
            });
        }
        offsets.push(offset_column);
        is_set.push(set_column);
    }
    Ok(BindingCells { offsets, is_set })
}

fn parse_offsets(cell: &Value, id: u32) -> Result<Vec<[f64; 2]>, String> {
    match cell {
        Value::Null => Ok(Vec::new()),
        Value::Array(points) => points
            .iter()
            .map(|point| {
                let pair = point.as_array().ok_or_else(|| malformed(id))?;
                if pair.len() != 2 {
                    return Err(malformed(id));
                }
                let x = pair[0]
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| malformed(id))?;
                let y = pair[1]
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| malformed(id))?;
                Ok([x, y])
            })
            .collect(),
        _ => Err(malformed(id)),
    }
}

fn write_binding_cells(binding: &mut Value, cells: &BindingCells) -> Result<(), String> {
    let object = binding
        .as_object_mut()
        .ok_or_else(|| "deform binding is malformed".to_string())?;
    object.insert(
        "values".into(),
        Value::Array(
            cells
                .offsets
                .iter()
                .map(|column| {
                    Value::Array(
                        column
                            .iter()
                            .map(|offsets| {
                                Value::Array(offsets.iter().copied().map(|pair| json!(pair)).collect())
                            })
                            .collect(),
                    )
                })
                .collect(),
        ),
    );
    object.insert(
        "isSet".into(),
        Value::Array(
            cells
                .is_set
                .iter()
                .map(|column| Value::Array(column.iter().copied().map(Value::Bool).collect()))
                .collect(),
        ),
    );
    Ok(())
}

pub(crate) fn update_deform_key(
    binding: &mut Value,
    id: u32,
    key: [usize; 2],
    offsets: &[[f64; 2]],
    x_axis: &[f64],
    y_axis: &[f64],
    replace_lengths: bool,
    overflow: UnsetFillOverflow,
    repeat: bool,
) -> Result<(), String> {
    if key[0] >= x_axis.len() || key[1] >= y_axis.len() {
        return Err("request.key must identify an existing parameter keypoint index".into());
    }
    let mut cells = parse_binding_cells(binding, id, x_axis, y_axis)?;
    for x in 0..cells.offsets.len() {
        for y in 0..cells.offsets[x].len() {
            if cells.is_set[x][y] && cells.offsets[x][y].len() != offsets.len() {
                if !replace_lengths {
                    return Err(format!("node {id} deform authored value length does not match geometry"));
                }
            }
        }
    }
    let unset_cells = cells.is_set.iter().flatten().filter(|set| !**set).count();
    let current_is_unset = !cells.is_set[key[0]][key[1]];
    // shortcut: oversized generated writes defer new cache expansion; large sparse rigs still need lazy off-key evaluation.
    let skipped_current = if current_is_unset { 1 } else { 0 };
    let fill_unset = match check_deform_output_budget(
        unset_cells.saturating_sub(skipped_current),
        1,
        offsets.len(),
    ) {
        Ok(()) => true,
        Err(error) => match overflow {
            UnsetFillOverflow::Error => return Err(error),
            UnsetFillOverflow::Defer => false,
        },
    };
    for x in 0..cells.offsets.len() {
        for y in 0..cells.offsets[x].len() {
            if !cells.is_set[x][y] && (x, y) != (key[0], key[1]) {
                if fill_unset {
                    cells.offsets[x][y] = vec![[0.0, 0.0]; offsets.len()];
                }
            }
        }
    }
    cells.offsets[key[0]][key[1]] = offsets.to_vec();
    cells.is_set[key[0]][key[1]] = true;
    if fill_unset && cells.is_set.iter().zip(&cells.offsets).all(|(flags, column)| {
        flags
            .iter()
            .zip(column)
            .all(|(set, cell)| !set || cell.len() == offsets.len())
    }) {
        fill_unset_offsets(&mut cells.offsets, &cells.is_set, x_axis, y_axis, repeat)?;
    }
    write_binding_cells(binding, &cells)
}

pub(crate) fn refill_deform_binding(
    binding: &mut Value,
    id: u32,
    x_axis: &[f64],
    y_axis: &[f64],
    repeat: bool,
) -> Result<(), String> {
    let mut cells = parse_binding_cells(binding, id, x_axis, y_axis)?;
    let mut authored = cells.offsets.iter().flatten().zip(cells.is_set.iter().flatten())
        .filter_map(|(cell, set)| set.then_some(cell.len()));
    let Some(vertices) = authored.next() else { return Ok(()) };
    if authored.any(|count| count != vertices) {
        return Err("deform binding values have inconsistent vertex counts".into());
    }
    let unset = cells.is_set.iter().flatten().filter(|set| !**set).count();
    if unset == 0 {
        return Ok(());
    }
    let eager = check_deform_output_budget(unset, 1, vertices).is_ok();
    for (cell, set) in cells.offsets.iter_mut().flatten().zip(cells.is_set.iter().flatten()) {
        if !set {
            // Empty caches are reconstructed by the bounded lazy evaluator.
            *cell = if eager { vec![[0.0, 0.0]; vertices] } else { Vec::new() };
        }
    }
    if eager {
        fill_unset_offsets(&mut cells.offsets, &cells.is_set, x_axis, y_axis, repeat)?;
    }
    binding["values"] = json!(cells.offsets);
    Ok(())
}

fn resize_offsets(offsets: &[[f64; 2]], new_len: usize) -> Vec<[f64; 2]> {
    let mut resized = offsets.to_vec();
    resized.resize(new_len, [0.0, 0.0]);
    resized
}

pub(crate) fn fill_unset_offsets(
    offsets: &mut [Vec<Vec<[f64; 2]>>],
    is_set: &[Vec<bool>],
    x_axis: &[f64],
    y_axis: &[f64],
    repeat: bool,
) -> Result<(), String> {
    let set_count = is_set.iter().flatten().filter(|set| **set).count();
    let total = x_axis.len().saturating_mul(y_axis.len());
    if set_count == 0 || set_count == total {
        return Ok(());
    }
    let vertex_count = offsets
        .first()
        .and_then(|column| column.first())
        .map(Vec::len)
        .unwrap_or(0);
    for vertex in 0..vertex_count {
        for component in 0..2 {
            let mut grid = vec![vec![0.0; y_axis.len()]; x_axis.len()];
            for x in 0..x_axis.len() {
                for y in 0..y_axis.len() {
                    grid[x][y] = offsets[x][y][vertex][component];
                }
            }
            crate::evaluation::reinterpolate(&mut grid, is_set, x_axis, y_axis, repeat)?;
            for x in 0..x_axis.len() {
                for y in 0..y_axis.len() {
                    if !is_set[x][y] {
                        offsets[x][y][vertex][component] = grid[x][y];
                    }
                }
            }
        }
    }
    Ok(())
}

fn transfer_grid_offsets(
    old_points: &[[f64; 2]],
    new_points: &[[f64; 2]],
    offsets: &[[f64; 2]],
) -> Vec<[f64; 2]> {
    let zeros = vec![[0.0, 0.0]; new_points.len()];
    if old_points.len() < 4 || offsets.len() != old_points.len() {
        return zeros;
    }
    let mut xs: Vec<f64> = old_points.iter().map(|point| point[0]).collect();
    let mut ys: Vec<f64> = old_points.iter().map(|point| point[1]).collect();
    xs.sort_by(f64::total_cmp);
    ys.sort_by(f64::total_cmp);
    xs.dedup();
    ys.dedup();
    let cols = xs.len();
    let rows = ys.len();
    if cols < 2 || rows < 2 || cols * rows != old_points.len() {
        return zeros;
    }
    new_points
        .iter()
        .map(|query| {
            let Some((cell_x, u)) = locate_interval(&xs, query[0]) else {
                return [0.0, 0.0];
            };
            let Some((cell_y, v)) = locate_interval(&ys, query[1]) else {
                return [0.0, 0.0];
            };
            let index = |cell_y: usize, cell_x: usize| cell_y * cols + cell_x;
            let corner = |cell_y: usize, cell_x: usize| {
                let slot = index(cell_y, cell_x);
                [
                    old_points[slot][0] + offsets[slot][0],
                    old_points[slot][1] + offsets[slot][1],
                ]
            };
            let deformed = bilinear(
                corner(cell_y, cell_x),
                corner(cell_y, cell_x + 1),
                corner(cell_y + 1, cell_x),
                corner(cell_y + 1, cell_x + 1),
                u,
                v,
            );
            [deformed[0] - query[0], deformed[1] - query[1]]
        })
        .collect()
}

fn locate_interval(axis: &[f64], value: f64) -> Option<(usize, f64)> {
    if axis.len() < 2 {
        return None;
    }
    const MAX_REL: f64 = 1e-4;
    const MAX_ABS: f64 = 1e-5;
    if value <= axis[0] {
        return phobos_is_close(value, axis[0], MAX_REL, MAX_ABS).then_some((0, 0.0));
    }
    let last = *axis.last()?;
    if value >= last {
        return phobos_is_close(value, last, MAX_REL, MAX_ABS)
            .then_some((axis.len() - 2, 1.0));
    }
    for index in 0..axis.len() - 1 {
        let start = axis[index];
        let end = axis[index + 1];
        if value >= start && value <= end {
            let span = end - start;
            let mut weight = if span > 0.0 { (value - start) / span } else { 0.0 };
            weight = weight.clamp(0.0, 1.0);
            return Some((index, weight));
        }
    }
    None
}

fn bilinear(p00: [f64; 2], p10: [f64; 2], p01: [f64; 2], p11: [f64; 2], u: f64, v: f64) -> [f64; 2] {
    let mix = |a: [f64; 2], b: [f64; 2], t: f64| [a[0] * (1.0 - t) + b[0] * t, a[1] * (1.0 - t) + b[1] * t];
    mix(mix(p00, p10, u), mix(p01, p11, u), v)
}

fn phobos_is_close(lhs: f64, rhs: f64, max_rel: f64, max_abs: f64) -> bool {
    if lhs == rhs {
        return true;
    }
    let difference = (lhs - rhs).abs();
    if difference <= max_abs {
        return true;
    }
    let scale = lhs.abs().max(rhs.abs());
    scale != 0.0 && difference / scale <= max_rel
}

fn build_position_remap(old_points: &[[f64; 2]], new_points: &[[f64; 2]]) -> Vec<usize> {
    const TOLERANCE: f64 = 1e-4;
    if old_points.is_empty() || old_points.len() != new_points.len() {
        return Vec::new();
    }
    let mut used = vec![false; new_points.len()];
    let mut remap = vec![0; old_points.len()];
    for (old_index, old_point) in old_points.iter().enumerate() {
        let mut matched = None;
        for (new_index, new_point) in new_points.iter().enumerate() {
            if used[new_index] {
                continue;
            }
            let dx = old_point[0] - new_point[0];
            let dy = old_point[1] - new_point[1];
            if dx * dx + dy * dy <= TOLERANCE * TOLERANCE {
                matched = Some(new_index);
                break;
            }
        }
        let Some(new_index) = matched else {
            return Vec::new();
        };
        remap[old_index] = new_index;
        used[new_index] = true;
    }
    remap
}

fn pair_points(values: &[f64]) -> Vec<[f64; 2]> {
    values
        .chunks(2)
        .filter(|pair| pair.len() == 2)
        .map(|pair| [pair[0], pair[1]])
        .collect()
}

fn malformed(id: u32) -> String {
    format!("node {id} deform binding is malformed")
}

fn reject_unresampleable_grid_depths(model: &Value, id: u32) -> Result<(), String> {
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    depth_resample::prepare_grid_depth_rebuffer(node)
        .map_err(|message| format!("node {id} {message}"))
}

fn reject_unresampleable_path_depths(model: &Value, id: u32) -> Result<(), String> {
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    depth_resample::prepare_path_depth_rebuffer(node)
        .map_err(|message| format!("node {id} {message}"))
}

fn capture_path_depth_vertex_count(node: &Value) -> Result<Option<usize>, String> {
    if !depth_resample::has_path_depth_payload(node) {
        return Ok(None);
    }
    Ok(Some(depth_resample::path_vertex_count(node.get("vertices"))?))
}

fn rebuffer_captured_path_depths(
    node: &mut Value,
    old_vertex_count: Option<usize>,
) -> Result<(), String> {
    let Some(old_vertex_count) = old_vertex_count else {
        return Ok(());
    };
    depth_resample::rebuffer_path_depth_payload(node, old_vertex_count)
}

fn capture_grid_depth_vertices(node: &Value) -> Result<Option<Vec<[f32; 2]>>, String> {
    if !depth_resample::has_grid_depth_payload(node) {
        return Ok(None);
    }
    Ok(Some(depth_resample::grid_vertices(node)?))
}

fn rebuffer_captured_depths(
    node: &mut Value,
    old_vertices: Option<Vec<[f32; 2]>>,
) -> Result<(), String> {
    let Some(old_vertices) = old_vertices else {
        return Ok(());
    };
    depth_resample::rebuffer_grid_depth_payload(node, &old_vertices)
}

fn deformable_targets(model: &Value, node_ids: &[u32]) -> Result<Vec<u32>, String> {
    // DefineVerticesCommand keeps only non-drawable Deformables and ignores the rest.
    filter_targets(
        model,
        node_ids,
        |node_type| matches!(node_type, "PathDeformer" | "GridDeformer"),
        "No non-drawable deformable targets",
    )
}

fn grid_targets(model: &Value, node_ids: &[u32]) -> Result<Vec<u32>, String> {
    // DefineGridCommand keeps only GridDeformer nodes and ignores the rest of the selection.
    filter_targets(
        model,
        node_ids,
        |node_type| node_type == "GridDeformer",
        "No GridDeformer targets",
    )
}

fn filter_targets(
    model: &Value,
    node_ids: &[u32],
    accept: impl Fn(&str) -> bool,
    empty_message: &str,
) -> Result<Vec<u32>, String> {
    if node_ids.is_empty() {
        return Err("at least one node UUID is required".into());
    }
    let mut targets = Vec::new();
    for id in node_ids {
        let node = find_node(model, *id).ok_or_else(|| format!("node {id} was not found"))?;
        if accept(node_type(node)) {
            targets.push(*id);
        }
    }
    if targets.is_empty() {
        return Err(empty_message.into());
    }
    Ok(targets)
}

fn mesh_group_targets(model: &Value, node_ids: &[u32]) -> Result<Vec<u32>, String> {
    if node_ids.is_empty() {
        return Err("at least one node UUID is required".into());
    }
    let mut targets = Vec::new();
    for id in node_ids {
        let node = find_node(model, *id).ok_or_else(|| format!("node {id} was not found"))?;
        match node_type(node) {
            "MeshGroup" => targets.push(*id),
            "Part" | "Composite" | "DynamicComposite" | "Mask" => {
                return Err(
                    "DefineMesh for non-MeshGroup drawables is outside Rust Core mode".into(),
                );
            }
            _ => {}
        }
    }
    if targets.is_empty() {
        return Err("No MeshGroup targets".into());
    }
    Ok(targets)
}

fn property_matches(node_type: &str, property: &str) -> bool {
    match property {
        "GridDeformDynamic" => node_type == "GridDeformer",
        "MeshGroupDynamic" | "MeshGroupTranslateChildren" => node_type == "MeshGroup",
        "PathDeformDynamic"
        | "PathDeformPhysicsEnabled"
        | "PathDeformCurveType"
        | "PathDeformGravity"
        | "PathDeformRestoreConstant"
        | "PathDeformDamping"
        | "PathDeformInputScale"
        | "PathDeformPropagateScale" => node_type == "PathDeformer",
        _ => false,
    }
}

fn apply_property(node: &mut Value, property: &str, value: &Value) -> Result<(), String> {
    let object = node.as_object_mut().ok_or("node must be an object")?;
    match property {
        "GridDeformDynamic" => {
            object.insert(
                "dynamic".into(),
                Value::Bool(required_bool(value, property)?),
            );
        }
        "MeshGroupDynamic" | "PathDeformDynamic" => {
            object.insert(
                "dynamic_deformation".into(),
                Value::Bool(required_bool(value, property)?),
            );
        }
        "MeshGroupTranslateChildren" => {
            object.insert(
                "translate_children".into(),
                Value::Bool(required_bool(value, property)?),
            );
        }
        "PathDeformPhysicsEnabled" => {
            if required_bool(value, property)? {
                object
                    .entry("physics".to_owned())
                    .or_insert_with(default_pendulum);
            } else {
                object.remove("physics");
            }
        }
        "PathDeformCurveType" => {
            let curve = value.as_str().ok_or("curveType must be a string")?;
            if curve != "Spline" && curve != "Bezier" {
                return Err("curveType must be Spline or Bezier".into());
            }
            object.insert("curve_type".into(), Value::String(curve.to_owned()));
        }
        "PathDeformGravity"
        | "PathDeformRestoreConstant"
        | "PathDeformDamping"
        | "PathDeformInputScale"
        | "PathDeformPropagateScale" => {
            let number = finite_number(value, property)?;
            let physics = object
                .get_mut("physics")
                .and_then(Value::as_object_mut)
                .ok_or("PathDeformer physics driver is not a pendulum")?;
            if physics.get("type").and_then(Value::as_str) != Some("Pendulum") {
                return Err("PathDeformer physics driver is not a pendulum".into());
            }
            let key = match property {
                "PathDeformGravity" => "gravity",
                "PathDeformRestoreConstant" => "restore_constant",
                "PathDeformDamping" => "damping",
                "PathDeformInputScale" => "input_scale",
                "PathDeformPropagateScale" => "propagate_scale",
                _ => unreachable!(),
            };
            physics.insert(key.to_owned(), json!(number));
        }
        other => return Err(format!("unsupported deformer property {other}")),
    }
    Ok(())
}

fn stamp_grid(object: &mut Map<String, Value>, axis_x: Vec<f64>, axis_y: Vec<f64>, dynamic: bool) {
    object.insert("grid_axis_x".into(), json!(axis_x));
    object.insert("grid_axis_y".into(), json!(axis_y));
    object.insert("formation".into(), json!("Bilinear"));
    object.insert("dynamic".into(), json!(dynamic));
}

fn stamp_path(
    object: &mut Map<String, Value>,
    vertices: Vec<f64>,
    curve_type: &str,
    dynamic: bool,
    physics: Option<Value>,
) {
    object.insert("curve_type".into(), json!(curve_type));
    object.insert("vertices".into(), json!(vertices));
    object.insert("dynamic_deformation".into(), json!(dynamic));
    if let Some(physics) = physics {
        object.insert("physics".into(), physics);
    } else {
        object.remove("physics");
    }
}

fn default_pendulum() -> Value {
    json!({
        "type": "Pendulum",
        "damping": PENDULUM_DAMPING,
        "restore_constant": PENDULUM_RESTORE,
        "gravity": PENDULUM_GRAVITY,
        "input_scale": PENDULUM_INPUT_SCALE,
        "propagate_scale": PENDULUM_PROPAGATE_SCALE,
    })
}

fn empty_mesh() -> Value {
    json!({ "verts": [], "indices": [], "origin": [0.0, 0.0] })
}

fn mesh_from_grid_points(points: &[f64]) -> Result<Value, String> {
    let (xs, ys) = adopt_exact_grid(points).ok_or("grid mesh is not rectangular")?;
    let cols = xs.len();
    let rows = ys.len();
    let mut verts = Vec::with_capacity(cols * rows * 2);
    for y in &ys {
        for x in &xs {
            verts.push(*x);
            verts.push(*y);
        }
    }
    let mut indices = Vec::new();
    for y in 0..rows.saturating_sub(1) {
        for x in 0..cols.saturating_sub(1) {
            let a = y * cols + x;
            let b = y * cols + x + 1;
            let c = (y + 1) * cols + x;
            let d = (y + 1) * cols + x + 1;
            if d > u16::MAX as usize {
                return Err("MeshGroup grid indices exceed 16-bit index range".into());
            }
            indices.extend([a, b, c, b, d, c]);
        }
    }
    let mut mesh = json!({
        "verts": verts,
        "indices": indices,
        "origin": [0.0, 0.0],
    });
    if xs.len() > 2 && ys.len() > 2 {
        mesh.as_object_mut()
            .ok_or("mesh must be an object")?
            .insert("grid_axes".into(), json!([ys, xs]));
    }
    Ok(mesh)
}

fn grid_vertices_from_node(node: &Value) -> Option<Vec<f64>> {
    let xs = node.get("grid_axis_x")?.as_array()?;
    let ys = node.get("grid_axis_y")?.as_array()?;
    let xs = numbers_of(xs, "grid_axis_x").ok()?;
    let ys = numbers_of(ys, "grid_axis_y").ok()?;
    if xs.len() < 2 || ys.len() < 2 {
        return None;
    }
    let mut points = Vec::with_capacity(xs.len() * ys.len() * 2);
    for y in &ys {
        for x in &xs {
            points.push(*x);
            points.push(*y);
        }
    }
    Some(points)
}

fn mesh_flat_vertices(node: &Value) -> Option<Vec<f64>> {
    let verts = node.get("mesh")?.get("verts")?;
    let values = required_numbers(verts, "mesh.verts").ok()?;
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

fn flat_vertices(value: Option<&Value>) -> Option<Vec<f64>> {
    let value = value?;
    let values = required_numbers(value, "vertices").ok()?;
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

fn parse_control_points(vertices: &[f64]) -> Result<Vec<f64>, String> {
    if vertices.is_empty() {
        return Err("vertices empty".into());
    }
    if vertices.len() % 2 != 0 {
        return Err("vertices length must be even (x,y pairs)".into());
    }
    if vertices.iter().any(|value| !value.is_finite()) {
        return Err("vertices must contain finite x,y values".into());
    }
    Ok(vertices.to_vec())
}

fn count_grid(points: &[f64]) -> bool {
    if points.len() < 8 {
        return false;
    }
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for pair in points.chunks(2) {
        if !xs.iter().any(|value: &f64| *value == pair[0]) {
            xs.push(pair[0]);
        }
        if !ys.iter().any(|value: &f64| *value == pair[1]) {
            ys.push(pair[1]);
        }
    }
    if xs.len() < 2 || ys.len() < 2 || xs.len() * ys.len() != points.len() / 2 {
        return false;
    }
    for y in &ys {
        let count = points.chunks(2).filter(|pair| pair[1] == *y).count();
        if count != xs.len() {
            return false;
        }
    }
    for x in &xs {
        let count = points.chunks(2).filter(|pair| pair[0] == *x).count();
        if count != ys.len() {
            return false;
        }
    }
    true
}

fn adopt_exact_grid(points: &[f64]) -> Option<(Vec<f64>, Vec<f64>)> {
    if !count_grid(points) {
        return None;
    }
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    let mut seen = Vec::new();
    for pair in points.chunks(2) {
        if !xs.iter().any(|value: &f64| *value == pair[0]) {
            xs.push(pair[0]);
        }
        if !ys.iter().any(|value: &f64| *value == pair[1]) {
            ys.push(pair[1]);
        }
        seen.push((pair[0], pair[1]));
    }
    xs.sort_by(f64::total_cmp);
    ys.sort_by(f64::total_cmp);
    if xs.len() * ys.len() != seen.len() {
        return None;
    }
    for y in &ys {
        for x in &xs {
            if !seen.iter().any(|point| point.0 == *x && point.1 == *y) {
                return None;
            }
        }
    }
    Some((xs, ys))
}

fn normalize_axis(values: &[f64]) -> Vec<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mut unique = Vec::new();
    for value in sorted {
        if unique
            .last()
            .is_none_or(|previous: &f64| !is_close(*previous, value))
        {
            unique.push(value);
        }
    }
    unique
}

fn is_close(left: f64, right: f64) -> bool {
    let difference = (left - right).abs();
    difference <= AXIS_TOLERANCE || difference / right.abs() <= AXIS_TOLERANCE
}

fn required_bool(value: &Value, name: &str) -> Result<bool, String> {
    value
        .as_bool()
        .ok_or_else(|| format!("{name} must be a boolean"))
}

fn required_number_array(request: &Value, key: &str) -> Result<Vec<f64>, String> {
    let value = request
        .get(key)
        .ok_or_else(|| format!("{key} not provided"))?;
    let numbers = required_numbers(value, key)?;
    if numbers.is_empty() {
        return Err(format!("{key} empty"));
    }
    Ok(numbers)
}

fn required_numbers(value: &Value, name: &str) -> Result<Vec<f64>, String> {
    let values = value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array of numbers"))?;
    values
        .iter()
        .map(|value| finite_number(value, name))
        .collect()
}

fn required_index_array(request: &Value, key: &str) -> Result<Vec<u64>, String> {
    let values = request
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{key} not provided"))?;
    if values.is_empty() {
        return Err(format!("{key} empty"));
    }
    values
        .iter()
        .map(|value| {
            value
                .as_u64()
                .filter(|index| *index <= u64::from(u16::MAX))
                .ok_or_else(|| format!("{key} must contain 16-bit indices"))
        })
        .collect()
}

fn parse_pair(value: &Value, name: &str) -> Result<[f64; 2], String> {
    let values = value
        .as_array()
        .filter(|values| values.len() == 2)
        .ok_or_else(|| format!("{name} must contain two numbers"))?;
    Ok([
        finite_number(&values[0], name)?,
        finite_number(&values[1], name)?,
    ])
}

fn node_type(node: &Value) -> &str {
    node.get("type").and_then(Value::as_str).unwrap_or("Node")
}

fn nonempty_array(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_array)
        .is_some_and(|values| !values.is_empty())
}

fn numbers_of(values: &[Value], name: &str) -> Result<Vec<f64>, String> {
    values
        .iter()
        .map(|value| finite_number(value, name))
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

    fn deform_binding(session: &mut crate::Session, parameter_id: u64) -> Value {
        ok(&call(session, &json!({"op":"parameter.get","id":parameter_id})))["bindings"][0].clone()
    }

    #[test]
    fn path_depths_rebuffer_with_geometry_in_one_undo() {
        let mut session = crate::Session::new();
        let path = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"PathDeformer","name":"Path"}),
        ));
        let path_id = path["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_vertices","nodes":[path_id],"vertices":[0.0,0.0,1.0,0.0,2.0,0.0]}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"depth.set","target":path_id,"depths":[0.0,0.5,1.0]}),
        ));
        let before = ok(&call(&mut session, &json!({"op":"snapshot"})));

        ok(&call(
            &mut session,
            &json!({
                "op":"deformer.define_vertices","nodes":[path_id],
                "vertices":[0.0,0.0,1.0,1.0,2.0,2.0,3.0,3.0,4.0,4.0]
            }),
        ));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"depth.list","target":path_id})))["depths"],
            json!([0.0, 0.25, 0.5, 0.75, 1.0])
        );
        ok(&call(&mut session, &json!({"op":"undo"})));
        let undone = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(undone["model"], before["model"]);
        assert_eq!(undone["canRedo"], true);
    }

    #[test]
    fn mapped_depth_conversions_resize_and_reject_dropped_payload() {
        let mut session = crate::Session::new();
        let path = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"PathDeformer","name":"Path"}),
        ));
        let path_id = path["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_vertices","nodes":[path_id],"vertices":[0.0,0.0,1.0,0.0]}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"depth.set","target":path_id,"depths":[0.1,0.2]}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"node.convert","nodes":[path_id],"className":"GridDeformer"}),
        ));
        let grid = ok(&call(&mut session, &json!({"op":"node.get","id":path_id})));
        assert_eq!(
            grid["depths"],
            json!([f64::from(0.1f32), f64::from(0.2f32), 0.0, 0.0])
        );

        ok(&call(
            &mut session,
            &json!({"op":"node.convert","nodes":[path_id],"className":"PathDeformer"}),
        ));
        let path = ok(&call(&mut session, &json!({"op":"node.get","id":path_id})));
        assert_eq!(path["depths"], grid["depths"]);

        let before = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let dropped = call(
            &mut session,
            &json!({"op":"node.convert","nodes":[path_id],"className":"Node"}),
        );
        assert_eq!(dropped["succeeded"], false, "{dropped}");
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"}))), before);
    }

    #[test]
    fn undriven_grid_resize_remaps_cells_and_convert_keeps_them() {
        let mut session = crate::Session::new();
        let model = json!({
            "nodes": {
                "uuid": 1, "name": "Root", "type": "Node", "enabled": true,
                "children": [{
                    "uuid": 4, "name": "Grid", "type": "GridDeformer", "enabled": true,
                    "grid_axis_x": [0.0, 2.0], "grid_axis_y": [0.0, 2.0]
                }]
            },
            "param": [{
                "uuid": 9, "name": "Deform", "is_vec2": false,
                "min": [0.0, 0.0], "max": [1.0, 0.0], "defaults": [0.0, 0.0],
                "axis_points": [[0.0, 1.0], [0.0]],
                "bindings": [{
                    "node": 4, "param_name": "deform",
                    "values": [
                        [[[1.0, 0.0], [0.0, 0.0], [0.0, 0.0], [1.0, 0.0]]],
                        [[[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]]
                    ],
                    "isSet": [[true], [true]]
                }]
            }],
            "automation": []
        });
        ok(&call(&mut session, &json!({"op":"new","model":model})));
        let before = ok(&call(&mut session, &json!({"op":"snapshot"})));
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[4],"axisX":[0.0,1.0,2.0],"axisY":[0.0,2.0]}),
        ));
        let binding = deform_binding(&mut session, 9);
        let expected = json!([[1.0, 0.0], [0.5, 0.0], [0.0, 0.0], [0.0, 0.0], [0.5, 0.0], [1.0, 0.0]]);
        assert_eq!(binding["values"][0][0], expected);
        assert_eq!(binding["values"][1][0].as_array().unwrap().len(), 6);
        assert!(binding["values"][1][0]
            .as_array()
            .unwrap()
            .iter()
            .all(|pair| pair == &json!([0.0, 0.0])));
        assert_eq!(binding["isSet"], json!([[true], [true]]));
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"})))["model"], before["model"]);
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[4],"axisX":[0.0,1.0,2.0],"axisY":[0.0,2.0]}),
        ));
        let resized = deform_binding(&mut session, 9);
        ok(&call(
            &mut session,
            &json!({"op":"node.convert","nodes":[4],"className":"Node"}),
        ));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"node.get","id":4})))["type"],
            "Node"
        );
        assert_eq!(deform_binding(&mut session, 9)["values"], resized["values"]);
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"node.get","id":4})))["type"],
            "GridDeformer"
        );

        let malformed = call(
            &mut session,
            &json!({"op":"new","model":{
                "nodes": model["nodes"].clone(),
                "param": [{
                    "uuid": 9, "name": "Deform", "is_vec2": false,
                    "axis_points": [[0.0, 1.0], [0.0]],
                    "bindings": [{"node": 4, "param_name": "deform", "values": [], "isSet": []}]
                }],
                "automation": []
            }}),
        );
        assert_eq!(malformed["succeeded"], true, "{malformed}");
        let blocked = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let failed = call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[4],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        );
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(failed["message"].as_str().unwrap().contains("deform binding is malformed"));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"}))), blocked);

        ok(&call(&mut session, &json!({"op":"new","model":{
            "nodes": {
                "uuid": 1, "name": "Root", "type": "Node", "children": [{
                    "uuid": 6, "name": "Textured", "type": "GridDeformer",
                    "grid_axis_x": [0.0, 1.0], "grid_axis_y": [0.0, 1.0],
                    "textures": [1]
                }]
            },
            "param": [], "automation": []
        }})));
        let textured = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let rejected = call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[6],"axisX":[0.0,1.0,2.0],"axisY":[0.0,1.0]}),
        );
        assert_eq!(rejected["succeeded"], false, "{rejected}");
        assert!(rejected["message"].as_str().unwrap().contains("deform geometry"));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"}))), textured);
        let part = call(
            &mut session,
            &json!({"op":"node.convert","nodes":[6],"className":"Part"}),
        );
        assert_eq!(part["succeeded"], false, "{part}");
        assert!(part["message"].as_str().unwrap().contains("cannot convert to Part"), "{part}");
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"}))), textured);
    }

    #[test]
    fn path_convert_reorders_matching_vertices_and_set_cells_stay_closed() {
        let mut session = crate::Session::new();
        ok(&call(&mut session, &json!({"op":"new","model":{
            "nodes": {
                "uuid": 1, "name": "Root", "type": "Node", "children": [{
                    "uuid": 4, "name": "Path", "type": "PathDeformer",
                    "vertices": [2.0, 0.0, 0.0, 0.0, 2.0, 2.0, 0.0, 2.0]
                }]
            },
            "param": [{
                "uuid": 9, "name": "Deform", "is_vec2": false,
                "axis_points": [[0.0, 1.0], [0.0]],
                "bindings": [{
                    "node": 4, "param_name": "deform",
                    "values": [
                        [[[1.0, 0.0], [2.0, 0.0], [3.0, 0.0], [4.0, 0.0]]],
                        [[[1.0, 0.0], [2.0, 0.0], [3.0, 0.0], [4.0, 0.0]]]
                    ],
                    "isSet": [[true], [true]]
                }]
            }],
            "automation": []
        }})));
        ok(&call(
            &mut session,
            &json!({"op":"node.convert","nodes":[4],"className":"GridDeformer"}),
        ));
        let binding = deform_binding(&mut session, 9);
        assert_eq!(
            binding["values"][0][0],
            json!([[2.0, 0.0], [1.0, 0.0], [4.0, 0.0], [3.0, 0.0]])
        );
        assert_eq!(binding["isSet"][0][0], true);

        ok(&call(&mut session, &json!({"op":"new","model":{
            "nodes": {
                "uuid": 1, "name": "Root", "type": "Node", "children": [{
                    "uuid": 4, "name": "Path", "type": "PathDeformer",
                    "vertices": [0.0, 0.0, 1.0, 0.0]
                }]
            },
            "param": [{
                "uuid": 9, "name": "Deform", "is_vec2": false,
                "axis_points": [[0.0, 1.0], [0.0]],
                "bindings": [{
                    "node": 4, "param_name": "deform",
                    "values": [
                        [[[0.0, 0.0], [0.0, 0.0]]],
                        [[[0.0, 0.0], [0.0, 0.0]]]
                    ],
                    "isSet": [[false], [false]]
                }]
            }],
            "automation": []
        }})));
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_vertices","nodes":[4],"vertices":[0.0,0.0,1.0,0.0,2.0,0.0]}),
        ));
        let unset = deform_binding(&mut session, 9);
        assert_eq!(unset["values"][0][0], json!([[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]));
        assert_eq!(unset["isSet"][0][0], false);
        ok(&call(&mut session, &json!({"op":"undo"})));
        ok(&call(
            &mut session,
            &json!({
                "op":"new","model":{
                    "nodes": {
                        "uuid": 1, "name": "Root", "type": "Node", "children": [{
                            "uuid": 4, "name": "Path", "type": "PathDeformer",
                            "vertices": [0.0, 0.0, 1.0, 0.0]
                        }]
                    },
                    "param": [{
                        "uuid": 9, "name": "Deform", "is_vec2": false,
                        "axis_points": [[0.0, 1.0], [0.0]],
                        "bindings": [{
                            "node": 4, "param_name": "deform",
                            "values": [
                                [[[1.0, 0.0], [0.0, 1.0]]],
                                [[[1.0, 0.0], [0.0, 1.0]]]
                            ],
                            "isSet": [[true], [true]]
                        }]
                    }],
                    "automation": []
                }
            }),
        ));
        let held = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let moved = call(
            &mut session,
            &json!({"op":"deformer.define_vertices","nodes":[4],"vertices":[0.0,0.0,1.0,0.0,2.0,0.0]}),
        );
        assert_eq!(moved["succeeded"], false, "{moved}");
        assert!(moved["message"].as_str().unwrap().contains("cannot match D"));
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"}))), held);
    }

    fn deform_model(axis: Value, bindings: Option<Value>) -> Value {
        let mut parameter = json!({
            "uuid": 9,
            "name": "Deform",
            "is_vec2": false,
            "axis_points": [axis, [0.0]],
        });
        if let Some(bindings) = bindings {
            parameter["bindings"] = bindings;
        }
        json!({
            "nodes": {
                "uuid": 1,
                "type": "Node",
                "children": [{
                    "uuid": 4,
                    "type": "PathDeformer",
                    "vertices": [0.0, 0.0, 1.0, 0.0]
                }]
            },
            "param": [parameter],
            "automation": []
        })
    }

    #[test]
    fn set_deform_creates_linear_binding_and_updates_sparse_existing_binding() {
        let mut created = deform_model(json!([0.0, 1.0, 2.0]), None);
        assert_eq!(
            set_deform(
                &mut created,
                &json!({"parameter":9,"nodes":[4],"key":[1,0],"values":[1.0,2.0,3.0,4.0]})
            )
            .unwrap(),
            [4]
        );
        let binding = &created["param"][0]["bindings"][0];
        assert_eq!(binding["interpolate_mode"], "Linear");
        assert_eq!(binding["isSet"], json!([[false], [true], [false]]));
        assert_eq!(binding["values"][0][0], json!([[1.0, 2.0], [3.0, 4.0]]));
        assert_eq!(binding["values"][1][0], json!([[1.0, 2.0], [3.0, 4.0]]));
        assert_eq!(binding["values"][2][0], json!([[1.0, 2.0], [3.0, 4.0]]));

        let mut edited = deform_model(
            json!([0.0, 1.0, 2.0, 3.0]),
            Some(json!([{
                "node": 4,
                "param_name": "deform",
                "interpolate_mode": "Cubic",
                "extension": {"keep": true},
                "values": [
                    [[[0.0, 0.0], [1.0, 1.0]]],
                    [null],
                    [null],
                    [[[3.0, 3.0], [4.0, 4.0]]]
                ],
                "isSet": [[true], [null], [false], [true]]
            }]))
        );
        set_deform(
            &mut edited,
            &json!({"parameter":9,"nodes":[4],"key":[1,0],"values":[1.0,1.0,2.0,2.0]}),
        )
        .unwrap();
        let binding = &edited["param"][0]["bindings"][0];
        assert_eq!(binding["interpolate_mode"], "Cubic");
        assert_eq!(binding["extension"], json!({"keep": true}));
        assert_eq!(binding["values"][0][0], json!([[0.0, 0.0], [1.0, 1.0]]));
        assert_eq!(binding["values"][1][0], json!([[1.0, 1.0], [2.0, 2.0]]));
        assert_eq!(binding["values"][2][0], json!([[2.0, 2.0], [3.0, 3.0]]));
        assert_eq!(binding["values"][3][0], json!([[3.0, 3.0], [4.0, 4.0]]));
        assert_eq!(binding["isSet"], json!([[true], [true], [false], [true]]));
    }

    #[test]
    fn repeat_refill_invalidates_oversized_caches_for_lazy_evaluation() {
        let axis: Vec<_> = (0..256).map(|x| x as f64 / 255.0).collect();
        let mut binding = json!({"node": 4, "param_name": "deform",
            "values": vec![json!([[]]); 256], "isSet": vec![json!([false]); 256]});
        binding["values"][0][0] = json!(vec![[10.0, 20.0]; 2000]);
        binding["values"][128][0] = json!(vec![[20.0, 40.0]; 2000]);
        binding["isSet"][0][0] = json!(true);
        binding["isSet"][128][0] = json!(true);
        binding["values"][255][0] = json!(vec![[20.0, 40.0]; 2000]);
        let flags = binding["isSet"].clone();
        refill_deform_binding(&mut binding, 4, &axis, &[0.0], true).unwrap();
        assert_eq!(binding["values"][255][0], json!([]));
        assert_eq!(binding["values"][0][0], json!(vec![[10.0, 20.0]; 2000]));
        assert_eq!(binding["isSet"], flags);
    }

    #[test]
    fn resized_deform_keys_defer_refill_until_authored_lengths_match() {
        let mut binding = json!({
            "node":4,
            "param_name":"deform",
            "values":[[[[9.0,9.0]]], [[]], [[[99.0,99.0]]]],
            "isSet":[[true],[false],[true]],
            "extension":{"keep":true}
        });
        let x_axis = [0.0, 0.5, 1.0];
        let y_axis = [0.0];
        update_deform_key(
            &mut binding,
            4,
            [0, 0],
            &[[0.0, 0.0], [2.0, 2.0]],
            &x_axis,
            &y_axis,
            true,
            UnsetFillOverflow::Defer,
            false,
        )
        .unwrap();
        assert_eq!(binding["values"][1][0], json!([[0.0, 0.0], [0.0, 0.0]]));
        assert_eq!(binding["values"][2][0].as_array().unwrap().len(), 1);

        update_deform_key(
            &mut binding,
            4,
            [2, 0],
            &[[4.0, 4.0], [6.0, 6.0]],
            &x_axis,
            &y_axis,
            true,
            UnsetFillOverflow::Defer,
            false,
        )
        .unwrap();
        assert_eq!(binding["values"][1][0], json!([[2.0, 2.0], [4.0, 4.0]]));
        assert_eq!(binding["isSet"], json!([[true], [false], [true]]));
        assert_eq!(binding["extension"], json!({"keep":true}));
    }

    #[test]
    fn generated_sparse_refill_defers_when_unset_cache_exceeds_budget() {
        let x_axis = (0..256).map(f64::from).collect::<Vec<_>>();
        let y_axis = [0.0];
        let mut binding = json!({
            "node":4,
            "param_name":"deform",
            "values":vec![json!([[]]); x_axis.len()],
            "isSet":vec![json!([false]); x_axis.len()],
            "extension":{"keep":true}
        });
        let offsets = vec![[1.0, 2.0]; 1_961];
        let key = [127, 0];
        binding["values"][0][0] = json!(vec![[7.0, 8.0]; offsets.len()]);

        update_deform_key(
            &mut binding,
            4,
            key,
            &offsets,
            &x_axis,
            &y_axis,
            false,
            UnsetFillOverflow::Defer,
            false,
        )
        .unwrap();

        for x in 0..x_axis.len() {
            if x == key[0] {
                assert_eq!(binding["values"][x][0].as_array().unwrap().len(), offsets.len());
                assert_eq!(binding["isSet"][x][0], true);
            } else {
                if x == 0 {
                    assert_eq!(binding["values"][x][0][0], json!([7.0, 8.0]));
                    assert_eq!(binding["values"][x][0].as_array().unwrap().len(), offsets.len());
                } else {
                    assert_eq!(binding["values"][x][0], json!([]));
                }
                assert_eq!(binding["isSet"][x][0], false);
            }
        }
        assert_eq!(binding["extension"], json!({"keep":true}));
    }

    #[test]
    fn set_deform_rejects_invalid_key_value_count_duplicates_and_partial_updates() {
        let mut model = deform_model(json!([0.0, 1.0]), None);
        let before = model.clone();
        for request in [
            json!({"parameter":9,"nodes":[4],"key":[2,0],"values":[1.0,2.0,3.0,4.0]}),
            json!({"parameter":9,"nodes":[4],"key":[0,0],"values":[1.0,2.0]}),
            json!({"parameter":9,"nodes":[4,4],"key":[0,0],"values":[1.0,2.0,3.0,4.0]}),
            json!({"parameter":9,"nodes":[4],"key":[0,0],"values":[f64::MAX,2.0,3.0,4.0]}),
        ] {
            assert!(set_deform(&mut model, &request).is_err());
            assert_eq!(model, before);
        }

        let mut extended = deform_model(json!([0.0, 1.0]), Some(json!([{
            "node":4,"param_name":"deform",
            "values":[[[[1.0,2.0,"keep"],[3.0,4.0]]],[[[0.0,0.0],[0.0,0.0]]]],
            "isSet":[[true],[false]]
        }])));
        let preserved = extended.clone();
        assert!(set_deform(&mut extended,
            &json!({"parameter":9,"nodes":[4],"key":[1,0],"values":[5.0,6.0,7.0,8.0]})).is_err());
        assert_eq!(extended, preserved);

        let duplicate = json!([
            {"node":4,"param_name":"deform","values":[[[[0.0,0.0],[0.0,0.0]]],[[[0.0,0.0],[0.0,0.0]]]],"isSet":[[true],[true]]},
            {"node":4,"param_name":"deform","values":[[[[0.0,0.0],[0.0,0.0]]],[[[0.0,0.0],[0.0,0.0]]]],"isSet":[[true],[true]]}
        ]);
        let mut model = deform_model(json!([0.0, 1.0]), Some(duplicate));
        let before = model.clone();
        assert!(set_deform(
            &mut model,
            &json!({"parameter":9,"nodes":[4],"key":[0,0],"values":[1.0,2.0,3.0,4.0]})
        )
        .is_err());
        assert_eq!(model, before);

        let mut model = deform_model(
            json!([0.0, 1.0]),
            Some(json!([{
                "node": 5,
                "param_name": "deform",
                "values": [[[[0.0, 0.0]]], [[[0.0, 0.0]]]],
                "isSet": [[true], [true]]
            }]))
        );
        model["nodes"]["children"].as_array_mut().unwrap().push(json!({
            "uuid": 5,
            "type": "PathDeformer",
            "vertices": [0.0, 0.0, 1.0, 0.0]
        }));
        let before = model.clone();
        assert!(set_deform(
            &mut model,
            &json!({"parameter":9,"nodes":[4,5],"key":[0,0],"values":[1.0,2.0,3.0,4.0]})
        )
        .is_err());
        assert_eq!(model, before);
    }

    fn deform_budget_case(target_count: usize, point_count: usize, axis_count: usize) -> (Value, Value) {
        let vertices = (0..point_count)
            .flat_map(|index| [index as f64, 0.0])
            .collect::<Vec<_>>();
        let node_ids = (0..target_count)
            .map(|index| 4 + index as u32)
            .collect::<Vec<_>>();
        let children = node_ids
            .iter()
            .map(|id| {
                json!({
                    "uuid": id,
                    "type": "PathDeformer",
                    "vertices": vertices
                })
            })
            .collect::<Vec<_>>();
        let axis = (0..axis_count)
            .map(|index| index as f64 / (axis_count - 1) as f64)
            .collect::<Vec<_>>();
        let values = vec![0.0; point_count * 2];
        (
            json!({
                "nodes": {"uuid":1,"type":"Node","children":children},
                "param": [{
                    "uuid":9,"name":"Deform","is_vec2":false,
                    "axis_points":[axis,[0.0]]
                }],
                "automation":[]
            }),
            json!({"parameter":9,"nodes":node_ids,"key":[0,0],"values":values}),
        )
    }

    #[test]
    fn set_deform_bounds_single_and_aggregate_target_output_before_allocation() {
        for (target_count, point_count) in [(1, 2_000), (3, 800)] {
            let (mut model, request) = deform_budget_case(target_count, point_count, 256);
            let before = model.clone();
            let error = set_deform(&mut model, &request).unwrap_err();
            assert!(error.contains("output size limit"), "{error}");
            assert_eq!(model, before);
        }
    }

    #[test]
    fn set_deform_rejects_grid_count_mismatch_before_expanding_geometry() {
        let mut model = deform_model(json!([0.0, 1.0]), None);
        model["nodes"]["children"][0] = json!({
            "uuid": 4,
            "type": "GridDeformer",
            "grid_axis_x": vec![0.0; 10_000],
            "grid_axis_y": vec![0.0; 10_000]
        });
        let before = model.clone();
        let error = set_deform(
            &mut model,
            &json!({"parameter":9,"nodes":[4],"key":[0,0],"values":[1.0,2.0,3.0,4.0]}),
        )
        .unwrap_err();
        assert!(error.contains("100000000"), "{error}");
        assert_eq!(model, before);

        let mut invalid = deform_model(json!([0.0, 1.0]), None);
        invalid["nodes"]["children"][0] = json!({
            "uuid": 4,
            "type": "GridDeformer",
            "grid_axis_x": [0.0, null],
            "grid_axis_y": [0.0, 1.0]
        });
        let before = invalid.clone();
        assert!(set_deform(
            &mut invalid,
            &json!({"parameter":9,"nodes":[4],"key":[0,0],"values":[0.0,0.0,0.0,0.0,0.0,0.0,0.0,0.0]})
        )
        .is_err());
        assert_eq!(invalid, before);
    }

    fn mesh_model(mesh: Value) -> Value {
        json!({
            "nodes": {
                "uuid": 1,
                "type": "Node",
                "children": [{"uuid":4,"type":"MeshGroup","mesh":mesh}]
            },
            "param": [],
            "automation": []
        })
    }

    #[test]
    fn define_mesh_patches_existing_mesh_and_preserves_omitted_fields() {
        let model = mesh_model(json!({
            "verts": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
            "uvs": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
            "indices": [0, 1, 2],
            "origin": [4.0, 5.0],
            "extension": {"keep": true}
        }));
        let request = json!({
            "vertices": [0.0, 0.0, 2.0, 0.0, 0.0, 2.0],
            "indices": [0, 1, 2]
        });
        let (updated, _) = define_mesh(&model, &[4], &request).unwrap();
        assert_eq!(updated["nodes"]["children"][0]["mesh"]["uvs"], model["nodes"]["children"][0]["mesh"]["uvs"]);
        assert_eq!(updated["nodes"]["children"][0]["mesh"]["origin"], json!([4.0, 5.0]));
        assert_eq!(updated["nodes"]["children"][0]["mesh"]["extension"], json!({"keep": true}));

        let patched = json!({
            "vertices": [0.0, 0.0, 2.0, 0.0, 0.0, 2.0],
            "uvs": [0.2, 0.3, 0.4, 0.5, 0.6, 0.7],
            "indices": [0, 1, 2],
            "origin": [8.0, 9.0]
        });
        let (updated, _) = define_mesh(&updated, &[4], &patched).unwrap();
        let mesh = &updated["nodes"]["children"][0]["mesh"];
        assert_eq!(mesh["uvs"], patched["uvs"]);
        assert_eq!(mesh["origin"], patched["origin"]);
        assert_eq!(mesh["extension"], json!({"keep": true}));
    }

    #[test]
    fn define_mesh_keeps_an_imported_empty_uv_array_when_uvs_are_omitted() {
        let model = mesh_model(json!({
            "verts": [],
            "uvs": [],
            "indices": [],
            "origin": [4.0, 5.0],
            "extension": {"keep": true}
        }));
        let request = json!({
            "vertices": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
            "indices": [0, 1, 2]
        });
        let (updated, _) = define_mesh(&model, &[4], &request).unwrap();
        let mesh = &updated["nodes"]["children"][0]["mesh"];
        assert_eq!(mesh["uvs"], json!([]));
        assert_eq!(mesh["origin"], json!([4.0, 5.0]));
        assert_eq!(mesh["extension"], json!({"keep": true}));
    }

    #[test]
    fn define_mesh_rejects_invalid_triangles_uvs_and_indices_without_mutation() {
        let model = mesh_model(json!({
            "verts": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
            "uvs": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
            "indices": [0, 1, 2],
            "origin": [0.0, 0.0]
        }));
        let invalid_requests = [
            json!({"vertices":[0.0,0.0,1.0,0.0,0.0,1.0],"indices":[0,1,2,2]}),
            json!({"vertices":[0.0,0.0,1.0,0.0,0.0,1.0],"uvs":[0.0,0.0],"indices":[0,1,2]}),
            json!({"vertices":[0.0,0.0,1.0,0.0,0.0,1.0],"indices":[0,1,3]}),
        ];
        for request in invalid_requests {
            assert!(define_mesh(&model, &[4], &request).is_err());
            assert_eq!(model["nodes"]["children"][0]["mesh"]["indices"], json!([0, 1, 2]));
        }

        let changed_count = json!({"vertices":[0.0,0.0,1.0,0.0],"indices":[0,1,1]});
        assert!(define_mesh(&model, &[4], &changed_count).is_err());
        assert_eq!(model["nodes"]["children"][0]["mesh"]["uvs"].as_array().unwrap().len(), 6);
    }
}
