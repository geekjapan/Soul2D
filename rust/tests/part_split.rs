//! `split.*`: a flattened layered "character" is split into Parts by prompts, applied in one undo step and replayed
//! from its plan. The fixture is generated here, so the ground truth is the visible region of each original layer.

mod common;

use base64::Engine;
use serde_json::{json, Value};
use soul2d_core::Session;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const W: u32 = 96;
const H: u32 = 128;
const HAIR: [u8; 4] = [90, 50, 20, 255];
const OUTLINE: [u8; 4] = [20, 20, 20, 255];

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "soul2d-part-split-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    common::scratch(dir)
}

// ------------------------------------------------------------------------------------------------ fixture

/// One original layer: where it paints and in what colour.
struct Layer {
    part: &'static str,
    colour: fn(u32, u32) -> Option<[u8; 4]>,
}

fn rect(x: u32, y: u32, w: u32, h: u32, colour: [u8; 4]) -> impl Fn(u32, u32) -> Option<[u8; 4]> {
    move |px, py| ((x..x + w).contains(&px) && (y..y + h).contains(&py)).then_some(colour)
}

/// Back to front. The hair is one colour across the symmetry axis x=48, the fringe is hair-coloured inside a dark
/// outline, and the two eyes sit on the face.
fn layers() -> Vec<Layer> {
    vec![
        Layer { part: "HairBack.L", colour: |x, y| rect(24, 10, 24, 60, HAIR)(x, y) },
        Layer { part: "HairBack.R", colour: |x, y| rect(48, 10, 24, 60, HAIR)(x, y) },
        Layer { part: "Chest", colour: |x, y| rect(34, 66, 28, 54, [40, 80, 200, 255])(x, y) },
        Layer {
            part: "Head",
            colour: |x, y| {
                let (dx, dy) = (f64::from(x) + 0.5 - 48.0, f64::from(y) + 0.5 - 40.0);
                (dx * dx + dy * dy <= 22.0 * 22.0).then_some([240, 200, 170, 255])
            },
        },
        Layer { part: "Eye.L", colour: |x, y| rect(38, 36, 6, 6, [60, 30, 30, 255])(x, y) },
        Layer { part: "Eye.R", colour: |x, y| rect(52, 36, 6, 6, [60, 30, 30, 255])(x, y) },
        Layer {
            part: "Fringe",
            colour: |x, y| {
                rect(30, 16, 36, 14, OUTLINE)(x, y).map(|outline| {
                    if (32..64).contains(&x) && (18..28).contains(&y) { HAIR } else { outline }
                })
            },
        },
    ]
}

/// The flattened picture (straight alpha) and, per pixel, the index of the topmost layer that paints it.
fn flatten() -> (Vec<u8>, Vec<Option<usize>>) {
    let layers = layers();
    let mut rgba = vec![0u8; (W * H * 4) as usize];
    let mut owner = vec![None; (W * H) as usize];
    for y in 0..H {
        for x in 0..W {
            for (index, layer) in layers.iter().enumerate() {
                if let Some(colour) = (layer.colour)(x, y) {
                    let at = (y * W + x) as usize;
                    rgba[at * 4..at * 4 + 4].copy_from_slice(&colour);
                    owner[at] = Some(index);
                }
            }
        }
    }
    (rgba, owner)
}

fn write_source(dir: &Path) -> String {
    let (rgba, _) = flatten();
    let path = dir.join("character.png");
    image::RgbaImage::from_raw(W, H, rgba).unwrap().save(&path).unwrap();
    path.to_str().unwrap().to_owned()
}

// -------------------------------------------------------------------------------------------------- helpers

fn run(session: &mut Session, op: &str, body: Value) -> Value {
    let mut request = body;
    request["op"] = json!(op);
    session.handle_request(&request)
}

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

fn set(session: &mut Session, name: &str, steps: Value) -> Value {
    ok(session, "split.part.set", json!({"name": name, "steps": steps}))
}

/// The prompts an agent would give for the fixture.
fn prompt_all(session: &mut Session) {
    set(session, "HairBack.L", json!([{"op": "flood", "point": [26, 50], "tolerance": 8}, {"op": "side", "axisX": 48, "keep": "left"}]));
    set(session, "HairBack.R", json!([{"op": "flood", "point": [70, 50], "tolerance": 8}, {"op": "side", "axisX": 48, "keep": "right"}]));
    set(session, "Chest", json!([{"op": "flood", "point": [48, 100], "tolerance": 0}]));
    set(session, "Head", json!([{"op": "flood", "point": [48, 60], "tolerance": 8}]));
    set(session, "Eye.L", json!([{"op": "flood", "point": [40, 38], "tolerance": 0}]));
    set(session, "Eye.R", json!([{"op": "flood", "point": [54, 38], "tolerance": 0}]));
    // The outline is line art: fill the hair-coloured inside, then grow over the outline and clip to the box.
    set(session, "Fringe", json!([
        {"op": "boundedFill", "point": [48, 22], "lineThreshold": 40},
        {"op": "grow", "radius": 3},
        {"op": "box", "rect": [30, 16, 36, 14], "mode": "intersect"},
    ]));
}

fn history_count(session: &Session) -> u64 {
    session.snapshot()["history"]["count"].as_u64().unwrap()
}

/// `(name, id, slot, trans)` of the Parts under the root in draw order.
fn parts_of(snapshot: &Value) -> Vec<(String, u64, u64, [f64; 2])> {
    snapshot["model"]["nodes"]["children"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|node| node["type"] == "Part")
        .map(|node| {
            let trans = &node["transform"]["trans"];
            (
                node["name"].as_str().unwrap().to_owned(),
                node["uuid"].as_u64().unwrap(),
                node["textures"][0].as_u64().unwrap(),
                [trans[0].as_f64().unwrap(), trans[1].as_f64().unwrap()],
            )
        })
        .collect()
}

fn texture(session: &mut Session, slot: u64) -> (Vec<u8>, image::RgbaImage) {
    let result = ok(session, "texture.get", json!({"slot": slot}));
    let bytes = base64::engine::general_purpose::STANDARD.decode(result["base64"].as_str().unwrap()).unwrap();
    let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();
    (bytes, decoded)
}

/// The applied Parts placed back on a source-sized canvas: per Part its texture and its top-left in source pixels.
fn placed(session: &mut Session) -> Vec<(String, image::RgbaImage, i64, i64)> {
    let snapshot = session.snapshot();
    parts_of(&snapshot)
        .into_iter()
        .map(|(name, _, slot, trans)| {
            let (_, image) = texture(session, slot);
            let (tw, th) = (i64::from(image.width()), i64::from(image.height()));
            let left = trans[0] as i64 + i64::from(W / 2) - tw / 2;
            let top = trans[1] as i64 + i64::from(H / 2) - th / 2;
            (name, image, left, top)
        })
        .collect()
}

fn texture_alpha_set(image: &image::RgbaImage, left: i64, top: i64) -> Vec<bool> {
    let mut set = vec![false; (W * H) as usize];
    for (x, y, pixel) in image.enumerate_pixels() {
        let (sx, sy) = (left + i64::from(x), top + i64::from(y));
        if pixel[3] > 0 && (0..i64::from(W)).contains(&sx) && (0..i64::from(H)).contains(&sy) {
            set[(sy as usize) * W as usize + sx as usize] = true;
        }
    }
    set
}

fn iou(a: &[bool], b: &[bool]) -> f64 {
    let both = a.iter().zip(b).filter(|(x, y)| **x && **y).count();
    let either = a.iter().zip(b).filter(|(x, y)| **x || **y).count();
    both as f64 / either as f64
}

/// Source-over of the placed textures in draw order, premultiplied, on a transparent canvas.
fn stack(parts: &[(String, image::RgbaImage, i64, i64)]) -> Vec<[u8; 4]> {
    let mut canvas = vec![[0u8; 4]; (W * H) as usize];
    for (_, image, left, top) in parts {
        for (x, y, pixel) in image.enumerate_pixels() {
            let (sx, sy) = (left + i64::from(x), top + i64::from(y));
            if !(0..i64::from(W)).contains(&sx) || !(0..i64::from(H)).contains(&sy) {
                continue;
            }
            let below = &mut canvas[(sy as usize) * W as usize + sx as usize];
            let keep = 255 - u32::from(pixel[3]);
            for c in 0..4 {
                below[c] = (u32::from(pixel[c]) + u32::from(below[c]) * keep / 255) as u8;
            }
        }
    }
    canvas
}

fn premultiplied_source() -> Vec<[u8; 4]> {
    let (rgba, _) = flatten();
    rgba.chunks_exact(4)
        .map(|p| {
            let a = u32::from(p[3]);
            [(u32::from(p[0]) * a / 255) as u8, (u32::from(p[1]) * a / 255) as u8, (u32::from(p[2]) * a / 255) as u8, p[3]]
        })
        .collect()
}

/// The document rendered by the session's CPU renderer at source scale (integer halves, like the SKILL says).
fn render_document(session: &mut Session) -> Vec<[u8; 4]> {
    let bounds = [-f64::from(W / 2), -f64::from(H / 2), f64::from(W - W / 2), f64::from(H - H / 2)];
    let result = ok(session, "render", json!({"width": W, "height": H, "bounds": bounds}));
    let bytes = base64::engine::general_purpose::STANDARD.decode(result["png"].as_str().unwrap()).unwrap();
    image::load_from_memory(&bytes).unwrap().to_rgba8().pixels().map(|p| p.0).collect()
}

fn session_with_draft() -> (Session, String) {
    let dir = temp_dir();
    let path = write_source(&dir);
    let mut session = Session::new();
    let opened = ok(&mut session, "split.open", json!({"path": path}));
    assert_eq!((opened["width"].clone(), opened["height"].clone()), (json!(W), json!(H)));
    (session, path)
}

// ---------------------------------------------------------------------------------------------------- tests

#[test]
fn prompts_split_the_character_into_parts_that_match_the_visible_layers_and_stack_back() {
    let (mut session, _) = session_with_draft();
    prompt_all(&mut session);
    let plan = ok(&mut session, "split.plan", json!({}));
    assert_eq!(plan["unclaimedPixels"], 0, "{plan}");
    assert_eq!(plan["multiplyClaimedPixels"], 0, "{plan}");
    assert_eq!(plan["parts"].as_array().unwrap().len(), 7);
    assert_eq!(plan["parts"][4]["name"], "Eye.L");
    assert_eq!(plan["parts"][4]["pixels"], 36);
    assert_eq!(plan["parts"][4]["bbox"], json!([38, 36, 6, 6]));
    assert_eq!(plan["plan"]["version"], 1);
    assert_eq!(plan["plan"]["source"]["width"], W);

    let result = ok(&mut session, "split.apply", json!({}));
    assert_eq!(result["created"].as_array().unwrap().len(), 7);

    let (_, owner) = flatten();
    let names: Vec<&str> = layers().iter().map(|layer| layer.part).collect();
    let applied = placed(&mut session);
    assert_eq!(applied.iter().map(|part| part.0.as_str()).collect::<Vec<_>>(), names, "draw order is back to front");
    for (index, (name, image, left, top)) in applied.iter().enumerate() {
        let truth: Vec<bool> = owner.iter().map(|o| *o == Some(index)).collect();
        let score = iou(&texture_alpha_set(image, *left, *top), &truth);
        assert!(score >= 0.9, "{name}: IoU {score}");
        assert_eq!(score, 1.0, "{name}: the fixture has hard edges, so every part is exact");
        assert_eq!((image.width() % 2, image.height() % 2), (0, 0), "{name}: even sizes keep the quad exact");
    }

    // Disjoint opaque masks and no padding: the stack equals the premultiplied source pixel for pixel.
    assert_eq!(stack(&applied), premultiplied_source());

    // The offsets are checked against the renderer, not against the formula that wrote them: the document drawn
    // through the session's CPU render op shows the source picture.
    let (source, _) = flatten();
    let rendered = render_document(&mut session);
    for (index, pixel) in rendered.iter().enumerate() {
        let expected = &source[index * 4..index * 4 + 4];
        if expected[3] == 0 {
            assert_eq!(pixel[3], 0, "pixel {index} should be empty");
        } else {
            let close = (0..3).all(|c| pixel[c].abs_diff(expected[c]) <= 1);
            assert!(close && pixel[3] == 255, "pixel {index}: rendered {pixel:?}, source {expected:?}");
        }
    }
}

#[test]
fn a_part_can_be_built_from_other_parts_and_the_draw_order_decides_what_shows() {
    let (mut session, _) = session_with_draft();
    prompt_all(&mut session);
    // The face under the eyes: its mask holds the eyes too, so they are claimed twice.
    set(&mut session, "Head", json!([
        {"op": "flood", "point": [48, 60], "tolerance": 8},
        {"op": "part", "name": "Eye.L"},
        {"op": "part", "name": "Eye.R"},
    ]));
    let plan = ok(&mut session, "split.plan", json!({}));
    assert_eq!(plan["multiplyClaimedPixels"], 72);
    assert_eq!(plan["parts"][3]["overlaps"], json!([{"part": "Eye.L", "pixels": 36}, {"part": "Eye.R", "pixels": 36}]));
    assert_eq!(plan["unclaimedPixels"], 0);

    // The eyes come before the head in the plan order, so the head covers them until the order is fixed.
    ok(&mut session, "split.part.order", json!({"names": ["HairBack.L", "HairBack.R", "Chest", "Head", "Eye.L", "Eye.R", "Fringe"]}));
    ok(&mut session, "split.apply", json!({}));
    assert_eq!(stack(&placed(&mut session)), premultiplied_source());

    // A referenced part cannot be removed.
    fails(&mut session, "split.part.remove", json!({"name": "Eye.L"}), "referenced by part \"Head\"");
}

#[test]
fn draft_operations_leave_the_document_alone_and_apply_is_one_undo_step() {
    let (mut session, _) = session_with_draft();
    let before = session.snapshot();
    prompt_all(&mut session);
    ok(&mut session, "split.part.order", json!({"names": ["Fringe", "Eye.R", "Eye.L", "Head", "Chest", "HairBack.R", "HairBack.L"]}));
    ok(&mut session, "split.plan", json!({}));
    let overlay = ok(&mut session, "split.overlay", json!({}));
    assert!(overlay["png"].as_str().unwrap().len() > 100);
    ok(&mut session, "split.part.remove", json!({"name": "Fringe"}));
    set(&mut session, "Fringe", json!([{"op": "box", "rect": [30, 16, 36, 14]}]));
    let draft = session.snapshot();
    for key in ["model", "revision", "history", "modified", "canUndo", "canRedo"] {
        assert_eq!(draft[key], before[key], "{key}");
    }
    assert_eq!(draft["partSplit"]["parts"], 7);
    assert_eq!(draft["partSplit"]["open"], true);
    assert_eq!(draft["modified"], false);
    ok(&mut session, "split.part.order", json!({"names": ["HairBack.L", "HairBack.R", "Chest", "Head", "Eye.L", "Eye.R", "Fringe"]}));

    let steps = history_count(&session);
    ok(&mut session, "split.apply", json!({}));
    let applied = session.snapshot();
    assert_eq!(history_count(&session), steps + 1, "one undo step");
    assert_eq!(applied["history"]["states"][steps as usize + 1]["label"], "split.apply");
    assert_eq!(parts_of(&applied).len(), 7);
    assert_eq!(applied["modified"], true);
    assert_eq!(applied["partSplit"]["parts"], 7, "apply keeps the draft");

    let undone = ok(&mut session, "undo", json!({}));
    assert_eq!(undone["model"], before["model"], "one undo removes every created Part");
    assert!(parts_of(&undone).is_empty());
    assert_eq!(undone["modified"], false);
    let redone = ok(&mut session, "redo", json!({}));
    assert_eq!(redone["model"], applied["model"], "one redo restores them all");
    assert_eq!(placed(&mut session).len(), 7);
}

#[test]
fn a_saved_plan_replays_in_a_fresh_session_to_the_same_parts() {
    let (mut session, _) = session_with_draft();
    prompt_all(&mut session);
    let mut padded = ok(&mut session, "split.plan", json!({}))["plan"].clone();
    // Give two parts padding so the replay covers it; the plan goes through text like a saved file.
    padded["parts"][3]["padding"] = json!(3);
    padded["parts"][5]["padding"] = json!(1);
    let saved = padded.to_string();
    ok(&mut session, "split.apply", json!({"plan": serde_json::from_str::<Value>(&saved).unwrap()}));
    let first = session.snapshot();

    let mut fresh = Session::new();
    assert_eq!(fresh.snapshot()["partSplit"]["open"], false);
    ok(&mut fresh, "split.apply", json!({"plan": serde_json::from_str::<Value>(&saved).unwrap()}));
    let second = fresh.snapshot();
    assert_eq!(fresh.snapshot()["partSplit"]["open"], false, "a plan replays without a draft");

    let (a, b) = (parts_of(&first), parts_of(&second));
    assert_eq!(a, b, "names, ids, slots and offsets");
    assert_eq!(a.len(), 7);
    for (part_a, part_b) in a.iter().zip(&b) {
        let (bytes_a, image_a) = texture(&mut session, part_a.2);
        let (bytes_b, _) = texture(&mut fresh, part_b.2);
        assert_eq!(bytes_a, bytes_b, "{}: texture bytes", part_a.0);
        assert_eq!(image_a.width() % 2, 0);
    }
    assert_eq!(first["model"], second["model"]);

    // Applying the draft gives the same Parts as the plan it produced (no padding).
    let mut direct = Session::new();
    let dir = temp_dir();
    let source = write_source(&dir);
    ok(&mut direct, "split.open", json!({"path": source}));
    prompt_all(&mut direct);
    ok(&mut direct, "split.apply", json!({}));
    let plan = ok(&mut direct, "split.plan", json!({}))["plan"].clone();
    let mut replay = Session::new();
    ok(&mut replay, "split.apply", json!({"plan": plan}));
    assert_eq!(direct.snapshot()["model"], replay.snapshot()["model"]);
}

#[test]
fn padding_is_transparent_and_adds_no_light_when_rendered() {
    // Chest overlaps the back hair by its padding ring; the ring must not tint the hair. Colour written into
    // alpha-0 texels of a premultiplied texture is added by the renderer, which is the halo this guards against.
    let render_with = |padding: u32| {
        let (mut session, _) = session_with_draft();
        prompt_all(&mut session);
        let steps = json!([{"op": "flood", "point": [48, 100], "tolerance": 0}]);
        ok(&mut session, "split.part.set", json!({"name": "Chest", "steps": steps, "padding": padding}));
        ok(&mut session, "split.apply", json!({}));
        let chest = placed(&mut session).into_iter().find(|part| part.0 == "Chest").unwrap();
        (render_document(&mut session), chest)
    };
    let (plain, _) = render_with(0);
    let (padded, (_, image, left, top)) = render_with(4);
    assert_eq!((image.width(), image.height()), (28 + 8, 54 + 8));
    assert_eq!(image.get_pixel(0, 0).0, [0, 0, 0, 0], "the padding corner is empty");
    assert_eq!(image.get_pixel(4, 4).0, [40, 80, 200, 255]);
    assert_eq!(image.pixels().filter(|pixel| pixel[3] > 0).count(), 28 * 54);
    assert_eq!(texture_alpha_set(&image, left, top).iter().filter(|set| **set).count(), 28 * 54);
    let differing: Vec<(usize, [u8; 4], [u8; 4])> =
        (0..plain.len()).filter(|&i| plain[i] != padded[i]).map(|i| (i, plain[i], padded[i])).collect();
    assert!(differing.is_empty(), "{} pixels differ, first: {:?}", differing.len(), &differing[..differing.len().min(3)]);
}

#[test]
fn apply_can_mesh_the_new_parts_in_the_same_undo_step() {
    let (mut session, _) = session_with_draft();
    prompt_all(&mut session);
    let steps = history_count(&session);
    let result = ok(&mut session, "split.apply", json!({"automesh": true}));
    // The default contour algorithm gives no mesh to the 6x6 eyes and the thin fringe; they keep their quad.
    assert_eq!(result["automesh"], json!({"meshed": 4, "skipped": [6, 7, 8]}), "{result}");
    assert_eq!(history_count(&session), steps + 1);
    let snapshot = session.snapshot();
    let child = |name: &str| snapshot["model"]["nodes"]["children"].as_array().unwrap().iter().find(|n| n["name"] == name).unwrap().clone();
    assert!(child("Chest")["mesh"]["verts"].as_array().unwrap().len() > 8, "{}", child("Chest")["mesh"]);
    assert_eq!(child("Eye.L")["mesh"]["verts"].as_array().unwrap().len(), 8);
    let undone = ok(&mut session, "undo", json!({}));
    assert!(parts_of(&undone).is_empty());

    // Any automesh.generate request works; the grid algorithm meshes every part.
    ok(&mut session, "redo", json!({}));
    ok(&mut session, "undo", json!({}));
    let result = ok(&mut session, "split.apply", json!({"automesh": {"algorithm": "grid"}}));
    assert_eq!(result["automesh"], json!({"meshed": 7, "skipped": []}), "{result}");
    fails(&mut session, "split.apply", json!({"automesh": {"algorithm": "voronoi"}}), "automesh failed");
    fails(&mut session, "split.apply", json!({"automesh": 3}), "automesh must be");
}

#[test]
fn apply_meshes_with_a_preset_the_user_saved() {
    let (mut session, _) = session_with_draft();
    prompt_all(&mut session);
    session.set_config_dir(temp_dir().join("settings"));
    ok(&mut session, "automesh.preset.save", json!({"name": "Thin", "algorithm": "contour", "sampling_step": 8, "min_distance": 4}));
    // The contour default meshes 4 parts and skips 6, 7 and 8. Without an algorithm the request means contour.
    let result = ok(&mut session, "split.apply", json!({"automesh": {"preset": "Thin"}}));
    assert_eq!(result["automesh"], json!({"meshed": 5, "skipped": [6, 7]}), "{result}");
    ok(&mut session, "undo", json!({}));
    let result = ok(&mut session, "split.apply", json!({"automesh": {"algorithm": "contour", "preset": "Thin", "sampling_step": 50, "min_distance": 16}}));
    assert_eq!(result["automesh"], json!({"meshed": 4, "skipped": [6, 7, 8]}), "{result}");
    ok(&mut session, "undo", json!({}));
    fails(&mut session, "split.apply", json!({"automesh": {"algorithm": "optimum", "preset": "Thin"}}), "unknown optimum preset \"Thin\"");
    fails(&mut session, "split.apply", json!({"automesh": {"preset": "Thick"}}), "unknown contour preset \"Thick\"");
}

#[test]
fn the_overlay_is_a_png_of_the_source_size() {
    let (mut session, _) = session_with_draft();
    prompt_all(&mut session);
    for body in [json!({}), json!({"part": "Head"})] {
        let overlay = ok(&mut session, "split.overlay", body);
        let bytes = base64::engine::general_purpose::STANDARD.decode(overlay["png"].as_str().unwrap()).unwrap();
        let image = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!((image.width(), image.height()), (W, H));
    }
    fails(&mut session, "split.overlay", json!({"part": "Nobody"}), "was not found");
}

#[test]
fn bad_input_is_refused_without_changing_the_session() {
    let mut session = Session::new();
    // Nothing is open yet.
    fails(&mut session, "split.part.set", json!({"name": "A", "steps": [{"op": "alpha"}]}), "no source is open");
    fails(&mut session, "split.plan", json!({}), "no source is open");
    fails(&mut session, "split.apply", json!({}), "no source is open");
    fails(&mut session, "split.open", json!({"path": "/nonexistent/character.png"}), "Could not load character.png");

    let (mut session, path) = session_with_draft();
    fails(&mut session, "split.apply", json!({}), "no parts");
    fails(&mut session, "split.part.set", json!({"name": "A", "steps": [{"op": "flood", "point": [W, 0]}]}), "outside");
    fails(&mut session, "split.part.set", json!({"name": "A", "steps": [{"op": "flood", "point": [0, H]}]}), "outside");
    fails(&mut session, "split.part.set", json!({"name": "A", "steps": [{"op": "box", "rect": [90, 0, 10, 10]}]}), "outside");
    fails(&mut session, "split.part.set", json!({"name": "A", "steps": [{"op": "grow", "radius": 0}]}), "radius");
    fails(&mut session, "split.part.set", json!({"name": "A", "steps": [{"op": "alpha", "thresh": 1}]}), "unknown field");
    fails(&mut session, "split.part.set", json!({"name": "A", "steps": [{"op": "magic"}]}), "unknown variant");
    fails(&mut session, "split.part.set", json!({"name": "A", "steps": []}), "steps must hold");
    fails(&mut session, "split.part.set", json!({"name": "", "steps": [{"op": "alpha"}]}), "name must be");
    fails(&mut session, "split.part.set", json!({"name": "A", "steps": [{"op": "part", "name": "Nobody"}]}), "unknown part");
    fails(&mut session, "split.part.set", json!({"name": "A", "steps": [{"op": "part", "name": "A"}]}), "cycle");
    fails(&mut session, "split.part.set", json!({"name": "A", "steps": [{"op": "alpha"}], "padding": 65}), "padding");
    fails(&mut session, "split.part.set", json!({"name": "A", "steps": [{"op": "alpha"}], "index": 3}), "index");

    set(&mut session, "A", json!([{"op": "alpha"}]));
    set(&mut session, "B", json!([{"op": "part", "name": "A"}]));
    // Closing a cycle through an existing part is refused too.
    fails(&mut session, "split.part.set", json!({"name": "A", "steps": [{"op": "part", "name": "B"}]}), "cycle");
    fails(&mut session, "split.part.order", json!({"names": ["A", "A"]}), "twice");
    fails(&mut session, "split.part.order", json!({"names": ["A"]}), "all 2 parts");
    fails(&mut session, "split.part.remove", json!({"name": "A"}), "referenced");
    fails(&mut session, "split.part.remove", json!({"name": "Z"}), "was not found");

    // A plan is validated like the draft, and must match its source.
    let plan = ok(&mut session, "split.plan", json!({}))["plan"].clone();
    let mut twice = plan.clone();
    twice["parts"] = json!([twice["parts"][0].clone(), twice["parts"][0].clone()]);
    fails(&mut session, "split.apply", json!({"plan": twice}), "used twice");
    let mut resized = plan.clone();
    resized["source"]["width"] = json!(W + 1);
    fails(&mut session, "split.apply", json!({"plan": resized}), "plan recorded");
    let mut future = plan.clone();
    future["version"] = json!(2);
    fails(&mut session, "split.apply", json!({"plan": future}), "plan version");
    let mut missing = plan.clone();
    missing["source"]["path"] = json!("/nonexistent/other.png");
    fails(&mut session, "split.apply", json!({"plan": missing}), "Could not load other.png");
    fails(&mut session, "split.apply", json!({"plan": {"version": 1}}), "plan:");

    // A part that selects nothing is allowed in the draft but refused at apply, and nothing is created.
    set(&mut session, "Empty", json!([{"op": "component", "point": [0, 0]}]));
    let plan = ok(&mut session, "split.plan", json!({}));
    assert_eq!(plan["parts"][2]["pixels"], 0);
    assert_eq!(plan["parts"][2]["bbox"], Value::Null);
    fails(&mut session, "split.apply", json!({}), "selects no visible pixels");
    // The path in an error is the one the caller gave; the error does not add directories.
    let directory = Path::new(&path).parent().unwrap().to_str().unwrap().to_owned();
    let missing = format!("{directory}/missing.png");
    let message = run(&mut session, "split.open", json!({"path": missing}))["message"].as_str().unwrap().to_owned();
    assert!(message.contains("Could not load missing.png"), "{message}");
    assert!(!message.contains(&directory) && !message.contains('/'), "{message}");
}

#[test]
fn an_oversized_plan_fails_with_the_work_limit_and_leaves_the_draft_unchanged() {
    let (mut session, _) = session_with_draft();
    // Each alpha step visits every pixel twice: 64 steps x 24576 per part, and the limit is 512 visits per pixel,
    // which is 256 such steps for this 96x128 picture. The statistics and texture checks are charged too, so
    // three parts fit and the fourth, which alone would use the whole limit, does not.
    let steps: Vec<Value> = (0..64).map(|_| json!({"op": "alpha"})).collect();
    for n in 0..3 {
        set(&mut session, &format!("P{n}"), json!(steps));
    }
    fails(&mut session, "split.part.set", json!({"name": "P3", "steps": steps}), "split work limit");
    assert_eq!(session.snapshot()["partSplit"]["parts"], 3);
    // The reads evaluate the same plan and stay within the limit.
    assert_eq!(ok(&mut session, "split.plan", json!({}))["parts"].as_array().unwrap().len(), 3);
}

#[test]
fn many_overlapping_parts_are_refused_by_the_work_limit_and_leave_the_draft_unchanged() {
    let (mut session, _) = session_with_draft();
    // Full-canvas parts: every pixel is claimed by all of them, so the pair counting is quadratic in the part count.
    // Per pixel one set costs 8 N + N (N - 1) / 2 visits: 500 for N = 25 (within 512), 533 for N = 26.
    let canvas = json!([{"op": "box", "rect": [0, 0, W, H]}]);
    for n in 0..25 {
        set(&mut session, &format!("P{n}"), canvas.clone());
    }
    fails(&mut session, "split.part.set", json!({"name": "P25", "steps": canvas}), "split work limit");
    assert_eq!(session.snapshot()["partSplit"]["parts"], 25);
    let plan = ok(&mut session, "split.plan", json!({}));
    assert_eq!(plan["multiplyClaimedPixels"], W * H);
    assert_eq!(plan["parts"][0]["overlaps"].as_array().unwrap().len(), 24);
}

#[test]
fn a_part_that_refers_to_another_cannot_push_the_draft_past_the_texture_limit() {
    // 8192 wide: A full-width is a legal texture (8192), B = A with padding 1 is not (8194).
    let dir = temp_dir();
    let path = dir.join("wide.png");
    image::RgbaImage::from_pixel(8192, 4, image::Rgba([10, 20, 30, 255])).save(&path).unwrap();
    let mut session = Session::new();
    ok(&mut session, "split.open", json!({"path": path.to_str().unwrap()}));
    set(&mut session, "A", json!([{"op": "box", "rect": [0, 0, 10, 4]}]));
    ok(&mut session, "split.part.set", json!({"name": "B", "steps": [{"op": "part", "name": "A"}], "padding": 1}));
    // Editing A changes B: the edit is refused although A's own texture is fine.
    fails(&mut session, "split.part.set", json!({"name": "A", "steps": [{"op": "alpha"}]}), "part \"B\" needs a 8194x");
    assert_eq!(ok(&mut session, "split.plan", json!({}))["parts"][1]["pixels"], 40);
    ok(&mut session, "split.apply", json!({}));
}

#[test]
fn index_moves_an_existing_part_and_apply_refuses_a_missing_parent() {
    let (mut session, _) = session_with_draft();
    let alpha = json!([{"op": "alpha"}]);
    for name in ["A", "B", "C"] {
        set(&mut session, name, alpha.clone());
    }
    let moved = ok(&mut session, "split.part.set", json!({"name": "A", "steps": alpha, "index": 2}));
    assert_eq!(moved["index"], 2);
    assert_eq!(moved["parts"], 3, "moving does not duplicate the part");
    let names: Vec<Value> =
        ok(&mut session, "split.plan", json!({}))["plan"]["parts"].as_array().unwrap().iter().map(|p| p["name"].clone()).collect();
    assert_eq!(names, [json!("B"), json!("C"), json!("A")]);
    fails(&mut session, "split.part.set", json!({"name": "A", "steps": alpha, "index": 3}), "index must be at most 2");

    fails(&mut session, "split.apply", json!({"parent": 999_999}), "node 999999 was not found");
    fails(&mut session, "split.apply", json!({"parent": "root"}), "parent must be a node UUID");
}

#[test]
fn a_plan_is_refused_when_the_source_pixels_changed_even_at_the_same_size() {
    let (mut session, path) = session_with_draft();
    prompt_all(&mut session);
    let plan = ok(&mut session, "split.plan", json!({}))["plan"].clone();
    assert!(plan["source"]["crc32"].is_u64(), "{plan}");

    // Same size, one pixel different, same path.
    let (mut rgba, _) = flatten();
    rgba[(H as usize / 2 * W as usize + 2) * 4..][..4].copy_from_slice(&[1, 2, 3, 255]);
    image::RgbaImage::from_raw(W, H, rgba).unwrap().save(&path).unwrap();
    fails(&mut session, "split.apply", json!({"plan": plan}), "checksum mismatch");
    assert!(parts_of(&session.snapshot()).is_empty());

    // The plan itself is not accepted without the checksum.
    let mut anonymous = plan.clone();
    anonymous["source"].as_object_mut().unwrap().remove("crc32");
    fails(&mut session, "split.apply", json!({"plan": anonymous}), "crc32");
}

#[test]
fn padding_that_makes_the_texture_too_large_is_refused_with_its_cause() {
    // A full-width part of an 8192-wide source cannot also carry padding: the texture would be 8194 wide.
    let dir = temp_dir();
    let path = dir.join("wide.png");
    image::RgbaImage::from_pixel(8192, 4, image::Rgba([10, 20, 30, 255])).save(&path).unwrap();
    let mut session = Session::new();
    ok(&mut session, "split.open", json!({"path": path.to_str().unwrap()}));
    let steps = json!([{"op": "alpha"}]);
    fails(&mut session, "split.part.set", json!({"name": "Wide", "steps": steps, "padding": 1}), "needs a 8194x");
    assert_eq!(session.snapshot()["partSplit"]["parts"], 0);
    let result = ok(&mut session, "split.part.set", json!({"name": "Wide", "steps": steps}));
    assert_eq!(result["part"]["pixels"], 8192 * 4);
    ok(&mut session, "split.apply", json!({}));
}

#[test]
fn closing_the_draft_and_replacing_the_document_drop_it() {
    let (mut session, _) = session_with_draft();
    prompt_all(&mut session);
    let closed = ok(&mut session, "split.close", json!({}));
    assert_eq!(closed["closed"], true);
    assert_eq!(session.snapshot()["partSplit"]["open"], false);
    fails(&mut session, "split.plan", json!({}), "no source is open");

    let (mut session, _) = session_with_draft();
    ok(&mut session, "new", json!({}));
    assert_eq!(session.snapshot()["partSplit"]["open"], false, "a new document starts without a draft");
}
