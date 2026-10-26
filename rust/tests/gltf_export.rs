//! `export.gltf` and the MCP tool `export_gltf`: a document as layered planes in glTF 2.0 (issue #245).
//!
//! No glTF validator is run here. `check` tests the structure a reader relies on.

mod common;

use base64::Engine;
use serde_json::{json, Value};
use soul2d_core::model_import::{inx_bytes, Texture};
use soul2d_core::Session;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
const UNITS: f64 = 100.0;
const META_KEY: &str = "io.github.geekjapan.soul2d.VrmMeta";

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir()
        .join(format!("soul2d-gltf-export-{}-{}", std::process::id(), SEQUENCE.fetch_add(1, Ordering::Relaxed)));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    common::scratch(path)
}

fn png(width: u32, height: u32, pixel: [u8; 4]) -> Texture {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba(pixel));
    let mut data = std::io::Cursor::new(Vec::new());
    image.write_to(&mut data, image::ImageFormat::Png).unwrap();
    Texture { tag: 0, data: data.into_inner() }
}

fn node(id: u32, name: &str, kind: &str, children: Value) -> Value {
    json!({
        "uuid": id, "name": name, "type": kind, "enabled": true, "zsort": 0.0,
        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
        "lockToRoot": false, "pinToMesh": false, "children": children,
    })
}

/// A quad of half the size `half` around `at`, with the two triangles wound against each other.
fn part(id: u32, name: &str, slot: u32, at: [f64; 2], half: f64, zsort: f64) -> Value {
    let mut part = node(id, name, "Part", json!([]));
    part["zsort"] = json!(zsort);
    part["transform"]["trans"] = json!([at[0], at[1], 0.0]);
    part["mesh"] = json!({
        "verts": [-half, -half, -half, half, half, -half, half, half],
        "uvs": [0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0],
        "indices": [0, 1, 2, 2, 3, 1],
        "origin": [0.0, 0.0],
    });
    part["textures"] = json!([slot, u32::MAX, u32::MAX]);
    for (key, value) in [("blend_mode", json!("Normal")), ("tint", json!([1.0, 1.0, 1.0])), ("opacity", json!(1.0)),
        ("screenTint", json!([0.0, 0.0, 0.0])), ("emissionStrength", json!(1.0)), ("mask_threshold", json!(0.5))]
    {
        part[key] = value;
    }
    part
}

/// Back (opaque, drawn first), Front (tinted, half opaque, Multiply, moved by the parameter Open), a disabled Part,
/// an empty DepthRigRoot, a one-axis parameter with three keys, a two-axis parameter and a parameter that moves
/// nothing.
fn fixture() -> (Value, Vec<Texture>) {
    let mut front = part(3, "Front", 1, [10.0, -20.0], 1.0, 0.0);
    front["tint"] = json!([0.5, 1.0, 1.0]);
    front["opacity"] = json!(0.5);
    front["blend_mode"] = json!("Multiply");
    front["screenTint"] = json!([0.1, 0.0, 0.0]);
    let mut hidden = part(4, "Hidden", 0, [0.0, 0.0], 1.0, 0.0);
    hidden["enabled"] = json!(false);
    let open = json!({
        "uuid": 40, "name": "Open", "is_vec2": false, "min": [0.0, 0.0], "max": [1.0, 1.0], "defaults": [0.0, 0.0],
        "axis_points": [[0.0, 0.5, 1.0], [0.0]], "merge_mode": "Additive",
        "bindings": [{"node": 3, "param_name": "transform.t.x", "interpolate_mode": "Linear",
            "values": [[0.0], [2.0], [5.0]], "isSet": [[true], [true], [true]]},
            {"node": 3, "param_name": "transform.t.y", "interpolate_mode": "Linear",
            "values": [[0.0], [1.0], [-3.0]], "isSet": [[true], [true], [true]]}],
    });
    let still = json!({
        "uuid": 42, "name": "Still", "is_vec2": false, "min": [0.0, 0.0], "max": [1.0, 1.0], "defaults": [0.0, 0.0],
        "axis_points": [[0.0, 1.0], [0.0]], "merge_mode": "Additive", "bindings": [],
    });
    let tilt = json!({
        "uuid": 41, "name": "Tilt", "is_vec2": true, "min": [-1.0, -1.0], "max": [1.0, 1.0], "defaults": [0.0, 0.0],
        "axis_points": [[0.0, 1.0], [0.0, 1.0]], "merge_mode": "Additive", "bindings": [],
    });
    let model = json!({
        "meta": {"name": "Layers", "artist": "Soul2D tests", "version": "1.0-alpha"}, "physics": {},
        "nodes": node(1, "Root", "Node", json!([
            part(2, "Back", 0, [0.0, 0.0], 2.0, 1.0),
            front,
            hidden,
            node(7, "Rig", "DepthRigRoot", json!([])),
        ])),
        "param": [open, tilt, still], "automation": [],
        "animations": {"Idle": {"length": 4, "timestep": 0.1, "additive": false, "animationWeight": 1.0,
            "leadIn": -1, "leadOut": -1, "lanes": []}},
    });
    // Texture 1 holds colour multiplied by an alpha of 128, and colour bleed in one fully transparent texel.
    let mut bled = image::RgbaImage::from_pixel(2, 2, image::Rgba([100, 50, 0, 128]));
    bled.put_pixel(1, 1, image::Rgba([7, 8, 9, 0]));
    let mut data = std::io::Cursor::new(Vec::new());
    bled.write_to(&mut data, image::ImageFormat::Png).unwrap();
    (model, vec![png(4, 4, [10, 20, 30, 255]), Texture { tag: 0, data: data.into_inner() }])
}

fn open(model: &Value, textures: &[Texture]) -> (Session, PathBuf) {
    let dir = temp_dir();
    let path = dir.join("document.inx");
    fs::write(&path, inx_bytes(model, textures).unwrap()).unwrap();
    let mut session = Session::new();
    let opened = session.handle_request(&json!({"op": "open", "path": path.to_str().unwrap()}));
    assert_eq!(opened["succeeded"], true, "{opened}");
    (session, dir)
}

fn call(session: &mut Session, arguments: Value) -> Value {
    let reply = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "export_gltf", "arguments": arguments}}))
        .unwrap();
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    reply["result"]["structuredContent"].clone()
}

fn export(session: &mut Session, path: &Path) -> Value {
    call(session, json!({"path": path.to_str().unwrap(), "unitsPerMetre": UNITS, "lossy": true}))
}

fn refusal(session: &mut Session, arguments: Value) -> String {
    let mut request = arguments;
    request["op"] = json!("export.gltf");
    let reply = session.handle_request(&request);
    assert_eq!(reply["succeeded"], false, "{reply}");
    reply["message"].as_str().unwrap().to_owned()
}

/// The JSON and the buffer of a `.glb`, or of a `.gltf` whose buffer is a data URI.
fn parse(path: &Path) -> (Value, Vec<u8>) {
    let bytes = fs::read(path).unwrap();
    if !bytes.starts_with(b"glTF") {
        let gltf: Value = serde_json::from_slice(&bytes).unwrap();
        let data = gltf["buffers"][0]["uri"].as_str().unwrap().strip_prefix("data:application/octet-stream;base64,");
        let buffer = base64::engine::general_purpose::STANDARD.decode(data.unwrap()).unwrap();
        return (gltf, buffer);
    }
    let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    assert_eq!((word(4), word(8)), (2, bytes.len()), "version and total length");
    let json_end = 20 + word(12);
    assert_eq!((&bytes[16..20], &bytes[json_end + 4..json_end + 8]), (b"JSON".as_slice(), b"BIN\0".as_slice()));
    assert_eq!((word(12) % 4, word(json_end) % 4, json_end + 8 + word(json_end)), (0, 0, bytes.len()));
    let gltf: Value = serde_json::from_slice(&bytes[20..json_end]).unwrap();
    assert!(gltf["buffers"][0].get("uri").is_none(), "a binary file keeps its buffer in the second chunk");
    (gltf, bytes[json_end + 8..].to_vec())
}

fn items(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

/// The numbers of accessor `index` in rows of its type, after the checks every accessor has to pass.
fn accessor(gltf: &Value, buffer: &[u8], index: &Value) -> Vec<Vec<f64>> {
    let accessor = &gltf["accessors"][index.as_u64().expect("an accessor index") as usize];
    let view = &gltf["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
    let (offset, length) = (view["byteOffset"].as_u64().unwrap() as usize, view["byteLength"].as_u64().unwrap() as usize);
    let width = match accessor["type"].as_str().unwrap() {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        other => panic!("{other}"),
    };
    let count = accessor["count"].as_u64().unwrap() as usize;
    assert!(count > 0 && offset % 4 == 0 && offset + length <= buffer.len(), "{accessor} {view}");
    let bytes = &buffer[offset..offset + length];
    let rows: Vec<Vec<f64>> = match accessor["componentType"].as_u64().unwrap() {
        5123 => {
            assert_eq!((length, view["target"].as_u64()), (count * 2, Some(34963)));
            bytes.chunks(2).map(|n| vec![f64::from(u16::from_le_bytes(n.try_into().unwrap()))]).collect()
        }
        5126 => {
            assert_eq!((length, view["target"].as_u64()), (count * width * 4, Some(34962)));
            let numbers: Vec<f64> = bytes.chunks(4).map(|n| f64::from(f32::from_le_bytes(n.try_into().unwrap()))).collect();
            let rows: Vec<Vec<f64>> = numbers.chunks(width).map(<[f64]>::to_vec).collect();
            // The bounds are the ones of the data, as glTF requires of positions.
            for (bound, pick) in [("min", f64::min as fn(f64, f64) -> f64), ("max", f64::max)] {
                let expected: Vec<f64> = (0..width).map(|axis| rows.iter().map(|row| row[axis]).fold(rows[0][axis], pick)).collect();
                // Compared in f64, as a validator does: a bound written as the short text of an f32 is not its value.
                let stated: Vec<f64> = items(&accessor[bound]).iter().map(|n| n.as_f64().unwrap()).collect();
                assert_eq!(stated, expected, "{bound} of {accessor}");
            }
            rows
        }
        other => panic!("component type {other}"),
    };
    assert!(rows.iter().flatten().all(|number| number.is_finite()));
    rows
}

fn no_null_and_no_empty_list(value: &Value, at: &str) {
    match value {
        Value::Null => panic!("{at} is null"),
        // A list of scene nodes or children may not be empty in glTF either.
        Value::Array(list) => {
            assert!(!list.is_empty(), "{at} is an empty list");
            list.iter().enumerate().for_each(|(index, item)| no_null_and_no_empty_list(item, &format!("{at}[{index}]")));
        }
        Value::Object(map) => map.iter().for_each(|(key, item)| no_null_and_no_empty_list(item, &format!("{at}.{key}"))),
        _ => {}
    }
}

/// The structure a glTF reader relies on. Returns the positions of every mesh.
fn check(gltf: &Value, buffer: &[u8]) -> Vec<Vec<Vec<f64>>> {
    no_null_and_no_empty_list(gltf, "gltf");
    assert_eq!(gltf["asset"]["version"], "2.0");
    assert_eq!((gltf["buffers"][0]["byteLength"].as_u64().unwrap() as usize, buffer.len() % 4), (buffer.len(), 0));
    let count = |key: &str| items(&gltf[key]).len() as u64;
    let below = |index: &Value, key: &str| assert!(index.as_u64().unwrap() < count(key), "{index} is no index into {key}");
    let mut parents = vec![0; count("nodes") as usize];
    for node in items(&gltf["nodes"]) {
        for child in items(&node["children"]) {
            below(child, "nodes");
            parents[child.as_u64().unwrap() as usize] += 1;
        }
        node.get("mesh").into_iter().for_each(|mesh| below(mesh, "meshes"));
    }
    for root in items(&gltf["scenes"][0]["nodes"]) {
        below(root, "nodes");
        parents[root.as_u64().unwrap() as usize] += 1;
    }
    assert!(parents.iter().all(|count| *count == 1), "every node has one place in the scene: {parents:?}");
    for material in items(&gltf["materials"]) {
        below(&material["pbrMetallicRoughness"]["baseColorTexture"]["index"], "textures");
        let factor = items(&material["pbrMetallicRoughness"]["baseColorFactor"]);
        assert!(factor.len() == 4 && factor.iter().all(|part| (0.0..=1.0).contains(&part.as_f64().unwrap())));
        assert!(items(&gltf["extensionsUsed"]).contains(&json!("KHR_materials_unlit")));
    }
    for texture in items(&gltf["textures"]) {
        below(&texture["source"], "images");
        below(&texture["sampler"], "samplers");
    }
    for image in items(&gltf["images"]) {
        let view = &gltf["bufferViews"][image["bufferView"].as_u64().unwrap() as usize];
        let offset = view["byteOffset"].as_u64().unwrap() as usize;
        assert!(view.get("target").is_none() && image["mimeType"] == "image/png");
        image::load_from_memory(&buffer[offset..offset + view["byteLength"].as_u64().unwrap() as usize]).unwrap();
    }
    let mut meshes = Vec::new();
    for mesh in items(&gltf["meshes"]) {
        let primitive = &mesh["primitives"][0];
        below(&primitive["material"], "materials");
        let positions = accessor(gltf, buffer, &primitive["attributes"]["POSITION"]);
        assert_eq!(accessor(gltf, buffer, &primitive["attributes"]["TEXCOORD_0"]).len(), positions.len());
        let indices: Vec<usize> = accessor(gltf, buffer, &primitive["indices"]).iter().map(|row| row[0] as usize).collect();
        assert_eq!(indices.len() % 3, 0);
        for triangle in indices.chunks(3) {
            let [a, b, c] = [0, 1, 2].map(|corner| &positions[triangle[corner]]);
            let area = (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]);
            assert!(area > 0.0, "{} has a triangle that is not counter-clockwise from +Z", mesh["name"]);
        }
        let targets = items(&primitive["targets"]);
        assert_eq!((items(&mesh["weights"]).len(), items(&mesh["extras"]["targetNames"]).len()), (targets.len(), targets.len()));
        for target in targets {
            assert_eq!(accessor(gltf, buffer, &target["POSITION"]).len(), positions.len());
        }
        meshes.push(positions);
    }
    meshes
}

#[track_caller]
fn near(actual: &[f64], expected: &[f64]) {
    assert_eq!(actual.len(), expected.len(), "{actual:?} is not {expected:?}");
    assert!(actual.iter().zip(expected).all(|(a, e)| (a - e).abs() < 1e-6), "{actual:?} is not {expected:?}");
}

fn item<'a>(answer: &'a Value, feature: &str, source: &str) -> &'a Value {
    let found = items(&answer["items"]).iter().find(|item| item["feature"] == feature && item["source"] == source);
    found.unwrap_or_else(|| panic!("no {feature} item for {source:?} in {}", answer["items"]))
}

#[test]
fn each_drawn_part_is_a_mesh_in_draw_order_with_its_texture_and_its_parameter_keys() {
    let (model, textures) = fixture();
    let (mut session, dir) = open(&model, &textures);
    let before = session.snapshot();
    let answer = export(&mut session, &dir.join("layers.glb"));
    assert_eq!((&answer["applied"], &answer["needs"]), (&json!(true), &json!([])), "{answer}");
    assert_eq!(session.snapshot(), before, "an export does not change the session");
    let (gltf, buffer) = parse(&dir.join("layers.glb"));
    let meshes = check(&gltf, &buffer);

    // Back is drawn first, so it is the layer behind. y is up in the file and down on the canvas.
    assert_eq!((&gltf["meshes"][0]["name"], &gltf["meshes"][1]["name"]), (&json!("Back"), &json!("Front")));
    for (corner, expected) in [[-0.02, 0.02, 0.0], [-0.02, -0.02, 0.0], [0.02, 0.02, 0.0], [0.02, -0.02, 0.0]].iter().enumerate() {
        near(&meshes[0][corner], expected);
    }
    near(&meshes[1][0], &[0.09, 0.21, 0.01]);
    near(&meshes[1][3], &[0.11, 0.19, 0.01]);

    // The tint, decoded from sRGB, and the opacity are the base colour, and the stored colour is divided by its
    // alpha. A fully transparent texel keeps its colour bleed.
    let factor = &gltf["materials"][1]["pbrMetallicRoughness"]["baseColorFactor"];
    near(&items(factor).iter().map(|part| part.as_f64().unwrap()).collect::<Vec<_>>(), &[0.2140411405, 1.0, 1.0, 0.5]);
    assert_eq!(gltf["materials"][1]["alphaMode"], "BLEND");
    let view = &gltf["bufferViews"][gltf["images"][1]["bufferView"].as_u64().unwrap() as usize];
    let offset = view["byteOffset"].as_u64().unwrap() as usize;
    let pixels = image::load_from_memory(&buffer[offset..offset + view["byteLength"].as_u64().unwrap() as usize]).unwrap();
    assert_eq!(pixels.to_rgba8().get_pixel(0, 0).0, [199, 100, 0, 128]);
    assert_eq!(pixels.to_rgba8().get_pixel(1, 1).0, [7, 8, 9, 0]);

    // Open moves Front by (2, 1) and by (5, -3) canvas units at its two keys away from the default, and down on the
    // canvas is down in the file. Back has no target.
    let front = &gltf["meshes"][1];
    assert_eq!(front["extras"]["targetNames"], json!(["Open=0.5", "Open=1"]));
    assert!(gltf["meshes"][0]["primitives"][0].get("targets").is_none());
    for (target, moved) in [(0, [0.02, -0.01, 0.0]), (1, [0.05, 0.03, 0.0])] {
        let deltas = accessor(&gltf, &buffer, &front["primitives"][0]["targets"][target]["POSITION"]);
        deltas.iter().for_each(|delta| near(delta, &moved));
    }

    // Puppet metadata is in the asset, and the file says who wrote it.
    assert_eq!(gltf["asset"]["extras"]["io.github.geekjapan.soul2d.PuppetMeta"], json!({"name": "Layers", "artist": "Soul2D tests"}));
    assert!(gltf["asset"]["generator"].as_str().unwrap().starts_with("Soul2D "));
    assert!(gltf["asset"].get("copyright").is_none());

    // The report names what the file does not hold.
    assert_eq!(answer["counts"], json!({"converted": 3, "approximated": 6, "kept": 0, "dropped": 6, "refused": 0}));
    assert_eq!(item(&answer, "part.blendMode", "Front")["outcome"], "dropped");
    assert_eq!(item(&answer, "part.screenTint", "Front")["outcome"], "dropped");
    assert_eq!(item(&answer, "part.tint", "Front")["outcome"], "approximated");
    assert_eq!(item(&answer, "part.mesh", "Hidden")["reason"], "it is not drawn");
    assert_eq!(item(&answer, "parameter.keyform", "Tilt")["outcome"], "dropped");
    assert_eq!(item(&answer, "parameter.keyform", "Open=1")["outcome"], "approximated");
    assert_eq!(item(&answer, "parameter.keyform", "Still=1")["reason"], "the key moves no vertex of a written Part");
    assert_eq!(item(&answer, "part.texture", "texture 1")["outcome"], "approximated");
    assert_eq!(item(&answer, "part.drawOrder", "")["outcome"], "approximated");
    assert_eq!(item(&answer, "animation", "Idle")["outcome"], "dropped");
    assert_eq!((&answer["direction"], &answer["rights"], &answer["unitsPerMetre"]), (&json!("soul2d-to-gltf"), &Value::Null, &json!(UNITS)));
    assert_eq!(answer["bytes"], fs::metadata(dir.join("layers.glb")).unwrap().len());

    // A `.gltf` is one file with the same buffer inside.
    let text = export(&mut session, &dir.join("layers.gltf"));
    assert_eq!(text["counts"], answer["counts"]);
    let (text_gltf, text_buffer) = parse(&dir.join("layers.gltf"));
    check(&text_gltf, &text_buffer);
    assert_eq!((text_buffer, &text_gltf["meshes"]), (buffer, &gltf["meshes"]));
}

#[test]
fn parts_share_a_texture_keep_consecutive_layers_and_stop_at_the_key_limit() {
    let (mut model, textures) = fixture();
    // Drawn between Back and Front: a Part without a texture, then Twin, which shares the texture of Back, is masked
    // by it, is tinted beyond white, which the renderer cuts as well, is a quarter opaque and is moved by 69 keys.
    let mut bare = part(6, "Bare", 0, [0.0, 0.0], 1.0, 0.75);
    bare["textures"] = json!([u32::MAX, u32::MAX, u32::MAX]);
    let mut twin = part(5, "Twin", 0, [0.0, 0.0], 1.0, 0.5);
    twin["masks"] = json!([{"source": 2, "mode": "Mask"}]);
    twin["tint"] = json!([2.0, 1.0, 1.0]);
    twin["opacity"] = json!(0.25);
    let children = model["nodes"]["children"].as_array_mut().unwrap();
    children.push(bare);
    children.push(twin);
    let points: Vec<f64> = (0..70).map(|key| f64::from(key) / 69.0).collect();
    let moved: Vec<[f64; 1]> = (0..70).map(|key| [f64::from(key)]).collect();
    model["param"].as_array_mut().unwrap().push(json!({
        "uuid": 43, "name": "Many".repeat(20), "is_vec2": false, "min": [0.0, 0.0], "max": [1.0, 1.0], "defaults": [0.0, 0.0],
        "axis_points": [points, [0.0]], "merge_mode": "Additive",
        "bindings": [{"node": 5, "param_name": "transform.t.x", "interpolate_mode": "Linear",
            "values": moved, "isSet": vec![[true]; 70]}],
    }));
    let (mut session, dir) = open(&model, &textures);
    let answer = export(&mut session, &dir.join("shared.glb"));
    assert_eq!(answer["applied"], true, "{answer}");
    let (gltf, buffer) = parse(&dir.join("shared.glb"));
    let meshes = check(&gltf, &buffer);

    let names: Vec<&str> = items(&gltf["meshes"]).iter().map(|mesh| mesh["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Back", "Twin", "Front"]);
    assert_eq!(item(&answer, "part.mesh", "Bare")["reason"], "it has no texture or no triangle");
    // A Part that is not written leaves no gap between the layers.
    let layers: Vec<f64> = meshes.iter().map(|positions| positions[0][2]).collect();
    near(&layers, &[0.0, 0.01, 0.02]);

    let texture = |material: usize| &gltf["materials"][material]["pbrMetallicRoughness"]["baseColorTexture"]["index"];
    assert_eq!((items(&gltf["images"]).len(), texture(0), texture(1), texture(2)), (2, &json!(0), &json!(0), &json!(1)));
    assert_eq!(gltf["materials"][1]["pbrMetallicRoughness"]["baseColorFactor"], json!([1.0, 1.0, 1.0, 0.25]));
    assert_eq!(item(&answer, "part.tint", "Twin")["outcome"], "approximated");
    assert_eq!(item(&answer, "mask", "Twin")["outcome"], "dropped");

    // Open and Still take three of the 64 keys, so Many has 61 targets and eight keys over the limit.
    // A long parameter name is cut, and the key that tells the targets apart is not.
    let targets = items(&gltf["meshes"][1]["extras"]["targetNames"]);
    assert_eq!((targets.len(), &targets[0]), (61, &json!(format!("{}=0.014492753623188406", "Many".repeat(8)))));
    assert_eq!(targets.iter().filter_map(Value::as_str).collect::<std::collections::BTreeSet<_>>().len(), 61);
    let over = items(&answer["items"]).iter().filter(|item| item["reason"] == "over the limit of 64 parameter keys");
    assert_eq!(over.count(), 8);
    let last = accessor(&gltf, &buffer, &gltf["meshes"][1]["primitives"][0]["targets"][60]["POSITION"]);
    near(&last[0], &[0.61, 0.0, 0.0]);
}

#[test]
fn nothing_is_written_until_the_caller_gives_the_scale_the_lossy_flag_and_the_acknowledgement() {
    let read_report = |session: &mut Session| {
        let read = session
            .handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "resources/read",
                "params": {"uri": "resource://soul2d/export/gltf-report"}}))
            .unwrap();
        serde_json::from_str::<Value>(read["result"]["contents"][0]["text"].as_str().unwrap()).unwrap()
    };
    let (model, textures) = fixture();
    let (mut session, dir) = open(&model, &textures);
    let path = dir.join("layers.glb");
    let path_text = path.to_str().unwrap();

    let bare = call(&mut session, json!({"path": path_text}));
    assert_eq!((&bare["applied"], &bare["needs"]), (&json!(false), &json!(["unitsPerMetre", "lossy"])));
    assert_eq!(bare["counts"]["converted"], 3);
    let dry = call(&mut session, json!({"path": path_text, "unitsPerMetre": UNITS, "lossy": true, "dryRun": true}));
    assert_eq!((&dry["applied"], &dry["needs"]), (&json!(false), &json!([])));
    assert!(!path.exists());
    assert_eq!(read_report(&mut session), Value::Null);

    // Rights data an import kept has to be acknowledged, is shown, and is written to the asset unchanged.
    let meta = json!({"name": "Synthetic", "licenseUrl": "https://example.invalid/terms", "notes": "c".repeat(2000)});
    let payload = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&meta).unwrap());
    let other = base64::engine::general_purpose::STANDARD.encode(b"{}");
    for (key, base64) in [(META_KEY, payload), ("io.github.geekjapan.soul2d.VrmHumanoid", other)] {
        let set = session.handle_request(&json!({"op": "extdata.set", "key": key, "base64": base64}));
        assert_eq!(set["succeeded"], true, "{set}");
    }
    let asked = call(&mut session, json!({"path": path_text, "unitsPerMetre": UNITS, "lossy": true}));
    assert_eq!((&asked["applied"], &asked["needs"]), (&json!(false), &json!(["acknowledgeRights"])));
    assert_eq!(asked["rights"][META_KEY]["licenseUrl"], "https://example.invalid/terms");
    assert_eq!((&asked["truncated"], asked["rights"][META_KEY]["notes"].as_str().unwrap().len()), (&json!(["rights"]), 1024));
    assert!(!path.exists());
    let arguments = json!({"path": path_text, "unitsPerMetre": UNITS, "lossy": true, "acknowledgeRights": true});
    let applied = call(&mut session, arguments);
    assert_eq!(applied["applied"], true);
    assert_eq!(item(&applied, "rights", META_KEY)["outcome"], "kept");
    assert_eq!(item(&applied, "extdata", "io.github.geekjapan.soul2d.VrmHumanoid")["outcome"], "dropped");
    assert_eq!(parse(&path).0["asset"]["extras"][META_KEY], meta);
    assert_eq!(read_report(&mut session), applied);

    // The copyright line of a kept glTF asset is the copyright line of the file.
    let asset = base64::engine::general_purpose::STANDARD.encode(br#"{"version":"2.0","copyright":"2026 Soul2D tests"}"#);
    let key = "io.github.geekjapan.soul2d.GltfAsset";
    assert_eq!(session.handle_request(&json!({"op": "extdata.set", "key": key, "base64": asset}))["succeeded"], true);
    let mut arguments = json!({"path": path_text, "unitsPerMetre": UNITS, "lossy": true, "acknowledgeRights": true});
    assert_eq!(refusal(&mut session, arguments.clone()), "export destination already exists");
    arguments["overwrite"] = json!(true);
    assert_eq!(call(&mut session, arguments)["applied"], true);
    assert_eq!(parse(&path).0["asset"]["copyright"], "2026 Soul2D tests");
}

#[test]
fn a_failed_write_keeps_the_last_applied_report() {
    let (model, textures) = fixture();
    let (mut session, dir) = open(&model, &textures);
    let applied = call(&mut session, json!({"path":dir.join("good.glb"),"unitsPerMetre":UNITS,"lossy":true}));
    let blocked = dir.join("directory.glb");fs::create_dir(&blocked).unwrap();
    let response = session.handle_request(&json!({"op":"export.gltf","path":blocked,"unitsPerMetre":UNITS,"lossy":true,"overwrite":true}));
    assert_eq!(response["succeeded"], false, "{response}");
    let read = session.handle_mcp(&json!({"jsonrpc":"2.0","id":1,"method":"resources/read",
        "params":{"uri":"resource://soul2d/export/gltf-report"}})).unwrap();
    assert_eq!(serde_json::from_str::<Value>(read["result"]["contents"][0]["text"].as_str().unwrap()).unwrap(), applied);
    assert!(blocked.is_dir());
}

#[test]
fn bones_are_nodes_that_rig_import_gltf_reads_back_where_they_were() {
    let (mut model, textures) = fixture();
    // A Part with the name of a standard bone is a mesh, which the reader below does not take for the bone.
    model["nodes"]["children"][0]["name"] = json!("Head");
    let (mut session, dir) = open(&model, &textures);
    for (component, value) in [("translationX", 30.0), ("translationY", -40.0), ("translationZ", 5.0)] {
        let moved = session.handle_request(&json!({"op": "node.transform", "id": 7, "component": component, "value": value}));
        assert_eq!(moved["succeeded"], true, "{moved}");
    }
    let added = session.handle_request(&json!({"op": "rig.add_skeleton", "root": 7}));
    assert_eq!(added["succeeded"], true, "{added}");
    let listed = session.handle_request(&json!({"op": "rig.list_bones", "root": 7}));
    let bones = items(&listed["result"]["items"]).to_vec();
    assert_eq!(bones.len(), 19);
    let path = dir.join("rig.glb");
    let answer = export(&mut session, &path);
    assert_eq!(answer["counts"]["converted"], 3 + 19, "{answer}");
    assert_eq!(item(&answer, "skeleton.restPose", "Rig")["outcome"], "approximated");
    let (gltf, buffer) = parse(&path);
    check(&gltf, &buffer);
    let rig = items(&gltf["nodes"]).iter().find(|node| node["name"] == "Rig").unwrap();
    // A bone is a child of its parent bone, and a bone without one is a child of the rig.
    let bone_node = |node: &Value, name: &Value| node["name"] == *name && node.get("mesh").is_none();
    let index_of = |name: &Value| items(&gltf["nodes"]).iter().position(|node| bone_node(node, name)).unwrap();
    for bone in &bones {
        let parent = bones.iter().find(|above| above["uuid"] == bone["parent"]);
        let above = parent.map_or(rig, |above| &gltf["nodes"][index_of(&above["boneId"])]);
        assert!(items(&above["children"]).contains(&json!(index_of(&bone["boneId"]))), "{} under {}", bone["boneId"], above["name"]);
    }
    assert_eq!(items(&rig["children"]).len(), bones.iter().filter(|bone| bone["parent"] == 7).count());
    let offset = [0.3, 0.4, 0.05];
    near(&items(&rig["translation"]).iter().map(|n| n.as_f64().unwrap()).collect::<Vec<_>>(), &offset);

    // A bone without a standard id is a node under its own name.
    let (mut loose, _) = fixture();
    loose["nodes"]["children"][3]["children"] = json!([node(8, "Loose", "DepthBone", json!([]))]);
    let (mut other, other_dir) = open(&loose, &textures);
    assert_eq!(export(&mut other, &other_dir.join("loose.glb"))["applied"], true);
    let (loose, _) = parse(&other_dir.join("loose.glb"));
    assert_eq!(items(&loose["nodes"]).iter().filter(|node| node["name"] == "Loose").count(), 1);

    // The same file read as a rig gives every bone its rest head, in the frame of the canvas.
    let mut reader = Session::new();
    let created = reader.handle_request(&json!({"op": "node.create", "className": "DepthRigRoot"}));
    let root = created["result"]["created"][0]["uuid"].as_u64().unwrap();
    let read = reader.handle_request(&json!({"op": "rig.import_gltf", "root": root, "path": path.to_str().unwrap(),
        "unitsPerSourceUnit": UNITS, "lossy": true}));
    assert_eq!(read["result"]["applied"], true, "{read}");
    for bone in &bones {
        let name = bone["boneId"].as_str().unwrap();
        let back = items(&read["result"]["bones"]).iter().find(|back| back["boneId"] == name).unwrap();
        for axis in 0..3 {
            let expected = bone["restHead"][axis].as_f64().unwrap() + offset[axis] * UNITS * [1.0, -1.0, 1.0][axis];
            let actual = back["restHead"][axis].as_f64().unwrap();
            assert!((actual - expected).abs() < 1e-3, "{name} axis {axis}: {actual} is not {expected}");
        }
    }
}

#[test]
fn a_request_that_cannot_be_written_safely_is_refused() {
    let (model, textures) = fixture();
    let (mut session, dir) = open(&model, &textures);
    let path = |name: &str| dir.join(name).to_str().unwrap().to_owned();
    let full = |name: &str| json!({"path": path(name), "unitsPerMetre": UNITS, "lossy": true});
    assert_eq!(refusal(&mut session, full("layers.fbx")), "path must name a .gltf or .glb file");
    assert_eq!(refusal(&mut session, full("layers")), "path must name a .gltf or .glb file");
    assert_eq!(refusal(&mut session, full("layers.glb/")), "path must name a file");
    assert!(refusal(&mut session, full("absent/layers.glb")).starts_with("could not resolve parent directory"));
    for units in [json!(0), json!(-1), json!("100")] {
        let mut request = full("layers.glb");
        request["unitsPerMetre"] = units;
        assert_eq!(refusal(&mut session, request), "unitsPerMetre must be a finite positive number");
    }
    let mut request = full("layers.glb");
    request["unitsPerMetre"] = json!(1e-300);
    assert_eq!(refusal(&mut session, request), "unitsPerMetre puts a point outside the range of the file");
    let mut request = full("layers.glb");
    request["lossy"] = json!("yes");
    assert_eq!(refusal(&mut session, request), "lossy must be a boolean");
    fs::write(dir.join("taken.glb"), b"kept").unwrap();
    assert_eq!(refusal(&mut session, full("taken.glb")), "export destination already exists");
    assert_eq!(fs::read(dir.join("taken.glb")).unwrap(), b"kept");
    assert!(!dir.join("layers.glb").exists(), "a refused export writes nothing");

    // A document without a drawn Part has nothing to write.
    let (mut empty, _) = fixture();
    empty["nodes"]["children"] = json!([]);
    empty["param"] = json!([]);
    let (mut session, dir) = open(&empty, &textures);
    let request = json!({"path": dir.join("empty.glb").to_str().unwrap(), "unitsPerMetre": UNITS, "lossy": true});
    assert_eq!(refusal(&mut session, request), "the document draws no textured Part to export");
    assert_eq!(refusal(&mut Session::new(), full("new.glb")), "the document draws no textured Part to export");

    // A file may hold two entries under one key. Neither is chosen over the other.
    let mut bytes = inx_bytes(&fixture().0, &textures).unwrap();
    bytes.extend(b"EXT_SECT");
    bytes.extend(2u32.to_be_bytes());
    for payload in [br#"{"name":"One"}"#, br#"{"name":"Two"}"#] {
        for part in [META_KEY.as_bytes(), payload.as_slice()] {
            bytes.extend((part.len() as u32).to_be_bytes());
            bytes.extend(part);
        }
    }
    let dir = temp_dir();
    fs::write(dir.join("twice.inx"), bytes).unwrap();
    let mut session = Session::new();
    let opened = session.handle_request(&json!({"op": "open", "path": dir.join("twice.inx").to_str().unwrap()}));
    assert_eq!(opened["succeeded"], true, "{opened}");
    let request = json!({"path": dir.join("twice.glb").to_str().unwrap(), "unitsPerMetre": UNITS, "lossy": true,
        "acknowledgeRights": true});
    assert_eq!(refusal(&mut session, request), format!("the document keeps two entries under {META_KEY}"));

    // An index of 16 bits cannot name the last of 65536 vertices.
    let (mut wide, _) = fixture();
    let numbers: Vec<f64> = (0..65536 * 2).map(|at| f64::from(at % 7)).collect();
    wide["nodes"]["children"][0]["mesh"]["verts"] = json!(numbers);
    wide["nodes"]["children"][0]["mesh"]["uvs"] = json!(vec![0.5; 65536 * 2]);
    let (mut session, dir) = open(&wide, &textures);
    let request = json!({"path": dir.join("wide.glb").to_str().unwrap(), "unitsPerMetre": UNITS, "lossy": true});
    assert_eq!(refusal(&mut session, request), "the Part Back has more than 65535 vertices");
}
