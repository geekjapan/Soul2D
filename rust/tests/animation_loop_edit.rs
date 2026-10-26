use serde_json::{json, Value};
use soul2d_core::Session;

const PARAM: u32 = 17;
const VEC: u32 = 18;
const LENGTH: i64 = 8;
const TIMESTEP: f64 = 0.125;

fn keys(sign: f64) -> Value {
    json!([
        {"frame": 1, "value": 1.0 * sign, "tension": 0.25},
        {"frame": 3, "value": 3.0 * sign, "tension": 0.5},
        {"frame": 5, "value": 0.0,       "tension": 0.5},
        {"frame": 7, "value": 2.0 * sign, "tension": 0.5},
    ])
}

fn lane(uuid: u32, target: i64, interpolation: &str, keys: Value) -> Value {
    json!({"uuid": uuid, "target": target, "interpolation": interpolation,
           "merge_mode": "Forced", "keyframes": keys})
}

fn model() -> Value {
    json!({
        "meta": {"name": "loop", "version": "1.0-alpha"},
        "nodes": {
            "uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
            "transform": {"trans": [0.0,0.0,0.0], "rot": [0.0,0.0,0.0], "scale": [1.0,1.0]},
            "lockToRoot": false, "pinToMesh": false,
        },
        "param": [
            {"uuid": PARAM, "name": "P", "is_vec2": false,
             "min": [-10.0,0.0], "max": [10.0,0.0], "defaults": [0.0,0.0],
             "axis_points": [[0.0,1.0],[0.0]], "merge_mode": "Additive", "bindings": []},
            {"uuid": VEC, "name": "V", "is_vec2": true,
             "min": [-10.0,-10.0], "max": [10.0,10.0], "defaults": [0.0,0.0],
             "axis_points": [[0.0,1.0],[0.0,1.0]], "merge_mode": "Additive", "bindings": []},
        ],
        "animations": {
            "L": {"length": LENGTH, "timestep": TIMESTEP, "additive": false,
                  "animationWeight": 1.0, "leadIn": -1, "leadOut": -1,
                  "lanes": [lane(PARAM, 0, "Linear", keys(1.0))]},
            "V": {"length": LENGTH, "timestep": TIMESTEP, "additive": false,
                  "animationWeight": 1.0, "leadIn": -1, "leadOut": -1,
                  "lanes": [lane(VEC, 0, "Linear", keys(1.0)),
                            lane(VEC, 1, "Linear", keys(-1.0))]},
            "S": {"length": LENGTH, "timestep": TIMESTEP, "additive": false,
                  "animationWeight": 1.0, "leadIn": -1, "leadOut": -1,
                  "lanes": [lane(PARAM, 0, "Linear",
                                 json!([{"frame": 1, "value": 2.25, "tension": 0.5}]))]},
            "O": {"length": 1, "timestep": TIMESTEP, "additive": false,
                  "animationWeight": 1.0, "leadIn": -1, "leadOut": -1,
                  "lanes": [lane(PARAM, 0, "Linear",
                                 json!([{"frame": 0, "value": 3.0, "tension": 0.5}]))]},
            "Run": {"length": 11, "timestep": 0.0625, "additive": false,
                    "animationWeight": 1.0, "leadIn": -1, "leadOut": -1,
                    "lanes": [lane(PARAM, 0, "Linear",
                                   json!([{"frame": 0, "value": 0.0, "tension": 0.5},
                                          {"frame": 10, "value": 1.0, "tension": 0.5}]))]},
        },
    })
}

fn session() -> Session {
    let mut session = Session::new();
    let response = session.handle_request(&json!({"op": "new", "model": model()}));
    assert_eq!(response["succeeded"], true, "{response}");
    session
}

fn ok(session: &mut Session, request: Value) -> Value {
    let response = session.handle_request(&request);
    assert_eq!(response["succeeded"], true, "{request}: {response}");
    response["result"].clone()
}

fn fail(session: &mut Session, request: Value) -> String {
    let response = session.handle_request(&request);
    assert_eq!(response["succeeded"], false, "{request}: {response}");
    response["message"].as_str().unwrap().to_owned()
}

fn sample(session: &mut Session, name: &str, frame: f64) -> f64 {
    let result = ok(session, json!({"op": "animation.sample", "name": name, "frame": frame}));
    result["values"][PARAM.to_string()][0].as_f64().unwrap()
}

fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() <= 1e-5 * expected.abs().max(1.0),
            "{actual} != {expected}");
}

fn set_lane_mode(session: &mut Session, name: &str, interpolation: &str) {
    ok(session, json!({"op": "animation.lane.set", "name": name, "parameter": PARAM,
                       "axis": 0, "interpolation": interpolation}));
}

fn enable_loop(session: &mut Session, name: &str) {
    ok(session, json!({"op": "animation.set", "name": name, "loopEditing": true}));
}

#[test]
fn loop_edit_wraps_the_interval_for_every_interpolation() {
    let mut session = session();
    enable_loop(&mut session, "L");
    let oracle = [("Nearest", 1.0), ("Stepped", 2.0), ("Linear", 1.375),
                  ("Cubic", 1.287109375), ("Bezier", 1.345703125)];
    for (mode, expected) in oracle {
        set_lane_mode(&mut session, "L", mode);
        close(sample(&mut session, "L", 0.25), expected);
    }
}

#[test]
fn loop_edit_off_keeps_the_legacy_edge_values() {
    let mut session = session();
    for mode in ["Nearest", "Stepped", "Linear", "Cubic", "Bezier"] {
        set_lane_mode(&mut session, "L", mode);
        close(sample(&mut session, "L", 0.25), 1.0);
    }
}

#[test]
fn loop_edit_exact_key_times_return_authored_values() {
    let mut session = session();
    enable_loop(&mut session, "L");
    let authored = [(1.0, 1.0), (3.0, 3.0), (5.0, 0.0), (7.0, 2.0)];
    for mode in ["Nearest", "Stepped", "Linear", "Cubic", "Bezier"] {
        set_lane_mode(&mut session, "L", mode);
        for (frame, expected) in authored {
            close(sample(&mut session, "L", frame), expected);
        }
    }
}

#[test]
fn loop_edit_frame_length_matches_frame_zero() {
    let mut session = session();
    enable_loop(&mut session, "L");
    let oracle = [("Nearest", 2.0), ("Stepped", 2.0), ("Linear", 1.5),
                  ("Cubic", 1.5), ("Bezier", 1.5)];
    for (mode, expected) in oracle {
        set_lane_mode(&mut session, "L", mode);
        close(sample(&mut session, "L", 8.0), expected);
    }
}

#[test]
fn loop_edit_single_key_stays_constant() {
    let mut single = session();
    enable_loop(&mut single, "S");
    for frame in [0.0, 0.25, 8.0] {
        close(sample(&mut single, "S", frame), 2.25);
    }
    let mut one_frame = session();
    enable_loop(&mut one_frame, "O");
    for frame in [0.0, 0.5, 1.0] {
        close(sample(&mut one_frame, "O", frame), 3.0);
    }
}

#[test]
fn loop_edit_mirrored_vec2_lanes_wrap_both_axes() {
    let mut session = session();
    enable_loop(&mut session, "V");
    let result = ok(&mut session, json!({"op": "animation.sample", "name": "V", "frame": 0.25}));
    let values = result["values"][VEC.to_string()].clone();
    close(values[0].as_f64().unwrap(), 1.375);
    close(values[1].as_f64().unwrap(), -1.375);
}

#[test]
fn loop_edit_editing_a_canonical_key_moves_every_cycle() {
    let mut session = session();
    enable_loop(&mut session, "L");
    ok(&mut session, json!({"op": "animation.key.set", "name": "L", "parameter": PARAM,
                            "axis": 0, "frame": 1, "value": 4.0}));
    close(sample(&mut session, "L", 0.25), 3.25);
    ok(&mut session, json!({"op": "undo"}));
    close(sample(&mut session, "L", 0.25), 1.375);
}

#[test]
fn loop_edit_flag_round_trips_and_preserves_the_keys() {
    let mut session = session();
    let before = session.snapshot()["model"]["animations"]["L"]["lanes"].clone();
    enable_loop(&mut session, "L");
    assert_eq!(session.snapshot()["model"]["animations"]["L"]["loopEditing"], true);
    ok(&mut session, json!({"op": "undo"}));
    assert!(session.snapshot()["model"]["animations"]["L"].get("loopEditing").is_none());
    ok(&mut session, json!({"op": "redo"}));
    assert_eq!(session.snapshot()["model"]["animations"]["L"]["loopEditing"], true);
    assert_eq!(session.snapshot()["model"]["animations"]["L"]["lanes"], before);
    ok(&mut session, json!({"op": "animation.set", "name": "L", "loopEditing": false}));
    assert!(session.snapshot()["model"]["animations"]["L"].get("loopEditing").is_none());
    assert_eq!(session.snapshot()["model"]["animations"]["L"]["lanes"], before);
}

#[test]
fn loop_edit_flag_survives_inx_write_and_read() {
    let mut session = session();
    enable_loop(&mut session, "L");
    let path = std::env::temp_dir().join(format!(
        "soul2d-loop-edit-{}-{}.inx", std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    ok(&mut session, json!({"op": "save", "path": path}));
    let mut reopened = Session::new();
    let response = reopened.handle_request(&json!({"op": "open", "path": path}));
    assert_eq!(response["succeeded"], true, "{response}");
    let clip = &reopened.snapshot()["model"]["animations"]["L"];
    assert_eq!(clip["loopEditing"], true);
    assert_eq!(clip["lanes"], session.snapshot()["model"]["animations"]["L"]["lanes"]);
}

#[test]
fn loop_edit_rejects_non_boolean_values() {
    let mut session = session();
    let message = fail(&mut session, json!({"op": "animation.set", "name": "L",
        "loopEditing": "yes"}));
    assert!(message.contains("loopEditing"), "{message}");
    fail(&mut session, json!({"op": "animation.set", "name": "L", "loopEditing": 1}));
    fail(&mut session, json!({"op": "animation.set", "name": "L", "loopEditing": Value::Null}));
    assert!(session.snapshot()["model"]["animations"]["L"].get("loopEditing").is_none());
}

#[test]
fn loop_edit_enable_requires_lead_markers_to_span_the_clip() {
    let mut session = session();
    ok(&mut session, json!({"op": "animation.set", "name": "L", "leadIn": 2}));
    let message = fail(&mut session, json!({"op": "animation.set", "name": "L",
        "loopEditing": true}));
    assert!(message.contains("lead"), "{message}");
    ok(&mut session, json!({"op": "animation.set", "name": "L", "leadIn": -1}));
    fail(&mut session, json!({"op": "animation.set", "name": "L", "loopEditing": true,
        "leadOut": 6}));
    assert!(session.snapshot()["model"]["animations"]["L"].get("loopEditing").is_none());
    enable_loop(&mut session, "L");
    fail(&mut session, json!({"op": "animation.set", "name": "L", "leadOut": 6}));
    assert_eq!(session.snapshot()["model"]["animations"]["L"]["leadOut"], -1);
    ok(&mut session, json!({"op": "animation.set", "name": "L", "loopEditing": false,
        "leadOut": 6}));
    assert!(session.snapshot()["model"]["animations"]["L"].get("loopEditing").is_none());
    assert_eq!(session.snapshot()["model"]["animations"]["L"]["leadOut"], 6);
}

#[test]
fn loop_edit_noop_false_on_an_old_clip_keeps_history_empty() {
    let mut session = session();
    let before = session.snapshot()["model"]["animations"]["L"].clone();
    ok(&mut session, json!({"op": "animation.set", "name": "L", "loopEditing": false}));
    assert_eq!(session.snapshot()["model"]["animations"]["L"], before);
    let undone = session.handle_request(&json!({"op": "undo"}));
    assert_eq!(undone["succeeded"], false, "{undone}");
}

#[test]
fn loop_edit_advance_wraps_past_the_clip_end() {
    let mut session = session();
    ok(&mut session, json!({"op": "animation.seek", "name": "Run", "frame": 10.0}));
    let mut frame = 0.0;
    for _ in 0..20 {
        let result = ok(&mut session, json!({"op": "animation.advance", "dt": 0.001}));
        frame = result["animation"]["frame"].as_f64().unwrap();
    }
    assert!((frame - 10.32).abs() <= 1e-4, "frame {frame}");
    ok(&mut session, json!({"op": "animation.seek", "name": "Run", "frame": 10.49}));
    let result = ok(&mut session, json!({"op": "animation.advance", "dt": 0.002}));
    let frame = result["animation"]["frame"].as_f64().unwrap();
    assert!((frame - 10.522).abs() <= 1e-3, "frame {frame}");
    ok(&mut session, json!({"op": "animation.seek", "name": "Run", "frame": 10.99}));
    let result = ok(&mut session, json!({"op": "animation.advance", "dt": 0.001}));
    let frame = result["animation"]["frame"].as_f64().unwrap();
    assert!(frame.abs() <= 1e-3, "frame {frame}");
}

#[test]
fn loop_edit_advance_without_loop_still_clamps() {
    let mut session = session();
    ok(&mut session, json!({"op": "animation.seek", "name": "Run", "frame": 10.0}));
    let result = ok(&mut session, json!({"op": "animation.advance", "dt": 0.001, "loop": false}));
    let frame = result["animation"]["frame"].as_f64().unwrap();
    assert!((frame - 10.0).abs() <= 1e-4, "frame {frame}");
}

#[test]
fn loop_edit_advance_respects_custom_lead_markers() {
    let mut session = session();
    ok(&mut session, json!({"op": "animation.set", "name": "Run", "leadIn": 2, "leadOut": 8}));
    ok(&mut session, json!({"op": "animation.seek", "name": "Run", "frame": 7.9}));
    let result = ok(&mut session, json!({"op": "animation.advance", "dt": 0.01}));
    let frame = result["animation"]["frame"].as_f64().unwrap();
    assert!((frame - 2.0).abs() <= 1e-3, "frame {frame}");
    ok(&mut session, json!({"op": "animation.seek", "name": "Run", "frame": 5.0}));
    let result = ok(&mut session, json!({"op": "animation.advance", "dt": 0.01}));
    let frame = result["animation"]["frame"].as_f64().unwrap();
    assert!((frame - 5.16).abs() <= 0.05, "frame {frame}");
}
