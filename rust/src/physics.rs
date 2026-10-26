//! SimplePhysics document settings, without a simulation step.
//!
//! The stored keys are the ones `SimplePhysics.serializeSelfImpl` writes.
//! Enum JSON uses the D member name (`Pendulum`, `SpringPendulum`,
//! `AngleLength`, `XY`, `LengthAngle`, `YX`). An unassigned `param` is
//! `uint.max`. Nothing here advances a clock or writes the driven parameter.
//! A Path `physics` driver is a different object and stays untouched.

use crate::nodes::{find_node, find_node_mut};
use crate::{finite_number, valid_id, validate_model};
use serde_json::{json, Map, Value};
use std::path::Path;

pub const UNASSIGNED_PARAM: u64 = u32::MAX as u64;

const MODEL_TYPES: &[&str] = &["Pendulum", "SpringPendulum"];
const MAP_MODES: &[&str] = &["AngleLength", "XY", "LengthAngle", "YX"];

pub fn stamp(node: &mut Value) {
    let Some(object) = node.as_object_mut() else {
        return;
    };
    object.insert("param".into(), json!(UNASSIGNED_PARAM));
    object.insert("model_type".into(), json!("Pendulum"));
    object.insert("map_mode".into(), json!("AngleLength"));
    object.insert("gravity".into(), json!(1.0));
    object.insert("length".into(), json!(100.0));
    object.insert("frequency".into(), json!(1.0));
    object.insert("angle_damping".into(), json!(0.5));
    object.insert("length_damping".into(), json!(0.5));
    object.insert("output_scale".into(), json!([1.0, 1.0]));
    object.insert("local_only".into(), json!(false));
}

/// The presets of the user (issue #237), in `physics-presets.json` of the settings directory. There is none
/// built in.
pub(crate) const PRESETS: crate::preset_store::Kind =
    crate::preset_store::Kind { file: "physics-presets.json", noun: "preset", entry: preset_entry, builtin: |_| false };

const NO_SETTING: &str = "physics.set requires a setting";

/// The settings a preset keeps. `param` is not one of them: it names a parameter of one document.
const PRESET_FIELDS: [&str; 9] = [
    "model_type", "map_mode", "gravity", "length", "frequency", "angle_damping", "length_damping", "output_scale",
    "local_only",
];

fn preset_entry(request: &Value) -> Result<Value, String> {
    let entry: Map<String, Value> = PRESET_FIELDS
        .iter()
        .filter_map(|key| Some(((*key).to_owned(), request.get(*key).filter(|value| !value.is_null())?.clone())))
        .collect();
    if entry.is_empty() {
        return Err("a preset needs at least one setting".into());
    }
    let entry = Value::Object(entry);
    parse_edit(&Value::Null, &entry)?;
    Ok(entry)
}

/// The request of `physics.preset.save` with the settings of its `node` below its own fields, so a preset can
/// be what a SimplePhysics node holds now.
pub(crate) fn preset_request(model: &Value, request: &Value) -> Result<Value, String> {
    // A preset keeps one output scale, is saved from one node and starts from no other preset. These would be
    // dropped without a word.
    for key in ["nodes", "output_scale_x", "output_scale_y", "preset"] {
        if request.get(key).is_some_and(|value| !value.is_null()) {
            return Err(format!("a preset cannot keep {key}"));
        }
    }
    let Some(id) = request.get("node").filter(|value| !value.is_null()) else {
        return Ok(request.clone());
    };
    let id = id.as_u64().and_then(valid_id).ok_or("a SimplePhysics node UUID is required")?;
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    if node.get("type").and_then(Value::as_str) != Some("SimplePhysics") {
        return Err(format!("node {id} is not SimplePhysics"));
    }
    let mut merged = request.clone();
    for key in PRESET_FIELDS {
        if let (None | Some(Value::Null), Some(value)) = (request.get(key), node.get(key)) {
            merged[key] = value.clone();
        }
    }
    Ok(merged)
}

/// `physics.set` with `preset`: the settings of the saved preset are written first and the settings of the
/// request after them, so a setting of the request wins, one axis of the output scale too.
pub(crate) fn apply_with_preset(
    model: &Value,
    dir: Option<&Path>,
    request: &Value,
) -> Result<(Value, Vec<u32>), String> {
    let Some(name) = request.get("preset").filter(|value| !value.is_null()) else {
        return apply(model, request);
    };
    let name = name.as_str().ok_or("preset must be a string")?;
    let mut preset = dir
        .map(|dir| PRESETS.load(dir))
        .transpose()?
        .and_then(|mut presets| presets.remove(name))
        .ok_or_else(|| format!("unknown physics preset {:?}", name.chars().take(64).collect::<String>()))?;
    for key in ["node", "nodes"] {
        if let Some(target) = request.get(key) {
            preset[key] = target.clone();
        }
    }
    // The parser of `physics.set` says whether the request has a setting of its own, so no list of keys can
    // fall behind it.
    let has_own = match parse_edit(model, request) {
        Ok(_) => true,
        Err(error) if error == NO_SETTING => false,
        Err(error) => return Err(error),
    };
    let (model, ids) = apply(model, &preset)?;
    if has_own {
        apply(&model, request)
    } else {
        Ok((model, ids))
    }
}

pub fn apply(model: &Value, request: &Value) -> Result<(Value, Vec<u32>), String> {
    if request.get("node").is_some() && request.get("nodes").is_some() {
        return Err("physics.set accepts node or nodes, not both".into());
    }
    let ids = target_ids(request)?;
    let edit = parse_edit(model, request)?;
    for id in &ids {
        let node = find_node(model, *id).ok_or_else(|| format!("node {id} was not found"))?;
        let kind = node.get("type").and_then(Value::as_str).unwrap_or("Node");
        if kind != "SimplePhysics" {
            return Err(format!("node {id} is not SimplePhysics"));
        }
    }
    let mut candidate = model.clone();
    for id in &ids {
        let node =
            find_node_mut(&mut candidate, *id).ok_or_else(|| format!("node {id} was not found"))?;
        write_edit(node, &edit)?;
    }
    validate_model(&candidate)?;
    Ok((candidate, ids))
}

struct Edit {
    param: Option<u64>,
    model_type: Option<String>,
    map_mode: Option<String>,
    gravity: Option<f64>,
    length: Option<f64>,
    frequency: Option<f64>,
    angle_damping: Option<f64>,
    length_damping: Option<f64>,
    output_scale: Option<[f64; 2]>,
    output_scale_x: Option<f64>,
    output_scale_y: Option<f64>,
    local_only: Option<bool>,
}

fn parse_edit(model: &Value, request: &Value) -> Result<Edit, String> {
    let mut any = false;
    let param = if request.get("param").is_some() {
        any = true;
        Some(parse_param(model, &request["param"])?)
    } else {
        None
    };
    let model_type = optional_enum(request, "model_type", MODEL_TYPES, &mut any)?;
    let map_mode = optional_enum(request, "map_mode", MAP_MODES, &mut any)?;
    let gravity = optional_finite(request, "gravity", &mut any)?;
    let length = optional_finite(request, "length", &mut any)?;
    let frequency = optional_finite(request, "frequency", &mut any)?;
    let angle_damping = optional_finite(request, "angle_damping", &mut any)?;
    let length_damping = optional_finite(request, "length_damping", &mut any)?;
    let output_scale = if request.get("output_scale").is_some() {
        any = true;
        Some(parse_scale(&request["output_scale"])?)
    } else {
        None
    };
    let output_scale_x = optional_finite(request, "output_scale_x", &mut any)?;
    let output_scale_y = optional_finite(request, "output_scale_y", &mut any)?;
    if output_scale.is_some() && (output_scale_x.is_some() || output_scale_y.is_some()) {
        return Err("output_scale cannot be combined with one axis".into());
    }
    let local_only = if request.get("local_only").is_some() {
        any = true;
        Some(
            request["local_only"]
                .as_bool()
                .ok_or("local_only must be a boolean")?,
        )
    } else {
        None
    };
    if !any {
        return Err(NO_SETTING.into());
    }
    Ok(Edit {
        param,
        model_type,
        map_mode,
        gravity,
        length,
        frequency,
        angle_damping,
        length_damping,
        output_scale,
        output_scale_x,
        output_scale_y,
        local_only,
    })
}

fn write_edit(node: &mut Value, edit: &Edit) -> Result<(), String> {
    let object = node.as_object_mut().ok_or("node must be an object")?;
    if let Some(param) = edit.param {
        object.insert("param".into(), json!(param));
    }
    if let Some(model_type) = &edit.model_type {
        object.insert("model_type".into(), json!(model_type));
    }
    if let Some(map_mode) = &edit.map_mode {
        object.insert("map_mode".into(), json!(map_mode));
    }
    if let Some(gravity) = edit.gravity {
        object.insert("gravity".into(), json!(gravity));
    }
    if let Some(length) = edit.length {
        object.insert("length".into(), json!(length));
    }
    if let Some(frequency) = edit.frequency {
        object.insert("frequency".into(), json!(frequency));
    }
    if let Some(angle_damping) = edit.angle_damping {
        object.insert("angle_damping".into(), json!(angle_damping));
    }
    if let Some(length_damping) = edit.length_damping {
        object.insert("length_damping".into(), json!(length_damping));
    }
    if let Some(scale) = edit.output_scale {
        object.insert("output_scale".into(), json!(scale));
    } else if edit.output_scale_x.is_some() || edit.output_scale_y.is_some() {
        let current = match object.get("output_scale") {
            Some(value) => parse_scale(value)?,
            None => [1.0, 1.0],
        };
        let scale = [
            edit.output_scale_x.unwrap_or(current[0]),
            edit.output_scale_y.unwrap_or(current[1]),
        ];
        object.insert("output_scale".into(), json!(scale));
    }
    if let Some(local_only) = edit.local_only {
        object.insert("local_only".into(), json!(local_only));
    }
    Ok(())
}

fn target_ids(request: &Value) -> Result<Vec<u32>, String> {
    if let Some(nodes) = request.get("nodes") {
        let list = nodes.as_array().ok_or("nodes must be an array")?;
        if list.is_empty() {
            return Err("at least one SimplePhysics node UUID is required".into());
        }
        let mut ids = Vec::new();
        for value in list {
            let id = value
                .as_u64()
                .and_then(valid_id)
                .ok_or("nodes must contain nonzero u32 UUIDs")?;
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        return Ok(ids);
    }
    let id = request
        .get("node")
        .and_then(Value::as_u64)
        .and_then(valid_id)
        .ok_or("a SimplePhysics node UUID is required")?;
    Ok(vec![id])
}

fn optional_enum(
    request: &Value,
    key: &str,
    allowed: &[&str],
    any: &mut bool,
) -> Result<Option<String>, String> {
    let Some(value) = request.get(key) else {
        return Ok(None);
    };
    *any = true;
    let text = value.as_str().ok_or_else(|| format!("unknown {key}"))?;
    if !allowed.contains(&text) {
        return Err(format!("unknown {key}: {text}"));
    }
    Ok(Some(text.to_owned()))
}

fn optional_finite(request: &Value, key: &str, any: &mut bool) -> Result<Option<f64>, String> {
    let Some(value) = request.get(key) else {
        return Ok(None);
    };
    *any = true;
    Ok(Some(finite_number(value, key)?))
}

fn parse_scale(value: &Value) -> Result<[f64; 2], String> {
    let list = value
        .as_array()
        .ok_or("output_scale must be a finite [x, y]")?;
    if list.len() != 2 {
        return Err("output_scale must be a finite [x, y]".into());
    }
    Ok([
        finite_number(&list[0], "output_scale")?,
        finite_number(&list[1], "output_scale")?,
    ])
}

fn parse_param(model: &Value, value: &Value) -> Result<u64, String> {
    if value.is_null() {
        return Ok(UNASSIGNED_PARAM);
    }
    let id = value
        .as_u64()
        .and_then(valid_id)
        .ok_or("param UUID is not a parameter")?;
    if !is_parameter(model, id) {
        return Err("param UUID is not a parameter".into());
    }
    Ok(u64::from(id))
}

fn is_parameter(model: &Value, id: u32) -> bool {
    fn walk(list: &[Value], id: u32) -> bool {
        for item in list {
            if item.get("groupUUID").is_some() {
                if let Some(children) = item.get("children").and_then(Value::as_array) {
                    if walk(children, id) {
                        return true;
                    }
                }
                continue;
            }
            if item.get("uuid").and_then(Value::as_u64) == Some(u64::from(id)) {
                return true;
            }
        }
        false
    }
    model
        .get("param")
        .and_then(Value::as_array)
        .is_some_and(|list| walk(list, id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;
    use serde_json::json;
    use std::fs;

    fn call(session: &mut Session, request: &Value) -> Value {
        session.handle_request(request)
    }

    fn ok(response: &Value) -> Value {
        assert_eq!(response["succeeded"], true, "{response}");
        response["result"].clone()
    }

    #[test]
    fn create_stores_serialize_self_defaults() {
        let mut session = Session::new();
        let created = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"SimplePhysics","name":"Hair"}),
        ));
        let id = created["created"][0]["uuid"].as_u64().unwrap();
        assert_eq!(id, 2);
        let node = ok(&call(&mut session, &json!({"op":"node.get","id":id})));
        assert_eq!(
            node,
            json!({
                "uuid": 2,
                "name": "Hair",
                "type": "SimplePhysics",
                "enabled": true,
                "zsort": 0.0,
                "transform": {"trans":[0.0,0.0,0.0], "rot":[0.0,0.0,0.0], "scale":[1.0,1.0]},
                "lockToRoot": false,
                "pinToMesh": false,
                "children": [],
                "param": UNASSIGNED_PARAM,
                "model_type": "Pendulum",
                "map_mode": "AngleLength",
                "gravity": 1.0,
                "length": 100.0,
                "frequency": 1.0,
                "angle_damping": 0.5,
                "length_damping": 0.5,
                "output_scale": [1.0, 1.0],
                "local_only": false,
            })
        );
    }

    #[test]
    fn set_is_one_undo_and_reload_keeps_fields() {
        let dir = std::env::temp_dir().join(format!("vsoul-physics-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("physics.inx");
        let mut session = Session::new();
        let created = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"SimplePhysics","name":"Hair"}),
        ));
        let id = created["created"][0]["uuid"].as_u64().unwrap();
        let parameter = ok(&call(
            &mut session,
            &json!({"op":"parameter.create","name":"Phys","dimensions":1}),
        ));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        let before = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let values = before["values"].clone();
        ok(&call(
            &mut session,
            &json!({
                "op":"physics.set","node":id,
                "param": parameter_id,
                "model_type":"SpringPendulum",
                "map_mode":"XY",
                "gravity":2.25,
                "length":123.0,
                "frequency":4.5,
                "angle_damping":0.35,
                "length_damping":0.45,
                "output_scale":[1.25, 1.75],
                "local_only": true
            }),
        ));
        let edited = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(
            edited["revision"].as_u64().unwrap(),
            before["revision"].as_u64().unwrap() + 1
        );
        assert_eq!(edited["values"], values);
        assert_eq!(edited["model"]["param"], before["model"]["param"]);
        let node = &edited["model"]["nodes"]["children"][0];
        assert_eq!(node["param"], parameter_id);
        assert_eq!(node["model_type"], "SpringPendulum");
        assert_eq!(node["map_mode"], "XY");
        assert_eq!(node["gravity"], 2.25);
        assert_eq!(node["length"], 123.0);
        assert_eq!(node["frequency"], 4.5);
        assert_eq!(node["angle_damping"], 0.35);
        assert_eq!(node["length_damping"], 0.45);
        assert_eq!(node["output_scale"], json!([1.25, 1.75]));
        assert_eq!(node["local_only"], true);
        ok(&call(&mut session, &json!({"op":"undo"})));
        let undone = ok(&call(&mut session, &json!({"op":"node.get","id":id})));
        assert_eq!(undone["model_type"], "Pendulum");
        assert_eq!(undone["param"], UNASSIGNED_PARAM);
        assert_eq!(undone["local_only"], false);
        ok(&call(&mut session, &json!({"op":"redo"})));
        ok(&call(
            &mut session,
            &json!({"op":"save","path": path.to_string_lossy()}),
        ));
        let mut reopened = Session::new();
        ok(&call(
            &mut reopened,
            &json!({"op":"open","path": path.to_string_lossy()}),
        ));
        let loaded = ok(&call(&mut reopened, &json!({"op":"node.get","id":id})));
        assert_eq!(loaded["model_type"], "SpringPendulum");
        assert_eq!(loaded["param"], parameter_id);
        assert_eq!(loaded["output_scale"], json!([1.25, 1.75]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_settings_leave_the_model_and_revision_unchanged() {
        let mut session = Session::new();
        let created = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"SimplePhysics","name":"Hair"}),
        ));
        let id = created["created"][0]["uuid"].as_u64().unwrap();
        let parameter = ok(&call(
            &mut session,
            &json!({"op":"parameter.create","name":"Phys","dimensions":1}),
        ));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        let path = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"PathDeformer","name":"Rope"}),
        ));
        let path_id = path["created"][0]["uuid"].as_u64().unwrap();
        let mut model = ok(&call(&mut session, &json!({"op":"snapshot"})))["model"].clone();
        model["nodes"]["children"][0]["future_driver"] = json!({"keep": true});
        model["nodes"]["children"][1]["physics"] = json!({
            "type": "SpringPendulum",
            "damping": 1.0,
            "future_path": {"keep": 7}
        });
        model["param"] = json!([
            model["param"][0].clone(),
            {"groupUUID": 40, "name": "Group", "children": []}
        ]);
        ok(&call(&mut session, &json!({"op":"new","model": model})));
        let held = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let held_bytes = crate::document::Document::new(held["model"].clone())
            .encode()
            .unwrap();
        for request in [
            json!({"op":"physics.set","node":id,"model_type":"spring_pendulum"}),
            json!({"op":"physics.set","node":id,"model_type":"pendulum"}),
            json!({"op":"physics.set","node":id,"map_mode":"xy"}),
            json!({"op":"physics.set","node":id,"gravity":"fast"}),
            json!({"op":"physics.set","node":id,"output_scale":[1.0]}),
            json!({"op":"physics.set","node":id,"output_scale":[1.0, null]}),
            json!({"op":"physics.set","node":id,"local_only":"yes"}),
            json!({"op":"physics.set","node":id,"param": path_id}),
            json!({"op":"physics.set","node":id,"param": 40}),
            json!({"op":"physics.set","node":id,"param": 999}),
            json!({"op":"physics.set","node":id,"param": UNASSIGNED_PARAM}),
            json!({"op":"physics.set","node":path_id,"gravity":2.0}),
            json!({"op":"physics.set","node":id}),
            json!({"op":"physics.step","node":id}),
            json!({"op":"node.insert","className":"SimplePhysics","nodes":[id]}),
            json!({"op":"node.convert","className":"SimplePhysics","nodes":[path_id]}),
        ] {
            let failed = call(&mut session, &request);
            assert_eq!(failed["succeeded"], false, "{request} -> {failed}");
            let after = ok(&call(&mut session, &json!({"op":"snapshot"})));
            assert_eq!(after["model"], held["model"], "{request}");
            assert_eq!(after["revision"], held["revision"], "{request}");
            let bytes = crate::document::Document::new(after["model"].clone())
                .encode()
                .unwrap();
            assert_eq!(bytes, held_bytes, "{request}");
        }
        let step = call(&mut session, &json!({"op":"physics.step","node":id}));
        assert!(step["message"]
            .as_str()
            .unwrap()
            .contains("physics dt"));
        ok(&call(
            &mut session,
            &json!({"op":"physics.set","node":id,"gravity":3.0,"param":parameter_id}),
        ));
        let edited = ok(&call(&mut session, &json!({"op":"snapshot"})));
        assert_eq!(
            edited["model"]["nodes"]["children"][0]["future_driver"],
            json!({"keep": true})
        );
        assert_eq!(edited["model"]["nodes"]["children"][0]["gravity"], 3.0);
        assert_eq!(
            edited["model"]["nodes"]["children"][1]["physics"],
            json!({"type":"SpringPendulum","damping":1.0,"future_path":{"keep":7}})
        );
        assert_eq!(edited["model"]["param"][0]["name"], "Phys");
        let mut changed = Vec::new();
        diff_paths(&held["model"], &edited["model"], "", &mut changed);
        assert_eq!(
            changed,
            vec![
                ".nodes.children[0].gravity".to_string(),
                ".nodes.children[0].param".to_string(),
            ]
        );
        let copied = call(&mut session, &json!({"op":"node.copy","nodes":[id]}));
        assert_eq!(copied["succeeded"], false, "{copied}");
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"snapshot"})))["model"],
            edited["model"]
        );
    }

    #[test]
    fn output_scale_axis_keeps_the_other_component() {
        let mut session = Session::new();
        let created = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"SimplePhysics","name":"Hair"}),
        ));
        let id = created["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"physics.set","node":id,"output_scale":[1.25, 1.75]}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"physics.set","node":id,"output_scale_x":1.7}),
        ));
        let node = ok(&call(&mut session, &json!({"op":"node.get","id":id})));
        assert_eq!(node["output_scale"], json!([1.7, 1.75]));
        let before = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let both = call(
            &mut session,
            &json!({"op":"physics.set","node":id,"output_scale":[2.0, 2.0],"output_scale_y":3.0}),
        );
        assert_eq!(both["succeeded"], false, "{both}");
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"}))), before);
    }

    fn diff_paths(before: &Value, after: &Value, path: &str, out: &mut Vec<String>) {
        if before == after {
            return;
        }
        match (before, after) {
            (Value::Object(left), Value::Object(right)) => {
                let mut keys: Vec<&String> = left.keys().chain(right.keys()).collect();
                keys.sort();
                keys.dedup();
                for key in keys {
                    let next = format!("{path}.{key}");
                    match (left.get(key), right.get(key)) {
                        (Some(lhs), Some(rhs)) => diff_paths(lhs, rhs, &next, out),
                        _ => out.push(next),
                    }
                }
            }
            (Value::Array(left), Value::Array(right)) if left.len() == right.len() => {
                for (index, (lhs, rhs)) in left.iter().zip(right.iter()).enumerate() {
                    diff_paths(lhs, rhs, &format!("{path}[{index}]"), out);
                }
            }
            _ => out.push(path.to_string()),
        }
    }
}
