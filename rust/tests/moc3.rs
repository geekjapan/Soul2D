//! `live2d.runtime.inspect` on synthetic runtime files: the layout is built here, so no Live2D sample is committed.

use serde_json::{json, Value};
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;

fn id(name: &str) -> Vec<u8> {
    let mut slot = name.as_bytes().to_vec();
    slot.resize(64, 0);
    slot
}

fn words<T: Copy>(values: &[T], bytes: fn(T) -> [u8; 4]) -> Vec<u8> {
    values.iter().flat_map(|value| bytes(*value)).collect()
}

/// A version-3 file with two parts, one warp deformer, two parameters and two drawables.
fn moc3(version: u8) -> Vec<u8> {
    let mut counts = vec![0u32; 32];
    counts[..6].copy_from_slice(&[2, 1, 1, 0, 2, 2]);
    let mut sections: Vec<(usize, Vec<u8>)> = vec![
        (0, words(&counts, u32::to_le_bytes)),
        (1, words(&[1200.0f32, 600.0, 900.0, 1200.0, 1800.0], f32::to_le_bytes)),
        (3, [id("PartBody"), id("PartFace")].concat()),
        (11, id("Warp")),
        (33, [id("ArtMesh0"), id("ArtMesh1")].concat()),
        (39, words(&[1i32, -1], i32::to_le_bytes)),
        (41, words(&[0i32, 1], i32::to_le_bytes)),
        (43, words(&[4i32, 3], i32::to_le_bytes)),
        (46, words(&[6i32, 3], i32::to_le_bytes)),
        (50, [id("ParamAngleX"), id("ParamEyeLOpen")].concat()),
        (51, words(&[30.0f32, 1.0], f32::to_le_bytes)),
        (52, words(&[-30.0f32, 0.0], f32::to_le_bytes)),
        (53, words(&[0.0f32, 1.0], f32::to_le_bytes)),
    ];
    let slots = 102;
    let mut offsets = vec![0u32; slots];
    let mut body = Vec::new();
    let start = 0x40 + 4 * slots;
    for (slot, bytes) in sections.drain(..) {
        offsets[slot] = (start + body.len()) as u32;
        body.extend(bytes);
        body.resize(body.len().div_ceil(64) * 64, 0);
    }
    // Slots this reader does not use still point inside the file.
    for offset in offsets.iter_mut().filter(|offset| **offset == 0) {
        *offset = start as u32;
    }
    let mut file = b"MOC3".to_vec();
    file.push(version);
    file.resize(0x40, 0);
    file.extend(words(&offsets, u32::to_le_bytes));
    file.extend(body);
    file
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("soul2d-moc3-test-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn inspect(session: &mut Session, path: &PathBuf) -> Value {
    session.handle_request(&json!({"op": "live2d.runtime.inspect", "path": path}))
}

#[test]
fn a_runtime_model_is_read_from_its_model3_and_moc3() {
    let dir = scratch("read");
    let mut session = Session::new();
    let before = session.snapshot();
    let moc = dir.join("model.moc3");
    fs::write(&moc, moc3(3)).unwrap();
    let read = inspect(&mut session, &moc);
    assert_eq!(read["succeeded"], true, "{read}");
    let model = &read["result"];
    assert_eq!(model["version"], 3);
    assert_eq!(model["canvas"], json!({"pixelsPerUnit": 1200.0, "origin": [600.0, 900.0], "size": [1200.0, 1800.0]}));
    assert_eq!(model["parts"], json!(["PartBody", "PartFace"]));
    assert_eq!(model["deformers"], json!({"ids": ["Warp"], "warp": 1, "rotation": 0}));
    assert_eq!(model["parameters"], json!([{"id": "ParamAngleX", "min": -30.0, "max": 30.0, "default": 0.0},
        {"id": "ParamEyeLOpen", "min": 0.0, "max": 1.0, "default": 1.0}]));
    assert_eq!(model["drawables"], json!([
        {"id": "ArtMesh0", "parentPart": "PartFace", "texture": 0, "vertices": 4, "triangles": 2},
        {"id": "ArtMesh1", "parentPart": null, "texture": 1, "vertices": 3, "triangles": 1}]));

    let model3 = dir.join("model.model3.json");
    fs::write(&model3, json!({"Version": 3, "FileReferences": {"Moc": "model.moc3",
        "Textures": ["model.2048/texture_00.png", "model.2048/texture_01.png"], "Physics": "model.physics3.json"},
        "Groups": [{"Target": "Parameter", "Name": "EyeBlink", "Ids": ["ParamEyeLOpen"]}]}).to_string()).unwrap();
    let read = inspect(&mut session, &model3);
    assert_eq!(read["result"]["model3"]["physics"], "model.physics3.json", "{read}");
    assert_eq!(read["result"]["parameters"], model["parameters"]);
    assert_eq!(session.snapshot(), before, "reading changes nothing");

    fs::write(&moc, moc3(2)).unwrap();
    assert_eq!(inspect(&mut session, &moc)["result"]["version"], 2, "version 2 has version 3's layout");

    // Refused rather than guessed: an unseen version, a big-endian flag, a reference leaving the folder, too few
    // textures for the drawables.
    fs::write(&moc, moc3(4)).unwrap();
    assert!(inspect(&mut session, &moc)["message"].as_str().unwrap().contains("version 4"));
    let mut big = moc3(3);
    big[5] = 1;
    fs::write(&moc, big).unwrap();
    assert!(inspect(&mut session, &moc)["message"].as_str().unwrap().contains("little-endian"));
    fs::write(&moc, moc3(3)).unwrap();
    fs::write(&model3, json!({"FileReferences": {"Moc": "../model.moc3"}}).to_string()).unwrap();
    assert!(inspect(&mut session, &model3)["message"].as_str().unwrap().contains("inside its folder"));
    fs::write(&model3, json!({"FileReferences": {"Moc": "model.moc3", "Textures": ["one.png"]}}).to_string()).unwrap();
    assert!(inspect(&mut session, &model3)["message"].as_str().unwrap().contains("texture"));
    fs::write(&moc, &moc3(3)[..300]).unwrap();
    assert_eq!(inspect(&mut session, &moc)["succeeded"], false, "a truncated file is refused");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_motion3_export_is_checked_against_a_runtime_model() {
    let dir = scratch("motion");
    let moc = dir.join("model.moc3");
    fs::write(&moc, moc3(3)).unwrap();
    let curve = |target: &str, id: &str| json!({"Target": target, "Id": id, "Segments": [0.0, 0.0, 0, 1.0, 1.0]});
    let motion = |curves: Vec<Value>| json!({"Version": 3, "Meta": {"Duration": 1.0, "Fps": 30.0, "Loop": false,
        "AreBeziersRestricted": true, "CurveCount": curves.len(), "TotalSegmentCount": curves.len(),
        "TotalPointCount": 2 * curves.len(), "UserDataCount": 0, "TotalUserDataSize": 0}, "Curves": curves});
    let mut session = Session::new();
    let known = dir.join("known.motion3.json");
    fs::write(&known, motion(vec![curve("Parameter", "ParamAngleX"), curve("PartOpacity", "PartFace"),
        curve("Model", "Opacity")]).to_string()).unwrap();
    let imported = session.handle_request(&json!({"op": "motion3.import", "name": "known", "path": known}));
    assert_eq!(imported["succeeded"], true, "{imported}");
    let out = dir.join("out.motion3.json");
    let exported = session.handle_request(&json!({"op": "motion3.export", "name": "known", "path": out, "runtimeModel": moc}));
    assert_eq!(exported["result"]["checkedCurves"], 2, "{exported}");
    assert!(out.exists());

    let unknown = dir.join("unknown.motion3.json");
    fs::write(&unknown, motion(vec![curve("Parameter", "ParamMissing"), curve("PartOpacity", "PartArm")]).to_string()).unwrap();
    session.handle_request(&json!({"op": "motion3.import", "name": "unknown", "path": unknown}));
    let refused_out = dir.join("refused.motion3.json");
    let refused = session.handle_request(&json!({"op": "motion3.export", "name": "unknown", "path": refused_out,
        "runtimeModel": moc}));
    assert_eq!(refused["message"], "the runtime model has no Parameter ParamMissing, PartOpacity PartArm", "{refused}");
    assert!(!refused_out.exists(), "nothing is written when the check fails");
    fs::remove_dir_all(dir).unwrap();
}
