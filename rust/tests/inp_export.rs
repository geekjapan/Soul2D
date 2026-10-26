use image::{Rgba, RgbaImage};
use serde_json::{json, Value};
use soul2d_core::inp_export::{
    export_inp, export_inp_report, InpExportOptions, Watermark, WatermarkBlend,
};
use soul2d_core::model_import::Texture;
use soul2d_core::pixels::{PixelFormat, PixelImage};
use soul2d_core::Session;
use std::fs;
use std::io::Cursor;

fn png(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Texture {
    let mut image = RgbaImage::new(width, height);
    for (x, y, out) in image.enumerate_pixels_mut() {
        *out = Rgba(pixel(x, y));
    }
    let mut data = Cursor::new(Vec::new());
    image.write_to(&mut data, image::ImageFormat::Png).unwrap();
    Texture { tag: 0, data: data.into_inner() }
}

fn part(id: u32, name: &str, enabled: bool, slot: u32, half: [f64; 2], uvs: [f64; 8]) -> Value {
    let [hw, hh] = half;
    json!({
        "uuid": id, "name": name, "type": "Part", "enabled": enabled, "zsort": 0.0,
        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
        "lockToRoot": false, "pinToMesh": false, "children": [],
        "mesh": {
            "verts": [-hw, -hh, -hw, hh, hw, -hh, hw, hh],
            "uvs": uvs,
            "indices": [0, 1, 2, 2, 1, 3],
            "origin": [0.0, 0.0],
        },
        "textures": [slot, u32::MAX, u32::MAX],
        "blend_mode": "Normal", "tint": [1.0, 1.0, 1.0], "screenTint": [0.0, 0.0, 0.0],
        "emissionStrength": 1.0, "mask_threshold": 0.5, "opacity": 1.0,
    })
}

fn node(id: u32, name: &str, kind: &str, children: Value) -> Value {
    json!({
        "uuid": id, "name": name, "type": kind, "enabled": true, "zsort": 0.0,
        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
        "lockToRoot": false, "pinToMesh": false, "children": children,
    })
}

fn parameter(id: u32, name: &str, bindings: Value) -> Value {
    json!({
        "uuid": id, "name": name, "is_vec2": false,
        "min": [0.0, 0.0], "max": [1.0, 0.0], "defaults": [0.0, 0.0],
        "axis_points": [[0.0, 1.0], [0.0]], "merge_mode": "Additive", "bindings": bindings,
    })
}

fn binding(node: u32) -> Value {
    json!({
        "node": node, "param_name": "transform.t.x", "interpolate_mode": "Linear",
        "values": [0.0, 5.0], "isSet": [true, true],
    })
}

const FULL: [f64; 8] = [0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0];
const LEFT_HALF: [f64; 8] = [0.0, 0.0, 0.0, 1.0, 0.5, 0.0, 0.5, 1.0];

/// Three textured Parts: A (4x4, full UVs), B (2x2, disabled), C (8x4, UVs cover its left half), a Camera,
/// a depth rig root holding a Part-free Node, and a parameter group whose children bind A and B.
fn fixture() -> (Value, Vec<Texture>) {
    let a = png(4, 4, |x, y| [(x * 40 + 10) as u8, (y * 40 + 10) as u8, 100, 255]);
    let b = png(2, 2, |_, _| [200, 0, 0, 255]);
    let c = png(8, 4, |x, y| [(x * 30) as u8, (y * 60) as u8, 7, 255]);
    let model = json!({
        "meta": {"name": "export", "version": "1.0-alpha", "thumbnailId": 3},
        "physics": {},
        "nodes": node(1, "Root", "Node", json!([
            part(2, "A", true, 0, [2.0, 2.0], FULL),
            part(3, "B", false, 1, [1.0, 1.0], FULL),
            part(4, "C", true, 2, [4.0, 2.0], LEFT_HALF),
            node(5, "Cam", "Camera", json!([node(6, "UnderCam", "Node", json!([]))])),
            node(7, "Rig", "DepthRigRoot", json!([node(8, "Kept", "Node", json!([]))])),
        ])),
        "param": [
            {"groupUUID": 40, "name": "Group", "children": [
                parameter(41, "Move", json!([binding(2), binding(3), binding(8), binding(7)])),
            ]},
            parameter(42, "Plain", json!([])),
        ],
        "automation": [],
        "animations": {"Idle": {
            "length": 4, "timestep": 0.1, "additive": false, "animationWeight": 1.0,
            "leadIn": -1, "leadOut": -1,
            "lanes": [{"uuid": 41, "target": 0}, {"uuid": 999, "target": 0}, {"uuid": 40, "target": 0}],
        }},
    });
    let thumbnail = png(1, 1, |_, _| [9, 9, 9, 255]);
    (model, vec![a, b, c, thumbnail])
}

fn small() -> InpExportOptions {
    InpExportOptions { atlas_resolution: 16, padding: 1, ..InpExportOptions::default() }
}

struct Parsed {
    model: Value,
    textures: Vec<(u8, Vec<u8>)>,
    tail: Vec<u8>,
}

fn be32(bytes: &[u8], at: usize) -> usize {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize
}

fn parse(bytes: &[u8]) -> Parsed {
    assert_eq!(&bytes[..8], b"TRNSRTS\0");
    let json_len = be32(bytes, 8);
    let model = serde_json::from_slice(&bytes[12..12 + json_len]).unwrap();
    let mut at = 12 + json_len;
    assert_eq!(&bytes[at..at + 8], b"TEX_SECT");
    let count = be32(bytes, at + 8);
    at += 12;
    let mut textures = Vec::new();
    for _ in 0..count {
        let length = be32(bytes, at);
        textures.push((bytes[at + 4], bytes[at + 5..at + 5 + length].to_vec()));
        at += 5 + length;
    }
    Parsed { model, textures, tail: bytes[at..].to_vec() }
}

fn decode(texture: &(u8, Vec<u8>)) -> RgbaImage {
    let format = if texture.0 == 0 { image::ImageFormat::Png } else { image::ImageFormat::Tga };
    image::load_from_memory_with_format(&texture.1, format).unwrap().into_rgba8()
}

fn find<'a>(node: &'a Value, id: u64) -> Option<&'a Value> {
    if node["uuid"] == id {
        return Some(node);
    }
    node["children"].as_array().into_iter().flatten().find_map(|child| find(child, id))
}

fn flat(value: &Value) -> Vec<f64> {
    value.as_array().unwrap().iter().map(|number| number.as_f64().unwrap()).collect()
}

fn px(image: &RgbaImage, x: u32, y: u32) -> [u8; 4] {
    image.get_pixel(x, y).0
}

#[test]
fn exports_prune_pack_flatten_and_rewrites_uvs_and_slots() {
    let (model, textures) = fixture();
    let export = export_inp_report(&model, &textures, &[], &small()).unwrap();
    assert_eq!(export.output_scale, 1.0);
    // D packs the original puppet, so the disabled B takes atlas space too: 6x6, 6x6 and 4x4 in 16x16.
    assert_eq!(export.atlas_count, 1);
    // Only B is pruned as disabled. The camera subtree leaves in flatten and the rig root is stripped first.
    assert_eq!(export.pruned_nodes, 1);
    assert!(export.skipped_parts.is_empty() && export.unbaked_deformers.is_empty());
    let parsed = parse(&export.bytes);
    assert!(parsed.tail.is_empty());

    // The albedo atlas is slot 0 (TGA like D); the thumbnail stays behind it and its id follows.
    assert_eq!(parsed.textures.len(), 2);
    assert_eq!(parsed.textures[0].0, 1);
    let albedo = decode(&parsed.textures[0]);
    assert_eq!(albedo.dimensions(), (16, 16));
    assert_eq!(parsed.textures[1].0, 0);
    assert_eq!(px(&decode(&parsed.textures[1]), 0, 0), [9, 9, 9, 255]);
    assert_eq!(parsed.model["meta"]["thumbnailId"], 1);

    let root = &parsed.model["nodes"];
    let names: Vec<&str> = root["children"].as_array().unwrap().iter().map(|n| n["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["A", "C", "Kept"]);
    assert!(find(root, 3).is_none() && find(root, 5).is_none() && find(root, 6).is_none());
    assert!(find(root, 7).is_none());

    // C (the largest texture) packs first at (0,0), then A below it, then B: mapping rectangles exclude the
    // 1 pixel padding. UVs are normalised to the Part's own UV box, then placed in the mapping.
    let a = find(root, 2).unwrap();
    let c = find(root, 4).unwrap();
    assert_eq!(a["textures"], json!([0, u32::MAX, u32::MAX]));
    assert_eq!(c["textures"], json!([0, u32::MAX, u32::MAX]));
    assert_eq!(flat(&a["mesh"]["uvs"]), [0.0625, 0.4375, 0.0625, 0.6875, 0.3125, 0.4375, 0.3125, 0.6875]);
    assert_eq!(flat(&c["mesh"]["uvs"]), [0.0625, 0.0625, 0.0625, 0.3125, 0.3125, 0.0625, 0.3125, 0.3125]);
    // Vertices and the rest of the Part are untouched.
    assert_eq!(flat(&c["mesh"]["verts"]), [-4.0, -2.0, -4.0, 2.0, 4.0, -2.0, 4.0, 2.0]);

    // C's left half (columns 0..3 of 8) lands in atlas pixels (1..4, 1..4); A's 4x4 in (1..4, 7..10).
    for i in 0..4u32 {
        assert_eq!(px(&albedo, 1 + i, 1 + i), [(i * 30) as u8, (i * 60) as u8, 7, 255]);
        assert_eq!(px(&albedo, 1 + i, 7 + i), [(i * 40 + 10) as u8, (i * 40 + 10) as u8, 100, 255]);
    }
    assert_eq!(px(&albedo, 4, 1), [90, 0, 7, 255]);
    // Padding and free space stay transparent; B (pruned from the tree) still has its pixels at (1..2, 13..14).
    assert_eq!(px(&albedo, 0, 0), [0, 0, 0, 0]);
    assert_eq!(px(&albedo, 5, 1), [0, 0, 0, 0]);
    assert_eq!(px(&albedo, 15, 15), [0, 0, 0, 0]);
    assert_eq!(px(&albedo, 2, 14), [200, 0, 0, 255]);

    // Parameters: the group dissolves, bindings to removed nodes go, orphan and group-targeting lanes go.
    let params = parsed.model["param"].as_array().unwrap();
    assert_eq!(params.iter().map(|p| p["uuid"].as_u64().unwrap()).collect::<Vec<_>>(), [41, 42]);
    let targets: Vec<u64> = params[0]["bindings"].as_array().unwrap().iter().map(|b| b["node"].as_u64().unwrap()).collect();
    assert_eq!(targets, [2, 8]);
    assert_eq!(parsed.model["animations"]["Idle"]["lanes"], json!([{"uuid": 41, "target": 0}]));
}

#[test]
fn the_file_opens_in_a_session_and_matches_export_inp() {
    let (model, textures) = fixture();
    let bytes = export_inp(&model, &textures, &small()).unwrap();
    assert_eq!(bytes, export_inp_report(&model, &textures, &[], &small()).unwrap().bytes);
    let path = format!("{}/exported-{}.inp", env!("CARGO_TARGET_TMPDIR"), std::process::id());
    fs::write(&path, &bytes).unwrap();
    let mut session = Session::new();
    let opened = session.handle_request(&json!({"op": "open", "path": path}));
    assert_eq!(opened["succeeded"], true, "{opened}");
    let snapshot = session.handle_request(&json!({"op": "snapshot"}));
    let names: Vec<&str> = snapshot["result"]["model"]["nodes"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["A", "C", "Kept"]);
    fs::remove_file(path).unwrap();
}

#[test]
fn a_clip_is_exported_as_it_is_with_its_markers() {
    let (mut model, textures) = fixture();
    let clip = json!({"length": 60, "timestep": 0.016, "additive": false, "animationWeight": 1.0, "leadIn": -1,
        "leadOut": -1, "lanes": [], "markers": [{"frame": 7, "text": "Blink"}]});
    model["animations"] = json!({"Idle": clip.clone()});
    let parsed = parse(&export_inp(&model, &textures, &small()).unwrap());
    assert_eq!(parsed.model["animations"]["Idle"], clip);
}

#[test]
fn a_clip_leaves_its_audio_tracks_behind() {
    let (mut model, textures) = fixture();
    let clip = json!({"length": 60, "timestep": 0.016, "additive": false, "animationWeight": 1.0, "leadIn": -1,
        "leadOut": -1, "lanes": []});
    let mut voiced = clip.clone();
    voiced["audio"] = json!([{"path": "/Users/someone/voice.wav", "frame": 3, "group": "Set"}]);
    voiced["images"] = json!([{"path": "/Users/someone/sky.png", "x": 0, "y": 0, "scale": 1, "opacity": 1}]);
    voiced["groups"] = json!([{"name": "Set", "x": 0, "y": 0, "scale": 1, "opacity": 1}]);
    voiced["modelGroup"] = json!("Set");
    model["animations"] = json!({"Idle": voiced});
    let parsed = parse(&export_inp(&model, &textures, &small()).unwrap());
    assert_eq!(parsed.model["animations"]["Idle"], clip, "a file name on this machine is not distributed");
}

#[test]
fn a_clip_leaves_its_shy_and_freeze_marks_and_bookmarks_behind() {
    let (mut model, textures) = fixture();
    let clip = json!({"length": 60, "timestep": 0.016, "additive": false, "animationWeight": 1.0, "leadIn": -1,
        "leadOut": -1, "lanes": [{"uuid": 42, "target": 0, "keyframes": []}]});
    let mut shy = clip.clone();
    shy["lanes"][0]["shy"] = json!(true);
    shy["lanes"][0]["frozen"] = json!(true);
    shy["shyActive"] = json!(true);
    shy["freezeFrame"] = json!(3);
    shy["bookmarks"] = json!([{"label": "Arm", "x": 1.0, "y": 2.0, "parameters": [42]}]);
    shy["controllers"] = json!([{"name": "Grip", "start": {"node": 1, "vertices": [0, 1, 2], "weights": [1.0, 0.0, 0.0]},
        "parameters": [42]}]);
    shy["forms"] = json!([{"node": 1, "mode": 2, "keys": [{"frame": 0, "offsets": [[1.0, 0.0]]}]}]);
    shy["formTime"] = json!({"keyframes": [{"frame": 0, "value": 0.0}]});
    shy["frameStep"] = json!({"keyframes": [{"frame": 0, "value": 2}]});
    shy["frameStepGroups"] = json!({"9": {"keyframes": [{"frame": 0, "value": 3}]}});
    model["animations"] = json!({"Idle": shy});
    let parsed = parse(&export_inp(&model, &textures, &small()).unwrap());
    assert_eq!(parsed.model["animations"]["Idle"], clip, "shy, freeze, bookmarks, controllers and form edits only serve the editor");
}

#[test]
fn keeping_unused_nodes_leaves_the_disabled_part_with_its_atlas_slot() {
    let (model, textures) = fixture();
    let options = InpExportOptions { prune_unused: false, ..small() };
    let export = export_inp_report(&model, &textures, &[], &options).unwrap();
    assert_eq!(export.pruned_nodes, 0);
    let parsed = parse(&export.bytes);
    let b = find(&parsed.model["nodes"], 3).unwrap();
    assert_eq!(b["enabled"], false);
    assert_eq!(b["textures"], json!([0, u32::MAX, u32::MAX]));
    // mapping [1, 13, 2, 2]
    assert_eq!(flat(&b["mesh"]["uvs"]), [0.0625, 0.8125, 0.0625, 0.9375, 0.1875, 0.8125, 0.1875, 0.9375]);
    // Cameras still leave: flatten removes them regardless.
    assert!(find(&parsed.model["nodes"], 5).is_none());
}

#[test]
fn parts_that_do_not_fit_one_atlas_use_more_and_empty_slots_follow_the_atlas() {
    let big = png(8, 8, |x, y| [(x * 10) as u8, (y * 10) as u8, 1, 255]);
    let glow = png(8, 8, |_, _| [250, 240, 230, 255]);
    let mut first = part(2, "First", true, 0, [4.0, 4.0], FULL);
    first["textures"] = json!([0, 2, u32::MAX]);
    let second = part(3, "Second", true, 1, [4.0, 4.0], FULL);
    let model = json!({
        "meta": {"name": "multi", "version": "1.0-alpha"}, "physics": {},
        "nodes": node(1, "Root", "Node", json!([first, second])),
        "param": [], "automation": [], "animations": {},
    });
    let textures = vec![big.clone(), big, glow];
    // 8 + 2 padding = 10 per Part: one 10x10 slot per 16x16 atlas.
    let export = export_inp_report(&model, &textures, &[], &small()).unwrap();
    assert_eq!(export.atlas_count, 2);
    let parsed = parse(&export.bytes);
    // Atlas 0: albedo + emissive; atlas 1: albedo only. Slots are numbered in tree order of use.
    assert_eq!(parsed.textures.len(), 3);
    assert!(parsed.textures.iter().all(|texture| texture.0 == 1));
    let first = find(&parsed.model["nodes"], 2).unwrap();
    let second = find(&parsed.model["nodes"], 3).unwrap();
    assert_eq!(first["textures"], json!([0, 1, u32::MAX]));
    assert_eq!(second["textures"], json!([2, u32::MAX, u32::MAX]));
    let emissive = decode(&parsed.textures[1]);
    assert_eq!(emissive.dimensions(), (16, 16));
    // Three channels in the file: alpha reads back as opaque, the copy sits at the mapping (1,1).
    assert_eq!(px(&emissive, 1, 1), [250, 240, 230, 255]);
    assert_eq!(px(&emissive, 12, 12), [0, 0, 0, 255]);
    let albedo_two = decode(&parsed.textures[2]);
    assert_eq!(px(&albedo_two, 1, 1), [0, 0, 1, 255]);
    assert_eq!(px(&albedo_two, 8, 8), [70, 70, 1, 255]);
}

#[test]
fn a_part_without_a_usable_uv_area_keeps_its_texture_and_is_reported() {
    let (mut model, textures) = fixture();
    let flat_uvs = [0.5; 8];
    model["nodes"]["children"][0] = part(2, "A", true, 0, [2.0, 2.0], flat_uvs);
    let export = export_inp_report(&model, &textures, &[], &small()).unwrap();
    assert_eq!(export.skipped_parts, [2]);
    let parsed = parse(&export.bytes);
    let a = find(&parsed.model["nodes"], 2).unwrap();
    // Slots follow tree order of use: A keeps its own PNG as slot 0, the atlas follows.
    assert_eq!(a["textures"], json!([0, u32::MAX, u32::MAX]));
    assert_eq!(flat(&a["mesh"]["uvs"]), flat_uvs);
    assert_eq!(find(&parsed.model["nodes"], 4).unwrap()["textures"], json!([1, u32::MAX, u32::MAX]));
    assert_eq!(parsed.textures[0].0, 0);
    assert_eq!(parsed.textures[1].0, 1);
}

#[test]
fn extension_sections_are_appended_unchanged_and_checked() {
    let (model, textures) = fixture();
    let mut ext = b"EXT_SECT".to_vec();
    ext.extend_from_slice(&1u32.to_be_bytes());
    ext.extend_from_slice(&3u32.to_be_bytes());
    ext.extend_from_slice(b"abc");
    ext.extend_from_slice(&2u32.to_be_bytes());
    ext.extend_from_slice(b"{}");
    let bytes = export_inp_report(&model, &textures, &ext, &small()).unwrap().bytes;
    assert_eq!(parse(&bytes).tail, ext);
    for broken in [&ext[..ext.len() - 1], &b"EXT_SECX\0\0\0\0"[..], &b"EXT_SECT"[..]] {
        let error = export_inp_report(&model, &textures, broken, &small()).unwrap_err();
        assert!(error.contains("extension"), "{error}");
    }
    let mut trailing = ext.clone();
    trailing.push(0);
    assert!(export_inp_report(&model, &textures, &trailing, &small()).unwrap_err().contains("trailing"));
}

#[test]
fn deformers_with_own_deform_bindings_are_reported_not_baked() {
    let (mut model, textures) = fixture();
    model["nodes"]["children"].as_array_mut().unwrap().push(node(9, "Grid", "GridDeformer", json!([])));
    model["param"][1]["bindings"] = json!([{
        "node": 9, "param_name": "deform", "interpolate_mode": "Linear",
        "values": [[[0.0, 0.0]], [[1.0, 1.0]]], "isSet": [[true], [true]],
    }]);
    let export = export_inp_report(&model, &textures, &[], &small()).unwrap();
    assert_eq!(export.unbaked_deformers, [9]);
    let parsed = parse(&export.bytes);
    assert_eq!(parsed.model["param"][1]["bindings"][0]["param_name"], "deform");
}

#[test]
fn options_have_d_defaults_and_validate() {
    let defaults = InpExportOptions::default();
    assert_eq!(
        (defaults.atlas_resolution, defaults.non_linear_scaling, defaults.scale, defaults.padding, defaults.prune_unused),
        (2048, false, 1.0, 16, true)
    );
    assert!(defaults.watermark.is_none() && defaults.validate().is_ok());

    let parsed = InpExportOptions::from_request(&json!({
        "atlasResolution": 1024, "nonLinearScaling": true, "scale": 0.5, "padding": 4, "optimizePruneUnused": false,
        "file": "ignored.inp",
    }))
    .unwrap();
    assert_eq!(
        (parsed.atlas_resolution, parsed.non_linear_scaling, parsed.scale, parsed.padding, parsed.prune_unused),
        (1024, true, 0.5, 4, false)
    );
    assert_eq!(InpExportOptions::from_request(&json!({"scale": null})).unwrap().scale, 1.0);

    for (request, message) in [
        (json!({"atlasResolution": 0}), "between 1 and 8192"),
        (json!({"atlasResolution": 8193}), "between 1 and 8192"),
        (json!({"atlasResolution": -1}), "positive integer"),
        (json!({"atlasResolution": 1.5}), "positive integer"),
        (json!({"padding": -1}), "padding"),
        (json!({"padding": 4096, "atlasResolution": 1024}), "padding"),
        (json!({"scale": 0}), "scale"),
        (json!({"scale": 17}), "scale"),
        (json!({"scale": "big"}), "number"),
        (json!({"nonLinearScaling": 1}), "boolean"),
        (json!({"optimizePruneUnused": "yes"}), "boolean"),
    ] {
        let error = InpExportOptions::from_request(&request).unwrap_err();
        assert!(error.contains(message), "{request}: {error}");
    }
    let nan = InpExportOptions { scale: f32::NAN, ..InpExportOptions::default() };
    assert!(nan.validate().is_err());

    let image = |format| PixelImage::zeroed(2, 2, format).unwrap();
    let mark = |image, opacity, loops| Watermark { image, opacity, blend: WatermarkBlend::Normal, loops };
    for (watermark, message) in [
        (mark(image(PixelFormat::Rgb), 1.0, 1), "RGBA"),
        (mark(image(PixelFormat::Rgba), 1.5, 1), "opacity"),
        (mark(image(PixelFormat::Rgba), f32::NAN, 1), "opacity"),
        (mark(image(PixelFormat::Rgba), 1.0, 0), "loops"),
        (mark(image(PixelFormat::Rgba), 1.0, 1001), "loops"),
    ] {
        let options = InpExportOptions { watermark: Some(watermark), ..InpExportOptions::default() };
        assert!(options.validate().unwrap_err().contains(message), "{message}");
    }
    assert_eq!(WatermarkBlend::from_name("ClipToLower").unwrap(), WatermarkBlend::ClipToLower);
    assert!(WatermarkBlend::from_name("Plaid").is_err());
}

#[test]
fn export_rejects_bad_input_without_panicking() {
    let (model, textures) = fixture();
    let invalid = InpExportOptions { atlas_resolution: 0, ..small() };
    assert!(export_inp(&model, &textures, &invalid).unwrap_err().contains("atlas resolution"));
    assert!(export_inp(&json!({"meta": {}}), &textures, &small()).unwrap_err().contains("nodes"));
    assert!(export_inp(&model, &textures[..1], &small()).unwrap_err().contains("texture slot 1"));

    // Wider than the 8192 decode limit.
    let wide = png(9000, 1, |_, _| [1, 2, 3, 255]);
    let mut huge = textures.clone();
    huge[0] = wide;
    assert!(export_inp(&model, &huge, &small()).unwrap_err().contains("could not decode texture slot 0"));

    // Fits the decode limit but not the atlas.
    let mut tall = textures.clone();
    tall[0] = png(15, 15, |_, _| [1, 2, 3, 255]);
    assert!(export_inp(&model, &tall, &small()).unwrap_err().contains("too big for the atlas"));

    let mut garbage = textures.clone();
    garbage[2] = Texture { tag: 0, data: b"not a png".to_vec() };
    assert!(export_inp(&model, &garbage, &small()).unwrap_err().contains("texture slot 2"));
    let mut bc7 = textures.clone();
    bc7[2] = Texture { tag: 2, data: vec![0; 16] };
    assert!(export_inp(&model, &bc7, &small()).unwrap_err().contains("unsupported image tag 2"));

    let mut no_parts = model.clone();
    no_parts["nodes"]["children"] = json!([]);
    assert!(export_inp(&no_parts, &textures, &small()).unwrap_err().contains("no Part"));
}

fn watermark(color: [u8; 4], blend: WatermarkBlend) -> Option<Watermark> {
    let mut pixels = Vec::new();
    for _ in 0..4 {
        pixels.extend(color);
    }
    let image = PixelImage::from_pixels(2, 2, PixelFormat::Rgba, pixels).unwrap();
    Some(Watermark { image, opacity: 1.0, blend, loops: 3 })
}

fn albedo_with(mark: Option<Watermark>) -> RgbaImage {
    let (model, textures) = fixture();
    let options = InpExportOptions { watermark: mark, ..small() };
    decode(&parse(&export_inp(&model, &textures, &options).unwrap()).textures[0])
}

#[test]
fn watermark_clip_to_lower_marks_only_opaque_atlas_pixels() {
    let image = albedo_with(watermark([100, 50, 20, 255], WatermarkBlend::ClipToLower));
    assert_eq!(px(&image, 2, 2), [100, 50, 20, 255]);
    assert_eq!(px(&image, 2, 8), [100, 50, 20, 255]);
    assert_eq!(px(&image, 0, 0), [0, 0, 0, 0]);
    assert_eq!(px(&image, 15, 15), [0, 0, 0, 0]);
}

#[test]
fn watermark_normal_covers_the_whole_atlas_and_multiply_scales_the_pixels() {
    let normal = albedo_with(watermark([100, 50, 20, 255], WatermarkBlend::Normal));
    assert_eq!(px(&normal, 0, 0), [100, 50, 20, 255]);
    assert_eq!(px(&normal, 3, 3), [100, 50, 20, 255]);

    // Multiply: src * dst. C's pixel (3,3) is [60, 120, 7]; times 128/255 gives 30.1, 60.2, 3.5.
    let multiplied = albedo_with(watermark([128, 128, 128, 255], WatermarkBlend::Multiply));
    assert_eq!(px(&multiplied, 3, 3), [30, 60, 4, 255]);
    assert_eq!(px(&multiplied, 0, 0), [0, 0, 0, 0]);

    // Opacity scales the source: half-opaque Normal over transparent keeps half the watermark.
    let mut half = watermark([200, 100, 0, 255], WatermarkBlend::Normal).unwrap();
    half.opacity = 0.5;
    let faded = albedo_with(Some(half));
    assert_eq!(px(&faded, 15, 15), [100, 50, 0, 128]);
}
