//! Form animation of an INX clip (ANI19-ANI21): form edit keys in both adjustment modes, timing and the stored field.

mod common;

use serde_json::{json, Value};
use soul2d_core::model_import::{inx_bytes, Texture};
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn temp_dir(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "soul2d-form-{}-{label}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    common::scratch(dir)
}

fn png() -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 255, 255, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
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

fn transform(at: [f64; 2], turn: f64) -> Value {
    json!({"trans": [at[0], at[1], 0.0], "rot": [0.0, 0.0, turn], "scale": [1.0, 1.0]})
}

/// Arm (5), a Node at (10, 0) turned a quarter, holding Hand (2), a 2 x 2 Part. Bend (10) offsets every vertex of
/// Hand by (0, 2) at 1; the clip Wave (11 frames) holds Bend at 1.
fn model() -> Value {
    let hand = json!({
        "uuid": 2, "name": "Hand", "type": "Part", "enabled": true, "zsort": 0.0, "transform": transform([0.0, 0.0], 0.0),
        "lockToRoot": false, "pinToMesh": false,
        "mesh": {"verts": [-1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0], "uvs": [0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0],
                 "indices": [0, 1, 2, 2, 1, 3], "origin": [0.0, 0.0]},
        "textures": [0], "blend_mode": "Normal", "tint": [1.0, 1.0, 1.0], "screenTint": [0.0, 0.0, 0.0],
        "emissionStrength": 1.0, "mask_threshold": 0.5, "opacity": 1.0, "children": [],
    });
    let arm = json!({"uuid": 5, "name": "Arm", "type": "Node", "enabled": true, "zsort": 0.0,
        "transform": transform([10.0, 0.0], std::f64::consts::FRAC_PI_2), "lockToRoot": false, "pinToMesh": false,
        "children": [hand]});
    let bent = vec![json!([0.0, 2.0]); 4];
    json!({
        "meta": {"name": "", "version": "1.0-alpha"}, "physics": {},
        "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
                  "transform": transform([0.0, 0.0], 0.0), "lockToRoot": false, "pinToMesh": false, "children": [arm]},
        "param": [{
            "uuid": 10, "name": "Bend", "is_vec2": false, "min": [0.0, 0.0], "max": [1.0, 0.0], "defaults": [0.0, 0.0],
            "axis_points": [[0.0, 1.0], [0.0]], "merge_mode": "Additive",
            "bindings": [{"node": 2, "param_name": "deform", "interpolate_mode": "Linear",
                          "values": [[vec![json!([0.0, 0.0]); 4]], [bent]], "isSet": [[true], [true]]}],
        }],
        "automation": [],
        "animations": {
            "Wave": {"length": 11, "timestep": 0.1, "additive": false, "animationWeight": 1.0, "leadIn": -1, "leadOut": -1,
                     "lanes": [{"uuid": 10, "target": 0, "interpolation": "Linear", "merge_mode": "Forced",
                                "keyframes": [{"frame": 0, "value": 1.0, "tension": 0.5}]}]},
        },
    })
}

fn open(label: &str, model: &Value) -> Session {
    let path = temp_dir(label).join("f.inp");
    fs::write(&path, inx_bytes(model, &[Texture { tag: 0, data: png() }]).unwrap()).unwrap();
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": path.to_str().unwrap()}));
    session
}

/// Hand's drawn vertices on `frame` of Wave.
fn drawn(session: &mut Session, frame: f64) -> Vec<[f64; 2]> {
    ok(session, json!({"op": "animation.seek", "name": "Wave", "frame": frame}));
    let geometry = ok(session, json!({"op": "scene.geometry"}));
    let part = geometry["parts"].as_array().unwrap().iter().find(|part| part["id"] == 2).unwrap().clone();
    part["vertices"].as_array().unwrap().iter().map(|v| [v[0].as_f64().unwrap(), v[1].as_f64().unwrap()]).collect()
}

fn moved(after: &[[f64; 2]], before: &[[f64; 2]]) -> Vec<[f64; 2]> {
    after.iter().zip(before).map(|(a, b)| [a[0] - b[0], a[1] - b[1]]).collect()
}

fn close(actual: &[[f64; 2]], expected: [f64; 2]) {
    for point in actual {
        assert!((point[0] - expected[0]).abs() < 1e-6 && (point[1] - expected[1]).abs() < 1e-6, "{actual:?} != {expected:?}");
    }
}

fn read(session: &mut Session, frame: i64) -> Value {
    ok(session, json!({"op": "animation.form.read", "name": "Wave", "frame": frame}))
}

#[test]
fn mode_1_follows_the_parent_angle_and_adds_to_the_deform_binding() {
    let mut session = open("mode1", &model());
    let before = drawn(&mut session, 0.0);
    // The Arm's quarter turn takes local (1, 0) to half of the drawn edge from vertex 0 to vertex 2.
    let turned = [(before[2][0] - before[0][0]) / 2.0, (before[2][1] - before[0][1]) / 2.0];
    assert!(turned[0].abs() < 1e-6 && (turned[1].abs() - 1.0).abs() < 1e-6, "the Arm turns a quarter: {turned:?}");
    ok(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 2, "frame": 0, "delta": [1.0, 0.0]}));
    close(&moved(&drawn(&mut session, 0.0), &before), turned);
    assert_eq!(read(&mut session, 0)["forms"], json!([{"node": 2, "mode": 1, "keys": [0],
        "offsets": [[1.0, 0.0], [1.0, 0.0], [1.0, 0.0], [1.0, 0.0]]}]));
}

#[test]
fn mode_2_moves_in_model_coordinates_whatever_the_parent_does() {
    let mut session = open("mode2", &model());
    let before = drawn(&mut session, 0.0);
    ok(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 2, "mode": 2, "frame": 0,
        "delta": [1.0, 0.0], "vertices": [3]}));
    let shift = moved(&drawn(&mut session, 0.0), &before);
    close(&shift[..3], [0.0, 0.0]);
    close(&shift[3..], [1.0, 0.0]);
    let both = ok(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 2, "frame": 0, "delta": [0.0, 0.0]}));
    assert_eq!(both["keys"], 1, "mode 1 and mode 2 are two form edits of the node");
    assert_eq!(read(&mut session, 0)["forms"].as_array().unwrap().len(), 2);
}

#[test]
fn keys_hold_before_and_after_and_move_linearly_between() {
    let mut session = open("between", &model());
    let before = drawn(&mut session, 0.0);
    for (frame, x) in [(6, 6.0), (2, 2.0)] {
        ok(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 2, "mode": 2, "frame": frame,
            "offsets": vec![[x, 0.0]; 4]}));
    }
    for (frame, x) in [(0.0, 2.0), (4.0, 4.0), (5.5, 5.5), (10.0, 6.0)] {
        close(&moved(&drawn(&mut session, frame), &before), [x, 0.0]);
    }
    assert_eq!(read(&mut session, 3)["forms"][0]["offsets"][0], json!([3.0, 0.0]));
    assert_eq!(read(&mut session, 3)["forms"][0]["keys"], json!([2, 6]), "keys are kept in frame order");
}

#[test]
fn frame_step_holds_the_form_before_time_remapping() {
    let mut session = open("frame-step", &model());
    for (frame, x) in [(0, 0.0), (6, 6.0)] {
        ok(&mut session, json!({"op":"animation.form.key","name":"Wave","node":2,"mode":2,
            "frame":frame,"offsets":vec![[x,0.0];4]}));
    }
    ok(&mut session, json!({"op":"animation.form.time","name":"Wave",
        "keyframes":[{"frame":0,"value":6.0},{"frame":6,"value":0.0}]}));
    ok(&mut session, json!({"op":"animation.frame_step","name":"Wave",
        "keyframes":[{"frame":0,"value":2}]}));
    assert_eq!(read(&mut session, 3)["formFrame"], 4.0);
    assert_eq!(read(&mut session, 3)["forms"][0]["offsets"][0], json!([4.0,0.0]));
    assert_eq!(drawn(&mut session, 2.0), drawn(&mut session, 3.0));
}

#[test]
fn the_timing_curve_shows_the_form_frame_it_maps_each_frame_to() {
    let mut session = open("time", &model());
    let before = drawn(&mut session, 0.0);
    for (frame, x) in [(2, 2.0), (6, 6.0)] {
        ok(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 2, "mode": 2, "frame": frame,
            "offsets": vec![[x, 0.0]; 4]}));
    }
    ok(&mut session, json!({"op": "animation.form.time", "name": "Wave",
        "keyframes": [{"frame": 10, "value": 2.0}, {"frame": 0, "value": 6.0}]}));
    let stored = &session.snapshot()["model"]["animations"]["Wave"]["formTime"];
    assert_eq!(stored["keyframes"][0]["frame"], 0, "the keys are stored in frame order");
    close(&moved(&drawn(&mut session, 0.0), &before), [6.0, 0.0]);
    close(&moved(&drawn(&mut session, 5.0), &before), [4.0, 0.0]);
    assert_eq!(read(&mut session, 5)["formFrame"], 4.0);
    let error = fail(&mut session, json!({"op": "animation.form.time", "name": "Wave", "interpolation": "Stepped"}));
    assert!(error.contains("keyframes not provided"), "{error}");
    ok(&mut session, json!({"op": "animation.form.time", "name": "Wave", "keyframes": null}));
    assert_eq!(session.snapshot()["model"]["animations"]["Wave"].get("formTime"), None);
    close(&moved(&drawn(&mut session, 0.0), &before), [2.0, 0.0]);
    let error = fail(&mut session, json!({"op": "animation.form.time", "name": "Wave", "keyframes": [{"frame": 11, "value": 0.0}]}));
    assert!(error.contains("outside animation length"), "{error}");
}

#[test]
fn a_key_is_one_undo_step_and_reset_and_delete_take_keys_away() {
    let mut session = open("undo", &model());
    let key = |session: &mut Session, frame: i64, extra: Value| {
        let mut request = json!({"op": "animation.form.key", "name": "Wave", "node": 2, "frame": frame});
        request.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        ok(session, request)
    };
    key(&mut session, 0, json!({"delta": [1.0, 0.0]}));
    key(&mut session, 4, json!({"delta": [0.0, 1.0]}));
    assert_eq!(read(&mut session, 4)["forms"][0]["offsets"][0], json!([1.0, 1.0]), "a delta moves what the frame shows");
    ok(&mut session, json!({"op": "undo"}));
    assert_eq!(read(&mut session, 4)["forms"][0]["keys"], json!([0]));
    key(&mut session, 4, json!({"reset": true}));
    assert_eq!(read(&mut session, 4)["forms"][0]["offsets"][0], json!([0.0, 0.0]), "Reset Form Edit keys the default form");
    let revision = session.snapshot()["revision"].clone();
    key(&mut session, 4, json!({"reset": true}));
    assert_eq!(session.snapshot()["revision"], revision, "a key that changes nothing writes no history");
    assert_eq!(ok(&mut session, json!({"op": "animation.form.delete", "name": "Wave", "node": 2, "frame": 0}))["deleted"], 1);
    assert_eq!(read(&mut session, 0)["forms"][0]["keys"], json!([4]));
    let error = fail(&mut session, json!({"op": "animation.form.delete", "name": "Wave", "node": 2, "frame": 0}));
    assert!(error.contains("no form key on frame 0"), "{error}");
    ok(&mut session, json!({"op": "animation.form.delete", "name": "Wave"}));
    assert_eq!(session.snapshot()["model"]["animations"]["Wave"].get("forms"), None, "the last key takes the field away");
    let error = fail(&mut session, json!({"op": "animation.form.delete", "name": "Wave"}));
    assert!(error.contains("no form edit to delete"), "{error}");
}

#[test]
fn keys_are_refused_for_what_a_form_edit_cannot_move() {
    let mut session = open("refuse", &model());
    let snapshot = session.snapshot()["model"].clone();
    let refusals = [
        (json!({"node": 9}), "node 9 was not found"),
        (json!({"node": 5}), "a Node node has no vertices to edit"),
        (json!({"node": 5, "mode": 2}), "a Node node has no vertices to edit"),
        (json!({"node": 2, "mode": 3}), "mode must be 1 or 2"),
        (json!({"node": 2, "offsets": [[1.0, 0.0]]}), "offsets must hold 4 pairs"),
        (json!({"node": 2, "offsets": vec![[0.0, 0.0]; 4], "reset": true}), "not both"),
        (json!({"node": 2, "delta": [1.0, 0.0], "vertices": [4]}), "indices below 4"),
        (json!({"node": 1}), "a Node node has no vertices to edit"),
        (json!({"node": 2, "reset": "true"}), "reset must be a boolean"),
        (json!({"node": 2, "delta": [1.0, "x"]}), "delta must"),
        (json!({"node": 2, "frame": 11}), "frame must be an integer frame from 0 to 10"),
        (json!({"node": 2, "offsets": vec![[0.0, 0.0]; 4], "delta": [1.0, 0.0]}), "give it without offsets or reset"),
        (json!({"node": 2, "vertices": [0]}), "give delta too"),
    ];
    for (extra, expected) in refusals {
        let mut request = json!({"op": "animation.form.key", "name": "Wave", "frame": 0});
        request.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let error = fail(&mut session, request);
        assert!(error.contains(expected), "{error} lacks {expected:?}");
    }
    assert_eq!(session.snapshot()["model"], snapshot, "a refusal leaves the document as it was");
}

#[test]
fn stored_form_edits_are_checked_and_a_broken_one_is_reported_and_not_drawn() {
    let invalid = [
        (json!([{"node": 2, "keys": []}]), "nonempty"),
        (json!([{"node": 2, "mode": 0, "keys": [{"frame": 0, "offsets": []}]}]), "mode must be 1 or 2"),
        (json!([{"node": 2, "keys": [{"frame": 3, "offsets": []}, {"frame": 1, "offsets": []}]}]), "sorted"),
        (json!([{"node": 2, "keys": [{"frame": 0, "offsets": [[0.0, 0.0]]}, {"frame": 1, "offsets": []}]}]), "same number"),
        (json!([{"node": 2, "keys": [{"frame": 0, "offsets": [[0.0]]}]}]), "[x, y]"),
        (json!([{"node": 2, "keys": [{"frame": 0, "offsets": []}]}, {"node": 2, "mode": 1, "keys": [{"frame": 0, "offsets": []}]}]),
            "repeats"),
        (json!({"node": 2}), "must be an array"),
    ];
    for (forms, expected) in invalid {
        let mut stored = model();
        stored["animations"]["Wave"]["forms"] = forms;
        let mut session = open("stored", &stored);
        let opened = session.snapshot();
        let error = fail(&mut session, json!({"op": "animation.set", "name": "Wave", "length": 12}));
        assert!(error.contains(expected), "{error} lacks {expected:?}");
        let error = fail(&mut session, json!({"op": "animation.seek", "name": "Wave", "frame": 0}));
        assert!(error.contains(expected), "a clip with such a field is not drawn either: {error}");
        assert_eq!(session.snapshot(), opened);
    }
    let mut stored = model();
    stored["animations"]["Wave"]["forms"] = json!([
        {"node": 2, "mode": 1, "keys": [{"frame": 0, "offsets": [[5.0, 0.0]]}]},
        {"node": 2, "mode": 2, "keys": [{"frame": 0, "offsets": vec![[1.0, 0.0]; 4]}]},
    ]);
    let mut session = open("broken", &stored);
    let forms = read(&mut session, 0)["forms"].clone();
    assert_eq!(forms[0]["broken"], "node 2 has 4 vertices and the form edit 1");
    assert_eq!(forms[1].get("broken"), None);
    let mut plain = open("plain", &model());
    let shift = moved(&drawn(&mut session, 0.0), &drawn(&mut plain, 0.0));
    close(&shift, [1.0, 0.0]);

    let mut timed = model();
    timed["animations"]["Wave"]["formTime"] = json!({"keyframes": [{"frame": 20, "value": 0.0}]});
    let mut session = open("timed", &timed);
    let error = fail(&mut session, json!({"op": "animation.set", "name": "Wave", "length": 12}));
    assert!(error.contains("formTime.keyframes[0].frame is outside animation length"), "{error}");
}

#[test]
fn mode_2_moves_by_the_offset_under_a_parent_scaled_unevenly() {
    let mut stretched = model();
    stretched["nodes"]["children"][0]["transform"]["scale"] = json!([2.0, 0.5]);
    let mut session = open("stretched", &stretched);
    let before = drawn(&mut session, 0.0);
    ok(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 2, "mode": 2, "frame": 0, "delta": [1.0, 0.5]}));
    close(&moved(&drawn(&mut session, 0.0), &before), [1.0, 0.5]);
}

/// Drawn vertices of Part `id` on frame 0 of Wave.
fn part_vertices(session: &mut Session, id: u64) -> Vec<[f64; 2]> {
    ok(session, json!({"op": "animation.seek", "name": "Wave", "frame": 0}));
    let geometry = ok(session, json!({"op": "scene.geometry"}));
    let part = geometry["parts"].as_array().unwrap().iter().find(|part| part["id"] == id).unwrap().clone();
    part["vertices"].as_array().unwrap().iter().map(|v| [v[0].as_f64().unwrap(), v[1].as_f64().unwrap()]).collect()
}

#[test]
fn mode_2_applies_after_glue_and_mode_1_before_it() {
    let mut glued = model();
    let mut cuff = glued["nodes"]["children"][0]["children"][0].clone();
    cuff["uuid"] = json!(7);
    cuff["name"] = json!("Cuff");
    cuff["transform"] = transform([0.0, 0.0], 0.0);
    cuff["weldedLinks"] = json!([{"targetUUID": 2, "weight": 0.5, "indices": [3, -1, -1, -1]}]);
    glued["nodes"]["children"][0]["children"][0]["weldedLinks"] =
        json!([{"targetUUID": 7, "weight": 0.5, "indices": [-1, -1, -1, 0]}]);
    glued["nodes"]["children"].as_array_mut().unwrap().push(cuff);
    let mut session = open("glued", &glued);
    let (hand, cuff) = (part_vertices(&mut session, 2), part_vertices(&mut session, 7));
    ok(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 2, "mode": 2, "frame": 0, "delta": [1.0, 0.0]}));
    close(&moved(&part_vertices(&mut session, 2), &hand), [1.0, 0.0]);
    close(&moved(&part_vertices(&mut session, 7), &cuff), [0.0, 0.0]);
    ok(&mut session, json!({"op": "undo"}));
    ok(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 2, "frame": 0, "delta": [1.0, 0.0]}));
    let (glued_hand, glued_cuff) = (moved(&part_vertices(&mut session, 2), &hand), moved(&part_vertices(&mut session, 7), &cuff));
    let half = [glued_hand[0][0] / 2.0, glued_hand[0][1] / 2.0];
    close(&glued_hand[3..], half);
    close(&glued_cuff[..1], half);
    close(&glued_cuff[1..], [0.0, 0.0]);
}

#[test]
fn a_mesh_group_is_refused_though_it_has_a_mesh() {
    let mut grouped = model();
    let mut group = grouped["nodes"]["children"][0]["children"][0].clone();
    group["uuid"] = json!(8);
    group["type"] = json!("MeshGroup");
    grouped["nodes"]["children"].as_array_mut().unwrap().push(group);
    let mut session = open("group", &grouped);
    let error = fail(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 8, "frame": 0}));
    assert!(error.contains("a MeshGroup node has no vertices to edit"), "{error}");
}

#[test]
fn mode_2_is_refused_inside_a_composite_and_mode_1_is_not() {
    let mut composed = model();
    composed["nodes"]["children"][0]["type"] = json!("Composite");
    let mut session = open("composite", &composed);
    let error = fail(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 2, "mode": 2, "frame": 0}));
    assert!(error.contains("Adjustment Mode 2 does not move a Part inside a composite"), "{error}");
    ok(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 2, "frame": 0, "delta": [1.0, 0.0]}));
    ok(&mut session, json!({"op": "animation.seek", "name": "Wave", "frame": 0}));
    let evaluated = ok(&mut session, json!({"op": "scene.evaluate"}));
    assert_eq!(evaluated["localOffsets"]["2"], json!(vec![[1.0, 2.0]; 4]), "the edit adds to Bend's offsets: {evaluated}");
    let error = fail(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 2, "frame": 0,
        "delta": [1.0, 0.0], "space": "model"}));
    assert_eq!(error, "node 2 is inside a composite; give delta in form space");
}

#[test]
fn scene_evaluate_gives_the_mode_2_offsets() {
    let mut session = open("evaluate", &model());
    ok(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 2, "mode": 2, "frame": 0, "delta": [1.0, 0.0]}));
    ok(&mut session, json!({"op": "animation.seek", "name": "Wave", "frame": 0}));
    let evaluated = ok(&mut session, json!({"op": "scene.evaluate"}));
    assert_eq!(evaluated["worldOffsets"], json!({"2": [[1.0, 0.0], [1.0, 0.0], [1.0, 0.0], [1.0, 0.0]]}));
}

#[test]
fn a_repeated_vertex_is_moved_once() {
    let mut session = open("repeat", &model());
    ok(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 2, "frame": 0, "delta": [1.0, 0.0],
        "vertices": [3, 3, 0]}));
    assert_eq!(read(&mut session, 0)["forms"][0]["offsets"], json!([[1.0, 0.0], [0.0, 0.0], [0.0, 0.0], [1.0, 0.0]]));
}

#[test]
fn a_grid_deformer_takes_a_mode_1_form_edit_that_moves_what_it_deforms() {
    let mut caged = model();
    let hand = caged["nodes"]["children"][0]["children"][0].take();
    caged["nodes"]["children"] = json!([{"uuid": 6, "name": "Cage", "type": "GridDeformer", "enabled": true, "zsort": 0.0,
        "transform": transform([0.0, 0.0], 0.0), "lockToRoot": false, "pinToMesh": false,
        "grid_axis_x": [-4.0, 4.0], "grid_axis_y": [-4.0, 4.0], "children": [hand]}]);
    let mut session = open("grid", &caged);
    let before = drawn(&mut session, 0.0);
    ok(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 6, "frame": 0, "delta": [1.0, 0.0]}));
    assert_eq!(read(&mut session, 0)["forms"][0]["offsets"].as_array().unwrap().len(), 4, "one offset per grid point");
    close(&moved(&drawn(&mut session, 0.0), &before), [1.0, 0.0]);
    let error = fail(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 6, "mode": 2, "frame": 0}));
    assert!(error.contains("Adjustment Mode 2 moves a drawn Part or Mask only"), "{error}");

    // Grid lines closer than the deformation tells apart count once, as the deformation counts them.
    caged["nodes"]["children"][0]["grid_axis_x"] = json!([-4.0, 0.0, 0.00001, 4.0]);
    let mut session = open("grid-close", &caged);
    let error = fail(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 6, "frame": 0,
        "offsets": vec![[0.0, 0.0]; 8]}));
    assert!(error.contains("offsets must hold 6 pairs"), "{error}");
    ok(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 6, "frame": 0, "delta": [1.0, 0.0]}));
    close(&moved(&drawn(&mut session, 0.0), &before), [1.0, 0.0]);
}

#[test]
fn a_model_space_delta_moves_the_vertex_that_far_in_the_picture_in_either_mode() {
    let mut session = open("model-space", &model());
    let key = |mode: u8, delta: [f64; 2], vertex: usize| json!({"op": "animation.form.key", "name": "Wave", "node": 2,
        "mode": mode, "frame": 0, "delta": delta, "vertices": [vertex], "space": "model"});
    let error = fail(&mut session, key(1, [1.0, 0.5], 3));
    assert_eq!(error, "seek animation \"Wave\" first; a model-space delta is measured in the picture shown");
    let before = drawn(&mut session, 0.0);
    ok(&mut session, key(1, [1.0, 0.5], 3));
    let shift = moved(&drawn(&mut session, 0.0), &before);
    close(&shift[..3], [0.0, 0.0]);
    close(&shift[3..], [1.0, 0.5]);
    let local = read(&mut session, 0)["forms"][0]["offsets"][3].clone();
    let local = [local[0].as_f64().unwrap(), local[1].as_f64().unwrap()];
    assert!((local[0].abs() - 0.5).abs() < 1e-6 && (local[1].abs() - 1.0).abs() < 1e-6, "turned back a quarter: {local:?}");
    drawn(&mut session, 0.0);
    ok(&mut session, key(1, [-1.0, -0.5], 3));
    close(&moved(&drawn(&mut session, 0.0), &before), [0.0, 0.0]);

    ok(&mut session, key(2, [0.0, 2.0], 0));
    let forms = read(&mut session, 0)["forms"].clone();
    assert_eq!(forms[1]["offsets"], json!([[0.0, 2.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]), "mode 2 takes it as it is");
    let mut odd = key(1, [1.0, 0.0], 0);
    odd["space"] = json!("screen");
    assert_eq!(fail(&mut session, odd), "space must be \"form\" or \"model\"");
    let error = fail(&mut session, json!({"op": "animation.form.key", "name": "Wave", "node": 2, "frame": 0,
        "reset": true, "space": "model"}));
    assert_eq!(error, "space says how delta is measured; give delta too");

    let mut flat = model();
    flat["nodes"]["children"][0]["transform"]["scale"] = json!([0.0, 1.0]);
    let mut session = open("model-space-flat", &flat);
    drawn(&mut session, 0.0);
    let shown = ok(&mut session, json!({"op": "scene.geometry"}));
    assert_eq!(fail(&mut session, key(1, [1.0, 0.0], 3)), "vertex 3 of node 2 does not follow its offset in the picture");
    assert_eq!(ok(&mut session, json!({"op": "scene.geometry"})), shown, "the picture is put back after a refusal");

    let mut session = open("model-space-frame", &model());
    drawn(&mut session, 4.0);
    let error = fail(&mut session, key(1, [1.0, 0.0], 3));
    assert_eq!(error, "seek a frame that shows form frame 0; a model-space delta is measured there");
    ok(&mut session, json!({"op": "animation.form.time", "name": "Wave", "keyframes": [{"frame": 0, "value": 4.0}]}));
    drawn(&mut session, 7.0);
    let mut timed = key(1, [1.0, 0.0], 3);
    timed["frame"] = json!(4);
    ok(&mut session, timed);

    // A curve from frame to frame is sampled in f32, and each frame still shows its own whole form frame.
    ok(&mut session, json!({"op": "animation.set", "name": "Wave", "length": 4000}));
    ok(&mut session, json!({"op": "animation.form.time", "name": "Wave",
        "keyframes": [{"frame": 0, "value": 0}, {"frame": 3999, "value": 3999}]}));
    for frame in 0..4000 {
        let read = ok(&mut session, json!({"op": "animation.form.read", "name": "Wave", "frame": frame}));
        assert_eq!(read["formFrame"], json!(f64::from(frame)), "frame {frame}");
    }
    let picture = |session: &mut Session| drawn(session, 1.0);
    let before = picture(&mut session);
    let mut whole = key(1, [1.0, 0.0], 3);
    whole["frame"] = json!(1);
    ok(&mut session, whole);
    close(&moved(&picture(&mut session), &before)[3..], [1.0, 0.0]);
}

#[test]
fn overlap_lists_a_folded_mesh_under_an_advanced_blend_mode() {
    let mut folded = model();
    let hand = &mut folded["nodes"]["children"][0]["children"][0];
    hand["mesh"]["indices"] = json!([0, 1, 2, 2, 1, 3, 0, 1, 3]);
    let mut session = open("overlap", &folded);
    assert_eq!(ok(&mut session, json!({"op": "scene.overlap"})), json!({"meshes": []}), "Normal is not listed");
    let listed = ok(&mut session, json!({"op": "scene.overlap", "all": true}));
    assert_eq!(listed["meshes"], json!([{"id": 2, "name": "Hand", "blendMode": "Normal", "pairs": 2,
        "triangles": [[0, 2], [1, 2]]}]));
    hand_blend(&mut folded, "Multiply");
    let mut session = open("overlap-multiply", &folded);
    assert_eq!(ok(&mut session, json!({"op": "scene.overlap"})), json!({"meshes": []}), "legacy Multiply is not listed");
    hand_blend(&mut folded, "Screen");
    let mut session = open("overlap-screen", &folded);
    assert_eq!(ok(&mut session, json!({"op": "scene.overlap"}))["meshes"][0]["blendMode"], "Screen");
    let mut quad = model();
    hand_blend(&mut quad, "Screen");
    let mut session = open("overlap-quad", &quad);
    assert_eq!(ok(&mut session, json!({"op": "scene.overlap"})), json!({"meshes": []}), "a plain quad does not overlap");
}

fn hand_blend(model: &mut Value, blend: &str) {
    model["nodes"]["children"][0]["children"][0]["blend_mode"] = json!(blend);
}
