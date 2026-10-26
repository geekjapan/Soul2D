//! `rig.bake_mmd_motion` through `Session::handle_request` and the MCP tool. Every VMD stream is built here (the byte
//! builders are a copy of the ones in `mmd_pose_apply.rs`); no third-party file is read.

use serde_json::{json, Value};
use soul2d_core::Session;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

// Shift-JIS names, pinned as byte literals.
const CENTER: &[u8] = &[0x83, 0x5A, 0x83, 0x93, 0x83, 0x5E, 0x81, 0x5B]; // センター
const HEAD: &[u8] = &[0x93, 0xAA]; // 頭
const LOWER_BODY: &[u8] = &[0x89, 0xBA, 0x94, 0xBC, 0x90, 0x67]; // 下半身

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A scratch directory that is removed when the test ends.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp_dir() -> TempDir {
    let dir = std::env::temp_dir().join(format!(
        "soul2d-mmd-bake-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// (x, y, z, w) of a rotation of `degrees` about the MMD axis (`0` x, `1` y, `2` z).
fn about(axis: usize, degrees: f64) -> [f64; 4] {
    let half = degrees.to_radians() / 2.0;
    let mut q = [0.0, 0.0, 0.0, half.cos()];
    q[axis] = half.sin();
    q
}

fn padded(bytes: &[u8], width: usize) -> Vec<u8> {
    let mut out = bytes.to_vec();
    out.resize(width, 0);
    out
}

/// A 111-byte bone key with the default linear MMD curve.
fn vmd_bone(name: &[u8], frame: u32, position: [f32; 3], rotation: [f64; 4]) -> Vec<u8> {
    let mut out = padded(name, 15);
    out.extend(frame.to_le_bytes());
    for value in position {
        out.extend(value.to_le_bytes());
    }
    for value in rotation {
        out.extend((value as f32).to_le_bytes());
    }
    out.extend((0..64).map(|index| [20u8, 20, 107, 107][(index / 4) % 4]));
    assert_eq!(out.len(), 111);
    out
}

fn vmd(bones: &[Vec<u8>]) -> Vec<u8> {
    let mut out = padded(b"Vocaloid Motion Data 0002", 30);
    out.extend(padded(b"model", 20));
    out.extend((bones.len() as u32).to_le_bytes());
    bones.iter().for_each(|bone| out.extend(bone));
    out.extend(0u32.to_le_bytes());
    out
}

fn write(dir: &TempDir, name: &str, bytes: &[u8]) -> String {
    let path = dir.0.join(name);
    fs::write(&path, bytes).unwrap();
    path.to_str().unwrap().to_owned()
}

/// A head that turns 30 degrees about MMD x over 30 frames, and a centre that moves one unit along x.
fn walk(dir: &TempDir) -> String {
    write(dir, "walk.vmd", &vmd(&[
        vmd_bone(HEAD, 0, [0.0; 3], about(0, 0.0)),
        vmd_bone(HEAD, 30, [0.0; 3], about(0, 30.0)),
        vmd_bone(CENTER, 0, [0.0; 3], about(0, 0.0)),
        vmd_bone(CENTER, 30, [1.0, 0.0, 0.0], about(0, 0.0)),
    ]))
}

// -------------------------------------------------------------------------------------------------------- rig

struct Rig {
    session: Session,
    root: u64,
    line: u64,
    pose: u64,
    grid: u64,
    bones: BTreeMap<String, u64>,
}

fn run(session: &mut Session, op: &str, body: Value) -> Value {
    let mut request = body;
    request["op"] = json!(op);
    session.handle_request(&request)
}

fn ok(session: &mut Session, op: &str, body: Value) -> Value {
    let reply = run(session, op, body);
    assert_eq!(reply["succeeded"], true, "{op}: {reply}");
    reply["result"].clone()
}

/// The standard skeleton, a 1D parameter "Line" and a 2D parameter "Pose", both with three keypoints per axis.
fn rig() -> Rig {
    let mut session = Session::new();
    let root = ok(&mut session, "node.create", json!({"className": "DepthRigRoot", "name": "Rig"}))["created"][0]
        ["uuid"]
        .as_u64()
        .unwrap();
    let listed = ok(&mut session, "rig.add_skeleton", json!({"root": root}));
    let bones: BTreeMap<String, u64> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["boneId"].as_str().unwrap().to_owned(), item["uuid"].as_u64().unwrap()))
        .collect();
    let line = ok(&mut session, "parameter.create", json!({"name": "Line", "dimensions": 1}))["uuid"].as_u64().unwrap();
    let pose = ok(&mut session, "parameter.create", json!({"name": "Pose", "dimensions": 2}))["uuid"].as_u64().unwrap();
    for id in [line, pose] {
        let parameter = ok(&mut session, "parameter.get", json!({"id": id}));
        assert_eq!(parameter["axis_points"][0].as_array().unwrap().len(), 3, "{parameter}");
    }
    let grid = ok(&mut session, "node.create", json!({"className": "GridDeformer", "name": "Body"}))["created"][0]["uuid"]
        .as_u64()
        .unwrap();
    ok(&mut session, "deformer.define_grid", json!({
        "nodes": [grid], "axisX": [-60.0, -20.0, 20.0, 60.0], "axisY": [-270.0, -230.0, -190.0, -150.0, -100.0, 40.0],
    }));
    for bone in ["Head", "Chest", "Pelvis"] {
        ok(&mut session, "rig.add_source", json!({"root": root, "target": grid, "bone": bones[bone]}));
    }
    Rig { session, root, line, pose, bones, grid }
}

impl Rig {
    fn body(&self, parameter: u64, path: &str, extra: Value) -> Value {
        let mut body = json!({"root": self.root, "parameter": parameter, "path": path});
        for (key, value) in extra.as_object().into_iter().flatten() {
            body[key] = value.clone();
        }
        body
    }

    fn bake(&mut self, parameter: u64, path: &str, extra: Value) -> Value {
        let body = self.body(parameter, path, extra);
        ok(&mut self.session, "rig.bake_mmd_motion", body)
    }

    fn fail(&mut self, body: Value) -> String {
        let before = self.session.snapshot();
        let reply = run(&mut self.session, "rig.bake_mmd_motion", body);
        assert_eq!(reply["succeeded"], false, "{reply}");
        assert_eq!(self.session.snapshot(), before, "a refused request changes nothing");
        reply["message"].as_str().unwrap().to_owned()
    }

    /// The value of one binding channel at `key`, `None` when the binding does not exist.
    fn cell(&mut self, parameter: u64, bone: &str, channel: &str, key: [usize; 2]) -> Option<f64> {
        let parameter = ok(&mut self.session, "parameter.get", json!({"id": parameter}));
        let bone = self.bones[bone];
        let binding = parameter["bindings"]
            .as_array()?
            .iter()
            .find(|binding| binding["node"] == bone && binding["param_name"] == channel)?
            .clone();
        binding["values"][key[0]][key[1]].as_f64()
    }

    fn pitch(&mut self, parameter: u64, key: [usize; 2]) -> f64 {
        self.cell(parameter, "Head", "transform.r.x", key).unwrap_or(0.0)
    }

    /// The physical parameter value of keypoint `index` along `axis`.
    fn key_value(&mut self, parameter: u64, axis: usize, index: usize) -> f64 {
        let parameter = ok(&mut self.session, "parameter.get", json!({"id": parameter}));
        let (min, max) = (parameter["min"][axis].as_f64().unwrap(), parameter["max"][axis].as_f64().unwrap());
        min + (max - min) * parameter["axis_points"][axis][index].as_f64().unwrap()
    }

    fn animation(&self, name: &str) -> Value {
        self.session.snapshot()["model"]["animations"][name].clone()
    }
}

fn settle(rig: &mut Rig) {
    while rig.session.snapshot()["deformPending"] == true {
        ok(&mut rig.session, "deform.continue", json!({}));
    }
}

fn near(actual: f64, expected: f64, tolerance: f64) {
    assert!((actual - expected).abs() <= tolerance, "{actual} vs {expected}");
}

// ------------------------------------------------------------------------------------------------------ tests

#[test]
fn sampled_poses_land_in_successive_keypoints_including_the_zero_before_a_later_channel() {
    let (mut rig, dir) = (rig(), temp_dir());
    let path = walk(&dir);
    let line = rig.line;
    let report = rig.bake(line, &path, json!({"frames": [0, 10, 30]}));

    // 0, 10 and 30 frames into a linear 0..30 degree turn. Key 0 is a written zero: left unset, it would be
    // extrapolated to -10 degrees from the other two keys.
    for (index, degrees) in [0.0, 10.0, 30.0].into_iter().enumerate() {
        near(rig.pitch(line, [index, 0]), f64::to_radians(degrees), 1e-5);
    }
    // The centre moves 0, 1/3 and 1 unit along x, 31.5 rig units per unit; y and z stay zero and get no binding.
    for (index, expected) in [0.0, 10.5, 31.5].into_iter().enumerate() {
        near(rig.cell(line, "Pelvis", "transform.t.x", [index, 0]).unwrap(), expected, 1e-4);
    }
    assert_eq!(rig.cell(line, "Pelvis", "transform.t.y", [0, 0]), None);
    assert_eq!(rig.cell(line, "Head", "transform.r.y", [1, 0]), None);

    assert_eq!(report["keypointsWritten"], 3);
    assert_eq!(report["animation"], Value::Null);
    assert_eq!(report["axis"], 0);
    assert_eq!(report["motionFrameRange"], json!([0, 30]));
    assert_eq!(report["frames"][1], json!({"frame": 10, "key": [1, 0], "namedBones": 1, "translationApplied": true, "locked": []}));
    assert_eq!(report["unmappedBoneCount"], 0);
    assert_eq!(report["locked"], json!([]));
    assert_eq!(report["translationScale"], 31.5);
    // Nothing is listed per bone, so the report stays small however long the motion is.
    assert!(report.to_string().len() < 1000, "{report}");
}

#[test]
fn the_animation_lane_drives_the_parameter_through_the_keypoints() {
    let (mut rig, dir) = (rig(), temp_dir());
    let path = walk(&dir);
    let line = rig.line;
    // Another lane under the same name is replaced, not kept.
    ok(&mut rig.session, "animation.create", json!({"name": "Walk"}));
    ok(&mut rig.session, "animation.key.set", json!({"name": "Walk", "parameter": rig.pose, "axis": 1, "frame": 3, "value": 0.25}));
    // The eye blinking track stays, the keys past the new length of 31 frames go.
    for (frame, value) in [(3, 0.5), (31, 0.0)] {
        ok(&mut rig.session, "animation.blink.set", json!({"name": "Walk", "frame": frame, "value": value, "effect": 50}));
    }
    // So do the audio tracks that start inside it.
    let sound = write(&dir, "sound.wav", b"RIFF\x24\0\0\0WAVEfmt ");
    for frame in [3, 31] {
        ok(&mut rig.session, "animation.audio.add", json!({"name": "Walk", "path": sound, "frame": frame}));
    }
    // And the image and group tracks.
    let sky = dir.0.join("sky.png");
    image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 255, 255])).save(&sky).unwrap();
    ok(&mut rig.session, "animation.group.add", json!({"name": "Walk", "group": "Set", "x": 2}));
    ok(&mut rig.session, "animation.group.set", json!({"name": "Walk", "group": "Set", "model": true}));
    ok(&mut rig.session, "animation.image.add", json!({"name": "Walk", "path": sky, "group": "Set"}));

    let report = rig.bake(line, &path, json!({"frames": [0, 10, 30], "animation": "Walk"}));
    assert_eq!(report["animation"], json!({"name": "Walk", "length": 31, "timestep": 1.0f32 / 30.0f32}));
    let walk = rig.animation("Walk");
    assert_eq!(walk["length"], 31);
    assert_eq!(walk["timestep"].as_f64().unwrap() as f32, 1.0f32 / 30.0f32);
    assert_eq!(walk["lanes"].as_array().unwrap().len(), 1);
    assert_eq!(walk["eyeBlink"], json!({"effect": 50.0, "keyframes": [{"frame": 3, "value": 0.5}]}));
    let sound = fs::canonicalize(&sound).unwrap().to_string_lossy().into_owned();
    assert_eq!(walk["audio"], json!([{"path": sound, "frame": 3}]));
    let sky = fs::canonicalize(&sky).unwrap().to_string_lossy().into_owned();
    let image = json!({"path": sky, "x": 0.0, "y": 0.0, "scale": 1.0, "opacity": 1.0, "group": "Set"});
    assert_eq!(walk["images"], json!([image]));
    assert_eq!(walk["groups"], json!([{"name": "Set", "x": 2.0, "y": 0.0, "scale": 1.0, "opacity": 1.0}]));
    assert_eq!(walk["modelGroup"], "Set");
    let lane = &walk["lanes"][0];
    assert_eq!((&lane["uuid"], &lane["target"], &lane["interpolation"], &lane["merge_mode"]), (&json!(line), &json!(0), &json!("Linear"), &json!("Forced")));
    let keys = lane["keyframes"].as_array().unwrap();
    assert_eq!(keys.iter().map(|k| k["frame"].as_i64().unwrap()).collect::<Vec<_>>(), [0, 10, 30]);
    for (key, index) in keys.iter().zip(0..3) {
        near(key["value"].as_f64().unwrap(), rig.key_value(line, 0, index), 1e-6);
    }

    // Sampling the animation at frame t gives a parameter value; the binding turns it into a head pitch. The motion
    // is linear between the sampled frames, so frame t of the animation is frame t of the VMD: t degrees.
    let points: Vec<f64> = ok(&mut rig.session, "parameter.get", json!({"id": line}))["axis_points"][0]
        .as_array()
        .unwrap()
        .iter()
        .map(|point| point.as_f64().unwrap())
        .collect();
    let (low, high) = (rig.key_value(line, 0, 0), rig.key_value(line, 0, 2));
    let pitches = [rig.pitch(line, [0, 0]), rig.pitch(line, [1, 0]), rig.pitch(line, [2, 0])];
    for frame in [0.0, 5.0, 10.0, 20.0, 30.0] {
        let sampled = ok(&mut rig.session, "animation.sample", json!({"name": "Walk", "frame": frame}));
        let position = (sampled["values"][line.to_string()][0].as_f64().unwrap() - low) / (high - low);
        let cell = usize::from(position > points[1]);
        let t = (position - points[cell]) / (points[cell + 1] - points[cell]);
        let pitch = pitches[cell] + (pitches[cell + 1] - pitches[cell]) * t;
        near(pitch, f64::to_radians(frame), 1e-4);
    }
}

#[test]
fn an_animation_starts_at_the_first_sampled_frame() {
    let (mut rig, dir) = (rig(), temp_dir());
    let path = walk(&dir);
    let line = rig.line;
    let report = rig.bake(line, &path, json!({"frames": [10, 20, 40], "animation": "Late"}));
    assert_eq!(report["animation"]["length"], 31);
    let frames: Vec<i64> =
        rig.animation("Late")["lanes"][0]["keyframes"].as_array().unwrap().iter().map(|k| k["frame"].as_i64().unwrap()).collect();
    assert_eq!(frames, [0, 10, 30]);
    // Keypoints hold the poses of frames 10, 20 and 40 (held at the last key, 30 degrees).
    for (index, degrees) in [10.0, 20.0, 30.0].into_iter().enumerate() {
        near(rig.pitch(line, [index, 0]), f64::to_radians(degrees), 1e-5);
    }
}

#[test]
fn a_2d_parameter_takes_the_motion_along_one_axis_at_a_fixed_other_key() {
    let (mut rig, dir) = (rig(), temp_dir());
    let path = walk(&dir);
    let pose = rig.pose;
    let report = rig.bake(pose, &path, json!({"frames": [0, 30], "axis": 1, "other": 2, "animation": "Sway"}));
    assert_eq!(report["frames"][1]["key"], json!([2, 1]));
    near(rig.pitch(pose, [2, 0]), 0.0, 1e-9);
    near(rig.pitch(pose, [2, 1]), f64::to_radians(30.0), 1e-5);
    let sway = rig.animation("Sway");
    assert_eq!(sway["length"], 31);
    assert_eq!(sway["lanes"][0]["target"], 1);
    near(sway["lanes"][0]["keyframes"][1]["value"].as_f64().unwrap(), rig.key_value(pose, 1, 1), 1e-6);
    // The lane moves the Y axis only; the X axis keeps its value, which stays at the default keypoint used for `other`.
    let sampled = ok(&mut rig.session, "animation.sample", json!({"name": "Sway", "frame": 30}));
    near(sampled["values"][pose.to_string()][1].as_f64().unwrap(), rig.key_value(pose, 1, 1), 1e-6);
}

#[test]
fn every_written_keypoint_gets_its_deform_refreshed() {
    let (mut rig, dir) = (rig(), temp_dir());
    let path = walk(&dir);
    let line = rig.line;
    rig.bake(line, &path, json!({"frames": [0, 10, 30]}));
    settle(&mut rig);
    let parameter = ok(&mut rig.session, "parameter.get", json!({"id": line}));
    let deform = parameter["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|binding| binding["node"] == rig.grid && binding["param_name"] == "deform")
        .expect("the grid gets a deform binding")
        .clone();
    // The head is turned 0, 10 and 30 degrees, so the three keypoints hold three different deforms.
    assert_ne!(deform["values"][1], deform["values"][0], "{deform}");
    assert_ne!(deform["values"][2], deform["values"][1], "{deform}");
}

#[test]
fn a_lower_body_turn_is_baked_with_its_compensation_on_every_frame() {
    let (mut rig, dir) = (rig(), temp_dir());
    let path = write(&dir, "turn.vmd", &vmd(&[
        vmd_bone(LOWER_BODY, 0, [0.0; 3], about(1, 0.0)),
        vmd_bone(LOWER_BODY, 20, [0.0; 3], about(1, 20.0)),
    ]));
    let line = rig.line;
    rig.bake(line, &path, json!({"frames": [0, 10, 20]}));
    // The Spine undoes what the Pelvis does (see rig.apply_mmd_pose), at each sampled key.
    for (index, degrees) in [0.0, 10.0, 20.0].into_iter().enumerate() {
        near(rig.cell(line, "Pelvis", "transform.r.y", [index, 0]).unwrap(), -f64::to_radians(degrees), 1e-5);
        near(rig.cell(line, "Spine", "transform.r.y", [index, 0]).unwrap(), f64::to_radians(degrees), 1e-5);
    }
}

#[test]
fn dry_run_changes_nothing_and_returns_the_report_of_the_real_run() {
    let (mut rig, dir) = (rig(), temp_dir());
    let path = walk(&dir);
    let before = rig.session.snapshot();
    let history = ok(&mut rig.session, "history.summary", json!({}));
    let line = rig.line;
    let dry = rig.bake(line, &path, json!({"frames": [0, 10, 30], "animation": "Walk", "dryRun": true}));
    assert_eq!(rig.session.snapshot(), before, "revision, model and modified flag are unchanged");
    assert_eq!(ok(&mut rig.session, "history.summary", json!({})), history);
    assert_eq!(rig.animation("Walk"), Value::Null);
    let wet = rig.bake(line, &path, json!({"frames": [0, 10, 30], "animation": "Walk"}));
    assert_eq!(dry, wet);
    assert_ne!(rig.session.snapshot()["revision"], before["revision"]);
}

#[test]
fn one_undo_restores_the_document_and_redo_brings_the_bake_back() {
    let (mut rig, dir) = (rig(), temp_dir());
    let path = walk(&dir);
    let before = rig.session.snapshot()["model"].clone();
    let line = rig.line;
    rig.bake(line, &path, json!({"frames": [0, 10, 30], "animation": "Walk"}));
    settle(&mut rig);
    let baked = rig.session.snapshot()["model"].clone();
    assert_ne!(baked, before);
    ok(&mut rig.session, "undo", json!({}));
    assert_eq!(rig.session.snapshot()["model"], before);
    ok(&mut rig.session, "redo", json!({}));
    assert_eq!(rig.session.snapshot()["model"], baked);
}

#[test]
fn refused_inputs_change_nothing_and_name_no_path() {
    let (mut rig, dir) = (rig(), temp_dir());
    let path = walk(&dir);
    let (line, pose) = (rig.line, rig.pose);
    let base = rig.body(line, &path, json!({"frames": [0, 10]}));
    let with = |key: &str, value: Value| {
        let mut body = base.clone();
        body[key] = value;
        body
    };
    let without = |key: &str| {
        let mut body = base.clone();
        body.as_object_mut().unwrap().remove(key);
        body
    };

    assert_eq!(rig.fail(without("frames")), "frames is required");
    for bad in [json!([]), json!("0"), json!(null), json!((0..257).collect::<Vec<u32>>())] {
        assert_eq!(rig.fail(with("frames", bad.clone())), "frames must be an array of 1 to 256 frame numbers", "{bad}");
    }
    for bad in [json!([-1, 3]), json!([0.5]), json!(["1"]), json!([0, 1000001])] {
        assert_eq!(rig.fail(with("frames", bad.clone())), "frames must be integers from 0 to 1000000", "{bad}");
    }
    for bad in [json!([3, 3]), json!([5, 2])] {
        assert_eq!(rig.fail(with("frames", bad.clone())), "frames must be strictly increasing", "{bad}");
    }
    assert_eq!(
        rig.fail(with("frames", json!([0, 1, 2, 3]))),
        "parameter has 3 keypoints on axis 0, 4 frames were requested"
    );
    assert_eq!(rig.fail(with("path", json!("walk.vpd"))), "path must name a .vmd file");
    assert_eq!(rig.fail(with("axis", json!(2))), "axis must be 0 or 1");
    assert_eq!(rig.fail(with("axis", json!(1))), "a 1D parameter has axis 0 and no other");
    assert_eq!(rig.fail(with("other", json!(0))), "a 1D parameter has axis 0 and no other");
    assert_eq!(rig.fail(with("animation", json!(" "))), "animation must be a non-empty string");
    assert_eq!(rig.fail(with("dryRun", json!("yes"))), "dryRun must be a boolean");
    assert_eq!(rig.fail(with("translationScale", json!(0))), "translationScale must be a positive number");
    assert_eq!(rig.fail(with("parameter", json!(9999))), "parameter 9999 was not found");
    assert_eq!(rig.fail(with("root", json!(1))), "Node is not a DepthRigRoot");
    // A 2D parameter: the other axis needs an existing keypoint.
    let mut body = rig.body(pose, &path, json!({"frames": [0, 10], "axis": 1, "other": 3}));
    assert_eq!(rig.fail(body.clone()), "other must identify an existing parameter keypoint index");
    body["other"] = json!(1);
    body["frames"] = json!([0, 1, 2, 3]);
    assert_eq!(rig.fail(body), "parameter has 3 keypoints on axis 1, 4 frames were requested");
    // A baked animation must fit the animation length limit.
    let mut long = base.clone();
    long["frames"] = json!([0, 1000000]);
    long["animation"] = json!("Long");
    let message = rig.fail(long);
    assert_eq!(message, "length must be between 1 and 1000000");
    // Files.
    let missing = dir.0.join("missing.vmd");
    let message = rig.fail(with("path", json!(missing.to_str().unwrap())));
    assert!(message.starts_with("could not open VMD file: "), "{message}");
    let truncated = write(&dir, "bad.vmd", b"Vocaloid Motion Data 0002");
    assert_eq!(rig.fail(with("path", json!(truncated))), "VMD is truncated in the header");
    let group = ok(&mut rig.session, "parameter.group.create", json!({"name": "Group"}));
    let id = group["groupUUID"].as_u64().expect("a group has a groupUUID");
    assert_eq!(rig.fail(with("parameter", json!(id))), format!("parameter {id} refers to a parameter group"));
}

#[test]
fn the_mcp_tool_maps_to_the_operation() {
    let (mut rig, dir) = (rig(), temp_dir());
    let path = walk(&dir);
    let answer = rig
        .session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "rig_bake_mmd_motion",
            "arguments": {"root": rig.root, "parameter": rig.line, "path": path, "frames": [0, 30], "dryRun": true}}}))
        .unwrap()["result"]
        .clone();
    assert_eq!(answer["isError"], false, "{answer}");
    assert_eq!(answer["structuredContent"]["keypointsWritten"], 2);
    let listed = rig.session.handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).unwrap();
    let tool = listed["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "rig_bake_mmd_motion").unwrap().clone();
    assert_eq!(tool["inputSchema"]["required"], json!(["root", "parameter", "path", "frames"]));
    let mut properties: Vec<&str> = tool["inputSchema"]["properties"].as_object().unwrap().keys().map(String::as_str).collect();
    properties.sort_unstable();
    assert_eq!(
        properties,
        [
            "animation", "axis", "boneTable", "dryRun", "frames", "morphs", "other", "parameter", "path", "root",
            "storeBoneTable", "translationScale"
        ]
    );
}

// ------------------------------------------------------------------------------- morphs and the bone table

const BLINK: &[u8] = &[0x82, 0xDC, 0x82, 0xCE, 0x82, 0xBD, 0x82, 0xAB]; // まばたき
const MOUTH_A: &[u8] = &[0x82, 0xA0]; // あ
const MOUTH_I: &[u8] = &[0x82, 0xA2]; // い

/// `vmd` with 23-byte morph keys (name, frame, weight) after the bone keys.
fn vmd_with_morphs(bones: &[Vec<u8>], morphs: &[(&[u8], u32, f32)]) -> Vec<u8> {
    let mut out = vmd(bones);
    out.truncate(out.len() - 4);
    out.extend((morphs.len() as u32).to_le_bytes());
    for (name, frame, weight) in morphs {
        out.extend(padded(name, 15));
        out.extend(frame.to_le_bytes());
        out.extend(weight.to_le_bytes());
    }
    out
}

/// The walk with a blink that closes over frames 0 to 10 and half reopens to 0.25 at frame 16, a mouth that opens
/// over frames 0 to 20, and a morph nothing maps. The blink keys are out of order and frame 10 is keyed twice (the
/// later key wins).
fn talk(dir: &TempDir) -> String {
    let bones = [vmd_bone(HEAD, 0, [0.0; 3], about(0, 0.0)), vmd_bone(HEAD, 30, [0.0; 3], about(0, 30.0))];
    let morphs: [(&[u8], u32, f32); 7] = [
        (BLINK, 10, 0.75),
        (BLINK, 10, 1.0),
        (BLINK, 0, 0.0),
        (BLINK, 16, 0.25),
        (MOUTH_A, 0, 0.0),
        (MOUTH_A, 20, 1.0),
        (MOUTH_I, 0, 1.0),
    ];
    write(dir, "talk.vmd", &vmd_with_morphs(&bones, &morphs))
}

fn create_mouth(rig: &mut Rig) -> u64 {
    let created = ok(&mut rig.session, "parameter.create", json!({"name": "Mouth", "dimensions": 1, "min": [0.0, 0.0], "max": [4.0, 1.0]}));
    created["uuid"].as_u64().unwrap()
}

#[test]
fn mapped_morphs_become_lanes_of_the_baked_animation_with_their_value_range() {
    let (mut rig, dir) = (rig(), temp_dir());
    let path = talk(&dir);
    let (line, pose) = (rig.line, rig.pose);
    let mouth = create_mouth(&mut rig);
    let morphs = json!({
        "まばたき": {"parameter": pose, "axis": 1, "range": [0.5, -1.0]},
        "あ": {"parameter": mouth},
        "笑い": {"parameter": pose},
    });
    let report = rig.bake(line, &path, json!({"frames": [0, 5, 20], "animation": "Talk", "morphs": morphs}));

    // A morph key has no curve: the weight is linear between keys and held after the last one. まばたき is 0, 0.5
    // (half way to the key at frame 10) and 0.25 (held from frame 16); on 0.5..-1 that is 0.5, -0.25 and 0.125.
    // あ is 0, 0.25 and 1 on the Mouth's own 0..4. 笑い is not in the file. Entries come in name order.
    assert_eq!(report["morphs"], json!([
        {"morph": "あ", "parameter": mouth, "axis": 0, "range": [0.0, 4.0], "inFile": true, "values": [0.0, 1.0, 4.0]},
        {"morph": "まばたき", "parameter": pose, "axis": 1, "range": [0.5, -1.0], "inFile": true, "values": [0.5, -0.25, 0.125]},
        {"morph": "笑い", "parameter": pose, "axis": 0, "range": [-1.0, 1.0], "inFile": false, "values": null},
    ]));
    // Only い is left unmapped.
    assert_eq!(report["unmappedMorphCount"], 1);

    let lanes = rig.animation("Talk")["lanes"].as_array().unwrap().clone();
    let summary: Vec<Value> = lanes
        .iter()
        .map(|lane| {
            let keys: Vec<Value> = lane["keyframes"].as_array().unwrap().iter().map(|k| json!([k["frame"], k["value"]])).collect();
            json!([lane["uuid"], lane["target"], lane["interpolation"], lane["merge_mode"], keys])
        })
        .collect();
    assert_eq!(summary[1], json!([mouth, 0, "Linear", "Forced", [[0, 0.0], [5, 1.0], [20, 4.0]]]));
    assert_eq!(summary[2], json!([pose, 1, "Linear", "Forced", [[0, 0.5], [5, -0.25], [20, 0.125]]]));
    assert_eq!((lanes.len(), &lanes[0]["uuid"]), (3, &json!(line)));

    // The existing animation path plays them: at animation frame 5, and half way between two lane keys.
    let at = |rig: &mut Rig, frame: f64| ok(&mut rig.session, "animation.sample", json!({"name": "Talk", "frame": frame}))["values"].clone();
    let sampled = at(&mut rig, 5.0);
    assert_eq!((sampled[pose.to_string()][1].as_f64().unwrap(), sampled[mouth.to_string()][0].as_f64().unwrap()), (-0.25, 1.0));
    assert_eq!(at(&mut rig, 2.5)[pose.to_string()][1].as_f64().unwrap(), 0.125);
}

#[test]
fn morphs_are_reported_the_same_by_a_dry_run_and_undone_with_the_bake() {
    let (mut rig, dir) = (rig(), temp_dir());
    let path = talk(&dir);
    let (line, pose) = (rig.line, rig.pose);
    let extra = json!({"frames": [0, 20], "animation": "Talk", "morphs": {"まばたき": {"parameter": pose}}});
    let before = rig.session.snapshot();
    let mut dry_extra = extra.clone();
    dry_extra["dryRun"] = json!(true);
    let dry = rig.bake(line, &path, dry_extra);
    assert_eq!(rig.session.snapshot(), before);
    assert_eq!(rig.bake(line, &path, extra), dry);
    settle(&mut rig);
    assert_eq!(rig.animation("Talk")["lanes"].as_array().unwrap().len(), 2);
    ok(&mut rig.session, "undo", json!({}));
    assert_eq!(rig.session.snapshot()["model"], before["model"]);
}

#[test]
fn refused_morph_mappings_name_the_entry_and_change_nothing() {
    let (mut rig, dir) = (rig(), temp_dir());
    let path = talk(&dir);
    let (line, pose) = (rig.line, rig.pose);
    let mouth = create_mouth(&mut rig);
    let group = ok(&mut rig.session, "parameter.group.create", json!({"name": "Group"}))["groupUUID"].as_u64().unwrap();
    let base = rig.body(line, &path, json!({"frames": [0, 20], "animation": "Talk"}));
    let mut refused = |morphs: Value| {
        let mut body = base.clone();
        body["morphs"] = morphs;
        rig.fail(body)
    };

    let shape = "morphs must be an object of at most 64 entries (MMD morph name to {parameter, axis, range})";
    let crowded: serde_json::Map<String, Value> = (0..65).map(|index| (format!("m{index}"), json!({"parameter": 1}))).collect();
    for bad in [json!([]), json!("あ"), Value::Object(crowded)] {
        assert_eq!(refused(bad), shape);
    }
    assert_eq!(
        refused(json!({"a\u{FFFD}": {"parameter": mouth}})),
        "morphs entry \"a\u{FFFD}\" is not a usable name (1 to 384 bytes, no control or U+FFFD characters)"
    );
    for bad in [json!(5), json!({}), json!({"parameter": "7"}), json!({"parameter": 0}), json!({"parameter": 1.5})] {
        assert_eq!(refused(json!({"あ": bad})), "morphs entry \"あ\" must be an object with an integer parameter id");
    }
    for bad in [json!(2), json!(-1), json!("0")] {
        assert_eq!(refused(json!({"あ": {"parameter": pose, "axis": bad}})), "morphs entry \"あ\": axis must be 0 or 1");
    }
    for bad in [json!([0.0]), json!([0.0, 1.0, 2.0]), json!([0.0, "1"]), json!([0.0, 1e39]), json!("0..1")] {
        assert_eq!(
            refused(json!({"あ": {"parameter": mouth, "range": bad}})),
            "morphs entry \"あ\": range must be two finite numbers"
        );
    }
    assert_eq!(refused(json!({"あ": {"parameter": 9999}})), "morphs entry \"あ\": parameter 9999 was not found or is a group");
    assert_eq!(
        refused(json!({"あ": {"parameter": group}})),
        format!("morphs entry \"あ\": parameter {group} was not found or is a group")
    );
    assert_eq!(
        refused(json!({"あ": {"parameter": mouth, "axis": 1}})),
        format!("morphs entry \"あ\": parameter {mouth} is 1D and has no axis 1")
    );
    assert_eq!(
        refused(json!({"あ": {"parameter": line}})),
        format!("morphs entry \"あ\" drives parameter {line}, which the motion is baked on")
    );
    assert_eq!(
        refused(json!({"あ": {"parameter": mouth}, "い": {"parameter": mouth}})),
        format!("morphs entry \"い\" drives axis 0 of parameter {mouth}, which morphs entry \"あ\" already drives")
    );
    // The other axis of a 2D bake parameter is refused too: a lane there would move it off keypoint `other`.
    let on_pose = json!({"frames": [0, 20], "axis": 1, "other": 2, "animation": "Talk", "morphs": {"あ": {"parameter": pose, "axis": 0}}});
    let body = rig.body(pose, &path, on_pose);
    assert_eq!(rig.fail(body), format!("morphs entry \"あ\" drives parameter {pose}, which the motion is baked on"));
    // Morph values have nowhere to go without the animation.
    let mut body = rig.body(line, &path, json!({"frames": [0, 20], "morphs": {"あ": {"parameter": mouth}}}));
    assert_eq!(rig.fail(body.clone()), "morphs needs animation: morph values are written as lanes of the baked animation");
    // An empty mapping is no mapping.
    body["morphs"] = json!({});
    assert_eq!(ok(&mut rig.session, "rig.bake_mmd_motion", body)["morphs"], json!([]));
}

#[test]
fn the_bake_takes_the_bone_table_and_stores_it_with_the_motion_in_one_undo_step() {
    let (mut rig, dir) = (rig(), temp_dir());
    let path = write(&dir, "kao.vmd", &vmd(&[
        vmd_bone(b"Kao", 0, [0.0; 3], about(0, 0.0)),
        vmd_bone(b"Kao", 30, [0.0; 3], about(0, 30.0)),
    ]));
    let line = rig.line;
    let plain = rig.bake(line, &path, json!({"frames": [0, 10, 30], "dryRun": true}));
    assert_eq!((&plain["boneTable"], &plain["unmappedBoneCount"], &plain["frames"][1]["namedBones"]), (&json!("default"), &json!(1), &json!(0)));

    let before = rig.session.snapshot()["model"].clone();
    let table = json!({"Kao": "Head", "頭": null});
    // A dry run stores nothing.
    rig.bake(line, &path, json!({"frames": [0], "boneTable": table, "storeBoneTable": true, "dryRun": true}));
    assert_eq!(rig.bake(line, &path, json!({"frames": [0], "dryRun": true}))["boneTable"], "default");
    let report = rig.bake(line, &path, json!({"frames": [0, 10, 30], "boneTable": table, "storeBoneTable": true}));
    settle(&mut rig);
    assert_eq!((&report["boneTable"], &report["unmappedBoneCount"], &report["frames"][1]["namedBones"]), (&json!("argument"), &json!(0), &json!(1)));
    for (index, degrees) in [0.0, 10.0, 30.0].into_iter().enumerate() {
        near(rig.pitch(line, [index, 0]), f64::to_radians(degrees), 1e-5);
    }
    // The stored table is the default of the next call, and goes with the bake on undo.
    assert_eq!(rig.bake(line, &path, json!({"frames": [0], "dryRun": true}))["boneTable"], "document");
    ok(&mut rig.session, "undo", json!({}));
    assert_eq!(rig.session.snapshot()["model"], before);
    assert_eq!(rig.bake(line, &path, json!({"frames": [0], "dryRun": true}))["boneTable"], "default");
    // A table that is not sound is refused here as in rig.apply_mmd_pose.
    let body = rig.body(line, &path, json!({"frames": [0], "boneTable": {"Kao": "Head"}}));
    assert_eq!(rig.fail(body), "boneTable entry \"Kao\" maps to Head, which \"頭\" already drives; map one of them to null");
}
