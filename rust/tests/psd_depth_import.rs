//! `depth.import_psd.plan` and the per-layer options of `depth.import_psd`, on PSDs written in real
//! Photoshop file order (bottom to top; a group is `divider, children..., folder`).

mod common;

use serde_json::{json, Value};
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------- PSD builder

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

// Literal-run PackBits, one row per entry.
fn rle(rows: &[&[u8]]) -> Vec<u8> {
    let encoded: Vec<Vec<u8>> = rows
        .iter()
        .map(|row| [&[row.len() as u8 - 1][..], row].concat())
        .collect();
    let mut out = be16(1).to_vec();
    for row in &encoded {
        out.extend(be16(row.len() as u16));
    }
    for row in &encoded {
        out.extend_from_slice(row);
    }
    out
}

struct L {
    name: &'static str,
    rect: [i32; 4], // top, left, bottom, right
    channels: Vec<(i16, Vec<u8>)>,
    opacity: u8,
    flags: u8,
    clipped: u8,
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

fn marker(name: &'static str, kind: u32, flags: u8, opacity: u8) -> L {
    L {
        name,
        rect: [0; 4],
        channels: (0..3).map(|id| (id, raw(&[]))).chain([(-1, raw(&[]))]).collect(),
        opacity,
        flags,
        clipped: 0,
        lsct: Some(kind),
    }
}

/// `</Layer group>`: the divider that precedes a group's children in file order.
fn divider() -> L {
    marker("</Layer group>", 3, 0, 255)
}

fn folder(name: &'static str, flags: u8, opacity: u8) -> L {
    marker(name, 1, flags, opacity)
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
    L { name, rect, channels, opacity: 255, flags: 0, clipped: 0, lsct: None }
}

/// A fully opaque gray layer.
fn gray(name: &'static str, rect: [i32; 4], value: u8) -> L {
    solid(name, rect, [value, value, value, 255])
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
    out.extend([layer.opacity, layer.clipped, layer.flags, 0]);
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

fn write(name: &str, bytes: &[u8]) -> String {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "soul2d-psd-depth-import-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    common::scratch(dir.clone());
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    path.to_str().unwrap().to_owned()
}

fn part(id: u32, name: &str) -> Value {
    json!({
        "uuid": id, "name": name, "type": "Part", "layerPath": format!("/{name}"),
        "enabled": true, "opacity": 1.0, "textures": [], "children": [],
        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
    })
}

/// A 3x2 document grid: six vertices at document pixels (0,0) (1,0) (2,0) (0,1) (1,1) (2,1).
fn grid(id: u32, name: &str, parts: Vec<Value>) -> Value {
    json!({
        "uuid": id, "name": name, "type": "GridDeformer",
        "grid_axis_x": [-1.5, -0.5, 0.5], "grid_axis_y": [-1.0, 0.0],
        "depths": [9.0, 9.0, 9.0, 9.0, 9.0, 9.0],
        "children": parts,
    })
}

fn session_with(grids: Vec<Value>) -> Session {
    let mut session = Session::new();
    let model = json!({
        "nodes": {"uuid": 1, "name": "Root", "type": "Node", "children": grids},
        "param": [], "automation": [],
    });
    let created = session.handle_request(&json!({"op": "new", "model": model}));
    assert_eq!(created["succeeded"], true, "{created}");
    session
}

/// Face (grid 4) and Body (grid 6), with the Face part at uuid 5 and the Body part at uuid 7.
fn two_grids() -> Session {
    session_with(vec![grid(4, "FaceGrid", vec![part(5, "Face")]), grid(6, "BodyGrid", vec![part(7, "Body")])])
}

fn run(session: &mut Session, op: &str, body: Value) -> Value {
    let mut request = body;
    request["op"] = json!(op);
    session.handle_request(&request)
}

fn ok(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "{response}");
    &response["result"]
}

fn depths(session: &mut Session, id: u32) -> Vec<f64> {
    let node = run(session, "node.get", json!({"id": id}));
    ok(&node)["depths"].as_array().unwrap().iter().map(|depth| depth.as_f64().unwrap()).collect()
}

fn assert_depths(actual: &[f64], expected: &[f64]) {
    assert_eq!(actual.len(), expected.len(), "{actual:?}");
    for (index, (a, e)) in actual.iter().zip(expected).enumerate() {
        assert!((a - e).abs() < 1e-6, "vertex {index}: {actual:?} != {expected:?}");
    }
}

const FULL: [i32; 4] = [0, 0, 2, 3];

/// Face 0.6 on its grid, Body -0.2 on its grid (Nearest, back -1, front 1).
fn face_body_psd() -> String {
    write("face-body.psd", &psd(3, 2, &[gray("Body", FULL, 102), gray("Face", FULL, 204)]))
}

fn rows<'a>(plan: &'a Value) -> Vec<(&'a str, &'a Value)> {
    plan["layers"].as_array().unwrap().iter().map(|row| (row["layerPath"].as_str().unwrap(), row)).collect()
}

// ---------------------------------------------------------------------------- tests

#[test]
fn plan_reports_the_matching_and_changes_nothing() {
    let mut session = two_grids();
    let path = face_body_psd();
    let before = session.snapshot();
    let plan = run(&mut session, "depth.import_psd.plan", json!({"path": path, "convolution": "Nearest"}));
    let plan = ok(&plan).clone();
    assert_eq!(session.snapshot(), before);

    let layers = rows(&plan);
    assert_eq!(layers.iter().map(|row| row.0).collect::<Vec<_>>(), ["/Face", "/Body"], "front layer first");
    let face = layers[0].1;
    assert_eq!(face["status"], "Matched");
    assert_eq!((face["target"].clone(), face["targetName"].clone()), (json!(4), json!("FaceGrid")));
    assert_eq!((face["matchedNode"].clone(), face["matchedNodeName"].clone()), (json!(5), json!("Face")));
    assert_eq!(face["problem"], false);
    assert_eq!(
        (face["maskedPixels"].clone(), face["zeroPixels"].clone(), face["depthRange01"].clone()),
        (json!(6), json!(0), json!(0.0))
    );
    assert_eq!((face["sampledVertices"].clone(), face["selectedVertices"].clone()), (json!(6), json!(6)));
    assert_eq!((face["left"].clone(), face["width"].clone(), face["height"].clone()), (json!(0), json!(3), json!(2)));

    assert_eq!(plan["document"], json!({"width": 3, "height": 2}));
    assert_eq!(plan["budget"], json!({"retainedBytes": 480, "maxRetainedBytes": 268435456, "layerCount": 2}));
    assert_eq!(plan["diagnostics"], json!([]));
    assert_eq!(plan["matchedLayers"], 2);
    assert_eq!(plan["targets"], json!([
        {"id": 4, "name": "FaceGrid", "type": "GridDeformer"},
        {"id": 6, "name": "BodyGrid", "type": "GridDeformer"},
    ]));
    let grids = plan["grids"].as_array().unwrap();
    assert_eq!(grids.len(), 2);
    assert_eq!(grids[0]["target"], 4);
    assert_eq!((grids[0]["sampledVertices"].clone(), grids[0]["missingVertices"].clone()), (json!(6), json!(0)));
    assert!((grids[0]["minDepth"].as_f64().unwrap() - 0.6).abs() < 1e-6);
    assert!((grids[1]["maxDepth"].as_f64().unwrap() + 0.2).abs() < 1e-6);
    // The plan is the dry run of the import: committing it gives the same figures and one undo.
    let applied = run(&mut session, "depth.import_psd", json!({"path": path, "convolution": "Nearest"}));
    assert_eq!(ok(&applied)["layers"], plan["layers"]);
    assert_depths(&depths(&mut session, 4), &[0.6; 6]);
    assert_depths(&depths(&mut session, 6), &[-0.2; 6]);
    ok(&run(&mut session, "undo", json!({})));
    assert_eq!(session.snapshot()["model"], before["model"]);
}

#[test]
fn mapping_overrides_choose_the_target_or_ignore_the_layer() {
    let mut session = two_grids();
    let path = write(
        "extra.psd",
        &psd(3, 2, &[gray("Body", FULL, 102), gray("Face", FULL, 204), gray("Extra", FULL, 255)]),
    );
    let plan = run(&mut session, "depth.import_psd.plan", json!({"path": path, "convolution": "Nearest"}));
    let plan = ok(&plan).clone();
    let extra = rows(&plan).into_iter().find(|row| row.0 == "/Extra").unwrap().1;
    assert_eq!(extra["status"], "Unmatched");
    assert_eq!(extra["problem"], true);
    assert_eq!(plan["unmatchedLayers"], 1);
    assert_eq!(plan["diagnostics"], json!(["unused-depth-layer: /Extra"]));

    // Extra is the front layer, so once bound to FaceGrid it wins every vertex over Face.
    let bound = run(&mut session, "depth.import_psd.plan", json!({
        "path": path, "convolution": "Nearest", "layers": [{"layerPath": "/Extra", "target": 4}],
    }));
    let bound = ok(&bound).clone();
    let by_path = |plan: &Value, name: &str| rows(plan).into_iter().find(|row| row.0 == name).unwrap().1.clone();
    let extra = by_path(&bound, "/Extra");
    assert_eq!((extra["status"].clone(), extra["manual"].clone(), extra["target"].clone()), (json!("Manual"), json!(true), json!(4)));
    assert_eq!((extra["selectedVertices"].clone(), by_path(&bound, "/Face")["selectedVertices"].clone()), (json!(6), json!(0)));
    assert_eq!(bound["unmatchedLayers"], 0);

    let ignored = run(&mut session, "depth.import_psd", json!({
        "path": path, "convolution": "Nearest",
        "layers": [{"layerPath": "/Extra", "target": 4}, {"layerPath": "/Face", "target": "ignore"}],
    }));
    let ignored = ok(&ignored).clone();
    let face = by_path(&ignored, "/Face");
    assert_eq!((face["status"].clone(), face["ignored"].clone(), face["problem"].clone()), (json!("Ignored"), json!(true), json!(true)));
    assert_depths(&depths(&mut session, 4), &[1.0; 6]);
    assert_depths(&depths(&mut session, 6), &[-0.2; 6]);

    // A missing node or a non-grid node is D's "UnmatchedManualGrid" and applies nothing.
    for target in [99, 5] {
        let mut fresh = two_grids();
        let missing = run(&mut fresh, "depth.import_psd", json!({
            "path": path, "convolution": "Nearest", "layers": [{"layerPath": "/Extra", "target": target}],
        }));
        let report = ok(&missing).clone();
        assert_eq!(by_path(&report, "/Extra")["status"], "UnmatchedManualGrid");
        assert_eq!(report["unmatchedLayers"], 1, "{report}");
    }
}

#[test]
fn a_missing_manual_target_reports_d_diagnostics() {
    let mut session = two_grids();
    let path = write("one.psd", &psd(3, 2, &[gray("Face", FULL, 204)]));
    let plan = run(&mut session, "depth.import_psd.plan", json!({
        "path": path, "layers": [{"layerPath": "/Face", "target": 99}],
    }));
    let plan = ok(&plan);
    assert_eq!(plan["unmatchedLayers"], 1);
    assert_eq!(plan["diagnostics"], json!([
        "No mapped targets were found. Remap a source layer to a GridDeformer or PathDeformer.",
        "Mapped target is missing for layer: /Face",
        "missing-manual-target: /Face",
    ]));
    let empty = write("empty.psd", &psd(3, 2, &[marker("Group", 1, 0, 255)]));
    let none = run(&mut session, "depth.import_psd.plan", json!({"path": empty}));
    assert_eq!(ok(&none)["diagnostics"], json!(["No source layers were loaded from the selected depth source."]));
}

#[test]
fn disabled_grids_are_skipped_and_keep_their_depths() {
    let mut session = two_grids();
    let path = face_body_psd();
    let applied = run(&mut session, "depth.import_psd", json!({
        "path": path, "convolution": "Nearest", "disabledGrids": [4],
    }));
    let report = ok(&applied);
    assert_eq!((report["changedGrids"].clone(), report["skippedGrids"].clone()), (json!(1), json!(1)));
    assert_eq!(report["grids"][0]["skipped"], true);
    assert_eq!(report["grids"][0]["enabled"], false);
    assert_eq!(report["grids"][1]["skipped"], false);
    assert_depths(&depths(&mut session, 4), &[9.0; 6]);
    assert_depths(&depths(&mut session, 6), &[-0.2; 6]);
}

#[test]
fn per_layer_invert_scale_offset_enabled_and_visible() {
    let path = face_body_psd();
    let import = |layers: Value| {
        let mut session = two_grids();
        let response = run(&mut session, "depth.import_psd", json!({
            "path": path, "convolution": "Nearest", "layers": layers,
        }));
        let report = ok(&response).clone();
        (depths(&mut session, 4), report)
    };
    // gray 204 is depth01 0.8; inverted it is 0.2, which maps to -0.6.
    assert_depths(&import(json!([{"layerPath": "/Face", "invert": true}])).0, &[-0.6; 6]);
    // The Z transform is value * scale + offset on the sampled depth: 0.6 * 2 + 0.5.
    let (transformed, report) = import(json!([{"layerPath": "/Face", "depthScale": 2.0, "depthOffset": 0.5}]));
    assert_depths(&transformed, &[1.7; 6]);
    assert_eq!(report["layers"][0]["depthScale"], 2.0);
    assert_eq!(report["layers"][0]["depthOffset"], 0.5);
    // "Use" off removes the layer from the target (nothing is sampled, so the old depths stay).
    let (disabled, report) = import(json!([{"layerPath": "/Face", "enabled": false}]));
    assert_depths(&disabled, &[9.0; 6]);
    assert_eq!((report["layers"][0]["enabled"].clone(), report["layers"][0]["sampledVertices"].clone()), (json!(false), json!(0)));
    // "Show" off hides a visible layer the same way.
    let (hidden, report) = import(json!([{"layerPath": "/Face", "visible": false}]));
    assert_depths(&hidden, &[9.0; 6]);
    assert_eq!(report["layers"][0]["visible"], false);
}

#[test]
fn show_overrides_the_visibility_stored_in_the_psd() {
    let mut face = gray("Face", FULL, 204);
    face.flags = 0x02;
    let path = write("hidden.psd", &psd(3, 2, &[face]));
    let mut session = session_with(vec![grid(4, "FaceGrid", vec![part(5, "Face")])]);
    let first = run(&mut session, "depth.import_psd", json!({"path": path, "convolution": "Nearest"}));
    assert_eq!(ok(&first)["layers"][0]["visible"], false);
    assert_depths(&depths(&mut session, 4), &[9.0; 6]);
    let shown = run(&mut session, "depth.import_psd", json!({
        "path": path, "convolution": "Nearest", "layers": [{"layerPath": "/Face", "visible": true}],
    }));
    assert_eq!(ok(&shown)["layers"][0]["visible"], true);
    assert_depths(&depths(&mut session, 4), &[0.6; 6]);
}

#[test]
fn a_layer_without_depth_follows_the_nearest_enabled_depth_below() {
    // Front covers the left two columns with white, Back covers everything with 102 (-0.2).
    let psd_bytes = psd(3, 2, &[gray("Back", FULL, 102), gray("Front", [0, 0, 2, 2], 255)]);
    let path = write("attach.psd", &psd_bytes);
    let grids = || session_with(vec![grid(4, "G", vec![part(5, "Front"), part(6, "Back")])]);
    let mut session = grids();
    ok(&run(&mut session, "depth.import_psd", json!({"path": path, "convolution": "Nearest"})));
    assert_depths(&depths(&mut session, 4), &[1.0, 1.0, -0.2, 1.0, 1.0, -0.2]);

    let mut session = grids();
    let attached = run(&mut session, "depth.import_psd", json!({
        "path": path, "convolution": "Nearest", "layers": [{"layerPath": "/Front", "depthEnabled": false}],
    }));
    assert_eq!(ok(&attached)["layers"][0]["depthEnabled"], false);
    assert_depths(&depths(&mut session, 4), &[-0.2; 6]);
}

#[test]
fn invalid_options_fail_and_leave_the_document_unchanged() {
    let mut session = two_grids();
    let path = face_body_psd();
    let before = session.snapshot();
    let cases = [
        (json!({"layers": [{"layerPath": "/Nope"}]}), "PSD layer not found: /Nope"),
        (json!({"layers": {"layerPath": "/Face"}}), "layers must be an array"),
        (json!({"layers": ["/Face"]}), "layers entries must be objects"),
        (json!({"layers": [{"target": 4}]}), "layers entries need a layerPath"),
        (json!({"layers": [{"layerPath": "/Face", "colour": 1}]}), "unknown layer option: colour"),
        (json!({"layers": [{"layerPath": "/Face", "target": "banana"}]}), "target must be \"auto\", \"ignore\" or a node id"),
        (json!({"layers": [{"layerPath": "/Face", "enabled": "yes"}]}), "enabled must be a boolean"),
        (json!({"layers": [{"layerPath": "/Face", "depthScale": "big"}]}), "depthScale must be finite"),
        (
            json!({"layers": [{"layerPath": "/Face"}, {"layerPath": "/Face", "invert": true}]}),
            "duplicate options for layer /Face",
        ),
        (json!({"disabledGrids": 4}), "disabledGrids must be an array"),
        (json!({"disabledGrids": ["x"]}), "disabledGrids entries must be a node id"),
    ];
    for (extra, message) in cases {
        let mut body = json!({"path": path});
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        for op in ["depth.import_psd", "depth.import_psd.plan"] {
            let failed = run(&mut session, op, body.clone());
            assert_eq!(failed["succeeded"], false, "{op} {body}");
            assert_eq!(failed["message"].as_str(), Some(message), "{op}");
            assert_eq!(session.snapshot(), before, "{op} {body}");
        }
    }
    // A PathDeformer target is refused (depths are only stored on grids).
    let mut with_path = session_with(vec![
        grid(4, "G", vec![part(5, "Face")]),
        json!({"uuid": 8, "name": "P", "type": "PathDeformer", "children": []}),
    ]);
    let before = with_path.snapshot();
    let refused = run(&mut with_path, "depth.import_psd", json!({
        "path": path, "layers": [{"layerPath": "/Face", "target": 8}],
    }));
    assert_eq!(refused["message"], "PSD depth import does not assign depths to PathDeformer");
    assert_eq!(with_path.snapshot(), before);
}

#[test]
fn groups_in_real_file_order_nest_their_children_only() {
    // Top to bottom: Above, Outer { Inner { Deep }, Mid }, Below. File order is the reverse, and a
    // group's divider precedes its children while its folder record follows them.
    let psd_bytes = psd(
        3,
        2,
        &[
            gray("Below", FULL, 51),
            divider(),
            gray("Mid", FULL, 102),
            divider(),
            gray("Deep", FULL, 153),
            folder("Inner", 0, 255),
            folder("Outer", 0x02, 128),
            gray("Above", FULL, 204),
        ],
    );
    let path = write("nested.psd", &psd_bytes);
    let mut session = two_grids();
    let plan = run(&mut session, "depth.import_psd.plan", json!({"path": path}));
    let plan = ok(&plan);
    let layers = rows(plan);
    assert_eq!(
        layers.iter().map(|row| row.0).collect::<Vec<_>>(),
        ["/Above", "/Outer/Inner/Deep", "/Outer/Mid", "/Below"]
    );
    let flags: Vec<bool> = layers.iter().map(|row| row.1["visible"].as_bool().unwrap()).collect();
    assert_eq!(flags, [true, false, false, true], "a hidden folder hides its children only");
    let opacity = |index: usize| layers[index].1["opacity"].as_f64().unwrap();
    assert!((opacity(0) - 1.0).abs() < 1e-9, "the layer above the group is outside it");
    assert!((opacity(1) - f64::from(128.0f32 / 255.0)).abs() < 1e-6);
    assert!((opacity(3) - 1.0).abs() < 1e-9);
}

#[test]
fn a_clipped_layer_takes_the_alpha_of_the_base_below_it() {
    // Base covers only column 0; Clip is white over the whole canvas and clipped to Base.
    let mut clip = gray("Clip", FULL, 255);
    clip.clipped = 1;
    let psd_bytes = psd(3, 2, &[gray("Base", [0, 0, 2, 1], 102), clip]);
    let path = write("clip.psd", &psd_bytes);
    let mut session = session_with(vec![grid(4, "G", vec![part(5, "Base"), part(6, "Clip")])]);
    let applied = run(&mut session, "depth.import_psd", json!({"path": path, "convolution": "Nearest"}));
    let report = ok(&applied);
    assert_eq!(report["layers"][0]["layerPath"], "/Clip");
    assert_eq!(report["layers"][0]["clipped"], true);
    assert_eq!(report["layers"][0]["maskedPixels"], 2, "only the two pixels over Base survive");
    // The four vertices outside Base take the depth of the nearest sampled vertex (D's KeepExisting), which is
    // 1.0 here; the stored 9.0 is only the fallback when nothing was sampled.
    assert_depths(&depths(&mut session, 4), &[1.0; 6]);
}

#[test]
fn keep_existing_fills_a_missing_vertex_from_the_nearest_sampled_one() {
    // Back covers column 0 (-0.2), Front covers column 1 (0.6); column 2 is covered by neither. Vertices 2 and 5
    // sit one unit from column 1 and two from column 0, so both take 0.6 (not the stored 9.0).
    let psd_bytes = psd(3, 2, &[gray("Back", [0, 0, 2, 1], 102), gray("Front", [0, 1, 2, 2], 204)]);
    let path = write("partial.psd", &psd_bytes);
    let mut session = session_with(vec![grid(4, "G", vec![part(5, "Back"), part(6, "Front")])]);
    let applied = run(&mut session, "depth.import_psd", json!({"path": path, "convolution": "Nearest"}));
    assert_eq!(ok(&applied)["grids"][0]["missingVertices"], 2);
    assert_depths(&depths(&mut session, 4), &[-0.2, 0.6, 0.6, -0.2, 0.6, 0.6]);
}

#[test]
fn rle_compressed_layers_import() {
    // Photoshop writes RLE; the previous private reader refused anything but raw channels.
    let rows: [&[u8]; 2] = [&[204, 204, 204], &[204, 204, 204]];
    let mut face = gray("Face", FULL, 0);
    face.channels = vec![
        (0, rle(&rows)),
        (1, rle(&rows)),
        (2, rle(&rows)),
        (-1, rle(&[&[255, 255, 255], &[255, 255, 255]])),
    ];
    let path = write("rle.psd", &psd(3, 2, &[face]));
    let mut session = session_with(vec![grid(4, "FaceGrid", vec![part(5, "Face")])]);
    ok(&run(&mut session, "depth.import_psd", json!({"path": path, "convolution": "Nearest"})));
    assert_depths(&depths(&mut session, 4), &[0.6; 6]);
}

// RLE channel of `width` x `height` pixels all equal to `value`, using 128-pixel repeat runs.
fn rle_fill(width: usize, height: usize, value: u8) -> Vec<u8> {
    let mut row = Vec::new();
    let mut left = width;
    while left > 0 {
        let run = left.min(128);
        row.extend([(257 - run) as u8, value]);
        left -= run;
    }
    let mut out = be16(1).to_vec();
    for _ in 0..height {
        out.extend(be16(row.len() as u16));
    }
    for _ in 0..height {
        out.extend_from_slice(&row);
    }
    out
}

#[test]
fn the_retained_memory_budget_rejects_oversized_layers() {
    // 3000 x 3000 = 9M pixels at 40 bytes each is 360 MB, over the 256 MiB budget, though the
    // document limit (100M pixels) is fine.
    let mut big = gray("Face", [0, 0, 3000, 3000], 0);
    big.channels = (0..4)
        .map(|channel| (if channel == 3 { -1 } else { channel as i16 }, rle_fill(3000, 3000, 255)))
        .collect();
    let path = write("big.psd", &psd(3000, 3000, &[big]));
    let mut session = two_grids();
    let before = session.snapshot();
    let failed = run(&mut session, "depth.import_psd.plan", json!({"path": path}));
    assert_eq!(failed["succeeded"], false, "{failed}");
    assert_eq!(failed["message"], "PSD Depth Map layers exceed the retained image memory budget");
    assert_eq!(session.snapshot(), before);
}

#[test]
fn the_mcp_tools_plan_and_apply() {
    let mut session = two_grids();
    let listed = session.handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    let tools = listed["result"]["tools"].as_array().unwrap();
    for name in ["depth_import_psd", "depth_import_psd_plan", "DepthMapCommand_ImportPSDDepths"] {
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap_or_else(|| panic!("{name} is listed"));
        assert_eq!(tool["inputSchema"]["required"], json!(["path"]), "{name}");
        assert!(tool["inputSchema"]["properties"]["layers"].is_object(), "{name}");
        assert!(tool["inputSchema"]["properties"]["disabledGrids"].is_object(), "{name}");
    }
    let path = face_body_psd();
    let mut call = |name: &str, arguments: Value| {
        session
            .handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": name, "arguments": arguments}}))
            .unwrap()["result"]
            .clone()
    };
    let planned = call("depth_import_psd_plan", json!({"path": path, "convolution": "Nearest"}));
    assert_eq!(planned["isError"], false, "{planned}");
    assert_eq!(planned["structuredContent"]["layers"][0]["layerPath"], "/Face");
    let applied = call("depth_import_psd", json!({
        "path": path, "convolution": "Nearest", "layers": [{"layerPath": "/Body", "target": "ignore"}],
    }));
    assert_eq!(applied["isError"], false, "{applied}");
    assert_eq!(applied["structuredContent"]["layers"][1]["status"], "Ignored");
    assert_eq!(applied["structuredContent"]["changedGrids"], 1);
}
