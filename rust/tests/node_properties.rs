//! Inspector property edits: `node.property.set`, `node.mask.edit`, `puppet.meta.set`, and the existing
//! `physics.set` / `deformer.set` operations the Inspector reuses. Stored keys follow the
//! `serializeSelfImpl` of the engine node classes.

mod common;

use serde_json::{json, Value};
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn ok(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn transform() -> Value {
    json!({"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]})
}

fn node(uuid: u64, name: &str, node_type: &str, extra: Value) -> Value {
    let mut entry = json!({
        "uuid": uuid, "name": name, "type": node_type, "enabled": true,
        "zsort": 0.0, "transform": transform(),
        "lockToRoot": false, "pinToMesh": false, "children": [],
    });
    for (key, value) in extra.as_object().cloned().unwrap_or_default() {
        entry[key] = value;
    }
    entry
}

fn mesh() -> Value {
    json!({"verts": [0.0, 0.0, 4.0, 0.0, 4.0, 4.0], "uvs": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0],
        "indices": [0, 1, 2], "origin": [0.0, 0.0]})
}

/// Ids: 1 root, 2 and 3 Parts, 4 Composite, 5 DynamicComposite, 6 MeshGroup, 7 Camera, 8 Mask,
/// 9 SimplePhysics, 10 PathDeformer, 11 GridDeformer, 12 Node.
fn fixture() -> Value {
    let part = |id, name| {
        node(id, name, "Part", json!({
            "mesh": mesh(), "textures": [], "blend_mode": "Normal", "tint": [1.0, 1.0, 1.0],
            "screenTint": [0.0, 0.0, 0.0], "emissionStrength": 1.0, "mask_threshold": 0.5, "opacity": 1.0,
        }))
    };
    let mut root = node(1, "Root", "Node", json!({}));
    root["children"] = json!([
        part(2, "Face"),
        part(3, "Hair"),
        node(4, "Group", "Composite", json!({
            "blend_mode": "Normal", "tint": [1.0, 1.0, 1.0], "screenTint": [0.0, 0.0, 0.0],
            "mask_threshold": 0.5, "opacity": 1.0, "propagate_meshgroup": true,
        })),
        node(5, "Dynamic", "DynamicComposite", json!({
            "blend_mode": "Normal", "tint": [1.0, 1.0, 1.0], "screenTint": [0.0, 0.0, 0.0],
            "emissionStrength": 1.0, "mask_threshold": 0.5, "opacity": 1.0, "auto_resized": false,
        })),
        node(6, "Mesh", "MeshGroup", json!({"dynamic_deformation": false, "translate_children": false})),
        node(7, "Cam", "Camera", json!({"viewport": [1920.0, 1080.0]})),
        node(8, "Cutout", "Mask", json!({"mesh": mesh()})),
        node(9, "Hair physics", "SimplePhysics", json!({
            "param": 4294967295u64, "model_type": "Pendulum", "map_mode": "AngleLength",
            "gravity": 1.0, "length": 100.0, "frequency": 1.0, "angle_damping": 0.5,
            "length_damping": 0.5, "output_scale": [1.0, 1.0], "local_only": false,
        })),
        node(10, "Path", "PathDeformer", json!({
            "curve_type": "Spline", "vertices": [0.0, 0.0, 10.0, 0.0], "dynamic_deformation": false,
        })),
        node(11, "Grid", "GridDeformer", json!({
            "grid_axis_x": [0.0, 1.0], "grid_axis_y": [0.0, 1.0], "formation": "Bilinear", "dynamic": false,
        })),
        node(12, "Plain", "Node", json!({})),
    ]);
    json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "physics": {"pixelsPerMeter": 1000.0, "gravity": 9.8},
        "nodes": root,
        "param": [],
        "animations": {},
    })
}

fn session() -> Session {
    let mut session = Session::new();
    ok(&request(&mut session, json!({"op": "new", "model": fixture()})));
    session
}

fn find(node: &Value, id: u64) -> Option<&Value> {
    if node["uuid"].as_u64() == Some(id) {
        return Some(node);
    }
    node["children"].as_array()?.iter().find_map(|child| find(child, id))
}

fn model_node(session: &Session, id: u64) -> Value {
    find(&session.snapshot()["model"]["nodes"], id).unwrap_or_else(|| panic!("node {id} missing")).clone()
}

fn set(session: &mut Session, id: u64, property: &str, value: Value) -> Value {
    request(session, json!({"op": "node.property.set", "id": id, "property": property, "value": value}))
}

/// Set a property, check the stored JSON equals `expected`, that nothing else in the node changed, and
/// that one undo restores the whole document.
fn assert_property(id: u64, property: &str, value: Value, expected: Value) {
    let mut session = session();
    let before = session.snapshot();
    let response = set(&mut session, id, property, value);
    assert_eq!(ok(&response)["value"], expected, "{id} {property}");
    let mut want = find(&before["model"]["nodes"], id).unwrap().clone();
    if property == "origin" {
        want["mesh"]["origin"] = expected.clone();
    } else {
        want[property] = expected.clone();
    }
    assert_eq!(model_node(&session, id), want, "{id} {property}: only the property may change");
    let after = session.snapshot();
    assert_eq!(after["history"]["count"].as_u64(), before["history"]["count"].as_u64().map(|n| n + 1));
    ok(&request(&mut session, json!({"op": "undo"})));
    assert_eq!(session.snapshot()["model"], before["model"], "{id} {property}: undo");
}

#[test]
fn every_property_stores_its_literal_json_and_undoes_in_one_step() {
    // (node, property, request value, stored JSON)
    let cases = [
        (12, "lockToRoot", json!(true), json!(true)),
        (12, "pinToMesh", json!(true), json!(true)),
        (12, "zsort", json!(2.5), json!(2.5)),
        (2, "zsort", json!(-3), json!(-3.0)),
        (4, "lockToRoot", json!(true), json!(true)),
        (7, "pinToMesh", json!(true), json!(true)),
        (2, "tint", json!([0.25, 0.5, 0.75]), json!([0.25, 0.5, 0.75])),
        (2, "screenTint", json!([0.5, 0.0, 1.0]), json!([0.5, 0.0, 1.0])),
        (2, "emissionStrength", json!(0.75), json!(0.75)),
        (2, "blend_mode", json!("SliceFromLower"), json!("SliceFromLower")),
        (2, "alphaBlend", json!("Atop"), json!("Atop")),
        (2, "opacity", json!(0.5), json!(0.5)),
        (2, "mask_threshold", json!(0.25), json!(0.25)),
        (5, "tint", json!([0.5, 0.5, 0.5]), json!([0.5, 0.5, 0.5])),
        (5, "screenTint", json!([0.1, 0.2, 0.3]), json!([0.1, 0.2, 0.3])),
        (5, "emissionStrength", json!(2), json!(2.0)),
        (5, "blend_mode", json!("ClipToLower"), json!("ClipToLower")),
        (5, "alphaBlend", json!("Conjoint"), json!("Conjoint")),
        (5, "opacity", json!(0.0), json!(0.0)),
        (5, "mask_threshold", json!(1.0), json!(1.0)),
        (5, "auto_resized", json!(true), json!(true)),
        (4, "tint", json!([0.0, 1.0, 0.5]), json!([0.0, 1.0, 0.5])),
        (4, "screenTint", json!([1.0, 1.0, 1.0]), json!([1.0, 1.0, 1.0])),
        (4, "blend_mode", json!("DestinationIn"), json!("DestinationIn")),
        (4, "alphaBlend", json!("Disjoint"), json!("Disjoint")),
        (4, "alphaBlend", json!("Out"), json!("Out")),
        (4, "opacity", json!(0.125), json!(0.125)),
        (4, "mask_threshold", json!(0.75), json!(0.75)),
        (4, "propagate_meshgroup", json!(false), json!(false)),
        (7, "viewport", json!([1280, 720]), json!([1280.0, 720.0])),
        (8, "origin", json!([3.5, -2.0]), json!([3.5, -2.0])),
        (2, "origin", json!([1, 2]), json!([1.0, 2.0])),
    ];
    for (id, property, value, expected) in cases {
        assert_property(id, property, value, expected);
    }
}

#[test]
fn every_blend_mode_is_accepted_by_a_part_and_composite_stops_at_destination_in() {
    let modes = [
        "Normal", "Multiply", "Screen", "Overlay", "Darken", "Lighten", "ColorDodge", "LinearDodge", "AddGlow",
        "ColorBurn", "HardLight", "SoftLight", "Difference", "Exclusion", "Subtract", "LinearBurn", "LinearLight",
        "Hue", "Color", "Inverse", "DestinationIn", "ClipToLower", "SliceFromLower",
    ];
    let mut session = session();
    for (index, mode) in modes.iter().enumerate() {
        ok(&set(&mut session, 2, "blend_mode", json!(mode)));
        assert_eq!(model_node(&session, 2)["blend_mode"], json!(mode));
        let composite = set(&mut session, 4, "blend_mode", json!(mode));
        assert_eq!(composite["succeeded"], json!(index < 21), "Composite {mode}");
    }
}

#[test]
fn an_alpha_blend_is_refused_with_a_blend_mode_that_has_no_cubism_formula() {
    let mut session = session();
    for (id, kind) in [(2, "Part"), (4, "Composite"), (5, "DynamicComposite")] {
        ok(&set(&mut session, id, "blend_mode", json!("Difference")));
        let before = session.snapshot();
        let refused = set(&mut session, id, "alphaBlend", json!("Atop"));
        assert_eq!(
            refused["message"],
            format!("{kind} node {id} blends as Difference, which has no Cubism alpha blend; alphaBlend must be Over")
        );
        assert_eq!(session.snapshot(), before);
        ok(&set(&mut session, id, "alphaBlend", json!("Over")));

        ok(&set(&mut session, id, "blend_mode", json!("Multiply")));
        ok(&set(&mut session, id, "alphaBlend", json!("Out")));
        let before = session.snapshot();
        assert_eq!(set(&mut session, id, "blend_mode", json!("Subtract"))["succeeded"], false, "{kind}");
        assert_eq!(session.snapshot(), before);
        ok(&set(&mut session, id, "blend_mode", json!("Hue")));
        assert_eq!(model_node(&session, id)["alphaBlend"], json!("Out"));
    }
    assert_eq!(
        set(&mut session, 2, "alphaBlend", json!("atop"))["message"],
        "alphaBlend must be one of Over, Atop, Out, Conjoint, Disjoint"
    );
    // A convert to a type without an alpha blend drops the key, also when it keeps the other blend keys.
    ok(&request(&mut session, json!({"op": "node.convert", "nodes": [4], "className": "Node"})));
    assert_eq!(model_node(&session, 4).get("alphaBlend"), None);
    for class in ["Mask", "MeshGroup"] {
        ok(&request(&mut session, json!({"op": "node.convert", "nodes": [5], "className": class})));
        let node = model_node(&session, 5);
        assert_eq!((node.get("alphaBlend"), node.get("blend_mode")), (None, Some(&json!("Hue"))), "{class}");
        ok(&request(&mut session, json!({"op": "undo"})));
        assert_eq!(model_node(&session, 5)["alphaBlend"], json!("Out"));
    }
}

#[test]
fn viewport_is_normalised_like_ex_camera() {
    let cases = [
        (json!([1921, 1079]), json!([1922.0, 1080.0])),
        (json!([0, 1]), json!([2.0, 2.0])),
        (json!([-640, 359.4]), json!([640.0, 360.0])),
        (json!([100.5, 3]), json!([102.0, 4.0])),
    ];
    let mut session = session();
    for (value, expected) in cases {
        ok(&set(&mut session, 7, "viewport", value.clone()));
        assert_eq!(model_node(&session, 7)["viewport"], expected, "{value}");
    }
}

#[test]
fn illegal_edits_are_rejected_and_leave_the_document_unchanged() {
    let cases = [
        (2, "opacity", json!(1.5)),
        (2, "opacity", json!(-0.1)),
        (2, "opacity", json!("1")),
        (2, "opacity", Value::Null),
        (2, "mask_threshold", json!(2)),
        (2, "tint", json!([1.0, 1.0])),
        (2, "tint", json!([1.0, 1.0, 2.0])),
        (2, "tint", json!([1.0, 1.0, "x"])),
        (2, "tint", json!(0.5)),
        (2, "emissionStrength", json!(-1)),
        (2, "blend_mode", json!("Bogus")),
        (2, "blend_mode", json!(3)),
        (2, "blend_mode", json!("normal")),
        (4, "blend_mode", json!("ClipToLower")),
        (2, "alphaBlend", json!("atop")),
        (2, "alphaBlend", json!("ConjointOver")),
        (2, "alphaBlend", json!(1)),
        (8, "alphaBlend", json!("Atop")),
        (12, "alphaBlend", json!("Atop")),
        (2, "lockToRoot", json!("true")),
        (2, "lockToRoot", json!(1)),
        (12, "zsort", json!("1")),
        (2, "auto_resized", json!(true)),
        (4, "auto_resized", json!(true)),
        (2, "propagate_meshgroup", json!(true)),
        (5, "propagate_meshgroup", json!(true)),
        (2, "viewport", json!([1920, 1080])),
        (7, "viewport", json!([1920])),
        (7, "viewport", json!("1920x1080")),
        (2, "origin", json!("0,0")),
        (8, "origin", json!([1])),
        (12, "origin", json!([0, 0])),
        (4, "origin", json!([0, 0])),
        // Stores no mesh, so there is no origin to edit.
        (5, "origin", json!([0, 0])),
        (6, "origin", json!([0, 0])),
        (12, "opacity", json!(0.5)),
        (7, "tint", json!([0, 0, 0])),
        (6, "opacity", json!(0.5)),
        (9, "opacity", json!(0.5)),
        (2, "unknown", json!(1)),
        (2, "pixelSnap", json!(true)),
        (1, "lockToRoot", json!(true)),
        (1, "zsort", json!(1)),
        (99, "opacity", json!(0.5)),
    ];
    let mut session = session();
    let before = session.snapshot();
    for (id, property, value) in cases {
        let response = set(&mut session, id, property, value.clone());
        assert_eq!(response["succeeded"], false, "{id} {property} {value} was accepted: {response}");
        assert_eq!(session.snapshot(), before, "{id} {property} {value} changed the document");
    }
    for body in [
        json!({"op": "node.property.set", "property": "opacity", "value": 0.5}),
        json!({"op": "node.property.set", "id": 0, "property": "opacity", "value": 0.5}),
        json!({"op": "node.property.set", "id": 2, "value": 0.5}),
        json!({"op": "node.property.set", "id": 2, "property": "opacity"}),
        json!({"op": "node.property.set", "id": 2, "property": 3, "value": 0.5}),
    ] {
        assert_eq!(request(&mut session, body.clone())["succeeded"], false, "{body}");
        assert_eq!(session.snapshot(), before);
    }
}

#[test]
fn setting_the_current_value_adds_no_history_step() {
    let mut session = session();
    let before = session.snapshot();
    ok(&set(&mut session, 2, "opacity", json!(1.0)));
    assert_eq!(session.snapshot()["history"]["count"], before["history"]["count"]);
}

fn mask_edit(session: &mut Session, id: u64, body: Value) -> Value {
    let mut body = body;
    body["op"] = json!("node.mask.edit");
    body["id"] = json!(id);
    request(session, body)
}

#[test]
fn masks_add_remove_change_mode_and_reorder_with_one_undo_each() {
    let mut session = session();
    let base = session.snapshot();
    let added = mask_edit(&mut session, 2, json!({"action": "add", "source": 8}));
    assert_eq!(ok(&added)["masks"], json!([{"source": 8, "mode": "Mask"}]));
    ok(&mask_edit(&mut session, 2, json!({"action": "add", "source": 3, "mode": "DodgeMask"})));
    assert_eq!(model_node(&session, 2)["masks"], json!([
        {"source": 8, "mode": "Mask"}, {"source": 3, "mode": "DodgeMask"}
    ]));
    ok(&mask_edit(&mut session, 2, json!({"action": "mode", "source": 3, "mode": "Mask"})));
    assert_eq!(model_node(&session, 2)["masks"][1], json!({"source": 3, "mode": "Mask"}));
    ok(&mask_edit(&mut session, 2, json!({"action": "move", "source": 3, "index": 0})));
    assert_eq!(model_node(&session, 2)["masks"], json!([
        {"source": 3, "mode": "Mask"}, {"source": 8, "mode": "Mask"}
    ]));
    ok(&mask_edit(&mut session, 2, json!({"action": "remove", "source": 3})));
    assert_eq!(model_node(&session, 2)["masks"], json!([{"source": 8, "mode": "Mask"}]));
    assert_eq!(session.snapshot()["history"]["count"].as_u64(), base["history"]["count"].as_u64().map(|n| n + 5));

    for expected in [
        json!([{"source": 3, "mode": "Mask"}, {"source": 8, "mode": "Mask"}]),
        json!([{"source": 8, "mode": "Mask"}, {"source": 3, "mode": "Mask"}]),
        json!([{"source": 8, "mode": "Mask"}, {"source": 3, "mode": "DodgeMask"}]),
        json!([{"source": 8, "mode": "Mask"}]),
    ] {
        ok(&request(&mut session, json!({"op": "undo"})));
        assert_eq!(model_node(&session, 2)["masks"], expected);
    }
    ok(&request(&mut session, json!({"op": "undo"})));
    assert_eq!(session.snapshot()["model"], base["model"]);

    // Composite and DynamicComposite carry the same list.
    ok(&mask_edit(&mut session, 4, json!({"action": "add", "source": 2})));
    ok(&mask_edit(&mut session, 5, json!({"action": "add", "source": 6, "mode": "DodgeMask"})));
    assert_eq!(model_node(&session, 4)["masks"], json!([{"source": 2, "mode": "Mask"}]));
    assert_eq!(model_node(&session, 5)["masks"], json!([{"source": 6, "mode": "DodgeMask"}]));
}

#[test]
fn illegal_mask_edits_leave_the_document_unchanged() {
    let mut session = session();
    ok(&mask_edit(&mut session, 2, json!({"action": "add", "source": 8})));
    let before = session.snapshot();
    let cases = [
        (2, json!({"action": "add", "source": 2})),
        (2, json!({"action": "add", "source": 8})),
        (2, json!({"action": "add", "source": 12})),
        (2, json!({"action": "add", "source": 10})),
        (2, json!({"action": "add", "source": 99})),
        (2, json!({"action": "add", "source": 3, "mode": "Dodge"})),
        (2, json!({"action": "remove", "source": 3})),
        (2, json!({"action": "mode", "source": 3, "mode": "Mask"})),
        (2, json!({"action": "mode", "source": 8})),
        (2, json!({"action": "move", "source": 3, "index": 0})),
        (2, json!({"action": "move", "source": 8, "index": -1})),
        (2, json!({"action": "rotate", "source": 8})),
        (2, json!({"action": "add"})),
        (12, json!({"action": "add", "source": 2})),
        (7, json!({"action": "add", "source": 2})),
        (8, json!({"action": "add", "source": 2})),
        (99, json!({"action": "add", "source": 2})),
    ];
    for (id, body) in cases {
        let response = mask_edit(&mut session, id, body.clone());
        assert_eq!(response["succeeded"], false, "{id} {body} was accepted: {response}");
        assert_eq!(session.snapshot(), before, "{id} {body} changed the document");
    }
}

#[test]
fn a_legacy_masked_by_list_becomes_masks_on_the_first_edit() {
    let mut model = fixture();
    model["nodes"]["children"][0]["masked_by"] = json!([8]);
    model["nodes"]["children"][0]["mask_mode"] = json!("DodgeMask");
    let mut session = Session::new();
    ok(&request(&mut session, json!({"op": "new", "model": model})));
    ok(&mask_edit(&mut session, 2, json!({"action": "add", "source": 3})));
    let part = model_node(&session, 2);
    assert_eq!(part["masks"], json!([{"source": 8, "mode": "DodgeMask"}, {"source": 3, "mode": "Mask"}]));
    assert!(part.get("masked_by").is_none() && part.get("mask_mode").is_none());
}

#[test]
fn puppet_metadata_is_stored_under_meta_and_physics() {
    let cases = [
        ("name", json!("Aoi"), "meta"),
        ("artist", json!("A, B"), "meta"),
        ("rigger", json!("C"), "meta"),
        ("contact", json!("c@example.org"), "meta"),
        ("licenseURL", json!("https://example.org/license"), "meta"),
        ("copyright", json!("(c) 2026"), "meta"),
        ("reference", json!("https://example.org"), "meta"),
        ("preservePixels", json!(true), "meta"),
        ("pixelsPerMeter", json!(500.0), "physics"),
        ("gravity", json!(0.0), "physics"),
    ];
    for (property, value, section) in cases {
        let mut session = session();
        let before = session.snapshot();
        let response = request(&mut session, json!({"op": "puppet.meta.set", "property": property, "value": value}));
        ok(&response);
        let after = session.snapshot();
        let mut expected = before["model"].clone();
        expected[section][property] = value.clone();
        assert_eq!(after["model"], expected, "{property}");
        assert_eq!(after["model"][section][property], value);
        ok(&request(&mut session, json!({"op": "undo"})));
        assert_eq!(session.snapshot()["model"], before["model"], "{property}: undo");
    }
}

#[test]
fn puppet_metadata_creates_a_missing_section_and_rejects_bad_values() {
    let mut model = fixture();
    model.as_object_mut().unwrap().remove("physics");
    let mut session = Session::new();
    ok(&request(&mut session, json!({"op": "new", "model": model})));
    ok(&request(&mut session, json!({"op": "puppet.meta.set", "property": "gravity", "value": 3.5})));
    assert_eq!(session.snapshot()["model"]["physics"], json!({"gravity": 3.5}));

    let before = session.snapshot();
    let long = "x".repeat(4097);
    let cases = [
        ("name", json!(3)),
        ("name", json!("a\u{0}b")),
        ("name", json!(long)),
        ("preservePixels", json!("yes")),
        ("pixelsPerMeter", json!(0.5)),
        ("pixelsPerMeter", json!("500")),
        ("gravity", json!(-1)),
        ("gravity", Value::Null),
        ("version", json!("2.0")),
        ("nodes", json!({})),
        ("thumbnailId", json!(1)),
    ];
    for (property, value) in cases {
        let response = request(&mut session, json!({"op": "puppet.meta.set", "property": property, "value": value}));
        assert_eq!(response["succeeded"], false, "{property} {value} was accepted");
        assert_eq!(session.snapshot(), before);
    }
    assert_eq!(request(&mut session, json!({"op": "puppet.meta.set", "property": "name"}))["succeeded"], false);
}

#[test]
fn simple_physics_inspector_settings_go_through_physics_set() {
    let mut session = session();
    let before = session.snapshot();
    let cases = [
        (json!({"model_type": "SpringPendulum"}), "model_type", json!("SpringPendulum")),
        (json!({"map_mode": "YX"}), "map_mode", json!("YX")),
        (json!({"local_only": true}), "local_only", json!(true)),
        (json!({"gravity": 2.5}), "gravity", json!(2.5)),
        (json!({"length": 80.0}), "length", json!(80.0)),
        (json!({"frequency": 3.0}), "frequency", json!(3.0)),
        (json!({"angle_damping": 1.5}), "angle_damping", json!(1.5)),
        (json!({"length_damping": 0.25}), "length_damping", json!(0.25)),
        (json!({"output_scale_x": 2.0}), "output_scale", json!([2.0, 1.0])),
        (json!({"output_scale_y": 3.0}), "output_scale", json!([2.0, 3.0])),
    ];
    let steps = cases.len();
    for (body, key, expected) in cases {
        let mut body = body;
        body["op"] = json!("physics.set");
        body["nodes"] = json!([9]);
        ok(&request(&mut session, body));
        assert_eq!(model_node(&session, 9)[key], expected, "{key}");
    }
    for _ in 0..steps {
        ok(&request(&mut session, json!({"op": "undo"})));
    }
    assert_eq!(session.snapshot()["model"], before["model"]);
    let wrong = request(&mut session, json!({"op": "physics.set", "nodes": [2], "gravity": 2.0}));
    assert_eq!(wrong["succeeded"], false);
    let bad = request(&mut session, json!({"op": "physics.set", "nodes": [9], "model_type": "Spring"}));
    assert_eq!(bad["succeeded"], false);
    assert_eq!(session.snapshot()["model"], before["model"]);
}

#[test]
fn deformer_inspector_settings_go_through_deformer_set() {
    let mut session = session();
    let before = session.snapshot();
    let cases = [
        (6, "MeshGroupDynamic", json!(true), "dynamic_deformation", json!(true)),
        (6, "MeshGroupTranslateChildren", json!(true), "translate_children", json!(true)),
        (11, "GridDeformDynamic", json!(true), "dynamic", json!(true)),
        (10, "PathDeformDynamic", json!(true), "dynamic_deformation", json!(true)),
        (10, "PathDeformCurveType", json!("Bezier"), "curve_type", json!("Bezier")),
    ];
    for (id, property, value, key, expected) in cases {
        ok(&request(&mut session, json!({"op": "deformer.set", "nodes": [id], "property": property, "value": value})));
        assert_eq!(model_node(&session, id)[key], expected, "{property}");
    }
    ok(&request(&mut session, json!({
        "op": "deformer.set", "nodes": [10], "property": "PathDeformPhysicsEnabled", "value": true
    })));
    ok(&request(&mut session, json!({
        "op": "deformer.set", "nodes": [10], "property": "PathDeformGravity", "value": 2.25
    })));
    assert_eq!(model_node(&session, 10)["physics"]["gravity"], json!(2.25));
    for _ in 0..7 {
        ok(&request(&mut session, json!({"op": "undo"})));
    }
    assert_eq!(session.snapshot()["model"], before["model"]);
    let wrong = request(&mut session, json!({
        "op": "deformer.set", "nodes": [2], "property": "MeshGroupDynamic", "value": true
    }));
    assert_eq!(wrong["succeeded"], false);
}

fn temp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "soul2d-nodeprops-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    common::scratch(dir.clone());
    dir.join(name)
}

#[test]
fn edited_properties_survive_save_and_reopen() {
    let mut session = session();
    ok(&set(&mut session, 2, "opacity", json!(0.5)));
    ok(&set(&mut session, 7, "viewport", json!([1281, 721])));
    ok(&set(&mut session, 4, "alphaBlend", json!("Disjoint")));
    ok(&mask_edit(&mut session, 2, json!({"action": "add", "source": 8, "mode": "DodgeMask"})));
    ok(&request(&mut session, json!({"op": "puppet.meta.set", "property": "artist", "value": "Aoi"})));
    ok(&request(&mut session, json!({"op": "puppet.meta.set", "property": "pixelsPerMeter", "value": 250.0})));
    let saved = session.snapshot()["model"].clone();
    let path = temp_path("props.inx");
    ok(&request(&mut session, json!({"op": "save", "path": path.to_str().unwrap()})));

    let mut reopened = Session::new();
    ok(&request(&mut reopened, json!({"op": "open", "path": path.to_str().unwrap()})));
    assert_eq!(reopened.snapshot()["model"], saved);
    assert_eq!(model_node(&reopened, 2)["opacity"], json!(0.5));
    assert_eq!(model_node(&reopened, 7)["viewport"], json!([1282.0, 722.0]));
    assert_eq!(model_node(&reopened, 4)["alphaBlend"], json!("Disjoint"));
    assert_eq!(model_node(&reopened, 2)["masks"], json!([{"source": 8, "mode": "DodgeMask"}]));
    assert_eq!(reopened.snapshot()["model"]["meta"]["artist"], json!("Aoi"));
    assert_eq!(reopened.snapshot()["model"]["physics"]["pixelsPerMeter"], json!(250.0));
}

fn call(session: &mut Session, name: &str, arguments: Value) -> Value {
    session
        .handle_mcp(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }))
        .expect("tools/call did not reply")
}

fn reply_json(reply: &Value) -> Value {
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[test]
fn mcp_tools_round_trip_and_reject_without_touching_the_document() {
    let mut session = session();
    let listing = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
        .expect("tools/list did not reply");
    let tools = listing["result"]["tools"].as_array().unwrap();
    let schema = |name: &str| {
        tools.iter().find(|tool| tool["name"] == name).unwrap_or_else(|| panic!("missing tool {name}"))
            ["inputSchema"].clone()
    };
    assert_eq!(schema("node_property_set")["required"], json!(["id", "property", "value"]));
    assert_eq!(schema("node_mask_edit")["required"], json!(["id", "action", "source"]));
    assert_eq!(schema("node_mask_edit")["properties"]["action"]["enum"], json!(["add", "remove", "mode", "move"]));
    assert_eq!(schema("puppet_meta_set")["required"], json!(["property", "value"]));
    assert_eq!(schema("Inspector_Apply_PartOpacity")["required"], json!(["value", "context"]));

    let reply = call(&mut session, "node_property_set", json!({"id": 2, "property": "opacity", "value": 0.5}));
    assert_eq!(reply_json(&reply), json!({"id": 2, "property": "opacity", "value": 0.5}));
    assert_eq!(model_node(&session, 2)["opacity"], json!(0.5));

    let reply = call(&mut session, "node_mask_edit", json!({"id": 2, "action": "add", "source": 8}));
    assert_eq!(reply_json(&reply), json!({"id": 2, "masks": [{"source": 8, "mode": "Mask"}]}));

    let reply = call(&mut session, "puppet_meta_set", json!({"property": "rigger", "value": "Sora"}));
    assert_eq!(reply_json(&reply), json!({"property": "rigger", "value": "Sora"}));
    assert_eq!(session.snapshot()["model"]["meta"]["rigger"], json!("Sora"));

    // D command ids reach the same operations.
    let reply = call(&mut session, "Inspector_Apply_PartTint", json!({"value": [0.5, 0.5, 0.5], "context": {"nodes": [3]}}));
    assert_eq!(reply_json(&reply)["value"], json!([0.5, 0.5, 0.5]));
    assert_eq!(model_node(&session, 3)["tint"], json!([0.5, 0.5, 0.5]));
    let reply = call(&mut session, "Inspector_Apply_PinToMesh", json!({"value": true, "context": {"nodes": [12]}}));
    assert_eq!(reply_json(&reply)["property"], json!("pinToMesh"));
    let reply = call(&mut session, "Inspector_Apply_SimplePhysicsGravity", json!({"value": 4.0, "context": {"nodes": [9]}}));
    reply_json(&reply);
    assert_eq!(model_node(&session, 9)["gravity"], json!(4.0));
    let reply = call(&mut session, "Inspector_Apply_SimplePhysicsOutputScaleY", json!({"value": 2.0, "context": {"nodes": [9]}}));
    reply_json(&reply);
    assert_eq!(model_node(&session, 9)["output_scale"], json!([1.0, 2.0]));

    let before = session.snapshot();
    for (name, arguments) in [
        ("node_property_set", json!({"id": 2, "property": "opacity", "value": 2})),
        ("node_property_set", json!({"id": 2, "property": "nope", "value": 1})),
        ("node_mask_edit", json!({"id": 2, "action": "add", "source": 8})),
        ("node_mask_edit", json!({"id": 12, "action": "add", "source": 8})),
        ("puppet_meta_set", json!({"property": "gravity", "value": -2})),
        ("Inspector_Apply_PartOpacity", json!({"value": 3, "context": {"nodes": [2]}})),
        ("Inspector_Apply_PartOpacity", json!({"value": 0.5, "context": {"nodes": [2, 3]}})),
        ("Inspector_Apply_PartOpacity", json!({"value": 0.5})),
        ("Inspector_Apply_SimplePhysicsLength", json!({"value": 1.0, "context": {"nodes": [2]}})),
    ] {
        let reply = call(&mut session, name, arguments.clone());
        assert_eq!(reply["result"]["isError"], true, "{name} {arguments}: {reply}");
        assert_eq!(session.snapshot(), before, "{name} {arguments}");
    }
}
