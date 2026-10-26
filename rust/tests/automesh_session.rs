mod common;

use serde_json::{json, Value};
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const SIZE: u32 = 64;

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "soul2d-automesh-session-{}-{}",
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

fn part(uuid: u64, texture: u64, extra: Value) -> Value {
    let mut node = json!({
        "uuid": uuid, "name": format!("Part{uuid}"), "type": "Part", "enabled": true, "zsort": 0.0,
        "transform": transform(), "children": [], "lockToRoot": false, "pinToMesh": false,
        "mesh": {"verts": [-8.0, -8.0, 8.0, -8.0, 8.0, 8.0, -8.0, 8.0],
                 "uvs": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
                 "indices": [0, 1, 2, 0, 2, 3], "origin": [3.0, 4.0]},
        "textures": [texture], "blend_mode": "Normal", "tint": [1, 1, 1], "screenTint": [0, 0, 0],
        "emissionStrength": 1, "opacity": 1,
    });
    for (key, value) in extra.as_object().unwrap() {
        node[key] = value.clone();
    }
    node
}

/// A 64x64 texture that is opaque where `inside` says so.
fn png(inside: impl Fn(u32, u32) -> bool) -> Vec<u8> {
    let image = image::RgbaImage::from_fn(SIZE, SIZE, |x, y| image::Rgba([200, 100, 50, if inside(x, y) { 255 } else { 0 }]));
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    png.into_inner()
}

/// An L shape: a bar at x 12..=27, y 8..=55 and a foot at x 12..=55, y 40..=55.
fn rectangle_png() -> Vec<u8> {
    png(|x, y| ((12..=27).contains(&x) && (8..=55).contains(&y)) || ((12..=55).contains(&x) && (40..=55).contains(&y)))
}

fn open(children: Vec<Value>) -> Session {
    open_with(children, rectangle_png())
}

fn open_with(children: Vec<Value>, texture: Vec<u8>) -> Session {
    let model = json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
            "transform": transform(), "lockToRoot": false, "pinToMesh": false,
            "children": children},
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

fn session() -> Session {
    open(vec![part(5, 0, json!({})), json!({
        "uuid": 6, "name": "Plain", "type": "Node", "enabled": true, "zsort": 0.0,
        "transform": transform(), "children": [], "lockToRoot": false, "pinToMesh": false,
    })])
}

fn generate(session: &mut Session, extra: Value) -> Value {
    let mut request = json!({"op": "automesh.generate", "id": 5});
    for (key, value) in extra.as_object().unwrap() {
        request[key] = value.clone();
    }
    session.handle_request(&request)
}

fn mesh(session: &Session) -> Value {
    fn find(node: &Value) -> Option<Value> {
        if node["uuid"] == 5 {
            return Some(node["mesh"].clone());
        }
        node["children"].as_array()?.iter().find_map(find)
    }
    find(&session.snapshot()["model"]["nodes"]).unwrap()
}

fn floats(value: &Value) -> Vec<f64> {
    value.as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect()
}

/// Checks the invariants every generated Part mesh must keep and returns (vertices, triangles).
fn check_mesh(mesh: &Value) -> (usize, usize) {
    let verts = floats(&mesh["verts"]);
    let uvs = floats(&mesh["uvs"]);
    let indices: Vec<u64> = mesh["indices"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    assert_eq!(verts.len(), uvs.len());
    assert_eq!(indices.len() % 3, 0);
    assert!(indices.iter().all(|&i| (i as usize) < verts.len() / 2));
    for (vert, uv) in verts.chunks(2).zip(uvs.chunks(2)) {
        for axis in 0..2 {
            // Part.normalizeUV: uv = position / texture size + 0.5.
            assert!((uv[axis] - (vert[axis] / f64::from(SIZE) + 0.5)).abs() < 1e-6, "{vert:?} {uv:?}");
            assert!((0.0..=1.0).contains(&uv[axis]), "uv out of range: {uv:?}");
        }
    }
    for tri in indices.chunks(3) {
        let p = |i: u64| [verts[i as usize * 2], verts[i as usize * 2 + 1]];
        let (a, b, c) = (p(tri[0]), p(tri[1]), p(tri[2]));
        let cross = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
        assert!(cross >= 0.0, "clockwise triangle {tri:?}");
    }
    assert_eq!(mesh["origin"], json!([3.0, 4.0]), "origin is kept");
    (verts.len() / 2, indices.len() / 3)
}

fn verts(session: &Session) -> Vec<f64> {
    floats(&mesh(session)["verts"])
}

#[test]
fn grid_builds_a_lattice_over_the_opaque_box() {
    let mut s = session();
    let reply = generate(&mut s, json!({"algorithm": "grid", "margin": 0.0, "x_segments": 4, "y_segments": 3}));
    assert_eq!(reply["succeeded"], true, "{reply}");
    assert_eq!(reply["result"], json!({"id": 5, "changed": true, "vertices": 20, "triangles": 24}));
    let m = mesh(&s);
    assert_eq!(check_mesh(&m), (20, 24));
    // Opaque box x 12..=55, y 8..=55 around the centre (32, 32).
    let v = verts(&s);
    assert_eq!(&v[..2], &[-20.0, -24.0]);
    assert_eq!(&v[v.len() - 2..], &[23.0, 23.0]);
    assert_eq!(m["gridAxes"][1], json!([-20.0, -9.25, 1.5, 12.25, 23.0]));
    let rows = floats(&m["gridAxes"][0]);
    for (row, expected) in rows.iter().zip([-24.0, -8.333333, 7.333334, 23.0]) {
        assert!((row - expected).abs() < 1e-4, "{rows:?}");
    }
}

#[test]
fn grid_defaults_extend_ten_percent_past_the_box() {
    let mut s = session();
    let reply = generate(&mut s, json!({"algorithm": "grid"}));
    assert_eq!(reply["result"], json!({"id": 5, "changed": true, "vertices": 25, "triangles": 32}));
    check_mesh(&mesh(&s));
    // First column sits 10% of the 43 px box width left of x = 12, relative to the centre.
    assert!((verts(&s)[0] - (-24.3)).abs() < 1e-4);
}

#[test]
fn option_fields_are_clamped_to_the_d_ranges() {
    let mut s = session();
    // x_segments 99 -> 20, y_segments -5 -> 2, margin 9 -> 1.
    let reply = generate(&mut s, json!({"algorithm": "grid", "x_segments": 99, "y_segments": -5, "margin": 9}));
    assert_eq!(reply["succeeded"], true, "{reply}");
    // (20 + 1 + 2 margin columns) by (2 + 1 + 2 margin rows).
    assert_eq!(reply["result"]["vertices"], 23 * 5);
    assert_eq!(reply["result"]["triangles"], 22 * 4 * 2);
}

#[test]
fn contour_optimum_and_skeleton_replace_the_mesh() {
    let cases = [
        (json!({"algorithm": "contour"}), 6, 6),
        (json!({"algorithm": "optimum"}), 26, 40),
        (json!({"algorithm": "optimum", "preset": "Small parts"}), 40, 62),
        (json!({"algorithm": "skeleton"}), 10, 10),
        (json!({"algorithm": "skeleton", "target_point_count": 4}), 4, 2),
        // 1 clamps up to the minimum of 2 points, which cannot form a Drawable.
        (json!({"algorithm": "skeleton", "target_point_count": 1}), 2, 0),
    ];
    for (request, vertices, triangles) in cases {
        let mut s = session();
        let reply = generate(&mut s, request.clone());
        assert_eq!(reply["succeeded"], true, "{request}: {reply}");
        if triangles == 0 {
            // Two skeleton points cannot form a Drawable: nothing is generated.
            assert_eq!(reply["result"]["changed"], false, "{request}");
            continue;
        }
        assert_eq!(reply["result"], json!({"id": 5, "changed": true, "vertices": vertices, "triangles": triangles}), "{request}");
        assert_eq!(check_mesh(&mesh(&s)), (vertices, triangles), "{request}");
        assert!(mesh(&s).get("gridAxes").is_none());
    }
}

#[test]
fn skeleton_vertices_and_uvs_follow_the_part_pipeline() {
    let mut s = session();
    generate(&mut s, json!({"algorithm": "skeleton", "target_point_count": 4}));
    let m = mesh(&s);
    assert_eq!(floats(&m["verts"]), [9.0, 15.0, 1.0, 20.0, -11.0, 16.0, -17.0, -4.0]);
    // uv = position / 64 + 0.5.
    assert_eq!(floats(&m["uvs"]), [0.640625, 0.734375, 0.515625, 0.8125, 0.328125, 0.75, 0.234375, 0.4375]);
    assert_eq!(m["origin"], json!([3.0, 4.0]));
}

#[test]
fn no_mesh_generated_is_a_successful_no_op() {
    let mut s = session();
    let before = s.snapshot();
    // Sampling every 80 px leaves too few contour vertices for a Drawable.
    let reply = generate(&mut s, json!({"algorithm": "contour", "preset": "Large parts"}));
    assert_eq!(reply["succeeded"], true, "{reply}");
    assert_eq!(reply["result"], json!({"id": 5, "changed": false, "message": "no mesh generated"}));
    assert_eq!(s.snapshot(), before);
    assert_eq!(s.snapshot()["canUndo"], false);
}

#[test]
fn a_shape_without_a_usable_skeleton_answers_no_mesh_and_is_not_an_error() {
    // A disc thins to a point. The other algorithms mesh it; skeleton has nothing to follow.
    let disc = png(|x, y| (x as i32 - 32).pow(2) + (y as i32 - 32).pow(2) <= 28 * 28);
    let mut s = open_with(vec![part(5, 0, json!({}))], disc);
    let before = s.snapshot();
    let call = |s: &mut Session, algorithm: &str| {
        let arguments = json!({"id": 5, "algorithm": algorithm});
        let params = json!({"name": "automesh_generate", "arguments": arguments});
        s.handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": params})).unwrap()["result"].clone()
    };
    let skeleton = call(&mut s, "skeleton");
    assert_eq!(skeleton["isError"], false, "{skeleton}");
    assert_eq!(skeleton["structuredContent"], json!({"id": 5, "changed": false, "message": "no mesh generated"}));
    assert_eq!(s.snapshot(), before);
    let grid = call(&mut s, "grid");
    assert_eq!((&grid["isError"], &grid["structuredContent"]["changed"]), (&json!(false), &json!(true)), "{grid}");
}

#[test]
fn one_undo_restores_the_previous_mesh_exactly() {
    let mut s = session();
    let before = s.snapshot();
    let original = mesh(&s);
    generate(&mut s, json!({"algorithm": "optimum"}));
    assert_ne!(mesh(&s), original);
    let generated = s.snapshot();
    assert_eq!(generated["canUndo"], true);
    let undone = s.handle_request(&json!({"op": "undo"}));
    assert_eq!(undone["succeeded"], true, "{undone}");
    assert_eq!(mesh(&s), original);
    assert_eq!(s.snapshot()["model"], before["model"]);
    assert_eq!(s.snapshot()["canUndo"], false, "automesh is a single history step");
    s.handle_request(&json!({"op": "redo"}));
    assert_eq!(s.snapshot()["model"], generated["model"]);
}

#[test]
fn bad_requests_leave_the_document_unchanged() {
    let mut s = open(vec![
        part(5, 0, json!({})),
        part(7, u32::MAX as u64, json!({})),
        json!({"uuid": 6, "name": "Plain", "type": "Node", "enabled": true, "zsort": 0.0,
               "transform": transform(), "children": [], "lockToRoot": false, "pinToMesh": false}),
    ]);
    let before = s.snapshot();
    let cases = [
        (json!({"id": 6, "algorithm": "grid"}), "is not a Part"),
        (json!({"id": 99, "algorithm": "grid"}), "was not found"),
        (json!({"id": 7, "algorithm": "grid"}), "has no texture"),
        (json!({"id": 5, "algorithm": "voronoi"}), "unknown algorithm"),
        (json!({"id": 5}), "algorithm must be one of"),
        (json!({"id": 5, "algorithm": "grid", "preset": "Normal parts"}), "has no presets"),
        (json!({"id": 5, "algorithm": "contour", "preset": "Nope"}), "unknown contour preset"),
        (json!({"id": 5, "algorithm": "contour", "sampling_step": "wide"}), "sampling_step must be a finite number"),
        (json!({"id": 5, "algorithm": "optimum", "scales": [1.0, "x"]}), "scales must be an array of finite numbers"),
        (json!({"id": 5, "algorithm": "grid", "scale_x": [0.5]}), "scale_x"),
        (json!({"algorithm": "grid"}), "id must be"),
        (json!({"id": 0, "algorithm": "grid"}), "id must be"),
    ];
    for (extra, expected) in cases {
        let mut request = json!({"op": "automesh.generate"});
        for (key, value) in extra.as_object().unwrap() {
            request[key] = value.clone();
        }
        let reply = s.handle_request(&request);
        assert_eq!(reply["succeeded"], false, "{extra}: {reply}");
        assert!(reply["message"].as_str().unwrap().contains(expected), "{extra}: {reply}");
        assert_eq!(s.snapshot(), before, "{extra} must not change the document");
    }
}

#[test]
fn a_part_with_depth_data_is_rejected() {
    let mut s = open(vec![part(5, 0, json!({"depths": [0.0, 0.0, 0.0, 0.0]}))]);
    let before = s.snapshot();
    let reply = generate(&mut s, json!({"algorithm": "grid"}));
    assert_eq!(reply["succeeded"], false, "{reply}");
    assert!(reply["message"].as_str().unwrap().contains("depth"), "{reply}");
    assert_eq!(s.snapshot(), before);
}

#[test]
fn a_part_with_skinning_weights_is_rejected() {
    let mut s = session();
    let id = |reply: Value| reply["result"]["created"][0]["uuid"].as_u64().unwrap();
    let rig = id(s.handle_request(&json!({"op": "node.create", "className": "DepthRigRoot", "name": "Rig"})));
    let bone = id(s.handle_request(&json!({
        "op": "node.create", "className": "DepthBone", "name": "Bone", "parents": [rig],
        "restHead": [0.0, 0.0, 0.0], "restTail": [0.0, 10.0, 0.0]
    })));
    for bind in [
        json!({"op": "rig.add_source", "root": rig, "target": 5, "bone": bone}),
        json!({"op": "rig.set_vertex_weights", "root": rig, "target": 5,
            "weights": [{"bone": bone, "values": [1.0, 1.0, 1.0, 1.0]}]}),
    ] {
        let reply = s.handle_request(&bind);
        assert_eq!(reply["succeeded"], true, "{reply}");
    }
    let before = s.snapshot();
    // The new mesh has other vertices, however many: the rows of the old ones cannot follow.
    let reply = generate(&mut s, json!({"algorithm": "grid"}));
    assert_eq!(reply["message"], "Part 5 has skinning weights that automesh cannot remap", "{reply}");
    assert_eq!(s.snapshot(), before);
}

#[test]
fn legacy_top_level_mesh_arrays_follow_the_new_mesh() {
    let legacy = json!({
        "vertices": [[-8.0, -8.0], [8.0, -8.0], [8.0, 8.0], [-8.0, 8.0]],
        "uvs": [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        "indices": [0, 1, 2, 0, 2, 3],
    });
    let mut s = open(vec![part(5, 0, legacy)]);
    let reply = generate(&mut s, json!({"algorithm": "skeleton", "target_point_count": 4}));
    assert_eq!(reply["succeeded"], true, "{reply}");
    let model = s.snapshot()["model"].clone();
    let node = &model["nodes"]["children"][0];
    assert_eq!(node["vertices"], json!([[9.0, 15.0], [1.0, 20.0], [-11.0, 16.0], [-17.0, -4.0]]));
    assert_eq!(node["uvs"].as_array().unwrap().len(), 4);
    assert_eq!(node["indices"], node["mesh"]["indices"]);
}

#[test]
fn mcp_tool_round_trip() {
    let mut s = session();
    let listed = s
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .unwrap();
    let tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "automesh_generate")
        .expect("automesh_generate is listed");
    assert_eq!(tool["inputSchema"]["required"], json!(["id", "algorithm"]));
    assert_eq!(tool["inputSchema"]["properties"]["algorithm"]["enum"], json!(["grid", "contour", "optimum", "skeleton"]));

    let called = s
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "automesh_generate",
            "arguments": {"id": 5, "algorithm": "skeleton", "target_point_count": 4},
        }}))
        .unwrap();
    assert_eq!(called["result"]["isError"], false, "{called}");
    assert_eq!(
        called["result"]["structuredContent"],
        json!({"id": 5, "changed": true, "vertices": 4, "triangles": 2})
    );
    assert_eq!(check_mesh(&mesh(&s)), (4, 2));

    let failed = s
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "automesh_generate",
            "arguments": {"id": 6, "algorithm": "grid"},
        }}))
        .unwrap();
    assert_eq!(failed["result"]["isError"], true);
}

const PRESET_FILE: &str = "automesh-presets.json";

/// A session with the fixture Part and a settings directory of its own, which does not exist yet.
fn preset_session() -> (Session, PathBuf) {
    let dir = temp_dir().join("settings");
    let mut s = session();
    s.set_config_dir(dir.clone());
    (s, dir)
}

fn preset(session: &mut Session, op: &str, extra: Value) -> Value {
    let mut request = json!({"op": format!("automesh.preset.{op}")});
    for (key, value) in extra.as_object().unwrap() {
        request[key] = value.clone();
    }
    session.handle_request(&request)
}

fn presets(session: &mut Session) -> Value {
    let reply = preset(session, "list", json!({}));
    assert_eq!(reply["succeeded"], true, "{reply}");
    reply["result"]["presets"].clone()
}

#[test]
fn a_saved_preset_is_listed_meshes_like_its_fields_and_is_deleted() {
    let (mut s, dir) = preset_session();
    assert_eq!(presets(&mut s), json!({}));
    assert!(!dir.exists(), "a list creates nothing");
    let before = s.snapshot();

    // Only the algorithm, the built-in preset and the option fields of that algorithm are kept.
    let fine = json!({"algorithm": "contour", "preset": "Thin and minimum parts", "sampling_step": 20});
    let saved = preset(&mut s, "save", json!({
        "name": "Fine", "algorithm": "contour", "preset": "Thin and minimum parts", "sampling_step": 20,
        "id": 5, "div_per_part": 9, "max_distance": null,
    }));
    assert_eq!(saved["result"], json!({"name": "Fine", "preset": fine, "replaced": false}), "{saved}");
    assert_eq!(s.snapshot(), before, "a preset is no change of the document");
    assert_eq!(presets(&mut s), json!({"Fine": fine}));
    let stored: Value = serde_json::from_slice(&fs::read(dir.join(PRESET_FILE)).unwrap()).unwrap();
    assert_eq!(stored, json!({"presets": {"Fine": fine}}));
    let read = s
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "resources/read", "params": {
            "uri": "resource://soul2d/automesh/presets",
        }}))
        .unwrap();
    let text = read["result"]["contents"][0]["text"].as_str().unwrap_or_else(|| panic!("{read}"));
    assert_eq!(serde_json::from_str::<Value>(text).unwrap(), json!({"presets": {"Fine": fine}}));

    // The preset meshes like its fields. The built-in preset alone gives 6 vertices and 5 triangles, and a
    // sampling step of 20 without it gives 5 and 5.
    let reply = generate(&mut s, json!({"algorithm": "contour", "preset": "Fine"}));
    assert_eq!(reply["result"], json!({"id": 5, "changed": true, "vertices": 4, "triangles": 3}), "{reply}");
    let mut explicit = session();
    generate(&mut explicit, fine.clone());
    assert_eq!(mesh(&s), mesh(&explicit));
    for (request, vertices, triangles) in [
        (json!({"algorithm": "contour", "preset": "Thin and minimum parts"}), 6, 5),
        (json!({"algorithm": "contour", "sampling_step": 20}), 5, 5),
        // A field of the call replaces the field of the preset: 12 is the step of the built-in preset.
        (json!({"algorithm": "contour", "preset": "Fine", "sampling_step": 12}), 6, 5),
        // A field that is null is a field that is not there.
        (json!({"algorithm": "contour", "preset": "Fine", "sampling_step": null}), 4, 3),
    ] {
        let mut other = session();
        other.set_config_dir(dir.clone());
        let reply = generate(&mut other, request.clone());
        assert_eq!(reply["result"], json!({"id": 5, "changed": true, "vertices": vertices, "triangles": triangles}), "{request}");
    }
    let mut s2 = session();
    s2.set_config_dir(dir.clone());
    // The preset belongs to the algorithm it was saved for.
    let reply = generate(&mut s2, json!({"algorithm": "optimum", "preset": "Fine"}));
    assert_eq!(reply["message"], "unknown optimum preset \"Fine\"", "{reply}");

    // Another host session reads it, and it outlives the document.
    assert_eq!(presets(&mut s2), json!({"Fine": fine}));
    assert_eq!(s.handle_request(&json!({"op": "new"}))["succeeded"], true);
    assert_eq!(presets(&mut s), json!({"Fine": fine}));

    // Saving the name again replaces the preset, with overwrite only.
    let coarse = json!({"algorithm": "optimum", "scales": [1.0, 0.5]});
    let again = json!({"name": "Fine", "algorithm": "optimum", "scales": [1.0, 0.5]});
    let refused = preset(&mut s, "save", again.clone());
    assert_eq!(refused["message"], "a preset named \"Fine\" is saved; pass overwrite to replace it", "{refused}");
    assert_eq!(presets(&mut s2), json!({"Fine": fine}));
    let mut again = again;
    again["overwrite"] = json!(true);
    let saved = preset(&mut s, "save", again);
    assert_eq!(saved["result"], json!({"name": "Fine", "preset": coarse, "replaced": true}), "{saved}");
    assert_eq!(presets(&mut s2), json!({"Fine": coarse}));
    let reply = generate(&mut s2, json!({"algorithm": "contour", "preset": "Fine"}));
    assert_eq!(reply["message"], "unknown contour preset \"Fine\"", "{reply}");
    // The optimum default gives 26 vertices and 40 triangles.
    let reply = generate(&mut s2, json!({"algorithm": "optimum", "preset": "Fine"}));
    assert_eq!(reply["result"], json!({"id": 5, "changed": true, "vertices": 32, "triangles": 50}), "{reply}");

    let deleted = preset(&mut s, "delete", json!({"name": "Fine"}));
    assert_eq!(deleted["result"], json!({"name": "Fine"}), "{deleted}");
    assert_eq!(presets(&mut s2), json!({}));
    let reply = generate(&mut s2, json!({"algorithm": "optimum", "preset": "Fine"}));
    assert_eq!(reply["message"], "unknown optimum preset \"Fine\"", "{reply}");
    let again = preset(&mut s, "delete", json!({"name": "Fine"}));
    assert_eq!(again["message"], "no saved preset is named \"Fine\"", "{again}");
}

#[test]
fn a_preset_that_cannot_be_saved_is_refused_and_the_file_stays() {
    let (mut s, dir) = preset_session();
    let base = json!({"algorithm": "contour", "sampling_step": 30});
    assert_eq!(preset(&mut s, "save", json!({"name": "Base", "algorithm": "contour", "sampling_step": 30}))["succeeded"], true);
    let file = fs::read(dir.join(PRESET_FILE)).unwrap();
    let long = "x".repeat(65);
    let cases = [
        (json!({"name": "Small parts", "algorithm": "contour"}), "\"Small parts\" is a built-in preset"),
        (json!({"name": "A", "algorithm": "grid"}), "a preset needs the algorithm contour or optimum"),
        (json!({"name": "A", "algorithm": "skeleton"}), "a preset needs the algorithm contour or optimum"),
        (json!({"name": "A"}), "a preset needs the algorithm contour or optimum"),
        (json!({"algorithm": "contour"}), "name must be a string"),
        (json!({"name": 3, "algorithm": "contour"}), "name must be a string"),
        (json!({"name": "", "algorithm": "contour"}), "name must be 1 to 64 characters"),
        (json!({"name": long, "algorithm": "contour"}), "name must be 1 to 64 characters"),
        (json!({"name": "a\nb", "algorithm": "contour"}), "name must be 1 to 64 characters"),
        (json!({"name": " a", "algorithm": "contour"}), "name must be 1 to 64 characters"),
        (json!({"name": "A", "algorithm": "contour", "sampling_step": "wide"}), "sampling_step must be a finite number"),
        (json!({"name": "A", "algorithm": "contour", "scales": [1.0, "x"]}), "scales must be an array of finite numbers"),
        (json!({"name": "A", "algorithm": "contour", "preset": "Nope"}), "unknown contour preset \"Nope\""),
        // A preset starts from a built-in one, not from another saved one.
        (json!({"name": "A", "algorithm": "contour", "preset": "Base"}), "unknown contour preset \"Base\""),
        (json!({"name": "A", "algorithm": "contour", "preset": 3}), "preset must be a string"),
        (json!({"name": "Base", "algorithm": "contour"}), "a preset named \"Base\" is saved; pass overwrite"),
        (json!({"name": "Base", "algorithm": "contour", "overwrite": false}), "a preset named \"Base\" is saved"),
        (json!({"name": "Base", "algorithm": "contour", "overwrite": "yes"}), "overwrite must be a boolean"),
        (json!({"name": "\u{3000}a", "algorithm": "contour"}), "name must be 1 to 64 characters"),
    ];
    for (request, expected) in cases {
        let reply = preset(&mut s, "save", request.clone());
        assert_eq!(reply["succeeded"], false, "{request}: {reply}");
        assert!(reply["message"].as_str().unwrap().starts_with(expected), "{request}: {reply}");
        assert_eq!(fs::read(dir.join(PRESET_FILE)).unwrap(), file, "{request}");
    }
    // A name of 64 characters is the longest, and it counts characters, not bytes.
    let longest = "あ".repeat(64);
    assert_eq!(preset(&mut s, "save", json!({"name": longest, "algorithm": "contour"}))["succeeded"], true);
    assert_eq!(preset(&mut s, "delete", json!({"name": longest}))["succeeded"], true);
    assert_eq!(preset(&mut s, "delete", json!({}))["message"], "name must be a string");
    assert_eq!(preset(&mut s, "delete", json!({"name": "Small parts"}))["message"], "\"Small parts\" is a built-in preset");
    let reply = preset(&mut s, "delete", json!({"name": "x".repeat(65)}));
    assert!(reply["message"].as_str().unwrap().starts_with("name must be 1 to 64 characters"), "{reply}");

    // A number is kept with the digits it was written with. Presets too long for the file to be read again are
    // refused before they are written.
    let digits = format!(r#"{{"name": "Long", "algorithm": "contour", "min_distance": 1.{}}}"#, "0".repeat(1 << 20));
    let reply = preset(&mut s, "save", serde_json::from_str(&digits).unwrap());
    assert_eq!(reply["message"], "the presets would be larger than 1048576 bytes", "{reply}");
    assert_eq!(fs::read(dir.join(PRESET_FILE)).unwrap(), file);

    // 64 presets are the most. One of them can still be replaced.
    for index in 1..64 {
        let reply = preset(&mut s, "save", json!({"name": format!("P{index}"), "algorithm": "optimum"}));
        assert_eq!(reply["succeeded"], true, "{index}: {reply}");
    }
    let file = fs::read(dir.join(PRESET_FILE)).unwrap();
    let reply = preset(&mut s, "save", json!({"name": "P64", "algorithm": "optimum"}));
    assert_eq!(reply["message"], "at most 64 presets can be saved", "{reply}");
    assert_eq!(fs::read(dir.join(PRESET_FILE)).unwrap(), file);
    let reply = preset(&mut s, "save", json!({"name": "P1", "algorithm": "contour", "min_distance": 9, "overwrite": true}));
    assert_eq!(reply["result"]["replaced"], true, "{reply}");
    let listed = presets(&mut s);
    assert_eq!(listed.as_object().unwrap().len(), 64);
    assert_eq!(listed["Base"], base);
    assert_eq!(listed["P1"], json!({"algorithm": "contour", "min_distance": 9}));
}

#[test]
fn a_session_without_a_settings_directory_has_no_saved_presets() {
    let mut s = session();
    assert_eq!(presets(&mut s), json!({}));
    for (op, request) in [("save", json!({"name": "A", "algorithm": "contour"})), ("delete", json!({"name": "A"}))] {
        let reply = preset(&mut s, op, request);
        assert_eq!(reply["message"], "this host has no settings directory to keep presets in", "{op}: {reply}");
    }
    let reply = generate(&mut s, json!({"algorithm": "contour", "preset": "A"}));
    assert_eq!(reply["message"], "unknown contour preset \"A\"", "{reply}");
}

#[test]
fn a_preset_file_that_is_not_valid_is_refused_and_not_written_over() {
    let (mut s, dir) = preset_session();
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(PRESET_FILE);
    let mut large = br#"{"presets": {}}"#.to_vec();
    large.resize((1 << 20) + 1, b' ');
    let many: serde_json::Map<String, Value> = (0..65).map(|i| (format!("P{i}"), json!({"algorithm": "contour"}))).collect();
    let files: [(Vec<u8>, &str); 9] = [
        (b"{".to_vec(), "EOF while parsing"),
        (b"[]".to_vec(), "presets must be an object"),
        (br#"{"presets": []}"#.to_vec(), "presets must be an object"),
        (br#"{"presets": {"Small parts": {"algorithm": "contour"}}}"#.to_vec(), "\"Small parts\" is a built-in preset"),
        (br#"{"presets": {" a": {"algorithm": "contour"}}}"#.to_vec(), "name must be 1 to 64 characters"),
        (br#"{"presets": {"A": {"algorithm": "grid"}}}"#.to_vec(), "preset \"A\": a preset needs the algorithm contour or optimum"),
        (br#"{"presets": {"A": {"algorithm": "contour", "scales": 3}}}"#.to_vec(), "preset \"A\": scales must be an array of numbers"),
        (large, "it is larger than 1048576 bytes"),
        (serde_json::to_vec(&json!({"presets": many})).unwrap(), "it holds more than 64 presets"),
    ];
    for (bytes, reason) in files {
        fs::write(&path, &bytes).unwrap();
        let expected = format!("automesh-presets.json of the settings directory is not valid: {reason}");
        let replies = [
            preset(&mut s, "list", json!({})),
            preset(&mut s, "save", json!({"name": "B", "algorithm": "contour"})),
            preset(&mut s, "delete", json!({"name": "B"})),
            generate(&mut s, json!({"algorithm": "contour", "preset": "B"})),
        ];
        for reply in replies {
            assert_eq!(reply["succeeded"], false, "{reason}: {reply}");
            let message = reply["message"].as_str().unwrap();
            assert!(message.starts_with(&expected), "{reason}: {reply}");
            assert!(!message.contains(dir.to_str().unwrap()), "the answer names no path: {reply}");
        }
        assert!(fs::read(&path).unwrap() == bytes, "{reason}: the file is kept");
        // A built-in preset and a call without one do not read the file.
        let mut other = session();
        other.set_config_dir(dir.clone());
        let reply = generate(&mut other, json!({"algorithm": "optimum", "preset": "Small parts"}));
        assert_eq!(reply["result"]["vertices"], 40, "{reason}: {reply}");
        let reply = generate(&mut other, json!({"algorithm": "optimum"}));
        assert_eq!(reply["result"]["vertices"], 26, "{reason}: {reply}");
        let reply = generate(&mut other, json!({"algorithm": "grid", "preset": "B"}));
        assert_eq!(reply["message"], "the grid algorithm has no presets", "{reason}: {reply}");
    }

    // A file of exactly 1 MiB is read.
    let mut largest = br#"{"presets": {"Edge": {"algorithm": "optimum"}}}"#.to_vec();
    largest.resize(1 << 20, b' ');
    fs::write(&path, &largest).unwrap();
    assert_eq!(presets(&mut s), json!({"Edge": {"algorithm": "optimum"}}));

    // A file written by hand is read; what is no option field of the algorithm is left out.
    fs::write(&path, br#"{"version": 1, "presets": {"Hand": {"algorithm": "contour", "sampling_step": 30, "div_per_part": 9, "note": "x"}}}"#).unwrap();
    let hand = json!({"algorithm": "contour", "sampling_step": 30});
    assert_eq!(presets(&mut s), json!({"Hand": hand}));
    // The next save writes the file without what was not read.
    assert_eq!(preset(&mut s, "save", json!({"name": "More", "algorithm": "optimum"}))["succeeded"], true);
    let stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored, json!({"presets": {"Hand": hand, "More": {"algorithm": "optimum"}}}));
}
