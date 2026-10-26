use serde_json::{json, Value};
use soul2d_core::Session;

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn node(uuid: u64, name: &str, kind: &str, children: Value) -> Value {
    let mut entry = json!({
        "uuid": uuid, "name": name, "type": kind, "enabled": true, "zsort": 0.0,
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

fn group(id: u64, name: &str, children: Value) -> Value {
    json!({"groupUUID": id, "name": name, "color": [0.15, 0.15, 0.15], "children": children})
}

fn session_of(children: Value, params: Value) -> Session {
    let model = json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": node(1, "Root", "Node", children),
        "param": params,
        "animations": {},
    });
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new", "model": model})));
    session
}

/// Root 1 with Parts 10 and 12, Nodes 11, 13 and 14; parameters 20, 21, 31, 33 and groups 30, 32.
fn fixture() -> Session {
    session_of(
        json!([
            node(10, "Ink Hair", "Part", json!([node(11, "Ink Arm", "Node", Value::Null), node(12, "ink shadow", "Part", Value::Null)])),
            node(13, "Line", "Node", Value::Null),
            node(14, "Ink Ink", "Node", Value::Null),
        ]),
        json!([
            plain(20, "Ink X"),
            group(30, "Ink group", json!([plain(31, "Ink Y"), group(32, "Inner", json!([plain(33, "Deep Ink")]))])),
            plain(21, "Mouth"),
        ]),
    )
}

fn history_count(session: &mut Session) -> u64 {
    let response = request(session, json!({"op": "history.summary"}));
    result(&response)["count"].as_u64().unwrap()
}

/// Every (uuid, name) of the document, nodes first then parameters and groups, as the model stores them.
fn names(session: &Session) -> Vec<(u64, String)> {
    fn walk(item: &Value, out: &mut Vec<(u64, String)>) {
        if let Some(id) = item["uuid"].as_u64().or(item["groupUUID"].as_u64()) {
            out.push((id, item["name"].as_str().unwrap().to_owned()));
        }
        item["children"].as_array().into_iter().flatten().for_each(|child| walk(child, out));
    }
    let model = &session.snapshot()["model"];
    let mut out = Vec::new();
    walk(&model["nodes"], &mut out);
    model["param"].as_array().unwrap().iter().for_each(|item| walk(item, &mut out));
    out
}

fn name_of(session: &Session, id: u64) -> String {
    names(session).into_iter().find(|(uuid, _)| *uuid == id).unwrap().1
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

fn matches(session: &mut Session, arguments: Value) -> Value {
    let mut call = json!({"op": "object.find"});
    call.as_object_mut().unwrap().extend(arguments.as_object().unwrap().clone());
    result(&request(session, call))["matches"].clone()
}

fn replace(find: &str, to: &str, ids: Value) -> Value {
    json!({"op": "object.replace_name", "ids": ids, "find": find, "replace": to})
}

#[test]
fn find_lists_matches_nodes_first_in_document_order() {
    let mut session = fixture();
    let before = session.snapshot();
    let count = history_count(&mut session);
    assert_eq!(
        matches(&mut session, json!({"text": "Ink"})),
        json!([
            {"kind": "node", "typeId": "Part", "uuid": 10, "name": "Ink Hair"},
            {"kind": "node", "typeId": "Node", "uuid": 11, "name": "Ink Arm"},
            {"kind": "node", "typeId": "Node", "uuid": 14, "name": "Ink Ink"},
            {"kind": "parameter", "typeId": "Parameter", "uuid": 20, "name": "Ink X"},
            {"kind": "parameter", "typeId": "Parameter", "uuid": 30, "name": "Ink group"},
            {"kind": "parameter", "typeId": "Parameter", "uuid": 31, "name": "Ink Y"},
            {"kind": "parameter", "typeId": "Parameter", "uuid": 33, "name": "Deep Ink"},
        ])
    );
    let insensitive = matches(&mut session, json!({"text": "Ink", "caseSensitive": false}));
    let ids: Vec<u64> = insensitive.as_array().unwrap().iter().map(|m| m["uuid"].as_u64().unwrap()).collect();
    assert_eq!(ids, vec![10, 11, 12, 14, 20, 30, 31, 33]);
    assert_eq!(insensitive[2]["name"], "ink shadow");
    assert_eq!(matches(&mut session, json!({"text": "INK"})), json!([]));
    let parts = matches(&mut session, json!({"text": "Ink", "caseSensitive": false, "types": ["Part"]}));
    let ids: Vec<u64> = parts.as_array().unwrap().iter().map(|m| m["uuid"].as_u64().unwrap()).collect();
    assert_eq!(ids, vec![10, 12]);
    let parameters = matches(&mut session, json!({"text": "Ink", "types": ["Parameter"]}));
    let ids: Vec<u64> = parameters.as_array().unwrap().iter().map(|m| m["uuid"].as_u64().unwrap()).collect();
    assert_eq!(ids, vec![20, 30, 31, 33]);
    let mixed = matches(&mut session, json!({"text": "Ink", "types": ["Parameter", "Part"]}));
    assert_eq!(mixed.as_array().unwrap().len(), 5);
    assert_eq!(matches(&mut session, json!({"text": "Nothing like this"})), json!([]));
    assert_eq!(session.snapshot(), before);
    assert_eq!(history_count(&mut session), count);
}

#[test]
fn find_refuses_a_malformed_request() {
    let mut session = fixture();
    for bad in [
        json!({"op": "object.find"}),
        json!({"op": "object.find", "text": ""}),
        json!({"op": "object.find", "text": 3}),
        json!({"op": "object.find", "text": "x".repeat(257)}),
        json!({"op": "object.find", "text": "Ink", "types": []}),
        json!({"op": "object.find", "text": "Ink", "types": "Part"}),
        json!({"op": "object.find", "text": "Ink", "types": ["Part", 1]}),
        json!({"op": "object.find", "text": "Ink", "types": vec!["Part"; 4097]}),
        json!({"op": "object.find", "text": "Ink", "caseSensitive": "no"}),
    ] {
        rejected(&mut session, bad);
    }
    assert_eq!(matches(&mut session, json!({"text": "x".repeat(256)})), json!([]));
}

#[test]
fn replace_renames_nodes_a_parameter_and_a_group_in_one_undo_step() {
    let mut session = fixture();
    let before = session.snapshot();
    let count = history_count(&mut session);
    let response = request(&mut session, replace("Ink", "Paint", json!([10, 11, 20, 30, 33, 13])));
    assert_eq!(
        result(&response),
        &json!({
            "renamed": [
                {"kind": "node", "uuid": 10, "from": "Ink Hair", "to": "Paint Hair"},
                {"kind": "node", "uuid": 11, "from": "Ink Arm", "to": "Paint Arm"},
                {"kind": "parameter", "uuid": 20, "from": "Ink X", "to": "Paint X"},
                {"kind": "parameter", "uuid": 30, "from": "Ink group", "to": "Paint group"},
                {"kind": "parameter", "uuid": 33, "from": "Deep Ink", "to": "Deep Paint"},
            ],
            "unchanged": [13],
        })
    );
    assert_eq!(history_count(&mut session), count + 1);
    assert_eq!(name_of(&session, 10), "Paint Hair");
    assert_eq!(name_of(&session, 11), "Paint Arm");
    assert_eq!(name_of(&session, 12), "ink shadow");
    assert_eq!(name_of(&session, 13), "Line");
    assert_eq!(name_of(&session, 20), "Paint X");
    assert_eq!(name_of(&session, 30), "Paint group");
    assert_eq!(name_of(&session, 31), "Ink Y");
    assert_eq!(name_of(&session, 33), "Deep Paint");
    result(&request(&mut session, json!({"op": "undo"})));
    assert_eq!(session.snapshot()["model"], before["model"]);
    assert_eq!(names(&session)[1].1, "Ink Hair");
    result(&request(&mut session, json!({"op": "redo"})));
    assert_eq!(name_of(&session, 33), "Deep Paint");
}

#[test]
fn replace_with_nothing_to_change_adds_no_undo_step() {
    let mut session = fixture();
    let before = session.snapshot();
    let count = history_count(&mut session);
    let reply = request(&mut session, replace("Ink", "Paint", json!([13, 21])));
    assert_eq!(result(&reply), &json!({"renamed": [], "unchanged": [13, 21]}));
    let same = request(&mut session, replace("Ink", "Ink", json!([10])));
    assert_eq!(result(&same), &json!({"renamed": [], "unchanged": [10]}));
    assert_eq!(session.snapshot(), before);
    assert_eq!(history_count(&mut session), count);
}

#[test]
fn replace_changes_every_occurrence_and_accepts_an_empty_replacement() {
    let mut session = fixture();
    let reply = request(&mut session, replace("Ink", "Paint", json!([14])));
    assert_eq!(result(&reply)["renamed"][0]["to"], "Paint Paint");
    let reply = request(&mut session, replace("Ink ", "", json!([10, 13])));
    assert_eq!(result(&reply), &json!({
        "renamed": [{"kind": "node", "uuid": 10, "from": "Ink Hair", "to": "Hair"}],
        "unchanged": [13],
    }));
    let mut session = fixture();
    let reply = request(&mut session, replace("Ink ", "", json!([14])));
    assert_eq!(result(&reply)["renamed"][0]["to"], "Ink");
}

#[test]
fn replace_finds_and_renames_a_parameter_in_a_group_inside_a_group() {
    let mut session = fixture();
    let reply = request(&mut session, replace("Deep", "Shallow", json!([33, 32])));
    assert_eq!(result(&reply), &json!({
        "renamed": [{"kind": "parameter", "uuid": 33, "from": "Deep Ink", "to": "Shallow Ink"}],
        "unchanged": [32],
    }));
    assert_eq!(session.snapshot()["model"]["param"][1]["children"][1]["children"][0]["name"], "Shallow Ink");
    let found = matches(&mut session, json!({"text": "Shallow"}));
    assert_eq!(found, json!([{"kind": "parameter", "typeId": "Parameter", "uuid": 33, "name": "Shallow Ink"}]));
}

#[test]
fn replace_judges_names_against_the_finished_document() {
    let mut session = session_of(
        json!([node(40, "a", "Node", Value::Null), node(41, "aa", "Node", Value::Null)]),
        json!([plain(50, "p"), plain(51, "pp")]),
    );
    let reply = request(&mut session, replace("a", "aa", json!([40, 41])));
    assert_eq!(result(&reply)["renamed"].as_array().unwrap().len(), 2);
    assert_eq!((name_of(&session, 40), name_of(&session, 41)), ("aa".into(), "aaaa".into()));
    let reply = request(&mut session, replace("p", "pp", json!([50, 51])));
    assert_eq!(result(&reply)["renamed"].as_array().unwrap().len(), 2);
    assert_eq!((name_of(&session, 50), name_of(&session, 51)), ("pp".into(), "pppp".into()));
}

fn collision_fixture() -> Session {
    session_of(
        json!([node(40, "Ink Hair", "Node", Value::Null), node(41, "Paint Hair", "Node", Value::Null)]),
        json!([plain(50, "Ink X"), plain(51, "Paint X"), plain(52, "ab"), plain(53, "ba"), plain(54, "Ink")]),
    )
}

#[test]
fn replace_refuses_a_parameter_name_that_is_taken() {
    let mut session = collision_fixture();
    let message = rejected(&mut session, replace("Ink", "Paint", json!([50])));
    assert!(message.contains("parameter 50") && message.contains("Paint X"), "{message}");
    let message = rejected(&mut session, replace("Ink", "Paint", json!([54, 50])));
    assert!(message.contains("parameter 50"), "{message}");
}

#[test]
fn replace_refuses_a_node_name_that_is_taken_even_after_a_change_that_would_succeed() {
    let mut session = collision_fixture();
    let message = rejected(&mut session, replace("Ink", "Paint", json!([50, 40])));
    assert!(message.contains("node 40") && message.contains("Name already taken"), "{message}");
    assert_eq!(name_of(&session, 50), "Ink X");
}

#[test]
fn replace_refuses_two_listed_parameters_that_would_end_with_one_name() {
    let mut session = collision_fixture();
    let message = rejected(&mut session, replace("a", "b", json!([52, 53])));
    assert!(message.contains("bb"), "{message}");
    let mut nodes = session_of(json!([node(40, "ab", "Node", Value::Null), node(41, "ba", "Node", Value::Null)]), json!([]));
    rejected(&mut nodes, replace("a", "b", json!([40, 41])));
}

#[test]
fn replace_refuses_an_empty_name_the_root_and_cuts_a_long_name_in_the_message() {
    let mut session = collision_fixture();
    let message = rejected(&mut session, replace("Ink", "", json!([54])));
    assert!(message.contains("empty"), "{message}");
    let mut nodes = session_of(json!([node(40, "Ink", "Node", Value::Null)]), json!([]));
    let message = rejected(&mut nodes, replace("Ink", "", json!([40])));
    assert!(message.contains("empty") && message.contains("node 40"), "{message}");
    rejected(&mut nodes, replace("Root", "Top", json!([1])));
    let long = "n".repeat(200);
    let mut taken = session_of(json!([node(40, "Ink", "Node", Value::Null), node(41, &long, "Node", Value::Null)]), json!([]));
    let message = rejected(&mut taken, replace("Ink", &long, json!([40])));
    assert!(message.contains(&"n".repeat(64)) && !message.contains(&"n".repeat(65)), "{message}");
    // A replace cannot grow a name without bound: 20 times 256 characters is refused, 20 times 200 is not.
    let mut grows = session_of(json!([node(40, &"a".repeat(20), "Node", Value::Null)]), json!([]));
    let message = rejected(&mut grows, replace("a", &"x".repeat(256), json!([40])));
    assert!(message.contains("longer than 4096"), "{message}");
    result(&request(&mut grows, replace("a", &"x".repeat(200), json!([40]))));
}

#[test]
fn replace_refuses_a_malformed_request() {
    let mut session = fixture();
    for bad in [
        replace("Ink", "Paint", json!([])),
        replace("Ink", "Paint", json!([999])),
        replace("Ink", "Paint", json!([10, 10])),
        replace("Ink", "Paint", json!([10, 0])),
        replace("Ink", "Paint", json!(["10"])),
        replace("Ink", "Paint", json!(10)),
        replace("Ink", "Paint", Value::Array(vec![json!(10); 4097])),
        replace("", "Paint", json!([10])),
        replace(&"x".repeat(257), "Paint", json!([10])),
        replace("Ink", &"x".repeat(257), json!([10])),
        json!({"op": "object.replace_name", "find": "Ink", "replace": "Paint"}),
        json!({"op": "object.replace_name", "ids": [10], "replace": "Paint"}),
        json!({"op": "object.replace_name", "ids": [10], "find": "Ink"}),
        json!({"op": "object.replace_name", "ids": [10], "find": "Ink", "replace": 1}),
    ] {
        rejected(&mut session, bad);
    }
}

fn rpc(session: &mut Session, method: &str, params: Value) -> Value {
    let response = session.handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}));
    response.unwrap()["result"].clone()
}

fn call(session: &mut Session, name: &str, arguments: Value) -> Value {
    rpc(session, "tools/call", json!({"name": name, "arguments": arguments}))
}

#[test]
fn the_tools_find_and_rename_through_mcp() {
    let mut session = fixture();
    let found = call(&mut session, "object_find", json!({"text": "Ink", "types": ["Part"]}));
    assert_eq!(found["isError"], false, "{found}");
    let body: Value = serde_json::from_str(found["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body, json!({"matches": [{"kind": "node", "typeId": "Part", "uuid": 10, "name": "Ink Hair"}]}));
    let renamed = call(&mut session, "object_replace_name", json!({"ids": [10, 20], "find": "Ink", "replace": "Paint"}));
    assert_eq!(renamed["isError"], false, "{renamed}");
    let body: Value = serde_json::from_str(renamed["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["renamed"][0], json!({"kind": "node", "uuid": 10, "from": "Ink Hair", "to": "Paint Hair"}));
    assert_eq!(body["renamed"][1]["to"], "Paint X");
    let read = rpc(&mut session, "resources/read", json!({"uri": "resource://soul2d/resources/10"}));
    let resource: Value = serde_json::from_str(read["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(resource["item"]["name"], "Paint Hair");
    let before = session.snapshot();
    let missing = call(&mut session, "object_replace_name", json!({"find": "Ink", "replace": "Paint"}));
    assert_eq!(missing["isError"], true);
    assert_eq!(missing["content"][0]["text"], "missing required argument(s) for object_replace_name: ids");
    let missing = call(&mut session, "object_find", json!({}));
    assert_eq!(missing["content"][0]["text"], "missing required argument(s) for object_find: text");
    assert_eq!(session.snapshot(), before);
}
