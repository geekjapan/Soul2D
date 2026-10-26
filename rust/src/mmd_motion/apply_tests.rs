//! `rig.apply_mmd_pose` against the rig's own forward path: the world rotation each bone ends up with at the written
//! keypoint (`bone_deform::keypoint_world_quats_wxyz`) must be the MMD world rotation, in rig axes. Session-level
//! behaviour (reports, dry run, undo, refusals) is in `rust/tests/mmd_pose_apply.rs`.

use super::retarget::quat_mul_wxyz;
use super::tests::BONE_BYTES;
use crate::bone_deform::keypoint_world_quats_wxyz;
use crate::Session;
use serde_json::{json, Value};
use std::collections::BTreeMap;

type Quat = [f64; 4];

fn request(session: &mut Session, op: &str, body: Value) -> Value {
    let mut body = body;
    body["op"] = json!(op);
    let reply = session.handle_request(&body);
    assert_eq!(reply["succeeded"], true, "{op}: {reply}");
    reply["result"].clone()
}

/// (x, y, z, w) of a rotation of `degrees` about the MMD axis `axis`.
fn about(axis: usize, degrees: f64) -> [f64; 4] {
    let half = degrees.to_radians() / 2.0;
    let mut q = [0.0, 0.0, 0.0, half.cos()];
    q[axis] = half.sin();
    q
}

fn name_bytes(name: &str) -> &'static [u8] {
    BONE_BYTES.iter().find(|(_, text, _)| *text == name).unwrap().0
}

/// MMD quaternion (x, y, z, w) to the rig's (w, x, y, z): the same rotation seen from the rig frame, (x, -y, -z).
fn to_rig(q: [f64; 4]) -> Quat {
    [q[3], q[0], -q[1], -q[2]]
}

fn product(factors: &[Quat]) -> Quat {
    factors.iter().fold([1.0, 0.0, 0.0, 0.0], |acc, q| quat_mul_wxyz(acc, *q))
}

struct Fixture {
    session: Session,
    root: u32,
    parameter: u32,
    bones: BTreeMap<String, u32>,
    rest_world: BTreeMap<u32, Quat>,
    dir: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let mut session = Session::new();
    let root = request(&mut session, "node.create", json!({"className": "DepthRigRoot", "name": "Rig"}))["created"][0]
        ["uuid"]
        .as_u64()
        .unwrap() as u32;
    let listed = request(&mut session, "rig.add_skeleton", json!({"root": root}));
    let bones: BTreeMap<String, u32> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["boneId"].as_str().unwrap().to_owned(), item["uuid"].as_u64().unwrap() as u32))
        .collect();
    let parameter = request(&mut session, "parameter.create", json!({"name": "Pose", "dimensions": 2}))["uuid"]
        .as_u64()
        .unwrap() as u32;
    // With no binding the pose is zero, so the world rotation is the rest orientation (parents are unposed too).
    let rest_world = keypoint_world_quats_wxyz(&session.snapshot()["model"], root, parameter, [1, 1]).unwrap();
    static SEQUENCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("soul2d-mmd-apply-{}-{sequence}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Fixture { session, root, parameter, bones, rest_world, dir }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Fixture {
    fn apply(&mut self, pose: &[(&str, [f64; 3], [f64; 4])], file: &str) {
        let mut text = format!("Vocaloid Pose Data file\r\n\r\nm.osm;\r\n{};\r\n\r\n", pose.len()).into_bytes();
        for (index, (name, p, q)) in pose.iter().enumerate() {
            text.extend(format!("Bone{index}{{").as_bytes());
            text.extend(name_bytes(name));
            text.extend(format!("\r\n{},{},{};\r\n{:.12},{:.12},{:.12},{:.12};\r\n}}\r\n", p[0], p[1], p[2], q[0], q[1], q[2], q[3]).as_bytes());
        }
        let path = self.dir.join(file);
        std::fs::write(&path, text).unwrap();
        let (root, parameter) = (self.root, self.parameter);
        request(
            &mut self.session,
            "rig.apply_mmd_pose",
            json!({"root": root, "parameter": parameter, "path": path.to_str().unwrap(), "key": [1, 1]}),
        );
    }

    /// Asserts that the rig bone's world rotation is `expected * rest` (up to the sign of the quaternion).
    fn assert_world(&self, bone: &str, expected: Quat) {
        let model = self.session.snapshot()["model"].clone();
        let worlds = keypoint_world_quats_wxyz(&model, self.root, self.parameter, [1, 1]).unwrap();
        let id = self.bones[bone];
        let want = quat_mul_wxyz(expected, self.rest_world[&id]);
        let got = worlds[&id];
        let dot: f64 = want.iter().zip(got).map(|(a, b)| a * b).sum();
        assert!((dot.abs() - 1.0).abs() < 1e-12 && want.iter().zip(got).all(|(a, b)| (a - b * dot.signum()).abs() < 1e-6),
            "{bone}: world {got:?}, wanted {want:?}");
    }
}

#[test]
fn every_chain_reaches_the_mmd_world_rotation() {
    let mut f = fixture();
    let (c, l, u, u2) = (about(1, 10.0), about(0, 20.0), about(1, 25.0), about(2, -12.0));
    let (neck, head) = (about(0, 8.0), about(1, -30.0));
    let (arm, elbow) = (about(2, 40.0), about(1, 35.0));
    let (leg, knee, ankle) = (about(0, 15.0), about(0, -30.0), about(2, 10.0));
    f.apply(
        &[
            ("センター", [0.0; 3], c),
            ("下半身", [0.0; 3], l),
            ("上半身", [0.0; 3], u),
            ("上半身2", [0.0; 3], u2),
            ("首", [0.0; 3], neck),
            ("頭", [0.0; 3], head),
            ("左腕", [0.0; 3], arm),
            ("左ひじ", [0.0; 3], elbow),
            ("左足", [0.0; 3], leg),
            ("左ひざ", [0.0; 3], knee),
            ("左足首", [0.0; 3], ankle),
        ],
        "chains.vpd",
    );
    let [c, l, u, u2, neck, head, arm, elbow, leg, knee, ankle] =
        [c, l, u, u2, neck, head, arm, elbow, leg, knee, ankle].map(to_rig);
    // MMD hangs 下半身 and 上半身 side by side under センター; the rig hangs Spine under Pelvis.
    f.assert_world("Pelvis", product(&[c, l]));
    f.assert_world("Spine", product(&[c, u]));
    f.assert_world("Chest", product(&[c, u, u2]));
    f.assert_world("Neck", product(&[c, u, u2, neck]));
    f.assert_world("Head", product(&[c, u, u2, neck, head])); // the Head ignores its parent in the rig
    f.assert_world("Clavicle.R", product(&[c, u, u2]));
    f.assert_world("UpperArm.R", product(&[c, u, u2, arm]));
    f.assert_world("Forearm.R", product(&[c, u, u2, arm, elbow]));
    f.assert_world("Thigh.R", product(&[c, l, leg]));
    f.assert_world("Shin.R", product(&[c, l, leg, knee]));
    f.assert_world("Foot.R", product(&[c, l, leg, knee, ankle])); // lockToRoot: ignores its parent too
    f.assert_world("UpperArm.L", product(&[c, u, u2]));
}

#[test]
fn a_lower_body_turn_alone_leaves_the_torso_where_mmd_does() {
    let mut f = fixture();
    let l = about(1, 35.0);
    f.apply(&[("下半身", [0.0; 3], l)], "lower.vpd");
    let (identity, l) = ([1.0, 0.0, 0.0, 0.0], to_rig(l));
    f.assert_world("Pelvis", l);
    for upper in ["Spine", "Chest", "Neck", "Head", "Clavicle.L", "UpperArm.R"] {
        f.assert_world(upper, identity);
    }
    f.assert_world("Thigh.L", l);
}
