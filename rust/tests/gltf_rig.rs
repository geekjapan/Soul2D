//! `rig.import_gltf` and the MCP tool `rig_import_gltf`: a humanoid rig from glTF 2.0 and VRM (issue #243).
//!
//! Every file is built here from the table below (issue #242). No model file is read.

mod common;

use base64::Engine;
use serde_json::{json, Value};
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
const UNITS: f64 = 100.0;
const META_KEY: &str = "io.github.geekjapan.soul2d.VrmMeta";

/// Standard bone, its parent, the VRM humanoid name, the Mixamo-style name and the rest head in the rig frame, in
/// metres: x to the right of the picture, y down, z toward the viewer. The character faces the viewer, so its left
/// arm is the `.R` arm at positive x.
const BONES: [(&str, &str, &str, &str, [f64; 3]); 19] = [
    ("Pelvis", "", "hips", "Hips", [0.0, -1.0, 0.0]),
    ("Spine", "Pelvis", "spine", "Spine", [0.0, -1.1, 0.01]),
    ("Chest", "Spine", "chest", "Spine2", [0.0, -1.25, 0.02]),
    ("Neck", "Chest", "neck", "Neck", [0.0, -1.45, 0.0]),
    ("Head", "Neck", "head", "Head", [0.0, -1.55, 0.0]),
    ("Clavicle.R", "Chest", "leftShoulder", "LeftShoulder", [0.03, -1.4, 0.0]),
    ("UpperArm.R", "Clavicle.R", "leftUpperArm", "LeftArm", [0.18, -1.4, 0.0]),
    ("Forearm.R", "UpperArm.R", "leftLowerArm", "LeftForeArm", [0.45, -1.38, 0.0]),
    ("Hand.R", "Forearm.R", "leftHand", "LeftHand", [0.7, -1.36, 0.0]),
    ("Clavicle.L", "Chest", "rightShoulder", "RightShoulder", [-0.03, -1.4, 0.0]),
    ("UpperArm.L", "Clavicle.L", "rightUpperArm", "RightArm", [-0.18, -1.4, 0.0]),
    ("Forearm.L", "UpperArm.L", "rightLowerArm", "RightForeArm", [-0.45, -1.38, 0.0]),
    ("Hand.L", "Forearm.L", "rightHand", "RightHand", [-0.7, -1.36, 0.0]),
    ("Thigh.R", "Pelvis", "leftUpperLeg", "LeftUpLeg", [0.1, -0.95, 0.0]),
    ("Shin.R", "Thigh.R", "leftLowerLeg", "LeftLeg", [0.1, -0.5, 0.02]),
    ("Foot.R", "Shin.R", "leftFoot", "LeftFoot", [0.1, -0.08, 0.0]),
    ("Thigh.L", "Pelvis", "rightUpperLeg", "RightUpLeg", [-0.1, -0.95, 0.0]),
    ("Shin.L", "Thigh.L", "rightLowerLeg", "RightLeg", [-0.1, -0.5, 0.02]),
    ("Foot.L", "Shin.L", "rightFoot", "RightFoot", [-0.1, -0.08, 0.0]),
];

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir()
        .join(format!("soul2d-gltf-rig-{}-{}", std::process::id(), SEQUENCE.fetch_add(1, Ordering::Relaxed)));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    common::scratch(path)
}

fn head(bone: &str) -> [f64; 3] {
    BONES.iter().find(|row| row.0 == bone).unwrap().4
}

/// A humanoid of `kind` ("vrm1", "vrm0" or "gltf2") without the bones in `skip`.
///
/// The node tree is not a list of world positions: the root has a column-major matrix that turns a quarter about Y,
/// scales by two and moves, the hips are turned another quarter about Y, and every other bone is a child with a
/// local offset, so a reader has to compose the transforms.
/// The left foot has a toe bone and the head a hair bone, which the standard skeleton has no bone for.
fn humanoid(kind: &str, skip: &[&str]) -> Value {
    let signs = if kind == "vrm0" { [-1.0, -1.0, -1.0] } else { [1.0, -1.0, 1.0] };
    let source = |rig: [f64; 3]| [signs[0] * rig[0], signs[1] * rig[1], signs[2] * rig[2]];
    let quarter = std::f64::consts::FRAC_1_SQRT_2;
    let offset = [0.5, -0.25, 0.125];
    let matrix = [0.0, 0.0, -2.0, 0.0, 0.0, 2.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, offset[0], offset[1], offset[2], 1.0];
    let mut nodes = vec![json!({"name": "Root", "matrix": matrix, "children": [1]})];
    let mut index_of = std::collections::BTreeMap::new();
    let mut stated = Vec::new();
    let extra = [("Foot.R", "leftToes", "LeftToeBase", [0.1, -0.02, 0.12]), ("Head", "", "Hair", [0.0, -1.7, -0.05])];
    let rows = BONES
        .iter()
        .filter(|row| !skip.contains(&row.0))
        .map(|row| (row.0, row.1, row.2, row.3, row.4))
        .chain(extra.iter().map(|row| (row.2, row.0, row.1, row.2, row.3)));
    for (bone, mut parent, vrm, mixamo, rig) in rows {
        while skip.contains(&parent) {
            parent = BONES.iter().find(|row| row.0 == parent).unwrap().1;
        }
        let at = nodes.len();
        let name = if kind == "gltf2" { format!("mixamorig:{mixamo}") } else { format!("J_{bone}") };
        let node = if parent.is_empty() {
            // The inverse of the root matrix.
            let here = source(rig);
            let delta = [here[0] - offset[0], here[1] - offset[1], here[2] - offset[2]];
            let translation = [-delta[2] / 2.0, delta[1] / 2.0, delta[0] / 2.0];
            // A null matrix is no matrix.
            json!({"name": name, "matrix": null, "translation": translation, "rotation": [0.0, quarter, 0.0, quarter]})
        } else {
            let (here, above) = (source(rig), source(head(parent)));
            let delta = [here[0] - above[0], here[1] - above[1], here[2] - above[2]];
            // The inverse of the two quarter turns, of the root and of the hips, and of the root scale.
            let parent_at: usize = index_of[parent];
            let children = nodes[parent_at].as_object_mut().unwrap().entry("children").or_insert_with(|| json!([]));
            children.as_array_mut().unwrap().push(json!(at));
            json!({"name": name, "translation": [-delta[0] / 2.0, delta[1] / 2.0, -delta[2] / 2.0]})
        };
        nodes.push(node);
        index_of.insert(bone, at);
        if !vrm.is_empty() {
            stated.push((vrm, at));
        }
    }
    let joints: Vec<usize> = (1..nodes.len()).collect();
    let mut gltf = json!({
        "asset": {"version": "2.0"},
        "nodes": nodes,
        "skins": [{"name": "Skin", "joints": joints}],
        "meshes": [{"name": "Body", "primitives": [{"attributes": {}, "targets": [{}, {}]}]}],
        "materials": [{"name": "Cloth"}],
        "images": [{"name": "Texture"}],
        "animations": [{"name": "Idle"}],
    });
    match kind {
        "vrm1" => {
            let bones: serde_json::Map<String, Value> =
                stated.iter().map(|(name, node)| ((*name).to_owned(), json!({"node": node}))).collect();
            gltf["extensionsUsed"] = json!(["VRMC_vrm", "VRMC_springBone", "VRMC_materials_mtoon"]);
            gltf["extensions"] = json!({
                "VRMC_vrm": {
                    "specVersion": "1.0",
                    "meta": {"name": "Synthetic", "authors": ["Soul2D tests"], "licenseUrl": "https://example.invalid/terms",
                        "modification": "allowModification", "allowRedistribution": false},
                    "humanoid": {"humanBones": bones},
                    "expressions": {"preset": {"happy": {}}, "custom": {"wink": {}}},
                },
                "VRMC_springBone": {"springs": [{"name": "Hair"}]},
            });
        }
        "vrm0" => {
            let bones: Vec<Value> = stated.iter().map(|(name, node)| json!({"bone": name, "node": node})).collect();
            gltf["extensionsUsed"] = json!(["VRM"]);
            gltf["extensions"] = json!({"VRM": {
                "meta": {"title": "Synthetic", "author": "Soul2D tests", "licenseName": "CC_BY",
                    "allowedUserName": "Everyone"},
                "humanoid": {"humanBones": bones},
                "blendShapeMaster": {"blendShapeGroups": [{"name": "Joy"}]},
                "secondaryAnimation": {"boneGroups": [{}]},
            }});
        }
        _ => gltf["asset"]["copyright"] = json!("2026 Soul2D tests"),
    }
    gltf
}

/// Writes `gltf` as text for `.gltf`, and otherwise as a binary file whose binary chunk is cut short: a rig import
/// reads the JSON chunk only.
fn write(name: &str, gltf: &Value) -> String {
    let path = temp_dir().join(name);
    let mut text = serde_json::to_vec(gltf).unwrap();
    if name.ends_with(".gltf") {
        fs::write(&path, text).unwrap();
    } else {
        text.resize(text.len().div_ceil(4) * 4, b' ');
        let mut bytes = b"glTF".to_vec();
        bytes.extend(2u32.to_le_bytes());
        bytes.extend((28 + text.len() as u32 + 4096).to_le_bytes());
        bytes.extend((text.len() as u32).to_le_bytes());
        bytes.extend(b"JSON");
        bytes.extend(&text);
        bytes.extend(4096u32.to_le_bytes());
        bytes.extend(b"BIN\0");
        bytes.extend([0u8; 16]);
        fs::write(&path, bytes).unwrap();
    }
    path.to_str().unwrap().to_owned()
}

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn rig_root(session: &mut Session) -> u64 {
    let created = request(session, json!({"op": "node.create", "className": "DepthRigRoot"}));
    result(&created)["created"][0]["uuid"].as_u64().unwrap()
}

fn call(session: &mut Session, arguments: Value) -> Value {
    let reply = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "rig_import_gltf", "arguments": arguments}}))
        .unwrap();
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    reply["result"]["structuredContent"].clone()
}

fn import(session: &mut Session, root: u64, path: &str) -> Value {
    call(session, json!({"root": root, "path": path, "unitsPerSourceUnit": UNITS, "lossy": true, "acknowledgeRights": true}))
}

fn refusal(session: &mut Session, root: u64, path: &str) -> String {
    let before = session.snapshot();
    let reply = request(session, json!({"op": "rig.import_gltf", "root": root, "path": path,
        "unitsPerSourceUnit": UNITS, "lossy": true, "acknowledgeRights": true}));
    assert_eq!(reply["succeeded"], false, "{reply}");
    assert_eq!(session.snapshot(), before, "a refused import changes nothing");
    reply["message"].as_str().unwrap().to_owned()
}

fn bone<'a>(answer: &'a Value, name: &str) -> &'a Value {
    answer["bones"].as_array().unwrap().iter().find(|bone| bone["boneId"] == name).unwrap_or_else(|| panic!("{name}"))
}

#[track_caller]
fn assert_point(actual: &Value, expected: [f64; 3], what: &str) {
    for axis in 0..3 {
        let value = actual[axis].as_f64().unwrap();
        assert!((value - expected[axis] * UNITS).abs() < 1e-6, "{what}: {actual} is not {expected:?} times {UNITS}");
    }
}

fn kept(session: &mut Session, key: &str) -> Option<Value> {
    let reply = request(session, json!({"op": "extdata.get", "key": key}));
    let base64 = result(&reply)["base64"].as_str()?.to_owned();
    Some(serde_json::from_slice(&base64::engine::general_purpose::STANDARD.decode(base64).unwrap()).unwrap())
}

#[test]
fn each_convention_puts_every_bone_where_the_file_has_it() {
    for (kind, file) in [("vrm1", "model.vrm"), ("vrm0", "model.vrm"), ("gltf2", "model.gltf")] {
        let gltf = humanoid(kind, &[]);
        let mut session = Session::new();
        let root = rig_root(&mut session);
        let answer = import(&mut session, root, &write(file, &gltf));
        assert_eq!(answer["applied"], true, "{kind} {answer}");
        assert_eq!(answer["source"]["convention"], kind);
        assert_eq!(answer["bones"].as_array().unwrap().len(), 19, "{kind}");
        for row in &BONES {
            assert_point(&bone(&answer, row.0)["restHead"], row.4, &format!("{kind} {}", row.0));
        }
        // A bone ends where the next one starts; the ends of a chain follow the standard proportion.
        assert_point(&bone(&answer, "UpperArm.R")["restTail"], head("Forearm.R"), kind);
        assert_point(&bone(&answer, "Pelvis")["restTail"], head("Spine"), kind);
        assert_point(&bone(&answer, "Head")["restTail"], [0.0, -1.55 - 0.1 * 10.0 / 6.0, 0.0], kind);
        let shin = ((0.5f64 - 0.08).powi(2) + 0.02f64.powi(2)).sqrt();
        assert_point(&bone(&answer, "Foot.L")["restTail"], [-0.1, -0.08, 0.4 * shin], kind);
        assert_eq!(bone(&answer, "Spine")["parent"], bone(&answer, "Pelvis")["uuid"], "{kind}");
        assert_eq!(bone(&answer, "Pelvis")["parent"], root, "{kind}");

        // What the file states about rights stays with the document, and the whole import is one undo step.
        let (key, stated) = match kind {
            "vrm1" => (META_KEY, gltf["extensions"]["VRMC_vrm"]["meta"].clone()),
            "vrm0" => (META_KEY, gltf["extensions"]["VRM"]["meta"].clone()),
            _ => ("io.github.geekjapan.soul2d.GltfAsset", gltf["asset"].clone()),
        };
        assert_eq!(kept(&mut session, key), Some(stated), "{kind}");
        let after = session.snapshot();
        result(&request(&mut session, json!({"op": "undo"})));
        assert_eq!(result(&request(&mut session, json!({"op": "rig.list_bones", "root": root})))["items"], json!([]));
        assert_eq!(kept(&mut session, key), None, "{kind}");
        result(&request(&mut session, json!({"op": "redo"})));
        assert_eq!(session.snapshot()["model"], after["model"], "{kind}");
        assert!(kept(&mut session, key).is_some(), "{kind}");
    }
}

#[test]
fn a_standard_bone_name_is_read_only_in_a_file_soul2d_wrote() {
    let mut gltf = humanoid("gltf2", &[]);
    let hips = gltf["nodes"].as_array_mut().unwrap().iter_mut().find(|node| node["name"] == "mixamorig:Hips").unwrap();
    hips["name"] = json!("Pelvis");
    let mut session = Session::new();
    let root = rig_root(&mut session);
    assert_eq!(refusal(&mut session, root, &write("foreign.gltf", &gltf)), "the file lacks the humanoid bone Hips");
    gltf["asset"]["generator"] = json!("Soul2D 0.9.1");
    let answer = import(&mut session, root, &write("own.gltf", &gltf));
    assert_eq!(answer["applied"], true, "{answer}");
    assert_point(&bone(&answer, "Pelvis")["restHead"], head("Pelvis"), "own");
}

#[test]
fn the_report_counts_what_a_rig_import_leaves_out() {
    let mut session = Session::new();
    let root = rig_root(&mut session);
    let answer = import(&mut session, root, &write("model.vrm", &humanoid("vrm1", &[])));
    // Kept: the toe bone and the rights. Approximated: the rest orientations. Dropped: the hair joint, the mesh
    // with its two morph targets, the skin, the image, the material, the animation, the spring, two expressions
    // and the MToon extension.
    assert_eq!(answer["counts"], json!({"converted": 19, "approximated": 1, "kept": 2, "dropped": 12, "refused": 0}));
    let item = |feature: &str, source: &str| {
        let items = answer["items"].as_array().unwrap();
        items.iter().find(|item| item["feature"] == feature && item["source"] == source).cloned().unwrap_or_default()
    };
    assert_eq!(item("skeleton.bone", "leftToes")["outcome"], "kept");
    assert_eq!(item("skeleton.bone", "leftToes")["target"], "extdata:io.github.geekjapan.soul2d.VrmHumanoid");
    assert_eq!(item("skeleton.bone", "J_Hair")["outcome"], "dropped");
    assert_eq!(item("mesh.geometry", "Body")["outcome"], "dropped");
    assert_eq!(item("extension", "VRMC_materials_mtoon")["outcome"], "dropped");
    assert_eq!(item("skeleton.restPose", "")["outcome"], "approximated");
    assert_eq!(answer["rights"]["modification"], "allowModification");
    assert_eq!(answer["rights"]["authors"], json!(["Soul2D tests"]));
    assert_eq!(answer["truncated"], json!([]));
    let toes = kept(&mut session, "io.github.geekjapan.soul2d.VrmHumanoid").unwrap();
    assert_point(&toes["bones"]["leftToes"], [0.1, -0.02, 0.12], "the kept toe bone");

    // The list is cut at 256 items for one feature, the counts are not, and names are shortened and cleaned. A
    // rights field that is cut is named too.
    let mut crowded = humanoid("gltf2", &[]);
    crowded["asset"]["copyright"] = json!("c".repeat(2000));
    crowded["asset"]["minVersion"] = serde_json::from_str(&"9".repeat(40)).unwrap();
    let long = format!("line\nbreak{}", "x".repeat(100));
    crowded["meshes"] = (0..300).map(|_| json!({"name": long})).collect();
    let mut session = Session::new();
    let root = rig_root(&mut session);
    let answer = import(&mut session, root, &write("crowded.gltf", &crowded));
    let items = answer["items"].as_array().unwrap();
    let meshes: Vec<&Value> = items.iter().filter(|item| item["feature"] == "mesh.geometry").collect();
    assert_eq!(meshes.len(), 256);
    assert_eq!(meshes[0]["source"], format!("line?break{}", "x".repeat(54)));
    assert_eq!(answer["truncated"], json!(["mesh.geometry", "rights"]));
    assert_eq!(answer["rights"]["copyright"], "c".repeat(1024));
    assert_eq!(answer["rights"].get("minVersion"), None, "a number of 40 digits is not shown");
    // 300 meshes, the skin, the image, the material, the animation and two joints without a table row.
    assert_eq!(answer["counts"]["dropped"], 306);
}

#[test]
fn nothing_is_written_until_the_caller_gives_the_scale_the_lossy_flag_and_the_acknowledgement() {
    let read_report = |session: &mut Session| {
        let read = session
            .handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "resources/read",
                "params": {"uri": "resource://soul2d/rig/import-report"}}))
            .unwrap();
        serde_json::from_str::<Value>(read["result"]["contents"][0]["text"].as_str().unwrap()).unwrap()
    };
    let mut session = Session::new();
    let root = rig_root(&mut session);
    let path = write("model.vrm", &humanoid("vrm1", &[]));
    let before = session.snapshot();

    let bare = call(&mut session, json!({"root": root, "path": path}));
    assert_eq!(bare["applied"], false);
    assert_eq!(bare["needs"], json!(["unitsPerSourceUnit", "lossy", "acknowledgeRights"]));
    assert_eq!(bare["rights"]["licenseUrl"], "https://example.invalid/terms");
    assert_eq!(bare["counts"]["converted"], 19);
    let partly = call(&mut session, json!({"root": root, "path": path, "unitsPerSourceUnit": UNITS, "lossy": true}));
    assert_eq!(partly["needs"], json!(["acknowledgeRights"]));
    let dry = call(&mut session, json!({"root": root, "path": path, "unitsPerSourceUnit": UNITS, "lossy": true,
        "acknowledgeRights": true, "dryRun": true}));
    assert_eq!((&dry["applied"], &dry["needs"]), (&json!(false), &json!([])));
    assert_eq!(session.snapshot(), before);
    assert_eq!(read_report(&mut session), Value::Null);

    let applied = import(&mut session, root, &path);
    assert_eq!(applied["applied"], true);
    assert_eq!(read_report(&mut session), applied);

    // A plain glTF without a copyright line states no rights, so it needs no acknowledgement.
    let mut plain = humanoid("gltf2", &[]);
    plain["asset"].as_object_mut().unwrap().remove("copyright");
    let mut session = Session::new();
    let root = rig_root(&mut session);
    let path = write("plain.gltf", &plain);
    let answer = call(&mut session, json!({"root": root, "path": path, "unitsPerSourceUnit": UNITS, "lossy": true}));
    assert_eq!((&answer["applied"], &answer["rights"]), (&json!(true), &Value::Null));
}

#[test]
fn a_bone_the_file_may_leave_out_is_placed_between_its_neighbours() {
    let middle = |a: [f64; 3], b: [f64; 3]| [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0, (a[2] + b[2]) / 2.0];
    let mut session = Session::new();
    let root = rig_root(&mut session);
    let skipped = ["Chest", "Neck", "Clavicle.L", "Clavicle.R"];
    let answer = import(&mut session, root, &write("model.vrm", &humanoid("vrm1", &skipped)));
    let chest = middle(head("Spine"), head("Head"));
    let neck = middle(chest, head("Head"));
    assert_point(&bone(&answer, "Chest")["restHead"], chest, "Chest");
    assert_point(&bone(&answer, "Neck")["restHead"], neck, "Neck");
    assert_point(&bone(&answer, "Clavicle.R")["restHead"], middle(neck, head("UpperArm.R")), "Clavicle.R");
    assert_eq!((&answer["counts"]["converted"], &answer["counts"]["approximated"]), (&json!(15), &json!(5)));

    // With a neck, the chest is placed between the spine and the neck.
    let mut session = Session::new();
    let root = rig_root(&mut session);
    let answer = import(&mut session, root, &write("model.vrm", &humanoid("vrm1", &["Chest"])));
    assert_point(&bone(&answer, "Chest")["restHead"], middle(head("Spine"), head("Neck")), "Chest");

    // VRM 0.x requires the chest and the neck, and every format requires the hands.
    let mut session = Session::new();
    let root = rig_root(&mut session);
    let message = refusal(&mut session, root, &write("model.vrm", &humanoid("vrm0", &["Chest"])));
    assert_eq!(message, "the file lacks the humanoid bone chest");
    let message = refusal(&mut session, root, &write("model.gltf", &humanoid("gltf2", &["Hand.L"])));
    assert_eq!(message, "the file lacks the humanoid bone RightHand");
}

fn set_bone(gltf: &mut Value, bone: &str, node: u64) {
    gltf["extensions"]["VRMC_vrm"]["humanoid"]["humanBones"][bone]["node"] = json!(node);
}

#[test]
fn a_file_that_must_not_be_changed_or_cannot_be_read_safely_is_refused() {
    let mut session = Session::new();
    let root = rig_root(&mut session);
    let mut refused = |name: &str, change: &dyn Fn(&mut Value), kind: &str| {
        let mut gltf = humanoid(kind, &[]);
        change(&mut gltf);
        refusal(&mut session, root, &write(name, &gltf))
    };
    // Rights. VRM 1.0 prohibits modification unless the file allows it with a value of the schema.
    let prohibited = "the VRM meta does not allow modification, so the model is not converted";
    let stated: &dyn Fn(&mut Value) = &|g| g["extensions"]["VRMC_vrm"]["meta"]["modification"] = json!("prohibited");
    let unknown: &dyn Fn(&mut Value) = &|g| g["extensions"]["VRMC_vrm"]["meta"]["modification"] = json!("Prohibited");
    let absent: &dyn Fn(&mut Value) = &|g| {
        g["extensions"]["VRMC_vrm"]["meta"].as_object_mut().unwrap().remove("modification");
    };
    assert_eq!(refused("a.vrm", stated, "vrm1"), prohibited);
    assert_eq!(refused("a.vrm", absent, "vrm1"), prohibited);
    assert_eq!(refused("a.vrm", unknown, "vrm1"), prohibited);
    for licence in ["CC_BY_ND", "CC_BY_NC_ND", "Redistribution_Prohibited"] {
        let message = refused("a.vrm", &|g| g["extensions"]["VRM"]["meta"]["licenseName"] = json!(licence), "vrm0");
        assert_eq!(message, "the VRM licence allows no derived work, so the model is not converted");
    }
    // The file itself.
    let huge = json!([1e300, 1e300, 1e300]);
    let huge_matrix = json!([1e300, 0, 0, 0, 0, 1e300, 0, 0, 0, 0, 1e300, 0, 0, 0, 0, 1]);
    let draco = "KHR_draco_mesh_compression";
    // Node 20 is the toe bone and node 21 the hair bone, which no humanoid table of a VRM 0.x file names.
    let state = |g: &mut Value, bone: Value| {
        g["extensions"]["VRM"]["humanoid"]["humanBones"].as_array_mut().unwrap().push(bone);
    };
    // Without a chest and a neck, the chest is placed between a spine and a head that are far apart.
    let far_apart = |g: &mut Value| {
        let bones = g["extensions"]["VRMC_vrm"]["humanoid"]["humanBones"].as_object_mut().unwrap();
        bones.remove("chest");
        bones.remove("neck");
        for (node, y) in [(1, 2e307), (2, 2.5e307), (5, 1.5e307)] {
            g["nodes"][node]["translation"] = json!([0.0, y, 0.0]);
        }
    };
    let cases: [(&str, &dyn Fn(&mut Value), &str); 19] = [
        ("both the VRM 0.x and the VRM 1.0", &|g| g["extensions"]["VRMC_vrm"] = json!({}), "vrm0"),
        ("only glTF 2 is read", &|g| g["asset"]["version"] = json!("1.0"), "gltf2"),
        ("requires the extension KHR_draco_mesh_compression", &|g| g["extensionsRequired"] = json!([draco]), "gltf2"),
        ("a node has two parents", &|g| g["nodes"][0]["children"] = json!([1, 1]), "gltf2"),
        ("the node graph has a cycle", &|g| g["nodes"][2]["children"] = json!([0]), "gltf2"),
        ("a child that does not exist", &|g| g["nodes"][0]["children"] = json!([1, 9999]), "gltf2"),
        ("translation must hold 3 finite numbers", &|g| g["nodes"][3]["translation"] = json!([0, 1]), "gltf2"),
        ("translation must hold 3 finite numbers", &|g| g["nodes"][3]["translation"] = json!([0, "1", 2]), "gltf2"),
        ("a node rotation has no length", &|g| g["nodes"][3]["rotation"] = json!([0, 0, 0, 0]), "gltf2"),
        ("matrix must hold 16 finite numbers", &|g| g["nodes"][0]["matrix"] = json!([1, 2, 3]), "gltf2"),
        (
            "a node transform is not finite",
            &|g| {
                g["nodes"][0]["matrix"] = huge_matrix.clone();
                g["nodes"][1]["scale"] = huge.clone();
            },
            "gltf2",
        ),
        ("more than 65536 nodes", &|g| g["nodes"] = (0..65_537).map(|_| json!({})).collect(), "gltf2"),
        ("the humanoid bone Hips is stated twice", &|g| g["nodes"][21]["name"] = json!("mixamorig:Hips"), "gltf2"),
        ("the humanoid bone hips is stated twice", &|g| state(g, json!({"bone": "hips", "node": 21})), "vrm0"),
        (
            // Two names that are one name once control characters are replaced.
            "the humanoid bone a?b is stated twice",
            &|g| {
                state(g, json!({"bone": "a\nb", "node": 21}));
                state(g, json!({"bone": "a\tb", "node": 0}));
            },
            "vrm0",
        ),
        ("a humanoid bone has no name", &|g| state(g, json!({"node": 21})), "vrm0"),
        ("the bone Spine is outside the finite range", &far_apart, "vrm1"),
        ("names a node that does not exist", &|g| set_bone(g, "hips", 9999), "vrm1"),
        ("two humanoid bones name the same node", &|g| set_bone(g, "spine", 1), "vrm1"),
    ];
    for (expected, change, kind) in cases {
        let message = refused(if kind == "gltf2" { "a.gltf" } else { "a.vrm" }, change, kind);
        assert!(message.contains(expected), "{expected}: {message}");
    }

    // The container.
    let dir = temp_dir();
    let file = |name: &str, bytes: &[u8]| {
        fs::write(dir.join(name), bytes).unwrap();
        dir.join(name).to_str().unwrap().to_owned()
    };
    let chunk = |version: u32, length: u32, kind: &[u8; 4], body: &[u8]| {
        [b"glTF".as_slice(), &version.to_le_bytes(), &0u32.to_le_bytes(), &length.to_le_bytes(), kind, body].concat()
    };
    fs::create_dir(dir.join("folder.gltf")).unwrap();
    let folder = dir.join("folder.gltf").to_str().unwrap().to_owned();
    let mut containers = vec![
        ("JSON chunk is cut short", file("short.glb", &chunk(2, 400, b"JSON", b"{}"))),
        ("must be version 2 and start with a JSON chunk", file("one.glb", &chunk(1, 2, b"JSON", b"{}"))),
        ("must be version 2 and start with a JSON chunk", file("bin.glb", &chunk(2, 2, b"BIN\0", b"{}"))),
        ("must be version 2 and start with a JSON chunk", file("head.glb", b"glTF\x02\0\0\0")),
        ("glTF JSON is too large", file("huge.glb", &chunk(2, u32::MAX, b"JSON", b"{}"))),
        ("glTF JSON is too large", file("huge.gltf", &vec![b' '; 16 * 1024 * 1024 + 1])),
        ("glTF JSON is not valid", file("text.gltf", b"{")),
        ("glTF JSON is not valid", file("deep.gltf", "[".repeat(100_000).as_bytes())),
        ("could not open glTF file", dir.join("absent.gltf").to_str().unwrap().to_owned()),
        ("path must name a .gltf, .glb or .vrm file", file("model.fbx", b"{}")),
    ];
    // Elsewhere a directory may not open at all, which is refused with another message.
    if cfg!(unix) {
        containers.push(("must be a regular file", folder));
    }
    for (expected, path) in containers {
        let message = refusal(&mut session, root, &path);
        assert!(message.contains(expected), "{expected}: {message}");
    }

    // The request and the document.
    let path = write("model.gltf", &humanoid("gltf2", &[]));
    let refused = |session: &mut Session, change: &dyn Fn(&mut Value)| {
        let mut request = json!({"op": "rig.import_gltf", "root": root, "path": path, "unitsPerSourceUnit": UNITS,
            "lossy": true, "acknowledgeRights": true});
        change(&mut request);
        let reply = session.handle_request(&request);
        assert_eq!(reply["succeeded"], false, "{reply}");
        reply["message"].as_str().unwrap().to_owned()
    };
    for units in [json!(0), json!(-1), json!("100")] {
        let message = refused(&mut session, &|r| r["unitsPerSourceUnit"] = units.clone());
        assert_eq!(message, "unitsPerSourceUnit must be a finite positive number");
    }
    // At 1e308 every bone is a finite number, and the offset between two of them may not be.
    for units in [1e308, 1.5e308] {
        let message = refused(&mut session, &|r| r["unitsPerSourceUnit"] = json!(units));
        assert_eq!(message, "unitsPerSourceUnit puts a bone outside the finite range");
    }
    let message = refused(&mut session, &|r| r["unitsPerSourceUnit"] = json!(5e-324));
    assert_eq!(message, "unitsPerSourceUnit leaves a bone without length");
    // A kept bone is checked like a standard one, and a dry run refuses what the write would refuse.
    let mut far = humanoid("vrm1", &[]);
    far["nodes"][20]["translation"] = json!([0.0, 1e300, 0.0]);
    let far = write("far.vrm", &far);
    for dry_run in [false, true] {
        let message = refused(&mut session, &|r| {
            r["path"] = json!(far);
            r["unitsPerSourceUnit"] = json!(1e10);
            r["dryRun"] = json!(dry_run);
        });
        assert_eq!(message, "unitsPerSourceUnit puts a bone outside the finite range");
    }
    assert_eq!(refused(&mut session, &|r| r["lossy"] = json!("yes")), "lossy must be a boolean");
    assert_eq!(refused(&mut session, &|r| r["root"] = json!(999_999)), "node 999999 was not found");
    result(&session.handle_request(&json!({"op": "rig.import_gltf", "root": root, "path": path, "unitsPerSourceUnit": UNITS,
        "lossy": true, "acknowledgeRights": true})));
    assert!(refused(&mut session, &|_| {}).starts_with("the DepthRigRoot already has bones"));
}

#[test]
fn rights_an_earlier_import_kept_are_not_replaced() {
    let mut session = Session::new();
    let first = rig_root(&mut session);
    let second = rig_root(&mut session);
    let third = rig_root(&mut session);
    let gltf = humanoid("vrm1", &[]);
    assert_eq!(import(&mut session, first, &write("model.vrm", &gltf))["applied"], true);
    // The same file again states the same rights.
    assert_eq!(import(&mut session, second, &write("model.vrm", &gltf))["applied"], true);
    let mut other = gltf.clone();
    other["extensions"]["VRMC_vrm"]["meta"]["name"] = json!("Another model");
    let other = write("other.vrm", &other);
    let message = refusal(&mut session, third, &other);
    assert_eq!(message, format!("the document already keeps other data under {META_KEY}"));
    let dry = request(&mut session, json!({"op": "rig.import_gltf", "root": third, "path": other,
        "unitsPerSourceUnit": UNITS, "lossy": true, "acknowledgeRights": true, "dryRun": true}));
    assert_eq!(dry["message"], format!("the document already keeps other data under {META_KEY}"));

    // The kept humanoid bones describe the last import, so the same file at another scale replaces them.
    let arguments = json!({"root": third, "path": write("model.vrm", &gltf), "unitsPerSourceUnit": 120.0,
        "lossy": true, "acknowledgeRights": true});
    let again = call(&mut session, arguments);
    assert_eq!(again["applied"], true);
    let replaced = json!({"feature": "skeleton.bone", "source": "", "outcome": "dropped",
        "reason": "the humanoid bones an earlier import kept are replaced"});
    assert!(again["items"].as_array().unwrap().contains(&replaced), "{again}");
    assert_eq!(again["counts"]["dropped"], 13);
    let humanoid = kept(&mut session, "io.github.geekjapan.soul2d.VrmHumanoid").unwrap();
    assert_eq!(humanoid["unitsPerSourceUnit"], 120.0);
    assert!((humanoid["bones"]["leftToes"][2].as_f64().unwrap() - 0.12 * 120.0).abs() < 1e-6, "{humanoid}");
}
