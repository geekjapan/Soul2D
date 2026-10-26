//! Session operations for PSD import, PSD merge and atlas planning (`import.psd`, `merge.psd.plan`,
//! `merge.psd.apply`, `atlas.build`), including undo of a merge that replaces textures.

mod common;

use serde_json::{json, Value};
use soul2d_core::editor_controller::{EditorController, UNSAVED_REPLACEMENT_MESSAGE};
use soul2d_core::Session;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------- PSD builder
// The same layout tests/model_import.rs builds; only what these fixtures need.

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
    opacity: u8,
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
        opacity: 255,
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
    L { name, rect, channels, opacity: 255, lsct: None }
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
    out.extend([layer.opacity, 0, 0, 0]);
    let mut extra = be32(0).to_vec(); // no mask
    extra.extend(be32(0)); // no blending ranges
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

// ---------------------------------------------------------------------------- fixtures

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "soul2d-import-session-{}-{}",
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

/// 12x12 canvas, top to bottom: Face (6x4), group Hat { Brim (6x4) }, Body (6x4).
fn base_psd() -> Vec<u8> {
    psd(
        12,
        12,
        &[
            solid("Body", [0, 0, 4, 6], [200, 100, 50, 255]),
            marker("</Layer group>", 3),
            solid("Brim", [4, 6, 8, 12], [10, 20, 30, 255]),
            marker("Hat", 1),
            solid("Face", [8, 0, 12, 6], [1, 2, 3, 255]),
        ],
    )
}

/// Replaces Body and Brim (same paths) and adds Extra.
fn merge_psd_bytes() -> Vec<u8> {
    psd(
        12,
        12,
        &[
            solid("Body", [0, 0, 4, 6], [9, 8, 7, 255]),
            marker("</Layer group>", 3),
            solid("Brim", [4, 6, 8, 12], [11, 12, 13, 255]),
            marker("Hat", 1),
            solid("Extra", [8, 6, 12, 12], [5, 5, 5, 255]),
        ],
    )
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

fn imported(dir: &Path) -> Session {
    let mut session = Session::new();
    let path = write(dir, "base.psd", &base_psd());
    run(&mut session, json!({"op": "import.psd", "path": path}));
    session
}

/// First pixel of every texture slot in the INX bytes.
fn texture_pixels(bytes: &[u8]) -> Vec<[u8; 4]> {
    assert_eq!(&bytes[..8], b"TRNSRTS\0");
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

// ---------------------------------------------------------------------------- import.psd

#[test]
fn importing_a_psd_replaces_the_session_like_new() {
    let dir = temp_dir();
    let mut session = Session::new();
    run(&mut session, json!({"op": "parameter.create", "dimensions": 1, "name": "Old"}));
    let before = run(&mut session, json!({"op": "snapshot"}));
    assert_eq!(before["canUndo"], true);

    let path = write(&dir, "base.psd", &base_psd());
    let result = run(&mut session, json!({"op": "import.psd", "path": path}));
    assert_eq!(result["imported"], json!({"nodes": 4, "textures": 3}));
    assert_eq!(result["revision"], before["revision"].as_u64().unwrap() + 1);
    assert_eq!((result["modified"].clone(), result["canUndo"].clone()), (json!(false), json!(false)));
    assert_eq!(result["model"]["param"], json!([]));
    assert_eq!(result["model"]["nodes"]["name"], "Root");
    assert_eq!(part_names(&result), ["Body", "Brim", "Face"]);
    assert_eq!(result["model"]["nodes"]["children"][1]["type"], "DynamicComposite");
    assert_eq!(saved_pixels(&mut session, &dir), [[200, 100, 50, 255], [10, 20, 30, 255], [1, 2, 3, 255]]);

    // Options: flat structure, and another group node type.
    let flat = run(&mut session, json!({"op": "import.psd", "path": path, "keepStructure": false}));
    assert_eq!(flat["imported"], json!({"nodes": 3, "textures": 3}));
    let composite = run(&mut session, json!({"op": "import.psd", "path": path, "layerGroupNodeType": "Composite"}));
    assert_eq!(composite["model"]["nodes"]["children"][1]["type"], "Composite");
}

#[test]
fn a_bad_import_leaves_the_session_untouched() {
    let dir = temp_dir();
    let mut session = imported(&dir);
    let before = run(&mut session, json!({"op": "snapshot"}));
    let garbage = write(&dir, "garbage.psd", b"not a photoshop file at all");
    let base = write(&dir, "base2.psd", &base_psd());
    for request in [
        json!({"op": "import.psd", "path": garbage}),
        json!({"op": "import.psd", "path": dir.to_str().unwrap()}),
        json!({"op": "import.psd", "path": dir.join("missing.psd").to_str().unwrap()}),
        json!({"op": "import.psd"}),
        json!({"op": "import.psd", "path": base, "keepStructure": "yes"}),
        json!({"op": "import.psd", "path": base, "layerGroupNodeType": "Part"}),
    ] {
        fail(&mut session, request);
        assert_eq!(run(&mut session, json!({"op": "snapshot"})), before);
    }
    assert!(fail(&mut session, json!({"op": "import.psd", "path": dir.to_str().unwrap()})).contains("regular file"));
}

#[test]
fn the_editor_refuses_an_import_over_unsaved_changes_like_new_and_open() {
    let dir = temp_dir();
    let path = write(&dir, "base.psd", &base_psd());
    let mut controller = EditorController::new();
    let first = controller.core_request(&json!({"op": "import.psd", "path": path}));
    assert_eq!(first["succeeded"], true, "{first}");
    controller.core_request(&json!({"op": "parameter.create", "dimensions": 1, "name": "Edit"}));
    assert_eq!(controller.snapshot()["modified"], true);

    let refused = controller.core_request(&json!({"op": "import.psd", "path": path}));
    assert_eq!(refused["message"], UNSAVED_REPLACEMENT_MESSAGE);
    let via_mcp = controller
        .mcp_request(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "import_psd", "arguments": {"path": path}}}))
        .unwrap();
    assert_eq!(via_mcp["result"]["isError"], true);
    assert_eq!(via_mcp["result"]["content"][0]["text"], UNSAVED_REPLACEMENT_MESSAGE);
    assert_eq!(controller.snapshot()["model"]["param"].as_array().unwrap().len(), 1, "document kept");

    let saved = dir.join("saved.inx");
    let response = controller.core_request(&json!({"op": "save", "path": saved.to_str().unwrap()}));
    assert_eq!(response["succeeded"], true, "{response}");
    let again = controller.mcp_request(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "import_psd", "arguments": {"path": path, "keepStructure": false}}}))
        .unwrap();
    assert_eq!(again["result"]["isError"], false, "{again}");
    assert_eq!(again["result"]["structuredContent"]["imported"], json!({"nodes": 3, "textures": 3}));
}

// ---------------------------------------------------------------------------- merge

#[test]
fn the_merge_plan_lists_layers_with_their_parts_and_changes_nothing() {
    let dir = temp_dir();
    let mut session = imported(&dir);
    let before = run(&mut session, json!({"op": "snapshot"}));
    let path = write(&dir, "merge.psd", &merge_psd_bytes());
    let plan = run(&mut session, json!({"op": "merge.psd.plan", "path": path}));
    assert_eq!(plan["document"], json!({"width": 12, "height": 12}));
    // Ids follow D's top-down creation: Root 1, Face 2, Hat 3, Brim 4, Body 5.
    assert_eq!(
        plan["layers"],
        json!([
            {"index": 0, "name": "Extra", "path": "/Extra", "rect": [6, 8, 6, 4], "target": null, "matchedBy": null},
            {"index": 1, "name": "Brim", "path": "/Hat/Brim", "rect": [6, 4, 6, 4], "target": 4, "matchedBy": "path"},
            {"index": 2, "name": "Body", "path": "/Body", "rect": [0, 0, 6, 4], "target": 5, "matchedBy": "path"},
        ])
    );
    assert_eq!(plan["unmappedParts"], json!([2]));
    assert_eq!(run(&mut session, json!({"op": "snapshot"})), before);
    assert_eq!(saved_pixels(&mut session, &dir).len(), 3);

    assert!(fail(&mut session, json!({"op": "merge.psd.plan", "path": path, "renameMapped": 1})).contains("renameMapped"));
    assert!(fail(&mut session, json!({"op": "merge.psd.plan", "path": dir.to_str().unwrap()})).contains("regular file"));
}

#[test]
fn a_linked_psd_relinks_without_a_path_and_hides_or_removes_parts_whose_layer_is_gone() {
    let dir = temp_dir();
    let mut session = imported(&dir);
    let source = dir.join("base.psd").to_str().unwrap().to_owned();
    assert_eq!(run(&mut session, json!({"op": "snapshot"}))["model"]["meta"]["psdSource"], source);
    // The artist edits the PSD in place: Face is gone, Extra is new.
    write(&dir, "base.psd", &merge_psd_bytes());
    let plan = run(&mut session, json!({"op": "merge.psd.plan"}));
    assert_eq!(plan["source"], source);
    assert_eq!(plan["missingLayers"], json!([{"part": 2, "path": "/Face"}]));
    let hidden = run(&mut session, json!({"op": "merge.psd.apply", "removed": "hide"}));
    assert_eq!((hidden["missingLayers"].clone(), hidden["removed"].clone()), (json!([2]), json!("hide")));
    let snapshot = run(&mut session, json!({"op": "snapshot"}));
    let face = snapshot["model"]["nodes"]["children"].as_array().unwrap().iter().find(|node| node["uuid"] == 2).unwrap();
    assert_eq!((face["name"].clone(), face["enabled"].clone()), (json!("Face"), json!(false)));
    run(&mut session, json!({"op": "undo"}));
    run(&mut session, json!({"op": "merge.psd.apply", "removed": "remove"}));
    assert_eq!(part_names(&run(&mut session, json!({"op": "snapshot"}))), ["Body", "Brim", "Extra"]);
    run(&mut session, json!({"op": "undo"}));
    // A gone Part with children is hidden, not removed, so the children stay.
    let body = run(&mut session, json!({"op": "snapshot"}))["model"]["nodes"]["children"].as_array().unwrap().iter()
        .find(|node| node["name"] == "Body").unwrap()["uuid"].clone();
    run(&mut session, json!({"op": "node.move", "ids": [body], "parent": 2}));
    run(&mut session, json!({"op": "merge.psd.apply", "removed": "remove"}));
    let snapshot = run(&mut session, json!({"op": "snapshot"}));
    assert_eq!(part_names(&snapshot), ["Brim", "Face", "Body", "Extra"]);
    let face = snapshot["model"]["nodes"]["children"].as_array().unwrap().iter().find(|node| node["uuid"] == 2).unwrap();
    assert_eq!(face["enabled"], false);
    run(&mut session, json!({"op": "undo"}));
    run(&mut session, json!({"op": "undo"}));
    run(&mut session, json!({"op": "merge.psd.apply", "removed": "remove"}));
    let moved = write(&dir, "moved.psd", &merge_psd_bytes());
    run(&mut session, json!({"op": "merge.psd.apply", "path": moved}));
    assert_eq!(run(&mut session, json!({"op": "snapshot"}))["model"]["meta"]["psdSource"], source, "a merge keeps the link");
    run(&mut session, json!({"op": "merge.psd.apply", "path": moved, "link": true}));
    assert_eq!(run(&mut session, json!({"op": "snapshot"}))["model"]["meta"]["psdSource"], moved, "link relinks");
    assert!(fail(&mut session, json!({"op": "merge.psd.apply", "removed": "drop"})).contains("keep, hide or remove"));
    let mut blank = Session::new();
    run(&mut blank, json!({"op": "new"}));
    assert!(fail(&mut blank, json!({"op": "merge.psd.plan"})).contains("not linked to a PSD"));
    assert!(fail(&mut blank, json!({"op": "merge.psd.apply"})).contains("not linked to a PSD"));
}

#[test]
fn a_merge_is_one_undo_step_that_swaps_textures_with_the_model() {
    let dir = temp_dir();
    let mut session = imported(&dir);
    let before = run(&mut session, json!({"op": "snapshot"}));
    let old = [[200, 100, 50, 255], [10, 20, 30, 255], [1, 2, 3, 255]];
    assert_eq!(saved_pixels(&mut session, &dir), old);

    let path = write(&dir, "merge.psd", &merge_psd_bytes());
    let result = run(&mut session, json!({"op": "merge.psd.apply", "path": path}));
    assert_eq!(result, json!({"created": [6], "replaced": [4, 5], "textures": 4, "missingLayers": [], "removed": "keep"}));
    let merged = run(&mut session, json!({"op": "snapshot"}));
    let new = [[9, 8, 7, 255], [11, 12, 13, 255], [1, 2, 3, 255], [5, 5, 5, 255]];
    assert_eq!(saved_pixels(&mut session, &dir), new);
    assert_eq!(part_names(&merged), ["Body", "Brim", "Face", "Extra"]);
    assert_eq!(merged["history"]["count"], 1);
    assert_eq!(merged["history"]["states"][1]["label"], "merge.psd.apply");
    assert_eq!((merged["modified"].clone(), merged["canUndo"].clone()), (json!(true), json!(true)));

    let undone = run(&mut session, json!({"op": "undo"}));
    assert_eq!(undone["model"], before["model"]);
    assert_eq!((undone["modified"].clone(), undone["canUndo"].clone(), undone["canRedo"].clone()), (json!(false), json!(false), json!(true)));
    assert_eq!(saved_pixels(&mut session, &dir), old, "old textures are back");

    let redone = run(&mut session, json!({"op": "redo"}));
    assert_eq!(redone["model"], merged["model"]);
    assert_eq!(redone["modified"], true);
    assert_eq!(saved_pixels(&mut session, &dir), new, "new textures are back");

    // Save, reopen in a fresh session: the same four textures and parts.
    let saved = dir.join("merged.inx");
    run(&mut session, json!({"op": "save", "path": saved.to_str().unwrap()}));
    assert_eq!(texture_pixels(&fs::read(&saved).unwrap()), new);
    let mut reopened = Session::new();
    let opened = run(&mut reopened, json!({"op": "open", "path": saved.to_str().unwrap()}));
    assert_eq!((opened["modified"].clone(), part_names(&opened)), (json!(false), vec!["Body".to_owned(), "Brim".into(), "Face".into(), "Extra".into()]));
    assert_eq!(saved_pixels(&mut reopened, &dir), new);
}

#[test]
fn a_merge_that_only_changes_pixels_still_records_an_undo_step() {
    let dir = temp_dir();
    let mut session = imported(&dir);
    let path = write(&dir, "body.psd", &psd(12, 12, &[solid("Body", [0, 0, 4, 6], [77, 66, 55, 255])]));
    let before = run(&mut session, json!({"op": "snapshot"}));
    let result = run(&mut session, json!({"op": "merge.psd.apply", "path": path}));
    assert_eq!(result, json!({"created": [], "replaced": [5], "textures": 3, "missingLayers": [], "removed": "keep"}));
    let merged = run(&mut session, json!({"op": "snapshot"}));
    assert_eq!(merged["model"], before["model"], "the model is identical; only a texture changed");
    assert_eq!(merged["history"]["count"], 1);
    assert_eq!(merged["modified"], true);
    assert_eq!(saved_pixels(&mut session, &dir)[0], [77, 66, 55, 255]);
    run(&mut session, json!({"op": "undo"}));
    assert_eq!(saved_pixels(&mut session, &dir)[0], [200, 100, 50, 255]);
}

#[test]
fn per_layer_choices_and_the_rename_and_retranslate_options_reach_the_model() {
    let dir = temp_dir();
    let mut session = imported(&dir);
    let path = write(&dir, "merge.psd", &merge_psd_bytes());
    // Extra replaces Face (part 2) and renames it; Brim is ignored; Body is added as a second Part.
    let result = run(
        &mut session,
        json!({"op": "merge.psd.apply", "path": path, "renameMapped": true, "retranslateMapped": true,
            "choices": [
                {"layer": 0, "action": "replace", "part": 2},
                {"layer": 1, "action": "ignore"},
                {"layer": 2, "action": "add"},
            ]}),
    );
    assert_eq!(result, json!({"created": [6], "replaced": [2], "textures": 4, "missingLayers": [], "removed": "keep"}));
    let snapshot = run(&mut session, json!({"op": "snapshot"}));
    assert_eq!(part_names(&snapshot), ["Body", "Brim", "Extra", "Body"]);
    // Face (1,2,3) is gone, Brim kept, Extra and the added Body are the new pixels.
    assert_eq!(
        saved_pixels(&mut session, &dir),
        [[200, 100, 50, 255], [10, 20, 30, 255], [5, 5, 5, 255], [9, 8, 7, 255]]
    );

    // Ignoring everything changes nothing and records nothing.
    let before = run(&mut session, json!({"op": "snapshot"}));
    let none = run(
        &mut session,
        json!({"op": "merge.psd.apply", "path": path,
            "choices": [{"layer": 0, "action": "ignore"}, {"layer": 1, "action": "ignore"}, {"layer": 2, "action": "ignore"}]}),
    );
    assert_eq!(none, json!({"created": [], "replaced": [], "textures": 4, "missingLayers": [], "removed": "keep"}));
    assert_eq!(run(&mut session, json!({"op": "snapshot"})), before);

    for choices in [json!({}), json!([{"layer": 9, "action": "add"}]), json!([{"layer": 0, "action": "replace", "part": 999}])] {
        fail(&mut session, json!({"op": "merge.psd.apply", "path": path, "choices": choices}));
        assert_eq!(run(&mut session, json!({"op": "snapshot"})), before, "a refused merge changes nothing");
    }
}

#[test]
fn merging_needs_an_inx_document_and_a_regular_file() {
    let dir = temp_dir();
    let mut session = imported(&dir);
    let garbage = write(&dir, "garbage.psd", b"nope");
    let before = run(&mut session, json!({"op": "snapshot"}));
    fail(&mut session, json!({"op": "merge.psd.apply", "path": garbage}));
    fail(&mut session, json!({"op": "merge.psd.apply", "path": dir.to_str().unwrap()}));
    assert_eq!(run(&mut session, json!({"op": "snapshot"})), before);
}

// ---------------------------------------------------------------------------- MCP

#[test]
fn the_mcp_tools_are_listed_and_call_the_session_operations() {
    let dir = temp_dir();
    let mut session = Session::new();
    let listed = session.handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    let tools = listed["result"]["tools"].as_array().unwrap();
    for (name, required) in [
        ("import_psd", json!(["path"])),
        ("merge_psd_plan", json!([])),
        ("merge_psd_apply", json!([])),
        ("atlas_build", json!([])),
    ] {
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap_or_else(|| panic!("{name} is listed"));
        assert_eq!(tool["inputSchema"]["required"], required, "{name}");
    }

    let call = |session: &mut Session, name: &str, arguments: Value| {
        session
            .handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": name, "arguments": arguments}}))
            .unwrap()["result"]
            .clone()
    };
    let path = write(&dir, "base.psd", &base_psd());
    let imported = call(&mut session, "import_psd", json!({"path": path}));
    assert_eq!(imported["isError"], false, "{imported}");
    assert_eq!(imported["structuredContent"]["imported"], json!({"nodes": 4, "textures": 3}));
    let merge = write(&dir, "merge.psd", &merge_psd_bytes());
    let plan = call(&mut session, "merge_psd_plan", json!({"path": merge}));
    assert_eq!(plan["structuredContent"]["layers"][2]["target"], 5);
    let applied = call(&mut session, "merge_psd_apply", json!({"path": merge}));
    assert_eq!(applied["structuredContent"], json!({"created": [6], "replaced": [4, 5], "textures": 4, "missingLayers": [], "removed": "keep"}));
    let atlas = call(&mut session, "atlas_build", json!({"resolution": 16, "padding": 0}));
    assert_eq!(atlas["isError"], false, "{atlas}");
    assert_eq!(atlas["structuredContent"]["atlasCount"], 1);
}

// ---------------------------------------------------------------------------- atlas.build

/// Three 6x4 layers on a 12x12 canvas; at resolution 8 two stack in one atlas and the third needs another.
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

#[test]
fn the_atlas_plan_places_parts_and_rewrites_uvs_without_touching_the_document() {
    let dir = temp_dir();
    let mut session = three_layer_session(&dir);
    let before = run(&mut session, json!({"op": "snapshot"}));
    let plan = run(&mut session, json!({"op": "atlas.build", "resolution": 8, "padding": 0}));
    assert_eq!(run(&mut session, json!({"op": "snapshot"})), before);

    assert_eq!(plan["resolution"], 8);
    assert_eq!(plan["atlasCount"], 2);
    assert_eq!(plan["outputScale"], 1.0);
    assert_eq!(plan["skipped"], json!([]));
    // Ids: Root 1, C 2, B 3, A 4 (top-down creation). Parts are packed largest first, ties in tree order.
    let parts = plan["parts"].as_array().unwrap();
    let ids: Vec<u64> = parts.iter().map(|part| part["id"].as_u64().unwrap()).collect();
    assert_eq!(ids, [4, 3, 2]);
    let placed: Vec<(u64, Value)> = parts.iter().map(|part| (part["atlas"].as_u64().unwrap(), part["mapping"].clone())).collect();
    assert_eq!(placed, [(0, json!([0, 0, 6, 4])), (0, json!([0, 4, 6, 4])), (1, json!([0, 0, 6, 4]))]);
    // Texture-space UVs of the full quad, normalised to the 8x8 atlas.
    assert_eq!(parts[0]["uvs"], json!([[0.0, 0.0], [0.0, 0.5], [0.75, 0.0], [0.75, 0.5]]));
    assert_eq!(parts[1]["uvs"], json!([[0.0, 0.5], [0.0, 1.0], [0.75, 0.5], [0.75, 1.0]]));

    // Explicit parts: only those, in tree order; a disabled Part is skipped by default but allowed by name.
    let only = run(&mut session, json!({"op": "atlas.build", "resolution": 8, "padding": 0, "parts": [2, 4]}));
    assert_eq!(only["atlasCount"], 1);
    assert_eq!(only["parts"].as_array().unwrap().len(), 2);
    run(&mut session, json!({"op": "node.enabled.set", "id": 3, "enabled": false}));
    let pruned = run(&mut session, json!({"op": "atlas.build", "resolution": 8, "padding": 0}));
    assert_eq!(pruned["atlasCount"], 1);
    assert_eq!(pruned["skipped"], json!([{"id": 3, "reason": "disabled"}]));
    let named = run(&mut session, json!({"op": "atlas.build", "resolution": 8, "padding": 0, "parts": [3]}));
    assert_eq!(named["parts"].as_array().unwrap().len(), 1);
}

#[test]
fn manual_atlas_places_are_kept_and_previewed() {
    let dir = temp_dir();
    let mut session = three_layer_session(&dir);
    let set = run(&mut session, json!({"op": "atlas.layout.set", "parts": [{"id": 2, "atlas": 0, "x": 9, "y": 10}]}));
    assert_eq!(set["atlasLayout"], json!({"parts": {"2": {"atlas": 0, "x": 9, "y": 10}}}));
    let plan = run(&mut session, json!({"op": "atlas.build", "resolution": 16, "padding": 0, "preview": 0}));
    let parts = plan["parts"].as_array().unwrap();
    let pinned = parts.iter().find(|part| part["id"] == 2).unwrap();
    assert_eq!((pinned["atlas"].clone(), pinned["mapping"].clone()), (json!(0), json!([9, 10, 6, 4])));
    for part in parts.iter().filter(|part| part["id"] != 2) {
        let [x, y]: [u64; 2] = [part["mapping"][0].as_u64().unwrap(), part["mapping"][1].as_u64().unwrap()];
        assert!(x + 6 <= 9 || y + 4 <= 10, "{part}");
    }
    let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, plan["preview"].as_str().unwrap()).unwrap();
    assert_eq!(image::load_from_memory(&png).unwrap().to_rgba8().dimensions(), (16, 16));
    assert!(fail(&mut session, json!({"op": "atlas.build", "resolution": 16, "padding": 0, "preview": 3})).contains("preview"));
    assert!(fail(&mut session, json!({"op": "atlas.build", "resolution": 8, "padding": 0})).contains("pinned"));
    assert!(fail(&mut session, json!({"op": "atlas.layout.set", "parts": [{"id": 1, "atlas": 0, "x": 0, "y": 0}]}))
        .contains("not a Part"));
    assert!(fail(&mut session, json!({"op": "atlas.layout.set", "parts": [{"id": 2, "atlas": 0, "x": 9, "y": 10,
        "scale": 1e-50}]})).contains("scale"), "a scale that is 0 as f32 is refused");
    run(&mut session, json!({"op": "atlas.layout.clear"}));
    assert!(run(&mut session, json!({"op": "snapshot"}))["model"].get("atlasLayout").is_none());
    run(&mut session, json!({"op": "undo"}));
    assert_eq!(run(&mut session, json!({"op": "snapshot"}))["model"]["atlasLayout"]["parts"]["2"]["x"], 9);
}

#[test]
fn a_large_part_forces_a_uniform_output_scale_unless_scaling_is_non_linear() {
    let dir = temp_dir();
    let mut session = three_layer_session(&dir);
    // 6 wide parts in a 6 pixel atlas: 6/6 + 0.01 > 1, so D scales everything to 6/6 - 0.015.
    let linear = run(&mut session, json!({"op": "atlas.build", "resolution": 6, "padding": 0}));
    assert_eq!(linear["outputScale"], f64::from(1.0f32 - 0.015f32));
    let non_linear = run(&mut session, json!({"op": "atlas.build", "resolution": 6, "padding": 0, "nonLinearScaling": true}));
    assert_eq!(non_linear["outputScale"], 1.0);
}

#[test]
fn atlas_requests_are_validated_before_anything_is_planned() {
    let dir = temp_dir();
    let mut session = three_layer_session(&dir);
    let before = run(&mut session, json!({"op": "snapshot"}));
    for request in [
        json!({"op": "atlas.build", "resolution": 9000}),
        json!({"op": "atlas.build", "resolution": 0}),
        json!({"op": "atlas.build", "resolution": "big"}),
        json!({"op": "atlas.build", "padding": -1}),
        json!({"op": "atlas.build", "scale": 0}),
        json!({"op": "atlas.build", "scale": 17}),
        json!({"op": "atlas.build", "nonLinearScaling": 1}),
        json!({"op": "atlas.build", "parts": 7}),
        json!({"op": "atlas.build", "parts": [999]}),
        json!({"op": "atlas.build", "parts": [1]}),
    ] {
        fail(&mut session, request);
    }
    assert_eq!(run(&mut session, json!({"op": "snapshot"})), before);
    // An empty document has no Part to atlas.
    let mut empty = Session::new();
    assert_eq!(fail(&mut empty, json!({"op": "atlas.build"})), "no Part with an albedo texture to atlas");
}

// ---------------------------------------------------------------------------- export copy

#[test]
fn an_export_copy_renders_the_same_picture_on_another_thread() {
    let dir = temp_dir();
    let mut session = three_layer_session(&dir);
    let render = json!({"op": "render", "width": 12, "height": 12, "bounds": [-6.0, -6.0, 6.0, 6.0]});
    let expected = run(&mut session, render.clone())["png"].clone();
    assert!(expected.as_str().unwrap().len() > 100);

    let copy = session.export_copy().unwrap();
    // A later edit of the live session does not reach the copy.
    run(&mut session, json!({"op": "node.enabled.set", "id": 4, "enabled": false}));
    let copied = std::thread::spawn(move || {
        let mut copy = copy.open().unwrap();
        let response = copy.handle_request(&render);
        assert_eq!(response["succeeded"], true, "{response}");
        (response["result"]["png"].clone(), copy.snapshot()["canUndo"].clone())
    })
    .join()
    .unwrap();
    assert_eq!(copied, (expected, json!(false)));
}

#[test]
fn an_export_copy_carries_manual_parameter_values_and_their_deformation() {
    let dir = temp_dir();
    let mut session = three_layer_session(&dir);
    let parameter = run(&mut session, json!({"op": "parameter.create", "dimensions": 1, "name": "Bend"}))["uuid"].clone();
    // Part 4 (layer A) stays at 0 and moves three units right at the end of the parameter's range.
    for (key, offset) in [([0, 0], 0.0), ([2, 0], 3.0)] {
        run(&mut session, json!({"op": "binding.set_deform", "parameter": parameter, "nodes": [4], "key": key,
            "values": [offset, 0.0, offset, 0.0, offset, 0.0, offset, 0.0]}));
    }
    let render = json!({"op": "render", "width": 12, "height": 12, "bounds": [-6.0, -6.0, 6.0, 6.0]});
    let rest = run(&mut session, render.clone())["png"].clone();
    run(&mut session, json!({"op": "value.set", "id": parameter, "value": [1.0, 0.0]}));
    let bent = run(&mut session, render.clone())["png"].clone();
    assert_ne!(bent, rest, "the value moves the Part");

    let copy = session.export_copy().unwrap();
    let copied = std::thread::spawn(move || {
        let mut copy = copy.open().unwrap();
        let response = copy.handle_request(&render);
        response["result"]["png"].clone()
    })
    .join()
    .unwrap();
    assert_eq!(copied, bent);
}

/// The extension block D's `EXT_SECT` writes after the texture section: one named section.
fn extension_block() -> Vec<u8> {
    let mut block = b"EXT_SECT".to_vec();
    block.extend(1u32.to_be_bytes());
    block.extend(7u32.to_be_bytes());
    block.extend_from_slice(b"soul2d.");
    block.extend(5u32.to_be_bytes());
    block.extend_from_slice(&[1, 2, 3, 4, 5]);
    block
}

#[test]
fn a_merge_keeps_the_extension_sections_after_the_textures() {
    let dir = temp_dir();
    let mut first = imported(&dir);
    let plain = dir.join("plain.inx");
    run(&mut first, json!({"op": "save", "path": plain.to_str().unwrap(), "mark_saved": false}));
    let mut with_extension = fs::read(&plain).unwrap();
    with_extension.extend(extension_block());
    let source = dir.join("with-extension.inx");
    fs::write(&source, &with_extension).unwrap();

    let mut session = Session::new();
    run(&mut session, json!({"op": "open", "path": source.to_str().unwrap()}));
    let path = write(&dir, "merge.psd", &merge_psd_bytes());
    run(&mut session, json!({"op": "merge.psd.apply", "path": path}));
    let merged = dir.join("merged.inx");
    run(&mut session, json!({"op": "save", "path": merged.to_str().unwrap(), "mark_saved": false}));
    let bytes = fs::read(&merged).unwrap();
    assert!(bytes.ends_with(&extension_block()), "the extension block is still the last thing in the file");
    assert_eq!(texture_pixels(&bytes).len(), 4);
    // The file reads back, and undo restores the original tail byte for byte.
    run(&mut session, json!({"op": "undo"}));
    let undone = dir.join("undone.inx");
    run(&mut session, json!({"op": "save", "path": undone.to_str().unwrap(), "mark_saved": false}));
    assert_eq!(fs::read(&undone).unwrap(), with_extension);
    let mut reopened = Session::new();
    run(&mut reopened, json!({"op": "open", "path": merged.to_str().unwrap()}));
}

