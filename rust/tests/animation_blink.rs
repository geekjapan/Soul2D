//! Eye blinking (ANI02): the model lists its eye parameters as `live2d.eyeBlink`, and a clip's `eyeBlink` track scales
//! them toward their minimum while it plays.

use serde_json::{json, Value};
use soul2d_core::Session;

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn parameter(uuid: u32, name: &str, is_vec2: bool, min: f64, max: f64, default: f64) -> Value {
    json!({
        "uuid": uuid, "name": name, "is_vec2": is_vec2, "min": [min, min], "max": [max, max],
        "defaults": [default, default], "axis_points": [[0.0, 1.0], if is_vec2 { vec![0.0, 1.0] } else { vec![0.0] }],
        "merge_mode": "Additive",
        "bindings": [],
    })
}

/// "EyeL" runs 0 to 1 and has a lane holding it at 0.8. "EyeR" runs 0.5 to 2 and has no lane. "Mouth" is not an eye.
/// "Look" is 2D and 30 a parameter group. The 12-frame clip "Idle" holds them.
fn session() -> Session {
    session_with(json!({}))
}

/// The same document with `live2d` as given.
fn session_with(live2d: Value) -> Session {
    let model = json!({
        "live2d": live2d,
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": {
            "uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
            "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
            "lockToRoot": false, "pinToMesh": false,
        },
        "param": [
            parameter(21, "EyeL", false, 0.0, 1.0, 1.0),
            parameter(22, "EyeR", false, 0.5, 2.0, 2.0),
            parameter(23, "Mouth", false, 0.0, 1.0, 1.0),
            parameter(24, "Look", true, -1.0, 1.0, 0.0),
            json!({"groupUUID": 30, "name": "Face", "children": []}),
        ],
        "animations": {"Idle": {
            "length": 12, "timestep": 0.1, "additive": false, "animationWeight": 1.0, "leadIn": -1, "leadOut": -1,
            "lanes": [{"uuid": 21, "target": 0, "interpolation": "Linear", "merge_mode": "Forced",
                       "keyframes": [{"frame": 0, "value": 0.8, "tension": 0.5}]}],
        }},
    });
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new", "model": model})));
    session
}

fn history_count(session: &mut Session) -> u64 {
    result(&request(session, json!({"op": "history.summary"})))["count"].as_u64().unwrap()
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

fn blink(fields: Value) -> Value {
    let mut request = json!({"op": "animation.blink.set", "name": "Idle"});
    request.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
    request
}

/// Eyes 21 and 22, open at frames 0 and 8 and closed at frame 4.
fn blinking() -> Session {
    let mut s = session();
    result(&request(&mut s, json!({"op": "animation.blink.parameters.set", "parameters": [21, 22]})));
    for (frame, value) in [(0, 1.0), (4, 0.0), (8, 1.0)] {
        result(&request(&mut s, blink(json!({"frame": frame, "value": value}))));
    }
    s
}

/// The sampled values of eyes 21 and 22 and of the mouth 23, axis 0.
fn sampled(session: &mut Session, frame: f64) -> [f64; 3] {
    let values = result(&request(session, json!({"op": "animation.sample", "name": "Idle", "frame": frame})))["values"]
        .clone();
    ["21", "22", "23"].map(|id| values[id][0].as_f64().unwrap_or(f64::NAN))
}

fn assert_close(actual: [f64; 3], expected: [f64; 3]) {
    assert!(
        actual.iter().zip(expected).all(|(actual, expected)| (actual - expected).abs() < 1e-6),
        "{actual:?} != {expected:?}"
    );
}

#[test]
fn eye_parameters_are_one_undo_step_and_refuse_what_cannot_blink() {
    let mut s = session();
    one_undo_step(&mut s, json!({"op": "animation.blink.parameters.set", "parameters": [22, 21]}));
    assert_eq!(s.snapshot()["model"]["live2d"]["eyeBlink"], json!([22, 21]));
    assert!(rejected(&mut s, json!({"op": "animation.blink.parameters.set", "parameters": [99]})).contains("99"));
    assert!(rejected(&mut s, json!({"op": "animation.blink.parameters.set", "parameters": [24]})).contains("2D"));
    assert!(rejected(&mut s, json!({"op": "animation.blink.parameters.set", "parameters": [21, 21]})).contains("twice"));
    assert!(rejected(&mut s, json!({"op": "animation.blink.parameters.set", "parameters": [30]})).contains("group"));
    rejected(&mut s, json!({"op": "animation.blink.parameters.set", "parameters": 21}));
    rejected(&mut s, json!({"op": "animation.blink.parameters.set", "parameters": [-1]}));

    one_undo_step(&mut s, json!({"op": "animation.blink.parameters.set", "parameters": []}));
    assert_eq!(s.snapshot()["model"]["live2d"].get("eyeBlink"), None);

    // Deleting an eye parameter takes it off the list.
    result(&request(&mut s, json!({"op": "animation.blink.parameters.set", "parameters": [21, 22]})));
    result(&request(&mut s, json!({"op": "parameter.delete", "id": 21})));
    assert_eq!(s.snapshot()["model"]["live2d"]["eyeBlink"], json!([22]));
}

#[test]
fn the_track_keeps_sorted_keys_and_an_effect_in_one_undo_step_each() {
    let mut s = session();
    // A track whose last key goes leaves no trace in the clip.
    one_undo_step(&mut s, blink(json!({"frame": 8, "value": 1.0})));
    result(&request(&mut s, json!({"op": "animation.blink.key.delete", "name": "Idle", "frame": 8})));
    assert_eq!(s.snapshot()["model"]["animations"]["Idle"].get("eyeBlink"), None);
    one_undo_step(&mut s, blink(json!({"frame": 8, "value": 1.0})));
    one_undo_step(&mut s, blink(json!({"frame": 0, "value": 1})));
    one_undo_step(&mut s, blink(json!({"frame": 4, "value": 0.25})));
    one_undo_step(&mut s, blink(json!({"frame": 4, "value": 0})));
    one_undo_step(&mut s, blink(json!({"effect": 50})));
    let track = || json!({"effect": 50.0, "keyframes": [
        {"frame": 0, "value": 1.0}, {"frame": 4, "value": 0.0}, {"frame": 8, "value": 1.0}]});
    assert_eq!(s.snapshot()["model"]["animations"]["Idle"]["eyeBlink"], track());

    for (fields, message) in [
        (json!({"frame": 2, "value": 1.5}), "value must be a number from 0 to 1"),
        (json!({"frame": 2, "value": -0.1}), "value must be a number from 0 to 1"),
        (json!({"effect": 100.5}), "effect must be a number from 0 to 100"),
        (json!({"effect": "50"}), "effect must be a number from 0 to 100"),
        (json!({"frame": 12, "value": 1}), "frame must be between 0 and 11"),
        (json!({"frame": 2}), "frame and value go together"),
        (json!({"value": 1}), "frame and value go together"),
        (json!({}), "animation.blink.set requires effect, or frame and value"),
    ] {
        assert_eq!(rejected(&mut s, blink(fields)), message);
    }
    assert!(rejected(&mut s, json!({"op": "animation.set", "name": "Idle", "length": 8})).contains("eyeBlink"));
    assert!(rejected(&mut s, json!({"op": "animation.blink.key.delete", "name": "Idle", "frame": 5})).contains("frame 5"));

    one_undo_step(&mut s, json!({"op": "animation.blink.key.delete", "name": "Idle", "frame": 4}));
    result(&request(&mut s, json!({"op": "animation.blink.key.delete", "name": "Idle", "frame": 0})));
    result(&request(&mut s, json!({"op": "animation.blink.key.delete", "name": "Idle", "frame": 8})));
    assert_eq!(s.snapshot()["model"]["animations"]["Idle"]["eyeBlink"], json!({"effect": 50.0}));

    // A rename carries the track.
    result(&request(&mut s, blink(json!({"frame": 3, "value": 0.5}))));
    result(&request(&mut s, json!({"op": "animation.rename", "name": "Idle", "new_name": "Blink"})));
    assert_eq!(
        s.snapshot()["model"]["animations"]["Blink"]["eyeBlink"],
        json!({"effect": 50.0, "keyframes": [{"frame": 3, "value": 0.5}]})
    );
}

#[test]
fn playback_scales_each_eye_toward_its_minimum() {
    let mut s = blinking();
    // EyeL: lane 0.8 times the openness. EyeR: from its default 2 toward its minimum 0.5. The mouth stays open.
    assert_close(sampled(&mut s, 0.0), [0.8, 2.0, 1.0]);
    assert_close(sampled(&mut s, 1.0), [0.6, 1.625, 1.0]);
    assert_close(sampled(&mut s, 2.0), [0.4, 1.25, 1.0]);
    assert_close(sampled(&mut s, 4.0), [0.0, 0.5, 1.0]);
    assert_close(sampled(&mut s, 11.0), [0.8, 2.0, 1.0]);

    result(&request(&mut s, blink(json!({"effect": 50}))));
    assert_close(sampled(&mut s, 4.0), [0.4, 1.25, 1.0]);
    result(&request(&mut s, blink(json!({"effect": 0}))));
    assert_close(sampled(&mut s, 4.0), [0.8, 2.0, 1.0]);
    result(&request(&mut s, blink(json!({"effect": 100}))));

    // An eye without a lane blinks from its runtime value.
    result(&request(&mut s, json!({"op": "value.set", "id": 22, "value": [1.0, 0.5]})));
    assert_close(sampled(&mut s, 2.0), [0.4, 0.75, 1.0]);

    // A track with an effect and no keys moves nothing.
    for frame in [0, 4, 8] {
        result(&request(&mut s, json!({"op": "animation.blink.key.delete", "name": "Idle", "frame": frame})));
    }
    assert_close(sampled(&mut s, 4.0), [0.8, 1.0, 1.0]);
    result(&request(&mut s, blink(json!({"frame": 4, "value": 0}))));
    assert_close(sampled(&mut s, 4.0), [0.0, 0.5, 1.0]);

    // Without listed eyes the track moves nothing.
    result(&request(&mut s, json!({"op": "animation.blink.parameters.set", "parameters": []})));
    assert_close(sampled(&mut s, 4.0), [0.8, 1.0, 1.0]);
}

#[test]
fn seeking_a_blinking_clip_previews_the_closed_eyes() {
    let mut s = blinking();
    result(&request(&mut s, json!({"op": "animation.seek", "name": "Idle", "frame": 4})));
    let values = s.snapshot()["values"].clone();
    assert_eq!((values["21"][0].as_f64(), values["22"][0].as_f64()), (Some(0.0), Some(0.5)), "{values}");
    assert_eq!(values["21"][1].as_f64(), Some(1.0), "axis 1 of a 1D eye does not blink: {values}");
}

#[test]
fn a_key_taken_while_the_clip_is_shown_records_the_eye_before_the_blink() {
    let mut s = blinking();
    result(&request(&mut s, json!({"op": "animation.seek", "name": "Idle", "frame": 4})));
    result(&request(&mut s, json!({"op": "animation.key.add", "name": "Idle", "parameter": 21, "frame": 4})));
    result(&request(&mut s, json!({"op": "animation.seek", "name": "Idle", "frame": 2})));
    result(&request(&mut s, json!({"op": "animation.key.add_all", "name": "Idle", "frame": 2, "parameters": [22]})));
    let lanes = s.snapshot()["model"]["animations"]["Idle"]["lanes"].clone();
    let keys = |uuid: u64| -> Vec<(i64, f64)> {
        let lane = lanes.as_array().unwrap().iter().find(|lane| lane["uuid"] == uuid && lane["target"] == 0).unwrap();
        lane["keyframes"].as_array().unwrap().iter().map(|key| (key["frame"].as_i64().unwrap(), key["value"].as_f64().unwrap())).collect()
    };
    assert_eq!(keys(21), [(0, 0.8), (4, 0.8)], "not the closed 0");
    assert_eq!(keys(22), [(2, 2.0)], "not the half-closed 1.25");
}

#[test]
fn an_eye_made_2d_leaves_the_list() {
    let mut s = blinking();
    result(&request(&mut s, json!({"op": "parameter.convert_to_2d", "id": 21})));
    assert_eq!(s.snapshot()["model"]["live2d"]["eyeBlink"], json!([22]));
    result(&request(&mut s, json!({"op": "parameter.convert_to_2d", "id": 22})));
    assert_eq!(s.snapshot()["model"]["live2d"].get("eyeBlink"), None, "an empty list leaves no key");
}

#[test]
fn a_file_listing_a_group_or_a_missing_parameter_blinks_only_the_eyes() {
    let mut s = session_with(json!({"eyeBlink": [30, 99, 21, 21]}));
    result(&request(&mut s, blink(json!({"frame": 0, "value": 1}))));
    result(&request(&mut s, blink(json!({"frame": 4, "value": 0}))));
    let values = result(&request(&mut s, json!({"op": "animation.sample", "name": "Idle", "frame": 4})))["values"].clone();
    assert_eq!(values["21"][0].as_f64(), Some(0.0), "{values}");
    assert_eq!((values.get("30"), values.get("99")), (None, None), "{values}");
    let half = result(&request(&mut s, json!({"op": "animation.sample", "name": "Idle", "frame": 2})))["values"].clone();
    let eye = half["21"][0].as_f64().unwrap();
    assert!((eye - 0.4).abs() < 1e-6, "an eye listed twice blinks once: {half}");
}

#[test]
fn a_bad_blink_key_from_a_file_can_be_replaced_or_deleted() {
    let mut s = session();
    let mut model = s.snapshot()["model"].clone();
    model["animations"]["Idle"]["eyeBlink"] = json!({"keyframes": [{"frame": 2, "value": 1.5}, {"frame": 6, "value": 0}]});
    result(&request(&mut s, json!({"op": "new", "model": model.clone()})));
    assert!(rejected(&mut s, json!({"op": "animation.sample", "name": "Idle", "frame": 0})).contains("value"));
    // An edit that leaves a bad key is refused; one that replaces or deletes it is not.
    assert!(rejected(&mut s, blink(json!({"frame": 6, "value": 1}))).contains("keyframes[0].value"));
    result(&request(&mut s, blink(json!({"frame": 2, "value": 1}))));
    assert_eq!(
        s.snapshot()["model"]["animations"]["Idle"]["eyeBlink"],
        json!({"keyframes": [{"frame": 2, "value": 1.0}, {"frame": 6, "value": 0}]})
    );
    result(&request(&mut s, json!({"op": "new", "model": model})));
    result(&request(&mut s, json!({"op": "animation.blink.key.delete", "name": "Idle", "frame": 2})));
    assert_eq!(s.snapshot()["model"]["animations"]["Idle"]["eyeBlink"], json!({"keyframes": [{"frame": 6, "value": 0}]}));
}
