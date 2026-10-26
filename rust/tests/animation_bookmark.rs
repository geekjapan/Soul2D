use serde_json::{json, Value};
use soul2d_core::Session;

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    response["result"].clone()
}

fn node(id: u32, name: &str, children: Value) -> Value {
    json!({
        "uuid": id, "name": name, "type": "Node", "enabled": true, "zsort": 0.0,
        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
        "lockToRoot": false, "pinToMesh": false, "children": children,
    })
}

fn parameter(id: u32, name: &str, node: u32) -> Value {
    json!({
        "uuid": id, "name": name, "is_vec2": false, "min": [0.0, 0.0], "max": [1.0, 1.0],
        "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0]], "merge_mode": "Additive",
        "bindings": [{"node": node, "param_name": "transform.t.x", "interpolate_mode": "Linear",
                      "values": [[0.0], [10.0]], "isSet": [[true], [true]]}],
    })
}

fn clip() -> Value {
    json!({"length": 12, "timestep": 0.1, "additive": false, "animationWeight": 1.0, "leadIn": -1, "leadOut": -1,
        "lanes": []})
}

/// Root > Body > Arm and Root > Head. Lean binds Body, Wave (in a group) binds Arm, Nod binds Head, Spin binds Root.
fn model() -> Value {
    json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": node(1, "Root", json!([node(2, "Body", json!([node(3, "Arm", json!([]))])), node(4, "Head", json!([]))])),
        "param": [
            parameter(21, "Lean", 2),
            {"groupUUID": 40, "name": "Hands", "children": [parameter(22, "Wave", 3)]},
            parameter(23, "Nod", 4),
            parameter(24, "Spin", 1),
        ],
        "animations": {"Idle": clip(), "Walk": clip()},
    })
}

fn session() -> Session {
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new", "model": model()})));
    session
}

fn labels(session: &Session, clip: &str) -> Value {
    session.snapshot()["model"]["animations"][clip].get("bookmarks").cloned().unwrap_or(Value::Null)
}

#[test]
fn a_node_lists_the_parameters_bound_to_it_or_above_it() {
    let mut session = session();
    let revision = session.snapshot()["revision"].clone();
    let parameters = |session: &mut Session, node: u32| {
        result(&request(session, json!({"op": "animation.bookmark.parameters", "node": node})))
    };
    assert_eq!(parameters(&mut session, 3), json!({"node": 3, "name": "Arm", "parameters": [
        {"uuid": 21, "name": "Lean"}, {"uuid": 22, "name": "Wave"}, {"uuid": 24, "name": "Spin"}]}));
    assert_eq!(parameters(&mut session, 4)["parameters"], json!([{"uuid": 23, "name": "Nod"}, {"uuid": 24, "name": "Spin"}]));
    assert_eq!(parameters(&mut session, 1)["parameters"], json!([{"uuid": 24, "name": "Spin"}]));
    let missing = request(&mut session, json!({"op": "animation.bookmark.parameters", "node": 99}));
    assert_eq!(missing["message"], "node 99 was not found");
    let bad = request(&mut session, json!({"op": "animation.bookmark.parameters"}));
    assert_eq!(bad["message"], "node must be a positive resource UUID");
    assert_eq!(session.snapshot()["revision"], revision);
}

#[test]
fn labels_are_added_replaced_copied_and_deleted_one_undo_each() {
    let mut session = session();
    let set = |label: &str, x: f64, parameters: Value| {
        json!({"op": "animation.bookmark.set", "name": "Idle", "label": label, "x": x, "y": -2.0, "parameters": parameters})
    };
    result(&request(&mut session, set("Arm", 1.5, json!([21, 22]))));
    result(&request(&mut session, set("Head", 3.0, json!([23]))));
    let mut replace = set("Root", 0.0, json!([24]));
    replace["index"] = json!(0);
    result(&request(&mut session, replace));
    let both = json!([
        {"label": "Root", "x": 0.0, "y": -2.0, "parameters": [24]},
        {"label": "Head", "x": 3.0, "y": -2.0, "parameters": [23]},
    ]);
    assert_eq!(labels(&session, "Idle"), both);
    assert_eq!(labels(&session, "Walk"), Value::Null);

    result(&request(&mut session, json!({"op": "animation.bookmark.copy", "name": "Idle"})));
    assert_eq!(labels(&session, "Walk"), both);
    let copied = session.snapshot();
    result(&request(&mut session, json!({"op": "animation.bookmark.copy", "name": "Idle", "to": ["Walk"]})));
    assert_eq!(session.snapshot(), copied, "copying again adds nothing");

    result(&request(&mut session, json!({"op": "animation.bookmark.delete", "name": "Idle", "index": 0})));
    assert_eq!(labels(&session, "Idle"), json!([both[1]]));
    result(&request(&mut session, json!({"op": "animation.bookmark.delete", "name": "Idle"})));
    assert_eq!(labels(&session, "Idle"), Value::Null, "the field goes with the last label");
    assert_eq!(labels(&session, "Walk"), both);

    for expected in [json!([both[1]]), both.clone()] {
        result(&request(&mut session, json!({"op": "undo"})));
        assert_eq!(labels(&session, "Idle"), expected);
    }
    result(&request(&mut session, json!({"op": "undo"})));
    assert_eq!(labels(&session, "Walk"), Value::Null, "the copy is one undo");
    result(&request(&mut session, json!({"op": "undo"})));
    assert_eq!(labels(&session, "Idle")[0]["label"], "Arm", "the replace is one undo");
}

#[test]
fn deleting_a_parameter_takes_it_off_every_label() {
    let mut session = session();
    let set = json!({"op": "animation.bookmark.set", "name": "Idle", "label": "Arm", "x": 0.0, "y": 0.0,
        "parameters": [22, 21]});
    result(&request(&mut session, set));
    result(&request(&mut session, json!({"op": "animation.bookmark.copy", "name": "Idle"})));
    result(&request(&mut session, json!({"op": "parameter.delete", "id": 22})));
    for clip in ["Idle", "Walk"] {
        assert_eq!(labels(&session, clip), json!([{"label": "Arm", "x": 0.0, "y": 0.0, "parameters": [21]}]));
    }
    result(&request(&mut session, json!({"op": "parameter.delete", "id": 21})));
    assert_eq!(labels(&session, "Idle")[0]["parameters"], json!([]), "a label with no parameters left stays");
    result(&request(&mut session, json!({"op": "animation.set", "name": "Idle", "length": 20})));
}

#[test]
fn labels_change_on_a_frozen_clip_and_a_null_list_reads_as_none() {
    let mut model = model();
    model["animations"]["Idle"]["lanes"] = json!([{"uuid": 21, "target": 0, "interpolation": "Linear",
        "merge_mode": "Forced", "frozen": true, "keyframes": [{"frame": 1, "value": 0.5}]}]);
    model["animations"]["Idle"]["freezeFrame"] = json!(1);
    model["animations"]["Walk"]["bookmarks"] = Value::Null;
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new", "model": model})));
    let set = |name: &str| json!({"op": "animation.bookmark.set", "name": name, "label": "Lean", "x": 0.0, "y": 0.0,
        "parameters": [21]});
    result(&request(&mut session, set("Idle")));
    result(&request(&mut session, json!({"op": "animation.set", "name": "Walk", "length": 20})));
    let none = request(&mut session, json!({"op": "animation.bookmark.delete", "name": "Walk"}));
    assert_eq!(none["message"], "animation \"Walk\" has no bookmarks");
    result(&request(&mut session, json!({"op": "animation.bookmark.copy", "name": "Idle"})));
    let label = json!([{"label": "Lean", "x": 0.0, "y": 0.0, "parameters": [21]}]);
    assert_eq!(labels(&session, "Walk"), label, "the copy replaces the null list");
    result(&request(&mut session, json!({"op": "undo"})));
    assert_eq!(labels(&session, "Walk"), Value::Null);
    result(&request(&mut session, set("Walk")));
    assert_eq!(labels(&session, "Walk"), label, "a label replaces the null list");
    result(&request(&mut session, json!({"op": "animation.bookmark.delete", "name": "Idle"})));
    result(&request(&mut session, json!({"op": "parameter.delete", "id": 21})));
    assert_eq!(labels(&session, "Walk")[0]["parameters"], json!([]));
    assert_eq!(session.snapshot()["model"]["animations"]["Idle"]["freezeFrame"], 1, "freeze stays on");
}

#[test]
fn malformed_bookmark_requests_and_fields_are_refused() {
    let mut session = session();
    let set = |fields: Value| {
        let mut call = json!({"op": "animation.bookmark.set", "name": "Idle", "label": "Arm", "x": 0.0, "y": 0.0,
            "parameters": [21]});
        call.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        call
    };
    let label = "label must be a non-empty string of at most 256 characters";
    let to = "to must be a non-empty array of other animation names";
    let opened = session.snapshot();
    for (bad, message) in [
        (set(json!({"label": null})), label),
        (set(json!({"label": " "})), label),
        (set(json!({"label": "a".repeat(257)})), label),
        (set(json!({"x": "left"})), "x must contain finite numbers"),
        (set(json!({"y": null})), "y must contain finite numbers"),
        (set(json!({"parameters": null})), "parameters must be a non-empty array of parameter UUIDs"),
        (set(json!({"parameters": []})), "parameters must be a non-empty array of parameter UUIDs"),
        (set(json!({"parameters": [99]})), "parameter 99 was not found"),
        (set(json!({"parameters": [40]})), "parameter 40 refers to a parameter group"),
        (set(json!({"parameters": [21, 21]})), "parameter 21 is listed twice"),
        (set(json!({"index": 0})), "index must be below 0, the number of bookmarks"),
        (set(json!({"name": "Missing"})), "animation \"Missing\" was not found"),
        (json!({"op": "animation.bookmark.delete", "name": "Idle"}), "animation \"Idle\" has no bookmarks"),
        (json!({"op": "animation.bookmark.copy", "name": "Idle"}), "animation \"Idle\" has no bookmarks"),
    ] {
        assert_eq!(request(&mut session, bad.clone())["message"], message, "{bad}");
        assert_eq!(session.snapshot(), opened);
    }
    let mut set = |fields: Value| result(&request(&mut session, set(fields)));
    set(json!({}));
    set(json!({"label": "a".repeat(256)}));
    let opened = session.snapshot();
    for (bad, message) in [
        (json!({"op": "animation.bookmark.delete", "name": "Idle", "index": 2}), "index must be below 2, the number of bookmarks"),
        (json!({"op": "animation.bookmark.delete", "name": "Idle", "index": -1}), "index must be below 2, the number of bookmarks"),
        (json!({"op": "animation.bookmark.copy", "name": "Idle", "to": []}), to),
        (json!({"op": "animation.bookmark.copy", "name": "Idle", "to": "Walk"}), to),
        (json!({"op": "animation.bookmark.copy", "name": "Idle", "to": ["Idle"]}), to),
        (json!({"op": "animation.bookmark.copy", "name": "Idle", "to": ["Walk", "Walk"]}), "animation \"Walk\" is listed twice"),
        (json!({"op": "animation.bookmark.copy", "name": "Idle", "to": ["Run"]}), "animation \"Run\" was not found"),
    ] {
        assert_eq!(request(&mut session, bad.clone())["message"], message, "{bad}");
        assert_eq!(session.snapshot(), opened);
    }
    result(&request(&mut session, json!({"op": "animation.delete", "name": "Walk"})));
    let alone = request(&mut session, json!({"op": "animation.bookmark.copy", "name": "Idle"}));
    assert_eq!(alone["message"], "there is no other animation to copy the bookmarks to");

    let stored = "animation.bookmarks[0] needs a label, a finite x and y, and parameter UUIDs";
    for (bookmarks, message) in [
        (json!({}), "animation.bookmarks must be an array"),
        (json!([{"label": "", "x": 0.0, "y": 0.0, "parameters": []}]), stored),
        (json!([{"label": "Arm", "y": 0.0, "parameters": []}]), stored),
        (json!([{"label": "Arm", "x": 0.0, "y": "up", "parameters": []}]), stored),
        (json!([{"label": "Arm", "x": 0.0, "y": 0.0}]), stored),
        (json!([{"label": "Arm", "x": 0.0, "y": 0.0, "parameters": [0]}]), stored),
    ] {
        let mut model = model();
        model["animations"]["Idle"]["bookmarks"] = bookmarks;
        let mut session = Session::new();
        result(&request(&mut session, json!({"op": "new", "model": model})));
        let reply = request(&mut session, json!({"op": "animation.set", "name": "Idle", "length": 20}));
        assert_eq!(reply["message"], message);
    }
}
