//! "Key all" and "unkey all" of the D timeline: keys on one frame for many parameters in one undo step.

use serde_json::{json, Value};
use soul2d_core::Session;

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

/// Parameters in creation order: a 1D one, a 2D one, and a 1D one inside a group. The clip "Idle" has 60 frames.
fn session() -> (Session, [u64; 3]) {
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new"})));
    let mut ids = [0; 3];
    for (index, (name, dimensions)) in [("One", 1), ("Two", 2), ("Grouped", 1)].into_iter().enumerate() {
        let created = result(&request(&mut session, json!({
            "op": "parameter.create", "name": name, "dimensions": dimensions, "min": [-1, -1], "max": [1, 1],
        })))
        .clone();
        ids[index] = created["uuid"].as_u64().unwrap();
    }
    result(&request(&mut session, json!({"op": "parameter.group.create", "name": "Group", "members": [ids[2]]})));
    result(&request(&mut session, json!({"op": "animation.create", "name": "Idle"})));
    (session, ids)
}

/// Every lane of "Idle" as (parameter, axis, keys as (frame, value)), sorted by parameter and axis.
fn lanes(session: &Session) -> Vec<(u64, u64, Vec<(i64, f64)>)> {
    let lanes = session.snapshot()["model"]["animations"]["Idle"]["lanes"].clone();
    let mut list: Vec<_> = lanes
        .as_array()
        .unwrap()
        .iter()
        .map(|lane| {
            let keys = lane["keyframes"].as_array().unwrap();
            let keys = keys.iter().map(|key| (key["frame"].as_i64().unwrap(), key["value"].as_f64().unwrap())).collect();
            (lane["uuid"].as_u64().unwrap(), lane["target"].as_u64().unwrap(), keys)
        })
        .collect();
    list.sort_by_key(|(parameter, axis, _)| (*parameter, *axis));
    list
}

fn history_count(session: &mut Session) -> u64 {
    result(&request(session, json!({"op": "history.summary"})))["count"].as_u64().unwrap()
}

/// A failing request leaves snapshot and history untouched.
fn rejected(session: &mut Session, value: Value) -> String {
    let before = session.snapshot();
    let count = history_count(session);
    let response = request(session, value);
    assert_eq!(response["succeeded"], false, "expected failure: {response}");
    assert_eq!(session.snapshot(), before);
    assert_eq!(history_count(session), count);
    response["message"].as_str().unwrap_or("").to_owned()
}

/// A succeeding request is one undo step that undo reverts and redo restores.
fn one_undo_step(session: &mut Session, value: Value) {
    let before = session.snapshot()["model"].clone();
    let count = history_count(session);
    result(&request(session, value));
    let after = session.snapshot()["model"].clone();
    assert_ne!(after, before, "request changed nothing");
    assert_eq!(history_count(session), count + 1);
    result(&request(session, json!({"op": "undo"})));
    assert_eq!(session.snapshot()["model"], before);
    result(&request(session, json!({"op": "redo"})));
    assert_eq!(session.snapshot()["model"], after);
}

fn pose(session: &mut Session, ids: [u64; 3]) {
    for (id, value) in [(ids[0], [0.5, 0.0]), (ids[1], [0.25, -0.5]), (ids[2], [-1.0, 0.0])] {
        result(&request(session, json!({"op": "value.set", "id": id, "value": value})));
    }
}

#[test]
fn key_all_records_every_parameter_on_the_frame_in_one_undo_step() {
    let (mut s, ids) = session();
    pose(&mut s, ids);
    one_undo_step(&mut s, json!({"op": "animation.key.add_all", "name": "Idle", "frame": 5}));
    assert_eq!(
        lanes(&s),
        [
            (ids[0], 0, vec![(5, 0.5)]),
            (ids[1], 0, vec![(5, 0.25)]),
            (ids[1], 1, vec![(5, -0.5)]),
            (ids[2], 0, vec![(5, -1.0)]),
        ]
    );
}

#[test]
fn key_all_takes_a_list_and_replaces_the_key_already_on_the_frame() {
    let (mut s, ids) = session();
    let set = |frame: i64, value: f64| {
        json!({"op": "animation.key.set", "name": "Idle", "parameter": ids[0], "axis": 0, "frame": frame, "value": value})
    };
    result(&request(&mut s, set(5, -0.25)));
    result(&request(&mut s, set(20, 1.0)));
    pose(&mut s, ids);
    one_undo_step(
        &mut s,
        json!({"op": "animation.key.add_all", "name": "Idle", "frame": 5, "parameters": [ids[1], ids[0]]}),
    );
    assert_eq!(
        lanes(&s),
        [(ids[0], 0, vec![(5, 0.5), (20, 1.0)]), (ids[1], 0, vec![(5, 0.25)]), (ids[1], 1, vec![(5, -0.5)])]
    );
}

#[test]
fn key_all_records_the_previewed_pose_while_the_clip_is_shown() {
    let (mut s, ids) = session();
    for (frame, value) in [(0, 0.0), (10, 1.0)] {
        result(&request(&mut s, json!({
            "op": "animation.key.set", "name": "Idle", "parameter": ids[0], "axis": 0, "frame": frame, "value": value,
        })));
    }
    result(&request(&mut s, json!({"op": "value.set", "id": ids[0], "value": [-0.75, 0.0]})));
    result(&request(&mut s, json!({"op": "animation.seek", "name": "Idle", "frame": 5})));
    result(&request(&mut s, json!({"op": "animation.key.add_all", "name": "Idle", "frame": 5, "parameters": [ids[0]]})));
    assert_eq!(lanes(&s), [(ids[0], 0, vec![(0, 0.0), (5, 0.5), (10, 1.0)])]);
}

#[test]
fn unkey_all_removes_the_keys_of_the_frame_and_no_others() {
    let (mut s, ids) = session();
    pose(&mut s, ids);
    for frame in [5, 9] {
        result(&request(&mut s, json!({"op": "animation.key.add_all", "name": "Idle", "frame": frame})));
    }
    one_undo_step(
        &mut s,
        json!({"op": "animation.key.delete_all", "name": "Idle", "frame": 5, "parameters": [ids[1]]}),
    );
    assert_eq!(
        lanes(&s),
        [
            (ids[0], 0, vec![(5, 0.5), (9, 0.5)]),
            (ids[1], 0, vec![(9, 0.25)]),
            (ids[1], 1, vec![(9, -0.5)]),
            (ids[2], 0, vec![(5, -1.0), (9, -1.0)]),
        ]
    );
    one_undo_step(&mut s, json!({"op": "animation.key.delete_all", "name": "Idle", "frame": 5}));
    assert_eq!(
        lanes(&s),
        [
            (ids[0], 0, vec![(9, 0.5)]),
            (ids[1], 0, vec![(9, 0.25)]),
            (ids[1], 1, vec![(9, -0.5)]),
            (ids[2], 0, vec![(9, -1.0)]),
        ]
    );
    let message = rejected(&mut s, json!({"op": "animation.key.delete_all", "name": "Idle", "frame": 5}));
    assert_eq!(message, "animation \"Idle\" has no key at frame 5");
    let message = rejected(
        &mut s,
        json!({"op": "animation.key.delete_all", "name": "Idle", "frame": 9, "parameters": [ids[0], ids[0]]}),
    );
    assert_eq!(message, format!("parameter {} is listed twice", ids[0]));
}

#[test]
fn a_bad_request_is_refused_and_changes_nothing() {
    let (mut s, ids) = session();
    pose(&mut s, ids);
    let group = s.snapshot()["model"]["param"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|item| item["groupUUID"].as_u64())
        .unwrap();
    let listing = |parameters: Value| json!({"name": "Idle", "frame": 5, "parameters": parameters});
    let not_a_list = "parameters must be a non-empty array of parameter UUIDs".to_owned();
    for op in ["animation.key.add_all", "animation.key.delete_all"] {
        for (arguments, message) in [
            (json!({"name": "Missing", "frame": 5}), "animation \"Missing\" was not found".to_owned()),
            (json!({"name": "Idle", "frame": 60}), "frame must be between 0 and 59".to_owned()),
            (json!({"name": "Idle"}), "frame must be an integer".to_owned()),
            (listing(json!([])), not_a_list.clone()),
            (listing(json!(ids[0])), not_a_list.clone()),
            (listing(json!(["x"])), not_a_list.clone()),
            (listing(json!([999])), "parameter 999 was not found".to_owned()),
            (listing(json!([group])), format!("parameter {group} refers to a parameter group")),
            (listing(json!([ids[1], ids[1]])), format!("parameter {} is listed twice", ids[1])),
        ] {
            let mut bad = arguments.clone();
            bad["op"] = json!(op);
            assert_eq!(rejected(&mut s, bad), message, "{op} {arguments}");
        }
    }

    let mut empty = Session::new();
    result(&request(&mut empty, json!({"op": "new"})));
    result(&request(&mut empty, json!({"op": "animation.create", "name": "Idle"})));
    let message = rejected(&mut empty, json!({"op": "animation.key.add_all", "name": "Idle", "frame": 0}));
    assert_eq!(message, "the model has no parameter to key");
}

#[test]
fn the_tools_run_through_mcp() {
    let (mut s, ids) = session();
    pose(&mut s, ids);
    let mut call = |name: &str, arguments: Value| {
        s.handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}}))
            .unwrap()
    };
    let response = call("animation_key_add_all", json!({"name": "Idle", "frame": 3}));
    assert_eq!(response["result"]["isError"], false, "{response}");
    let response = call("animation_key_delete_all", json!({"name": "Idle", "frame": 3, "parameters": [ids[1]]}));
    assert_eq!(response["result"]["isError"], false, "{response}");
    let refused = call("animation_key_add_all", json!({"name": "Idle"}));
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(refused["result"]["content"][0]["text"].as_str().unwrap().contains("missing required argument(s)"));
    assert_eq!(
        lanes(&s),
        [(ids[0], 0, vec![(3, 0.5)]), (ids[1], 0, vec![]), (ids[1], 1, vec![]), (ids[2], 0, vec![(3, -1.0)])]
    );
}
