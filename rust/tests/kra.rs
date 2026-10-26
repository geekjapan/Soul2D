use flate2::write::DeflateEncoder;
use flate2::Compression;
use serde_json::{json, Value};
use soul2d_core::kra::{self, engine_blend, Kind, Limits as KraLimits};
use soul2d_core::model_import::{
    apply_merge, import_kra, import_psd, merge_kra, Choice, ImportOptions, Match, MergeOptions, Texture,
};
use soul2d_core::psd::Limits;
use soul2d_core::Session;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------------------------
// A tiny KRA writer: ZIP (stored and deflated entries) and an LZF compressor
// ---------------------------------------------------------------------------------------------

struct Member {
    name: String,
    data: Vec<u8>,
    deflate: bool,
    /// Overrides for corrupt archives.
    method: Option<u16>,
    flags: u16,
    crc: Option<u32>,
    size: Option<u32>,
}

fn member(name: &str, data: &[u8], deflate: bool) -> Member {
    Member { name: name.into(), data: data.to_vec(), deflate, method: None, flags: 0, crc: None, size: None }
}

fn zip(members: &[Member]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut directory = Vec::new();
    for member in members {
        let packed = if member.deflate {
            let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(&member.data).unwrap();
            encoder.finish().unwrap()
        } else {
            member.data.clone()
        };
        let method = member.method.unwrap_or(if member.deflate { 8 } else { 0 });
        let crc = member.crc.unwrap_or_else(|| crc32fast::hash(&member.data));
        let size = member.size.unwrap_or(member.data.len() as u32);
        let offset = out.len() as u32;
        let name = member.name.as_bytes();
        let mut common = Vec::new();
        common.extend_from_slice(&member.flags.to_le_bytes());
        common.extend_from_slice(&method.to_le_bytes());
        common.extend_from_slice(&[0; 4]);
        common.extend_from_slice(&crc.to_le_bytes());
        common.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        common.extend_from_slice(&size.to_le_bytes());
        common.extend_from_slice(&(name.len() as u16).to_le_bytes());
        common.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&common);
        out.extend_from_slice(name);
        out.extend_from_slice(&packed);

        directory.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        directory.extend_from_slice(&20u16.to_le_bytes());
        directory.extend_from_slice(&20u16.to_le_bytes());
        directory.extend_from_slice(&common);
        directory.extend_from_slice(&[0; 2 + 2 + 2 + 4]);
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name);
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

/// Literal runs plus back references over a 64 byte window, enough to exercise both decoder paths.
fn lzf(input: &[u8]) -> Vec<u8> {
    fn flush(out: &mut Vec<u8>, literals: &mut Vec<u8>) {
        for run in literals.chunks(32) {
            out.push((run.len() - 1) as u8);
            out.extend_from_slice(run);
        }
        literals.clear();
    }
    let mut out = Vec::new();
    let mut literals = Vec::new();
    let mut at = 0;
    while at < input.len() {
        let mut best = (0, 0);
        for distance in 1..=at.min(64) {
            let mut length = 0;
            while at + length < input.len() && length < 264 && input[at + length] == input[at + length - distance] {
                length += 1;
            }
            if length > best.0 {
                best = (length, distance);
            }
        }
        if best.0 >= 3 {
            flush(&mut out, &mut literals);
            let (length, distance) = (best.0 - 2, best.1 - 1);
            if length < 7 {
                out.push(((length << 5) | (distance >> 8)) as u8);
            } else {
                out.push(((7 << 5) | (distance >> 8)) as u8);
                out.push((length - 7) as u8);
            }
            out.push((distance & 0xff) as u8);
            at += best.0;
        } else {
            literals.push(input[at]);
            at += 1;
        }
    }
    flush(&mut out, &mut literals);
    out
}

/// Planar BGRA like Krita stores a tile.
fn planar(width: usize, height: usize, pixel: impl Fn(usize, usize) -> [u8; 4]) -> Vec<u8> {
    let area = width * height;
    let mut out = vec![0; area * 4];
    for y in 0..height {
        for x in 0..width {
            let [r, g, b, a] = pixel(x, y);
            let at = y * width + x;
            out[at] = b;
            out[area + at] = g;
            out[2 * area + at] = r;
            out[3 * area + at] = a;
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Count {
    /// Krita: the length in the header includes the flag byte.
    WithFlag,
    /// Other writers: the length is the LZF data alone.
    PayloadOnly,
}

fn layer_file(width: usize, height: usize, tiles: &[(i32, i32, Vec<u8>)], count: Count) -> Vec<u8> {
    let mut out = format!("VERSION 2\nTILEWIDTH {width}\nTILEHEIGHT {height}\nPIXELSIZE 4\nDATA {}\n", tiles.len()).into_bytes();
    for (left, top, planes) in tiles {
        if planes.is_empty() {
            out.extend_from_slice(format!("{left},{top},LZF,0\n").as_bytes());
            continue;
        }
        let packed = lzf(planes);
        let length = packed.len() + usize::from(count == Count::WithFlag);
        out.extend_from_slice(format!("{left},{top},LZF,{length}\n").as_bytes());
        out.push(1);
        out.extend_from_slice(&packed);
    }
    out
}

const MAINDOC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE DOC PUBLIC '-//KDE//DTD krita 2.0//EN' 'http://www.calligra.org/DTD/krita-2.0.dtd'>
<DOC xmlns="http://www.calligra.org/DTD/krita" kritaVersion="5.2.2" syntaxVersion="2.0">
 <IMAGE name="doc" width="12" height="8" colorspacename="RGBA" mime="application/x-kra">
  <ProjectionBackgroundColor ColorData="255,255,255,255"/>
  <layers>
   <layer name="Hat" nodetype="grouplayer" uuid="{g}" visible="1" opacity="128" collapsed="1" x="0" y="0" compositeop="normal">
    <layers>
     <layer name="Brim" nodetype="paintlayer" uuid="{b}" filename="layer2" colorspacename="RGBA" visible="1" opacity="200" compositeop="multiply" x="1" y="2"/>
     <layer name="Blur" nodetype="filterlayer" uuid="{f}" visible="1"/>
    </layers>
   </layer>
   <layer name="Body" nodetype="paintlayer" uuid="{o}" filename="layer1" colorspacename="RGBA" visible="1" opacity="255" compositeop="normal" x="0" y="0"/>
   <layer name="Ghost" nodetype="paintlayer" uuid="{h}" filename="layer3" colorspacename="RGBA" visible="0" opacity="0" compositeop="lighter color" x="3" y="1"/>
   <layer name="Empty" nodetype="paintlayer" uuid="{e}" filename="layer4" colorspacename="RGBA" visible="1" opacity="255" compositeop="normal" x="0" y="0"/>
  </layers>
 </IMAGE>
</DOC>"#;

#[test]
fn entity_declarations_are_refused_before_expansion() {
    for prolog in [
        "<!DOCTYPE DOC [<!ENTITY e 'expanded'>]>",
        "<!DOCTYPE DOC [<!ENTITY % p 'x'><!ENTITY e 'expanded'>]>",
        "<?xml version=\"?>\"?><!DOCTYPE DOC [<!ENTITY e 'expanded'>]>",
    ] {
        let xml = format!("{prolog}<DOC><IMAGE name='&e;' width='1' height='1' colorspacename='RGBA'/></DOC>");
        let input = zip(&[member("mimetype", b"application/x-krita", false), member("maindoc.xml", xml.as_bytes(), true)]);
        let error = kra::parse(&input, &KraLimits::default()).unwrap_err();
        assert!(error.contains("entity declarations"), "{error}");
    }
}

#[test]
fn external_doctypes_comments_and_builtin_entities_remain_supported() {
    for prefix in [
        "\u{feff}<?xml version='1.0'?><!DOCTYPE DOC PUBLIC '-//KDE//DTD krita 2.0//EN' 'http://www.calligra.org/DTD/krita-2.0.dtd'>",
        "<?probe ignored?><!-- a comment --><!DOCTYPE DOC SYSTEM 'urn:example:[x]'>",
        "<!DOCTYPE DOC []>",
    ] {
        let xml = format!("{prefix}<DOC><IMAGE name='a&amp;b' width='1' height='1' colorspacename='RGBA'><layers/></IMAGE></DOC>");
        let input = zip(&[member("mimetype", b"application/x-krita", false), member("maindoc.xml", xml.as_bytes(), true)]);
        assert_eq!(kra::parse(&input, &KraLimits::default()).unwrap().name, "a&b");
    }
}

fn body_pixel(x: usize, y: usize) -> [u8; 4] {
    if (1..3).contains(&x) && (1..3).contains(&y) {
        [100 + 10 * x as u8, 50 + 10 * y as u8, 7, 128]
    } else {
        [0, 0, 0, 0]
    }
}

fn brim_pixel(x: usize, y: usize) -> [u8; 4] {
    [x as u8 * 60, y as u8 * 60, 9, 255]
}

fn ghost_pixel(x: usize, y: usize) -> [u8; 4] {
    [200, 100 + x as u8, 5 + y as u8, 255]
}

fn files(count: Count) -> Vec<Member> {
    let body = layer_file(
        4,
        4,
        &[(0, 0, planar(4, 4, body_pixel)), (-4, 0, planar(4, 4, |_, _| [9, 9, 9, 0]))],
        count,
    );
    let brim = layer_file(4, 4, &[(4, 0, planar(4, 4, brim_pixel))], count);
    let ghost = layer_file(4, 4, &[(0, 0, planar(4, 4, ghost_pixel))], count);
    let empty = b"VERSION 2\nTILEWIDTH 4\nTILEHEIGHT 4\nPIXELSIZE 4\nDATA 0\n".to_vec();
    vec![
        member("mimetype", b"application/x-krita", false),
        member("maindoc.xml", MAINDOC.as_bytes(), true),
        member("doc/layers/layer1", &body, true),
        member("doc/layers/layer2", &brim, false),
        member("doc/layers/layer3", &ghost, true),
        member("doc/layers/layer4", &empty, true),
    ]
}

fn kra_bytes(count: Count) -> Vec<u8> {
    zip(&files(count))
}

fn limits() -> Limits {
    Limits::default()
}

fn rejected(bytes: &[u8]) -> String {
    kra::parse(bytes, &KraLimits::default()).unwrap_err()
}

fn replace(members: &mut [Member], name: &str, data: &[u8]) {
    members.iter_mut().find(|member| member.name == name).unwrap().data = data.to_vec();
}

fn maindoc_with(from: &str, to: &str) -> Vec<u8> {
    let mut members = files(Count::WithFlag);
    assert!(MAINDOC.contains(from), "{from}");
    replace(&mut members, "maindoc.xml", MAINDOC.replacen(from, to, 1).as_bytes());
    zip(&members)
}

// ---------------------------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------------------------

#[test]
fn reads_the_layer_tree_pixels_and_attributes() {
    for count in [Count::WithFlag, Count::PayloadOnly] {
        let document = kra::parse(&kra_bytes(count), &KraLimits::default()).unwrap();
        assert_eq!((document.name.as_str(), document.width, document.height), ("doc", 12, 8));
        let summary: Vec<(&str, Kind)> = document.layers.iter().map(|layer| (layer.name.as_str(), layer.kind)).collect();
        assert_eq!(
            summary,
            [
                ("Hat", Kind::Group { collapsed: true }),
                ("Brim", Kind::Paint),
                ("", Kind::End),
                ("Body", Kind::Paint),
                ("Ghost", Kind::Paint),
            ]
        );

        // The filter layer Blur is skipped, and it is visible. The paint layer Empty (no tiles) is empty, not skipped.
        let skipped = &document.skipped;
        assert_eq!(skipped.types, [("filterlayer".to_owned(), 1)]);
        assert_eq!((skipped.other, skipped.visible), (0, true));

        let hat = &document.layers[0];
        assert_eq!((hat.opacity, hat.visible, hat.blend), (128, true, "Normal"));
        assert!(hat.rgba.is_empty());

        // Brim: tile at (4,0) moved by the layer offset (1,2), all 4x4 opaque.
        let brim = &document.layers[1];
        assert_eq!([brim.left, brim.top, brim.right, brim.bottom], [5, 2, 9, 6]);
        assert_eq!((brim.opacity, brim.visible, brim.blend), (200, true, "Multiply"));
        let expected: Vec<u8> = (0..4).flat_map(|y| (0..4).flat_map(move |x| brim_pixel(x, y))).collect();
        assert_eq!(brim.rgba, expected);

        // Body: the empty tile at (-4,0) widens the box but the crop keeps the 2x2 of non-transparent pixels.
        let body = &document.layers[3];
        assert_eq!([body.left, body.top, body.right, body.bottom], [1, 1, 3, 3]);
        assert_eq!((body.opacity, body.visible, body.blend), (255, true, "Normal"));
        assert_eq!(body.rgba, [110, 60, 7, 128, 120, 60, 7, 128, 110, 70, 7, 128, 120, 70, 7, 128]);

        // Ghost: hidden, "lighter color" is D's Lighten, offset (3,1).
        let ghost = &document.layers[4];
        assert_eq!([ghost.left, ghost.top, ghost.right, ghost.bottom], [3, 1, 7, 5]);
        assert_eq!((ghost.opacity, ghost.visible, ghost.blend), (0, false, "Lighten"));
        assert_eq!(&ghost.rgba[..8], [200, 100, 5, 255, 200, 101, 5, 255]);
        assert_eq!(&ghost.rgba[ghost.rgba.len() - 4..], [200, 103, 8, 255]);
    }
}

#[test]
fn blend_keys_map_to_the_engine_names_d_uses() {
    for (key, name) in [
        ("normal", "Normal"),
        ("multiply", "Multiply"),
        ("screen", "Screen"),
        ("overlay", "Overlay"),
        ("darken", "Darken"),
        ("lighten", "Lighten"),
        ("lighter color", "Lighten"),
        ("dodge", "ColorDodge"),
        ("linear_dodge", "LinearDodge"),
        ("burn", "ColorBurn"),
        ("hard_light", "HardLight"),
        ("soft_light", "SoftLight"),
        ("diff", "Difference"),
        ("exclusion", "Exclusion"),
        ("subtract", "Subtract"),
        ("pass through", "Normal"),
        ("hue", "Normal"),
        ("", "Normal"),
    ] {
        assert_eq!(engine_blend(key), name, "{key}");
    }
}

#[test]
fn a_fully_transparent_layer_has_no_area() {
    let mut members = files(Count::WithFlag);
    let clear = layer_file(4, 4, &[(0, 0, planar(4, 4, |_, _| [1, 2, 3, 0]))], Count::WithFlag);
    replace(&mut members, "doc/layers/layer3", &clear);
    let document = kra::parse(&zip(&members), &KraLimits::default()).unwrap();
    let ghost = &document.layers[4];
    assert_eq!((ghost.right - ghost.left, ghost.bottom - ghost.top, ghost.rgba.len()), (0, 0, 0));
}

#[test]
fn raw_tiles_and_krita_sized_tiles_decode() {
    // Krita stores a tile as-is when LZF does not shrink it: 64 bytes of 4x4 planar data, flag 0.
    let planes = planar(4, 4, |x, y| [x as u8 * 3, y as u8 * 5, 1, 255]);
    let mut file = b"VERSION 2\nTILEWIDTH 4\nTILEHEIGHT 4\nPIXELSIZE 4\nDATA 1\n0,0,LZF,65\n".to_vec();
    file.push(0);
    file.extend_from_slice(&planes);
    let mut members = files(Count::WithFlag);
    replace(&mut members, "doc/layers/layer3", &file);
    let document = kra::parse(&zip(&members), &KraLimits::default()).unwrap();
    assert_eq!(&document.layers[4].rgba[..8], [0, 0, 1, 255, 3, 0, 1, 255]);
}

#[test]
fn multi_tile_layers_place_each_tile() {
    let tiles = [
        (0, 0, planar(4, 4, |_, _| [10, 0, 0, 255])),
        (4, 0, planar(4, 4, |_, _| [20, 0, 0, 255])),
        (0, 4, planar(4, 4, |_, _| [30, 0, 0, 255])),
    ];
    for count in [Count::WithFlag, Count::PayloadOnly] {
        let mut members = files(count);
        replace(&mut members, "doc/layers/layer3", &layer_file(4, 4, &tiles, count));
        let document = kra::parse(&zip(&members), &KraLimits::default()).unwrap();
        let ghost = &document.layers[4];
        assert_eq!([ghost.left, ghost.top, ghost.right, ghost.bottom], [3, 1, 11, 9]);
        let at = |x: usize, y: usize| ghost.rgba[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4].to_vec();
        assert_eq!(at(0, 0), [10, 0, 0, 255]);
        assert_eq!(at(5, 3), [20, 0, 0, 255]);
        assert_eq!(at(2, 7), [30, 0, 0, 255]);
        assert_eq!(at(6, 6), [0, 0, 0, 0]);
    }
}

// ---------------------------------------------------------------------------------------------
// Import and merge through model_import
// ---------------------------------------------------------------------------------------------

fn texture_pixels(texture: &Texture) -> (u32, u32, Vec<u8>) {
    let image = image::load_from_memory_with_format(&texture.data, image::ImageFormat::Png).unwrap();
    (image.width(), image.height(), image.into_rgba8().into_raw())
}

#[test]
fn import_kra_builds_the_node_tree_d_builds() {
    let imported = import_kra(&kra_bytes(Count::WithFlag), &ImportOptions::default(), &limits()).unwrap();
    let root = &imported.model["nodes"];
    assert_eq!((root["uuid"].clone(), root["name"].clone(), root["type"].clone()), (json!(1), json!("Root"), json!("Node")));
    // D inserts each node at index 0, so the XML order (Hat, Body, Ghost) comes out reversed.
    let names: Vec<&str> = root["children"].as_array().unwrap().iter().map(|n| n["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Ghost", "Body", "Hat"]);
    let (ghost, body, hat) = (&root["children"][0], &root["children"][1], &root["children"][2]);

    // zSort is the record position counting the group's end marker: Hat 1, Brim 2, End 3, Body 4, Ghost 5.
    assert_eq!((hat["type"].clone(), hat["zsort"].clone(), hat["enabled"].clone()), (json!("DynamicComposite"), json!(1.0), json!(true)));
    assert_eq!(hat["blend_mode"], "Normal");
    assert!((hat["opacity"].as_f64().unwrap() - 128.0 / 255.0).abs() < 1e-6);
    let brim = &hat["children"][0];
    assert_eq!((brim["name"].clone(), brim["zsort"].clone(), brim["blend_mode"].clone()), (json!("Brim"), json!(2.0), json!("Multiply")));
    assert!((brim["opacity"].as_f64().unwrap() - 200.0 / 255.0).abs() < 1e-6);
    assert_eq!(brim["psdLayerPath"], "/Hat/Brim");
    assert_eq!((body["zsort"].clone(), body["psdLayerPath"].clone()), (json!(4.0), json!("/Body")));
    assert_eq!((ghost["zsort"].clone(), ghost["enabled"].clone(), ghost["blend_mode"].clone()), (json!(5.0), json!(false), json!("Lighten")));

    // Canvas 12x8: centre (6,4). Ghost 4x4 at (3,1) -> (-1,-1); Body 2x2 at (1,1) -> (-4,-2);
    // Brim 4x4 at (5,2) -> (1,0); the group moves to its child's centre.
    assert_eq!(ghost["transform"]["trans"], json!([-1.0, -1.0, 0.0]));
    assert_eq!(body["transform"]["trans"], json!([-4.0, -2.0, 0.0]));
    assert_eq!(hat["transform"]["trans"], json!([1.0, 0.0, 0.0]));
    assert_eq!(brim["transform"]["trans"], json!([0.0, 0.0, 0.0]));
    assert_eq!(body["mesh"]["verts"], json!([-1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0]));

    // Textures follow Parts in pre-order: Ghost, Body, Brim. Pixels are premultiplied like D.
    assert_eq!((ghost["textures"].clone(), body["textures"].clone(), brim["textures"].clone()), (json!([0]), json!([1]), json!([2])));
    assert_eq!(imported.textures.len(), 3);
    let (width, height, pixels) = texture_pixels(&imported.textures[1]);
    assert_eq!((width, height), (2, 2));
    assert_eq!(pixels[..4], [55, 30, 3, 128]);
    let (width, height, pixels) = texture_pixels(&imported.textures[2]);
    assert_eq!((width, height), (4, 4));
    assert_eq!(pixels[..4], [0, 0, 9, 255]);
    assert_eq!(pixels[(4 + 3) * 4..(4 + 3) * 4 + 4], [180, 60, 9, 255]);
}

#[test]
fn import_kra_can_flatten_groups() {
    let options = ImportOptions { keep_structure: false, ..ImportOptions::default() };
    let imported = import_kra(&kra_bytes(Count::WithFlag), &options, &limits()).unwrap();
    let names: Vec<&str> =
        imported.model["nodes"]["children"].as_array().unwrap().iter().map(|n| n["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Ghost", "Body", "Brim"]);
    assert_eq!(imported.model["nodes"]["children"][2]["psdLayerPath"], "/Hat/Brim");
}

#[test]
fn merge_kra_matches_layers_in_document_order() {
    let kra_bytes = kra_bytes(Count::WithFlag);
    let imported = import_kra(&kra_bytes, &ImportOptions::default(), &limits()).unwrap();
    let options = MergeOptions { rename_mapped: false, retranslate_mapped: false, first_id: 100 };
    let plan = merge_kra(&imported.model, &imported.textures, &kra_bytes, &options, &limits()).unwrap();
    let described: Vec<(&str, &str, Option<u32>, Option<Match>)> =
        plan.layers.iter().map(|l| (l.name.as_str(), l.path.as_str(), l.target, l.matched_by)).collect();
    // The import numbered Root 1, Hat 2, Brim 3, Body 4, Ghost 5 in D's creation order.
    let ids: Vec<u64> = ["Hat", "Brim", "Body", "Ghost"]
        .iter()
        .map(|name| find_id(&imported.model["nodes"], name).unwrap())
        .collect();
    assert_eq!(
        described,
        [
            ("Brim", "/Hat/Brim", Some(ids[1] as u32), Some(Match::Path)),
            ("Body", "/Body", Some(ids[2] as u32), Some(Match::Path)),
            ("Ghost", "/Ghost", Some(ids[3] as u32), Some(Match::Path)),
        ]
    );
    assert_eq!(plan.document_size, [12, 8]);
    assert_eq!(plan.layers[0].width, 4);
    assert_eq!((plan.layers[1].left, plan.layers[1].top, plan.layers[1].width, plan.layers[1].height), (1, 1, 2, 2));
    assert!(plan.unmapped_parts.is_empty());

    // Against a model whose Parts carry other paths: only the last component can match, and the
    // unmatched layer is added.
    let mut other = imported.model.clone();
    other["nodes"]["children"][2]["children"][0]["psdLayerPath"] = json!("/Elsewhere/Brim");
    other["nodes"]["children"][1]["psdLayerPath"] = json!("");
    other["nodes"]["children"][1]["name"] = json!("Unrelated");
    let plan = merge_kra(&other, &imported.textures, &kra_bytes, &options, &limits()).unwrap();
    let described: Vec<(Option<u32>, Option<Match>)> = plan.layers.iter().map(|l| (l.target, l.matched_by)).collect();
    assert_eq!(described, [(Some(ids[1] as u32), Some(Match::Name)), (None, None), (Some(ids[3] as u32), Some(Match::Path))]);
    assert_eq!(plan.unmapped_parts, [find_id(&other["nodes"], "Unrelated").unwrap() as u32]);

    // Applying the default choices replaces Brim and Ghost, adds Body, and the model stays valid.
    let merged = apply_merge(&other, &imported.textures, &plan, &plan.default_choices()).unwrap();
    assert_eq!((merged.replaced.len(), merged.created.len()), (2, 1));
    let added = merged.created[0];
    assert!(added >= 100);
    let added_node = find_node(&merged.model["nodes"], u64::from(added)).unwrap();
    assert_eq!((added_node["name"].clone(), added_node["psdLayerPath"].clone()), (json!("Body"), json!("/Body")));
    assert_eq!(plan.default_choices()[1], Choice::Add);
}

fn find_node<'a>(node: &'a Value, id: u64) -> Option<&'a Value> {
    if node["uuid"] == id {
        return Some(node);
    }
    node["children"].as_array().into_iter().flatten().find_map(|child| find_node(child, id))
}

fn find_id(node: &Value, name: &str) -> Option<u64> {
    if node["name"] == name {
        return node["uuid"].as_u64();
    }
    node["children"].as_array().into_iter().flatten().find_map(|child| find_id(child, name))
}

#[test]
fn psd_import_is_unaffected_by_the_shared_source_model() {
    // A bad PSD still reports through the shared entry point.
    assert!(import_psd(b"8BPS", &ImportOptions::default(), &limits()).is_err());
}

// ---------------------------------------------------------------------------------------------
// Rejections
// ---------------------------------------------------------------------------------------------

#[test]
fn corrupt_zip_containers_are_rejected() {
    let good = kra_bytes(Count::WithFlag);
    assert!(rejected(b"").contains("not a ZIP"));
    assert!(rejected(b"PK\x03\x04 definitely not an archive, just text").contains("end record"));
    assert!(rejected(&good[..good.len() - 1]).contains("end record"));
    assert!(rejected(&good[..good.len() / 2]).contains("end record"));

    // Directory offset past the end record.
    let mut bad = good.clone();
    let eocd = bad.len() - 22;
    let end = bad.len() as u32;
    bad[eocd + 16..eocd + 20].copy_from_slice(&end.to_le_bytes());
    assert!(rejected(&bad).contains("directory"));

    // Entry count that disagrees with the per-disk count.
    let mut bad = good.clone();
    bad[eocd + 8] = bad[eocd + 8].wrapping_add(1);
    assert!(rejected(&bad).contains("multi-disk"));

    // Entry count larger than the directory holds.
    let mut bad = good.clone();
    bad[eocd + 8] += 3;
    bad[eocd + 10] += 3;
    assert!(rejected(&bad).contains("ZIP directory") || rejected(&bad).contains("truncated"));

    // A local header offset that points nowhere.
    let mut bad = good.clone();
    let directory = u32::from_le_bytes(good[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    bad[directory + 42..directory + 46].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(rejected(&bad).contains("truncated") || rejected(&bad).contains("local header"));

    // Bad signature in the directory.
    let mut bad = good.clone();
    bad[directory] ^= 0xff;
    assert!(rejected(&bad).contains("directory entry"));

    let mut members = files(Count::WithFlag);
    members[2].method = Some(99);
    assert!(rejected(&zip(&members)).contains("unsupported ZIP compression method 99"));

    let mut members = files(Count::WithFlag);
    members[1].flags = 1;
    assert!(rejected(&zip(&members)).contains("encrypted"));

    let mut members = files(Count::WithFlag);
    members[2].crc = Some(1);
    assert!(rejected(&zip(&members)).contains("CRC"));

    // Declared size smaller than the data: the inflater is stopped, not trusted.
    let mut members = files(Count::WithFlag);
    members[2].size = Some(10);
    assert!(rejected(&zip(&members)).contains("recorded size"));
    let mut members = files(Count::WithFlag);
    members[3].size = Some(10);
    assert!(rejected(&zip(&members)).contains("recorded size"));

    // Deflate data that is not deflate.
    let mut members = files(Count::WithFlag);
    members[2].method = Some(8);
    members[2].deflate = false;
    assert!(rejected(&zip(&members)).contains("deflate") || rejected(&zip(&members)).contains("recorded size"));
}

#[test]
fn missing_or_wrong_entries_are_rejected() {
    let mut members = files(Count::WithFlag);
    members[0].data = b"application/x-krita-archive".to_vec();
    assert!(rejected(&zip(&members)).contains("mimetype"));

    let members: Vec<Member> = files(Count::WithFlag).into_iter().filter(|m| m.name != "mimetype").collect();
    assert!(rejected(&zip(&members)).contains("no file 'mimetype'"));
    let members: Vec<Member> = files(Count::WithFlag).into_iter().filter(|m| m.name != "maindoc.xml").collect();
    assert!(rejected(&zip(&members)).contains("no file 'maindoc.xml'"));
    let members: Vec<Member> = files(Count::WithFlag).into_iter().filter(|m| m.name != "doc/layers/layer1").collect();
    assert!(rejected(&zip(&members)).contains("doc/layers/layer1"));
}

#[test]
fn bad_documents_are_rejected() {
    let mut members = files(Count::WithFlag);
    replace(&mut members, "maindoc.xml", b"<DOC><IMAGE name=\"doc\"");
    assert!(rejected(&zip(&members)).contains("Invalid KRA document"));
    replace(&mut members, "maindoc.xml", &[0xff, 0xfe, 0xfd]);
    assert!(rejected(&zip(&members)).contains("UTF-8"));
    replace(&mut members, "maindoc.xml", b"<OTHER/>");
    assert!(rejected(&zip(&members)).contains("DOC"));
    replace(&mut members, "maindoc.xml", b"<DOC><NOT_IMAGE/></DOC>");
    assert!(rejected(&zip(&members)).contains("IMAGE"));

    // kra-d fails on any missing attribute, so every attribute D reads is required.
    for attribute in ["uuid=\"{b}\" ", "filename=\"layer2\" ", "compositeop=\"multiply\" ", "opacity=\"200\" ", "visible=\"1\" opacity=\"200\""] {
        let bad = maindoc_with(attribute, "");
        let error = rejected(&bad);
        assert!(error.contains("missing attribute") || error.contains("filename"), "{attribute}: {error}");
    }
    assert!(rejected(&maindoc_with("width=\"12\"", "width=\"wide\"")).contains("not an integer"));
    assert!(rejected(&maindoc_with("width=\"12\"", "width=\"-12\"")).contains("canvas size"));
    assert!(rejected(&maindoc_with("x=\"1\" y=\"2\"", "x=\"2147483647\" y=\"2\"")).contains("canvas range"));

    // Non-RGBA8 documents and layers.
    assert!(rejected(&maindoc_with("colorspacename=\"RGBA\" mime", "colorspacename=\"RGBA16\" mime")).contains("8-bit"));
    assert!(rejected(&maindoc_with("filename=\"layer1\" colorspacename=\"RGBA\"", "filename=\"layer1\" colorspacename=\"GRAYA\"")).contains("8-bit"));
    let mut members = files(Count::WithFlag);
    replace(&mut members, "doc/layers/layer1", b"VERSION 2\nTILEWIDTH 4\nTILEHEIGHT 4\nPIXELSIZE 8\nDATA 1\n0,0,LZF,3\n\x00ab");
    assert!(rejected(&zip(&members)).contains("8-bit"));
}

#[test]
fn bad_tile_data_is_rejected() {
    let tile = planar(4, 4, |_, _| [1, 2, 3, 255]);
    let good = lzf(&tile);
    let with = |layer: Vec<u8>| {
        let mut members = files(Count::WithFlag);
        replace(&mut members, "doc/layers/layer1", &layer);
        rejected(&zip(&members))
    };
    let header = "VERSION 2\nTILEWIDTH 4\nTILEHEIGHT 4\nPIXELSIZE 4\n";
    let block = |left: &str, packed: &[u8]| {
        let mut out = format!("{header}DATA 1\n{left},LZF,{}\n", packed.len() + 1).into_bytes();
        out.push(1);
        out.extend_from_slice(packed);
        out
    };
    let error = with(block("0,0", &good[..good.len() - 2]));
    assert!(error.contains("LZF"), "{error}");

    // Header problems.
    assert!(with(b"VERSION 2\nTILEWIDTH 4\n".to_vec()).contains("tile header line"));
    assert!(with(b"VERSION 2\nTILEWIDTH 4\nTILEHEIGHT 4\nPIXELSIZE 4\nSIZE 1\n".to_vec()).contains("no DATA"));
    assert!(with(b"VERSION 2\nTILEWIDTH x\nTILEHEIGHT 4\nPIXELSIZE 4\nDATA 1\n".to_vec()).contains("TILEWIDTH"));
    assert!(with(b"VERSION 2\nTILEWIDTH 0\nTILEHEIGHT 4\nPIXELSIZE 4\nDATA 1\n".to_vec()).contains("tile size"));
    assert!(with(b"VERSION 2\nTILEWIDTH 99999\nTILEHEIGHT 4\nPIXELSIZE 4\nDATA 1\n".to_vec()).contains("tile size"));
    assert!(with(b"VERSION 2\nTILEWIDTH 4\nTILEHEIGHT 4\nPIXELSIZE 4\nDATA -1\n".to_vec()).contains("negative"));
    assert!(with(b"VERSION 2\nTILEWIDTH 4\nTILEHEIGHT 4\nPIXELSIZE 4\nDATA 99999999999\n".to_vec()).contains("tile count"));
    assert!(with(format!("{header}DATA 1\n0,0\n").into_bytes()).contains("malformed tile header"));
    assert!(with(format!("{header}DATA 1\nx,0,LZF,5\n").into_bytes()).contains("integer"));
    assert!(with(format!("{header}DATA 1\n0,0,LZF,-5\n").into_bytes()).contains("out of range"));
    assert!(with(format!("{header}DATA 1\n99999999999,0,LZF,5\n").into_bytes()).contains("out of range"));
    assert!(with(format!("{header}DATA 2\n0,0,LZF,3\n\x00abcdefg0,0,LZF,5\n").into_bytes()).contains("next tile header"));
    assert!(with(format!("{header}DATA 3\n").into_bytes()).contains("tile"));

    // LZF streams: a back reference that runs past the tile, a back reference before the start, a stream that
    // ends early, a run cut off by the end of the data, and a back reference that overruns.
    let overrun: Vec<u8> = [vec![31u8], vec![1; 32], vec![0xe0, 31, 0]].concat();
    assert!(with(block("0,0", &overrun)).contains("overruns"));
    let literal_overrun: Vec<u8> = [vec![31u8], vec![1; 32], vec![19], vec![2; 20], vec![19], vec![3; 20]].concat();
    assert!(with(block("0,0", &literal_overrun)).contains("overruns"));
    assert!(with(block("0,0", &[0x20, 0x05])).contains("before the start"));
    assert!(with(block("0,0", &[0x00, 0x01])).contains("ends before"));
    assert!(with(block("0,0", &[0x1f, 1, 2, 3])).contains("truncated"));
    assert!(with(block("0,0", &[0x00, 0x01, 0xe0])).contains("truncated"));
    let long_reference: Vec<u8> = [vec![0u8, 7], vec![0xe0, 255, 0], vec![0xe0, 255, 0]].concat();
    assert!(with(block("0,0", &long_reference)).contains("overruns"));
}

#[test]
fn limits_cap_entries_sizes_pixels_and_layers() {
    let bytes = kra_bytes(Count::WithFlag);
    let parse = |limits: KraLimits| kra::parse(&bytes, &limits).unwrap_err();
    assert!(parse(KraLimits { max_entries: 3, ..KraLimits::default() }).contains("more than 3 entries"));
    assert!(parse(KraLimits { max_entry_bytes: 100, ..KraLimits::default() }).contains("larger than"));
    assert!(parse(KraLimits { max_xml_bytes: 100, ..KraLimits::default() }).contains("maindoc.xml"));
    assert!(parse(KraLimits { max_total_bytes: 200, ..KraLimits::default() }).contains("memory budget"));
    assert!(parse(KraLimits { max_pixels: 90, ..KraLimits::default() }).contains("canvas size"));
    assert!(parse(KraLimits { max_pixels: 95, ..KraLimits::default() }).contains("canvas size"));
    assert!(parse(KraLimits { max_layers: 2, ..KraLimits::default() }).contains("more than 2 layers"));

    // A tile far from the others widens the box past the cap without allocating it.
    let tiles = [(0, 0, planar(4, 4, |_, _| [1, 2, 3, 255])), (4_000_000, 4_000_000, planar(4, 4, |_, _| [1, 2, 3, 255]))];
    let mut members = files(Count::WithFlag);
    replace(&mut members, "doc/layers/layer1", &layer_file(4, 4, &tiles, Count::WithFlag));
    let error = rejected(&zip(&members));
    assert!(error.contains("memory budget"), "{error}");

    // Many tiles on the same spot cost decode work, not memory, and are capped by pixel count.
    let repeated: Vec<(i32, i32, Vec<u8>)> = (0..30).map(|_| (0, 0, planar(4, 4, |_, _| [1, 2, 3, 255]))).collect();
    replace(&mut members, "doc/layers/layer1", &layer_file(4, 4, &repeated, Count::WithFlag));
    let limited = kra::parse(&zip(&members), &KraLimits { max_pixels: 400, ..KraLimits::default() });
    assert!(limited.unwrap_err().contains("memory budget"));

    // Seven 4x4 tile boxes (112 pixels) exceed 100 pixels together although each is small.
    let layer = "<layer name=\"p\" nodetype=\"paintlayer\" uuid=\"u\" filename=\"layer2\" colorspacename=\"RGBA\" visible=\"1\" opacity=\"255\" compositeop=\"normal\" x=\"0\" y=\"0\"/>";
    let doc = format!("<DOC><IMAGE name=\"doc\" width=\"10\" height=\"10\" colorspacename=\"RGBA\"><layers>{}</layers></IMAGE></DOC>", layer.repeat(7));
    let mut members = files(Count::WithFlag);
    replace(&mut members, "maindoc.xml", doc.as_bytes());
    let error = kra::parse(&zip(&members), &KraLimits { max_pixels: 100, ..KraLimits::default() }).unwrap_err();
    assert!(error.contains("memory budget"), "{error}");
    assert_eq!(kra::parse(&zip(&members), &KraLimits::default()).unwrap().layers.len(), 7);

    // Nested groups beyond the depth cap.
    let mut nested = String::new();
    for _ in 0..60 {
        nested.push_str("<layer name=\"g\" nodetype=\"grouplayer\" uuid=\"u\" visible=\"1\" opacity=\"255\" collapsed=\"0\" x=\"0\" y=\"0\"><layers>");
    }
    for _ in 0..60 {
        nested.push_str("</layers></layer>");
    }
    let doc = format!("<DOC><IMAGE name=\"doc\" width=\"1\" height=\"1\" colorspacename=\"RGBA\"><layers>{nested}</layers></IMAGE></DOC>");
    let members = vec![member("mimetype", b"application/x-krita", false), member("maindoc.xml", doc.as_bytes(), true)];
    assert!(rejected(&zip(&members)).contains("nested deeper"));
}

/// A refusal by a limit must say which limit and its value, so a user can tell a file that is too big from a corrupt one.
#[test]
fn refusals_name_the_limit_and_its_value() {
    let bytes = kra_bytes(Count::WithFlag);
    let parse = |limits: KraLimits| kra::parse(&bytes, &limits).unwrap_err();
    assert!(parse(KraLimits { max_entry_bytes: 100, ..KraLimits::default() }).contains("limit 100 bytes"));
    assert!(parse(KraLimits { max_total_bytes: 200, ..KraLimits::default() }).contains("limit 200 bytes"));
    assert!(parse(KraLimits { max_pixels: 90, ..KraLimits::default() }).contains("limit 90 pixels"));

    // Seven 4x4 tile boxes cover 112 pixels in all.
    let layer = "<layer name=\"p\" nodetype=\"paintlayer\" uuid=\"u\" filename=\"layer2\" colorspacename=\"RGBA\" visible=\"1\" opacity=\"255\" compositeop=\"normal\" x=\"0\" y=\"0\"/>";
    let doc = format!("<DOC><IMAGE name=\"doc\" width=\"10\" height=\"10\" colorspacename=\"RGBA\"><layers>{}</layers></IMAGE></DOC>", layer.repeat(7));
    let mut members = files(Count::WithFlag);
    replace(&mut members, "maindoc.xml", doc.as_bytes());
    let error = kra::parse(&zip(&members), &KraLimits { max_pixels: 100, ..KraLimits::default() }).unwrap_err();
    assert!(error.contains("cover 112 pixels, limit 100"), "{error}");

    // A tile far from the others widens one layer's box past the limit.
    let far = [(0, 0, planar(4, 4, |_, _| [1, 2, 3, 255])), (4_000_000, 4_000_000, planar(4, 4, |_, _| [1, 2, 3, 255]))];
    let mut members = files(Count::WithFlag);
    replace(&mut members, "doc/layers/layer1", &layer_file(4, 4, &far, Count::WithFlag));
    let error = rejected(&zip(&members));
    assert!(error.contains("limit 100000000 pixels"), "{error}");

    // Two opaque tiles 8192 pixels apart make a texture 8196 wide, which is over the texture side limit.
    let wide = [(0, 0, planar(4, 4, |_, _| [1, 2, 3, 255])), (8192, 0, planar(4, 4, |_, _| [1, 2, 3, 255]))];
    let mut members = files(Count::WithFlag);
    replace(&mut members, "doc/layers/layer1", &layer_file(4, 4, &wide, Count::WithFlag));
    let error = import_kra(&zip(&members), &ImportOptions::default(), &limits()).unwrap_err();
    assert!(error.contains("8196x4") && error.contains("at most 8192 per side and 16777216 pixels"), "{error}");
}

#[test]
fn import_reports_kra_errors_and_limits_through_the_shared_path() {
    assert!(import_kra(b"not a zip at all", &ImportOptions::default(), &limits()).is_err());
    let bad = ImportOptions { group_node_type: "Mesh".into(), ..ImportOptions::default() };
    assert!(import_kra(&kra_bytes(Count::WithFlag), &bad, &limits()).unwrap_err().contains("group node type"));
    let bad = ImportOptions { first_id: 0, ..ImportOptions::default() };
    assert!(import_kra(&kra_bytes(Count::WithFlag), &bad, &limits()).unwrap_err().contains("first_id"));
    let options = MergeOptions { rename_mapped: false, retranslate_mapped: false, first_id: 0 };
    let model = json!({"nodes": {"uuid": 1, "name": "Root", "type": "Node", "children": []}});
    assert!(merge_kra(&model, &[], &kra_bytes(Count::WithFlag), &options, &limits()).unwrap_err().contains("first_id"));
    let tight = Limits { max_pixels: 10, ..Limits::default() };
    assert!(import_kra(&kra_bytes(Count::WithFlag), &ImportOptions::default(), &tight).is_err());
}

#[test]
fn hostile_bytes_never_panic() {
    let base = files(Count::WithFlag);
    let limits = KraLimits { max_pixels: 1 << 20, ..KraLimits::default() };
    // Truncations of the archive.
    let whole = zip(&base);
    for cut in 0..whole.len() {
        let _ = kra::parse(&whole[..cut], &limits);
    }
    // Every byte of the archive flipped.
    for at in 0..whole.len() {
        let mut bytes = whole.clone();
        bytes[at] ^= 0x5a;
        let _ = kra::parse(&bytes, &limits);
    }
    // Every byte of each inner file mutated and re-zipped with a valid CRC, so the parsers see it.
    for target in 1..base.len() {
        for at in 0..base[target].data.len() {
            for value in [0x00, 0xff, b'0', b'9', b'\n', b',', b'-'] {
                let mut members = files(Count::WithFlag);
                members[target].data[at] = value;
                let _ = kra::parse(&zip(&members), &limits);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Group blend modes and the skipped-layer report
// ---------------------------------------------------------------------------------------------

/// A `size` x `size` KRA whose layer tree is `layers` (top to bottom, as Krita writes it). `tiles` holds the planar
/// pixels of `doc/layers/<file>`, one 4-pixel-wide tile at the origin per file.
fn kra_of(size: usize, layers: &str, tiles: &[(&str, Vec<u8>)]) -> Vec<u8> {
    kra_with(size, layers, tiles, Vec::new())
}

/// [`kra_of`] with more archive members, for example a tile file of another size or a default pixel.
fn kra_with(size: usize, layers: &str, tiles: &[(&str, Vec<u8>)], extra: Vec<Member>) -> Vec<u8> {
    let maindoc = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><DOC xmlns=\"http://www.calligra.org/DTD/krita\" \
         kritaVersion=\"5.2.2\" syntaxVersion=\"2.0\"><IMAGE name=\"doc\" width=\"{size}\" \
         height=\"{size}\" colorspacename=\"RGBA\" mime=\"application/x-kra\"><layers>{layers}</layers></IMAGE></DOC>"
    );
    let mut members =
        vec![member("mimetype", b"application/x-krita", false), member("maindoc.xml", maindoc.as_bytes(), true)];
    for (file, planes) in tiles {
        let data = layer_file(size, size, &[(0, 0, planes.clone())], Count::WithFlag);
        members.push(member(&format!("doc/layers/{file}"), &data, true));
    }
    members.extend(extra);
    zip(&members)
}

fn paint_xml(name: &str, file: &str, op: &str) -> String {
    format!(
        "<layer name=\"{name}\" nodetype=\"paintlayer\" uuid=\"{{{file}}}\" filename=\"{file}\" \
         colorspacename=\"RGBA\" visible=\"1\" opacity=\"255\" compositeop=\"{op}\" x=\"0\" y=\"0\"/>"
    )
}

fn group_xml(name: &str, op: &str, inner: &str) -> String {
    format!(
        "<layer name=\"{name}\" nodetype=\"grouplayer\" uuid=\"{{g-{name}}}\" visible=\"1\" opacity=\"255\" \
         collapsed=\"0\" x=\"0\" y=\"0\" compositeop=\"{op}\"><layers>{inner}</layers></layer>"
    )
}

/// A 4x4 backdrop of one opaque colour (`backdrop`), and `ink`: the left two columns opaque.
fn shade_over_backdrop(group_op: &str) -> Vec<u8> {
    let group = group_xml("Shade", group_op, &paint_xml("Ink", "layer1", "normal"));
    let layers = group + &paint_xml("Backdrop", "layer2", "normal");
    let ink = planar(4, 4, |x, _| [51, 102, 204, if x < 2 { 255 } else { 0 }]);
    let backdrop = planar(4, 4, |_, _| [250, 100, 200, 255]);
    kra_of(4, &layers, &[("layer1", ink), ("layer2", backdrop)])
}

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Runs `import.kra` and `render` (one texel per pixel) through a session; returns the import result and the pixels.
fn import_and_render(bytes: &[u8]) -> (Value, image::RgbaImage) {
    let unique = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("soul2d-kra-{}-{unique}.kra", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    let mut session = Session::new();
    let mut run = |request: Value| {
        let response = session.handle_request(&request);
        assert_eq!(response["succeeded"], true, "{request} -> {response}");
        response["result"].clone()
    };
    let imported = run(json!({"op": "import.kra", "path": path.to_str().unwrap()}))["imported"].clone();
    let rendered = run(json!({"op": "render", "width": 4, "height": 4, "bounds": [-2, -2, 2, 2]}));
    std::fs::remove_file(&path).unwrap();
    let encoded = rendered["png"].as_str().unwrap();
    let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded).unwrap();
    (imported, image::load_from_memory(&png).unwrap().into_rgba8())
}

#[test]
fn a_groups_blend_mode_is_applied_and_rendered() {
    let bytes = shade_over_backdrop("multiply");
    let document = kra::parse(&bytes, &KraLimits::default()).unwrap();
    assert_eq!((document.layers[0].name.as_str(), document.layers[0].blend), ("Shade", "Multiply"));
    let imported = import_kra(&bytes, &ImportOptions::default(), &limits()).unwrap();
    let shade = &imported.model["nodes"]["children"][1];
    assert_eq!(
        (shade["name"].clone(), shade["type"].clone(), shade["blend_mode"].clone()),
        (json!("Shade"), json!("DynamicComposite"), json!("Multiply"))
    );
    assert!(imported.report.is_empty());

    // Multiply is src * dst / 255 per channel. Ink (51, 102, 204) over Backdrop (250, 100, 200):
    // 51 * 250 / 255 = 50, 102 * 100 / 255 = 40, 204 * 200 / 255 = 160. Normal would give Ink's (51, 102, 204).
    // Where Ink is transparent (the right two columns) the group adds nothing and Backdrop shows as it is.
    let (reply, pixels) = import_and_render(&bytes);
    assert_eq!(reply, json!({"nodes": 3, "textures": 2}));
    for (x, y) in [(0, 0), (1, 3)] {
        assert_eq!(pixels.get_pixel(x, y).0, [50, 40, 160, 255], "({x}, {y})");
    }
    for (x, y) in [(2, 0), (3, 3)] {
        assert_eq!(pixels.get_pixel(x, y).0, [250, 100, 200, 255], "({x}, {y})");
    }
}

#[test]
fn a_pass_through_groups_blend_mode_is_ignored() {
    // Krita does not composite the projection of a pass-through group, so its op is not applied and not reported.
    let group = group_xml("Shade", "multiply", &paint_xml("Ink", "layer1", "normal"));
    let through = group.replacen("collapsed=", "passthrough=\"1\" collapsed=", 1);
    assert_ne!(through, group);
    let bytes = kra_of(4, &through, &[("layer1", planar(4, 4, |_, _| [1, 2, 3, 255]))]);
    let document = kra::parse(&bytes, &KraLimits::default()).unwrap();
    assert_eq!((document.layers[0].name.as_str(), document.layers[0].blend), ("Shade", "Normal"));
    let imported = import_kra(&bytes, &ImportOptions::default(), &limits()).unwrap();
    assert_eq!(imported.model["nodes"]["children"][0]["blend_mode"], "Normal");
    assert!(imported.report.is_empty());
}

#[test]
fn a_pass_through_group_with_a_blended_layer_imports_as_a_plain_node() {
    // The same shape as `shade_over_backdrop`, but the group passes through and Ink carries the Multiply.
    let group = group_xml("Shade", "normal", &paint_xml("Ink", "layer1", "multiply"));
    let through = group.replacen("collapsed=", "passthrough=\"1\" collapsed=", 1);
    assert_ne!(through, group);
    let layers = through + &paint_xml("Backdrop", "layer2", "normal");
    let ink = planar(4, 4, |x, _| [51, 102, 204, if x < 2 { 255 } else { 0 }]);
    let backdrop = planar(4, 4, |_, _| [250, 100, 200, 255]);
    let bytes = kra_of(4, &layers, &[("layer1", ink.clone()), ("layer2", backdrop.clone())]);
    let document = kra::parse(&bytes, &KraLimits::default()).unwrap();
    assert_eq!((document.layers[0].name.as_str(), document.layers[0].pass_through), ("Shade", true));
    let imported = import_kra(&bytes, &ImportOptions::default(), &limits()).unwrap();
    let shade = &imported.model["nodes"]["children"][1];
    assert_eq!((shade["name"].clone(), shade["type"].clone()), (json!("Shade"), json!("Node")));
    assert!(imported.report.is_empty());

    // Ink multiplies with Backdrop, which lies below the group. Multiply is src * dst / 255 per channel:
    // 51 * 250 / 255 = 50, 102 * 100 / 255 = 40, 204 * 200 / 255 = 160. Blended in a buffer of its own the layer would
    // find nothing to multiply with. Where Ink is transparent (the right two columns) Backdrop shows as it is.
    let (_, pixels) = import_and_render(&bytes);
    for (x, y) in [(0, 0), (1, 3)] {
        assert_eq!(pixels.get_pixel(x, y).0, [50, 40, 160, 255], "({x}, {y})");
    }
    for (x, y) in [(2, 0), (3, 3)] {
        assert_eq!(pixels.get_pixel(x, y).0, [250, 100, 200, 255], "({x}, {y})");
    }

    // A group without the attribute stays an isolated group: Ink multiplies with an empty buffer and adds nothing.
    let isolated = kra_of(4, &(group + &paint_xml("Backdrop", "layer2", "normal")), &[("layer1", ink), ("layer2", backdrop)]);
    let shade = &import_kra(&isolated, &ImportOptions::default(), &limits()).unwrap().model["nodes"]["children"][1];
    assert_eq!(shade["type"], "DynamicComposite");
    assert_eq!(import_and_render(&isolated).1.get_pixel(0, 0).0, [250, 100, 200, 255]);
}

#[test]
fn a_group_that_cannot_blend_says_so() {
    let bytes = shade_over_backdrop("multiply");
    let flat = ImportOptions { keep_structure: false, ..ImportOptions::default() };
    let expected = json!({"count": 1, "groups": [{"name": "Shade", "blend": "Multiply"}]});
    assert_eq!(import_kra(&bytes, &flat, &limits()).unwrap().report["groupBlendNotApplied"], expected);

    // The renderer nests Composites 16 deep; the 17th group becomes a plain node, which has no blend mode.
    let mut inner = paint_xml("Ink", "layer1", "normal");
    for depth in (0..17).rev() {
        let op = if depth == 16 || depth == 3 { "multiply" } else { "normal" };
        inner = group_xml(&format!("g{depth}"), op, &inner);
    }
    let deep = kra_of(4, &inner, &[("layer1", planar(4, 4, |_, _| [1, 2, 3, 255]))]);
    let report = import_kra(&deep, &ImportOptions::default(), &limits()).unwrap().report;
    assert_eq!(report["groupBlendNotApplied"], json!({"count": 1, "groups": [{"name": "g16", "blend": "Multiply"}]}));
    // g3 is a Composite and carries its mode.
    let mut node = &import_kra(&deep, &ImportOptions::default(), &limits()).unwrap().model["nodes"];
    for depth in 0..=3 {
        node = &node["children"][0];
        assert_eq!(node["name"], format!("g{depth}"));
    }
    assert_eq!(node["blend_mode"], "Multiply");

    // A Normal group needs no note.
    assert!(import_kra(&shade_over_backdrop("normal"), &flat, &limits()).unwrap().report.is_empty());
}

#[test]
fn skipped_layers_are_reported_by_type() {
    let shape = "<layer name=\"Lines\" nodetype=\"shapelayer\" uuid=\"{s}\" visible=\"1\" opacity=\"255\"/>";
    let file = "<layer name=\"Ref\" nodetype=\"filelayer\" uuid=\"{f}\" visible=\"0\" opacity=\"255\"/>";
    // A mask under a layer is never read, so it is not counted.
    let ink = paint_xml("Ink", "layer1", "normal")
        .replace("/>", "><masks><mask name=\"M\" nodetype=\"transparencymask\" visible=\"1\"/></masks></layer>");
    let tiles = [("layer1", planar(4, 4, |_, _| [9, 8, 7, 255]))];
    let bytes = kra_of(4, &format!("{shape}{ink}{file}"), &tiles);
    let expected = json!({
        "visible": true,
        "types": [{"type": "shapelayer", "count": 1}, {"type": "filelayer", "count": 1}],
        "other": 0
    });
    assert_eq!(import_kra(&bytes, &ImportOptions::default(), &limits()).unwrap().report["skippedLayers"], expected);
    let (reply, _) = import_and_render(&bytes);
    assert_eq!(reply, json!({"nodes": 1, "textures": 1, "skippedLayers": expected}));

    // Only a hidden layer is skipped.
    let hidden = kra_of(4, &format!("{ink}{file}"), &tiles);
    let report = import_kra(&hidden, &ImportOptions::default(), &limits()).unwrap().report;
    let types = json!([{"type": "filelayer", "count": 1}]);
    assert_eq!(report["skippedLayers"], json!({"visible": false, "types": types, "other": 0}));

    // The merge plan carries the same report; a file that skipped nothing has no such key.
    let imported = import_kra(&bytes, &ImportOptions::default(), &limits()).unwrap();
    let options = MergeOptions { rename_mapped: false, retranslate_mapped: false, first_id: 100 };
    let plan = merge_kra(&imported.model, &imported.textures, &bytes, &options, &limits()).unwrap();
    assert_eq!(plan.to_json()["skippedLayers"], expected);

    let clean = kra_of(4, &ink, &tiles);
    let imported = import_kra(&clean, &ImportOptions::default(), &limits()).unwrap();
    assert!(imported.report.is_empty());
    let plan = merge_kra(&imported.model, &imported.textures, &clean, &options, &limits()).unwrap();
    assert!(plan.to_json().get("skippedLayers").is_none());
    assert_eq!(import_and_render(&clean).0, json!({"nodes": 1, "textures": 1}));
}

#[test]
fn the_skipped_report_is_bounded() {
    // 20 distinct types and a type name of 100 characters, which is cut to 64.
    let long = "x".repeat(100);
    let mut layers: String = (0..20)
        .map(|n| format!("<layer name=\"v{n}\" nodetype=\"type{n:02}\" uuid=\"{{u{n}}}\" visible=\"0\"/>"))
        .collect();
    layers.push_str(&format!("<layer name=\"l\" nodetype=\"{long}\" uuid=\"{{l}}\" visible=\"0\"/>"));
    layers.push_str(&format!("<layer name=\"m\" nodetype=\"{long}\" uuid=\"{{m}}\" visible=\"0\"/>"));
    let document = kra::parse(&kra_of(4, &layers, &[]), &KraLimits::default()).unwrap();
    let skipped = &document.skipped;
    // The first 16 names are kept (type00..type15); the 4 types after them and the 2 long-named nodes are `other`.
    assert_eq!(skipped.types.len(), 16);
    assert_eq!(skipped.types[15], ("type15".to_owned(), 1));
    assert_eq!((skipped.other, skipped.visible), (6, false));

    let layers = format!("<layer name=\"l\" nodetype=\"{long}\" uuid=\"{{l}}\" visible=\"1\"/>");
    let document = kra::parse(&kra_of(4, &layers, &[]), &KraLimits::default()).unwrap();
    assert_eq!(document.skipped.types, [("x".repeat(64), 1)]);
}

// ---------------------------------------------------------------------------------------------
// The default pixel of a paint layer
// ---------------------------------------------------------------------------------------------

/// An opaque pixel that differs from its neighbours, so that the tile does not compress to its own size, which the
/// reader would take for an uncompressed tile.
fn tile_pixel(x: usize, y: usize) -> [u8; 4] {
    [200 + x as u8, 100 + y as u8, 50, 255]
}

/// The tile file of `doc/layers/<file>`: a `tile` x `tile` tile of [`tile_pixel`]s at each of `at`.
fn tile_file(file: &str, tile: usize, at: &[(i32, i32)]) -> Member {
    let tiles: Vec<(i32, i32, Vec<u8>)> = at.iter().map(|&(x, y)| (x, y, planar(tile, tile, tile_pixel))).collect();
    member(&format!("doc/layers/{file}"), &layer_file(tile, tile, &tiles, Count::WithFlag), true)
}

/// `doc/layers/<file>.defaultpixel`: Krita writes the pixel in memory order, B, G, R, A.
fn default_pixel(file: &str, bytes: &[u8]) -> Member {
    member(&format!("doc/layers/{file}.defaultpixel"), bytes, false)
}

const OPAQUE_DEFAULT: [u8; 4] = [30, 20, 10, 255];

fn assert_pixels(image: &image::RgbaImage, expected: impl Fn(u32, u32) -> [u8; 4]) {
    for (x, y, pixel) in image.enumerate_pixels() {
        assert_eq!(pixel.0, expected(x, y), "({x}, {y})");
    }
}

#[test]
fn a_layer_without_tiles_takes_its_opaque_default_pixel_over_the_canvas() {
    // Krita's raster background: no tiles, an opaque default pixel. B, G, R, A = 30, 20, 10, 255 is RGBA (10, 20, 30, 255).
    let layers = paint_xml("Background", "layer1", "normal");
    let extra = vec![tile_file("layer1", 4, &[]), default_pixel("layer1", &OPAQUE_DEFAULT)];
    let bytes = kra_with(4, &layers, &[], extra);
    let document = kra::parse(&bytes, &KraLimits::default()).unwrap();
    assert!(document.skipped.is_empty());
    let background = &document.layers[0];
    assert_eq!([background.left, background.top, background.right, background.bottom], [0, 0, 4, 4]);
    assert_eq!(background.rgba, [10, 20, 30, 255].repeat(16));

    let (reply, pixels) = import_and_render(&bytes);
    assert_eq!(reply, json!({"nodes": 1, "textures": 1}));
    assert_pixels(&pixels, |_, _| [10, 20, 30, 255]);
}

#[test]
fn default_only_tiles_do_not_expand_the_layer_box() {
    for positions in [vec![(8192, 0)], vec![(-8192, 0), (8192, 0)]] {
        let tiles: Vec<_> = positions.iter().map(|&(x, y)| (x, y, planar(4, 4, |_, _| [255; 4]))).collect();
        let extra = vec![
            member("doc/layers/layer1", &layer_file(4, 4, &tiles, Count::WithFlag), true),
            default_pixel("layer1", &[255; 4]),
        ];
        let bytes = kra_with(4, &paint_xml("Background", "layer1", "normal"), &[], extra);
        let document = kra::parse(&bytes, &KraLimits { max_pixels: 32, ..KraLimits::default() }).unwrap();
        let layer = &document.layers[0];
        assert_eq!([layer.left, layer.top, layer.right, layer.bottom], [0, 0, 4, 4]);
        assert_eq!(layer.rgba, [255; 4].repeat(16));
        let (reply, pixels) = import_and_render(&bytes);
        assert_eq!(reply, json!({"nodes": 1, "textures": 1}));
        assert_pixels(&pixels, |_, _| [255; 4]);
        if positions.len() == 2 {
            assert!(kra::parse(&bytes, &KraLimits { max_pixels: 31, ..KraLimits::default() })
                .unwrap_err().contains("tiles exceed the memory budget"));
        }
    }
}

#[test]
fn raw_default_only_tiles_keep_the_original_bounds_without_a_default() {
    let mut data = b"VERSION 2\nTILEWIDTH 4\nTILEHEIGHT 4\nPIXELSIZE 4\nDATA 1\n8192,0,LZF,65\n".to_vec();
    data.push(0);
    data.extend([255; 64]);
    for has_default in [false, true] {
        let mut extra = vec![member("doc/layers/layer1", &data, false)];
        if has_default {
            extra.push(default_pixel("layer1", &[255; 4]));
        }
        let bytes = kra_with(4, &paint_xml("Background", "layer1", "normal"), &[], extra);
        let document = kra::parse(&bytes, &KraLimits::default()).unwrap();
        let layer = &document.layers[0];
        let expected = if has_default { [0, 0, 4, 4] } else { [8192, 0, 8196, 4] };
        assert_eq!([layer.left, layer.top, layer.right, layer.bottom], expected);
        assert_eq!(layer.rgba, [255; 64]);
    }
}

#[test]
fn a_default_only_tile_keeps_its_overlap_without_expanding_bounds() {
    let tiles = [(0, 0, planar(4, 4, |_, _| [10, 20, 30, 255])),
                 (1, 1, planar(4, 4, |_, _| [255; 4]))];
    let extra = vec![
        member("doc/layers/layer1", &layer_file(4, 4, &tiles, Count::WithFlag), true),
        default_pixel("layer1", &[255; 4]),
    ];
    let bytes = kra_with(4, &paint_xml("Background", "layer1", "normal"), &[], extra);
    let document = kra::parse(&bytes, &KraLimits::default()).unwrap();
    let layer = &document.layers[0];
    assert_eq!([layer.left, layer.top, layer.right, layer.bottom], [0, 0, 4, 4]);
    let (_, pixels) = import_and_render(&bytes);
    assert_pixels(&pixels, |x, y| if x >= 1 && y >= 1 { [255; 4] } else { [10, 20, 30, 255] });
}

#[test]
fn pixels_that_no_tile_covers_take_the_default_pixel() {
    // One 2x2 tile at the top left of a 4x4 canvas; the tile has a hole (a transparent pixel), which the tile owns.
    let tile = planar(2, 2, |x, y| if (x, y) == (1, 1) { [0; 4] } else { tile_pixel(x, y) });
    let extra = vec![
        member("doc/layers/layer1", &layer_file(2, 2, &[(0, 0, tile)], Count::WithFlag), true),
        default_pixel("layer1", &OPAQUE_DEFAULT),
    ];
    let bytes = kra_with(4, &paint_xml("Background", "layer1", "normal"), &[], extra);
    let (reply, pixels) = import_and_render(&bytes);
    assert_eq!(reply, json!({"nodes": 1, "textures": 1}));
    assert_pixels(&pixels, |x, y| match (x < 2 && y < 2, (x, y) == (1, 1)) {
        (_, true) => [0; 4],
        (true, false) => tile_pixel(x as usize, y as usize),
        (false, false) => [10, 20, 30, 255],
    });
}

#[test]
fn a_transparent_missing_or_wrong_sized_default_pixel_leaves_a_tile_less_layer_empty() {
    let layers = paint_xml("Background", "layer1", "normal") + &paint_xml("Ink", "layer2", "normal");
    let none: &[u8] = b"";
    // No entry; alpha 0; one byte short; one byte long; empty. An entry that is not 4 bytes long is not inflated.
    for entry in [None, Some(&[9, 9, 9, 0][..]), Some(&[255, 255, 255][..]), Some(&[255, 255, 255, 255, 255][..]), Some(none)] {
        let mut extra = vec![tile_file("layer1", 4, &[])];
        extra.extend(entry.map(|bytes| default_pixel("layer1", bytes)));
        let bytes = kra_with(4, &layers, &[("layer2", planar(4, 4, |_, _| [1, 2, 3, 255]))], extra);
        let document = kra::parse(&bytes, &KraLimits::default()).unwrap();
        assert_eq!(document.layers.len(), 1, "{entry:?}");
        assert!(document.skipped.is_empty(), "{entry:?}");
        let imported = import_kra(&bytes, &ImportOptions::default(), &limits()).unwrap();
        assert!(imported.report.is_empty(), "{entry:?}");
        assert_eq!(import_and_render(&bytes).0, json!({"nodes": 1, "textures": 1}));
    }
}

#[test]
fn the_default_pixel_counts_towards_the_pixel_budget_before_it_is_filled() {
    // Two canvas-sized layers cover 32 pixels of a 4x4 canvas.
    let layers = paint_xml("A", "layer1", "normal") + &paint_xml("B", "layer2", "normal");
    let extra = || {
        vec![
            tile_file("layer1", 4, &[]),
            default_pixel("layer1", &OPAQUE_DEFAULT),
            tile_file("layer2", 4, &[]),
            default_pixel("layer2", &OPAQUE_DEFAULT),
        ]
    };
    let bytes = kra_with(4, &layers, &[], extra());
    let error = kra::parse(&bytes, &KraLimits { max_pixels: 31, ..KraLimits::default() }).unwrap_err();
    assert!(error.contains("cover 32 pixels, limit 31"), "{error}");
    assert_eq!(kra::parse(&bytes, &KraLimits { max_pixels: 32, ..KraLimits::default() }).unwrap().layers.len(), 2);

    // Two 10000 x 10000 layers (the whole default budget each) are refused without being allocated.
    let bytes = kra_with(10_000, &layers, &[], extra());
    let error = kra::parse(&bytes, &KraLimits::default()).unwrap_err();
    assert!(error.contains("cover 200000000 pixels, limit 100000000"), "{error}");
}

#[test]
fn a_layer_offset_moves_the_canvas_the_default_pixel_covers() {
    // The layer sits at (3, 2). Its tile at (-3, -2) lands at (0, 0) in the document; the canvas in layer pixels is
    // (-3, -2) to (1, 2), so the layer covers exactly the 4x4 canvas.
    let layers = paint_xml("Background", "layer1", "normal").replace("x=\"0\" y=\"0\"", "x=\"3\" y=\"2\"");
    assert!(layers.contains("x=\"3\""));
    for tiled in [false, true] {
        let at: &[(i32, i32)] = if tiled { &[(-3, -2)] } else { &[] };
        let extra = vec![tile_file("layer1", 3, at), default_pixel("layer1", &OPAQUE_DEFAULT)];
        let bytes = kra_with(4, &layers, &[], extra);
        let document = kra::parse(&bytes, &KraLimits::default()).unwrap();
        let layer = &document.layers[0];
        assert_eq!([layer.left, layer.top, layer.right, layer.bottom], [0, 0, 4, 4], "tiled {tiled}");
        let (_, pixels) = import_and_render(&bytes);
        assert_pixels(&pixels, |x, y| {
            if tiled && x < 3 && y < 3 {
                tile_pixel(x as usize, y as usize)
            } else {
                [10, 20, 30, 255]
            }
        });
    }
}

#[test]
fn a_gap_between_the_canvas_and_a_tile_outside_it_takes_the_default_pixel() {
    // Krita's paint device holds the default pixel wherever no tile exists, and its exact bounds are the image joined
    // with every pixel that differs from the default one. The gap (x from 4 to 8) is therefore part of the layer.
    let extra = vec![tile_file("layer1", 3, &[(8, 0)]), default_pixel("layer1", &OPAQUE_DEFAULT)];
    let bytes = kra_with(4, &paint_xml("Background", "layer1", "normal"), &[], extra);
    let document = kra::parse(&bytes, &KraLimits::default()).unwrap();
    let layer = &document.layers[0];
    assert_eq!([layer.left, layer.top, layer.right, layer.bottom], [0, 0, 11, 4]);
    let imported = import_kra(&bytes, &ImportOptions::default(), &limits()).unwrap();
    let texture = image::load_from_memory(&imported.textures[0].data).unwrap().to_rgba8();
    assert_eq!(texture.dimensions(), (11, 4), "import keeps the complete off-canvas bounds");
    assert_eq!(texture.get_pixel(6, 0).0, [10, 20, 30, 255], "off-canvas default pixels survive import");
    for (index, pixel) in layer.rgba.chunks_exact(4).enumerate() {
        let (x, y) = (index % 11, index / 11);
        let expected = if x >= 8 && y < 3 { tile_pixel(x - 8, y) } else { [10, 20, 30, 255] };
        assert_eq!(pixel, expected, "({x}, {y})");
    }
}

// ---------------------------------------------------------------------------------------------
// Blend modes the engine does not have
// ---------------------------------------------------------------------------------------------

/// The `blend_mode` of the node called `name`, wherever it is in the tree.
fn blend_of(node: &Value, name: &str) -> Option<String> {
    if node["name"] == name {
        return node["blend_mode"].as_str().map(str::to_owned);
    }
    node["children"].as_array()?.iter().find_map(|child| blend_of(child, name))
}

#[test]
fn blend_modes_the_engine_lacks_are_reported_with_the_files_own_names() {
    // Paint layers p0..p8 and two groups, top to bottom. `a&#9;b&#10;c` holds a tab and a line feed, which the report drops.
    let long = "x".repeat(100);
    let ops = [
        ("A", "linear_burn"), ("C", "hue"), ("B", "linear_burn"), ("D", "multiply"), ("E", "normal"),
        ("F", long.as_str()), ("G", "a&#9;b&#10;c"),
    ];
    let mut layers: String = ops.iter().enumerate().map(|(n, (name, op))| paint_xml(name, &format!("p{n}"), op)).collect();
    layers += &group_xml("Grp", "hard_mix", &paint_xml("H", "p7", "normal"));
    // A pass-through group blends nothing in a buffer of its own, so its op is not a lost mode.
    let through = group_xml("Through", "hue", &paint_xml("I", "p8", "normal")).replacen("collapsed=", "passthrough=\"1\" collapsed=", 1);
    layers += &through;
    let names: Vec<String> = (0..9).map(|n| format!("p{n}")).collect();
    let tiles: Vec<(&str, Vec<u8>)> = names.iter().map(|name| (name.as_str(), planar(4, 4, |_, _| [9, 8, 7, 255]))).collect();
    let bytes = kra_of(4, &layers, &tiles);

    let expected = json!({
        "count": 6,
        "modes": [
            {"mode": "linear_burn", "count": 2}, {"mode": "hue", "count": 1}, {"mode": "x".repeat(64), "count": 1},
            {"mode": "abc", "count": 1}, {"mode": "hard_mix", "count": 1}
        ],
        "other": 0
    });
    let imported = import_kra(&bytes, &ImportOptions::default(), &limits()).unwrap();
    assert_eq!(imported.report["blendModesNotApplied"], expected);
    // The group's lost mode is not reported twice: the group is a Composite, which is not what `groupBlendNotApplied` is for.
    assert!(imported.report.get("groupBlendNotApplied").is_none());
    for name in ["A", "B", "C", "E", "F", "G", "H", "I", "Grp", "Through"] {
        assert_eq!(blend_of(&imported.model["nodes"], name).as_deref(), Some("Normal"), "{name}");
    }
    assert_eq!(blend_of(&imported.model["nodes"], "D").as_deref(), Some("Multiply"));

    // The reply of `import.kra` and the merge plan carry the same value.
    let (reply, _) = import_and_render(&bytes);
    assert_eq!(reply, json!({"nodes": 11, "textures": 9, "blendModesNotApplied": expected}));
    let options = MergeOptions { rename_mapped: false, retranslate_mapped: false, first_id: 100 };
    let plan = merge_kra(&imported.model, &imported.textures, &bytes, &options, &limits()).unwrap();
    assert_eq!(plan.to_json()["blendModesNotApplied"], expected);

    // A file whose modes the engine has says nothing.
    let mapped = kra_of(4, &(paint_xml("A", "p0", "multiply") + &paint_xml("B", "p1", "normal")), &tiles[..2]);
    let imported = import_kra(&mapped, &ImportOptions::default(), &limits()).unwrap();
    assert!(imported.report.is_empty());
    let plan = merge_kra(&imported.model, &imported.textures, &mapped, &options, &limits()).unwrap();
    assert!(plan.to_json().get("blendModesNotApplied").is_none());
}

#[test]
fn the_blend_mode_report_is_bounded() {
    // 20 distinct unmapped ops: the first 16 are named, the other 4 layers are `other`; the count covers all 20.
    let layers: String = (0..20).map(|n| paint_xml(&format!("L{n}"), "p0", &format!("op{n:02}"))).collect();
    let tiles = [("p0", planar(4, 4, |_, _| [9, 8, 7, 255]))];
    let report = import_kra(&kra_of(4, &layers, &tiles), &ImportOptions::default(), &limits()).unwrap().report;
    let value = &report["blendModesNotApplied"];
    let modes = value["modes"].as_array().unwrap();
    assert_eq!((value["count"].clone(), modes.len(), value["other"].clone()), (json!(20), 16, json!(4)));
    assert_eq!(modes[15], json!({"mode": "op15", "count": 1}));
}
