//! `rig.apply_mmd_pose` through `Session::handle_request` and the MCP tool. Every VPD and VMD stream is built here;
//! no third-party file is read. The world-rotation check of the hierarchy compensation runs in the crate
//! (`rust/src/mmd_motion/apply_tests.rs`), where the rig's forward path is reachable.

use serde_json::{json, Value};
use soul2d_core::mmd_motion::{MAX_BONE_TABLE_ENTRIES, MAX_MMD_NAME_BYTES};
use soul2d_core::Session;
use std::collections::BTreeMap;
use std::f64::consts::FRAC_PI_6;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

// Shift-JIS names, pinned as byte literals.
const CENTER: &[u8] = &[0x83, 0x5A, 0x83, 0x93, 0x83, 0x5E, 0x81, 0x5B]; // センター
const LOWER_BODY: &[u8] = &[0x89, 0xBA, 0x94, 0xBC, 0x90, 0x67]; // 下半身
const UPPER_BODY: &[u8] = &[0x8F, 0xE3, 0x94, 0xBC, 0x90, 0x67]; // 上半身
const HEAD: &[u8] = &[0x93, 0xAA]; // 頭
const LEFT_ARM: &[u8] = &[0x8D, 0xB6, 0x98, 0x72]; // 左腕
const RIGHT_ARM: &[u8] = &[0x89, 0x45, 0x98, 0x72]; // 右腕
const LEFT_IK: &[u8] = &[0x8D, 0xB6, 0x91, 0xAB, 0x82, 0x68, 0x82, 0x6A]; // 左足ＩＫ
const BLINK: &[u8] = &[0x82, 0xDC, 0x82, 0xCE, 0x82, 0xBD, 0x82, 0xAB]; // まばたき

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A scratch directory that is removed when the test ends.
struct TempDir(PathBuf);

impl std::ops::Deref for TempDir {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp_dir() -> TempDir {
    let dir = std::env::temp_dir().join(format!(
        "soul2d-mmd-pose-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

// ------------------------------------------------------------------------------------------------ file writers

/// (x, y, z, w) of a rotation of `degrees` about the MMD axis (`0` x, `1` y, `2` z).
fn about(axis: usize, degrees: f64) -> [f64; 4] {
    let half = degrees.to_radians() / 2.0;
    let mut q = [0.0, 0.0, 0.0, half.cos()];
    q[axis] = half.sin();
    q
}

fn vpd(bones: &[(&[u8], [f64; 3], [f64; 4])]) -> Vec<u8> {
    let mut out = format!("Vocaloid Pose Data file\r\n\r\nmodel.osm;\r\n{};\r\n\r\n", bones.len()).into_bytes();
    for (index, (name, p, q)) in bones.iter().enumerate() {
        out.extend(format!("Bone{index}{{").as_bytes());
        out.extend(*name);
        out.extend(format!("\r\n{:.9},{:.9},{:.9};\r\n{:.12},{:.12},{:.12},{:.12};\r\n}}\r\n\r\n", p[0], p[1], p[2], q[0], q[1], q[2], q[3]).as_bytes());
    }
    out
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

fn vmd(bones: &[Vec<u8>], morph_names: &[&[u8]]) -> Vec<u8> {
    let mut out = padded(b"Vocaloid Motion Data 0002", 30);
    out.extend(padded(b"model", 20));
    out.extend((bones.len() as u32).to_le_bytes());
    bones.iter().for_each(|bone| out.extend(bone));
    out.extend((morph_names.len() as u32).to_le_bytes());
    for name in morph_names {
        out.extend(padded(name, 15));
        out.extend(0u32.to_le_bytes());
        out.extend(1.0f32.to_le_bytes());
    }
    out
}

fn write(dir: &PathBuf, name: &str, bytes: &[u8]) -> String {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    path.to_str().unwrap().to_owned()
}

// -------------------------------------------------------------------------------------------------------- rig

struct Rig {
    session: Session,
    root: u64,
    parameter: u64,
    bones: BTreeMap<String, u64>,
    grid: u64,
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

/// A document with the standard skeleton, a 2D parameter "Pose" (default keypoint `[1, 1]`) and a Grid skinned to
/// the Head, Chest and Pelvis bones.
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
    assert_eq!(bones.len(), 19);
    let parameter = ok(&mut session, "parameter.create", json!({"name": "Pose", "dimensions": 2}))["uuid"]
        .as_u64()
        .unwrap();
    let grid = ok(&mut session, "node.create", json!({"className": "GridDeformer", "name": "Body"}))["created"][0]
        ["uuid"]
        .as_u64()
        .unwrap();
    ok(&mut session, "deformer.define_grid", json!({
        "nodes": [grid], "axisX": [-60.0, -20.0, 20.0, 60.0], "axisY": [-270.0, -230.0, -190.0, -150.0, -100.0, 40.0],
    }));
    for bone in ["Head", "Chest", "Pelvis"] {
        ok(&mut session, "rig.add_source", json!({"root": root, "target": grid, "bone": bones[bone]}));
    }
    Rig { session, root, parameter, bones, grid }
}

impl Rig {
    fn apply(&mut self, path: &str, extra: Value) -> Value {
        let mut body = json!({"root": self.root, "parameter": self.parameter, "path": path});
        for (key, value) in extra.as_object().into_iter().flatten() {
            body[key] = value.clone();
        }
        ok(&mut self.session, "rig.apply_mmd_pose", body)
    }

    fn fail(&mut self, body: Value) -> String {
        let before = self.session.snapshot();
        let reply = run(&mut self.session, "rig.apply_mmd_pose", body);
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

    fn pose(&mut self, bone: &str, key: [usize; 2]) -> [f64; 3] {
        let parameter = self.parameter;
        ["transform.r.x", "transform.r.y", "transform.r.z"].map(|channel| self.cell(parameter, bone, channel, key).unwrap_or(0.0))
    }

    fn deform(&mut self, key: [usize; 2]) -> Option<Value> {
        let parameter = ok(&mut self.session, "parameter.get", json!({"id": self.parameter}));
        let binding = parameter["bindings"]
            .as_array()?
            .iter()
            .find(|binding| binding["node"] == self.grid && binding["param_name"] == "deform")?
            .clone();
        Some(binding["values"][key[0]][key[1]].clone())
    }
}

fn assert_near(actual: [f64; 3], expected: [f64; 3], tolerance: f64) {
    for axis in 0..3 {
        assert!((actual[axis] - expected[axis]).abs() <= tolerance, "{actual:?} vs {expected:?}");
    }
}

fn settle(rig: &mut Rig) {
    while rig.session.snapshot()["deformPending"] == true {
        ok(&mut rig.session, "deform.continue", json!({}));
    }
}

// ------------------------------------------------------------------------------------------------------ tests

#[test]
fn a_head_turn_writes_the_literal_bindings_and_bakes_the_deform() {
    let mut rig = rig();
    let dir = temp_dir();
    // 30 degrees about the MMD x axis. An upright rig bone sits a half turn about z from the world axes, so the
    // pose is pitch +30 degrees, no yaw, no roll (doc/mmd-motion.md, "Coordinates").
    let path = write(&dir, "head.vpd", &vpd(&[(HEAD, [0.0; 3], about(0, 30.0))]));
    assert!(rig.deform([1, 1]).is_none());
    let report = rig.apply(&path, json!({}));
    settle(&mut rig);

    assert_eq!(report["key"], json!([1, 1]));
    assert_eq!(report["format"], "vpd");
    assert_eq!(report["frame"], Value::Null);
    assert_eq!(report["translationApplied"], false);
    assert_eq!(report["mapped"].as_array().unwrap().len(), 1);
    assert_eq!(report["mapped"][0]["rig"], "Head");
    assert_eq!(report["mapped"][0]["mmd"], json!(["頭"]));
    assert_near(rig.pose("Head", [1, 1]), [FRAC_PI_6, 0.0, 0.0], 1e-9);
    for other in ["Neck", "Spine", "Pelvis", "UpperArm.L"] {
        assert_eq!(rig.pose(other, [1, 1]), [0.0; 3], "{other}");
    }
    let baked = rig.deform([1, 1]).expect("a deform binding is written at the key");
    assert!(baked.to_string().contains('.'), "{baked}");
    assert!(baked.as_array().unwrap().iter().flat_map(|row| row.as_array().unwrap()).any(|v| v.as_f64().unwrap().abs() > 1e-6));
}

#[test]
fn rotations_about_each_mmd_axis_and_the_arm_sides() {
    let mut rig = rig();
    let dir = temp_dir();
    // Upright Head: x -> pitch +30, y -> yaw -30, z -> roll -30 degrees.
    for (axis, expected) in [(0, [FRAC_PI_6, 0.0, 0.0]), (1, [0.0, -FRAC_PI_6, 0.0]), (2, [0.0, 0.0, -FRAC_PI_6])] {
        let path = write(&dir, "axis.vpd", &vpd(&[(HEAD, [0.0; 3], about(axis, 30.0))]));
        rig.apply(&path, json!({}));
        assert_near(rig.pose("Head", [1, 1]), expected, 1e-9);
    }
    // MMD 左 is screen right, which the skeleton calls .R; a turn about z reads the same on any bone in the xy plane.
    let path = write(&dir, "arms.vpd", &vpd(&[(LEFT_ARM, [0.0; 3], about(2, 30.0)), (RIGHT_ARM, [0.0; 3], about(2, -10.0))]));
    let report = rig.apply(&path, json!({}));
    let rigs: Vec<&str> = report["mapped"].as_array().unwrap().iter().map(|m| m["rig"].as_str().unwrap()).collect();
    assert_eq!(rigs, ["UpperArm.R", "UpperArm.L"], "table order: 左腕 (.R) then 右腕 (.L)");
    assert_near(rig.pose("UpperArm.R", [1, 1]), [0.0, 0.0, -FRAC_PI_6], 1e-9);
    assert_near(rig.pose("UpperArm.L", [1, 1]), [0.0, 0.0, 10.0f64.to_radians()], 1e-9);
}

#[test]
fn centre_translation_uses_the_rig_scale_and_the_override() {
    let mut rig = rig();
    let dir = temp_dir();
    let path = write(&dir, "centre.vpd", &vpd(&[(CENTER, [1.0, 2.0, 3.0], about(0, 0.0))]));
    // Pelvis head y = 48, Head head y = -204: the span is 252 rig units, 8 nominal MMD units, so 31.5 per unit.
    let report = rig.apply(&path, json!({}));
    assert_eq!(report["translationApplied"], true);
    assert_eq!(report["translationScale"], 31.5);
    let parameter = rig.parameter;
    let t = ["transform.t.x", "transform.t.y", "transform.t.z"].map(|c| rig.cell(parameter, "Pelvis", c, [1, 1]).unwrap());
    assert_near(t, [31.5, -63.0, -94.5], 1e-9);
    assert_eq!(report["mapped"].as_array().unwrap().last().unwrap()["channel"], "translation");

    let overridden = rig.apply(&path, json!({"translationScale": 2.0, "key": [2, 1]}));
    assert_eq!(overridden["translationScale"], 2.0);
    let t = ["transform.t.x", "transform.t.y", "transform.t.z"].map(|c| rig.cell(parameter, "Pelvis", c, [2, 1]).unwrap());
    assert_near(t, [2.0, -4.0, -6.0], 1e-9);
    // Only the centre was named: no rotation was written.
    assert_eq!(rig.pose("Pelvis", [1, 1]), [0.0; 3]);
}

#[test]
fn the_lower_body_turn_is_undone_on_the_spine_and_the_report_lists_the_sources() {
    let mut rig = rig();
    let dir = temp_dir();
    let path = write(&dir, "body.vpd", &vpd(&[(LOWER_BODY, [0.0; 3], about(1, 20.0))]));
    let report = rig.apply(&path, json!({}));
    // Pelvis takes the turn; Spine is written with the inverse, so the torso stays where MMD leaves it.
    assert_near(rig.pose("Pelvis", [1, 1]), [0.0, -20.0f64.to_radians(), 0.0], 1e-9);
    assert_near(rig.pose("Spine", [1, 1]), [0.0, 20.0f64.to_radians(), 0.0], 1e-9);
    let spine = report["mapped"].as_array().unwrap().iter().find(|m| m["rig"] == "Spine").unwrap();
    assert_eq!(spine["mmd"], json!(["下半身"]));
    // The feet ignore their parent (lockToRoot), so they take the whole MMD chain (センター, 下半身, 足, ひざ, 足首).
    // The head hangs under 上半身 in MMD and sees no lower-body turn there, so it is not written.
    let rigs: Vec<&str> = report["mapped"].as_array().unwrap().iter().map(|m| m["rig"].as_str().unwrap()).collect();
    assert_eq!(rigs, ["Pelvis", "Spine", "Foot.R", "Foot.L"]);
}

/// Applying `second` after `first` ends exactly where applying `second` to a fresh keypoint does: same model, same
/// report, and the dry run of `second` reports the same on both.
fn assert_replaces(first: &[u8], second: &[u8]) -> Rig {
    let dir = temp_dir();
    let (first, second) = (write(&dir, "first.vpd", first), write(&dir, "second.vpd", second));
    let mut used = rig();
    used.apply(&first, json!({}));
    settle(&mut used);
    let dry = used.apply(&second, json!({"dryRun": true}));
    let report = used.apply(&second, json!({}));
    settle(&mut used);
    let mut fresh = rig();
    fresh.apply(&second, json!({}));
    settle(&mut fresh);
    assert_eq!(dry, report);
    // A fresh parameter gets no binding for a bone the pose leaves alone, so the documents differ; the pose does not.
    for bone in used.bones.keys().cloned().collect::<Vec<_>>() {
        assert_eq!(used.pose(&bone, [1, 1]), fresh.pose(&bone, [1, 1]), "{bone}");
    }
    assert_eq!(used.deform([1, 1]), fresh.deform([1, 1]));
    used
}

#[test]
fn a_lower_body_back_at_zero_clears_the_earlier_spine_compensation() {
    let turn = vpd(&[(LOWER_BODY, [0.0; 3], about(1, 20.0))]);
    let mut used = assert_replaces(&turn, &vpd(&[(LOWER_BODY, [0.0; 3], about(1, 0.0)), (HEAD, [0.0; 3], about(0, 10.0))]));
    assert_eq!(used.pose("Pelvis", [1, 1]), [0.0; 3]);
    assert_eq!(used.pose("Spine", [1, 1]), [0.0; 3]);
    // Not named at all: the same result, and the report says the Pelvis and Spine were reset.
    let mut used = assert_replaces(&turn, &vpd(&[(HEAD, [0.0; 3], about(0, 10.0))]));
    assert_eq!(used.pose("Spine", [1, 1]), [0.0; 3]);
    let dir = temp_dir();
    let report = used.apply(&write(&dir, "head.vpd", &vpd(&[(HEAD, [0.0; 3], about(0, 10.0))])), json!({"dryRun": true}));
    let reset: Vec<(&str, &str)> =
        report["reset"].as_array().unwrap().iter().map(|r| (r["rig"].as_str().unwrap(), r["channel"].as_str().unwrap())).collect();
    // No pose moved the Pelvis, so there is no translation to reset and none is created.
    assert!(reset.contains(&("Pelvis", "rotation")) && !reset.contains(&("Pelvis", "translation")), "{reset:?}");
    let moved = vpd(&[(CENTER, [1.0, 0.0, 0.0], about(0, 0.0))]);
    used.apply(&write(&dir, "moved.vpd", &moved), json!({}));
    let report = used.apply(&write(&dir, "head2.vpd", &vpd(&[(HEAD, [0.0; 3], about(0, 10.0))])), json!({}));
    let reset: Vec<(&str, &str)> =
        report["reset"].as_array().unwrap().iter().map(|r| (r["rig"].as_str().unwrap(), r["channel"].as_str().unwrap())).collect();
    assert!(reset.contains(&("Pelvis", "translation")), "a translation an earlier pose wrote is reset: {reset:?}");
    assert!(reset.contains(&("Spine", "rotation")), "{reset:?}");
    assert_eq!(report["mapped"].as_array().unwrap().iter().map(|m| m["rig"].as_str().unwrap()).collect::<Vec<_>>(), ["Head"]);
    assert_eq!(report["mapped"][0]["namedInFile"], true);
    assert_eq!(report["translationApplied"], false);
}

#[test]
fn an_upper_body_only_pose_computes_against_a_neutral_pelvis() {
    let turn = vpd(&[(LOWER_BODY, [0.0; 3], about(1, 20.0))]);
    let mut used = assert_replaces(&turn, &vpd(&[(UPPER_BODY, [0.0; 3], about(0, 5.0))]));
    assert_eq!(used.pose("Pelvis", [1, 1]), [0.0; 3]);
    assert_near(used.pose("Spine", [1, 1]), [5.0f64.to_radians(), 0.0, 0.0], 1e-9);
}

#[test]
fn the_report_states_the_angles_that_are_stored() {
    let mut rig = rig();
    let dir = temp_dir();
    // 1e-5 degrees is 1.7e-7 rad, below the 1e-6 tolerance the standard parameters use for "unchanged".
    let first = write(&dir, "first.vpd", &vpd(&[(HEAD, [0.0; 3], about(0, 30.0))]));
    let second = write(&dir, "second.vpd", &vpd(&[(HEAD, [0.0; 3], about(0, 30.00001))]));
    let before = rig.apply(&first, json!({}))["mapped"][0]["pitch"].as_f64().unwrap();
    let reported = rig.apply(&second, json!({}))["mapped"][0]["pitch"].as_f64().unwrap();
    assert_ne!(before, reported);
    assert_eq!(rig.pose("Head", [1, 1])[0], reported);
}

#[test]
fn a_vmd_is_sampled_between_two_keys_and_unmapped_names_are_reported() {
    let mut rig = rig();
    let dir = temp_dir();
    let file = vmd(
        &[
            vmd_bone(HEAD, 0, [0.0; 3], about(0, 0.0)),
            vmd_bone(HEAD, 10, [0.0; 3], about(0, 30.0)),
            vmd_bone(LEFT_IK, 3, [0.0; 3], about(0, 0.0)),
            vmd_bone(&[0x88, 0x9F], 3, [0.0; 3], about(0, 0.0)), // 亜: decodes, but is no standard bone
        ],
        &[BLINK, &[0x88, 0x9F]],
    );
    let path = write(&dir, "motion.vmd", &file);
    let report = rig.apply(&path, json!({"frame": 5}));
    assert_eq!(report["frame"], 5.0);
    assert_near(rig.pose("Head", [1, 1]), [15.0f64.to_radians(), 0.0, 0.0], 1e-5);
    assert_eq!(report["unmappedBoneCount"], 2);
    assert_eq!(report["unmappedBones"][0], json!({"text": "左足ＩＫ", "hex": "8db691ab826882 6a".replace(' ', ""), "standard": true}));
    assert_eq!(report["unmappedBones"][1], json!({"text": "亜", "hex": "889f", "standard": false}));
    assert_eq!(report["unmappedMorphCount"], 2);
    assert_eq!(report["unmappedMorphs"][0]["text"], "まばたき");
    assert_eq!(report["unmappedMorphs"][0]["standard"], true);
    assert_eq!(report["zeroQuaternions"], 0);

    // Held before the first key and after the last.
    rig.apply(&path, json!({"frame": 0}));
    assert_near(rig.pose("Head", [1, 1]), [0.0; 3], 1e-9);
    rig.apply(&path, json!({"frame": 99.5}));
    assert_near(rig.pose("Head", [1, 1]), [FRAC_PI_6, 0.0, 0.0], 1e-6);
}

#[test]
fn a_bone_with_a_rotation_lock_is_reported_and_left_alone() {
    let mut rig = rig();
    let dir = temp_dir();
    let spine = rig.bones["Spine"];
    ok(&mut rig.session, "rig.set_constraint", json!({"bone": spine, "constraint": {"lockRotation": true}}));
    let path = write(&dir, "body.vpd", &vpd(&[(LOWER_BODY, [0.0; 3], about(1, 20.0)), (UPPER_BODY, [0.0; 3], about(0, 5.0))]));
    let report = rig.apply(&path, json!({}));
    assert_eq!(report["locked"], json!(["Spine"]));
    assert!(rig.cell(rig.parameter, "Spine", "transform.r.y", [1, 1]).is_none());
    assert_near(rig.pose("Pelvis", [1, 1]), [0.0, -20.0f64.to_radians(), 0.0], 1e-9);
    // Not named, but the lock suppressed the turn that would undo the Pelvis: still reported.
    let path = write(&dir, "lower.vpd", &vpd(&[(LOWER_BODY, [0.0; 3], about(1, 20.0))]));
    assert_eq!(rig.apply(&path, json!({"dryRun": true}))["locked"], json!(["Spine"]));
    let path = write(&dir, "head.vpd", &vpd(&[(HEAD, [0.0; 3], about(0, 10.0))]));
    assert_eq!(rig.apply(&path, json!({"dryRun": true}))["locked"], json!([]));
}

#[test]
fn zero_quaternions_are_counted_in_the_report() {
    let mut rig = rig();
    let dir = temp_dir();
    let path = write(&dir, "zero.vpd", &vpd(&[(HEAD, [0.0; 3], [0.0; 4])]));
    let report = rig.apply(&path, json!({}));
    assert_eq!(report["zeroQuaternions"], 1);
    assert_eq!(rig.pose("Head", [1, 1]), [0.0; 3]);
}

#[test]
fn dry_run_changes_nothing_and_returns_the_report_of_the_real_run() {
    let mut rig = rig();
    let dir = temp_dir();
    let path = write(&dir, "pose.vpd", &vpd(&[
        (CENTER, [1.0, 0.0, 0.0], about(1, 5.0)),
        (UPPER_BODY, [0.0; 3], about(0, 12.0)),
        (HEAD, [0.0; 3], about(2, 30.0)),
    ]));
    let before = rig.session.snapshot();
    let history = ok(&mut rig.session, "history.summary", json!({}));
    let dry = rig.apply(&path, json!({"dryRun": true}));
    assert_eq!(rig.session.snapshot(), before, "revision, model and modified flag are unchanged");
    assert_eq!(ok(&mut rig.session, "history.summary", json!({})), history);
    let wet = rig.apply(&path, json!({}));
    assert_eq!(dry, wet);
    assert_ne!(rig.session.snapshot()["revision"], before["revision"]);
}

#[test]
fn one_undo_restores_the_previous_pose() {
    let mut rig = rig();
    let dir = temp_dir();
    let first = write(&dir, "first.vpd", &vpd(&[(HEAD, [0.0; 3], about(0, 10.0))]));
    let second = write(&dir, "second.vpd", &vpd(&[
        (HEAD, [0.0; 3], about(0, 30.0)),
        (LOWER_BODY, [0.0; 3], about(1, 15.0)),
        (CENTER, [1.0, 1.0, 1.0], about(0, 0.0)),
    ]));
    rig.apply(&first, json!({}));
    settle(&mut rig);
    let after_first = rig.session.snapshot();
    rig.apply(&second, json!({}));
    settle(&mut rig);
    assert_ne!(rig.session.snapshot()["model"], after_first["model"]);
    ok(&mut rig.session, "undo", json!({}));
    assert_eq!(rig.session.snapshot()["model"], after_first["model"]);
    assert_near(rig.pose("Head", [1, 1]), [10.0f64.to_radians(), 0.0, 0.0], 1e-9);
    ok(&mut rig.session, "redo", json!({}));
    assert_near(rig.pose("Head", [1, 1]), [FRAC_PI_6, 0.0, 0.0], 1e-9);
}

#[test]
fn the_standard_parameters_take_a_pose_and_the_default_key_follows_the_value() {
    let mut rig = rig();
    let dir = temp_dir();
    ok(&mut rig.session, "rig.add_standard_parameters", json!({"root": rig.root}));
    let parameters = rig.session.resource_parameters();
    let face = parameters.as_array().unwrap().iter().find(|p| p["name"] == "Face::Yaw-Pitch").unwrap()["uuid"]
        .as_u64()
        .unwrap();
    ok(&mut rig.session, "value.set", json!({"id": face, "value": [1.0, 0.0]}));
    let path = write(&dir, "head.vpd", &vpd(&[(HEAD, [0.0; 3], about(2, 30.0))]));
    let mut body = json!({"root": rig.root, "parameter": face, "path": path});
    let report = ok(&mut rig.session, "rig.apply_mmd_pose", body.clone());
    assert_eq!(report["key"], json!([4, 2]), "the keypoint closest to the current value");
    // Head roll had no binding in the standard parameters; it is created. Yaw and pitch at this key are rewritten.
    assert_near(
        ["transform.r.x", "transform.r.y", "transform.r.z"].map(|c| rig.cell(face, "Head", c, [4, 2]).unwrap()),
        [0.0, 0.0, -FRAC_PI_6],
        1e-9,
    );
    body["key"] = json!([0, 0]);
    assert_eq!(ok(&mut rig.session, "rig.apply_mmd_pose", body)["key"], json!([0, 0]));
}

#[test]
fn the_mcp_tool_maps_to_the_operation() {
    let mut rig = rig();
    let dir = temp_dir();
    let path = write(&dir, "head.vpd", &vpd(&[(HEAD, [0.0; 3], about(0, 30.0))]));
    let answer = rig
        .session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "rig_apply_mmd_pose",
            "arguments": {"root": rig.root, "parameter": rig.parameter, "path": path, "dryRun": true}}}))
        .unwrap()["result"]
        .clone();
    assert_eq!(answer["isError"], false, "{answer}");
    assert_eq!(answer["structuredContent"]["mapped"][0]["rig"], "Head");
    let listed = rig.session.handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).unwrap();
    let tool = listed["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "rig_apply_mmd_pose").unwrap().clone();
    assert_eq!(tool["inputSchema"]["required"], json!(["root", "parameter", "path"]));
    let mut properties: Vec<&str> = tool["inputSchema"]["properties"].as_object().unwrap().keys().map(String::as_str).collect();
    properties.sort_unstable();
    assert_eq!(
        properties,
        ["boneTable", "dryRun", "frame", "key", "parameter", "path", "root", "storeBoneTable", "translationScale"]
    );
}

// ------------------------------------------------------------------------------------------- bone table override

const UPPER_BODY3: &[u8] = &[0x8F, 0xE3, 0x94, 0xBC, 0x90, 0x67, 0x33]; // 上半身3, not a standard name
const TABLE_KEY: &str = "io.github.geekjapan.soul2d.MmdBoneTable";

impl Rig {
    fn poses(&mut self, key: [usize; 2]) -> BTreeMap<String, [f64; 3]> {
        let names: Vec<String> = self.bones.keys().cloned().collect();
        names.into_iter().map(|name| (name.clone(), self.pose(&name, key))).collect()
    }

    /// The stored bone table, `None` when the document holds none.
    fn stored_table(&mut self) -> Option<Value> {
        let entry = ok(&mut self.session, "extdata.get", json!({"key": TABLE_KEY}));
        let payload = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, entry["base64"].as_str()?).unwrap();
        Some(serde_json::from_slice(&payload).unwrap())
    }
}

#[test]
fn an_override_redirects_one_name_and_changes_only_that_bone() {
    let mut rig = rig();
    let dir = temp_dir();
    let path = write(&dir, "pose.vpd", &vpd(&[(&b"Ude"[..], [0.0; 3], about(2, 30.0)), (HEAD, [0.0; 3], about(0, 10.0))]));
    // Left alone, "Ude" is an unknown name: reported, and no arm is written.
    let plain = rig.apply(&path, json!({}));
    assert_eq!(plain["boneTable"], "default");
    assert_eq!(plain["unmappedBones"], json!([{"text": "Ude", "hex": "556465", "standard": false}]));
    let without = rig.poses([1, 1]);
    assert_eq!(without["UpperArm.R"], [0.0; 3]);
    assert_near(without["Head"], [10.0f64.to_radians(), 0.0, 0.0], 1e-9);

    // Read as 左腕, the standard counterpart of UpperArm.R: a turn about z is roll -30 degrees there.
    let report = rig.apply(&path, json!({"key": [2, 1], "boneTable": {"Ude": "UpperArm.R", "左腕": null}}));
    assert_eq!(report["boneTable"], "argument");
    assert_eq!(report["unmappedBoneCount"], 0);
    let arm = report["mapped"].as_array().unwrap().iter().find(|m| m["rig"] == "UpperArm.R").unwrap();
    assert_eq!(arm["mmd"], json!(["Ude"]), "the report spells the name as the file does");
    let with = rig.poses([2, 1]);
    assert_near(with["UpperArm.R"], [0.0, 0.0, -FRAC_PI_6], 1e-9);
    for (bone, pose) in &with {
        if bone != "UpperArm.R" {
            assert_eq!(pose, &without[bone], "{bone}");
        }
    }
    // An entry for a name the file does not hold, and a standard name mapped to its own bone, change nothing.
    let extra = json!({"Ude": "UpperArm.R", "左腕": null, "Nai": "Neck", "首": null, "頭": "Head"});
    rig.apply(&path, json!({"key": [2, 2], "boneTable": extra}));
    assert_eq!(rig.poses([2, 2]), with);
}

#[test]
fn a_key_with_kanji_outside_the_standard_vocabulary_matches_a_name_given_as_shift_jis() {
    let mut rig = rig();
    let dir = temp_dir();
    // 日本 (0x93FA 0x967B) is no part of the standard MMD names. Twenty-two of them make a 44 byte Shift-JIS name
    // and a 132 byte key, which a VPD file may hold and the key limit has to accept.
    let short: &[u8] = &[0x93, 0xFA, 0x96, 0x7B];
    let long = short.repeat(22);
    let path = write(&dir, "pose.vpd", &vpd(&[(short, [0.0; 3], about(2, 30.0)), (long.as_slice(), [0.0; 3], about(0, 10.0))]));
    let plain = rig.apply(&path, json!({}));
    assert_eq!(plain["unmappedBones"][0], json!({"text": "日本", "hex": "93fa967b", "standard": false}));
    let table = json!({"日本": "UpperArm.R", "左腕": null, "頭": null, "日本".repeat(22): "Head"});
    let report = rig.apply(&path, json!({"key": [2, 1], "boneTable": table}));
    assert_eq!(report["unmappedBoneCount"], 0);
    let arm = report["mapped"].as_array().unwrap().iter().find(|m| m["rig"] == "UpperArm.R").unwrap();
    assert_eq!(arm["mmd"], json!(["日本"]));
    let poses = rig.poses([2, 1]);
    assert_near(poses["UpperArm.R"], [0.0, 0.0, -FRAC_PI_6], 1e-9);
    assert_near(poses["Head"], [10.0f64.to_radians(), 0.0, 0.0], 1e-9);
}

#[test]
fn an_overridden_name_takes_the_place_of_the_standard_name_in_the_hierarchy_compensation() {
    let mut rig = rig();
    let dir = temp_dir();
    let path = write(&dir, "pose.vpd", &vpd(&[(UPPER_BODY3, [0.0; 3], about(0, 20.0))]));
    let report = rig.apply(&path, json!({"boneTable": {"上半身3": "Chest", "上半身2": null}}));
    let turn = [20.0f64.to_radians(), 0.0, 0.0];
    assert_near(rig.pose("Chest", [1, 1]), turn, 1e-9);
    // The Neck follows the Chest in the rig and needs nothing. The Head ignores its rig parent, so it is written
    // with the whole MMD chain 上半身, 上半身2, 首, 頭, in which the overridden name now stands for 上半身2.
    assert_eq!(rig.pose("Neck", [1, 1]), [0.0; 3]);
    assert_near(rig.pose("Head", [1, 1]), turn, 1e-9);
    let rigs: Vec<&str> = report["mapped"].as_array().unwrap().iter().map(|m| m["rig"].as_str().unwrap()).collect();
    assert_eq!(rigs, ["Chest", "Head"]);
    assert_eq!(report["mapped"][1]["mmd"], json!(["上半身3"]));
    assert_eq!(report["mapped"][1]["namedInFile"], false);
}

#[test]
fn null_takes_a_bone_out_of_the_pose() {
    let mut rig = rig();
    let dir = temp_dir();
    let path = write(&dir, "pose.vpd", &vpd(&[
        (HEAD, [0.0; 3], about(0, 30.0)),
        (LEFT_ARM, [0.0; 3], about(2, 30.0)),
        (CENTER, [1.0, 0.0, 0.0], about(0, 0.0)),
    ]));
    let report = rig.apply(&path, json!({"boneTable": {"頭": null, "センター": null}}));
    let parameter = rig.parameter;
    assert_eq!(rig.cell(parameter, "Head", "transform.r.x", [1, 1]), None);
    assert_eq!(rig.cell(parameter, "Pelvis", "transform.t.x", [1, 1]), None);
    assert_near(rig.pose("UpperArm.R", [1, 1]), [0.0, 0.0, -FRAC_PI_6], 1e-9);
    assert_eq!(report["translationApplied"], false);
    assert_eq!(report["mapped"].as_array().unwrap().len(), 1);
    assert_eq!(report["unmappedBones"], json!([
        {"text": "頭", "hex": "93aa", "standard": true},
        {"text": "センター", "hex": "835a8393835e815b", "standard": true},
    ]));
}

#[test]
fn a_bone_table_that_is_not_sound_is_refused_by_entry() {
    let mut rig = rig();
    let dir = temp_dir();
    let pose = write(&dir, "pose.vpd", &vpd(&[(HEAD, [0.0; 3], about(0, 30.0))]));
    let base = json!({"root": rig.root, "parameter": rig.parameter, "path": pose});
    let mut refused = |table: Value| {
        let mut body = base.clone();
        body["boneTable"] = table;
        rig.fail(body)
    };

    let shape = "boneTable must be an object of at most 64 entries (MMD bone name to rig bone id or null)";
    let crowded: serde_json::Map<String, Value> = (0..65).map(|index| (format!("bone{index}"), Value::Null)).collect();
    for bad in [json!([]), json!("頭"), Value::Object(crowded)] {
        assert_eq!(refused(bad), shape);
    }
    let long = "x".repeat(385);
    for (name, label) in [("", ""), ("a\u{FFFD}", "a\u{FFFD}"), ("tab\there", "tab?here"), (long.as_str(), &long[..16])] {
        assert_eq!(
            refused(json!({name: null})),
            format!("boneTable entry \"{label}\" is not a usable name (1 to 384 bytes, no control or U+FFFD characters)")
        );
    }
    assert_eq!(refused(json!({"Kao": 5})), "boneTable entry \"Kao\" must be a rig bone id (a string) or null");
    assert_eq!(refused(json!({"Kao": "Tail"})), "boneTable entry \"Kao\" names an unknown rig bone \"Tail\"");
    // A standard name has a known place in the MMD hierarchy (or no rig counterpart at all); it is not redirected.
    for standard in ["頭", "センター", "腰", "左足ＩＫ"] {
        assert_eq!(
            refused(json!({standard: "Neck"})),
            format!(
                "boneTable entry \"{standard}\" redirects a standard MMD bone, whose place in the hierarchy is fixed; \
                 a standard name can only be mapped to null"
            )
        );
    }
    // One source per rig bone: against the standard name, and against another entry.
    assert_eq!(
        refused(json!({"Kao": "Head"})),
        "boneTable entry \"Kao\" maps to Head, which \"頭\" already drives; map one of them to null"
    );
    assert_eq!(
        refused(json!({"Atama": "Head", "Kao": "Head", "頭": null})),
        "boneTable entry \"Kao\" maps to Head, which \"Atama\" already drives; map one of them to null"
    );

    let mut body = base.clone();
    body["storeBoneTable"] = json!(true);
    assert_eq!(rig.fail(body.clone()), "storeBoneTable needs boneTable");
    body["storeBoneTable"] = json!("yes");
    assert_eq!(rig.fail(body), "storeBoneTable must be a boolean");
}

#[test]
fn a_stored_table_is_the_default_of_later_calls_survives_save_and_reopen_and_is_undone() {
    let mut rig = rig();
    let dir = temp_dir();
    let path = write(&dir, "pose.vpd", &vpd(&[(&b"Kao"[..], [0.0; 3], about(0, 30.0))]));
    let table = json!({"Kao": "Head", "頭": null});
    let before = rig.session.snapshot();

    // A dry run stores nothing.
    rig.apply(&path, json!({"boneTable": table, "storeBoneTable": true, "dryRun": true}));
    assert_eq!(rig.stored_table(), None);
    assert_eq!(rig.session.snapshot(), before);

    rig.apply(&path, json!({"boneTable": table, "storeBoneTable": true}));
    settle(&mut rig);
    assert_eq!(rig.stored_table(), Some(table.clone()));
    assert_near(rig.pose("Head", [1, 1]), [FRAC_PI_6, 0.0, 0.0], 1e-9);
    let stored = rig.session.snapshot();

    // The pose and the stored table are one undo step.
    ok(&mut rig.session, "undo", json!({}));
    assert_eq!(rig.session.snapshot()["model"], before["model"]);
    assert_eq!(rig.stored_table(), None);
    assert_eq!(rig.apply(&path, json!({"dryRun": true}))["boneTable"], "default");
    ok(&mut rig.session, "redo", json!({}));
    assert_eq!(rig.session.snapshot()["model"], stored["model"]);
    assert_eq!(rig.stored_table(), Some(table.clone()));

    // Later calls use it; an explicit table, the empty one included, takes precedence and is not stored.
    let later = rig.apply(&path, json!({"key": [2, 1]}));
    assert_eq!(later["boneTable"], "document");
    assert_near(rig.pose("Head", [2, 1]), [FRAC_PI_6, 0.0, 0.0], 1e-9);
    let explicit = rig.apply(&path, json!({"key": [2, 2], "boneTable": {}}));
    assert_eq!((&explicit["boneTable"], &explicit["unmappedBoneCount"]), (&json!("argument"), &json!(1)));
    assert_eq!(rig.pose("Head", [2, 2]), [0.0; 3]);
    assert_eq!(rig.stored_table(), Some(table.clone()));

    let saved = dir.join("saved.inx");
    ok(&mut rig.session, "save", json!({"path": saved.to_str().unwrap()}));
    let mut reopened = Session::new();
    ok(&mut reopened, "open", json!({"path": saved.to_str().unwrap()}));
    let body = json!({"root": rig.root, "parameter": rig.parameter, "path": path, "key": [0, 0], "dryRun": true});
    let report = ok(&mut reopened, "rig.apply_mmd_pose", body);
    assert_eq!(report["boneTable"], "document");
    assert_eq!((&report["mapped"][0]["rig"], &report["mapped"][0]["mmd"]), (&json!("Head"), &json!(["Kao"])));
}

#[test]
fn the_largest_table_that_is_accepted_is_read_back_from_the_document() {
    let mut rig = rig();
    let dir = temp_dir();
    let path = write(&dir, "pose.vpd", &vpd(&[(&b"Kao"[..], [0.0; 3], about(0, 30.0))]));
    // The most entries, each with the longest key, and every byte of the key escaped in the stored JSON.
    let entries = (0..MAX_BONE_TABLE_ENTRIES).map(|i| (format!("{i:02}{}", "\"".repeat(MAX_MMD_NAME_BYTES - 2)), Value::Null));
    let table = Value::Object(entries.collect());
    assert!(serde_json::to_vec(&table).unwrap().len() > 48 * 1024);
    rig.apply(&path, json!({"boneTable": table, "storeBoneTable": true}));
    assert_eq!(rig.stored_table(), Some(table));
    assert_eq!(rig.apply(&path, json!({"dryRun": true}))["boneTable"], "document");
}

#[test]
fn storing_a_table_is_an_undo_step_even_when_the_pose_changes_nothing_and_an_empty_table_removes_it() {
    let mut rig = rig();
    let dir = temp_dir();
    // Nothing in this file drives the rig, so the model stays as it is.
    let path = write(&dir, "pose.vpd", &vpd(&[(&b"Kao"[..], [0.0; 3], about(0, 30.0))]));
    let before = rig.session.snapshot();
    rig.apply(&path, json!({"boneTable": {"Kao": null}, "storeBoneTable": true}));
    let after = rig.session.snapshot();
    assert_eq!(after["model"], before["model"]);
    assert_eq!(after["history"]["count"].as_u64().unwrap(), before["history"]["count"].as_u64().unwrap() + 1);
    assert_eq!(rig.stored_table(), Some(json!({"Kao": null})));
    ok(&mut rig.session, "undo", json!({}));
    assert_eq!(rig.stored_table(), None);
    ok(&mut rig.session, "redo", json!({}));
    assert_eq!(rig.stored_table(), Some(json!({"Kao": null})));

    rig.apply(&path, json!({"boneTable": {}, "storeBoneTable": true}));
    assert_eq!(rig.stored_table(), None);
    assert_eq!(rig.apply(&path, json!({"dryRun": true}))["boneTable"], "default");
}

#[test]
fn a_stored_table_that_is_not_valid_refuses_the_call_until_an_explicit_table_is_given() {
    let mut rig = rig();
    let dir = temp_dir();
    let path = write(&dir, "pose.vpd", &vpd(&[(HEAD, [0.0; 3], about(0, 30.0))]));
    let encode = |bytes: &[u8]| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
    let base = json!({"root": rig.root, "parameter": rig.parameter, "path": path});
    let hint = "(stored with the document; pass boneTable to replace it)";
    // An empty table padded with spaces past the 64 KiB that are read.
    let mut oversize = b"{}".to_vec();
    oversize.resize(64 * 1024 + 1, b' ');
    for (payload, message) in [
        (&br#"{"Kao": "Tail"}"#[..], "boneTable entry \"Kao\" names an unknown rig bone \"Tail\""),
        (&b"not json"[..], "the bone table stored with the document is too large or not JSON"),
        (&oversize[..], "the bone table stored with the document is too large or not JSON"),
    ] {
        ok(&mut rig.session, "extdata.set", json!({"key": TABLE_KEY, "base64": encode(payload)}));
        assert_eq!(rig.fail(base.clone()), format!("{message} {hint}"));
    }
    assert_eq!(rig.apply(&path, json!({"boneTable": {}}))["boneTable"], "argument");
}

#[test]
fn refusals_have_fixed_messages_without_paths() {
    let mut rig = rig();
    let dir = temp_dir();
    let pose = write(&dir, "pose.vpd", &vpd(&[(HEAD, [0.0; 3], about(0, 30.0))]));
    let (root, parameter) = (rig.root, rig.parameter);
    let base = json!({"root": root, "parameter": parameter, "path": pose});
    let with = |key: &str, value: Value| {
        let mut body = base.clone();
        body[key] = value;
        body
    };

    // No depth rig: the root is the document's top node.
    assert_eq!(rig.fail(with("root", json!(1))), "Node is not a DepthRigRoot");
    // The keypoint does not exist.
    assert_eq!(rig.fail(with("key", json!([9, 9]))), "key must identify an existing parameter keypoint index");
    assert_eq!(rig.fail(with("key", json!([1]))), "request.key must identify an existing parameter keypoint index");
    assert_eq!(rig.fail(with("parameter", json!(9999))), "parameter 9999 was not found");
    // Request shape.
    assert_eq!(rig.fail(with("frame", json!(3))), "frame applies to a .vmd file only");
    assert_eq!(rig.fail(with("dryRun", json!("yes"))), "dryRun must be a boolean");
    assert_eq!(rig.fail(with("translationScale", json!(0))), "translationScale must be a positive number");
    assert_eq!(rig.fail(with("path", json!("notes.txt"))), "path must name a .vpd or .vmd file");
    let motion = write(&dir, "motion.vmd", &vmd(&[vmd_bone(HEAD, 0, [0.0; 3], about(0, 0.0))], &[]));
    assert_eq!(rig.fail(with("path", json!(motion))), "frame is required for a .vmd file");
    let mut body = with("path", json!(motion));
    for frame in [json!(-1), json!(1_000_001), json!("3")] {
        body["frame"] = frame;
        assert_eq!(rig.fail(body.clone()), "frame must be a number from 0 to 1000000");
    }
    // Files.
    let missing = dir.join("missing.vpd");
    let message = rig.fail(with("path", json!(missing.to_str().unwrap())));
    assert!(message.starts_with("could not open VPD file: "), "{message}");
    let oversize = write(&dir, "big.vpd", &vec![b' '; 1024 * 1024 + 1]);
    assert_eq!(rig.fail(with("path", json!(oversize))), "VPD source exceeds the size limit");
    let malformed = write(&dir, "bad.vpd", b"not a pose");
    assert_eq!(rig.fail(with("path", json!(malformed))), "not a VPD file: unknown header");
    let truncated = write(&dir, "bad.vmd", b"Vocaloid Motion Data 0002");
    let mut body = with("path", json!(truncated));
    body["frame"] = json!(0);
    assert_eq!(rig.fail(body), "VMD is truncated in the header");
    assert!(!message.contains(dir.to_str().unwrap()), "{message}");
}

#[test]
fn a_pose_adds_no_binding_for_a_bone_it_leaves_alone() {
    let dir = temp_dir();
    let mut rig = rig();
    let head = write(&dir, "head.vpd", &vpd(&[(HEAD, [0.0; 3], about(0, 10.0))]));
    let report = rig.apply(&head, json!({}));
    let parameter = rig.parameter;
    assert!(rig.cell(parameter, "Head", "transform.r.x", [1, 1]).is_some());
    assert_eq!(rig.cell(parameter, "Head", "transform.r.z", [1, 1]), None, "a zero channel of a named bone needs no binding");
    for bone in ["Neck", "Pelvis", "Spine"] {
        assert_eq!(rig.cell(parameter, bone, "transform.r.z", [1, 1]), None, "{bone} is still free for another parameter");
    }
    assert_eq!(report["reset"], json!([]));
}

#[test]
fn the_last_report_dry_run_or_not_is_readable_as_a_resource() {
    let read_report = |session: &mut Session| {
        let read = session
            .handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "resources/read",
                "params": {"uri": "resource://soul2d/rig/mmd-report"}}))
            .unwrap();
        serde_json::from_str::<Value>(read["result"]["contents"][0]["text"].as_str().unwrap()).unwrap()
    };
    let mut rig = rig();
    let dir = temp_dir();
    assert_eq!(read_report(&mut rig.session), Value::Null);
    let path = write(&dir, "head.vpd", &vpd(&[(HEAD, [0.0; 3], about(0, 30.0))]));
    let before = rig.session.snapshot();
    let planned = rig.apply(&path, json!({"dryRun": true}));
    assert_eq!(rig.session.snapshot(), before, "a dry run changes nothing");
    assert_eq!(read_report(&mut rig.session), planned);
    let applied = rig.apply(&path, json!({"key": [2, 1]}));
    assert_eq!(read_report(&mut rig.session), applied);
    assert_eq!(applied["key"], json!([2, 1]));
}
