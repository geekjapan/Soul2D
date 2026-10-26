use serde_json::{json, Value};
use soul2d_core::Session;

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn transform() -> Value {
    json!({"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]})
}

/// Part 4 is a quad; parameter 7 is 1D with its default on the middle keypoint, parameter 8 is 2D with its default
/// on keypoint [1, 1], parameter 9 is 1D with its default between keypoints. Parameter 10 is 1D with its default on
/// its first keypoint, stored as integers like its binding, which is set at every key. Parameter 11 has its default
/// on an axis point stored at f32 precision, with a binding set there. Parameter 12 has its default on the first point
/// of an axis that is not symmetric, with a binding set at every key.
fn session() -> Session {
    let part = json!({
        "uuid": 4, "name": "Quad", "type": "Part", "enabled": true, "zsort": 0.0, "transform": transform(),
        "children": [], "lockToRoot": false, "pinToMesh": false,
        "mesh": {"verts": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0], "uvs": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
                 "indices": [0, 1, 2, 0, 2, 3], "origin": [0.0, 0.0]},
        "textures": [], "blend_mode": "Normal", "tint": [1, 1, 1], "screenTint": [0, 0, 0],
        "emissionStrength": 1, "opacity": 1,
    });
    let parameter = |id: u64, vec2: bool, min: [f64; 2], defaults: [f64; 2], y: Value| {
        json!({"uuid": id, "name": format!("P{id}"), "is_vec2": vec2, "min": min, "max": [1.0, 1.0],
               "defaults": defaults, "axis_points": [[0.0, 0.5, 1.0], y], "merge_mode": "Additive", "bindings": []})
    };
    let mut stored = parameter(10, false, [-1.0, 0.0], [0.0, 0.0], json!([0.0]));
    for (key, value) in [("min", json!([-1, 0])), ("max", json!([1, 1])), ("defaults", json!([-1, 0]))] {
        stored[key] = value;
    }
    stored["axis_points"] = json!([[0, 0.5, 1], [0]]);
    stored["bindings"] = json!([{"node": 4, "param_name": "transform.t.y", "interpolate_mode": "Linear",
                                 "values": [[0], [0], [1]], "isSet": [[true], [true], [true]]}]);
    let mut third = parameter(11, false, [0.0, 0.0], [1.0, 0.0], json!([0.0]));
    third["max"] = json!([3.0, 1.0]);
    third["axis_points"] = json!([[0.0, 0.33333334, 1.0], [0.0]]);
    third["bindings"] = json!([{"node": 4, "param_name": "transform.t.x", "interpolate_mode": "Linear",
                                "values": [[0.0], [0.5], [0.0]], "isSet": [[false], [true], [false]]}]);
    let mut uneven = parameter(12, false, [0.0, 0.0], [0.0, 0.0], json!([0.0]));
    uneven["axis_points"] = json!([[0.0, 0.3, 1.0], [0.0]]);
    uneven["bindings"] = json!([{"node": 4, "param_name": "transform.t.x", "interpolate_mode": "Linear",
                                 "values": [[0.0], [0.0], [1.0]], "isSet": [[true], [true], [true]]}]);
    let model = json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0, "transform": transform(),
                  "lockToRoot": false, "pinToMesh": false, "children": [part]},
        "param": [
            parameter(7, false, [-1.0, 0.0], [0.0, 0.0], json!([0.0])),
            parameter(8, true, [-1.0, -1.0], [0.0, 0.0], json!([0.0, 0.5, 1.0])),
            parameter(9, false, [-1.0, 0.0], [0.5, 0.0], json!([0.0])),
            stored,
            third,
            uneven,
        ],
        "animations": {},
    });
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new", "model": model})));
    session
}

fn trs(parameter: u64, key: [usize; 2], x: f64) -> Value {
    json!({"op": "binding.set_trs", "parameter": parameter, "nodes": [4], "key": key, "translation": [x, 0.0]})
}

fn lock(session: &mut Session, locked: bool) {
    let state = result(&request(session, json!({"op": "default_form.lock.set", "locked": locked}))).clone();
    assert_eq!(state["defaultFormLocked"], locked);
}

/// The request fails with the lock's message and changes nothing.
fn refused(session: &mut Session, value: Value, parameter: u64, name: &str) {
    let before = session.snapshot();
    let response = request(session, value);
    let message = format!(
        "the default forms are locked: this edit changes the {name} keyform of node 4 at the default value of \
         parameter {parameter}; turn the lock off with default_form_lock_set or the \"Lock default forms\" check box"
    );
    assert_eq!((response["succeeded"].clone(), response["message"].clone()), (json!(false), json!(message)));
    assert_eq!(session.snapshot(), before);
}

#[test]
fn a_locked_default_form_refuses_edits_of_the_keyforms_at_the_default_value() {
    let mut session = session();
    assert_eq!(session.snapshot()["defaultFormLocked"], false, "unlocked at first");
    // Unlocked, the default keyforms can be made and edited; those edits are undone below while locked. The other
    // keys of parameter 7 hold the same value, so unsetting its default key leaves the value as it is.
    for key in [[0, 0], [2, 0], [1, 0]] {
        result(&request(&mut session, trs(7, key, 0.5)));
    }
    result(&request(&mut session, trs(8, [1, 1], 0.5)));
    let deform = json!({"op": "binding.set_deform", "parameter": 7, "nodes": [4], "key": [1, 0],
                        "values": [1, 0, 0, 0, 0, 0, 0, 0]});
    result(&request(&mut session, deform.clone()));
    lock(&mut session, true);
    let revision = session.snapshot()["revision"].clone();
    assert_eq!(session.snapshot()["canUndo"], true);
    refused(&mut session, trs(8, [1, 1], 0.25), 8, "transform.t.x");
    // A parameter inside a group is locked as well.
    result(&request(&mut session, json!({"op": "parameter.group.create", "members": [7]})));
    assert!(session.snapshot()["model"]["param"].as_array().unwrap().iter().all(|item| item["uuid"] != 7));
    refused(&mut session, trs(7, [1, 0], 0.25), 7, "transform.t.x");
    let mut moved = deform.clone();
    moved["values"][0] = json!(2);
    refused(&mut session, moved, 7, "deform");
    let unset = json!({"op": "binding.keypoint", "parameter": 7, "key": [1, 0], "action": "unset"});
    refused(&mut session, unset, 7, "transform.t.x");
    assert_eq!(session.snapshot()["revision"].as_u64(), revision.as_u64().map(|r| r + 1), "a refused edit is no step");
    // The same value again is no edit. Other keypoints, a 2D parameter's other keypoints, a default between
    // keypoints and a default keyform made while locked are not locked.
    result(&request(&mut session, trs(7, [1, 0], 0.5)));
    result(&request(&mut session, trs(7, [2, 0], 1.0)));
    result(&request(&mut session, trs(8, [1, 0], 1.0)));
    result(&request(&mut session, trs(9, [1, 0], 1.0)));
    let rotate =
        json!({"op": "binding.set_trs", "parameter": 8, "nodes": [4], "key": [1, 1], "rotation_degrees": 30.0});
    result(&request(&mut session, rotate));
    let mut deform = deform;
    deform["key"] = json!([0, 0]);
    result(&request(&mut session, deform));
    // An edit that writes back a stored 0 as 0.0 does not change it, nor does refilling for a repeating parameter.
    let stored = |key: [usize; 2]| {
        json!({"op": "binding.set_trs", "parameter": 10, "nodes": [4], "key": key, "translation": [0.0, 2.0]})
    };
    result(&request(&mut session, stored([2, 0])));
    result(&request(&mut session, json!({"op": "parameter.set_repeat", "id": 10, "repeat": true})));
    refused(&mut session, stored([0, 0]), 10, "transform.t.y");
    // Flipping the axis brings the other end's keyform to the default; the axis points it writes back as 0.0 are the
    // same numbers.
    refused(&mut session, json!({"op": "parameter.flip", "id": 10}), 10, "transform.t.y");
    refused(&mut session, trs(11, [1, 0], 0.25), 11, "transform.t.x");
    // On an axis that is not symmetric a flip moves the points too: the default keyform is still compared, and a
    // default left between keypoints loses its keyform.
    refused(&mut session, json!({"op": "parameter.flip", "id": 12}), 12, "transform.t.x");
    refused(&mut session, json!({"op": "parameter.flip", "id": 11}), 11, "transform.t.x");
    // A mesh edit re-expresses the keyforms of its node on the new vertices.
    result(&request(&mut session, json!({"op": "mesh.edit", "id": 4, "edits": [{"op": "add", "pos": [0.5, 0.5]}]})));
    // A 1D parameter is evaluated on its one Y point, so a Y default does not take the lock off.
    result(&request(&mut session, json!({"op": "parameter.set_defaults", "id": 7, "value": [0.0, 0.5]})));
    refused(&mut session, trs(7, [1, 0], 0.25), 7, "transform.t.x");
    // Editing a parameter is not editing a keyform.
    result(&request(&mut session, json!({"op": "parameter.set_defaults", "id": 7, "value": [1.0, 0.0]})));
    result(&request(&mut session, json!({"op": "parameter.set_defaults", "id": 7, "value": [0.0, 0.0]})));
    let axes = json!({"op": "parameter.set_axes", "id": 8, "min": [-1, -1], "max": [1, 1],
                      "axis_points": [[0.0, 0.5, 1.0], [0.0, 0.25, 0.5, 1.0]]});
    result(&request(&mut session, axes));
    // Dropping the default's axis point drops its keyform.
    let dropped = json!({"op": "parameter.set_axes", "id": 8, "min": [-1, -1], "max": [1, 1],
                         "axis_points": [[0.0, 0.5, 1.0], [0.0, 0.25, 1.0]]});
    refused(&mut session, dropped, 8, "transform.t.x");
    // Removing a binding is no keyform edit.
    let remove =
        json!({"op": "binding.remove", "parameter": 8, "bindings": [{"node": 4, "param_name": "transform.r.z"}]});
    assert_eq!(result(&request(&mut session, remove))["removed"], 1);
    // Undo is no edit: it can go back past the default keyform edits made before the lock.
    while session.snapshot()["canUndo"] == true {
        result(&request(&mut session, json!({"op": "undo"})));
    }
    assert_eq!(session.snapshot()["model"]["param"][0]["bindings"], json!([]));
    result(&request(&mut session, json!({"op": "redo"})));
    // Unlocked again, the default keyform takes the edit.
    lock(&mut session, false);
    result(&request(&mut session, trs(7, [1, 0], 0.25)));
    // A new document starts unlocked.
    lock(&mut session, true);
    result(&request(&mut session, json!({"op": "new"})));
    assert_eq!(session.snapshot()["defaultFormLocked"], false);
    for locked in [json!(1), json!("true"), Value::Null] {
        let response = request(&mut session, json!({"op": "default_form.lock.set", "locked": locked}));
        assert_eq!(response["message"], "locked must be a boolean");
    }
    assert_eq!(session.snapshot()["defaultFormLocked"], false);
}

#[test]
fn a_locked_default_form_lets_a_depth_rig_refresh_the_keyforms_it_drives() {
    let mut session = Session::new();
    let created = |session: &mut Session, value: Value| result(&request(session, value))["created"][0]["uuid"].clone();
    let rig = created(&mut session, json!({"op": "node.create", "className": "DepthRigRoot", "name": "Rig"}));
    let bone = created(&mut session, json!({"op": "node.create", "className": "DepthBone", "name": "Bone",
        "parents": [rig], "restHead": [0.0, 0.0, 0.0], "restTail": [0.0, 100.0, 0.0]}));
    let grid = created(&mut session, json!({"op": "node.create", "className": "GridDeformer", "name": "Grid"}));
    for step in [
        json!({"op": "deformer.define_grid", "nodes": [grid], "axisX": [0.0, 1.0], "axisY": [0.0, 1.0]}),
        json!({"op": "depth.set", "target": grid, "depths": [0.0, 0.0, 0.0, 0.0]}),
        json!({"op": "rig.add_source", "root": rig, "target": grid, "bone": bone}),
    ] {
        result(&request(&mut session, step));
    }
    let parameter = result(&request(&mut session, json!({"op": "parameter.create", "name": "Yaw", "dimensions": 1,
        "min": [-1.0, 0.0], "max": [1.0, 1.0]})))["uuid"].clone();
    lock(&mut session, true);
    // A pose at another keypoint refreshes the driven deform there, and refills its default key, which is not set.
    for angle in [1.5707963267948966, 0.5] {
        result(&request(&mut session, json!({"op": "binding.set_trs", "parameter": parameter, "nodes": [bone],
            "key": [2, 0], "rotation_y": angle})));
    }
    // A depth edit refreshes the driven deform at every keypoint; at the default the bone is at rest.
    result(&request(&mut session, json!({"op": "depth.set", "target": grid, "depths": [1.0, 1.0, 1.0, 1.0]})));
    let parameter = result(&request(&mut session, json!({"op": "parameter.get", "id": parameter}))).clone();
    let deform = parameter["bindings"].as_array().unwrap().iter().find(|b| b["param_name"] == "deform").unwrap();
    assert_eq!(deform["isSet"], json!([[true], [true], [true]]), "every keypoint refreshed, the default too");
    assert_eq!(parameter["defaults"], json!([0.0, 0.0]));
    // With the default key set now, a pose at another keypoint still refreshes it to the same rest deform.
    let id = parameter["uuid"].clone();
    result(&request(&mut session, json!({"op": "binding.set_trs", "parameter": id, "nodes": [bone], "key": [2, 0],
        "rotation_y": 1.0})));
}
