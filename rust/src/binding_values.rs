//! Numeric editors of a binding keypoint (port of `ngRustCoreBindingEditorRead/Write` and the manual
//! deformation editor of `panels/rustcore.d`): the eight transform channels of one node at one keypoint, and
//! the per-vertex deform offsets. Reading works on the session snapshot's model; writing builds the
//! `binding.set_trs` / `binding.set_deform` requests, one undo step each.

use crate::deformers::node_geometry_points;
use serde_json::{json, Value};

/// The channels of a TRS binding and their value when no binding exists.
const CHANNELS: [(&str, f64); 8] = [
    ("transform.t.x", 0.0),
    ("transform.t.y", 0.0),
    ("transform.r.z", 0.0),
    ("transform.s.x", 1.0),
    ("transform.s.y", 1.0),
    ("transform.t.z", 0.0),
    ("transform.r.x", 0.0),
    ("transform.r.y", 0.0),
];

/// What the editor shows for one node at one keypoint. Rotations are degrees (the file stores radians).
#[derive(Clone, Debug, PartialEq)]
pub struct TrsValues {
    pub translation: [f64; 2],
    pub translation_z: f64,
    pub scale: [f64; 2],
    pub rotation_degrees: f64,
    pub rotation_x_degrees: f64,
    pub rotation_y_degrees: f64,
    /// Whether the keypoint is explicitly set, in the order tx, ty, rz, sx, sy, tz, rx, ry.
    pub is_set: [bool; 8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrsChannel {
    Translation,
    Scale,
    Rotation,
    TranslationZ,
    RotationX,
    RotationY,
}

fn find_leaf(list: &Value, id: u32) -> Option<&Value> {
    for item in list.as_array()? {
        if item.get("groupUUID").is_none() && item["uuid"].as_u64() == Some(u64::from(id)) {
            return Some(item);
        }
        if let Some(found) = find_leaf(&item["children"], id) {
            return Some(found);
        }
    }
    None
}

fn find_node(node: &Value, id: u32) -> Option<&Value> {
    if node["uuid"].as_u64() == Some(u64::from(id)) {
        return Some(node);
    }
    node["children"].as_array()?.iter().find_map(|child| find_node(child, id))
}

fn checked_parameter<'a>(model: &'a Value, parameter: u32, key: [usize; 2]) -> Result<&'a Value, String> {
    let found = find_leaf(&model["param"], parameter).ok_or_else(|| format!("Parameter not found: {parameter}"))?;
    let points = found.get("axis_points").or_else(|| found.get("axisPoints"));
    let in_range = |axis: usize| points.and_then(|p| p.get(axis)).and_then(Value::as_array).is_some_and(|a| key[axis] < a.len());
    if !in_range(0) || !in_range(1) {
        return Err("Select an existing parameter keypoint".into());
    }
    Ok(found)
}

fn binding_of<'a>(parameter: &'a Value, node: u32, channel: &str) -> Option<&'a Value> {
    parameter["bindings"].as_array()?.iter().find(|binding| {
        binding["node"].as_u64() == Some(u64::from(node)) && binding["param_name"].as_str() == Some(channel)
    })
}

/// The eight channels at `key` of `parameter` for `node`; a channel without a binding reads as its neutral
/// value and not set.
pub fn read_trs(model: &Value, parameter: u32, node: u32, key: [usize; 2]) -> Result<TrsValues, String> {
    let found = checked_parameter(model, parameter, key)?;
    let mut value = [0.0; 8];
    let mut is_set = [false; 8];
    for (slot, (channel, neutral)) in CHANNELS.iter().enumerate() {
        value[slot] = *neutral;
        if let Some(binding) = binding_of(found, node, channel) {
            if let Some(number) = binding["values"][key[0]][key[1]].as_f64() {
                value[slot] = number;
            }
            is_set[slot] = binding["isSet"][key[0]][key[1]].as_bool() == Some(true);
        }
    }
    Ok(TrsValues {
        translation: [value[0], value[1]],
        rotation_degrees: value[2].to_degrees(),
        scale: [value[3], value[4]],
        translation_z: value[5],
        rotation_x_degrees: value[6].to_degrees(),
        rotation_y_degrees: value[7].to_degrees(),
        is_set,
    })
}

/// The `binding.set_trs` request for one channel group of `values` (D writes per channel, never all eight).
pub fn trs_request(parameter: u32, node: u32, key: [usize; 2], channel: TrsChannel, values: &TrsValues) -> Result<Value, String> {
    let mut request = json!({"op": "binding.set_trs", "parameter": parameter, "nodes": [node], "key": key});
    let mut put = |field: &str, value: Value, numbers: &[f64]| {
        if numbers.iter().all(|n| n.is_finite()) {
            request[field] = value;
            Ok(())
        } else {
            Err("Binding values must be finite".to_owned())
        }
    };
    match channel {
        TrsChannel::Translation => put("translation", json!(values.translation), &values.translation)?,
        TrsChannel::Scale => put("scale", json!(values.scale), &values.scale)?,
        TrsChannel::Rotation => put("rotation_degrees", json!(values.rotation_degrees), &[values.rotation_degrees])?,
        TrsChannel::TranslationZ => put("translation_z", json!(values.translation_z), &[values.translation_z])?,
        TrsChannel::RotationX => {
            put("rotation_x", json!(values.rotation_x_degrees.to_radians()), &[values.rotation_x_degrees])?
        }
        TrsChannel::RotationY => {
            put("rotation_y", json!(values.rotation_y_degrees.to_radians()), &[values.rotation_y_degrees])?
        }
    }
    Ok(request)
}

/// The deform offsets of one node at one keypoint.
#[derive(Clone, Debug, PartialEq)]
pub struct DeformValues {
    pub offsets: Vec<[f64; 2]>,
    /// An explicit deform key is stored here (otherwise the offsets start at zero).
    pub is_set: bool,
}

pub fn read_deform(model: &Value, parameter: u32, node: u32, key: [usize; 2]) -> Result<DeformValues, String> {
    let found = checked_parameter(model, parameter, key)?;
    let target = find_node(&model["nodes"], node).ok_or_else(|| format!("Node not found: {node}"))?;
    let count = node_geometry_points(target).len();
    if count == 0 {
        return Err(format!("Node {node} has no deform geometry"));
    }
    let mut offsets = vec![[0.0, 0.0]; count];
    let mut is_set = false;
    if let Some(binding) = binding_of(found, node, "deform") {
        is_set = binding["isSet"][key[0]][key[1]].as_bool() == Some(true);
        if let Some(stored) = binding["values"][key[0]][key[1]].as_array() {
            if stored.len() == count {
                for (slot, pair) in offsets.iter_mut().zip(stored) {
                    if let (Some(x), Some(y)) = (pair[0].as_f64(), pair[1].as_f64()) {
                        *slot = [x, y];
                    }
                }
            }
        }
    }
    Ok(DeformValues { offsets, is_set })
}

/// The `binding.set_deform` request: `values` is one flat x, y pair per vertex.
pub fn deform_request(parameter: u32, node: u32, key: [usize; 2], offsets: &[[f64; 2]]) -> Result<Value, String> {
    if offsets.is_empty() {
        return Err("Manual deform offsets must contain one XY pair per vertex".into());
    }
    if offsets.iter().flatten().any(|n| !n.is_finite()) {
        return Err("Manual deform offsets must be finite".into());
    }
    let flat: Vec<f64> = offsets.iter().flatten().copied().collect();
    Ok(json!({"op": "binding.set_deform", "parameter": parameter, "nodes": [node], "key": key, "values": flat}))
}
