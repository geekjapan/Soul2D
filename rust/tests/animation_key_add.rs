use serde_json::{json, Value};
use soul2d_core::Session;

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn model() -> Value {
    json!({
        "meta": { "name": "fixture", "version": "1.0-alpha" },
        "nodes": {
            "uuid": 1, "name": "Root", "type": "Node", "enabled": true,
            "zsort": 0.0,
            "transform": { "trans": [0.0,0.0,0.0], "rot": [0.0,0.0,0.0], "scale": [1.0,1.0] },
            "lockToRoot": false, "pinToMesh": false,
        },
        "param": [
            {
                "uuid": 17, "name": "Move", "is_vec2": true,
                "min": [-1.0,-1.0], "max": [1.0,1.0], "defaults": [0.0,0.0],
                "axis_points": [[0.0,1.0],[0.0,1.0]], "merge_mode": "Additive", "bindings": [],
            },
            {
                "uuid": 23, "name": "Blink", "is_vec2": false,
                "min": [-1.0,0.0], "max": [1.0,1.0], "defaults": [0.0,0.0],
                "axis_points": [[0.0,1.0],[0.0]], "merge_mode": "Additive", "bindings": [],
            },
            {
                "groupUUID": 40, "name": "Extras",
                "children": [{
                    "uuid": 41, "name": "Target", "is_vec2": true,
                    "min": [-1.0,-1.0], "max": [1.0,1.0], "defaults": [0.0,0.0],
                    "axis_points": [[0.0,1.0],[0.0,1.0]], "merge_mode": "Additive", "bindings": [],
                }],
            },
        ],
        "animations": {},
    })
}

fn animation(lanes: Value) -> Value {
    json!({
        "length": 12, "timestep": 0.1, "additive": false, "animationWeight": 1.0,
        "leadIn": -1, "leadOut": -1, "lanes": lanes,
    })
}

fn fixture() -> Value {
    let mut model = model();
    model["animations"] = json!({
        "Idle": animation(json!([
            {"uuid":17,"target":0,"interpolation":"Linear","merge_mode":"Forced",
             "keyframes":[{"frame":0,"value":-0.25,"tension":0.5},
                          {"frame":4,"value":-0.5,"tension":0.5}]},
            {"uuid":17,"target":1,"interpolation":"Linear","merge_mode":"Forced",
             "keyframes":[{"frame":0,"value":0.5,"tension":0.5},
                          {"frame":4,"value":0.75,"tension":0.5}]},
        ])),
        "Walk": animation(json!([
            {"uuid":41,"target":0,"interpolation":"Stepped","merge_mode":"Additive",
             "custom_lane":"keep",
             "keyframes":[{"frame":6,"value":0.0,"tension":0.9,"custom_key":true}]},
        ])),
    });
    model
}

fn session() -> Session {
    let mut session = Session::new();
    result(&request(&mut session, json!({"op":"new","model":fixture()})));
    session
}

fn lanes(snapshot: &Value, animation: &str, parameter: u64) -> Vec<Value> {
    snapshot["model"]["animations"][animation]["lanes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|lane| lane["uuid"] == parameter)
        .cloned()
        .collect()
}

fn keys_at(snapshot: &Value, animation: &str, parameter: u64, axis: i64) -> Vec<Value> {
    lanes(snapshot, animation, parameter)
        .into_iter()
        .filter(|lane| lane["target"] == axis)
        .flat_map(|lane| lane["keyframes"].as_array().unwrap().clone())
        .collect()
}

fn set_value(session: &mut Session, id: u64, value: [f64; 2]) {
    result(&request(
        session,
        json!({"op":"value.set","id":id,"value":value}),
    ));
}

#[test]
fn add_writes_both_axes_from_manual_values_in_one_undo() {
    let mut session = session();
    set_value(&mut session, 17, [0.125, 0.375]);
    let revision = session.snapshot()["revision"].as_u64().unwrap();
    let response = request(
        &mut session,
        json!({"op":"animation.key.add","name":"Idle","parameter":17,"frame":2}),
    );
    assert_eq!(result(&response)["revision"].as_u64().unwrap(), revision + 1);
    let axis0 = keys_at(&session.snapshot(), "Idle", 17, 0);
    let axis1 = keys_at(&session.snapshot(), "Idle", 17, 1);
    assert_eq!(
        json!(axis0),
        json!([{"frame":0,"value":-0.25,"tension":0.5},
               {"frame":2,"value":0.125,"tension":0.5},
               {"frame":4,"value":-0.5,"tension":0.5}])
    );
    assert_eq!(
        json!(axis1),
        json!([{"frame":0,"value":0.5,"tension":0.5},
               {"frame":2,"value":0.375,"tension":0.5},
               {"frame":4,"value":0.75,"tension":0.5}])
    );
    result(&request(&mut session, json!({"op":"undo"})));
    assert_eq!(keys_at(&session.snapshot(), "Idle", 17, 0).len(), 2);
    assert_eq!(keys_at(&session.snapshot(), "Idle", 17, 1).len(), 2);
    result(&request(&mut session, json!({"op":"redo"})));
    assert_eq!(keys_at(&session.snapshot(), "Idle", 17, 0).len(), 3);
    assert_eq!(keys_at(&session.snapshot(), "Idle", 17, 1).len(), 3);
}

#[test]
fn scalar_parameter_adds_only_axis_zero() {
    let mut session = session();
    set_value(&mut session, 23, [0.5, 0.0]);
    result(&request(
        &mut session,
        json!({"op":"animation.key.add","name":"Walk","parameter":23,"frame":6}),
    ));
    let target = lanes(&session.snapshot(), "Walk", 23);
    assert_eq!(target.len(), 1);
    assert_eq!(target[0]["target"], 0);
    assert_eq!(target[0]["interpolation"], "Linear");
    assert_eq!(target[0]["merge_mode"], "Forced");
    assert_eq!(
        target[0]["keyframes"],
        json!([{"frame":6,"value":0.5,"tension":0.5}])
    );
}

#[test]
fn add_captures_animation_preview_values_instead_of_manual() {
    let mut session = session();
    set_value(&mut session, 17, [0.0, 0.0]);
    result(&request(
        &mut session,
        json!({"op":"animation.seek","name":"Idle","frame":4}),
    ));
    result(&request(
        &mut session,
        json!({"op":"animation.key.add","name":"Idle","parameter":17,"frame":6}),
    ));
    let axis0 = keys_at(&session.snapshot(), "Idle", 17, 0);
    let axis1 = keys_at(&session.snapshot(), "Idle", 17, 1);
    assert_eq!(
        axis0.last().unwrap(),
        &json!({"frame":6,"value":-0.5,"tension":0.5})
    );
    assert_eq!(
        axis1.last().unwrap(),
        &json!({"frame":6,"value":0.75,"tension":0.5})
    );
    assert!(session.snapshot()["animation"].is_null());
}

#[test]
fn add_updates_existing_key_and_preserves_lane_metadata() {
    let mut session = session();
    set_value(&mut session, 41, [0.42, 0.0]);
    result(&request(
        &mut session,
        json!({"op":"animation.key.add","name":"Walk","parameter":41,"frame":6}),
    ));
    let lane = &lanes(&session.snapshot(), "Walk", 41)[0];
    assert_eq!(lane["interpolation"], "Stepped");
    assert_eq!(lane["merge_mode"], "Additive");
    assert_eq!(lane["custom_lane"], "keep");
    assert_eq!(
        lane["keyframes"],
        json!([{"frame":6,"value":0.42,"tension":0.9,"custom_key":true}])
    );
}

#[test]
fn repeat_add_is_a_noop() {
    let mut session = session();
    result(&request(
        &mut session,
        json!({"op":"animation.key.add","name":"Idle","parameter":17,"frame":2}),
    ));
    let revision = session.snapshot()["revision"].as_u64().unwrap();
    let snapshot = session.snapshot();
    result(&request(
        &mut session,
        json!({"op":"animation.key.add","name":"Idle","parameter":17,"frame":2}),
    ));
    assert_eq!(session.snapshot()["revision"], revision);
    assert_eq!(session.snapshot(), snapshot);
    result(&request(&mut session, json!({"op":"undo"})));
    assert_eq!(keys_at(&session.snapshot(), "Idle", 17, 0).len(), 2);
    assert_eq!(session.snapshot()["canRedo"], true);
}

#[test]
fn failing_second_axis_rolls_back_everything() {
    let mut fixture = fixture();
    fixture["animations"]["Idle"]["lanes"] = json!([
        {"uuid":17,"target":0,"interpolation":"Linear","merge_mode":"Forced",
         "keyframes":[{"frame":0,"value":-0.25,"tension":0.5},
                      {"frame":4,"value":-0.5,"tension":0.5}]},
        {"uuid":17,"target":1,"interpolation":"Linear","merge_mode":"Forced",
         "keyframes":[{"frame":99,"value":0.0,"tension":0.5}]},
    ]);
    let mut session = Session::new();
    result(&request(&mut session, json!({"op":"new","model":fixture})));
    let before = session.snapshot();
    let response = request(
        &mut session,
        json!({"op":"animation.key.add","name":"Idle","parameter":17,"frame":2}),
    );
    assert_eq!(response["succeeded"], false);
    assert_eq!(session.snapshot(), before);
    assert_eq!(keys_at(&session.snapshot(), "Idle", 17, 0).len(), 2);
}

#[test]
fn invalid_targets_reject_without_changes() {
    let mut session = session();
    let before = session.snapshot();
    for invalid in [
        json!({"op":"animation.key.add","parameter":17,"frame":2}),
        json!({"op":"animation.key.add","name":"Missing","parameter":17,"frame":2}),
        json!({"op":"animation.key.add","name":"Idle","frame":2}),
        json!({"op":"animation.key.add","name":"Idle","parameter":999,"frame":2}),
        json!({"op":"animation.key.add","name":"Idle","parameter":40,"frame":2}),
        json!({"op":"animation.key.add","name":"Idle","parameter":17,"frame":-1}),
        json!({"op":"animation.key.add","name":"Idle","parameter":17,"frame":12}),
        json!({"op":"animation.key.add","name":"Idle","parameter":17,"frame":2.5}),
        json!({"op":"animation.key.add","name":"Idle","parameter":"17","frame":2}),
    ] {
        assert_eq!(request(&mut session, invalid.clone())["succeeded"], false, "{invalid}");
        assert_eq!(session.snapshot(), before);
    }
}

#[test]
fn add_does_not_touch_the_clipboard() {
    let mut session = session();
    result(&request(
        &mut session,
        json!({"op":"animation.key.copy","name":"Idle","parameter":17,"frame":4}),
    ));
    let clipboard = session.snapshot()["animationClipboard"].clone();
    result(&request(
        &mut session,
        json!({"op":"animation.key.add","name":"Walk","parameter":23,"frame":6}),
    ));
    assert_eq!(session.snapshot()["animationClipboard"], clipboard);
}

#[test]
fn mcp_animation_key_add_reports_schema_and_shares_state() {
    let mut session = session();
    let listed = session
        .handle_mcp(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .unwrap();
    let tools = listed["result"]["tools"].as_array().unwrap();
    let tool = tools
        .iter()
        .find(|tool| tool["name"] == "animation_key_add")
        .expect("animation_key_add is not listed");
    assert_eq!(
        tool["inputSchema"]["required"],
        json!(["name", "parameter", "frame"])
    );
    assert_eq!(
        tool["inputSchema"]["properties"]["frame"],
        json!({"type":"integer","minimum":0,"maximum":2147483647})
    );
    let called = session
        .handle_mcp(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"animation_key_add",
            "arguments":{"name":"Idle","parameter":17,"frame":2},
        }}))
        .unwrap();
    assert_eq!(called["result"]["isError"], false);
    assert_eq!(keys_at(&session.snapshot(), "Idle", 17, 0).len(), 3);
    let read = session
        .handle_mcp(&json!({"jsonrpc":"2.0","id":3,"method":"resources/read","params":{
            "uri":"resource://vsoul/snapshot",
        }}))
        .unwrap();
    let text = read["result"]["contents"][0]["text"].as_str().unwrap();
    let snapshot: Value = serde_json::from_str(text).unwrap();
    assert_eq!(snapshot, session.snapshot());
}
