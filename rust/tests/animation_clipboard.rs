use serde_json::{json, Value};
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static TEMP_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "soul2d-animclip-test-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
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
            {"uuid":41,"target":1,"interpolation":"Linear","merge_mode":"Forced",
             "keyframes":[{"frame":4,"value":0.9,"tension":0.5}]},
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

fn walk_lanes(snapshot: &Value, parameter: u64) -> Vec<Value> {
    snapshot["model"]["animations"]["Walk"]["lanes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|lane| lane["uuid"] == parameter)
        .cloned()
        .collect()
}

#[test]
fn copy_paste_moves_two_axes_across_parameters_and_animations_in_one_undo() {
    let mut session = session();
    let before = session.snapshot();
    let copied = request(
        &mut session,
        json!({"op":"animation.key.copy","name":"Idle","parameter":17,"frame":4}),
    );
    assert_eq!(
        result(&copied)["keys"],
        json!([{"axis":0,"value":-0.5},{"axis":1,"value":0.75}])
    );
    assert_eq!(result(&copied)["count"], 2);
    assert_eq!(
        session.snapshot()["animationClipboard"],
        result(&copied)["keys"]
    );
    let revision = session.snapshot()["revision"].as_u64().unwrap();
    assert_eq!(revision, before["revision"]);

    let pasted = request(
        &mut session,
        json!({"op":"animation.key.paste","name":"Walk","parameter":41,"frame":6}),
    );
    assert_eq!(result(&pasted), &session.snapshot());
    let snapshot = session.snapshot();
    assert_eq!(snapshot["revision"], revision + 1);
    let lanes = walk_lanes(&snapshot, 41);
    assert_eq!(lanes.len(), 2);
    assert_eq!(
        lanes.iter().find(|lane| lane["target"] == 0).unwrap()["keyframes"],
        json!([{"frame":6,"value":-0.5,"tension":0.9,"custom_key":true}])
    );
    let axis1 = lanes.iter().find(|lane| lane["target"] == 1).unwrap();
    assert_eq!(axis1["interpolation"], "Linear");
    assert_eq!(axis1["merge_mode"], "Forced");
    assert_eq!(
        axis1["keyframes"],
        json!([{"frame":6,"value":0.75,"tension":0.5}])
    );

    result(&request(&mut session, json!({"op":"undo"})));
    assert_eq!(session.snapshot()["model"], before["model"]);
    result(&request(&mut session, json!({"op":"redo"})));
    assert_eq!(session.snapshot()["model"], snapshot["model"]);
    assert!(request(&mut session, json!({"op":"redo"}))["succeeded"] == false);
}

#[test]
fn scalar_paste_skips_axis_one() {
    let mut session = session();
    result(&request(
        &mut session,
        json!({"op":"animation.key.copy","name":"Idle","parameter":17,"frame":4}),
    ));
    result(&request(
        &mut session,
        json!({"op":"animation.key.paste","name":"Walk","parameter":23,"frame":6}),
    ));
    let lanes = walk_lanes(&session.snapshot(), 23);
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0]["target"], 0);
    assert_eq!(
        lanes[0]["keyframes"],
        json!([{"frame":6,"value":-0.5,"tension":0.5}])
    );
}

#[test]
fn axis_one_only_clipboard_to_scalar_fails_without_changes() {
    let mut session = session();
    result(&request(
        &mut session,
        json!({"op":"animation.key.copy","name":"Idle","parameter":41,"frame":4}),
    ));
    assert_eq!(
        session.snapshot()["animationClipboard"],
        json!([{"axis":1,"value":0.9}])
    );
    let before = session.snapshot();
    let rejected = request(
        &mut session,
        json!({"op":"animation.key.paste","name":"Walk","parameter":23,"frame":6}),
    );
    assert_eq!(rejected["succeeded"], false);
    assert_eq!(session.snapshot(), before);
}

#[test]
fn failed_copies_preserve_clipboard_and_session_state() {
    let mut session = session();
    result(&request(
        &mut session,
        json!({"op":"animation.key.copy","name":"Idle","parameter":17,"frame":4}),
    ));
    let before = session.snapshot();
    for invalid in [
        json!({"op":"animation.key.copy"}),
        json!({"op":"animation.key.copy","name":"Idle","parameter":17,"frame":5}),
        json!({"op":"animation.key.copy","name":"Idle","parameter":23,"frame":4}),
        json!({"op":"animation.key.copy","name":"Idle","parameter":40,"frame":4}),
        json!({"op":"animation.key.copy","name":"Idle","parameter":999,"frame":4}),
        json!({"op":"animation.key.copy","name":"Missing","parameter":17,"frame":4}),
        json!({"op":"animation.key.copy","name":"Idle","parameter":17,"frame":12}),
        json!({"op":"animation.key.copy","name":"Idle","parameter":17,"frame":-1}),
        json!({"op":"animation.key.copy","name":"Idle","parameter":17,"frame":"4"}),
        json!({"op":"animation.key.copy","name":"Idle","parameter":"17","frame":4}),
        json!({"op":"animation.key.copy","name":"  ","parameter":17,"frame":4}),
    ] {
        assert_eq!(
            request(&mut session, invalid)["succeeded"],
            false,
            "request should fail"
        );
        assert_eq!(session.snapshot(), before);
    }
}

#[test]
fn clipboard_keeps_source_lane_order() {
    let mut model = fixture();
    model["animations"]["Idle"]["lanes"] = json!([
        {"uuid":17,"target":1,"interpolation":"Linear","merge_mode":"Forced",
         "keyframes":[{"frame":4,"value":0.75,"tension":0.5}]},
        {"uuid":17,"target":0,"interpolation":"Linear","merge_mode":"Forced",
         "keyframes":[{"frame":4,"value":-0.5,"tension":0.5}]},
    ]);
    let mut session = Session::new();
    result(&request(&mut session, json!({"op":"new","model":model})));
    let copied = request(
        &mut session,
        json!({"op":"animation.key.copy","name":"Idle","parameter":17,"frame":4}),
    );
    assert_eq!(
        result(&copied)["keys"],
        json!([{"axis":1,"value":0.75},{"axis":0,"value":-0.5}])
    );
    assert_eq!(
        session.snapshot()["animationClipboard"],
        result(&copied)["keys"]
    );
}

#[test]
fn duplicate_lanes_copy_each_and_paste_last_value_wins() {
    let mut model = fixture();
    model["animations"]["Idle"]["lanes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"uuid":17,"target":0,"interpolation":"Linear","merge_mode":"Forced",
            "keyframes":[{"frame":4,"value":9.0,"tension":0.5}]}));
    model["animations"]["Walk"]["lanes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"uuid":41,"target":0,"interpolation":"Bezier","merge_mode":"Weighted",
            "keyframes":[{"frame":2,"value":5.0,"tension":0.1}]}));
    let mut session = Session::new();
    result(&request(&mut session, json!({"op":"new","model":model})));
    let copied = request(
        &mut session,
        json!({"op":"animation.key.copy","name":"Idle","parameter":17,"frame":4}),
    );
    assert_eq!(
        result(&copied)["keys"],
        json!([{"axis":0,"value":-0.5},{"axis":1,"value":0.75},{"axis":0,"value":9.0}])
    );
    assert_eq!(result(&copied)["count"], 3);
    let revision = session.snapshot()["revision"].as_u64().unwrap();
    let before = session.snapshot()["model"].clone();
    result(&request(
        &mut session,
        json!({"op":"animation.key.paste","name":"Walk","parameter":41,"frame":6}),
    ));
    let snapshot = session.snapshot();
    assert_eq!(snapshot["revision"], revision + 1);
    let lanes = walk_lanes(&snapshot, 41);
    assert_eq!(lanes.len(), 3);
    assert_eq!(
        lanes[0]["keyframes"],
        json!([{"frame":6,"value":9.0,"tension":0.9,"custom_key":true}])
    );
    assert_eq!(lanes[1]["target"], 0);
    assert_eq!(lanes[1]["interpolation"], "Bezier");
    assert_eq!(lanes[1]["merge_mode"], "Weighted");
    assert_eq!(
        lanes[1]["keyframes"],
        json!([{"frame":2,"value":5.0,"tension":0.1}])
    );
    let axis1 = lanes.iter().find(|lane| lane["target"] == 1).unwrap();
    assert_eq!(
        axis1["keyframes"],
        json!([{"frame":6,"value":0.75,"tension":0.5}])
    );
    result(&request(&mut session, json!({"op":"undo"})));
    assert_eq!(session.snapshot()["model"], before);
    result(&request(&mut session, json!({"op":"redo"})));
    assert_eq!(session.snapshot()["model"], snapshot["model"]);
}

#[test]
fn copy_leaves_document_history_and_runtime_unchanged() {
    let mut session = session();
    result(&request(
        &mut session,
        json!({"op":"animation.key.set","name":"Idle","parameter":17,"axis":0,"frame":8,"value":1.0}),
    ));
    let before = session.snapshot();
    result(&request(
        &mut session,
        json!({"op":"animation.key.copy","name":"Idle","parameter":17,"frame":4}),
    ));
    let mut expected = before.clone();
    expected["animationClipboard"] = json!([{"axis":0,"value":-0.5},{"axis":1,"value":0.75}]);
    assert_eq!(session.snapshot(), expected);

    result(&request(&mut session, json!({"op":"undo"})));
    assert_eq!(session.snapshot()["canRedo"], true);
    result(&request(
        &mut session,
        json!({"op":"animation.key.copy","name":"Idle","parameter":41,"frame":4}),
    ));
    assert_eq!(session.snapshot()["canRedo"], true);
    result(&request(&mut session, json!({"op":"redo"})));
    assert_eq!(session.snapshot()["model"], before["model"]);
}

#[test]
fn repeat_paste_is_a_noop() {
    let mut session = session();
    result(&request(
        &mut session,
        json!({"op":"animation.key.copy","name":"Idle","parameter":17,"frame":4}),
    ));
    let pre_paste = session.snapshot()["model"].clone();
    result(&request(
        &mut session,
        json!({"op":"animation.key.paste","name":"Walk","parameter":41,"frame":6}),
    ));
    let pasted = session.snapshot();
    result(&request(
        &mut session,
        json!({"op":"animation.key.paste","name":"Walk","parameter":41,"frame":6}),
    ));
    assert_eq!(session.snapshot(), pasted);
    result(&request(&mut session, json!({"op":"undo"})));
    assert_eq!(session.snapshot()["model"], pre_paste);
    assert_eq!(session.snapshot()["canUndo"], false);
    assert_eq!(session.snapshot()["canRedo"], true);
}

#[test]
fn paste_preserves_destination_metadata_and_empty_clipboard_fails() {
    let mut session = session();
    let before = session.snapshot();
    assert_eq!(
        request(
            &mut session,
            json!({"op":"animation.key.paste","name":"Walk","parameter":41,"frame":6})
        )["succeeded"],
        false
    );
    assert_eq!(session.snapshot(), before);

    result(&request(
        &mut session,
        json!({"op":"animation.key.copy","name":"Idle","parameter":17,"frame":4}),
    ));
    result(&request(
        &mut session,
        json!({"op":"animation.key.paste","name":"Walk","parameter":41,"frame":6}),
    ));
    let snapshot = session.snapshot();
    let lane = &walk_lanes(&snapshot, 41)[0];
    assert_eq!(lane["target"], 0);
    assert_eq!(lane["interpolation"], "Stepped");
    assert_eq!(lane["merge_mode"], "Additive");
    assert_eq!(lane["custom_lane"], "keep");
    assert_eq!(
        lane["keyframes"],
        json!([{"frame":6,"value":-0.5,"tension":0.9,"custom_key":true}])
    );
}

#[test]
fn failing_second_axis_leaves_both_axes_unchanged() {
    let mut model = fixture();
    model["animations"]["Walk"]["lanes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"uuid":41,"target":1,"interpolation":"Linear","merge_mode":"Forced",
            "keyframes":"corrupt"}));
    let mut session = Session::new();
    result(&request(&mut session, json!({"op":"new","model":model})));
    result(&request(
        &mut session,
        json!({"op":"animation.key.copy","name":"Idle","parameter":17,"frame":4}),
    ));
    let before = session.snapshot();
    assert_eq!(
        request(
            &mut session,
            json!({"op":"animation.key.paste","name":"Walk","parameter":41,"frame":2})
        )["succeeded"],
        false
    );
    assert_eq!(session.snapshot(), before);
}

#[test]
fn new_and_open_clear_the_key_clipboard() {
    let mut session = session();
    result(&request(
        &mut session,
        json!({"op":"animation.key.copy","name":"Idle","parameter":17,"frame":4}),
    ));
    assert_ne!(session.snapshot()["animationClipboard"], json!([]));

    result(&request(&mut session, json!({"op":"new","model":fixture()})));
    assert_eq!(session.snapshot()["animationClipboard"], json!([]));

    result(&request(
        &mut session,
        json!({"op":"animation.key.copy","name":"Idle","parameter":17,"frame":4}),
    ));
    let directory = temp_dir();
    let source = directory.join("clipboard.inx");
    result(&request(
        &mut session,
        json!({"op":"save","path":source.to_str().unwrap()}),
    ));
    result(&request(
        &mut session,
        json!({"op":"open","path":source.to_str().unwrap()}),
    ));
    assert_eq!(session.snapshot()["animationClipboard"], json!([]));
    assert_eq!(
        request(
            &mut session,
            json!({"op":"animation.key.paste","name":"Walk","parameter":41,"frame":6})
        )["succeeded"],
        false
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn saved_and_reopened_document_keeps_pasted_keys() {
    let directory = temp_dir();
    let saved = directory.join("pasted.inx");
    let mut session = session();
    result(&request(
        &mut session,
        json!({"op":"animation.key.copy","name":"Idle","parameter":17,"frame":4}),
    ));
    result(&request(
        &mut session,
        json!({"op":"animation.key.paste","name":"Walk","parameter":41,"frame":6}),
    ));
    let edited = session.snapshot()["model"].clone();
    result(&request(
        &mut session,
        json!({"op":"save","path":saved.to_str().unwrap()}),
    ));
    result(&request(
        &mut session,
        json!({"op":"open","path":saved.to_str().unwrap()}),
    ));
    assert_eq!(session.snapshot()["model"], edited);
    let lanes = walk_lanes(&session.snapshot(), 41);
    assert_eq!(lanes.len(), 2);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn mcp_tools_drive_copy_paste_and_report_shared_state() {
    let mut session = session();
    let listed = session
        .handle_mcp(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .unwrap();
    let tools = listed["result"]["tools"].as_array().unwrap();
    let frame_schema = json!({"type":"integer","minimum":0,"maximum":2147483647});
    for name in [
        "animation_key_set",
        "animation_key_delete",
        "animation_key_copy",
        "animation_key_paste",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("tool {name} is not listed"));
        assert_eq!(tool["inputSchema"]["properties"]["frame"], frame_schema);
    }
    for name in ["animation_key_copy", "animation_key_paste"] {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap();
        assert_eq!(
            tool["inputSchema"]["required"],
            json!(["name", "parameter", "frame"])
        );
        for property in ["name", "parameter", "frame"] {
            assert!(tool["inputSchema"]["properties"].get(property).is_some());
        }
    }

    let copied = session
        .handle_mcp(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"animation_key_copy",
            "arguments":{"name":"Idle","parameter":17,"frame":0},
        }}))
        .unwrap();
    assert_eq!(copied["result"]["isError"], false);
    assert_eq!(
        copied["result"]["structuredContent"],
        json!({"count":2,"keys":[{"axis":0,"value":-0.25},{"axis":1,"value":0.5}]})
    );

    let pasted = session
        .handle_mcp(&json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
            "name":"animation_key_paste",
            "arguments":{"name":"Walk","parameter":41,"frame":0},
        }}))
        .unwrap();
    assert_eq!(pasted["result"]["isError"], false);
    assert_eq!(pasted["result"]["structuredContent"], session.snapshot());

    let read = session
        .handle_mcp(&json!({"jsonrpc":"2.0","id":4,"method":"resources/read","params":{
            "uri":"resource://vsoul/snapshot",
        }}))
        .unwrap();
    let text = read["result"]["contents"][0]["text"].as_str().unwrap();
    let snapshot: Value = serde_json::from_str(text).unwrap();
    assert_eq!(snapshot, session.snapshot());
    assert_eq!(
        snapshot["animationClipboard"],
        json!([{"axis":0,"value":-0.25},{"axis":1,"value":0.5}])
    );
    let lanes = walk_lanes(&snapshot, 41);
    assert_eq!(lanes.len(), 2);
    assert_eq!(
        lanes[0]["keyframes"],
        json!([
            {"frame":0,"value":-0.25,"tension":0.5},
            {"frame":6,"value":0.0,"tension":0.9,"custom_key":true}
        ])
    );
}
