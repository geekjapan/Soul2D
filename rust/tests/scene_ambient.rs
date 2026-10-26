mod common;

use serde_json::{json, Value};
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "soul2d-ambient-test-{}-{}",
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

/// An INX with one 2x2 textured quad covering (2,2)..(6,6) whose texels are all `texel`.
fn open_fixture(texel: [u8; 4]) -> Session {
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(2, 2, image::Rgba(texel)))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let texture = png.into_inner();
    let part = json!({
        "uuid": 5, "name": "Quad", "type": "Part", "enabled": true, "zsort": 0.0,
        "transform": transform(), "children": [], "lockToRoot": false, "pinToMesh": false,
        "mesh": {"verts": [2.0, 2.0, 6.0, 2.0, 6.0, 6.0, 2.0, 6.0],
                 "uvs": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
                 "indices": [0, 1, 2, 0, 2, 3], "origin": [0.0, 0.0]},
        "vertices": [[2.0, 2.0], [6.0, 2.0], [6.0, 6.0], [2.0, 6.0]],
        "uvs": [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        "indices": [0, 1, 2, 0, 2, 3],
        "textures": [0], "blend_mode": "Normal", "tint": [1, 1, 1], "screenTint": [0, 0, 0],
        "emissionStrength": 1, "opacity": 1,
    });
    let model = json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
            "transform": transform(), "lockToRoot": false, "pinToMesh": false,
            "children": [part]},
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

fn center_pixel(session: &mut Session) -> [u8; 4] {
    let response = session.handle_request(
        &json!({"op": "render", "width": 8, "height": 8, "bounds": [0, 0, 8, 8]}),
    );
    assert_eq!(response["succeeded"], true, "{response}");
    let png = response["result"]["png"].as_str().unwrap().to_owned();
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, png).unwrap();
    let image = image::load_from_memory(&bytes).unwrap().into_rgba8();
    assert_eq!(image.get_pixel(0, 0).0, [0, 0, 0, 0], "outside the quad stays transparent");
    image.get_pixel(4, 4).0
}

fn set_ambient(session: &mut Session, color: Value) -> Value {
    session.handle_request(&json!({"op": "scene.ambient.set", "color": color}))
}

#[test]
fn ambient_light_scales_rendered_color_but_not_alpha() {
    let mut session = open_fixture([200, 100, 80, 255]);
    assert_eq!(session.snapshot()["scene"], json!({"ambientLight": [1.0, 1.0, 1.0]}));
    assert_eq!(center_pixel(&mut session), [200, 100, 80, 255]);

    let reply = set_ambient(&mut session, json!([0.5, 1.0, 0.25]));
    assert_eq!(reply["succeeded"], true, "{reply}");
    assert_eq!(reply["result"], json!({"ambientLight": [0.5, 1.0, 0.25]}));
    assert_eq!(session.snapshot()["scene"], json!({"ambientLight": [0.5, 1.0, 0.25]}));
    assert_eq!(center_pixel(&mut session), [100, 100, 20, 255]);

    set_ambient(&mut session, json!([0, 0, 0]));
    assert_eq!(center_pixel(&mut session), [0, 0, 0, 255], "black ambient keeps coverage");

    set_ambient(&mut session, json!([1, 1, 1]));
    assert_eq!(center_pixel(&mut session), [200, 100, 80, 255]);
}

#[test]
fn ambient_light_scales_straight_color_of_premultiplied_texels() {
    // The renderer reads texel rgb as premultiplied by alpha.
    let mut session = open_fixture([100, 50, 40, 128]);
    assert_eq!(center_pixel(&mut session), [199, 100, 80, 128]);
    set_ambient(&mut session, json!([0.5, 1.0, 0.25]));
    assert_eq!(center_pixel(&mut session), [100, 100, 20, 128]);
}

#[test]
fn ambient_light_is_session_only_and_rejects_bad_colors() {
    let mut session = open_fixture([200, 100, 80, 255]);
    let before = session.snapshot();
    assert_eq!(before["modified"], false);
    set_ambient(&mut session, json!([0.5, 1.0, 0.25]));
    let after = session.snapshot();
    for key in ["history", "canUndo", "canRedo", "modified", "revision", "model"] {
        assert_eq!(after[key], before[key], "{key} must not change");
    }
    assert_eq!(after["scene"]["ambientLight"], json!([0.5, 1.0, 0.25]));

    for bad in [
        json!([0.5, 1.0]),
        json!([0.5, 1.0, 0.25, 1.0]),
        json!([0.5, 1.0, -0.01]),
        json!([0.5, 1.0, 1.01]),
        json!([0.5, "1", 0.25]),
        json!([0.5, null, 0.25]),
        json!("white"),
        json!(null),
        json!({"r": 1}),
    ] {
        let reply = set_ambient(&mut session, bad.clone());
        assert_eq!(reply["succeeded"], false, "accepted {bad}");
        assert_eq!(session.snapshot(), after, "state changed by {bad}");
    }
    let missing = session.handle_request(&json!({"op": "scene.ambient.set"}));
    assert_eq!(missing["succeeded"], false);
    assert_eq!(session.snapshot(), after);

    // Like the D global, the setting outlives new/open.
    session.handle_request(&json!({"op": "new"}));
    assert_eq!(session.snapshot()["scene"]["ambientLight"], json!([0.5, 1.0, 0.25]));
}

#[test]
fn mcp_tool_sets_ambient_light() {
    let mut session = open_fixture([200, 100, 80, 255]);
    let listed = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .unwrap();
    let tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "scene_ambient_set")
        .expect("scene_ambient_set must be listed");
    assert_eq!(tool["inputSchema"]["required"], json!(["color"]));
    let call = |session: &mut Session, color: Value| {
        session
            .handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": "scene_ambient_set", "arguments": {"color": color}}}))
            .unwrap()
    };
    let ok = call(&mut session, json!([0.5, 1.0, 0.25]));
    assert_eq!(ok["result"]["isError"], false, "{ok}");
    assert_eq!(center_pixel(&mut session), [100, 100, 20, 255]);
    let bad = call(&mut session, json!([2, 0, 0]));
    assert_eq!(bad["result"]["isError"], true, "{bad}");
    assert_eq!(session.snapshot()["scene"]["ambientLight"], json!([0.5, 1.0, 0.25]));
}
