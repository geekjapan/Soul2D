//! Parameter controllers of an INX animation (ANI18): add, set, delete, read, move and bake.

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
        "soul2d-controller-{}-{label}-{}",
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

/// A 2 x 2 Part centred on `at`.
fn part(id: u32, name: &str, at: [f64; 2]) -> Value {
    json!({
        "uuid": id, "name": name, "type": "Part", "enabled": true, "zsort": 0.0,
        "transform": {"trans": [at[0], at[1], 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
        "lockToRoot": false, "pinToMesh": false,
        "mesh": {"verts": [-1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0], "uvs": [0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0],
                 "indices": [0, 1, 2, 2, 1, 3], "origin": [0.0, 0.0]},
        "textures": [0], "blend_mode": "Normal", "tint": [1.0, 1.0, 1.0], "screenTint": [0.0, 0.0, 0.0],
        "emissionStrength": 1.0, "mask_threshold": 0.5, "opacity": 1.0, "children": [],
    })
}

/// A parameter from `range[0]` to `range[1]` that sets `binding` of `node` from `values[0]` to `values[1]`.
fn parameter(id: u32, name: &str, node: u32, binding: &str, range: [f64; 2], values: [f64; 2]) -> Value {
    json!({
        "uuid": id, "name": name, "is_vec2": false, "min": [range[0], 0.0], "max": [range[1], 0.0],
        "defaults": [range[0].max(0.0), 0.0], "axis_points": [[0.0, 1.0], [0.0]], "merge_mode": "Additive",
        "bindings": [{"node": node, "param_name": binding, "interpolate_mode": "Linear",
                      "values": [[values[0]], [values[1]]], "isSet": [[true], [true]]}],
    })
}

fn lane(id: u32, from: f64, to: f64) -> Value {
    json!({"uuid": id, "target": 0, "interpolation": "Linear", "merge_mode": "Forced",
        "keyframes": [{"frame": 0, "value": from, "tension": 0.5}, {"frame": 10, "value": to, "tension": 0.5}]})
}

/// Hand (2) at the origin, moved 0 to 10 right by Slide (10) and 0 to 10 along y by Lift (11). Arm (3) at (20, 0),
/// turned by Turn (12) from -pi to pi. Ball (4) at (0, 3), moved 0 to 10 right by Roll (13), which the clip Walk
/// (11 frames) keys from 0 to 1. Lift has a lane of its own only in the clip Hold, at 0.5.
fn session(label: &str) -> Session {
    session_with(label, Value::Null)
}

/// The fixture with `controllers` stored on Walk as they are, unless null.
fn session_with(label: &str, controllers: Value) -> Session {
    let dir = temp_dir(label);
    let half = std::f64::consts::PI;
    let mut model = json!({
        "meta": {"name": "", "version": "1.0-alpha"}, "physics": {},
        "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
                  "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                  "lockToRoot": false, "pinToMesh": false,
                  "children": [part(2, "Hand", [0.0, 0.0]), part(3, "Arm", [20.0, 0.0]), part(4, "Ball", [0.0, 3.0])]},
        "param": [
            parameter(10, "Slide", 2, "transform.t.x", [0.0, 1.0], [0.0, 10.0]),
            parameter(11, "Lift", 2, "transform.t.y", [0.0, 1.0], [0.0, 10.0]),
            parameter(12, "Turn", 3, "transform.r.z", [-1.0, 1.0], [-half, half]),
            parameter(13, "Roll", 4, "transform.t.x", [0.0, 1.0], [0.0, 10.0]),
        ],
        "automation": [],
        "animations": {
            "Walk": {"length": 11, "timestep": 0.1, "additive": false, "animationWeight": 1.0, "leadIn": -1,
                     "leadOut": -1, "lanes": [lane(13, 0.0, 1.0)]},
            "Hold": {"length": 11, "timestep": 0.1, "additive": false, "animationWeight": 1.0, "leadIn": -1,
                     "leadOut": -1, "lanes": [lane(11, 0.5, 0.5)]},
        },
    });
    if !controllers.is_null() {
        model["animations"]["Walk"]["controllers"] = controllers;
    }
    let path = dir.join("c.inp");
    fs::write(&path, inx_bytes(&model, &[Texture { tag: 0, data: png() }]).unwrap()).unwrap();
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": path.to_str().unwrap()}));
    session
}

fn read(session: &mut Session, clip: &str, frame: i64) -> Vec<Value> {
    let result = ok(session, json!({"op": "animation.controller.read", "name": clip, "frame": frame}));
    result["controllers"].as_array().unwrap().clone()
}

fn at(controller: &Value, end: &str) -> [f64; 2] {
    [controller[end][0].as_f64().unwrap(), controller[end][1].as_f64().unwrap()]
}

fn close(actual: [f64; 2], expected: [f64; 2]) {
    assert!((actual[0] - expected[0]).abs() < 0.01 && (actual[1] - expected[1]).abs() < 0.01, "{actual:?} != {expected:?}");
}

/// The value of the key of `parameter` on `frame` in `clip`.
fn keyed(session: &Session, clip: &str, parameter: u64, frame: i64) -> Option<f64> {
    let lanes = session.snapshot()["model"]["animations"][clip]["lanes"].clone();
    let lane = lanes.as_array()?.iter().find(|lane| lane["uuid"] == parameter)?.clone();
    let key = lane["keyframes"].as_array()?.iter().find(|key| key["frame"] == frame)?.clone();
    key["value"].as_f64()
}

#[test]
fn a_point_controller_is_pinned_moved_and_keyed_in_one_undo() {
    let mut session = session("move");
    let added = ok(&mut session, json!({"op": "animation.controller.add", "name": "Walk", "controller": "Grip",
        "x": 0.5, "y": 0.5}));
    assert_eq!((&added["start"]["node"], &added["parameters"]), (&json!(2), &json!([10, 11])), "{added}");
    close(at(&read(&mut session, "Walk", 0)[0], "start"), [0.5, 0.5]);

    let moved = ok(&mut session, json!({"op": "animation.controller.move", "name": "Walk", "controller": "Grip",
        "frame": 4, "x": 4.5, "y": 3.5}));
    assert_eq!(moved["reached"], true, "{moved}");
    let slide = keyed(&session, "Walk", 10, 4).unwrap();
    let lift = keyed(&session, "Walk", 11, 4).unwrap();
    assert!((slide - 0.4).abs() < 0.001 && (lift - 0.3).abs() < 0.001, "{slide} {lift}");
    close(at(&read(&mut session, "Walk", 4)[0], "start"), [4.5, 3.5]);
    ok(&mut session, json!({"op": "undo"}));
    assert_eq!(keyed(&session, "Walk", 10, 4), None, "moving is one undo");
    assert_eq!(read(&mut session, "Walk", 0).len(), 1, "the controller stays");

    let far = ok(&mut session, json!({"op": "animation.controller.move", "name": "Walk", "controller": "Grip",
        "frame": 2, "x": 30.5, "y": 0.5}));
    assert_eq!(far["reached"], false);
    assert!((far["distance"].as_f64().unwrap() - 20.0).abs() < 0.01, "Slide stops at its maximum: {far}");
    assert_eq!(keyed(&session, "Walk", 10, 2), Some(1.0));
    let back = ok(&mut session, json!({"op": "animation.controller.move", "name": "Walk", "controller": "Grip",
        "frame": 2, "x": 5.5, "y": 0.5}));
    assert_eq!(back["reached"], true, "the solve leaves the end of the range it starts from: {back}");
    assert!((keyed(&session, "Walk", 10, 2).unwrap() - 0.5).abs() < 0.001);
}

#[test]
fn the_solve_starts_from_the_frame_with_the_other_lanes_of_the_clip() {
    let mut session = session("lanes");
    // Lift's lane holds Hand at y 5 on every frame of Hold, so the controller is pinned there.
    ok(&mut session, json!({"op": "animation.controller.add", "name": "Hold", "controller": "Grip", "x": 0.0, "y": 5.0,
        "parameters": [10]}));
    let moved = ok(&mut session, json!({"op": "animation.controller.move", "name": "Hold", "controller": "Grip",
        "frame": 3, "x": 6.0, "y": 5.0}));
    assert_eq!(moved["reached"], true, "Lift's lane holds the Part at y 5: {moved}");
    assert_eq!((&moved["keys"][0]["parameter"], &moved["keys"][0]["axis"]), (&json!(10), &json!(0)));
    assert!((moved["keys"][0]["value"].as_f64().unwrap() - 0.6).abs() < 0.001, "{moved}");
}

#[test]
fn a_direction_controller_turns_toward_the_point_it_is_moved_to() {
    let mut session = session("direction");
    let added = ok(&mut session, json!({"op": "animation.controller.add", "name": "Walk", "controller": "Aim",
        "x": 19.5, "y": 0.0, "end": [20.5, 0.0]}));
    assert_eq!((&added["end"]["node"], &added["parameters"]), (&json!(3), &json!([12])));
    let moved = ok(&mut session, json!({"op": "animation.controller.move", "name": "Walk", "controller": "Aim",
        "frame": 0, "x": 19.5, "y": 10.0}));
    assert_eq!(moved["reached"], true, "{moved}");
    let aim = &read(&mut session, "Walk", 0)[0];
    let (start, end) = (at(aim, "start"), at(aim, "end"));
    let (aim, goal) = ([end[0] - start[0], end[1] - start[1]], [19.5 - start[0], 10.0 - start[1]]);
    let (cross, dot) = (aim[0] * goal[1] - aim[1] * goal[0], aim[0] * goal[0] + aim[1] * goal[1]);
    assert!(cross.abs() < 0.01 && dot > 0.0, "the end points at (19.5, 10): {start:?} {end:?}");
}

#[test]
fn a_bake_keys_the_followed_target_on_every_frame_and_removes_the_controllers() {
    let mut session = session("bake");
    ok(&mut session, json!({"op": "animation.controller.add", "name": "Walk", "controller": "Grip", "x": 0.0, "y": 0.0}));
    ok(&mut session, json!({"op": "animation.controller.add", "name": "Walk", "controller": "Goal", "x": 0.0, "y": 3.0}));
    let set = ok(&mut session, json!({"op": "animation.controller.set", "name": "Walk", "controller": "Grip",
        "target": "Goal"}));
    assert_eq!(set["target"], "Goal");
    let before = session.snapshot();
    let baked = ok(&mut session, json!({"op": "animation.controller.bake", "name": "Walk"}));
    assert_eq!(baked, json!({"from": 0, "to": 10, "keys": 22, "unreached": []}));
    for frame in [0, 5, 10] {
        let slide = keyed(&session, "Walk", 10, frame).unwrap();
        let lift = keyed(&session, "Walk", 11, frame).unwrap();
        assert!((slide - frame as f64 / 10.0).abs() < 0.001 && (lift - 0.3).abs() < 0.001, "{frame}: {slide} {lift}");
    }
    assert_eq!(session.snapshot()["model"]["animations"]["Walk"].get("controllers"), None);
    ok(&mut session, json!({"op": "undo"}));
    assert_eq!(session.snapshot()["model"], before["model"], "baking is one undo");

    ok(&mut session, json!({"op": "animation.controller.set", "name": "Walk", "controller": "Grip", "weight": 50}));
    ok(&mut session, json!({"op": "animation.controller.bake", "name": "Walk"}));
    for frame in [5, 10] {
        let slide = keyed(&session, "Walk", 10, frame).unwrap();
        assert!((slide - frame as f64 / 20.0).abs() < 0.001, "{frame}: half of the followed value, {slide}");
    }
}

#[test]
fn a_point_follows_the_line_of_a_direction_target_or_only_its_ends_without_extend() {
    let mut session = session("line");
    ok(&mut session, json!({"op": "animation.controller.add", "name": "Walk", "controller": "Grip", "x": 0.0, "y": 0.0,
        "parameters": [11]}));
    ok(&mut session, json!({"op": "animation.controller.add", "name": "Walk", "controller": "Rail", "x": -0.5, "y": 3.0,
        "end": [0.5, 3.0]}));
    ok(&mut session, json!({"op": "animation.controller.set", "name": "Walk", "controller": "Grip", "target": "Rail"}));
    // On frame 10 Roll has moved Ball, and Rail with it, 10 right; Grip can only go up and down.
    let baked = ok(&mut session, json!({"op": "animation.controller.bake", "name": "Walk", "from": 10, "to": 10}));
    assert_eq!(baked["unreached"], json!([]), "the line through Rail reaches past its ends");
    let lift = keyed(&session, "Walk", 11, 10).unwrap();
    assert!((lift - 0.3).abs() < 0.001, "{lift}");
    ok(&mut session, json!({"op": "undo"}));
    ok(&mut session, json!({"op": "animation.controller.set", "name": "Walk", "controller": "Grip", "extend": false}));
    let baked = ok(&mut session, json!({"op": "animation.controller.bake", "name": "Walk", "from": 10, "to": 10}));
    let distance = baked["unreached"][0]["distance"].as_f64().unwrap();
    assert!((distance - 9.5).abs() < 0.01, "Rail's nearer end is 9.5 to the side: {baked}");
}

#[test]
fn refusals_leave_the_document_unchanged() {
    let mut session = session("refusals");
    ok(&mut session, json!({"op": "animation.controller.add", "name": "Walk", "controller": "Grip", "x": 0.0, "y": 0.0}));
    let opened = session.snapshot();
    let refusals = [
        (json!({"op": "animation.controller.add", "name": "Walk", "controller": "Grip", "x": 0.0, "y": 0.0}),
            "animation \"Walk\" has a controller named \"Grip\" already"),
        (json!({"op": "animation.controller.add", "name": "Walk", "controller": "Air", "x": 50.0, "y": 50.0}),
            "no Part is drawn at (50, 50) on frame 0"),
        (json!({"op": "animation.controller.add", "name": "Walk", "controller": "Air", "x": 0.0, "y": 0.0,
            "parameters": [13]}), "parameter 13 cannot move the Part of controller \"Air\""),
        (json!({"op": "animation.controller.add", "name": "Walk", "controller": "Air", "x": 0.0, "y": 0.0,
            "parameters": [10, 10]}), "parameter 10 is listed twice"),
        (json!({"op": "animation.controller.add", "name": "Walk", "controller": "Air", "x": 0.0, "y": 0.0,
            "parameters": []}), "parameters must not be empty"),
        (json!({"op": "animation.controller.add", "name": "Walk", "controller": " ", "x": 0.0, "y": 0.0}),
            "controller must not be empty"),
        (json!({"op": "animation.controller.add", "name": "Walk", "controller": "Air", "x": 0.0, "y": 0.0, "frame": 11}),
            "frame must be an integer frame from 0 to 10"),
        (json!({"op": "animation.controller.add", "name": "Run", "controller": "Air", "x": 0.0, "y": 0.0}),
            "animation \"Run\" was not found"),
        (json!({"op": "animation.controller.set", "name": "Walk", "controller": "Grip", "target": "Grip"}),
            "a controller cannot follow itself"),
        (json!({"op": "animation.controller.set", "name": "Walk", "controller": "Grip", "target": "Nobody"}),
            "animation \"Walk\" has no controller named \"Nobody\""),
        (json!({"op": "animation.controller.set", "name": "Walk", "controller": "Grip", "weight": 150}),
            "weight must be a number from 0 to 100"),
        (json!({"op": "animation.controller.set", "name": "Walk", "controller": "Grip", "parameters": []}),
            "parameters must not be empty"),
        (json!({"op": "animation.controller.delete", "name": "Hold"}), "animation \"Hold\" has no controllers"),
        (json!({"op": "animation.controller.move", "name": "Walk", "controller": "Grip", "frame": 1.5, "x": 0, "y": 0}),
            "frame must be an integer frame from 0 to 10"),
        (json!({"op": "animation.controller.move", "name": "Walk", "controller": "Grip", "frame": 1, "x": 0}),
            "x and y must be [x, y], finite"),
        (json!({"op": "animation.controller.bake", "name": "Walk"}),
            "no controller of animation \"Walk\" follows another of its controllers"),
        (json!({"op": "animation.controller.bake", "name": "Walk", "from": 5, "to": 4}), "from must not be after to"),
        (json!({"op": "animation.controller.read", "name": "Walk", "frame": -1}),
            "frame must be an integer frame from 0 to 10"),
    ];
    for (request, expected) in refusals {
        assert_eq!(fail(&mut session, request.clone()), expected, "{request}");
        assert_eq!(session.snapshot(), opened, "{request}");
    }
    ok(&mut session, json!({"op": "animation.key.set", "name": "Walk", "parameter": 10, "axis": 0, "frame": 0,
        "value": 0.0}));
    ok(&mut session, json!({"op": "animation.lane.set", "name": "Walk", "parameter": 10, "axis": 0, "frozen": true}));
    ok(&mut session, json!({"op": "animation.freeze.set", "name": "Walk", "frame": 0}));
    ok(&mut session, json!({"op": "animation.controller.add", "name": "Walk", "controller": "Goal", "x": 0.0, "y": 3.0}));
    ok(&mut session, json!({"op": "animation.controller.set", "name": "Walk", "controller": "Grip", "target": "Goal"}));
    assert_eq!(fail(&mut session, json!({"op": "animation.controller.bake", "name": "Walk"})),
        "target tracking cannot run while freeze is on; turn freeze off first");
    let frozen = session.snapshot();
    assert_eq!(fail(&mut session, json!({"op": "animation.controller.move", "name": "Walk", "controller": "Grip",
        "frame": 0, "x": 1.0, "y": 0.0})),
        "the lane of parameter 10 axis 0 in animation \"Walk\" is frozen; turn freeze off first");
    assert_eq!(session.snapshot(), frozen, "a frozen lane takes no key");

    let mut session = session_with("caps", json!([{"name": "Grip",
        "start": {"node": 2, "vertices": [0, 1, 2], "weights": [1.0, 0.0, 0.0]}, "parameters": vec![10; 17]}]));
    assert_eq!(fail(&mut session, json!({"op": "animation.controller.move", "name": "Walk", "controller": "Grip",
        "frame": 0, "x": 1.0, "y": 0.0})), "a controller moves at most 16 parameter axes; list fewer parameters");
    ok(&mut session, json!({"op": "animation.controller.add", "name": "Walk", "controller": "Goal", "x": 0.0, "y": 3.0}));
    ok(&mut session, json!({"op": "animation.controller.set", "name": "Walk", "controller": "Grip", "target": "Goal",
        "parameters": [10]}));
    ok(&mut session, json!({"op": "animation.set", "name": "Walk", "length": 2001}));
    assert_eq!(fail(&mut session, json!({"op": "animation.controller.bake", "name": "Walk"})),
        "bake at most 2000 frames at a time");
}

#[test]
fn a_controller_keys_forced_lanes_from_the_values_the_frame_draws_with() {
    let mut session = session("forced");
    ok(&mut session, json!({"op": "animation.controller.add", "name": "Walk", "controller": "Grip", "x": 0.0, "y": 0.0}));
    ok(&mut session, json!({"op": "animation.controller.move", "name": "Walk", "controller": "Grip", "frame": 0,
        "x": 2.0, "y": 0.0}));
    let refused = "the lane of parameter 10 axis 0 does not draw its key on frame 4, so a controller cannot key it";
    for change in [json!({"merge_mode": "Multiplicative"}), json!({"interpolation": "Stepped"})] {
        let mut lane = json!({"op": "animation.lane.set", "name": "Walk", "parameter": 10, "axis": 0});
        lane.as_object_mut().unwrap().extend(change.as_object().unwrap().clone());
        ok(&mut session, lane);
        let before = session.snapshot();
        assert_eq!(fail(&mut session, json!({"op": "animation.controller.move", "name": "Walk", "controller": "Grip",
            "frame": 4, "x": 3.0, "y": 0.0})), refused, "{change}");
        assert_eq!(session.snapshot(), before);
        ok(&mut session, json!({"op": "undo"}));
    }
    // Slide's value is 0, so an Additive lane draws the key as it is.
    ok(&mut session, json!({"op": "animation.lane.set", "name": "Walk", "parameter": 10, "axis": 0,
        "merge_mode": "Additive"}));
    ok(&mut session, json!({"op": "animation.controller.move", "name": "Walk", "controller": "Grip", "frame": 4,
        "x": 3.0, "y": 0.0}));
    assert!((keyed(&session, "Walk", 10, 4).unwrap() - 0.3).abs() < 0.001);

    // A direction on Hand following the level Rail on Ball is already level, so baking keeps the values Hold
    // draws with: Lift's lane at 0.5, Slide at its default. Eye blinking, here half shut on Lift, plays on top.
    ok(&mut session, json!({"op": "animation.blink.parameters.set", "parameters": [11]}));
    ok(&mut session, json!({"op": "animation.blink.set", "name": "Hold", "frame": 2, "value": 0.5}));
    ok(&mut session, json!({"op": "animation.controller.add", "name": "Hold", "controller": "Aim", "x": -0.5, "y": 5.0,
        "end": [0.5, 5.0]}));
    ok(&mut session, json!({"op": "animation.controller.add", "name": "Hold", "controller": "Rail", "x": -0.5, "y": 3.0,
        "end": [0.5, 3.0]}));
    ok(&mut session, json!({"op": "animation.controller.set", "name": "Hold", "controller": "Aim", "target": "Rail"}));
    let baked = ok(&mut session, json!({"op": "animation.controller.bake", "name": "Hold", "from": 2, "to": 2}));
    assert_eq!(baked["unreached"], json!([]));
    assert_eq!((keyed(&session, "Hold", 10, 2), keyed(&session, "Hold", 11, 2)), (Some(0.0), Some(0.5)), "{baked}");
}

#[test]
fn set_changes_or_clears_what_a_controller_moves_and_follows() {
    let mut session = session("set");
    let added = ok(&mut session, json!({"op": "animation.controller.add", "name": "Walk", "controller": "Reach",
        "x": 0.0, "y": 0.0, "end": [0.0, 3.0]}));
    assert_eq!(added["parameters"], json!([10, 11, 13]), "the end's Part moves it too");
    ok(&mut session, json!({"op": "animation.controller.add", "name": "Walk", "controller": "Goal", "x": 0.0, "y": 3.0}));
    let set = ok(&mut session, json!({"op": "animation.controller.set", "name": "Walk", "controller": "Reach",
        "parameters": [13], "target": "Goal", "extend": false, "weight": 25}));
    assert_eq!(set["parameters"], json!([13]), "a parameter of the end's Part is allowed");
    assert_eq!((&set["target"], &set["extend"], &set["weight"]), (&json!("Goal"), &json!(false), &json!(25.0)));
    let cleared = ok(&mut session, json!({"op": "animation.controller.set", "name": "Walk", "controller": "Reach",
        "parameters": null, "target": null}));
    assert_eq!((cleared["parameters"].clone(), cleared.get("target")), (json!([13]), None), "{cleared}");
}

#[test]
fn a_controller_is_pinned_to_the_frontmost_part_and_a_lost_target_reads_as_broken() {
    let mut session = session_with("front", json!([{"name": "Grip", "target": "Ghost",
        "start": {"node": 2, "vertices": [0, 1, 2], "weights": [1.0, 0.0, 0.0]}, "parameters": [10]}]));
    assert_eq!(read(&mut session, "Walk", 0)[0]["broken"], "target \"Ghost\" is not a controller of the animation");
    // Lifted to y 3 on frame 0, Hand lies under Ball, which is drawn after it.
    ok(&mut session, json!({"op": "animation.key.set", "name": "Walk", "parameter": 11, "axis": 0, "frame": 0,
        "value": 0.3}));
    let added = ok(&mut session, json!({"op": "animation.controller.add", "name": "Walk", "controller": "Top",
        "x": 0.0, "y": 3.0}));
    assert_eq!(added["start"]["node"], 4);
}

#[test]
fn deleting_a_controller_clears_the_targets_that_named_it() {
    let mut session = session("delete");
    for (name, y) in [("Grip", 0.0), ("Goal", 3.0)] {
        ok(&mut session, json!({"op": "animation.controller.add", "name": "Walk", "controller": name, "x": 0.0, "y": y}));
    }
    ok(&mut session, json!({"op": "animation.controller.set", "name": "Walk", "controller": "Grip", "target": "Goal"}));
    let deleted = ok(&mut session, json!({"op": "animation.controller.delete", "name": "Walk", "controller": "Goal"}));
    assert_eq!(deleted, json!({"deleted": ["Goal"]}));
    let left = &session.snapshot()["model"]["animations"]["Walk"]["controllers"];
    assert_eq!(left.as_array().map(Vec::len), Some(1));
    assert_eq!(left[0].get("target"), None);
    ok(&mut session, json!({"op": "animation.controller.delete", "name": "Walk"}));
    assert_eq!(session.snapshot()["model"]["animations"]["Walk"].get("controllers"), None);
}

#[test]
fn a_deleted_parameter_or_part_leaves_the_controller_readable_and_marked() {
    let mut session = session("lifecycle");
    ok(&mut session, json!({"op": "animation.controller.add", "name": "Walk", "controller": "Grip", "x": 0.0, "y": 0.0}));
    ok(&mut session, json!({"op": "parameter.delete", "id": 11}));
    let stored = &session.snapshot()["model"]["animations"]["Walk"]["controllers"][0];
    assert_eq!(stored["parameters"], json!([10]), "a deleted parameter leaves the controller");
    ok(&mut session, json!({"op": "node.delete", "nodes": [2]}));
    let read = read(&mut session, "Walk", 0);
    assert_eq!(read[0]["broken"], "node 2 draws no mesh");
}

#[test]
fn a_stored_controller_is_checked_when_the_clip_is_set() {
    let good = json!({"name": "Grip", "start": {"node": 2, "vertices": [0, 1, 2], "weights": [1.0, 0.0, 0.0]},
        "parameters": [10]});
    let cases = [
        (json!({"name": ""}), "animation.controllers[0].name must be a non-empty string"),
        (json!({"name": "Grip", "start": {"node": 2, "vertices": [0, 1], "weights": [1.0, 0.0, 0.0]}, "parameters": []}),
            "animation.controllers[0].start needs a node, three vertex indices and three weights"),
        (json!({"name": "Grip", "start": good["start"], "parameters": "all"}),
            "animation.controllers[0].parameters must be an array"),
        (json!({"name": "Grip", "start": good["start"], "parameters": [10], "weight": -1}),
            "animation.controllers[0].weight must be a number from 0 to 100"),
        (json!({"name": "Grip", "start": good["start"], "parameters": [10], "target": "Grip"}),
            "animation.controllers[0].target must name another controller"),
    ];
    let repeated = "animation.controllers[1] repeats the controller name \"Grip\"";
    let many: Vec<Value> = (0..65).map(|index| json!({"name": format!("C{index}"), "start": good["start"],
        "parameters": [10]})).collect();
    for (controllers, expected) in cases.into_iter().map(|(one, expected)| (json!([one]), expected))
        .chain([(json!([good, good]), repeated), (json!(many), "an animation holds at most 64 controllers")])
    {
        let mut session = session_with("stored", controllers);
        let opened = session.snapshot();
        let message = fail(&mut session, json!({"op": "animation.set", "name": "Walk", "length": 12}));
        assert!(message.contains(expected), "{message}");
        assert_eq!(session.snapshot(), opened);
    }
    let mut session = session_with("stored", json!([good]));
    ok(&mut session, json!({"op": "animation.set", "name": "Walk", "length": 12}));
}
