use serde_json::{json, Value};
use soul2d_core::cv::{
    ApproximationMethod, BinaryMask, DistanceTransform, Point, RetrievalMode, WorkBudget,
};
use soul2d_core::Session;

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn mask(width: u32, height: u32, pixels: &[u8]) -> BinaryMask {
    BinaryMask::new(width, height, pixels.to_vec()).unwrap()
}

fn points(pairs: &[[u32; 2]]) -> Vec<Point> {
    pairs.iter().map(|&[x, y]| Point { x, y }).collect()
}

#[test]
fn binary_mask_constructor_rejects_bad_inputs() {
    assert!(BinaryMask::new(0, 3, vec![]).is_err());
    assert!(BinaryMask::new(3, 0, vec![]).is_err());
    assert!(BinaryMask::new(u32::MAX, u32::MAX, vec![]).is_err());
    assert!(BinaryMask::new(2, 2, vec![0; 3]).is_err());
    let over_cap = vec![0u8; (4097 * 4097) as usize];
    assert!(BinaryMask::new(4097, 4097, over_cap).is_err());
    let ok = BinaryMask::new(2, 2, vec![0, 1, 2, 3]).unwrap();
    assert_eq!(ok.size(), [2, 2]);
    assert_eq!(ok.pixels(), &[0, 1, 2, 3]);
}

#[test]
fn from_rgba_thresholds_alpha_and_preserves_input() {
    let rgba = [1, 2, 3, 0, 4, 5, 6, 10, 7, 8, 9, 16, 10, 11, 12, 255];
    let copy = rgba;
    let at16 = BinaryMask::from_rgba(2, 2, &rgba, 16).unwrap();
    assert_eq!(at16.pixels(), &[0, 0, 255, 255]);
    let at0 = BinaryMask::from_rgba(2, 2, &rgba, 0).unwrap();
    assert_eq!(at0.pixels(), &[255, 255, 255, 255]);
    assert_eq!(rgba, copy);
    assert!(BinaryMask::from_rgba(2, 2, &rgba[..15], 16).is_err());
    assert!(BinaryMask::from_rgba(0, 2, &rgba[..4], 0).is_err());
    assert!(BinaryMask::from_rgba(4097, 4097, &[], 16).is_err());
    assert!(BinaryMask::from_rgba(u32::MAX, u32::MAX, &[], 16).is_err());
}

#[test]
fn distance_single_background_center() {
    let mask = mask(3, 3, &[1, 1, 1, 1, 0, 1, 1, 1, 1]);
    let mut budget = WorkBudget::new(1_000_000);
    let DistanceTransform::Field(field) = mask.distance_transform(&mut budget).unwrap() else {
        panic!("expected distance field");
    };
    assert_eq!(field.size(), [3, 3]);
    let samples = field.samples();
    let corner = f64::from(samples[0].distance);
    assert_eq!(corner, 1.4142135381698608);
    assert_eq!(samples[1].distance, 1.0);
    assert_eq!(samples[4].distance, 0.0);
    for sample in samples {
        assert_eq!(sample.nearest, Point { x: 1, y: 1 });
    }
}

#[test]
fn distance_all_background_self_nearest() {
    let mask = mask(3, 2, &[0; 6]);
    let mut budget = WorkBudget::new(1_000_000);
    let DistanceTransform::Field(field) = mask.distance_transform(&mut budget).unwrap() else {
        panic!("expected distance field");
    };
    for (index, sample) in field.samples().iter().enumerate() {
        assert_eq!(sample.distance, 0.0);
        assert_eq!(
            sample.nearest,
            Point {
                x: (index % 3) as u32,
                y: (index / 3) as u32,
            }
        );
    }
}

#[test]
fn distance_no_background() {
    let mask = mask(3, 2, &[1; 6]);
    let mut budget = WorkBudget::new(1_000_000);
    assert!(matches!(
        mask.distance_transform(&mut budget).unwrap(),
        DistanceTransform::NoBackground
    ));
}

#[test]
fn distance_horizontal_tie_chooses_left() {
    let mask = mask(3, 1, &[0, 1, 0]);
    let mut budget = WorkBudget::new(1_000_000);
    let DistanceTransform::Field(field) = mask.distance_transform(&mut budget).unwrap() else {
        panic!("expected distance field");
    };
    assert_eq!(field.samples()[1].distance, 1.0);
    assert_eq!(field.samples()[1].nearest, Point { x: 0, y: 0 });
}

#[test]
fn distance_vertical_tie_chooses_top() {
    let mask = mask(1, 3, &[0, 1, 0]);
    let mut budget = WorkBudget::new(1_000_000);
    let DistanceTransform::Field(field) = mask.distance_transform(&mut budget).unwrap() else {
        panic!("expected distance field");
    };
    assert_eq!(field.samples()[1].distance, 1.0);
    assert_eq!(field.samples()[1].nearest, Point { x: 0, y: 0 });
}

const BORDERED_SQUARE_7: [u8; 49] = [
    0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0,
    0, 0, 255, 255, 255, 0, 0,
    0, 0, 255, 255, 255, 0, 0,
    0, 0, 255, 255, 255, 0, 0,
    0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0,
];

#[test]
fn thin_bordered_square_leaves_center() {
    let mask = mask(7, 7, &BORDERED_SQUARE_7);
    let mut budget = WorkBudget::new(1_000_000);
    let thinned = mask.thin(&mut budget).unwrap();
    let mut expected = [0u8; 49];
    expected[24] = 255;
    assert_eq!(thinned.pixels(), &expected);
}

#[test]
fn thin_full_foreground_and_tiny_masks_are_noops() {
    let full = mask(3, 3, &[255; 9]);
    let mut budget = WorkBudget::new(1_000_000);
    assert_eq!(full.thin(&mut budget).unwrap().pixels(), &[255; 9]);
    let one = mask(1, 1, &[255]);
    let mut budget = WorkBudget::new(1_000_000);
    assert_eq!(one.thin(&mut budget).unwrap().pixels(), &[255]);
    let two = mask(2, 2, &[255; 4]);
    let mut budget = WorkBudget::new(1_000_000);
    assert_eq!(two.thin(&mut budget).unwrap().pixels(), &[255; 4]);
}

#[test]
fn thin_preserves_nonzero_bytes_and_budget_zero_errors() {
    let mut varied = BORDERED_SQUARE_7;
    varied[24] = 77;
    let mask = mask(7, 7, &varied);
    let source = mask.pixels().to_vec();
    let mut budget = WorkBudget::new(1_000_000);
    let thinned = mask.thin(&mut budget).unwrap();
    assert_eq!(thinned.pixels()[24], 77);
    assert_eq!(mask.pixels(), &source[..]);
    let mut none = WorkBudget::new(0);
    assert!(mask.thin(&mut none).is_err());
    assert_eq!(mask.pixels(), &source[..]);
}

#[test]
fn longest_path_literals() {
    let empty = mask(3, 3, &[0; 9]);
    let mut budget = WorkBudget::new(1_000_000);
    assert_eq!(empty.longest_path(&mut budget).unwrap(), Vec::<Point>::new());

    let single = mask(3, 3, &[0, 0, 0, 0, 255, 0, 0, 0, 0]);
    let mut budget = WorkBudget::new(1_000_000);
    assert_eq!(
        single.longest_path(&mut budget).unwrap(),
        points(&[[1, 1]])
    );

    let line = mask(5, 3, &[0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0]);
    let mut budget = WorkBudget::new(1_000_000);
    assert_eq!(
        line.longest_path(&mut budget).unwrap(),
        points(&[[4, 1], [3, 1], [2, 1], [1, 1], [0, 1]])
    );

    let grid = mask(3, 3, &[255; 9]);
    let mut budget = WorkBudget::new(1_000_000);
    assert_eq!(
        grid.longest_path(&mut budget).unwrap(),
        points(&[[2, 0], [1, 0], [0, 0]])
    );

    let disconnected = mask(
        6,
        3,
        &[
            0, 0, 0, 0, 0, 0,
            255, 255, 255, 0, 255, 255,
            0, 0, 0, 0, 0, 0,
        ],
    );
    let mut budget = WorkBudget::new(1_000_000);
    assert_eq!(
        disconnected.longest_path(&mut budget).unwrap(),
        points(&[[2, 1], [1, 1], [0, 1]])
    );
}

#[test]
fn longest_path_isolated_grid_points_within_small_budget() {
    let mut pixels = vec![0u8; 65 * 65];
    for y in (0..65).step_by(3) {
        for x in (0..65).step_by(3) {
            pixels[y * 65 + x] = 255;
        }
    }
    let mask = mask(65, 65, &pixels);
    let mut budget = WorkBudget::new(100_000);
    assert_eq!(
        mask.longest_path(&mut budget).unwrap(),
        points(&[[0, 0]])
    );
    assert_eq!(mask.pixels(), &pixels[..]);
}

const RECT_5: [u8; 25] = [
    0, 0, 0, 0, 0,
    0, 255, 255, 255, 0,
    0, 255, 255, 255, 0,
    0, 255, 255, 255, 0,
    0, 0, 0, 0, 0,
];

const RING_5: [u8; 25] = [
    0, 0, 0, 0, 0,
    0, 255, 255, 255, 0,
    0, 255, 0, 255, 0,
    0, 255, 255, 255, 0,
    0, 0, 0, 0, 0,
];

#[test]
fn contours_singleton_and_empty() {
    let single = mask(3, 3, &[0, 0, 0, 0, 255, 0, 0, 0, 0]);
    let mut budget = WorkBudget::new(1_000_000);
    let found = single
        .contours(RetrievalMode::External, ApproximationMethod::Simple, &mut budget)
        .unwrap();
    assert_eq!(found.points, vec![points(&[[1, 1]])]);
    assert_eq!(found.hierarchy.len(), 1);

    let empty = mask(4, 3, &[0; 12]);
    let mut budget = WorkBudget::new(1_000_000);
    let found = empty
        .contours(RetrievalMode::External, ApproximationMethod::Simple, &mut budget)
        .unwrap();
    assert!(found.points.is_empty());
    assert!(found.hierarchy.is_empty());
}

#[test]
fn contours_rectangle_simple_and_ring() {
    for pixels in [&RECT_5[..], &RING_5[..]] {
        let mask = mask(5, 5, pixels);
        let mut budget = WorkBudget::new(1_000_000);
        let found = mask
            .contours(RetrievalMode::External, ApproximationMethod::Simple, &mut budget)
            .unwrap();
        assert_eq!(
            found.points,
            vec![points(&[[1, 1], [3, 1], [3, 3], [1, 3], [1, 1]])]
        );
    }
}

#[test]
fn contours_approximation_variants() {
    let mask = mask(5, 5, &RECT_5);
    let mut budget = WorkBudget::new(1_000_000);
    let found = mask
        .contours(RetrievalMode::External, ApproximationMethod::None, &mut budget)
        .unwrap();
    assert_eq!(
        found.points,
        vec![points(&[
            [1, 1],
            [2, 1],
            [3, 1],
            [3, 2],
            [3, 3],
            [2, 3],
            [1, 3],
            [1, 2],
            [1, 1]
        ])]
    );
    for method in [ApproximationMethod::Tc89L1, ApproximationMethod::Tc89Kcos] {
        let mut budget = WorkBudget::new(1_000_000);
        let found = mask
            .contours(RetrievalMode::External, method, &mut budget)
            .unwrap();
        assert_eq!(found.points, vec![points(&[[1, 1], [3, 3], [1, 1]])]);
    }
}

#[test]
fn contours_all_modes_have_bounded_hierarchy() {
    let mask = mask(5, 5, &RECT_5);
    for mode in [
        RetrievalMode::External,
        RetrievalMode::List,
        RetrievalMode::Ccomp,
        RetrievalMode::Tree,
    ] {
        let mut budget = WorkBudget::new(1_000_000);
        let found = mask
            .contours(mode, ApproximationMethod::Simple, &mut budget)
            .unwrap();
        assert_eq!(found.hierarchy.len(), found.points.len());
        for entry in &found.hierarchy {
            for index in [entry.next, entry.prev, entry.child, entry.parent] {
                assert!(index.is_none_or(|index| index < found.points.len()));
            }
        }
    }
}

#[test]
fn contours_budget_exhaustion_returns_no_partial() {
    let mask = mask(5, 5, &RECT_5);
    let mut budget = WorkBudget::new(0);
    assert!(mask
        .contours(RetrievalMode::External, ApproximationMethod::Simple, &mut budget)
        .is_err());
}

const MODEL: &str = r#"{
    "meta": { "name": "fixture", "version": "1.0-alpha" },
    "nodes": {
        "uuid": 1, "name": "Root", "type": "Node", "enabled": true,
        "zsort": 0.0,
        "transform": { "trans": [0.0,0.0,0.0], "rot": [0.0,0.0,0.0], "scale": [1.0,1.0] },
        "lockToRoot": false, "pinToMesh": false
    },
    "param": [],
    "animations": {}
}"#;

fn session() -> Session {
    let mut session = Session::new();
    let model: Value = serde_json::from_str(MODEL).unwrap();
    let created = session.handle_request(&json!({"op": "new", "model": model}));
    assert_eq!(created["succeeded"], true, "{created}");
    session
}

fn cv_request(method: &str, size: [u32; 2], pixels: Vec<u8>) -> Value {
    json!({"op": "cv.analyze", "method": method, "size": size, "pixels": pixels})
}

#[test]
fn analyze_distance_literals() {
    let mut session = session();
    let response = request(
        &mut session,
        cv_request("distance", [3, 3], vec![1, 1, 1, 1, 0, 1, 1, 1, 1]),
    );
    let result = result(&response);
    assert_eq!(result["method"], "distance");
    assert_eq!(result["size"], json!([3, 3]));
    assert_eq!(result["hasBackground"], true);
    assert_eq!(result["distances"][0], 1.4142135381698608);
    assert_eq!(result["distances"][1], 1.0);
    assert_eq!(result["nearest"][1], json!([1, 1]));
    assert_eq!(result["nearest"][8], json!([1, 1]));
}

#[test]
fn analyze_distance_no_background() {
    let mut session = session();
    let response = request(&mut session, cv_request("distance", [3, 2], vec![1; 6]));
    let result = result(&response);
    assert_eq!(result["hasBackground"], false);
    assert_eq!(result["distances"], Value::Null);
    assert_eq!(result["nearest"], Value::Null);
}

#[test]
fn analyze_thin_and_path_results() {
    let mut session = session();
    let response = request(
        &mut session,
        cv_request("thin", [7, 7], BORDERED_SQUARE_7.to_vec()),
    );
    let body = result(&response);
    let mut expected = vec![0u8; 49];
    expected[24] = 255;
    assert_eq!(body["pixels"], json!(expected));

    let response = request(
        &mut session,
        cv_request("path", [5, 3], vec![
            0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0,
        ]),
    );
    assert_eq!(
        result(&response)["path"],
        json!([[4, 1], [3, 1], [2, 1], [1, 1], [0, 1]])
    );
}

#[test]
fn analyze_contours_result_and_hierarchy_encoding() {
    let mut session = session();
    let response = request(
        &mut session,
        cv_request("contours", [5, 5], RECT_5.to_vec()),
    );
    let result = result(&response);
    assert_eq!(
        result["contours"],
        json!([[[1, 1], [3, 1], [3, 3], [1, 3], [1, 1]]])
    );
    assert_eq!(result["hierarchy"], json!([[-1, -1, -1, -1]]));
}

#[test]
fn analyze_validation_rejects_and_preserves_snapshot() {
    let mut session = session();
    let before = session.snapshot();
    let bad_requests = vec![
        json!({"op": "cv.analyze"}),
        json!({"op": "cv.analyze", "method": "warp", "size": [1, 1], "pixels": [0]}),
        json!({"op": "cv.analyze", "method": "distance", "size": [1, 1], "pixels": [0],
            "mode": "external"}),
        json!({"op": "cv.analyze", "method": "distance", "size": [1, 1], "pixels": [0],
            "approximation": "fast"}),
        json!({"op": "cv.analyze", "method": "distance", "size": [0, 1], "pixels": [0]}),
        json!({"op": "cv.analyze", "method": "distance", "size": [2, 2], "pixels": [0, 0, 0]}),
        json!({"op": "cv.analyze", "method": "distance", "size": [2, 2],
            "pixels": [0, 0, 0, 256]}),
        json!({"op": "cv.analyze", "method": "distance", "size": [2, 2],
            "pixels": [0, 0, 0, -1]}),
        json!({"op": "cv.analyze", "method": "distance", "size": [257, 256],
            "pixels": vec![0; 65792]}),
        json!({"op": "cv.analyze", "method": "distance", "size": [2, 2],
            "pixels": [0, 0, 0, 0], "mode": 3}),
    ];
    for bad in bad_requests {
        let response = request(&mut session, bad);
        assert_eq!(response["succeeded"], false, "{response}");
        assert_eq!(session.snapshot(), before);
    }
}

#[test]
fn analyze_is_read_only_over_session_and_mcp() {
    let mut session = session();
    let before = session.snapshot();
    let body = cv_request("path", [1, 1], vec![255]);
    let response = request(&mut session, body);
    assert_eq!(result(&response)["path"], json!([[0, 0]]));
    assert_eq!(session.snapshot(), before);

    let listed = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .unwrap();
    let tools = listed["result"]["tools"].as_array().unwrap();
    let tool = tools
        .iter()
        .find(|tool| tool["name"] == "cv_analyze")
        .expect("cv_analyze tool");
    assert_eq!(tool["inputSchema"]["required"], json!(["method", "size", "pixels"]));
    assert_eq!(
        tool["inputSchema"]["properties"]["method"]["enum"],
        json!(["distance", "thin", "path", "contours"])
    );

    let called = session
        .handle_mcp(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "cv_analyze",
                "arguments": {
                    "method": "distance",
                    "size": [1, 3],
                    "pixels": [0, 1, 0],
                },
            },
        }))
        .unwrap();
    let structured = &called["result"]["structuredContent"];
    assert_eq!(structured["method"], "distance");
    assert_eq!(structured["nearest"][1], json!([0, 0]));
    assert_eq!(session.snapshot(), before);
}
