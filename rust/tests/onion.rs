//! `animation.onion` and `animation.trajectory` (Cubism UI08).

mod common;

use base64::Engine;
use serde_json::{json, Value};
use soul2d_core::model_import::{inx_bytes, Texture};
use soul2d_core::Session;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn temp_dir(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("soul2d-app-shell-{}-{label}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    common::scratch(dir)
}

fn png(width: u32, height: u32, pixels: &[[u8; 4]]) -> Vec<u8> {
    let raw: Vec<u8> = pixels.iter().flatten().copied().collect();
    let image = image::RgbaImage::from_raw(width, height, raw).unwrap();
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

fn part(id: u32, name: &str, slot: u32, children: Value) -> Value {
    json!({
        "uuid": id, "name": name, "type": "Part", "enabled": true, "zsort": 0.0,
        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
        "lockToRoot": false, "pinToMesh": false,
        "mesh": {"verts": [-1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0], "uvs": [0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0],
                 "indices": [0, 1, 2, 2, 1, 3], "origin": [0.0, 0.0]},
        "textures": [slot], "blend_mode": "Normal", "tint": [1.0, 1.0, 1.0], "screenTint": [0.0, 0.0, 0.0],
        "emissionStrength": 1.0, "mask_threshold": 0.5, "opacity": 1.0, "children": children,
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

fn model(children: Vec<Value>, params: Vec<Value>, animations: Value) -> Value {
    json!({
        "meta": {"name": "", "version": "1.0-alpha"}, "physics": {},
        "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
                  "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                  "lockToRoot": false, "pinToMesh": false, "children": children},
        "param": params, "automation": [], "animations": animations,
    })
}

fn write_inp(dir: &Path, file: &str, model: &Value, textures: &[Vec<u8>]) -> String {
    let textures: Vec<Texture> = textures.iter().map(|data| Texture { tag: 0, data: data.clone() }).collect();
    let path = dir.join(file);
    fs::write(&path, inx_bytes(model, &textures).unwrap()).unwrap();
    path.to_str().unwrap().to_owned()
}

/// A 10 x 10 white Part (id 2) that parameter 10 moves 10 units right, and animation Walk (keys at frames 0 and 10, length 11) that takes
/// the parameter from 0 to 1: at frame f the Part is f units right of where it was.
fn session(dir: &Path) -> Session {
    let mut face = part(2, "Face", 0, json!([]));
    face["mesh"]["verts"] = json!([-5.0, -5.0, -5.0, 5.0, 5.0, -5.0, 5.0, 5.0]);
    let walk = json!({"length": 11, "timestep": 0.1, "additive": false, "animationWeight": 1.0, "leadIn": -1, "leadOut": -1,
        "lanes": [{"uuid": 10, "target": 0, "interpolation": "Linear", "merge_mode": "Forced",
                   "keyframes": [{"frame": 0, "value": 0.0, "tension": 0.5}, {"frame": 10, "value": 1.0, "tension": 0.5}]}]});
    let path = write_inp(dir, "o.inp", &model(vec![face], vec![parameter(10, "Move", 2)], json!({"Walk": walk})),
                         &[png(2, 2, &[[255, 255, 255, 255]; 4])]);
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": path}));
    session
}

fn pixel(png: &Value, x: u32, y: u32) -> [u8; 4] {
    let data = base64::engine::general_purpose::STANDARD.decode(png.as_str().unwrap()).unwrap();
    image::load_from_memory(&data).unwrap().to_rgba8().get_pixel(x, y).0
}

fn near(actual: [u8; 4], expected: [u8; 4]) {
    assert!(actual.iter().zip(expected).all(|(a, e)| a.abs_diff(e) <= 2), "{actual:?} != {expected:?}");
}

#[test]
fn trajectory_follows_vertices_through_the_frames_and_leaves_the_preview_alone() {
    let dir = temp_dir("trajectory");
    let mut session = session(&dir);
    ok(&mut session, json!({"op": "animation.seek", "name": "Walk", "frame": 3}));
    let before = session.snapshot();
    let result = ok(&mut session, json!({"op": "animation.trajectory", "name": "Walk", "node": 2, "step": 5, "vertices": [0, 3]}));
    assert_eq!(result["frames"], json!([0.0, 5.0, 10.0]));
    assert_eq!(result["paths"], json!([
        {"vertex": 0, "points": [[-5.0, -5.0], [0.0, -5.0], [5.0, -5.0]]},
        {"vertex": 3, "points": [[5.0, 5.0], [10.0, 5.0], [15.0, 5.0]]},
    ]));
    assert_eq!(ok(&mut session, json!({"op": "animation.trajectory", "name": "Walk", "node": 2, "from": 4, "to": 6}))["paths"].as_array().unwrap().len(), 4);
    assert_eq!(session.snapshot(), before, "the preview at frame 3 is still there");
}

#[test]
fn trajectory_thins_big_meshes_and_long_clips_when_asked_for_the_default() {
    let dir = temp_dir("trajectory-big");
    let mut big = part(2, "Big", 0, json!([]));
    let verts: Vec<f64> = (0..300).flat_map(|index| [f64::from(index), 0.0]).collect();
    big["mesh"]["verts"] = json!(verts);
    big["mesh"]["uvs"] = json!(vec![0.0; 600]);
    big["mesh"]["indices"] = json!([0, 1, 2]);
    let long = json!({"length": 5000, "timestep": 0.1, "additive": false, "animationWeight": 1.0, "leadIn": -1, "leadOut": -1,
        "lanes": [{"uuid": 10, "target": 0, "interpolation": "Linear", "merge_mode": "Forced",
                   "keyframes": [{"frame": 0, "value": 0.0, "tension": 0.5}, {"frame": 4999, "value": 1.0, "tension": 0.5}]}]});
    let path = write_inp(&dir, "b.inp", &model(vec![big], vec![parameter(10, "Move", 2)], json!({"Long": long})),
                         &[png(2, 2, &[[255, 255, 255, 255]; 4])]);
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": path}));
    let result = ok(&mut session, json!({"op": "animation.trajectory", "name": "Long", "node": 2}));
    let paths = result["paths"].as_array().unwrap();
    assert_eq!((paths.len(), paths[1]["vertex"].as_u64()), (150, Some(2)), "every second of 300 vertices");
    let frames = result["frames"].as_array().unwrap();
    assert_eq!((frames.len(), frames[1].as_f64()), (1667, Some(3.0)), "every third frame of 5000 keeps within 2000");
    assert_eq!(paths[0]["points"].as_array().unwrap().len(), 1667);
}

#[test]
fn trajectory_refuses_what_it_cannot_follow() {
    let dir = temp_dir("trajectory-errors");
    let mut session = session(&dir);
    ok(&mut session, json!({"op": "animation.seek", "name": "Walk", "frame": 3}));
    let before = session.snapshot();
    let ask = |extra: Value| {
        let mut request = json!({"op": "animation.trajectory", "name": "Walk", "node": 2});
        request.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        request
    };
    assert!(fail(&mut session, ask(json!({"name": "Nope"}))).contains("Nope"));
    assert_eq!(fail(&mut session, ask(json!({"node": 1}))), "node 1 draws no mesh");
    assert_eq!(fail(&mut session, ask(json!({"vertices": [4]}))), "vertices must be indices below 4");
    assert_eq!(fail(&mut session, ask(json!({"to": 11}))), "to must be 0 to 10");
    assert_eq!(fail(&mut session, ask(json!({"from": 6, "to": 4}))), "to must not be before from");
    assert!(fail(&mut session, ask(json!({"step": 0.001}))).contains("raise step"));
    let tenths = ok(&mut session, ask(json!({"from": 0.1, "to": 0.3, "step": 0.1, "vertices": [0]})));
    assert_eq!(tenths["frames"].as_array().unwrap().len(), 3, "the end frame stays in despite float rounding");
    assert_eq!(fail(&mut session, ask(json!({"step": 0}))), "step must be above 0");
    assert_eq!(session.snapshot(), before, "the preview survives every refusal");
}

#[test]
fn onion_file_output_protects_the_open_document_and_keeps_other_output_modes() {
    let dir = temp_dir("onion-output");
    let mut session = session(&dir);
    let source = dir.join("o.inp");
    let bytes = fs::read(&source).unwrap();
    let before = session.snapshot();
    let ask = |path: Value| json!({"op":"animation.onion","name":"Walk","frame":5,"size":16,"path":path});
    assert!(fail(&mut session, ask(json!(source))).contains("would overwrite the open document"));
    #[cfg(unix)]
    {
        let alias = dir.join("alias.png");
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        assert!(fail(&mut session, ask(json!(alias))).contains("would overwrite the open document"));
    }
    let target = dir.join("onion.png");
    fs::write(&target, b"replace this output").unwrap();
    let output = ok(&mut session, ask(json!(target)));
    assert_eq!(fs::read(&target).unwrap(), base64::engine::general_purpose::STANDARD.decode(output["png"].as_str().unwrap()).unwrap());
    assert_eq!(ok(&mut session, ask(Value::Null))["png"], output["png"]);
    for path in [json!(""), json!(format!("{}/", dir.display())), json!(123)] {
        fail(&mut session, ask(path));
    }
    assert_eq!(fs::read(&source).unwrap(), bytes);
    assert_eq!(session.snapshot(), before);
}

#[test]
fn onion_draws_past_frames_red_and_future_frames_blue_behind_the_current_one() {
    let dir = temp_dir("onion");
    let mut session = session(&dir);
    ok(&mut session, json!({"op": "physics.step", "dt": 0.1}));
    let before = session.snapshot();
    assert_eq!(before["physics"]["active"], true, "there is physics output to lose");
    let result = ok(&mut session, json!({"op": "animation.onion", "name": "Walk", "frame": 5, "before": 1, "after": 1, "step": 5, "size": 22}));
    // Frames 0 and 10 span x -5..5 and 5..15; with the margin the union is -6..16 by -6..6 at one pixel per unit.
    assert_eq!((result["width"].as_u64(), result["height"].as_u64()), (Some(22), Some(12)));
    assert_eq!(result["frames"], json!([0.0, 10.0, 5.0]));
    let png = &result["png"];
    near(pixel(png, 4, 6), [255, 64, 64, 77]);
    near(pixel(png, 17, 6), [64, 64, 255, 77]);
    near(pixel(png, 10, 6), [255, 255, 255, 255]);
    near(pixel(png, 0, 0), [0, 0, 0, 0]);
    assert_eq!(session.snapshot(), before, "the session is unchanged");

    // The same picture from the view the editor's camera gives.
    let viewed = ok(&mut session, json!({"op": "animation.onion", "name": "Walk", "frame": 5, "before": 1, "after": 1, "step": 5,
                                         "bounds": [-6.0, -6.0, 16.0, 6.0], "width": 22, "height": 12}));
    assert_eq!(viewed["png"], result["png"]);
    let zoomed = ok(&mut session, json!({"op": "animation.onion", "name": "Walk", "frame": 5, "before": 1, "after": 1, "step": 5,
                                         "bounds": [-6.0, -6.0, 5.0, 6.0], "width": 22, "height": 24}));
    near(pixel(&zoomed["png"], 20, 12), [255, 255, 255, 255]);
    near(pixel(&zoomed["png"], 10, 12), [255, 64, 64, 77]);
    let faint = ok(&mut session, json!({"op": "animation.onion", "name": "Walk", "frame": 5, "before": 1, "after": 0, "step": 5, "size": 22, "opacity": 0.5}));
    assert_eq!(faint["frames"], json!([0.0, 5.0]), "no future frame");
    near(pixel(&faint["png"], 4, 6), [255, 64, 64, 128]);
    let edge = ok(&mut session, json!({"op": "animation.onion", "name": "Walk", "frame": 0, "before": 2, "after": 0}));
    assert_eq!(edge["frames"], json!([0.0]), "frames before the start are left out");
    let end = ok(&mut session, json!({"op": "animation.onion", "name": "Walk", "frame": 10, "before": 0, "after": 2, "step": 5}));
    assert_eq!(end["frames"], json!([10.0]), "frames past the end are left out");
    let step = ok(&mut session, json!({"op": "animation.onion", "name": "Walk", "frame": 10, "before": 0, "after": 1, "step": 1}));
    assert_eq!(step["frames"], json!([10.0]), "the clip ends at frame 10, its last key, not at its length of 11");
    let later = ok(&mut session, json!({"op": "animation.onion", "name": "Walk", "frame": 5, "before": 0, "after": 1, "step": 5}));
    assert_eq!(later["frames"], json!([10.0, 5.0]), "no past frame");

    // Two ghosts each side, 2.5 frames apart: the farther one alone is 0.15 of 255, and the texel at the Part's edge 80% of that.
    let far = ok(&mut session, json!({"op": "animation.onion", "name": "Walk", "frame": 5, "step": 2.5, "size": 22}));
    assert_eq!(far["frames"], json!([0.0, 10.0, 2.5, 7.5, 5.0]));
    near(pixel(&far["png"], 2, 6), [255, 64, 64, 31]);
}

#[test]
fn onion_refuses_bad_settings() {
    let dir = temp_dir("onion-errors");
    let mut session = session(&dir);
    let ask = |extra: Value| {
        let mut request = json!({"op": "animation.onion", "name": "Walk", "frame": 5});
        request.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        request
    };
    assert_eq!(fail(&mut session, ask(json!({"before": 9}))), "before must be 0 to 8");
    assert_eq!(fail(&mut session, ask(json!({"opacity": 2}))), "opacity must be 0 to 1");
    assert_eq!(fail(&mut session, ask(json!({"step": 0}))), "step must be above 0");
    assert_eq!(fail(&mut session, ask(json!({"size": 4}))), "size must be 16 to 4096");
    assert_eq!(fail(&mut session, ask(json!({"size": 5000}))), "size must be 16 to 4096");
    assert_eq!(fail(&mut session, ask(json!({"path": 5}))), "path must be a string");
    assert!(fail(&mut session, ask(json!({"name": "Nope"}))).contains("Nope"));
    assert_eq!(fail(&mut session, ask(json!({"width": 10}))), "bounds, width and height go together");
    assert_eq!(fail(&mut session, ask(json!({"bounds": [0, 0, 1, 1], "width": 5000, "height": 10}))), "width and height must be 1 to 4096");
    assert_eq!(fail(&mut session, ask(json!({"bounds": [1, 0, 0, 1], "width": 5, "height": 10}))), "bounds must have increasing min and max coordinates");
}
