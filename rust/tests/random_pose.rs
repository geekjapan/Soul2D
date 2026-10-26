use serde_json::{json, Value};
use soul2d_core::Session;

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn param(id: u64, name: &str, is_vec2: bool, min: [f64; 2], max: [f64; 2]) -> Value {
    json!({
        "uuid": id, "name": name, "is_vec2": is_vec2, "min": min, "max": max, "defaults": [0.0, 0.0],
        "axis_points": if is_vec2 { json!([[0.0, 1.0], [0.0, 1.0]]) } else { json!([[0.0, 1.0], [0.0]]) },
        "merge_mode": "Additive", "bindings": [],
    })
}

/// 101 AngleX, 102 Move (2D), 103 EyeL, 104 Other, 105 Deep (2D, group in group), 106 Breath.
fn session() -> Session {
    let model = json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": {
            "uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
            "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
            "lockToRoot": false, "pinToMesh": false,
        },
        "param": [
            param(101, "ParamAngleX", false, [-30.0, 0.0], [30.0, 1.0]),
            param(102, "Move", true, [-2.0, -1.0], [3.0, 1.0]),
            param(103, "ParamEyeLOpen", false, [0.0, 0.0], [1.0, 1.0]),
            param(104, "Other", false, [0.0, 0.0], [1.0, 1.0]),
            {"groupUUID": 40, "name": "Outer", "children": [
                {"groupUUID": 41, "name": "Inner", "children": [param(105, "Deep", true, [-5.0, -5.0], [5.0, 5.0])]},
            ]},
            param(106, "ParamBreath", false, [0.0, 0.0], [1.0, 1.0]),
        ],
        "animations": {},
    });
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new", "model": model})));
    session
}

fn seek(session: &mut Session, arguments: Value) -> Value {
    let mut value = arguments;
    value["op"] = json!("random_pose.seek");
    result(&request(session, value)).clone()
}

fn history_count(session: &mut Session) -> u64 {
    result(&request(session, json!({"op": "history.summary"})))["count"].as_u64().unwrap()
}

/// Run an invalid request: it must fail and leave snapshot and history untouched.
fn rejected(session: &mut Session, value: Value) {
    let before = session.snapshot();
    let count = history_count(session);
    let response = request(session, value);
    assert_eq!(response["succeeded"], false, "expected failure: {response}");
    assert_eq!(session.snapshot(), before);
    assert_eq!(history_count(session), count);
}

fn pair(snapshot: &Value, id: u64) -> [f64; 2] {
    let value = &snapshot["values"][id.to_string()];
    [value[0].as_f64().unwrap(), value[1].as_f64().unwrap()]
}

#[test]
fn the_pose_depends_only_on_seed_and_time() {
    let mut session = session();
    let first = seek(&mut session, json!({"time": 2.2, "seed": 5}));
    assert_eq!(first["randomPose"], json!({"pattern": "B", "seed": 5, "time": 2.2, "parameters": null}));
    assert_ne!(first["values"], json!({}));
    assert_eq!(seek(&mut session, json!({"time": 2.2, "seed": 5}))["values"], first["values"]);
    assert_ne!(seek(&mut session, json!({"time": 2.2, "seed": 6}))["values"], first["values"]);
    assert_ne!(seek(&mut session, json!({"time": 2.3, "seed": 5}))["values"], first["values"]);
    let omitted = seek(&mut session, json!({"time": 2.2}));
    assert_eq!(seek(&mut session, json!({"time": 2.2, "seed": 0}))["values"], omitted["values"]);
}

#[test]
fn every_value_stays_in_range_at_any_group_depth() {
    let mut session = session();
    let ranges = [(101, [-30.0, 0.0], [30.0, 0.0]), (102, [-2.0, -1.0], [3.0, 1.0]), (103, [0.0, 0.0], [1.0, 0.0]),
        (104, [0.0, 0.0], [1.0, 0.0]), (105, [-5.0, -5.0], [5.0, 5.0]), (106, [0.0, 0.0], [1.0, 0.0])];
    let mut moved_deep = false;
    for seed in [0, 1, 99, u64::MAX] {
        for step in 0..200 {
            let snapshot = seek(&mut session, json!({"time": f64::from(step) * 0.37, "seed": seed}));
            for (id, low, high) in ranges {
                let value = pair(&snapshot, id);
                for axis in 0..2 {
                    assert!(value[axis] >= low[axis] && value[axis] <= high[axis], "{id} axis {axis}: {value:?}");
                }
            }
            moved_deep |= pair(&snapshot, 105) != [0.0, 0.0];
        }
    }
    assert!(moved_deep, "the parameter inside two groups never moved");
}

#[test]
fn a_one_dimensional_parameter_keeps_its_second_value() {
    let mut session = session();
    result(&request(&mut session, json!({"op": "value.set", "id": 101, "value": [4.0, 0.5]})));
    for time in [0.0, 1.0, 3.3] {
        let snapshot = seek(&mut session, json!({"time": time, "seed": 3}));
        assert_eq!(pair(&snapshot, 101)[1], 0.5);
        assert_ne!(pair(&snapshot, 101)[0], 4.0);
    }
}

#[test]
fn parameters_limits_the_pose_to_those_parameters() {
    let mut session = session();
    let snapshot = seek(&mut session, json!({"time": 1.2, "seed": 2, "parameters": [104]}));
    assert_eq!(snapshot["randomPose"]["parameters"], json!([104]));
    assert_ne!(pair(&snapshot, 104), [0.0, 0.0]);
    for id in [101, 102, 103, 105, 106] {
        assert_eq!(pair(&snapshot, id), [0.0, 0.0], "parameter {id}");
    }
}

#[test]
fn a_bad_request_is_refused_and_changes_nothing() {
    let mut session = session();
    let bad = [
        json!({"time": 1.0, "parameters": [40]}),
        json!({"time": 1.0, "parameters": [999]}),
        json!({"time": 1.0, "parameters": []}),
        json!({"time": 1.0, "parameters": [1.5]}),
        json!({"time": 1.0, "parameters": ["101"]}),
        json!({"time": 1.0, "parameters": 101}),
        json!({"time": -0.5}),
        json!({"time": 1_000_000_001.0}),
        json!({"time": "1"}),
        json!({}),
        json!({"time": 1.0, "pattern": "C"}),
        json!({"time": 1.0, "pattern": 1}),
        json!({"time": 1.0, "seed": -1}),
        json!({"time": 1.0, "seed": 1.5}),
        json!({"time": 1.0, "seed": "1"}),
    ];
    for arguments in bad {
        let mut value = arguments.clone();
        value["op"] = json!("random_pose.seek");
        rejected(&mut session, value.clone());
        seek(&mut session, json!({"time": 3.0, "seed": 4}));
        rejected(&mut session, value);
        result(&request(&mut session, json!({"op": "animation.reset"})));
    }
}

#[test]
fn a_pose_ends_at_the_latest_time_and_an_advance_past_it_leaves_it_in_place() {
    let mut session = session();
    let last = seek(&mut session, json!({"time": 1_000_000_000.0, "seed": 9}));
    assert_eq!(last["randomPose"]["time"], 1_000_000_000.0);
    assert_ne!(last["values"], seek(&mut session, json!({"time": 999_999_999.0, "seed": 9}))["values"]);
    let near = seek(&mut session, json!({"time": 999_999_999.5, "seed": 9}));
    let response = request(&mut session, json!({"op": "animation.advance", "dt": 1.0}));
    assert_eq!(response["succeeded"], false, "{response}");
    assert_eq!(response["message"], "a random pose ends at 1000000000 seconds");
    assert_eq!(session.snapshot(), near);
}

#[test]
fn advance_continues_a_seek_and_reset_restores_the_manual_values() {
    let mut session = session();
    result(&request(&mut session, json!({"op": "value.set", "id": 102, "value": [1.0, 0.5]})));
    let before = session.snapshot();
    let count = history_count(&mut session);

    let direct = seek(&mut session, json!({"time": 2.5, "seed": 9}));
    seek(&mut session, json!({"time": 0.0, "seed": 9}));
    let advanced = result(&request(&mut session, json!({"op": "animation.advance", "dt": 2.5}))).clone();
    assert_eq!(advanced["values"], direct["values"]);
    assert_eq!(advanced["randomPose"], direct["randomPose"]);
    let looped = result(&request(&mut session, json!({"op": "animation.advance", "dt": 0.0, "loop": false}))).clone();
    assert_eq!(looped["values"], direct["values"]);
    for snapshot in [&direct, &advanced] {
        assert_ne!(snapshot["values"], before["values"]);
        assert_eq!(snapshot["revision"], before["revision"]);
        assert_eq!(snapshot["model"], before["model"]);
        assert_eq!(snapshot["canUndo"], before["canUndo"]);
    }
    assert_eq!(history_count(&mut session), count);

    let reset = result(&request(&mut session, json!({"op": "animation.reset"}))).clone();
    assert_eq!(reset["randomPose"], Value::Null);
    assert_eq!(reset["values"], before["values"]);
    assert_eq!(session.snapshot(), before);
    assert_eq!(history_count(&mut session), count);
}

#[test]
fn pattern_a_moves_only_the_named_parameters_on_a_sine() {
    let mut session = session();
    let base = session.snapshot();
    let at = |session: &mut Session, time: f64| seek(session, json!({"time": time, "seed": 7, "pattern": "A"}));
    // Computed from the formula with an independent implementation of the generator: the phase of
    // ParamAngleX (id 101, seed 7) is 0.9018505181159894, so 30 * sin(2 * pi * (time / 4 + phase)).
    let start = at(&mut session, 0.0);
    assert!((pair(&start, 101)[0] - -17.350_175_150_9).abs() < 1e-6, "{:?}", pair(&start, 101));
    let later = at(&mut session, 1.0);
    assert!((pair(&later, 101)[0] - 24.473_892_645_8).abs() < 1e-6, "{:?}", pair(&later, 101));
    assert!((pair(&at(&mut session, 4.0), 101)[0] - pair(&start, 101)[0]).abs() < 1e-6);
    for id in [102, 104, 105, 106] {
        assert_eq!(pair(&later, id), pair(&base, id), "parameter {id}");
    }
    // The wave of ParamEyeLOpen runs 0..2 but the parameter stops at 1: 1 + sin(2 pi (2/3 + 0.8752707988673294)) > 1.
    for step in 0..100 {
        let eye = pair(&at(&mut session, f64::from(step) * 0.11), 103)[0];
        assert!((0.0..=1.0).contains(&eye), "eye {eye}");
    }
    let eye = pair(&at(&mut session, 3.0 * 0.4), 103)[0];
    let expected = (1.0 + (std::f64::consts::TAU * (0.4 + 0.875_270_798_867_329_4)).sin()).min(1.0);
    assert!((eye - expected).abs() < 1e-9, "{eye} vs {expected}");
}

#[test]
fn posing_swings_the_face_and_moves_the_rest_like_pattern_b() {
    let mut session = session();
    let b = seek(&mut session, json!({"time": 1.75, "seed": 7, "pattern": "B"}));
    let posing = seek(&mut session, json!({"time": 1.75, "seed": 7, "pattern": "Posing"}));
    for id in [101, 102, 104, 105] {
        assert_eq!(pair(&posing, id), pair(&b, id), "parameter {id}");
    }
    // ParamBreath (id 106, seed 7) has phase 0.8337676570912711: 0.5 + 0.5 * sin(2 * pi * (1.75 / 3.5 + phase)).
    assert!((pair(&posing, 106)[0] - 0.932_328_856_2).abs() < 1e-6, "{:?}", pair(&posing, 106));
    assert_ne!(pair(&posing, 106), pair(&b, 106));
}

#[test]
fn a_motion3_operation_leaves_a_random_pose_in_place() {
    let mut session = session();
    let before = seek(&mut session, json!({"time": 1.0, "seed": 3}));
    rejected(&mut session, json!({"op": "motion3.reset"}));
    rejected(&mut session, json!({"op": "motion3.advance", "dt": 0.1}));
    assert_eq!(session.snapshot()["randomPose"], before["randomPose"]);
}

#[test]
fn an_animation_seek_replaces_a_random_pose_and_the_reverse() {
    let mut session = session();
    result(&request(&mut session, json!({"op": "animation.create", "name": "Idle"})));
    seek(&mut session, json!({"time": 1.0}));
    let replaced = result(&request(&mut session, json!({"op": "animation.seek", "name": "Idle", "frame": 0}))).clone();
    assert_eq!(replaced["randomPose"], Value::Null);
    assert_eq!(replaced["animation"]["name"], "Idle");
    let back = seek(&mut session, json!({"time": 1.0}));
    assert_eq!(back["animation"], Value::Null);
    assert_eq!(back["randomPose"]["time"], 1.0);
}

#[test]
fn the_pose_is_reachable_over_mcp() {
    let mut session = session();
    let before = session.snapshot()["values"].clone();
    let mut rpc = |method: &str, params: Value| {
        session.handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})).unwrap()["result"]
            .clone()
    };
    let state = |rpc: &mut dyn FnMut(&str, Value) -> Value| -> Value {
        let read = rpc("resources/read", json!({"uri": "resource://soul2d/document/state"}));
        serde_json::from_str(read["contents"][0]["text"].as_str().unwrap()).unwrap()
    };
    let missing = rpc("tools/call", json!({"name": "random_pose_seek", "arguments": {}}));
    assert_eq!(missing["isError"], true);
    let called = rpc("tools/call", json!({"name": "random_pose_seek", "arguments": {"time": 1.0, "seed": 7}}));
    assert_eq!(called["isError"], false, "{called}");
    let posed = state(&mut rpc)["values"].clone();
    assert_ne!(posed, before);
    let reset = rpc("tools/call", json!({"name": "animation_reset", "arguments": {}}));
    assert_eq!(reset["isError"], false, "{reset}");
    assert_eq!(state(&mut rpc)["values"], before);
}
