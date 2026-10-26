
mod common;

use serde_json::{json, Value};
use soul2d_core::model_import::{inx_bytes, Texture};
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const MOVE: u32 = 10;
const SWING: u32 = 11;
const OTHER: u32 = 12;
const TILT: u32 = 13;
const LIMB: u32 = 14;
const LENGTH: i32 = 11;
const TIMESTEP: f64 = 0.0625;

fn temp_dir(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "soul2d-physics-bake-{}-{nanos}-{label}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst),
    ));
    fs::create_dir(&dir).unwrap();
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

fn close(actual: &Value, expected: f64) {
    let value = actual.as_f64().unwrap_or(f64::NAN);
    assert!((value - expected).abs() <= 1e-5 * expected.abs().max(1.0), "{value} != {expected}");
}

fn simple(id: u32, name: &str, param: Option<u32>, map_mode: &str, length: f64) -> Value {
    let mut node = json!({
        "uuid": id, "name": name, "type": "SimplePhysics", "enabled": true, "zsort": 0.0,
        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
        "model_type": "Pendulum", "map_mode": map_mode, "gravity": 1.0, "length": length,
        "frequency": 1.0, "angle_damping": 0.5, "length_damping": 0.5,
        "output_scale": [1.0, 1.0], "local_only": false, "children": [],
    });
    if let Some(param) = param {
        node["param"] = json!(param);
    }
    node
}

fn model() -> Value {
    json!({
        "meta": {"name": "", "version": "1.0-alpha"}, "physics": {},
        "nodes": {
            "uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
            "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
            "lockToRoot": false, "pinToMesh": false,
            "children": [
                {
                    "uuid": 2, "name": "Body", "type": "Part", "enabled": true, "zsort": 0.0,
                    "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                    "lockToRoot": false, "pinToMesh": false,
                    "mesh": {"verts": [-1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0],
                             "uvs": [0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0],
                             "indices": [0, 1, 2, 2, 1, 3], "origin": [0.0, 0.0]},
                    "textures": [0], "blend_mode": "Normal", "tint": [1.0, 1.0, 1.0],
                    "screenTint": [0.0, 0.0, 0.0], "emissionStrength": 1.0, "mask_threshold": 0.5,
                    "opacity": 1.0,
                    "children": [
                        simple(3, "Hair", Some(SWING), "XY", 30.0),
                        simple(5, "Fringe", Some(TILT), "AngleLength", 60.0),
                    ],
                },
                simple(6, "Loose", None, "XY", 30.0),
                {
                    "uuid": 7, "name": "Hidden", "type": "Node", "enabled": false, "zsort": 0.0,
                    "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                    "children": [simple(8, "Limb", Some(LIMB), "XY", 30.0)],
                },
            ],
        },
        "param": [
            {"uuid": MOVE, "name": "Move", "is_vec2": false, "min": [0.0, 0.0], "max": [1.0, 0.0],
             "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0]], "merge_mode": "Additive",
             "bindings": [{"node": 2, "param_name": "transform.t.x", "interpolate_mode": "Linear",
                           "values": [[0.0], [30.0]], "isSet": [[true], [true]]}]},
            {"uuid": SWING, "name": "Swing", "is_vec2": true, "min": [-2.0, -2.0], "max": [2.0, 2.0],
             "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0, 1.0]], "merge_mode": "Additive",
             "bindings": []},
            {"uuid": TILT, "name": "Tilt", "is_vec2": false, "min": [-2.0, 0.0], "max": [2.0, 0.0],
             "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0]], "merge_mode": "Additive",
             "bindings": []},
            {"groupUUID": 40, "name": "Extras", "children": [
                {"uuid": OTHER, "name": "Other", "is_vec2": false, "min": [0.0, 0.0], "max": [1.0, 0.0],
                 "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0]], "merge_mode": "Additive",
                 "bindings": []}]},
            {"uuid": LIMB, "name": "Limb", "is_vec2": false, "min": [-2.0, 0.0], "max": [2.0, 0.0],
             "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0]], "merge_mode": "Additive",
             "bindings": []},
        ],
        "automation": [],
        "animations": {
            "Run": {"length": LENGTH, "timestep": TIMESTEP, "additive": false, "animationWeight": 1.0,
                "leadIn": -1, "leadOut": -1,
                "lanes": [
                    {"uuid": MOVE, "target": 0, "interpolation": "Linear", "merge_mode": "Forced",
                     "keyframes": [{"frame": 0, "value": 0.0, "tension": 0.5},
                                   {"frame": 10, "value": 1.0, "tension": 0.5}]},
                    {"uuid": OTHER, "target": 0, "interpolation": "Stepped", "merge_mode": "Forced",
                     "keyframes": [{"frame": 3, "value": 0.5, "tension": 0.5}]},
                ]},
        },
    })
}

fn session_with(label: &str, model: &Value) -> (Session, PathBuf) {
    let dir = temp_dir(label);
    let path = dir.join("fixture.inx");
    fs::write(&path, inx_bytes(model, &[Texture { tag: 0, data: png() }]).unwrap()).unwrap();
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": path.to_str().unwrap()}));
    (session, dir)
}

fn session(label: &str) -> Session {
    session_with(label, &model()).0
}

fn reference(session: &mut Session, warmup: bool, from: i32, to: i32, id: u32) -> Vec<[f64; 2]> {
    ok(session, json!({"op": "animation.seek", "name": "Run", "frame": 0}));
    let mut frames = Vec::new();
    if warmup {
        for frame in 0..LENGTH {
            frames.push((frame, TIMESTEP, false));
        }
    }
    for frame in 0..=to {
        frames.push((frame, if warmup || frame != 0 { TIMESTEP } else { 0.0 }, frame >= from));
    }
    let mut outputs = Vec::new();
    for (index, (_, dt, record)) in frames.iter().enumerate() {
        if index > 0 {
            ok(session, json!({"op": "animation.advance", "dt": TIMESTEP}));
        }
        let snapshot = ok(session, json!({"op": "physics.step", "dt": dt}));
        if *record {
            let pair = &snapshot["values"][id.to_string()];
            outputs.push([pair[0].as_f64().unwrap(), pair[1].as_f64().unwrap()]);
        }
    }
    outputs
}

fn lane(animation: &Value, id: u32, axis: u64) -> Option<&Value> {
    animation["lanes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|lane| lane["uuid"].as_u64() == Some(u64::from(id)) && lane["target"].as_u64() == Some(axis))
}

fn lane_frames(animation: &Value, id: u32, axis: u64) -> Vec<i64> {
    lane(animation, id, axis)
        .unwrap()["keyframes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| key["frame"].as_i64().unwrap())
        .collect()
}

#[test]
fn bake_keys_the_simple_physics_outputs_the_clip_draws() {
    let (mut session, dir) = session_with("bake", &model());
    let swing = reference(&mut session, true, 0, 10, SWING);
    let tilt = reference(&mut session, true, 0, 10, TILT);
    assert!(swing.iter().any(|value| value[0].abs() > 1e-4), "the anchor never moved the pendulum");
    let before = session.snapshot()["model"].clone();

    let baked = ok(&mut session, json!({"op": "animation.physics.bake", "name": "Run"}));
    assert_eq!(
        baked,
        json!({"name": "Run", "from": 0, "to": 10, "frames": 11, "keys": 33,
               "parameters": [SWING, TILT], "warmup": true})
    );

    let animation = &session.snapshot()["model"]["animations"]["Run"];
    for axis in 0..2 {
        let lane = lane(animation, SWING, axis).unwrap();
        assert_eq!(lane["interpolation"], "Linear");
        assert_eq!(lane["merge_mode"], "Forced");
        assert_eq!(lane_frames(animation, SWING, axis), (0..=10).collect::<Vec<_>>());
    }
    assert_eq!(lane(animation, TILT, 0).unwrap()["interpolation"], "Linear");
    assert_eq!(lane_frames(animation, TILT, 0), (0..=10).collect::<Vec<_>>());
    assert!(lane(animation, LIMB, 0).is_none(), "a disabled ancestor's output is not baked");
    assert_eq!(lane(animation, OTHER, 0).unwrap()["interpolation"], "Stepped", "unrelated lane kept");
    assert_eq!(lane(animation, MOVE, 0).unwrap()["keyframes"].as_array().unwrap().len(), 2);
    assert_eq!(animation["leadIn"], -1);
    assert_eq!(animation["timestep"], json!(TIMESTEP));

    ok(&mut session, json!({"op": "physics.reset"}));
    for frame in 0..=10 {
        let sampled = ok(&mut session, json!({"op": "animation.sample", "name": "Run", "frame": frame}));
        for axis in 0..2 {
            close(&sampled["values"][SWING.to_string()][axis], swing[frame as usize][axis]);
        }
        close(&sampled["values"][TILT.to_string()][0], tilt[frame as usize][0]);
    }
    let baked_model = session.snapshot()["model"].clone();

    let mut second = session_with("bake-again", &model()).0;
    assert_eq!(ok(&mut second, json!({"op": "animation.physics.bake", "name": "Run"})), baked);
    assert_eq!(second.snapshot()["model"], baked_model);

    ok(&mut session, json!({"op": "undo"}));
    assert_eq!(session.snapshot()["model"], before);
    ok(&mut session, json!({"op": "redo"}));
    assert_eq!(session.snapshot()["model"], baked_model);

    let saved = dir.join("baked.inx");
    ok(&mut session, json!({"op": "save", "path": saved.to_str().unwrap()}));
    let mut reopened = Session::new();
    ok(&mut reopened, json!({"op": "open", "path": saved.to_str().unwrap()}));
    for frame in [0, 5, 10] {
        let sampled =
            ok(&mut reopened, json!({"op": "animation.sample", "name": "Run", "frame": frame}));
        close(&sampled["values"][SWING.to_string()][0], swing[frame][0]);
        close(&sampled["values"][TILT.to_string()][0], tilt[frame][0]);
    }
}

#[test]
fn a_subset_and_partial_range_keys_only_what_is_asked() {
    let mut session = session("partial");
    ok(&mut session, json!({"op": "animation.key.set", "name": "Run", "parameter": SWING,
                            "axis": 0, "frame": 0, "value": -0.5}));
    ok(&mut session, json!({"op": "animation.key.set", "name": "Run", "parameter": SWING,
                            "axis": 0, "frame": 10, "value": 0.5}));
    let swing = reference(&mut session, true, 3, 6, SWING);
    let before_lanes = session.snapshot()["model"]["animations"]["Run"]["lanes"].clone();

    let baked = ok(&mut session, json!({"op": "animation.physics.bake", "name": "Run",
                                        "parameters": [SWING], "from": 3, "to": 6}));
    assert_eq!(
        baked,
        json!({"name": "Run", "from": 3, "to": 6, "frames": 4, "keys": 8,
               "parameters": [SWING], "warmup": true})
    );
    let animation = &session.snapshot()["model"]["animations"]["Run"];
    assert_eq!(lane_frames(animation, SWING, 0), vec![0, 3, 4, 5, 6, 10], "outside keys kept");
    assert_eq!(lane_frames(animation, SWING, 1), vec![3, 4, 5, 6]);
    assert!(lane(animation, TILT, 0).is_none(), "an unselected output is not keyed");
    for other in before_lanes.as_array().unwrap() {
        let id = other["uuid"].as_u64().unwrap();
        if id != u64::from(SWING) {
            assert!(lane(animation, id as u32, other["target"].as_u64().unwrap()).is_some());
        }
    }
    for (index, frame) in (3..=6).enumerate() {
        let sampled = ok(&mut session, json!({"op": "animation.sample", "name": "Run", "frame": frame}));
        for axis in 0..2 {
            close(&sampled["values"][SWING.to_string()][axis], swing[index][axis]);
        }
    }
}

#[test]
fn without_warmup_the_bake_keys_a_fresh_pendulum() {
    let (mut session, _dir) = session_with("cold", &model());
    let swing = reference(&mut session, false, 0, 10, SWING);
    assert_eq!(swing[0], [0.0, 0.0], "a fresh pendulum draws its rest value on frame 0");
    assert_ne!(swing[0], swing[5], "the clip moves the pendulum");

    let baked = ok(&mut session, json!({"op": "animation.physics.bake", "name": "Run", "warmup": false}));
    assert_eq!(baked["frames"], 11);
    assert_eq!(baked["keys"], 33);
    assert_eq!(baked["warmup"], false);
    for frame in 0..=10 {
        let sampled = ok(&mut session, json!({"op": "animation.sample", "name": "Run", "frame": frame}));
        for axis in 0..2 {
            close(&sampled["values"][SWING.to_string()][axis], swing[frame as usize][axis]);
        }
    }
    let warmed = reference(&mut session, true, 0, 10, SWING);
    assert_ne!(warmed, swing, "warmup changes the recorded values");
}

#[test]
fn malformed_requests_refuse_without_changing_anything() {
    let mut session = session("refusals");
    let opened = session.snapshot();
    for (request, expect) in [
        (json!({"op": "animation.physics.bake"}), "name must be a string"),
        (json!({"op": "animation.physics.bake", "name": 3}), "name must be a string"),
        (json!({"op": "animation.physics.bake", "name": "Nope"}), "animation \"Nope\" was not found"),
        (json!({"op": "animation.physics.bake", "name": "Run", "parameters": []}),
         "parameters must not be empty"),
        (json!({"op": "animation.physics.bake", "name": "Run", "parameters": "x"}),
         "parameters must be an array of parameter UUIDs"),
        (json!({"op": "animation.physics.bake", "name": "Run", "parameters": [0]}),
         "parameters must be parameter UUIDs"),
        (json!({"op": "animation.physics.bake", "name": "Run", "parameters": [11, 11]}),
         "parameter 11 is listed twice"),
        (json!({"op": "animation.physics.bake", "name": "Run", "parameters": [999]}),
         "parameter 999 was not found"),
        (json!({"op": "animation.physics.bake", "name": "Run", "parameters": [40]}),
         "parameter 40 is a parameter group"),
        (json!({"op": "animation.physics.bake", "name": "Run", "parameters": [MOVE]}),
         "parameter 10 is not the output of an enabled SimplePhysics node"),
        (json!({"op": "animation.physics.bake", "name": "Run", "parameters": [LIMB]}),
         "parameter 14 is not the output of an enabled SimplePhysics node"),
        (json!({"op": "animation.physics.bake", "name": "Run", "from": -1}),
         "from must be an integer frame from 0 to 10"),
        (json!({"op": "animation.physics.bake", "name": "Run", "from": 1.5}),
         "from must be an integer frame from 0 to 10"),
        (json!({"op": "animation.physics.bake", "name": "Run", "to": 11}),
         "to must be an integer frame from 0 to 10"),
        (json!({"op": "animation.physics.bake", "name": "Run", "from": 5, "to": 4}),
         "from must not be after to"),
        (json!({"op": "animation.physics.bake", "name": "Run", "warmup": "yes"}),
         "warmup must be a boolean"),
    ] {
        assert_eq!(fail(&mut session, request.clone()), expect, "{request}");
        assert_eq!(session.snapshot(), opened, "{request}");
    }
    assert_eq!(session.snapshot()["canUndo"], false, "a refused bake left no history");
}

#[test]
fn clip_state_that_would_not_draw_the_bake_refuses_it() {
    let (mut session, _dir) = session_with("additive", &model());
    ok(&mut session, json!({"op": "animation.set", "name": "Run", "additive": true}));
    let kept = session.snapshot();
    assert!(fail(&mut session, json!({"op": "animation.physics.bake", "name": "Run"})).contains("additive"));
    assert_eq!(session.snapshot(), kept);

    let (mut session, _dir) = session_with("weight", &model());
    ok(&mut session, json!({"op": "animation.set", "name": "Run", "animationWeight": 0.5}));
    let kept = session.snapshot();
    assert!(fail(&mut session, json!({"op": "animation.physics.bake", "name": "Run"}))
        .contains("animationWeight"));
    assert_eq!(session.snapshot(), kept);

    let (mut session, _dir) = session_with("freeze", &model());
    ok(&mut session, json!({"op": "animation.freeze.set", "name": "Run", "frame": 0, "mark": "all"}));
    let kept = session.snapshot();
    assert!(fail(&mut session, json!({"op": "animation.physics.bake", "name": "Run"})).contains("freeze"));
    assert_eq!(session.snapshot(), kept);

    for (label, lane_set) in [
        ("stepped", json!({"interpolation": "Stepped"})),
        ("nearest", json!({"interpolation": "Nearest"})),
        ("additive", json!({"merge_mode": "Additive"})),
        ("multiplicative", json!({"merge_mode": "Multiplicative"})),
    ] {
        let (mut session, _dir) = session_with(label, &model());
        ok(&mut session, json!({"op": "animation.key.set", "name": "Run", "parameter": SWING,
                                "axis": 0, "frame": 0, "value": 0.0}));
        let mut request = json!({"op": "animation.lane.set", "name": "Run", "parameter": SWING, "axis": 0});
        for (key, value) in lane_set.as_object().unwrap() {
            request[key] = value.clone();
        }
        ok(&mut session, request);
        let kept = session.snapshot();
        assert!(fail(&mut session, json!({"op": "animation.physics.bake", "name": "Run"}))
            .contains("Linear and Forced"), "{label}");
        assert_eq!(session.snapshot(), kept, "{label}");
    }

    let mut doubled = model();
    for _ in 0..2 {
        doubled["animations"]["Run"]["lanes"].as_array_mut().unwrap().push(
            json!({"uuid": SWING, "target": 0, "interpolation": "Linear", "merge_mode": "Forced",
                   "keyframes": [{"frame": 0, "value": 0.0, "tension": 0.5}]}),
        );
    }
    let (mut session, _dir) = session_with("duplicate", &doubled);
    let kept = session.snapshot();
    assert!(fail(&mut session, json!({"op": "animation.physics.bake", "name": "Run"}))
        .contains("two lanes on axis 0"));
    assert_eq!(session.snapshot(), kept);

    let (mut session, _dir) = session_with("blink", &model());
    ok(&mut session, json!({"op": "animation.blink.parameters.set", "parameters": [TILT]}));
    ok(&mut session, json!({"op": "animation.blink.set", "name": "Run", "frame": 0, "value": 0.5}));
    let kept = session.snapshot();
    assert!(fail(&mut session, json!({"op": "animation.physics.bake", "name": "Run"})).contains("blink"));
    assert_eq!(session.snapshot(), kept);

    let (mut session, _dir) = session_with("framestep", &model());
    ok(&mut session, json!({"op": "animation.frame_step", "name": "Run",
                            "keyframes": [{"frame": 0, "value": 2}]}));
    let kept = session.snapshot();
    assert!(fail(&mut session, json!({"op": "animation.physics.bake", "name": "Run"}))
        .contains("frame stepping"));
    assert_eq!(session.snapshot(), kept);

    let mut formed = model();
    formed["animations"]["Run"]["forms"] = json!([{"node": 2, "mode": 1, "keyframes": []}]);
    let (mut session, _dir) = session_with("forms", &formed);
    let kept = session.snapshot();
    assert!(fail(&mut session, json!({"op": "animation.physics.bake", "name": "Run"}))
        .contains("form edits or form timing"));
    assert_eq!(session.snapshot(), kept);

    let mut pathed = model();
    pathed["nodes"]["children"].as_array_mut().unwrap().push(json!({
        "uuid": 20, "name": "Path", "type": "PathDeformer", "enabled": true, "zsort": 0.0,
        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
        "curve_type": "Bezier", "vertices": [0.0, 0.0, 0.0, 10.0],
        "physics": {"type": "Pendulum", "gravity": 0.0, "input_scale": 0.0}, "children": [],
    }));
    let (mut session, _dir) = session_with("path", &pathed);
    let kept = session.snapshot();
    assert!(fail(&mut session, json!({"op": "animation.physics.bake", "name": "Run"}))
        .contains("PathDeformer"));
    assert_eq!(session.snapshot(), kept);

    let mut dangling = model();
    dangling["nodes"]["children"][1]["param"] = json!(999);
    let (mut session, _dir) = session_with("dangling", &dangling);
    let kept = session.snapshot();
    assert!(fail(&mut session, json!({"op": "animation.physics.bake", "name": "Run"}))
        .contains("parameter 999 was not found"));
    assert_eq!(session.snapshot(), kept);

    let mut quiet = model();
    quiet["nodes"]["children"][0]["enabled"] = json!(false);
    let (mut session, _dir) = session_with("quiet", &quiet);
    let kept = session.snapshot();
    assert!(fail(&mut session, json!({"op": "animation.physics.bake", "name": "Run"}))
        .contains("no enabled SimplePhysics node"));
    assert_eq!(session.snapshot(), kept);
}

#[test]
fn limits_and_out_of_range_outputs_refuse_the_bake() {
    let (mut session, _dir) = session_with("timestep", &model());
    ok(&mut session, json!({"op": "animation.set", "name": "Run", "timestep": 12.0}));
    let kept = session.snapshot();
    assert!(fail(&mut session, json!({"op": "animation.physics.bake", "name": "Run"}))
        .contains("physics step limit"));
    assert_eq!(session.snapshot(), kept);

    let (mut session, _dir) = session_with("steps", &model());
    ok(&mut session, json!({"op": "animation.set", "name": "Run", "length": 5000}));
    let kept = session.snapshot();
    assert!(fail(&mut session, json!({"op": "animation.physics.bake", "name": "Run"}))
        .contains("the limit is 4000"));
    assert_eq!(session.snapshot(), kept);

    let mut heavy = model();
    for extra in 0..25_u32 {
        heavy["nodes"]["children"][0]["children"].as_array_mut().unwrap().push(simple(
            100 + extra, "Extra", Some(200 + extra), "XY", 30.0,
        ));
        heavy["param"].as_array_mut().unwrap().push(json!({
            "uuid": 200 + extra, "name": format!("Extra {extra}"), "is_vec2": true,
            "min": [-2.0, -2.0], "max": [2.0, 2.0], "defaults": [0.0, 0.0],
            "axis_points": [[0.0, 1.0], [0.0, 1.0]], "merge_mode": "Additive", "bindings": [],
        }));
    }
    heavy["animations"]["Run"]["length"] = json!(4000);
    let (mut session, _dir) = session_with("keys", &heavy);
    let kept = session.snapshot();
    assert!(fail(&mut session, json!({"op": "animation.physics.bake", "name": "Run",
                                      "warmup": false, "to": 3999}))
        .contains("the limit is 200000"));
    assert_eq!(session.snapshot(), kept);

    let (mut session, _dir) = session_with("range", &model());
    ok(&mut session, json!({"op": "physics.set", "nodes": [3], "output_scale": [100.0, 100.0]}));
    let kept = session.snapshot();
    assert!(fail(&mut session, json!({"op": "animation.physics.bake", "name": "Run"}))
        .contains("outside its range"));
    assert_eq!(session.snapshot(), kept);
}

#[test]
fn a_late_simulation_error_leaves_the_live_physics_runtime_untouched() {
    let (mut baked, _a) = session_with("live-bake", &model());
    let (mut control, _b) = session_with("live-control", &model());
    for session in [&mut baked, &mut control] {
        ok(session, json!({"op": "physics.set", "nodes": [3], "output_scale": [100.0, 100.0]}));
        ok(session, json!({"op": "animation.seek", "name": "Run", "frame": 0}));
        ok(session, json!({"op": "animation.advance", "dt": TIMESTEP}));
        ok(session, json!({"op": "physics.step", "dt": TIMESTEP}));
    }
    let before = baked.snapshot();
    let history = ok(&mut baked, json!({"op": "history.summary"}));
    assert!(fail(&mut baked, json!({"op": "animation.physics.bake", "name": "Run"}))
        .contains("outside its range"));
    assert_eq!(baked.snapshot(), before);
    assert_eq!(ok(&mut baked, json!({"op": "history.summary"})), history);
    for session in [&mut baked, &mut control] {
        ok(session, json!({"op": "animation.advance", "dt": TIMESTEP}));
    }
    assert_eq!(
        ok(&mut baked, json!({"op": "physics.step", "dt": TIMESTEP})),
        ok(&mut control, json!({"op": "physics.step", "dt": TIMESTEP}))
    );
}

#[test]
fn native_and_empty_documents_refuse_the_bake() {
    let mut empty = Session::new();
    let kept = empty.snapshot();
    assert_eq!(
        fail(&mut empty, json!({"op": "animation.physics.bake", "name": "Run"})),
        "animation \"Run\" was not found"
    );
    assert_eq!(empty.snapshot(), kept);

    let dir = temp_dir("native");
    std::fs::write(dir.join("m.cmo3"), authoring_bytes(&native_artmesh_xml(""))).unwrap();
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": dir.join("m.cmo3").to_str().unwrap()}));
    let kept = session.snapshot();
    assert!(fail(&mut session, json!({"op": "animation.physics.bake", "name": "Run"}))
        .contains("not supported"));
    assert_eq!(session.snapshot(), kept);
}

fn authoring_bytes(xml: &str) -> Vec<u8> {
    use flate2::write::DeflateEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(xml.as_bytes()).unwrap();
    let compressed = encoder.finish().unwrap();
    let mut zip = Vec::new();
    zip.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
    zip.extend_from_slice(&20u16.to_le_bytes());
    zip.extend_from_slice(&0x0808u16.to_le_bytes());
    zip.extend_from_slice(&8u16.to_le_bytes());
    zip.extend_from_slice(&[0; 4]);
    zip.extend_from_slice(&[0; 12]);
    zip.extend_from_slice(&8u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(b"contents");
    zip.extend_from_slice(&compressed);
    zip.extend_from_slice(b"PK\x07\x08");
    zip.extend_from_slice(&crc32fast::hash(xml.as_bytes()).to_le_bytes());
    zip.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
    zip.extend_from_slice(&(xml.len() as u32).to_le_bytes());

    let seed = 0x562e_099f_u32;
    let mask = seed as u8;
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([255, 255, 255, 255]),
    ))
    .write_to(&mut png, image::ImageFormat::Png)
    .unwrap();
    let entries = [
        ("main.xml", "main_xml", [0x01, 0x21], zip),
        ("mesh.png", "", [0x01, 0x10], png.into_inner()),
        ("opaque.bin", "", [0x01, 0x10], vec![0, 1, 2, 0xff]),
    ];
    let mut bytes = vec![0; 58];
    bytes[..4].copy_from_slice(b"CAFF");
    bytes[4..8].copy_from_slice(&45u32.to_be_bytes());
    bytes[14..18].copy_from_slice(&seed.to_be_bytes());
    bytes[54..58].copy_from_slice(&((entries.len() as u32) ^ seed).to_be_bytes());
    let mut fields = Vec::new();
    for (name, alias, flags, _) in &entries {
        bytes.push(name.len() as u8 ^ mask);
        bytes.extend(name.as_bytes().iter().map(|byte| byte ^ mask));
        bytes.push(alias.len() as u8 ^ mask);
        bytes.extend(alias.as_bytes().iter().map(|byte| byte ^ mask));
        bytes.extend_from_slice(&(0x1234_5678u32 ^ seed).to_be_bytes());
        let offset_field = bytes.len();
        bytes.extend_from_slice(&seed.to_be_bytes());
        let size_field = bytes.len();
        bytes.extend_from_slice(&seed.to_be_bytes());
        bytes.extend(flags.iter().map(|byte| byte ^ mask));
        bytes.extend([0; 8].iter().map(|byte| byte ^ mask));
        fields.push((offset_field, size_field));
    }
    for (index, (_, _, _, payload)) in entries.iter().enumerate() {
        let offset = bytes.len();
        bytes[fields[index].0..fields[index].0 + 4]
            .copy_from_slice(&((offset as u32 ^ seed).to_be_bytes()));
        bytes[fields[index].1..fields[index].1 + 4]
            .copy_from_slice(&((payload.len() as u32 ^ seed).to_be_bytes()));
        bytes.extend(payload.iter().map(|byte| byte ^ mask));
    }
    bytes.extend_from_slice(b"bc");
    bytes
}

fn native_artmesh_xml(extra: &str) -> String {
    format!(
        r#"<root fileFormatVersion='401000004'><CModelSource>
      <s xs.n='name'>Fixture</s><CImageCanvas xs.n='canvas'><i xs.n='pixelWidth'>100</i><i xs.n='pixelHeight'>100</i></CImageCanvas>
      <CParameterSourceSet xs.n='parameterSourceSet'><carray_list xs.n='_sources' count='1'><CParameterSource><CParameterGuid xs.n='guid' uuid='parameter-guid'/><CParameterId xs.n='id' idstr='ParamTest'/><f xs.n='minValue'>-1</f><f xs.n='maxValue'>1</f><f xs.n='defaultValue'>0</f></CParameterSource></carray_list></CParameterSourceSet>
      <CDrawableSourceSet xs.n='drawableSourceSet'><carray_list xs.n='_sources' count='1'><CArtMeshSource xs.ref='mesh'/></carray_list></CDrawableSourceSet>
      <CDeformerSourceSet xs.n='deformerSourceSet'><carray_list xs.n='_sources' count='0'/></CDeformerSourceSet>
      <CPartSourceSet xs.n='partSourceSet'><carray_list xs.n='_sources' count='1'><CPartSource xs.ref='part'/></carray_list></CPartSourceSet>
      <CPartSource xs.n='rootPart' xs.ref='part'/>
    </CModelSource>
    <CArtMeshSource xs.id='mesh'><ACDrawableSource xs.n='super'><CArtMeshGuid xs.n='guid' uuid='mesh-guid'/><CArtMeshId xs.n='id' idstr='mesh'/><CPartGuid xs.n='parentGuid' uuid='part-guid'/></ACDrawableSource>
      <KeyformGridSource xs.n='keyformGridSource'><array_list xs.n='keyformBindings' count='0'/><array_list xs.n='keyformsOnGrid' count='1'><KeyformOnGrid><KeyformGridAccessKey xs.n='accessKey'><array_list xs.n='_keyOnParameterList' count='0'/></KeyformGridAccessKey><CFormGuid xs.n='keyformGuid' uuid='form-guid'/></KeyformOnGrid></array_list></KeyformGridSource>
      <float-array xs.n='positions' count='6'>0 0 1 0 1 1</float-array><float-array xs.n='uvs' count='6'>0 0 1 0 1 1</float-array><int-array xs.n='indices' count='3'>0 1 2</int-array>
      <GTexture2D xs.n='texture' xs.ref='texture'/><carray_list xs.n='keyforms' count='1'><CArtMeshForm><ACDrawableForm xs.n='super'><ACForm xs.n='super'>
        <CFormGuid xs.n='guid' uuid='form-guid'/><CArtMeshSource xs.n='_source' xs.ref='mesh'/>
      </ACForm><CoordType xs.n='coordType'><s xs.n='coordName'>Canvas</s></CoordType>{extra}<CFloatColor xs.n='multiplyColor' red='1' green='1' blue='1' alpha='1'/>
      </ACDrawableForm><float-array xs.n='positions' count='6'>1 2 3 4 5 6</float-array></CArtMeshForm></carray_list>
    </CArtMeshSource><CPartSource xs.id='part'><CPartGuid xs.n='guid' uuid='part-guid'/><CPartId xs.n='id' idstr='root'/><carray_list xs.n='keyforms' count='0'/></CPartSource>
    <GTexture2D xs.id='texture'><GTexture xs.n='super'><GTextureGuid xs.n='guid' uuid='texture-guid'/><FilterMode xs.n='filterMode'><MinFilter xs.n='minFilter' v='LINEAR'/><MagFilter xs.n='magFilter' v='LINEAR'/></FilterMode></GTexture>
      <CImageResource xs.n='srcImageResource' width='2' height='3'><file xs.n='imageFileBuf' path='mesh.png'/></CImageResource><null xs.n='transformImageResource01toLogical01'/>
    </GTexture2D></root>"#
    )
}
