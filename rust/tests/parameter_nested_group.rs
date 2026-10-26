//! Parameter edits on a parameter in a group inside a group. Soul2D creates top-level groups only, so the nested
//! document is written by hand, as a file from another tool would bring it in.

use serde_json::{json, Value};
use soul2d_core::Session;

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn node(uuid: u64, name: &str) -> Value {
    json!({
        "uuid": uuid, "name": name, "type": "Node", "enabled": true, "zsort": 0.0,
        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
        "lockToRoot": false, "pinToMesh": false, "children": [],
    })
}

/// A 1D parameter over -1..1 with the axis points 0, 0.5, 1.
fn plain(id: u64, name: &str, bindings: Value) -> Value {
    json!({
        "uuid": id, "name": name, "is_vec2": false, "min": [-1.0, 0.0], "max": [1.0, 1.0], "defaults": [0.0, 0.0],
        "axis_points": [[0.0, 0.5, 1.0], [0.0]], "merge_mode": "Additive", "bindings": bindings,
    })
}

fn binding(node: u64, values: [f64; 3]) -> Value {
    json!({
        "node": node, "param_name": "transform.t.x", "interpolate_mode": "Linear",
        "values": [[values[0]], [values[1]], [values[2]]], "isSet": [[true], [true], [true]],
    })
}

/// Group 30 holds parameter 31, group 40 and parameter 32. Group 40 holds parameters 41 and 42. Parameter 20 is at
/// the top level.
fn session() -> Session {
    let mut root = node(1, "Root");
    root["children"] = json!([node(4, "Left"), node(5, "Right")]);
    let inner = json!({"groupUUID": 40, "name": "Eyes", "color": [0.15, 0.15, 0.15], "children": [
        plain(41, "Blink", json!([binding(4, [1.0, 2.0, 3.0]), binding(5, [4.0, 5.0, 6.0])])),
        plain(42, "Wink", json!([])),
    ]});
    let outer = json!({"groupUUID": 30, "name": "Face", "color": [0.15, 0.15, 0.15], "children": [
        plain(31, "Outer", json!([])), inner, plain(32, "After", json!([])),
    ]});
    let model = json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": root, "param": [outer, plain(20, "Top", json!([]))], "animations": {},
    });
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new", "model": model})));
    session
}

fn params(session: &Session) -> Value {
    session.snapshot()["model"]["param"].clone()
}

/// The ids of a list of parameters and groups, in order.
fn ids(list: &Value) -> Vec<u64> {
    list.as_array().unwrap().iter().map(|item| item["uuid"].as_u64().or(item["groupUUID"].as_u64()).unwrap()).collect()
}

fn outer(session: &Session) -> Value {
    params(session)[0]["children"].clone()
}

fn inner(session: &Session) -> Value {
    outer(session)[1]["children"].clone()
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
fn one_undo_step(session: &mut Session, value: Value) -> Value {
    let before = params(session);
    let count = history_count(session);
    let reply = result(&request(session, value)).clone();
    assert_ne!(params(session), before, "request changed nothing");
    assert_eq!(history_count(session), count + 1);
    let after = params(session);
    result(&request(session, json!({"op": "undo"})));
    assert_eq!(params(session), before);
    result(&request(session, json!({"op": "redo"})));
    assert_eq!(params(session), after);
    reply
}

#[test]
fn field_edits_reach_a_parameter_in_a_group_inside_a_group() {
    let mut s = session();
    one_undo_step(&mut s, json!({"op": "parameter.set_defaults", "id": 41, "value": [0.5, 0.0]}));
    assert_eq!(inner(&s)[0]["defaults"], json!([0.5, 0.0]));

    one_undo_step(&mut s, json!({"op": "parameter.flip", "id": 41}));
    assert_eq!(inner(&s)[0]["bindings"][0]["values"], json!([[3.0], [2.0], [1.0]]));

    one_undo_step(&mut s, json!({"op": "parameter.mirror", "id": 41}));
    assert_eq!(inner(&s)[0]["bindings"][0]["values"], json!([[-3.0], [-2.0], [-1.0]]));

    one_undo_step(&mut s, json!({"op": "parameter.link", "id": 41, "target": 42}));
    assert_eq!(inner(&s)[0]["bindings"].as_array().unwrap().len(), 3);
    one_undo_step(&mut s, json!({"op": "parameter.unlink", "id": 41, "target": 42}));
    assert_eq!(inner(&s)[0]["bindings"].as_array().unwrap().len(), 2);
    assert_eq!(ids(&params(&s)), [30, 20]);
    assert_eq!(ids(&outer(&s)), [31, 40, 32]);
    assert_eq!(ids(&inner(&s)), [41, 42]);
}

#[test]
fn a_copy_and_a_split_land_beside_the_parameter_in_the_inner_group() {
    let mut s = session();
    let copy = one_undo_step(&mut s, json!({"op": "parameter.duplicate", "id": 41}));
    assert_eq!(copy["name"], "Blink (Copy)");
    let copy_id = copy["uuid"].as_u64().unwrap();
    assert_eq!(ids(&inner(&s)), [41, 42, copy_id]);

    let split = one_undo_step(&mut s, json!({"op": "parameter.split", "id": 41, "nodes": [5]}));
    let split_id = split["uuid"].as_u64().unwrap();
    assert_eq!(ids(&inner(&s)), [41, split_id, 42, copy_id]);
    assert_eq!(inner(&s)[0]["bindings"].as_array().unwrap().len(), 1);
    assert_eq!(inner(&s)[1]["bindings"][0]["node"], 5);
    assert_eq!(ids(&outer(&s)), [31, 40, 32]);
}

#[test]
fn to_2d_moves_the_parameter_to_the_end_of_the_inner_group() {
    let mut s = session();
    one_undo_step(&mut s, json!({"op": "parameter.convert_to_2d", "id": 41}));
    assert_eq!(ids(&inner(&s)), [42, 41]);
    assert_eq!(inner(&s)[1]["is_vec2"], true);
    assert_eq!(ids(&outer(&s)), [31, 40, 32]);
}

#[test]
fn a_parameter_moves_out_of_into_and_inside_the_inner_group() {
    let mut s = session();
    one_undo_step(&mut s, json!({"op": "parameter.move", "id": 41, "group": null}));
    assert_eq!(ids(&params(&s)), [30, 20, 41]);
    assert_eq!(params(&s)[2].get("parentUUID"), None);
    assert_eq!(ids(&inner(&s)), [42]);

    one_undo_step(&mut s, json!({"op": "parameter.move", "id": 20, "group": 40}));
    assert_eq!(ids(&params(&s)), [30, 41]);
    assert_eq!(ids(&inner(&s)), [42, 20]);
    assert_eq!(inner(&s)[1]["parentUUID"], 40);

    one_undo_step(&mut s, json!({"op": "parameter.move", "id": 20, "group": 40, "index": 0}));
    assert_eq!(ids(&inner(&s)), [20, 42]);

    one_undo_step(&mut s, json!({"op": "parameter.move", "id": 20, "group": 30, "index": 1}));
    assert_eq!(ids(&outer(&s)), [31, 20, 40, 32]);
    assert_eq!(outer(&s)[1]["parentUUID"], 30);

    let message = rejected(&mut s, json!({"op": "parameter.move", "id": 42, "group": 40, "index": 2}));
    assert_eq!(message, "index 2 is outside the target list of 0 entries");
}

#[test]
fn an_inner_group_takes_a_colour_and_releases_its_parameters_into_the_outer_group() {
    let mut s = session();
    one_undo_step(&mut s, json!({"op": "parameter.group.set_color", "id": 40, "color": [0.5, 0.25, 1.0]}));
    assert_eq!(outer(&s)[1]["color"], json!([0.5, 0.25, 1.0]));
    assert_eq!(params(&s)[0]["color"], json!([0.15, 0.15, 0.15]));

    let reply = one_undo_step(&mut s, json!({"op": "parameter.group.delete", "id": 40}));
    assert_eq!(ids(&reply["released"]), [41, 42]);
    assert_eq!(ids(&params(&s)), [30, 20]);
    assert_eq!(ids(&outer(&s)), [31, 41, 42, 32]);
    assert_eq!(outer(&s)[1]["parentUUID"], 30);
    assert_eq!(outer(&s)[2]["parentUUID"], 30);

    // A top-level group still releases to the top level, at its own position and without a parent.
    one_undo_step(&mut s, json!({"op": "parameter.group.delete", "id": 30}));
    assert_eq!(ids(&params(&s)), [31, 41, 42, 32, 20]);
    assert_eq!(params(&s)[1].get("parentUUID"), None);
}

#[test]
fn a_group_or_a_missing_id_is_refused_by_name_at_any_depth() {
    let mut s = session();
    for op in ["parameter.flip", "parameter.duplicate", "parameter.convert_to_2d"] {
        assert_eq!(rejected(&mut s, json!({"op": op, "id": 40})), "parameter 40 refers to a parameter group");
        assert_eq!(rejected(&mut s, json!({"op": op, "id": 30})), "parameter 30 refers to a parameter group");
        assert_eq!(rejected(&mut s, json!({"op": op, "id": 99})), "parameter 99 was not found");
    }
    for request in [
        json!({"op": "parameter.move", "id": 20, "group": 99}),
        json!({"op": "parameter.move", "id": 20, "group": 41}),
        json!({"op": "parameter.group.delete", "id": 41}),
        json!({"op": "parameter.group.set_color", "id": 99, "color": [0.0, 0.0, 0.0]}),
    ] {
        let id = request["group"].as_u64().or(request["id"].as_u64()).unwrap();
        assert_eq!(rejected(&mut s, request), format!("parameter group {id} was not found"));
    }
}

#[test]
fn the_tools_reach_the_inner_group_through_mcp() {
    let mut s = session();
    let mut call = |name: &str, arguments: Value| {
        let response = s
            .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": name, "arguments": arguments}}))
            .unwrap();
        assert_eq!(response["result"]["isError"], false, "{name}: {response}");
    };
    call("parameter_flip", json!({"id": 41}));
    call("parameter_duplicate", json!({"id": 41, "name": "Blink 2"}));
    call("parameter_move", json!({"id": 42, "group": null}));
    call("parameter_group_delete", json!({"id": 40}));
    assert_eq!(ids(&params(&s)), [30, 20, 42]);
    let names: Vec<Value> = outer(&s).as_array().unwrap().iter().map(|item| item["name"].clone()).collect();
    assert_eq!(names, ["Outer", "Blink", "Blink 2", "After"]);
}
