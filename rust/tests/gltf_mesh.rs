//! `mesh.import_gltf` and the MCP tool `mesh_import_gltf`: the mesh import prototype (issue #244).
//!
//! The model is built here from the tables below. No model file is read.

mod common;

use base64::Engine;
use serde_json::{json, Value};
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
const UNITS: f64 = 100.0;

/// Standard bone, its parent, the Mixamo-style name and the rest head in the rig frame, in metres: x to the right of
/// the picture, y down, z toward the viewer. The character faces the viewer, so its left arm is the `.R` arm.
const BONES: [(&str, &str, &str, [f64; 3]); 19] = [
    ("Pelvis", "", "Hips", [0.0, -1.0, 0.0]),
    ("Spine", "Pelvis", "Spine", [0.0, -1.1, 0.0]),
    ("Chest", "Spine", "Spine2", [0.0, -1.25, 0.02]),
    ("Neck", "Chest", "Neck", [0.0, -1.45, 0.02]),
    ("Head", "Neck", "Head", [0.0, -1.55, 0.02]),
    ("Clavicle.R", "Chest", "LeftShoulder", [0.03, -1.4, 0.0]),
    ("UpperArm.R", "Clavicle.R", "LeftArm", [0.18, -1.4, 0.0]),
    ("Forearm.R", "UpperArm.R", "LeftForeArm", [0.45, -1.38, 0.0]),
    ("Hand.R", "Forearm.R", "LeftHand", [0.7, -1.36, 0.0]),
    ("Clavicle.L", "Chest", "RightShoulder", [-0.03, -1.4, 0.0]),
    ("UpperArm.L", "Clavicle.L", "RightArm", [-0.18, -1.4, 0.0]),
    ("Forearm.L", "UpperArm.L", "RightForeArm", [-0.45, -1.38, 0.0]),
    ("Hand.L", "Forearm.L", "RightHand", [-0.7, -1.36, 0.0]),
    ("Thigh.R", "Pelvis", "LeftUpLeg", [0.1, -0.95, 0.0]),
    ("Shin.R", "Thigh.R", "LeftLeg", [0.1, -0.5, 0.02]),
    ("Foot.R", "Shin.R", "LeftFoot", [0.1, -0.08, 0.0]),
    ("Thigh.L", "Pelvis", "RightUpLeg", [-0.1, -0.95, 0.0]),
    ("Shin.L", "Thigh.L", "RightLeg", [-0.1, -0.5, 0.02]),
    ("Foot.L", "Shin.L", "RightFoot", [-0.1, -0.08, 0.0]),
];
/// The upper row of the texture has these colours and the lower row is black. A face takes one of the colours.
const COLOURS: [[u8; 4]; 4] = [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [255, 255, 0, 255]];
const RED: usize = 0;
const GREEN: usize = 1;
const BLUE: usize = 2;
const YELLOW: usize = 3;

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir()
        .join(format!("soul2d-gltf-mesh-{}-{}", std::process::id(), SEQUENCE.fetch_add(1, Ordering::Relaxed)));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    common::scratch(path)
}

/// The place of a rig point in the file: glTF has y up.
fn source(rig: [f64; 3]) -> [f64; 3] {
    [rig[0], -rig[1], rig[2]]
}

/// Index of a bone in the joints of the skin, which lists the bones in table order and then the hair bone.
fn joint(bone: &str) -> usize {
    BONES.iter().position(|row| row.0 == bone).unwrap_or(BONES.len())
}

#[derive(Default)]
struct Buffer {
    bytes: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
}

impl Buffer {
    fn view(&mut self, raw: &[u8]) -> usize {
        self.bytes.resize(self.bytes.len().div_ceil(4) * 4, 0);
        self.views.push(json!({"buffer": 0, "byteOffset": self.bytes.len(), "byteLength": raw.len()}));
        self.bytes.extend(raw);
        self.views.len() - 1
    }

    fn accessor(&mut self, kind: &str, component: u32, count: usize, raw: &[u8]) -> usize {
        let view = self.view(raw);
        self.accessors.push(json!({"bufferView": view, "componentType": component, "count": count, "type": kind}));
        self.accessors.len() - 1
    }

    fn floats(&mut self, kind: &str, width: usize, values: &[f64]) -> usize {
        let raw: Vec<u8> = values.iter().flat_map(|value| (*value as f32).to_le_bytes()).collect();
        self.accessor(kind, 5126, values.len() / width, &raw)
    }
}

/// The faces of one primitive. A position is stored `scale` times its offset from `origin`, a point of the file.
struct Faces {
    scale: f64,
    origin: [f64; 3],
    positions: Vec<f64>,
    uvs: Vec<f64>,
    indices: Vec<u16>,
    joints: Vec<u8>,
    weights: Vec<f64>,
}

impl Faces {
    fn new(scale: f64, origin: [f64; 3]) -> Faces {
        Faces { scale, origin, positions: vec![], uvs: vec![], indices: vec![], joints: vec![], weights: vec![] }
    }

    /// Four corners, in rig metres, as two triangles in the order given. `bones[i]` moves corner i: one bone, or
    /// two that share it.
    fn face(&mut self, corners: [[f64; 3]; 4], colour: usize, bones: [&[(&str, f64)]; 4]) {
        let first = (self.positions.len() / 3) as u16;
        for (corner, bones) in corners.iter().zip(bones) {
            let at = source(*corner);
            self.positions.extend((0..3).map(|axis| (at[axis] - self.origin[axis]) * self.scale));
            self.uvs.extend([(colour as f64 + 0.5) / COLOURS.len() as f64, 0.25]);
            let mut row = [(0u8, 0.0); 4];
            for (slot, (bone, weight)) in row.iter_mut().zip(bones) {
                *slot = (joint(bone) as u8, *weight);
            }
            self.joints.extend(row.iter().map(|slot| slot.0));
            self.weights.extend(row.iter().map(|slot| slot.1));
        }
        self.indices.extend([0, 1, 2, 0, 2, 3].map(|corner| first + corner));
    }

    /// The rectangle from `(x0, y0)` to `(x1, y1)` at depth `z`. Its corners are left top, left bottom, right bottom
    /// and right top when `front` turns it to the viewer, and run the other way round when not.
    fn quad(&mut self, [x0, y0, x1, y1]: [f64; 4], z: f64, colour: usize, front: bool, bones: [&[(&str, f64)]; 4]) {
        let corners = [[x0, y0, z], [x0, y1, z], [x1, y1, z], [x1, y0, z]];
        if front {
            self.face(corners, colour, bones);
        } else {
            let back = [bones[0], bones[3], bones[2], bones[1]];
            self.face([corners[0], corners[3], corners[2], corners[1]], colour, back);
        }
    }

    /// The primitive in `buffer`. `skinned` writes the joints and weights.
    fn primitive(&self, buffer: &mut Buffer, material: usize, skinned: bool) -> Value {
        let position = buffer.floats("VEC3", 3, &self.positions);
        let uv = buffer.floats("VEC2", 2, &self.uvs);
        let raw: Vec<u8> = self.indices.iter().flat_map(|index| index.to_le_bytes()).collect();
        let indices = buffer.accessor("SCALAR", 5123, self.indices.len(), &raw);
        let mut attributes = json!({"POSITION": position, "TEXCOORD_0": uv});
        if skinned {
            attributes["JOINTS_0"] = json!(buffer.accessor("VEC4", 5121, self.joints.len() / 4, &self.joints));
            attributes["WEIGHTS_0"] = json!(buffer.floats("VEC4", 4, &self.weights));
        }
        json!({"attributes": attributes, "indices": indices, "material": material})
    }
}

/// A humanoid in a plain glTF with these meshes, all in canvas centimetres once imported:
///
/// - `Body.0`, a closed box from (-20, -140) to (20, -100), 20 deep, blue.
/// - `Body.1`, a red sleeve from (0, -135) to (50, -125) at depth 20 and a green chest plate from (-15, -140) to
///   (15, -120) at depth 12. The sleeve comes first in the file and is the nearer one.
/// - `Cape`, a yellow sheet from (-30, -140) to (30, -90) at depth -20 that faces away and has two sides.
/// - `Fringe`, a red strip from (-10, -170) to (10, -160) on a hair bone, which is no standard bone.
/// - `Badge`, a green square from (-70, -30) to (-60, -20) without a skin, placed by its node, which mirrors it.
/// - `Back`, a sheet that faces away and has one side, `Lines`, which is no triangle list, and `Bare`, which has no
///   material.
///
/// The skinned positions are stored in centimetres and the inverse bind matrices scale them to metres.
fn model() -> (Value, Vec<u8>) {
    let head = |bone: &str| BONES.iter().find(|row| row.0 == bone).unwrap().3;
    let mut nodes = vec![json!({"name": "Armature", "children": [1]})];
    for (at, (_, parent, mixamo, rig)) in BONES.iter().enumerate() {
        let (here, above) = (source(*rig), if parent.is_empty() { [0.0; 3] } else { source(head(parent)) });
        nodes.push(json!({"name": format!("mixamorig:{mixamo}"),
            "translation": [here[0] - above[0], here[1] - above[1], here[2] - above[2]]}));
        if !parent.is_empty() {
            let children = nodes[joint(parent) + 1].as_object_mut().unwrap().entry("children").or_insert(json!([]));
            children.as_array_mut().unwrap().push(json!(at + 1));
        }
    }
    let hair = [0.0, -1.7, -0.05];
    let (hair_here, hair_above) = (source(hair), source(head("Head")));
    nodes.push(json!({"name": "Hair",
        "translation": [hair_here[0] - hair_above[0], hair_here[1] - hair_above[1], hair_here[2] - hair_above[2]]}));
    nodes[joint("Head") + 1]["children"] = json!([nodes.len() - 1]);
    let joints: Vec<usize> = (1..nodes.len()).collect();

    let mut buffer = Buffer::default();
    // A joint at p turns centimetres of the mesh into metres about itself: 0.01 times the point, less p.
    let mut inverse = Vec::new();
    for at in &joints {
        let rig = if *at <= BONES.len() { BONES[*at - 1].3 } else { hair };
        let p = source(rig);
        inverse.extend([0.01, 0.0, 0.0, 0.0, 0.0, 0.01, 0.0, 0.0, 0.0, 0.0, 0.01, 0.0, -p[0], -p[1], -p[2], 1.0]);
    }
    let inverse = buffer.floats("MAT4", 16, &inverse);

    let centimetres = || Faces::new(100.0, [0.0; 3]);
    let mut torso = centimetres();
    let (spine, hips): (&[(&str, f64)], &[(&str, f64)]) = (&[("Spine", 1.0)], &[("Spine", 0.5), ("Pelvis", 0.5)]);
    torso.quad([-0.2, -1.4, 0.2, -1.0], 0.1, BLUE, true, [spine, hips, hips, spine]);
    torso.quad([-0.2, -1.4, 0.2, -1.0], -0.1, BLUE, false, [spine; 4]);
    for x in [-0.2, 0.2] {
        torso.face([[x, -1.4, -0.1], [x, -1.0, -0.1], [x, -1.0, 0.1], [x, -1.4, 0.1]], BLUE, [spine; 4]);
    }
    for y in [-1.4, -1.0] {
        torso.face([[-0.2, y, -0.1], [0.2, y, -0.1], [0.2, y, 0.1], [-0.2, y, 0.1]], BLUE, [spine; 4]);
    }
    let mut layered = centimetres();
    let (upper, fore): (&[(&str, f64)], &[(&str, f64)]) = (&[("UpperArm.R", 1.0)], &[("Forearm.R", 2.0)]);
    layered.quad([0.0, -1.35, 0.5, -1.25], 0.2, RED, true, [upper, upper, fore, fore]);
    let (chest, hair_bone, none): (&[(&str, f64)], &[(&str, f64)], &[(&str, f64)]) =
        (&[("Chest", 1.0)], &[("Hair", 1.0)], &[]);
    layered.quad([-0.15, -1.4, 0.15, -1.2], 0.12, GREEN, true, [chest; 4]);
    let mut cape = centimetres();
    cape.quad([-0.3, -1.4, 0.3, -0.9], -0.2, YELLOW, false, [spine; 4]);
    let mut fringe = centimetres();
    fringe.quad([-0.1, -1.7, 0.1, -1.6], 0.1, RED, true, [hair_bone; 4]);
    // The node of the badge is at (-0.7, -0.3) of the rig and mirrors x. The mesh is stored as its mirror image,
    // a square from 0.6 to 0.7 that shows its front, so the node turns each triangle the other way round.
    let badge_node = source([-0.7, -0.3, 0.0]);
    let mut badge = Faces::new(1.0, source([0.7, -0.3, 0.0]));
    badge.quad([0.6, -0.3, 0.7, -0.2], 0.05, GREEN, true, [none; 4]);
    let mut back = Faces::new(1.0, [0.0; 3]);
    back.quad([0.4, -0.5, 0.5, -0.4], 0.0, RED, false, [none; 4]);

    let mut body = torso.primitive(&mut buffer, 0, true);
    body["attributes"]["NORMAL"] = body["attributes"]["POSITION"].clone();
    let target = json!({"POSITION": body["attributes"]["POSITION"]});
    body["targets"] = json!([target, target]);
    let layered = layered.primitive(&mut buffer, 0, true);
    let cape = cape.primitive(&mut buffer, 1, true);
    let fringe = fringe.primitive(&mut buffer, 0, true);
    let badge = badge.primitive(&mut buffer, 0, false);
    let back = back.primitive(&mut buffer, 0, false);
    let mut lines = back.clone();
    lines["mode"] = json!(1);
    let mut bare = back.clone();
    bare.as_object_mut().unwrap().remove("material");

    let cell = |x: u32, y: u32| image::Rgba(if y == 0 { COLOURS[x as usize] } else { [0, 0, 0, 255] });
    let texture = image::RgbaImage::from_fn(COLOURS.len() as u32, 2, cell);
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(texture).write_to(&mut png, image::ImageFormat::Png).unwrap();
    let png = buffer.view(&png.into_inner());

    let first_mesh = nodes.len();
    nodes.push(json!({"name": "BodyNode", "mesh": 0, "skin": 0}));
    nodes.push(json!({"name": "CapeNode", "mesh": 1, "skin": 0}));
    nodes.push(json!({"name": "FringeNode", "mesh": 2, "skin": 0}));
    nodes.push(json!({"name": "BadgeNode", "mesh": 3, "translation": badge_node, "scale": [-1, 1, 1]}));
    nodes.push(json!({"name": "BackNode", "mesh": 4}));
    nodes.push(json!({"name": "LinesNode", "mesh": 5}));
    nodes.push(json!({"name": "BareNode", "mesh": 6}));
    let scene: Vec<usize> = std::iter::once(0).chain(first_mesh..nodes.len()).collect();
    let gltf = json!({
        "asset": {"version": "2.0", "generator": "Soul2D tests"},
        "scene": 0,
        "scenes": [{"nodes": scene}],
        "nodes": nodes,
        "skins": [{"name": "Skin", "joints": joints, "inverseBindMatrices": inverse}],
        "meshes": [
            {"name": "Body", "primitives": [body, layered]}, {"name": "Cape", "primitives": [cape]},
            {"name": "Fringe", "primitives": [fringe]}, {"name": "Badge", "primitives": [badge]},
            {"name": "Back", "primitives": [back]}, {"name": "Lines", "primitives": [lines]},
            {"name": "Bare", "primitives": [bare]},
        ],
        "materials": [
            {"name": "Paint", "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}},
            {"name": "Cloth", "doubleSided": true, "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}},
        ],
        "textures": [{"source": 0}],
        "images": [{"name": "Palette", "bufferView": png, "mimeType": "image/png"}],
        "accessors": buffer.accessors,
        "bufferViews": buffer.views,
        "buffers": [{"byteLength": buffer.bytes.len()}],
    });
    (gltf, buffer.bytes)
}

/// Writes the model as a `.glb`, or as a `.gltf` whose buffer is a data URI.
fn write(name: &str, gltf: &Value, bin: &[u8]) -> String {
    let path = temp_dir().join(name);
    if name.ends_with(".gltf") {
        let mut gltf = gltf.clone();
        let data = base64::engine::general_purpose::STANDARD.encode(bin);
        gltf["buffers"][0]["uri"] = json!(format!("data:application/octet-stream;base64,{data}"));
        fs::write(&path, serde_json::to_vec(&gltf).unwrap()).unwrap();
    } else {
        let mut text = serde_json::to_vec(gltf).unwrap();
        text.resize(text.len().div_ceil(4) * 4, b' ');
        let mut padded = bin.to_vec();
        padded.resize(bin.len().div_ceil(4) * 4, 0);
        let mut bytes = b"glTF".to_vec();
        bytes.extend(2u32.to_le_bytes());
        bytes.extend((28 + text.len() as u32 + padded.len() as u32).to_le_bytes());
        bytes.extend((text.len() as u32).to_le_bytes());
        bytes.extend(b"JSON");
        bytes.extend(&text);
        bytes.extend((padded.len() as u32).to_le_bytes());
        bytes.extend(b"BIN\0");
        bytes.extend(&padded);
        fs::write(&path, bytes).unwrap();
    }
    path.to_str().unwrap().to_owned()
}

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

/// A session whose DepthRigRoot has the bones of the file at `path`.
fn rigged(path: &str) -> (Session, u64) {
    let mut session = Session::new();
    let created = request(&mut session, json!({"op": "node.create", "className": "DepthRigRoot"}));
    let root = result(&created)["created"][0]["uuid"].as_u64().unwrap();
    let rig = request(&mut session, json!({"op": "rig.import_gltf", "root": root, "path": path,
        "unitsPerSourceUnit": UNITS, "lossy": true, "acknowledgeRights": true}));
    assert_eq!(result(&rig)["applied"], true, "{rig}");
    (session, root)
}

fn call(session: &mut Session, arguments: Value) -> Value {
    let reply = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "mesh_import_gltf", "arguments": arguments}}))
        .unwrap();
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    reply["result"]["structuredContent"].clone()
}

fn refusal(session: &mut Session, root: u64, path: &str) -> String {
    let before = session.snapshot();
    let reply = request(session, json!({"op": "mesh.import_gltf", "root": root, "path": path,
        "unitsPerSourceUnit": UNITS, "lossy": true, "acknowledgeRights": true}));
    assert_eq!(reply["succeeded"], false, "{reply}");
    assert_eq!(session.snapshot(), before, "a refused import changes nothing");
    reply["message"].as_str().unwrap().to_owned()
}

fn node(session: &Session, id: &Value) -> Value {
    fn find(node: &Value, id: &Value) -> Option<Value> {
        if node["uuid"] == *id {
            return Some(node.clone());
        }
        node["children"].as_array()?.iter().find_map(|child| find(child, id))
    }
    find(&session.snapshot()["model"]["nodes"], id).unwrap()
}

fn floats(value: &Value) -> Vec<f64> {
    value.as_array().unwrap().iter().map(|number| number.as_f64().unwrap()).collect()
}

#[track_caller]
fn assert_near(actual: &[f64], expected: &[f64], what: &str) {
    let near = actual.len() == expected.len() && actual.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1e-4);
    assert!(near, "{what}: {actual:?} is not {expected:?}");
}

/// The outcome and the reason of the one report item of `feature` for `source`.
#[track_caller]
fn item(answer: &Value, feature: &str, source: &str) -> (String, String) {
    let items = answer["items"].as_array().unwrap().iter();
    let found: Vec<&Value> = items.filter(|item| item["feature"] == feature && item["source"] == source).collect();
    assert_eq!(found.len(), 1, "{feature} {source}: {answer}");
    (found[0]["outcome"].as_str().unwrap().to_owned(), found[0]["reason"].as_str().unwrap().to_owned())
}

fn outcome(outcome: &str, reason: &str) -> (String, String) {
    (outcome.to_owned(), reason.to_owned())
}

/// How many pixels of the picture from (-80, -180) to (80, 0), one for each canvas unit, have each colour of the
/// texture. `SOUL2D_PROTOTYPE_RENDERS` names a directory that also gets the picture, for the prototype note.
fn pixels(session: &mut Session, name: &str) -> [usize; 4] {
    pixels_in(session, name, [-80, -180, 80, 0])
}

/// The same count for a picture of 160 by 180 pixels that shows `bounds`.
fn pixels_in(session: &mut Session, name: &str, bounds: [i32; 4]) -> [usize; 4] {
    let reply = request(session, json!({"op": "render", "width": 160, "height": 180, "bounds": bounds}));
    let png = base64::engine::general_purpose::STANDARD.decode(result(&reply)["png"].as_str().unwrap()).unwrap();
    let picture = image::load_from_memory(&png).unwrap().to_rgba8();
    // The pictures of doc/3d-mesh-import-prototype.md, three pixels a canvas unit. An empty name writes none.
    if let Some(directory) = std::env::var_os("SOUL2D_PROTOTYPE_RENDERS").filter(|_| !name.is_empty()) {
        let large = image::imageops::resize(&picture, 480, 540, image::imageops::FilterType::Nearest);
        large.save(PathBuf::from(directory).join(format!("{name}.png"))).unwrap();
    }
    COLOURS.map(|colour| picture.pixels().filter(|pixel| pixel.0 == colour).count())
}

#[test]
fn a_skinned_model_becomes_parts_with_depth_and_weights_and_the_report_counts_what_is_lost() {
    let (gltf, bin) = model();
    for file in ["model.glb", "model.gltf"] {
        let path = write(file, &gltf, &bin);
        let (mut session, root) = rigged(&path);
        let arguments = json!({"root": root, "path": path, "unitsPerSourceUnit": UNITS});
        let before = session.snapshot();

        // The loss needs a yes first, and a dry run changes nothing.
        let asked = call(&mut session, arguments.clone());
        assert_eq!((&asked["applied"], &asked["needs"]), (&json!(false), &json!(["lossy"])), "{asked}");
        let mut lossy = arguments.clone();
        lossy["lossy"] = json!(true);
        lossy["dryRun"] = json!(true);
        let dry = call(&mut session, lossy.clone());
        assert_eq!((&dry["applied"], &dry["needs"]), (&json!(false), &json!([])), "{dry}");
        assert_eq!(session.snapshot(), before);
        lossy["dryRun"] = json!(false);
        let answer = call(&mut session, lossy);
        assert_eq!(answer["applied"], true, "{file} {answer}");
        assert_eq!(answer["source"], json!({"format": "glTF 2.0", "convention": "gltf2"}));

        // Five primitives are Parts. The box shows 2 of its 12 triangles, and the one-sided back, the lines and
        // the mesh without a material are not imported.
        let parts = answer["parts"].as_array().unwrap();
        let listed: Vec<(&str, u64, u64, u64)> = parts
            .iter()
            .map(|part| {
                let count = |key: &str| part[key].as_u64().unwrap();
                (part["name"].as_str().unwrap(), count("vertices"), count("triangles"), count("bones"))
            })
            .collect();
        let expected =
            [("Body.0", 4, 2, 2), ("Body.1", 8, 4, 3), ("Cape", 4, 2, 1), ("Fringe", 4, 2, 1), ("Badge", 4, 2, 0)];
        assert_eq!(listed, expected, "{file}");
        assert_eq!(
            answer["counts"],
            json!({"converted": 4, "approximated": 8, "kept": 0, "dropped": 6, "refused": 0}),
            "{answer}"
        );
        let item = |feature: &str, source: &str| item(&answer, feature, source);
        let flat = |hidden: usize, total: usize| {
            let reason = format!(
                "seen from the front with z as depth: {hidden} of {total} triangles turn away or stand on edge and \
                 are not drawn, the others are drawn far to near"
            );
            ("approximated".to_owned(), reason)
        };
        assert_eq!(item("mesh.geometry", "Body.0"), flat(10, 12));
        assert_eq!(item("mesh.geometry", "Body.1"), flat(0, 4));
        assert_eq!(item("mesh.geometry", "Cape"), flat(0, 2));
        let dropped = |reason: &str| outcome("dropped", reason);
        assert_eq!(item("mesh.geometry", "Back"), dropped("no triangle of it shows its front"));
        assert_eq!(item("mesh.geometry", "Lines"), dropped("only triangle lists are read"));
        assert_eq!(item("mesh.geometry", "Bare"), dropped("it has no base colour texture"));
        assert_eq!(item("mesh.attribute", "Body.0 NORMAL"), dropped("a flat Part has no use for it"));
        // Two morph targets are one item and count as two.
        assert_eq!(item("morph.target", "Body.0"), dropped("the prototype reads no morph target, and it has 2"));
        let moved = "the weight on a bone that is no standard bone goes to the standard bone above it";
        assert_eq!(item("mesh.skinWeights", "Fringe"), outcome("approximated", moved));
        let shading = "the base colour texture and factor are kept; shading is dropped, and alpha is always blended";
        assert_eq!(item("material", "Cloth"), outcome("approximated", shading));

        // The Part of the two layers: the farther chest plate comes first although the file has the sleeve first.
        // x and y are the canvas place, and the stored depth times the depth scale is z in canvas units.
        let scale = answer["depthScale"].as_f64().unwrap();
        let layered = node(&session, &parts[1]["id"]);
        let verts = [
            [-15.0, -140.0], [-15.0, -120.0], [15.0, -120.0], [15.0, -140.0],
            [0.0, -135.0], [0.0, -125.0], [50.0, -125.0], [50.0, -135.0],
        ];
        assert_near(&floats(&layered["mesh"]["verts"]), &verts.concat(), "verts");
        assert_eq!(layered["mesh"]["indices"], json!([0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7]));
        assert_eq!(layered["mesh"]["origin"], json!([0.0, 0.0]));
        assert_near(&floats(&layered["mesh"]["uvs"])[..2], &[0.375, 0.25], "the green cell");
        let depths: Vec<f64> = floats(&layered["depths"]).iter().map(|depth| depth * scale).collect();
        assert_near(&depths, &[12.0, 12.0, 12.0, 12.0, 20.0, 20.0, 20.0, 20.0], "depths");
        // The nearer Part has the smaller zsort and is drawn later: the mean depths are 10, 16 and -20.
        let zsort = |part: usize| node(&session, &parts[part]["id"])["zsort"].as_f64().unwrap();
        assert_near(&[zsort(0), zsort(1), zsort(2)], &[-10.0, -16.0, 20.0], "zsort");
        // The bones the rig scales depth by span 80 units, from the cape to the sleeve: 80 * 0.42 / 2.9.
        assert!((scale - 11.586_207).abs() < 1e-5, "{scale}");

        // Weights are rows for each standard bone in the order of the binding. A weight of 2 in the file is all
        // the weight of its vertex, a shared vertex keeps its halves, and the hair bone gives its weight to Head.
        let rig = node(&session, &json!(root));
        let binding = |part: usize| {
            let rows = rig["bindings"].as_array().unwrap().iter();
            rows.clone().find(|binding| binding["target"] == parts[part]["id"]).unwrap().clone()
        };
        let bone_names = |binding: &Value| -> Vec<String> {
            let sources = binding["sourceBoneUuids"].as_array().unwrap().iter();
            sources.map(|id| node(&session, id)["boneId"].as_str().unwrap().to_owned()).collect()
        };
        let sleeve = binding(1);
        assert_eq!(sleeve["targetKind"], "part");
        assert_eq!(bone_names(&sleeve), ["Chest", "Forearm.R", "UpperArm.R"]);
        assert_eq!(
            sleeve["vertexWeights"],
            json!([
                [1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0],
                [0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0],
            ])
        );
        let torso = binding(0);
        assert_eq!(bone_names(&torso), ["Pelvis", "Spine"]);
        assert_eq!(torso["vertexWeights"], json!([[0.0, 0.5, 0.5, 0.0], [1.0, 0.5, 0.5, 1.0]]));
        assert_eq!(bone_names(&binding(3)), ["Head"]);
        // The Part without a skin is placed by its node, which mirrors it, and is not bound.
        let badge = node(&session, &parts[4]["id"]);
        let square = [[-60.0, -30.0], [-60.0, -20.0], [-70.0, -20.0], [-70.0, -30.0]];
        assert_near(&floats(&badge["mesh"]["verts"]), &square.concat(), "badge");
        assert!(rig["bindings"].as_array().unwrap().iter().all(|binding| binding["target"] != parts[4]["id"]));

        // The last applied report is a resource.
        let read = session
            .handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "resources/read",
                "params": {"uri": "resource://soul2d/mesh/import-report"}}))
            .unwrap();
        let text = read["result"]["contents"][0]["text"].as_str().unwrap();
        assert_eq!(serde_json::from_str::<Value>(text).unwrap(), answer);

        // One undo step takes the Parts, the bindings and the texture away again.
        let texture = json!({"op": "texture.get", "slot": 0});
        assert_eq!(result(&request(&mut session, texture.clone()))["format"], "png");
        result(&request(&mut session, json!({"op": "undo"})));
        assert_eq!(session.snapshot()["model"], before["model"]);
        assert_eq!(request(&mut session, texture)["message"], "texture slot 0 is out of range (0 textures)");
    }
}

/// A session with the bones and the meshes of the model, and the answer of the mesh import.
fn imported() -> (Session, u64, Value) {
    let (gltf, bin) = model();
    let path = write("model.glb", &gltf, &bin);
    let (mut session, root) = rigged(&path);
    let answer = call(&mut session, json!({"root": root, "path": path, "unitsPerSourceUnit": UNITS, "lossy": true}));
    assert_eq!(answer["applied"], true, "{answer}");
    (session, root, answer)
}

/// The uuid of the standard bone `name`.
fn bone(session: &Session, name: &str) -> u64 {
    fn find(node: &Value, name: &str) -> Option<u64> {
        if node["boneId"] == name {
            return node["uuid"].as_u64();
        }
        node["children"].as_array()?.iter().find_map(|child| find(child, name))
    }
    find(&session.snapshot()["model"]["nodes"], name).unwrap()
}

/// A parameter whose one key puts `pose` on the bone `name`. One key holds at every value of the parameter.
fn posed(session: &mut Session, name: &str, pose: Value) -> u64 {
    let created = request(session, json!({"op": "parameter.create", "dimensions": 1, "name": format!("Pose {name}")}));
    let parameter = result(&created)["uuid"].as_u64().unwrap();
    let nodes = [bone(session, name)];
    let mut key = json!({"op": "binding.set_trs", "parameter": parameter, "nodes": nodes, "key": [1, 0]});
    for (field, value) in pose.as_object().unwrap() {
        key[field] = value.clone();
    }
    result(&request(session, key));
    parameter
}

#[test]
fn a_stored_depth_is_the_z_of_the_file_for_the_bones() {
    let (mut session, root, answer) = imported();
    let offsets = |session: &mut Session, part: usize, parameter: u64| -> Vec<f64> {
        let preview = request(session, json!({
            "op": "rig.preview_deform", "root": root, "targets": [answer["parts"][part]["id"]], "parameter": parameter
        }));
        let pairs = result(&preview)["targets"][0]["offsets"].as_array().unwrap().iter();
        pairs.flat_map(|pair| floats(pair)).collect()
    };
    // The chest and the head bone point straight up, so a quarter turn about their own axis is one about Y.
    // The chest bone is at x 0 and depth 2, and the chest plate at depth 12: the turn brings both its sides, from
    // x -15 and x 15, to x 10, which is the 10 units of depth between them and the bone.
    let turn = json!({"rotation_y": std::f64::consts::FRAC_PI_2});
    let chest = posed(&mut session, "Chest", turn.clone());
    let plate = [[25.0, 0.0], [25.0, 0.0], [-5.0, 0.0], [-5.0, 0.0]];
    assert_near(&offsets(&mut session, 1, chest)[..8], &plate.concat(), "chest plate");
    // The head bone is at depth 2 too and the fringe at depth 10: both its sides go to x 8.
    let (mut session, root, answer) = imported();
    let head = posed(&mut session, "Head", turn);
    let preview = request(&mut session, json!({
        "op": "rig.preview_deform", "root": root, "targets": [answer["parts"][3]["id"]], "parameter": head
    }));
    let pairs = result(&preview)["targets"][0]["offsets"].as_array().unwrap().iter();
    let fringe = [[18.0, 0.0], [18.0, 0.0], [-2.0, 0.0], [-2.0, 0.0]];
    assert_near(&pairs.flat_map(|pair| floats(pair)).collect::<Vec<f64>>(), &fringe.concat(), "fringe");
}

#[test]
fn the_picture_is_right_at_rest_and_keeps_its_draw_order_when_the_model_turns() {
    // At rest every colour has the area that a view from the front shows, in canvas units:
    // - red: the sleeve, 50 by 10, and the fringe, 20 by 10.
    // - green: the chest plate, 30 by 20, less the 15 by 10 the nearer sleeve covers, and the badge, 10 by 10.
    // - blue: the front of the box, 40 by 40, less the chest plate and less the 5 by 10 of sleeve beside it.
    // - yellow: the cape, 60 by 50, less the box and less the 10 by 10 of sleeve beside the box.
    let (mut session, ..) = imported();
    assert_eq!(pixels(&mut session, "rest"), [700, 550, 950, 1300]);

    // The chest bone 20 units along itself, which is up, takes the chest plate and the sleeve along. They leave
    // the box and the cape, and the sleeve still covers its 15 by 10 of the plate. The fringe stays: the standard
    // skeleton keeps what the head moves from the bones above the head.
    let (mut session, ..) = imported();
    posed(&mut session, "Chest", json!({"translation": [0.0, 20.0]}));
    assert_eq!(pixels(&mut session, "chest-up"), [700, 550, 1600, 1400]);

    // A rig that a parameter already poses moves the Parts from the import on.
    let (gltf, bin) = model();
    let path = write("model.glb", &gltf, &bin);
    let (mut driven, root) = rigged(&path);
    posed(&mut driven, "Chest", json!({"translation": [0.0, 20.0]}));
    let answer = call(&mut driven, json!({"root": root, "path": path, "unitsPerSourceUnit": UNITS, "lossy": true}));
    assert_eq!(answer["applied"], true, "{answer}");
    assert_eq!(pixels(&mut driven, ""), [700, 550, 1600, 1400]);

    // A saved document opens with the Parts, their texture, their depths and their weights.
    let saved = temp_dir().join("posed.inx").to_string_lossy().into_owned();
    result(&request(&mut session, json!({"op": "save", "path": saved})));
    let mut reopened = Session::new();
    result(&request(&mut reopened, json!({"op": "open", "path": saved})));
    assert_eq!(pixels(&mut reopened, ""), [700, 550, 1600, 1400]);

    // Half a turn of the pelvis about Y mirrors every bound vertex in x and in depth. The Parts keep their order
    // and their triangles, so the picture is the mirror of the rest picture. A view in 3D, where every bone below
    // the pelvis turns and a face with one side is not drawn from behind, shows the cape whole, 3000, and the
    // badge, which has no bone: [0, 100, 0, 3000].
    let (mut session, ..) = imported();
    posed(&mut session, "Pelvis", json!({"rotation_y": std::f64::consts::PI}));
    assert_eq!(pixels(&mut session, "half-turn"), [700, 550, 950, 1300]);
}

#[test]
fn the_parts_lie_on_the_bones_of_a_rig_root_that_is_moved_and_scaled() {
    let (gltf, bin) = model();
    let path = write("model.glb", &gltf, &bin);
    let (mut session, root) = rigged(&path);
    let moved = json!({"op": "node.transform", "id": root, "translation": [30, 40, 0], "scale": [2, 2]});
    result(&request(&mut session, moved));
    // The document root is moved as well. A Part is below it, so this move is not in the vertices.
    let top = session.snapshot()["model"]["nodes"]["uuid"].clone();
    result(&request(&mut session, json!({"op": "node.transform", "id": top, "translation": [10, 20, 0]})));
    let answer = call(&mut session, json!({"root": root, "path": path, "unitsPerSourceUnit": UNITS, "lossy": true}));
    assert_eq!(answer["applied"], true, "{answer}");
    // The badge has no bone and still goes where the rig root puts its frame: twice (-60, -30), moved by (30, 40).
    let badge = node(&session, &answer["parts"][4]["id"]);
    assert_near(&floats(&badge["mesh"]["verts"])[..2], &[-90.0, -20.0], "badge");
    // A pixel shows two canvas units, so the picture is the rest picture, moved by (40, 60).
    let bounds = [-120, -300, 200, 60];
    assert_eq!(pixels_in(&mut session, "", bounds), [700, 550, 950, 1300]);
    // The chest bone moves the same Parts by the same 20 units of the rig, about the same pivots.
    posed(&mut session, "Chest", json!({"translation": [0.0, 20.0]}));
    assert_eq!(pixels_in(&mut session, "", bounds), [700, 550, 1600, 1400]);

    // A rig root that is turned a quarter in the picture plane turns the picture with it.
    let (mut session, root) = rigged(&path);
    let turned = json!({"op": "node.transform", "id": root, "rotation": [0.0, 0.0, std::f64::consts::FRAC_PI_2]});
    result(&request(&mut session, turned));
    let answer = call(&mut session, json!({"root": root, "path": path, "unitsPerSourceUnit": UNITS, "lossy": true}));
    assert_eq!(answer["applied"], true, "{answer}");
    assert_eq!(pixels_in(&mut session, "", [15, -90, 175, 90]), [700, 550, 950, 1300]);
}

#[test]
fn a_moved_texture_a_posed_skin_and_a_weight_above_the_skeleton_are_listed() {
    let (mut gltf, mut bin) = model();
    // The cloth moves its texture coordinates.
    gltf["materials"][1]["pbrMetallicRoughness"]["baseColorTexture"]["extensions"] =
        json!({"KHR_texture_transform": {"offset": [0.5, 0.0]}});
    // The hair joint becomes the armature node, which no standard bone is above. Its inverse bind matrix stays, so
    // the skin is no longer at its bind pose. Each corner of the fringe gets as much weight from the head.
    gltf["skins"][0]["joints"][joint("Hair")] = json!(0);
    let start = |gltf: &Value, attribute: &str| {
        let accessor = gltf["meshes"][2]["primitives"][0]["attributes"][attribute].as_u64().unwrap() as usize;
        let view = gltf["accessors"][accessor]["bufferView"].as_u64().unwrap() as usize;
        gltf["bufferViews"][view]["byteOffset"].as_u64().unwrap() as usize
    };
    let (joints, weights) = (start(&gltf, "JOINTS_0"), start(&gltf, "WEIGHTS_0"));
    // The first corner of the fringe has its u outside the texture.
    let uv = start(&gltf, "TEXCOORD_0");
    bin[uv..uv + 4].copy_from_slice(&1.5f32.to_le_bytes());
    for corner in 0..4 {
        bin[joints + corner * 4 + 1] = joint("Head") as u8;
        bin[weights + corner * 16 + 4..weights + corner * 16 + 8].copy_from_slice(&1f32.to_le_bytes());
    }
    let path = write("listed.glb", &gltf, &bin);
    let (mut session, root) = rigged(&path);
    let answer = call(&mut session, json!({"root": root, "path": path, "unitsPerSourceUnit": UNITS, "lossy": true}));
    assert_eq!(answer["applied"], true, "{answer}");
    let extension = "its base colour texture has an extension, which is not read";
    assert_eq!(item(&answer, "mesh.geometry", "Cape"), outcome("dropped", extension));
    let pose = "the nodes of the skin are not at its bind pose, and its meshes are placed by its first joint";
    assert_eq!(item(&answer, "mesh.geometry", "Skin"), outcome("approximated", pose));
    let left = "the weight on a bone without a standard bone above it is left out";
    assert_eq!(item(&answer, "mesh.skinWeights", "Fringe"), outcome("approximated", left));
    let outside = "its texture coordinates leave the texture, and the sampler of the file is not read";
    assert_eq!(item(&answer, "mesh.uv", "Fringe"), outcome("approximated", outside));
    // The fringe is the third Part now, and the head has all the weight that is left.
    assert_eq!((&answer["parts"][2]["name"], &answer["parts"][2]["bones"]), (&json!("Fringe"), &json!(1)));
    let rig = node(&session, &json!(root));
    let bindings = rig["bindings"].as_array().unwrap();
    let fringe = bindings.iter().find(|binding| binding["target"] == answer["parts"][2]["id"]).unwrap();
    assert_eq!(fringe["vertexWeights"], json!([[1.0, 1.0, 1.0, 1.0]]));
}

#[test]
fn a_file_that_points_outside_itself_or_outside_its_buffers_is_refused() {
    let (gltf, bin) = model();
    let good = write("model.glb", &gltf, &bin);
    // Accessor 0 holds the inverse bind matrices. The first primitive follows: positions 1, texture coordinates
    // 2, indices 3, joints 4 and weights 5.
    // A view of `bytes` zero bytes at the end of the buffer, and its index.
    fn zeros(gltf: &mut Value, bin: &mut Vec<u8>, bytes: usize) -> usize {
        bin.resize(bin.len().div_ceil(4) * 4, 0);
        let views = gltf["bufferViews"].as_array_mut().unwrap();
        views.push(json!({"buffer": 0, "byteOffset": bin.len(), "byteLength": bytes}));
        let view = views.len() - 1;
        bin.resize(bin.len() + bytes, 0);
        gltf["buffers"][0]["byteLength"] = json!(bin.len());
        view
    }
    let cases: [(&str, &dyn Fn(&mut Value, &mut Vec<u8>), &str); 17] = [
        ("model.gltf", &|gltf, _| gltf["buffers"][0]["uri"] = json!("model.bin"),
            "a buffer or image outside the glTF file is not read"),
        ("model.gltf", &|gltf, _| gltf["buffers"][0]["uri"] = json!("https://example.invalid/model.bin"),
            "a buffer or image outside the glTF file is not read"),
        ("model.glb", &|gltf, _| gltf["accessors"][1]["count"] = json!(1_000_000),
            "an accessor is outside its buffer view"),
        ("model.glb", &|gltf, bin| gltf["bufferViews"][1]["byteLength"] = json!(bin.len() + 1),
            "a buffer view is outside its buffer"),
        ("model.glb", &|gltf, _| gltf["accessors"][2]["sparse"] = json!({"count": 1}), "sparse accessors are not read"),
        ("model.glb", &|gltf, bin| {
            let at = gltf["bufferViews"][3]["byteOffset"].as_u64().unwrap() as usize;
            bin[at..at + 2].copy_from_slice(&999u16.to_le_bytes());
        }, "a triangle list is cut short or names a vertex that does not exist"),
        ("model.glb", &|gltf, bin| {
            let at = gltf["bufferViews"][1]["byteOffset"].as_u64().unwrap() as usize;
            bin[at..at + 4].copy_from_slice(&f32::NAN.to_le_bytes());
        }, "an accessor holds a number that is not finite"),
        ("model.glb", &|gltf, _| gltf["buffers"][0]["byteLength"] = json!(1 << 30),
            "a buffer is shorter than its byteLength"),
        ("model.glb", &|gltf, _| gltf["materials"][0]["pbrMetallicRoughness"]["baseColorFactor"] = json!([2, 0, 0, 1]),
            "a base colour factor must hold 4 numbers from 0 to 1"),
        ("model.glb", &|gltf, _| {
            gltf["materials"][0]["pbrMetallicRoughness"]["baseColorFactor"] = json!([1, 1, 1, 1, 1]);
        }, "a base colour factor must hold 4 numbers from 0 to 1"),
        // A number of forty digits. No exporter writes one, and its text is not carried further.
        ("model.glb", &|gltf, _| {
            let long: Value = serde_json::from_str(&format!("0.{}1", "0".repeat(38))).unwrap();
            gltf["materials"][0]["pbrMetallicRoughness"]["baseColorFactor"] = json!([1, 1, 1, long]);
        }, "a base colour factor must hold 4 numbers from 0 to 1"),
        ("model.glb", &|gltf, _| gltf["accessors"][0]["count"] = json!(19),
            "a skin has fewer inverse bind matrices than joints"),
        // Nine images name one PNG whose header states the largest texture, 4096 by 4096. The header no longer
        // fits the picture, so none is decoded, and the ninth is more than the pixels a file may show.
        ("model.glb", &|gltf, bin| {
            let image = gltf["images"][0].clone();
            let view = image["bufferView"].as_u64().unwrap() as usize;
            let at = gltf["bufferViews"][view]["byteOffset"].as_u64().unwrap() as usize;
            for field in [16, 20] {
                bin[at + field..at + field + 4].copy_from_slice(&4096u32.to_be_bytes());
            }
            let nine = |item: &Value, change: &dyn Fn(&mut Value, usize)| -> Value {
                let items = (0..9).map(|at| {
                    let mut item = item.clone();
                    change(&mut item, at);
                    item
                });
                json!(items.collect::<Vec<Value>>())
            };
            gltf["images"] = nine(&image, &|_, _| ());
            gltf["textures"] = nine(&json!({}), &|texture, at| texture["source"] = json!(at));
            let paint = gltf["materials"][0].clone();
            let textured = |material: &mut Value, at: usize| {
                material["pbrMetallicRoughness"]["baseColorTexture"]["index"] = json!(at);
            };
            gltf["materials"] = nine(&paint, &textured);
            let badge = gltf["meshes"][3]["primitives"][0].clone();
            gltf["meshes"][3]["primitives"] = nine(&badge, &|primitive, at| primitive["material"] = json!(at));
        }, "the file has more than 134217728 pixels of images to read"),
        // The line list, which is not read, gets sixty-five attributes more.
        ("model.glb", &|gltf, _| {
            for extra in 0..65 {
                gltf["meshes"][5]["primitives"][0]["attributes"][format!("_EXTRA{extra}")] = json!(1);
            }
        }, "a primitive has more than 64 attributes"),
        // One mesh lists its first primitive 257 times.
        ("model.glb", &|gltf, _| {
            let primitive = gltf["meshes"][0]["primitives"][0].clone();
            gltf["meshes"][0]["primitives"] = json!(vec![primitive; 257]);
        }, "the file has more than 256 primitives to read"),
        // Five primitives share 60,000 positions, which no triangle of them uses.
        ("model.glb", &|gltf, bin| {
            let view = zeros(gltf, bin, 60_000 * 12);
            let accessors = gltf["accessors"].as_array_mut().unwrap();
            accessors.push(json!({"bufferView": view, "componentType": 5126, "count": 60_000, "type": "VEC3"}));
            accessors.push(json!({"bufferView": view, "componentType": 5126, "count": 60_000, "type": "VEC2"}));
            let attributes = json!({"POSITION": accessors.len() - 2, "TEXCOORD_0": accessors.len() - 1});
            let primitive = json!({"attributes": attributes, "indices": 3, "material": 0});
            gltf["meshes"][3]["primitives"] = json!(vec![primitive; 5]);
        }, "the file has more than 262140 vertices to read"),
        ("model.glb", &|gltf, bin| {
            let view = zeros(gltf, bin, 800_000 * 2);
            let accessors = gltf["accessors"].as_array_mut().unwrap();
            accessors.push(json!({"bufferView": view, "componentType": 5123, "count": 800_000, "type": "SCALAR"}));
            gltf["meshes"][0]["primitives"][0]["indices"] = json!(accessors.len() - 1);
        }, "the file has more than 786432 triangle corners to read"),
    ];
    for (file, change, message) in cases {
        let (mut gltf, mut bin) = (gltf.clone(), bin.clone());
        change(&mut gltf, &mut bin);
        let (mut session, root) = rigged(&good);
        let path = write(file, &gltf, &bin);
        if message.contains("outside the glTF file") {
            // `write` embeds the buffer of a `.gltf`: put the changed URI back.
            fs::write(&path, serde_json::to_vec(&gltf).unwrap()).unwrap();
        }
        assert_eq!(refusal(&mut session, root, &path), message, "{file}");
    }
    // A binary chunk that ends before its stated length.
    let (mut session, root) = rigged(&good);
    let whole = fs::read(&good).unwrap();
    let cut = temp_dir().join("cut.glb");
    fs::write(&cut, &whole[..whole.len() - 8]).unwrap();
    assert_eq!(refusal(&mut session, root, cut.to_str().unwrap()), "binary glTF BIN chunk is cut short");
    // Bones that another scale put somewhere else.
    let (mut session, root) = rigged(&good);
    let before = session.snapshot();
    let half = request(&mut session, json!({"op": "mesh.import_gltf", "root": root, "path": good,
        "unitsPerSourceUnit": UNITS / 2.0, "lossy": true}));
    assert_eq!(half["message"], "the bones below root are not where this file and unitsPerSourceUnit put them");
    assert_eq!(session.snapshot(), before);
    // A rig root that is turned out of the picture plane.
    let (mut session, root) = rigged(&good);
    result(&request(&mut session, json!({"op": "node.transform", "id": root, "rotation": [0.5, 0.0, 0.0]})));
    assert_eq!(
        refusal(&mut session, root, &good),
        "the DepthRigRoot is moved or turned out of the picture plane, where no mesh is put"
    );
    // A rig without the bones of the file.
    let mut session = Session::new();
    let created = request(&mut session, json!({"op": "node.create", "className": "DepthRigRoot"}));
    let root = result(&created)["created"][0]["uuid"].as_u64().unwrap();
    assert_eq!(
        refusal(&mut session, root, &good),
        "the DepthRigRoot has no bone Pelvis; rig.import_gltf adds the bones of the file"
    );
    // A rig that a parameter poses takes the meshes too. The parameter gets a deform binding for each of the 20
    // bodies, the cape and the fringe.
    let (mut gltf, bin) = model();
    let body = gltf["meshes"][0]["primitives"][0].clone();
    gltf["meshes"][0]["primitives"] = json!(vec![body; 20]);
    let many = write("bodies.glb", &gltf, &bin);
    let (mut session, root) = rigged(&good);
    let parameter = posed(&mut session, "Chest", json!({"translation": [0.0, 20.0]}));
    let answer = call(&mut session, json!({"root": root, "path": many, "unitsPerSourceUnit": UNITS, "lossy": true}));
    assert_eq!(answer["applied"], true, "{answer}");
    let model = session.snapshot()["model"].clone();
    let parameters = model["param"].as_array().unwrap().iter().filter(|item| item["uuid"] == parameter);
    let bindings = parameters.flat_map(|item| item["bindings"].as_array().unwrap());
    assert_eq!(bindings.filter(|binding| binding["param_name"] == "deform").count(), 22);
}

#[test]
fn rights_are_kept_before_a_mesh_is_read_and_an_image_that_is_no_png_drops_its_primitives() {
    let (mut gltf, bin) = model();
    let plain = write("plain.glb", &gltf, &bin);
    gltf["asset"]["copyright"] = json!("2026 Soul2D tests");
    let stated = write("stated.glb", &gltf, &bin);
    // The bones came from the file without the notice, so the document keeps no rights of this file.
    let (mut session, root) = rigged(&plain);
    let unkept = "the document does not keep the rights this file states; rig.import_gltf keeps them";
    assert_eq!(refusal(&mut session, root, &stated), unkept);
    // With the rights kept, the user still has to acknowledge them.
    let (mut session, root) = rigged(&stated);
    let arguments = json!({"root": root, "path": stated, "unitsPerSourceUnit": UNITS, "lossy": true});
    let asked = call(&mut session, arguments.clone());
    assert_eq!((&asked["applied"], &asked["needs"]), (&json!(false), &json!(["acknowledgeRights"])), "{asked}");
    assert_eq!(asked["rights"]["copyright"], "2026 Soul2D tests");
    let mut acknowledged = arguments;
    acknowledged["acknowledgeRights"] = json!(true);
    let answer = call(&mut session, acknowledged);
    assert_eq!((&answer["applied"], &answer["counts"]["kept"]), (&json!(true), &json!(1)), "{answer}");

    // The image view holds something else than a PNG: no primitive has a texture that is read, so none is a Part.
    let (gltf, mut bin) = model();
    let image = gltf["images"][0]["bufferView"].as_u64().unwrap() as usize;
    let at = gltf["bufferViews"][image]["byteOffset"].as_u64().unwrap() as usize;
    bin[at..at + 4].copy_from_slice(b"\xff\xd8\xff\xe0");
    let path = write("jpeg.glb", &gltf, &bin);
    let (mut session, root) = rigged(&path);
    let before = session.snapshot();
    let answer = call(&mut session, json!({"root": root, "path": path, "unitsPerSourceUnit": UNITS, "lossy": true}));
    // Nothing is written, so there is no undo step that would take the bones away instead.
    assert_eq!((&answer["applied"], &answer["needs"], &answer["parts"]), (&json!(false), &json!([]), &Value::Null));
    assert_eq!(session.snapshot(), before);
    let items = answer["items"].as_array().unwrap();
    let reasons: Vec<&str> = items.iter().map(|item| item["reason"].as_str().unwrap()).collect();
    assert_eq!(reasons.iter().filter(|reason| **reason == "its image is not a PNG").count(), 5, "{answer}");

    // A PNG whose header states more pixels than a texture holds is not decoded.
    let (gltf, mut bin) = model();
    for field in [16, 20] {
        bin[at + field..at + field + 4].copy_from_slice(&8192u32.to_be_bytes());
    }
    let path = write("large.glb", &gltf, &bin);
    let (mut session, root) = rigged(&path);
    let answer = call(&mut session, json!({"root": root, "path": path, "unitsPerSourceUnit": UNITS, "lossy": true}));
    let large = "its image is empty or larger than a texture can be";
    let items = answer["items"].as_array().unwrap();
    assert_eq!(items.iter().filter(|item| item["reason"] == large).count(), 5, "{answer}");

    // A PNG whose first chunk is no header states no size, and is listed like one that does not decode.
    let (gltf, mut bin) = model();
    bin[at + 12] = b'X';
    let path = write("headless.glb", &gltf, &bin);
    let (mut session, root) = rigged(&path);
    let answer = call(&mut session, json!({"root": root, "path": path, "unitsPerSourceUnit": UNITS, "lossy": true}));
    let undecoded = "its image does not decode within the limits of a texture";
    let items = answer["items"].as_array().unwrap();
    assert_eq!(items.iter().filter(|item| item["reason"] == undecoded).count(), 5, "{answer}");

    // An image beside the file is not read either, and one in a data URI is.
    let (mut gltf, bin) = model();
    let view = &gltf["bufferViews"][image];
    let (at, length) = (view["byteOffset"].as_u64().unwrap() as usize, view["byteLength"].as_u64().unwrap() as usize);
    let data = base64::engine::general_purpose::STANDARD.encode(&bin[at..at + length]);
    for (uri, parts, beside) in [("palette.png".to_owned(), 0, 5), (format!("data:image/png;base64,{data}"), 5, 0)] {
        gltf["images"][0] = json!({"uri": uri});
        let path = write("image.glb", &gltf, &bin);
        let (mut session, root) = rigged(&path);
        let arguments = json!({"root": root, "path": path, "unitsPerSourceUnit": UNITS, "lossy": true});
        let answer = call(&mut session, arguments);
        assert_eq!(answer["parts"].as_array().map_or(0, Vec::len), parts, "{answer}");
        let items = answer["items"].as_array().unwrap().iter();
        let outside = items.filter(|item| item["reason"] == "a buffer or image outside the glTF file is not read");
        assert_eq!(outside.count(), beside, "{answer}");
    }
}

#[test]
fn a_base_colour_factor_becomes_tint_and_opacity_and_a_mesh_over_the_vertex_limit_is_listed() {
    let (mut gltf, mut bin) = model();
    // The factor is linear and a tint is in the encoding of the texture: 0.214041 is the middle grey 0.5.
    gltf["materials"][1]["pbrMetallicRoughness"]["baseColorFactor"] = json!([0.214_041, 1.0, 0.0, 0.25]);
    gltf["materials"][1]["alphaMode"] = json!("BLEND");
    // The paint states no alpha mode, so it is opaque and the alpha of its factor does not count.
    gltf["materials"][0]["pbrMetallicRoughness"]["baseColorFactor"] = json!([1.0, 1.0, 1.0, 0.5]);
    // The positions of the last mesh become 65,536 points, one more than a Part mesh holds.
    let position = gltf["meshes"][6]["primitives"][0]["attributes"]["POSITION"].as_u64().unwrap() as usize;
    bin.resize(bin.len().div_ceil(4) * 4, 0);
    let views = gltf["bufferViews"].as_array_mut().unwrap();
    views.push(json!({"buffer": 0, "byteOffset": bin.len(), "byteLength": 65_536 * 12}));
    gltf["accessors"][position] = json!({"bufferView": views.len() - 1, "componentType": 5126, "count": 65_536,
        "type": "VEC3"});
    bin.resize(bin.len() + 65_536 * 12, 0);
    gltf["buffers"][0]["byteLength"] = json!(bin.len());
    gltf["meshes"][6]["primitives"][0]["material"] = json!(0);
    // The back and the lines share that accessor, so their nodes are taken out.
    gltf["nodes"].as_array_mut().unwrap().retain(|node| node["name"] != "BackNode" && node["name"] != "LinesNode");
    // A name of the file is cut where it is read, and leaves room for what follows it in a report item.
    gltf["meshes"][3]["name"] = json!("B".repeat(100));
    // The first corner of the fringe loses its weights.
    let weights = gltf["meshes"][2]["primitives"][0]["attributes"]["WEIGHTS_0"].as_u64().unwrap() as usize;
    let view = gltf["accessors"][weights]["bufferView"].as_u64().unwrap() as usize;
    let at = gltf["bufferViews"][view]["byteOffset"].as_u64().unwrap() as usize;
    bin[at..at + 16].fill(0);
    let path = write("factor.glb", &gltf, &bin);
    let (mut session, root) = rigged(&path);
    let answer = call(&mut session, json!({"root": root, "path": path, "unitsPerSourceUnit": UNITS, "lossy": true}));
    assert_eq!(answer["applied"], true, "{answer}");
    let cape = node(&session, &answer["parts"][2]["id"]);
    assert_near(&floats(&cape["tint"]), &[0.5, 1.0, 0.0], "tint");
    assert_eq!(cape["opacity"], json!(0.25));
    assert_eq!(node(&session, &answer["parts"][0]["id"])["opacity"], json!(1.0));
    let items = answer["items"].as_array().unwrap();
    let huge = items.iter().find(|item| item["source"] == "Bare").unwrap();
    assert_eq!((&huge["outcome"], &huge["reason"]), (&json!("dropped"), &json!("it has more than 65535 vertices")));
    // A Part whose skin leaves a vertex without a bone is imported unbound, and the report says so.
    let unbound =
        items.iter().find(|item| item["feature"] == "mesh.skinWeights" && item["source"] == "Fringe").unwrap();
    let reason = "a vertex has no weight on a standard bone or on a bone below one";
    assert_eq!((&unbound["outcome"], &unbound["reason"]), (&json!("dropped"), &json!(reason)));
    assert_eq!((&answer["parts"][3]["name"], &answer["parts"][3]["bones"]), (&json!("Fringe"), &json!(0)));
    assert_eq!(answer["parts"][4]["name"], json!("B".repeat(48)));
}

#[test]
fn a_file_export_gltf_wrote_is_read_back_with_the_same_picture() {
    // The imported model is written as layered planes and read into a second document. The export writes each
    // Part at its rest shape, one layer apart, without a skin.
    let (mut session, ..) = imported();
    let exported = temp_dir().join("layers.glb");
    let export = request(&mut session, json!({
        "op": "export.gltf", "path": exported.to_str().unwrap(), "unitsPerMetre": UNITS, "lossy": true
    }));
    assert_eq!(result(&export)["applied"], true, "{export}");
    let path = exported.to_str().unwrap();
    let (mut second, root) = rigged(path);
    let answer = call(&mut second, json!({"root": root, "path": path, "unitsPerSourceUnit": UNITS, "lossy": true}));
    assert_eq!(answer["applied"], true, "{answer}");
    let parts = answer["parts"].as_array().unwrap();
    assert_eq!(parts.len(), 5, "{answer}");
    assert!(parts.iter().all(|part| part["bones"] == 0), "{answer}");
    assert_eq!(pixels(&mut second, ""), [700, 550, 950, 1300]);
}
