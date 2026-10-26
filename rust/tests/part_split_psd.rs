//! `split.open` on a PSD, and the quality measure of the split: a small layered picture is generated here, flattened,
//! split through the public `split.*` requests and compared with the layers it came from. No third-party artwork.

mod common;

use base64::Engine;
use serde_json::{json, Value};
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const SIZE: u32 = 64;
const CLAIMED: u8 = 128;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "soul2d-split-psd-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    common::scratch(dir)
}

// ------------------------------------------------------------------------------------------ the layered picture

type Paint = fn(u32, u32) -> [u8; 4];

/// One original layer, back to front in [`layers`].
struct Layer {
    name: &'static str,
    paint: Paint,
}

fn rect(x: u32, y: u32, w: u32, h: u32, colour: [u8; 3]) -> impl Fn(u32, u32) -> [u8; 4] {
    move |px, py| {
        if (x..x + w).contains(&px) && (y..y + h).contains(&py) {
            [colour[0], colour[1], colour[2], 255]
        } else {
            [0; 4]
        }
    }
}

/// A disc whose rim fades from opaque to transparent over three pixels.
fn soft_disc(cx: f64, cy: f64, radius: f64, colour: [u8; 3]) -> impl Fn(u32, u32) -> [u8; 4] {
    move |px, py| {
        let distance = ((f64::from(px) + 0.5 - cx).powi(2) + (f64::from(py) + 0.5 - cy).powi(2)).sqrt();
        let alpha = ((radius + 1.5 - distance) / 3.0).clamp(0.0, 1.0);
        [colour[0], colour[1], colour[2], (alpha * 255.0).round() as u8]
    }
}

const MID_BLUE: [u8; 3] = [60, 60, 200];

/// Back to front. `Hidden` lies entirely behind `Mid`; `Touch` has the colour of `Mid` and shares an edge with it;
/// `Cap` overlaps `Back`; `SoftAlone` has a soft rim on a transparent canvas, `SoftOver` one that blends with `Back`.
fn layers() -> Vec<Layer> {
    vec![
        Layer { name: "Back", paint: |x, y| rect(4, 4, 40, 40, [200, 60, 60])(x, y) },
        Layer { name: "Hidden", paint: |x, y| rect(12, 12, 8, 8, [30, 160, 30])(x, y) },
        Layer { name: "Mid", paint: |x, y| rect(10, 10, 16, 16, MID_BLUE)(x, y) },
        Layer { name: "Touch", paint: |x, y| rect(26, 10, 12, 16, MID_BLUE)(x, y) },
        Layer { name: "Cap", paint: |x, y| rect(14, 30, 20, 8, [240, 220, 60])(x, y) },
        Layer { name: "SoftAlone", paint: |x, y| soft_disc(10.0, 54.0, 7.0, [90, 200, 220])(x, y) },
        Layer { name: "SoftOver", paint: |x, y| soft_disc(46.0, 46.0, 10.0, [240, 140, 200])(x, y) },
    ]
}

/// Straight-alpha source-over of `top` on `bottom`.
fn over(top: [u8; 4], bottom: [u8; 4]) -> [u8; 4] {
    let (ta, ba) = (f64::from(top[3]) / 255.0, f64::from(bottom[3]) / 255.0);
    let alpha = ta + ba * (1.0 - ta);
    if alpha == 0.0 {
        return [0; 4];
    }
    let mix = |c: usize| ((f64::from(top[c]) * ta + f64::from(bottom[c]) * ba * (1.0 - ta)) / alpha).round() as u8;
    [mix(0), mix(1), mix(2), (alpha * 255.0).round() as u8]
}

/// The flattened picture, and per pixel the topmost layer that claims it (alpha of at least 128): the ground truth.
fn flatten() -> (Vec<[u8; 4]>, Vec<Option<usize>>) {
    let layers = layers();
    let mut picture = vec![[0u8; 4]; (SIZE * SIZE) as usize];
    let mut owner = vec![None; picture.len()];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let at = (y * SIZE + x) as usize;
            for (index, layer) in layers.iter().enumerate() {
                let pixel = (layer.paint)(x, y);
                picture[at] = over(pixel, picture[at]);
                if pixel[3] >= CLAIMED {
                    owner[at] = Some(index);
                }
            }
        }
    }
    (picture, owner)
}

fn write_png(dir: &std::path::Path) -> String {
    let rgba: Vec<u8> = flatten().0.into_iter().flatten().collect();
    let path = dir.join("flat.png");
    image::RgbaImage::from_raw(SIZE, SIZE, rgba).unwrap().save(&path).unwrap();
    path.to_str().unwrap().to_owned()
}

// ----------------------------------------------------------------------------------------------- PSD writer

fn be16(value: u16) -> [u8; 2] {
    value.to_be_bytes()
}

fn be32(value: u32) -> [u8; 4] {
    value.to_be_bytes()
}

/// One full-canvas, uncompressed layer record and its channel data (R, G, B, transparency).
fn psd_layer(name: &str, pixels: &[[u8; 4]], hidden: bool, side: u32, clipped: bool) -> (Vec<u8>, Vec<u8>) {
    psd_layer_record(name, pixels, hidden, side, clipped, None)
}

fn psd_layer_record(name: &str, pixels: &[[u8; 4]], hidden: bool, side: u32, clipped: bool, group: Option<u32>) -> (Vec<u8>, Vec<u8>) {
    let planes: Vec<Vec<u8>> = [0usize, 1, 2, 3]
        .iter()
        .map(|&c| [&be16(0)[..], &pixels.iter().map(|pixel| pixel[c]).collect::<Vec<u8>>()].concat())
        .collect();
    let mut record = Vec::new();
    for value in [0i32, 0, side as i32, side as i32] {
        record.extend(value.to_be_bytes());
    }
    record.extend(be16(4));
    for (plane, id) in planes.iter().zip([0i16, 1, 2, -1]) {
        record.extend(id.to_be_bytes());
        record.extend(be32(plane.len() as u32));
    }
    record.extend(b"8BIMnorm");
    record.extend([255, u8::from(clipped), if hidden { 2 } else { 0 }, 0]);
    let mut extra = Vec::new();
    extra.extend(be32(0)); // layer mask
    extra.extend(be32(0)); // blending ranges
    extra.push(name.len() as u8);
    extra.extend(name.as_bytes());
    while extra.len() % 4 != 0 {
        extra.push(0); // the Pascal name pads to four bytes, and so does what precedes it
    }
    if let Some(kind) = group {
        extra.extend(b"8BIMlsct");
        extra.extend(be32(4));
        extra.extend(be32(kind));
    }
    record.extend(be32(extra.len() as u32));
    record.extend(extra);
    (record, planes.concat())
}

/// A version 1, 8-bit RGB document of `side` x `side` pixels from layers given bottom to top.
fn psd_bytes(side: u32, layers: &[(&str, Vec<[u8; 4]>, bool)]) -> Vec<u8> {
    psd_bytes_with_clipping(side, &layers.iter().map(|(name, pixels, hidden)|
        (*name, pixels.clone(), *hidden, false)).collect::<Vec<_>>())
}

fn psd_bytes_with_clipping(side: u32, layers: &[(&str, Vec<[u8; 4]>, bool, bool)]) -> Vec<u8> {
    psd_bytes_from_records(side, layers.iter().map(|(name, pixels, hidden, clipped)|
        psd_layer(name, pixels, *hidden, side, *clipped)).collect())
}

fn psd_bytes_from_records(side: u32, layers: Vec<(Vec<u8>, Vec<u8>)>) -> Vec<u8> {
    let (mut records, mut data) = (Vec::new(), Vec::new());
    let count = layers.len();
    for (record, planes) in layers {
        records.extend(record);
        data.extend(planes);
    }
    let mut info = (count as i16).to_be_bytes().to_vec();
    info.extend(records);
    info.extend(data);
    if info.len() % 2 == 1 {
        info.push(0);
    }
    let mut section = be32(info.len() as u32).to_vec();
    section.extend(info);
    section.extend(be32(0)); // global layer mask info
    let mut out = b"8BPS".to_vec();
    out.extend(be16(1));
    out.extend([0; 6]);
    out.extend(be16(4));
    out.extend(be32(side));
    out.extend(be32(side));
    out.extend(be16(8));
    out.extend(be16(3));
    out.extend(be32(0)); // colour mode data
    out.extend(be32(0)); // image resources
    out.extend(be32(section.len() as u32));
    out.extend(section);
    out.extend([0, 0]); // merged image: raw, and deliberately absent
    out
}

/// The layered picture as a PSD, with a hidden full-canvas layer on top that must not show.
fn write_psd(dir: &std::path::Path) -> String {
    let mut stack: Vec<(&str, Vec<[u8; 4]>, bool)> = layers()
        .iter()
        .map(|layer| {
            let pixels = (0..SIZE * SIZE).map(|at| (layer.paint)(at % SIZE, at / SIZE)).collect();
            (layer.name, pixels, false)
        })
        .collect();
    stack.push(("Ghost", vec![[255, 0, 255, 255]; (SIZE * SIZE) as usize], true));
    let path = dir.join("layered.psd");
    fs::write(&path, psd_bytes(SIZE, &stack)).unwrap();
    path.to_str().unwrap().to_owned()
}

// ---------------------------------------------------------------------------------------------------- helpers

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

/// A request that must fail, naming `needle`, and leave the session as it was. Returns the message.
fn fails(session: &mut Session, op: &str, body: Value, needle: &str) -> String {
    let before = session.snapshot();
    let reply = run(session, op, body);
    assert_eq!(reply["succeeded"], false, "{op} should fail: {reply}");
    let message = reply["message"].as_str().unwrap_or("").to_owned();
    assert!(message.contains(needle), "{op}: {message:?} lacks {needle:?}");
    assert_eq!(session.snapshot(), before, "{op}: the failed request changed the session");
    message
}

fn decode_png(base64_png: &str) -> image::RgbaImage {
    let bytes = base64::engine::general_purpose::STANDARD.decode(base64_png).unwrap();
    image::load_from_memory(&bytes).unwrap().to_rgba8()
}

/// The draft's picture as the overlay shows it: straight-alpha source over mid grey, no parts drawn.
fn overlay(session: &mut Session) -> image::RgbaImage {
    decode_png(ok(session, "split.overlay", json!({}))["png"].as_str().unwrap())
}

fn set(session: &mut Session, name: &str, steps: Value) -> Value {
    ok(session, "split.part.set", json!({"name": name, "steps": steps}))
}

/// The prompts a client would give for the layered picture (everything but `Hidden`).
fn prompt_all(session: &mut Session) {
    set(session, "Back", json!([{"op": "flood", "point": [6, 6], "tolerance": 10}]));
    // Mid and Touch share one colour and one edge: the flood finds both, the vertical line tells them apart.
    set(session, "Mid", json!([{"op": "flood", "point": [12, 12], "tolerance": 0}, {"op": "side", "axisX": 26, "keep": "left"}]));
    set(session, "Touch", json!([{"op": "flood", "point": [30, 12], "tolerance": 0}, {"op": "side", "axisX": 26, "keep": "right"}]));
    set(session, "Cap", json!([{"op": "flood", "point": [20, 34], "tolerance": 0}]));
    set(session, "SoftAlone", json!([{"op": "component", "point": [10, 54]}]));
    // The opaque core, grown over the soft rim; the rim that blends with Back cannot be told from Back by colour.
    set(session, "SoftOver", json!([{"op": "flood", "point": [46, 46], "tolerance": 0}, {"op": "grow", "radius": 4}]));
}

/// `(name, slot, trans)` of the Parts under the root in draw order.
fn applied_parts(snapshot: &Value) -> Vec<(String, u64, [f64; 2])> {
    snapshot["model"]["nodes"]["children"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|node| node["type"] == "Part")
        .map(|node| {
            let trans = &node["transform"]["trans"];
            (
                node["name"].as_str().unwrap().to_owned(),
                node["textures"][0].as_u64().unwrap(),
                [trans[0].as_f64().unwrap(), trans[1].as_f64().unwrap()],
            )
        })
        .collect()
}

/// The set of source pixels a Part's texture covers with alpha of at least 128.
fn claimed_by_part(session: &mut Session, slot: u64, trans: [f64; 2]) -> Vec<bool> {
    let result = ok(session, "texture.get", json!({"slot": slot}));
    let image = decode_png(result["base64"].as_str().unwrap());
    let (tw, th) = (i64::from(image.width()), i64::from(image.height()));
    let left = trans[0] as i64 + i64::from(SIZE / 2) - tw / 2;
    let top = trans[1] as i64 + i64::from(SIZE / 2) - th / 2;
    let mut set = vec![false; (SIZE * SIZE) as usize];
    for (x, y, pixel) in image.enumerate_pixels() {
        let (sx, sy) = (left + i64::from(x), top + i64::from(y));
        if pixel[3] >= CLAIMED && (0..i64::from(SIZE)).contains(&sx) && (0..i64::from(SIZE)).contains(&sy) {
            set[(sy * i64::from(SIZE) + sx) as usize] = true;
        }
    }
    set
}

fn iou(a: &[bool], b: &[bool]) -> f64 {
    let both = a.iter().zip(b).filter(|(x, y)| **x && **y).count();
    let either = a.iter().zip(b).filter(|(x, y)| **x || **y).count();
    both as f64 / either as f64
}

fn premultiplied(pixel: [u8; 4]) -> [f64; 4] {
    let alpha = f64::from(pixel[3]);
    [f64::from(pixel[0]) * alpha / 255.0, f64::from(pixel[1]) * alpha / 255.0, f64::from(pixel[2]) * alpha / 255.0, alpha]
}

/// Mean and maximum absolute difference per channel, in premultiplied 8-bit values: straight colour at a very low
/// alpha is not meaningful, and the premultiplied value is what ends up on screen.
fn difference(rendered: &[[u8; 4]], expected: &[[u8; 4]]) -> (f64, f64) {
    let (mut sum, mut max) = (0.0f64, 0.0f64);
    for (a, b) in rendered.iter().zip(expected) {
        for (x, y) in premultiplied(*a).iter().zip(premultiplied(*b)) {
            let d = (x - y).abs();
            sum += d;
            max = max.max(d);
        }
    }
    (sum / (rendered.len() * 4) as f64, max)
}

fn render_document(session: &mut Session) -> Vec<[u8; 4]> {
    let half = f64::from(SIZE / 2);
    let result = ok(session, "render", json!({"width": SIZE, "height": SIZE, "bounds": [-half, -half, half, half]}));
    decode_png(result["png"].as_str().unwrap()).pixels().map(|p| p.0).collect()
}

// ---------------------------------------------------------------------------------------------------- PSD input

#[test]
fn split_psd_clipping_scales_colour_by_fractional_base_alpha() {
    let dir = temp_dir();
    let path = dir.join("fractional-clip.psd");
    let base = vec![[0, 0, 0, 64], [0, 0, 0, 192], [0, 0, 0, 0], [0, 0, 0, 255]];
    fs::write(&path, psd_bytes_with_clipping(2, &[
        ("Base", base, false, false),
        ("Clipped", vec![[255, 0, 0, 255]; 4], false, true),
    ])).unwrap();
    let mut session = Session::new();
    ok(&mut session, "split.open", json!({"path": path}));
    let drawn = overlay(&mut session);
    for (actual, expected) in drawn.pixels().zip([
        [160u8, 96, 96, 255], [224, 32, 32, 255], [128, 128, 128, 255], [255, 0, 0, 255],
    ]) {
        assert!(actual.0.into_iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1),
            "fractional clip: {:?} != {expected:?}", actual.0);
    }
}

#[test]
fn a_clipped_layer_in_linear_burn_linear_light_hue_or_color_blends_with_its_base_by_the_formula() {
    // A layer of (128, 179, 51) clipped to a base of (204, 153, 102) that is opaque in the first pixel and half
    // transparent (128) in the second, shown over the grey 128 of the overlay. The first pixel is the formula by
    // hand. In the second the import stores the base as (102, 76, 51) of alpha 128, cut to whole numbers, which is
    // (203.2, 151.4, 101.6) of 255. The formula on that is rounded to a byte twice on the way, once premultiplied
    // as the texture of the clipping stack and once straight in the picture, and the overlay puts 128/255 of it
    // over 127/255 of the grey, cut to a whole number. A difference of 1 is allowed, as elsewhere in this file.
    let dir = temp_dir();
    let cases: [(&[u8; 4], [[u8; 3]; 2]); 4] = [
        (b"lbrn", [[77, 77, 0], [101, 101, 63]]),
        (b"lLit", [[205, 255, 0], [166, 191, 63]]),
        (b"hue ", [[145, 186, 84], [135, 156, 105]]),
        (b"colr", [[141, 192, 64], [133, 159, 95]]),
    ];
    for (key, expected) in cases {
        let base = vec![[204, 153, 102, 255], [204, 153, 102, 128], [0, 0, 0, 0], [0, 0, 0, 0]];
        let mut clipped = psd_layer("Clipped", &[[128, 179, 51, 255]; 4], false, 2, true);
        let blend_at = clipped.0.windows(8).position(|bytes| bytes == b"8BIMnorm").unwrap() + 4;
        clipped.0[blend_at..blend_at + 4].copy_from_slice(key);
        let path = dir.join(format!("clip-{}.psd", String::from_utf8_lossy(key).trim()));
        fs::write(&path, psd_bytes_from_records(2, vec![psd_layer("Base", &base, false, 2, false), clipped])).unwrap();
        let mut session = Session::new();
        ok(&mut session, "split.open", json!({"path": path}));
        let drawn = overlay(&mut session);
        for (actual, expected) in drawn.pixels().zip(expected) {
            assert!(actual.0.into_iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1),
                "{}: {:?} != {expected:?}", String::from_utf8_lossy(key), actual.0);
        }
    }
}

#[test]
fn clipping_stacks_keep_the_base_alpha_visibility_and_group_opacity() {
    let dir = temp_dir();
    let solid = |name: &str, color: [u8; 4], hidden, clipped| psd_layer(name, &vec![color; 4], hidden, 2, clipped);
    let mut translucent_base = solid("Base", [0, 0, 0, 128], false, false);
    // Opacity follows the unique blend signature in this synthetic record.
    let opacity_at = translucent_base.0.windows(8).position(|bytes| bytes == b"8BIMnorm").unwrap() + 8;
    translucent_base.0[opacity_at] = 128;
    let mut group = psd_layer_record("Group", &[], false, 0, false, Some(1));
    let opacity_at = group.0.windows(8).position(|bytes| bytes == b"8BIMnorm").unwrap() + 8;
    group.0[opacity_at] = 128;
    let mut pass_group = psd_layer_record("Pass group", &[], false, 0, true, Some(1));
    let blend_at = pass_group.0.windows(8).position(|bytes| bytes == b"8BIMnorm").unwrap() + 4;
    pass_group.0[blend_at..blend_at + 4].copy_from_slice(b"pass");
    let mut screen = solid("Screen", [255, 0, 0, 255], false, false);
    let blend_at = screen.0.windows(8).position(|bytes| bytes == b"8BIMnorm").unwrap() + 4;
    screen.0[blend_at..blend_at + 4].copy_from_slice(b"scrn");
    let mut inner_clip = screen.clone();
    inner_clip.0[blend_at + 5] = 1;
    let outer_group = pass_group.clone();
    let cases = [
        (vec![solid("Base", [0, 0, 0, 128], false, false),
            solid("Red", [255, 0, 0, 255], false, true), solid("Green", [0, 255, 0, 255], false, true)],
            [64u8, 192, 64, 255]),
        (vec![translucent_base, solid("Red", [255, 0, 0, 255], false, true)], [160, 96, 96, 255]),
        (vec![solid("Base", [0, 0, 0, 128], true, false), solid("Red", [255, 0, 0, 255], false, true)],
            [128, 128, 128, 255]),
        (vec![solid("Background", [0, 0, 255, 255], false, false),
            solid("Base", [0, 0, 0, 128], false, false), solid("Red", [255, 0, 0, 255], false, true)],
            [128, 0, 127, 255]),
        (vec![psd_layer_record("</Layer group>", &[], false, 0, false, Some(3)),
            solid("First", [0, 0, 0, 128], false, false), solid("Second", [0, 0, 0, 128], false, false),
            group, solid("Red", [255, 0, 0, 255], false, true)], [176, 80, 80, 255]),
        (vec![solid("Background", [0, 0, 255, 255], false, false),
            psd_layer("Base", &[[128, 128, 128, 128], [0, 0, 0, 0], [128, 128, 128, 128], [0, 0, 0, 0]], false, 2, false),
            psd_layer_record("</Layer group>", &[], false, 0, false, Some(3)), screen, pass_group],
            [128, 64, 191, 255]),
        (vec![solid("Background", [0, 0, 255, 255], false, false),
            psd_layer("Base", &[[128, 128, 128, 128], [0, 0, 0, 0], [128, 128, 128, 128], [0, 0, 0, 0]], false, 2, false),
            psd_layer_record("</Layer group>", &[], false, 0, false, Some(3)),
            solid("Inner base", [0, 255, 0, 128], false, false), inner_clip, outer_group],
            [96, 96, 159, 255]),
    ];
    for (index, (records, expected)) in cases.into_iter().enumerate() {
        let path = dir.join(format!("stack-{index}.psd"));
        fs::write(&path, psd_bytes_from_records(2, records)).unwrap();
        let mut session = Session::new();
        ok(&mut session, "split.open", json!({"path": path}));
        for (pixel_index, pixel) in overlay(&mut session).pixels().enumerate() {
            let expected = if index >= 5 && pixel_index % 2 == 1 { [0, 0, 255, 255] } else { expected };
            assert!(pixel.0.into_iter().zip(expected).all(|(actual, expected)| actual.abs_diff(expected) <= 1),
                "clipping stack {index}: {:?} != {expected:?}", pixel.0);
        }
    }
}

#[test]
fn a_clipping_stack_under_the_deepest_composite_groups_still_opens() {
    let dir = temp_dir();
    // The renderer nests 16 Composites. A stack is one more, so under 16 groups the stencil clip draws instead.
    for (depth, base_alpha, expected) in [(15, 128, [192u8, 64, 64, 255]), (16, 255, [255, 0, 0, 255])] {
        let mut records = vec![psd_layer_record("</Layer group>", &[], false, 0, false, Some(3)); depth];
        records.push(psd_layer("Base", &[[0, 0, 0, base_alpha]; 4], false, 2, false));
        records.push(psd_layer("Clipped", &[[255, 0, 0, 255]; 4], false, 2, true));
        records.extend((0..depth).map(|_| psd_layer_record("Group", &[], false, 0, false, Some(1))));
        let path = dir.join(format!("deep-{depth}.psd"));
        fs::write(&path, psd_bytes_from_records(2, records)).unwrap();
        let mut session = Session::new();
        ok(&mut session, "split.open", json!({"path": path}));
        for pixel in overlay(&mut session).pixels() {
            assert!(pixel.0.into_iter().zip(expected).all(|(actual, expected)| actual.abs_diff(expected) <= 1),
                "{depth} groups: {:?} != {expected:?}", pixel.0);
        }
    }
}

#[test]
fn a_psd_opens_as_its_flattened_visible_layers() {
    let dir = temp_dir();
    let (psd, png) = (write_psd(&dir), write_png(&dir));
    let mut session = Session::new();
    let opened = ok(&mut session, "split.open", json!({"path": psd}));
    assert_eq!((opened["width"].clone(), opened["height"].clone()), (json!(SIZE), json!(SIZE)));
    assert_eq!(session.snapshot()["partSplit"]["open"], true);

    // Known pixels, through the overlay of a draft without parts: the layers' colours, the empty canvas as grey,
    // and the hidden full-canvas layer absent.
    let picture = overlay(&mut session);
    assert_eq!(picture.dimensions(), (SIZE, SIZE));
    assert_eq!(picture.get_pixel(0, 0).0, [128, 128, 128, 255], "empty canvas, hidden Ghost layer not drawn");
    assert_eq!(picture.get_pixel(6, 6).0, [200, 60, 60, 255], "Back");
    assert_eq!(picture.get_pixel(12, 12).0, [60, 60, 200, 255], "Mid hides Hidden");
    assert_eq!(picture.get_pixel(20, 34).0, [240, 220, 60, 255], "Cap");
    assert_eq!(picture.get_pixel(10, 54).0, [90, 200, 220, 255], "core of SoftAlone");

    // The same picture as the PNG of the flattened layers: every pixel, soft rims included.
    let mut reference = Session::new();
    ok(&mut reference, "split.open", json!({"path": png}));
    let expected = overlay(&mut reference);
    let wrong: Vec<_> = picture
        .enumerate_pixels()
        .filter(|(x, y, pixel)| pixel.0.iter().zip(expected.get_pixel(*x, *y).0).any(|(a, b)| a.abs_diff(b) > 1))
        .map(|(x, y, pixel)| (x, y, pixel.0, expected.get_pixel(x, y).0))
        .collect();
    assert!(wrong.is_empty(), "{} pixels differ by more than 1, first {:?}", wrong.len(), &wrong[..wrong.len().min(3)]);

    // And it splits like the PNG: the claimed pixels of a flood are the same.
    for session in [&mut session, &mut reference] {
        let part = set(session, "Cap", json!([{"op": "flood", "point": [20, 34], "tolerance": 0}]));
        assert_eq!((part["part"]["pixels"].clone(), part["part"]["bbox"].clone()), (json!(160), json!([14, 30, 20, 8])));
    }
}

#[test]
fn an_odd_sized_psd_keeps_its_last_row_and_column() {
    let dir = temp_dir();
    let side = 5;
    let pixels: Vec<[u8; 4]> = (0..side * side).map(|i| [(i * 10) as u8, (255 - i * 10) as u8, (i * 7) as u8, 255]).collect();
    let path = dir.join("odd.psd");
    std::fs::write(&path, psd_bytes(side, &[("Odd", pixels.clone(), false)])).unwrap();
    let mut session = Session::new();
    let opened = ok(&mut session, "split.open", json!({"path": path.to_str().unwrap()}));
    assert_eq!((opened["width"].clone(), opened["height"].clone()), (json!(side), json!(side)));
    let picture = overlay(&mut session);
    let drawn: Vec<[u8; 4]> = picture.pixels().map(|pixel| pixel.0).collect();
    assert_eq!(drawn, pixels, "every texel of the 5x5 layer at its own pixel");
}

#[test]
fn a_psd_with_a_picture_outside_the_limits_or_without_pixels_is_refused_by_name() {
    let dir = temp_dir();
    let mut session = Session::new();
    let dir_text = dir.to_str().unwrap().to_owned();
    let mut refused = |file: &str, bytes: Vec<u8>, needle: &str| {
        let path = dir.join(file);
        fs::write(&path, bytes).unwrap();
        let message = fails(&mut session, "split.open", json!({"path": path.to_str().unwrap()}), needle);
        assert!(message.contains(file), "{message:?} should name {file}");
        assert!(!message.contains(&dir_text), "{message:?} holds the directory");
    };

    let one = |pixels: usize| vec![("L", vec![[1, 2, 3, 255]; pixels], false)];
    // Wider than the texture limit of 8192 pixels, though the PSD reader would take it.
    let mut wide = psd_bytes(1, &one(1));
    wide[18..22].copy_from_slice(&be32(9000));
    refused("wide.psd", wide, "9000x1 is not a supported texture size");
    // 4097 x 4097 stays inside the side limit and above 16 megapixels.
    let mut large = psd_bytes(1, &one(1));
    large[14..18].copy_from_slice(&be32(4097));
    large[18..22].copy_from_slice(&be32(4097));
    refused("large.psd", large, "4097x4097 is not a supported texture size");
    // No layer record at all (a flattened PSD): only the stored merged image would hold pixels.
    refused("flat.psd", psd_bytes(4, &[]), "no layer with pixels");
    let mut sixteen = psd_bytes(2, &one(4));
    sixteen[22..24].copy_from_slice(&be16(16));
    refused("deep.psd", sixteen, "Only 8-bit PSD channels are supported");
    refused("garbage.psd", b"8BPS not really".to_vec(), "Could not load garbage.psd");
    refused("empty.psd", Vec::new(), "Could not load empty.psd");

    // A name that does not exist: the message names the file, not where it was looked for.
    let missing = dir.join("missing.psd");
    let message = fails(&mut session, "split.open", json!({"path": missing.to_str().unwrap()}), "missing.psd");
    assert!(!message.contains(&dir_text), "{message:?}");
    assert_eq!(session.snapshot()["partSplit"]["open"], false, "a refused open leaves no draft");
}

// ------------------------------------------------------------------------------------------------------ quality

/// Lowest IoU the current steps reach per layer, with a small margin below the measured value
/// (`Back` 0.984, `SoftOver` 0.921, every other layer exactly 1, for the PNG and the PSD alike).
fn iou_floor(name: &str) -> f64 {
    match name {
        "Back" => 0.97,
        "SoftOver" => 0.90,
        _ => 0.99,
    }
}

#[test]
fn split_parts_match_the_visible_layers_and_the_stack_reproduces_the_flattened_picture() {
    let dir = temp_dir();
    for source in [write_png(&dir), write_psd(&dir)] {
        measure_split(&source);
    }
}

fn measure_split(source: &str) {
    let mut session = Session::new();
    ok(&mut session, "split.open", json!({"path": source}));
    prompt_all(&mut session);

    // The limit: a layer that is entirely hidden has no pixel of its own in the picture, so nothing can recover it.
    // Asking for its area claims the pixels of the layer in front of it, and nothing else.
    let (picture, owner) = flatten();
    let names: Vec<&str> = layers().iter().map(|layer| layer.name).collect();
    let hidden = names.iter().position(|name| *name == "Hidden").unwrap();
    assert_eq!(owner.iter().filter(|o| **o == Some(hidden)).count(), 0, "Hidden owns no visible pixel");
    assert!(picture.iter().all(|pixel| *pixel != [30, 160, 30, 255]), "Hidden's colour is not in the picture");
    let probe = ok(&mut session, "split.part.set", json!({
        "name": "Hidden", "index": 1, "steps": [{"op": "box", "rect": [12, 12, 8, 8]}],
    }));
    assert_eq!(probe["part"]["pixels"], 64);
    assert_eq!(probe["part"]["overlaps"], json!([{"part": "Mid", "pixels": 64}]), "the area belongs to Mid");
    ok(&mut session, "split.part.remove", json!({"name": "Hidden"}));

    let plan = ok(&mut session, "split.plan", json!({}));
    assert_eq!(plan["unclaimedPixels"], 0, "{plan}");
    ok(&mut session, "split.apply", json!({}));
    let applied = applied_parts(&session.snapshot());
    assert_eq!(applied.iter().map(|part| part.0.as_str()).collect::<Vec<_>>(), ["Back", "Mid", "Touch", "Cap", "SoftAlone", "SoftOver"]);

    let mut report = Vec::new();
    for (name, slot, trans) in &applied {
        let layer = names.iter().position(|candidate| candidate == name).unwrap();
        let truth: Vec<bool> = owner.iter().map(|o| *o == Some(layer)).collect();
        report.push((name.clone(), iou(&claimed_by_part(&mut session, *slot, *trans), &truth)));
    }
    for (name, score) in &report {
        assert!(*score >= iou_floor(name), "{name}: IoU {score} is below {} for {source}; all {report:?}", iou_floor(name));
    }

    // Stacked back to front, the applied Parts draw the flattened picture (compared premultiplied, 0 to 255).
    // Measured: mean 0.022 and max 1.12 from the PNG, mean 0.041 and max 1.98 from the PSD.
    let (mean, max) = difference(&render_document(&mut session), &picture);
    assert!(mean <= 0.06 && max <= 2.5, "reproduction: mean {mean:.4}, max {max:.4}; IoU {report:?} for {source}");
}
