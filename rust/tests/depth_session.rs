//! `depth.edit.apply` and the DepthDraw session operations (`depth.draw.*`), through `Session::handle_request` and
//! through the MCP tools. Literal expectations come from D's `applyToTargets`, `ngComposeDepthDrawTarget`,
//! `ngApplyDepthDrawTargetResultWithSummary` and the DepthDraw fit-Z, cleanup and export code.

mod common;

use serde_json::{json, Value};
use soul2d_core::Session;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// ------------------------------------------------------------------------------------------ helpers

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "soul2d-depth-session-{label}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    common::scratch(dir)
}

fn run(session: &mut Session, op: &str, body: Value) -> Value {
    let mut request = body;
    request["op"] = json!(op);
    session.handle_request(&request)
}

/// The result of a request that must succeed.
fn ok(session: &mut Session, op: &str, body: Value) -> Value {
    let reply = run(session, op, body);
    assert_eq!(reply["succeeded"], true, "{op}: {reply}");
    reply["result"].clone()
}

/// A request that must fail with `needle` in its message and leave the whole session untouched.
fn fails(session: &mut Session, op: &str, body: Value, needle: &str) {
    let before = session.snapshot();
    let reply = run(session, op, body);
    assert_eq!(reply["succeeded"], false, "{op} should fail: {reply}");
    let message = reply["message"].as_str().unwrap_or("");
    assert!(message.contains(needle), "{op}: {message:?} lacks {needle:?}");
    assert_eq!(session.snapshot(), before, "{op}: the failed request changed the session");
}

fn history(session: &Session) -> u64 {
    session.snapshot()["history"]["count"].as_u64().unwrap()
}

fn node(session: &Session, id: u64) -> Value {
    fn find(node: &Value, id: u64) -> Option<Value> {
        if node["uuid"].as_u64() == Some(id) {
            return Some(node.clone());
        }
        node["children"].as_array()?.iter().find_map(|child| find(child, id))
    }
    find(&session.snapshot()["model"]["nodes"], id).unwrap()
}

fn floats(value: &Value) -> Vec<f32> {
    value.as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect()
}

fn depths(session: &Session, id: u64) -> Vec<f32> {
    floats(&node(session, id)["depths"])
}

fn transform() -> Value {
    json!({"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]})
}

fn part(id: u32, name: &str) -> Value {
    json!({
        "uuid": id, "name": name, "type": "Part", "layerPath": format!("/{name}"),
        "enabled": true, "opacity": 1.0, "textures": [], "children": [], "transform": transform(),
    })
}

fn session_with(children: Vec<Value>) -> Session {
    let model = json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
            "transform": transform(), "lockToRoot": false, "pinToMesh": false, "children": children},
        "param": [], "automation": [], "animations": {},
    });
    let mut session = Session::new();
    let reply = session.handle_request(&json!({"op": "new", "model": model}));
    assert_eq!(reply["succeeded"], true, "{reply}");
    session
}

/// A 2x2 grid with the depths D's `applyToTargets` test uses.
fn edit_session() -> Session {
    session_with(vec![json!({
        "uuid": 5, "name": "Grid", "type": "GridDeformer", "transform": transform(), "children": [],
        "grid_axis_x": [0.0, 10.0], "grid_axis_y": [0.0, 10.0], "depths": [0.9, 0.4, -0.2, -0.8],
    })])
}

fn attached(index: u64, amount: f64) -> Value {
    json!({"type": "attached-point", "index": index, "amount": amount})
}

/// A 3x3 grid (id 5) whose vertices land on document pixels (0,3,5) x (0,3,5) of a 6x6 canvas, with the Parts
/// `back`, `mid` and `front` under it. Every stored depth is 0.5.
fn draw_session() -> Session {
    session_with(vec![json!({
        "uuid": 5, "name": "Grid", "type": "GridDeformer", "transform": transform(),
        "grid_axis_x": [-3.0, 0.0, 2.0], "grid_axis_y": [-3.0, 0.0, 2.0], "depths": vec![0.5; 9],
        "children": [part(6, "back"), part(7, "mid"), part(8, "front")],
    })])
}

fn write_png(dir: &Path, name: &str, width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> String {
    let path = dir.join(name);
    image::RgbaImage::from_fn(width, height, |x, y| image::Rgba(pixel(x, y))).save(&path).unwrap();
    path.to_str().unwrap().to_owned()
}

fn grey(value: u8) -> [u8; 4] {
    [value, value, value, 255]
}

/// Depth -1 (grey 0), 1 (grey 255) at columns 0-2 / 3-5, and the bottom row transparent.
fn mid_png(dir: &Path) -> String {
    write_png(dir, "mid.png", 6, 6, |x, y| if y == 5 { [0, 0, 0, 0] } else if x < 3 { grey(255) } else { grey(0) })
}

/// The three layers back (all -1), mid (1 / -1 columns) and front (all 1), bound to grid 5 in that order.
fn loaded(session: &mut Session, dir: &Path) {
    let files = [
        ("back", write_png(dir, "back.png", 6, 6, |_, _| grey(0))),
        ("mid", mid_png(dir)),
        ("front", write_png(dir, "front.png", 6, 6, |_, _| grey(255))),
    ];
    for (id, path) in files {
        ok(session, "depth.draw.load_png", json!({"path": path}));
        ok(session, "depth.draw.layer.update", json!({"layer": id, "convolution": "Nearest"}));
        ok(session, "depth.draw.binding.update", json!({"layer": id, "target": 5}));
    }
}

fn mcp(session: &mut Session, name: &str, arguments: Value) -> Value {
    let reply = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}}))
        .unwrap();
    reply["result"].clone()
}

fn mcp_ok(session: &mut Session, name: &str, arguments: Value) -> Value {
    let result = mcp(session, name, arguments);
    assert_eq!(result["isError"], false, "{name}: {result}");
    result["structuredContent"].clone()
}

// ------------------------------------------------------------------------------- depth.edit.apply

#[test]
fn a_changed_operation_list_is_saved_and_baked_in_one_undo_step() {
    let mut session = edit_session();
    let steps = history(&session);
    let result = ok(&mut session, "depth.edit.apply", json!({
        "target": 5, "operations": [attached(2, 0.3)], "depths": [0.0, 0.0, 0.0, 0.0], "directEdits": false,
    }));
    assert_eq!(result["plan"], "SetOpsThenApply");
    assert_eq!(history(&session), steps + 1, "one undo step");
    assert_eq!(depths(&session, 5), [0.9, 0.4, 0.1, -0.8], "the operation is added on top of the existing depths");
    let grid = node(&session, 5);
    assert_eq!(grid["depth-ops"].as_array().unwrap().len(), 1);
    assert_eq!(floats(&grid["depth-op-base-depths"]), [0.9, 0.4, -0.2, -0.8], "the base keeps the manual depths");
    assert_eq!(floats(&result["depths"]), [0.9, 0.4, 0.1, -0.8]);

    ok(&mut session, "undo", json!({}));
    let grid = node(&session, 5);
    assert_eq!(floats(&grid["depths"]), [0.9, 0.4, -0.2, -0.8]);
    assert!(grid.get("depth-ops").is_none() && grid.get("depth-op-base-depths").is_none());
    ok(&mut session, "redo", json!({}));
    assert_eq!(depths(&session, 5), [0.9, 0.4, 0.1, -0.8]);
}

#[test]
fn direct_edits_clear_saved_operations_and_write_the_depths_in_one_undo_step() {
    let mut session = edit_session();
    ok(&mut session, "depth.edit.apply", json!({"target": 5, "operations": [attached(2, 0.3)]}));
    let steps = history(&session);
    let result = ok(&mut session, "depth.edit.apply", json!({
        "target": 5, "operations": [attached(2, 0.3)], "depths": [1.1, 0.4, 0.1, -0.8], "directEdits": true,
    }));
    assert_eq!(result["plan"], "ClearOpsThenSetDepths");
    assert_eq!(history(&session), steps + 1, "clearing the recipe and writing the depths is one undo step");
    let grid = node(&session, 5);
    assert_eq!(floats(&grid["depths"]), [1.1, 0.4, 0.1, -0.8]);
    assert!(grid.get("depth-ops").is_none() && grid.get("depth-op-base-depths").is_none());

    ok(&mut session, "undo", json!({}));
    let grid = node(&session, 5);
    assert_eq!(floats(&grid["depths"]), [0.9, 0.4, 0.1, -0.8], "undo restores the applied depths");
    assert_eq!(grid["depth-ops"].as_array().unwrap().len(), 1, "and the recipe");
    assert_eq!(floats(&grid["depth-op-base-depths"]), [0.9, 0.4, -0.2, -0.8]);
}

#[test]
fn direct_edits_without_saved_operations_just_set_the_depths() {
    let mut session = edit_session();
    let steps = history(&session);
    let result = ok(&mut session, "depth.edit.apply", json!({
        "target": 5, "operations": [], "depths": [0.0, 0.25, 0.5, 0.75], "directEdits": true,
    }));
    assert_eq!(result["plan"], "SetDepths");
    assert_eq!(history(&session), steps + 1);
    assert_eq!(depths(&session, 5), [0.0, 0.25, 0.5, 0.75]);
}

#[test]
fn an_unchanged_operation_list_does_not_regenerate_depths_or_record_a_step() {
    let mut session = edit_session();
    ok(&mut session, "depth.edit.apply", json!({"target": 5, "operations": [attached(2, 0.3)]}));
    // A hand edit of the stored depths that the saved recipe would overwrite if it were applied again.
    ok(&mut session, "depth.set", json!({"target": 5, "depths": [9.0, 9.0, 9.0, 9.0]}));
    let steps = history(&session);
    let listed = ok(&mut session, "depth.ops.list", json!({"target": 5}));
    // The list as `depth.ops.list` returns it (with indexInList) is the same list.
    let result = ok(&mut session, "depth.edit.apply", json!({"target": 5, "operations": listed["operations"]}));
    assert_eq!(result["plan"], "Nothing");
    assert_eq!(history(&session), steps);
    assert_eq!(depths(&session, 5), [9.0, 9.0, 9.0, 9.0]);
}

#[test]
fn a_bad_edit_apply_request_changes_nothing() {
    let mut session = edit_session();
    ok(&mut session, "depth.edit.apply", json!({"target": 5, "operations": [attached(2, 0.3)]}));
    fails(&mut session, "depth.edit.apply", json!({"target": 99, "operations": []}), "was not found");
    fails(&mut session, "depth.edit.apply", json!({"target": 1, "operations": []}), "support depth operations");
    fails(&mut session, "depth.edit.apply", json!({"target": 5}), "operations must be an array");
    fails(&mut session, "depth.edit.apply", json!({"target": 5, "operations": [{"type": "bogus"}]}), "unknown depth operation");
    fails(
        &mut session,
        "depth.edit.apply",
        json!({"target": 5, "operations": [attached(9, 0.1)]}),
        "less than vertices length",
    );
    fails(
        &mut session,
        "depth.edit.apply",
        json!({"target": 5, "operations": [], "depths": [1.0], "directEdits": true}),
        "depths length must match",
    );
    fails(&mut session, "depth.edit.apply", json!({"target": 5, "operations": [], "directEdits": true}), "depths not provided");
    fails(&mut session, "depth.edit.apply", json!({"target": 5, "operations": [], "directEdits": "yes"}), "directEdits");
}

#[test]
fn depth_edit_apply_round_trips_over_mcp() {
    let mut session = edit_session();
    let listed = session.handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    let tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "depth_edit_apply")
        .expect("depth_edit_apply is listed")
        .clone();
    // The operation also reads the target under `id`, so `target` is not required.
    assert_eq!(tool["inputSchema"]["required"], json!(["operations"]));
    let steps = history(&session);
    let result = mcp_ok(&mut session, "depth_edit_apply", json!({"target": 5, "operations": [attached(0, 0.1)]}));
    assert_eq!(result["plan"], "SetOpsThenApply");
    assert_eq!(history(&session), steps + 1);
    assert_eq!(floats(&result["depths"]), [1.0, 0.4, -0.2, -0.8]);
    let failed = mcp(&mut session, "depth_edit_apply", json!({"target": 99, "operations": []}));
    assert_eq!(failed["isError"], true);
}

// --------------------------------------------------------------------------- depth.draw: loading

#[test]
fn load_png_makes_a_layer_and_sets_the_canvas_from_the_first_image() {
    let dir = temp_dir("load");
    let mut session = draw_session();
    let steps = history(&session);
    let result = ok(&mut session, "depth.draw.load_png", json!({"path": mid_png(&dir)}));
    assert_eq!(history(&session), steps, "the DepthDraw session is outside the undo history");
    assert_eq!((result["documentWidth"].clone(), result["documentHeight"].clone()), (json!(6), json!(6)));
    let layer = &result["layer"];
    assert_eq!(layer["id"], "mid");
    assert_eq!(layer["layerPath"], "/mid");
    assert_eq!(layer["displayName"], "mid");
    assert_eq!(layer["bounds"], json!([0, 0, 6, 6]));
    assert_eq!((layer["width"].clone(), layer["height"].clone()), (json!(6), json!(6)));
    assert_eq!(layer["channel"], "AverageRGB");
    assert_eq!(layer["convolution"], "Gaussian3x3");
    assert_eq!(result["retainedPngBytes"], 6 * 6 * 8);

    let named = ok(&mut session, "depth.draw.load_png", json!({"path": mid_png(&dir), "id": "second"}));
    assert_eq!(named["layers"], 2);
    assert_eq!(named["layer"]["layerPath"], "/second");

    let manifest = ok(&mut session, "depth.draw.manifest.get", json!({}));
    assert_eq!(manifest["layers"], json!([
        {"id": "mid", "hasDepthPixels": true, "hasNormalCoverage": false},
        {"id": "second", "hasDepthPixels": true, "hasNormalCoverage": false},
    ]));
    assert_eq!(manifest["manifest"]["documentWidth"], 6);
    assert_eq!(manifest["retainedPngBytes"], 2 * 6 * 6 * 8);
}

#[test]
fn a_bad_png_request_changes_nothing() {
    let dir = temp_dir("load-bad");
    let mut session = draw_session();
    let path = mid_png(&dir);
    ok(&mut session, "depth.draw.load_png", json!({"path": path}));
    fails(&mut session, "depth.draw.load_png", json!({"path": path}), "already has pixels");
    fails(&mut session, "depth.draw.load_png", json!({}), "path is required");
    fails(&mut session, "depth.draw.load_png", json!({"path": dir.join("missing.png").to_str().unwrap()}), "could not open");
    fails(&mut session, "depth.draw.load_png", json!({"path": dir.to_str().unwrap()}), "regular file");
    let text = dir.join("not-a-png.png");
    fs::write(&text, b"this is not a png").unwrap();
    fails(&mut session, "depth.draw.load_png", json!({"path": text.to_str().unwrap()}), "PNG");
    // A header that promises 20000 x 20000 pixels (3.2 GB retained) is refused before any pixel is decoded.
    let huge = dir.join("huge.png");
    fs::write(&huge, png_header_only(20000, 20000)).unwrap();
    fails(
        &mut session,
        "depth.draw.load_png",
        json!({"path": huge.to_str().unwrap(), "id": "huge"}),
        "retained memory budget",
    );
}

/// A PNG whose IHDR claims `width` x `height` grey pixels, followed by a one-byte IDAT and IEND.
fn png_header_only(width: u32, height: u32) -> Vec<u8> {
    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut crc = crc32fast::Hasher::new();
        crc.update(kind);
        crc.update(data);
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend(kind);
        out.extend(data);
        out.extend(crc.finalize().to_be_bytes());
        out
    }
    let mut header = width.to_be_bytes().to_vec();
    header.extend(height.to_be_bytes());
    header.extend([8, 0, 0, 0, 0]);
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    out.extend(chunk(b"IHDR", &header));
    out.extend(chunk(b"IDAT", &[0]));
    out.extend(chunk(b"IEND", &[]));
    out
}

// ------------------------------------------------------------------- depth.draw: edit and compose

#[test]
fn layer_update_changes_the_four_groups_and_keeps_the_other_fields() {
    let dir = temp_dir("update");
    let mut session = draw_session();
    ok(&mut session, "depth.draw.load_png", json!({"path": mid_png(&dir)}));
    let result = ok(&mut session, "depth.draw.layer.update", json!({
        "layer": "mid",
        "xyOffset": [2.0, -1.0], "xyScale": [0.5, 2.0],
        "backDepth": -2.0, "frontDepth": 3.0, "invert": true, "zScale": 0.5, "zOffset": 0.25,
        "channel": "R", "convolution": "Median3x3", "customRadius": 99, "alphaThreshold": 0.5,
        "visible": false, "enabled": false,
    }));
    let layer = &result["layer"];
    assert_eq!(layer["xyOffset"], json!([2.0, -1.0]));
    assert_eq!(layer["xyScale"], json!([0.5, 2.0]));
    assert_eq!(
        [&layer["backDepth"], &layer["frontDepth"], &layer["zScale"], &layer["zOffset"]],
        [&json!(-2.0), &json!(3.0), &json!(0.5), &json!(0.25)]
    );
    assert_eq!(layer["invert"], true);
    assert_eq!((layer["channel"].clone(), layer["convolution"].clone()), (json!("R"), json!("Median3x3")));
    assert_eq!(layer["customRadius"], 64, "the radius is clamped to 1..64");
    assert_eq!(layer["alphaThreshold"], 0.5);
    assert_eq!((layer["visible"].clone(), layer["enabled"].clone()), (json!(false), json!(false)));

    // Only the fields given change.
    let again = ok(&mut session, "depth.draw.layer.update", json!({"layer": "mid", "visible": true}));
    assert_eq!(again["layer"]["visible"], true);
    assert_eq!(again["layer"]["enabled"], false);
    assert_eq!(again["layer"]["zScale"], 0.5);

    fails(&mut session, "depth.draw.layer.update", json!({"layer": "nope", "visible": true}), "was not found");
    fails(&mut session, "depth.draw.layer.update", json!({"layer": "mid"}), "no layer field");
    fails(&mut session, "depth.draw.layer.update", json!({"layer": "mid", "channel": "Hue"}), "Hue");
    fails(&mut session, "depth.draw.layer.update", json!({"layer": "mid", "zScale": "big"}), "zScale");
    fails(&mut session, "depth.draw.layer.update", json!({"layer": "mid", "xyOffset": [1.0]}), "xyOffset");
}

#[test]
fn binding_update_creates_and_changes_a_binding() {
    let dir = temp_dir("binding");
    let mut session = draw_session();
    ok(&mut session, "depth.draw.load_png", json!({"path": mid_png(&dir)}));
    let created = ok(&mut session, "depth.draw.binding.update", json!({"layer": "mid", "target": 5}));
    assert_eq!(created["created"], true);
    assert_eq!(created["binding"], json!({
        "layerId": "mid", "targetNodeUuid": "5", "targetGridUuid": "5", "order": 0, "enabled": true,
        "useNormalLayerAlpha": true, "coverageThreshold": 0.009999999776482582, "mergePolicy": "Frontmost",
    }));
    let changed = ok(&mut session, "depth.draw.binding.update", json!({
        "layer": "mid", "target": 5, "enabled": false, "order": 3, "mergePolicy": "Add",
        "useNormalLayerAlpha": false, "coverageThreshold": 0.5,
    }));
    assert_eq!(changed["created"], false);
    assert_eq!(changed["binding"], json!({
        "layerId": "mid", "targetNodeUuid": "5", "targetGridUuid": "5", "order": 3, "enabled": false,
        "useNormalLayerAlpha": false, "coverageThreshold": 0.5, "mergePolicy": "Add",
    }));
    assert_eq!(ok(&mut session, "depth.draw.manifest.get", json!({}))["manifest"]["bindings"].as_array().unwrap().len(), 1);

    fails(&mut session, "depth.draw.binding.update", json!({"layer": "nope", "target": 5}), "was not found");
    fails(&mut session, "depth.draw.binding.update", json!({"layer": "mid", "target": 1}), "GridDeformer or PathDeformer");
    fails(&mut session, "depth.draw.binding.update", json!({"layer": "mid"}), "target is required");
    fails(&mut session, "depth.draw.binding.update", json!({"layer": "mid", "target": 5, "mergePolicy": "Best"}), "Best");
}

#[test]
fn compose_samples_the_bound_layer_at_the_grid_vertices_without_changing_the_document() {
    let dir = temp_dir("compose");
    let mut session = draw_session();
    ok(&mut session, "depth.draw.load_png", json!({"path": mid_png(&dir)}));
    ok(&mut session, "depth.draw.layer.update", json!({"layer": "mid", "convolution": "Nearest"}));
    ok(&mut session, "depth.draw.binding.update", json!({"layer": "mid", "target": 5}));
    let before = session.snapshot();
    let result = ok(&mut session, "depth.draw.compose", json!({"target": 5}));
    assert_eq!(session.snapshot(), before, "compose is read-only");
    // Vertices sit on document pixels x,y in {0, 3, 5}: columns 0 / 3 / 5 are 1 / -1 / -1, the bottom row is
    // transparent and keeps the stored 0.5.
    assert_eq!(floats(&result["depths"]), [1.0, -1.0, -1.0, 1.0, -1.0, -1.0, 0.5, 0.5, 0.5]);
    assert_eq!(result["winningLayerIds"], json!(["mid", "mid", "mid", "mid", "mid", "mid", "", "", ""]));
    assert_eq!((result["sampledVertices"].clone(), result["missingVertices"].clone()), (json!(6), json!(3)));
    assert_eq!((result["hasDepthRange"].clone(), result["minDepth"].clone(), result["maxDepth"].clone()), (json!(true), json!(-1.0), json!(1.0)));
    assert_eq!(result["layerStats"], json!([{
        "layerId": "mid", "sampledVertices": 6, "missingVertices": 3, "winningVertices": 6, "contributedVertices": 6,
        "additiveMerge": false, "hasDepthRange": true, "minDepth": -1.0, "maxDepth": 1.0,
    }]));
    assert_eq!(result["apply"], json!({
        "succeeded": true, "changedTargets": 1, "changedVertices": 6, "layersApplied": 1, "layersSkipped": 0,
        "missingSamples": 3,
    }));

    // With no target given, the selected one is used.
    fails(&mut session, "depth.draw.compose", json!({}), "no target is selected");
    ok(&mut session, "depth.draw.select", json!({"target": 5}));
    assert_eq!(ok(&mut session, "depth.draw.compose", json!({}))["target"], 5);
    fails(&mut session, "depth.draw.compose", json!({"target": 1}), "GridDeformer or PathDeformer");
    fails(&mut session, "depth.draw.compose", json!({"target": 99}), "was not found");
}

#[test]
fn merge_policies_and_z_transform_decide_the_composed_depth() {
    let dir = temp_dir("merge");
    let mut session = draw_session();
    loaded(&mut session, &dir);
    // back (-1 everywhere) < mid < front (1 everywhere): Frontmost keeps the highest sample.
    ok(&mut session, "depth.draw.binding.update", json!({"layer": "back", "target": 5, "order": 0}));
    ok(&mut session, "depth.draw.binding.update", json!({"layer": "mid", "target": 5, "order": 1}));
    ok(&mut session, "depth.draw.binding.update", json!({"layer": "front", "target": 5, "order": 2}));
    let result = ok(&mut session, "depth.draw.compose", json!({"target": 5}));
    assert_eq!(floats(&result["depths"]), [1.0; 9]);
    assert_eq!(
        result["winningLayerIds"],
        json!(["mid", "front", "front", "mid", "front", "front", "front", "front", "front"]),
        "mid wins column 0 because front's equal sample is not in front of it"
    );

    // Add: the samples sum on the base. back -1, mid (1, -1, -1; bottom row transparent), front +1.
    for layer in ["back", "mid", "front"] {
        ok(&mut session, "depth.draw.binding.update", json!({"layer": layer, "target": 5, "mergePolicy": "Add"}));
    }
    let added = ok(&mut session, "depth.draw.compose", json!({"target": 5}));
    // The first Add starts from the stored 0.5; the transparent bottom row only gets back and front.
    assert_eq!(floats(&added["depths"]), [1.5, -0.5, -0.5, 1.5, -0.5, -0.5, 0.5, 0.5, 0.5]);

    // The layer z transform scales and shifts the sample: front becomes 1 * 0.5 + 0.25 = 0.75.
    ok(&mut session, "depth.draw.binding.update", json!({"layer": "front", "target": 5, "mergePolicy": "Replace"}));
    ok(&mut session, "depth.draw.layer.update", json!({"layer": "front", "zScale": 0.5, "zOffset": 0.25}));
    let replaced = ok(&mut session, "depth.draw.compose", json!({"target": 5}));
    assert_eq!(floats(&replaced["depths"]), [0.75; 9]);
    assert_eq!(replaced["layerStats"][2]["maxDepth"], 0.75);

    // A disabled binding drops out.
    ok(&mut session, "depth.draw.binding.update", json!({"layer": "front", "target": 5, "enabled": false}));
    let without = ok(&mut session, "depth.draw.compose", json!({"target": 5}));
    assert_eq!(without["layerStats"].as_array().unwrap().len(), 2);
}

// ---------------------------------------------------------------------------- depth.draw.apply

#[test]
fn apply_writes_the_composed_depths_in_one_undo_step() {
    let dir = temp_dir("apply");
    let mut session = draw_session();
    ok(&mut session, "depth.draw.load_png", json!({"path": mid_png(&dir)}));
    ok(&mut session, "depth.draw.layer.update", json!({"layer": "mid", "convolution": "Nearest"}));
    ok(&mut session, "depth.draw.binding.update", json!({"layer": "mid", "target": 5}));
    let steps = history(&session);
    let result = ok(&mut session, "depth.draw.apply", json!({"target": 5}));
    assert_eq!(history(&session), steps + 1, "one undo step");
    assert_eq!(depths(&session, 5), [1.0, -1.0, -1.0, 1.0, -1.0, -1.0, 0.5, 0.5, 0.5]);
    assert_eq!(result["apply"], json!({
        "succeeded": true, "changedTargets": 1, "changedVertices": 6, "layersApplied": 1, "layersSkipped": 0,
        "missingSamples": 3,
    }));
    let grid = node(&session, 5);
    assert!(grid.get("depth-ops").is_none() && grid.get("depth-op-base-depths").is_none());

    ok(&mut session, "undo", json!({}));
    assert_eq!(depths(&session, 5), [0.5; 9]);
    ok(&mut session, "redo", json!({}));
    assert_eq!(depths(&session, 5)[0], 1.0);

    // Applying again changes nothing and records nothing.
    let steps = history(&session);
    let again = ok(&mut session, "depth.draw.apply", json!({"target": 5}));
    assert_eq!(again["apply"]["changedVertices"], 0);
    assert_eq!(history(&session), steps);
}

#[test]
fn apply_on_a_grid_with_saved_operations_refreshes_the_base_and_runs_the_operations() {
    let dir = temp_dir("apply-ops");
    let mut session = draw_session();
    ok(&mut session, "depth.edit.apply", json!({"target": 5, "operations": [attached(0, 0.25)]}));
    assert_eq!(depths(&session, 5)[0], 0.75);
    ok(&mut session, "depth.draw.load_png", json!({"path": mid_png(&dir)}));
    ok(&mut session, "depth.draw.layer.update", json!({"layer": "mid", "convolution": "Nearest"}));
    ok(&mut session, "depth.draw.binding.update", json!({"layer": "mid", "target": 5}));
    let steps = history(&session);
    let result = ok(&mut session, "depth.draw.apply", json!({"target": 5}));
    assert_eq!(history(&session), steps + 1, "one undo step for the base and the depths together");
    let grid = node(&session, 5);
    // The composed depths become the operation base; the depths are that base plus the attached point (D).
    assert_eq!(floats(&grid["depth-op-base-depths"]), [1.0, -1.0, -1.0, 1.0, -1.0, -1.0, 0.5, 0.5, 0.5]);
    assert_eq!(floats(&grid["depths"]), [1.25, -1.0, -1.0, 1.0, -1.0, -1.0, 0.5, 0.5, 0.5]);
    assert_eq!(grid["depth-ops"].as_array().unwrap().len(), 1, "the operation list stays");
    assert_eq!(result["apply"]["changedVertices"], 6);

    ok(&mut session, "undo", json!({}));
    let grid = node(&session, 5);
    assert_eq!(floats(&grid["depths"]), [0.75, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5]);
    assert_eq!(floats(&grid["depth-op-base-depths"]), [0.5; 9], "undo restores the previous base too");
}

#[test]
fn apply_refuses_a_target_that_cannot_hold_depths() {
    let dir = temp_dir("apply-bad");
    let mut session = draw_session();
    ok(&mut session, "depth.draw.load_png", json!({"path": mid_png(&dir)}));
    fails(&mut session, "depth.draw.apply", json!({}), "no target is selected");
    fails(&mut session, "depth.draw.apply", json!({"target": 6}), "GridDeformer or PathDeformer");
    fails(&mut session, "depth.draw.apply", json!({"target": 99}), "was not found");
}

#[test]
fn a_path_deformer_target_composes_and_applies_without_operations() {
    let dir = temp_dir("path");
    let mut session = session_with(vec![json!({
        "uuid": 9, "name": "Hair", "type": "PathDeformer", "transform": transform(), "children": [],
        "vertices": [-3.0, -3.0, 0.0, 0.0, 2.0, 2.0],
    })]);
    ok(&mut session, "depth.draw.load_png", json!({"path": mid_png(&dir)}));
    ok(&mut session, "depth.draw.layer.update", json!({"layer": "mid", "convolution": "Nearest"}));
    ok(&mut session, "depth.draw.binding.update", json!({"layer": "mid", "target": 9}));
    // Document pixels (0,0), (3,3) and (5,5): 1, -1 and the transparent bottom row, which keeps the base 0.
    let composed = ok(&mut session, "depth.draw.compose", json!({"target": 9}));
    assert_eq!(floats(&composed["depths"]), [1.0, -1.0, 0.0]);
    let steps = history(&session);
    ok(&mut session, "depth.draw.apply", json!({"target": 9}));
    assert_eq!(history(&session), steps + 1);
    assert_eq!(depths(&session, 9), [1.0, -1.0, 0.0]);
}

// ------------------------------------------------------------ auto bind, select, display, diagnostics

#[test]
fn auto_bind_matches_layers_to_parts_and_selection_follows() {
    let dir = temp_dir("auto");
    // No Part is called "back": its layer finds nothing.
    let mut session = session_with(vec![json!({
        "uuid": 5, "name": "Grid", "type": "GridDeformer", "transform": transform(),
        "grid_axis_x": [-3.0, 0.0, 2.0], "grid_axis_y": [-3.0, 0.0, 2.0], "depths": vec![0.5; 9],
        "children": [part(6, "elsewhere"), part(7, "mid"), part(8, "front")],
    })]);
    for (name, pixel) in [("back", grey(0)), ("mid", grey(100)), ("front", grey(255))] {
        ok(&mut session, "depth.draw.load_png", json!({"path": write_png(&dir, &format!("{name}.png"), 6, 6, |_, _| pixel)}));
    }
    let result = ok(&mut session, "depth.draw.auto_bind", json!({"matchDirectGridName": false}));
    assert_eq!(result["matched"], 2);
    let statuses: Vec<(&str, &str)> = result["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["layerId"].as_str().unwrap(), r["status"].as_str().unwrap()))
        .collect();
    assert_eq!(statuses, [("back", "Unmatched"), ("mid", "Matched"), ("front", "Matched")]);
    assert_eq!(result["results"][1]["matchedNodeName"], "mid");
    assert_eq!(result["results"][1]["targetGridName"], "Grid");
    assert_eq!(result["results"][1]["targetGridUuid"], "5");
    // D binds with the layer's index as the order.
    assert_eq!(result["bindings"], json!([
        {"layerId": "mid", "targetNodeUuid": "7", "targetGridUuid": "5", "order": 1, "enabled": true,
         "useNormalLayerAlpha": true, "coverageThreshold": 0.009999999776482582, "mergePolicy": "Frontmost"},
        {"layerId": "front", "targetNodeUuid": "8", "targetGridUuid": "5", "order": 2, "enabled": true,
         "useNormalLayerAlpha": true, "coverageThreshold": 0.009999999776482582, "mergePolicy": "Frontmost"},
    ]));

    // Running it again keeps the same two bindings.
    let again = ok(&mut session, "depth.draw.auto_bind", json!({}));
    assert_eq!(again["bindings"].as_array().unwrap().len(), 2);

    let selected = ok(&mut session, "depth.draw.select", json!({"layer": "mid", "target": 5}));
    assert_eq!((selected["selectedLayerId"].clone(), selected["selectedGridUuid"].clone()), (json!("mid"), json!("5")));
    let by_point = ok(&mut session, "depth.draw.select", json!({"clear": true, "point": [1.0, 1.0]}));
    assert_eq!(by_point["hit"], true);
    assert_eq!(by_point["selectedLayerId"], "front", "the topmost layer plane under the point");
    assert_eq!(by_point["selectedGridUuid"], "5");
    let miss = ok(&mut session, "depth.draw.select", json!({"clear": true, "point": [60.0, 60.0]}));
    assert_eq!((miss["hit"].clone(), miss["selectedLayerId"].clone()), (json!(false), json!("")));
    fails(&mut session, "depth.draw.select", json!({}), "nothing to select");
    fails(&mut session, "depth.draw.select", json!({"layer": "nope"}), "was not found");
    fails(&mut session, "depth.draw.select", json!({"target": 7}), "has no binding");
}

#[test]
fn display_options_are_set_individually() {
    let mut session = draw_session();
    let result = ok(&mut session, "depth.draw.display", json!({"showRawDepth": true, "showComposite": false}));
    assert_eq!(result["changed"], true);
    assert_eq!(result["display"], json!({
        "showNormalImage": true, "showRawDepth": true, "showCoverage": false, "showComposite": false,
        "showLayerPlanes": true, "showDepthRanges": true, "showMissingVertices": true, "showWinningLayer": false,
        "useGpuPreview": false,
    }));
    assert_eq!(ok(&mut session, "depth.draw.display", json!({"showRawDepth": true}))["changed"], false);
    fails(&mut session, "depth.draw.display", json!({}), "no display option");
    fails(&mut session, "depth.draw.display", json!({"showRawDepth": 1}), "showRawDepth");
}

#[test]
fn diagnostics_report_the_layer_table_the_gaps_and_the_manifest_check() {
    let dir = temp_dir("diagnostics");
    let mut session = draw_session();
    loaded(&mut session, &dir);
    ok(&mut session, "depth.draw.select", json!({"target": 5}));
    let result = ok(&mut session, "depth.draw.diagnostics", json!({}));
    assert_eq!(result["target"], 5);
    assert_eq!(result["composed"], true);
    assert_eq!(result["source"], json!({
        "totalLayers": 3, "usableDepthLayers": 3, "disabledLayers": 0, "invisibleLayers": 0,
        "layersMissingDepthPixels": 0, "layersMissingTarget": 0, "layersMissingCoverage": 3,
        "layersWithAmbiguousBindings": 0, "totalBindings": 3, "enabledBindings": 3,
    }));
    let rows = result["rows"].as_array().unwrap();
    assert_eq!(rows.iter().map(|r| r["layerId"].as_str().unwrap()).collect::<Vec<_>>(), ["back", "mid", "front"]);
    let mid = &rows[1];
    assert_eq!((mid["targetDisplayName"].clone(), mid["mergePolicy"].clone()), (json!("Grid"), json!("Frontmost")));
    assert_eq!(
        (mid["sampledVertices"].clone(), mid["missingVertices"].clone(), mid["winningVertices"].clone()),
        (json!(6), json!(3), json!(2))
    );
    assert_eq!((mid["minDepth"].clone(), mid["maxDepth"].clone()), (json!(-1.0), json!(1.0)));
    assert_eq!(mid["warnings"], json!({
        "missingDepthPixels": false, "missingTarget": false, "missingCoverage": true, "mostlyMissingSamples": false,
    }));
    let gaps = result["depthSpace"]["gaps"].as_array().unwrap();
    assert_eq!((gaps[0]["backLayerId"].clone(), gaps[0]["frontLayerId"].clone()), (json!("back"), json!("mid")));
    assert_eq!((gaps[0]["valid"].clone(), gaps[0]["overlap"].clone()), (json!(true), json!(false)), "back max -1, mid min -1");
    assert_eq!((gaps[0]["backDepth"].clone(), gaps[0]["frontDepth"].clone()), (json!(-1.0), json!(-1.0)));
    assert_eq!((gaps[1]["valid"].clone(), gaps[1]["overlap"].clone()), (json!(true), json!(false)), "mid max 1, front min 1");
    assert_eq!(result["validation"]["ok"], true);

    // Without a target the table has no composed statistics.
    let plain = ok(&mut session, "depth.draw.diagnostics", json!({"target": 5}));
    assert_eq!(plain["rows"][1]["sampledVertices"], 6);
    ok(&mut session, "depth.draw.select", json!({"clear": true}));
    let bare = ok(&mut session, "depth.draw.diagnostics", json!({}));
    assert_eq!((bare["composed"].clone(), bare["rows"][1]["sampledVertices"].clone()), (json!(false), json!(0)));
}

// ----------------------------------------------------------------------------- cleanup and fit z

/// 6x6 grey 200, with a hole of depth 0 at (3, 3).
fn holed_png(dir: &Path) -> String {
    write_png(dir, "holed.png", 6, 6, |x, y| if (x, y) == (3, 3) { grey(0) } else { grey(200) })
}

fn exported_pixels(path: &Path) -> image::RgbaImage {
    image::open(path).unwrap().to_rgba8()
}

#[test]
fn gap_fill_repairs_a_hole_and_records_a_replayable_operation() {
    let dir = temp_dir("gap");
    let out = temp_dir("gap-out");
    let mut session = draw_session();
    ok(&mut session, "depth.draw.load_png", json!({"path": holed_png(&dir)}));
    let first = ok(&mut session, "depth.draw.cleanup", json!({"layer": "holed", "kind": "gap_fill"}));
    assert_eq!((first["succeeded"].clone(), first["changed"].clone()), (json!(true), json!(true)));
    assert_eq!(first["detected"], json!({"zero": 1, "depression": 0, "cliff": 1, "focusedAdded": 0, "total": 1}));
    assert_eq!((first["filled"].clone(), first["remaining"].clone()), (json!(1), json!(0)));
    ok(&mut session, "depth.draw.export_png", json!({"dir": out.to_str().unwrap()}));
    let pixels = exported_pixels(&out.join("000-holed.png"));
    assert_eq!(pixels.get_pixel(3, 3).0, [200, 200, 200, 255], "the hole takes the median of its neighbours");
    assert_eq!(pixels.get_pixel(0, 0).0, [200, 200, 200, 255]);

    let second = ok(&mut session, "depth.draw.cleanup", json!({"layer": "holed", "kind": "gap_fill"}));
    assert_eq!(second["changed"], false, "nothing is left to fill");
    let manifest = ok(&mut session, "depth.draw.manifest.get", json!({}));
    assert_eq!(manifest["manifest"]["layers"][0]["cleanupOperations"], json!([
        {"kind": "AlphaDepthGapFill", "contourThickness": 2, "focusedRules": []},
    ]));

    // A focused rule lifts a region that is not a hole: the rule box is clamped to the layer.
    let focused = ok(&mut session, "depth.draw.cleanup", json!({
        "layer": "holed", "kind": "gap_fill", "rules": [{"x": -3, "y": 0, "w": 100, "h": 2, "lift": 10, "radius": 4}],
    }));
    assert_eq!(focused["succeeded"], true);
    assert_eq!(focused["changed"], false, "a flat region has nothing to lift");
}

#[test]
fn contour_repair_rewrites_the_border_band_from_the_inside() {
    let dir = temp_dir("contour");
    let out = temp_dir("contour-out");
    let mut session = draw_session();
    // A 4x4 opaque square at (1..4, 1..4): depth 100 on its border ring, 200 inside; transparent outside.
    let path = write_png(&dir, "square.png", 6, 6, |x, y| {
        let inside = (1..=4).contains(&x) && (1..=4).contains(&y);
        let core = (2..=3).contains(&x) && (2..=3).contains(&y);
        if !inside { [0, 0, 0, 0] } else if core { grey(200) } else { grey(100) }
    });
    ok(&mut session, "depth.draw.load_png", json!({"path": path}));
    let result = ok(&mut session, "depth.draw.cleanup", json!({"layer": "square", "kind": "contour_repair", "thickness": 0}));
    assert_eq!(
        (result["contourPixels"].clone(), result["filledPixels"].clone(), result["changed"].clone()),
        (json!(12), json!(12), json!(true)),
        "thickness 0 is raised to 1: the 12-pixel border ring is re-estimated"
    );
    ok(&mut session, "depth.draw.export_png", json!({"dir": out.to_str().unwrap()}));
    let pixels = exported_pixels(&out.join("000-square.png"));
    assert_eq!(pixels.get_pixel(1, 1).0, [200, 200, 200, 255]);
    assert_eq!(pixels.get_pixel(4, 2).0, [200, 200, 200, 255]);
    assert_eq!(pixels.get_pixel(0, 0).0, [0, 0, 0, 0], "outside the mask stays empty");
    let manifest = ok(&mut session, "depth.draw.manifest.get", json!({}));
    assert_eq!(manifest["manifest"]["layers"][0]["cleanupOperations"], json!([
        {"kind": "ContourRepair", "contourThickness": 1, "focusedRules": []},
    ]));
    // Thickness 2 grows the band once more: all 16 pixels of the square are seeded, none has a neighbour left
    // to fill from, so nothing changes and no operation is recorded.
    let wide = ok(&mut session, "depth.draw.cleanup", json!({"layer": "square", "kind": "contour_repair", "thickness": 99}));
    assert_eq!(
        (wide["contourPixels"].clone(), wide["filledPixels"].clone(), wide["changed"].clone()),
        (json!(16), json!(0), json!(false))
    );

    fails(&mut session, "depth.draw.cleanup", json!({"layer": "square", "kind": "blur"}), "gap_fill or contour_repair");
    fails(&mut session, "depth.draw.cleanup", json!({"layer": "nope", "kind": "gap_fill"}), "was not found");
    fails(&mut session, "depth.draw.cleanup", json!({"layer": "square", "kind": "gap_fill", "rules": [{"x": 1}]}), "rule.y");
}

#[test]
fn fit_z_places_a_layer_in_a_gap() {
    let dir = temp_dir("fit");
    let mut session = draw_session();
    loaded(&mut session, &dir);
    // mid samples -1 / 1 on the grid. Fitting [-1, 1] into [0 + 0.5, 2 - 0.5] gives scale 0.5, offset 1.0.
    let values = ok(&mut session, "depth.draw.fit_z", json!({
        "layer": "mid", "target": 5, "gap": "values", "back": 0.0, "front": 2.0, "margin": 0.5,
    }));
    assert_eq!((values["succeeded"].clone(), values["applied"].clone()), (json!(true), json!(true)));
    assert_eq!((values["zScale"].clone(), values["zOffset"].clone()), (json!(0.5), json!(1.0)));
    assert_eq!((values["rawMin"].clone(), values["rawMax"].clone()), (json!(-1.0), json!(1.0)));
    assert_eq!((values["targetMin"].clone(), values["targetMax"].clone()), (json!(0.5), json!(1.5)));
    let layer = &ok(&mut session, "depth.draw.manifest.get", json!({}))["manifest"]["layers"][1];
    assert_eq!((layer["zScale"].clone(), layer["zOffset"].clone()), (json!(0.5), json!(1.0)));

    // Back to the identity, then fit between the neighbours: back ends at -1, front starts at 1.
    ok(&mut session, "depth.draw.layer.update", json!({"layer": "mid", "zScale": 1.0, "zOffset": 0.0}));
    let adjacent = ok(&mut session, "depth.draw.fit_z", json!({"layer": "mid", "target": 5, "margin": 0.5, "apply": false}));
    assert_eq!((adjacent["gapBack"].clone(), adjacent["gapFront"].clone()), (json!(-1.0), json!(1.0)));
    assert_eq!((adjacent["zScale"].clone(), adjacent["zOffset"].clone()), (json!(0.5), json!(0.0)));
    assert_eq!(adjacent["applied"], false, "apply:false only reports");
    let untouched = &ok(&mut session, "depth.draw.manifest.get", json!({}))["manifest"]["layers"][1];
    assert_eq!(untouched["zScale"], 1.0);
    ok(&mut session, "depth.draw.fit_z", json!({"layer": "mid", "target": 5, "margin": 0.5}));
    let fitted = &ok(&mut session, "depth.draw.manifest.get", json!({}))["manifest"]["layers"][1];
    assert_eq!((fitted["zScale"].clone(), fitted["zOffset"].clone()), (json!(0.5), json!(0.0)));

    // The target's own range, and the range between two chosen layers.
    let target = ok(&mut session, "depth.draw.fit_z", json!({"layer": "mid", "target": 5, "gap": "target", "apply": false}));
    assert_eq!((target["gapBack"].clone(), target["gapFront"].clone()), (json!(0.5), json!(0.5)));
    assert_eq!(target["succeeded"], false);
    assert_eq!(target["error"], "gap is empty after margin");
    let layers = ok(&mut session, "depth.draw.fit_z", json!({
        "layer": "mid", "target": 5, "gap": "layers", "backLayer": "back", "frontLayer": "front", "apply": false,
    }));
    assert_eq!((layers["gapBack"].clone(), layers["gapFront"].clone(), layers["succeeded"].clone()), (json!(-1.0), json!(1.0), json!(true)));

    // The first layer has no neighbour on one side.
    let none = ok(&mut session, "depth.draw.fit_z", json!({"layer": "back", "target": 5}));
    assert_eq!((none["succeeded"].clone(), none["applied"].clone()), (json!(false), json!(false)));
    assert_eq!(none["error"], "adjacent layers are required");

    fails(&mut session, "depth.draw.fit_z", json!({"layer": "mid", "target": 5, "gap": "wide"}), "gap must be");
    fails(&mut session, "depth.draw.fit_z", json!({"layer": "mid", "target": 5, "gap": "values", "back": 1.0}), "front is required");
    fails(&mut session, "depth.draw.fit_z", json!({"layer": "mid", "target": 5, "gap": "layers", "backLayer": "x", "frontLayer": "front"}), "was not found");
    ok(&mut session, "depth.draw.binding.update", json!({"layer": "mid", "target": 5, "enabled": false}));
    fails(&mut session, "depth.draw.fit_z", json!({"layer": "mid", "target": 5}), "is not bound");
}

// ---------------------------------------------------------------------------- manifest and files

#[test]
fn export_png_refuses_source_and_duplicate_targets_before_creating_directories() {
    let dir = temp_dir("export-protected");
    let mut session = draw_session();
    let source = dir.join("source.inx");
    fails(&mut session, "depth.draw.export_png", json!({"dir":""}), "must not be empty");
    ok(&mut session, "save", json!({"path": source}));
    let before = fs::read(&source).unwrap();
    let output = dir.join("missing/nested");
    fails(&mut session, "depth.draw.export_png", json!({"dir": output, "manifest": source, "overwrite": true}), "own input");
    assert_eq!(fs::read(&source).unwrap(), before);
    assert!(!dir.join("missing").exists());

    let image = holed_png(&dir);
    ok(&mut session, "depth.draw.load_png", json!({"path": image, "id": "owned"}));
    let image_bytes = fs::read(&image).unwrap();
    fails(&mut session, "depth.draw.export_png", json!({"dir": output, "manifest": image, "overwrite": true}), "own input");
    assert_eq!(fs::read(&image).unwrap(), image_bytes);
    fails(&mut session, "depth.draw.export_png", json!({"dir": output, "manifest": output.join("000-owned.png"), "overwrite": true}), "same file");
    assert!(!dir.join("missing").exists());
    fails(&mut session, "depth.draw.export_png", json!({"dir": output, "manifest": dir.join("elsewhere/m.json")}), "parent directory");
    assert!(!dir.join("missing").exists());
    // Retained pixels can still be exported after the original input is removed.
    fs::remove_file(&image).unwrap();
    let result = ok(&mut session, "depth.draw.export_png", json!({"dir": output}));
    assert_eq!(result["exportedLayers"], 1);
    assert!(output.join("000-owned.png").is_file());

    let collision = dir.join("000-owned.png");
    fs::copy(output.join("000-owned.png"), &collision).unwrap();
    let mut second = draw_session();
    ok(&mut second, "depth.draw.load_png", json!({"path":collision,"id":"owned"}));
    let bytes = fs::read(&collision).unwrap();
    fails(&mut second, "depth.draw.export_png", json!({"dir":dir,"overwrite":true}), "own input");
    assert_eq!(fs::read(&collision).unwrap(), bytes);
    assert!(!dir.join("manifest.json").exists());
}

#[cfg(unix)]
#[test]
fn export_png_keeps_retained_pixels_when_the_source_symlink_is_dangling() {
    use std::os::unix::fs::symlink;
    let dir = temp_dir("export-dangling-source");
    let image = holed_png(&dir);
    let alias = dir.join("alias.png");symlink(&image, &alias).unwrap();
    let mut session = draw_session();
    ok(&mut session, "depth.draw.load_png", json!({"path":alias,"id":"owned"}));
    fs::remove_file(image).unwrap();
    let output = dir.join("output");
    assert_eq!(ok(&mut session, "depth.draw.export_png", json!({"dir":output}))["exportedLayers"], 1);
    assert!(output.join("000-owned.png").is_file());
    assert!(fs::symlink_metadata(alias).unwrap().file_type().is_symlink());
}

#[test]
fn export_png_refuses_case_equivalent_sources_and_planned_outputs() {
    let dir = temp_dir("export-case-collisions");
    let image = holed_png(&dir);
    let source = dir.join("000-Owned.PNG");fs::rename(image, &source).unwrap();
    let original = fs::read(&source).unwrap();
    let mut session = draw_session();
    ok(&mut session, "depth.draw.load_png", json!({"path":source,"id":"owned"}));
    fails(&mut session, "depth.draw.export_png", json!({"dir":dir,"overwrite":true}), "own input");
    assert_eq!(fs::read(&source).unwrap(), original);
    assert!(!dir.join("manifest.json").exists());
    let output = dir.join("missing");
    fails(&mut session, "depth.draw.export_png", json!({"dir":output,"manifest":output.join("000-OWNED.PNG"),"overwrite":true}), "same file");
    assert!(!output.exists());
}

#[cfg(unix)]
#[test]
fn export_png_resolves_manifest_links_and_preserves_source_documents() {
    use std::os::unix::fs::symlink;
    let dir = temp_dir("export-links");
    let mut session = draw_session();
    let source = dir.join("source.inx");
    ok(&mut session, "save", json!({"path": source}));
    let original = fs::read(&source).unwrap();
    let alias = dir.join("manifest-link");symlink(&source, &alias).unwrap();
    let output = dir.join("new-output");
    fails(&mut session, "depth.draw.export_png", json!({"dir": output, "manifest": alias, "overwrite": true}), "own input");
    assert_eq!(fs::read(&source).unwrap(), original);assert!(!output.exists());
    fs::remove_file(&alias).unwrap();
    let external = dir.join("external.json");fs::write(&external, b"old").unwrap();symlink(&external, &alias).unwrap();
    let result = ok(&mut session, "depth.draw.export_png", json!({"dir": output, "manifest": alias, "overwrite": true}));
    assert_eq!(result["manifest"], fs::canonicalize(&external).unwrap().to_string_lossy().as_ref());
    assert!(fs::symlink_metadata(&alias).unwrap().file_type().is_symlink());
    assert!(serde_json::from_slice::<Value>(&fs::read(&external).unwrap()).is_ok());
}

#[test]
fn export_png_writes_depth_pngs_and_a_manifest_and_refuses_to_overwrite() {
    let dir = temp_dir("export-in");
    let out = temp_dir("export-out");
    let mut session = draw_session();
    loaded(&mut session, &dir);
    ok(&mut session, "depth.draw.load_png", json!({"path": holed_png(&dir)}));
    ok(&mut session, "depth.draw.cleanup", json!({"layer": "holed", "kind": "gap_fill"}));
    let result = ok(&mut session, "depth.draw.export_png", json!({"dir": out.to_str().unwrap()}));
    assert_eq!((result["exportedLayers"].clone(), result["skippedLayers"].clone()), (json!(4), json!(0)));
    for name in ["000-back.png", "001-mid.png", "002-front.png", "003-holed.png", "manifest.json"] {
        assert!(out.join(name).is_file(), "{name}");
    }
    let mid = exported_pixels(&out.join("001-mid.png"));
    assert_eq!(mid.get_pixel(0, 0).0, [255, 255, 255, 255]);
    assert_eq!(mid.get_pixel(4, 0).0, [0, 0, 0, 255]);
    assert_eq!(mid.get_pixel(0, 5).0, [0, 0, 0, 0]);
    let manifest: Value = serde_json::from_slice(&fs::read(out.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["layers"][1]["sourcePath"], "001-mid.png", "relative to the manifest");
    assert_eq!(manifest["layers"][3]["cleanupOperations"], json!([]), "cleanup is baked into the PNG");
    assert_eq!(exported_pixels(&out.join("003-holed.png")).get_pixel(3, 3).0, [200, 200, 200, 255]);
    assert_eq!(manifest["bindings"].as_array().unwrap().len(), 3);

    // Existing files are kept unless asked; nothing is written when any destination exists.
    let stamp = fs::read(out.join("000-back.png")).unwrap();
    fs::remove_file(out.join("manifest.json")).unwrap();
    fails(&mut session, "depth.draw.export_png", json!({"dir": out.to_str().unwrap()}), "already exists");
    assert!(!out.join("manifest.json").exists(), "the refused export wrote nothing");
    let again = ok(&mut session, "depth.draw.export_png", json!({"dir": out.to_str().unwrap(), "overwrite": true}));
    assert_eq!(again["exportedLayers"], 4);
    assert!(out.join("manifest.json").is_file());
    assert_eq!(fs::read(out.join("000-back.png")).unwrap(), stamp);
    // A directory in the way is not overwritten even when asked.
    let blocked = temp_dir("export-blocked");
    fs::create_dir_all(blocked.join("000-back.png")).unwrap();
    fails(&mut session, "depth.draw.export_png", json!({"dir": blocked.to_str().unwrap(), "overwrite": true}), "not a regular file");
    // The manifest can live elsewhere; PNG references are then relative to it.
    let elsewhere = temp_dir("export-manifest");
    let moved = temp_dir("export-moved");
    ok(&mut session, "depth.draw.export_png", json!({
        "dir": moved.to_str().unwrap(), "manifest": elsewhere.join("m.json").to_str().unwrap(),
    }));
    let reference: Value = serde_json::from_slice(&fs::read(elsewhere.join("m.json")).unwrap()).unwrap();
    let source = reference["layers"][0]["sourcePath"].as_str().unwrap().to_owned();
    assert!(source.ends_with("000-back.png") && Path::new(&source).is_absolute(), "{source}");
}

#[test]
fn an_exported_manifest_loads_back_and_the_png_attaches_to_its_layer() {
    let dir = temp_dir("roundtrip-in");
    let out = temp_dir("roundtrip-out");
    let mut session = draw_session();
    loaded(&mut session, &dir);
    ok(&mut session, "depth.draw.layer.update", json!({"layer": "mid", "zScale": 0.5, "zOffset": 0.25}));
    ok(&mut session, "depth.draw.export_png", json!({"dir": out.to_str().unwrap()}));

    let mut other = draw_session();
    let set = ok(&mut other, "depth.draw.manifest.set", json!({"path": out.join("manifest.json").to_str().unwrap()}));
    assert_eq!(set["layers"], json!([
        {"id": "back", "hasDepthPixels": false, "hasNormalCoverage": false},
        {"id": "mid", "hasDepthPixels": false, "hasNormalCoverage": false},
        {"id": "front", "hasDepthPixels": false, "hasNormalCoverage": false},
    ]));
    assert_eq!(set["validation"]["ok"], true, "the PNG files exist next to the manifest: {}", set["validation"]);
    assert_eq!(other.snapshot()["modified"], true, "the session changed since the document was opened");
    // Pixel-less layers compose to nothing.
    let nothing = ok(&mut other, "depth.draw.compose", json!({"target": 5}));
    assert_eq!(nothing["sampledVertices"], 0);

    // The exported PNG reattaches to its layer and the saved settings survive.
    let attached = ok(&mut other, "depth.draw.load_png", json!({"path": out.join("001-mid.png").to_str().unwrap(), "id": "mid"}));
    assert_eq!(attached["layers"], 3, "no new layer");
    assert_eq!((attached["layer"]["zScale"].clone(), attached["layer"]["zOffset"].clone()), (json!(0.5), json!(0.25)));
    assert_eq!(attached["layer"]["convolution"], "Nearest");
    let composed = ok(&mut other, "depth.draw.compose", json!({"target": 5}));
    assert_eq!(composed["layerStats"][0]["layerId"], "mid");
    assert_eq!(composed["layerStats"][0]["maxDepth"], 0.75, "1 * 0.5 + 0.25");

    // A size mismatch is refused.
    let wrong = write_png(&dir, "wrong.png", 4, 4, |_, _| grey(9));
    fails(&mut other, "depth.draw.load_png", json!({"path": wrong, "id": "front"}), "saved as 6x6");
    // Both manifest and path, a bad merge policy, and a non-object are refused.
    fails(&mut other, "depth.draw.manifest.set", json!({"manifest": {}, "path": "x"}), "either manifest or path");
    fails(&mut other, "depth.draw.manifest.set", json!({}), "manifest or path is required");
    fails(
        &mut other,
        "depth.draw.manifest.set",
        json!({"manifest": {"bindings": [{"layerId": "a", "mergePolicy": "Best"}]}}),
        "Best",
    );
    fails(&mut other, "depth.draw.manifest.set", json!({"manifest": 7}), "manifest must be an object");
    // Null clears.
    let cleared = ok(&mut other, "depth.draw.manifest.set", json!({"manifest": null}));
    assert_eq!(cleared["layers"], json!([]));
}

#[test]
fn a_manifest_layer_claiming_a_huge_size_without_pixels_is_harmless() {
    let mut session = draw_session();
    ok(&mut session, "depth.draw.manifest.set", json!({"manifest": {
        "layers": [{"id": "huge", "width": 2147483647, "height": 2147483647}],
        "bindings": [{"layerId": "huge", "targetGridUuid": "5", "targetNodeUuid": "5"}],
    }}));
    let composed = ok(&mut session, "depth.draw.compose", json!({"target": 5}));
    assert_eq!((composed["sampledVertices"].clone(), composed["layerStats"].clone()), (json!(0), json!([])));
    let diagnostics = ok(&mut session, "depth.draw.diagnostics", json!({"target": 5}));
    assert_eq!(diagnostics["source"]["layersMissingDepthPixels"], 1);
    fails(&mut session, "depth.draw.cleanup", json!({"layer": "huge", "kind": "gap_fill"}), "no depth pixels");
}

#[test]
fn the_session_is_saved_with_the_document_and_reset_by_a_new_one() {
    let dir = temp_dir("persist");
    let mut session = draw_session();
    assert_eq!(session.snapshot()["modified"], false);
    loaded(&mut session, &dir);
    ok(&mut session, "depth.draw.cleanup", json!({"layer": "mid", "kind": "gap_fill"}));
    ok(&mut session, "depth.draw.layer.update", json!({"layer": "front", "zOffset": 0.25}));
    ok(&mut session, "depth.draw.select", json!({"layer": "mid", "target": 5}));
    let snapshot = session.snapshot();
    assert_eq!(snapshot["modified"], true, "a DepthDraw change counts as unsaved work");
    assert_eq!(snapshot["depthDraw"], json!({"revision": 12, "layers": 3, "bindings": 3}));
    let expected = ok(&mut session, "depth.draw.manifest.get", json!({}))["manifest"].clone();
    fails(&mut session, "undo", json!({}), "nothing to undo");
    assert_eq!(history(&session), 0, "none of the DepthDraw changes is an undo step");

    let file = dir.join("scene.inx");
    ok(&mut session, "save", json!({"path": file.to_str().unwrap()}));
    assert_eq!(session.snapshot()["modified"], false);

    let mut reopened = Session::new();
    ok(&mut reopened, "open", json!({"path": file.to_str().unwrap()}));
    assert_eq!(reopened.snapshot()["modified"], false);
    let back = ok(&mut reopened, "depth.draw.manifest.get", json!({}));
    assert_eq!(back["manifest"], expected, "layers, bindings, selection and cleanup survive save and open");
    assert_eq!(back["layers"][1], json!({"id": "mid", "hasDepthPixels": false, "hasNormalCoverage": false}));
    // The recorded cleanup is replayed when the pixels come back.
    let attached = ok(&mut reopened, "depth.draw.load_png", json!({"path": mid_png(&dir), "id": "mid"}));
    assert_eq!(attached["layer"]["cleanupOperations"], expected["layers"][1]["cleanupOperations"]);

    // A document replacement resets the session.
    ok(&mut reopened, "new", json!({}));
    let fresh = ok(&mut reopened, "depth.draw.manifest.get", json!({}));
    assert_eq!(fresh["manifest"]["layers"], json!([]));
    assert_eq!(reopened.snapshot()["depthDraw"], json!({"revision": 0, "layers": 0, "bindings": 0}));
    // ...and a document saved without one carries no manifest key.
    let plain = dir.join("plain.inx");
    ok(&mut reopened, "save", json!({"path": plain.to_str().unwrap()}));
    let mut reread = Session::new();
    ok(&mut reread, "open", json!({"path": plain.to_str().unwrap()}));
    assert!(reread.snapshot()["model"].get("depthDrawSessionManifest").is_none());
    // Opening a document replaces the session as well: this one still holds three layers.
    assert_eq!(session.snapshot()["depthDraw"]["layers"], 3);
    ok(&mut session, "open", json!({"path": plain.to_str().unwrap()}));
    assert_eq!(session.snapshot()["depthDraw"], json!({"revision": 0, "layers": 0, "bindings": 0}));
    assert_eq!(session.snapshot()["modified"], false);
}

#[test]
fn a_manifest_this_build_cannot_read_stays_in_the_file() {
    let dir = temp_dir("unreadable");
    let model = json!({
        "meta": {"name": "x", "version": "1.0-alpha"},
        "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
            "transform": transform(), "lockToRoot": false, "pinToMesh": false, "children": []},
        "param": [], "automation": [], "animations": {},
        "depthDrawSessionManifest": "{\"bindings\": [{\"mergePolicy\": \"Newer\"}]}",
    });
    let mut session = Session::new();
    ok(&mut session, "new", json!({"model": model}));
    let manifest = ok(&mut session, "depth.draw.manifest.get", json!({}));
    assert_eq!(manifest["unreadable"], true);
    assert_eq!(manifest["manifest"]["layers"], json!([]));
    let file = dir.join("unreadable.inx");
    ok(&mut session, "save", json!({"path": file.to_str().unwrap()}));
    let mut reread = Session::new();
    ok(&mut reread, "open", json!({"path": file.to_str().unwrap()}));
    assert_eq!(
        reread.snapshot()["model"]["depthDrawSessionManifest"],
        "{\"bindings\": [{\"mergePolicy\": \"Newer\"}]}",
        "the text D or a newer build wrote is kept"
    );
    // A collected copy keeps it too.
    let out = temp_dir("unreadable-collect");
    ok(&mut session, "export.collect", json!({"dir": out.to_str().unwrap()}));
    let mut collected = Session::new();
    ok(&mut collected, "open", json!({"path": out.join("unreadable.inx").to_str().unwrap()}));
    assert_eq!(collected.snapshot()["model"]["depthDrawSessionManifest"], "{\"bindings\": [{\"mergePolicy\": \"Newer\"}]}");
    assert!(!out.join("assets").exists(), "nothing to copy, no assets folder");
    // Nor does a manifest that is JSON but no object stop it.
    let mut model = session.snapshot()["model"].clone();
    model["depthDrawSessionManifest"] = json!("[]");
    let mut odd = Session::new();
    ok(&mut odd, "new", json!({"model": model}));
    let out = temp_dir("not-an-object-collect");
    ok(&mut odd, "export.collect", json!({"dir": out.to_str().unwrap()}));
}

#[test]
fn collect_copies_the_document_and_its_depth_sources_and_names_them_relatively() {
    let dir = temp_dir("collect-sources");
    let other = temp_dir("collect-other");
    // The reply names the resolved folder.
    let out = fs::canonicalize(temp_dir("collect-out")).unwrap().join("shot");
    let mut session = draw_session();
    loaded(&mut session, &dir);
    // Another back.png (in other case), and one source two layers name.
    let second = write_png(&other, "Back.png", 6, 6, |_, _| grey(100));
    ok(&mut session, "depth.draw.load_png", json!({"path": second, "id": "back2"}));
    ok(&mut session, "depth.draw.load_png", json!({"path": mid_png(&dir), "id": "mid2"}));
    let before = session.snapshot();
    let open_manifest = ok(&mut session, "depth.draw.manifest.get", json!({}))["manifest"].clone();

    let collected = mcp_ok(&mut session, "export_collect", json!({"dir": out.to_str().unwrap()}));
    let assets = out.join("assets");
    assert_eq!(collected["path"], out.join("model.inx").to_str().unwrap());
    let files: Vec<PathBuf> = ["back.png", "mid.png", "front.png", "Back-2.png"].iter().map(|f| assets.join(f)).collect();
    assert_eq!(collected["files"], json!(files.iter().map(|f| f.to_str().unwrap()).collect::<Vec<_>>()));
    assert_eq!(fs::read(&files[0]).unwrap(), fs::read(dir.join("back.png")).unwrap());
    assert_eq!(fs::read(&files[3]).unwrap(), fs::read(&second).unwrap());
    assert_eq!(session.snapshot(), before, "the open document is unchanged");
    assert_eq!(ok(&mut session, "depth.draw.manifest.get", json!({}))["manifest"], open_manifest);

    let mut copy = Session::new();
    ok(&mut copy, "open", json!({"path": out.join("model.inx").to_str().unwrap()}));
    let manifest = ok(&mut copy, "depth.draw.manifest.get", json!({}))["manifest"].clone();
    let sources: Vec<&str> = manifest["layers"].as_array().unwrap().iter().map(|l| l["sourcePath"].as_str().unwrap()).collect();
    assert_eq!(sources, ["assets/back.png", "assets/mid.png", "assets/front.png", "assets/Back-2.png", "assets/mid.png"]);
    // Everything else in the manifest is as the open document has it.
    let mut expected = open_manifest.clone();
    for (layer, source) in expected["layers"].as_array_mut().unwrap().iter_mut().zip(&sources) {
        layer["sourcePath"] = json!(source);
    }
    assert_eq!(manifest, expected);
    // The copy finds its images beside it from any working directory (the tests run in rust/), and they load
    // by the names it holds.
    let validation = ok(&mut copy, "depth.draw.manifest.get", json!({}))["validation"].clone();
    assert_eq!(validation["missingSourceLayerIds"], json!([]), "{validation}");
    ok(&mut copy, "depth.draw.load_png", json!({"path": "assets/Back-2.png", "id": "back2"}));

    // A second run replaces the folder's files only with overwrite.
    fails(&mut session, "export.collect", json!({"dir": out.to_str().unwrap()}), "already exists");
    fs::write(&files[1], b"stale").unwrap();
    ok(&mut session, "export.collect", json!({"dir": out.to_str().unwrap(), "overwrite": true}));
    assert_eq!(fs::read(&files[1]).unwrap(), fs::read(dir.join("mid.png")).unwrap());
    // The copy collects again: its sources are the images beside it.
    let again = temp_dir("collect-again");
    ok(&mut copy, "export.collect", json!({"dir": again.to_str().unwrap()}));
    assert_eq!(fs::read(again.join("assets/Back-2.png")).unwrap(), fs::read(&second).unwrap());
}

#[test]
fn collect_refuses_a_missing_source_and_its_own_inputs_and_writes_nothing() {
    let dir = temp_dir("collect-refuse");
    let out = temp_dir("collect-refuse-out");
    let mut session = draw_session();
    loaded(&mut session, &dir);
    fails(&mut session, "export.collect", json!({"dir": 3}), "dir must be a string");
    fails(&mut session, "export.collect", json!({"dir": out.to_str().unwrap(), "overwrite": "yes"}), "overwrite must be");
    // The open document's own file.
    let saved = out.join("model.inx");
    ok(&mut session, "save", json!({"path": saved.to_str().unwrap()}));
    fails(&mut session, "export.collect", json!({"dir": out.to_str().unwrap(), "overwrite": true}), "its own input");
    fs::remove_file(&saved).unwrap();
    let mut session = draw_session();
    loaded(&mut session, &dir);
    // A source in the folder's assets/ would be copied onto itself.
    let inside = temp_dir("collect-self");
    fs::create_dir_all(inside.join("assets")).unwrap();
    let path = write_png(&inside.join("assets"), "own.png", 6, 6, |_, _| grey(9));
    ok(&mut session, "depth.draw.load_png", json!({"path": path}));
    fails(&mut session, "export.collect", json!({"dir": inside.to_str().unwrap(), "overwrite": true}), "its own input");
    fs::remove_file(&path).unwrap();
    fails(&mut session, "export.collect", json!({"dir": out.to_str().unwrap()}), "was not found");
    // A folder in the source's place is not an image either.
    fs::create_dir(&path).unwrap();
    fails(&mut session, "export.collect", json!({"dir": out.to_str().unwrap()}), "was not found");
    assert_eq!(fs::read_dir(&out).unwrap().count(), 0, "nothing was written");
}

// ---------------------------------------------------------------------------------------------- mcp

#[test]
fn every_depthdraw_tool_is_listed_and_round_trips_over_mcp() {
    let dir = temp_dir("mcp");
    let out = temp_dir("mcp-out");
    let mut session = draw_session();
    let listed = session.handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    let tools = listed["result"]["tools"].as_array().unwrap();
    let required = |name: &str| -> Value {
        tools.iter().find(|tool| tool["name"] == name).unwrap_or_else(|| panic!("{name} is listed"))["inputSchema"]["required"].clone()
    };
    for (name, expected) in [
        ("depth_draw_manifest_get", json!([])),
        ("depth_draw_manifest_set", json!([])),
        ("depth_draw_load_png", json!(["path"])),
        ("depth_draw_layer_update", json!(["layer"])),
        ("depth_draw_binding_update", json!(["layer", "target"])),
        ("depth_draw_auto_bind", json!([])),
        ("depth_draw_compose", json!([])),
        ("depth_draw_apply", json!([])),
        ("depth_draw_cleanup", json!(["layer", "kind"])),
        ("depth_draw_fit_z", json!(["layer"])),
        ("depth_draw_export_png", json!(["dir"])),
        ("depth_draw_diagnostics", json!([])),
        ("depth_draw_display", json!([])),
        ("depth_draw_select", json!([])),
    ] {
        assert_eq!(required(name), expected, "{name}");
    }

    let loaded_png = mcp_ok(&mut session, "depth_draw_load_png", json!({"path": mid_png(&dir)}));
    assert_eq!(loaded_png["layer"]["id"], "mid");
    let updated = mcp_ok(&mut session, "depth_draw_layer_update", json!({"layer": "mid", "convolution": "Nearest"}));
    assert_eq!(updated["layer"]["convolution"], "Nearest");
    let bound = mcp_ok(&mut session, "depth_draw_binding_update", json!({"layer": "mid", "target": 5}));
    assert_eq!(bound["created"], true);
    let bound = mcp_ok(&mut session, "depth_draw_auto_bind", json!({"matchDirectGridName": false}));
    assert_eq!(bound["matched"], 1);
    mcp_ok(&mut session, "depth_draw_select", json!({"target": 5, "layer": "mid"}));
    let composed = mcp_ok(&mut session, "depth_draw_compose", json!({}));
    assert_eq!(floats(&composed["depths"]), [1.0, -1.0, -1.0, 1.0, -1.0, -1.0, 0.5, 0.5, 0.5]);
    let diagnostics = mcp_ok(&mut session, "depth_draw_diagnostics", json!({}));
    assert_eq!(diagnostics["rows"][0]["sampledVertices"], 6);
    let display = mcp_ok(&mut session, "depth_draw_display", json!({"showWinningLayer": true}));
    assert_eq!(display["display"]["showWinningLayer"], true);
    let fitted = mcp_ok(&mut session, "depth_draw_fit_z", json!({"layer": "mid", "gap": "values", "back": 0.0, "front": 2.0}));
    assert_eq!((fitted["zScale"].clone(), fitted["zOffset"].clone()), (json!(1.0), json!(1.0)));
    let cleaned = mcp_ok(&mut session, "depth_draw_cleanup", json!({"layer": "mid", "kind": "contour_repair"}));
    assert_eq!(cleaned["kind"], "contour_repair");
    let steps = history(&session);
    let applied = mcp_ok(&mut session, "depth_draw_apply", json!({}));
    assert_eq!(history(&session), steps + 1);
    assert_eq!(applied["apply"]["succeeded"], true);
    let exported = mcp_ok(&mut session, "depth_draw_export_png", json!({"dir": out.to_str().unwrap()}));
    assert_eq!(exported["exportedLayers"], 1);
    let manifest = mcp_ok(&mut session, "depth_draw_manifest_get", json!({}));
    let set = mcp_ok(&mut session, "depth_draw_manifest_set", json!({"manifest": manifest["manifest"]}));
    assert_eq!(set["manifest"], manifest["manifest"]);

    let failed = mcp(&mut session, "depth_draw_apply", json!({"target": 99}));
    assert_eq!(failed["isError"], true);
    let failed = mcp(&mut session, "depth_draw_select", json!({}));
    assert_eq!(failed["isError"], true);
}
