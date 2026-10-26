//! Application-shell operations: INP import/merge, image folder import, image merge and the puppet
//! tools (`import.inp`, `merge.inp`, `import.folder`, `merge.images`, `tool.*`), and the D command-name
//! and resource surface of the MCP endpoint.

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

fn texture_pixels(session: &mut Session, slot: u32) -> Vec<u8> {
    let result = ok(session, json!({"op": "texture.get", "slot": slot}));
    let data = base64::engine::general_purpose::STANDARD.decode(result["base64"].as_str().unwrap()).unwrap();
    image::load_from_memory(&data).unwrap().to_rgba8().into_raw()
}

fn find<'a>(node: &'a Value, id: u64) -> Option<&'a Value> {
    if node["uuid"] == id {
        return Some(node);
    }
    node["children"].as_array()?.iter().find_map(|child| find(child, id))
}

// ------------------------------------------------------------------------------------ import.inp

#[test]
fn import_inp_opens_the_file_as_an_unsaved_project() {
    let dir = temp_dir("import-inp");
    let path = write_inp(&dir, "a.inp", &model(vec![part(2, "Face", 0, json!([]))], vec![], json!({})), &[png(1, 1, &[[1, 2, 3, 255]])]);
    let mut session = Session::new();
    ok(&mut session, json!({"op": "history.limit.set", "limit": 7}));
    let snapshot = ok(&mut session, json!({"op": "import.inp", "path": path}));
    assert_eq!(snapshot["history"]["limit"], 7, "an import keeps the session's history limit");
    assert_eq!(snapshot["model"]["nodes"]["children"][0]["name"], "Face");
    assert_eq!(snapshot["modified"], false);
    assert_eq!(texture_pixels(&mut session, 0), vec![1, 2, 3, 255]);
    // D incImportINP does not adopt the file as the project path: a later save must ask for one.
    let saved = ok(&mut session, json!({"op": "save", "path": dir.join("b.inx").to_str().unwrap()}));
    assert!(saved.is_object());
    assert!(dir.join("b.inx").exists());
    assert_eq!(fail(&mut session, json!({"op": "import.inp", "path": dir.join("missing.inp").to_str().unwrap()})).contains("could not open"), true);
    fs::write(dir.join("junk.inp"), b"nope").unwrap();
    assert!(fail(&mut session, json!({"op": "import.inp", "path": dir.join("junk.inp").to_str().unwrap()})).contains("magic"));
}

// ------------------------------------------------------------------------------------ merge.inp

#[test]
fn merge_inp_appends_children_parameters_and_animations_with_fresh_ids() {
    let dir = temp_dir("merge-inp");
    let first = png(1, 1, &[[10, 20, 30, 255]]);
    let second = png(1, 1, &[[40, 50, 60, 255]]);
    // Both list their parameter 10 as an eye for blinking.
    let eye = |mut model: Value| {
        model["live2d"] = json!({"eyeBlink": [10]});
        model
    };
    let target = write_inp(
        &dir, "target.inp",
        &eye(model(vec![part(2, "Face", 0, json!([]))], vec![parameter(10, "Blink", 2)], json!({"idle": {"timestep": 0.0166, "lanes": []}}))),
        &[first.clone()],
    );
    let source = write_inp(
        &dir, "source.inp",
        &eye(model(vec![part(2, "Eye", 0, json!([]))], vec![parameter(10, "Smile", 2)], json!({"idle": {"timestep": 0.5, "lanes": []},
            "wave": {"timestep": 0.25, "lanes": [], "bookmarks": [{"label": "Eye", "x": 0.0, "y": 0.0, "parameters": [10]}],
                "controllers": [{"name": "Lid", "start": {"node": 2, "vertices": [0, 1, 2], "weights": [1.0, 0.0, 0.0]},
                    "end": {"node": 2, "vertices": [0, 1, 2], "weights": [0.0, 1.0, 0.0]}, "parameters": [10]}],
                "forms": [{"node": 2, "mode": 1, "keys": [{"frame": 0, "offsets": []}]}]}}))),
        &[second.clone()],
    );
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": target}));
    let merged = ok(&mut session, json!({"op": "merge.inp", "path": source}));
    // The source's 2 and 10 collide with the target; the lowest unused ids are 3 and 4.
    assert_eq!(merged["created"], json!([3]));
    let snapshot = session.snapshot();
    let nodes = &snapshot["model"]["nodes"];
    assert_eq!(nodes["children"][0]["name"], "Face");
    assert_eq!(nodes["children"][1]["uuid"], 3);
    assert_eq!(nodes["children"][1]["name"], "Eye");
    assert_eq!(nodes["children"][1]["textures"], json!([1]));
    let params = snapshot["model"]["param"].as_array().unwrap();
    assert_eq!(params.len(), 2);
    assert_eq!((params[1]["uuid"].as_u64(), params[1]["name"].as_str()), (Some(4), Some("Smile")));
    assert_eq!(params[1]["bindings"][0]["node"], 3, "the binding follows the renumbered node");
    assert_eq!(snapshot["model"]["live2d"]["eyeBlink"], json!([10, 4]), "the source's eye joins under its new id");
    let animations = snapshot["model"]["animations"].as_object().unwrap();
    assert_eq!(animations.keys().cloned().collect::<Vec<_>>(), ["idle", "idle (2)", "wave"]);
    assert_eq!(animations["idle"]["timestep"], 0.0166);
    assert_eq!(animations["idle (2)"]["timestep"], 0.5);
    assert_eq!(animations["wave"]["bookmarks"][0]["parameters"], json!([4]), "the label follows the renumbered parameter");
    let lid = &animations["wave"]["controllers"][0];
    assert_eq!((&lid["parameters"], &lid["start"]["node"], &lid["end"]["node"]), (&json!([4]), &json!(3), &json!(3)),
        "a controller follows its renumbered parameter and Part");
    assert_eq!(animations["wave"]["forms"][0]["node"], 3, "a form edit follows its renumbered node");
    assert_eq!(texture_pixels(&mut session, 0), vec![10, 20, 30, 255]);
    assert_eq!(texture_pixels(&mut session, 1), vec![40, 50, 60, 255]);
    assert_eq!(snapshot["modified"], true);

    // One undo step brings back the model and the texture list.
    ok(&mut session, json!({"op": "undo"}));
    let before = session.snapshot();
    assert_eq!(before["model"]["nodes"]["children"].as_array().unwrap().len(), 1);
    assert!(fail(&mut session, json!({"op": "texture.get", "slot": 1})).contains("out of range"));
}

#[test]
fn merge_inp_can_skip_parameters_and_animations_and_keeps_free_ids() {
    let dir = temp_dir("merge-inp-flags");
    let tex = png(1, 1, &[[1, 1, 1, 255]]);
    let target = write_inp(&dir, "t.inp", &model(vec![part(2, "Face", 0, json!([]))], vec![], json!({})), &[tex.clone()]);
    let source = write_inp(
        &dir, "s.inp",
        &model(vec![part(7, "Hair", 0, json!([part(8, "Strand", 0, json!([]))]))], vec![parameter(9, "Sway", 7)], json!({"wave": {"lanes": []}})),
        &[tex],
    );
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": target}));
    let merged = ok(&mut session, json!({"op": "merge.inp", "path": source, "mergeParameters": false, "mergeAnimations": false}));
    assert_eq!(merged["created"], json!([7]));
    let snapshot = session.snapshot();
    assert!(find(&snapshot["model"]["nodes"], 8).is_some(), "ids that do not collide are kept");
    assert_eq!(snapshot["model"]["param"], json!([]));
    assert_eq!(snapshot["model"]["animations"], json!({}));
    // Both parts use the same pixels but each keeps its own slot after the merge.
    assert_eq!(find(&snapshot["model"]["nodes"], 7).unwrap()["textures"], json!([1]));
}

#[test]
fn merge_inp_rejects_what_it_cannot_merge() {
    let dir = temp_dir("merge-inp-errors");
    let mut moved = model(vec![], vec![], json!({}));
    moved["nodes"]["transform"]["trans"] = json!([5.0, 0.0, 0.0]);
    let moved = write_inp(&dir, "moved.inp", &moved, &[]);
    let plain = write_inp(&dir, "plain.inp", &model(vec![], vec![], json!({})), &[]);
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": plain}));
    assert!(fail(&mut session, json!({"op": "merge.inp", "path": moved})).contains("root transforms"));
    assert!(fail(&mut session, json!({"op": "merge.inp"})).contains("path"));
    assert!(fail(&mut session, json!({"op": "merge.inp", "path": plain, "mergeParameters": "yes"})).contains("mergeParameters"));
}

// ------------------------------------------------------------------------------------ import.folder

#[test]
fn import_folder_makes_one_part_per_image() {
    let dir = temp_dir("folder");
    fs::write(dir.join("b.png"), png(4, 2, &[[200, 100, 50, 128]; 8])).unwrap();
    fs::write(dir.join("a.png"), png(2, 2, &[[10, 10, 10, 0]; 4])).unwrap();
    let tga = image::RgbaImage::from_raw(1, 1, vec![9, 8, 7, 255]).unwrap();
    tga.save_with_format(dir.join("c.tga"), image::ImageFormat::Tga).unwrap();
    fs::write(dir.join("notes.txt"), b"ignored").unwrap();
    let photo = image::RgbaImage::from_pixel(8, 8, image::Rgba([90, 120, 200, 255]));
    fs::write(dir.join("photo.jpg"), soul2d_core::export::encode_jpeg(&photo).unwrap()).unwrap();
    let mut session = Session::new();
    let snapshot = ok(&mut session, json!({"op": "import.folder", "path": dir.to_str().unwrap()}));
    let root = &snapshot["model"]["nodes"];
    let names: Vec<&str> = root["children"].as_array().unwrap().iter().map(|part| part["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["a", "b", "c", "photo"]);
    let ids: Vec<u64> = root["children"].as_array().unwrap().iter().map(|part| part["uuid"].as_u64().unwrap()).collect();
    assert_eq!(ids, [2, 3, 4, 5]);
    let b = &root["children"][1];
    assert_eq!(b["mesh"]["verts"], json!([-2.0, -1.0, -2.0, 1.0, 2.0, -1.0, 2.0, 1.0]));
    assert_eq!(b["mesh"]["uvs"], json!([0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0]));
    assert_eq!(b["mesh"]["indices"], json!([0, 1, 2, 2, 1, 3]));
    assert_eq!(b["textures"], json!([1]));
    assert_eq!(root["children"][0]["zsort"].as_f64().unwrap(), 0.0);
    assert_eq!(b["zsort"].as_f64().unwrap(), -f64::from(1.0f32 / 100.0));
    assert_eq!(root["children"][2]["zsort"].as_f64().unwrap(), -f64::from(2.0f32 / 100.0));
    assert_eq!(snapshot["imported"]["skipped"], json!([]));
    assert_eq!(snapshot["imported"]["nodes"], 4);
    // Premultiplied like inTexPremultiply: 200 * 128 / 255 = 100, 100 * 128 / 255 = 50, 50 * 128 / 255 = 25.
    assert_eq!(&texture_pixels(&mut session, 1)[..4], &[100, 50, 25, 128]);
    assert_eq!(texture_pixels(&mut session, 2), vec![9, 8, 7, 255]);
    assert_eq!(snapshot["modified"], false);
}

#[test]
fn import_folder_rejects_empty_and_broken_folders() {
    let dir = temp_dir("folder-errors");
    let mut session = Session::new();
    assert!(fail(&mut session, json!({"op": "import.folder", "path": dir.to_str().unwrap()})).contains("No images imported"));
    fs::write(dir.join("bad.png"), b"not a png").unwrap();
    assert!(fail(&mut session, json!({"op": "import.folder", "path": dir.to_str().unwrap()})).starts_with("Could not load bad.png:"));
    assert!(fail(&mut session, json!({"op": "import.folder", "path": dir.join("nope").to_str().unwrap()})).contains("could not read folder"));
}

// ------------------------------------------------------------------------------------ merge.images

#[test]
fn merge_images_adds_parts_in_one_undo_step() {
    let dir = temp_dir("merge-images");
    fs::write(dir.join("x.png"), png(2, 4, &[[255, 0, 0, 255]; 8])).unwrap();
    fs::write(dir.join("y.PNG"), png(2, 2, &[[0, 255, 0, 255]; 4])).unwrap();
    let x = dir.join("x.png").to_str().unwrap().to_owned();
    let y = dir.join("y.PNG").to_str().unwrap().to_owned();
    let mut session = Session::new();
    ok(&mut session, json!({"op": "new"}));
    let created = ok(&mut session, json!({"op": "merge.images", "paths": format!("{x}|{y}")}));
    assert_eq!(created, json!({"created": [2, 3], "parent": 1}));
    let snapshot = session.snapshot();
    let children = snapshot["model"]["nodes"]["children"].as_array().unwrap();
    assert_eq!(children[0]["name"], "x.png");
    assert_eq!(children[1]["name"], "y.PNG");
    assert_eq!(children[0]["mesh"]["verts"], json!([-1.0, -2.0, -1.0, 2.0, 1.0, -2.0, 1.0, 2.0]));
    assert_eq!(children[1]["textures"], json!([1]));
    assert_eq!(texture_pixels(&mut session, 0)[..4], [255, 0, 0, 255]);

    // A parent other than the root receives the new Part.
    let nested = ok(&mut session, json!({"op": "merge.images", "paths": [x.clone()], "parent": 2}));
    assert_eq!(nested, json!({"created": [4], "parent": 2}));
    assert_eq!(find(&session.snapshot()["model"]["nodes"], 2).unwrap()["children"][0]["uuid"], 4);

    ok(&mut session, json!({"op": "undo"}));
    ok(&mut session, json!({"op": "undo"}));
    assert_eq!(session.snapshot()["model"]["nodes"]["children"], Value::Null);
}

#[test]
fn merge_images_validates_before_it_loads() {
    let dir = temp_dir("merge-images-errors");
    fs::write(dir.join("a.png"), png(1, 1, &[[0, 0, 0, 255]])).unwrap();
    fs::write(dir.join("b.jpg"), b"x").unwrap();
    let a = dir.join("a.png").to_str().unwrap().to_owned();
    let mut session = Session::new();
    assert_eq!(fail(&mut session, json!({"op": "merge.images", "paths": [a.clone(), "c.bmp"]})), "Invalid file type .bmp");
    assert_eq!(fail(&mut session, json!({"op": "merge.images", "paths": ""})), "Paths not provided");
    assert!(fail(&mut session, json!({"op": "merge.images", "paths": [dir.join("b.jpg").to_str().unwrap()]})).contains("Could not load b.jpg: not a JPEG file"));
    assert!(fail(&mut session, json!({"op": "merge.images", "paths": [a.clone()], "parent": 99})).contains("99"));
    assert_eq!(session.snapshot()["model"]["nodes"]["children"], Value::Null, "nothing was added");
}

// ------------------------------------------------------------------------------------ sketch.snapshot

/// One straight-alpha RGBA picture of `bounds` at `width` x `height`, optionally of `solo` only.
fn render(session: &mut Session, width: u32, height: u32, bounds: [f64; 4], solo: Option<Value>) -> Vec<u8> {
    let mut request = json!({"op": "render", "width": width, "height": height, "bounds": bounds});
    if let Some(solo) = solo {
        request["solo"] = solo;
    }
    let data = base64::engine::general_purpose::STANDARD.decode(ok(session, request)["png"].as_str().unwrap()).unwrap();
    image::load_from_memory(&data).unwrap().to_rgba8().into_raw()
}

/// A 10 x 10 Part (id 2, zsort -5) of one texel per unit, with half-transparent and clear texels and a clear bottom row, moved 10
/// units right by parameter 10 at 1. Its bounds, 10% wider on each side, are 12 units; the snapshot keeps the 10 x 9 that draw.
fn sketch_session(dir: &Path) -> Session {
    let mut face = part(2, "Face", 0, json!([]));
    face["zsort"] = json!(-5.0);
    face["mesh"]["verts"] = json!([-5.0, -5.0, -5.0, 5.0, 5.0, -5.0, 5.0, 5.0]);
    let pixels: Vec<[u8; 4]> = (0..100u8).map(|i| [i * 2, 255 - i, 50, if i >= 90 { 0 } else { [255, 128, 0, 255][usize::from(i % 4)] }]).collect();
    let path = write_inp(dir, "s.inp", &model(vec![face], vec![parameter(10, "Move", 2)], json!({})), &[png(10, 10, &pixels)]);
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": path}));
    session
}

#[test]
fn sketch_snapshot_keeps_the_current_picture_as_a_faint_front_part() {
    let dir = temp_dir("sketch");
    let mut session = sketch_session(&dir);
    ok(&mut session, json!({"op": "value.set", "id": 10, "value": [1.0, 0.0]}));
    let bounds = [5.0, -5.0, 15.0, 5.0];
    let before = render(&mut session, 10, 10, bounds, None);
    assert_eq!(before[3..12].iter().step_by(4).collect::<Vec<_>>(), [&255, &128, &0], "texels are drawn as they are");

    let result = ok(&mut session, json!({"op": "sketch.snapshot", "opacity": 1.0}));
    assert_eq!(result, json!({"created": [3], "parent": 1, "width": 10, "height": 9}));
    let snapshot = session.snapshot();
    let guide = find(&snapshot["model"]["nodes"], 3).unwrap();
    assert_eq!(guide["name"], "Snapshot");
    assert_eq!(guide["opacity"], 1.0);
    assert_eq!(guide["zsort"], -6.0, "in front of the Part at -5");
    assert_eq!(guide["transform"]["trans"], json!([10.0, -0.5, 0.0]), "at the drawn texels of the moved Part");
    assert_eq!(guide["transform"]["scale"], json!([1.0, 1.0]));
    let kept = render(&mut session, 10, 10, bounds, Some(json!([3])));
    for (index, (kept, before)) in kept.iter().zip(&before).enumerate() {
        assert!(kept.abs_diff(*before) <= 2, "channel {index}: {kept} != {before}");
    }

    ok(&mut session, json!({"op": "undo"}));
    assert!(find(&session.snapshot()["model"]["nodes"], 3).is_none(), "one undo step");
}

#[test]
fn sketch_snapshot_is_not_tinted_twice_by_the_ambient_light() {
    let dir = temp_dir("sketch-ambient");
    let mut session = sketch_session(&dir);
    ok(&mut session, json!({"op": "scene.ambient.set", "color": [0.5, 0.5, 0.5]}));
    let bounds = [-5.0, -5.0, 5.0, 5.0];
    let before = render(&mut session, 10, 10, bounds, Some(json!([2])));
    ok(&mut session, json!({"op": "sketch.snapshot", "opacity": 1.0}));
    let kept = render(&mut session, 10, 10, bounds, Some(json!([3])));
    assert!((100..=140).contains(&before[1]), "the Part is lit at half: {}", before[1]);
    // Only opaque pixels: the renderer lights half-transparent texels differently from the picture it made of them,
    // so soft edges differ under a dim light (measured: green 253 against 128 at ambient 0.5).
    for (index, (kept, before)) in kept.iter().zip(&before).enumerate().filter(|(index, _)| before[index / 4 * 4 + 3] == 255) {
        assert!(kept.abs_diff(*before) <= 2, "channel {index}: {kept} != {before}");
    }
    assert_eq!(session.snapshot()["scene"]["ambientLight"], json!([0.5, 0.5, 0.5]), "the light is put back");
}

#[test]
fn sketch_snapshot_defaults_to_a_quarter_opacity_and_shrinks_large_puppets() {
    let dir = temp_dir("sketch-large");
    let mut session = sketch_session(&dir);
    ok(&mut session, json!({"op": "sketch.snapshot"}));
    assert_eq!(find(&session.snapshot()["model"]["nodes"], 3).unwrap()["opacity"], 0.25);
    let again = ok(&mut session, json!({"op": "sketch.snapshot"}));
    assert_eq!((again["width"].as_u64(), again["height"].as_u64()), (Some(10), Some(9)), "a snapshot does not grow the next one");
    ok(&mut session, json!({"op": "undo"}));
    ok(&mut session, json!({"op": "undo"}));

    // 6000 units wide, 7200 with the margin: drawn at 4096 pixels, cropped to the drawn part and scaled back up.
    let mut wide = part(2, "Wide", 0, json!([]));
    wide["mesh"]["verts"] = json!([-3000.0, -1.0, -3000.0, 1.0, 3000.0, -1.0, 3000.0, 1.0]);
    let path = write_inp(&dir, "w.inp", &model(vec![wide], vec![], json!({})), &[png(1, 1, &[[9, 9, 9, 255]])]);
    ok(&mut session, json!({"op": "open", "path": path}));
    let result = ok(&mut session, json!({"op": "sketch.snapshot"}));
    assert_eq!(result["height"].as_u64(), Some(1));
    let snapshot = session.snapshot();
    let scale = &find(&snapshot["model"]["nodes"], 3).unwrap()["transform"]["scale"];
    assert!((result["width"].as_f64().unwrap() * 7200.0 / 4096.0 - 6000.0).abs() < 4.0, "covers the drawn 6000 units");
    assert!((scale[0].as_f64().unwrap() - 7200.0 / 4096.0).abs() < 1e-9, "{scale}");
    assert!((scale[1].as_f64().unwrap() - 2.4).abs() < 1e-9, "{scale}");
    let trans = &find(&snapshot["model"]["nodes"], 3).unwrap()["transform"]["trans"];
    assert!(trans[0].as_f64().unwrap().abs() < 2.0 && trans[1].as_f64().unwrap().abs() < 2.0, "centred on the puppet: {trans}");
}

#[test]
fn sketch_snapshot_refuses_bad_opacity_and_an_empty_puppet() {
    let dir = temp_dir("sketch-errors");
    let mut session = sketch_session(&dir);
    assert_eq!(fail(&mut session, json!({"op": "sketch.snapshot", "opacity": 1.5})), "opacity must be 0 to 1");
    assert_eq!(fail(&mut session, json!({"op": "sketch.snapshot", "opacity": "x"})), "opacity must be 0 to 1");
    ok(&mut session, json!({"op": "new"}));
    assert_eq!(fail(&mut session, json!({"op": "sketch.snapshot"})), "The puppet draws nothing to keep");
    assert_eq!(session.snapshot()["model"]["nodes"]["children"], Value::Null, "nothing was added");
}

// ------------------------------------------------------------------------------------ tools

#[test]
fn regenerate_node_ids_renumbers_nodes_and_follows_references() {
    let dir = temp_dir("regen");
    let mut masked = part(2, "Face", 0, json!([part(3, "Eye", 0, json!([]))]));
    masked["masks"] = json!([{"source": 3, "mode": "Mask"}]);
    let path = write_inp(&dir, "r.inp", &model(vec![masked], vec![parameter(4, "Blink", 3)], json!({})), &[png(1, 1, &[[1, 1, 1, 255]])]);
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": path}));
    let result = ok(&mut session, json!({"op": "tool.regenerate_node_ids"}));
    assert_eq!(result, json!({"regenerated": 3}));
    // Children first, root last, each taking the lowest id nothing uses: 3 -> 5, 2 -> 6, 1 -> 7.
    let snapshot = session.snapshot();
    let root = &snapshot["model"]["nodes"];
    assert_eq!(root["uuid"], 7);
    assert_eq!(root["children"][0]["uuid"], 6);
    assert_eq!(root["children"][0]["children"][0]["uuid"], 5);
    assert_eq!(root["children"][0]["masks"][0]["source"], 5);
    assert_eq!(snapshot["model"]["param"][0]["uuid"], 4, "parameters keep their ids");
    assert_eq!(snapshot["model"]["param"][0]["bindings"][0]["node"], 5);
    assert_eq!(snapshot["modified"], true);
    ok(&mut session, json!({"op": "undo"}));
    assert_eq!(session.snapshot()["model"]["nodes"]["uuid"], 1);
    // The full repair of D is the same regeneration.
    assert_eq!(ok(&mut session, json!({"op": "tool.repair"})), json!({"regenerated": 3}));
}

#[test]
fn fake_layer_names_use_the_part_names() {
    let dir = temp_dir("fake");
    let path = write_inp(&dir, "f.inp", &model(vec![part(2, "Face", 0, json!([part(3, "Eye L", 0, json!([]))]))], vec![], json!({})), &[png(1, 1, &[[1, 1, 1, 255]])]);
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": path}));
    assert_eq!(ok(&mut session, json!({"op": "tool.generate_fake_layer_names"})), json!({"changed": true}));
    let snapshot = session.snapshot();
    assert_eq!(snapshot["model"]["nodes"]["children"][0]["psdLayerPath"], "/Face");
    assert_eq!(snapshot["model"]["nodes"]["children"][0]["children"][0]["psdLayerPath"], "/Eye L");
    assert_eq!(ok(&mut session, json!({"op": "tool.generate_fake_layer_names"})), json!({"changed": false}));
}

#[test]
fn premultiply_and_rebleed_rewrite_the_texture_slots() {
    let dir = temp_dir("textures");
    let straight = png(2, 1, &[[200, 100, 50, 128], [10, 10, 10, 0]]);
    let strip = png(3, 1, &[[255, 0, 0, 255], [9, 9, 9, 0], [7, 7, 7, 0]]);
    let path = write_inp(&dir, "t.inp", &model(vec![part(2, "A", 0, json!([])), part(3, "B", 1, json!([]))], vec![], json!({})), &[straight, strip]);
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": path}));

    assert_eq!(ok(&mut session, json!({"op": "tool.premultiply_textures"})), json!({"textures": 2, "changed": true}));
    assert_eq!(texture_pixels(&mut session, 0), vec![100, 50, 25, 128, 0, 0, 0, 0]);

    // Bleeding spreads colour one ring per pass into the fully transparent pixels and keeps alpha 0.
    assert_eq!(ok(&mut session, json!({"op": "tool.rebleed_textures"})), json!({"textures": 2, "changed": true}));
    assert_eq!(texture_pixels(&mut session, 1), vec![255, 0, 0, 255, 255, 0, 0, 0, 255, 0, 0, 0]);
    assert_eq!(texture_pixels(&mut session, 0), vec![100, 50, 25, 128, 100, 50, 25, 0]);
    assert_eq!(ok(&mut session, json!({"op": "tool.rebleed_textures"})), json!({"textures": 2, "changed": false}));

    ok(&mut session, json!({"op": "undo"}));
    ok(&mut session, json!({"op": "undo"}));
    assert_eq!(texture_pixels(&mut session, 0), vec![200, 100, 50, 128, 10, 10, 10, 0]);
}

#[test]
fn import_session_data_copies_the_tracking_bindings() {
    let dir = temp_dir("session-data");
    let plain = write_inp(&dir, "plain.inp", &model(vec![], vec![], json!({})), &[]);
    let mut source = Session::new();
    ok(&mut source, json!({"op": "new"}));
    ok(&mut source, json!({"op": "extdata.set", "key": "com.inochi2d.inochi-session.bindings", "base64": "AQID"}));
    let with_data = dir.join("with.inx");
    ok(&mut source, json!({"op": "save", "path": with_data.to_str().unwrap()}));

    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": plain.clone()}));
    assert_eq!(fail(&mut session, json!({"op": "tool.import_session_data", "path": plain})), "There was no Inochi Session data to import!");
    let result = ok(&mut session, json!({"op": "tool.import_session_data", "path": with_data.to_str().unwrap()}));
    assert_eq!(result, json!({"key": "com.inochi2d.inochi-session.bindings", "bytes": 3}));
    let stored = ok(&mut session, json!({"op": "extdata.get", "key": "com.inochi2d.inochi-session.bindings"}));
    assert_eq!(stored["base64"], "AQID");
}

// ------------------------------------------------------------------------------------ MCP surface

fn rpc(session: &mut Session, id: u64, method: &str, params: Value) -> Value {
    let response = session.handle_mcp(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap();
    assert!(response.get("error").is_none(), "{method}: {response}");
    response["result"].clone()
}

fn read_json(session: &mut Session, uri: &str) -> Value {
    let result = rpc(session, 1, "resources/read", json!({"uri": uri}));
    serde_json::from_str(result["contents"][0]["text"].as_str().unwrap()).unwrap()
}

/// MCP allows only an object as `structuredContent`; a validating client (Claude Code) refused `nodes_list`
/// while its list was put there (found by the real agent run of #99), so a list answer is text only.
#[test]
fn structured_content_is_present_for_an_object_answer_and_absent_for_a_list() {
    let mut session = Session::new();
    session.handle_request(&json!({"op": "new"}));
    let listed = call(&mut session, "nodes_list", json!({}));
    assert!(listed.get("structuredContent").is_none(), "{listed}");
    assert_eq!(serde_json::from_str::<Value>(listed["content"][0]["text"].as_str().unwrap()).unwrap()[0]["name"], "Root");
    let summary = call(&mut session, "history_summary", json!({}));
    assert!(summary["structuredContent"].is_object(), "{summary}");
    assert!(summary["structuredContent"]["revision"].is_number(), "{summary}");
}

fn call(session: &mut Session, name: &str, arguments: Value) -> Value {
    rpc(session, 2, "tools/call", json!({"name": name, "arguments": arguments}))
}

fn open_sample(session: &mut Session, label: &str) {
    let dir = temp_dir(label);
    let mut face = part(2, "Face", 0, json!([]));
    face["children"] = json!([]);
    let path = write_inp(&dir, "m.inp", &model(vec![face], vec![parameter(10, "Blink", 2)], json!({})), &[png(1, 1, &[[1, 1, 1, 255]])]);
    ok(session, json!({"op": "open", "path": path}));
}

#[test]
fn resources_list_and_read_use_the_soul2d_namespace() {
    let mut session = Session::new();
    open_sample(&mut session, "res");
    let listed = rpc(&mut session, 1, "resources/list", json!({}));
    let uris: Vec<&str> = listed["resources"].as_array().unwrap().iter().map(|r| r["uri"].as_str().unwrap()).collect();
    for expected in [
        "resource://soul2d/document/state",
        "resource://soul2d/resources/find?selector=*",
        "resource://soul2d/guides/resources",
        "resource://soul2d/guides/selectors",
        "resource://soul2d/guides/find",
        "resource://soul2d/skill/operate-soul2d",
        "resource://soul2d/skill/operate-soul2d/families/editor-state",
        "resource://soul2d/skill/operate-soul2d/families/session",
        "resource://soul2d/resources/1",
        "resource://soul2d/resources/2",
        "resource://soul2d/resources/10",
        "resource://soul2d/bindings/get?parameter=10&target=2&name=transform.t.x",
        "resource://soul2d/textures/0",
    ] {
        assert!(uris.contains(&expected), "{expected} in {uris:?}");
    }
    assert!(!uris.contains(&"resource://soul2d/document/animation-source"), "only CAN3 documents list it");

    let state = read_json(&mut session, "resource://soul2d/document/state");
    assert_eq!(state["documentFormat"], "inx");
    assert_eq!((state["modified"].clone(), state["canUndo"].clone(), state["canRedo"].clone()), (json!(false), json!(false), json!(false)));
    assert_eq!(state["motion3"], Value::Null);
    assert!(state.get("model").is_none(), "the state resource carries no model payload");
    assert_eq!(state["defaultFormLocked"], false);
    let locked = call(&mut session, "default_form_lock_set", json!({"locked": true}));
    assert!(locked.get("isError").is_none_or(|error| error == false), "{locked}");
    let revision = state["revision"].clone();
    let locked = read_json(&mut session, "resource://soul2d/document/state");
    let flags = (locked["defaultFormLocked"].clone(), locked["revision"].clone());
    assert_eq!(flags, (json!(true), revision.clone()), "no edit");
    let one = json!({"views": [{"id": 1, "active": true}], "active": 1, "synchronized": false});
    assert_eq!(locked["views"], one);
    let two = json!({"views": [{"id": 1, "active": false}, {"id": 2, "active": true}], "active": 2,
        "synchronized": false});
    for (tool, arguments, expected) in [
        ("view_add", json!({}), two.clone()),
        ("view_list", json!({}), two.clone()),
        ("view_select", json!({"id": 1}), json!({"views": [{"id": 1, "active": true}, {"id": 2, "active": false}],
            "active": 1, "synchronized": false})),
        ("view_sync_set", json!({"synchronized": true}), json!({"views": [{"id": 1, "active": true},
            {"id": 2, "active": false}], "active": 1, "synchronized": true})),
        ("view_sync_set", json!({"synchronized": false}), json!({"views": [{"id": 1, "active": true},
            {"id": 2, "active": false}], "active": 1, "synchronized": false})),
        ("view_remove", json!({"id": 2}), one.clone()),
    ] {
        let answer = call(&mut session, tool, arguments);
        assert!(answer.get("isError").is_none_or(|error| error == false), "{tool}: {answer}");
        assert_eq!(read_json(&mut session, "resource://soul2d/document/state")["views"], expected, "{tool}");
    }
    assert_eq!(read_json(&mut session, "resource://soul2d/document/state")["revision"], revision, "no view edit");

    let node = read_json(&mut session, "resource://soul2d/resources/2");
    assert_eq!((node["item"]["typeId"].as_str(), node["item"]["name"].as_str()), (Some("Part"), Some("Face")));
    let parameter = read_json(&mut session, "resource://soul2d/resources/10");
    assert_eq!((parameter["item"]["typeId"].as_str(), parameter["item"]["name"].as_str()), (Some("Parameter"), Some("Blink")));
    assert_eq!(read_json(&mut session, "resource://soul2d/resources/999"), json!({"item": null}));

    let binding = read_json(&mut session, "resource://soul2d/bindings/get?parameter=10&target=2&name=transform.t.x");
    assert_eq!(binding["item"]["typeId"], "Binding");
    assert_eq!(binding["item"]["parameter"], json!({"uuid": 10, "name": "Blink"}));
    assert_eq!(binding["item"]["target"], json!({"uuid": 2, "name": "Face", "typeId": "Part"}));
    assert_eq!(binding["item"]["interpolateMode"], "Linear");
    assert_eq!(binding["item"]["data"]["values"], json!([[0.0], [10.0]]));
    let missing = read_json(&mut session, "resource://soul2d/bindings/get?parameter=10&target=2&name=nope");
    assert_eq!(missing, json!({"item": null, "error": "Binding not found"}));
    let bad = read_json(&mut session, "resource://soul2d/bindings/get?parameter=x&target=2&name=a");
    assert_eq!(bad["error"], "Binding read parameter and target must be integer UUIDs");
    let absent = read_json(&mut session, "resource://soul2d/bindings/get?parameter=11&target=2&name=a");
    assert_eq!(absent["error"], "Parameter not found: 11");

    let found = read_json(&mut session, "resource://soul2d/resources/find?selector=Part");
    assert_eq!(found["items"][0]["uuid"], 2);
}

#[test]
fn guides_and_prompts_carry_the_d_texts() {
    let mut session = Session::new();
    let skill = rpc(&mut session, 1, "resources/read", json!({"uri": "resource://soul2d/skill/operate-soul2d"}));
    assert_eq!(skill["contents"][0]["mimeType"], "text/markdown");
    assert!(skill["contents"][0]["text"].as_str().unwrap().contains("name: operate-soul2d"));
    let guide = rpc(&mut session, 1, "resources/read", json!({"uri": "resource://soul2d/guides/find"}));
    assert_eq!(guide["contents"][0]["mimeType"], "text/markdown");
    let text = guide["contents"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("# Find Guide\n\nEndpoint: `resource://soul2d/resources/find?selector=...`"));
    assert!(text.contains("- part named Eye: `resource://soul2d/resources/find?selector=Part.%22Eye%22`"));
    let selectors = rpc(&mut session, 1, "resources/read", json!({"uri": "resource://soul2d/guides/selectors"}));
    assert!(selectors["contents"][0]["text"].as_str().unwrap().contains("- `A > B` child combinator"));

    let listed = rpc(&mut session, 2, "prompts/list", json!({}));
    let names: Vec<&str> = listed["prompts"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["resources/find", "resources/get", "selectors/guide", "resources/guide", "tools/guide"]);
    let prompt = rpc(&mut session, 3, "prompts/get", json!({"name": "selectors/guide"}));
    assert_eq!(prompt["description"], "Selector syntax");
    assert_eq!(prompt["messages"][0]["role"], "assistant");
    let text = prompt["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.starts_with("Selector Syntax (soul2d.core.selector)\n\nUsed by:\n- resource://soul2d/resources/find?selector=..."));
    assert!(text.contains("- Binding:active     → only active bindings for the armed parameter."));
    let tools = rpc(&mut session, 4, "prompts/get", json!({"name": "tools/guide"}));
    assert!(tools["messages"][0]["content"]["text"].as_str().unwrap().starts_with("Tools Input & Context"));
    let unknown = session.handle_mcp(&json!({"jsonrpc": "2.0", "id": 5, "method": "prompts/get", "params": {"name": "nope"}})).unwrap();
    assert_eq!(unknown["error"]["code"], -32602);
    let init = rpc(&mut session, 6, "initialize", json!({}));
    assert!(init["capabilities"]["prompts"].is_object());
}

#[test]
fn d_command_names_reach_the_same_operations() {
    let mut session = Session::new();
    let listed = rpc(&mut session, 1, "tools/list", json!({}));
    let names: Vec<&str> = listed["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in [
        "Live2DCommand_AuthoringInspect", "Live2DCommand_ParameterSet", "Motion3Command_SetPoint", "Motion3Command_Advance",
        "FileCommand_OpenFile", "FileCommand_SaveFile", "FileCommand_ImportINP", "FileCommand_ImportImageFolder",
        "FileCommand_MergeINP", "FileCommand_MergeImageFiles", "ToolCommand_RegenerateNodeIDs", "ToolCommand_RebleedTexture",
        "ViewCommand_SaveScreenshot", "ViewportCommand_ResetPhysics", "ViewportCommand_ResetParameters",
        "CommandScopeCommand_GetCurrentCommandScope", "import_inp", "tool_repair",
    ] {
        assert!(names.contains(&expected), "{expected}");
    }

    let dir = temp_dir("aliases");
    let folder = dir.join("imgs");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("p.png"), png(2, 2, &[[5, 5, 5, 255]; 4])).unwrap();
    let imported = call(&mut session, "FileCommand_ImportImageFolder", json!({"path": folder.to_str().unwrap()}));
    assert_eq!(imported["isError"], false);
    assert_eq!(session.snapshot()["model"]["nodes"]["children"][0]["name"], "p");

    let saved = dir.join("out.inx");
    let result = call(&mut session, "FileCommand_SaveFile", json!({"file": saved.to_str().unwrap()}));
    assert_eq!(result["isError"], false);
    assert!(saved.exists());
    call(&mut session, "FileCommand_OpenFile", json!({"file": saved.to_str().unwrap()}));
    assert_eq!(session.snapshot()["modified"], false);

    let missing = call(&mut session, "FileCommand_OpenFile", json!({}));
    assert_eq!(missing["isError"], true);
    assert_eq!(missing["content"][0]["text"], "missing required argument(s) for FileCommand_OpenFile: file");

    let repaired = call(&mut session, "ToolCommand_RegenerateNodeIDs", json!({}));
    assert_eq!(repaired["structuredContent"], json!({"regenerated": 2}));

    let extra = dir.join("extra.png");
    fs::write(&extra, png(1, 1, &[[0, 0, 0, 255]])).unwrap();
    let parent = session.snapshot()["model"]["nodes"]["children"][0]["uuid"].clone();
    let merged = call(&mut session, "FileCommand_MergeImageFiles",
        json!({"paths": extra.to_str().unwrap(), "context": {"nodes": [parent.clone()]}}));
    assert_eq!(merged["structuredContent"]["parent"], parent);

    let shot = dir.join("shot.jpg");
    let taken = call(&mut session, "ViewCommand_SaveScreenshot", json!({"filename": shot.to_str().unwrap(), "size": 64}));
    assert_eq!(taken["isError"], false, "{taken}");
    let written = dir.join("shot.png");
    assert_eq!(taken["structuredContent"]["path"], written.to_str().unwrap());
    let size = image::image_dimensions(&written).unwrap();
    assert_eq!(size.0.max(size.1), 64);
}

#[test]
fn motion3_set_point_flattens_changes_and_rejects_loose_fields() {
    let mut session = Session::new();
    // The alias fails before any operation runs when the arguments do not have D's shape.
    let loose = call(&mut session, "Motion3Command_SetPoint",
        json!({"name": "m", "curve": 0, "point": 0, "changes": {}, "time": 1.0}));
    assert_eq!(loose["content"][0]["text"], "Put Motion3 point time/value inside changes");
    let unknown = call(&mut session, "Motion3Command_SetPoint",
        json!({"name": "m", "curve": 0, "point": 0, "changes": {"tension": 1.0}}));
    assert_eq!(unknown["content"][0]["text"], "Unknown Motion3 point change: tension");
    let none = call(&mut session, "Motion3Command_SetPoint", json!({"name": "m", "curve": 0, "point": 0}));
    assert_eq!(none["content"][0]["text"], "missing required argument(s) for Motion3Command_SetPoint: changes");
    let flat = call(&mut session, "Motion3Command_SetPoint", json!({"name": "m", "curve": 0, "point": 0, "changes": 1}));
    assert_eq!(flat["content"][0]["text"], "Motion3 point changes must be an object with time and/or value");
    // A well-formed call reaches the operation, which reports the missing clip.
    let missing = call(&mut session, "Motion3Command_SetPoint",
        json!({"name": "m", "curve": 0, "point": 0, "changes": {"value": 0.5}}));
    assert_eq!(missing["isError"], true);
    assert!(missing["content"][0]["text"].as_str().unwrap().contains("Motion3"), "{missing}");
}

#[test]
fn viewport_resets_and_the_scope_tool() {
    let mut session = Session::new();
    open_sample(&mut session, "viewport");
    ok(&mut session, json!({"op": "value.set", "id": 10, "value": [1.0, 0.0]}));
    assert_eq!(session.snapshot()["values"]["10"], json!([1.0, 0.0]));
    call(&mut session, "ViewportCommand_ResetParameters", json!({}));
    assert_eq!(session.snapshot()["values"]["10"], json!([0.0, 0.0]));
    let physics = call(&mut session, "ViewportCommand_ResetPhysics", json!({}));
    assert_eq!(physics["isError"], false);

    let scope = call(&mut session, "CommandScopeCommand_GetCurrentCommandScope", json!({}));
    let body = &scope["structuredContent"];
    assert_eq!(body["scope"]["id"], "normal");
    assert_eq!(body["scopeStack"], json!([body["scope"].clone()]));
    assert_eq!(body["lifecycle"], json!({"finish": [], "cancel": []}));
    let tools: Vec<&str> = body["availableCommands"].as_array().unwrap().iter().map(|c| c["tool"].as_str().unwrap()).collect();
    assert!(tools.contains(&"FileCommand_ImportINP") && tools.windows(2).all(|pair| pair[0] <= pair[1]), "sorted tool names");
    assert_eq!(body["toolSpecification"], "Use tools/list to read the input schema for each returned tool name.");
}

#[test]
fn the_editor_state_resource_says_there_is_no_editor_and_keeps_the_session_fields() {
    let uri = "resource://soul2d/editor/state";
    let mut session = Session::new();
    assert_eq!(
        read_json(&mut session, uri),
        json!({"editor": false, "document": {"path": null, "format": "inx", "modified": false, "revision": 0}})
    );
    open_sample(&mut session, "editor-state");
    let listed = rpc(&mut session, 1, "resources/list", json!({}));
    assert!(listed["resources"].as_array().unwrap().iter().any(|resource| resource["uri"] == uri), "{listed}");
    let opened = read_json(&mut session, uri);
    assert_eq!(opened["editor"], false);
    assert!(opened["document"]["path"].as_str().unwrap().ends_with("m.inp"), "{opened}");
    assert_eq!(opened["document"]["modified"], false);
    ok(&mut session, json!({"op": "node.rename", "id": 2, "name": "Head"}));
    let edited = read_json(&mut session, uri);
    assert_eq!(edited["document"]["modified"], true);
    assert!(edited["document"]["revision"].as_u64() > opened["document"]["revision"].as_u64());
    // The state only a window holds is absent, not empty: no selection, camera or tool is made up.
    let keys: Vec<&String> = edited.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["document", "editor"]);
}

#[test]
fn editor_tools_are_listed_and_refused_without_a_window() {
    let mut session = Session::new();
    open_sample(&mut session, "editor-tools");
    let before = session.snapshot();
    let listed = rpc(&mut session, 1, "tools/list", json!({}));
    for (name, arguments) in [
        ("editor_quit", json!({})),
        ("editor_selection_set", json!({"nodes": [2]})),
        ("editor_armed_set", json!({"parameter": 10})),
        ("editor_camera_set", json!({"centre": [0.0, 0.0], "zoom": 1.0})),
        ("editor_camera_fit", json!({"nodes": [2]})),
        ("editor_camera_sync", json!({})),
        ("editor_parameters_set", json!({"filter":"Blink"})),
        ("editor_native_parameters_set", json!({"filter":"Param"})),
        ("editor_tool_set", json!({"tool": "brush"})),
        ("editor_recovery_keep", json!({})),
        ("editor_recovery_restore", json!({"id": "rust-recovery-1-2.inx"})),
        ("editor_settings_set", json!({"key": "DarkMode", "value": true})),
        ("editor_shortcut_set", json!({"command": "EditCommand.Undo", "shortcut": "Ctrl+Z"})),
        ("editor_grid_set", json!({"show": true})),
        ("editor_viewport_set", json!({"vertices":false,"background":[255,0,37],"partSort":"size_sort"})),
        ("editor_about_set", json!({"open":true})),
        ("editor_about_set", json!({"open":false})),
        ("editor_welcome_set", json!({"open":true})),
        ("editor_welcome_set", json!({"open":false})),
        ("editor_command_browser_set", json!({"open":true})),
        ("editor_command_browser_set", json!({"open":false})),
        ("editor_command_browser_set", json!({"open":null,"extra":1})),
        ("editor_texture_viewer_set", json!({"open":true})),
        ("editor_texture_viewer_set", json!({"open":false})),
        ("editor_texture_viewer_set", json!({"open":null,"extra":1})),
        ("editor_address_dialog", json!({"action":"open","target":"configuration"})),
        ("editor_address_dialog", json!({"action":"copy"})),
        ("editor_address_dialog", json!({"action":"close"})),
        ("editor_guides_set", json!({"lines": []})),
        ("editor_physics_preview_set", json!({"enabled":true})),
        ("editor_solo_set", json!({"show": true})),
        ("editor_onion_set", json!({"onion": true})),
        ("editor_playback_set", json!({"lightweight": true})),
        ("editor_timeline_zoom_set", json!({"zoom":2.5})),
        ("editor_timeline_zoom_set", json!({"zoom":null,"extra":true})),
        ("editor_panel_set", json!({"panel":"outline","visible":false})),
        // Malformed arguments get the same answer: there is nothing here they could apply to.
        ("editor_camera_set", json!({"zoom": "wide", "extra": 1})),
    ] {
        let tool = listed["tools"].as_array().unwrap().iter().find(|tool| tool["name"] == name).unwrap_or_else(|| panic!("{name} is listed"));
        assert_eq!(tool["inputSchema"]["additionalProperties"], false, "{name}");
        assert!(tool["description"].as_str().unwrap().starts_with("Action: "), "{name}");
        let refused = call(&mut session, name, arguments);
        assert_eq!(refused["isError"], true, "{name}: {refused}");
        assert_eq!(refused["content"][0]["text"], format!("{name} needs the editor: this host has no window"));
    }
    // The two readers answer the same way; their descriptions say "Read:", not "Action:".
    for name in ["editor_recovery_list", "editor_settings_get"] {
        let tool = listed["tools"].as_array().unwrap().iter().find(|tool| tool["name"] == name).unwrap_or_else(|| panic!("{name} is listed"));
        assert_eq!(tool["inputSchema"]["additionalProperties"], false, "{name}");
        assert!(tool["description"].as_str().unwrap().starts_with("Read: "), "{name}");
        let refused = call(&mut session, name, json!({}));
        assert_eq!(refused["isError"], true, "{name}: {refused}");
        assert_eq!(refused["content"][0]["text"], format!("{name} needs the editor: this host has no window"));
    }
    assert_eq!(session.snapshot(), before, "a refused editor tool leaves the session untouched");
}

// ------------------------------------------------------------------------------------ crash dump

#[test]
fn timestamps_follow_the_iso_basic_shape() {
    use std::time::{Duration, UNIX_EPOCH};
    assert_eq!(soul2d_core::crashdump::timestamp(UNIX_EPOCH), "19700101T000000.000000");
    // 2024-02-29 13:14:15.000250 UTC
    let at = UNIX_EPOCH + Duration::new(1_709_212_455, 250_000);
    assert_eq!(soul2d_core::crashdump::timestamp(at), "20240229T131415.000250");
}

#[test]
fn reports_have_d_sections_and_hide_the_bearer_token() {
    let args: Vec<String> = ["soul2d-core", "--bearer", "s3cret", "--mcp-port", "0"].iter().map(|a| a.to_string()).collect();
    assert_eq!(
        soul2d_core::crashdump::format_report(&args, "boom"),
        "=== Args State ===\nsoul2d-core,\n--bearer,\n<redacted>,\n--mcp-port,\n0\n\n=== Exception ===\nboom"
    );
}

#[test]
fn a_panic_writes_a_report_to_the_dump_directory() {
    // The deliberate panic below marks this thread as failed, so the registered scratch directory is kept.
    let root = temp_dir("crash");
    let dir = root.join("nested");
    soul2d_core::crashdump::install_in(Some(dir.clone()), "soul2d-test-crashdump");
    let caught = std::panic::catch_unwind(|| {
        panic!("simulated failure");
    });
    assert!(caught.is_err());
    let reports: Vec<PathBuf> = fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().path()).collect();
    assert_eq!(reports.len(), 1, "{reports:?}");
    let name = reports[0].file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with("soul2d-test-crashdump-") && name.ends_with(".txt"), "{name}");
    let text = fs::read_to_string(&reports[0]).unwrap();
    assert!(text.starts_with("=== Args State ===\n"), "{text}");
    assert!(text.contains("\n\n=== Exception ===\npanic at tests/app_shell.rs:"), "{text}");
    assert!(text.contains("simulated failure"), "{text}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rebleed_does_not_change_what_is_drawn() {
    let dir = temp_dir("halo");
    // An opaque red texel between two fully transparent ones.
    let strip = png(4, 4, &[[[0, 0, 0, 0]; 4], [[0, 0, 0, 0], [255, 0, 0, 255], [255, 0, 0, 255], [0, 0, 0, 0]], [[0, 0, 0, 0], [255, 0, 0, 255], [255, 0, 0, 255], [0, 0, 0, 0]], [[0, 0, 0, 0]; 4]].concat());
    let mut back = part(3, "Back", 1, json!([]));
    back["zsort"] = json!(1.0);
    let nodes = vec![part(2, "A", 0, json!([])), back];
    let path = write_inp(&dir, "h.inp", &model(nodes, vec![], json!({})), &[strip, png(1, 1, &[[0, 0, 255, 255]])]);
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": path}));
    let render = |session: &mut Session| ok(session, json!({"op": "render", "width": 64, "height": 64, "bounds": [-2, -2, 2, 2]}));
    let before = render(&mut session);
    assert_eq!(ok(&mut session, json!({"op": "tool.rebleed_textures"}))["changed"], true);
    assert_eq!(render(&mut session), before);
}

// ------------------------------------------------------------------------------------ texture.set

/// A session with texture slot 0 = 2x1 (used by Part A and Part B) and slot 1 = 1x1 (Part C).
fn texture_session(label: &str) -> (Session, PathBuf) {
    let dir = temp_dir(label);
    let nodes = vec![part(2, "A", 0, json!([])), part(3, "B", 0, json!([])), part(4, "C", 1, json!([]))];
    let textures = [png(2, 1, &[[1, 2, 3, 255], [4, 5, 6, 255]]), png(1, 1, &[[7, 8, 9, 255]])];
    let path = write_inp(&dir, "ts.inp", &model(nodes, vec![], json!({})), &textures);
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": path}));
    (session, dir)
}

fn texture_b64(session: &mut Session, slot: u32) -> String {
    ok(session, json!({"op": "texture.get", "slot": slot}))["base64"].as_str().unwrap().to_owned()
}

fn history_count(session: &mut Session) -> u64 {
    ok(session, json!({"op": "history.summary"}))["count"].as_u64().unwrap()
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[test]
fn texture_set_reads_a_file_premultiplies_and_is_one_undo_step() {
    let (mut session, dir) = texture_session("set-path");
    let old = texture_b64(&mut session, 0);
    let file = dir.join("new.png");
    fs::write(&file, png(2, 1, &[[200, 100, 50, 128], [10, 20, 30, 0]])).unwrap();
    let steps = history_count(&mut session);

    let result = ok(&mut session, json!({"op": "texture.set", "slot": 0, "path": file.to_str().unwrap()}));
    assert_eq!(result, json!({"slot": 0, "width": 2, "height": 1, "changed": true}));
    // Straight alpha in, premultiplied out: 200 * 128 / 255 = 100.
    assert_eq!(texture_pixels(&mut session, 0), vec![100, 50, 25, 128, 0, 0, 0, 0]);
    assert_eq!(texture_pixels(&mut session, 1), vec![7, 8, 9, 255]);
    assert_eq!(history_count(&mut session), steps + 1);

    ok(&mut session, json!({"op": "undo"}));
    assert_eq!(texture_b64(&mut session, 0), old);
    ok(&mut session, json!({"op": "redo"}));
    assert_eq!(texture_pixels(&mut session, 0), vec![100, 50, 25, 128, 0, 0, 0, 0]);
}

#[test]
fn texture_set_reads_base64_and_keeps_premultiplied_input_as_is() {
    let (mut session, _dir) = texture_session("set-b64");
    let image = png(2, 1, &[[200, 100, 50, 128], [9, 9, 9, 255]]);
    let result = ok(&mut session, json!({"op": "texture.set", "slot": 0, "png_base64": b64(&image), "premultiplied": true}));
    assert_eq!(result["changed"], true);
    assert_eq!(texture_pixels(&mut session, 0), vec![200, 100, 50, 128, 9, 9, 9, 255]);
    // The same call again changes nothing and records no step.
    let steps = history_count(&mut session);
    let again = ok(&mut session, json!({"op": "texture.set", "slot": 0, "png_base64": b64(&image), "premultiplied": true}));
    assert_eq!(again, json!({"slot": 0, "width": 2, "height": 1, "changed": false}));
    assert_eq!(history_count(&mut session), steps);
    // As straight alpha the same image stores different pixels.
    let straight = ok(&mut session, json!({"op": "texture.set", "slot": 0, "png_base64": b64(&image)}));
    assert_eq!(straight["changed"], true);
    assert_eq!(texture_pixels(&mut session, 0), vec![100, 50, 25, 128, 9, 9, 9, 255]);
}

#[test]
fn texture_set_stores_tga_slots_in_their_own_format_and_reads_tga_files() {
    let dir = temp_dir("set-tga");
    let tga = |pixel: [u8; 4]| {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_raw(1, 1, pixel.to_vec()).unwrap().write_to(&mut out, image::ImageFormat::Tga).unwrap();
        out.into_inner()
    };
    let textures = [Texture { tag: 1, data: tga([1, 2, 3, 255]) }];
    let path = dir.join("tga.inp");
    fs::write(&path, inx_bytes(&model(vec![part(2, "A", 0, json!([]))], vec![], json!({})), &textures).unwrap()).unwrap();
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": path.to_str().unwrap()}));
    let file = dir.join("new.tga");
    fs::write(&file, tga([40, 50, 60, 255])).unwrap();
    ok(&mut session, json!({"op": "texture.set", "slot": 0, "path": file.to_str().unwrap()}));
    assert_eq!(ok(&mut session, json!({"op": "texture.get", "slot": 0}))["format"], "tga");
    let stored = base64::engine::general_purpose::STANDARD.decode(texture_b64(&mut session, 0)).unwrap();
    let decoded = image::load_from_memory_with_format(&stored, image::ImageFormat::Tga).unwrap().to_rgba8();
    assert_eq!(decoded.into_raw(), vec![40, 50, 60, 255]);
}

#[test]
fn texture_set_refuses_a_different_size_unless_allow_resize() {
    let (mut session, _dir) = texture_session("set-resize");
    let old = texture_b64(&mut session, 0);
    let bigger = b64(&png(3, 2, &[[10, 20, 30, 255]; 6]));
    let message = fail(&mut session, json!({"op": "texture.set", "slot": 0, "png_base64": bigger}));
    assert_eq!(message, "texture slot 0 is 2x1 but the image is 3x2; pass allowResize to change the size");
    assert_eq!(texture_b64(&mut session, 0), old);

    let result = ok(&mut session, json!({"op": "texture.set", "slot": 0, "png_base64": bigger, "allowResize": true}));
    assert_eq!(result, json!({"slot": 0, "width": 3, "height": 2, "changed": true}));
    let size = ok(&mut session, json!({"op": "texture.get", "slot": 0}));
    assert_eq!((size["width"].clone(), size["height"].clone()), (json!(3), json!(2)));
    ok(&mut session, json!({"op": "undo"}));
    assert_eq!(texture_b64(&mut session, 0), old);
}

#[test]
fn texture_set_refuses_bad_input_and_leaves_the_document_unchanged() {
    let (mut session, dir) = texture_session("set-refuse");
    let one = b64(&png(2, 1, &[[1, 1, 1, 255]; 2]));
    let file = dir.join("ok.png");
    fs::write(&file, png(2, 1, &[[1, 1, 1, 255]; 2])).unwrap();
    let path = file.to_str().unwrap();
    // Over the 16-megapixel texture cap although `allowResize` lets the size change, and over the side cap.
    let huge = b64(&png(8192, 2049, &vec![[0, 0, 0, 0]; 8192 * 2049]));
    let wide = b64(&png(8193, 1, &vec![[0, 0, 0, 0]; 8193]));
    let not_png = b64(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]);
    let both = "texture.set needs exactly one of path (non-empty) or png_base64";
    let cases = [
        (json!({"op": "texture.set", "slot": 5, "png_base64": one}), "texture slot 5 is out of range (2 textures)"),
        (json!({"op": "texture.set", "slot": -1, "png_base64": one}), "slot must be a nonnegative integer"),
        (json!({"op": "texture.set", "png_base64": one}), "slot must be a nonnegative integer"),
        (json!({"op": "texture.set", "slot": 0}), both),
        (json!({"op": "texture.set", "slot": 0, "path": ""}), both),
        (json!({"op": "texture.set", "slot": 0, "path": path, "png_base64": one}), both),
        (json!({"op": "texture.set", "slot": 0, "png_base64": "not base64!"}), "png_base64 is not valid base64"),
        (json!({"op": "texture.set", "slot": 0, "png_base64": 7}), "png_base64 must be a string"),
        (json!({"op": "texture.set", "slot": 0, "png_base64": one, "premultiplied": "yes"}), "premultiplied must be a boolean"),
        (json!({"op": "texture.set", "slot": 0, "png_base64": not_png}), "Could not load png_base64: "),
        (json!({"op": "texture.set", "slot": 0, "png_base64": huge, "allowResize": true}), "Could not load png_base64: 8192x2049 is not a supported texture size"),
        (json!({"op": "texture.set", "slot": 0, "png_base64": wide, "allowResize": true}), "Could not load png_base64: 8193x1 is not a supported texture size"),
        (json!({"op": "texture.set", "slot": 0, "png_base64": "A".repeat(8 * 1024 * 1024 + 4)}), "png_base64 is too large; use path for big images"),
        (json!({"op": "texture.set", "slot": 0, "path": dir.join("missing.png").to_str().unwrap()}), "Could not load missing.png: "),
        (json!({"op": "texture.set", "slot": 0, "path": dir.join("notes.txt").to_str().unwrap()}), "Invalid file type "),
    ];
    let old = [texture_b64(&mut session, 0), texture_b64(&mut session, 1)];
    let steps = history_count(&mut session);
    for (request, expected) in cases {
        let message = fail(&mut session, request.clone());
        assert!(message.starts_with(expected), "{request}: {message}");
        // Only the name the caller gave appears, never the directory.
        assert!(!message.contains(dir.to_str().unwrap()), "{message}");
    }
    assert_eq!([texture_b64(&mut session, 0), texture_b64(&mut session, 1)], old);
    assert_eq!(history_count(&mut session), steps);
}

#[test]
fn texture_set_changes_what_is_drawn() {
    let dir = temp_dir("set-render");
    let path = write_inp(&dir, "r.inp", &model(vec![part(2, "A", 0, json!([]))], vec![], json!({})), &[png(1, 1, &[[255, 0, 0, 255]])]);
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": path}));
    let render = |session: &mut Session| ok(session, json!({"op": "render", "width": 16, "height": 16, "bounds": [-2, -2, 2, 2]}));
    let before = render(&mut session);
    ok(&mut session, json!({"op": "texture.set", "slot": 0, "png_base64": b64(&png(1, 1, &[[0, 0, 255, 255]]))}));
    let after = render(&mut session);
    assert_ne!(after, before);
    ok(&mut session, json!({"op": "undo"}));
    assert_eq!(render(&mut session), before);
    ok(&mut session, json!({"op": "redo"}));
    assert_eq!(render(&mut session), after);
}

#[test]
fn texture_set_is_a_closed_mcp_tool() {
    let (mut session, _dir) = texture_session("set-mcp");
    let listed = session.handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    let tool = listed["result"]["tools"].as_array().unwrap().iter().find(|tool| tool["name"] == "texture_set").unwrap().clone();
    assert_eq!(tool["inputSchema"]["additionalProperties"], false);
    assert_eq!(tool["inputSchema"]["required"], json!(["slot"]));
    let applied = call(&mut session, "texture_set", json!({"slot": 1, "png_base64": b64(&png(1, 1, &[[1, 2, 3, 255]]))}));
    assert_eq!(applied["isError"], false, "{applied}");
    assert_eq!(applied["structuredContent"], json!({"slot": 1, "width": 1, "height": 1, "changed": true}));
    let refused = call(&mut session, "texture_set", json!({"slot": 1, "mask": "x"}));
    assert_eq!(refused["isError"], true);
}

#[test]
fn texture_export_and_replace_round_trip_changes_only_edited_slots_in_one_undo() {
    let (mut session, dir) = texture_session("export-replace");
    let out = dir.join("slots");
    fs::create_dir(&out).unwrap();
    let old = [texture_b64(&mut session, 0), texture_b64(&mut session, 1)];
    let steps = history_count(&mut session);

    let exported = ok(&mut session, json!({"op": "texture.export", "directory": out}));
    assert_eq!(exported["written"], json!(["slot-0.png", "slot-1.png"]));
    assert_eq!(exported["count"], 2);
    assert_eq!(history_count(&mut session), steps);
    assert_eq!([texture_b64(&mut session, 0), texture_b64(&mut session, 1)], old);
    let files = [fs::read(out.join("slot-0.png")).unwrap(), fs::read(out.join("slot-1.png")).unwrap()];
    for (slot, bytes) in files.iter().enumerate() {
        let decoded = image::load_from_memory(bytes).unwrap().to_rgba8().into_raw();
        assert_eq!(decoded, texture_pixels(&mut session, slot as u32));
    }
    let blocked = fail(&mut session, json!({"op": "texture.export", "directory": out}));
    assert!(blocked.contains("slot-0.png"), "{blocked}");
    assert!(!blocked.contains(out.to_str().unwrap()), "{blocked}");
    assert_eq!([fs::read(out.join("slot-0.png")).unwrap(), fs::read(out.join("slot-1.png")).unwrap()], files);
    let again = ok(&mut session, json!({"op": "texture.export", "directory": out, "overwrite": true}));
    assert_eq!(again["written"], json!(["slot-0.png", "slot-1.png"]));
    assert_eq!([fs::read(out.join("slot-0.png")).unwrap(), fs::read(out.join("slot-1.png")).unwrap()], files);
    assert_eq!(history_count(&mut session), steps);

    let same = ok(&mut session, json!({"op": "texture.replace", "directory": out}));
    assert_eq!(same, json!({"missing": [], "changed": [], "count": 2}));
    assert_eq!(history_count(&mut session), steps);
    assert_eq!([texture_b64(&mut session, 0), texture_b64(&mut session, 1)], old);
    let refused = call(&mut session, "texture_replace", json!({"directory": out, "mask": "x"}));
    assert_eq!(refused["isError"], true);
    assert_eq!(history_count(&mut session), steps);

    let slot0 = files[0].clone();
    fs::write(out.join("slot-0.png"), png(2, 2, &[[9, 9, 9, 255]; 4])).unwrap();
    let mut slot1 = image::load_from_memory(&files[1]).unwrap().to_rgba8();
    slot1.put_pixel(0, 0, image::Rgba([9, 8, 7, 255]));
    slot1.write_to(&mut fs::File::create(out.join("slot-1.png")).unwrap(), image::ImageFormat::Png).unwrap();
    let resized = fail(&mut session, json!({"op": "texture.replace", "directory": out}));
    assert!(resized.contains("slot-0.png"), "{resized}");
    assert_eq!(history_count(&mut session), steps);
    assert_eq!([texture_b64(&mut session, 0), texture_b64(&mut session, 1)], old);
    fs::write(out.join("slot-0.png"), &slot0).unwrap();
    fs::remove_file(out.join("slot-0.png")).unwrap();
    let replaced = ok(&mut session, json!({"op": "texture.replace", "directory": out}));
    assert_eq!(replaced, json!({"missing": ["slot-0.png"], "changed": [1], "count": 2}));
    assert_eq!(texture_pixels(&mut session, 0), vec![1, 2, 3, 255, 4, 5, 6, 255]);
    assert_eq!(texture_pixels(&mut session, 1), vec![9, 8, 7, 255]);
    assert_eq!(history_count(&mut session), steps + 1);
    let read = read_json(&mut session, "resource://soul2d/textures/1");
    assert_eq!(read["base64"], texture_b64(&mut session, 1));
    assert_eq!(read["format"], "png");
    ok(&mut session, json!({"op": "undo"}));
    assert_eq!([texture_b64(&mut session, 0), texture_b64(&mut session, 1)], old);
    assert_eq!(history_count(&mut session), steps + 1);

    let missing = fail(&mut session, json!({"op": "texture.export", "directory": dir.join("missing")}));
    assert!(missing.starts_with("texture.export could not resolve directory"), "{missing}");
    let file = fail(&mut session, json!({"op": "texture.replace", "directory": dir.join("ts.inp")}));
    assert_eq!(file, "texture.replace needs a directory");
    assert_eq!([texture_b64(&mut session, 0), texture_b64(&mut session, 1)], old);

    let listed = session.handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    let tool = listed["result"]["tools"].as_array().unwrap().iter().find(|tool| tool["name"] == "texture_export").unwrap();
    assert_eq!(tool["inputSchema"]["additionalProperties"], false);
    assert_eq!(tool["inputSchema"]["required"], json!(["directory"]));
}

#[test]
fn texture_export_replace_round_trip_keeps_a_tga_slot_without_premultiply() {
    let dir = temp_dir("export-tga");
    let tga = |pixel: [u8; 4]| {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_raw(1, 1, pixel.to_vec()).unwrap().write_to(&mut out, image::ImageFormat::Tga).unwrap();
        out.into_inner()
    };
    let pixel = [200, 100, 50, 128];
    let textures = [Texture { tag: 1, data: tga(pixel) }];
    let path = dir.join("tga.inp");
    fs::write(&path, inx_bytes(&model(vec![part(2, "A", 0, json!([]))], vec![], json!({})), &textures).unwrap()).unwrap();
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": path.to_str().unwrap()}));
    let out = dir.join("slots");
    fs::create_dir(&out).unwrap();
    ok(&mut session, json!({"op": "texture.export", "directory": out}));
    let decoded = image::load_from_memory(&fs::read(out.join("slot-0.png")).unwrap()).unwrap().to_rgba8().into_raw();
    assert_eq!(decoded, pixel);
    let steps = history_count(&mut session);
    let replaced = ok(&mut session, json!({"op": "texture.replace", "directory": out}));
    assert_eq!(replaced["changed"], json!([]));
    assert_eq!(history_count(&mut session), steps);
    let got = ok(&mut session, json!({"op": "texture.get", "slot": 0}));
    assert_eq!(got["format"], "tga");
    let stored = base64::engine::general_purpose::STANDARD.decode(got["base64"].as_str().unwrap()).unwrap();
    let decoded = image::load_from_memory_with_format(&stored, image::ImageFormat::Tga).unwrap().to_rgba8();
    assert_eq!(decoded.into_raw(), pixel);
}
