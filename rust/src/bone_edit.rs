//! DepthBone rest and constraint edits.
//!
//! The field set matches `SetDepthBoneRestCommand` and
//! `SetDepthBoneConstraintCommand`. `constraintType`, `hingeAxis`,
//! `rotationLimits`, and `maxStepRadians` are stored only. The pose path in
//! `bone_deform` already applies `lockRotation`, `lockTranslation`, and
//! `allowParentToTargets`.

use crate::nodes;
use serde_json::{json, Value};

pub(crate) fn set_rest(
    model: &Value,
    bone_id: u32,
    head: [f64; 3],
    tail: [f64; 3],
    roll: f64,
) -> Result<Value, String> {
    let mut candidate = model.clone();
    let node = bone_mut(&mut candidate, bone_id)?;
    let object = node.as_object_mut().ok_or("node must be an object")?;
    object.insert("restHead".into(), json!(head));
    object.insert("restTail".into(), json!(tail));
    object.insert("restRoll".into(), json!(roll));
    Ok(candidate)
}

pub(crate) fn set_constraint(model: &Value, bone_id: u32, patch: &Value) -> Result<Value, String> {
    let patch = constraint_object(patch)?;
    let mut candidate = model.clone();
    let node = bone_mut(&mut candidate, bone_id)?;
    if let Some(value) = patch.get("constraintType") {
        let text = value.as_str().ok_or("constraintType must be a string")?;
        node.as_object_mut()
            .ok_or("node must be an object")?
            .insert("constraintType".into(), json!(text));
    }
    if let Some(value) = patch.get("lockRotation") {
        insert_bool(node, "lockRotation", value)?;
    }
    if let Some(value) = patch.get("lockTranslation") {
        insert_bool(node, "lockTranslation", value)?;
    }
    if let Some(value) = patch.get("allowParentToTargets") {
        insert_bool(node, "allowParentToTargets", value)?;
    }
    if let Some(value) = patch.get("hingeAxis") {
        let axis = finite_vec3(value, "hingeAxis")?;
        node.as_object_mut()
            .ok_or("node must be an object")?
            .insert("hingeAxis".into(), json!(axis));
    }
    if let Some(value) = patch.get("rotationLimits") {
        let limits = finite_list(value, "rotationLimits")?;
        node.as_object_mut()
            .ok_or("node must be an object")?
            .insert("rotationLimits".into(), json!(limits));
    }
    if let Some(value) = patch.get("maxStepRadians") {
        let step = crate::finite_number(value, "maxStepRadians")?;
        node.as_object_mut()
            .ok_or("node must be an object")?
            .insert("maxStepRadians".into(), json!(step));
    }
    Ok(candidate)
}

fn bone_mut(model: &mut Value, bone_id: u32) -> Result<&mut Value, String> {
    let node = nodes::find_node_mut(model, bone_id)
        .ok_or_else(|| format!("node {bone_id} was not found"))?;
    if node.get("type").and_then(Value::as_str) != Some("DepthBone") {
        return Err("Node is not a DepthBone".into());
    }
    Ok(node)
}

fn constraint_object(value: &Value) -> Result<Value, String> {
    match value {
        Value::Object(_) => Ok(value.clone()),
        Value::String(text) => {
            let parsed = serde_json::from_str::<Value>(text)
                .map_err(|_| "constraint must be a JSON object".to_string())?;
            if !parsed.is_object() {
                return Err("constraint must be a JSON object".into());
            }
            Ok(parsed)
        }
        _ => Err("constraint must be a JSON object".into()),
    }
}

fn insert_bool(node: &mut Value, key: &str, value: &Value) -> Result<(), String> {
    let flag = value
        .as_bool()
        .ok_or_else(|| format!("{key} must be a boolean"))?;
    node.as_object_mut()
        .ok_or("node must be an object")?
        .insert(key.to_string(), json!(flag));
    Ok(())
}

fn finite_vec3(value: &Value, name: &str) -> Result<[f64; 3], String> {
    let items = value
        .as_array()
        .filter(|items| items.len() == 3)
        .ok_or_else(|| format!("{name} must be [x,y,z]"))?;
    let mut output = [0.0; 3];
    for (index, item) in items.iter().enumerate() {
        output[index] = crate::finite_number(item, &format!("{name}[{index}]"))?;
    }
    Ok(output)
}

fn finite_list(value: &Value, name: &str) -> Result<Vec<f64>, String> {
    let items = value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array"))?;
    let mut output = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        output.push(crate::finite_number(item, &format!("{name}[{index}]"))?);
    }
    Ok(output)
}

pub(crate) fn parse_vec3(value: &Value, name: &str) -> Result<[f64; 3], String> {
    finite_vec3(value, name)
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

    fn driven_grid() -> (crate::Session, u64, u64, u64) {
        let mut session = crate::Session::new();
        let rig = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let bone = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Head","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ));
        let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
        let grid = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid"}),
        ));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id}),
        ));
        let parameter = ok(&call(
            &mut session,
            &json!({"op":"parameter.create","name":"Yaw","dimensions":1,"min":[-1.0,0.0],"max":[1.0,1.0]}),
        ));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[2,0],"rotation_y":std::f64::consts::FRAC_PI_2
            }),
        ));
        (session, bone_id, grid_id, parameter_id)
    }

    fn deform_cell(session: &mut crate::Session, parameter_id: u64, grid_id: u64) -> Value {
        let stored = ok(&call(
            session,
            &json!({"op":"parameter.get","id":parameter_id}),
        ));
        stored["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == "deform" && binding["node"] == grid_id)
            .unwrap()
            .clone()
    }

    #[test]
    fn rest_refreshes_deform_in_one_undo_and_round_trips() {
        let (mut session, bone_id, grid_id, parameter_id) = driven_grid();
        let before = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let posed = deform_cell(&mut session, parameter_id, grid_id);
        assert_eq!(posed["isSet"][2][0], true);
        assert_eq!(posed["isSet"][0][0], false);
        ok(&call(
            &mut session,
            &json!({
                "op":"rig.set_rest","bone":bone_id,
                "restHead":[0.0,0.0,0.0],"restTail":[100.0,0.0,0.0],"restRoll":0.25
            }),
        ));
        let bone = ok(&call(&mut session, &json!({"op":"node.get","id":bone_id})));
        assert_eq!(bone["restHead"], json!([0.0, 0.0, 0.0]));
        assert_eq!(bone["restTail"], json!([100.0, 0.0, 0.0]));
        assert_eq!(bone["restRoll"], 0.25);
        let refreshed = deform_cell(&mut session, parameter_id, grid_id);
        assert_eq!(refreshed["isSet"][0][0], true);
        assert_eq!(refreshed["isSet"][2][0], true);
        assert_ne!(refreshed["values"][2][0], posed["values"][2][0]);
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"snapshot"})))["model"],
            before["model"]
        );

        ok(&call(
            &mut session,
            &json!({
                "op":"rig.set_rest","bone":bone_id,
                "restHead":[1.0,2.0,3.0],"restTail":[1.0,12.0,3.0],"restRoll":0.25
            }),
        ));
        let path =
            std::env::temp_dir().join(format!("vsoul-rest-{}-{}.inx", std::process::id(), bone_id));
        let _ = std::fs::remove_file(&path);
        ok(&call(
            &mut session,
            &json!({"op":"save","path":path.to_string_lossy(),"mark_saved":true}),
        ));
        let mut loaded = crate::Session::new();
        ok(&call(
            &mut loaded,
            &json!({"op":"open","path":path.to_string_lossy()}),
        ));
        let _ = std::fs::remove_file(&path);
        let saved = ok(&call(&mut loaded, &json!({"op":"node.get","id":bone_id})));
        assert_eq!(saved["restHead"], json!([1.0, 2.0, 3.0]));
        assert_eq!(saved["restTail"], json!([1.0, 12.0, 3.0]));
        assert_eq!(saved["restRoll"], 0.25);
    }

    #[test]
    fn lock_rotation_removes_the_yaw_offset_in_one_undo() {
        let (mut session, bone_id, grid_id, parameter_id) = driven_grid();
        let before = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let posed = deform_cell(&mut session, parameter_id, grid_id);
        ok(&call(
            &mut session,
            &json!({
                "op":"rig.set_constraint","bone":bone_id,
                "constraint":{"lockRotation":true}
            }),
        ));
        let bone = ok(&call(&mut session, &json!({"op":"node.get","id":bone_id})));
        assert_eq!(bone["lockRotation"], true);
        assert_eq!(bone["allowParentToTargets"], true);
        let locked = deform_cell(&mut session, parameter_id, grid_id);
        assert_ne!(locked["values"][2][0], posed["values"][2][0]);
        let flat = locked["values"][2][0].as_array().unwrap();
        assert!(
            flat.iter().all(|pair| {
                pair.as_array()
                    .unwrap()
                    .iter()
                    .all(|value| value.as_f64().unwrap().abs() < 1e-6)
            }),
            "{flat:?}"
        );
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"snapshot"})))["model"],
            before["model"]
        );
    }

    #[test]
    fn constraint_fields_round_trip_without_solving_a_hinge() {
        let (mut session, bone_id, grid_id, parameter_id) = driven_grid();
        let posed = deform_cell(&mut session, parameter_id, grid_id);
        ok(&call(
            &mut session,
            &json!({
                "op":"rig.set_constraint","bone":bone_id,
                "constraint":{
                    "constraintType":"hinge",
                    "hingeAxis":[1.0,0.0,0.0],
                    "rotationLimits":[-0.5,0.5],
                    "maxStepRadians":0.125
                }
            }),
        ));
        let bone = ok(&call(&mut session, &json!({"op":"node.get","id":bone_id})));
        assert_eq!(bone["constraintType"], "hinge");
        assert_eq!(bone["hingeAxis"], json!([1.0, 0.0, 0.0]));
        assert_eq!(bone["rotationLimits"], json!([-0.5, 0.5]));
        assert_eq!(bone["maxStepRadians"], 0.125);
        assert_eq!(bone["lockRotation"], false);
        let after = deform_cell(&mut session, parameter_id, grid_id);
        assert_eq!(after["values"][2][0], posed["values"][2][0]);
        let path = std::env::temp_dir().join(format!(
            "vsoul-constraint-{}-{}.inx",
            std::process::id(),
            bone_id
        ));
        let _ = std::fs::remove_file(&path);
        ok(&call(
            &mut session,
            &json!({"op":"save","path":path.to_string_lossy(),"mark_saved":true}),
        ));
        let mut loaded = crate::Session::new();
        ok(&call(
            &mut loaded,
            &json!({"op":"open","path":path.to_string_lossy()}),
        ));
        let _ = std::fs::remove_file(&path);
        let saved = ok(&call(&mut loaded, &json!({"op":"node.get","id":bone_id})));
        assert_eq!(saved["constraintType"], "hinge");
        assert_eq!(saved["hingeAxis"], json!([1.0, 0.0, 0.0]));
        assert_eq!(saved["rotationLimits"], json!([-0.5, 0.5]));
        assert_eq!(saved["maxStepRadians"], 0.125);
    }

    #[test]
    fn allow_parent_false_changes_ancestor_compensation_in_one_undo() {
        let mut session = crate::Session::new();
        let rig = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let parent = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Parent","parents":[rig_id],
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        ));
        let parent_id = parent["created"][0]["uuid"].as_u64().unwrap();
        let child = ok(&call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","name":"Child","parents":[parent_id],
                "restHead":[0.0,100.0,0.0],"restTail":[0.0,200.0,0.0]
            }),
        ));
        let child_id = child["created"][0]["uuid"].as_u64().unwrap();
        let parent_grid = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"ParentGrid"}),
        ));
        let parent_grid_id = parent_grid["created"][0]["uuid"].as_u64().unwrap();
        let child_grid = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"ChildGrid","parents":[parent_grid_id]}),
        ));
        let child_grid_id = child_grid["created"][0]["uuid"].as_u64().unwrap();
        for id in [parent_grid_id, child_grid_id] {
            ok(&call(
                &mut session,
                &json!({"op":"deformer.define_grid","nodes":[id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
            ));
        }
        ok(&call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":parent_grid_id,"bone":parent_id}),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":child_grid_id,"bone":child_id}),
        ));
        let parameter = ok(&call(
            &mut session,
            &json!({"op":"parameter.create","name":"Yaw","dimensions":1,"min":[-1.0,0.0],"max":[1.0,1.0]}),
        ));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[parent_id],
                "key":[2,0],"rotation_y":std::f64::consts::FRAC_PI_2
            }),
        ));
        ok(&call(
            &mut session,
            &json!({"op":"depth.set","target":parent_grid_id,"depths":[1.0,1.0,1.0,1.0]}),
        ));
        let before = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let inherited = deform_cell(&mut session, parameter_id, child_grid_id);
        ok(&call(
            &mut session,
            &json!({
                "op":"rig.set_constraint","bone":child_id,
                "constraint":{"allowParentToTargets":false}
            }),
        ));
        let bone = ok(&call(&mut session, &json!({"op":"node.get","id":child_id})));
        assert_eq!(bone["allowParentToTargets"], false);
        let blocked = deform_cell(&mut session, parameter_id, child_grid_id);
        assert_ne!(blocked["values"][2][0], inherited["values"][2][0]);
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"snapshot"})))["model"],
            before["model"]
        );
    }

    #[test]
    fn malformed_rest_and_constraint_leave_the_model_unchanged() {
        let (mut session, bone_id, _, _) = driven_grid();
        let grid = ok(&call(
            &mut session,
            &json!({"op":"node.create","className":"Node","name":"Plain"}),
        ));
        let plain_id = grid["created"][0]["uuid"].as_u64().unwrap();
        let held = ok(&call(&mut session, &json!({"op":"snapshot"})));
        for request in [
            json!({"op":"rig.set_rest","bone":plain_id,"restHead":[0.0,0.0,0.0],"restTail":[0.0,1.0,0.0],"restRoll":0.0}),
            json!({"op":"rig.set_rest","bone":bone_id,"restHead":[1.0,0.0],"restTail":[0.0,1.0,0.0],"restRoll":0.0}),
            json!({"op":"rig.set_rest","bone":bone_id,"restHead":[0.0,0.0,0.0],"restTail":[0.0,1.0,0.0],"restRoll":"inf"}),
            json!({"op":"rig.set_constraint","bone":plain_id,"constraint":{"lockRotation":true}}),
            json!({"op":"rig.set_constraint","bone":bone_id,"constraint":[1,2]}),
            json!({"op":"rig.set_constraint","bone":bone_id,"constraint":"not-json"}),
            json!({"op":"rig.set_constraint","bone":bone_id,"constraint":{"hingeAxis":[1.0,0.0]}}),
            json!({"op":"rig.set_constraint","bone":bone_id,"constraint":{"maxStepRadians":"nope"}}),
            json!({"op":"rig.set_constraint","bone":bone_id,"constraint":{"lockRotation":1}}),
        ] {
            let failed = call(&mut session, &request);
            assert_eq!(failed["succeeded"], false, "{failed}");
            assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"}))), held);
        }
        let not_bone = call(
            &mut session,
            &json!({"op":"rig.set_rest","bone":plain_id,"restHead":[0.0,0.0,0.0],"restTail":[0.0,1.0,0.0],"restRoll":0.0}),
        );
        assert_eq!(not_bone["message"], "Node is not a DepthBone");
    }

    #[test]
    fn constraint_object_parser_rejects_non_objects() {
        let error = constraint_object(&json!("[]")).unwrap_err();
        assert!(
            error.contains("constraint must be a JSON object"),
            "{error}"
        );
        let parsed = constraint_object(&json!("{\"lockRotation\":true}")).unwrap();
        assert_eq!(parsed["lockRotation"], true);
    }

    #[test]
    fn transforming_a_bone_refreshes_deform_and_honors_lock_rotation() {
        let (mut session, bone_id, grid_id, parameter_id) = driven_grid();
        let before = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let posed = deform_cell(&mut session, parameter_id, grid_id);
        ok(&call(
            &mut session,
            &json!({"op":"node.transform","id":bone_id,"translation":[30.0,0.0,0.0]}),
        ));
        let moved = ok(&call(&mut session, &json!({"op":"node.get","id":bone_id})));
        assert_eq!(moved["transform"]["trans"], json!([30.0, 0.0, 0.0]));
        let shifted = deform_cell(&mut session, parameter_id, grid_id);
        assert_ne!(shifted["values"][2][0], posed["values"][2][0]);
        assert!(parameter_channels(&mut session, parameter_id, bone_id)
            .iter()
            .all(|name| name != "transform.t.z"));
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"snapshot"})))["model"],
            before["model"]
        );

        ok(&call(
            &mut session,
            &json!({"op":"rig.set_constraint","bone":bone_id,"constraint":{"lockRotation":true}}),
        ));
        let locked = deform_cell(&mut session, parameter_id, grid_id);
        assert!(offsets_near_zero(&locked["values"][2][0]));
        let locked_snapshot = ok(&call(&mut session, &json!({"op":"snapshot"})));
        ok(&call(
            &mut session,
            &json!({"op":"node.transform","id":bone_id,"rotation":[0.0,0.4,0.0]}),
        ));
        let rotated = ok(&call(&mut session, &json!({"op":"node.get","id":bone_id})));
        assert_eq!(rotated["transform"]["rot"][1], 0.4);
        let still_locked = deform_cell(&mut session, parameter_id, grid_id);
        assert!(offsets_near_zero(&still_locked["values"][2][0]));
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"snapshot"})))["model"],
            locked_snapshot["model"]
        );

        let rig_id = before["model"]["nodes"]["children"][0]["uuid"]
            .as_u64()
            .unwrap();
        let root_before = ok(&call(&mut session, &json!({"op":"snapshot"})));
        ok(&call(
            &mut session,
            &json!({"op":"node.transform","id":rig_id,"translation":[4.0,0.0,0.0]}),
        ));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"node.get","id":rig_id})))["transform"]["trans"],
            json!([4.0, 0.0, 0.0])
        );
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"snapshot"})))["model"],
            root_before["model"]
        );

        let held = ok(&call(&mut session, &json!({"op":"snapshot"})));
        let failed = call(
            &mut session,
            &json!({"op":"node.transform","id":bone_id,"translation":["inf",0.0,0.0]}),
        );
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"}))), held);
        let root_move = call(
            &mut session,
            &json!({"op":"node.move","nodes":[1],"parent":bone_id}),
        );
        assert_eq!(root_move["succeeded"], false, "{root_move}");
        assert_eq!(ok(&call(&mut session, &json!({"op":"snapshot"}))), held);
    }

    #[test]
    fn zsort_on_a_bone_does_not_refresh_every_deform_key() {
        let (mut session, bone_id, grid_id, parameter_id) = driven_grid();
        let before = deform_cell(&mut session, parameter_id, grid_id);
        assert_eq!(before["isSet"][2][0], true);
        assert_eq!(before["isSet"][0][0], false);
        let posed = before["values"][2][0].clone();
        ok(&call(
            &mut session,
            &json!({"op":"node.transform","id":bone_id,"component":"zSort","value":4.0}),
        ));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"node.get","id":bone_id})))["zsort"],
            4.0
        );
        let after = deform_cell(&mut session, parameter_id, grid_id);
        assert_eq!(after["isSet"][0][0], false);
        assert_eq!(after["values"][2][0], posed);
        ok(&call(&mut session, &json!({"op":"undo"})));
        assert_eq!(
            ok(&call(&mut session, &json!({"op":"node.get","id":bone_id})))["zsort"],
            0.0
        );
    }

    fn parameter_channels(
        session: &mut crate::Session,
        parameter_id: u64,
        bone_id: u64,
    ) -> Vec<String> {
        let stored = ok(&call(
            session,
            &json!({"op":"parameter.get","id":parameter_id}),
        ));
        stored["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|binding| binding["node"] == bone_id)
            .map(|binding| binding["param_name"].as_str().unwrap().to_owned())
            .collect()
    }

    fn offsets_near_zero(cell: &Value) -> bool {
        cell.as_array().unwrap().iter().all(|pair| {
            pair.as_array()
                .unwrap()
                .iter()
                .all(|value| value.as_f64().unwrap().abs() < 1e-6)
        })
    }
}
