mod common;

use serde_json::{json, Value};
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const TEMPLATE_FILE: &str = "animation-templates.json";

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "soul2d-animation-templates-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    common::scratch(path)
}

fn call(session: &mut Session, op: &str, mut body: Value) -> Value {
    body["op"] = json!(op);
    session.handle_request(&body)
}

fn ok(session: &mut Session, op: &str, body: Value) -> Value {
    let reply = call(session, op, body);
    assert_eq!(reply["succeeded"], true, "{op}: {reply}");
    reply["result"].clone()
}

fn message(session: &mut Session, op: &str, body: Value) -> String {
    let reply = call(session, op, body);
    assert_eq!(reply["succeeded"], false, "{op}: {reply}");
    reply["message"].as_str().unwrap().to_owned()
}

fn parameter(id: u32, name: &str, vec2: bool) -> Value {
    json!({
        "uuid": id, "name": name, "is_vec2": vec2, "min": [0.0, 0.0], "max": [1.0, 1.0],
        "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], if vec2 { json!([0.0, 1.0]) } else { json!([0.0]) }],
        "merge_mode": "Additive", "bindings": [],
    })
}

fn key(frame: i32, value: f64) -> Value {
    json!({"frame": frame, "value": value, "tension": 0.25})
}

fn lane(id: u32, axis: u32, keys: Value) -> Value {
    json!({"uuid": id, "target": axis, "keyframes": keys, "interpolation": "Linear", "merge_mode": "Forced"})
}

fn clip(length: u32, lanes: Value) -> Value {
    json!({"length": length, "timestep": 0.1, "additive": false, "animationWeight": 1.0, "leadIn": -1, "leadOut": -1,
        "lanes": lanes})
}

/// Wave and Look (two axes, in a group) are keyed in Walk; Idle is empty. Blink has no key.
fn model() -> Value {
    json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
            "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
            "lockToRoot": false, "pinToMesh": false, "children": []},
        "param": [
            parameter(21, "Wave", false),
            {"groupUUID": 40, "name": "Face", "children": [parameter(22, "Look", true)]},
            parameter(23, "Blink", false),
        ],
        "animations": {
            "Walk": clip(20, json!([
                lane(21, 0, json!([key(2, 0.1), key(5, 0.5), key(9, 0.9)])),
                lane(22, 1, json!([key(4, 0.4), key(12, 1.0)])),
            ])),
            "Idle": clip(5, json!([])),
            "Long": clip(100_001, json!([])),
        },
    })
}

fn session() -> (Session, PathBuf) {
    let dir = temp_dir().join("settings");
    let mut session = Session::new();
    session.set_config_dir(dir.clone());
    ok(&mut session, "new", json!({"model": model()}));
    (session, dir)
}

fn templates(session: &mut Session) -> Value {
    ok(session, "animation.template.list", json!({}))["presets"].clone()
}

fn save(name: &str, template: &str, from: i32, to: i32) -> Value {
    json!({"name": name, "template": template, "from": from, "to": to})
}

fn lanes_of(session: &Session, clip: &str) -> Value {
    session.snapshot()["model"]["animations"][clip]["lanes"].clone()
}

#[test]
fn a_range_is_saved_listed_applied_and_deleted() {
    let (mut session, dir) = session();
    let revision = session.snapshot()["revision"].clone();
    assert_eq!(templates(&mut session), json!({}));
    let saved = ok(&mut session, "animation.template.save", save("Walk", "Step", 4, 9));
    assert_eq!((&saved["template"], &saved["replaced"]), (&json!("Step"), &json!(false)));
    let step = json!({"length": 6, "lanes": [
        {"parameter": "Wave", "axis": 0, "keyframes": [{"frame": 1, "value": 0.5, "tension": 0.25},
                                                       {"frame": 5, "value": 0.9, "tension": 0.25}]},
        {"parameter": "Look", "axis": 1, "keyframes": [{"frame": 0, "value": 0.4, "tension": 0.25}]},
    ]});
    assert_eq!(saved["entry"], step);
    assert_eq!(templates(&mut session), json!({"Step": step}));
    let read = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "resources/read", "params": {
            "uri": "resource://soul2d/animation/templates",
        }}))
        .unwrap();
    let text = read["result"]["contents"][0]["text"].as_str().unwrap_or_else(|| panic!("{read}"));
    assert_eq!(serde_json::from_str::<Value>(text).unwrap(), json!({"presets": {"Step": step}}));
    assert!(dir.join(TEMPLATE_FILE).is_file());
    assert_eq!(session.snapshot()["revision"], revision, "saving is no document change");

    let called = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "animation_template_apply", "arguments": {"name": "Idle", "template": "Step", "frame": 0},
        }}))
        .unwrap();
    let text = called["result"]["content"][0]["text"].as_str().unwrap_or_else(|| panic!("{called}"));
    let report: Value = serde_json::from_str(text).unwrap();
    assert_eq!(report, json!({"applied": 2, "lanes": [{"parameter": 21, "axis": 0}, {"parameter": 22, "axis": 1}],
        "unmatched": [], "dropped": 1}));
    let applied = lanes_of(&session, "Idle");
    let keys = |id: u64, axis: u64| -> Vec<(i64, f64)> {
        let lane = applied.as_array().unwrap().iter().find(|lane| lane["uuid"] == id && lane["target"] == axis);
        let lane = lane.unwrap_or_else(|| panic!("no lane {id} {axis} in {applied}"));
        lane["keyframes"].as_array().unwrap().iter().map(|key| (key["frame"].as_i64().unwrap(),
            key["value"].as_f64().unwrap())).collect()
    };
    assert_eq!(keys(21, 0), vec![(1, 0.5)], "the key at 5 is past the end of Idle");
    assert_eq!(keys(22, 1), vec![(0, 0.4)]);
    assert_eq!(applied[0]["keyframes"][0]["tension"], 0.25, "the tension is kept");

    ok(&mut session, "undo", json!({}));
    assert_eq!(lanes_of(&session, "Idle"), json!([]), "applying is one undo");
    let report = ok(&mut session, "animation.template.apply", json!({"name": "Idle", "template": "Step", "frame": 4}));
    assert_eq!(report, json!({"applied": 1, "lanes": [{"parameter": 22, "axis": 1}], "unmatched": [], "dropped": 2}),
        "a lane whose keys all fall past the end is not listed");
    ok(&mut session, "undo", json!({}));

    let revision = session.snapshot()["revision"].clone();
    assert_eq!(ok(&mut session, "animation.template.delete", json!({"template": "Step"})), json!({"template": "Step"}));
    assert_eq!(templates(&mut session), json!({}));
    assert_eq!(session.snapshot()["revision"], revision, "deleting is no document change");
    assert_eq!(message(&mut session, "animation.template.delete", json!({"template": "Step"})),
        "no saved template is named \"Step\"");
}

#[test]
fn a_host_without_a_settings_directory_keeps_no_template() {
    let mut session = Session::new();
    ok(&mut session, "new", json!({"model": model()}));
    assert_eq!(templates(&mut session), json!({}));
    for (op, body) in [
        ("animation.template.save", save("Walk", "Step", 0, 19)),
        ("animation.template.apply", json!({"name": "Walk", "template": "Step", "frame": 0})),
        ("animation.template.delete", json!({"template": "Step"})),
    ] {
        assert_eq!(message(&mut session, op, body), "this host has no settings directory to keep templates in");
    }
}

#[test]
fn applying_later_keeps_relative_frames_and_reports_what_does_not_fit() {
    let (mut session, _) = session();
    ok(&mut session, "animation.template.save", json!({"name": "Walk", "template": "Look only", "from": 0, "to": 19,
        "parameters": [22]}));
    assert_eq!(templates(&mut session)["Look only"]["lanes"].as_array().unwrap().len(), 1);
    let report = ok(&mut session, "animation.template.apply",
        json!({"name": "Walk", "template": "Look only", "frame": 10}));
    assert_eq!(report, json!({"applied": 1, "lanes": [{"parameter": 22, "axis": 1}], "unmatched": [], "dropped": 1}));
    let look = lanes_of(&session, "Walk")[1]["keyframes"].clone();
    let frames: Vec<i64> = look.as_array().unwrap().iter().map(|key| key["frame"].as_i64().unwrap()).collect();
    assert_eq!(frames, vec![4, 12, 14]);

    // A model without Look, or with Look as a one-axis parameter, leaves the lane unmatched.
    let mut other = model();
    other["param"][1]["children"][0] = parameter(22, "Look", false);
    ok(&mut session, "new", json!({"model": other}));
    let refused = message(&mut session, "animation.template.apply",
        json!({"name": "Idle", "template": "Look only", "frame": 0}));
    assert_eq!(refused, "no key of template \"Look only\" fits animation \"Idle\" from frame 0");
}

#[test]
fn an_unmatched_lane_is_reported_and_the_others_apply() {
    let (mut session, _) = session();
    ok(&mut session, "animation.template.save", save("Walk", "Step", 0, 19));
    let mut other = model();
    other["param"][0]["name"] = json!("Arm");
    ok(&mut session, "new", json!({"model": other}));
    let report = ok(&mut session, "animation.template.apply", json!({"name": "Walk", "template": "Step", "frame": 0}));
    assert_eq!(report["unmatched"], json!([{"parameter": "Wave", "axis": 0}]));
    assert_eq!(report["applied"], 2);
}

#[test]
fn refusals_leave_the_document_and_the_file_unchanged() {
    let (mut session, dir) = session();
    ok(&mut session, "animation.template.save", save("Walk", "Step", 0, 19));
    let file = fs::read(dir.join(TEMPLATE_FILE)).unwrap();
    let opened = session.snapshot();
    let refusals = [
        ("animation.template.save", save("Walk", "Step", 0, 19),
            "a template named \"Step\" is saved; pass overwrite to replace it"),
        ("animation.template.save", save("Walk", "Late", 9, 4), "from must not be after to"),
        ("animation.template.save", save("Walk", "Late", 0, 20), "to must be an integer frame from 0 to 19"),
        ("animation.template.save", save("Walk", "Gap", 13, 19), "animation \"Walk\" has no key from frame 13 to 19"),
        ("animation.template.save", save("Run", "Late", 0, 1), "animation \"Run\" was not found"),
        ("animation.template.save", save("Long", "Late", 0, 100_000),
            "a template holds at most 100000 frames; from 0 to 100000 is more"),
        ("animation.template.save", save("Walk", "", 0, 19), "template must"),
        ("animation.template.apply", json!({"name": "Walk", "template": "None", "frame": 0}),
            "no saved template is named \"None\""),
        ("animation.template.apply", json!({"name": "Walk", "template": "Step", "frame": 20}),
            "frame must be an integer frame from 0 to 19"),
        ("animation.template.apply", json!({"name": "Walk", "template": "Step", "frame": 1.5}),
            "frame must be an integer frame from 0 to 19"),
        ("animation.template.apply", json!({"name": "Walk", "frame": 0}), "template must be a string"),
        ("animation.template.delete", json!({}), "template must be a string"),
    ];
    for (op, body, expected) in refusals {
        let refused = message(&mut session, op, body.clone());
        assert!(refused.contains(expected), "{op} {body}: {refused}");
        assert!(!refused.contains("preset"), "{op} {body}: {refused}");
        assert_eq!(session.snapshot(), opened, "{op} {body}");
        assert_eq!(fs::read(dir.join(TEMPLATE_FILE)).unwrap(), file, "{op} {body}");
    }
    ok(&mut session, "animation.template.save", json!({"name": "Walk", "template": "Step", "from": 2, "to": 2,
        "overwrite": true}));
    assert_eq!(templates(&mut session)["Step"]["length"], 1);
    ok(&mut session, "animation.template.save", save("Walk", "my preset", 2, 2));
    let refused = message(&mut session, "animation.template.save", save("Walk", "my preset", 2, 2));
    assert_eq!(refused, "a template named \"my preset\" is saved; pass overwrite to replace it", "the name is the user's");
}

#[test]
fn a_name_two_parameters_share_is_refused() {
    let (mut session, _) = session();
    ok(&mut session, "animation.template.save", save("Walk", "Step", 0, 19));
    let mut twice = model();
    twice["param"][2]["name"] = json!("Wave");
    ok(&mut session, "new", json!({"model": twice}));
    let opened = session.snapshot();
    let refused = message(&mut session, "animation.template.apply", json!({"name": "Idle", "template": "Step",
        "frame": 0}));
    assert_eq!(refused, "2 parameters are named \"Wave\"; rename all but one first");
    assert_eq!(session.snapshot(), opened);
    let refused = message(&mut session, "animation.template.save", save("Walk", "Twice", 0, 19));
    assert_eq!(refused, "2 parameters are named \"Wave\"; rename all but one first");
    assert_eq!(session.snapshot(), opened);
}

#[test]
fn a_frozen_lane_refuses_the_apply() {
    let (mut session, _) = session();
    ok(&mut session, "animation.template.save", save("Walk", "Step", 0, 19));
    let mut frozen = model();
    frozen["animations"]["Walk"]["freezeFrame"] = json!(0);
    frozen["animations"]["Walk"]["lanes"][0]["frozen"] = json!(true);
    ok(&mut session, "new", json!({"model": frozen}));
    let opened = session.snapshot();
    let refused = message(&mut session, "animation.template.apply", json!({"name": "Walk", "template": "Step",
        "frame": 1}));
    assert!(refused.contains("frozen"), "{refused}");
    assert_eq!(session.snapshot(), opened);
}

#[test]
fn malformed_entries_in_the_file_are_refused() {
    let (mut session, dir) = session();
    ok(&mut session, "animation.template.save", save("Walk", "Step", 0, 19));
    let good = fs::read_to_string(dir.join(TEMPLATE_FILE)).unwrap();
    let good: Value = serde_json::from_str(&good).unwrap();
    let entry = good["presets"]["Step"].clone();
    let bad = |change: &dyn Fn(&mut Value)| {
        let mut entry = entry.clone();
        change(&mut entry);
        entry
    };
    let cases = [
        (bad(&|entry| entry["length"] = json!(0)), "length must be an integer from 1 to 100000"),
        (bad(&|entry| entry["lanes"] = json!([])), "lanes must be a non-empty array"),
        (bad(&|entry| entry["lanes"][0]["parameter"] = json!(" ")), "lanes[0].parameter must be a parameter name"),
        (bad(&|entry| entry["lanes"][0]["axis"] = json!(2)), "lanes[0].axis must be 0 or 1"),
        (bad(&|entry| entry["lanes"][0]["keyframes"] = json!([])), "lanes[0].keyframes must be a non-empty array"),
        (bad(&|entry| entry["lanes"][0]["keyframes"][0]["frame"] = json!(20)), "lanes[0].keyframes"),
        (bad(&|entry| entry["lanes"][1] = entry["lanes"][0].clone()), "lanes[1] repeats parameter \"Wave\" axis 0"),
    ];
    let opened = session.snapshot();
    for (entry, expected) in cases {
        let file = json!({"presets": {"Step": entry}});
        fs::write(dir.join(TEMPLATE_FILE), file.to_string()).unwrap();
        let listed = call(&mut session, "animation.template.list", json!({}));
        let applied = call(&mut session, "animation.template.apply", json!({"name": "Idle", "template": "Step",
            "frame": 0}));
        for reply in [listed, applied] {
            assert_eq!(reply["succeeded"], false, "{entry}: {reply}");
            assert!(reply["message"].as_str().unwrap().contains(expected), "{entry}: {reply}");
            assert!(!reply["message"].as_str().unwrap().contains("preset"), "{entry}: {reply}");
        }
        assert_eq!(session.snapshot(), opened, "{entry}");
    }
    let many: serde_json::Map<String, Value> = (0..65).map(|index| (format!("T{index}"), entry.clone())).collect();
    fs::write(dir.join(TEMPLATE_FILE), json!({"presets": many}).to_string()).unwrap();
    let refused = message(&mut session, "animation.template.list", json!({}));
    assert!(refused.ends_with("it holds more than 64 templates"), "{refused}");
    fs::write(dir.join(TEMPLATE_FILE), good.to_string()).unwrap();
    assert_eq!(templates(&mut session)["Step"], entry);
}
