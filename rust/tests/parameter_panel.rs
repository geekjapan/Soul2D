use serde_json::{json, Value};
use soul2d_core::Session;

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn node(uuid: u64, name: &str, children: Value) -> Value {
    let mut entry = json!({
        "uuid": uuid, "name": name, "type": "Node", "enabled": true, "zsort": 0.0,
        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
        "lockToRoot": false, "pinToMesh": false,
    });
    if !children.is_null() {
        entry["children"] = children;
    }
    entry
}

fn plain(id: u64, name: &str) -> Value {
    json!({
        "uuid": id, "name": name, "is_vec2": false,
        "min": [-1.0, 0.0], "max": [1.0, 1.0], "defaults": [0.0, 0.0],
        "axis_points": [[0.0, 0.5, 1.0], [0.0]], "merge_mode": "Additive", "bindings": [],
    })
}

fn binding(node: u64, channel: &str, values: Value, flags: Value) -> Value {
    json!({"node": node, "param_name": channel, "interpolate_mode": "Linear", "values": values, "isSet": flags})
}

fn session_with(params: Value) -> Session {
    let model = json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": node(1, "Root", json!([node(4, "Left", Value::Null), node(5, "Right", Value::Null)])),
        "param": params,
        "animations": {},
    });
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new", "model": model})));
    session
}

fn params(session: &Session) -> Value {
    session.snapshot()["model"]["param"].clone()
}

fn history_count(session: &mut Session) -> u64 {
    let response = request(session, json!({"op": "history.summary"}));
    result(&response)["count"].as_u64().unwrap()
}

/// Run an invalid request: it must fail and leave snapshot and history untouched.
fn rejected(session: &mut Session, value: Value) -> String {
    let before = session.snapshot();
    let count = history_count(session);
    let response = request(session, value);
    assert_eq!(response["succeeded"], false, "expected failure: {response}");
    assert_eq!(session.snapshot(), before);
    assert_eq!(history_count(session), count);
    response["message"].as_str().or(response["error"].as_str()).unwrap_or("").to_owned()
}

/// Run a valid request: it must add exactly one undo step, and undo must restore the old state.
fn one_undo_step(session: &mut Session, value: Value) -> Value {
    let before = params(session);
    let count = history_count(session);
    let reply = result(&request(session, value)).clone();
    assert_ne!(params(session), before, "request changed nothing");
    assert_eq!(history_count(session), count + 1);
    result(&request(session, json!({"op": "undo"})));
    assert_eq!(params(session), before);
    result(&request(session, json!({"op": "redo"})));
    reply
}

fn fixture() -> Session {
    let mut eyes = plain(17, "Eyes");
    eyes["bindings"] = json!([binding(4, "transform.t.x", json!([[1.0], [2.0], [3.0]]),
        json!([[true], [true], [true]]))]);
    session_with(json!([
        eyes,
        {"groupUUID": 30, "name": "Face", "color": [0.15, 0.15, 0.15], "children": [plain(31, "Blink")]},
        plain(18, "Mouth"),
    ]))
}

fn ids(list: &Value) -> Vec<u64> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|item| item["uuid"].as_u64().or(item["groupUUID"].as_u64()).unwrap())
        .collect()
}

#[test]
fn duplicate_copies_bindings_beside_the_source_with_a_unique_name() {
    let mut session = fixture();
    let copy = one_undo_step(&mut session, json!({"op": "parameter.duplicate", "id": 17}));
    assert_eq!(copy["uuid"], 2);
    assert_eq!(copy["name"], "Eyes (Copy)");
    assert_eq!(copy["bindings"], params(&session)[0]["bindings"]);
    assert_eq!(ids(&params(&session)), vec![17, 30, 18, 2]);

    one_undo_step(&mut session, json!({"op": "parameter.duplicate", "id": 31}));
    assert_eq!(ids(&params(&session)[1]["children"]), vec![31, 3]);
    assert_eq!(params(&session)[1]["children"][1]["name"], "Blink (Copy)");

    let second = one_undo_step(&mut session, json!({"op": "parameter.duplicate", "id": 17}));
    assert_eq!(second["uuid"], 6);
    assert_eq!(second["name"], "Eyes (Copy 2)");

    rejected(&mut session, json!({"op": "parameter.duplicate", "id": 30}));
    rejected(&mut session, json!({"op": "parameter.duplicate", "id": 999}));
    rejected(&mut session, json!({"op": "parameter.duplicate", "id": 17, "name": "Mouth"}));
    rejected(&mut session, json!({"op": "parameter.duplicate", "id": 17, "name": 5}));
    rejected(&mut session, json!({"op": "parameter.duplicate"}));
}

#[test]
fn group_create_rename_color_and_delete_round_trip() {
    let mut session = fixture();
    let group = one_undo_step(&mut session, json!({"op": "parameter.group.create"}));
    assert_eq!(group, json!({
        "groupUUID": 2, "name": "New Parameter Group", "color": [0.15, 0.15, 0.15], "children": []
    }));
    let again = one_undo_step(&mut session, json!({"op": "parameter.group.create"}));
    assert_eq!(again["name"], "New Parameter Group 2");
    assert_eq!(again["groupUUID"], 3);

    // The existing rename operation already renames groups.
    one_undo_step(&mut session, json!({"op": "parameter.rename", "id": 2, "name": "Eyes group"}));
    assert_eq!(params(&session)[3]["name"], "Eyes group");

    one_undo_step(&mut session, json!({"op": "parameter.group.set_color", "id": 30, "color": [0.25, 0.15, 0.25]}));
    assert_eq!(params(&session)[1]["color"], json!([0.25, 0.15, 0.25]));

    // Deleting a group releases its children at the group's position.
    let released = one_undo_step(&mut session, json!({"op": "parameter.group.delete", "id": 30}));
    assert_eq!(released["released"][0]["uuid"], 31);
    assert_eq!(ids(&params(&session)), vec![17, 31, 18, 2, 3]);

    rejected(&mut session, json!({"op": "parameter.group.delete", "id": 17}));
    rejected(&mut session, json!({"op": "parameter.group.delete", "id": 999}));
    rejected(&mut session, json!({"op": "parameter.group.create", "name": "Mouth"}));
    rejected(&mut session, json!({"op": "parameter.group.set_color", "id": 17, "color": [0, 0, 0]}));
    rejected(&mut session, json!({"op": "parameter.group.set_color", "id": 2, "color": [0, 0]}));
}

#[test]
fn group_create_with_members_moves_them_in_order() {
    let mut session = fixture();
    let group = one_undo_step(
        &mut session,
        json!({"op": "parameter.group.create", "name": "Pair", "members": [18, 17]}),
    );
    assert_eq!(group["name"], "Pair");
    assert_eq!(ids(&group["children"]), vec![18, 17]);
    assert_eq!(group["children"][0]["parentUUID"], 2);
    assert_eq!(ids(&params(&session)), vec![30, 2]);

    // A member that is already inside another group leaves that group.
    let mut session = fixture();
    one_undo_step(&mut session, json!({"op": "parameter.group.create", "members": [31]}));
    assert_eq!(params(&session)[1]["children"], json!([]));

    rejected(&mut session, json!({"op": "parameter.group.create", "members": [17, 17]}));
    rejected(&mut session, json!({"op": "parameter.group.create", "members": [30]}));
    rejected(&mut session, json!({"op": "parameter.group.create", "members": [999]}));
    rejected(&mut session, json!({"op": "parameter.group.create", "members": "17"}));
}

#[test]
fn move_changes_group_membership_and_order() {
    let mut session = fixture();
    one_undo_step(&mut session, json!({"op": "parameter.move", "id": 17, "group": 30}));
    assert_eq!(ids(&params(&session)), vec![30, 18]);
    assert_eq!(ids(&params(&session)[0]["children"]), vec![31, 17]);
    assert_eq!(params(&session)[0]["children"][1]["parentUUID"], 30);

    one_undo_step(&mut session, json!({"op": "parameter.move", "id": 18, "group": 30, "index": 0}));
    assert_eq!(ids(&params(&session)[0]["children"]), vec![18, 31, 17]);

    // Reordering inside one list: 17 goes to the front of the group.
    one_undo_step(&mut session, json!({"op": "parameter.move", "id": 17, "group": 30, "index": 0}));
    assert_eq!(ids(&params(&session)[0]["children"]), vec![17, 18, 31]);

    // Moving out removes the stale parent link.
    one_undo_step(&mut session, json!({"op": "parameter.move", "id": 17, "group": null, "index": 0}));
    assert_eq!(ids(&params(&session)), vec![17, 30]);
    assert!(params(&session)[0].get("parentUUID").is_none());

    rejected(&mut session, json!({"op": "parameter.move", "id": 30, "group": null}));
    rejected(&mut session, json!({"op": "parameter.move", "id": 17, "group": 18}));
    rejected(&mut session, json!({"op": "parameter.move", "id": 17, "group": 999}));
    rejected(&mut session, json!({"op": "parameter.move", "id": 17, "index": 9}));
    rejected(&mut session, json!({"op": "parameter.move", "id": 17, "index": -1}));
    rejected(&mut session, json!({"op": "parameter.move", "id": 999}));
}

#[test]
fn flip_reverses_axis_points_and_key_grids() {
    let mut one_d = plain(7, "Wink");
    one_d["axis_points"] = json!([[0.0, 0.25, 1.0], [0.0]]);
    one_d["bindings"] = json!([binding(4, "transform.t.x", json!([[10.0], [15.0], [30.0]]),
        json!([[true], [false], [true]]))]);
    let mut two_d = plain(8, "Look");
    two_d["is_vec2"] = json!(true);
    two_d["min"] = json!([-1.0, -1.0]);
    two_d["axis_points"] = json!([[0.0, 1.0], [0.0, 0.25, 1.0]]);
    two_d["bindings"] = json!([binding(4, "transform.t.y", json!([[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]),
        json!([[true, true, true], [true, true, true]]))]);
    let mut session = session_with(json!([one_d, two_d]));

    one_undo_step(&mut session, json!({"op": "parameter.flip", "id": 7}));
    let flipped = &params(&session)[0];
    assert_eq!(flipped["axis_points"], json!([[0.0, 0.75, 1.0], [0.0]]));
    assert_eq!(flipped["bindings"][0]["values"], json!([[30.0], [15.0], [10.0]]));
    assert_eq!(flipped["bindings"][0]["isSet"], json!([[true], [false], [true]]));

    one_undo_step(&mut session, json!({"op": "parameter.flip", "id": 8, "axis": 1}));
    let flipped = &params(&session)[1];
    assert_eq!(flipped["axis_points"], json!([[0.0, 1.0], [0.0, 0.75, 1.0]]));
    assert_eq!(flipped["bindings"][0]["values"], json!([[3.0, 2.0, 1.0], [6.0, 5.0, 4.0]]));

    one_undo_step(&mut session, json!({"op": "parameter.flip", "id": 8, "axis": 0}));
    assert_eq!(params(&session)[1]["bindings"][0]["values"], json!([[6.0, 5.0, 4.0], [3.0, 2.0, 1.0]]));

    rejected(&mut session, json!({"op": "parameter.flip", "id": 7, "axis": 1}));
    rejected(&mut session, json!({"op": "parameter.flip", "id": 7, "axis": 2}));
    rejected(&mut session, json!({"op": "parameter.flip", "id": 999}));
}

#[test]
fn mirror_negates_set_keys_by_channel_and_reinterpolates() {
    let mut eyes = plain(7, "Eyes");
    eyes["bindings"] = json!([
        binding(4, "transform.t.x", json!([[1.0], [2.0], [2.0]]), json!([[true], [true], [false]])),
        binding(4, "transform.t.y", json!([[5.0], [6.0], [6.0]]), json!([[true], [true], [false]])),
        binding(4, "transform.r.z", json!([[1.0], [1.0], [1.0]]), json!([[true], [false], [false]])),
        binding(5, "deform", json!([[[[1.0, 2.0], [3.0, 4.0]]], [[[5.0, 6.0], [7.0, 8.0]]], [[[5.0, 6.0], [7.0, 8.0]]]]),
            json!([[true], [true], [false]])),
    ]);
    let mut session = session_with(json!([eyes]));
    one_undo_step(&mut session, json!({"op": "parameter.mirror", "id": 7, "axis": 0}));
    let bindings = params(&session)[0]["bindings"].clone();
    assert_eq!(bindings[0]["values"], json!([[-1.0], [-2.0], [-2.0]]));
    assert_eq!(bindings[0]["isSet"], json!([[true], [true], [false]]));
    assert_eq!(bindings[1]["values"], json!([[5.0], [6.0], [6.0]]));
    assert_eq!(bindings[2]["values"], json!([[-1.0], [-1.0], [-1.0]]));
    assert_eq!(bindings[3]["values"][0][0], json!([[-1.0, 2.0], [-3.0, 4.0]]));
    assert_eq!(bindings[3]["values"][1][0], json!([[-5.0, 6.0], [-7.0, 8.0]]));
    assert_eq!(bindings[3]["values"][2][0], json!([[-5.0, 6.0], [-7.0, 8.0]]));

    rejected(&mut session, json!({"op": "parameter.mirror", "id": 7, "axis": 1}));
    rejected(&mut session, json!({"op": "parameter.mirror", "id": 999}));
}

#[test]
fn mirror_vertically_on_a_2d_parameter_negates_the_y_channels() {
    let mut look = plain(8, "Look");
    look["is_vec2"] = json!(true);
    look["min"] = json!([-1.0, -1.0]);
    look["axis_points"] = json!([[0.0, 1.0], [0.0, 1.0]]);
    look["bindings"] = json!([
        binding(4, "transform.t.x", json!([[1.0, 1.0], [1.0, 1.0]]), json!([[true, false], [false, false]])),
        binding(4, "transform.t.y", json!([[2.0, 2.0], [2.0, 2.0]]), json!([[true, false], [false, false]])),
    ]);
    let mut session = session_with(json!([look]));
    one_undo_step(&mut session, json!({"op": "parameter.mirror", "id": 8, "axis": 1}));
    let bindings = params(&session)[0]["bindings"].clone();
    assert_eq!(bindings[0]["values"], json!([[1.0, 1.0], [1.0, 1.0]]));
    assert_eq!(bindings[1]["values"], json!([[-2.0, -2.0], [-2.0, -2.0]]));
}

#[test]
fn set_defaults_uses_the_session_value_or_an_explicit_one() {
    let mut session = session_with(json!([plain(7, "Eyes")]));
    result(&request(&mut session, json!({"op": "value.set", "id": 7, "value": [0.5, 0.0]})));
    let reply = one_undo_step(&mut session, json!({"op": "parameter.set_defaults", "id": 7}));
    assert_eq!(reply, json!({"id": 7, "defaults": [0.5, 0.0]}));
    assert_eq!(params(&session)[0]["defaults"], json!([0.5, 0.0]));

    one_undo_step(&mut session, json!({"op": "parameter.set_defaults", "id": 7, "value": [-1.0, 0.0]}));
    assert_eq!(params(&session)[0]["defaults"], json!([-1.0, 0.0]));

    rejected(&mut session, json!({"op": "parameter.set_defaults", "id": 7, "value": [5.0, 0.0]}));
    rejected(&mut session, json!({"op": "parameter.set_defaults", "id": 7, "value": [0.0]}));
    rejected(&mut session, json!({"op": "parameter.set_defaults", "id": 999}));
}

#[test]
fn set_all_defaults_takes_every_session_value_in_one_undo_step() {
    let mut session = session_with(json!([
        plain(7, "Eyes"),
        {"groupUUID": 30, "name": "Face", "color": [0.15, 0.15, 0.15], "children": [
            plain(31, "Blink"),
            // A document may hold a group inside a group; its parameters have session values like any other.
            {"groupUUID": 40, "name": "Mouth", "color": [0.15, 0.15, 0.15], "children": [plain(41, "Smile")]},
        ]},
        plain(8, "Brow"),
    ]));
    result(&request(&mut session, json!({"op": "value.set", "id": 7, "value": [0.5, 0.0]})));
    result(&request(&mut session, json!({"op": "value.set", "id": 31, "value": [-1.0, 0.0]})));
    result(&request(&mut session, json!({"op": "value.set", "id": 41, "value": [1.0, 0.0]})));
    let reply = one_undo_step(&mut session, json!({"op": "parameter.set_all_defaults"}));
    assert_eq!(reply, json!({"parameters": 4, "changed": 3}));
    let list = params(&session);
    assert_eq!(list[0]["defaults"], json!([0.5, 0.0]));
    assert_eq!(list[1]["children"][0]["defaults"], json!([-1.0, 0.0]));
    assert_eq!(list[1]["children"][1]["children"][0]["defaults"], json!([1.0, 0.0]));
    assert_eq!(list[2]["defaults"], json!([0.0, 0.0]));
    rejected(&mut session, json!({"op": "parameter.set_defaults", "id": 40}));

    // The MCP tool takes no argument; a second call finds nothing to change and adds no undo step.
    let count = history_count(&mut session);
    let mut mcp = |method: &str, params: Value| {
        session.handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})).unwrap()["result"].clone()
    };
    let scoped = mcp("tools/call", json!({"name": "parameter_set_all_defaults", "arguments": {"id": 8}}));
    assert_eq!(scoped["content"][0]["text"], "unknown argument(s) for parameter_set_all_defaults: id");
    let again = mcp("tools/call", json!({"name": "parameter_set_all_defaults", "arguments": {}}));
    assert_eq!(again["isError"], false, "{again}");
    assert_eq!(again["structuredContent"], json!({"parameters": 4, "changed": 0}));
    let read = mcp("resources/read", json!({"uri": "resource://vsoul/parameters/31"}));
    let record: Value = serde_json::from_str(read["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(record["defaults"], json!([-1.0, 0.0]));
    assert_eq!(history_count(&mut session), count);
}

#[test]
fn split_moves_the_bindings_of_chosen_nodes_into_a_new_parameter() {
    let mut eyes = plain(7, "Eyes");
    let tx4 = binding(4, "transform.t.x", json!([[1.0], [1.0], [1.0]]), json!([[true], [false], [false]]));
    let ty5 = binding(5, "transform.t.y", json!([[2.0], [2.0], [2.0]]), json!([[true], [false], [false]]));
    let ty4 = binding(4, "transform.t.y", json!([[3.0], [3.0], [3.0]]), json!([[true], [false], [false]]));
    eyes["bindings"] = json!([tx4.clone(), ty5.clone(), ty4.clone()]);
    let mut session = session_with(json!([eyes, plain(18, "Mouth")]));

    let split = one_undo_step(&mut session, json!({"op": "parameter.split", "id": 7, "nodes": [4]}));
    assert_eq!(split["uuid"], 2);
    assert_eq!(split["name"], "Eyes (Split)");
    assert_eq!(split["min"], json!([-1.0, 0.0]));
    assert_eq!(split["axis_points"], json!([[0.0, 0.5, 1.0], [0.0]]));
    assert_eq!(split["bindings"], json!([tx4, ty4]));
    assert_eq!(ids(&params(&session)), vec![7, 2, 18]);
    assert_eq!(params(&session)[0]["bindings"], json!([ty5]));
    // The new parameter is a normal one: the session tracks its value.
    assert_eq!(session.snapshot()["values"]["2"], json!([0.0, 0.0]));

    rejected(&mut session, json!({"op": "parameter.split", "id": 7, "nodes": [4]}));
    rejected(&mut session, json!({"op": "parameter.split", "id": 18, "nodes": [4]}));
    rejected(&mut session, json!({"op": "parameter.split", "id": 7, "nodes": []}));
    rejected(&mut session, json!({"op": "parameter.split", "id": 7, "nodes": [0]}));
    rejected(&mut session, json!({"op": "parameter.split", "id": 7}));
}

#[test]
fn create_mouth_adds_the_five_point_2d_parameter() {
    let mut session = session_with(json!([plain(7, "Eyes")]));
    let mouth = one_undo_step(&mut session, json!({"op": "parameter.create_mouth"}));
    assert_eq!(mouth["name"], "Mouth #1\u{0}");
    assert_eq!(mouth["is_vec2"], true);
    assert_eq!(mouth["min"], json!([-1.0, 0.0]));
    assert_eq!(mouth["max"], json!([1.0, 1.0]));
    assert_eq!(mouth["axis_points"], json!([[0.0, 0.25, 0.5, 0.6, 1.0], [0.0, 0.3, 0.5, 0.6, 1.0]]));
    assert_eq!(session.snapshot()["values"][mouth["uuid"].to_string()], json!([0.0, 0.0]));
    rejected(&mut session, json!({"op": "parameter.create_mouth", "name": "Eyes"}));
    rejected(&mut session, json!({"op": "parameter.create_mouth", "name": 3}));
}

fn named<'a>(list: &'a Value, name: &str) -> &'a Value {
    list.as_array().unwrap().iter().find(|item| item["name"] == name).unwrap_or(&Value::Null)
}

#[test]
fn create_standard_adds_the_missing_parameters_of_the_cubism_list() {
    // One standard name is taken already, with a range of its own.
    let mut mine = plain(7, "ParamAngleX");
    mine["min"] = json!([-45.0, 0.0]);
    mine["max"] = json!([45.0, 1.0]);
    let mut session = session_with(json!([mine.clone()]));

    let reply = one_undo_step(&mut session, json!({"op": "parameter.create_standard"}));
    assert_eq!(reply["existing"], json!(["ParamAngleX"]));
    let created = reply["created"].as_array().unwrap();
    assert_eq!(created.len(), 26, "the 27 rows the list always asks for, less the one that exists");
    assert_eq!(created[0], "ParamAngleY");
    assert_eq!(created[25], "ParamHairBack");
    let list = params(&session);
    assert_eq!(list.as_array().unwrap().len(), 27);
    assert_eq!(list[0], mine, "a parameter that exists is left as it is");
    // An eye starts open; a symmetric range gets a key at its middle.
    let eye = named(&list, "ParamEyeLOpen");
    assert_eq!((&eye["min"], &eye["max"], &eye["defaults"]), (&json!([0.0, 0.0]), &json!([1.0, 1.0]), &json!([1.0, 0.0])));
    assert_eq!(eye["axis_points"], json!([[0.0, 1.0], [0.0]]));
    assert_eq!(eye["is_vec2"], false);
    assert_eq!(session.snapshot()["values"][eye["uuid"].to_string()], json!([1.0, 0.0]));
    let body = named(&list, "ParamBodyAngleZ");
    assert_eq!((&body["min"], &body["max"], &body["defaults"]), (&json!([-10.0, 0.0]), &json!([10.0, 1.0]), &json!([0.0, 0.0])));
    assert_eq!(body["axis_points"], json!([[0.0, 0.5, 1.0], [0.0]]));
    assert_eq!(named(&list, "ParamArmLA"), &Value::Null, "a row marked as needed only sometimes waits for all");

    // A second call finds nothing missing and adds no undo step.
    let count = history_count(&mut session);
    let again = result(&request(&mut session, json!({"op": "parameter.create_standard"}))).clone();
    assert_eq!(again["created"], json!([]));
    assert_eq!(again["existing"].as_array().unwrap().len(), 27);
    assert_eq!(history_count(&mut session), count);

    // `all` adds the 13 remaining rows, the two of the one row that holds two IDs among them.
    let rest = one_undo_step(&mut session, json!({"op": "parameter.create_standard", "all": true}));
    assert_eq!(rest["created"].as_array().unwrap().len(), 13);
    assert_eq!(rest["existing"].as_array().unwrap().len(), 27);
    let list = params(&session);
    assert_eq!(list.as_array().unwrap().len(), 40);
    let arm = named(&list, "ParamArmLA");
    assert_eq!((&arm["min"], &arm["max"]), (&json!([-30.0, 0.0]), &json!([30.0, 1.0])));
    assert_ne!(named(&list, "ParamBustX"), &Value::Null);
    assert_ne!(named(&list, "ParamBustY"), &Value::Null);
    let mut uuids = ids(&list);
    uuids.sort_unstable();
    uuids.dedup();
    assert_eq!(uuids.len(), 40, "every parameter has its own id");

    rejected(&mut session, json!({"op": "parameter.create_standard", "all": "yes"}));

    // A group that holds a standard name blocks that parameter. Saying it "exists" would be wrong, so the call is refused.
    let mut session = session_with(json!([{"groupUUID": 30, "name": "ParamBreath", "children": [plain(31, "Blink")]}]));
    let message = rejected(&mut session, json!({"op": "parameter.create_standard"}));
    assert_eq!(message, "ParamBreath is the name of a parameter group, so the parameter cannot be added");
}

#[test]
fn create_standard_finds_a_taken_name_inside_a_group_and_runs_through_mcp() {
    let mut session = session_with(json!([
        {"groupUUID": 30, "name": "Face", "color": [0.15, 0.15, 0.15], "children": [plain(31, "ParamBreath")]},
    ]));
    let reply = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "parameter_create_standard", "arguments": {}}}))
        .unwrap();
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    assert_eq!(reply["result"]["structuredContent"]["existing"], json!(["ParamBreath"]));
    assert_eq!(reply["result"]["structuredContent"]["created"].as_array().unwrap().len(), 26);
    // The group is untouched and the new parameters stand beside it; a resource read shows one of them.
    let list = params(&session);
    assert_eq!(list.as_array().unwrap().len(), 27);
    assert_eq!(list[0]["children"].as_array().unwrap().len(), 1);
    let mouth = named(&list, "ParamMouthOpenY")["uuid"].as_u64().unwrap();
    let read = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "resources/read", "params": {
            "uri": format!("resource://vsoul/parameters/{mouth}")}}))
        .unwrap();
    let record: Value = serde_json::from_str(read["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!((&record["name"], &record["max"]), (&json!("ParamMouthOpenY"), &json!([1.0, 1.0])));
    let refused = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "parameter_create_standard", "arguments": {"all": "yes"}}}))
        .unwrap();
    assert_eq!(refused["result"]["isError"], true);
}

fn bound_fixture() -> Session {
    let mut eyes = plain(7, "Eyes");
    eyes["bindings"] = json!([
        binding(4, "transform.t.x", json!([[0.0], [10.0], [4.0]]), json!([[true], [true], [true]])),
        binding(4, "transform.t.y", json!([[3.0], [3.0], [3.0]]), json!([[true], [true], [true]])),
        binding(5, "transform.s.x", json!([[2.0], [2.0], [2.0]]), json!([[true], [true], [true]])),
    ]);
    session_with(json!([eyes]))
}

#[test]
fn keypoint_actions_edit_one_key_of_the_selected_bindings() {
    let mut session = bound_fixture();
    let tx = json!({"node": 4, "param_name": "transform.t.x"});

    one_undo_step(&mut session, json!({
        "op": "binding.keypoint", "parameter": 7, "key": [1, 0], "action": "unset", "bindings": [tx]
    }));
    let bindings = params(&session)[0]["bindings"].clone();
    assert_eq!(bindings[0]["isSet"], json!([[true], [false], [true]]));
    assert_eq!(bindings[0]["values"], json!([[0.0], [2.0], [4.0]]));
    assert_eq!(bindings[1]["isSet"], json!([[true], [true], [true]]));

    // Reset sets the neutral value: 0 for translation, 1 for scale.
    let mut fresh = bound_fixture();
    one_undo_step(&mut fresh, json!({"op": "binding.keypoint", "parameter": 7, "key": [2, 0], "action": "reset"}));
    let bindings = params(&fresh)[0]["bindings"].clone();
    assert_eq!(bindings[0]["values"], json!([[0.0], [10.0], [0.0]]));
    assert_eq!(bindings[1]["values"], json!([[3.0], [3.0], [0.0]]));
    assert_eq!(bindings[2]["values"], json!([[2.0], [2.0], [1.0]]));

    let mut fresh = bound_fixture();
    one_undo_step(&mut fresh, json!({
        "op": "binding.keypoint", "parameter": 7, "key": [0, 0], "action": "invert",
        "bindings": [{"node": 4, "param_name": "transform.t.y"}]
    }));
    assert_eq!(params(&fresh)[0]["bindings"][1]["values"][0], json!([-3.0]));
    assert_eq!(params(&fresh)[0]["bindings"][0]["values"][0], json!([0.0]));

    // Horizontal mirroring negates t.x but keeps t.y and scale.
    let mut fresh = bound_fixture();
    one_undo_step(&mut fresh, json!({"op": "binding.keypoint", "parameter": 7, "key": [1, 0], "action": "mirror_horizontal"}));
    let bindings = params(&fresh)[0]["bindings"].clone();
    assert_eq!(bindings[0]["values"][1], json!([-10.0]));
    assert_eq!(bindings[1]["values"][1], json!([3.0]));
    assert_eq!(bindings[2]["values"][1], json!([2.0]));

    rejected(&mut session, json!({"op": "binding.keypoint", "parameter": 7, "key": [3, 0], "action": "unset"}));
    rejected(&mut session, json!({"op": "binding.keypoint", "parameter": 7, "key": [0, 1], "action": "unset"}));
    rejected(&mut session, json!({"op": "binding.keypoint", "parameter": 7, "key": [0, 0], "action": "mirror_vertical"}));
    rejected(&mut session, json!({"op": "binding.keypoint", "parameter": 7, "key": [0, 0], "action": "explode"}));
    rejected(&mut session, json!({"op": "binding.keypoint", "parameter": 7, "key": [0, 0], "action": "unset",
        "bindings": [{"node": 9, "param_name": "transform.t.x"}]}));
}

#[test]
fn binding_remove_and_interpolation_change_only_the_chosen_bindings() {
    let mut session = bound_fixture();
    let ty = json!({"node": 4, "param_name": "transform.t.y"});
    one_undo_step(&mut session, json!({
        "op": "binding.set_interpolation", "parameter": 7, "mode": "Cubic", "bindings": [ty.clone()]
    }));
    let modes: Vec<Value> = params(&session)[0]["bindings"]
        .as_array().unwrap().iter().map(|b| b["interpolate_mode"].clone()).collect();
    assert_eq!(modes, vec![json!("Linear"), json!("Cubic"), json!("Linear")]);

    let reply = one_undo_step(&mut session, json!({"op": "binding.remove", "parameter": 7, "bindings": [ty]}));
    assert_eq!(reply["removed"], 1);
    let remaining: Vec<Value> = params(&session)[0]["bindings"]
        .as_array().unwrap().iter().map(|b| b["param_name"].clone()).collect();
    assert_eq!(remaining, vec![json!("transform.t.x"), json!("transform.s.x")]);

    rejected(&mut session, json!({"op": "binding.remove", "parameter": 7, "bindings": []}));
    rejected(&mut session, json!({"op": "binding.remove", "parameter": 7, "bindings": [{"node": 4, "param_name": "zSort"}]}));
    rejected(&mut session, json!({"op": "binding.remove", "parameter": 7}));
    rejected(&mut session, json!({"op": "binding.set_interpolation", "parameter": 7, "mode": "Wavy",
        "bindings": [{"node": 4, "param_name": "transform.t.x"}]}));
}

#[test]
fn mcp_tools_reach_the_new_operations() {
    let mut session = fixture();
    let reply = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "parameter_group_create", "arguments": {"name": "Pair", "members": [18]}}}))
        .unwrap();
    assert_eq!(reply["result"]["isError"], false);
    assert_eq!(ids(&params(&session)), vec![17, 30, 2]);
    let reply = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "parameter_move", "arguments": {"id": 18, "group": null, "index": 0}}}))
        .unwrap();
    assert_eq!(reply["result"]["isError"], false);
    assert_eq!(ids(&params(&session)), vec![18, 17, 30, 2]);
    let reply = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "parameter_split", "arguments": {"id": 18, "nodes": [4]}}}))
        .unwrap();
    assert_eq!(reply["result"]["isError"], true);
}

#[test]
fn set_current_marks_an_interpolated_key_as_set_without_changing_its_value() {
    let mut session = bound_fixture();
    result(&request(&mut session, json!({
        "op": "binding.keypoint", "parameter": 7, "key": [1, 0], "action": "unset",
        "bindings": [{"node": 4, "param_name": "transform.t.x"}]
    })));
    let tx = &params(&session)[0]["bindings"][0];
    assert_eq!((tx["values"][1][0].clone(), tx["isSet"][1][0].clone()), (json!(2.0), json!(false)));
    let before_set = params(&session);
    one_undo_step(&mut session, json!({
        "op": "binding.keypoint", "parameter": 7, "key": [1, 0], "action": "set_current",
        "bindings": [{"node": 4, "param_name": "transform.t.x"}]
    }));
    let tx = &params(&session)[0]["bindings"][0];
    assert_eq!((tx["values"][1][0].clone(), tx["isSet"][1][0].clone()), (json!(2.0), json!(true)));
    assert_ne!(params(&session), before_set);

    // A deform key keeps its interpolated offsets when it becomes set.
    let mut eyes = plain(8, "Brow");
    eyes["bindings"] = json!([binding(5, "deform", json!([[[[1.0, 2.0]]], [[[1.0, 2.0]]], [[[1.0, 2.0]]]]),
        json!([[true], [false], [false]]))]);
    let mut session = session_with(json!([eyes]));
    one_undo_step(&mut session, json!({"op": "binding.keypoint", "parameter": 8, "key": [2, 0], "action": "set_current"}));
    let deform = &params(&session)[0]["bindings"][0];
    assert_eq!(deform["isSet"], json!([[true], [false], [true]]));
    assert_eq!(deform["values"][2][0], json!([[1.0, 2.0]]));
}

/// A 1D parameter over 0..4 whose X keys are 10, 20, 40 at 0, 2 and 4; node 4 moves by the key value.
fn loop_parameter(id: u64) -> Value {
    let mut wheel = plain(id, "Wheel");
    wheel["min"] = json!([0.0, 0.0]);
    wheel["max"] = json!([4.0, 1.0]);
    wheel["bindings"] = json!([binding(4, "transform.t.x", json!([[10.0], [20.0], [40.0]]),
        json!([[true], [true], [true]]))]);
    wheel
}

fn trans_x(session: &mut Session, request_values: Value) -> f64 {
    let mut ask = json!({"op": "evaluate"});
    if !request_values.is_null() {
        ask["values"] = request_values;
    }
    let evaluated = request(session, ask);
    result(&evaluated)["nodes"]["children"][0]["transform"]["trans"][0].as_f64().unwrap()
}

fn set_value(session: &mut Session, x: f64) -> Value {
    request(session, json!({"op": "value.set", "id": 7, "value": [x, 0.0]}))
}

#[test]
fn a_repeating_parameter_wraps_written_values_and_evaluates_on_a_circle() {
    let mut session = session_with(json!([loop_parameter(7)]));
    let reply = one_undo_step(&mut session, json!({"op": "parameter.set_repeat", "id": 7, "repeat": true}));
    assert_eq!(reply, json!({"id": 7, "repeat": true}));
    assert_eq!(params(&session)[0]["repeat"], json!(true));

    // The value at max is the same point as min, so the key at min answers; the reply and the session agree.
    let reply = set_value(&mut session, 4.0);
    assert_eq!(result(&reply)["value"], json!([0.0, 0.0]));
    assert_eq!(session.snapshot()["values"]["7"], json!([0.0, 0.0]));
    assert_eq!(trans_x(&mut session, Value::Null), 10.0);

    for (written, stored, evaluated) in [(12.0, 0.0, 10.0), (6.0, 2.0, 20.0), (-2.0, 2.0, 20.0), (-4.0, 0.0, 10.0)] {
        let reply = set_value(&mut session, written);
        assert_eq!(result(&reply)["value"], json!([stored, 0.0]), "value.set {written}");
        assert_eq!(session.snapshot()["values"]["7"], json!([stored, 0.0]));
        assert_eq!(trans_x(&mut session, Value::Null), evaluated, "evaluate after value.set {written}");
    }
    // Just below max still interpolates toward the key at max: 20 + 0.95 * (40 - 20).
    result(&set_value(&mut session, 3.9));
    let near_end = trans_x(&mut session, Value::Null);
    assert!((near_end - 39.0).abs() < 1e-9, "got {near_end}");

    // The `values` override of an evaluate request is wrapped too and leaves the session value alone.
    assert_eq!(trans_x(&mut session, json!({"7": [6.0, 0.0]})), 20.0);
    assert_eq!(session.snapshot()["values"]["7"], json!([3.9, 0.0]));

    // A default is wrapped like any written value.
    let reply = result(&request(&mut session, json!({"op": "parameter.set_defaults", "id": 7, "value": [9.0, 0.0]}))).clone();
    assert_eq!(reply, json!({"id": 7, "defaults": [1.0, 0.0]}));

    // A value that is not a finite number is still refused, and the Y axis keeps its range.
    rejected(&mut session, json!({"op": "value.set", "id": 7, "value": [null, 0.0]}));
    rejected(&mut session, json!({"op": "value.set", "id": 7, "value": [1.0, 3.0]}));
    rejected(&mut session, json!({"op": "parameter.set_defaults", "id": 7, "value": [1.0]}));
}

#[test]
fn repeat_refills_unset_scalar_and_deform_keys_without_changing_authored_keys() {
    let mut wheel = loop_parameter(7);
    let flags = json!([[true], [true], [false]]);
    wheel["bindings"] = json!([
        binding(4, "transform.t.x", json!([[10.0], [20.0], [20.0]]), flags.clone()),
        binding(5, "deform", json!([[[[10.0, 20.0]]], [[[20.0, 40.0]]], [[[20.0, 40.0]]]]), flags.clone())
    ]);
    let mut session = session_with(json!([wheel]));
    one_undo_step(&mut session, json!({"op": "parameter.set_repeat", "id": 7, "repeat": true}));
    let bindings = &params(&session)[0]["bindings"];
    assert_eq!(bindings[0]["values"], json!([[10.0], [20.0], [10.0]]));
    assert_eq!(bindings[1]["values"], json!([[[[10.0, 20.0]]], [[[20.0, 40.0]]], [[[10.0, 20.0]]]]));
    assert_eq!(bindings[0]["isSet"], flags);
    assert_eq!(bindings[1]["isSet"], flags);
    assert!((trans_x(&mut session, json!({"7": [3.9, 0.0]})) - 10.5).abs() < 1e-9);
    one_undo_step(&mut session, json!({"op": "parameter.set_repeat", "id": 7, "repeat": false}));
    assert_eq!(params(&session)[0]["bindings"][0]["values"], json!([[10.0], [20.0], [20.0]]));
    assert_eq!(params(&session)[0]["bindings"][1]["values"][2][0], json!([[20.0, 40.0]]));
}

#[test]
fn repeat_toggle_keeps_null_unset_scalar_flags() {
    let mut wheel = loop_parameter(7);
    wheel["bindings"][0]["values"] = json!([[10.0], [20.0], [20.0]]);
    wheel["bindings"][0]["isSet"] = json!([[true], [true], [null]]);
    let mut session = session_with(json!([wheel]));
    one_undo_step(&mut session, json!({"op": "parameter.set_repeat", "id": 7, "repeat": true}));
    let binding = &params(&session)[0]["bindings"][0];
    assert_eq!(binding["values"], json!([[10.0], [20.0], [10.0]]));
    assert_eq!(binding["isSet"], json!([[true], [true], [null]]));
}

#[test]
fn repeat_lazy_caches_cannot_be_expanded_past_the_limit_by_keypoint_edits() {
    let mut wheel = loop_parameter(7);
    wheel["axis_points"] = json!([(0..256).map(|x| x as f64 / 255.0).collect::<Vec<_>>(), [0.0]]);
    let mut deform = binding(5, "deform", json!(vec![json!([[]]); 256]), json!(vec![json!([false]); 256]));
    for (x, value) in [(0, 10.0), (128, 20.0)] {
        deform["values"][x][0] = json!(vec![[value, value * 2.0]; 2000]);
        deform["isSet"][x][0] = json!(true);
    }
    wheel["bindings"] = json!([deform]);
    let mut session = session_with(json!([wheel]));
    result(&request(&mut session, json!({"op": "parameter.set_repeat", "id": 7, "repeat": true})));
    assert_eq!(params(&session)[0]["bindings"][0]["values"][255][0], json!([]));
    assert!(rejected(&mut session, json!({"op": "binding.keypoint", "parameter": 7,
        "key": [0, 0], "action": "invert"})).contains("limit"));
    assert!(rejected(&mut session, json!({"op": "parameter.mirror", "id": 7, "axis": 0})).contains("limit"));
}

#[test]
fn a_keypoint_edit_of_a_large_binding_with_every_key_set_allocates_nothing_and_runs() {
    let mut wheel = loop_parameter(7);
    wheel["axis_points"] = json!([(0..64).map(|x| x as f64 / 63.0).collect::<Vec<_>>(), [0.0]]);
    // 64 keys of 8000 vertices are 1,024,000 stored values, over the limit on what an edit may allocate.
    wheel["bindings"] = json!([binding(5, "deform", json!(vec![json!([vec![[1.0, 2.0]; 8000]]); 64]),
        json!(vec![json!([true]); 64]))]);
    let mut session = session_with(json!([wheel]));
    result(&request(&mut session, json!({"op": "binding.keypoint", "parameter": 7, "key": [0, 0], "action": "invert"})));
    let values = &params(&session)[0]["bindings"][0]["values"];
    assert_eq!(values[0][0][0], json!([-1.0, -2.0]));
    assert_eq!(values[1][0][0], json!([1.0, 2.0]));
}

#[test]
fn repeat_key_edits_refill_the_seam_and_keep_interpolation_modes() {
    for mode in ["Linear", "Nearest", "Cubic"] {
        let mut wheel = loop_parameter(7);
        wheel["repeat"] = json!(true);
        wheel["bindings"][0]["interpolate_mode"] = json!(mode);
        wheel["bindings"][0]["extension"] = json!({"keep": true});
        let mut session = session_with(json!([wheel]));
        one_undo_step(&mut session, json!({"op": "binding.keypoint", "parameter": 7,
            "key": [2, 0], "action": "unset"}));
        let b = &params(&session)[0]["bindings"][0];
        assert_eq!(b["values"], json!([[10.0], [20.0], [10.0]]));
        assert_eq!(b["isSet"], json!([[true], [true], [false]]));
        assert_eq!(b["interpolate_mode"], mode);
        assert_eq!(b["extension"], json!({"keep": true}));
        if mode == "Linear" {
            one_undo_step(&mut session, json!({"op": "binding.set_trs", "parameter": 7,
                "nodes": [4], "key": [0, 0], "translation": [30.0, 0.0]}));
            assert_eq!(params(&session)[0]["bindings"][0]["values"], json!([[30.0], [20.0], [30.0]]));
        }
    }
}

#[test]
fn a_repeating_parameter_wraps_a_stored_value_outside_its_range() {
    // A hand-written document can carry a default outside the range; evaluation must still land inside it.
    for (default, starts_at, expected) in [(6.0, 2.0, 20.0), (-2.0, 2.0, 20.0), (4.0, 0.0, 10.0), (3.9, 3.9, 39.0)] {
        let mut wheel = loop_parameter(7);
        wheel["repeat"] = json!(true);
        wheel["defaults"] = json!([default, 0.0]);
        let mut session = session_with(json!([wheel]));
        let got = trans_x(&mut session, Value::Null);
        assert!((got - expected).abs() < 1e-9, "default {default}: got {got}");
        // The session starts where `value.set` would put that value, after a load and after a reset. The
        // document keeps the default as written.
        assert_eq!(session.snapshot()["values"]["7"], json!([starts_at, 0.0]), "default {default}");
        result(&set_value(&mut session, 1.0));
        let reset = request(&mut session, json!({"op": "values.reset"}));
        assert_eq!(result(&reset)["values"]["7"], json!([starts_at, 0.0]), "reset to default {default}");
        assert_eq!(params(&session)[0]["defaults"], json!([default, 0.0]));
    }
    // A 2D parameter ignores the flag: its value is clamped as before.
    let mut flat = loop_parameter(7);
    flat["is_vec2"] = json!(true);
    flat["axis_points"] = json!([[0.0, 0.5, 1.0], [0.0, 1.0]]);
    flat["bindings"] = json!([binding(4, "transform.t.x", json!([[10.0, 10.0], [20.0, 20.0], [40.0, 40.0]]),
        json!([[true, true], [true, true], [true, true]]))]);
    flat["repeat"] = json!(true);
    flat["defaults"] = json!([6.0, 0.0]);
    let mut session = session_with(json!([flat]));
    assert_eq!(trans_x(&mut session, Value::Null), 40.0);
}

#[test]
fn a_link_follows_a_repeating_driver_around_the_circle() {
    let mut driver = plain(20, "Look");
    driver["min"] = json!([0.0, 0.0]);
    driver["max"] = json!([4.0, 1.0]);
    driver["repeat"] = json!(true);
    driver["defaults"] = json!([6.0, 0.0]);
    let mut open = plain(21, "Open");
    open["min"] = json!([1.0, 0.0]);
    open["max"] = json!([3.0, 1.0]);
    open["defaults"] = json!([2.0, 0.0]);
    open["axis_points"] = json!([[0.0, 1.0], [0.0]]);
    open["bindings"] = json!([binding(4, "transform.t.x", json!([[0.0], [10.0]]), json!([[true], [true]]))]);
    // The link is written by hand: a commit would already wrap the stored default, and this checks evaluation.
    driver["bindings"] = json!([{"node": 21, "param_name": "X", "interpolate_mode": "Linear",
        "values": [[1.0], [2.0], [3.0]], "isSet": [[true], [true], [true]]}]);
    let mut session = session_with(json!([driver, open]));
    // Look = 6 is the middle of the loop, so Open = 2 and Left moves by (2 - 1) / 2 * 10 = 5.
    assert_eq!(trans_x(&mut session, Value::Null), 5.0);
}

#[test]
fn set_repeat_is_one_undo_step_and_refuses_what_cannot_repeat() {
    let mut session = fixture();
    let before = history_count(&mut session);
    one_undo_step(&mut session, json!({"op": "parameter.set_repeat", "id": 31, "repeat": true}));
    assert_eq!(params(&session)[1]["children"][0]["repeat"], json!(true));

    // The same value again commits nothing.
    result(&request(&mut session, json!({"op": "parameter.set_repeat", "id": 31, "repeat": true})));
    assert_eq!(history_count(&mut session), before + 1);

    // Turning it off removes the key instead of writing false.
    one_undo_step(&mut session, json!({"op": "parameter.set_repeat", "id": 31, "repeat": false}));
    assert!(params(&session)[1]["children"][0].get("repeat").is_none());
    let count = history_count(&mut session);
    result(&request(&mut session, json!({"op": "parameter.set_repeat", "id": 17, "repeat": false})));
    assert_eq!(history_count(&mut session), count);

    let mut flat = plain(8, "Pad");
    flat["is_vec2"] = json!(true);
    flat["axis_points"] = json!([[0.0, 1.0], [0.0, 1.0]]);
    let mut session = session_with(json!([flat, {"groupUUID": 30, "name": "Face", "children": []}, plain(9, "Roll")]));
    assert!(rejected(&mut session, json!({"op": "parameter.set_repeat", "id": 8, "repeat": true})).contains("2D"));
    assert!(rejected(&mut session, json!({"op": "parameter.set_repeat", "id": 30, "repeat": true})).contains("group"));
    rejected(&mut session, json!({"op": "parameter.set_repeat", "id": 999, "repeat": true}));
    rejected(&mut session, json!({"op": "parameter.set_repeat", "id": 9}));
    rejected(&mut session, json!({"op": "parameter.set_repeat", "id": 9, "repeat": "yes"}));
    rejected(&mut session, json!({"op": "parameter.set_repeat", "repeat": true}));
}

#[test]
fn a_range_wider_than_a_float_can_measure_is_refused() {
    // Evaluation divides by the width of the range. Past the largest float the width is not a number, and every value
    // would stand at the first key.
    let wide = json!({"op": "parameter.create", "dimensions": 1, "min": [-1e308, 0.0], "max": [1e308, 1.0]});
    let mut session = session_with(json!([loop_parameter(7)]));
    assert!(rejected(&mut session, wide).contains("wider than a float"));
    let axes = json!({"op": "parameter.set_axes", "id": 7, "min": [-1e308, 0.0], "max": [1e308, 1.0],
        "axis_points": [[0.0, 0.5, 1.0], [0.0]]});
    assert!(rejected(&mut session, axes).contains("wider than a float"));
    // The second axis counts as well, and so does a model that arrives with such a range.
    let tall = json!({"op": "parameter.create", "dimensions": 2, "min": [0.0, -1e308], "max": [1.0, 1e308]});
    assert!(rejected(&mut session, tall).contains("wider than a float"));
    let mut loaded = loop_parameter(7);
    loaded["min"] = json!([-1e308, 0.0]);
    loaded["max"] = json!([1e308, 1.0]);
    let nodes = node(1, "Root", json!([node(4, "Left", Value::Null), node(5, "Right", Value::Null)]));
    let model = json!({"meta": {"name": "fixture", "version": "1.0-alpha"}, "nodes": nodes, "param": [loaded],
        "animations": {}});
    assert!(rejected(&mut Session::new(), json!({"op": "new", "model": model})).contains("wider than a float"));
    // The widest range a float can measure still works.
    let widest = json!({"op": "parameter.create", "dimensions": 1, "min": [-8e307, 0.0], "max": [8e307, 1.0]});
    result(&request(&mut session, widest));
}

#[test]
fn a_document_with_a_repeat_flag_keeps_it_through_save_and_open() {
    let directory = std::env::temp_dir().join(format!("soul2d-repeat-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("wheel.inx");
    let mut session = session_with(json!([loop_parameter(7)]));
    result(&request(&mut session, json!({"op": "parameter.set_repeat", "id": 7, "repeat": true})));
    result(&request(&mut session, json!({"op": "binding.keypoint", "parameter": 7,
        "key": [2, 0], "action": "unset"})));
    result(&request(&mut session, json!({"op": "save", "path": path.to_str().unwrap()})));
    let mut reopened = Session::new();
    result(&request(&mut reopened, json!({"op": "open", "path": path.to_str().unwrap()})));
    assert_eq!(params(&reopened)[0]["repeat"], json!(true));
    assert_eq!(params(&reopened)[0]["bindings"][0]["values"], json!([[10.0], [20.0], [10.0]]));
    assert_eq!(params(&reopened)[0]["bindings"][0]["isSet"], json!([[true], [true], [false]]));
    assert_eq!(result(&set_value(&mut reopened, 6.0))["value"], json!([2.0, 0.0]));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_repeat_flag_that_is_not_a_boolean_is_refused() {
    let mut wheel = loop_parameter(7);
    wheel["repeat"] = json!("yes");
    let model = json!({"meta": {"name": "f", "version": "1.0-alpha"}, "nodes": node(1, "Root", Value::Null),
        "param": [wheel], "animations": {}});
    let mut session = Session::new();
    let response = request(&mut session, json!({"op": "new", "model": model}));
    assert_eq!(response["succeeded"], false, "{response}");
}

#[test]
fn duplicate_keeps_the_repeat_flag_and_convert_to_2d_drops_it() {
    let mut session = session_with(json!([loop_parameter(7)]));
    result(&request(&mut session, json!({"op": "parameter.set_repeat", "id": 7, "repeat": true})));
    let copy = one_undo_step(&mut session, json!({"op": "parameter.duplicate", "id": 7}));
    assert_eq!(copy["repeat"], json!(true));
    let converted = one_undo_step(&mut session, json!({"op": "parameter.convert_to_2d", "id": 7}));
    assert!(converted.get("repeat").is_none(), "{converted}");
    assert_eq!(converted["is_vec2"], json!(true));
}

#[test]
fn mcp_sets_repeat_and_the_parameter_resource_shows_it() {
    let mut session = session_with(json!([loop_parameter(7)]));
    let reply = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "parameter_set_repeat", "arguments": {"id": 7, "repeat": true}}}))
        .unwrap();
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    let read = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "resources/read", "params": {
            "uri": "resource://vsoul/parameters/7"}}))
        .unwrap();
    let record: Value = serde_json::from_str(read["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(record["repeat"], json!(true));
    let missing = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "parameter_set_repeat", "arguments": {"id": 7}}}))
        .unwrap();
    assert_eq!(missing["result"]["isError"], true);
}

#[test]
fn without_repeat_the_same_values_are_still_refused() {
    let mut session = session_with(json!([loop_parameter(7)]));
    for written in [4.5, 6.0, 12.0, -2.0] {
        rejected(&mut session, json!({"op": "value.set", "id": 7, "value": [written, 0.0]}));
    }
    rejected(&mut session, json!({"op": "evaluate", "values": {"7": [6.0, 0.0]}}));
    // The end of the range is a valid value there, and the key at max answers.
    assert_eq!(result(&set_value(&mut session, 4.0))["value"], json!([4.0, 0.0]));
    assert_eq!(trans_x(&mut session, Value::Null), 40.0);
}

/// A clip that drives parameter 7 from 0 at frame 0 to `end` at frame 48, on a straight line.
fn add_turning_clip(session: &mut Session, end: f64) {
    result(&request(session, json!({"op": "animation.create", "name": "Turn"})));
    result(&request(session, json!({"op": "animation.set", "name": "Turn", "length": 60})));
    for (frame, value) in [(0, 0.0), (48, end)] {
        result(&request(session, json!({"op": "animation.key.set", "name": "Turn", "parameter": 7, "axis": 0,
            "frame": frame, "value": value})));
    }
    result(&request(session, json!({"op": "animation.lane.set", "name": "Turn", "parameter": 7, "axis": 0,
        "interpolation": "Linear"})));
}

/// Seek the clip and check where node 4 stands. A lane is sampled in `f32`, so the pose is close, not equal.
fn seek_stands_at(session: &mut Session, frame: u32, expected: f64) {
    result(&request(session, json!({"op": "animation.seek", "name": "Turn", "frame": frame})));
    let stands = trans_x(session, Value::Null);
    assert!((stands - expected).abs() < 1e-4, "frame {frame}: {stands}, expected {expected}");
}

#[test]
fn an_animation_turns_a_repeating_parameter_past_its_range() {
    let mut session = session_with(json!([loop_parameter(7)]));
    add_turning_clip(&mut session, 12.0);
    // Without the flag the pose stops at the key at max once the lane passes 4.
    seek_stands_at(&mut session, 24, 40.0);

    result(&request(&mut session, json!({"op": "parameter.set_repeat", "id": 7, "repeat": true})));
    // The lane keeps its own value, so a reader sees how far the clip has turned.
    let sampled = request(&mut session, json!({"op": "animation.sample", "name": "Turn", "frame": 24}));
    assert_eq!(result(&sampled)["values"]["7"], json!([6.0, 0.0]));
    // Frames 8, 24 and 40 are the lane values 2, 6 and 10: three turns through the same point.
    for frame in [8, 24, 40] {
        seek_stands_at(&mut session, frame, 20.0);
    }
    let turned = session.snapshot()["values"]["7"][0].as_f64().unwrap();
    assert!((turned - 10.0).abs() < 1e-4, "the snapshot keeps the lane value too: {turned}");
    // Frame 12 is the lane value 3, halfway between the keys at 2 and 4. Frame 18 is 4.5, past the seam: a
    // quarter of the way from the key at 0 to the key at 2.
    seek_stands_at(&mut session, 12, 30.0);
    seek_stands_at(&mut session, 18, 12.5);
    // The pose is the one the same value gives when it is written by hand.
    result(&request(&mut session, json!({"op": "animation.reset"})));
    result(&set_value(&mut session, 3.0));
    assert_eq!(trans_x(&mut session, Value::Null), 30.0);
}

#[test]
fn a_lane_value_far_past_the_range_stands_where_the_value_it_wraps_to_stands() {
    // A preview keeps the lane value, so evaluation has to find its place on the circle. Over 0..3, 2^60 leaves 1: a
    // third of the way round, between the keys 10 and 20. Divided by 3 first, 2^60 has no fraction left.
    let mut wheel = loop_parameter(7);
    wheel["max"] = json!([3.0, 1.0]);
    wheel["repeat"] = json!(true);
    let mut session = session_with(json!([wheel]));
    let far = 2f64.powi(60);
    add_turning_clip(&mut session, far);
    seek_stands_at(&mut session, 48, 10.0 + 10.0 * 2.0 / 3.0);
    result(&request(&mut session, json!({"op": "animation.reset"})));
    assert_eq!(result(&set_value(&mut session, far))["value"], json!([1.0, 0.0]));
    let by_hand = trans_x(&mut session, Value::Null);
    assert!((by_hand - (10.0 + 10.0 * 2.0 / 3.0)).abs() < 1e-9, "got {by_hand}");

    // A range so narrow that the division overflows: the preview still stands where `value.set` puts the value.
    let mut wheel = loop_parameter(7);
    wheel["max"] = json!([1e-300, 1.0]);
    wheel["repeat"] = json!(true);
    let mut session = session_with(json!([wheel]));
    let far = f64::from(1e30_f32);
    add_turning_clip(&mut session, far);
    result(&set_value(&mut session, far));
    let by_hand = trans_x(&mut session, Value::Null);
    assert!((10.0..=40.0).contains(&by_hand), "got {by_hand}");
    seek_stands_at(&mut session, 48, by_hand);
}

#[test]
fn an_animation_that_runs_below_the_range_turns_the_other_way() {
    let mut session = session_with(json!([loop_parameter(7)]));
    result(&request(&mut session, json!({"op": "parameter.set_repeat", "id": 7, "repeat": true})));
    add_turning_clip(&mut session, -12.0);
    // Frame 4 is the lane value -1, the same point as 3; frame 24 is -6, the same point as 2.
    seek_stands_at(&mut session, 4, 30.0);
    seek_stands_at(&mut session, 24, 20.0);
}

#[test]
fn a_motion3_curve_turns_a_repeating_parameter_past_its_range() {
    let motion = json!({"Version": 3,
        "Meta": {"Duration": 2, "Fps": 30, "Loop": true, "AreBeziersRestricted": false, "CurveCount": 1,
            "TotalSegmentCount": 1, "TotalPointCount": 2, "UserDataCount": 0, "TotalUserDataSize": 0},
        "Curves": [{"Target": "Parameter", "Id": "Turn", "Segments": [0, 0, 0, 2, 12]}]});
    let pose_at = |session: &mut Session, time: f64| {
        request(session, json!({"op": "motion3.seek", "name": "Turn", "time": time}))
    };
    for repeat in [false, true] {
        let mut model = session_with(json!([loop_parameter(7)])).snapshot()["model"].clone();
        model["param"][0]["repeat"] = json!(repeat);
        model["live2d"] = json!({"motions": {"Turn": motion}});
        let mut session = Session::new();
        result(&request(&mut session, json!({"op": "new", "model": model})));
        result(&request(&mut session, json!({"op": "motion3.bind", "name": "Turn",
            "bindings": [{"id": "Turn", "parameter": 7, "axis": 0}]})));
        if !repeat {
            // The curve is at 6 after one second: outside 0..4, so the pose is refused.
            assert_eq!(pose_at(&mut session, 1.0)["succeeded"], false);
            continue;
        }
        result(&pose_at(&mut session, 1.0));
        assert_eq!(session.snapshot()["values"]["7"], json!([6.0, 0.0]));
        assert_eq!(trans_x(&mut session, Value::Null), 20.0);
        result(&pose_at(&mut session, 0.5));
        assert_eq!(trans_x(&mut session, Value::Null), 30.0);
    }
}
