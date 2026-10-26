//! Parameter panel features beyond the first port: parameter links, flip-pair mirroring, the binding and
//! parameter clipboards, "Copy to" / "Swap with" and "To 2D". Expected numbers are worked out from
//! commands/parameter/{base,paramedit}.d, commands/binding/*.d, utils/transform.d and core/param/binding.d.

use serde_json::{json, Value};
use soul2d_core::Session;

const FLIP_KEY: &str = "com.inochi2d.creator.FlipConfig";

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn transform() -> Value {
    json!({"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]})
}

fn node(uuid: u64, name: &str) -> Value {
    json!({
        "uuid": uuid, "name": name, "type": "Node", "enabled": true, "zsort": 0.0,
        "transform": transform(), "lockToRoot": false, "pinToMesh": false, "children": [],
    })
}

fn part(uuid: u64, name: &str, verts: Value, indices: Value) -> Value {
    let count = verts.as_array().unwrap().len();
    json!({
        "uuid": uuid, "name": name, "type": "Part", "enabled": true, "zsort": 0.0,
        "transform": transform(), "children": [], "lockToRoot": false, "pinToMesh": false,
        "mesh": {"verts": verts, "uvs": vec![0.0; count], "indices": indices, "origin": [0.0, 0.0]},
        "textures": [0], "blend_mode": "Normal", "tint": [1, 1, 1], "screenTint": [0, 0, 0],
        "emissionStrength": 1, "opacity": 1,
    })
}

fn parameter(id: u64, name: &str, vec2: bool, min: [f64; 2], max: [f64; 2], axes: Value, bindings: Value) -> Value {
    json!({
        "uuid": id, "name": name, "is_vec2": vec2, "min": min, "max": max, "defaults": [0.0, 0.0],
        "axis_points": axes, "merge_mode": "Additive", "bindings": bindings,
    })
}

/// A 1D parameter over -1..1 with the axis points 0, 0.5, 1.
fn plain(id: u64, name: &str, bindings: Value) -> Value {
    parameter(id, name, false, [-1.0, 0.0], [1.0, 1.0], json!([[0.0, 0.5, 1.0], [0.0]]), bindings)
}

fn binding(node: u64, channel: &str, values: Value, flags: Value) -> Value {
    json!({"node": node, "param_name": channel, "interpolate_mode": "Linear", "values": values, "isSet": flags})
}

fn scalar_binding(node: u64, channel: &str, values: &[f64], flags: &[bool]) -> Value {
    binding(
        node,
        channel,
        json!(values.iter().map(|v| vec![*v]).collect::<Vec<_>>()),
        json!(flags.iter().map(|f| vec![*f]).collect::<Vec<_>>()),
    )
}

fn session_with(extra_nodes: Vec<Value>, params: Value) -> Session {
    let mut children = vec![node(4, "Left"), node(5, "Right")];
    children.extend(extra_nodes);
    let mut root = node(1, "Root");
    root["children"] = json!(children);
    let model = json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": root, "param": params, "animations": {},
    });
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new", "model": model})));
    session
}

/// Left (4) and Right (5) are flip partners.
fn set_pairs(session: &mut Session, pairs: &[(u64, u64)]) {
    let items: Vec<Value> = pairs.iter().map(|(a, b)| json!({"uuid1": a, "uuid2": b})).collect();
    let payload = serde_json::to_vec(&items).unwrap();
    let text = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, payload);
    result(&request(session, json!({"op": "extdata.set", "key": FLIP_KEY, "base64": text})));
}

fn params(session: &Session) -> Value {
    session.snapshot()["model"]["param"].clone()
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
    response["message"].as_str().or(response["error"].as_str()).unwrap_or("").to_owned()
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

fn find<'a>(list: &'a Value, id: u64) -> &'a Value {
    fn walk<'a>(list: &'a Value, id: u64) -> Option<&'a Value> {
        list.as_array()?.iter().find_map(|item| {
            if item["uuid"].as_u64() == Some(id) {
                Some(item)
            } else {
                walk(&item["children"], id)
            }
        })
    }
    walk(list, id).unwrap_or_else(|| panic!("parameter {id} not found in {list}"))
}

fn binding_of<'a>(parameter: &'a Value, node: u64, channel: &str) -> &'a Value {
    parameter["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["node"] == node && b["param_name"] == channel)
        .unwrap_or_else(|| panic!("no {channel} binding on {node} in {parameter}"))
}

fn trans_x(evaluated: &Value, index: usize) -> f64 {
    evaluated["nodes"]["children"][index]["transform"]["trans"][0].as_f64().unwrap()
}

// ------------------------------------------------------------------------------------------ links

fn link_fixture() -> Session {
    // Look (20) drives Open (21): Open ranges 1..3, starts at 2 and moves Left's X by 0..10.
    let mut open = parameter(
        21, "Open", false, [1.0, 0.0], [3.0, 1.0], json!([[0.0, 1.0], [0.0]]),
        json!([scalar_binding(4, "transform.t.x", &[0.0, 10.0], &[true, true])]),
    );
    open["defaults"] = json!([2.0, 0.0]);
    session_with(vec![], json!([plain(20, "Look", json!([])), open]))
}

#[test]
fn a_link_stores_the_engine_binding_and_the_driven_parameter_follows_it() {
    let mut session = link_fixture();
    let created = one_undo_step(&mut session, json!({"op": "parameter.link", "id": 20, "target": 21}));
    let expected = json!({
        "node": 21, "param_name": "X", "interpolate_mode": "Linear",
        "values": [[1.0], [2.0], [3.0]], "isSet": [[true], [true], [true]],
    });
    assert_eq!(created, expected);
    assert_eq!(params(&session)[0]["bindings"], json!([expected]));

    // Look = 0.5 forces Open to 2.5, which moves Left by (2.5 - 1) / 2 * 10 = 7.5.
    let evaluated = request(&mut session, json!({"op": "evaluate", "values": {"20": [0.5, 0.0]}}));
    assert_eq!(trans_x(result(&evaluated), 0), 7.5);
    let evaluated = request(&mut session, json!({"op": "evaluate", "values": {"20": [-1.0, 0.0]}}));
    assert_eq!(trans_x(result(&evaluated), 0), 0.0);
    // The scene path (render) follows the driver too.
    result(&request(&mut session, json!({"op": "scene.evaluate", "values": {"20": [1.0, 0.0]}})));
    result(&request(&mut session, json!({"op": "value.set", "id": 20, "value": [1.0, 0.0]})));
    let evaluated = request(&mut session, json!({"op": "evaluate"}));
    assert_eq!(trans_x(result(&evaluated), 0), 10.0);

    rejected(&mut session, json!({"op": "parameter.link", "id": 20, "target": 20}));
    rejected(&mut session, json!({"op": "parameter.link", "id": 20, "target": 21, "axis": 1}));
    rejected(&mut session, json!({"op": "parameter.link", "id": 20, "target": 21, "target_axis": 1}));
    rejected(&mut session, json!({"op": "parameter.link", "id": 20, "target": 99}));
    rejected(&mut session, json!({"op": "parameter.link", "id": 20}));
    // Linking again is refused: the binding already exists.
    let message = rejected(&mut session, json!({"op": "parameter.link", "id": 20, "target": 21}));
    assert!(message.contains("already"), "{message}");

    // A binding whose target is a parameter does not trip the operations that walk node bindings.
    result(&request(&mut session, json!({
        "op": "node.transform", "id": 4, "component": "translationX", "value": 3.0
    })));
    result(&request(&mut session, json!({
        "op": "binding.set_trs", "parameter": 21, "nodes": [4], "key": [1, 0], "translation": [5.0, 0.0]
    })));
    assert_eq!(params(&session)[1]["bindings"][0]["values"], json!([[0.0], [5.0]]));
    result(&request(&mut session, json!({"op": "node.delete", "id": 4})));
    assert_eq!(params(&session)[0]["bindings"][0]["param_name"], "X");
    result(&request(&mut session, json!({"op": "render.packet"})));
}

#[test]
fn unlink_deleting_the_target_and_legacy_files_keep_the_document_valid() {
    let mut session = link_fixture();
    result(&request(&mut session, json!({"op": "parameter.link", "id": 20, "target": 21})));
    one_undo_step(&mut session, json!({"op": "parameter.unlink", "id": 20, "target": 21}));
    assert_eq!(params(&session)[0]["bindings"], json!([]));
    rejected(&mut session, json!({"op": "parameter.unlink", "id": 20, "target": 21}));
    rejected(&mut session, json!({"op": "parameter.unlink", "id": 20, "target": 21, "target_axis": 2}));

    // Deleting the driven parameter takes the link along.
    let mut session = link_fixture();
    result(&request(&mut session, json!({"op": "parameter.link", "id": 20, "target": 21})));
    one_undo_step(&mut session, json!({"op": "parameter.delete", "id": 21}));
    assert_eq!(params(&session)[0]["bindings"], json!([]));
    result(&request(&mut session, json!({"op": "evaluate"})));

    // The older integer form of the axis is read and stored as "X".
    let legacy = binding(21, "X", json!([[1.0], [2.0], [3.0]]), json!([[true], [true], [true]]));
    let mut legacy_json = legacy.clone();
    legacy_json["param_name"] = json!(0);
    let mut open = plain(21, "Open", json!([]));
    open["min"] = json!([1.0, 0.0]);
    open["max"] = json!([3.0, 1.0]);
    let mut session = session_with(vec![], json!([plain(20, "Look", json!([legacy_json])), open]));
    assert_eq!(params(&session)[0]["bindings"], json!([legacy]));
    result(&request(&mut session, json!({"op": "evaluate", "values": {"20": [1.0, 0.0]}})));
}

#[test]
fn keypoint_actions_treat_a_link_like_a_plain_number() {
    let mut session = link_fixture();
    result(&request(&mut session, json!({"op": "parameter.link", "id": 20, "target": 21})));
    let x = json!({"node": 21, "param_name": "X"});

    // Mirroring negates in both directions; reset goes to the driven axis' default (2); unset re-interpolates.
    one_undo_step(&mut session, json!({"op": "parameter.mirror", "id": 20, "axis": 0}));
    assert_eq!(binding_of(&params(&session)[0], 21, "X")["values"], json!([[-1.0], [-2.0], [-3.0]]));
    let mut session = link_fixture();
    result(&request(&mut session, json!({"op": "parameter.link", "id": 20, "target": 21})));
    one_undo_step(&mut session, json!({
        "op": "binding.keypoint", "parameter": 20, "key": [0, 0], "action": "reset", "bindings": [x.clone()]
    }));
    assert_eq!(binding_of(&params(&session)[0], 21, "X")["values"], json!([[2.0], [2.0], [3.0]]));
    one_undo_step(&mut session, json!({
        "op": "binding.keypoint", "parameter": 20, "key": [2, 0], "action": "unset", "bindings": [x.clone()]
    }));
    let linked = binding_of(&params(&session)[0], 21, "X").clone();
    assert_eq!(linked["values"], json!([[2.0], [2.0], [2.0]]));
    assert_eq!(linked["isSet"], json!([[true], [true], [false]]));
    one_undo_step(&mut session, json!({
        "op": "binding.set_interpolation", "parameter": 20, "mode": "Nearest", "bindings": [x.clone()]
    }));
    one_undo_step(&mut session, json!({"op": "binding.remove", "parameter": 20, "bindings": [x]}));
}

#[test]
fn link_tools_are_reachable_over_mcp() {
    let mut session = link_fixture();
    let call = |session: &mut Session, name: &str, arguments: Value| {
        session
            .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": name, "arguments": arguments}}))
            .unwrap()
    };
    assert_eq!(call(&mut session, "parameter_link", json!({"id": 20, "target": 21}))["result"]["isError"], false);
    assert_eq!(params(&session)[0]["bindings"][0]["param_name"], "X");
    assert_eq!(call(&mut session, "parameter_unlink", json!({"id": 20, "target": 21}))["result"]["isError"], false);
    assert_eq!(params(&session)[0]["bindings"], json!([]));
    assert_eq!(call(&mut session, "parameter_unlink", json!({"id": 20, "target": 21}))["result"]["isError"], true);
}

// ------------------------------------------------------------------------------------------ mirrored autofill

#[test]
fn mirrored_autofill_fills_the_partner_binding_from_the_other_half() {
    let session = || {
        let eyes = plain(
            20, "Eyes",
            json!([
                scalar_binding(4, "transform.t.x", &[2.0, 0.0, 6.0], &[true, true, true]),
                scalar_binding(4, "transform.t.y", &[1.0, 0.0, 3.0], &[true, true, true]),
            ]),
        );
        let mut session = session_with(vec![], json!([eyes]));
        set_pairs(&mut session, &[(4, 5)]);
        session
    };

    // Direction 1 fills the low half (x = 0) of Right from the mirrored high half of Left: X is negated,
    // Y is not. The new bindings get the zero key at x = 0.5 as `createBinding` does.
    let mut s = session();
    let reply = one_undo_step(&mut s, json!({"op": "parameter.mirrored_autofill", "id": 20, "direction": 1}));
    assert_eq!(reply["filled"], 2);
    let eyes = params(&s)[0].clone();
    assert_eq!(eyes["bindings"].as_array().unwrap().len(), 4);
    let tx = binding_of(&eyes, 5, "transform.t.x");
    assert_eq!(tx["values"], json!([[-6.0], [0.0], [0.0]]));
    assert_eq!(tx["isSet"], json!([[true], [true], [false]]));
    let ty = binding_of(&eyes, 5, "transform.t.y");
    assert_eq!(ty["values"], json!([[3.0], [0.0], [0.0]]));
    assert_eq!(ty["isSet"], json!([[true], [true], [false]]));
    assert_eq!(tx["interpolate_mode"], "Linear");
    // The source bindings are untouched.
    assert_eq!(binding_of(&eyes, 4, "transform.t.x")["values"], json!([[2.0], [0.0], [6.0]]));

    // Direction 2 fills the high half from the low half.
    let mut s = session();
    one_undo_step(&mut s, json!({"op": "parameter.mirrored_autofill", "id": 20, "direction": 2}));
    let tx = binding_of(&params(&s)[0], 5, "transform.t.x").clone();
    assert_eq!(tx["values"], json!([[0.0], [0.0], [-2.0]]));
    assert_eq!(tx["isSet"], json!([[false], [true], [true]]));

    // A cell that is already set is not overwritten.
    let mut eyes = plain(20, "Eyes", json!([scalar_binding(4, "transform.t.x", &[2.0, 0.0, 6.0], &[true, true, true])]));
    eyes["bindings"].as_array_mut().unwrap().push(scalar_binding(5, "transform.t.x", &[9.0, 9.0, 9.0], &[true, false, false]));
    let mut s = session_with(vec![], json!([eyes]));
    set_pairs(&mut s, &[(4, 5)]);
    one_undo_step(&mut s, json!({"op": "parameter.mirrored_autofill", "id": 20, "direction": 2}));
    let tx = binding_of(&params(&s)[0], 5, "transform.t.x").clone();
    assert_eq!(tx["isSet"], json!([[true], [false], [true]]));
    assert_eq!(tx["values"][0], json!([9.0]));
    assert_eq!(tx["values"][2], json!([-2.0]));

    // Without pairs a node mirrors onto itself: x = 2 gets the negated x = 0 value.
    let eyes = plain(20, "Eyes", json!([scalar_binding(4, "transform.t.x", &[2.0, 2.0, 2.0], &[true, false, false])]));
    let mut s = session_with(vec![], json!([eyes]));
    one_undo_step(&mut s, json!({"op": "parameter.mirrored_autofill", "id": 20, "direction": 2}));
    let tx = binding_of(&params(&s)[0], 4, "transform.t.x").clone();
    assert_eq!(tx["isSet"], json!([[true], [false], [true]]));
    assert_eq!(tx["values"], json!([[2.0], [0.0], [-2.0]]));

    rejected(&mut s, json!({"op": "parameter.mirrored_autofill", "id": 20, "direction": 3}));
    rejected(&mut s, json!({"op": "parameter.mirrored_autofill", "id": 20, "direction": 5}));
    rejected(&mut s, json!({"op": "parameter.mirrored_autofill", "id": 99, "direction": 1}));
    rejected(&mut s, json!({"op": "parameter.mirrored_autofill", "id": 20}));
}

#[test]
fn mirrored_autofill_on_the_y_axis_of_a_2d_parameter() {
    let look = parameter(
        20, "Look", true, [-1.0, -1.0], [1.0, 1.0], json!([[0.0, 1.0], [0.0, 0.5, 1.0]]),
        json!([binding(
            4, "transform.t.y",
            json!([[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]),
            json!([[true, true, true], [true, true, true]]),
        )]),
    );
    let mut s = session_with(vec![], json!([look]));
    set_pairs(&mut s, &[(4, 5)]);
    // Direction 3 fills y = 1 of Right from y = 0 of Left; Y is mirrored, so t.y is negated.
    one_undo_step(&mut s, json!({"op": "parameter.mirrored_autofill", "id": 20, "direction": 3}));
    let ty = binding_of(&params(&s)[0], 5, "transform.t.y").clone();
    assert_eq!(ty["values"][0][2], json!(-1.0));
    assert_eq!(ty["values"][1][2], json!(-4.0));
    // Only the y = 1 keys are set: no keypoint is a real zero in both axes, so no zero key is added.
    assert_eq!(ty["isSet"], json!([[false, false, true], [false, false, true]]));
    // Direction 4 fills y = 0 from y = 1.
    let look2 = params(&s)[0].clone();
    let mut s = session_with(vec![], json!([look2]));
    set_pairs(&mut s, &[(4, 5)]);
    one_undo_step(&mut s, json!({"op": "parameter.mirrored_autofill", "id": 20, "direction": 4}));
    let ty = binding_of(&params(&s)[0], 5, "transform.t.y").clone();
    assert_eq!(ty["values"][0][0], json!(-3.0));
    assert_eq!(ty["values"][1][0], json!(-6.0));
}

// ------------------------------------------------------------------------------------------ deformations

fn left_mesh() -> Value {
    part(6, "LeftPart", json!([-3.0, 0.0, -1.0, 0.0, -1.0, 2.0, -3.0, 2.0]), json!([0, 1, 2, 0, 2, 3]))
}

fn right_mesh() -> Value {
    part(7, "RightPart", json!([3.0, 0.0, 1.0, 0.0, 1.0, 2.0, 3.0, 2.0]), json!([0, 1, 2, 0, 2, 3]))
}

fn cell(offsets: &[[f64; 2]]) -> Value {
    json!(offsets)
}

#[test]
fn mirrored_autofill_transfers_deformation_through_the_partner_mesh() {
    let offsets = [[1.0, 2.0], [3.0, 4.0], [5.0, 6.0], [7.0, 8.0]];
    let zero = [[0.0, 0.0]; 4];
    let eyes = plain(20, "Eyes", json!([binding(
        6, "deform",
        json!([[cell(&zero)], [cell(&zero)], [cell(&offsets)]]),
        json!([[false], [false], [true]]),
    )]));
    let mut s = session_with(vec![left_mesh(), right_mesh()], json!([eyes]));
    set_pairs(&mut s, &[(6, 7)]);
    one_undo_step(&mut s, json!({"op": "parameter.mirrored_autofill", "id": 20, "direction": 1}));
    // Every Right vertex sits on the mirror image of a Left vertex, so its offset is that vertex' offset
    // with the sign of x flipped.
    let deform = binding_of(&params(&s)[0], 7, "deform").clone();
    assert_eq!(deform["isSet"], json!([[true], [true], [false]]));
    assert_eq!(deform["values"][0], json!([cell(&[[-1.0, 2.0], [-3.0, 4.0], [-5.0, 6.0], [-7.0, 8.0]])]));
    assert_eq!(deform["values"][1], json!([cell(&zero)]));
    assert_eq!(deform["values"][2], json!([cell(&zero)]));
    assert_eq!(binding_of(&params(&s)[0], 6, "deform")["isSet"], json!([[false], [false], [true]]));
}

#[test]
fn flip_deform_and_symmetrize_use_the_nodes_own_mesh() {
    // A mesh that is its own mirror image: vertex 0 <-> 1 and 2 <-> 3 swap places under x -> -x.
    let sym = part(6, "Sym", json!([-1.0, 0.0, 1.0, 0.0, 1.0, 2.0, -1.0, 2.0]), json!([0, 1, 2, 0, 2, 3]));
    let offsets = [[1.0, 0.0], [2.0, 0.0], [3.0, 0.0], [4.0, 0.0]];
    let eyes = plain(20, "Eyes", json!([binding(
        6, "deform",
        json!([[cell(&offsets)], [cell(&offsets)], [cell(&offsets)]]),
        json!([[true], [true], [true]]),
    )]));
    let mut s = session_with(vec![sym], json!([eyes]));

    one_undo_step(&mut s, json!({"op": "binding.keypoint", "parameter": 20, "key": [0, 0], "action": "flip_deform"}));
    let deform = binding_of(&params(&s)[0], 6, "deform").clone();
    assert_eq!(deform["values"][0], json!([cell(&[[-2.0, 0.0], [-1.0, 0.0], [-4.0, 0.0], [-3.0, 0.0]])]));
    assert_eq!(deform["values"][1], json!([cell(&offsets)]));

    let mut s = session_with(
        vec![part(6, "Sym", json!([-1.0, 0.0, 1.0, 0.0, 1.0, 2.0, -1.0, 2.0]), json!([0, 1, 2, 0, 2, 3]))],
        json!([plain(20, "Eyes", json!([binding(
            6, "deform",
            json!([[cell(&offsets)], [cell(&offsets)], [cell(&offsets)]]),
            json!([[true], [true], [true]]),
        )]))]),
    );
    one_undo_step(&mut s, json!({"op": "binding.keypoint", "parameter": 20, "key": [0, 0], "action": "symmetrize_deform"}));
    let deform = binding_of(&params(&s)[0], 6, "deform").clone();
    assert_eq!(deform["values"][0], json!([cell(&[[-0.5, 0.0], [0.5, 0.0], [-0.5, 0.0], [0.5, 0.0]])]));

    // Only deformation bindings are touched; a parameter without any has nothing to flip.
    let mut s = session_with(vec![], json!([plain(20, "Eyes", json!([scalar_binding(4, "transform.t.x", &[1.0, 1.0, 1.0], &[true, true, true])]))]));
    let reply = result(&request(&mut s, json!({
        "op": "binding.keypoint", "parameter": 20, "key": [0, 0], "action": "flip_deform"
    }))).clone();
    assert_eq!(reply["edited"], 0);
}

// ------------------------------------------------------------------------------------------ set from mirror

fn mirror_fixture() -> Session {
    let eyes = plain(20, "Eyes", json!([
        scalar_binding(4, "transform.t.x", &[2.0, 0.0, 6.0], &[true, true, true]),
        scalar_binding(5, "transform.t.x", &[10.0, 0.0, 20.0], &[true, true, true]),
    ]));
    let mut session = session_with(vec![], json!([eyes]));
    set_pairs(&mut session, &[(4, 5)]);
    session
}

#[test]
fn set_from_mirror_pulls_the_partner_value_across_the_chosen_axis() {
    let mut s = mirror_fixture();
    let left = json!([{"node": 4, "param_name": "transform.t.x"}]);
    one_undo_step(&mut s, json!({
        "op": "binding.keypoint", "parameter": 20, "key": [0, 0], "action": "set_from_mirror_horizontal",
        "bindings": left
    }));
    // Left x = 0 takes Right's value at the mirrored x = 2 (20), negated across the horizontal axis.
    let eyes = params(&s)[0].clone();
    assert_eq!(binding_of(&eyes, 4, "transform.t.x")["values"], json!([[-20.0], [0.0], [6.0]]));
    assert_eq!(binding_of(&eyes, 5, "transform.t.x")["values"], json!([[10.0], [0.0], [20.0]]));

    // Vertical mirroring needs a 2D parameter; a 1D parameter has no Y.
    rejected(&mut s, json!({
        "op": "binding.keypoint", "parameter": 20, "key": [0, 0], "action": "set_from_mirror_vertical", "bindings": left
    }));
    rejected(&mut s, json!({
        "op": "binding.keypoint", "parameter": 20, "key": [0, 0], "action": "set_from_mirror_diagonal"
    }));
    rejected(&mut s, json!({
        "op": "binding.keypoint", "parameter": 20, "key": [5, 0], "action": "set_from_mirror_horizontal"
    }));
}

#[test]
fn set_from_1d_mirror_pushes_to_the_partner_when_bindings_are_chosen() {
    // Without chosen bindings every binding pulls in turn: Left gets -20 (Right's x = 2) and then Right
    // gets -6 (Left's x = 2, which the first step did not change).
    let mut s = mirror_fixture();
    one_undo_step(&mut s, json!({
        "op": "binding.keypoint", "parameter": 20, "key": [0, 0], "action": "set_from_mirror_1d"
    }));
    let eyes = params(&s)[0].clone();
    assert_eq!(binding_of(&eyes, 4, "transform.t.x")["values"][0], json!([-20.0]));
    assert_eq!(binding_of(&eyes, 5, "transform.t.x")["values"][0], json!([-6.0]));

    // With Left chosen the value flows the other way: Right takes Left's mirrored x = 2 value, negated.
    let mut s = mirror_fixture();
    one_undo_step(&mut s, json!({
        "op": "binding.keypoint", "parameter": 20, "key": [0, 0], "action": "set_from_mirror_1d",
        "bindings": [{"node": 4, "param_name": "transform.t.x"}]
    }));
    let eyes = params(&s)[0].clone();
    assert_eq!(binding_of(&eyes, 4, "transform.t.x")["values"][0], json!([2.0]));
    assert_eq!(binding_of(&eyes, 5, "transform.t.x")["values"][0], json!([-6.0]));
}

#[test]
fn set_from_diagonal_and_vertical_mirror_of_a_2d_parameter() {
    let look = parameter(
        20, "Look", true, [-1.0, -1.0], [1.0, 1.0], json!([[0.0, 1.0], [0.0, 1.0]]),
        json!([
            binding(4, "transform.t.x", json!([[1.0, 2.0], [3.0, 4.0]]), json!([[true, true], [true, true]])),
            binding(5, "transform.t.x", json!([[10.0, 20.0], [30.0, 40.0]]), json!([[true, true], [true, true]])),
        ]),
    );
    let mut s = session_with(vec![], json!([look]));
    set_pairs(&mut s, &[(4, 5)]);
    let left = json!([{"node": 4, "param_name": "transform.t.x"}]);
    // Diagonal: Left (0, 0) takes Right (1, 1) = 40, negated by the diagonal scale.
    one_undo_step(&mut s, json!({
        "op": "binding.keypoint", "parameter": 20, "key": [0, 0], "action": "set_from_mirror_diagonal", "bindings": left
    }));
    assert_eq!(binding_of(&params(&s)[0], 4, "transform.t.x")["values"][0][0], json!(-40.0));
    // Vertical: t.x is not negated across Y; Left (0, 0) takes Right (0, 1) = 20.
    let mut s = session_with(vec![], json!([params(&s)[0].clone()]));
    set_pairs(&mut s, &[(4, 5)]);
    one_undo_step(&mut s, json!({
        "op": "binding.keypoint", "parameter": 20, "key": [0, 0], "action": "set_from_mirror_vertical", "bindings": left
    }));
    assert_eq!(binding_of(&params(&s)[0], 4, "transform.t.x")["values"][0][0], json!(20.0));
}

// ------------------------------------------------------------------------------------------ parameter clipboard

fn paste_fixture() -> Session {
    let eyes = plain(20, "Eyes", json!([
        scalar_binding(4, "transform.t.x", &[1.0, 2.0, 3.0], &[true, true, true]),
        scalar_binding(4, "transform.t.y", &[4.0, 5.0, 6.0], &[true, true, true]),
    ]));
    let mut session = session_with(vec![], json!([eyes, plain(21, "Brows", json!([])), plain(22, "Mouth", json!([]))]));
    set_pairs(&mut session, &[(4, 5)]);
    session
}

#[test]
fn copy_and_paste_move_bindings_between_parameters_and_consume_the_clipboard() {
    let mut s = paste_fixture();
    let count = history_count(&mut s);
    let copied = result(&request(&mut s, json!({"op": "parameter.copy", "id": 20}))).clone();
    assert_eq!(copied, json!({"copied": 20, "name": "Eyes"}));
    assert_eq!(history_count(&mut s), count, "copy is not an undo step");
    assert_eq!(s.snapshot()["parameterClipboard"], json!({"uuid": 20, "name": "Eyes"}));

    // Plain paste keeps the nodes and the values.
    one_undo_step(&mut s, json!({"op": "parameter.paste", "id": 21}));
    let brows = params(&s)[1].clone();
    assert_eq!(binding_of(&brows, 4, "transform.t.x")["values"], json!([[1.0], [2.0], [3.0]]));
    assert_eq!(binding_of(&brows, 4, "transform.t.y")["isSet"], json!([[true], [true], [true]]));
    assert_eq!(s.snapshot()["parameterClipboard"], Value::Null);
    let message = rejected(&mut s, json!({"op": "parameter.paste", "id": 22}));
    assert!(message.contains("clipboard"), "{message}");

    // Paste and flip lands on the partner node, mirrored: X is negated, Y is not.
    result(&request(&mut s, json!({"op": "parameter.copy", "id": 20})));
    one_undo_step(&mut s, json!({"op": "parameter.paste", "id": 22, "flip": true}));
    let mouth = params(&s)[2].clone();
    assert_eq!(binding_of(&mouth, 5, "transform.t.x")["values"], json!([[-1.0], [-2.0], [-3.0]]));
    assert_eq!(binding_of(&mouth, 5, "transform.t.y")["values"], json!([[4.0], [5.0], [6.0]]));
    assert_eq!(binding_of(&mouth, 5, "transform.t.x")["isSet"], json!([[true], [true], [true]]));
    assert_eq!(mouth["bindings"].as_array().unwrap().len(), 2);

    rejected(&mut s, json!({"op": "parameter.copy", "id": 99}));
    rejected(&mut s, json!({"op": "parameter.paste", "id": 20, "flip": "yes"}));
}

#[test]
fn paste_resamples_a_parameter_with_other_axis_points() {
    let eyes = plain(20, "Eyes", json!([scalar_binding(4, "transform.t.x", &[1.0, 2.0, 3.0], &[true, true, true])]));
    let target = parameter(21, "Wide", false, [-1.0, 0.0], [1.0, 1.0], json!([[0.0, 1.0], [0.0]]), json!([]));
    let mut s = session_with(vec![], json!([eyes, target]));
    result(&request(&mut s, json!({"op": "parameter.copy", "id": 20})));
    // The target has two axis points; the copy is read at the same key indexes of its own axis points.
    one_undo_step(&mut s, json!({"op": "parameter.paste", "id": 21}));
    assert_eq!(binding_of(&params(&s)[1], 4, "transform.t.x")["values"], json!([[1.0], [2.0]]));
    // A target with more keys than the copy has fails and leaves the document alone.
    let wide = parameter(22, "Wider", false, [-1.0, 0.0], [1.0, 1.0], json!([[0.0, 0.25, 0.5, 1.0], [0.0]]), json!([]));
    let mut s = session_with(vec![], json!([plain(20, "Eyes", json!([scalar_binding(4, "transform.t.x", &[1.0, 2.0, 3.0], &[true, true, true])])), wide]));
    result(&request(&mut s, json!({"op": "parameter.copy", "id": 20})));
    rejected(&mut s, json!({"op": "parameter.paste", "id": 22}));
}

#[test]
fn duplicate_and_flip_creates_a_mirrored_copy_next_to_the_original() {
    let mut s = paste_fixture();
    let copy = one_undo_step(&mut s, json!({"op": "parameter.duplicate_flip", "id": 20}));
    assert_eq!(copy["name"], "Eyes (Copy)");
    let id = copy["uuid"].as_u64().unwrap();
    assert!(id != 20 && id != 21 && id != 22);
    let copy = find(&params(&s), id).clone();
    assert_eq!(binding_of(&copy, 5, "transform.t.x")["values"], json!([[-1.0], [-2.0], [-3.0]]));
    assert_eq!(binding_of(&copy, 5, "transform.t.y")["values"], json!([[4.0], [5.0], [6.0]]));
    assert_eq!(copy["bindings"].as_array().unwrap().len(), 2);
    assert_eq!(copy["axis_points"], json!([[0.0, 0.5, 1.0], [0.0]]));
    // The original is untouched.
    assert_eq!(binding_of(find(&params(&s), 20), 4, "transform.t.x")["values"], json!([[1.0], [2.0], [3.0]]));
    rejected(&mut s, json!({"op": "parameter.duplicate_flip", "id": 99}));
    rejected(&mut s, json!({"op": "parameter.duplicate_flip"}));
}

// ------------------------------------------------------------------------------------------ binding clipboard

fn binding_fixture() -> Session {
    let p = parameter(
        40, "Pose", false, [0.0, 0.0], [1.0, 1.0], json!([[0.0, 1.0], [0.0]]),
        json!([
            scalar_binding(4, "transform.t.x", &[1.0, 2.0], &[true, true]),
            scalar_binding(4, "transform.t.y", &[3.0, 4.0], &[true, true]),
            scalar_binding(5, "transform.t.x", &[10.0, 20.0], &[true, true]),
        ]),
    );
    session_with(vec![], json!([p, parameter(41, "Other", false, [0.0, 0.0], [1.0, 1.0], json!([[0.0, 1.0], [0.0]]), json!([]))]))
}

#[test]
fn copy_and_paste_a_keypoint_of_the_chosen_bindings() {
    let mut s = binding_fixture();
    let both = json!([{"node": 4, "param_name": "transform.t.x"}, {"node": 4, "param_name": "transform.t.y"}]);
    let count = history_count(&mut s);
    let summary = result(&request(&mut s, json!({
        "op": "binding.copy", "parameter": 40, "key": [0, 0], "bindings": both
    }))).clone();
    assert_eq!(summary, json!({"parameter": 40, "key": [0, 0], "bindings": [
        {"node": 4, "param_name": "transform.t.x", "set": true},
        {"node": 4, "param_name": "transform.t.y", "set": true}]}));
    assert_eq!(s.snapshot()["bindingClipboard"], summary);
    assert_eq!(history_count(&mut s), count);

    // Pasting without chosen bindings writes every copied binding at the new key.
    one_undo_step(&mut s, json!({"op": "binding.paste", "parameter": 40, "key": [1, 0]}));
    let pose = params(&s)[0].clone();
    assert_eq!(binding_of(&pose, 4, "transform.t.x")["values"], json!([[1.0], [1.0]]));
    assert_eq!(binding_of(&pose, 4, "transform.t.y")["values"], json!([[3.0], [3.0]]));
    assert_eq!(binding_of(&pose, 5, "transform.t.x")["values"], json!([[10.0], [20.0]]));

    // Another parameter receives new bindings; the zero key of the new binding is overwritten by the paste.
    one_undo_step(&mut s, json!({"op": "binding.paste", "parameter": 41, "key": [0, 0]}));
    let other = params(&s)[1].clone();
    let tx = binding_of(&other, 4, "transform.t.x");
    assert_eq!(tx["values"], json!([[1.0], [1.0]]));
    assert_eq!(tx["isSet"], json!([[true], [false]]));

    // A chosen binding that is not on the clipboard receives nothing when the clipboard holds two.
    let before = s.snapshot();
    let reply = result(&request(&mut s, json!({
        "op": "binding.paste", "parameter": 40, "key": [1, 0], "bindings": [{"node": 5, "param_name": "transform.t.x"}]
    }))).clone();
    assert_eq!(reply["pasted"], 0);
    assert_eq!(s.snapshot()["model"], before["model"]);

    rejected(&mut s, json!({"op": "binding.paste", "parameter": 40, "key": [4, 0]}));
    rejected(&mut s, json!({"op": "binding.copy", "parameter": 40, "key": [0, 0], "bindings": [{"node": 9, "param_name": "transform.t.x"}]}));
}

#[test]
fn a_single_copied_binding_can_be_pasted_onto_one_other_binding_and_an_unset_key_unsets() {
    let mut s = binding_fixture();
    result(&request(&mut s, json!({
        "op": "binding.copy", "parameter": 40, "key": [0, 0], "bindings": [{"node": 4, "param_name": "transform.t.y"}]
    })));
    one_undo_step(&mut s, json!({
        "op": "binding.paste", "parameter": 40, "key": [1, 0], "bindings": [{"node": 5, "param_name": "transform.t.x"}]
    }));
    assert_eq!(binding_of(&params(&s)[0], 5, "transform.t.x")["values"], json!([[10.0], [3.0]]));

    // An unset copied key unsets the target key.
    let mut s = binding_fixture();
    result(&request(&mut s, json!({
        "op": "binding.keypoint", "parameter": 40, "key": [1, 0], "action": "unset",
        "bindings": [{"node": 4, "param_name": "transform.t.x"}]
    })));
    result(&request(&mut s, json!({
        "op": "binding.copy", "parameter": 40, "key": [1, 0], "bindings": [{"node": 4, "param_name": "transform.t.x"}]
    })));
    assert_eq!(s.snapshot()["bindingClipboard"]["bindings"][0]["set"], false);
    one_undo_step(&mut s, json!({
        "op": "binding.paste", "parameter": 40, "key": [1, 0], "bindings": [{"node": 5, "param_name": "transform.t.x"}]
    }));
    // The explicit pair has no matching key but is a single entry against a single binding: its stored value is used.
    assert_eq!(binding_of(&params(&s)[0], 5, "transform.t.x")["isSet"], json!([[true], [true]]));
}

#[test]
fn paste_blends_by_ratio_and_inverts_a_deform_shape() {
    // Blend Form: current + ratio × (copied − current).
    let mut s = binding_fixture();
    let tx = json!([{"node": 4, "param_name": "transform.t.x"}]);
    result(&request(&mut s, json!({"op": "binding.copy", "parameter": 40, "key": [0, 0], "bindings": tx})));
    one_undo_step(&mut s, json!({"op": "binding.paste", "parameter": 40, "key": [1, 0], "ratio": 0.25}));
    assert_eq!(binding_of(&params(&s)[0], 4, "transform.t.x")["values"], json!([[1.0], [1.75]]));
    rejected(&mut s, json!({"op": "binding.paste", "parameter": 40, "key": [1, 0], "ratio": 1.5}));

    // Invert coordinates: the copied shape mirrored about the centre of the rest bounds (x = 2).
    let zero = [[0.0, 0.0]; 3];
    let bend = plain(20, "Bend", json!([binding(
        6, "deform",
        json!([[cell(&zero)], [cell(&zero)], [cell(&[[1.0, 0.0], [0.0, 0.0], [0.0, 1.0]])]]),
        json!([[true], [false], [true]]),
    )]));
    let mut s = session_with(vec![part(6, "Card", json!([0.0, 0.0, 4.0, 0.0, 0.0, 2.0]), json!([0, 1, 2]))], json!([bend]));
    let deform = json!([{"node": 6, "param_name": "deform"}]);
    result(&request(&mut s, json!({"op": "binding.copy", "parameter": 20, "key": [2, 0], "bindings": deform})));
    one_undo_step(&mut s, json!({"op": "binding.paste", "parameter": 20, "key": [1, 0], "invert": {"axis": "vertical"}}));
    let values = binding_of(&params(&s)[0], 6, "deform")["values"].clone();
    assert_eq!(values[1], json!([cell(&[[3.0, 0.0], [-4.0, 0.0], [4.0, 1.0]])]), "{values}");
    rejected(&mut s, json!({"op": "binding.paste", "parameter": 20, "key": [1, 0], "invert": {"axis": "up"}}));
}

#[test]
fn multi_key_editing_changes_every_set_key_of_the_chosen_channel() {
    let card = part(6, "Card", json!([0.0, 0.0, 4.0, 0.0, 0.0, 2.0]), json!([0, 1, 2]));
    let look = plain(20, "Look", json!([
        scalar_binding(6, "opacity", &[0.5, 0.0, 0.8], &[true, false, true]),
        scalar_binding(6, "tint.r", &[0.5, 0.5, 1.0], &[true, true, true]),
        scalar_binding(6, "tint.g", &[1.0, 1.0, 1.0], &[true, true, true]),
        scalar_binding(6, "tint.b", &[1.0, 1.0, 1.0], &[true, true, true]),
    ]));
    let mut s = session_with(vec![card], json!([look]));
    let reply = one_undo_step(&mut s, json!({"op": "binding.multi_edit", "nodes": [6], "channel": "opacity",
        "value": 0.3, "method": "add"}));
    assert_eq!(reply["changed"], 2, "{reply}");
    let opacity = binding_of(&params(&s)[0], 6, "opacity")["values"].clone();
    assert_eq!((opacity[0][0].as_f64().unwrap() * 1e9).round(), 0.8e9);
    assert_eq!(opacity[2], json!([1.0]));
    one_undo_step(&mut s, json!({"op": "binding.multi_edit", "nodes": [6], "channel": "tint",
        "value": [0.5, 1.0, 1.0], "method": "multiply"}));
    assert_eq!(binding_of(&params(&s)[0], 6, "tint.r")["values"], json!([[0.25], [0.25], [0.5]]));
    let before = s.snapshot();
    let none = result(&request(&mut s, json!({"op": "binding.multi_edit", "nodes": [4], "channel": "opacity",
        "value": 1.0}))).clone();
    assert_eq!(none["changed"], 0);
    assert_eq!(s.snapshot()["model"], before["model"]);
    rejected(&mut s, json!({"op": "binding.multi_edit", "nodes": [6], "channel": "opacity", "value": 1.0,
        "method": "screen"}));
    rejected(&mut s, json!({"op": "binding.multi_edit", "nodes": [6], "channel": "tint", "value": 1.0}));
}

#[test]
fn a_weight_limit_scales_a_deform_binding_by_another_parameter() {
    let zero = [[0.0, 0.0]; 3];
    let bend = plain(20, "Bend", json!([binding(
        6, "deform",
        json!([[cell(&zero)], [cell(&zero)], [cell(&[[2.0, 0.0], [2.0, 0.0], [2.0, 0.0]])]]),
        json!([[true], [false], [true]]),
    )]));
    let card = part(6, "Card", json!([0.0, 0.0, 4.0, 0.0, 0.0, 2.0]), json!([0, 1, 2]));
    let mut s = session_with(vec![card], json!([bend, plain(21, "Mouth", json!([]))]));
    let offset = |s: &mut Session, mouth: f64| {
        let scene = result(&request(s, json!({"op": "scene.evaluate", "values": {"20": [1.0, 0.0], "21": [mouth, 0.0]}}))).clone();
        scene["localOffsets"]["6"][0][0].as_f64().unwrap()
    };
    assert_eq!(offset(&mut s, 0.5), 2.0);
    let limits = json!([{"parameter": 21, "points": [[0.0, 1.0], [1.0, 0.0]]}]);
    one_undo_step(&mut s, json!({"op": "binding.weight_limits.set", "parameter": 20, "node": 6, "limits": limits}));
    assert_eq!(offset(&mut s, 0.5), 1.0);
    assert_eq!(offset(&mut s, -1.0), 2.0);
    assert_eq!(offset(&mut s, 1.0), 0.0);
    // The smallest of two limits applies.
    let two = json!([{"parameter": 21, "points": [[0.0, 1.0], [1.0, 0.0]]}, {"parameter": 21, "points": [[0.0, 0.25], [1.0, 0.25]]}]);
    one_undo_step(&mut s, json!({"op": "binding.weight_limits.set", "parameter": 20, "node": 6, "limits": two}));
    assert_eq!(offset(&mut s, 0.0), 0.5);
    for bad in [json!([{"parameter": 20, "points": [[0.0, 1.0], [1.0, 0.0]]}]),
        json!([{"parameter": 21, "points": [[0.0, 1.0]]}]),
        json!([{"parameter": 21, "points": [[1.0, 1.0], [0.0, 0.0]]}]),
        json!([{"parameter": 21, "points": [[0.0, 1.5], [1.0, 0.0]]}])] {
        rejected(&mut s, json!({"op": "binding.weight_limits.set", "parameter": 20, "node": 6, "limits": bad}));
    }
}

#[test]
fn reflecting_a_key_flips_a_rotation_deformer_position_and_angle() {
    let mut pivot = node(6, "Shoulder");
    pivot["transform"]["trans"] = json!([3.0, 0.0, 0.0]);
    pivot["rotationDeformer"] = json!({"handleLength": 100.0});
    let swing = plain(20, "Swing", json!([
        scalar_binding(6, "transform.t.x", &[0.0, 0.0, 1.0], &[true, false, true]),
        scalar_binding(6, "transform.r.z", &[0.0, 0.0, 0.5], &[true, false, true]),
    ]));
    let mut s = session_with(vec![pivot], json!([swing]));
    result(&request(&mut s, json!({"op": "binding.copy", "parameter": 20, "key": [2, 0]})));
    one_undo_step(&mut s, json!({"op": "binding.paste", "parameter": 20, "key": [2, 0], "invert": {"axis": "vertical"}}));
    // Position 3 + 1 = 4 reflected about 0 is -4, an offset of -7 from the rest 3.
    assert_eq!(binding_of(&params(&s)[0], 6, "transform.t.x")["values"][2], json!([-7.0]));
    assert_eq!(binding_of(&params(&s)[0], 6, "transform.r.z")["values"][2], json!([-0.5]));
}

#[test]
fn copy_to_and_swap_with_work_on_compatible_nodes_only() {
    let mut s = binding_fixture();
    let ps = params(&s);
    let sel = vec![(4, "transform.t.x".to_owned()), (4, "transform.t.y".to_owned())];
    let model = s.snapshot()["model"].clone();
    assert_eq!(soul2d_core::parameter_flip::compatible_nodes(&model, 40, &sel), vec![5]);
    assert_eq!(soul2d_core::parameter_flip::compatible_nodes(&model, 40, &[(4, "transform.t.x".to_owned())]), vec![5]);
    assert_eq!(soul2d_core::parameter_flip::compatible_nodes(&model, 40, &[(9, "transform.t.x".to_owned())]), Vec::<u32>::new());
    drop(ps);

    let both = json!([{"node": 4, "param_name": "transform.t.x"}, {"node": 4, "param_name": "transform.t.y"}]);
    one_undo_step(&mut s, json!({
        "op": "binding.copy_to", "parameter": 40, "key": [0, 0], "bindings": both, "target": 5
    }));
    let pose = params(&s)[0].clone();
    assert_eq!(binding_of(&pose, 5, "transform.t.x")["values"], json!([[1.0], [20.0]]));
    let ty = binding_of(&pose, 5, "transform.t.y");
    assert_eq!(ty["values"], json!([[3.0], [3.0]]));
    assert_eq!(ty["isSet"], json!([[true], [false]]));

    let mut s = binding_fixture();
    one_undo_step(&mut s, json!({
        "op": "binding.swap_with", "parameter": 40, "key": [0, 0],
        "bindings": [{"node": 4, "param_name": "transform.t.x"}], "target": 5
    }));
    let pose = params(&s)[0].clone();
    assert_eq!(binding_of(&pose, 4, "transform.t.x")["values"], json!([[10.0], [2.0]]));
    assert_eq!(binding_of(&pose, 5, "transform.t.x")["values"], json!([[1.0], [20.0]]));

    let one = json!([{"node": 4, "param_name": "transform.t.x"}]);
    rejected(&mut s, json!({"op": "binding.copy_to", "parameter": 40, "key": [0, 0], "bindings": one, "target": 4}));
    rejected(&mut s, json!({"op": "binding.copy_to", "parameter": 40, "key": [0, 0], "bindings": one, "target": 99}));
    rejected(&mut s, json!({"op": "binding.swap_with", "parameter": 40, "key": [0, 0], "bindings": one}));
    rejected(&mut s, json!({"op": "binding.swap_with", "parameter": 40, "key": [0, 0], "bindings": [
        {"node": 4, "param_name": "transform.t.x"}, {"node": 5, "param_name": "transform.t.x"}], "target": 4}));
}

#[test]
fn compatibility_follows_has_param_and_vertex_counts() {
    // opacity exists on parts only; a deformation fits a deformable with the same vertex count.
    let offsets = [[1.0, 1.0], [1.0, 1.0], [1.0, 1.0], [1.0, 1.0]];
    let fade = |node: u64| scalar_binding(node, "opacity", &[1.0, 0.5, 0.0], &[true, true, true]);
    let p = plain(20, "Fade", json!([
        fade(6),
        scalar_binding(5, "transform.t.x", &[1.0, 0.5, 0.0], &[true, true, true]),
        fade(7),
        fade(8),
        binding(6, "deform", json!([[cell(&offsets)], [cell(&offsets)], [cell(&offsets)]]), json!([[true], [true], [true]])),
    ]));
    let triangle = part(8, "Tri", json!([0.0, 0.0, 1.0, 0.0, 0.0, 1.0]), json!([0, 1, 2]));
    let s = session_with(vec![left_mesh(), right_mesh(), triangle], json!([p]));
    let model = s.snapshot()["model"].clone();
    let compat = |bindings: &[(u32, &str)]| {
        let owned: Vec<(u32, String)> = bindings.iter().map(|(n, c)| (*n, (*c).to_owned())).collect();
        soul2d_core::parameter_flip::compatible_nodes(&model, 20, &owned)
    };
    // The plain Node 5 has no opacity; both other parts do.
    assert_eq!(compat(&[(6, "opacity")]), vec![7, 8]);
    // Only the four-vertex part fits the four-vertex deformation.
    assert_eq!(compat(&[(6, "deform")]), vec![7]);
    assert_eq!(compat(&[(6, "opacity"), (6, "deform")]), vec![7]);
    assert_eq!(compat(&[(5, "transform.t.x")]), vec![6, 7, 8]);
    // A selection that spans two nodes offers nothing.
    assert_eq!(compat(&[(6, "opacity"), (7, "opacity")]), Vec::<u32>::new());
}

// ------------------------------------------------------------------------------------------ To 2D

#[test]
fn to_2d_keeps_the_uuid_and_x_axis_and_puts_the_keys_on_the_first_y_point() {
    let one_d = parameter(
        30, "Smile", false, [0.0, 0.0], [1.0, 1.0], json!([[0.0, 0.25, 1.0], [0.0]]),
        json!([scalar_binding(4, "transform.t.x", &[1.0, 2.0, 5.0], &[true, false, true])]),
    );
    let mut s = session_with(vec![], json!([one_d]));
    let converted = one_undo_step(&mut s, json!({"op": "parameter.convert_to_2d", "id": 30}));
    assert_eq!(converted["uuid"], 30);
    assert_eq!(converted["name"], "Smile");
    assert_eq!(converted["is_vec2"], true);
    assert_eq!(converted["min"], json!([0.0, 0.0]));
    assert_eq!(converted["max"], json!([1.0, 1.0]));
    assert_eq!(converted["axis_points"], json!([[0.0, 0.25, 1.0], [0.0, 1.0]]));
    assert_eq!(converted["merge_mode"], "Additive");
    let tx = binding_of(&converted, 4, "transform.t.x");
    assert_eq!(tx["isSet"], json!([[true, false], [false, false], [true, false]]));
    assert_eq!(tx["values"], json!([[1.0, 1.0], [2.0, 2.0], [5.0, 5.0]]));
    assert_eq!(params(&s)[0], converted);
    // The parameter keeps working as a 2D one.
    result(&request(&mut s, json!({"op": "value.set", "id": 30, "value": [0.5, 1.0]})));

    // The minimum of X is fed to D's key lookup as an offset: 0.75 selects the second Y point.
    let shifted = parameter(
        30, "Smile", false, [0.75, 0.0], [1.75, 1.0], json!([[0.0, 1.0], [0.0]]),
        json!([scalar_binding(4, "transform.t.x", &[1.0, 3.0], &[true, true])]),
    );
    let mut s = session_with(vec![], json!([shifted]));
    let converted = one_undo_step(&mut s, json!({"op": "parameter.convert_to_2d", "id": 30}));
    assert_eq!(converted["min"], json!([0.75, 0.75]));
    let tx = binding_of(&converted, 4, "transform.t.x");
    assert_eq!(tx["isSet"], json!([[false, true], [false, true]]));
    assert_eq!(tx["values"], json!([[1.0, 1.0], [3.0, 3.0]]));
}

#[test]
fn to_2d_of_a_grouped_parameter_moves_it_to_the_end_of_its_group_and_rejects_bad_targets() {
    let group = json!({"groupUUID": 50, "name": "Face", "color": [0.15, 0.15, 0.15], "children": [
        plain(30, "Smile", json!([scalar_binding(4, "transform.t.x", &[1.0, 2.0, 3.0], &[true, true, true])])),
        plain(31, "Frown", json!([])),
    ]});
    let mut s = session_with(vec![], json!([group, plain(32, "Top", json!([]))]));
    one_undo_step(&mut s, json!({"op": "parameter.convert_to_2d", "id": 30}));
    let children = params(&s)[0]["children"].clone();
    assert_eq!(children[0]["uuid"], 31);
    assert_eq!(children[1]["uuid"], 30);
    assert_eq!(children[1]["is_vec2"], true);
    assert_eq!(children[1]["axis_points"], json!([[0.0, 0.5, 1.0], [0.0, 1.0]]));

    rejected(&mut s, json!({"op": "parameter.convert_to_2d", "id": 30}));
    rejected(&mut s, json!({"op": "parameter.convert_to_2d", "id": 50}));
    rejected(&mut s, json!({"op": "parameter.convert_to_2d", "id": 99}));
    rejected(&mut s, json!({"op": "parameter.convert_to_2d"}));

    // A top-level parameter stays where it is.
    one_undo_step(&mut s, json!({"op": "parameter.convert_to_2d", "id": 32}));
    assert_eq!(params(&s)[1]["uuid"], 32);
    assert_eq!(params(&s)[1]["is_vec2"], true);
}

// ------------------------------------------------------------------------------------------ groups, MCP

#[test]
fn group_rename_is_covered_by_parameter_rename() {
    let group = json!({"groupUUID": 50, "name": "Face", "color": [0.15, 0.15, 0.15], "children": []});
    let mut s = session_with(vec![], json!([group]));
    one_undo_step(&mut s, json!({"op": "parameter.rename", "id": 50, "name": "Head"}));
    assert_eq!(params(&s)[0]["name"], "Head");
}

#[test]
fn the_new_tools_are_reachable_over_mcp() {
    let mut s = paste_fixture();
    let call = |session: &mut Session, name: &str, arguments: Value| {
        session
            .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": name, "arguments": arguments}}))
            .unwrap()
    };
    let ok = |reply: &Value| reply["result"]["isError"] == false;
    assert!(ok(&call(&mut s, "parameter_copy", json!({"id": 20}))));
    assert!(ok(&call(&mut s, "parameter_paste", json!({"id": 21, "flip": true}))));
    assert_eq!(binding_of(&params(&s)[1], 5, "transform.t.x")["values"], json!([[-1.0], [-2.0], [-3.0]]));
    assert!(ok(&call(&mut s, "parameter_duplicate_flip", json!({"id": 20}))));
    assert!(ok(&call(&mut s, "parameter_mirrored_autofill", json!({"id": 20, "direction": 1}))));
    assert!(ok(&call(&mut s, "parameter_convert_to_2d", json!({"id": 22}))));
    assert_eq!(params(&s)[2]["is_vec2"], true);
    let key = json!([0, 0]);
    assert!(ok(&call(&mut s, "binding_copy", json!({"parameter": 20, "key": key}))));
    assert!(ok(&call(&mut s, "binding_paste", json!({"parameter": 21, "key": [1, 0]}))));
    let one = json!([{"node": 4, "param_name": "transform.t.x"}]);
    assert!(ok(&call(&mut s, "binding_copy_to", json!({"parameter": 20, "key": key, "bindings": one, "target": 5}))));
    assert!(ok(&call(&mut s, "binding_swap_with", json!({"parameter": 20, "key": key, "bindings": one, "target": 5}))));
    assert!(ok(&call(&mut s, "binding_keypoint", json!({
        "parameter": 20, "key": key, "action": "flip_deform"}))));
    assert!(!ok(&call(&mut s, "parameter_paste", json!({"id": 21}))));
}

#[test]
fn corner_synthesis_adds_the_edge_keys_and_subtracts_the_reference() {
    let lines = binding(2, "transform.t.x", json!([[0.0, -10.0, 0.0], [5.0, 1.0, -5.0], [0.0, 10.0, 0.0]]),
        json!([[false, true, false], [true, true, true], [false, true, false]]));
    let partial = binding(2, "transform.t.y", json!([[0.0, 3.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]]),
        json!([[false, true, false], [false, true, false], [false, false, false]]));
    let axes = json!([[0.0, 0.5, 1.0], [0.0, 0.5, 1.0]]);
    let face = parameter(30, "Angle", true, [-1.0, -1.0], [1.0, 1.0], axes, json!([lines, partial]));
    let mut session = session_with(vec![node(2, "Head")], json!([face]));
    let reply = one_undo_step(&mut session, json!({"op": "parameter.synthesize_corners", "id": 30}));
    assert_eq!(reply["reference"], json!([1, 1]));
    assert_eq!(reply["written"], 4);
    assert_eq!(reply["skipped"], json!([{"node": 2, "param_name": "transform.t.y"}]));
    let x = binding_of(find(&params(&session), 30), 2, "transform.t.x").clone();
    // Corner (0, 0) = column -10 + row 5 - reference 1; corner (2, 2) = 10 - 5 - 1.
    assert_eq!(x["values"][0][0], json!(-6.0));
    assert_eq!(x["values"][2][2], json!(4.0));
    assert_eq!(x["isSet"][2][0], true);
    let plain_parameter = plain(31, "Plain", json!([]));
    let mut session = session_with(vec![node(2, "Head")], json!([plain_parameter]));
    assert!(rejected(&mut session, json!({"op": "parameter.synthesize_corners", "id": 31})).contains("2D parameter"));
}
