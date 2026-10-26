//! Timeline markers of an INX animation: a note on a frame, kept in the clip as `markers: [{frame, text}]`.

use serde_json::{json, Value};
use soul2d_core::Session;

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

/// An empty document with the 60-frame clips "Idle" and "Walk".
fn session() -> Session {
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new"})));
    for name in ["Idle", "Walk"] {
        result(&request(&mut session, json!({"op": "animation.create", "name": name})));
    }
    session
}

fn clip(session: &Session, name: &str) -> Value {
    session.snapshot()["model"]["animations"][name].clone()
}

fn history_count(session: &mut Session) -> u64 {
    result(&request(session, json!({"op": "history.summary"})))["count"].as_u64().unwrap()
}

fn set(frame: i64, text: &str) -> Value {
    json!({"op": "animation.marker.set", "name": "Idle", "frame": frame, "text": text})
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

#[test]
fn markers_are_kept_sorted_by_frame_and_one_per_frame() {
    let mut s = session();
    let bare = clip(&s, "Idle");
    one_undo_step(&mut s, set(30, "Blink"));
    one_undo_step(&mut s, set(10, "Start"));
    one_undo_step(&mut s, set(59, ""));
    assert_eq!(
        clip(&s, "Idle")["markers"],
        json!([{"frame": 10, "text": "Start"}, {"frame": 30, "text": "Blink"}, {"frame": 59, "text": ""}])
    );
    assert_eq!(clip(&s, "Walk").get("markers"), None, "a marker belongs to one clip");

    // A second marker on a frame replaces the text of the first.
    one_undo_step(&mut s, set(30, "Blink twice"));
    assert_eq!(
        clip(&s, "Idle")["markers"],
        json!([{"frame": 10, "text": "Start"}, {"frame": 30, "text": "Blink twice"}, {"frame": 59, "text": ""}])
    );

    // The same marker again changes nothing and adds no undo step.
    let count = history_count(&mut s);
    result(&request(&mut s, set(30, "Blink twice")));
    assert_eq!(history_count(&mut s), count);

    one_undo_step(&mut s, json!({"op": "animation.marker.delete", "name": "Idle", "frame": 10}));
    assert_eq!(clip(&s, "Idle")["markers"], json!([{"frame": 30, "text": "Blink twice"}, {"frame": 59, "text": ""}]));
    for frame in [30, 59] {
        one_undo_step(&mut s, json!({"op": "animation.marker.delete", "name": "Idle", "frame": frame}));
    }
    assert_eq!(clip(&s, "Idle"), bare, "a clip without markers is the clip that never had one");
}

#[test]
fn markers_follow_their_clip_and_leave_the_keys_alone() {
    let mut s = session();
    result(&request(&mut s, json!({"op": "parameter.create", "name": "P", "dimensions": 1})));
    let parameter = s.snapshot()["model"]["param"][0]["uuid"].clone();
    result(&request(
        &mut s,
        json!({"op": "animation.key.set", "name": "Idle", "parameter": parameter, "axis": 0, "frame": 5, "value": 0.5}),
    ));
    let lanes = clip(&s, "Idle")["lanes"].clone();
    one_undo_step(&mut s, set(5, "Key here"));
    assert_eq!(clip(&s, "Idle")["lanes"], lanes);

    result(&request(&mut s, json!({"op": "animation.rename", "name": "Idle", "new_name": "Rest"})));
    assert_eq!(clip(&s, "Rest")["markers"], json!([{"frame": 5, "text": "Key here"}]));
    result(&request(&mut s, json!({"op": "animation.delete", "name": "Rest"})));
    assert!(s.snapshot()["model"]["animations"].get("Rest").is_none());
}

#[test]
fn a_clip_cannot_be_cut_below_a_marker() {
    let mut s = session();
    result(&request(&mut s, set(10, "Start")));
    result(&request(&mut s, set(30, "Blink")));
    let message = rejected(&mut s, json!({"op": "animation.set", "name": "Idle", "length": 30}));
    assert_eq!(message, "animation.markers[1].frame is outside animation length");
    result(&request(&mut s, json!({"op": "animation.set", "name": "Idle", "length": 31})));
    result(&request(&mut s, json!({"op": "animation.set", "name": "Idle", "length": 600})));
    result(&request(&mut s, set(599, "End")));
    assert_eq!(clip(&s, "Idle")["markers"].as_array().unwrap().len(), 3);
}

#[test]
fn a_bad_marker_request_is_refused_and_changes_nothing() {
    let mut s = session();
    result(&request(&mut s, set(10, "Start")));
    let delete = |frame: Value| json!({"op": "animation.marker.delete", "name": "Idle", "frame": frame});
    for (bad, message) in [
        (json!({"op": "animation.marker.set", "name": "Missing", "frame": 0, "text": ""}), "animation \"Missing\" was not found"),
        (set(60, "Past the end"), "frame must be between 0 and 59"),
        (set(-1, "Before the start"), "frame must be between 0 and 59"),
        (json!({"op": "animation.marker.set", "name": "Idle", "frame": 1.5, "text": ""}), "frame must be an integer"),
        (json!({"op": "animation.marker.set", "name": "Idle", "text": ""}), "frame must be an integer"),
        (json!({"op": "animation.marker.set", "name": "Idle", "frame": 1}), "text must be a string"),
        (json!({"op": "animation.marker.set", "name": "Idle", "frame": 1, "text": 7}), "text must be a string"),
        (set(1, &"x".repeat(257)), "text must be at most 256 characters"),
        (delete(json!(11)), "animation \"Idle\" has no marker at frame 11"),
        (delete(json!(60)), "frame must be between 0 and 59"),
        (json!({"op": "animation.marker.delete", "name": "Missing", "frame": 10}), "animation \"Missing\" was not found"),
    ] {
        assert_eq!(rejected(&mut s, bad.clone()), message, "{bad}");
    }
    result(&request(&mut s, set(1, &"x".repeat(256))));
}

#[test]
fn a_marker_list_from_another_tool_is_edited_in_place_or_refused() {
    let model = |markers: Value| {
        json!({
            "meta": {"name": "fixture", "version": "1.0-alpha"},
            "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
                "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                "lockToRoot": false, "pinToMesh": false, "children": []},
            "param": [],
            "animations": {"Idle": {"length": 60, "timestep": 0.016, "additive": false, "animationWeight": 1.0,
                "leadIn": -1, "leadOut": -1, "lanes": [], "markers": markers}},
        })
    };
    // A field this version does not know stays on the marker when its text changes.
    let mut s = Session::new();
    let known = json!([{"frame": 4, "text": "Old", "color": "red"}]);
    result(&request(&mut s, json!({"op": "new", "model": model(known)})));
    one_undo_step(&mut s, set(4, "New"));
    assert_eq!(clip(&s, "Idle")["markers"], json!([{"frame": 4, "text": "New", "color": "red"}]));

    // A list that is out of order is put in order by the first edit.
    let mut s = Session::new();
    let unsorted = json!([{"frame": 30, "text": "A"}, {"frame": 10, "text": "B"}]);
    result(&request(&mut s, json!({"op": "new", "model": model(unsorted)})));
    one_undo_step(&mut s, set(20, "C"));
    assert_eq!(
        clip(&s, "Idle")["markers"],
        json!([{"frame": 10, "text": "B"}, {"frame": 20, "text": "C"}, {"frame": 30, "text": "A"}])
    );

    // Two markers on a frame, or a marker without a frame: which one an edit means is not ours to guess.
    for (markers, message) in [
        (json!([{"frame": 4, "text": "A"}, {"frame": 4, "text": "B"}]), "animation.markers must have unique frames"),
        (json!([{"frame": 4, "text": "A"}, {"text": "B"}]), "animation.markers[1].frame must be an integer"),
    ] {
        let mut s = Session::new();
        result(&request(&mut s, json!({"op": "new", "model": model(markers)})));
        assert_eq!(rejected(&mut s, set(4, "New")), message);
        assert_eq!(rejected(&mut s, json!({"op": "animation.marker.delete", "name": "Idle", "frame": 4})), message);
    }

    // A marker outside the clip cannot be set, but one that a file brought in can be deleted.
    let mut s = Session::new();
    let outside = json!([{"frame": -3, "text": "Early"}, {"frame": 4, "text": "In"}, {"frame": 999, "text": "Late"}]);
    result(&request(&mut s, json!({"op": "new", "model": model(outside)})));
    assert_eq!(rejected(&mut s, set(999, "Later")), "frame must be between 0 and 59");
    // Such a marker can be locked, unlocked and moved into the clip, so a locked one is never stuck out there.
    let outside = |frame: i64, extra: Value| {
        let mut request = json!({"op": "animation.marker.set", "name": "Idle", "frame": frame});
        request.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        request
    };
    one_undo_step(&mut s, outside(-3, json!({"locked": true})));
    let message = rejected(&mut s, json!({"op": "animation.marker.delete", "name": "Idle", "frame": -3}));
    assert_eq!(message, "the marker at frame -3 is locked");
    one_undo_step(&mut s, outside(-3, json!({"locked": false})));
    one_undo_step(&mut s, outside(999, json!({"to": 7})));
    assert_eq!(clip(&s, "Idle")["markers"][2], json!({"frame": 7, "text": "Late"}));
    one_undo_step(&mut s, outside(7, json!({"to": 59})));
    result(&request(&mut s, json!({"op": "new", "model": model(json!([{"frame": -3, "text": "Early"}, {"frame": 4, "text": "In"}, {"frame": 999, "text": "Late"}]))})));
    for frame in [999, -3] {
        one_undo_step(&mut s, json!({"op": "animation.marker.delete", "name": "Idle", "frame": frame}));
    }
    assert_eq!(clip(&s, "Idle")["markers"], json!([{"frame": 4, "text": "In"}]));
    let message = rejected(&mut s, json!({"op": "animation.marker.delete", "name": "Idle", "frame": 999}));
    assert_eq!(message, "frame must be between 0 and 59");

    // A `markers` value of another shape is not ours to rewrite.
    let mut s = Session::new();
    result(&request(&mut s, json!({"op": "new", "model": model(json!("three"))})));
    assert_eq!(rejected(&mut s, set(4, "New")), "animation.markers must be an array");
    result(&request(&mut s, json!({"op": "animation.set", "name": "Idle", "length": 10})));
}

#[test]
fn the_marker_tools_run_through_mcp_and_the_snapshot_resource_shows_them() {
    let mut s = session();
    let mut call = |name: &str, arguments: Value| {
        s.handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}}))
            .unwrap()
    };
    let response = call("animation_marker_set", json!({"name": "Idle", "frame": 12, "text": "Wave"}));
    assert_eq!(response["result"]["isError"], false, "{response}");
    let response = call("animation_marker_set", json!({"name": "Idle", "frame": 40, "text": "Rest"}));
    assert_eq!(response["result"]["isError"], false, "{response}");
    let response = call("animation_marker_delete", json!({"name": "Idle", "frame": 40}));
    assert_eq!(response["result"]["isError"], false, "{response}");
    let refused = call("animation_marker_set", json!({"name": "Idle", "frame": 12}));
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    let text = refused["result"]["content"][0]["text"].as_str().unwrap();
    // `text` is optional in the schema, for a move or a lock, so the core answers for a marker that has none of them.
    assert!(text.contains("text must be a string"), "{text}");
    let response = call("animation_marker_set", json!({"name": "Idle", "frame": 12, "to": 13, "locked": true}));
    assert_eq!(response["result"]["isError"], false, "{response}");
    let response = call("animation_marker_set", json!({"name": "Idle", "frame": 13, "to": 12, "locked": false}));
    assert_eq!(response["result"]["isError"], false, "{response}");

    let read = s
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "resources/read",
            "params": {"uri": "resource://vsoul/snapshot"}}))
        .unwrap();
    let snapshot: Value = serde_json::from_str(read["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(snapshot["model"]["animations"]["Idle"]["markers"], json!([{"frame": 12, "text": "Wave"}]));
}

#[test]
fn a_marker_moves_in_one_undo_step_and_keeps_its_text_and_other_fields() {
    let mut s = session();
    result(&request(&mut s, set(10, "Start")));
    result(&request(&mut s, set(30, "Blink")));
    one_undo_step(&mut s, json!({"op": "animation.marker.set", "name": "Idle", "frame": 10, "to": 40}));
    assert_eq!(clip(&s, "Idle")["markers"], json!([{"frame": 30, "text": "Blink"}, {"frame": 40, "text": "Start"}]));
    // A move and a new text in one request.
    one_undo_step(&mut s, json!({"op": "animation.marker.set", "name": "Idle", "frame": 40, "to": 5, "text": "Go"}));
    assert_eq!(clip(&s, "Idle")["markers"], json!([{"frame": 5, "text": "Go"}, {"frame": 30, "text": "Blink"}]));
    for (bad, message) in [
        (json!({"frame": 5, "to": 30}), "animation \"Idle\" already has a marker at frame 30"),
        (json!({"frame": 5, "to": 60}), "to must be between 0 and 59"),
        (json!({"frame": 5, "to": 1.5}), "to must be an integer"),
        (json!({"frame": 7, "to": 8}), "animation \"Idle\" has no marker at frame 7"),
        (json!({"frame": 7, "to": 8, "text": "New"}), "animation \"Idle\" has no marker at frame 7"),
    ] {
        let mut bad = bad;
        bad["op"] = json!("animation.marker.set");
        bad["name"] = json!("Idle");
        assert_eq!(rejected(&mut s, bad), message);
    }
}

#[test]
fn a_locked_marker_changes_only_in_a_request_that_unlocks_it() {
    let mut s = session();
    result(&request(&mut s, set(10, "Start")));
    let marker = |extra: Value| {
        let mut request = json!({"op": "animation.marker.set", "name": "Idle", "frame": 10});
        request.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        request
    };
    one_undo_step(&mut s, marker(json!({"locked": true})));
    assert_eq!(clip(&s, "Idle")["markers"], json!([{"frame": 10, "text": "Start", "locked": true}]));
    for bad in [
        marker(json!({"text": "Other"})),
        marker(json!({"to": 20})),
        marker(json!({"to": 20, "locked": true})),
        json!({"op": "animation.marker.delete", "name": "Idle", "frame": 10}),
    ] {
        assert_eq!(rejected(&mut s, bad), "the marker at frame 10 is locked");
    }
    assert_eq!(rejected(&mut s, marker(json!({"locked": "yes"}))), "locked must be a boolean");
    // Unlocking and moving in one request; the unlocked marker has no `locked` key, as one never locked.
    one_undo_step(&mut s, marker(json!({"locked": false, "to": 20})));
    assert_eq!(clip(&s, "Idle")["markers"], json!([{"frame": 20, "text": "Start"}]));
    // A marker can be made locked.
    one_undo_step(&mut s, json!({"op": "animation.marker.set", "name": "Idle", "frame": 3, "text": "Hold", "locked": true}));
    assert_eq!(clip(&s, "Idle")["markers"][0], json!({"frame": 3, "text": "Hold", "locked": true}));
}
