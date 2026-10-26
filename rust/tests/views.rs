//! `view.*` (Cubism multiview): views with their own parameter values and one active view.

use serde_json::{json, Value};
use soul2d_core::Session;

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    response["result"].clone()
}

fn message(response: &Value) -> &str {
    assert_eq!(response["succeeded"], false, "request passed: {response}");
    response["message"].as_str().unwrap()
}

fn transform() -> Value {
    json!({"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]})
}

/// Part 4 is a unit quad moved along x by parameter 7 (-10 at its minimum, +10 at its maximum). Parameter 9 binds
/// nothing.
fn model() -> Value {
    let part = json!({
        "uuid": 4, "name": "Quad", "type": "Part", "enabled": true, "zsort": 0.0, "transform": transform(),
        "children": [], "lockToRoot": false, "pinToMesh": false,
        "mesh": {"verts": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0], "uvs": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
                 "indices": [0, 1, 2, 0, 2, 3], "origin": [0.0, 0.0]},
        "textures": [4294967295u32], "blend_mode": "Normal", "tint": [1, 1, 1], "screenTint": [0, 0, 0],
        "emissionStrength": 1, "opacity": 1,
    });
    let parameter = |id: u64, bindings: Value| {
        json!({"uuid": id, "name": format!("P{id}"), "is_vec2": false, "min": [-1.0, 0.0], "max": [1.0, 1.0],
               "defaults": [0.0, 0.0], "axis_points": [[0.0, 0.5, 1.0], [0.0]], "merge_mode": "Additive",
               "bindings": bindings})
    };
    json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0, "transform": transform(),
                  "lockToRoot": false, "pinToMesh": false, "children": [part]},
        "param": [
            parameter(7, json!([{"node": 4, "param_name": "transform.t.x", "interpolate_mode": "Linear",
                                 "values": [[-10.0], [0.0], [10.0]], "isSet": [[true], [true], [true]]}])),
            parameter(9, json!([])),
        ],
        "animations": {},
    })
}

fn session() -> Session {
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new", "model": model()})));
    session
}

fn views(ids: &[u64], active: u64, synchronized: bool) -> Value {
    json!({
        "views": ids.iter().map(|id| json!({"id": id, "active": *id == active})).collect::<Vec<_>>(),
        "active": active,
        "synchronized": synchronized,
    })
}

fn set(session: &mut Session, id: u64, x: f64) {
    result(&request(session, json!({"op": "value.set", "id": id, "value": [x, 0.0]})));
}

fn select(session: &mut Session, id: u64) -> Value {
    result(&request(session, json!({"op": "view.select", "id": id})))
}

fn value_of(session: &Session, id: &str) -> Value {
    session.snapshot()["values"][id].clone()
}

/// The left edge of the quad in the evaluated scene: 0 at the default, moved by parameter 7.
fn left(session: &mut Session) -> f64 {
    let geometry = result(&request(session, json!({"op": "scene.geometry"})));
    let vertices = geometry["parts"][0]["vertices"].as_array().unwrap().clone();
    vertices.iter().map(|vertex| vertex[0].as_f64().unwrap()).fold(f64::INFINITY, f64::min)
}

#[test]
fn each_view_keeps_its_own_values_and_the_active_one_poses_the_model() {
    let mut session = session();
    let start = session.snapshot();
    assert_eq!(start["views"], views(&[1], 1, false));
    assert_eq!(result(&request(&mut session, json!({"op": "view.list"}))), views(&[1], 1, false));

    set(&mut session, 7, 1.0);
    assert_eq!(left(&mut session), 10.0);
    // A new view shows the default forms and becomes active.
    assert_eq!(result(&request(&mut session, json!({"op": "view.add"}))), views(&[1, 2], 2, false));
    assert_eq!(value_of(&session, "7"), json!([0.0, 0.0]));
    assert_eq!(left(&mut session), 0.0);
    set(&mut session, 7, -0.5);
    assert_eq!(left(&mut session), -5.0);

    assert_eq!(select(&mut session, 1), views(&[1, 2], 1, false));
    assert_eq!(value_of(&session, "7"), json!([1.0, 0.0]));
    assert_eq!(left(&mut session), 10.0);
    select(&mut session, 2);
    assert_eq!(value_of(&session, "7"), json!([-0.5, 0.0]));
    assert_eq!(left(&mut session), -5.0);
    // Selecting the active view changes nothing.
    assert_eq!(select(&mut session, 2), views(&[1, 2], 2, false));
    assert_eq!(value_of(&session, "7"), json!([-0.5, 0.0]));

    // No view operation is an edit.
    let after = session.snapshot();
    let edits = |state: &Value| (state["revision"].clone(), state["history"].clone());
    assert_eq!(edits(&after), edits(&start));
    assert_eq!(after["views"], views(&[1, 2], 2, false));
}

#[test]
fn a_view_comes_back_fitted_to_the_parameters_the_document_has_now() {
    let mut session = session();
    result(&request(&mut session, json!({"op": "view.add"})));
    set(&mut session, 7, 0.5);
    set(&mut session, 9, 1.0);
    select(&mut session, 1);
    result(&request(&mut session, json!({"op": "parameter.delete", "id": 9})));
    select(&mut session, 2);
    assert_eq!(session.snapshot()["values"], json!({"7": [0.5, 0.0]}));

    // Undo brings the parameter back at its default, in the active view, and leaves the views alone.
    result(&request(&mut session, json!({"op": "undo"})));
    assert_eq!(session.snapshot()["values"], json!({"7": [0.5, 0.0], "9": [0.0, 0.0]}));
    assert_eq!(session.snapshot()["views"], views(&[1, 2], 2, false));
}

#[test]
fn a_parameter_made_with_the_id_of_a_removed_one_starts_at_its_default_in_every_view() {
    let mut session = session();
    let create = |session: &mut Session| result(&request(session, json!({"op": "parameter.create", "name": "New",
        "dimensions": 1, "min": [-1.0, 0.0], "max": [1.0, 1.0]})))["uuid"].as_u64().unwrap();
    let removed = create(&mut session);
    result(&request(&mut session, json!({"op": "view.add"})));
    set(&mut session, removed, 0.8);
    select(&mut session, 1);
    result(&request(&mut session, json!({"op": "parameter.delete", "id": removed})));
    assert_eq!(create(&mut session), removed, "the new parameter takes the free id");
    select(&mut session, 2);
    assert_eq!(value_of(&session, &removed.to_string()), json!([0.0, 0.0]));
}

#[test]
fn a_value_outside_a_narrowed_range_falls_back_to_the_default() {
    let mut session = session();
    result(&request(&mut session, json!({"op": "view.add"})));
    set(&mut session, 7, 1.0);
    set(&mut session, 9, -0.75);
    select(&mut session, 1);
    result(&request(&mut session, json!({"op": "parameter.set_axes", "id": 7, "min": [-1.0, 0.0], "max": [0.5, 1.0],
        "axis_points": [[0.0, 0.5, 1.0], [0.0]]})));
    select(&mut session, 2);
    assert_eq!(session.snapshot()["values"], json!({"7": [0.0, 0.0], "9": [-0.75, 0.0]}));
}

#[test]
fn a_switch_ends_the_previews_made_from_the_values_it_leaves() {
    let mut session = session();
    result(&request(&mut session, json!({"op": "view.add"})));
    set(&mut session, 7, 0.5);
    result(&request(&mut session, json!({"op": "random_pose.seek", "time": 1.0, "seed": 5})));
    assert!(!session.snapshot()["randomPose"].is_null());
    select(&mut session, 1);
    let state = session.snapshot();
    assert_eq!((state["randomPose"].clone(), state["values"]["7"].clone()), (Value::Null, json!([0.0, 0.0])));
    result(&request(&mut session, json!({"op": "physics.step", "dt": 0.0})));
    assert_eq!(session.snapshot()["physics"]["active"], true);
    select(&mut session, 2);
    let state = session.snapshot();
    assert_eq!((state["physics"]["active"].clone(), state["values"]["7"].clone()), (json!(false), json!([0.5, 0.0])));
}

#[test]
fn a_depth_rig_pose_comes_back_with_its_view() {
    let mut session = Session::new();
    let created = |session: &mut Session, value: Value| result(&request(session, value))["created"][0]["uuid"].clone();
    let rig = created(&mut session, json!({"op": "node.create", "className": "DepthRigRoot", "name": "Rig"}));
    let bone = created(&mut session, json!({"op": "node.create", "className": "DepthBone", "name": "Bone",
        "parents": [rig], "restHead": [0.0, 0.0, 0.0], "restTail": [0.0, 100.0, 0.0]}));
    let grid = created(&mut session, json!({"op": "node.create", "className": "GridDeformer", "name": "Grid"}));
    for step in [
        json!({"op": "deformer.define_grid", "nodes": [grid], "axisX": [0.0, 1.0], "axisY": [0.0, 1.0]}),
        json!({"op": "depth.set", "target": grid, "depths": [1.0, 1.0, 1.0, 1.0]}),
        json!({"op": "rig.add_source", "root": rig, "target": grid, "bone": bone}),
    ] {
        result(&request(&mut session, step));
    }
    let parameter = result(&request(&mut session, json!({"op": "parameter.create", "name": "Yaw", "dimensions": 1,
        "min": [-1.0, 0.0], "max": [1.0, 1.0]})))["uuid"].clone();
    result(&request(&mut session, json!({"op": "binding.set_trs", "parameter": parameter, "nodes": [bone],
        "key": [2, 0], "rotation_y": 1.0})));
    let offsets =
        |session: &mut Session| result(&request(session, json!({"op": "scene.evaluate"})))["localOffsets"].clone();
    // The document as a new session shows it at the default forms.
    let mut fresh = Session::new();
    result(&request(&mut fresh, json!({"op": "new", "model": session.snapshot()["model"].clone()})));
    let rest = offsets(&mut fresh);
    result(&request(&mut session, json!({"op": "value.set", "id": parameter, "value": [0.5, 0.0]})));
    let posed = offsets(&mut session);
    assert_ne!(posed, rest, "the pose moves the grid");
    result(&request(&mut session, json!({"op": "view.add"})));
    assert_eq!(offsets(&mut session), rest);
    select(&mut session, 1);
    assert_eq!(offsets(&mut session), posed);
    // After an edit made in another view, the pose preview is made again from the view's values, as `value.set`
    // makes it.
    select(&mut session, 2);
    result(&request(&mut session, json!({"op": "binding.set_trs", "parameter": parameter, "nodes": [bone],
        "key": [1, 0], "rotation_y": 0.5})));
    let mut fresh = Session::new();
    result(&request(&mut fresh, json!({"op": "new", "model": session.snapshot()["model"].clone()})));
    result(&request(&mut fresh, json!({"op": "value.set", "id": parameter, "value": [0.5, 0.0]})));
    let reposed = offsets(&mut fresh);
    assert_ne!(reposed, posed, "the edit changes the pose");
    select(&mut session, 1);
    assert_eq!(offsets(&mut session), reposed);
    // Turning sync off gives every view the shared values with their pose.
    select(&mut session, 2);
    select(&mut session, 1);
    result(&request(&mut session, json!({"op": "view.sync.set", "synchronized": true})));
    result(&request(&mut session, json!({"op": "value.set", "id": parameter, "value": [1.0, 0.0]})));
    let shared = offsets(&mut session);
    assert_ne!(shared, reposed, "the shared value has its own pose");
    result(&request(&mut session, json!({"op": "view.sync.set", "synchronized": false})));
    select(&mut session, 2);
    assert_eq!(offsets(&mut session), shared);
}

#[test]
fn removing_a_view_activates_its_neighbour_and_the_last_view_stays() {
    let mut session = session();
    for _ in 0..2 {
        result(&request(&mut session, json!({"op": "view.add"})));
    }
    set(&mut session, 7, 0.25);
    select(&mut session, 2);
    set(&mut session, 7, -0.25);
    select(&mut session, 3);
    // The active view was the last: the one before it becomes active.
    assert_eq!(result(&request(&mut session, json!({"op": "view.remove", "id": 3}))), views(&[1, 2], 2, false));
    assert_eq!(value_of(&session, "7"), json!([-0.25, 0.0]));
    // Otherwise the next one does.
    select(&mut session, 1);
    assert_eq!(result(&request(&mut session, json!({"op": "view.remove", "id": 1}))), views(&[2], 2, false));
    assert_eq!(value_of(&session, "7"), json!([-0.25, 0.0]));
    // An inactive view goes without a switch, and a new view takes the next id.
    result(&request(&mut session, json!({"op": "view.add"})));
    assert_eq!(result(&request(&mut session, json!({"op": "view.remove", "id": 2}))), views(&[3], 3, false));

    let before = session.snapshot();
    let last = request(&mut session, json!({"op": "view.remove", "id": 3}));
    assert_eq!(message(&last), "the last view cannot be removed");
    assert_eq!(message(&request(&mut session, json!({"op": "view.remove", "id": 8}))), "view 8 was not found");
    assert_eq!(message(&request(&mut session, json!({"op": "view.select", "id": 8}))), "view 8 was not found");
    for id in [json!("3"), json!(-1), json!(1.5), Value::Null] {
        assert_eq!(message(&request(&mut session, json!({"op": "view.select", "id": id}))), "id must be a view id");
    }
    assert_eq!(message(&request(&mut session, json!({"op": "view.sync.set", "synchronized": 1}))),
        "synchronized must be a boolean");
    assert_eq!(session.snapshot(), before);
}

#[test]
fn synchronized_views_share_the_active_values_and_keep_them_when_unsynchronized() {
    let mut session = session();
    set(&mut session, 7, 0.5);
    result(&request(&mut session, json!({"op": "view.add"})));
    set(&mut session, 7, -0.5);
    assert_eq!(result(&request(&mut session, json!({"op": "view.sync.set", "synchronized": true}))),
        views(&[1, 2], 2, true));
    select(&mut session, 1);
    assert_eq!(value_of(&session, "7"), json!([-0.5, 0.0]));
    set(&mut session, 7, 1.0);
    select(&mut session, 2);
    assert_eq!(value_of(&session, "7"), json!([1.0, 0.0]));
    // A view added while synchronized shows the shared values too.
    result(&request(&mut session, json!({"op": "view.add"})));
    assert_eq!(value_of(&session, "7"), json!([1.0, 0.0]));

    result(&request(&mut session, json!({"op": "view.sync.set", "synchronized": false})));
    set(&mut session, 7, -1.0);
    select(&mut session, 1);
    assert_eq!(value_of(&session, "7"), json!([1.0, 0.0]));
    select(&mut session, 3);
    assert_eq!(value_of(&session, "7"), json!([-1.0, 0.0]));
    // Turning sync off again leaves each view its own values.
    result(&request(&mut session, json!({"op": "view.sync.set", "synchronized": false})));
    select(&mut session, 1);
    assert_eq!(value_of(&session, "7"), json!([1.0, 0.0]));
}

#[test]
fn views_are_capped_and_a_new_or_opened_document_starts_with_one() {
    let mut session = session();
    for _ in 1..16 {
        result(&request(&mut session, json!({"op": "view.add"})));
    }
    let before = session.snapshot();
    assert_eq!(message(&request(&mut session, json!({"op": "view.add"}))), "a document has at most 16 views");
    assert_eq!(session.snapshot(), before);
    assert_eq!(before["views"]["active"], 16);

    result(&request(&mut session, json!({"op": "new", "model": model()})));
    assert_eq!(session.snapshot()["views"], views(&[1], 1, false));
}
