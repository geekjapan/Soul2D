//! Session operations for INP export, Krita import and merge, extension data, single-texture reads and
//! texture-free scene geometry (`export.inp`, `import.kra`, `merge.kra.plan`, `merge.kra.apply`,
//! `extdata.get`, `extdata.set`, `texture.get`, `scene.geometry`).

mod common;

use base64::Engine;
use serde_json::{json, Value};
use soul2d_core::editor_controller::{EditorController, FILE_OP_BLOCKED_MESSAGE, UNSAVED_REPLACEMENT_MESSAGE};
use soul2d_core::Session;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// ---------------------------------------------------------------------------- PSD builder
// The same layout tests/import_session.rs builds; only what these fixtures need.

const TRAILER: [u8; 6] = [0, 0, 1, 2, 3, 4];

fn be16(value: u16) -> [u8; 2] {
    value.to_be_bytes()
}

fn be32(value: u32) -> [u8; 4] {
    value.to_be_bytes()
}

fn raw(pixels: &[u8]) -> Vec<u8> {
    [&be16(0)[..], pixels].concat()
}

struct L {
    name: &'static str,
    rect: [i32; 4], // top, left, bottom, right
    channels: Vec<(i16, Vec<u8>)>,
    lsct: Option<u32>,
}

fn block(key: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = [&b"8BIM"[..], key, &be32(payload.len() as u32)].concat();
    out.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        out.push(0);
    }
    out
}

fn marker(name: &'static str, kind: u32) -> L {
    L {
        name,
        rect: [0; 4],
        channels: (0..3).map(|id| (id, raw(&[]))).chain([(-1, raw(&[]))]).collect(),
        lsct: Some(kind),
    }
}

fn solid(name: &'static str, rect: [i32; 4], rgba: [u8; 4]) -> L {
    let [top, left, bottom, right] = rect;
    let count = ((bottom - top) * (right - left)) as usize;
    let channels = (0..4)
        .map(|channel| {
            let id = if channel == 3 { -1 } else { channel as i16 };
            (id, raw(&vec![rgba[channel]; count]))
        })
        .collect();
    L { name, rect, channels, lsct: None }
}

fn layer_record(layer: &L) -> Vec<u8> {
    let mut out = Vec::new();
    for value in layer.rect {
        out.extend(value.to_be_bytes());
    }
    out.extend(be16(layer.channels.len() as u16));
    for (id, data) in &layer.channels {
        out.extend(id.to_be_bytes());
        out.extend(be32(data.len() as u32));
    }
    out.extend_from_slice(b"8BIM");
    out.extend_from_slice(if layer.lsct.is_some() { b"pass" } else { b"norm" });
    out.extend([255, 0, 0, 0]);
    let mut extra = be32(0).to_vec();
    extra.extend(be32(0));
    extra.push(layer.name.len() as u8);
    extra.extend_from_slice(layer.name.as_bytes());
    while extra.len() % 4 != 0 {
        extra.push(0);
    }
    if let Some(kind) = layer.lsct {
        extra.extend(block(b"lsct", &be32(kind)));
    }
    out.extend(be32(extra.len() as u32));
    out.extend(extra);
    out
}

fn psd(width: u32, height: u32, layers: &[L]) -> Vec<u8> {
    let mut info = (layers.len() as i16).to_be_bytes().to_vec();
    for layer in layers {
        info.extend(layer_record(layer));
    }
    for layer in layers {
        for (_, data) in &layer.channels {
            info.extend_from_slice(data);
        }
    }
    if info.len() % 2 == 1 {
        info.push(0);
    }
    let mut body = be32(info.len() as u32).to_vec();
    body.extend_from_slice(&info);
    let mut section = be32(body.len() as u32).to_vec();
    section.extend(body);

    let mut out = b"8BPS".to_vec();
    out.extend(be16(1));
    out.extend([0; 6]);
    out.extend(be16(4));
    out.extend(be32(height));
    out.extend(be32(width));
    out.extend(be16(8));
    out.extend(be16(3));
    out.extend(be32(0));
    let resource = [&b"8BIM"[..], &be16(1005), &[0, 0], &be32(4), &[1, 2, 3, 4]].concat();
    out.extend(be32(resource.len() as u32));
    out.extend(resource);
    out.extend(section);
    out.extend(TRAILER);
    out
}

// ---------------------------------------------------------------------------- KRA builder

/// A stored-only ZIP with the members in the order given.
fn zip(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut directory = Vec::new();
    for (name, data) in members {
        let mut common = Vec::new();
        common.extend_from_slice(&0u16.to_le_bytes()); // flags
        common.extend_from_slice(&0u16.to_le_bytes()); // stored
        common.extend_from_slice(&[0; 4]);
        common.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
        common.extend_from_slice(&(data.len() as u32).to_le_bytes());
        common.extend_from_slice(&(data.len() as u32).to_le_bytes());
        common.extend_from_slice(&(name.len() as u16).to_le_bytes());
        common.extend_from_slice(&0u16.to_le_bytes());
        let offset = out.len() as u32;
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&common);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        directory.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        directory.extend_from_slice(&20u16.to_le_bytes());
        directory.extend_from_slice(&20u16.to_le_bytes());
        directory.extend_from_slice(&common);
        directory.extend_from_slice(&[0; 2 + 2 + 2 + 4]);
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }
    let directory_offset = out.len() as u32;
    out.extend_from_slice(&directory);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(members.len() as u16).to_le_bytes());
    out.extend_from_slice(&(members.len() as u16).to_le_bytes());
    out.extend_from_slice(&(directory.len() as u32).to_le_bytes());
    out.extend_from_slice(&directory_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// LZF with literal runs only (a valid stream).
fn lzf(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for run in input.chunks(32) {
        out.push((run.len() - 1) as u8);
        out.extend_from_slice(run);
    }
    out
}

/// One solid tile of `w` x `h` at (0,0), planar BGRA like Krita stores it.
fn tile_file(w: usize, h: usize, rgba: [u8; 4]) -> Vec<u8> {
    let area = w * h;
    let planes: Vec<u8> = [rgba[2], rgba[1], rgba[0], rgba[3]].iter().flat_map(|&v| vec![v; area]).collect();
    let packed = lzf(&planes);
    let mut out = format!("VERSION 2\nTILEWIDTH {w}\nTILEHEIGHT {h}\nPIXELSIZE 4\nDATA 1\n0,0,LZF,{}\n", packed.len() + 1)
        .into_bytes();
    out.push(1);
    out.extend_from_slice(&packed);
    out
}

/// A layer of the fixture documents: `rect` is [top, left, bottom, right] like the PSD builder.
enum K {
    Layer(&'static str, [i32; 4], [u8; 4]),
    Group(&'static str, Vec<K>),
}

fn kra_xml(layers: &[K], files: &mut Vec<(String, Vec<u8>)>) -> String {
    let mut xml = String::new();
    for layer in layers {
        match layer {
            K::Layer(name, [top, left, bottom, right], rgba) => {
                let file = format!("layer{}", files.len() + 1);
                files.push((format!("doc/layers/{file}"), tile_file((right - left) as usize, (bottom - top) as usize, *rgba)));
                xml += &format!(
                    "<layer name=\"{name}\" nodetype=\"paintlayer\" uuid=\"{{{file}}}\" filename=\"{file}\" \
                     colorspacename=\"RGBA\" visible=\"1\" opacity=\"255\" compositeop=\"normal\" x=\"{left}\" y=\"{top}\"/>"
                );
            }
            K::Group(name, children) => {
                let inner = kra_xml(children, files);
                xml += &format!(
                    "<layer name=\"{name}\" nodetype=\"grouplayer\" uuid=\"{{g-{name}}}\" visible=\"1\" opacity=\"255\" \
                     collapsed=\"0\" x=\"0\" y=\"0\" compositeop=\"normal\"><layers>{inner}</layers></layer>"
                );
            }
        }
    }
    xml
}

/// Layers are listed top to bottom, as Krita's `maindoc.xml` does.
fn kra(width: u32, height: u32, layers: &[K]) -> Vec<u8> {
    let mut files = Vec::new();
    let inner = kra_xml(layers, &mut files);
    let maindoc = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<DOC xmlns=\"http://www.calligra.org/DTD/krita\" \
         kritaVersion=\"5.2.2\" syntaxVersion=\"2.0\"><IMAGE name=\"doc\" width=\"{width}\" height=\"{height}\" \
         colorspacename=\"RGBA\" mime=\"application/x-kra\"><ProjectionBackgroundColor ColorData=\"255,255,255,255\"/>\
         <layers>{inner}</layers></IMAGE></DOC>"
    );
    let mut members: Vec<(&str, Vec<u8>)> =
        vec![("mimetype", b"application/x-krita".to_vec()), ("maindoc.xml", maindoc.into_bytes())];
    members.extend(files.iter().map(|(name, data)| (name.as_str(), data.clone())));
    zip(&members)
}

// ---------------------------------------------------------------------------- fixtures

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "soul2d-session-ops2-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    common::scratch(path)
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> String {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    path.to_str().unwrap().to_owned()
}

fn run(session: &mut Session, request: Value) -> Value {
    let response = session.handle_request(&request);
    assert_eq!(response["succeeded"], true, "{request} -> {response}");
    response["result"].clone()
}

fn fail(session: &mut Session, request: Value) -> String {
    let response = session.handle_request(&request);
    assert_eq!(response["succeeded"], false, "{request} -> {response}");
    response["message"].as_str().unwrap().to_owned()
}

fn call(session: &mut Session, name: &str, arguments: Value) -> Value {
    session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}}))
        .unwrap()["result"]
        .clone()
}

/// The two layered formats carry the same fixture documents, so every test runs against both.
#[derive(Clone, Copy)]
enum Format {
    Psd,
    Kra,
}

const FORMATS: [Format; 2] = [Format::Psd, Format::Kra];

impl Format {
    fn import_op(self) -> &'static str {
        match self {
            Self::Psd => "import.psd",
            Self::Kra => "import.kra",
        }
    }

    fn plan_op(self) -> &'static str {
        match self {
            Self::Psd => "merge.psd.plan",
            Self::Kra => "merge.kra.plan",
        }
    }

    fn apply_op(self) -> &'static str {
        match self {
            Self::Psd => "merge.psd.apply",
            Self::Kra => "merge.kra.apply",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Psd => "PSD",
            Self::Kra => "KRA",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Psd => "psd",
            Self::Kra => "kra",
        }
    }

    /// 12x12 canvas, top to bottom: Face (6x4), group Hat { Brim (6x4) }, Body (6x4).
    fn base(self) -> Vec<u8> {
        match self {
            Self::Psd => psd(
                12,
                12,
                &[
                    solid("Body", [0, 0, 4, 6], [200, 100, 50, 255]),
                    marker("</Layer group>", 3),
                    solid("Brim", [4, 6, 8, 12], [10, 20, 30, 255]),
                    marker("Hat", 1),
                    solid("Face", [8, 0, 12, 6], [1, 2, 3, 255]),
                ],
            ),
            Self::Kra => kra(
                12,
                12,
                &[
                    K::Layer("Face", [8, 0, 12, 6], [1, 2, 3, 255]),
                    K::Group("Hat", vec![K::Layer("Brim", [4, 6, 8, 12], [10, 20, 30, 255])]),
                    K::Layer("Body", [0, 0, 4, 6], [200, 100, 50, 255]),
                ],
            ),
        }
    }

    /// Replaces Body and Brim (same paths) and adds Extra.
    fn merge(self) -> Vec<u8> {
        match self {
            Self::Psd => psd(
                12,
                12,
                &[
                    solid("Body", [0, 0, 4, 6], [9, 8, 7, 255]),
                    marker("</Layer group>", 3),
                    solid("Brim", [4, 6, 8, 12], [11, 12, 13, 255]),
                    marker("Hat", 1),
                    solid("Extra", [8, 6, 12, 12], [5, 5, 5, 255]),
                ],
            ),
            Self::Kra => kra(
                12,
                12,
                &[
                    K::Layer("Extra", [8, 6, 12, 12], [5, 5, 5, 255]),
                    K::Group("Hat", vec![K::Layer("Brim", [4, 6, 8, 12], [11, 12, 13, 255])]),
                    K::Layer("Body", [0, 0, 4, 6], [9, 8, 7, 255]),
                ],
            ),
        }
    }

    fn imported(self, dir: &Path) -> Session {
        let mut session = Session::new();
        let path = write(dir, &format!("base.{}", self.extension()), &self.base());
        run(&mut session, json!({"op": self.import_op(), "path": path}));
        session
    }
}

/// First pixel of every texture slot in the INX bytes.
fn texture_pixels(bytes: &[u8]) -> Vec<[u8; 4]> {
    let json_len = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let mut at = 12 + json_len;
    assert_eq!(&bytes[at..at + 8], b"TEX_SECT");
    let count = u32::from_be_bytes(bytes[at + 8..at + 12].try_into().unwrap());
    at += 12;
    let mut pixels = Vec::new();
    for _ in 0..count {
        let length = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        assert_eq!(bytes[at + 4], 0, "PNG");
        let png = &bytes[at + 5..at + 5 + length];
        let image = image::load_from_memory_with_format(png, image::ImageFormat::Png).unwrap().into_rgba8();
        pixels.push(image.get_pixel(0, 0).0);
        at += 5 + length;
    }
    pixels
}

/// The number of textures in the INX bytes (the atlases of an INP are TGA, so no pixels are read).
fn texture_count(bytes: &[u8]) -> u32 {
    let json_len = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let at = 12 + json_len;
    assert_eq!(&bytes[at..at + 8], b"TEX_SECT");
    u32::from_be_bytes(bytes[at + 8..at + 12].try_into().unwrap())
}

/// The textures the session would write, through `save` into a fresh file that is not remembered.
fn saved_pixels(session: &mut Session, dir: &Path) -> Vec<[u8; 4]> {
    let path = dir.join(format!("probe-{}.inx", SEQUENCE.fetch_add(1, Ordering::Relaxed)));
    run(session, json!({"op": "save", "path": path.to_str().unwrap(), "mark_saved": false}));
    texture_pixels(&fs::read(path).unwrap())
}

fn part_names(snapshot: &Value) -> Vec<String> {
    fn walk(node: &Value, out: &mut Vec<String>) {
        if node["type"] == "Part" {
            out.push(node["name"].as_str().unwrap().to_owned());
        }
        for child in node["children"].as_array().into_iter().flatten() {
            walk(child, out);
        }
    }
    let mut names = Vec::new();
    walk(&snapshot["model"]["nodes"], &mut names);
    names
}

fn snapshot(session: &mut Session) -> Value {
    run(session, json!({"op": "snapshot"}))
}

/// The extension block that follows the textures in INX bytes (empty for none).
fn ext_block(bytes: &[u8]) -> Vec<u8> {
    let json_len = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let mut at = 12 + json_len + 8;
    let count = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    at += 4;
    for _ in 0..count {
        let length = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        at += 5 + length;
    }
    bytes[at..].to_vec()
}

/// `ext_section` entries the way D writes them.
fn encode_ext(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = b"EXT_SECT".to_vec();
    out.extend((entries.len() as u32).to_be_bytes());
    for (name, payload) in entries {
        out.extend((name.len() as u32).to_be_bytes());
        out.extend(name.as_bytes());
        out.extend((payload.len() as u32).to_be_bytes());
        out.extend(*payload);
    }
    out
}

/// Saves the session, then replaces everything after the textures by `tail_after_textures`.
fn inx_with_tail(session: &mut Session, dir: &Path, name: &str, tail_after_textures: &[u8]) -> String {
    let probe = dir.join(format!("probe-{}.inx", SEQUENCE.fetch_add(1, Ordering::Relaxed)));
    run(session, json!({"op": "save", "path": probe.to_str().unwrap(), "mark_saved": false}));
    let mut bytes = fs::read(probe).unwrap();
    let keep = bytes.len() - ext_block(&bytes).len();
    bytes.truncate(keep);
    bytes.extend_from_slice(tail_after_textures);
    write(dir, name, &bytes)
}

// ---------------------------------------------------------------------------- import.kra / merge.kra

#[test]
fn a_krita_import_builds_the_same_document_a_psd_does_and_replaces_the_session() {
    let dir = temp_dir();
    for format in FORMATS {
        let mut session = Session::new();
        run(&mut session, json!({"op": "parameter.create", "dimensions": 1, "name": "Old"}));
        let before = snapshot(&mut session);
        assert_eq!(before["canUndo"], true);

        let path = write(&dir, &format!("base.{}", format.extension()), &format.base());
        let result = run(&mut session, json!({"op": format.import_op(), "path": path}));
        assert_eq!(result["imported"], json!({"nodes": 4, "textures": 3}), "{}", format.label());
        assert_eq!(result["revision"], before["revision"].as_u64().unwrap() + 1);
        assert_eq!((result["modified"].clone(), result["canUndo"].clone()), (json!(false), json!(false)));
        assert_eq!(result["model"]["param"], json!([]));
        assert_eq!(part_names(&result), ["Body", "Brim", "Face"]);
        assert_eq!(result["model"]["nodes"]["children"][1]["type"], "DynamicComposite");
        assert_eq!(
            saved_pixels(&mut session, &dir),
            [[200, 100, 50, 255], [10, 20, 30, 255], [1, 2, 3, 255]],
            "{}",
            format.label()
        );

        let flat = run(&mut session, json!({"op": format.import_op(), "path": path, "keepStructure": false}));
        assert_eq!(flat["imported"], json!({"nodes": 3, "textures": 3}));
        let composite =
            run(&mut session, json!({"op": format.import_op(), "path": path, "layerGroupNodeType": "Composite"}));
        assert_eq!(composite["model"]["nodes"]["children"][1]["type"], "Composite");
    }
}

#[test]
fn a_bad_krita_import_leaves_the_session_untouched() {
    let dir = temp_dir();
    let mut session = Format::Kra.imported(&dir);
    let before = snapshot(&mut session);
    let garbage = write(&dir, "garbage.kra", b"not a krita file at all");
    let psd_file = write(&dir, "wrong.kra", &Format::Psd.base());
    let base = write(&dir, "base2.kra", &Format::Kra.base());
    for request in [
        json!({"op": "import.kra", "path": garbage}),
        json!({"op": "import.kra", "path": psd_file}),
        json!({"op": "import.kra", "path": dir.to_str().unwrap()}),
        json!({"op": "import.kra", "path": dir.join("missing.kra").to_str().unwrap()}),
        json!({"op": "import.kra"}),
        json!({"op": "import.kra", "path": base, "keepStructure": "yes"}),
        json!({"op": "import.kra", "path": base, "layerGroupNodeType": "Part"}),
    ] {
        fail(&mut session, request);
        assert_eq!(snapshot(&mut session), before);
    }
    assert!(fail(&mut session, json!({"op": "import.kra", "path": dir.to_str().unwrap()})).contains("regular file"));
    assert!(fail(&mut session, json!({"op": "import.kra", "path": dir.join("missing.kra").to_str().unwrap()}))
        .contains("could not open KRA file"));
}

#[test]
fn the_editor_refuses_a_krita_import_over_unsaved_changes_and_over_a_dirty_draft() {
    let dir = temp_dir();
    let path = write(&dir, "base.kra", &Format::Kra.base());
    let mut controller = EditorController::new();
    let first = controller.core_request(&json!({"op": "import.kra", "path": path}));
    assert_eq!(first["succeeded"], true, "{first}");
    controller.core_request(&json!({"op": "parameter.create", "dimensions": 1, "name": "Edit"}));
    assert_eq!(controller.snapshot()["modified"], true);

    let refused = controller.core_request(&json!({"op": "import.kra", "path": path}));
    assert_eq!(refused["message"], UNSAVED_REPLACEMENT_MESSAGE);
    let via_mcp = controller
        .mcp_request(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "import_kra", "arguments": {"path": path}}}))
        .unwrap();
    assert_eq!(via_mcp["result"]["isError"], true);
    assert_eq!(via_mcp["result"]["content"][0]["text"], UNSAVED_REPLACEMENT_MESSAGE);
    assert_eq!(controller.snapshot()["model"]["param"].as_array().unwrap().len(), 1, "document kept");

    let saved = dir.join("saved.inx");
    let response = controller.core_request(&json!({"op": "save", "path": saved.to_str().unwrap()}));
    assert_eq!(response["succeeded"], true, "{response}");

    // A draft that is not applied blocks file operations before the unsaved-changes check.
    let node = controller.snapshot()["model"]["nodes"]["children"][0]["uuid"].as_u64().unwrap();
    controller.select_node(node).unwrap();
    controller.set_draft_name("Renamed".into());
    assert!(controller.draft_dirty());
    let blocked = controller.core_request(&json!({"op": "import.kra", "path": path}));
    assert_eq!(blocked["message"], FILE_OP_BLOCKED_MESSAGE);
    let export = dir.join("blocked.inp");
    let blocked = controller.core_request(&json!({"op": "export.inp", "path": export.to_str().unwrap()}));
    assert_eq!(blocked["message"], FILE_OP_BLOCKED_MESSAGE);
    assert!(!export.exists());
    controller.discard_draft();

    let again = controller
        .mcp_request(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "import_kra", "arguments": {"path": path, "keepStructure": false}}}))
        .unwrap();
    assert_eq!(again["result"]["isError"], false, "{again}");
    assert_eq!(again["result"]["structuredContent"]["imported"], json!({"nodes": 3, "textures": 3}));
}

#[test]
fn the_merge_plan_lists_layers_with_their_parts_and_changes_nothing() {
    let dir = temp_dir();
    for format in FORMATS {
        let mut session = format.imported(&dir);
        let before = snapshot(&mut session);
        let path = write(&dir, &format!("merge.{}", format.extension()), &format.merge());
        let plan = run(&mut session, json!({"op": format.plan_op(), "path": path}));
        assert_eq!(plan["document"], json!({"width": 12, "height": 12}));
        // Ids follow D's top-down creation: Root 1, Face 2, Hat 3, Brim 4, Body 5.
        assert_eq!(
            plan["layers"],
            json!([
                {"index": 0, "name": "Extra", "path": "/Extra", "rect": [6, 8, 6, 4], "target": null, "matchedBy": null},
                {"index": 1, "name": "Brim", "path": "/Hat/Brim", "rect": [6, 4, 6, 4], "target": 4, "matchedBy": "path"},
                {"index": 2, "name": "Body", "path": "/Body", "rect": [0, 0, 6, 4], "target": 5, "matchedBy": "path"},
            ]),
            "{}",
            format.label()
        );
        assert_eq!(plan["unmappedParts"], json!([2]));
        assert_eq!(snapshot(&mut session), before);
        assert_eq!(saved_pixels(&mut session, &dir).len(), 3);

        assert!(fail(&mut session, json!({"op": format.plan_op(), "path": path, "renameMapped": 1}))
            .contains("renameMapped"));
        assert!(fail(&mut session, json!({"op": format.plan_op(), "path": dir.to_str().unwrap()}))
            .contains("regular file"));
    }
}

#[test]
fn a_krita_merge_is_one_undo_step_that_swaps_textures_with_the_model() {
    let dir = temp_dir();
    for format in FORMATS {
        let mut session = format.imported(&dir);
        let before = snapshot(&mut session);
        let old = [[200, 100, 50, 255], [10, 20, 30, 255], [1, 2, 3, 255]];
        assert_eq!(saved_pixels(&mut session, &dir), old);

        let path = write(&dir, &format!("merge.{}", format.extension()), &format.merge());
        let result = run(&mut session, json!({"op": format.apply_op(), "path": path}));
        assert_eq!(result, json!({"created": [6], "replaced": [4, 5], "textures": 4, "missingLayers": [], "removed": "keep"}), "{}", format.label());
        let merged = snapshot(&mut session);
        let new = [[9, 8, 7, 255], [11, 12, 13, 255], [1, 2, 3, 255], [5, 5, 5, 255]];
        assert_eq!(saved_pixels(&mut session, &dir), new);
        assert_eq!(part_names(&merged), ["Body", "Brim", "Face", "Extra"]);
        assert_eq!(merged["history"]["count"], 1);
        assert_eq!(merged["history"]["states"][1]["label"], format.apply_op());
        assert_eq!((merged["modified"].clone(), merged["canUndo"].clone()), (json!(true), json!(true)));

        let undone = run(&mut session, json!({"op": "undo"}));
        assert_eq!(undone["model"], before["model"]);
        assert_eq!(
            (undone["modified"].clone(), undone["canUndo"].clone(), undone["canRedo"].clone()),
            (json!(false), json!(false), json!(true))
        );
        assert_eq!(saved_pixels(&mut session, &dir), old, "old textures are back");

        let redone = run(&mut session, json!({"op": "redo"}));
        assert_eq!(redone["model"], merged["model"]);
        assert_eq!(redone["modified"], true);
        assert_eq!(saved_pixels(&mut session, &dir), new, "new textures are back");

        let saved = dir.join(format!("merged-{}.inx", format.extension()));
        run(&mut session, json!({"op": "save", "path": saved.to_str().unwrap()}));
        let mut reopened = Session::new();
        let opened = run(&mut reopened, json!({"op": "open", "path": saved.to_str().unwrap()}));
        assert_eq!(opened["modified"], false);
        assert_eq!(part_names(&opened), ["Body", "Brim", "Face", "Extra"]);
        assert_eq!(saved_pixels(&mut reopened, &dir), new);
    }
}

#[test]
fn krita_merge_choices_and_guards_match_the_psd_merge() {
    let dir = temp_dir();
    let mut session = Format::Kra.imported(&dir);
    let path = write(&dir, "merge.kra", &Format::Kra.merge());
    // Extra replaces Face (part 2) and renames it; Brim is ignored; Body is added as a second Part.
    let result = run(
        &mut session,
        json!({"op": "merge.kra.apply", "path": path, "renameMapped": true, "retranslateMapped": true,
            "choices": [
                {"layer": 0, "action": "replace", "part": 2},
                {"layer": 1, "action": "ignore"},
                {"layer": 2, "action": "add"},
            ]}),
    );
    assert_eq!(result, json!({"created": [6], "replaced": [2], "textures": 4, "missingLayers": [], "removed": "keep"}));
    assert_eq!(part_names(&snapshot(&mut session)), ["Body", "Brim", "Extra", "Body"]);
    assert_eq!(
        saved_pixels(&mut session, &dir),
        [[200, 100, 50, 255], [10, 20, 30, 255], [5, 5, 5, 255], [9, 8, 7, 255]]
    );

    let before = snapshot(&mut session);
    let none = run(
        &mut session,
        json!({"op": "merge.kra.apply", "path": path,
            "choices": [{"layer": 0, "action": "ignore"}, {"layer": 1, "action": "ignore"}, {"layer": 2, "action": "ignore"}]}),
    );
    assert_eq!(none, json!({"created": [], "replaced": [], "textures": 4, "missingLayers": [], "removed": "keep"}));
    assert_eq!(snapshot(&mut session), before, "ignoring everything records nothing");
    for choices in [json!({}), json!([{"layer": 9, "action": "add"}]), json!([{"layer": 0, "action": "replace", "part": 999}])] {
        fail(&mut session, json!({"op": "merge.kra.apply", "path": path, "choices": choices}));
        assert_eq!(snapshot(&mut session), before, "a refused merge changes nothing");
    }
    let garbage = write(&dir, "garbage.kra", b"nope");
    for request in [
        json!({"op": "merge.kra.apply", "path": garbage}),
        json!({"op": "merge.kra.apply", "path": dir.to_str().unwrap()}),
        json!({"op": "merge.kra.apply"}),
    ] {
        fail(&mut session, request);
        assert_eq!(snapshot(&mut session), before);
    }

    // An empty INX session has no Part to match, so every layer is planned as an addition.
    let mut empty = Session::new();
    let plan = run(&mut empty, json!({"op": "merge.kra.plan", "path": path}));
    assert_eq!(plan["unmappedParts"], json!([]));
    assert_eq!(plan["layers"][0]["target"], Value::Null);
}

#[test]
fn the_krita_tools_are_listed_and_call_the_session_operations() {
    let dir = temp_dir();
    let mut session = Session::new();
    let listed = session.handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    let tools = listed["result"]["tools"].as_array().unwrap();
    for (name, required) in [
        ("import_kra", json!(["path"])),
        ("merge_kra_plan", json!(["path"])),
        ("merge_kra_apply", json!(["path"])),
        ("export_inp", json!(["path"])),
        ("extdata_get", json!(["key"])),
        ("extdata_set", json!(["key", "base64"])),
        ("texture_get", json!(["slot"])),
        ("scene_geometry", json!([])),
    ] {
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap_or_else(|| panic!("{name} is listed"));
        assert_eq!(tool["inputSchema"]["required"], required, "{name}");
    }

    let path = write(&dir, "base.kra", &Format::Kra.base());
    let imported = call(&mut session, "import_kra", json!({"path": path}));
    assert_eq!(imported["isError"], false, "{imported}");
    assert_eq!(imported["structuredContent"]["imported"], json!({"nodes": 4, "textures": 3}));
    let merge = write(&dir, "merge.kra", &Format::Kra.merge());
    let plan = call(&mut session, "merge_kra_plan", json!({"path": merge}));
    assert_eq!(plan["structuredContent"]["layers"][2]["target"], 5);
    let applied = call(&mut session, "merge_kra_apply", json!({"path": merge}));
    assert_eq!(applied["structuredContent"], json!({"created": [6], "replaced": [4, 5], "textures": 4, "missingLayers": [], "removed": "keep"}));
}

// ---------------------------------------------------------------------------- extdata

const OTHER_KEY: &str = "com.example.Other";
const FLIP_KEY: &str = "com.inochi2d.creator.FlipConfig";

/// A session opened from an INX with textures and one extension entry; returns the file path too.
fn with_extension_entry(dir: &Path) -> (Session, String) {
    let mut source = Format::Psd.imported(dir);
    let path = inx_with_tail(&mut source, dir, "ext.inx", &encode_ext(&[(OTHER_KEY, b"hello")]));
    let mut session = Session::new();
    run(&mut session, json!({"op": "open", "path": path}));
    (session, path)
}

#[test]
fn extension_data_is_read_by_key() {
    let dir = temp_dir();
    let (mut session, _) = with_extension_entry(&dir);
    let before = snapshot(&mut session);
    let present = run(&mut session, json!({"op": "extdata.get", "key": OTHER_KEY}));
    assert_eq!(present, json!({"key": OTHER_KEY, "present": true, "base64": "aGVsbG8="}));
    let absent = run(&mut session, json!({"op": "extdata.get", "key": FLIP_KEY}));
    assert_eq!(absent, json!({"key": FLIP_KEY, "present": false, "base64": null}));
    assert_eq!(snapshot(&mut session), before, "reading changes nothing");

    // A document without an extension block has no entries at all.
    let mut plain = Format::Psd.imported(&dir);
    assert_eq!(run(&mut plain, json!({"op": "extdata.get", "key": OTHER_KEY}))["present"], false);
}

#[test]
fn setting_extension_data_is_one_undo_step_and_survives_save_and_reopen() {
    let dir = temp_dir();
    let (mut session, _) = with_extension_entry(&dir);
    let before = snapshot(&mut session);
    assert_eq!((before["modified"].clone(), before["canUndo"].clone()), (json!(false), json!(false)));
    let textures = saved_pixels(&mut session, &dir);

    let set = run(&mut session, json!({"op": "extdata.set", "key": FLIP_KEY, "base64": b64(b"[1,2]")}));
    assert_eq!(set, json!({"key": FLIP_KEY, "present": true, "bytes": 5, "changed": true}));
    let after = snapshot(&mut session);
    assert_eq!((after["modified"].clone(), after["canUndo"].clone()), (json!(true), json!(true)));
    assert_eq!(after["history"]["count"], 1);
    assert_eq!(after["history"]["states"][1]["label"], "extdata.set");
    assert_eq!(after["model"], before["model"], "the model is untouched");
    assert_eq!(run(&mut session, json!({"op": "extdata.get", "key": FLIP_KEY}))["base64"], "WzEsMl0=");
    assert_eq!(run(&mut session, json!({"op": "extdata.get", "key": OTHER_KEY}))["base64"], "aGVsbG8=");

    // Replace in place keeps the entry order; the saved file carries both entries and the textures.
    run(&mut session, json!({"op": "extdata.set", "key": OTHER_KEY, "base64": b64(b"bye")}));
    assert_eq!(snapshot(&mut session)["history"]["count"], 2);
    let saved = dir.join("saved.inx");
    run(&mut session, json!({"op": "save", "path": saved.to_str().unwrap()}));
    let bytes = fs::read(&saved).unwrap();
    assert_eq!(ext_block(&bytes), encode_ext(&[(OTHER_KEY, b"bye"), (FLIP_KEY, b"[1,2]")]));
    assert_eq!(texture_pixels(&bytes), textures);
    let mut reopened = Session::new();
    let opened = run(&mut reopened, json!({"op": "open", "path": saved.to_str().unwrap()}));
    assert_eq!(opened["modified"], false);
    assert_eq!(run(&mut reopened, json!({"op": "extdata.get", "key": FLIP_KEY}))["base64"], "WzEsMl0=");
    assert_eq!(run(&mut reopened, json!({"op": "extdata.get", "key": OTHER_KEY}))["base64"], "Ynll");

    // Undo restores the old bytes step by step, redo brings them back.
    run(&mut session, json!({"op": "undo"}));
    assert_eq!(run(&mut session, json!({"op": "extdata.get", "key": OTHER_KEY}))["base64"], "aGVsbG8=");
    assert_eq!(run(&mut session, json!({"op": "extdata.get", "key": FLIP_KEY}))["present"], true);
    let undone = run(&mut session, json!({"op": "undo"}));
    assert_eq!(undone["modified"], true, "the saved file has the later bytes");
    assert_eq!(run(&mut session, json!({"op": "extdata.get", "key": FLIP_KEY}))["present"], false);
    assert_eq!(saved_pixels(&mut session, &dir), textures);
    let redone = run(&mut session, json!({"op": "redo"}));
    assert_eq!(redone["canRedo"], true);
    assert_eq!(run(&mut session, json!({"op": "extdata.get", "key": FLIP_KEY}))["base64"], "WzEsMl0=");
    let last = run(&mut session, json!({"op": "redo"}));
    assert_eq!((last["modified"].clone(), last["canRedo"].clone()), (json!(false), json!(false)));
}

#[test]
fn undoing_an_extension_change_makes_the_document_unmodified_again() {
    let dir = temp_dir();
    let (mut session, _) = with_extension_entry(&dir);
    run(&mut session, json!({"op": "extdata.set", "key": FLIP_KEY, "base64": b64(b"x")}));
    assert_eq!(snapshot(&mut session)["modified"], true);
    let undone = run(&mut session, json!({"op": "undo"}));
    assert_eq!((undone["modified"].clone(), undone["canRedo"].clone()), (json!(false), json!(true)));
}

#[test]
fn removing_the_last_entry_drops_the_extension_block_and_a_no_op_records_nothing() {
    let dir = temp_dir();
    let (mut session, _) = with_extension_entry(&dir);
    let before = snapshot(&mut session);

    // Nothing changes: the same bytes, or removing a key that is not there.
    let same = run(&mut session, json!({"op": "extdata.set", "key": OTHER_KEY, "base64": "aGVsbG8="}));
    assert_eq!(same, json!({"key": OTHER_KEY, "present": true, "bytes": 5, "changed": false}));
    let missing = run(&mut session, json!({"op": "extdata.set", "key": FLIP_KEY, "base64": null}));
    assert_eq!(missing, json!({"key": FLIP_KEY, "present": false, "bytes": 0, "changed": false}));
    assert_eq!(snapshot(&mut session), before);

    let removed = run(&mut session, json!({"op": "extdata.set", "key": OTHER_KEY, "base64": null}));
    assert_eq!(removed, json!({"key": OTHER_KEY, "present": false, "bytes": 0, "changed": true}));
    assert_eq!(snapshot(&mut session)["history"]["count"], 1);
    let saved = dir.join("none.inx");
    run(&mut session, json!({"op": "save", "path": saved.to_str().unwrap()}));
    assert_eq!(ext_block(&fs::read(&saved).unwrap()), Vec::<u8>::new());

    // An empty payload is a present key.
    let empty = run(&mut session, json!({"op": "extdata.set", "key": FLIP_KEY, "base64": ""}));
    assert_eq!(empty, json!({"key": FLIP_KEY, "present": true, "bytes": 0, "changed": true}));
    assert_eq!(run(&mut session, json!({"op": "extdata.get", "key": FLIP_KEY})), json!({"key": FLIP_KEY, "present": true, "base64": ""}));
}

#[test]
fn bad_extension_requests_leave_the_session_unchanged() {
    let dir = temp_dir();
    let (mut session, _) = with_extension_entry(&dir);
    let before = snapshot(&mut session);
    let too_long_text = "A".repeat(22_369_700);
    let over_cap_after_decoding = "A".repeat(22_369_624);
    let long_key = "k".repeat(257);
    for request in [
        json!({"op": "extdata.set", "key": FLIP_KEY}),
        json!({"op": "extdata.set", "key": FLIP_KEY, "base64": 5}),
        json!({"op": "extdata.set", "key": FLIP_KEY, "base64": "***"}),
        json!({"op": "extdata.set", "key": "", "base64": "AA=="}),
        json!({"op": "extdata.set", "key": long_key, "base64": "AA=="}),
        json!({"op": "extdata.set", "key": 7, "base64": "AA=="}),
        json!({"op": "extdata.set", "base64": "AA=="}),
        json!({"op": "extdata.set", "key": FLIP_KEY, "base64": too_long_text}),
        json!({"op": "extdata.set", "key": FLIP_KEY, "base64": over_cap_after_decoding}),
        json!({"op": "extdata.get"}),
        json!({"op": "extdata.get", "key": ""}),
        json!({"op": "extdata.get", "key": long_key}),
    ] {
        fail(&mut session, request);
        assert_eq!(snapshot(&mut session), before);
    }
    assert_eq!(
        fail(&mut session, json!({"op": "extdata.set", "key": FLIP_KEY})),
        "base64 is required (null removes the key)"
    );
    assert_eq!(
        fail(&mut session, json!({"op": "extdata.set", "key": FLIP_KEY, "base64": over_cap_after_decoding})),
        "base64 payload is too large"
    );
    // The largest payload is accepted: 16 MiB decoded.
    let largest = b64(&vec![7u8; 16 * 1024 * 1024]);
    let accepted = run(&mut session, json!({"op": "extdata.set", "key": FLIP_KEY, "base64": largest}));
    assert_eq!(accepted["bytes"], 16 * 1024 * 1024);
}

#[test]
fn a_document_with_too_many_entries_or_foreign_trailing_data_is_handled_without_guessing() {
    let dir = temp_dir();
    let mut source = Format::Psd.imported(&dir);
    let names: Vec<String> = (0..4096).map(|index| format!("key{index}")).collect();
    let entries: Vec<(&str, &[u8])> = names.iter().map(|name| (name.as_str(), &b"v"[..])).collect();
    let full = inx_with_tail(&mut source, &dir, "full.inx", &encode_ext(&entries));
    let mut session = Session::new();
    run(&mut session, json!({"op": "open", "path": full}));
    let before = snapshot(&mut session);
    assert!(fail(&mut session, json!({"op": "extdata.set", "key": "one-more", "base64": "AA=="})).contains("4096"));
    assert_eq!(snapshot(&mut session), before);
    // An existing key can still be replaced or removed.
    assert_eq!(run(&mut session, json!({"op": "extdata.set", "key": "key7", "base64": "AA=="}))["changed"], true);
    assert_eq!(run(&mut session, json!({"op": "extdata.set", "key": "key8", "base64": null}))["changed"], true);

    let junk = inx_with_tail(&mut source, &dir, "junk.inx", b"MYSTERY!");
    let mut odd = Session::new();
    run(&mut odd, json!({"op": "open", "path": junk}));
    let before = snapshot(&mut odd);
    assert_eq!(fail(&mut odd, json!({"op": "extdata.get", "key": FLIP_KEY})), "unrecognised data after the texture section");
    assert_eq!(fail(&mut odd, json!({"op": "extdata.set", "key": FLIP_KEY, "base64": "AA=="})), "unrecognised data after the texture section");
    assert_eq!(snapshot(&mut odd), before);
}

#[test]
fn the_extension_tools_round_trip_through_mcp() {
    let dir = temp_dir();
    let (mut session, _) = with_extension_entry(&dir);
    let got = call(&mut session, "extdata_get", json!({"key": OTHER_KEY}));
    assert_eq!(got["structuredContent"], json!({"key": OTHER_KEY, "present": true, "base64": "aGVsbG8="}));
    let set = call(&mut session, "extdata_set", json!({"key": FLIP_KEY, "base64": "WzFd"}));
    assert_eq!(set["structuredContent"], json!({"key": FLIP_KEY, "present": true, "bytes": 3, "changed": true}));
    assert_eq!(call(&mut session, "extdata_get", json!({"key": FLIP_KEY}))["structuredContent"]["base64"], "WzFd");
    let removed = call(&mut session, "extdata_set", json!({"key": FLIP_KEY, "base64": null}));
    assert_eq!(removed["structuredContent"]["present"], false);
    let failed = call(&mut session, "extdata_set", json!({"key": FLIP_KEY}));
    assert_eq!(failed["isError"], true);
}

// ---------------------------------------------------------------------------- texture.get

fn png_bytes(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
    let mut data = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(width, height, image::Rgba(pixel)))
        .write_to(&mut data, image::ImageFormat::Png)
        .unwrap();
    data.into_inner()
}

fn tga_bytes(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
    let mut data = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(width, height, image::Rgba(pixel)))
        .write_to(&mut data, image::ImageFormat::Tga)
        .unwrap();
    data.into_inner()
}

/// An INX file whose texture section is `textures` (tag, data), taken from a saved PSD import.
fn inx_with_textures(dir: &Path, name: &str, textures: &[(u8, Vec<u8>)]) -> String {
    let mut session = Format::Psd.imported(dir);
    let probe = dir.join(format!("probe-{}.inx", SEQUENCE.fetch_add(1, Ordering::Relaxed)));
    run(&mut session, json!({"op": "save", "path": probe.to_str().unwrap(), "mark_saved": false}));
    let bytes = fs::read(probe).unwrap();
    let json_len = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let mut out = bytes[..12 + json_len].to_vec();
    out.extend_from_slice(b"TEX_SECT");
    out.extend((textures.len() as u32).to_be_bytes());
    for (tag, data) in textures {
        out.extend((data.len() as u32).to_be_bytes());
        out.push(*tag);
        out.extend_from_slice(data);
    }
    write(dir, name, &out)
}

#[test]
fn texture_get_returns_one_slot_with_its_decoded_size() {
    let dir = temp_dir();
    let png = png_bytes(3, 2, [10, 20, 30, 255]);
    let tga = tga_bytes(2, 5, [200, 100, 50, 255]);
    let path = inx_with_textures(&dir, "tex.inx", &[(0, png.clone()), (1, tga.clone()), (2, vec![1, 2, 3])]);
    let mut session = Session::new();
    run(&mut session, json!({"op": "open", "path": path}));
    let before = snapshot(&mut session);

    let first = run(&mut session, json!({"op": "texture.get", "slot": 0}));
    assert_eq!(first, json!({"slot": 0, "width": 3, "height": 2, "format": "png", "base64": b64(&png)}));
    let second = run(&mut session, json!({"op": "texture.get", "slot": 1}));
    assert_eq!(second, json!({"slot": 1, "width": 2, "height": 5, "format": "tga", "base64": b64(&tga)}));
    assert_eq!(snapshot(&mut session), before, "reading changes nothing");

    for request in [
        json!({"op": "texture.get", "slot": 3}),
        json!({"op": "texture.get", "slot": 4294967295u64}),
        json!({"op": "texture.get", "slot": -1}),
        json!({"op": "texture.get", "slot": "0"}),
        json!({"op": "texture.get", "slot": 0.5}),
        json!({"op": "texture.get"}),
        json!({"op": "texture.get", "slot": 2}),
    ] {
        fail(&mut session, request);
    }
    assert_eq!(fail(&mut session, json!({"op": "texture.get", "slot": 3})), "texture slot 3 is out of range (3 textures)");
    assert_eq!(fail(&mut session, json!({"op": "texture.get", "slot": 2})), "texture slot 2 has unsupported image tag 2");

    let empty = fail(&mut Session::new(), json!({"op": "texture.get", "slot": 0}));
    assert_eq!(empty, "texture slot 0 is out of range (0 textures)");

    let mcp = call(&mut session, "texture_get", json!({"slot": 1}));
    assert_eq!(mcp["structuredContent"]["format"], "tga");
    assert_eq!(call(&mut session, "texture_get", json!({"slot": 9}))["isError"], true);
}

// ---------------------------------------------------------------------------- model.statistics

#[test]
fn model_statistics_reports_the_deepest_nesting_of_offscreen_passes() {
    let mut session = Session::new();
    let none = json!({"nodes": 1, "maskReferences": 0, "maxOffscreenDepth": 0, "offscreenDepthLimit": 16});
    assert_eq!(run(&mut session, json!({"op": "model.statistics"})), none);

    let create = |session: &mut Session, class: &str, parent: Option<u64>| {
        let mut request = json!({"op": "node.create", "className": class});
        if let Some(parent) = parent {
            request["parents"] = json!([parent]);
        }
        run(session, request)["created"][0]["uuid"].as_u64().unwrap()
    };
    let outer = create(&mut session, "Composite", None);
    let plain = create(&mut session, "Node", Some(outer));
    let middle = create(&mut session, "DynamicComposite", Some(plain));
    create(&mut session, "Composite", Some(middle));
    create(&mut session, "Composite", None);
    let before = snapshot(&mut session);

    let expected = json!({"nodes": 6, "maskReferences": 0, "maxOffscreenDepth": 3, "offscreenDepthLimit": 16});
    let called = call(&mut session, "model_statistics", json!({}));
    assert_eq!(called["isError"], false, "{called}");
    assert_eq!(called["structuredContent"], expected);
    let read = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 3, "method": "resources/read",
            "params": {"uri": "resource://soul2d/document/statistics"}}))
        .unwrap();
    let text = read["result"]["contents"][0]["text"].as_str().unwrap();
    assert_eq!(serde_json::from_str::<Value>(text).unwrap(), expected);
    assert_eq!(snapshot(&mut session), before, "read only");
}

// ---------------------------------------------------------------------------- scene.geometry

/// Flattens a `render.packet` the way the viewport does: Parts in painter order, composites inline.
fn packet_parts(packet: &Value) -> Vec<(u64, Value, Value)> {
    fn walk(scope: usize, packet: &Value, out: &mut Vec<(u64, Value, Value)>) {
        for id in packet["scopes"][scope].as_array().unwrap() {
            let draw = packet["draws"].as_array().unwrap().iter().find(|draw| draw["id"] == *id).unwrap();
            match draw["kind"].as_str().unwrap() {
                "Composite" => walk(draw["scope"].as_u64().unwrap() as usize, packet, out),
                "Part" => out.push((id.as_u64().unwrap(), draw["vertices"].clone(), draw["indices"].clone())),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(0, packet, &mut out);
    out
}

/// The PSD fixture imported with this group node type.
fn drawable(dir: &Path, group: &str) -> Session {
    let mut session = Session::new();
    let path = write(dir, "base.psd", &Format::Psd.base());
    run(&mut session, json!({"op": "import.psd", "path": path, "layerGroupNodeType": group}));
    session
}

fn geometry_summary(geometry: &Value) -> Vec<(u64, Value, Value)> {
    geometry["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| (part["id"].as_u64().unwrap(), part["vertices"].clone(), part["zsort"].clone()))
        .collect()
}

#[test]
fn a_lock_marks_the_parts_below_it_in_scene_geometry_and_is_saved_with_the_file() {
    fn subtree(node: &Value, out: &mut Vec<u64>) {
        out.push(node["uuid"].as_u64().unwrap());
        for child in node["children"].as_array().into_iter().flatten() {
            subtree(child, out);
        }
    }
    let locked_parts = |session: &mut Session| -> Vec<u64> {
        let geometry = run(session, json!({"op": "scene.geometry"}));
        let parts = geometry["parts"].as_array().unwrap();
        parts.iter().filter(|part| part["locked"] == true).map(|part| part["id"].as_u64().unwrap()).collect()
    };
    let dir = temp_dir();
    let mut session = drawable(&dir, "Node");
    let model = snapshot(&mut session)["model"].clone();
    let group = model["nodes"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["children"].as_array().is_some_and(|list| !list.is_empty()))
        .expect("the fixture has a layer group");
    let group_id = group["uuid"].as_u64().unwrap();
    let mut below = Vec::new();
    subtree(group, &mut below);
    let geometry = run(&mut session, json!({"op": "scene.geometry"}));
    let all: Vec<u64> = geometry["parts"].as_array().unwrap().iter().map(|part| part["id"].as_u64().unwrap()).collect();
    let inside: Vec<u64> = all.iter().copied().filter(|id| below.contains(id)).collect();
    assert!(!inside.is_empty() && inside.len() < all.len(), "{inside:?} of {all:?}");
    assert_eq!(locked_parts(&mut session), [0u64; 0]);

    // A lock on the group covers the Parts in it without writing to them.
    let reply = run(&mut session, json!({"op": "node.lock.set", "nodes": [group_id]}));
    assert_eq!(reply, json!({"nodes": [group_id], "locked": true, "recursive": false}));
    assert_eq!(locked_parts(&mut session), inside);
    let model = snapshot(&mut session)["model"].clone();
    let stored = serde_json::to_string(&model).unwrap().matches("\"locked\":true").count();
    assert_eq!(stored, 1, "only the group stores the lock");

    // A lock stops no edit.
    run(&mut session, json!({"op": "node.rename", "id": inside[0], "name": "still editable"}));

    let path = dir.join("locked.inx");
    run(&mut session, json!({"op": "save", "path": path.to_str().unwrap()}));
    let mut reopened = Session::new();
    run(&mut reopened, json!({"op": "open", "path": path.to_str().unwrap()}));
    assert_eq!(locked_parts(&mut reopened), inside, "the lock is saved with the file");

    // The same value again is no undo step; the toggle and undo both bring the lock back off.
    let before = snapshot(&mut session);
    run(&mut session, json!({"op": "node.lock.set", "nodes": [group_id], "locked": true}));
    assert_eq!(snapshot(&mut session), before);
    run(&mut session, json!({"op": "node.lock.set", "nodes": [inside[0]], "siblings": true, "locked": true}));
    run(&mut session, json!({"op": "node.lock.set", "nodes": [group_id], "locked": false}));
    assert_eq!(locked_parts(&mut session), inside, "each Part of the level carries its own lock now");
    run(&mut session, json!({"op": "node.lock.set", "nodes": [group_id], "recursive": true, "locked": false}));
    assert_eq!(locked_parts(&mut session), [0u64; 0]);
    run(&mut session, json!({"op": "undo"}));
    assert_eq!(locked_parts(&mut session), inside);

    fail(&mut session, json!({"op": "node.lock.set", "nodes": [group_id], "locked": "yes"}));
    fail(&mut session, json!({"op": "node.lock.set", "nodes": [987654]}));
    fail(&mut session, json!({"op": "node.lock.set"}));
}

#[test]
fn scene_geometry_lists_drawn_parts_without_textures_exactly_like_the_render_packet() {
    let dir = temp_dir();
    for group in ["Node", "Composite", "DynamicComposite"] {
        let mut session = drawable(&dir, group);
        let before = snapshot(&mut session);
        let geometry = run(&mut session, json!({"op": "scene.geometry"}));
        assert_eq!(snapshot(&mut session), before, "read only");
        assert_eq!(geometry.as_object().unwrap().keys().collect::<Vec<_>>(), ["parts"]);

        let parts = geometry["parts"].as_array().unwrap();
        for part in parts {
            let mut keys: Vec<&String> = part.as_object().unwrap().keys().collect();
            keys.sort();
            assert_eq!(keys, ["id", "indices", "opacity", "vertices", "zsort"]);
            assert_eq!(part["indices"], json!([0, 1, 2, 2, 1, 3]));
        }
        let packet = run(&mut session, json!({"op": "render.packet"}));
        for part in parts {
            let draw = packet["draws"].as_array().unwrap().iter().find(|draw| draw["id"] == part["id"]).unwrap();
            assert_eq!(part["opacity"], draw["material"]["opacity"], "the opacity the packet draws with ({group})");
        }
        let found: Vec<(u64, Value, Value)> = parts
            .iter()
            .map(|part| (part["id"].as_u64().unwrap(), part["vertices"].clone(), part["indices"].clone()))
            .collect();
        assert_eq!(found, packet_parts(&packet), "same evaluation, same painter order ({group})");
    }
}

#[test]
fn scene_geometry_gives_world_space_vertices_painter_order_and_zsort() {
    let dir = temp_dir();
    let mut session = drawable(&dir, "Composite");
    let geometry = run(&mut session, json!({"op": "scene.geometry"}));
    // Ids: Root 1, Face 2, Hat 3 (a Composite holding Brim 4), Body 5. The canvas is 12x12 and the document
    // is centred on it, so Body (canvas x 0..6, y 0..4) sits at x -6..0, y -6..-2. Brim is inside the
    // composite and is listed at the composite's place; larger zsort draws first.
    assert_eq!(
        geometry_summary(&geometry),
        [
            (5, json!([[-6.0, -6.0], [-6.0, -2.0], [0.0, -6.0], [0.0, -2.0]]), json!(5.0)),
            (4, json!([[0.0, -2.0], [0.0, 2.0], [6.0, -2.0], [6.0, 2.0]]), json!(5.0)),
            (2, json!([[-6.0, 2.0], [-6.0, 6.0], [0.0, 2.0], [0.0, 6.0]]), json!(1.0)),
        ],
        "{geometry}"
    );

    // The same evaluation as render.packet: a transform moves the vertices (the translation is absolute and
    // puts Body's centre at (10,0)), a hidden Part leaves the list.
    run(&mut session, json!({"op": "node.transform", "id": 5, "translation": [10.0, 0.0, 0.0]}));
    let moved = run(&mut session, json!({"op": "scene.geometry"}));
    assert_eq!(geometry_summary(&moved)[0].1, json!([[7.0, -2.0], [7.0, 2.0], [13.0, -2.0], [13.0, 2.0]]));
    run(&mut session, json!({"op": "node.enabled.set", "id": 2, "enabled": false}));
    let hidden = run(&mut session, json!({"op": "scene.geometry"}));
    assert_eq!(geometry_summary(&hidden).iter().map(|part| part.0).collect::<Vec<_>>(), [5, 4]);

    // Deformation overrides reach the geometry like they reach the packet.
    let deformed = run(
        &mut session,
        json!({"op": "scene.geometry", "deformations": {"4": [[1.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]}}),
    );
    assert_eq!(geometry_summary(&deformed)[1].1, json!([[1.0, -2.0], [0.0, 2.0], [6.0, -2.0], [6.0, 2.0]]));
    assert!(fail(&mut session, json!({"op": "scene.geometry", "deformations": {"99": []}})).contains("99"));
}

#[test]
fn an_empty_scene_has_no_geometry_and_the_tool_answers_through_mcp() {
    let mut empty = Session::new();
    assert_eq!(run(&mut empty, json!({"op": "scene.geometry"})), json!({"parts": []}));
    let dir = temp_dir();
    let mut session = drawable(&dir, "Node");
    let answer = call(&mut session, "scene_geometry", json!({}));
    assert_eq!(answer["isError"], false, "{answer}");
    assert_eq!(answer["structuredContent"]["parts"].as_array().unwrap().len(), 3);
    assert!(answer["structuredContent"]["parts"][0].get("rgba").is_none());
}

// ---------------------------------------------------------------------------- export.inp

/// Three 6x4 layers on a 12x12 canvas; at resolution 8 two stack in one atlas and the third needs another.
/// Ids: Root 1, C 2, B 3, A 4.
fn three_layer_session(dir: &Path) -> Session {
    let mut session = Session::new();
    let bytes = psd(
        12,
        12,
        &[
            solid("A", [0, 0, 4, 6], [10, 0, 0, 255]),
            solid("B", [4, 0, 8, 6], [0, 10, 0, 255]),
            solid("C", [8, 0, 12, 6], [0, 0, 10, 255]),
        ],
    );
    let path = write(dir, "three.psd", &bytes);
    run(&mut session, json!({"op": "import.psd", "path": path}));
    session
}

fn names_of(model: &Value) -> Vec<String> {
    fn walk(node: &Value, out: &mut Vec<String>) {
        out.push(node["name"].as_str().unwrap().to_owned());
        for child in node["children"].as_array().into_iter().flatten() {
            walk(child, out);
        }
    }
    let mut names = Vec::new();
    walk(&model["nodes"], &mut names);
    names
}

#[test]
fn export_inp_writes_a_runtime_file_and_leaves_the_session_alone() {
    let dir = temp_dir();
    let mut session = three_layer_session(&dir);
    let before = snapshot(&mut session);
    let target = dir.join("out.inp");
    let result = run(
        &mut session,
        json!({"op": "export.inp", "path": target.to_str().unwrap(), "atlasResolution": 8, "padding": 0}),
    );
    let bytes = fs::read(&target).unwrap();
    assert_eq!(
        result,
        json!({
            "path": target.to_str().unwrap(), "bytes": bytes.len(), "atlases": 2, "outputScale": 1.0,
            "prunedNodes": 0, "skippedParts": [], "unbakedDeformers": [],
        })
    );
    assert_eq!(snapshot(&mut session), before, "no history, modified state or revision change");
    assert_eq!(&bytes[..8], b"TRNSRTS\0");

    // The runtime file opens as a document of its own: the same Parts, the two atlas textures only.
    let mut runtime = Session::new();
    let opened = run(&mut runtime, json!({"op": "open", "path": target.to_str().unwrap()}));
    assert_eq!(names_of(&opened["model"]), ["Root", "A", "B", "C"]);
    assert_eq!(texture_count(&bytes), 2);
    // A and B share the first atlas, C needs the second.
    let slots: Vec<Value> = (0..3).map(|child| opened["model"]["nodes"]["children"][child]["textures"].clone()).collect();
    assert_eq!(slots, [json!([0, u32::MAX, u32::MAX]), json!([0, u32::MAX, u32::MAX]), json!([1, u32::MAX, u32::MAX])]);

    // The session still has its own path and unsaved state: saving works as before.
    let saved = dir.join("work.inx");
    run(&mut session, json!({"op": "save", "path": saved.to_str().unwrap()}));
    assert_eq!(snapshot(&mut session)["modified"], false);
    assert_eq!(texture_pixels(&fs::read(&saved).unwrap()).len(), 3);
}

#[test]
fn export_inp_forces_the_extension_and_never_replaces_a_file_unless_asked() {
    let dir = temp_dir();
    let mut session = three_layer_session(&dir);
    let named = dir.join("model.inx");
    let result = run(&mut session, json!({"op": "export.inp", "path": named.to_str().unwrap(), "atlasResolution": 8, "padding": 0}));
    let forced = dir.join("model.inp");
    assert_eq!(result["path"], forced.to_str().unwrap());
    assert!(forced.exists() && !named.exists());
    let bare = run(&mut session, json!({"op": "export.inp", "path": dir.join("bare").to_str().unwrap(), "padding": 0}));
    assert_eq!(bare["path"], dir.join("bare.inp").to_str().unwrap());
    assert!(dir.join("bare.inp").exists());

    // An existing file stays as it is without `overwrite`.
    fs::write(&forced, b"precious").unwrap();
    for request in [
        json!({"op": "export.inp", "path": forced.to_str().unwrap()}),
        json!({"op": "export.inp", "path": named.to_str().unwrap(), "overwrite": false}),
    ] {
        assert_eq!(fail(&mut session, request), "export destination already exists");
        assert_eq!(fs::read(&forced).unwrap(), b"precious");
    }
    let replaced = run(&mut session, json!({"op": "export.inp", "path": forced.to_str().unwrap(), "overwrite": true, "padding": 0}));
    assert_eq!(fs::read(&forced).unwrap().len() as u64, replaced["bytes"].as_u64().unwrap());
    assert_eq!(&fs::read(&forced).unwrap()[..8], b"TRNSRTS\0");
    let leftovers: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert_eq!(leftovers, Vec::<String>::new());

    // The open document's own path is never overwritten, even with `overwrite`.
    let mut runtime = Session::new();
    run(&mut runtime, json!({"op": "open", "path": forced.to_str().unwrap()}));
    let before = fs::read(&forced).unwrap();
    assert_eq!(
        fail(&mut runtime, json!({"op": "export.inp", "path": forced.to_str().unwrap(), "overwrite": true})),
        "export.inp would overwrite the open document"
    );
    assert_eq!(fs::read(&forced).unwrap(), before);
}

#[test]
fn export_inp_options_prune_and_scale_reach_the_report() {
    let dir = temp_dir();
    let mut session = three_layer_session(&dir);
    run(&mut session, json!({"op": "node.enabled.set", "id": 3, "enabled": false}));
    let before = snapshot(&mut session);

    // A disabled Part still takes atlas space but leaves the file; the report counts it.
    let pruned = run(
        &mut session,
        json!({"op": "export.inp", "path": dir.join("pruned").to_str().unwrap(), "atlasResolution": 8, "padding": 0}),
    );
    assert_eq!((pruned["atlases"].clone(), pruned["prunedNodes"].clone()), (json!(2), json!(1)));
    let mut runtime = Session::new();
    let opened = run(&mut runtime, json!({"op": "open", "path": dir.join("pruned.inp").to_str().unwrap()}));
    assert_eq!(names_of(&opened["model"]), ["Root", "A", "C"]);
    let kept = run(
        &mut session,
        json!({"op": "export.inp", "path": dir.join("kept").to_str().unwrap(), "atlasResolution": 8, "padding": 0,
            "optimizePruneUnused": false}),
    );
    assert_eq!(kept["prunedNodes"], 0);
    let mut runtime = Session::new();
    let opened = run(&mut runtime, json!({"op": "open", "path": dir.join("kept.inp").to_str().unwrap()}));
    assert_eq!(names_of(&opened["model"]), ["Root", "A", "B", "C"]);

    // 6 wide parts in a 6 pixel atlas force D's uniform output scale unless scaling is non-linear.
    let linear = run(
        &mut session,
        json!({"op": "export.inp", "path": dir.join("linear").to_str().unwrap(), "atlasResolution": 6, "padding": 0}),
    );
    assert_eq!(linear["outputScale"], f64::from(1.0f32 - 0.015f32));
    let non_linear = run(
        &mut session,
        json!({"op": "export.inp", "path": dir.join("nonlinear").to_str().unwrap(), "atlasResolution": 6,
            "padding": 0, "nonLinearScaling": true}),
    );
    assert_eq!(non_linear["outputScale"], 1.0);
    assert_eq!(snapshot(&mut session), before);
}

#[test]
fn export_inp_passes_the_extension_section_through_and_follows_extension_edits() {
    let dir = temp_dir();
    let (mut session, _) = with_extension_entry(&dir);
    run(&mut session, json!({"op": "extdata.set", "key": FLIP_KEY, "base64": b64(b"[1]")}));
    let before = snapshot(&mut session);
    let target = dir.join("ext.inp");
    run(&mut session, json!({"op": "export.inp", "path": target.to_str().unwrap(), "atlasResolution": 16, "padding": 0}));
    let bytes = fs::read(&target).unwrap();
    assert_eq!(ext_block(&bytes), encode_ext(&[(OTHER_KEY, b"hello"), (FLIP_KEY, b"[1]")]));
    let mut runtime = Session::new();
    run(&mut runtime, json!({"op": "open", "path": target.to_str().unwrap()}));
    assert_eq!(run(&mut runtime, json!({"op": "extdata.get", "key": FLIP_KEY}))["base64"], "WzFd");
    assert_eq!(snapshot(&mut session), before);

    // After the entries are gone the export carries no extension block.
    run(&mut session, json!({"op": "extdata.set", "key": FLIP_KEY, "base64": null}));
    run(&mut session, json!({"op": "extdata.set", "key": OTHER_KEY, "base64": null}));
    let bare = dir.join("bare.inp");
    run(&mut session, json!({"op": "export.inp", "path": bare.to_str().unwrap(), "atlasResolution": 16, "padding": 0}));
    assert_eq!(ext_block(&fs::read(&bare).unwrap()), Vec::<u8>::new());
}

#[test]
fn bad_export_requests_write_nothing_and_leave_the_session_unchanged() {
    let dir = temp_dir();
    let mut session = three_layer_session(&dir);
    let before = snapshot(&mut session);
    let out = |name: &str| dir.join(name).to_str().unwrap().to_owned();
    for (request, expected) in [
        (json!({"op": "export.inp"}), "path must be a string"),
        (json!({"op": "export.inp", "path": out("a"), "atlasResolution": 9000}), ""),
        (json!({"op": "export.inp", "path": out("b"), "atlasResolution": 0}), ""),
        (json!({"op": "export.inp", "path": out("c"), "atlasResolution": "big"}), ""),
        (json!({"op": "export.inp", "path": out("d"), "padding": -1}), ""),
        (json!({"op": "export.inp", "path": out("e"), "scale": 0}), ""),
        (json!({"op": "export.inp", "path": out("f"), "scale": 17}), ""),
        (json!({"op": "export.inp", "path": out("g"), "nonLinearScaling": 1}), ""),
        (json!({"op": "export.inp", "path": out("h"), "optimizePruneUnused": "yes"}), ""),
        (json!({"op": "export.inp", "path": out("i"), "overwrite": "yes"}), "overwrite must be a boolean"),
        (json!({"op": "export.inp", "path": dir.join("missing-dir").join("x").to_str().unwrap()}), ""),
        (json!({"op": "export.inp", "path": format!("{}/", dir.to_str().unwrap())}), ""),
    ] {
        let message = fail(&mut session, request.clone());
        assert!(message.contains(expected), "{request}: {message}");
        assert_eq!(snapshot(&mut session), before);
    }
    let files: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files, ["three.psd"], "nothing was written");
    assert!(!dir.with_extension("inp").exists(), "no file next to the directory either");

    let mut empty = Session::new();
    assert_eq!(
        fail(&mut empty, json!({"op": "export.inp", "path": out("empty")})),
        "no Part with an albedo texture to export"
    );
    assert!(!dir.join("empty.inp").exists());
}

#[test]
fn export_inp_runs_through_mcp() {
    let dir = temp_dir();
    let mut session = three_layer_session(&dir);
    let target = dir.join("mcp.inp");
    let answer = call(
        &mut session,
        "export_inp",
        json!({"path": target.to_str().unwrap(), "atlasResolution": 8, "padding": 0}),
    );
    assert_eq!(answer["isError"], false, "{answer}");
    assert_eq!(answer["structuredContent"]["atlases"], 2);
    assert_eq!(answer["structuredContent"]["bytes"], fs::metadata(&target).unwrap().len());
    let again = call(&mut session, "export_inp", json!({"path": target.to_str().unwrap()}));
    assert_eq!(again["isError"], true);
    assert_eq!(again["content"][0]["text"], "export destination already exists");
}

#[test]
fn export_inp_writes_the_finished_deform_refresh_while_one_is_pending() {
    let dir = temp_dir();
    let mut session = three_layer_session(&dir);
    let rig = run(&mut session, json!({"op": "node.create", "className": "DepthRigRoot", "name": "Rig"}));
    let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
    let bone = run(&mut session, json!({"op": "node.create", "className": "DepthBone", "name": "Bone",
        "parents": [rig_id], "restHead": [0.0, 0.0, 0.0], "restTail": [0.0, 100.0, 0.0]}));
    let bone_id = bone["created"][0]["uuid"].as_u64().unwrap();
    let grid = run(&mut session, json!({"op": "node.create", "className": "GridDeformer", "name": "Big"}));
    let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
    let axis_x: Vec<f64> = (0..400).map(|i| i as f64 * 0.01).collect();
    let axis_y: Vec<f64> = (0..500).map(|i| i as f64 * 0.01).collect();
    run(&mut session, json!({"op": "deformer.define_grid", "nodes": [grid_id], "axisX": axis_x, "axisY": axis_y}));
    run(&mut session, json!({"op": "rig.add_source", "root": rig_id, "target": grid_id, "bone": bone_id}));
    let parameter = run(&mut session, json!({"op": "parameter.create", "name": "Pose", "dimensions": 1,
        "min": [-1.0, 0.0], "max": [1.0, 1.0]}));
    let parameter_id = parameter["uuid"].as_u64().unwrap();
    run(&mut session, json!({"op": "binding.set_trs", "parameter": parameter_id, "nodes": [bone_id],
        "key": [2, 0], "translation": [4.0, 0.0]}));
    run(&mut session, json!({"op": "node.transform", "id": bone_id, "translation": [12.0, 0.0, 0.0]}));
    let pending = snapshot(&mut session);
    assert_eq!(pending["deformPending"], true);

    let early = dir.join("early.inp");
    run(&mut session, json!({"op": "export.inp", "path": early.to_str().unwrap(), "atlasResolution": 8, "padding": 0}));
    assert_eq!(snapshot(&mut session), pending, "the export does not advance the refresh");
    while snapshot(&mut session)["deformPending"] == true {
        run(&mut session, json!({"op": "deform.continue"}));
    }
    let late = dir.join("late.inp");
    run(&mut session, json!({"op": "export.inp", "path": late.to_str().unwrap(), "atlasResolution": 8, "padding": 0}));
    assert_eq!(fs::read(&early).unwrap(), fs::read(&late).unwrap());
}
