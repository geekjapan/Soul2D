mod common;

use serde_json::{json, Value};
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "soul2d-solo-test-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    common::scratch(path)
}

fn transform() -> Value {
    json!({"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]})
}

/// A white textured quad from (x0, y0) to (x1, y1).
fn quad(uuid: u64, [x0, y0, x1, y1]: [f64; 4]) -> Value {
    json!({
        "uuid": uuid, "name": format!("Quad {uuid}"), "type": "Part", "enabled": true, "zsort": 0.0,
        "transform": transform(), "children": [], "lockToRoot": false, "pinToMesh": false,
        "mesh": {"verts": [x0, y0, x1, y0, x1, y1, x0, y1],
                 "uvs": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
                 "indices": [0, 1, 2, 0, 2, 3], "origin": [0.0, 0.0]},
        "textures": [0], "blend_mode": "Normal", "tint": [1, 1, 1], "screenTint": [0, 0, 0],
        "emissionStrength": 1, "opacity": 1,
    })
}

/// Group 3 holds quads 5 (top left) and 6 (top right); quad 7 (bottom) is a sibling of the group.
fn open_fixture() -> Session {
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 255, 255, 255])))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let texture = png.into_inner();
    let group = json!({
        "uuid": 3, "name": "Group", "type": "Node", "enabled": true, "zsort": 0.0,
        "transform": transform(), "lockToRoot": false, "pinToMesh": false,
        "children": [quad(5, [0.0, 0.0, 4.0, 4.0]), quad(6, [4.0, 0.0, 8.0, 4.0])],
    });
    let model = json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
            "transform": transform(), "lockToRoot": false, "pinToMesh": false,
            "children": [group, quad(7, [0.0, 4.0, 8.0, 8.0])]},
        "param": [], "animations": {},
    });
    let raw = serde_json::to_vec(&model).unwrap();
    let mut file = b"TRNSRTS\0".to_vec();
    file.extend_from_slice(&(raw.len() as u32).to_be_bytes());
    file.extend_from_slice(&raw);
    file.extend_from_slice(b"TEX_SECT");
    file.extend_from_slice(&1u32.to_be_bytes());
    file.extend_from_slice(&(texture.len() as u32).to_be_bytes());
    file.push(0);
    file.extend_from_slice(&texture);
    let path = temp_dir().join("fixture.inx");
    fs::write(&path, file).unwrap();
    let mut session = Session::new();
    let opened = session.handle_request(&json!({"op": "open", "path": path.to_str().unwrap()}));
    assert_eq!(opened["succeeded"], true, "{opened}");
    session
}

/// The pixels in the top left, top right and bottom quad.
fn pixels(session: &mut Session, extra: Value) -> [[u8; 4]; 3] {
    let mut request = json!({"op": "render", "width": 8, "height": 8, "bounds": [0, 0, 8, 8]});
    for (key, value) in extra.as_object().unwrap() {
        request[key] = value.clone();
    }
    let response = session.handle_request(&request);
    assert_eq!(response["succeeded"], true, "{response}");
    let png = response["result"]["png"].as_str().unwrap().to_owned();
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, png).unwrap();
    let image = image::load_from_memory(&bytes).unwrap().into_rgba8();
    [(2, 2), (6, 2), (4, 6)].map(|(x, y)| image.get_pixel(x, y).0)
}

/// Which of the three quads have visible pixels.
fn drawn(session: &mut Session, solo: Option<Value>) -> [bool; 3] {
    let extra = solo.map_or(json!({}), |solo| json!({"solo": solo}));
    pixels(session, extra).map(|pixel| pixel[3] != 0)
}

#[test]
fn solo_renders_the_named_nodes_and_the_nodes_below_them() {
    let mut session = open_fixture();
    let before = session.snapshot();
    assert_eq!(drawn(&mut session, None), [true, true, true]);
    assert_eq!(drawn(&mut session, Some(Value::Null)), [true, true, true]);
    assert_eq!(drawn(&mut session, Some(json!([5]))), [true, false, false]);
    assert_eq!(drawn(&mut session, Some(json!([6, 7]))), [false, true, true]);
    assert_eq!(drawn(&mut session, Some(json!([3]))), [true, true, false], "a group brings its children");
    assert_eq!(drawn(&mut session, Some(json!([1]))), [true, true, true]);
    assert_eq!(drawn(&mut session, Some(json!([]))), [false, false, false]);
    for (solo, message) in [
        (json!([999]), "solo names no node with UUID 999"),
        (json!([5, 998, 999]), "solo names no node with UUID 998"),
        (json!(5), "solo must be an array of node UUIDs"),
        (json!([-1]), "solo must be an array of node UUIDs"),
        (json!(["5"]), "solo must be an array of node UUIDs"),
    ] {
        let response = session.handle_request(
            &json!({"op": "render", "width": 8, "height": 8, "bounds": [0, 0, 8, 8], "solo": solo}),
        );
        assert_eq!((response["succeeded"].clone(), response["message"].clone()), (json!(false), json!(message)));
    }
    // With a cover the whole picture is drawn under it and the solo nodes on top.
    let white = [255, 255, 255, 255];
    let covered = pixels(&mut session, json!({"solo": [5], "solo_cover": [0, 0, 0, 255]}));
    assert_eq!(covered, [white, [0, 0, 0, 255], [0, 0, 0, 255]]);
    let covered = pixels(&mut session, json!({"solo": [5], "solo_cover": [0, 0, 0, 0]}));
    assert_eq!(covered, [white; 3], "a clear cover leaves the picture as it is");
    let malformed = "solo_cover must be [r, g, b, a], each an integer from 0 to 255";
    for (extra, message) in [
        (json!({"solo_cover": [0, 0, 0, 255]}), "solo_cover needs solo"),
        (json!({"solo": [5], "solo_cover": [0, 0, 0]}), malformed),
        (json!({"solo": [5], "solo_cover": [0, 0, 0, 256]}), malformed),
        (json!({"solo": [5], "solo_cover": [0, 0, 0, 0, 0]}), malformed),
        (json!({"solo": [5], "solo_cover": "black"}), malformed),
    ] {
        let mut request = json!({"op": "render", "width": 8, "height": 8, "bounds": [0, 0, 8, 8]});
        for (key, value) in extra.as_object().unwrap() {
            request[key] = value.clone();
        }
        let response = session.handle_request(&request);
        assert_eq!((response["succeeded"].clone(), response["message"].clone()), (json!(false), json!(message)));
    }
    assert_eq!(session.snapshot(), before, "a render changes nothing");
}
