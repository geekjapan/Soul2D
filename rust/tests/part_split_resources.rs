mod common;

use base64::Engine;
use serde_json::{json, Value};
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const PLAN_URI: &str = "resource://soul2d/part-split/plan";
const SOURCE_URI: &str = "resource://soul2d/part-split/source";
const MASK_URI: &str = "resource://soul2d/part-split/masks/%E9%AB%AA%20L%2F%26%3F%2B%23%25";

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "soul2d-part-split-res-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&dir).unwrap();
    common::scratch(dir)
}

fn write_png(dir: &PathBuf, name: &str, pixel: impl Fn(u32, u32) -> [u8; 4]) -> String {
    let mut image = image::RgbaImage::new(8, 8);
    for (x, y, px) in image.enumerate_pixels_mut() {
        *px = image::Rgba(pixel(x, y));
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    let path = dir.join(name);
    fs::write(&path, bytes.into_inner()).unwrap();
    path.to_str().unwrap().to_owned()
}

fn session_with_split(dir: &PathBuf, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Session {
    let path = write_png(dir, "source.png", pixel);
    let mut session = Session::new();
    ok(&mut session, "split.open", json!({"path": path}));
    session
}

fn left_right(session: &mut Session) {
    set(session, "Left", json!([{"op": "box", "rect": [0, 0, 4, 8]}]));
    set(session, "Right", json!([{"op": "box", "rect": [4, 0, 4, 8]}]));
}

fn two_part_session(dir: &PathBuf) -> Session {
    let mut session = session_with_split(dir, |x, _| if x < 4 { [255, 0, 0, 255] } else { [0, 0, 255, 255] });
    left_right(&mut session);
    session
}

fn rpc(session: &mut Session, method: &str, params: Value) -> Value {
    session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
        .unwrap()
}

fn listed(session: &mut Session) -> Vec<Value> {
    rpc(session, "resources/list", json!({}))["result"]["resources"]
        .as_array()
        .unwrap()
        .clone()
}

fn read(session: &mut Session) -> Value {
    let response = rpc(session, "resources/read", json!({"uri": PLAN_URI}));
    serde_json::from_str::<Value>(response["result"]["contents"][0]["text"].as_str().unwrap())
        .unwrap()
}

fn read_image(session: &mut Session, uri: &str) -> image::DynamicImage {
    let before = session.snapshot();
    let response = rpc(session, "resources/read", json!({"uri": uri}));
    assert!(response.get("error").is_none(), "{response}");
    let content = &response["result"]["contents"][0];
    assert_eq!(content["uri"], uri);
    assert_eq!(content["mimeType"], "image/png");
    assert!(content.get("text").is_none());
    let png = base64::engine::general_purpose::STANDARD.decode(content["blob"].as_str().unwrap()).unwrap();
    let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap();
    assert_eq!(session.snapshot(), before, "image reads must be read-only");
    image
}

#[test]
fn source_resource_returns_loaded_straight_alpha_pixels_until_reopened() {
    let dir = temp_dir();
    let path = write_png(&dir, "source.png", |x, y| [32, 64, 96, if (x, y) == (0, 0) { 0 } else { 255 }]);
    let mut session = Session::new();
    ok(&mut session, "split.open", json!({"path": path}));
    let expected: Vec<u8> = (0..64).flat_map(|i| [32, 64, 96, if i == 0 { 0 } else { 255 }]).collect();
    let source = read_image(&mut session, SOURCE_URI).to_rgba8();
    assert_eq!(source.dimensions(), (8, 8));
    assert_eq!(source.as_raw(), &expected);
    write_png(&dir, "source.png", |_, _| [0, 255, 0, 255]);
    assert_eq!(read_image(&mut session, SOURCE_URI).to_rgba8().as_raw(), &expected,
        "reading the loaded source must not reload a changed file");
    ok(&mut session, "split.open", json!({"path": path}));
    assert_eq!(read_image(&mut session, SOURCE_URI).to_rgba8().into_raw(), [0, 255, 0, 255].repeat(64));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn mask_resources_preserve_soft_coverage_references_and_transparent_source_pixels() {
    let dir = temp_dir();
    let mut session = session_with_split(&dir, |x, y| [32, 64, 96, if (x, y) == (0, 0) { 0 } else { 255 }]);
    ok(&mut session, "split.part.set", json!({"name":"髪 L/&?+#%", "padding":2,
        "steps":[{"op":"box", "rect":[0,0,4,8]}, {"op":"feather", "radius":1}]}));
    set(&mut session, "Copy", json!([{"op":"part", "name":"髪 L/&?+#%"}]));
    set(&mut session, "A%2F", json!([{"op":"part", "name":"髪 L/&?+#%"}]));
    let expected = [255, 255, 255, 170, 85, 0, 0, 0].repeat(8);
    let mask = read_image(&mut session, MASK_URI);
    assert_eq!(mask.color(), image::ColorType::L8);
    let mask = mask.to_luma8();
    assert_eq!(mask.dimensions(), (8, 8), "resource masks are neither cropped nor padded");
    assert_eq!(mask.into_raw(), expected);
    assert_eq!(read_image(&mut session, "resource://soul2d/part-split/masks/Copy").to_luma8().into_raw(), expected);
    assert_eq!(read_image(&mut session, "resource://soul2d/part-split/masks/A%252F").to_luma8().into_raw(), expected,
        "selectors must be decoded exactly once");
    ok(&mut session, "split.apply", json!({}));
    assert_eq!(session.snapshot()["partSplit"]["applied"], true);
    assert_eq!(read_image(&mut session, MASK_URI).to_luma8().into_raw(), expected);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn split_image_resources_are_discoverable_without_embedding_images() {
    let dir = temp_dir();
    let mut session = session_with_split(&dir, |_, _| [9, 8, 7, 255]);
    set(&mut session, "髪 L/&?+#%", json!([{"op":"box", "rect":[0,0,4,8]}]));
    let before = session.snapshot();
    let resources = listed(&mut session);
    for uri in [SOURCE_URI, MASK_URI] {
        let resource = resources.iter().find(|r| r["uri"] == uri).expect("image resource listed");
        assert_eq!(resource["mimeType"], "image/png");
        assert!(resource.get("blob").is_none());
    }
    let templates = rpc(&mut session, "resources/templates/list", json!({}));
    assert!(templates["result"]["resourceTemplates"].as_array().unwrap().iter().any(|t|
        t["uriTemplate"] == "resource://soul2d/part-split/masks/{name}" && t["mimeType"] == "image/png"));
    assert_eq!(session.snapshot(), before);
    ok(&mut session, "split.close", json!({}));
    assert!(!listed(&mut session).iter().any(|r| r["uri"] == MASK_URI));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn split_image_resource_refusals_preserve_document_draft_and_history() {
    let dir = temp_dir();
    let mut session = session_with_split(&dir, |_, _| [9, 8, 7, 255]);
    left_right(&mut session);
    let check = |session: &mut Session, uri: &str, code| {
        let before = session.snapshot();
        let response = rpc(session, "resources/read", json!({"uri":uri}));
        assert_eq!(response["error"]["code"], code, "{uri}: {response}");
        assert_eq!(session.snapshot(), before, "refused read must not change state");
    };
    for suffix in ["", "%", "%GG", "%FF", "Left/extra", "Left?other=x", "Left#fragment"] {
        check(&mut session, &format!("resource://soul2d/part-split/masks/{suffix}"), -32602);
    }
    check(&mut session, "resource://soul2d/part-split/masks/Missing", -32002);
    ok(&mut session, "split.close", json!({}));
    check(&mut session, SOURCE_URI, -32002);
    check(&mut session, "resource://soul2d/part-split/masks/Left", -32002);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/physics3-editor.cmo3");
    ok(&mut session, "open", json!({"path":path}));
    check(&mut session, SOURCE_URI, -32002);
    check(&mut session, "resource://soul2d/part-split/masks/Left", -32002);
    assert!(!listed(&mut session).iter().any(|r| r["uri"] == SOURCE_URI));
    fs::remove_dir_all(dir).unwrap();
}

fn read_error(session: &mut Session) -> (i64, String) {
    let response = rpc(session, "resources/read", json!({"uri": PLAN_URI}));
    let error = &response["error"];
    (error["code"].as_i64().unwrap(), error["message"].as_str().unwrap().to_owned())
}

fn ok(session: &mut Session, op: &str, request: Value) -> Value {
    let mut call = json!({"op": op});
    call.as_object_mut().unwrap().extend(request.as_object().unwrap().clone());
    let response = session.handle_request(&call);
    assert_eq!(response["succeeded"], json!(true), "{op} failed: {response}");
    response["result"].clone()
}

fn fails(session: &mut Session, op: &str, request: Value) {
    let mut call = json!({"op": op});
    call.as_object_mut().unwrap().extend(request.as_object().unwrap().clone());
    let response = session.handle_request(&call);
    assert_eq!(response["succeeded"], json!(false), "{op} should refuse: {response}");
}

fn set(session: &mut Session, name: &str, steps: Value) {
    ok(session, "split.part.set", json!({"name": name, "steps": steps}));
}

fn write_mask(dir: &PathBuf, coverage: &[u8]) -> String {
    let path = dir.join("mask.png");
    image::GrayImage::from_raw(8, 8, coverage.to_vec())
        .unwrap()
        .save(&path)
        .unwrap();
    path.to_str().unwrap().to_owned()
}

#[test]
fn raster_masks_preserve_coverage_modes_alpha_and_plan_replay() {
    let dir = temp_dir();
    let mut session = session_with_split(&dir, |x, y| {
        [
            200,
            100,
            50,
            match (x, y) {
                (3, 3) => 0,
                (4, 4) => 128,
                _ => 255,
            },
        ]
    });
    let coverage = [0, 32, 64, 128, 192, 255, 16, 255].repeat(8);
    let path = write_mask(&dir, &coverage);
    let before = session.snapshot();
    let inspected = ok(&mut session, "split.mask.inspect", json!({"path":path}));
    assert_eq!(
        inspected,
        json!({"width":8, "height":8, "step":{
        "op":"raster", "path":fs::canonicalize(&path).unwrap(),
        "crc32":crc32fast::hash(&coverage), "mode":"add"}})
    );
    assert_eq!(session.snapshot(), before);
    let step = inspected["step"].clone();
    set(&mut session, "Raster", json!([step]));
    set(
        &mut session,
        "Reference",
        json!([{"op":"part", "name":"Raster"}]),
    );
    assert_eq!(
        read_image(&mut session, "resource://soul2d/part-split/masks/Raster")
            .to_luma8()
            .into_raw(),
        coverage
    );
    assert_eq!(
        read_image(&mut session, "resource://soul2d/part-split/masks/Reference")
            .to_luma8()
            .into_raw(),
        coverage
    );
    set(
        &mut session,
        "Add",
        json!([{"op":"box", "rect":[0,0,2,8]}, step]),
    );
    assert_eq!(
        read_image(&mut session, "resource://soul2d/part-split/masks/Add")
            .to_luma8()
            .into_raw(),
        [255, 255, 64, 128, 192, 255, 16, 255].repeat(8)
    );
    let mut subtract = step.clone();
    subtract["mode"] = json!("subtract");
    set(
        &mut session,
        "Subtract",
        json!([{"op":"box", "rect":[0,0,8,8]}, subtract]),
    );
    assert_eq!(
        read_image(&mut session, "resource://soul2d/part-split/masks/Subtract")
            .to_luma8()
            .into_raw(),
        [255, 223, 191, 127, 63, 0, 239, 0].repeat(8)
    );
    let mut intersect = step.clone();
    intersect["mode"] = json!("intersect");
    set(
        &mut session,
        "Intersect",
        json!([{"op":"box", "rect":[0,0,4,8]}, intersect]),
    );
    assert_eq!(
        read_image(&mut session, "resource://soul2d/part-split/masks/Intersect")
            .to_luma8()
            .into_raw(),
        [0, 32, 64, 128, 0, 0, 0, 0].repeat(8)
    );
    let plan = ok(&mut session, "split.plan", json!({}))["plan"].clone();
    assert_eq!(plan["parts"][0]["steps"][0], step);
    let before_apply = session.snapshot();
    ok(&mut session, "split.apply", json!({}));
    let applied = session.snapshot();
    assert_eq!(
        applied["history"]["count"].as_u64().unwrap(),
        before_apply["history"]["count"].as_u64().unwrap() + 1
    );
    let texture = ok(&mut session, "texture.get", json!({"slot":0}));
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(texture["base64"].as_str().unwrap())
        .unwrap();
    let texture = image::load_from_memory(&bytes).unwrap().to_rgba8();
    let mut alpha = [32, 64, 128, 192, 255, 16, 255, 0].repeat(8);
    alpha[3 * 8 + 2] = 0;
    alpha[4 * 8 + 3] = 96;
    assert_eq!(texture.dimensions(), (8, 8));
    assert_eq!(texture.pixels().map(|p| p[3]).collect::<Vec<_>>(), alpha);
    assert_eq!(texture.get_pixel(2, 3).0, [0, 0, 0, 0]);
    ok(&mut session, "undo", json!({}));
    assert_eq!(session.snapshot()["model"], before_apply["model"]);
    ok(&mut session, "redo", json!({}));
    let mut replay = Session::new();
    ok(&mut replay, "split.apply", json!({"plan":plan}));
    assert_eq!(replay.snapshot()["model"], applied["model"]);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn changed_raster_files_and_unsealed_steps_refuse_without_mutation() {
    let dir = temp_dir();
    let mut session = session_with_split(&dir, |_, _| [200, 100, 50, 255]);
    let path = write_mask(&dir, &[255; 64]);
    let step = ok(&mut session, "split.mask.inspect", json!({"path":path}))["step"].clone();
    set(&mut session, "Raster", json!([step]));
    let plan = ok(&mut session, "split.plan", json!({}))["plan"].clone();
    let before = session.snapshot();
    let mut unsealed = plan.clone();
    unsealed["parts"][0]["steps"][0]
        .as_object_mut()
        .unwrap()
        .remove("crc32");
    let mut replay = Session::new();
    let empty = replay.snapshot();
    fails(&mut replay, "split.apply", json!({"plan":unsealed}));
    assert_eq!(replay.snapshot(), empty);
    for broken in [
        json!({"op":"raster", "path":path}),
        json!({"op":"raster", "path":path, "crc32":null}),
        json!({"op":"raster", "path":path, "crc32":4294967296u64}),
        json!({"op":"raster", "path":path, "crc32":-1}),
        json!({"op":"raster", "path":path, "crc32":0.5}),
        json!({"op":"raster", "path":path, "crc32":0, "extra":true}),
    ] {
        fails(
            &mut session,
            "split.part.set",
            json!({"name":"Invalid", "steps":[broken]}),
        );
        assert_eq!(session.snapshot(), before);
    }
    write_mask(&dir, &[128; 64]);
    for op in ["split.plan", "split.overlay", "split.apply"] {
        fails(&mut session, op, json!({}));
        assert_eq!(session.snapshot(), before);
    }
    let resource = rpc(
        &mut session,
        "resources/read",
        json!({"uri":"resource://soul2d/part-split/masks/Raster"}),
    );
    assert_eq!(resource["error"]["code"], -32002);
    assert_eq!(session.snapshot(), before);
    fails(&mut replay, "split.apply", json!({"plan":plan}));
    assert_eq!(replay.snapshot(), empty);
    write_mask(&dir, &[255; 64]);
    assert_eq!(ok(&mut session, "split.plan", json!({}))["plan"], plan);
    fs::remove_file(&path).unwrap();
    fails(&mut session, "split.apply", json!({}));
    assert_eq!(session.snapshot(), before);
    fails(&mut replay, "split.apply", json!({"plan":plan}));
    assert_eq!(replay.snapshot(), empty);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn raster_inspection_refuses_formats_and_limits_without_state_changes() {
    let dir = temp_dir();
    let mut session = session_with_split(&dir, |_, _| [200, 100, 50, 255]);
    let path = write_mask(&dir, &[128; 64]);
    let valid = fs::read(&path).unwrap();
    let before = session.snapshot();
    let tools = rpc(&mut session, "tools/list", json!({}));
    let tool = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "split_mask_inspect")
        .unwrap();
    assert_eq!(tool["inputSchema"]["required"], json!(["path"]));
    assert_eq!(
        soul2d_core::manifest::effect_of("split_mask_inspect"),
        Some("-")
    );
    let inspected = rpc(
        &mut session,
        "tools/call",
        json!({"name":"split_mask_inspect", "arguments":{"path":path}}),
    );
    assert_ne!(inspected["result"]["isError"], true, "{inspected}");
    for (depth, color) in [
        (1, 0),
        (2, 0),
        (4, 0),
        (16, 0),
        (8, 2),
        (8, 3),
        (8, 4),
        (8, 6),
    ] {
        let mut bad = valid.clone();
        bad[24] = depth;
        bad[25] = color;
        let checksum = crc32fast::hash(&bad[12..29]);
        bad[29..33].copy_from_slice(&checksum.to_be_bytes());
        fs::write(&path, bad).unwrap();
        let response = session.handle_request(&json!({"op":"split.mask.inspect", "path":path}));
        assert_eq!(response["succeeded"], false);
        assert!(
            response["message"]
                .as_str()
                .unwrap()
                .contains("8-bit grayscale PNG"),
            "{response}"
        );
        assert_eq!(session.snapshot(), before);
    }
    for (width, height) in [
        (0u32, 8u32),
        (4, 8),
        (8, 4),
        (8192, 8192),
        (u32::MAX, u32::MAX),
    ] {
        let mut bad = valid.clone();
        bad[16..20].copy_from_slice(&width.to_be_bytes());
        bad[20..24].copy_from_slice(&height.to_be_bytes());
        let checksum = crc32fast::hash(&bad[12..29]);
        bad[29..33].copy_from_slice(&checksum.to_be_bytes());
        fs::write(&path, bad).unwrap();
        fails(&mut session, "split.mask.inspect", json!({"path":path}));
        assert_eq!(session.snapshot(), before);
    }
    let mut transparent = valid[..33].to_vec();
    transparent.extend_from_slice(&2u32.to_be_bytes());
    transparent.extend_from_slice(b"tRNS");
    transparent.extend_from_slice(&[0, 128]);
    transparent.extend_from_slice(&crc32fast::hash(b"tRNS\x00\x80").to_be_bytes());
    transparent.extend_from_slice(&valid[33..]);
    fs::write(&path, transparent).unwrap();
    let response = session.handle_request(&json!({"op":"split.mask.inspect","path":path}));
    assert!(
        response["message"]
            .as_str()
            .unwrap()
            .contains("without alpha"),
        "{response}"
    );
    assert_eq!(session.snapshot(), before);
    fs::write(&path, &valid[..33]).unwrap();
    fails(&mut session, "split.mask.inspect", json!({"path":path}));
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(32 * 1024 * 1024 + 1).unwrap();
    drop(file);
    let response = session.handle_request(&json!({"op":"split.mask.inspect","path":path}));
    assert!(
        response["message"].as_str().unwrap().contains("32 MiB"),
        "{response}"
    );
    assert_eq!(session.snapshot(), before);
    let missing = dir.join("missing.png");
    let response = session.handle_request(&json!({"op":"split.part.set","name":"Bad", "steps":[
        {"op":"raster","path":missing,"crc32":0}, {"op":"part","name":"Unknown"}]}));
    assert!(
        response["message"]
            .as_str()
            .unwrap()
            .contains("unknown part"),
        "{response}"
    );
    assert_eq!(session.snapshot(), before);
    fails(&mut session, "split.mask.inspect", json!({"path":dir}));
    ok(&mut session, "split.close", json!({}));
    let closed = session.snapshot();
    fails(&mut session, "split.mask.inspect", json!({"path":path}));
    assert_eq!(session.snapshot(), closed);
    ok(
        &mut session,
        "open",
        json!({"path":"tests/fixtures/physics3-editor.cmo3"}),
    );
    let native = session.snapshot();
    fails(&mut session, "split.mask.inspect", json!({"path":path}));
    assert_eq!(session.snapshot(), native);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn components_share_seed_selection_and_preserve_applied_state() {
    let dir = temp_dir();
    let mut session = session_with_split(&dir, |x, y| {
        [
            32,
            64,
            96,
            match (x, y) {
                (0, 0) => 1,
                (1, 0) | (3, 3) => 128,
                (4, 4) => 129,
                (7, 0) | (0, 1) | (1, 1) | (6, 7) | (7, 7) => 255,
                _ => 0,
            },
        ]
    });
    let before = session.snapshot();
    let expected = json!({"components":[
        {"seed":[0,0], "pixels":4, "bbox":[0,0,2,2]},
        {"seed":[7,0], "pixels":1, "bbox":[7,0,1,1]},
        {"seed":[3,3], "pixels":1, "bbox":[3,3,1,1]},
        {"seed":[4,4], "pixels":1, "bbox":[4,4,1,1]},
        {"seed":[6,7], "pixels":2, "bbox":[6,7,2,1]}
    ], "totalComponents":5, "nextOffset":null});
    assert_eq!(ok(&mut session, "split.components", json!({})), expected);
    assert_eq!(
        ok(&mut session, "split.components", json!({"threshold":128})),
        json!({"components":[
        {"seed":[7,0], "pixels":1, "bbox":[7,0,1,1]},
        {"seed":[0,1], "pixels":2, "bbox":[0,1,2,1]},
        {"seed":[4,4], "pixels":1, "bbox":[4,4,1,1]},
        {"seed":[6,7], "pixels":2, "bbox":[6,7,2,1]}
    ], "totalComponents":4, "nextOffset":null})
    );
    assert_eq!(
        ok(&mut session, "split.components", json!({"threshold":255})),
        json!({"components":[], "totalComponents":0, "nextOffset":null})
    );
    for request in [
        json!({"threshold":-1}),
        json!({"threshold":256}),
        json!({"threshold":0.5}),
        json!({"threshold":"0"}),
        json!({"offset":-1}),
        json!({"offset":0.5}),
        json!({"offset":4294967296u64}),
    ] {
        fails(&mut session, "split.components", request);
    }
    assert_eq!(session.snapshot(), before);
    set(
        &mut session,
        "Island",
        json!([{"op":"component", "point":[0,0]}]),
    );
    let mask = read_image(&mut session, "resource://soul2d/part-split/masks/Island")
        .to_luma8()
        .into_raw();
    let mut literal = vec![0; 64];
    for index in [0, 1, 8, 9] {
        literal[index] = 255;
    }
    assert_eq!(mask, literal);
    ok(&mut session, "split.apply", json!({}));
    let applied = session.snapshot();
    assert_eq!(applied["partSplit"]["applied"], true);
    assert_eq!(ok(&mut session, "split.components", json!({})), expected);
    assert_eq!(session.snapshot(), applied);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn components_page_all_islands_through_mcp_without_mutation() {
    let dir = temp_dir();
    let path = dir.join("checkerboard.png");
    let mut session = Session::new();
    let tools = rpc(&mut session, "tools/list", json!({}));
    assert!(tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "split_components"));
    assert_eq!(
        soul2d_core::manifest::effect_of("split_components"),
        Some("-")
    );
    for total in [33, 65, 128] {
        let image = image::RgbaImage::from_fn(16, 16, |x, y| {
            image::Rgba([
                1,
                2,
                3,
                if (x + y) % 2 == 0 && y * 8 + x / 2 < total {
                    255
                } else {
                    0
                },
            ])
        });
        image.save(&path).unwrap();
        ok(&mut session, "split.open", json!({"path":path}));
        let before = session.snapshot();
        for offset in [0, 32, 64, 96, 128, u32::MAX] {
            let response = rpc(
                &mut session,
                "tools/call",
                json!({"name":"split_components", "arguments":{"offset":offset}}),
            );
            assert_ne!(response["result"]["isError"], true, "{response}");
            let value: Value =
                serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            let expected: Vec<Value> = if offset < total {
                (offset..(offset + 32).min(total))
                    .map(|i| {
                        let y = i / 8;
                        let x = 2 * (i % 8) + y % 2;
                        json!({"seed":[x,y], "pixels":1, "bbox":[x,y,1,1]})
                    })
                    .collect()
            } else {
                Vec::new()
            };
            assert_eq!(
                value,
                json!({"components":expected, "totalComponents":total,
            "nextOffset":if offset < total && total - offset > 32 { Some(offset+32) } else { None }})
            );
            assert_eq!(session.snapshot(), before);
        }
    }
    ok(&mut session, "split.close", json!({}));
    let closed = session.snapshot();
    fails(&mut session, "split.components", json!({}));
    assert_eq!(session.snapshot(), closed);
    let can3 = dir.join("fixture.can3");
    let container =
        soul2d_core::caff::CaffDocument::parse(include_bytes!("fixtures/physics3-editor.cmo3"))
            .unwrap();
    fs::write(
        &can3,
        container
            .replace_main_xml(include_bytes!("fixtures/can3-curve-editor.xml"))
            .unwrap(),
    )
    .unwrap();
    for path in [PathBuf::from("tests/fixtures/physics3-editor.cmo3"), can3] {
        ok(&mut session, "open", json!({"path":path}));
        let native = session.snapshot();
        fails(&mut session, "split.components", json!({}));
        assert_eq!(session.snapshot(), native);
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn plan_resource_lists_and_reads() {
    let directory = temp_dir();
    let mut session = two_part_session(&directory);
    let listed = listed(&mut session);
    assert!(listed.iter().any(|r| r["uri"] == PLAN_URI && r["mimeType"] == "application/json"),
        "listed: {listed:?}");
    let before = session.snapshot();
    let first = read(&mut session);
    assert_eq!(first["documentRevision"], json!(0));
    assert_eq!(first["draftRevision"], json!(3));
    assert_eq!(first["plan"]["version"], json!(1));
    assert_eq!(first["plan"]["source"]["width"], json!(8));
    assert_eq!(first["plan"]["source"]["height"], json!(8));
    let names: Vec<&str> = first["parts"].as_array().unwrap()
        .iter().map(|part| part["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Left", "Right"]);
    assert_eq!(first["parts"][0]["pixels"], json!(32));
    assert_eq!(first["parts"][0]["bbox"], json!([0, 0, 4, 8]));
    assert_eq!(first["parts"][1]["pixels"], json!(32));
    assert_eq!(first["parts"][1]["bbox"], json!([4, 0, 4, 8]));
    let plan_names: Vec<&str> = first["plan"]["parts"].as_array().unwrap()
        .iter().map(|part| part["name"].as_str().unwrap()).collect();
    assert_eq!(plan_names, ["Left", "Right"]);
    assert_eq!(first["unclaimedPixels"], json!(0));
    assert_eq!(first["multiplyClaimedPixels"], json!(0));
    let second = read(&mut session);
    assert_eq!(second, first);
    assert_eq!(session.snapshot(), before);
}

#[test]
fn plan_resource_tracks_draft_edits() {
    let directory = temp_dir();
    let mut session = two_part_session(&directory);
    set(&mut session, "Left", json!([{"op": "box", "rect": [0, 0, 2, 8]}]));
    let after_resize = read(&mut session);
    assert_eq!(after_resize["documentRevision"], json!(0));
    assert_eq!(after_resize["draftRevision"], json!(4));
    assert_eq!(after_resize["parts"][0]["pixels"], json!(16));
    assert_eq!(after_resize["parts"][0]["bbox"], json!([0, 0, 2, 8]));
    assert_eq!(after_resize["parts"][1]["pixels"], json!(32));
    assert_eq!(after_resize["unclaimedPixels"], json!(16));
    ok(&mut session, "split.part.order", json!({"names": ["Right", "Left"]}));
    let reordered = read(&mut session);
    assert_eq!(reordered["documentRevision"], json!(0));
    assert_eq!(reordered["draftRevision"], json!(5));
    let names: Vec<&str> = reordered["parts"].as_array().unwrap()
        .iter().map(|part| part["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Right", "Left"]);
    let plan_names: Vec<&str> = reordered["plan"]["parts"].as_array().unwrap()
        .iter().map(|part| part["name"].as_str().unwrap()).collect();
    assert_eq!(plan_names, ["Right", "Left"]);
    let before = session.snapshot();
    let _ = read(&mut session);
    assert_eq!(session.snapshot(), before);
}

#[test]
fn plan_resource_refuses_closed_and_native() {
    let mut fresh = Session::new();
    let _ = ok(&mut fresh, "new", json!({}));
    assert!(listed(&mut fresh).iter().any(|r| r["uri"] == PLAN_URI));
    let before = fresh.snapshot();
    let (code, message) = read_error(&mut fresh);
    assert_eq!(code, -32002);
    assert_eq!(message, "no source is open (call split.open first)");
    assert_eq!(fresh.snapshot(), before);
    let _ = ok(&mut fresh, "open", json!({"path": "tests/fixtures/physics3-editor.cmo3"}));
    assert!(!listed(&mut fresh).iter().any(|r| r["uri"] == PLAN_URI));
    let before = fresh.snapshot();
    let (code, message) = read_error(&mut fresh);
    assert_eq!(code, -32002);
    assert_eq!(message, "split plans require an INP/INX document");
    let physics = rpc(&mut fresh, "resources/read",
        json!({"uri": "resource://soul2d/document/physics-source"}));
    let physics = serde_json::from_str::<Value>(physics["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(physics["present"], json!(true));
    assert_eq!(physics["groups"][0]["name"], json!("Sway"));
    assert_eq!(fresh.snapshot(), before);
}

#[test]
fn plan_resource_replays_nested_plan() {
    let directory = temp_dir();
    let mut session = two_part_session(&directory);
    let context = read(&mut session);
    let mut replay = Session::new();
    let before = replay.snapshot();
    fails(&mut replay, "split.apply", json!({"plan": context}));
    assert_eq!(replay.snapshot(), before);
    let applied = ok(&mut replay, "split.apply", json!({"plan": context["plan"].clone()}));
    assert_eq!(applied["created"].as_array().unwrap().len(), 2);
    let parts_of = |session: &Session| -> Vec<(String, Value)> {
        session.snapshot()["model"]["nodes"]["children"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|node| node["type"] == "Part")
            .map(|node| (node["name"].as_str().unwrap().to_owned(), node["transform"]["trans"].clone()))
            .collect()
    };
    let parts = parts_of(&replay);
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0], ("Left".to_owned(), json!([-2.0, 0.0, 0.0])));
    assert_eq!(parts[1], ("Right".to_owned(), json!([2.0, 0.0, 0.0])));
    assert_eq!(replay.snapshot()["history"]["count"].as_u64().unwrap(), 1);
    let _ = ok(&mut replay, "undo", json!({}));
    assert!(parts_of(&replay).is_empty());
    let _ = ok(&mut replay, "redo", json!({}));
    assert_eq!(parts_of(&replay).len(), 2);
}

#[test]
fn plan_resource_close_and_replacement() {
    let directory = temp_dir();
    let mut session = two_part_session(&directory);
    let _ = read(&mut session);
    ok(&mut session, "split.close", json!({}));
    let before = session.snapshot();
    let (code, _) = read_error(&mut session);
    assert_eq!(code, -32002);
    assert_eq!(session.snapshot(), before);
    ok(&mut session, "new", json!({}));
    let path = write_png(&directory, "second.png", |_, y| if y < 4 { [0, 128, 0, 255] } else { [255, 255, 0, 255] });
    ok(&mut session, "split.open", json!({"path": path}));
    set(&mut session, "Top", json!([{"op": "box", "rect": [0, 0, 8, 4]}]));
    set(&mut session, "Bottom", json!([{"op": "box", "rect": [0, 4, 8, 4]}]));
    let context = read(&mut session);
    assert_eq!(context["documentRevision"], json!(1));
    assert_eq!(context["draftRevision"], json!(3));
    assert_eq!(context["plan"]["source"]["path"], json!(path));
    let names: Vec<&str> = context["parts"].as_array().unwrap()
        .iter().map(|part| part["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Top", "Bottom"]);
    assert_eq!(context["parts"][0]["pixels"], json!(32));
    assert_eq!(context["parts"][0]["bbox"], json!([0, 0, 8, 4]));
    assert_eq!(context["parts"][1]["bbox"], json!([0, 4, 8, 4]));
    assert_eq!(context["parts"][1]["pixels"], json!(32));
    let before = session.snapshot();
    let _ = read(&mut session);
    assert_eq!(session.snapshot(), before);
}
