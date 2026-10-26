//! Standard depth parameters, matching `AddStandardDepthParameters`.
//!
//! Axis ranges come from D `standardDepthParameterSpecs`. Bindings are the
//! full `standardDepthBoneBindingSpecs` list. A later call adds only the rows
//! that are missing. An existing row whose value disagrees with D is left
//! unchanged and the command fails.

use crate::evaluation::upsert_linear_binding;
use crate::nodes;
use crate::{fresh_resource_id, valid_id};
use serde_json::{json, Value};

const AXIS_EPS: f64 = 1e-6;
const AXIS5: [f64; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

struct ParamSpec {
    name: &'static str,
    is_vec2: bool,
    min: [f64; 2],
    max: [f64; 2],
    axis_x: &'static [f64],
    axis_y: &'static [f64],
}

struct BindingSpec {
    parameter: &'static str,
    bone: &'static str,
    channel: &'static str,
    cells: &'static [([f64; 2], f64)],
}

const PARAMS: &[ParamSpec] = &[
    ParamSpec {
        name: "Face::Yaw-Pitch",
        is_vec2: true,
        min: [-1.0, -1.0],
        max: [1.0, 1.0],
        axis_x: &AXIS5,
        axis_y: &AXIS5,
    },
    ParamSpec {
        name: "Face::Roll",
        is_vec2: false,
        min: [-1.0, 0.0],
        max: [1.0, 0.0],
        axis_x: &AXIS5,
        axis_y: &[0.0],
    },
    ParamSpec {
        name: "Body::Yaw-Pitch",
        is_vec2: true,
        min: [-1.0, -1.0],
        max: [1.0, 1.0],
        axis_x: &AXIS5,
        axis_y: &AXIS5,
    },
    ParamSpec {
        name: "Body::Roll",
        is_vec2: false,
        min: [-1.0, 0.0],
        max: [1.0, 0.0],
        axis_x: &AXIS5,
        axis_y: &[0.0],
    },
];

const HEAD_YAW: &[([f64; 2], f64)] = &[
    ([-1.0, -1.0], -0.5235988),
    ([-1.0, 0.0], -0.5235988),
    ([-1.0, 1.0], -0.5235988),
    ([0.0, -1.0], 0.0),
    ([0.0, 0.0], 0.0),
    ([0.0, 1.0], 0.0),
    ([1.0, -1.0], 0.5235988),
    ([1.0, 0.0], 0.5235988),
    ([1.0, 1.0], 0.5235988),
];

const HEAD_PITCH: &[([f64; 2], f64)] = &[
    ([-1.0, -1.0], -0.34906584),
    ([-1.0, 1.0], 0.34906584),
    ([0.0, -1.0], -0.34906584),
    ([0.0, 0.0], 0.0),
    ([0.0, 1.0], 0.34906584),
    ([1.0, -1.0], -0.34906584),
    ([1.0, 0.0], 0.0),
    ([1.0, 1.0], 0.34906584),
];

const SPINE_YAW: &[([f64; 2], f64)] = &[
    ([-1.0, -1.0], -0.5235988),
    ([-1.0, 0.0], -0.5235988),
    ([-1.0, 1.0], -0.5235988),
    ([0.0, -1.0], 0.0),
    ([0.0, 0.0], 0.0),
    ([0.0, 1.0], 0.0),
    ([1.0, -1.0], 0.5235988),
    ([1.0, 0.0], 0.5235988),
    ([1.0, 1.0], 0.5235988),
];

const SPINE_PITCH: &[([f64; 2], f64)] = &[
    ([-1.0, -1.0], -0.5235988),
    ([-1.0, 0.0], 0.0),
    ([-1.0, 1.0], 0.17453292),
    ([0.0, -1.0], -0.5235988),
    ([0.0, 0.0], 0.0),
    ([0.0, 1.0], 0.17453292),
    ([1.0, -1.0], -0.5235988),
    ([1.0, 0.0], 0.0),
    ([1.0, 1.0], 0.17453292),
];

const CHEST_PITCH: &[([f64; 2], f64)] = &[
    ([-1.0, -1.0], 0.2617994),
    ([-1.0, 0.0], 0.0),
    ([-1.0, 1.0], 0.34906584),
    ([0.0, -1.0], 0.2617994),
    ([0.0, 0.0], 0.0),
    ([0.0, 1.0], 0.34906584),
    ([1.0, -1.0], 0.2617994),
    ([1.0, 0.0], 0.0),
    ([1.0, 1.0], 0.34906584),
];

const CHEST_YAW: &[([f64; 2], f64)] = &[([-1.0, -1.0], 0.0), ([0.0, 0.0], 0.0), ([1.0, 1.0], 0.0)];

const CLAVICLE_PITCH: &[([f64; 2], f64)] = &[([-1.0, 1.0], 0.0), ([0.0, 0.0], 0.0)];

const NECK_ROLL: &[([f64; 2], f64)] = &[
    ([-1.0, 0.0], -0.41800633),
    ([0.0, 0.0], 0.0),
    ([1.0, 0.0], 0.41887903),
];

const PELVIS_TY: &[([f64; 2], f64)] = &[([0.0, -1.0], 26.0), ([0.0, 0.0], 0.0)];
const SHIN_L_TY: &[([f64; 2], f64)] = &[([0.0, -1.0], 93.0), ([0.0, 0.0], 0.0)];
const SHIN_R_TY: &[([f64; 2], f64)] = &[([0.0, -1.0], 93.0), ([0.0, 0.0], 0.0)];
const SHIN_L_TX: &[([f64; 2], f64)] = &[([0.0, -1.0], 68.0), ([0.0, 0.0], 0.0)];
const SHIN_R_TX: &[([f64; 2], f64)] = &[([0.0, -1.0], -67.0), ([0.0, 0.0], 0.0)];

const ROLL_PELVIS_TX: &[([f64; 2], f64)] =
    &[([-1.0, 0.0], 30.0), ([0.0, 0.0], 0.0), ([1.0, 0.0], -30.0)];
const ROLL_PELVIS_TY: &[([f64; 2], f64)] =
    &[([-1.0, 0.0], -28.0), ([0.0, 0.0], 0.0), ([1.0, 0.0], -28.0)];
const ROLL_PELVIS_RZ: &[([f64; 2], f64)] = &[
    ([-1.0, 0.0], -0.049741887),
    ([0.0, 0.0], 0.0),
    ([1.0, 0.0], 0.049741887),
];
const ROLL_SPINE_RZ: &[([f64; 2], f64)] = &[
    ([-1.0, 0.0], -0.15271631),
    ([0.0, 0.0], 0.0),
    ([1.0, 0.0], 0.15271631),
];
const ROLL_CHEST_RY: &[([f64; 2], f64)] =
    &[([-1.0, 0.0], 0.0), ([0.0, 0.0], 0.0), ([1.0, 0.0], 0.0)];
const ROLL_CHEST_RZ: &[([f64; 2], f64)] = &[
    ([-1.0, 0.0], -0.123918384),
    ([0.0, 0.0], 0.0),
    ([1.0, 0.0], 0.123918384),
];

const BINDINGS: &[BindingSpec] = &[
    BindingSpec {
        parameter: "Face::Yaw-Pitch",
        bone: "Head",
        channel: "transform.r.y",
        cells: HEAD_YAW,
    },
    BindingSpec {
        parameter: "Face::Yaw-Pitch",
        bone: "Head",
        channel: "transform.r.x",
        cells: HEAD_PITCH,
    },
    BindingSpec {
        parameter: "Face::Roll",
        bone: "Neck",
        channel: "transform.r.z",
        cells: NECK_ROLL,
    },
    BindingSpec {
        parameter: "Body::Yaw-Pitch",
        bone: "Spine",
        channel: "transform.r.y",
        cells: SPINE_YAW,
    },
    BindingSpec {
        parameter: "Body::Yaw-Pitch",
        bone: "Spine",
        channel: "transform.r.x",
        cells: SPINE_PITCH,
    },
    BindingSpec {
        parameter: "Body::Yaw-Pitch",
        bone: "Chest",
        channel: "transform.r.x",
        cells: CHEST_PITCH,
    },
    BindingSpec {
        parameter: "Body::Yaw-Pitch",
        bone: "Chest",
        channel: "transform.r.y",
        cells: CHEST_YAW,
    },
    BindingSpec {
        parameter: "Body::Yaw-Pitch",
        bone: "Clavicle.L",
        channel: "transform.r.x",
        cells: CLAVICLE_PITCH,
    },
    BindingSpec {
        parameter: "Body::Yaw-Pitch",
        bone: "Pelvis",
        channel: "transform.t.y",
        cells: PELVIS_TY,
    },
    BindingSpec {
        parameter: "Body::Yaw-Pitch",
        bone: "Shin.L",
        channel: "transform.t.y",
        cells: SHIN_L_TY,
    },
    BindingSpec {
        parameter: "Body::Yaw-Pitch",
        bone: "Shin.R",
        channel: "transform.t.y",
        cells: SHIN_R_TY,
    },
    BindingSpec {
        parameter: "Body::Yaw-Pitch",
        bone: "Shin.L",
        channel: "transform.t.x",
        cells: SHIN_L_TX,
    },
    BindingSpec {
        parameter: "Body::Yaw-Pitch",
        bone: "Shin.R",
        channel: "transform.t.x",
        cells: SHIN_R_TX,
    },
    BindingSpec {
        parameter: "Body::Roll",
        bone: "Pelvis",
        channel: "transform.t.x",
        cells: ROLL_PELVIS_TX,
    },
    BindingSpec {
        parameter: "Body::Roll",
        bone: "Pelvis",
        channel: "transform.t.y",
        cells: ROLL_PELVIS_TY,
    },
    BindingSpec {
        parameter: "Body::Roll",
        bone: "Pelvis",
        channel: "transform.r.z",
        cells: ROLL_PELVIS_RZ,
    },
    BindingSpec {
        parameter: "Body::Roll",
        bone: "Spine",
        channel: "transform.r.z",
        cells: ROLL_SPINE_RZ,
    },
    BindingSpec {
        parameter: "Body::Roll",
        bone: "Chest",
        channel: "transform.r.y",
        cells: ROLL_CHEST_RY,
    },
    BindingSpec {
        parameter: "Body::Roll",
        bone: "Chest",
        channel: "transform.r.z",
        cells: ROLL_CHEST_RZ,
    },
];

const REQUIRED_BONES: &[&str] = &[
    "Head",
    "Neck",
    "Spine",
    "Chest",
    "Clavicle.L",
    "Pelvis",
    "Shin.L",
    "Shin.R",
];

pub(crate) fn add(model: &Value, root_id: u32) -> Result<Value, String> {
    let root =
        nodes::find_node(model, root_id).ok_or_else(|| format!("node {root_id} was not found"))?;
    if root.get("type").and_then(Value::as_str) != Some("DepthRigRoot") {
        return Err("Node is not a DepthRigRoot".into());
    }
    let bones = bones_by_id(root)?;
    for name in REQUIRED_BONES {
        if !bones.contains_key(*name) {
            return Err(format!("Missing depth bone '{name}'"));
        }
    }
    for spec in PARAMS {
        if let Some(existing) = find_parameter(model, spec.name) {
            let is_vec2 = existing
                .get("is_vec2")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if is_vec2 != spec.is_vec2 {
                return Err(type_conflict(existing, spec));
            }
            if !structure_ok(existing, spec)? || !axes_ok(existing, spec)? {
                return Err(format!(
                    "Existing parameter '{}' does not match the standard depth axes",
                    spec.name
                ));
            }
        }
    }
    reject_conflicts(model, &bones)?;

    let mut candidate = model.clone();
    let mut changed = false;
    for spec in PARAMS {
        if find_parameter(&candidate, spec.name).is_none() {
            let id = fresh_resource_id(&candidate)?;
            let parameter = json!({
                "uuid": id,
                "name": spec.name,
                "is_vec2": spec.is_vec2,
                "min": spec.min,
                "max": spec.max,
                "defaults": [0.0, 0.0],
                "axis_points": [spec.axis_x, spec.axis_y],
                "merge_mode": "Additive",
                "bindings": [],
            });
            candidate
                .as_object_mut()
                .ok_or("model must be an object")?
                .entry("param")
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .ok_or("model.param must be an array")?
                .push(parameter);
            changed = true;
        }
    }
    for spec in BINDINGS {
        let bone_id = bones[spec.bone];
        let parameter = find_parameter_mut(&mut candidate, spec.parameter)
            .ok_or_else(|| format!("Missing parameter '{}'", spec.parameter))?;
        let cells = indexed_cells(parameter, spec.cells)?;
        if upsert_linear_binding(parameter, bone_id, spec.channel, &cells, 1e-6)? {
            changed = true;
        }
    }
    if !changed {
        return Err("Standard depth parameters already exist".into());
    }
    Ok(candidate)
}

fn reject_conflicts(
    model: &Value,
    bones: &std::collections::BTreeMap<&str, u32>,
) -> Result<(), String> {
    for spec in BINDINGS {
        let Some(parameter) = find_parameter(model, spec.parameter) else {
            continue;
        };
        let bone_id = bones[spec.bone];
        let Some(binding) = channel_binding(parameter, bone_id, spec.channel) else {
            continue;
        };
        let cells = indexed_cells(parameter, spec.cells)?;
        for (x, y, value) in cells {
            if !cell_is_set(binding, x, y)? {
                continue;
            }
            let stored = cell_value(binding, x, y)?;
            // Match `upsert_linear_binding`: a wider check would accept a cell
            // that the write path then overwrites.
            if (stored - value).abs() > 1e-6 {
                return Err(format!(
                    "Existing binding '{}' '{}' '{}' does not match the standard depth value",
                    spec.parameter, spec.bone, spec.channel
                ));
            }
        }
    }
    Ok(())
}

fn channel_binding<'a>(parameter: &'a Value, bone_id: u32, channel: &str) -> Option<&'a Value> {
    parameter
        .get("bindings")
        .and_then(Value::as_array)
        .and_then(|bindings| {
            bindings.iter().find(|binding| {
                binding.get("node").and_then(Value::as_u64) == Some(u64::from(bone_id))
                    && binding.get("param_name").and_then(Value::as_str) == Some(channel)
            })
        })
}

fn cell_is_set(binding: &Value, x: usize, y: usize) -> Result<bool, String> {
    Ok(binding
        .get("isSet")
        .and_then(Value::as_array)
        .and_then(|columns| columns.get(x))
        .and_then(Value::as_array)
        .and_then(|column| column.get(y))
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

fn cell_value(binding: &Value, x: usize, y: usize) -> Result<f64, String> {
    binding
        .get("values")
        .and_then(Value::as_array)
        .and_then(|columns| columns.get(x))
        .and_then(Value::as_array)
        .and_then(|column| column.get(y))
        .and_then(Value::as_f64)
        .ok_or_else(|| "binding value must be numeric".to_string())
}

fn type_conflict(parameter: &Value, spec: &ParamSpec) -> String {
    let is_vec2 = parameter
        .get("is_vec2")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    format!(
        "Existing parameter '{}' is {} but the standard depth template requires {}",
        spec.name,
        if is_vec2 { "2D" } else { "1D" },
        if spec.is_vec2 { "2D" } else { "1D" }
    )
}

fn structure_ok(parameter: &Value, spec: &ParamSpec) -> Result<bool, String> {
    let is_vec2 = parameter
        .get("is_vec2")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if is_vec2 != spec.is_vec2 {
        return Ok(false);
    }
    let min = pair(parameter, "min")?;
    let max = pair(parameter, "max")?;
    if (min[0] - spec.min[0]).abs() > AXIS_EPS || (max[0] - spec.max[0]).abs() > AXIS_EPS {
        return Ok(false);
    }
    if spec.is_vec2
        && ((min[1] - spec.min[1]).abs() > AXIS_EPS || (max[1] - spec.max[1]).abs() > AXIS_EPS)
    {
        return Ok(false);
    }
    Ok(true)
}

fn axes_ok(parameter: &Value, spec: &ParamSpec) -> Result<bool, String> {
    let needed = BINDINGS
        .iter()
        .filter(|binding| binding.parameter == spec.name);
    let mut saw = false;
    for binding in needed {
        saw = true;
        for (point, _) in binding.cells {
            if !has_axis_value(parameter, 0, point[0])? || !has_axis_value(parameter, 1, point[1])?
            {
                return Ok(false);
            }
        }
    }
    if saw {
        return Ok(true);
    }
    let axes = parameter
        .get("axis_points")
        .and_then(Value::as_array)
        .ok_or("parameter.axis_points must contain two arrays")?;
    Ok(axis_matches(&axes[0], spec.axis_x)? && axis_matches(&axes[1], spec.axis_y)?)
}

fn axis_matches(stored: &Value, expected: &[f64]) -> Result<bool, String> {
    let stored = stored.as_array().ok_or("parameter axis must be an array")?;
    if stored.len() != expected.len() {
        return Ok(false);
    }
    for (stored, expected) in stored.iter().zip(expected) {
        let stored = stored.as_f64().ok_or("parameter axis must be numeric")?;
        if (stored - expected).abs() > AXIS_EPS {
            return Ok(false);
        }
    }
    Ok(true)
}

fn has_axis_value(parameter: &Value, axis: usize, value: f64) -> Result<bool, String> {
    let is_vec2 = parameter
        .get("is_vec2")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !is_vec2 && axis == 1 {
        return Ok(value.abs() <= AXIS_EPS);
    }
    let min = pair(parameter, "min")?;
    let max = pair(parameter, "max")?;
    let points = parameter
        .get("axis_points")
        .and_then(Value::as_array)
        .and_then(|axes| axes.get(axis))
        .and_then(Value::as_array)
        .ok_or("parameter.axis_points must contain two arrays")?;
    let span = max[axis] - min[axis];
    for point in points {
        let point = point.as_f64().ok_or("parameter axis must be numeric")?;
        let physical = if span.abs() <= AXIS_EPS {
            min[axis]
        } else {
            min[axis] + span * point
        };
        if (physical - value).abs() <= AXIS_EPS {
            return Ok(true);
        }
    }
    Ok(false)
}

fn indexed_cells(
    parameter: &Value,
    cells: &[([f64; 2], f64)],
) -> Result<Vec<(usize, usize, f64)>, String> {
    let mut indexed = Vec::with_capacity(cells.len());
    for (point, value) in cells {
        let x = axis_index(parameter, 0, point[0])?;
        let y = axis_index(parameter, 1, point[1])?;
        indexed.push((x, y, *value));
    }
    Ok(indexed)
}

fn axis_index(parameter: &Value, axis: usize, value: f64) -> Result<usize, String> {
    let is_vec2 = parameter
        .get("is_vec2")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !is_vec2 && axis == 1 {
        return Ok(0);
    }
    let min = pair(parameter, "min")?;
    let max = pair(parameter, "max")?;
    let points = parameter
        .get("axis_points")
        .and_then(Value::as_array)
        .and_then(|axes| axes.get(axis))
        .and_then(Value::as_array)
        .ok_or("parameter.axis_points must contain two arrays")?;
    let span = max[axis] - min[axis];
    for (index, point) in points.iter().enumerate() {
        let point = point.as_f64().ok_or("parameter axis must be numeric")?;
        let physical = if span.abs() <= AXIS_EPS {
            min[axis]
        } else {
            min[axis] + span * point
        };
        if (physical - value).abs() <= AXIS_EPS {
            return Ok(index);
        }
    }
    Err("Standard depth binding keypoint is not present in parameter".into())
}

fn bones_by_id(root: &Value) -> Result<std::collections::BTreeMap<&'static str, u32>, String> {
    let mut found = Vec::new();
    collect_bones(root, &mut found)?;
    let mut map = std::collections::BTreeMap::new();
    for name in REQUIRED_BONES {
        if let Some(id) = found.iter().find_map(|(id, bone_id, node_name)| {
            if bone_id == name || node_name == name {
                Some(*id)
            } else {
                None
            }
        }) {
            map.insert(*name, id);
        }
    }
    Ok(map)
}

fn collect_bones(node: &Value, found: &mut Vec<(u32, String, String)>) -> Result<(), String> {
    if node.get("type").and_then(Value::as_str) == Some("DepthBone") {
        let id = node
            .get("uuid")
            .and_then(Value::as_u64)
            .and_then(valid_id)
            .ok_or("node is missing uuid")?;
        let bone_id = node
            .get("boneId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let name = node
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        found.push((id, bone_id, name));
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            collect_bones(child, found)?;
        }
    }
    Ok(())
}

fn find_parameter<'a>(model: &'a Value, name: &str) -> Option<&'a Value> {
    fn walk<'a>(list: &'a [Value], name: &str) -> Option<&'a Value> {
        for item in list {
            if item.get("name").and_then(Value::as_str) == Some(name) {
                return Some(item);
            }
            if let Some(children) = item.get("children").and_then(Value::as_array) {
                if let Some(found) = walk(children, name) {
                    return Some(found);
                }
            }
        }
        None
    }
    model
        .get("param")
        .and_then(Value::as_array)
        .and_then(|list| walk(list, name))
}

fn find_parameter_mut<'a>(model: &'a mut Value, name: &str) -> Option<&'a mut Value> {
    fn walk<'a>(list: &'a mut [Value], name: &str) -> Option<&'a mut Value> {
        for item in list {
            if item.get("name").and_then(Value::as_str) == Some(name) {
                return Some(item);
            }
            if let Some(children) = item.get_mut("children").and_then(Value::as_array_mut) {
                if let Some(found) = walk(children, name) {
                    return Some(found);
                }
            }
        }
        None
    }
    model
        .get_mut("param")
        .and_then(Value::as_array_mut)
        .and_then(|list| walk(list, name))
}

fn pair(parameter: &Value, key: &str) -> Result<[f64; 2], String> {
    let array = parameter
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("parameter {key} must contain two numbers"))?;
    if array.len() != 2 {
        return Err(format!("parameter {key} must contain two numbers"));
    }
    Ok([
        array[0]
            .as_f64()
            .ok_or_else(|| format!("parameter {key} must be numeric"))?,
        array[1]
            .as_f64()
            .ok_or_else(|| format!("parameter {key} must be numeric"))?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(session: &mut crate::Session, request: &Value) -> Value {
        session.handle_request(request)
    }

    #[test]
    fn standard_parameters_follow_the_d_axes_and_one_undo() {
        let mut session = crate::Session::new();
        let rig = call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        );
        let rig_id = rig["result"]["created"][0]["uuid"].as_u64().unwrap();
        let missing = call(
            &mut session,
            &json!({"op":"rig.add_standard_parameters","root":rig_id}),
        );
        assert_eq!(missing["succeeded"], false);
        assert!(missing["message"]
            .as_str()
            .unwrap()
            .contains("Missing depth bone"));
        assert!(
            call(&mut session, &json!({"op":"snapshot"}))["result"]["model"]["param"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        call(
            &mut session,
            &json!({"op":"rig.add_skeleton","root":rig_id,"scale":1}),
        );
        let created = call(
            &mut session,
            &json!({"op":"rig.add_standard_parameters","root":rig_id}),
        );
        assert_eq!(created["succeeded"], true, "{created}");
        let params = call(&mut session, &json!({"op":"parameters"}))["result"].clone();
        let face = params
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "Face::Yaw-Pitch")
            .unwrap();
        assert_eq!(face["is_vec2"], true);
        assert_eq!(face["min"], json!([-1.0, -1.0]));
        assert_eq!(face["max"], json!([1.0, 1.0]));
        assert_eq!(
            face["axis_points"],
            json!([[0.0, 0.25, 0.5, 0.75, 1.0], [0.0, 0.25, 0.5, 0.75, 1.0]])
        );
        let roll = params
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "Face::Roll")
            .unwrap();
        assert_eq!(roll["is_vec2"], false);
        let neck = roll["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["param_name"] == "transform.r.z")
            .unwrap();
        assert!((neck["values"][0][0].as_f64().unwrap() + 0.41800633).abs() < 1e-6);
        assert!((neck["values"][4][0].as_f64().unwrap() - 0.41887903).abs() < 1e-6);
        let body = params
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "Body::Yaw-Pitch")
            .unwrap();
        let shin = body["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["param_name"] == "transform.t.x")
            .unwrap();
        assert!(
            (shin["values"][2][0].as_f64().unwrap() - 68.0).abs() < 1e-6
                || (shin["values"][2][0].as_f64().unwrap() + 67.0).abs() < 1e-6
        );
        let yaw = face["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["param_name"] == "transform.r.y")
            .unwrap();
        assert!((yaw["values"][0][2].as_f64().unwrap() + 0.5235988).abs() < 1e-6);
        assert!((yaw["values"][4][2].as_f64().unwrap() - 0.5235988).abs() < 1e-6);
        let before = call(&mut session, &json!({"op":"snapshot"}));
        let again = call(
            &mut session,
            &json!({"op":"rig.add_standard_parameters","root":rig_id}),
        );
        assert_eq!(again["succeeded"], false);
        assert_eq!(again["message"], "Standard depth parameters already exist");
        assert_eq!(
            call(&mut session, &json!({"op":"snapshot"}))["result"],
            before["result"]
        );
        call(&mut session, &json!({"op":"undo"}));
        assert!(call(&mut session, &json!({"op":"parameters"}))["result"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_b6_subset_gains_only_the_missing_rows_in_one_undo() {
        let mut session = crate::Session::new();
        let rig = call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        );
        let rig_id = rig["result"]["created"][0]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({"op":"rig.add_skeleton","root":rig_id,"scale":1}),
        );
        call(
            &mut session,
            &json!({"op":"rig.add_standard_parameters","root":rig_id}),
        );
        let full = call(&mut session, &json!({"op":"snapshot"}))["result"]["model"].clone();
        let bones = bone_ids(&full);
        let mut subset = full.clone();
        strip_to_b6(&mut subset, &bones);
        let path =
            std::env::temp_dir().join(format!("vsoul-b6-{}-{}.inx", std::process::id(), rig_id));
        let bytes = crate::document::Document::new(subset).encode().unwrap();
        std::fs::write(&path, bytes).unwrap();
        let mut loaded = crate::Session::new();
        let opened = call(
            &mut loaded,
            &json!({"op":"open","path":path.to_string_lossy()}),
        );
        assert_eq!(opened["succeeded"], true, "{opened}");
        let _ = std::fs::remove_file(&path);
        let before = call(&mut loaded, &json!({"op":"snapshot"}));
        assert!(face_roll_bindings(&before["result"]["model"]).is_empty());
        let upgraded = call(
            &mut loaded,
            &json!({"op":"rig.add_standard_parameters","root":rig_id}),
        );
        assert_eq!(upgraded["succeeded"], true, "{upgraded}");
        let after = call(&mut loaded, &json!({"op":"snapshot"}));
        let neck = face_roll_bindings(&after["result"]["model"]);
        assert_eq!(neck.len(), 1);
        assert!((neck[0]["values"][0][0].as_f64().unwrap() + 0.41800633).abs() < 1e-6);
        let head = head_yaw(&after["result"]["model"], bones["Head"]);
        let old_head = head_yaw(&before["result"]["model"], bones["Head"]);
        assert_eq!(head["values"], old_head["values"]);
        call(&mut loaded, &json!({"op":"undo"}));
        let restored = call(&mut loaded, &json!({"op":"snapshot"}));
        assert_eq!(restored["result"]["model"], before["result"]["model"]);
        assert!(face_roll_bindings(&restored["result"]["model"]).is_empty());
    }

    #[test]
    fn a_disagreeing_standard_value_is_not_overwritten() {
        let mut session = crate::Session::new();
        let rig = call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        );
        let rig_id = rig["result"]["created"][0]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({"op":"rig.add_skeleton","root":rig_id,"scale":1}),
        );
        call(
            &mut session,
            &json!({"op":"rig.add_standard_parameters","root":rig_id}),
        );
        let snapshot = call(&mut session, &json!({"op":"snapshot"}));
        let bones = bone_ids(&snapshot["result"]["model"]);
        let head = bones["Head"];
        let parameters = call(&mut session, &json!({"op":"parameters"}));
        let face = parameter_id(&parameters["result"], "Face::Yaw-Pitch");
        call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter": face,
                "nodes":[head],"key":[0,2],"rotation_y":0.1
            }),
        );
        let held = call(&mut session, &json!({"op":"snapshot"}));
        let failed = call(
            &mut session,
            &json!({"op":"rig.add_standard_parameters","root":rig_id}),
        );
        assert_eq!(failed["succeeded"], false, "{failed}");
        let message = failed["message"].as_str().unwrap();
        assert!(message.contains("Face::Yaw-Pitch"), "{message}");
        assert!(message.contains("Head"), "{message}");
        assert!(message.contains("transform.r.y"), "{message}");
        assert_eq!(
            call(&mut session, &json!({"op":"snapshot"}))["result"],
            held["result"]
        );
    }

    #[test]
    fn a_cell_inside_the_old_conflict_gap_is_not_overwritten() {
        let mut session = crate::Session::new();
        let rig = call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        );
        let rig_id = rig["result"]["created"][0]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({"op":"rig.add_skeleton","root":rig_id,"scale":1}),
        );
        call(
            &mut session,
            &json!({"op":"rig.add_standard_parameters","root":rig_id}),
        );
        let snapshot = call(&mut session, &json!({"op":"snapshot"}));
        let bones = bone_ids(&snapshot["result"]["model"]);
        let chest = bones["Chest"];
        let parameters = call(&mut session, &json!({"op":"parameters"}));
        let body = parameter_id(&parameters["result"], "Body::Yaw-Pitch");
        call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter": body,
                "nodes":[chest],"key":[2,2],"rotation_y":0.00005
            }),
        );
        let held = call(&mut session, &json!({"op":"snapshot"}));
        let failed = call(
            &mut session,
            &json!({"op":"rig.add_standard_parameters","root":rig_id}),
        );
        assert_eq!(failed["succeeded"], false, "{failed}");
        let message = failed["message"].as_str().unwrap();
        assert!(message.contains("Body::Yaw-Pitch"), "{message}");
        assert!(message.contains("Chest"), "{message}");
        assert!(message.contains("transform.r.y"), "{message}");
        assert_eq!(
            call(&mut session, &json!({"op":"snapshot"}))["result"],
            held["result"]
        );
    }

    fn bone_ids(model: &Value) -> std::collections::BTreeMap<String, u64> {
        let mut ids = std::collections::BTreeMap::new();
        fn walk(node: &Value, ids: &mut std::collections::BTreeMap<String, u64>) {
            if node.get("type").and_then(Value::as_str) == Some("DepthBone") {
                if let Some(name) = node.get("boneId").and_then(Value::as_str) {
                    ids.insert(name.to_owned(), node["uuid"].as_u64().unwrap());
                }
            }
            if let Some(children) = node.get("children").and_then(Value::as_array) {
                for child in children {
                    walk(child, ids);
                }
            }
        }
        walk(&model["nodes"], &mut ids);
        ids
    }

    fn strip_to_b6(model: &mut Value, bones: &std::collections::BTreeMap<String, u64>) {
        let keep = [
            bones["Head"],
            bones["Spine"],
            bones["Chest"],
            bones["Clavicle.L"],
        ];
        let params = model["param"].as_array_mut().unwrap();
        for parameter in params {
            let name = parameter["name"].as_str().unwrap().to_owned();
            let bindings = parameter["bindings"].as_array_mut().unwrap();
            if name == "Face::Roll" || name == "Body::Roll" {
                bindings.clear();
                continue;
            }
            bindings.retain(|binding| keep.contains(&binding["node"].as_u64().unwrap()));
        }
    }

    fn face_roll_bindings(model: &Value) -> Vec<Value> {
        model["param"]
            .as_array()
            .unwrap()
            .iter()
            .find(|parameter| parameter["name"] == "Face::Roll")
            .unwrap()["bindings"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn head_yaw(model: &Value, head: u64) -> Value {
        model["param"]
            .as_array()
            .unwrap()
            .iter()
            .find(|parameter| parameter["name"] == "Face::Yaw-Pitch")
            .unwrap()["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["node"] == head && binding["param_name"] == "transform.r.y")
            .unwrap()
            .clone()
    }

    fn parameter_id(parameters: &Value, name: &str) -> u64 {
        parameters
            .as_array()
            .unwrap()
            .iter()
            .find(|parameter| parameter["name"] == name)
            .unwrap()["uuid"]
            .as_u64()
            .unwrap()
    }

    #[test]
    fn mismatched_existing_parameter_does_not_rebuild_axes() {
        let mut session = crate::Session::new();
        let rig = call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        );
        let rig_id = rig["result"]["created"][0]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({"op":"rig.add_skeleton","root":rig_id,"scale":1}),
        );
        call(
            &mut session,
            &json!({
                "op":"parameter.create","dimensions":1,"name":"Face::Yaw-Pitch","min":[-1.0,0.0],"max":[1.0,0.0]
            }),
        );
        let before = call(&mut session, &json!({"op":"snapshot"}));
        let failed = call(
            &mut session,
            &json!({"op":"rig.add_standard_parameters","root":rig_id}),
        );
        assert_eq!(failed["succeeded"], false);
        assert!(failed["message"].as_str().unwrap().contains("1D"));
        assert_eq!(
            call(&mut session, &json!({"op":"snapshot"}))["result"],
            before["result"]
        );
    }
}
