//! Contract tests for `src/depth_draw.rs`.
//!
//! The module is pure, so it is compiled straight into this test (`#[path]`). The composer cases load the
//! D regression fixtures from `tests/fixtures/depthdraw` and skip with a message when they
//! are absent; everything else uses literal values read from the D code and `source/soul2d_tests/regression.d`.


use soul2d_core::depth_draw::*;
use serde_json::{json, Value};
use std::path::PathBuf;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/depthdraw");

const IDENTITY: [[f64; 4]; 4] =
    [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-5
}

fn near_all(actual: &[f32], expected: &[f32]) -> bool {
    actual.len() == expected.len() && actual.iter().zip(expected).all(|(a, b)| near(*a, *b))
}

fn fixture_files() -> Option<Vec<PathBuf>> {
    let dir = PathBuf::from(FIXTURES);
    if !dir.is_dir() {
        eprintln!("SKIP: depthdraw fixtures not found at {FIXTURES}");
        return None;
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    Some(files)
}

fn load(path: &PathBuf) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn floats(value: &Value) -> Vec<f32> {
    value.as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect()
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_owned()).collect()
}

/// Grid vertices row by row (y outer, x inner), the order D `DefineGrid` produces.
fn grid_vertices(axis_x: &[f32], axis_y: &[f32]) -> Vec<[f32; 2]> {
    axis_y.iter().flat_map(|y| axis_x.iter().map(move |x| [*x, *y])).collect()
}

fn policy(name: &str) -> MergePolicy {
    MergePolicy::from_name(name).unwrap()
}

fn compose_fixture(root: &Value) -> ComposeResult {
    let doc = &root["document"];
    let (dw, dh) = (doc["width"].as_i64().unwrap() as i32, doc["height"].as_i64().unwrap() as i32);
    let grid = &root["grid"];
    let vertices = grid_vertices(&floats(&grid["axisX"]), &floats(&grid["axisY"]));
    let base = floats(&grid["baseDepths"]);
    let mut layers = Vec::new();
    for l in root["layers"].as_array().unwrap() {
        let bounds: Vec<i32> = l["bounds"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap() as i32).collect();
        let mut layer = Layer {
            id: l["id"].as_str().unwrap().to_owned(),
            width: l["width"].as_i64().unwrap() as i32,
            height: l["height"].as_i64().unwrap() as i32,
            bounds: Rect { left: bounds[0], top: bounds[1], width: bounds[2], height: bounds[3] },
            depth_pixels: l["pixels"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u8).collect(),
            ..Layer::default()
        };
        for (key, slot) in [
            ("zScale", &mut layer.z_scale),
            ("zOffset", &mut layer.z_offset),
            ("backDepth", &mut layer.back_depth),
            ("frontDepth", &mut layer.front_depth),
        ] {
            if let Some(v) = l.get(key) {
                *slot = v.as_f64().unwrap() as f32;
            }
        }
        layers.push(layer);
    }
    let bindings: Vec<Binding> = root["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| Binding {
            layer_id: b["layerId"].as_str().unwrap().to_owned(),
            target_grid_uuid: 7,
            target_node_uuid: 7,
            order: b["order"].as_i64().unwrap_or(0) as i32,
            merge_policy: b.get("mergePolicy").map_or(MergePolicy::Frontmost, |m| policy(m.as_str().unwrap())),
            ..Binding::default()
        })
        .collect();
    let points = document_points(&vertices, &IDENTITY, dw, dh);
    compose_target(&layers, &bindings, &Target { grid_uuid: 7, document_points: &points, base_depths: &base }).unwrap()
}

fn check_result(id: &str, result: &ComposeResult, depths: &[f32], winners: &[String], tolerance: f32) {
    assert_eq!(result.depths.len(), depths.len(), "{id}: depth count");
    for (i, (a, e)) in result.depths.iter().zip(depths).enumerate() {
        assert!((a - e).abs() <= tolerance, "{id}: depth[{i}] actual={a} expected={e}");
    }
    assert_eq!(result.winning_layer_ids, winners, "{id}: winners");
}

/// The 4x4 layer of D's merge-policy test: only the four vertex pixels (1,1) (3,1) (1,3) (3,3) are set.
fn vertex_layer(id: &str, gray: u8) -> Layer {
    let mut pixels = vec![0u8; 16 * 4];
    for (x, y) in [(1, 1), (3, 1), (1, 3), (3, 3)] {
        let i = (y * 4 + x) * 4;
        pixels[i..i + 4].copy_from_slice(&[gray, gray, gray, 255]);
    }
    Layer {
        id: id.into(),
        width: 4,
        height: 4,
        bounds: Rect { left: 0, top: 0, width: 4, height: 4 },
        convolution: Convolution::Nearest,
        depth_pixels: pixels,
        ..Layer::default()
    }
}

#[test]
fn every_depthdraw_fixture_file_is_checked_exactly() {
    let Some(files) = fixture_files() else { return };
    let (mut active, mut retired) = (0, 0);
    for path in files {
        let root = load(&path);
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if let Some(status) = root.get("status") {
            assert_eq!(status, "retired-external-reference", "{name}");
            assert!(root.get("generatedBy").is_none(), "{name}: a tombstone carries no executable record");
            let replacement = root["replacement"].as_str().unwrap();
            assert!(path.with_file_name(replacement).exists(), "{name}: replacement {replacement} must exist");
            retired += 1;
            continue;
        }
        assert_eq!(root["generatedBy"], "local-contract", "{name}");
        let id = root["id"].as_str().unwrap();
        if id == "local-merge-policies-smoke" {
            check_merge_policies(&root);
        } else {
            let result = compose_fixture(&root);
            let tolerance = root.get("tolerance").map_or(1e-5, |t| t.as_f64().unwrap() as f32);
            check_result(id, &result, &floats(&root["expectedDepths"]), &strings(&root["expectedWinners"]), tolerance);
        }
        active += 1;
    }
    assert_eq!(active, 7, "five synthetic + two local fixtures");
    assert_eq!(retired, 42, "retired records are tombstones, never counted as passes");
}

fn check_merge_policies(root: &Value) {
    let vertices = grid_vertices(&floats(&root["grid"]["axisX"]), &floats(&root["grid"]["axisY"]));
    let base = floats(&root["grid"]["baseDepths"]);
    let points = document_points(&vertices, &IDENTITY, 4, 4);
    let cases = root["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 5);
    for case in cases {
        let name = case["policy"].as_str().unwrap();
        let layers = [vertex_layer("low", 0), vertex_layer("high", 255)];
        let bindings: Vec<Binding> = [("low", 0), ("high", 1)]
            .iter()
            .map(|(id, order)| Binding {
                layer_id: (*id).into(),
                target_grid_uuid: 9,
                order: *order,
                merge_policy: policy(name),
                ..Binding::default()
            })
            .collect();
        let result =
            compose_target(&layers, &bindings, &Target { grid_uuid: 9, document_points: &points, base_depths: &base }).unwrap();
        check_result(name, &result, &floats(&case["expectedDepths"]), &strings(&case["expectedWinners"]), 1e-5);
    }
}

#[test]
fn fixture_values_match_the_synthetic_design_notes() {
    // README: gray 51 maps to -0.6, and zScale 2 / zOffset 0.25 make -0.95.
    let layer = Layer { z_scale: 2.0, z_offset: 0.25, ..Layer::default() };
    let raw = -1.0 + 2.0 * 51.0 / 255.0;
    assert!(near(raw, -0.6));
    assert!(near(layer.apply_z_transform(raw), -0.95));
}

/// D `testDepthDrawComposerContracts`: back/front layers with their own normal coverage.
fn session_back_front() -> (Vec<Layer>, Vec<Binding>, Vec<[f32; 2]>) {
    let mut a = vec![0u8; 64];
    let mut b = vec![0u8; 64];
    let set = |p: &mut Vec<u8>, x: usize, y: usize, gray: u8| {
        let i = (y * 4 + x) * 4;
        p[i..i + 4].copy_from_slice(&[gray, gray, gray, 255]);
    };
    set(&mut a, 1, 1, 0);
    set(&mut b, 1, 1, 255);
    set(&mut b, 3, 1, 255);
    let make = |id: &str, px: Vec<u8>| Layer {
        id: id.into(),
        width: 4,
        height: 4,
        bounds: Rect { left: 0, top: 0, width: 4, height: 4 },
        rgba: px.clone(),
        depth_pixels: px.clone(),
        normal_coverage: px,
        ..Layer::default()
    };
    let back = make("back", a);
    let mut front = make("front", b);
    front.z_scale = 0.5;
    front.z_offset = 0.25;
    let bindings = vec![
        Binding { layer_id: "back".into(), target_grid_uuid: 5, order: 0, ..Binding::default() },
        Binding { layer_id: "front".into(), target_grid_uuid: 5, order: 1, ..Binding::default() },
    ];
    let points = document_points(&grid_vertices(&[-1.0, 1.0], &[-1.0, 1.0]), &IDENTITY, 4, 4);
    (vec![back, front], bindings, points)
}

#[test]
fn composer_stats_match_the_d_regression_numbers() {
    let (layers, bindings, points) = session_back_front();
    let base = [0.25f32; 4];
    let target = Target { grid_uuid: 5, document_points: &points, base_depths: &base };
    let result = compose_target(&layers, &bindings, &target).unwrap();
    assert!(near_all(&result.depths, &[0.75, 0.75, 0.25, 0.25]));
    assert_eq!(result.winning_layer_ids, ["front", "front", "", ""]);
    assert_eq!(result.layer_stats.len(), 2);
    let (back, front) = (&result.layer_stats[0], &result.layer_stats[1]);
    assert_eq!((back.sampled_vertices, back.missing_vertices, back.winning_vertices), (1, 3, 0));
    assert_eq!((front.sampled_vertices, front.missing_vertices, front.winning_vertices), (2, 2, 2));
    assert_eq!((result.sampled_vertices, result.missing_vertices), (3, 5));
    assert!(result.has_depth_range && near(result.min_depth, 0.25) && near(result.max_depth, 0.75));

    // Layer opacity below the alpha threshold leaves every vertex missing and the base depth intact.
    let mut faint = layers[0].clone();
    faint.opacity = 0.005;
    let result = compose_target(&[faint], &bindings[..1], &target).unwrap();
    assert_eq!((result.sampled_vertices, result.missing_vertices), (0, 4));
    assert_eq!(result.depths, [0.25; 4]);
}

#[test]
fn bindings_apply_in_order_then_index_and_skip_disabled_layers() {
    let (mut layers, mut bindings, points) = session_back_front();
    let base = [0.25f32; 4];
    let target = Target { grid_uuid: 5, document_points: &points, base_depths: &base };
    // Replace in reversed order: back (order 2) is applied last and wins where it samples.
    bindings[0].order = 2;
    bindings[0].merge_policy = MergePolicy::Replace;
    bindings[1].merge_policy = MergePolicy::Replace;
    let result = compose_target(&layers, &bindings, &target).unwrap();
    assert_eq!(result.winning_layer_ids, ["back", "front", "", ""]);
    assert!(near_all(&result.depths, &[-1.0, 0.75, 0.25, 0.25]));
    // Equal orders keep the binding list order.
    bindings[0].order = 1;
    let result = compose_target(&layers, &bindings, &target).unwrap();
    assert_eq!(result.winning_layer_ids, ["front", "front", "", ""]);
    // A hidden layer, a disabled layer and a disabled binding are all skipped.
    layers[0].visible = false;
    assert_eq!(compose_target(&layers, &bindings, &target).unwrap().layer_stats.len(), 1);
    layers[0].visible = true;
    layers[0].enabled = false;
    assert_eq!(compose_target(&layers, &bindings, &target).unwrap().layer_stats.len(), 1);
    layers[0].enabled = true;
    bindings[0].enabled = false;
    assert_eq!(compose_target(&layers, &bindings, &target).unwrap().layer_stats.len(), 1);
    // Add starts from the base depth and counts each contribution.
    bindings[0].enabled = true;
    bindings[0].merge_policy = MergePolicy::Add;
    bindings[1].merge_policy = MergePolicy::Add;
    let result = compose_target(&layers, &bindings, &target).unwrap();
    assert!(near_all(&result.depths, &[0.0, 1.0, 0.25, 0.25]), "{:?}", result.depths);
    assert!(result.layer_stats.iter().all(|s| s.additive_merge));
    assert_eq!((result.layer_stats[0].contributed_vertices, result.layer_stats[1].contributed_vertices), (1, 2));
    // Base depths shorter than the vertex list are padded with zeros.
    let short = Target { grid_uuid: 5, document_points: &points, base_depths: &[0.5] };
    let result = compose_target(&[], &[], &short).unwrap();
    assert_eq!(result.depths, [0.5, 0.0, 0.0, 0.0]);
}

#[test]
fn xy_transform_moves_where_a_layer_is_sampled() {
    let mut layer = vertex_layer("l", 255);
    layer.bounds = Rect { left: 10, top: 20, width: 4, height: 4 };
    assert_eq!(layer_pixel_from_document(&layer, [11.0, 21.0]), [1.0, 1.0]);
    layer.xy_offset = [2.0, 0.0];
    layer.xy_scale = [2.0, 0.0];
    // A zero scale falls back to 1; x is offset then divided by 2.
    assert_eq!(layer_pixel_from_document(&layer, [15.0, 21.0]), [1.5, 1.0]);
    let (min, max) = layer_document_bounds(&layer);
    assert_eq!((min, max), ([12.0, 20.0], [20.0, 24.0]));
    layer.xy_scale = [-1.0, 1.0];
    let (min, max) = layer_document_bounds(&layer);
    assert_eq!((min, max), ([8.0, 20.0], [12.0, 24.0]));
}

#[test]
fn document_points_use_the_grid_world_matrix_and_document_centre() {
    let world = [[2.0, 0.0, 0.0, 10.0], [0.0, 2.0, 0.0, -4.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
    let points = document_points(&[[1.0, 1.0], [-1.0, 0.5]], &world, 100, 50);
    assert_eq!(points, [[62.0, 23.0], [58.0, 22.0]]);
}

// ----------------------------------------------------------------------------------------- sampling

fn cross_rgba(center: u8, other: u8) -> Vec<u8> {
    let mut px = Vec::new();
    for i in 0..9 {
        let g = if i == 4 { center } else { other };
        px.extend_from_slice(&[g, g, g, 255]);
    }
    px
}

fn sample(rgba: &[u8], w: i32, h: i32, x: f32, y: f32, mutate: impl FnOnce(&mut SampleSettings)) -> Sample {
    let mut s = SampleSettings::default();
    mutate(&mut s);
    sample_rgba(rgba, w, h, None, 1.0, x, y, &s).unwrap()
}

#[test]
fn convolutions_over_a_white_pixel_in_a_black_field() {
    let px = cross_rgba(255, 0);
    let at_centre = |c: Convolution| sample(&px, 3, 3, 1.0, 1.0, |s| s.convolution = c).value;
    assert!(near(at_centre(Convolution::Nearest), 1.0));
    assert!(near(at_centre(Convolution::Box3x3), -7.0 / 9.0));
    assert!(near(at_centre(Convolution::Gaussian3x3), -0.5));
    assert!(near(at_centre(Convolution::Median3x3), -1.0));
    assert!(near(at_centre(Convolution::Frontmost3x3), 1.0));
    assert!(near(at_centre(Convolution::Backmost3x3), -1.0));
    // Gaussian5x5 weights are 1-4-6-4-1 per axis; inside the 3x3 image the centre weighs 36, the edges 24
    // and the corners 16 (196 in all): (36 - 160) / 196.
    assert!(near(at_centre(Convolution::Gaussian5x5), -124.0 / 196.0));
    // A window clipped by the image edge only averages the pixels that exist: corner (0,0), Box3x3.
    assert!(near(sample(&px, 3, 3, 0.0, 0.0, |s| s.convolution = Convolution::Box3x3).value, -0.5));
}

#[test]
fn custom_radius_windows_and_clamping() {
    let mut px = vec![0u8; 5 * 5 * 4];
    for i in 0..25 {
        let g = if i == 12 { 255 } else { 0 };
        px[i * 4..i * 4 + 4].copy_from_slice(&[g, g, g, 255]);
    }
    let run = |c, r| {
        sample(&px, 5, 5, 2.0, 2.0, |s| {
            s.convolution = c;
            s.custom_radius = r;
        })
    };
    assert!(near(run(Convolution::BoxCustom, 2).value, (24.0 * -1.0 + 1.0) / 25.0));
    assert!(near(run(Convolution::FrontmostCustom, 1).value, 1.0));
    assert!(near(run(Convolution::MedianCustom, 2).value, -1.0));
    // exp(-d2 / 2) weights with sigma 1: centre 1, 4 edges e^-0.5, 4 corners e^-1 inside radius 1.
    let (edge, corner) = ((-0.5f32).exp(), (-1.0f32).exp());
    let total = 1.0 + 4.0 * edge + 4.0 * corner;
    let others = -(4.0 * edge + 4.0 * corner);
    assert!(near(run(Convolution::GaussianCustom, 2).value, {
        // radius 2 adds the ring at distance 2; recompute exactly with the same window.
        let mut num = 0.0f32;
        let mut den = 0.0f32;
        for dy in -2i32..=2 {
            for dx in -2i32..=2 {
                let w = (-((dx * dx + dy * dy) as f32) / 2.0).exp();
                num += if dx == 0 && dy == 0 { w } else { -w };
                den += w;
            }
        }
        num / den
    }));
    assert!(total > 0.0 && others < 0.0);
    assert_eq!(normalize_custom_radius(0), 1);
    assert_eq!(normalize_custom_radius(1000), 64);
}

#[test]
fn channel_invert_scale_opacity_and_threshold() {
    let red = [255u8, 0, 0, 255];
    let nearest = |m: &dyn Fn(&mut SampleSettings)| {
        sample(&red, 1, 1, 0.0, 0.0, |s| {
            s.convolution = Convolution::Nearest;
            m(s)
        })
    };
    assert!(near(nearest(&|s| s.channel = Channel::R).value, 1.0));
    assert!(near(nearest(&|s| s.channel = Channel::G).value, -1.0));
    assert!(near(nearest(&|_| {}).value, -1.0 + 2.0 / 3.0));
    assert!(near(nearest(&|s| s.channel = Channel::Luminance).value, -1.0 + 2.0 * 0.2126));
    assert!(near(
        nearest(&|s| {
            s.channel = Channel::R;
            s.invert = true
        })
        .value,
        -1.0
    ));
    assert!(near(
        nearest(&|s| {
            s.channel = Channel::R;
            s.depth_scale = 0.5
        })
        .value,
        0.5
    ));
    let faint = sample_rgba(
        &red,
        1,
        1,
        None,
        0.005,
        0.0,
        0.0,
        &SampleSettings { convolution: Convolution::Nearest, ..SampleSettings::default() },
    )
    .unwrap();
    assert!(!faint.valid, "alpha 0.005 is not above the 0.01 threshold");
    // x = 0.5 rounds away from zero to pixel 1, which is outside a 1x1 image.
    assert!(!sample(&red, 1, 1, 0.5, 0.0, |s| s.convolution = Convolution::Nearest).valid);
}

#[test]
fn coverage_alpha_must_exceed_one_half() {
    let rgba = [255u8, 255, 255, 255, 0, 0, 0, 255];
    let coverage = [0u8, 0, 0, 255, 0, 0, 0, 100];
    let settings = SampleSettings { convolution: Convolution::Nearest, ..SampleSettings::default() };
    let cov = Coverage { rgba: &coverage, width: 2, height: 1, opacity: 1.0 };
    assert!(sample_rgba(&rgba, 2, 1, Some(cov), 1.0, 0.0, 0.0, &settings).unwrap().valid);
    assert!(!sample_rgba(&rgba, 2, 1, Some(cov), 1.0, 1.0, 0.0, &settings).unwrap().valid, "100/255 < 0.5");
    // A coverage image of another size is read at the same relative position: x=0 -> 1, x=1 -> 3 of 4.
    let big = [0u8, 0, 0, 0, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 0];
    let cov4 = Coverage { rgba: &big, width: 4, height: 1, opacity: 1.0 };
    assert!(sample_rgba(&rgba, 2, 1, Some(cov4), 1.0, 0.0, 0.0, &settings).unwrap().valid);
    assert!(!sample_rgba(&rgba, 2, 1, Some(cov4), 1.0, 1.0, 0.0, &settings).unwrap().valid);
    // Bad buffers are errors, not panics.
    assert!(sample_rgba(&rgba, 4, 4, None, 1.0, 0.0, 0.0, &settings).is_err());
    assert!(sample_rgba(&rgba, -1, 1, None, 1.0, 0.0, 0.0, &settings).is_err());
    let short = Coverage { rgba: &[0; 4], width: 2, height: 1, opacity: 1.0 };
    assert!(sample_rgba(&rgba, 2, 1, Some(short), 1.0, 0.0, 0.0, &settings).is_err());
}

#[test]
fn names_round_trip_and_unknown_names_are_rejected() {
    for c in [Channel::AverageRgb, Channel::R, Channel::G, Channel::B, Channel::Luminance] {
        assert_eq!(Channel::from_name(c.name()).unwrap(), c);
    }
    for name in [
        "Nearest", "Box3x3", "Box5x5", "Gaussian3x3", "Gaussian5x5", "Median3x3", "Frontmost3x3", "Backmost3x3",
        "BoxCustom", "GaussianCustom", "MedianCustom", "FrontmostCustom", "BackmostCustom",
    ] {
        assert_eq!(Convolution::from_name(name).unwrap().name(), name);
    }
    for name in ["Replace", "Frontmost", "Backmost", "Add", "KeepExistingWhereMissing"] {
        assert_eq!(MergePolicy::from_name(name).unwrap().name(), name);
    }
    assert!(Channel::from_name("Alpha").is_err());
    assert!(Convolution::from_name("Lanczos").is_err());
    assert!(MergePolicy::from_name("Blend").is_err());
}

// --------------------------------------------------------------------------------- gap fill, cleanup

const GAP_DEPTH: [u8; 25] = [
    80, 80, 80, 80, 80, //
    80, 10, 0, 30, 80, //
    80, 80, 20, 80, 80, //
    80, 80, 0, 80, 80, //
    80, 80, 80, 80, 80,
];

fn gap_layer() -> Layer {
    let mut pixels = Vec::new();
    for d in GAP_DEPTH {
        pixels.extend_from_slice(&[d, d, d, 255]);
    }
    Layer {
        id: "gap-layer".into(),
        width: 5,
        height: 5,
        bounds: Rect { left: 0, top: 0, width: 5, height: 5 },
        depth_pixels: pixels,
        alpha_mask: vec![1; 25],
        ..Layer::default()
    }
}

#[test]
fn alpha_depth_gap_fill_matches_the_d_session_test() {
    let mut layer = gap_layer();
    // D feeds an extreme rule that normalizes to a rectangle outside the layer.
    let extreme = FocusedRule {
        layer_index: i32::MAX,
        x: i32::MAX,
        y: i32::MIN,
        w: i32::MAX,
        h: i32::MAX,
        lift: i32::MAX,
        radius: i32::MAX,
    };
    let summary = apply_layer_alpha_depth_gap_fill(&mut layer, 0, &[extreme]).unwrap();
    assert!(summary.succeeded && summary.changed);
    assert_eq!((summary.detected.total, summary.filled.filled, summary.filled.remaining), (5, 5, 0));
    assert_eq!(decode_depth_pixels(&layer.depth_pixels, layer.channel).unwrap(), vec![80u8; 25]);
    assert_eq!(layer.cleanup_operations.len(), 1);
    assert_eq!(layer.cleanup_operations[0].kind, CleanupKind::AlphaDepthGapFill);
    let stored = layer.cleanup_operations[0].focused_rules[0];
    assert_eq!((stored.x, stored.y, stored.w, stored.h, stored.lift, stored.radius), (5, 0, 0, 5, 255, MAX_FOCUSED_RULE_RADIUS));
    // The fill is repeatable from the filled depth: nothing left to detect.
    let again = apply_layer_alpha_depth_gap_fill(&mut layer, 0, &[]).unwrap();
    assert!(again.succeeded && !again.changed);
    assert_eq!((again.detected.total, again.filled.filled), (0, 0));
    // A layer without depth pixels reports failure without erroring.
    let mut empty = Layer::default();
    assert!(!apply_layer_alpha_depth_gap_fill(&mut empty, 0, &[]).unwrap().succeeded);
}

#[test]
fn gap_detection_reports_each_kind() {
    let depth: Vec<u8> = GAP_DEPTH.to_vec();
    let alpha = vec![1u8; 25];
    let d = detect_alpha_depth_gaps(&depth, &alpha, 5, 5, 0, &[]).unwrap();
    assert_eq!(d.zero, 2);
    assert_eq!(d.depression, 3);
    assert_eq!(d.total, 5);
    let mut expected = vec![0u8; 25];
    for i in [6, 7, 8, 12, 17] {
        expected[i] = 1;
    }
    assert_eq!(d.mask, expected);
    // Pixels outside the alpha mask are never gaps.
    let mut hole = alpha.clone();
    hole[7] = 0;
    assert_eq!(detect_alpha_depth_gaps(&depth, &hole, 5, 5, 0, &[]).unwrap().zero, 1);
    // A focused rule for this layer adds pixels the generic pass misses: lift 2 flags the 80s below p65 = 88.
    let rule = FocusedRule { layer_index: 0, x: 0, y: 0, w: 5, h: 1, lift: 2, radius: 16 };
    let focused = detect_alpha_depth_gaps(&depth, &alpha, 5, 5, 0, &[rule]).unwrap();
    assert_eq!(focused.focused_added, 5);
    assert_eq!(focused.total, 10);
    // A rule for another layer is ignored.
    assert_eq!(detect_alpha_depth_gaps(&depth, &alpha, 5, 5, 1, &[rule]).unwrap().total, 5);
    assert!(detect_alpha_depth_gaps(&depth, &alpha[..24], 5, 5, 0, &[]).is_err());
    assert!(detect_alpha_depth_gaps(&depth[..24], &alpha[..24], 5, 5, 0, &[]).is_err());
}

#[test]
fn median_fill_uses_the_median_of_valid_neighbours() {
    // One hole in a field of 100 with a column of 200 on the right.
    let mut depth = vec![100u8; 9];
    depth[4] = 0;
    depth[2] = 200;
    depth[5] = 200;
    depth[8] = 200;
    let alpha = vec![1u8; 9];
    let mut fill = vec![0u8; 9];
    fill[4] = 1;
    let r = median_fill_depth(&depth, &alpha, &fill, 3, 3).unwrap();
    // Neighbours: five 100s and three 200s -> sorted index 4 of 8 -> 100.
    assert_eq!(r.depth[4], 100);
    assert_eq!((r.filled, r.remaining), (1, 0));
    // With nothing valid anywhere the pixels stay pending.
    let r = median_fill_depth(&[0, 0, 0, 0], &[1; 4], &[1; 4], 2, 2).unwrap();
    assert_eq!((r.filled, r.remaining), (0, 4));
    assert!(median_fill_depth(&[0; 4], &[1; 3], &[1; 4], 2, 2).is_err());
}

#[test]
fn contour_band_erosion_and_inpaint() {
    // A 5x5 mask with a 3x3 block in the middle.
    let mut mask = vec![0u8; 25];
    for y in 1..4 {
        for x in 1..4 {
            mask[y * 5 + x] = 1;
        }
    }
    let band1 = build_layer_contour_band_mask(&mask, 5, 5, 1).unwrap();
    assert_eq!(band1.iter().filter(|v| **v != 0).count(), 8, "the border of the block, not its centre");
    assert_eq!(band1[12], 0);
    let band2 = build_layer_contour_band_mask(&mask, 5, 5, 2).unwrap();
    assert_eq!(band2.iter().filter(|v| **v != 0).count(), 9);
    // Erosion of a 3x3 positive block leaves its centre.
    let eroded = erode_positive_depth_mask(&mask, 5, 5, 1).unwrap();
    assert_eq!(eroded.iter().filter(|v| **v != 0).count(), 1);
    assert_eq!(eroded[12], 1);
    // Inpaint: a masked zero pixel inside a plateau takes the plateau value; unmasked zeros stay.
    let mut depth = vec![0u8; 25];
    for i in [6, 7, 8, 11, 13, 16, 17, 18] {
        depth[i] = 90;
    }
    let r = inpaint_masked_layer_depth(&depth, &mask, 5, 5).unwrap();
    assert_eq!(r.pixels[12], 90);
    assert_eq!(r.filled_mask[12], 1);
    assert_eq!(r.pixels[0], 0);
    assert!(inpaint_masked_layer_depth(&depth, &mask[..24], 5, 5).is_err());
}

#[test]
fn contour_repair_records_a_replayable_operation() {
    let mut layer = gap_layer();
    // A flat plateau with a rim of zeros under the alpha mask.
    let mut pixels = Vec::new();
    for i in 0..25 {
        let rim = i % 5 == 0 || i % 5 == 4 || i < 5 || i >= 20;
        let d = if rim { 0 } else { 90 };
        pixels.extend_from_slice(&[d, d, d, 255]);
    }
    layer.depth_pixels = pixels;
    layer.alpha_mask.clear();
    let summary = repair_layer_contour_depth(&mut layer, 1).unwrap();
    assert!(summary.succeeded && summary.changed);
    assert_eq!(summary.contour_pixels, 16);
    assert!(summary.filled_pixels > 0);
    assert_eq!(layer.cleanup_operations.len(), 1);
    assert_eq!(layer.cleanup_operations[0].kind, CleanupKind::ContourRepair);
    assert_eq!(layer.cleanup_operations[0].contour_thickness, 1);
    assert_eq!(normalize_contour_thickness(0), 1);
    assert_eq!(normalize_contour_thickness(999), 64);
}

#[test]
fn mask_helpers_follow_the_d_definitions() {
    let rgba = [1u8, 2, 3, 0, 4, 5, 6, 1, 7, 8, 9, 255];
    assert_eq!(alpha_mask_from_rgba(&rgba, 1).unwrap(), [0, 1, 1]);
    assert_eq!(alpha_mask_from_rgba(&rgba, 255).unwrap(), [0, 0, 1]);
    assert!(alpha_mask_from_rgba(&rgba[..11], 1).is_err());
    assert_eq!(mask_from_depth_pixels(&[0, 5, 255]), [0, 1, 1]);
    assert_eq!(apply_binary_mask(&[9, 9, 9, 9], &[1, 0, 1]), [9, 0, 9, 0]);
    // Alpha 0 decodes to 0; the average of (255, 0, 0) is 85.
    assert_eq!(decode_depth_pixels(&[255, 0, 0, 255, 255, 255, 255, 0], Channel::AverageRgb).unwrap(), [85, 0]);
    assert_eq!(decode_depth_pixels(&[255, 0, 0, 255], Channel::R).unwrap(), [255]);
    assert_eq!(
        normalize_focused_rule(FocusedRule { x: -4, y: 3, w: 99, h: -1, lift: 300, radius: -2, layer_index: 0 }, 10, 8),
        FocusedRule { x: 0, y: 3, w: 10, h: 0, lift: 255, radius: 0, layer_index: 0 }
    );
    assert!(!histogram_dimensions_supported(-1, 4));
    assert!(!histogram_dimensions_supported(20_000, 20_000));
    assert!(histogram_dimensions_supported(2000, 2000));
}

// --------------------------------------------------------------------------- session, reload, bind

fn two_layer_session() -> Session {
    let mut s = Session::new();
    for (id, path) in [("psd:0", "/a"), ("psd:1", "/b")] {
        s.layers.push(Layer {
            id: id.into(),
            layer_path: path.into(),
            display_name: path.trim_start_matches('/').into(),
            width: 4,
            height: 4,
            bounds: Rect { left: 0, top: 0, width: 4, height: 4 },
            ..Layer::default()
        });
    }
    s.bindings.push(Binding { layer_id: "psd:0".into(), target_grid_uuid: 42, ..Binding::default() });
    s.bindings.push(Binding { layer_id: "psd:1".into(), target_grid_uuid: 43, ..Binding::default() });
    s
}

#[test]
fn session_updates_dirty_only_the_bound_targets() {
    let mut s = two_layer_session();
    assert!(s.update_layer_z_transform("psd:1", -2.0, 2.0, true, 0.5, 0.25));
    assert_eq!(s.dirty_target_grid_ids(), [43]);
    assert_eq!(s.layers[1].z_scale, 0.5);
    assert_eq!(s.target_preview_revision(43), 1);
    s.clear_preview_dirty();
    assert!(s.update_layer_xy_transform("psd:0", [1.0, 2.0], [3.0, 4.0]));
    assert_eq!(s.dirty_target_grid_ids(), [42]);
    s.clear_preview_dirty();
    assert!(s.update_layer_visibility("psd:1", false, false));
    assert_eq!(s.dirty_target_grid_ids(), [43]);
    assert!(!s.layers[1].visible && !s.layers[1].enabled);
    s.clear_preview_dirty();
    assert!(s.update_binding_sampling("psd:1", 43, false, 0.45));
    assert!(!s.bindings[1].use_normal_layer_alpha);
    assert_eq!(s.bindings[1].coverage_threshold, 0.45);
    assert_eq!(s.dirty_target_grid_ids(), [43]);
    s.clear_preview_dirty();
    assert!(s.update_binding_state("psd:0", 42, true, 3, MergePolicy::Backmost));
    assert_eq!((s.bindings[0].order, s.bindings[0].merge_policy), (3, MergePolicy::Backmost));
    assert!(!s.update_layer_visibility("missing", true, true));
    assert!(!s.update_binding_state("psd:0", 99, true, 0, MergePolicy::Add));
    assert!(s.diagnostics_revision() > 0);
    s.clear();
    assert!(s.layers.is_empty() && s.bindings.is_empty() && s.dirty_target_grid_ids().is_empty());
}

#[test]
fn gpu_preview_normalizes_custom_median_and_dirties_everything() {
    let mut s = two_layer_session();
    s.layers[1].convolution = Convolution::MedianCustom;
    let mut display = s.display;
    display.use_gpu_preview = true;
    assert!(s.update_display_options(display));
    assert_eq!(s.layers[1].convolution, Convolution::Median3x3);
    assert_eq!(s.dirty_target_grid_ids(), [42, 43]);
    s.clear_preview_dirty();
    // Already active: a later request for the unsupported mode is normalized too, and only its target is dirtied.
    assert!(s.update_layer_sampling("psd:1", Channel::B, Convolution::MedianCustom, 6, 0.35));
    assert_eq!(s.layers[1].convolution, Convolution::Median3x3);
    assert_eq!((s.layers[1].channel, s.layers[1].custom_radius, s.layers[1].alpha_threshold), (Channel::B, 6, 0.35));
    assert_eq!(s.dirty_target_grid_ids(), [43]);
    s.clear_preview_dirty();
    assert!(!s.update_display_options(display), "no change, nothing dirty");
    assert!(s.dirty_target_grid_ids().is_empty());
}

#[test]
fn selection_by_layer_plane_prefers_the_topmost_layer() {
    let mut s = two_layer_session();
    s.layers[1].bounds = Rect { left: 2, top: 2, width: 4, height: 4 };
    assert!(s.select_layer_plane_at_document_point([3.0, 3.0]));
    assert_eq!((s.selected_layer_id.as_str(), s.selected_grid_uuid), ("psd:1", 43));
    assert!(s.select_layer_plane_at_document_point([0.5, 0.5]));
    assert_eq!((s.selected_layer_id.as_str(), s.selected_grid_uuid), ("psd:0", 42));
    assert!(!s.select_layer_plane_at_document_point([50.0, 50.0]));
    assert!(s.select_target_grid(43) && !s.select_target_grid(0) && !s.select_target_grid(99));
    assert!(s.select_layer("psd:0") && !s.select_layer("x"));
}

#[test]
fn reload_carries_settings_by_layer_key_and_replays_cleanup() {
    // The cleaned layer from the gap-fill test, saved, then reloaded from its pristine source pixels.
    let pristine = gap_layer();
    let mut cleaned = pristine.clone();
    apply_layer_alpha_depth_gap_fill(&mut cleaned, 0, &[]).unwrap();
    assert_ne!(cleaned.depth_pixels, pristine.depth_pixels);
    let mut previous = Session::new();
    previous.layers.push(Layer { xy_offset: [1.0, 2.0], sample_depth_scale: 0.25, ..cleaned.clone() });
    previous.bindings.push(Binding { layer_id: "gap-layer".into(), target_grid_uuid: 44, ..Binding::default() });
    previous.selected_layer_id = "gap-layer".into();
    previous.selected_grid_uuid = 44;
    let persisted = session_from_persistent_json(&session_to_persistent_json(&previous)).unwrap().unwrap();
    assert!(persisted.layers[0].depth_pixels.is_empty(), "pixels are not persisted");
    let mut reloaded = Session::new();
    reloaded.layers.push(Layer { cleanup_operations: Vec::new(), ..pristine });
    let result = carry_reload_state(&mut reloaded, &persisted).unwrap();
    assert_eq!(
        result,
        ReloadStateResult { matched_layers: 1, preserved_bindings: 1, preserved_selected_layer: true, preserved_selected_target: true }
    );
    assert_eq!(reloaded.layers[0].depth_pixels, cleaned.depth_pixels, "cleanup replayed on the fresh pixels");
    assert_eq!(reloaded.layers[0].cleanup_operations.len(), 1);
    assert_eq!(reloaded.layers[0].xy_offset, [1.0, 2.0]);
    assert_eq!(reloaded.layers[0].sample_depth_scale, 0.25);
    assert_eq!(reloaded.dirty_target_grid_ids(), [44]);
}

#[test]
fn reload_matches_duplicate_keys_one_to_one_and_ignores_unrelated_layers() {
    let make = |id: &str, offset: [f32; 2], scale: f32| Layer {
        id: id.into(),
        layer_path: "/duplicate".into(),
        display_name: "duplicate".into(),
        width: 4,
        height: 4,
        xy_offset: offset,
        sample_depth_scale: scale,
        ..Layer::default()
    };
    let mut previous = Session::new();
    previous.layers = vec![make("previous-a", [1.0, 2.0], 0.25), make("previous-b", [3.0, 4.0], 0.75)];
    previous.bindings = vec![
        Binding { layer_id: "previous-a".into(), target_grid_uuid: 501, ..Binding::default() },
        Binding { layer_id: "previous-b".into(), target_grid_uuid: 502, ..Binding::default() },
    ];
    previous.selected_layer_id = "previous-b".into();
    previous.selected_grid_uuid = 502;
    let mut reloaded = Session::new();
    reloaded.layers = vec![make("reloaded-a", [0.0; 2], 1.0), make("reloaded-b", [0.0; 2], 1.0)];
    let r = carry_reload_state(&mut reloaded, &previous).unwrap();
    assert_eq!((r.matched_layers, r.preserved_bindings), (2, 2));
    assert_eq!(reloaded.layers[0].xy_offset, [1.0, 2.0]);
    assert_eq!(reloaded.layers[1].xy_offset, [3.0, 4.0]);
    assert_eq!(reloaded.layers[1].sample_depth_scale, 0.75);
    assert_eq!(reloaded.bindings[0].layer_id, "reloaded-a");
    assert_eq!(reloaded.bindings[1].layer_id, "reloaded-b");
    assert_eq!(reloaded.selected_layer_id, "reloaded-b");

    // A same-size PSD layer at the same positional id but another name gets nothing.
    let psd = |path: &str| Layer {
        id: "psd:0".into(),
        layer_path: path.into(),
        display_name: path.trim_start_matches('/').into(),
        source_path: "same.psd".into(),
        width: 4,
        height: 4,
        ..Layer::default()
    };
    let mut unrelated_prev = Session::new();
    unrelated_prev.layers = vec![psd("/old-layer")];
    unrelated_prev.bindings = vec![Binding { layer_id: "psd:0".into(), target_grid_uuid: 501, ..Binding::default() }];
    let mut unrelated = Session::new();
    unrelated.layers = vec![psd("/new-layer")];
    let r = carry_reload_state(&mut unrelated, &unrelated_prev).unwrap();
    assert_eq!((r.matched_layers, r.preserved_bindings), (0, 0));
    assert!(unrelated.bindings.is_empty());

    // A lone flat image reloaded under a new path still takes the old settings.
    let flat = |path: &str, id: &str| Layer {
        id: id.into(),
        source_path: path.into(),
        layer_path: path.into(),
        width: 2,
        height: 2,
        ..Layer::default()
    };
    let mut flat_prev = Session::new();
    flat_prev.layers = vec![Layer { z_scale: 3.0, ..flat("a.png", "f1") }];
    let mut flat_new = Session::new();
    flat_new.layers = vec![flat("b.PNG", "f2")];
    assert_eq!(carry_reload_state(&mut flat_new, &flat_prev).unwrap().matched_layers, 1);
    assert_eq!(flat_new.layers[0].z_scale, 3.0);
}

fn nodes() -> Vec<BindNode> {
    let part = |uuid, name: &str, path: &str, target: Option<(u64, &str)>| BindNode {
        uuid,
        name: name.into(),
        layer_path: path.into(),
        is_part: true,
        direct_target: false,
        target: target.map(|(u, n)| (u, n.to_owned())),
    };
    vec![
        part(10, "Face", "/Head/Face", Some((100, "FaceGrid"))),
        part(11, "Hair", "", None),
        part(12, "Body", "/Torso/Body", Some((101, "BodyGrid"))),
        part(13, "Body", "/Other/Body", Some((102, "OtherGrid"))),
        BindNode {
            uuid: 101,
            name: "ArmGrid".into(),
            layer_path: String::new(),
            is_part: false,
            direct_target: true,
            target: Some((101, "ArmGrid".into())),
        },
    ]
}

fn bind(path: &str, name: &str, direct: bool) -> AutoBindResult {
    let layer = Layer { id: "L".into(), layer_path: path.into(), display_name: name.into(), ..Layer::default() };
    auto_bind_layer(&nodes(), &layer, direct)
}

#[test]
fn auto_bind_priorities_and_statuses() {
    let exact = bind("/Head/Face", "Face", true);
    assert_eq!(
        (exact.status.as_str(), exact.matched_node_uuid, exact.target_grid_uuid, exact.target_grid_name.as_str()),
        ("Matched", 10, 100, "FaceGrid")
    );
    // Base name match when the path differs.
    assert_eq!(bind("/x/Face", "Face", true).matched_node_uuid, 10);
    // The exact path wins over a base-name match on another part.
    let body = bind("/Torso/Body", "Body", true);
    assert_eq!((body.status.as_str(), body.matched_node_uuid), ("Matched", 12));
    // Two parts with the same base name and no exact path are ambiguous.
    let ambiguous = bind("/elsewhere/Body", "Body", true);
    assert!(ambiguous.ambiguous && !ambiguous.matched);
    assert_eq!(ambiguous.status, "Ambiguous");
    // A part with no depth target ancestor: the path falls back to "/name".
    let hair = bind("/Hair", "Hair", true);
    assert_eq!((hair.status.as_str(), hair.matched), ("UnmatchedWithoutGrid", false));
    assert_eq!(hair.matched_node_name, "Hair");
    // A depth-mapped grid is matched by name only when asked.
    assert_eq!(bind("/Arm", "ArmGrid", true).status, "Matched");
    assert_eq!(bind("/Arm", "ArmGrid", false).status, "Unmatched");
    assert_eq!(bind("/nothing", "", true).status, "Unmatched");
    // The display name falls back to the last path segment.
    assert_eq!(bind("C:\\psd\\Face", "", true).layer_name, "Face");
}

#[test]
fn auto_bind_session_reuses_stale_bindings_and_appends_new_ones() {
    let mut s = Session::new();
    for (id, path, name) in [("l0", "/Head/Face", "Face"), ("l1", "/Torso/Body", "Body"), ("l2", "/none", "none")] {
        s.layers.push(Layer { id: id.into(), layer_path: path.into(), display_name: name.into(), ..Layer::default() });
    }
    // l0 has a stale binding to a node that is no longer a depth target; l1 already binds the right grid.
    s.bindings.push(Binding { layer_id: "l0".into(), target_grid_uuid: 999, ..Binding::default() });
    s.bindings.push(Binding { layer_id: "l1".into(), target_grid_uuid: 101, enabled: false, ..Binding::default() });
    let results = auto_bind_session(&mut s, &nodes(), true, &|uuid| uuid == 101);
    assert_eq!(results.iter().map(|r| r.status.as_str()).collect::<Vec<_>>(), ["Matched", "Matched", "Unmatched"]);
    assert_eq!(s.bindings.len(), 2, "the stale binding was replaced, the existing one re-enabled");
    assert_eq!((s.bindings[0].target_grid_uuid, s.bindings[0].target_node_uuid, s.bindings[0].order), (100, 10, 0));
    assert!(s.bindings[1].enabled);
    assert_eq!(s.bindings[1].target_node_uuid, 12);
    let b = binding_from_auto_bind(&results[2], 7);
    assert!(!b.enabled && b.order == 7);
    // A layer with no binding gets a new one appended.
    let mut fresh = Session::new();
    fresh.layers.push(Layer { id: "n".into(), layer_path: "/Head/Face".into(), display_name: "Face".into(), ..Layer::default() });
    auto_bind_session(&mut fresh, &nodes(), true, &|_| true);
    assert_eq!(fresh.bindings.len(), 1);
    assert_eq!(fresh.bindings[0].target_grid_uuid, 100);
}

// --------------------------------------------------------------------------------------- diagnostics

#[test]
fn diagnostics_rows_summary_and_sorting() {
    let (layers, mut bindings, points) = session_back_front();
    let mut s = Session::new();
    s.layers = layers;
    s.bindings = bindings.clone();
    let base = [0.25f32; 4];
    let result =
        compose_target(&s.layers, &s.bindings, &Target { grid_uuid: 5, document_points: &points, base_depths: &base }).unwrap();
    let rows = layer_stack_rows(&s, Some(&result), Some(&|uuid| format!("grid-{uuid}")));
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].sampled_vertices, rows[0].missing_vertices, rows[0].winning_vertices), (1, 3, 0));
    assert!(rows[0].warning_mostly_missing_samples && !rows[1].warning_mostly_missing_samples);
    assert_eq!(rows[1].target_display_name, "grid-5");
    assert!(rows[1].has_depth_range && near(rows[1].min_depth, 0.75) && near(rows[1].max_depth, 0.75));
    assert!(rows.iter().all(|r| r.has_normal_coverage && !r.warning_missing_coverage));

    let by_sampled = filter_and_sort_rows(rows.clone(), "", StackSort::Sampled, true);
    assert_eq!(by_sampled.iter().map(|r| r.layer_id.as_str()).collect::<Vec<_>>(), ["front", "back"]);
    assert_eq!(filter_and_sort_rows(rows.clone(), "GRID-5", StackSort::SourceOrder, false).len(), 2);
    assert_eq!(filter_and_sort_rows(rows.clone(), "front", StackSort::Name, false).len(), 1);
    assert_eq!(filter_and_sort_rows(rows, "", StackSort::DepthMin, false)[0].layer_id, "back");

    let d = source_diagnostics(&s);
    assert_eq!((d.total_layers, d.usable_depth_layers, d.layers_missing_target, d.enabled_bindings), (2, 2, 0, 2));
    bindings.push(Binding { layer_id: "front".into(), target_grid_uuid: 6, ..Binding::default() });
    s.bindings = bindings;
    assert_eq!(source_diagnostics(&s).layers_with_ambiguous_bindings, 1);

    let composite = composite_preview_summary(&result);
    assert_eq!((composite.vertex_count, composite.winning_vertices, composite.final_missing_vertices), (4, 2, 2));
    assert!(composite.points[0].sampled && composite.points[2].missing);
    assert_eq!(composite.points[0].winning_layer_color, winning_layer_color("front"));

    // Back layer range [-1, -1], front [0.75, 0.75]: a gap of 1.75, no overlap.
    let space = depth_space_summary(&s, Some(&result));
    assert_eq!(space.layers.len(), 2);
    assert_eq!(space.gaps.len(), 1);
    let gap = &space.gaps[0];
    assert!(gap.valid && !gap.overlap);
    assert!(near(gap.back_depth, -1.0) && near(gap.front_depth, 0.75));
    assert!(near(space.layers[0].representative_depth, -1.0));
}

#[test]
fn winning_layer_color_is_the_fnv_hash_mapped_into_a_band() {
    // FNV-1a of "" is the offset basis 2166136261 = 0x811C9DC5: r = 0xC5, g = 0x9D, b = 0x1C.
    let c = winning_layer_color("");
    assert!(near(c[0], 0.25 + 0xC5 as f32 / 255.0 * 0.7));
    assert!(near(c[1], 0.25 + 0x9D as f32 / 255.0 * 0.7));
    assert!(near(c[2], 0.25 + 0x1C as f32 / 255.0 * 0.7));
    assert_eq!(c[3], 1.0);
    assert_ne!(winning_layer_color("a"), winning_layer_color("b"));
}

// ------------------------------------------------------------------------------------------- fit z

#[test]
fn fit_z_maps_the_raw_range_into_the_gap() {
    let raw = range_from_values(&[f32::NAN, 0.2, -0.4, 0.6, f32::INFINITY]);
    assert_eq!(raw, DepthRange { valid: true, min_depth: -0.4, max_depth: 0.6 });
    // Gap [1, 2] with a 0.1 margin: scale 0.8 / 1.0, offset 1.1 - (-0.4)(0.8).
    let fit = fit_z_to_gap(raw, 1.0, 2.0, 0.1);
    assert!(fit.succeeded);
    assert!(near(fit.z_scale, 0.8) && near(fit.z_offset, 1.42));
    assert!(near(fit.target_min, 1.1) && near(fit.target_max, 1.9));
    // A flat raw range is centred in the gap with unit scale.
    let flat = fit_z_to_gap(range_from_values(&[0.5, 0.5]), 1.0, 2.0, 0.0);
    assert!(near(flat.z_scale, 1.0) && near(flat.z_offset, 1.0));
    assert_eq!(fit_z_to_gap(raw, 1.0, 1.1, 0.1).error, "gap is empty after margin");
    assert_eq!(fit_z_to_gap(DepthRange::default(), 1.0, 2.0, 0.0).error, "raw range has no usable samples");
    assert_eq!(fit_z_to_gap(raw, f32::NAN, 2.0, 0.0).error, "gap values must be finite");
    let mut layer = Layer::default();
    assert!(apply_fit_z_to_gap(&mut layer, raw, 1.0, 2.0, 0.1).succeeded);
    assert!(near(layer.z_scale, 0.8) && near(layer.z_offset, 1.42));
    let mut untouched = Layer::default();
    assert!(!apply_fit_z_to_gap(&mut untouched, raw, 2.0, 1.0, 0.0).succeeded);
    assert_eq!((untouched.z_scale, untouched.z_offset), (1.0, 0.0));
}

#[test]
fn gaps_come_from_neighbouring_target_or_selected_ranges() {
    let r = |a, b| DepthRange { valid: true, min_depth: a, max_depth: b };
    let ranges = [r(-1.0, -0.5), DepthRange::default(), r(0.5, 1.0)];
    let gap = gap_from_adjacent_ranges(&ranges, 1);
    assert!(gap.valid && gap.back == -0.5 && gap.front == 0.5);
    assert_eq!(gap_from_adjacent_ranges(&ranges, 0).error, "adjacent layers are required");
    assert_eq!(gap_from_adjacent_ranges(&ranges, 2).error, "adjacent layers are required");
    assert_eq!(
        gap_from_adjacent_ranges(&[DepthRange::default(), r(0.0, 0.0), r(1.0, 1.0)], 1).error,
        "back adjacent layer has no usable range"
    );
    assert_eq!(
        gap_from_adjacent_ranges(&[r(0.0, 0.0), r(0.0, 0.0), DepthRange::default()], 1).error,
        "front adjacent layer has no usable range"
    );
    assert_eq!(gap_from_adjacent_ranges(&[r(0.0, 2.0), r(0.0, 0.0), r(1.0, 3.0)], 1).error, "adjacent layer ranges overlap");
    let t = gap_from_target_range(r(0.2, 0.9));
    assert!(t.valid && t.back == 0.2 && t.front == 0.9);
    assert_eq!(gap_from_target_range(DepthRange::default()).error, "target has no usable depth range");
    assert!(gap_from_selected_layer_ranges(r(0.0, 0.1), r(0.2, 0.3)).valid);
    assert_eq!(gap_from_selected_layer_ranges(r(0.0, 0.5), r(0.2, 0.3)).error, "selected layer ranges overlap");
    assert_eq!(
        gap_from_selected_layer_ranges(DepthRange::default(), r(0.2, 0.3)).error,
        "selected back layer has no usable range"
    );
    let bad = FitZGap { valid: false, error: "no gap".into(), back: 0.0, front: 0.0 };
    assert_eq!(fit_z_to_gap_checked(r(0.0, 1.0), &bad, 0.0).error, "no gap");
    assert_eq!(fit_z_to_gap_checked(r(0.0, 1.0), &FitZGap::default(), 0.0).error, "gap is invalid");
    let fit = fit_z_to_gap_checked(r(0.0, 1.0), &t, 0.0);
    let diag = fit_z_diagnostics(r(0.0, 1.0), &t, &fit);
    assert!(diag.succeeded && diag.gap_valid && diag.raw_range_valid);
    assert!(near(diag.z_scale, 0.7) && near(diag.z_offset, 0.2));
}

// ---------------------------------------------------------------------------------------- readback

#[test]
fn readback_comparison_counts_each_mismatch_kind() {
    let cpu_depths = [0.25, 0.75];
    let cpu_winners = ["".to_owned(), "front".to_owned()];
    let same = compare_readback(&cpu_depths, &cpu_winners, &[0.25, 0.750001], &cpu_winners, 1e-5);
    assert!(same.ok);
    let off = compare_readback(&cpu_depths, &cpu_winners, &[0.26, 0.75], &cpu_winners, 1e-5);
    assert!(!off.ok && off.depth_mismatches == 1);
    assert_eq!(off.error, "DepthDraw readback depth mismatch");
    let wrong = ["x".to_owned(), "front".to_owned()];
    let r = compare_readback(&cpu_depths, &cpu_winners, &cpu_depths, &wrong, 1e-5);
    assert_eq!((r.winner_mismatches, r.missing_flag_mismatches), (1, 1));
    assert_eq!(
        compare_readback(&cpu_depths, &cpu_winners, &[0.0], &cpu_winners, 1e-5).error,
        "DepthDraw readback depth length mismatch"
    );
    assert_eq!(
        compare_readback(&cpu_depths, &cpu_winners, &cpu_depths, &wrong[..1], 1e-5).error,
        "DepthDraw readback winner length mismatch"
    );
    assert!(!compare_readback(&[], &[], &[], &[], f32::NAN).ok);
    assert!(compare_readback(&[f32::NAN], &["".to_owned()], &[f32::NAN], &["".to_owned()], 0.0).ok, "NaN matches NaN");

    let (layers, bindings, points) = session_back_front();
    let base = [0.25f32; 4];
    let cpu =
        compose_target(&layers, &bindings, &Target { grid_uuid: 5, document_points: &points, base_depths: &base }).unwrap();
    assert!(compare_compose_readback(&cpu, &cpu, 1e-5).ok);
    let mut other = cpu.clone();
    other.target_grid_uuid = 6;
    other.layer_stats[1].winning_vertices = 0;
    other.sampled_vertices += 1;
    let r = compare_compose_readback(&cpu, &other, 1e-5);
    assert_eq!((r.target_mismatches, r.layer_stat_mismatches, r.aggregate_stat_mismatches), (1, 1, 1));
    other.layer_stats.pop();
    let r = compare_compose_readback(&cpu, &other, 1e-5);
    assert_eq!(r.error, "DepthDraw readback target GridDeformer mismatch");
    assert_eq!(r.layer_stat_mismatches, 1);
}

#[test]
fn apply_summary_counts_changed_vertices_and_skipped_layers() {
    let (layers, bindings, points) = session_back_front();
    let base = [0.25f32; 4];
    let result =
        compose_target(&layers, &bindings, &Target { grid_uuid: 5, document_points: &points, base_depths: &base }).unwrap();
    let summary = summarize_apply(&result, 5, 4, Some(&[0.25; 4]));
    assert!(summary.succeeded);
    assert_eq!(
        (summary.changed_vertices, summary.changed_targets, summary.layers_applied, summary.layers_skipped),
        (2, 1, 1, 1)
    );
    assert_eq!(summary.missing_samples, 5);
    assert_eq!(summarize_apply(&result, 5, 4, Some(&[0.75, 0.75, 0.25, 0.25])).changed_targets, 0);
    // A target that cannot hold depths (no stored array) or a wrong grid or count never succeeds.
    assert!(!summarize_apply(&result, 5, 4, None).succeeded);
    assert!(!summarize_apply(&result, 6, 4, Some(&[0.0; 4])).succeeded);
    assert!(!summarize_apply(&result, 5, 3, Some(&[0.0; 3])).succeeded);
    assert_eq!(count_changed_vertices(&[1.0], &[1.0, 2.0]), 2);
}

// -------------------------------------------------------------------------------- manifest, png export

fn rich_session() -> Session {
    let mut s = two_layer_session();
    s.selected_layer_id = "psd:1".into();
    s.selected_grid_uuid = 43;
    s.source_identity = "model.psd".into();
    s.document_width = 640;
    s.document_height = 480;
    s.display.show_raw_depth = true;
    s.layers[0].channel = Channel::Luminance;
    s.layers[0].convolution = Convolution::GaussianCustom;
    s.layers[0].custom_radius = 5;
    s.layers[0].xy_scale = [2.0, 0.5];
    s.layers[0].cleanup_operations = vec![
        CleanupOperation {
            kind: CleanupKind::AlphaDepthGapFill,
            contour_thickness: 2,
            focused_rules: vec![FocusedRule { layer_index: 0, x: 1, y: 2, w: 3, h: 1, lift: 10, radius: 4 }],
        },
        CleanupOperation { kind: CleanupKind::ContourRepair, contour_thickness: 3, focused_rules: vec![] },
    ];
    s.bindings[1].merge_policy = MergePolicy::Add;
    s.bindings[1].coverage_threshold = 0.2;
    s.bindings[1].target_node_uuid = 18_446_744_073_709_551_615;
    s
}

#[test]
fn manifest_round_trips_every_field_with_the_d_keys() {
    let s = rich_session();
    let manifest = session_to_manifest(&s);
    assert_eq!(manifest["version"], 1);
    assert_eq!(manifest["selectedGridUuid"], "43", "uuids are strings in the manifest");
    assert_eq!(manifest["bindings"][1]["targetNodeUuid"], "18446744073709551615");
    assert_eq!(manifest["layers"][0]["channel"], "Luminance");
    assert_eq!(manifest["layers"][0]["convolution"], "GaussianCustom");
    assert_eq!(manifest["layers"][0]["cleanupOperations"][1]["kind"], "ContourRepair");
    assert_eq!(manifest["display"]["showRawDepth"], true);
    let back = session_from_manifest(&manifest).unwrap();
    assert_eq!(back.layers, s.layers);
    assert_eq!(back.bindings, s.bindings);
    assert_eq!((back.selected_layer_id.as_str(), back.selected_grid_uuid, back.document_width), ("psd:1", 43, 640));
    assert_eq!(back.display, s.display);
    assert_eq!(session_to_manifest(&back), manifest);
}

#[test]
fn manifest_defaults_clamping_and_errors() {
    let s = session_from_manifest(&json!({"layers": [{"id": "a", "width": 8, "height": 6,
        "channel": "G", "customRadius": 999, "cleanupOperations": [{"kind": "AlphaDepthGapFill",
        "focusedRules": [{"layerIndex": -4, "x": -2, "y": 99, "w": 99, "h": 99, "lift": 999, "radius": 999}]},
        {"kind": "ContourRepair", "contourThickness": 1000}]}],
        "bindings": [{"layerId": "a", "targetGridUuid": "12"}]}))
    .unwrap();
    let l = &s.layers[0];
    assert_eq!(l.bounds, Rect { left: 0, top: 0, width: 8, height: 6 });
    assert_eq!((l.opacity, l.z_scale, l.back_depth, l.front_depth), (1.0, 1.0, -1.0, 1.0));
    assert_eq!((l.channel, l.convolution, l.custom_radius), (Channel::G, Convolution::Gaussian3x3, 64));
    let rule = l.cleanup_operations[0].focused_rules[0];
    assert_eq!(rule, FocusedRule { layer_index: 0, x: 0, y: 6, w: 8, h: 0, lift: 255, radius: 64 });
    assert_eq!(l.cleanup_operations[1].contour_thickness, 64);
    let b = &s.bindings[0];
    assert_eq!((b.target_grid_uuid, b.enabled, b.use_normal_layer_alpha, b.merge_policy), (12, true, true, MergePolicy::Frontmost));
    for bad in [
        json!([]),
        json!({"layers": 3}),
        json!({"layers": [3]}),
        json!({"layers": [{"channel": "Alpha"}]}),
        json!({"layers": [{"width": "wide"}]}),
        json!({"layers": [{"bounds": [1, 2]}]}),
        json!({"layers": [{"cleanupOperations": [{"kind": "Blur"}]}]}),
        json!({"bindings": [{"mergePolicy": "Mix"}]}),
        json!({"bindings": [{"targetGridUuid": "twelve"}]}),
        json!({"selectedGridUuid": 7}),
        json!({"display": 4}),
    ] {
        assert!(session_from_manifest(&bad).is_err(), "{bad}");
    }
    assert!(session_from_persistent_json("").unwrap().is_none());
    assert!(session_from_persistent_json("{").is_err());
    let rt = session_from_persistent_json(&session_to_persistent_json(&rich_session())).unwrap().unwrap();
    assert_eq!(rt.layers, rich_session().layers);
}

#[test]
fn manifest_loading_normalizes_the_gpu_unsupported_median() {
    let mut s = rich_session();
    s.layers[1].convolution = Convolution::MedianCustom;
    let mut manifest = session_to_manifest(&s);
    assert_eq!(session_from_manifest(&manifest).unwrap().layers[1].convolution, Convolution::MedianCustom);
    manifest["display"]["useGpuPreview"] = json!(true);
    assert_eq!(session_from_manifest(&manifest).unwrap().layers[1].convolution, Convolution::Median3x3);
}

#[test]
fn manifest_validation_reports_each_problem() {
    let mut s = rich_session();
    s.layers[0].source_path = "gone.png".into();
    s.layers.push(Layer { id: "psd:0".into(), ..Layer::default() });
    s.bindings.push(Binding { layer_id: "ghost".into(), target_grid_uuid: 50, ..Binding::default() });
    s.bindings.push(Binding { layer_id: "psd:0".into(), target_grid_uuid: 0, ..Binding::default() });
    s.bindings.push(Binding { layer_id: "psd:1".into(), target_grid_uuid: 43, ..Binding::default() });
    s.selected_layer_id = "nothing".into();
    s.selected_grid_uuid = 77;
    let v = validate_manifest(&s, Some(&|uuid| uuid != 42), &|path| path != "gone.png");
    assert!(!v.ok());
    assert_eq!(v.missing_source_layer_ids, ["psd:0"]);
    assert_eq!(v.duplicate_layer_ids, ["psd:0"]);
    assert_eq!(v.missing_binding_layer_ids, ["ghost"]);
    assert_eq!(v.duplicate_binding_keys, ["psd:1\u{0}43"]);
    assert_eq!(v.missing_target_layer_ids, ["psd:0", "psd:0"], "grid 42 is gone, and one binding has no grid");
    assert_eq!(v.missing_selected_layer_id, "nothing");
    assert_eq!(v.missing_selected_grid_uuid, 77);
    assert!(validate_manifest(&two_layer_session(), None, &|_| true).ok());
}

#[test]
fn png_export_writes_grey_depth_and_drops_pixels() {
    let mut s = Session::new();
    let mut px = Vec::new();
    for (r, a) in [(255u8, 255u8), (0, 128)] {
        px.extend_from_slice(&[r, 0, 0, a]);
    }
    s.layers.push(Layer {
        id: "a b/c".into(),
        width: 2,
        height: 1,
        depth_pixels: px,
        channel: Channel::R,
        cleanup_operations: vec![CleanupOperation { kind: CleanupKind::ContourRepair, contour_thickness: 1, focused_rules: vec![] }],
        ..Layer::default()
    });
    s.layers.push(Layer { id: String::new(), display_name: "No Pixels".into(), width: 2, height: 2, ..Layer::default() });
    assert_eq!(layer_to_export_rgba(&s.layers[0]).unwrap(), [255, 255, 255, 255, 0, 0, 0, 128]);
    assert!(layer_to_export_rgba(&s.layers[1]).is_err());
    assert_eq!(safe_layer_file_name(&s.layers[0], 0), "a_b_c");
    assert_eq!(safe_layer_file_name(&s.layers[1], 1), "No_Pixels");
    assert_eq!(safe_layer_file_name(&Layer::default(), 4), "layer-4");
    let export = export_png_session(&s).unwrap();
    assert_eq!((export.exported_layers, export.skipped_layers), (1, 1));
    assert_eq!(export.files[0].0, "000-a_b_c.png");
    assert_eq!(&export.files[0].1[..8], b"\x89PNG\r\n\x1a\n");
    let decoded = image::load_from_memory(&export.files[0].1).unwrap().to_rgba8();
    assert_eq!(decoded.as_raw().as_slice(), &[255, 255, 255, 255, 0, 0, 0, 128]);
    assert_eq!(export.manifest["layers"][0]["sourcePath"], "000-a_b_c.png");
    assert_eq!(export.manifest["layers"][0]["cleanupOperations"], json!([]), "cleanup is baked into the PNG");
    assert_eq!(export.manifest["layers"][1]["sourcePath"], "");
}

// ------------------------------------------------------------------------------------------ tool maths

#[test]
fn depth_camera_projection_round_trips_and_clamps() {
    let flat = DepthCamera::default();
    assert_eq!(project_depth_point([3.0, 4.0], 9.0, &flat), [3.0, 4.0], "depth has no screen effect in the top view");
    let cam = DepthCamera { yaw: 0.5, pitch: -0.3, zoom: 2.0, pan: [10.0, -5.0] };
    for depth in [0.0, 1.5, -2.0] {
        let p = project_depth_point([12.0, -7.0], depth, &cam);
        let m = unproject_depth_point(p, depth, &cam);
        assert!((m[0] - 12.0).abs() < 1e-3 && (m[1] + 7.0).abs() < 1e-3, "{depth}: {m:?}");
    }
    // Yaw of 90 degrees: depth moves the point sideways in the view.
    let side = DepthCamera { yaw: std::f32::consts::FRAC_PI_2, ..DepthCamera::default() };
    assert!(near(project_depth_point([0.0, 0.0], 1.0, &side)[0], 1.0));
    let mut c = DepthCamera::default();
    c.rotate(100.0, 10_000.0);
    assert!(near(c.yaw, -1.0) && near(c.pitch, 1.35));
    c.zoom_by(1.0);
    assert!(near(c.zoom, 1.08));
    c.zoom_by(10_000.0);
    assert_eq!(c.zoom, 8.0);
    c.zoom_by(-10_000.0);
    assert_eq!(c.zoom, 0.1);
    c.pan_by(1.0, 2.0);
    assert_eq!(c.pan, [1.0, 2.0]);
}

#[test]
fn depth_display_scale_and_rounding() {
    assert!(near(depth_display_scale_for_bounds([0.0, 0.0], [5.0, 5.0]), 1.0), "never below 1");
    assert!((depth_display_scale_for_bounds([0.0, 0.0], [2900.0, 100.0]) - 420.0).abs() < 1e-2);
    assert_eq!(round_depth(0.12345), 0.123);
    assert_eq!(round_depth(-0.0005), -0.001);
    assert_eq!(clamp_depth(f32::NAN), 0.0);
    assert_eq!(clamp_depth(f32::INFINITY), 0.0);
    assert_eq!(clamp_depth(0.2004), 0.2);
}

#[test]
fn direct_depth_drag_from_literal_pointer_travel() {
    // Dragging up by 50 px adds 0.3 to every dragged vertex; others stay.
    let delta = visible_depth_delta([5.0, 50.0], [5.0, 100.0], [0.0, 0.0]);
    assert!(near(delta, 0.3));
    let start = [0.0, 0.1, -0.2, 0.5];
    let moved = direct_depth_drag(&start, &[1, 2, 9], delta);
    assert_eq!(moved, [0.0, 0.4, 0.1, 0.5]);
    // With a visible depth axis the delta is the projection of the pointer travel on it.
    assert!(near(visible_depth_delta([30.0, 0.0], [10.0, 0.0], [10.0, 0.0]), 2.0));
    assert!(near(visible_depth_delta([10.0, 40.0], [10.0, 0.0], [10.0, 0.0]), 0.0));
}

#[test]
fn selection_rules_for_click_and_rectangle() {
    let mut sel = vec![1, 2];
    assert_eq!(click_selection(&mut sel, 2, false, false), Some(2), "kept as a group until release");
    assert_eq!(sel, [1, 2]);
    assert_eq!(click_selection(&mut sel, 5, false, false), None);
    assert_eq!(sel, [5]);
    click_selection(&mut sel, 6, false, true);
    assert_eq!(sel, [5, 6]);
    click_selection(&mut sel, 5, false, true);
    assert_eq!(sel, [6]);
    click_selection(&mut sel, 6, true, false);
    assert!(sel.is_empty());
    let pts = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
    assert_eq!(vertices_in_rect(&pts, [9.0, 11.0], [-1.0, -1.0]), [0, 3]);
    assert_eq!(vertices_in_rect(&pts, [5.0, -1.0], [11.0, 5.0]), [1]);
    assert_eq!(nearest_vertex(&pts, [8.0, 1.0], 5.0), Some(1));
    assert_eq!(nearest_vertex(&pts, [5.0, 5.0], 1.0), None);
    assert_eq!(nearest_vertex(&[], [0.0, 0.0], f32::MAX), None);
    assert_eq!(snap_to_vertex(&pts, [9.0, 9.0]), [10.0, 10.0]);
    assert_eq!(snap_to_vertex(&[], [9.0, 9.0]), [9.0, 9.0]);
}

#[test]
fn tool_gestures_produce_the_documented_numbers() {
    assert!(near(attached_point_amount(100.0, 0.0), 0.6), "100 px up");
    assert!(near(attached_point_amount(100.0, 200.0), -0.6), "100 px down");
    let s = BrushSettings::default();
    // A real drag keeps its endpoints.
    assert_eq!(ring_endpoints([0.0, 0.0], [30.0, 40.0], &s), ([0.0, 0.0], [30.0, 40.0]));
    // A click makes a ring of length 2 * radius_x along the angle.
    let (p0, p1) = ring_endpoints([10.0, 10.0], [10.0, 10.5], &BrushSettings { angle: 0.0, radius_x: 58.0, ..s });
    assert_eq!((p0, p1), ([-48.0, 10.0], [68.0, 10.5]));
    let (q0, q1) = ring_endpoints([0.0, 0.0], [0.0, 0.0], &BrushSettings { angle: 90.0, radius_x: 10.0, ..s });
    assert!(near(q0[1], -10.0) && near(q1[1], 10.0) && q0[0].abs() < 1e-4);
    let (l0, l1) = ring_preview_line([1.0, 1.0], [1.0, 1.0], &BrushSettings { angle: 0.0, radius_x: 5.0, ..s });
    assert_eq!((l0, l1), ([1.0, 1.0], [6.0, 1.0]));
    assert_eq!(plane_radii([0.0, 0.0], [1.0, -2.0]), (4.0, 4.0));
    assert_eq!(plane_radii([0.0, 0.0], [30.0, -12.0]), (30.0, 12.0));
}

#[test]
fn op_json_matches_the_session_vocabulary() {
    let s = BrushSettings::default();
    let ring = ring_op([0.0, 0.0], [10.0, 0.0], &s);
    assert_eq!(ring["type"], "ring");
    assert_eq!(ring["p1"], json!([10.0, 0.0]));
    assert_eq!(ring["width"].as_f64(), Some(42.0), "width comes from radius_y");
    assert_eq!((ring["p0Angle"].as_f64(), ring["p1Angle"].as_f64()), (Some(180.0), Some(0.0)));
    assert!((ring["hardness"].as_f64().unwrap() - 1.85).abs() < 1e-6);
    let plane = plane_op([5.0, 6.0], 30.0, 12.0, &s);
    assert_eq!(plane["type"], "plane");
    assert!((plane["targetDepth"].as_f64().unwrap() - 0.18).abs() < 1e-6, "amount is the plane's target depth");
    assert!((plane["flattenStrength"].as_f64().unwrap() - 0.75).abs() < 1e-6);
    assert_eq!(attached_point_op(3, 0.5)["index"], 3);
    assert_eq!(op_label(&attached_point_op(3, 0.5)), "attached point 3");
    assert_eq!(op_label(&ring_op([1.4, 2.6], [0.0, 0.0], &s)), "ring 1,3");
    assert_eq!(op_label(&plane), "plane 5,6");
    assert_eq!(op_value_label(&plane), "0.18");
    assert_eq!(op_value_label(&attached_point_op(3, -0.456)), "-0.46");
    assert_eq!(op_label(&json!({})), "operation");
}

#[test]
fn ring_edit_handles_and_drags() {
    let (p0, p1) = ([0.0, 0.0], [100.0, 0.0]);
    assert_eq!(ring_hit(p0, p1, [3.0, 4.0], 14.0).map(|h| h.0), Some(RingHandle::P0));
    assert_eq!(ring_hit(p0, p1, [97.0, 1.0], 14.0).map(|h| h.0), Some(RingHandle::P1));
    assert_eq!(ring_hit(p0, p1, [50.0, 10.0], 14.0).map(|h| h.0), Some(RingHandle::Body));
    assert_eq!(ring_hit(p0, p1, [50.0, 30.0], 14.0), None);
    let grid = [[0.0, 0.0], [50.0, 0.0], [100.0, 0.0]];
    assert_eq!(ring_drag(RingHandle::P1, p0, p1, [100.0, 0.0], [48.0, 3.0], Some(&grid)), (p0, [50.0, 0.0]));
    assert_eq!(ring_drag(RingHandle::P0, p0, p1, [0.0, 0.0], [20.0, 20.0], None), ([20.0, 20.0], p1));
    assert_eq!(ring_drag(RingHandle::Body, p0, p1, [50.0, 5.0], [60.0, 15.0], None), ([10.0, 10.0], [110.0, 10.0]));
}

#[test]
fn apply_plan_follows_d_apply_to_targets() {
    assert_eq!(apply_plan(false, false, false), ApplyPlan::Nothing);
    assert_eq!(apply_plan(true, false, false), ApplyPlan::Nothing);
    assert_eq!(apply_plan(false, true, false), ApplyPlan::SetOpsThenApply);
    assert_eq!(apply_plan(true, true, false), ApplyPlan::SetOpsThenApply);
    assert_eq!(apply_plan(false, false, true), ApplyPlan::SetDepths);
    assert_eq!(apply_plan(true, true, true), ApplyPlan::ClearOpsThenSetDepths, "direct edits bake the recipe away");
}

#[test]
fn colours_are_deterministic() {
    assert_eq!(depth_ramp_color(-1.0, -1.0, 1.0), [46, 102, 242, 255]);
    assert_eq!(depth_ramp_color(0.0, -1.0, 1.0), [217, 217, 217, 255]);
    assert_eq!(depth_ramp_color(1.0, -1.0, 1.0), [255, 122, 31, 255]);
    assert_eq!(depth_ramp_color(5.0, 3.0, 3.0), [217, 217, 217, 255], "a flat range is grey");
    assert_eq!(depth_ramp_color(f32::NAN, 0.0, 1.0), [217, 217, 217, 255]);
    assert_eq!(operation_color(0.2, false), [13, 158, 255, 235]);
    assert_eq!(operation_color(-0.2, true), [255, 122, 56, 255]);
    let flat = operation_color(0.0, false);
    assert_eq!((flat[0], flat[1]), (26, 102));
    assert!(flat[2] == 229 || flat[2] == 230);
}

#[test]
fn png_layers_load_as_one_layer_and_respect_the_memory_budget() {
    let mut s = Session::new();
    s.layers.push(Layer { id: "x".into(), width: 2, height: 1, depth_pixels: vec![255, 0, 0, 255, 0, 0, 0, 128], ..Layer::default() });
    let png = layer_png_bytes(&s.layers[0]).unwrap();
    let mut retained = 0;
    let layer = load_png_layer("/tmp/depth maps/head.png", "", &png, &mut retained).unwrap();
    assert_eq!((layer.id.as_str(), layer.layer_path.as_str(), layer.display_name.as_str()), ("head", "/head", "head"));
    assert_eq!((layer.width, layer.height), (2, 1));
    assert_eq!(layer.bounds, Rect { left: 0, top: 0, width: 2, height: 1 });
    assert_eq!(layer.depth_pixels, [85, 85, 85, 255, 0, 0, 0, 128], "average of (255, 0, 0)");
    assert_eq!(layer.rgba, layer.depth_pixels);
    assert_eq!(retained, 16);
    assert_eq!(load_png_layer("a.png", "named", &png, &mut retained).unwrap().id, "named");
    assert!(load_png_layer("a.png", "", b"not a png", &mut retained).is_err());
    let mut full = 256 * 1024 * 1024 - 8;
    assert!(load_png_layer("a.png", "", &png, &mut full).is_err(), "2 pixels need 16 bytes");
    assert!(!reserve_png_retained_layer(u64::MAX, 2, &mut 0));
    let mut ok = 0;
    assert!(reserve_png_retained_layer(4096, 4096, &mut ok) && ok == 4096 * 4096 * 8);
}

#[test]
fn the_persistent_manifest_survives_save_and_open_as_a_top_level_model_key() {
    // D stores the manifest string on the puppet as `depthDrawSessionManifest`; the model keeps unknown keys.
    let transform = json!({"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]});
    let manifest = session_to_persistent_json(&rich_session());
    let model = json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0, "transform": transform,
            "lockToRoot": false, "pinToMesh": false, "children": []},
        "param": [], "animations": {},
        "depthDrawSessionManifest": manifest,
    });
    let mut session = soul2d_core::Session::new();
    let reply = session.handle_request(&json!({"op": "new", "model": model}));
    assert_eq!(reply["succeeded"], true, "{reply}");
    let dir = std::env::temp_dir().join(format!("depth-draw-manifest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("manifest.inx");
    let saved = session.handle_request(&json!({"op": "save", "path": path}));
    assert_eq!(saved["succeeded"], true, "{saved}");
    let mut reopened = soul2d_core::Session::new();
    let opened = reopened.handle_request(&json!({"op": "open", "path": path}));
    assert_eq!(opened["succeeded"], true, "{opened}");
    let stored = reopened.snapshot()["model"]["depthDrawSessionManifest"].as_str().unwrap().to_owned();
    let back = session_from_persistent_json(&stored).unwrap().unwrap();
    assert_eq!(back.layers, rich_session().layers);
    std::fs::remove_dir_all(dir).unwrap();
}
