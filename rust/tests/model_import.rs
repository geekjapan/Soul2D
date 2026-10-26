use serde_json::{json, Value};
use soul2d_core::model_import::{
    apply_merge, import_psd, inx_bytes, merge_psd, Choice, ImportOptions, ImportedModel, Match, MergeOptions,
    MergePlan, Texture,
};
use soul2d_core::psd::Limits;
use soul2d_core::Session;

// Bytes of image-data section appended after the layer and mask section.
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
    luni: Option<Vec<u16>>,
    rect: [i32; 4], // top, left, bottom, right
    channels: Vec<(i16, Vec<u8>)>,
    blend: [u8; 4],
    opacity: u8,
    clipped: u8,
    flags: u8,
    lsct: Option<u32>,
    mask: Vec<u8>,
    extra: Vec<u8>,
}

fn l(name: &'static str) -> L {
    L {
        name,
        luni: None,
        rect: [0; 4],
        channels: Vec::new(),
        blend: *b"norm",
        opacity: 255,
        clipped: 0,
        flags: 0,
        lsct: None,
        mask: Vec::new(),
        extra: Vec::new(),
    }
}

fn block(key: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = [&b"8BIM"[..], key, &be32(payload.len() as u32)].concat();
    out.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        out.push(0);
    }
    out
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn luni_block(units: &[u16]) -> Vec<u8> {
    let mut payload = be32(units.len() as u32).to_vec();
    for unit in units {
        payload.extend(be16(*unit));
    }
    block(b"luni", &payload)
}

fn marker(name: &'static str, kind: u32, flags: u8, opacity: u8) -> L {
    let mut layer = l(name);
    layer.lsct = Some(kind);
    layer.flags = flags;
    layer.opacity = opacity;
    layer.blend = *b"pass";
    layer.channels = (0..3).map(|id| (id, raw(&[]))).chain([(-1, raw(&[]))]).collect();
    layer
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
    out.extend_from_slice(&layer.blend);
    out.extend([layer.opacity, layer.clipped, layer.flags, 0]);
    let mut extra = be32(layer.mask.len() as u32).to_vec();
    extra.extend_from_slice(&layer.mask);
    extra.extend(be32(0));
    extra.push(layer.name.len() as u8);
    extra.extend_from_slice(layer.name.as_bytes());
    while extra.len() % 4 != (4 + layer.mask.len() + 4) % 4 {
        extra.push(0);
    }
    if let Some(kind) = layer.lsct {
        extra.extend(block(b"lsct", &be32(kind)));
    }
    if let Some(units) = &layer.luni {
        extra.extend(luni_block(units));
    }
    extra.extend_from_slice(&layer.extra);
    out.extend(be32(extra.len() as u32));
    out.extend(extra);
    out
}

fn layer_info(layers: &[L], negative: bool) -> Vec<u8> {
    let count = layers.len() as i16;
    let mut out = (if negative { -count } else { count }).to_be_bytes().to_vec();
    for layer in layers {
        out.extend(layer_record(layer));
    }
    for layer in layers {
        for (_, data) in &layer.channels {
            out.extend_from_slice(data);
        }
    }
    if out.len() % 2 == 1 {
        out.push(0);
    }
    out
}

fn section(info: &[u8], tail: &[u8]) -> Vec<u8> {
    let mut body = be32(info.len() as u32).to_vec();
    body.extend_from_slice(info);
    body.extend_from_slice(tail);
    let mut out = be32(body.len() as u32).to_vec();
    out.extend(body);
    out
}

fn header(channels: u16, height: u32, width: u32, depth: u16, mode: u16) -> Vec<u8> {
    let mut out = b"8BPS".to_vec();
    out.extend(be16(1));
    out.extend([0; 6]);
    out.extend(be16(channels));
    out.extend(be32(height));
    out.extend(be32(width));
    out.extend(be16(depth));
    out.extend(be16(mode));
    out
}

// Header, empty colour mode data, a two-resource image resources section, layer section, trailer.
fn psd_with_section(width: u32, height: u32, layer_section: &[u8]) -> Vec<u8> {
    let mut out = header(4, height, width, 8, 3);
    out.extend(be32(0));
    let resource = [&b"8BIM"[..], &be16(1005), &[0, 0], &be32(4), &[1, 2, 3, 4]].concat();
    out.extend(be32(resource.len() as u32));
    out.extend(resource);
    out.extend_from_slice(layer_section);
    out.extend(TRAILER);
    out
}

fn psd(width: u32, height: u32, layers: &[L]) -> Vec<u8> {
    psd_with_section(width, height, &section(&layer_info(layers, false), &[]))
}

fn solid(name: &'static str, rect: [i32; 4], rgba: [u8; 4]) -> L {
    let [top, left, bottom, right] = rect;
    let count = ((bottom - top) * (right - left)) as usize;
    let mut layer = l(name);
    layer.rect = rect;
    layer.channels = (0..4)
        .map(|channel| {
            let id = if channel == 3 { -1 } else { channel as i16 };
            (id, raw(&vec![rgba[channel]; count]))
        })
        .collect();
    layer
}

fn decode(texture: &Texture) -> image::RgbaImage {
    assert_eq!(texture.tag, 0, "imports store PNG");
    image::load_from_memory_with_format(&texture.data, image::ImageFormat::Png).unwrap().into_rgba8()
}

fn names(node: &Value) -> Vec<&str> {
    node["children"].as_array().unwrap().iter().map(|child| child["name"].as_str().unwrap()).collect()
}

fn child<'a>(node: &'a Value, name: &str) -> &'a Value {
    node["children"].as_array().unwrap().iter().find(|child| child["name"] == name).unwrap()
}

/// 10x8 document in real file order (bottom to top; a group's divider precedes its children, its
/// folder record follows them). Top-down: Top, Outer { Inner { Deep }, Mid }, Bottom.
fn nested_fixture() -> Vec<u8> {
    let mut bottom = solid("Bottom", [0, 0, 2, 4], [200, 100, 50, 255]);
    bottom.opacity = 128;
    bottom.blend = *b"mul ";
    let mut mid = solid("Mid", [3, 1, 6, 4], [10, 20, 30, 255]);
    mid.flags = 2;
    mid.blend = *b"scrn";
    let mut deep = solid("Deep", [1, 6, 4, 9], [0, 255, 0, 255]);
    deep.luni = Some(utf16("\u{6df1}\u{3044}\u{2603}\0"));
    deep.blend = *b"diff";
    let mut top = solid("Top", [5, 7, 7, 9], [200, 100, 50, 128]);
    top.blend = *b"hue ";
    psd(
        10,
        8,
        &[
            bottom,
            marker("</Layer group>", 3, 0, 255),
            mid,
            marker("</Layer group>", 3, 0, 255),
            deep,
            marker("Inner", 1, 0, 255),
            marker("Outer", 1, 2, 200),
            top,
        ],
    )
}

fn import_nested(options: &ImportOptions) -> ImportedModel {
    import_psd(&nested_fixture(), options, &Limits::default()).unwrap()
}

fn options(first_id: u32) -> ImportOptions {
    ImportOptions { first_id, ..ImportOptions::default() }
}

const DEEP: &str = "\u{6df1}\u{3044}\u{2603}";

#[test]
fn nested_groups_in_real_file_order_build_the_expected_tree() {
    let imported = import_nested(&options(10));
    let root = &imported.model["nodes"];
    assert_eq!((root["uuid"].clone(), root["name"].clone(), root["type"].clone()), (json!(10), json!("Root"), json!("Node")));
    // D inserts every node at index 0, so siblings end up bottom first.
    assert_eq!(names(root), ["Bottom", "Outer", "Top"]);
    let outer = child(root, "Outer");
    assert_eq!(names(outer), ["Mid", "Inner"]);
    let inner = child(outer, "Inner");
    assert_eq!(names(inner), [DEEP]);
    for (node, kind) in [(outer, "DynamicComposite"), (inner, "Node"), (child(root, "Top"), "Part")] {
        assert_eq!(node["type"], kind, "{}", node["name"]);
    }
    assert_eq!(imported.model["meta"], json!({"name": "", "version": "1.0-alpha"}));
    assert_eq!(imported.model["param"], json!([]));
}

#[test]
fn uuids_zsort_visibility_and_layer_paths() {
    let imported = import_nested(&options(10));
    let root = &imported.model["nodes"];
    let outer = child(root, "Outer");
    let inner = child(outer, "Inner");
    let deep = child(inner, DEEP);
    // (node, uuid, zsort, enabled). Ids follow D's top-down creation order; zsort is the 1-based
    // position of the record counting dividers (Top 1, Outer 2, Inner 3, Deep 4, divider 5, Mid 6,
    // divider 7, Bottom 8).
    let rows = [
        (child(root, "Top"), 11, 1.0, true),
        (outer, 12, 2.0, false),
        // A pass-through group with a blended layer is a plain node: its children keep the stack positions.
        (inner, 13, 0.0, true),
        (deep, 14, 4.0, true),
        (child(outer, "Mid"), 15, 6.0, false),
        (child(root, "Bottom"), 16, 8.0, true),
    ];
    for (node, uuid, zsort, enabled) in rows {
        assert_eq!(node["uuid"], uuid, "{}", node["name"]);
        assert_eq!(node["zsort"], zsort, "{}", node["name"]);
        assert_eq!(node["enabled"], enabled, "{}", node["name"]);
    }
    assert_eq!(deep["psdLayerPath"], format!("/Outer/Inner/{DEEP}"));
    assert_eq!(child(outer, "Mid")["psdLayerPath"], "/Outer/Mid");
    assert_eq!(child(root, "Bottom")["psdLayerPath"], "/Bottom");
}

#[test]
fn blend_modes_and_opacity() {
    let imported = import_nested(&options(10));
    let root = &imported.model["nodes"];
    let outer = child(root, "Outer");
    let expected = [
        (child(root, "Bottom"), "Multiply", 128.0 / 255.0),
        (child(outer, "Mid"), "Screen", 1.0),
        (child(child(outer, "Inner"), DEEP), "Difference", 1.0),
        (child(root, "Top"), "Hue", 1.0),
        // The folder's pass-through falls back to Normal.
        (outer, "Normal", 200.0 / 255.0),
    ];
    for (node, blend, opacity) in expected {
        assert_eq!(node["blend_mode"], blend, "{}", node["name"]);
        let got = node["opacity"].as_f64().unwrap();
        assert!((got - opacity).abs() < 1e-6, "{}: {got} vs {opacity}", node["name"]);
    }
    assert_eq!(child(root, "Top")["opacity"], 1.0);
    assert_eq!(outer["tint"], json!([1.0, 1.0, 1.0]));
}

#[test]
fn part_quads_use_integer_half_extents() {
    let imported = import_nested(&options(10));
    let root = &imported.model["nodes"];
    let outer = child(root, "Outer");
    let uvs = json!([0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0]);
    let bottom = &child(root, "Bottom")["mesh"];
    // 4x2 layer
    assert_eq!(bottom["verts"], json!([-2.0, -1.0, -2.0, 1.0, 2.0, -1.0, 2.0, 1.0]));
    assert_eq!(bottom["uvs"], uvs);
    assert_eq!(bottom["indices"], json!([0, 1, 2, 2, 1, 3]));
    assert_eq!(bottom["origin"], json!([0.0, 0.0]));
    // 3x3 layer: 3 / 2 == 1, so the quad is 2x2 although the texture is 3x3.
    let mid = &child(outer, "Mid")["mesh"];
    assert_eq!(mid["verts"], json!([-1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0]));
    assert_eq!(mid["uvs"], uvs);
    let top = &child(root, "Top")["mesh"];
    assert_eq!(top["verts"], json!([-1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0]));
}

#[test]
fn positions_follow_layer_centres_and_centralized_groups() {
    let imported = import_nested(&options(10));
    let root = &imported.model["nodes"];
    let outer = child(root, "Outer");
    let inner = child(outer, "Inner");
    // Layer centre minus document centre (5, 4); a group sits at the centre of its children's bounds
    // and its children are re-expressed relative to it.
    let rows = [
        (child(root, "Bottom"), [-3.0, -3.0, 0.0]),
        (outer, [-0.5, -1.0, 0.0]),
        (child(outer, "Mid"), [-2.5, 1.0, 0.0]),
        (inner, [2.5, -1.0, 0.0]),
        (child(inner, DEEP), [0.0, 0.0, 0.0]),
        (child(root, "Top"), [3.0, 2.0, 0.0]),
    ];
    for (node, trans) in rows {
        assert_eq!(node["transform"]["trans"], json!(trans), "{}", node["name"]);
    }
    assert_eq!(root["transform"]["trans"], json!([0.0, 0.0, 0.0]));
}

#[test]
fn textures_are_premultiplied_pngs_in_preorder_slots() {
    let imported = import_nested(&options(10));
    let root = &imported.model["nodes"];
    let outer = child(root, "Outer");
    let deep = child(child(outer, "Inner"), DEEP);
    let slots = [
        (child(root, "Bottom"), 0, (4, 2), [200, 100, 50, 255]),
        (child(outer, "Mid"), 1, (3, 3), [10, 20, 30, 255]),
        (deep, 2, (3, 3), [0, 255, 0, 255]),
        // rgb * 128 / 255 rounded down: 200 -> 100, 100 -> 50, 50 -> 25
        (child(root, "Top"), 3, (2, 2), [100, 50, 25, 128]),
    ];
    assert_eq!(imported.textures.len(), 4);
    for (node, slot, size, pixel) in slots {
        assert_eq!(node["textures"], json!([slot]), "{}", node["name"]);
        let image = decode(&imported.textures[slot]);
        assert_eq!((image.width(), image.height()), size, "{}", node["name"]);
        assert_eq!(image.get_pixel(0, 0).0, pixel, "{}", node["name"]);
        assert_eq!(image.get_pixel(size.0 - 1, size.1 - 1).0, pixel, "{}", node["name"]);
    }
}

#[test]
fn flattening_drops_groups_but_keeps_order_paths_and_ids() {
    let imported = import_nested(&ImportOptions { keep_structure: false, ..options(10) });
    let root = &imported.model["nodes"];
    assert_eq!(names(root), ["Bottom", "Mid", DEEP, "Top"]);
    // Ids in processing order: Top, Deep, Mid, Bottom.
    let rows = [("Top", 11, 1.0), (DEEP, 12, 4.0), ("Mid", 13, 6.0), ("Bottom", 14, 8.0)];
    for (name, uuid, zsort) in rows {
        let node = child(root, name);
        assert_eq!((node["uuid"].clone(), node["zsort"].clone(), node["type"].clone()), (json!(uuid), json!(zsort), json!("Part")));
    }
    // The hidden Outer group does not hide its children when the structure is not kept.
    assert_eq!(child(root, DEEP)["enabled"], true);
    assert_eq!(child(root, "Mid")["enabled"], false);
    assert_eq!(child(root, DEEP)["psdLayerPath"], format!("/Outer/Inner/{DEEP}"));
    assert_eq!(child(root, DEEP)["transform"]["trans"], json!([2.0, -2.0, 0.0]));
    assert_eq!(child(root, "Mid")["transform"]["trans"], json!([-3.0, 0.0, 0.0]));
    assert_eq!(imported.textures.len(), 4);
}

#[test]
fn group_node_type_is_configurable_and_checked() {
    let imported = import_nested(&ImportOptions { group_node_type: "Node".into(), ..options(10) });
    let outer = child(&imported.model["nodes"], "Outer");
    assert_eq!(outer["type"], "Node");
    assert!(outer.get("opacity").is_none() && outer.get("blend_mode").is_none());
    let imported = import_nested(&ImportOptions { group_node_type: "Composite".into(), ..options(10) });
    assert_eq!(child(&imported.model["nodes"], "Outer")["type"], "Composite");

    let request = json!({"keepStructure": false, "layerGroupNodeType": "Node"});
    assert_eq!(
        ImportOptions::from_request(&request, 5).unwrap(),
        ImportOptions { keep_structure: false, group_node_type: "Node".into(), first_id: 5, exact_quads: false }
    );
    assert_eq!(ImportOptions::from_request(&json!({"layerGroupNodeType": ""}), 1).unwrap(), ImportOptions::default());
    assert!(ImportOptions::from_request(&json!({"keepStructure": "yes"}), 1).is_err());

    for bad in [
        ImportOptions { group_node_type: "MeshGroup".into(), ..options(1) },
        options(0),
        options(u32::MAX),
        // Seven nodes do not fit below u32::MAX.
        options(u32::MAX - 3),
    ] {
        assert!(import_psd(&nested_fixture(), &bad, &Limits::default()).is_err(), "{bad:?}");
    }
}

#[test]
fn zero_area_layers_make_no_part_but_keep_their_position() {
    let empty = l("Empty");
    let bytes = psd(4, 4, &[solid("Below", [0, 0, 2, 2], [1, 2, 3, 255]), empty, solid("Above", [0, 0, 2, 2], [4, 5, 6, 255])]);
    let imported = import_psd(&bytes, &options(1), &Limits::default()).unwrap();
    let root = &imported.model["nodes"];
    assert_eq!(names(root), ["Below", "Above"]);
    assert_eq!(child(root, "Above")["zsort"], 1.0);
    assert_eq!(child(root, "Below")["zsort"], 3.0);
    assert_eq!(imported.textures.len(), 2);
}

#[test]
fn imported_model_opens_and_renders_in_a_session() {
    // 8x8 document, one 4x4 layer at its centre: world (0, 0), quad -2..2.
    let bytes = psd(8, 8, &[solid("Square", [2, 2, 6, 6], [200, 100, 80, 255])]);
    let imported = import_psd(&bytes, &ImportOptions::default(), &Limits::default()).unwrap();
    let path = std::env::temp_dir().join(format!("soul2d-model-import-{}.inx", std::process::id()));
    std::fs::write(&path, inx_bytes(&imported.model, &imported.textures).unwrap()).unwrap();
    let mut session = Session::new();
    let opened = session.handle_request(&json!({"op": "open", "path": path.to_str().unwrap()}));
    assert_eq!(opened["succeeded"], true, "{opened}");
    let response = session.handle_request(&json!({"op": "render", "width": 8, "height": 8, "bounds": [-4, -4, 4, 4]}));
    assert_eq!(response["succeeded"], true, "{response}");
    let png = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        response["result"]["png"].as_str().unwrap(),
    )
    .unwrap();
    let image = image::load_from_memory(&png).unwrap().into_rgba8();
    assert_eq!(image.get_pixel(0, 0).0, [0, 0, 0, 0]);
    assert_eq!(image.get_pixel(4, 4).0, [200, 100, 80, 255]);
    std::fs::remove_file(path).ok();
}

#[test]
fn hostile_input_is_rejected_without_panicking() {
    let good = nested_fixture();
    let limits = Limits::default();
    assert!(import_psd(b"", &options(1), &limits).is_err());
    assert!(import_psd(b"not a psd at all", &options(1), &limits).is_err());
    for cut in [8, 26, 60, good.len() / 2, good.len() - 20] {
        assert!(import_psd(&good[..cut], &options(1), &limits).is_err(), "truncated at {cut}");
    }
    // Corrupting any single byte must give a result, never a panic.
    for index in 0..good.len() {
        let mut bytes = good.clone();
        bytes[index] ^= 0xff;
        let _ = import_psd(&bytes, &options(1), &limits);
    }

    // A divider with no open group, and a group that is never closed.
    let stray = psd(4, 4, &[solid("A", [0, 0, 1, 1], [1, 1, 1, 255]), marker("</Layer group>", 3, 0, 255)]);
    assert_eq!(import_psd(&stray, &options(1), &limits).unwrap_err(), "Unexpected closing layer group");
    let open = psd(4, 4, &[solid("A", [0, 0, 1, 1], [1, 1, 1, 255]), marker("G", 1, 0, 255)]);
    assert!(import_psd(&open, &options(1), &limits).unwrap_err().contains("never closed"));

    // 49 nested groups exceed the nesting cap; 48 do not.
    let nested = |depth: usize| {
        let mut layers = Vec::new();
        layers.extend((0..depth).map(|_| marker("</Layer group>", 3, 0, 255)));
        layers.push(solid("Leaf", [0, 0, 1, 1], [1, 1, 1, 255]));
        layers.extend((0..depth).map(|_| marker("G", 1, 0, 255)));
        psd(4, 4, &layers)
    };
    assert!(import_psd(&nested(48), &options(1), &limits).is_ok());
    assert!(import_psd(&nested(49), &options(1), &limits).unwrap_err().contains("nested deeper"));

    // A layer wider than any texture the renderer accepts.
    let wide = psd(4, 4, &[solid("Wide", [0, 0, 1, 1], [1, 1, 1, 255]), {
        let mut layer = l("Huge");
        layer.rect = [0, 0, 1, 9000];
        layer.channels = vec![(0, raw(&[]))];
        layer
    }]);
    assert!(import_psd(&wide, &options(1), &limits).unwrap_err().contains("larger than the supported texture size"));

    // Resource limits come from the caller.
    let tiny_document = Limits { max_pixels: 50, ..Limits::default() };
    assert!(import_psd(&good, &options(1), &tiny_document).is_err());
    let tiny_budget = Limits { max_pixels: 10, ..Limits::default() };
    let single = psd(4, 4, &[solid("A", [0, 0, 4, 4], [1, 1, 1, 255])]);
    assert!(import_psd(&single, &options(1), &tiny_budget).is_err());
    let few_layers = Limits { max_layers: 3, ..Limits::default() };
    assert!(import_psd(&good, &options(1), &few_layers).is_err());
    let small_bytes = Limits { max_channel_bytes: 40, ..Limits::default() };
    assert!(import_psd(&single, &options(1), &small_bytes).is_err());
}

// ---------------------------------------------------------------------------------------------
// Merge
// ---------------------------------------------------------------------------------------------

/// 12x10 re-export: Mid (top level), Outer { Inner { Deep } }, Fresh, in real file order.
fn merge_fixture() -> Vec<u8> {
    let mut deep = solid("Deep", [4, 7, 6, 11], [9, 8, 7, 255]);
    deep.luni = Some(utf16("\u{6df1}\u{3044}\u{2603}\0"));
    psd(
        12,
        10,
        &[
            solid("Fresh", [0, 0, 3, 5], [1, 2, 3, 255]),
            marker("</Layer group>", 3, 0, 255),
            marker("</Layer group>", 3, 0, 255),
            deep,
            marker("Inner", 1, 0, 255),
            marker("Outer", 1, 0, 255),
            solid("Mid", [2, 8, 5, 10], [5, 5, 5, 255]),
        ],
    )
}

fn merge_options(rename: bool, retranslate: bool) -> MergeOptions {
    MergeOptions { rename_mapped: rename, retranslate_mapped: retranslate, first_id: 12 }
}

fn plan_for(imported: &ImportedModel, options: &MergeOptions) -> MergePlan {
    merge_psd(&imported.model, &imported.textures, &merge_fixture(), options, &Limits::default()).unwrap()
}

#[test]
fn merge_matches_layers_by_path_then_by_name() {
    let imported = import_nested(&options(10));
    let plan = plan_for(&imported, &merge_options(false, false));
    assert_eq!(plan.document_size, [12, 10]);
    let rows: Vec<_> = plan
        .layers
        .iter()
        .map(|layer| (layer.name.as_str(), layer.path.as_str(), layer.target, layer.matched_by))
        .collect();
    assert_eq!(
        rows,
        [
            // "/Mid" is not "/Outer/Mid", but the last components agree.
            ("Mid", "/Mid", Some(15), Some(Match::Name)),
            (DEEP, "/Outer/Inner/\u{6df1}\u{3044}\u{2603}", Some(14), Some(Match::Path)),
            ("Fresh", "/Fresh", None, None),
        ]
    );
    // Parts in tree order: Bottom 16, Mid 15, Deep 14, Top 11.
    assert_eq!(plan.unmapped_parts, [16, 11]);
    assert_eq!(plan.default_choices(), [Choice::Replace(15), Choice::Replace(14), Choice::Add]);
    assert_eq!((plan.layers[0].left, plan.layers[0].top, plan.layers[0].width, plan.layers[0].height), (8, 2, 2, 3));
    let summary = plan.to_json();
    assert_eq!(summary["document"], json!({"width": 12, "height": 10}));
    assert_eq!(summary["unmappedParts"], json!([16, 11]));
    assert_eq!(
        summary["layers"][0],
        json!({"index": 0, "name": "Mid", "path": "/Mid", "rect": [8, 2, 2, 3], "target": 15, "matchedBy": "name"})
    );
    assert_eq!(summary["layers"][2]["target"], Value::Null);
}

fn part(id: u32, name: &str, path: Option<&str>) -> Value {
    let mut node = json!({
        "uuid": id, "name": name, "type": "Part", "enabled": true, "zsort": 0.0,
        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
        "lockToRoot": false, "pinToMesh": false, "children": [], "textures": [0],
    });
    if let Some(path) = path {
        node["psdLayerPath"] = json!(path);
    }
    node
}

#[test]
fn merge_prefers_an_exact_path_over_an_earlier_name_match() {
    let model = json!({
        "meta": {"name": "", "version": "1.0-alpha"},
        "nodes": {
            "uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
            "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
            "children": [part(2, "One", Some("/Other/Shared")), part(3, "Shared", None)],
        },
        "param": [],
    });
    let bytes = psd(
        4,
        4,
        &[
            solid("Shared", [0, 0, 1, 1], [1, 1, 1, 255]),
            marker("</Layer group>", 3, 0, 255),
            solid("Shared", [0, 0, 1, 1], [2, 2, 2, 255]),
            marker("Z", 1, 0, 255),
        ],
    );
    let options = MergeOptions { first_id: 10, ..merge_options(false, false) };
    let plan = merge_psd(&model, &[], &bytes, &options, &Limits::default()).unwrap();
    let rows: Vec<_> = plan.layers.iter().map(|layer| (layer.path.as_str(), layer.target, layer.matched_by)).collect();
    assert_eq!(
        rows,
        [
            // "/Z/Shared" has no exact match; the first Part whose last component is "Shared" wins.
            ("/Z/Shared", Some(2), Some(Match::Name)),
            // Top-level "/Shared" equals part 3's fallback path "/name", though part 2 matches by name first.
            ("/Shared", Some(3), Some(Match::Path)),
        ]
    );
    assert!(plan.unmapped_parts.is_empty());
}

fn texture_pixel(textures: &[Texture], slot: usize) -> [u8; 4] {
    decode(&textures[slot]).get_pixel(0, 0).0
}

#[test]
fn default_merge_replaces_matched_parts_and_adds_new_layers() {
    let imported = import_nested(&options(10));
    let plan = plan_for(&imported, &merge_options(false, false));
    let merged = apply_merge(&imported.model, &imported.textures, &plan, &plan.default_choices()).unwrap();
    assert_eq!((merged.created.clone(), merged.replaced.clone()), (vec![17], vec![15, 14]));

    let root = &merged.model["nodes"];
    assert_eq!(names(root), ["Bottom", "Outer", "Top", "Fresh"]);
    let before = &imported.model["nodes"];
    // Untouched Parts keep their exact JSON, texture slot included.
    assert_eq!(child(root, "Bottom"), child(before, "Bottom"));
    assert_eq!(child(root, "Top"), child(before, "Top"));

    // Slots are rebuilt in tree order and the two replaced textures are gone: Bottom, Mid', Deep', Top, Fresh.
    assert_eq!(merged.textures.len(), 5);
    assert_eq!(merged.textures[0], imported.textures[0]);
    assert_eq!(merged.textures[3], imported.textures[3]);
    assert_eq!(texture_pixel(&merged.textures, 1), [5, 5, 5, 255]);
    assert_eq!(texture_pixel(&merged.textures, 2), [9, 8, 7, 255]);
    assert_eq!(texture_pixel(&merged.textures, 4), [1, 2, 3, 255]);

    let outer = child(root, "Outer");
    let mid = child(outer, "Mid");
    let old_mid = child(child(before, "Outer"), "Mid");
    assert_eq!(mid["textures"], json!([1]));
    assert_eq!(mid["psdLayerPath"], "/Mid");
    assert_eq!(mid["name"], "Mid");
    // D keeps the old quad and position: the 2x3 texture is stretched over the old 2x2 mesh.
    assert_eq!(mid["mesh"], old_mid["mesh"]);
    assert_eq!(mid["transform"], old_mid["transform"]);
    let deep = child(child(outer, "Inner"), DEEP);
    assert_eq!(deep["textures"], json!([2]));

    let fresh = child(root, "Fresh");
    assert_eq!(fresh["uuid"], 17);
    assert_eq!(fresh["type"], "Part");
    assert_eq!(fresh["textures"], json!([4]));
    assert_eq!(fresh["psdLayerPath"], "/Fresh");
    assert_eq!(fresh["mesh"]["verts"], json!([-2.0, -1.0, -2.0, 1.0, 2.0, -1.0, 2.0, 1.0]));
    assert_eq!(fresh["transform"]["trans"], json!([-4.0, -4.0, 0.0]));
    assert_eq!((fresh["zsort"].clone(), fresh["enabled"].clone(), fresh["blend_mode"].clone()), (json!(0.0), json!(true), json!("Normal")));
    assert_eq!(fresh["opacity"], 1.0);
    // The input is not modified.
    assert_eq!(names(&imported.model["nodes"]), ["Bottom", "Outer", "Top"]);
}

#[test]
fn per_layer_choices_add_ignore_and_remap() {
    let imported = import_nested(&options(10));
    let plan = plan_for(&imported, &merge_options(true, true));
    // Mid is added instead of replaced, Deep is ignored, and Fresh is dragged onto Top.
    let choices = [Choice::Add, Choice::Ignore, Choice::Replace(11)];
    let merged = apply_merge(&imported.model, &imported.textures, &plan, &choices).unwrap();
    assert_eq!((merged.created.clone(), merged.replaced.clone()), (vec![17], vec![11]));
    let root = &merged.model["nodes"];
    assert_eq!(names(root), ["Bottom", "Outer", "Fresh", "Mid"]);

    let top = child(root, "Fresh");
    assert_eq!(top["uuid"], 11);
    assert_eq!(top["psdLayerPath"], "/Fresh");
    // retranslate: the layer's canvas position (-4, -4) in the root's space
    assert_eq!(top["transform"]["trans"], json!([-4.0, -4.0, 0.0]));
    assert_eq!(top["mesh"], child(&imported.model["nodes"], "Top")["mesh"]);

    let added = &root["children"][3];
    assert_eq!((added["uuid"].clone(), added["name"].clone(), added["psdLayerPath"].clone()), (json!(17), json!("Mid"), json!("/Mid")));
    assert_eq!(added["mesh"]["verts"], json!([-1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0]));
    assert_eq!(added["transform"]["trans"], json!([3.0, -2.0, 0.0]));

    // The ignored Deep layer leaves its Part and texture alone; Top's old texture is dropped.
    let deep = child(child(child(root, "Outer"), "Inner"), DEEP);
    assert_eq!(deep, child(child(child(&imported.model["nodes"], "Outer"), "Inner"), DEEP));
    assert_eq!(merged.textures.len(), 5);
    assert_eq!(texture_pixel(&merged.textures, 2), [0, 255, 0, 255]);
    assert_eq!(texture_pixel(&merged.textures, 3), [1, 2, 3, 255]);
    assert_eq!(texture_pixel(&merged.textures, 4), [5, 5, 5, 255]);
    assert_eq!((top["textures"].clone(), added["textures"].clone()), (json!([3]), json!([4])));

    // All layers ignored: nothing changes except that unused slots are compacted away (none are).
    let none = apply_merge(&imported.model, &imported.textures, &plan, &[Choice::Ignore; 3]).unwrap();
    assert_eq!(none.model, imported.model);
    assert_eq!(none.textures, imported.textures);
}

#[test]
fn rename_and_retranslate_use_the_parent_space() {
    let mut imported = import_nested(&options(10));
    let plan = plan_for(&imported, &merge_options(true, true));
    let choices = [Choice::Ignore, Choice::Replace(14), Choice::Ignore];
    // Deep is a child of Inner at canvas (2, -2). The layer wants canvas (3, 0).
    let merged = apply_merge(&imported.model, &imported.textures, &plan, &choices).unwrap();
    let inner = child(child(&merged.model["nodes"], "Outer"), "Inner");
    let deep = child(inner, DEEP);
    assert_eq!(deep["transform"]["trans"], json!([1.0, 2.0, 0.0]));

    // Scale the parent: local = inverse(parent) * world.
    let inner_mut = &mut imported.model["nodes"]["children"][1]["children"][1];
    assert_eq!(inner_mut["name"], "Inner");
    inner_mut["transform"]["scale"] = json!([2.0, 2.0]);
    let merged = apply_merge(&imported.model, &imported.textures, &plan, &choices).unwrap();
    let deep = child(child(child(&merged.model["nodes"], "Outer"), "Inner"), DEEP);
    assert_eq!(deep["transform"]["trans"], json!([0.5, 1.0, 0.0]));

    // lockToRoot parts take the canvas position directly.
    imported.model["nodes"]["children"][1]["children"][1]["children"][0]["lockToRoot"] = json!(true);
    let merged = apply_merge(&imported.model, &imported.textures, &plan, &choices).unwrap();
    let deep = child(child(child(&merged.model["nodes"], "Outer"), "Inner"), DEEP);
    assert_eq!(deep["transform"]["trans"], json!([3.0, 0.0, 0.0]));

    // Without the options only the texture changes.
    let plan = plan_for(&imported, &merge_options(false, false));
    let merged = apply_merge(&imported.model, &imported.textures, &plan, &choices).unwrap();
    let deep = child(child(child(&merged.model["nodes"], "Outer"), "Inner"), DEEP);
    assert_eq!(deep["transform"], child(child(child(&imported.model["nodes"], "Outer"), "Inner"), DEEP)["transform"]);
}

#[test]
fn choices_come_from_a_request_and_bad_ones_are_rejected() {
    let imported = import_nested(&options(10));
    let plan = plan_for(&imported, &merge_options(false, false));
    assert_eq!(plan.choices_from_request(&json!({})).unwrap(), plan.default_choices());
    let request = json!({"choices": [
        {"layer": 2, "action": "replace", "part": 11},
        {"layer": 1, "action": "ignore"},
        {"layer": 0, "action": "add"},
    ]});
    assert_eq!(plan.choices_from_request(&request).unwrap(), [Choice::Add, Choice::Ignore, Choice::Replace(11)]);
    for bad in [
        json!({"choices": {}}),
        json!({"choices": [{"layer": 3, "action": "add"}]}),
        json!({"choices": [{"layer": 0, "action": "add"}, {"layer": 0, "action": "ignore"}]}),
        json!({"choices": [{"layer": 0, "action": "replace"}]}),
        json!({"choices": [{"layer": 0, "action": "merge"}]}),
    ] {
        assert!(plan.choices_from_request(&bad).is_err(), "{bad}");
    }

    let defaults = plan.default_choices();
    assert!(apply_merge(&imported.model, &imported.textures, &plan, &defaults[..2]).is_err());
    let unknown = [Choice::Replace(999), Choice::Add, Choice::Add];
    assert!(apply_merge(&imported.model, &imported.textures, &plan, &unknown).unwrap_err().contains("999"));
    let group = [Choice::Replace(12), Choice::Add, Choice::Add];
    assert!(apply_merge(&imported.model, &imported.textures, &plan, &group).unwrap_err().contains("not a Part"));

    let mut tilted = imported.model.clone();
    tilted["nodes"]["children"][1]["children"][1]["transform"]["rot"] = json!([0.5, 0.0, 0.0]);
    let retranslate = plan_for(&imported, &merge_options(false, true));
    let deep_only = [Choice::Ignore, Choice::Replace(14), Choice::Ignore];
    assert!(apply_merge(&tilted, &imported.textures, &retranslate, &deep_only).is_err());

    // A slot that does not exist in the file cannot be compacted.
    let short = &imported.textures[..2];
    assert!(apply_merge(&imported.model, short, &plan, &defaults).is_err());
}

#[test]
fn merge_rejects_corrupt_and_oversized_input() {
    let imported = import_nested(&options(10));
    let good = merge_fixture();
    let limits = Limits::default();
    let options = merge_options(false, false);
    let run = |bytes: &[u8], limits: &Limits| merge_psd(&imported.model, &imported.textures, bytes, &options, limits);
    assert!(run(b"", &limits).is_err());
    assert!(run(&good[..good.len() / 2], &limits).is_err());
    for index in 0..good.len() {
        let mut bytes = good.clone();
        bytes[index] ^= 0xff;
        let _ = run(&bytes, &limits);
    }
    assert!(run(&good, &Limits { max_pixels: 30, ..Limits::default() }).is_err());
    // The existing textures count against the byte budget.
    let used: usize = imported.textures.iter().map(|t| t.data.len()).sum();
    assert!(run(&good, &Limits { max_channel_bytes: used as u64, ..Limits::default() }).is_err());
    assert!(run(&good, &Limits { max_channel_bytes: used as u64 - 1, ..Limits::default() }).is_err());
    let wide = psd(4, 4, &[{
        let mut layer = l("Huge");
        layer.rect = [0, 0, 1, 9000];
        layer.channels = vec![(0, raw(&[]))];
        layer
    }]);
    assert!(run(&wide, &limits).unwrap_err().contains("larger than the supported texture size"));
    assert!(merge_psd(&imported.model, &[], &good, &MergeOptions { first_id: 0, ..options.clone() }, &limits).is_err());
    assert!(merge_psd(&json!({}), &[], &good, &options, &limits).is_err());
}

#[test]
fn merged_model_keeps_its_thumbnail_texture() {
    let mut imported = import_nested(&options(10));
    // Slot 1 is Mid's texture; the default merge replaces it, so only the thumbnail still uses it.
    imported.model["meta"]["thumbnailId"] = json!(1);
    let plan = plan_for(&imported, &merge_options(false, false));
    let merged = apply_merge(&imported.model, &imported.textures, &plan, &plan.default_choices()).unwrap();
    assert_eq!(merged.textures.len(), 6);
    assert_eq!(merged.model["meta"]["thumbnailId"], 5);
    assert_eq!(merged.textures[5], imported.textures[1]);
}

// ---------------------------------------------------------------------------------------------
// Clipping masks and grouped rendering
// ---------------------------------------------------------------------------------------------

fn clipped(mut layer: L) -> L {
    layer.clipped = 1;
    layer
}

fn masks(node: &Value) -> Vec<(u64, &str)> {
    node["masks"]
        .as_array()
        .map(|list| list.iter().map(|m| (m["source"].as_u64().unwrap(), m["mode"].as_str().unwrap())).collect())
        .unwrap_or_default()
}

fn id_of(node: &Value) -> u64 {
    node["uuid"].as_u64().unwrap()
}

/// Imports `bytes` with `options` into a session, renders the 8x8 canvas 1:1 and returns its pixels row by row.
fn render_import(tag: &str, bytes: &[u8], options: &ImportOptions) -> Vec<[u8; 4]> {
    let imported = import_psd(bytes, options, &Limits::default()).unwrap();
    render_model(tag, &imported.model, &imported.textures, 8, json!({}))
}

/// Renders the 8x8 canvas of `model` at `size` pixels with the given scene `deformations`.
fn render_model(tag: &str, model: &Value, textures: &[Texture], size: u32, deformations: Value) -> Vec<[u8; 4]> {
    let path = std::env::temp_dir().join(format!("soul2d-clip-{}-{tag}.inx", std::process::id()));
    std::fs::write(&path, inx_bytes(model, textures).unwrap()).unwrap();
    let mut session = Session::new();
    let opened = session.handle_request(&json!({"op": "open", "path": path.to_str().unwrap()}));
    assert_eq!(opened["succeeded"], true, "{opened}");
    let response = session.handle_request(
        &json!({"op": "render", "width": size, "height": size, "bounds": [-4, -4, 4, 4], "deformations": deformations}),
    );
    std::fs::remove_file(path).ok();
    assert_eq!(response["succeeded"], true, "{response}");
    let png = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        response["result"]["png"].as_str().unwrap(),
    )
    .unwrap();
    image::load_from_memory(&png).unwrap().into_rgba8().pixels().map(|pixel| pixel.0).collect()
}

const NONE: [u8; 4] = [0, 0, 0, 0];
const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

fn at(pixels: &[[u8; 4]], x: usize, y: usize) -> [u8; 4] {
    pixels[y * 8 + x]
}

#[test]
fn a_clipped_layer_is_masked_by_the_layer_below_it() {
    // File order is bottom to top: a 4x4 base in the top left corner, then a full canvas layer clipped to it.
    let bytes = psd(
        8,
        8,
        &[solid("Base", [0, 0, 4, 4], [255, 0, 0, 255]), clipped(solid("Shade", [0, 0, 8, 8], [0, 255, 0, 255]))],
    );
    let imported = import_psd(&bytes, &options(1), &Limits::default()).unwrap();
    let root = &imported.model["nodes"];
    assert_eq!(masks(child(root, "Shade")), [(id_of(child(root, "Base")), "Mask")]);
    assert!(child(root, "Base").get("masks").is_none());

    let pixels = render_import("one", &bytes, &options(1));
    assert_eq!(at(&pixels, 0, 0), GREEN);
    assert_eq!(at(&pixels, 3, 3), GREEN);
    assert_eq!(at(&pixels, 4, 0), NONE);
    assert_eq!(at(&pixels, 0, 4), NONE);
    assert_eq!(at(&pixels, 7, 7), NONE);
}

#[test]
fn consecutive_clipped_layers_share_the_base_and_an_unclipped_one_ends_the_run() {
    // Base 4x4 red; a strip along the top rows and one along columns 2..4, both clipped to it; a free layer above.
    let bytes = psd(
        8,
        8,
        &[
            solid("Base", [0, 0, 4, 4], [255, 0, 0, 255]),
            clipped(solid("Strip", [0, 0, 2, 8], [0, 0, 255, 255])),
            clipped(solid("Column", [0, 2, 8, 4], [0, 255, 0, 255])),
            solid("Free", [6, 6, 8, 8], [0, 0, 255, 255]),
        ],
    );
    let imported = import_psd(&bytes, &options(1), &Limits::default()).unwrap();
    let root = &imported.model["nodes"];
    let base = id_of(child(root, "Base"));
    assert_eq!(masks(child(root, "Strip")), [(base, "Mask")]);
    assert_eq!(masks(child(root, "Column")), [(base, "Mask")], "the base is not the layer just below");
    assert!(child(root, "Free").get("masks").is_none());

    let pixels = render_import("run", &bytes, &options(1));
    assert_eq!(at(&pixels, 0, 0), BLUE, "strip over the base");
    assert_eq!(at(&pixels, 2, 0), GREEN, "the upper clipped layer is on top");
    assert_eq!(at(&pixels, 3, 3), GREEN);
    assert_eq!(at(&pixels, 0, 3), RED, "base only");
    assert_eq!(at(&pixels, 5, 0), NONE, "the strip does not spill past the base");
    assert_eq!(at(&pixels, 3, 6), NONE, "the column does not spill past the base");
    assert_eq!(at(&pixels, 7, 7), BLUE, "an unclipped layer is drawn everywhere");
}

#[test]
fn clipping_stays_inside_its_group() {
    // The layer below the group must not become the base of a layer clipped at the bottom of the group.
    let bytes = psd(
        8,
        8,
        &[
            solid("Outside", [0, 0, 4, 4], [255, 0, 0, 255]),
            marker("</Layer group>", 3, 0, 255),
            clipped(solid("Orphan", [0, 0, 8, 8], [0, 255, 0, 255])),
            marker("G", 1, 0, 255),
        ],
    );
    let imported = import_psd(&bytes, &options(1), &Limits::default()).unwrap();
    let group = child(&imported.model["nodes"], "G");
    assert!(child(group, "Orphan").get("masks").is_none());
    let pixels = render_import("orphan", &bytes, &options(1));
    assert_eq!(at(&pixels, 7, 7), GREEN, "drawn unclipped, not cut to the layer outside the group");
    assert_eq!(at(&pixels, 0, 0), GREEN);
}

#[test]
fn a_layer_clipped_to_a_group_is_masked_by_the_group_or_by_its_parts() {
    // Group G holds red at columns 0..2 and blue at columns 4..6 (rows 0..4); the shade is clipped to the group.
    let bytes = psd(
        8,
        8,
        &[
            marker("</Layer group>", 3, 0, 255),
            solid("Left", [0, 0, 4, 2], [255, 0, 0, 255]),
            solid("Right", [0, 4, 4, 6], [0, 0, 255, 255]),
            marker("G", 1, 0, 255),
            clipped(solid("Shade", [0, 0, 8, 8], [0, 255, 0, 255])),
        ],
    );
    let expect = |pixels: &[[u8; 4]]| {
        assert_eq!(at(pixels, 0, 0), GREEN);
        assert_eq!(at(pixels, 1, 3), GREEN);
        assert_eq!(at(pixels, 2, 0), NONE, "the gap between the two parts");
        assert_eq!(at(pixels, 5, 2), GREEN);
        assert_eq!(at(pixels, 0, 4), NONE, "below the group");
        assert_eq!(at(pixels, 7, 7), NONE);
    };
    // The group is a DynamicComposite by default: it is the mask source.
    let imported = import_psd(&bytes, &options(1), &Limits::default()).unwrap();
    let root = &imported.model["nodes"];
    assert_eq!(masks(child(root, "Shade")), [(id_of(child(root, "G")), "Mask")]);
    expect(&render_import("group", &bytes, &options(1)));
    // A plain Node cannot be a mask source, so every Part below it is one.
    let plain = ImportOptions { group_node_type: "Node".into(), ..options(1) };
    let imported = import_psd(&bytes, &plain, &Limits::default()).unwrap();
    let root = &imported.model["nodes"];
    let group = child(root, "G");
    let mut sources: Vec<u64> = masks(child(root, "Shade")).iter().map(|(source, _)| *source).collect();
    sources.sort();
    let mut parts = [id_of(child(group, "Left")), id_of(child(group, "Right"))];
    parts.sort();
    assert_eq!(sources, parts);
    expect(&render_import("node", &bytes, &plain));
    // The same when the structure is flattened.
    expect(&render_import("flat", &bytes, &ImportOptions { keep_structure: false, ..options(1) }));
}

#[test]
fn a_group_with_a_blend_mode_and_partial_opacity_renders_through_the_normal_path() {
    // Opaque background (200, 100, 50) and a Multiply group at 128/255 holding an opaque (100, 200, 255) layer.
    let mut group = marker("G", 1, 0, 128);
    group.blend = *b"mul ";
    let bytes = psd(
        8,
        8,
        &[
            solid("Back", [0, 0, 8, 8], [200, 100, 50, 255]),
            marker("</Layer group>", 3, 0, 255),
            solid("Inner", [2, 2, 6, 6], [100, 200, 255, 255]),
            group,
        ],
    );
    let dynamic = render_import("dyn", &bytes, &options(1));
    assert_eq!(at(&dynamic, 0, 0), [200, 100, 50, 255]);
    // Multiply of the group at opacity a over the back: back * (1 - a + a * inner), channel by channel.
    assert_eq!(at(&dynamic, 3, 3), [139, 89, 50, 255]);
    // Composite groups are drawn by the same code and give the same picture.
    let composite = ImportOptions { group_node_type: "Composite".into(), ..options(1) };
    assert_eq!(render_import("comp", &bytes, &composite), dynamic);
}

/// File order is bottom to top; every case has an 8x8 canvas.
fn unmasked_square(name: &'static str, side: i32, rgba: [u8; 4]) -> L {
    solid(name, [0, 0, side, side], rgba)
}

#[test]
fn a_clipped_group_keeps_its_clip_in_all_three_import_shapes() {
    // A 4x4 base in the top left corner; a group clipped to it holds a full canvas green layer.
    let bytes = psd(
        8,
        8,
        &[
            unmasked_square("Base", 4, [255, 0, 0, 255]),
            marker("</Layer group>", 3, 0, 255),
            solid("Fill", [0, 0, 8, 8], [0, 255, 0, 255]),
            clipped(marker("G", 1, 0, 255)),
        ],
    );
    let node = ImportOptions { group_node_type: "Node".into(), ..options(1) };
    let flat = ImportOptions { keep_structure: false, ..options(1) };
    for (tag, shape) in [("cgroup", options(1)), ("cnode", node), ("cflat", flat)] {
        let pixels = render_import(tag, &bytes, &shape);
        assert_eq!(at(&pixels, 0, 0), GREEN, "{tag}: inside the base");
        assert_eq!(at(&pixels, 3, 3), GREEN, "{tag}: inside the base");
        assert_eq!(at(&pixels, 4, 4), NONE, "{tag}: outside the base");
        assert_eq!(at(&pixels, 7, 7), NONE, "{tag}: outside the base");
    }
}

#[test]
fn a_hidden_layer_below_a_base_group_does_not_widen_the_clip() {
    // Group G: a red 4x2 part and a hidden full canvas part; the shade is clipped to G.
    let mut hidden = solid("Hidden", [0, 0, 8, 8], [0, 0, 255, 255]);
    hidden.flags = 2;
    let bytes = psd(
        8,
        8,
        &[
            marker("</Layer group>", 3, 0, 255),
            hidden,
            solid("Left", [0, 0, 4, 2], [255, 0, 0, 255]),
            marker("G", 1, 0, 255),
            clipped(solid("Shade", [0, 0, 8, 8], [0, 255, 0, 255])),
        ],
    );
    let node = ImportOptions { group_node_type: "Node".into(), ..options(1) };
    let flat = ImportOptions { keep_structure: false, ..options(1) };
    for (tag, shape) in [("hgroup", options(1)), ("hnode", node), ("hflat", flat)] {
        let pixels = render_import(tag, &bytes, &shape);
        assert_eq!(at(&pixels, 0, 0), GREEN, "{tag}: inside the visible part");
        assert_eq!(at(&pixels, 1, 3), GREEN, "{tag}: inside the visible part");
        assert_eq!(at(&pixels, 5, 5), NONE, "{tag}: the hidden part is not a mask");
        assert_eq!(at(&pixels, 4, 1), NONE, "{tag}: the hidden part is not a mask");
    }
}

#[test]
fn a_clipped_layer_is_hidden_with_a_base_that_shows_nothing_and_keeps_its_mask() {
    let mut base = unmasked_square("Base", 4, [255, 0, 0, 255]);
    base.flags = 2;
    let bytes = psd(8, 8, &[base, clipped(solid("Shade", [0, 0, 8, 8], [0, 255, 0, 255]))]);
    let imported = import_psd(&bytes, &options(1), &Limits::default()).unwrap();
    let root = &imported.model["nodes"];
    let shade = child(root, "Shade");
    assert_eq!(shade["enabled"], false, "Photoshop hides the clipped layers with their base");
    assert_eq!(masks(shade), [(id_of(child(root, "Base")), "Mask")], "re-enabling the base restores the clip");
    assert!(render_import("hbase", &bytes, &options(1)).iter().all(|pixel| *pixel == NONE));

    // An empty layer has no node; what is clipped to it has nothing to show either.
    let bytes = psd(8, 8, &[l("Empty"), clipped(solid("Shade", [0, 0, 8, 8], [0, 255, 0, 255]))]);
    let imported = import_psd(&bytes, &options(1), &Limits::default()).unwrap();
    assert_eq!(child(&imported.model["nodes"], "Shade")["enabled"], false);
    assert!(render_import("ebase", &bytes, &options(1)).iter().all(|pixel| *pixel == NONE));
}

/// A group of `parts` one pixel parts with `clips` one pixel layers clipped to it.
fn bound_group(parts: usize, clips: usize) -> Vec<u8> {
    let mut layers = vec![marker("</Layer group>", 3, 0, 255)];
    layers.extend((0..parts).map(|_| unmasked_square("p", 1, [255, 0, 0, 255])));
    layers.push(marker("G", 1, 0, 255));
    layers.extend((0..clips).map(|_| clipped(unmasked_square("c", 1, [0, 255, 0, 255]))));
    psd(8, 8, &layers)
}

#[test]
fn clipping_refuses_more_mask_bindings_than_the_render_limit() {
    // 1000 parts with 1001 layers clipped to the group: 1,001,000 bindings when the group is a plain node or
    // flattened, just over the render limit of 1,000,000 (MAX_PACKET_MASK_REFERENCES).
    let bytes = bound_group(1000, 1001);
    let plain = ImportOptions { group_node_type: "Node".into(), ..options(1) };
    let error = import_psd(&bytes, &plain, &Limits::default()).unwrap_err();
    assert!(error.contains("MAX_PACKET_MASK_REFERENCES") && error.contains("1000000"), "{error}");
    let flat = ImportOptions { keep_structure: false, ..options(1) };
    assert!(import_psd(&bytes, &flat, &Limits::default()).unwrap_err().contains("1000000"));
    // A group node is one mask source, so the same file imports with the default group type.
    let imported = import_psd(&bytes, &options(1), &Limits::default()).unwrap();
    assert_eq!(imported.model["nodes"]["children"].as_array().unwrap().len(), 1002);
    // One layer fewer is exactly the limit and is accepted.
    assert!(import_psd(&bound_group(1000, 1000), &plain, &Limits::default()).is_ok());
}

#[test]
fn a_run_of_clipped_layers_rasterises_its_base_once() {
    // A group base and 130 clipped layers, rendered on a 1024x1024 target. The base is a group, so every mask raster
    // draws its whole scope and charges a full target; 130 of them pass the 100-million-pixel work limit that the
    // single raster of the run stays far below.
    let mut layers = vec![
        marker("</Layer group>", 3, 0, 255),
        unmasked_square("Base", 4, [255, 0, 0, 255]),
        marker("G", 1, 0, 255),
    ];
    let shade = |i: usize| [i as u8, 0, 255 - i as u8, 255];
    layers.extend((0..130).map(|i| clipped(solid("S", [0, 2 * (i % 4) as i32, 8, 2 * (i % 4) as i32 + 2], shade(i)))));
    let bytes = psd(8, 8, &layers);
    let imported = import_psd(&bytes, &options(1), &Limits::default()).unwrap();
    let pixels = render_model("run130", &imported.model, &imported.textures, 1024, json!({}));
    // The strips are two canvas pixels wide; the last layer of each strip is on top. 128 px per canvas pixel; look at
    // the middle of a pixel.
    let top = |strip: usize| shade((0..130).rev().find(|i| i % 4 == strip).unwrap());
    let sample = |x: usize, y: usize| pixels[(y * 128 + 64) * 1024 + x * 128 + 64];
    for x in 0..4 {
        assert_eq!(sample(x, 1), top(x / 2), "column {x} inside the base");
        assert_eq!(sample(x, 6), NONE, "column {x} below the base");
    }
    assert_eq!(sample(5, 1), NONE, "column 5 is outside the base");
}

fn nested_groups(depth: usize) -> Vec<u8> {
    let mut layers: Vec<L> = (0..depth).map(|_| marker("</Layer group>", 3, 0, 255)).collect();
    layers.push(solid("Leaf", [0, 0, 8, 8], [0, 255, 0, 255]));
    layers.extend((0..depth).map(|_| marker("G", 1, 0, 255)));
    psd(8, 8, &layers)
}

fn group_types(model: &Value) -> Vec<String> {
    let mut types = Vec::new();
    let mut node = &model["nodes"];
    while let Some(next) = node["children"].as_array().and_then(|list| list.first()) {
        if next["name"] != "G" {
            break;
        }
        types.push(next["type"].as_str().unwrap().to_owned());
        node = next;
    }
    types
}

#[test]
fn groups_nested_past_the_composite_limit_become_plain_nodes_and_still_render() {
    // The renderer nests 16 composites. The 17th group and the ones below it import as plain nodes.
    for (depth, composites) in [(16, 16), (17, 16), (20, 16)] {
        let imported = import_psd(&nested_groups(depth), &options(1), &Limits::default()).unwrap();
        let types = group_types(&imported.model);
        assert_eq!(types.len(), depth);
        assert_eq!(types.iter().filter(|kind| *kind == "DynamicComposite").count(), composites, "{depth}: {types:?}");
        assert!(types[composites..].iter().all(|kind| kind == "Node"), "{depth}: {types:?}");
        let pixels = render_model(&format!("deep{depth}"), &imported.model, &imported.textures, 8, json!({}));
        assert_eq!(at(&pixels, 4, 4), GREEN, "depth {depth}");
    }
}

#[test]
fn an_imported_group_follows_its_deformer_like_a_composite() {
    // A red 2x2 square in the top left corner of the 8x8 canvas inside a group, under a GridDeformer moved 2 px right.
    let bytes = psd(
        8,
        8,
        &[
            marker("</Layer group>", 3, 0, 255),
            unmasked_square("Dot", 2, [255, 0, 0, 255]),
            marker("G", 1, 0, 255),
        ],
    );
    let identity = json!({"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]});
    let under_grid = |model: &Value| {
        let mut model = model.clone();
        let content = model["nodes"]["children"].take();
        model["nodes"]["children"] = json!([{
            "uuid": 900, "name": "Grid", "type": "GridDeformer", "dynamic": false, "enabled": true,
            "transform": identity, "grid_axis_x": [-4.0, 4.0], "grid_axis_y": [-4.0, 4.0], "children": content,
        }]);
        model
    };
    let moved = json!({"900": [[2.0, 0.0], [2.0, 0.0], [2.0, 0.0], [2.0, 0.0]]});
    for group_type in ["DynamicComposite", "Composite"] {
        let shape = ImportOptions { group_node_type: group_type.into(), ..options(1) };
        let imported = import_psd(&bytes, &shape, &Limits::default()).unwrap();
        let model = under_grid(&imported.model);
        let rest = render_model("grid0", &model, &imported.textures, 8, json!({}));
        assert_eq!(at(&rest, 0, 0), RED, "{group_type}");
        assert_eq!(at(&rest, 2, 0), NONE, "{group_type}");
        let pixels = render_model("grid2", &model, &imported.textures, 8, moved.clone());
        assert_eq!(at(&pixels, 0, 0), NONE, "{group_type}: the square left its place");
        assert_eq!(at(&pixels, 2, 0), RED, "{group_type}: the group moved with the deformer");
        assert_eq!(at(&pixels, 3, 1), RED, "{group_type}");
        assert_eq!(at(&pixels, 4, 0), NONE, "{group_type}");
    }
    // A DynamicComposite that keeps a three vertex mesh (converted from a triangle Part) cannot be warped by a
    // deformer; it must still render, unwarped, instead of failing the whole render.
    let imported = import_psd(&bytes, &options(1), &Limits::default()).unwrap();
    let mut model = under_grid(&imported.model);
    let group = &mut model["nodes"]["children"][0]["children"][0];
    group["auto_resized"] = json!(false);
    group["mesh"] = json!({"verts": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0]});
    let pixels = render_model("tri", &model, &imported.textures, 8, moved);
    assert_eq!(at(&pixels, 0, 0), RED);
}

#[test]
fn import_psd_operation_stores_the_masks_and_a_session_renders_them() {
    let bytes = psd(
        8,
        8,
        &[unmasked_square("Base", 4, [255, 0, 0, 255]), clipped(solid("Shade", [0, 0, 8, 8], [0, 255, 0, 255]))],
    );
    let path = std::env::temp_dir().join(format!("soul2d-clip-{}-op.psd", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    let mut session = Session::new();
    let imported = session.handle_request(&json!({"op": "import.psd", "path": path.to_str().unwrap()}));
    std::fs::remove_file(path).ok();
    assert_eq!(imported["succeeded"], true, "{imported}");
    let snapshot = session.snapshot();
    let root = &snapshot["model"]["nodes"];
    assert_eq!(masks(child(root, "Shade")), [(id_of(child(root, "Base")), "Mask")]);
    let response = session.handle_request(&json!({"op": "render", "width": 8, "height": 8, "bounds": [-4, -4, 4, 4]}));
    assert_eq!(response["succeeded"], true, "{response}");
    let png = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        response["result"]["png"].as_str().unwrap(),
    )
    .unwrap();
    let pixels: Vec<[u8; 4]> = image::load_from_memory(&png).unwrap().into_rgba8().pixels().map(|pixel| pixel.0).collect();
    assert_eq!((at(&pixels, 1, 1), at(&pixels, 6, 6)), (GREEN, NONE));
}

#[test]
fn a_pass_through_group_with_a_blended_layer_blends_against_what_is_below_it() {
    // Bottom to top: a grey canvas, then a group holding a red Multiply layer. Photoshop multiplies it into the grey.
    let group = |opacity: u8, blend: &[u8; 4]| {
        let mut multiply = unmasked_square("Mul", 8, [255, 0, 0, 255]);
        multiply.blend = *blend;
        psd(8, 8, &[
            unmasked_square("Base", 8, [128, 128, 128, 255]),
            marker("</Layer group>", 3, 0, 255),
            multiply,
            marker("G", 1, 0, opacity),
        ])
    };
    let kind = |bytes: &[u8]| {
        let imported = import_psd(bytes, &options(1), &Limits::default()).unwrap();
        let groups = imported.model["nodes"]["children"].as_array().unwrap();
        let group = groups.iter().find(|node| node["name"] == "G").unwrap();
        (group["type"].as_str().unwrap().to_owned(), imported)
    };
    let (found, imported) = kind(&group(255, b"mul "));
    assert_eq!(found, "Node");
    let pixels = render_model("passthrough", &imported.model, &imported.textures, 8, json!({}));
    assert_eq!(at(&pixels, 4, 4), [128, 0, 0, 255]);
    // Nothing to gain without a blended layer, and a group opacity needs the buffer.
    assert_eq!(kind(&group(255, b"norm")).0, "DynamicComposite");
    assert_eq!(kind(&group(128, b"mul ")).0, "DynamicComposite");

    // A layer above the group and one inside it above the blended one: the order of the whole stack is kept.
    let mut multiply = unmasked_square("Mul", 8, [255, 0, 0, 255]);
    multiply.blend = *b"mul ";
    let stacked = psd(8, 8, &[
        unmasked_square("Base", 8, [128, 128, 128, 255]),
        marker("</Layer group>", 3, 0, 255),
        multiply,
        solid("Dot", [0, 0, 2, 2], [0, 255, 0, 255]),
        marker("G", 1, 0, 255),
        solid("Top", [6, 6, 8, 8], [0, 0, 255, 255]),
        solid("Top2", [6, 6, 8, 8], [0, 0, 255, 255]),
    ]);
    let (found, imported) = kind(&stacked);
    assert_eq!(found, "Node");
    let pixels = render_model("passthrough-stack", &imported.model, &imported.textures, 8, json!({}));
    assert_eq!([at(&pixels, 4, 4), at(&pixels, 0, 0), at(&pixels, 7, 7)], [[128, 0, 0, 255], GREEN, BLUE]);

    // A Multiply group inside the pass-through group blends with what is below the outer group too.
    let mut inner = marker("Inner", 1, 0, 255);
    inner.blend = *b"mul ";
    let nested = psd(8, 8, &[
        unmasked_square("Base", 8, [128, 128, 128, 255]),
        marker("</Layer group>", 3, 0, 255),
        marker("</Layer group>", 3, 0, 255),
        unmasked_square("Red", 8, [255, 0, 0, 255]),
        inner,
        marker("G", 1, 0, 255),
    ]);
    let (found, imported) = kind(&nested);
    assert_eq!(found, "Node");
    let pixels = render_model("passthrough-nested", &imported.model, &imported.textures, 8, json!({}));
    assert_eq!(at(&pixels, 4, 4), [128, 0, 0, 255]);

    // A group that a layer is clipped to stays one mask source.
    let mut multiply = unmasked_square("Mul", 8, [255, 0, 0, 255]);
    multiply.blend = *b"mul ";
    let mut clipped = unmasked_square("Clipped", 8, [0, 255, 0, 255]);
    clipped.clipped = 1;
    let base = psd(8, 8, &[marker("</Layer group>", 3, 0, 255), multiply, marker("G", 1, 0, 255), clipped]);
    assert_eq!(kind(&base).0, "DynamicComposite");
}

fn keyed(name: &'static str, key: &[u8; 4]) -> L {
    let mut layer = solid(name, [0, 0, 2, 2], [1, 2, 3, 255]);
    layer.blend = *key;
    layer
}

#[test]
fn blend_modes_the_engine_lacks_are_reported_with_the_psd_names() {
    // File order, bottom to top. Mul and Norm are fine, Vivid1/Vivid2/Sat have no engine mode, Odd has an unknown key that
    // prints, Raw one that does not. G is a group in a mode the engine lacks; P passes through, which is not a lost mode.
    let mut group = marker("G", 1, 0, 255);
    group.blend = *b"pLit";
    let bytes = psd(8, 8, &[
        keyed("Mul", b"mul "),
        keyed("Norm", b"norm"),
        keyed("Vivid1", b"vLit"),
        keyed("Sat", b"sat "),
        keyed("Vivid2", b"vLit"),
        keyed("Odd", b"Xy z"),
        keyed("Raw", &[1, 2, 3, 4]),
        marker("</Layer group>", 3, 0, 255),
        keyed("Inner", b"norm"),
        group,
        marker("</Layer group>", 3, 0, 255),
        keyed("InP", b"norm"),
        marker("P", 1, 0, 255),
    ]);
    let imported = import_psd(&bytes, &options(1), &Limits::default()).unwrap();
    let expected = json!({
        "count": 6,
        "modes": [
            {"mode": "VividLight", "count": 2}, {"mode": "Saturation", "count": 1}, {"mode": "Xy z", "count": 1},
            {"mode": "unknown", "count": 1}, {"mode": "PinLight", "count": 1}
        ],
        "other": 0
    });
    assert_eq!(imported.report["blendModesNotApplied"], expected);
    let root = &imported.model["nodes"];
    let modes: Vec<(&str, &str)> = ["Mul", "Norm", "Vivid1", "Sat", "Vivid2", "Odd", "Raw", "G", "P"]
        .into_iter()
        .map(|name| (name, child(root, name)["blend_mode"].as_str().unwrap()))
        .collect();
    assert_eq!(
        modes,
        [("Mul", "Multiply"), ("Norm", "Normal"), ("Vivid1", "Normal"), ("Sat", "Normal"), ("Vivid2", "Normal"),
         ("Odd", "Normal"), ("Raw", "Normal"), ("G", "Normal"), ("P", "Normal")]
    );

    // The merge plan carries the value too.
    let plan = merge_psd(&imported.model, &imported.textures, &bytes, &merge_options(false, false), &Limits::default()).unwrap();
    assert_eq!(plan.to_json()["blendModesNotApplied"], expected);

    // Only modes the engine has, and Normal: no key.
    let mapped = psd(8, 8, &[keyed("Mul", b"mul "), keyed("Norm", b"norm")]);
    let imported = import_psd(&mapped, &options(1), &Limits::default()).unwrap();
    assert!(imported.report.is_empty());
    let plan = merge_psd(&imported.model, &imported.textures, &mapped, &merge_options(false, false), &Limits::default()).unwrap();
    assert!(plan.to_json().get("blendModesNotApplied").is_none());
}

#[test]
fn linear_burn_linear_light_hue_and_color_layers_are_imported_and_drawn_with_their_formula() {
    // A layer of (128, 179, 51) above a base of (204, 153, 102), both opaque. The pixels are the formulas by hand:
    // b + s - 1; b + 2s - 1; the hue of the layer with the saturation and the luminosity (0.30, 0.59, 0.11) of
    // the base; the hue and the saturation of the layer with the luminosity of the base.
    let cases: [(&[u8; 4], &str, [u8; 4]); 4] = [
        (b"lbrn", "LinearBurn", [77, 77, 0, 255]),
        (b"lLit", "LinearLight", [205, 255, 0, 255]),
        (b"hue ", "Hue", [145, 186, 84, 255]),
        (b"colr", "Color", [141, 192, 64, 255]),
    ];
    for (key, name, expected) in cases {
        let mut layer = unmasked_square("Layer", 8, [128, 179, 51, 255]);
        layer.blend = *key;
        let bytes = psd(8, 8, &[unmasked_square("Base", 8, [204, 153, 102, 255]), layer]);
        let imported = import_psd(&bytes, &options(1), &Limits::default()).unwrap();
        assert!(imported.report.is_empty(), "{name}: {:?}", imported.report);
        assert_eq!(child(&imported.model["nodes"], "Layer")["blend_mode"], name);
        let pixels = render_model(name, &imported.model, &imported.textures, 8, json!({}));
        assert_eq!(at(&pixels, 4, 4), expected, "{name}");

        // A group in the mode around a Normal layer: the composite blends what it drew in the same way.
        let mut group = marker("G", 1, 0, 255);
        group.blend = *key;
        let bytes = psd(8, 8, &[
            unmasked_square("Base", 8, [204, 153, 102, 255]),
            marker("</Layer group>", 3, 0, 255),
            unmasked_square("Layer", 8, [128, 179, 51, 255]),
            group,
        ]);
        let imported = import_psd(&bytes, &options(1), &Limits::default()).unwrap();
        assert!(imported.report.is_empty(), "{name}: {:?}", imported.report);
        let group = child(&imported.model["nodes"], "G");
        assert_eq!([&group["type"], &group["blend_mode"]], ["DynamicComposite", name]);
        let pixels = render_model(name, &imported.model, &imported.textures, 8, json!({}));
        assert_eq!(at(&pixels, 4, 4), expected, "{name} group");
    }
}

#[test]
fn the_psd_blend_mode_report_is_bounded() {
    // 20 distinct unknown keys: 16 are named, 4 layers are `other`; the count covers all 20.
    let key = |n: usize| <[u8; 4]>::try_from(format!("k{n:03}").as_bytes()).unwrap();
    let layers: Vec<L> = (0..20).map(|n| keyed("L", &key(n))).collect();
    let imported = import_psd(&psd(8, 8, &layers), &options(1), &Limits::default()).unwrap();
    let value = &imported.report["blendModesNotApplied"];
    let modes = value["modes"].as_array().unwrap();
    assert_eq!((value["count"].clone(), modes.len(), value["other"].clone()), (json!(20), 16, json!(4)));
    assert_eq!(modes[15], json!({"mode": "k015", "count": 1}));
}
