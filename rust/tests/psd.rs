use base64::Engine;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use soul2d_core::psd::{
    apply_mask_feather, apply_mask_settings, decode_rle, decode_zip, group_states, parse, reserve_layer_extraction,
    BlendMode, Layer, LayerType, Limits,
};
use std::io::Write;

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

// Literal-run PackBits, one row per entry.
fn rle(rows: &[&[u8]]) -> Vec<u8> {
    let encoded: Vec<Vec<u8>> = rows
        .iter()
        .map(|row| {
            let mut out = Vec::new();
            for chunk in row.chunks(128) {
                out.push(chunk.len() as u8 - 1);
                out.extend_from_slice(chunk);
            }
            out
        })
        .collect();
    rle_raw_rows(&encoded)
}

fn rle_raw_rows(encoded: &[Vec<u8>]) -> Vec<u8> {
    let mut out = be16(1).to_vec();
    for row in encoded {
        out.extend(be16(row.len() as u16));
    }
    for row in encoded {
        out.extend_from_slice(row);
    }
    out
}

fn zip(pixels: &[u8], width: usize, prediction: bool) -> Vec<u8> {
    let mut data = pixels.to_vec();
    if prediction {
        for row in data.chunks_mut(width) {
            for x in (1..row.len()).rev() {
                row[x] = row[x].wrapping_sub(row[x - 1]);
            }
        }
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&data).unwrap();
    [&be16(if prediction { 3 } else { 2 })[..], &encoder.finish().unwrap()].concat()
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

fn parse_default(bytes: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let document = parse(bytes, &Limits::default())?;
    (0..document.layers.len()).map(|i| document.layer_rgba(i)).collect()
}

// Group file: divider, Child (RLE/raw/ZIP/ZIP+prediction channels), hidden group opener, Top.
fn grouped_layers() -> Vec<L> {
    let mut child = l("Child");
    child.luni = Some(utf16("Kid\u{2603}\0"));
    child.rect = [1, 2, 3, 4];
    child.blend = *b"mul ";
    child.opacity = 128;
    child.clipped = 1;
    child.channels = vec![
        (0, rle(&[&[10, 20], &[30, 40]])),
        (1, raw(&[11, 21, 31, 41])),
        (2, zip(&[12, 22, 32, 42], 2, false)),
        (-1, zip(&[255, 255, 128, 0], 2, true)),
    ];
    let mut top = l("Top");
    top.rect = [0, 0, 2, 1];
    top.channels = vec![(0, raw(&[1, 2])), (1, raw(&[3, 4])), (2, raw(&[5, 6]))];
    vec![
        marker("</Layer group>", 3, 0, 255),
        child,
        marker("Group", 1, 2, 200),
        top,
    ]
}

#[test]
fn parses_group_document_with_every_channel_encoding() {
    let bytes = psd(4, 4, &grouped_layers());
    let document = parse(&bytes, &Limits::default()).unwrap();
    assert_eq!((document.width, document.height, document.channels), (4, 4, 4));
    assert!(!document.has_transparency_mask);
    let names: Vec<&str> = document.layers.iter().map(|layer| layer.name.as_str()).collect();
    assert_eq!(names, ["</Layer group>", "Kid\u{2603}", "Group", "Top"]);
    let kinds: Vec<LayerType> = document.layers.iter().map(|layer| layer.kind).collect();
    assert_eq!(
        kinds,
        [LayerType::SectionDivider, LayerType::Any, LayerType::OpenFolder, LayerType::Any]
    );

    let child = &document.layers[1];
    assert_eq!((child.top, child.left, child.bottom, child.right), (1, 2, 3, 4));
    assert_eq!((child.width(), child.height()), (2, 2));
    assert_eq!(child.blend_mode, BlendMode::Multiply);
    assert_eq!(child.blend_mode.key(), *b"mul ");
    assert_eq!((child.opacity, child.clipped, child.visible()), (128, true, true));
    let ids: Vec<i16> = child.channels.iter().map(|channel| channel.id).collect();
    assert_eq!(ids, [0, 1, 2, -1]);
    assert!(child.has_pixel_data() && !document.layers[0].has_pixel_data());

    let group = &document.layers[2];
    assert!(group.is_group() && !group.visible());
    assert_eq!((group.opacity, group.blend_mode), (200, BlendMode::PassThrough));
    assert!(!document.layers[0].is_group());

    assert_eq!(document.layer_rgba(0).unwrap(), Vec::<u8>::new());
    assert_eq!(
        document.layer_rgba(1).unwrap(),
        [10, 11, 12, 255, 20, 21, 22, 255, 30, 31, 32, 128, 40, 41, 42, 0]
    );
    assert_eq!(document.layer_rgba(3).unwrap(), [1, 3, 5, 255, 2, 4, 6, 255]);
    assert!(document.layer_rgba(4).is_err());
}

#[test]
fn group_states_follow_forward_folder_and_divider_nesting() {
    let bytes = psd(4, 4, &grouped_layers());
    let document = parse(&bytes, &Limits::default()).unwrap();
    // File order is bottom to top, so a group's divider comes first; reverse it for the walk.
    let top_down: Vec<Layer> = document.layers.iter().rev().cloned().collect();
    let states = group_states(&top_down);
    let summary: Vec<(&str, bool)> = states.iter().map(|s| (s.path.as_str(), s.visible)).collect();
    assert_eq!(summary, [("", true), ("", true), ("/Group", false), ("", true)]);
    assert_eq!(states[2].opacity, 200.0 / 255.0);
    assert_eq!(states[0].opacity, 1.0);

    // Port of the D regression: nested hidden/semi-transparent groups.
    let folder = |name: &'static str, kind, flags, opacity| marker(name, kind, flags, opacity);
    let mut child = l("Child");
    child.rect = [0, 0, 1, 1];
    let layers: Vec<Layer> = {
        let mut all = vec![
            folder("Hidden", 1, 2, 128),
            child,
            folder("Nested", 2, 0, 128),
            l("Nested Child"),
            marker("", 3, 0, 255),
            l("Sibling"),
            marker("", 3, 0, 255),
            l("Root"),
        ];
        all.reverse();
        let bytes = psd(4, 4, &all);
        let mut parsed = parse(&bytes, &Limits::default()).unwrap().layers;
        parsed.reverse();
        parsed
    };
    let states = group_states(&layers);
    assert_eq!((states[1].path.as_str(), states[1].visible), ("/Hidden", false));
    assert_eq!(states[1].opacity, 128.0 / 255.0);
    assert_eq!((states[3].path.as_str(), states[3].visible), ("/Hidden/Nested", false));
    assert!((states[3].opacity - (128.0f32 / 255.0) * (128.0f32 / 255.0)).abs() < 1e-6);
    assert_eq!(states[5].path, "/Hidden");
    assert_eq!(states[7].path, "");
}

#[test]
fn unicode_names_override_pascal_names_only_when_non_empty() {
    let mut cases = Vec::new();
    for (pascal, luni, expected) in [
        ("Pascal", Some(utf16("Name")), "Name"),
        ("Pascal", Some(utf16("Name\0")), "Name"),
        ("Pascal", Some(utf16("")), "Pascal"),
        ("Pascal", Some(utf16("\0")), "Pascal"),
        ("A", Some(vec![0xD83D, 0xDE00]), "\u{1F600}"),
        ("Pascal", None, "Pascal"),
        ("Exactly3", None, "Exactly3"),
    ] {
        let mut layer = l(pascal);
        layer.luni = luni;
        cases.push((layer, expected));
    }
    let (layers, expected): (Vec<L>, Vec<&str>) = cases.into_iter().unzip();
    let bytes = psd(1, 1, &layers);
    let document = parse(&bytes, &Limits::default()).unwrap();
    let names: Vec<&str> = document.layers.iter().map(|layer| layer.name.as_str()).collect();
    assert_eq!(names, expected);
}

#[test]
fn negative_layer_count_marks_transparency_mask() {
    let mut layer = l("Only");
    layer.rect = [0, 0, 1, 1];
    layer.channels = vec![(-1, raw(&[7])), (0, raw(&[1])), (1, raw(&[2])), (2, raw(&[3]))];
    let bytes = psd_with_section(2, 2, &section(&layer_info(&[layer], true), &[]));
    let document = parse(&bytes, &Limits::default()).unwrap();
    assert!(document.has_transparency_mask);
    assert_eq!(document.layer_rgba(0).unwrap(), [1, 2, 3, 7]);
}

fn mask_bytes(rect: [i32; 4], default: u8, flags: u8, pad: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for value in rect {
        out.extend(value.to_be_bytes());
    }
    out.push(default);
    out.push(flags);
    out.extend_from_slice(pad);
    out
}

fn masked_layer(mask: Vec<u8>, mask_data: &[u8]) -> L {
    let mut layer = l("Masked");
    layer.rect = [20, 10, 22, 12];
    layer.channels = vec![
        (0, raw(&[100; 4])),
        (1, raw(&[100; 4])),
        (2, raw(&[100; 4])),
        (-3, raw(mask_data)),
    ];
    layer.mask = mask;
    layer
}

fn masked_alpha(layer: L) -> (Vec<u8>, soul2d_core::psd::Document<'static>) {
    let bytes = Box::leak(psd(30, 30, &[layer]).into_boxed_slice());
    let document = parse(bytes, &Limits::default()).unwrap();
    let alpha = document.layer_rgba(0).unwrap().chunks(4).map(|px| px[3]).collect();
    (alpha, document)
}

#[test]
fn layer_mask_alpha_follows_position_flags_and_settings() {
    // Mask rect (20,10)-(21,12): top row of the layer, document-positioned. Second row is outside.
    let doc_rect = [20, 10, 21, 12];
    let (alpha, document) = masked_alpha(masked_layer(mask_bytes(doc_rect, 128, 0, &[0, 0]), &[0, 255]));
    assert_eq!(alpha, [0, 255, 128, 128]);
    let mask = document.layers[0].layer_mask.unwrap();
    assert_eq!((mask.top, mask.left, mask.bottom, mask.right), (20, 10, 21, 12));
    assert_eq!((mask.density, mask.feather, mask.default_color), (255, 0.0, 128));
    assert!(document.layers[0].vector_mask.is_none());

    // The same rect in layer-local coordinates (flag bit 0) lies far from the layer in document space.
    let local_rect = [0, 0, 1, 2];
    let (alpha, _) = masked_alpha(masked_layer(mask_bytes(local_rect, 128, 1, &[0, 0]), &[0, 255]));
    assert_eq!(alpha, [0, 255, 128, 128]);
    let (alpha, _) = masked_alpha(masked_layer(mask_bytes(local_rect, 128, 0, &[0, 0]), &[0, 255]));
    assert_eq!(alpha, [128, 128, 128, 128]);

    let (alpha, _) = masked_alpha(masked_layer(mask_bytes(doc_rect, 128, 0x02, &[0, 0]), &[0, 255]));
    assert_eq!(alpha, [255, 255, 255, 255]);
    let (alpha, _) = masked_alpha(masked_layer(mask_bytes(doc_rect, 128, 0x04, &[0, 0]), &[0, 255]));
    assert_eq!(alpha, [255, 0, 127, 127]);

    // Parameters (flag 0x10): user density 128 and feather 2.5 in a 28-byte mask block.
    let mut with_params = mask_bytes(doc_rect, 0, 0x10, &[]);
    with_params.extend([0x03, 128]);
    with_params.extend(2.5f64.to_be_bytes());
    assert_eq!(with_params.len(), 28);
    let (_, document) = masked_alpha(masked_layer(with_params, &[0, 255]));
    let mask = document.layers[0].layer_mask.unwrap();
    assert_eq!((mask.density, mask.feather), (128, 2.5));
}

#[test]
fn two_masks_split_into_vector_and_layer_mask() {
    // 36 bytes: the first block (flag bit 3) is the vector mask, the second the real layer mask.
    let mut data = mask_bytes([1, 2, 3, 4], 9, 0x08, &[]);
    data.extend([0x01, 7]);
    for value in [5i32, 6, 7, 8] {
        data.extend(value.to_be_bytes());
    }
    assert_eq!(data.len(), 36);
    let mut layer = masked_layer(data, &[]);
    layer.channels.truncate(3);
    let bytes = psd(30, 30, &[layer]);
    let document = parse(&bytes, &Limits::default()).unwrap();
    let vector = document.layers[0].vector_mask.unwrap();
    let real = document.layers[0].layer_mask.unwrap();
    assert_eq!(((vector.top, vector.left, vector.bottom, vector.right), vector.default_color), ((1, 2, 3, 4), 9));
    assert_eq!(((real.top, real.left, real.bottom, real.right), real.default_color), ((5, 6, 7, 8), 7));
    assert!(real.position_relative_to_layer && !vector.position_relative_to_layer);
}

#[test]
fn duplicate_mask_kinds_are_rejected() {
    let mut data = mask_bytes([1, 2, 3, 4], 9, 0x00, &[]);
    data.extend([0x00, 7]);
    for value in [5i32, 6, 7, 8] {
        data.extend(value.to_be_bytes());
    }
    let bytes = psd(30, 30, &[masked_layer(data, &[])]);
    assert_eq!(parse(&bytes, &Limits::default()).err().unwrap(), "A layer mask already exists.");
}

#[test]
fn mask_helpers_match_the_d_regression_values() {
    assert_eq!(apply_mask_settings(0, 255, true, false), 255);
    assert_eq!(apply_mask_settings(0, 128, false, false), 127);
    assert_eq!(apply_mask_settings(0, 255, false, true), 255);
    assert_eq!(apply_mask_settings(200, 0, false, false), 255);

    let hard: Vec<u8> = [0u8, 0, 255, 255, 255].repeat(5);
    assert_eq!(apply_mask_feather(&hard, 5, 5, 0.0, 255), hard);
    assert_eq!(apply_mask_feather(&hard, 5, 4, 1.0, 255), hard);
    let soft = apply_mask_feather(&hard, 5, 5, 1.0, 255);
    // Expected rows come from an independent re-implementation of D's three-pass box blur.
    assert_eq!(&soft[..5], [214, 200, 214, 237, 251]);
    assert_eq!(&soft[10..15], [177, 150, 176, 220, 247]);
    assert_eq!(&soft[20..], [214, 200, 214, 237, 251]);
}

#[test]
fn rle_decoder_matches_the_d_regression_cases() {
    let mut out = [0u8; 5];
    decode_rle(&[1, 10, 20, 0xFE, 30], &mut out).unwrap();
    assert_eq!(out, [10, 20, 30, 30, 30]);
    assert!(decode_rle(&[2, 1], &mut [0; 3]).unwrap_err().contains("literal run exceeds source"));
    assert!(decode_rle(&[0xFE, 1], &mut [0; 2]).unwrap_err().contains("replicated run exceeds destination"));
    assert!(decode_rle(&[0xFE], &mut [0; 3]).unwrap_err().contains("missing replicated byte"));
    assert!(decode_rle(&[1, 10, 20, 0], &mut [0; 3]).is_err());
    let mut out = [0u8; 2];
    decode_rle(&[1, 10, 20, 0x80], &mut out).unwrap();
    assert_eq!(out, [10, 20]);
    assert!(decode_rle(&[1, 10, 20, 0, 30], &mut [0; 2]).is_err());
    assert!(decode_rle(&[1, 10, 20], &mut [0; 3]).unwrap_err().contains("shorter"));
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn zip_decoder_handles_prediction_and_rejects_bombs() {
    let plain = [10, 20, 40, 5, 6, 9];
    assert_eq!(decode_zip(&deflate(&plain), 3, 2, false).unwrap(), plain);
    assert_eq!(decode_zip(&deflate(&[10, 10, 20, 5, 1, 3]), 3, 2, true).unwrap(), plain);
    let bomb = deflate(&vec![0u8; 1_000_000]);
    assert!(decode_zip(&bomb, 4, 4, false).unwrap_err().contains("exceeds its declared dimensions"));
    assert!(decode_zip(&deflate(&[1, 2, 3]), 2, 2, false).unwrap_err().contains("length"));
    assert!(decode_zip(&[1, 2, 3], 2, 2, false).is_err());
    let truncated = deflate(&plain);
    assert!(decode_zip(&truncated[..truncated.len() - 3], 3, 2, false).is_err());
}

fn one_channel_layer(channel: Vec<u8>) -> L {
    let mut layer = l("Chan");
    layer.rect = [0, 0, 2, 2];
    layer.channels = vec![(0, channel), (1, raw(&[0; 4])), (2, raw(&[0; 4]))];
    layer
}

fn extract_one(layer: L) -> Result<Vec<u8>, String> {
    let bytes = psd(2, 2, &[layer]);
    let document = parse(&bytes, &Limits::default())?;
    document.layer_rgba(0)
}

#[test]
fn rle_rows_that_overrun_or_underrun_their_scanline_are_rejected() {
    let good = vec![vec![1, 10, 11], vec![1, 12, 13]];
    assert_eq!(extract_one(one_channel_layer(rle_raw_rows(&good))).unwrap()[..8], [10, 0, 0, 255, 11, 0, 0, 255]);
    // Replicate run of 5 into a 2 pixel row.
    let replicate = vec![vec![0xFC, 7], vec![1, 12, 13]];
    let error = extract_one(one_channel_layer(rle_raw_rows(&replicate))).unwrap_err();
    assert!(error.contains("replicated run exceeds destination"), "{error}");
    // Row 0 carries three literal bytes, so it spills into row 1 even though the total fits.
    let spill = vec![vec![2, 1, 2, 3], vec![0, 9]];
    let error = extract_one(one_channel_layer(rle_raw_rows(&spill))).unwrap_err();
    assert!(error.contains("literal run exceeds destination"), "{error}");
    let short = vec![vec![0, 1], vec![1, 12, 13]];
    assert!(extract_one(one_channel_layer(rle_raw_rows(&short))).unwrap_err().contains("shorter"));
    // Row sizes claiming more data than the channel holds.
    let mut lying = rle_raw_rows(&good);
    lying[3] = 200;
    assert!(extract_one(one_channel_layer(lying)).unwrap_err().contains("Truncated PSD RLE"));
    // A row table that does not fit.
    assert!(extract_one(one_channel_layer(vec![0, 1, 0])).is_err());
}

#[test]
fn bad_channel_payloads_are_reported_not_panicked() {
    assert!(extract_one(one_channel_layer(raw(&[1, 2, 3]))).unwrap_err().contains("Truncated raw"));
    let mut unknown = be16(9).to_vec();
    unknown.extend([0; 8]);
    assert!(extract_one(one_channel_layer(unknown)).unwrap_err().contains("compression type: 9"));
    assert!(extract_one(one_channel_layer(vec![0])).unwrap_err().contains("missing compression header"));
    assert!(extract_one(one_channel_layer(zip(&[0; 8], 2, false))).unwrap_err().contains("exceeds"));
    // Missing colour channels read as black, missing alpha as opaque.
    let mut sparse = l("Sparse");
    sparse.rect = [0, 0, 1, 1];
    sparse.channels = vec![(1, raw(&[9]))];
    assert_eq!(extract_one(sparse).unwrap(), [0, 9, 0, 255]);
}

#[test]
fn limits_cap_pixels_layers_and_channel_memory() {
    let bytes = psd(4, 4, &grouped_layers());
    let tight = Limits {
        max_pixels: 16,
        ..Limits::default()
    };
    assert!(parse(&bytes, &tight).is_ok());
    let tiny = Limits {
        max_pixels: 15,
        ..Limits::default()
    };
    assert!(parse(&bytes, &tiny).unwrap_err().contains("dimensions"));
    let few = Limits {
        max_layers: 3,
        ..Limits::default()
    };
    assert!(parse(&bytes, &few).unwrap_err().contains("layer count"));

    let channels = Limits {
        max_layer_channels: 3,
        ..Limits::default()
    };
    assert!(parse(&bytes, &channels).unwrap().layer_rgba(1).unwrap_err().contains("memory budget"));
    let bytes_cap = Limits {
        max_channel_bytes: 15,
        ..Limits::default()
    };
    assert!(parse(&bytes, &bytes_cap).unwrap().layer_rgba(1).unwrap_err().contains("memory budget"));
    let exact = Limits {
        max_channel_bytes: 16,
        ..Limits::default()
    };
    assert_eq!(parse(&bytes, &exact).unwrap().layer_rgba(1).unwrap().len(), 16);
}

#[test]
fn hostile_headers_and_section_lengths_fail_cleanly() {
    let good = psd(4, 4, &grouped_layers());
    assert!(parse(&good, &Limits::default()).is_ok());

    let patched = |offset: usize, value: &[u8]| {
        let mut bytes = good.clone();
        bytes[offset..offset + value.len()].copy_from_slice(value);
        bytes
    };
    let error = |bytes: Vec<u8>| parse(&bytes, &Limits::default()).err().unwrap();

    assert!(error(patched(0, b"8BPX")).contains("must be a Photoshop PSD"));
    assert!(error(patched(4, &be16(2))).contains("Version"));
    assert!(error(patched(6, &[0, 0, 0, 0, 0, 1])).contains("reserve bytes"));
    assert!(error(patched(14, &be32(0x7FFF_FFFF))).contains("dimensions"));
    assert!(error(patched(18, &be32(0x7FFF_FFFF))).contains("dimensions"));
    assert!(error(patched(14, &[be32(20_000), be32(20_000)].concat())).contains("dimensions"));
    assert!(error(patched(14, &be32(0))).contains("dimensions"));
    assert!(error(patched(18, &be32(0x8000_0000))).contains("dimensions"));
    assert!(error(patched(22, &be16(16))).contains("Only 8-bit PSD channels are supported (found 16-bit)"));
    assert!(error(patched(24, &be16(1))).contains("Only RGB PSD documents are supported (found color mode 1)"));
    // Colour mode data, image resources and the layer section all claim more than the file holds.
    assert!(error(patched(26, &be32(0xFFFF_FFFF))).contains("Truncated"));
    assert!(error(patched(30, &be32(0xFFFF_FFFF))).contains("Truncated"));
    let section_at = 26 + 4 + 4 + 4 + 12;
    assert!(error(patched(section_at, &be32(0xFFFF_FFF0))).contains("Truncated"));
    // Layer info longer than its section; section too short to hold the claimed layer count.
    assert!(error(patched(section_at + 4, &be32(0x7FFF_FFFF))).contains("Truncated"));
    assert!(error(patched(section_at + 8, &be16(0x7FFF))).contains("layer count exceeds"));
    assert!(error(patched(section_at + 8, &be16(0x8000))).contains("layer count exceeds"));
}

#[test]
fn hostile_layer_records_fail_cleanly() {
    let one = |layer: L| psd(4, 4, &[layer]);
    let mut base = l("Hostile");
    base.rect = [0, 0, 1, 1];
    base.channels = vec![(0, raw(&[1])), (1, raw(&[2])), (2, raw(&[3]))];
    let bytes = one(base);
    assert!(parse(&bytes, &Limits::default()).is_ok());
    // Record starts after: header(26) + 4 + 4 + 4 + resource(12) + section len(4) + info len(4) + count(2).
    let record = 26 + 4 + 4 + 4 + 12 + 4 + 4 + 2;
    let patched = |offset: usize, value: &[u8]| {
        let mut copy = bytes.clone();
        copy[record + offset..record + offset + value.len()].copy_from_slice(value);
        copy
    };
    let error = |bytes: Vec<u8>| parse(&bytes, &Limits::default()).err().unwrap();
    assert!(error(patched(0, &(i32::MAX).to_be_bytes())).contains("layer bounds"));
    assert!(error(patched(8, &(i32::MIN).to_be_bytes())).contains("layer bounds"));
    assert!(error(patched(4, &i32::MAX.to_be_bytes())).contains("layer bounds"));
    assert!(error(patched(8, &(-5i32).to_be_bytes())).contains("layer bounds"));
    // Channel table claims 65535 channels.
    assert!(error(patched(16, &be16(0xFFFF))).contains("channel table"));
    // First channel length points far past the section.
    assert!(error(patched(20, &be32(0xFFFF_FFF0))).contains("exceeds the layer info section"));
    // Bad blend signature and an extra-data length past the end.
    assert!(error(patched(36, b"8BIX")).contains("8BIM"));
    let extra_len = 16 + 2 + 18 + 4 + 4 + 4;
    assert!(error(patched(extra_len, &be32(0xFFFF_FFFF))).contains("Truncated"));
    // A mask length beyond the extra data.
    assert!(error(patched(extra_len + 4, &be32(0xFFFF_FFFF))).contains("Truncated"));
}

#[test]
fn hostile_additional_info_blocks_fail_cleanly() {
    let with_extra = |extra: Vec<u8>| {
        let mut layer = l("Extra");
        layer.rect = [0, 0, 1, 1];
        layer.channels = vec![(0, raw(&[1])), (1, raw(&[2])), (2, raw(&[3]))];
        layer.extra = extra;
        parse_default(&psd(2, 2, &[layer]))
    };
    let err = |result: Result<Vec<Vec<u8>>, String>| result.err().unwrap();
    // luni claiming more characters than its block holds.
    let mut luni = be32(0xFFFF_FFFF).to_vec();
    luni.extend([0, 65, 0, 66]);
    assert!(err(with_extra(block(b"luni", &luni))).contains("Unicode layer name exceeds"));
    assert!(err(with_extra(block(b"luni", &[0, 0]))).contains("Unicode layer name block is truncated"));
    // Block length past the end of the layer record.
    let mut long_block = b"8BIMxxxx".to_vec();
    long_block.extend(be32(0x4000_0000));
    assert!(err(with_extra(long_block)).contains("exceeds its containing section"));
    let mut huge = b"8BIMxxxx".to_vec();
    huge.extend(be32(u32::MAX));
    assert!(err(with_extra(huge)).contains("exceeds its containing section"));
    assert!(err(with_extra(b"8BIM".to_vec())).contains("header is truncated"));
    assert!(err(with_extra(b"XXXXyyyyzzzz".to_vec())).contains("8BIM"));
    assert!(err(with_extra(block(b"lsct", &[0, 0]))).contains("section divider block is truncated"));
    assert!(err(with_extra(block(b"lsct", &be32(9)))).contains("section divider type 9"));
    // Unknown blocks are skipped, unknown blend keys are kept.
    assert!(with_extra(block(b"zzzz", &[1, 2, 3])).is_ok());
}

#[test]
fn every_truncation_of_a_valid_file_is_rejected_or_decodes_without_panicking() {
    let bytes = psd(4, 4, &grouped_layers());
    let section_end = bytes.len() - TRAILER.len();
    for len in 0..bytes.len() {
        match parse_default(&bytes[..len]) {
            Ok(_) => assert!(len >= section_end, "prefix of {len} bytes parsed"),
            Err(message) => assert!(!message.is_empty()),
        }
    }
    assert!(parse_default(&bytes).is_ok());
    // Single-byte corruption anywhere must never panic.
    for index in 0..bytes.len() {
        for value in [0u8, 0xFF, 0x80] {
            let mut copy = bytes.clone();
            copy[index] = value;
            let _ = parse_default(&copy);
        }
    }
}

#[test]
fn lr16_block_replaces_layers_from_an_empty_layer_info() {
    let mut layer = l("Deep");
    layer.rect = [0, 0, 1, 1];
    layer.channels = vec![(0, raw(&[4])), (1, raw(&[5])), (2, raw(&[6]))];
    let info = layer_info(&[layer], false);
    let mut tail = be32(0).to_vec();
    tail.extend(block(b"Lr16", &info));
    let bytes = psd_with_section(2, 2, &section(&[], &tail));
    let document = parse(&bytes, &Limits::default()).unwrap();
    assert_eq!(document.layers.len(), 1);
    assert_eq!(document.layers[0].name, "Deep");
    assert_eq!(document.layer_rgba(0).unwrap(), [4, 5, 6, 255]);

    // Global mask info of 13 bytes plus a plain unknown block are skipped.
    let mut tail = be32(13).to_vec();
    tail.extend([0; 13]);
    tail.extend(block(b"vmsk", &[1, 2, 3, 4]));
    let bytes = psd_with_section(2, 2, &section(&layer_info(&[l("Plain")], false), &tail));
    assert_eq!(parse(&bytes, &Limits::default()).unwrap().layers.len(), 1);
    let mut bad_tail = be32(0).to_vec();
    bad_tail.extend(b"8BIMvmsk");
    bad_tail.extend(be32(1000));
    let bytes = psd_with_section(2, 2, &section(&layer_info(&[l("Plain")], false), &bad_tail));
    assert!(parse(&bytes, &Limits::default()).is_err());
}

const MINIMAL_PSD: &str = "OEJQUwABAAAAAAAAAAMAAAABAAAAAQAIAAMAAAAAAAAAAAAAAAQAAAAAAAAAAAA=";
const LAYERED_PSD: &str = "OEJQUwABAAAAAAAAAAMAAAAEAAAABAAIAAMAAAAAAAAAAAAAAT4AAAE2AAIAAAAAAAAAAAAAAAQAAAAEAAQAAAAAABIAAQAAABIAAgAAABL//wAAABI4QklNbm9ybf8AAAAAAAAYAAAAAAAAAAANU3ludGhldGljQmFjawAAAAAAAAAAAAAAAAAEAAAABAAEAAAAAAASAAEAAAASAAIAAAAS//8AAAASOEJJTW5vcm3/AAAAAAAAGAAAAAAAAAAADlN5bnRoZXRpY0Zyb250AAAAQEBAQEBAQEBAQEBAQEBAQAAAQEBAQEBAQEBAQEBAQEBAQAAAQEBAQEBAQEBAQEBAQEBAQAAA/////////////////////wAAwMDAwMDAwMDAwMDAwMDAwAAAwMDAwMDAwMDAwMDAwMDAwAAAwMDAwMDAwMDAwMDAwMDAwAAA/////////////////////wAAAAA=";

fn fixture(encoded: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(encoded).unwrap()
}

#[test]
fn d_regression_fixtures_parse_to_the_documented_values() {
    let minimal = fixture(MINIMAL_PSD);
    let document = parse(&minimal, &Limits::default()).unwrap();
    assert_eq!((document.width, document.height, document.channels), (1, 1, 3));
    assert!(document.layers.is_empty());

    // Zero-length layer section (the D "flat only" fixture).
    let mut flat = minimal.clone();
    flat[34..38].fill(0);
    flat.drain(38..42);
    let document = parse(&flat, &Limits::default()).unwrap();
    assert!(document.layers.is_empty() && document.width == 1 && document.height == 1);

    let layered = fixture(LAYERED_PSD);
    let document = parse(&layered, &Limits::default()).unwrap();
    assert_eq!((document.width, document.height, document.channels), (4, 4, 3));
    let names: Vec<&str> = document.layers.iter().map(|layer| layer.name.as_str()).collect();
    assert_eq!(names, ["SyntheticBack", "SyntheticFront"]);
    for (index, gray) in [(0usize, 0x40u8), (1, 0xC0)] {
        let layer = &document.layers[index];
        assert_eq!((layer.top, layer.left, layer.bottom, layer.right), (0, 0, 4, 4));
        assert_eq!((layer.opacity, layer.flags, layer.clipped), (255, 0, false));
        assert_eq!((layer.blend_mode, layer.kind), (BlendMode::Normal, LayerType::Any));
        let ids: Vec<i16> = layer.channels.iter().map(|channel| channel.id).collect();
        assert_eq!(ids, [0, 1, 2, -1]);
        assert!(layer.layer_mask.is_none() && layer.vector_mask.is_none());
        assert_eq!(document.layer_rgba(index).unwrap(), [gray, gray, gray, 255].repeat(16));
    }

    let mut sixteen_bit = minimal.clone();
    sixteen_bit[22..24].copy_from_slice(&[0, 16]);
    let error = parse(&sixteen_bit, &Limits::default()).err().unwrap();
    assert!(error.contains("Only 8-bit PSD channels"), "{error}");
    let mut grayscale = minimal;
    grayscale[24..26].copy_from_slice(&[0, 1]);
    let error = parse(&grayscale, &Limits::default()).err().unwrap();
    assert!(error.contains("Only RGB PSD documents"), "{error}");
}

fn mask_layer_with_channels(mask_channel: bool) -> Layer {
    let mut layer = l("Budget");
    layer.rect = [0, 0, 10, 10];
    if mask_channel {
        layer.channels = vec![(-3, raw(&[]))];
    }
    let bytes = psd(10, 10, &[layer]);
    parse(&bytes, &Limits::default()).unwrap().layers.remove(0)
}

#[test]
fn extraction_budget_reserves_layer_and_mask_memory() {
    let plain = mask_layer_with_channels(false);
    let mut retained = 0;
    assert!(reserve_layer_extraction(&plain, 4, 800, &mut retained));
    assert_eq!(retained, 400);
    assert!(reserve_layer_extraction(&plain, 4, 800, &mut retained));
    assert_eq!(retained, 800);
    assert!(!reserve_layer_extraction(&plain, 4, 800, &mut retained));
    assert_eq!(retained, 800);
    assert!(!reserve_layer_extraction(&plain, 0, 800, &mut 0));

    // A mask channel adds 8 bytes per pixel plus six prefix-sum lines of (10 + 1) * 8 bytes.
    let masked = mask_layer_with_channels(true);
    let needed = 400 + 800 + 6 * 11 * 8;
    let mut retained = 0;
    assert!(!reserve_layer_extraction(&masked, 4, needed - 1, &mut retained));
    assert_eq!(retained, 0);
    assert!(reserve_layer_extraction(&masked, 4, needed, &mut retained));
    assert_eq!(retained, needed);
}

#[test]
fn rgba_output_is_capped_even_when_the_layer_has_no_channels() {
    let mut layer = l("Empty");
    layer.rect = [0, 0, 1000, 1000];
    let bytes = psd(1000, 1000, &[layer]);
    let limits = Limits {
        max_channel_bytes: 3_999_999,
        ..Limits::default()
    };
    let error = parse(&bytes, &limits).unwrap().layer_rgba(0).unwrap_err();
    assert!(error.contains("memory budget"), "{error}");
    assert_eq!(parse(&bytes, &Limits::default()).unwrap().layer_rgba(0).unwrap().len(), 4_000_000);
}
