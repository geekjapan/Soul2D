use serde_json::{json, Value};
use soul2d_core::cv::WorkBudget;
use soul2d_core::pixels::{PixelFormat, PixelImage};
use soul2d_core::Session;

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

#[test]
fn storage_formats_shapes_and_mutation() {
    for (format, channels) in [
        (PixelFormat::Mono, 1usize),
        (PixelFormat::Rgb, 3),
        (PixelFormat::Rgba, 4),
    ] {
        let pixels = vec![9u8; 2 * channels];
        let mut image = PixelImage::from_pixels(2, 1, format, pixels.clone()).unwrap();
        assert_eq!(image.size(), [2, 1]);
        assert_eq!(image.format(), format);
        assert_eq!(image.bit_depth(), 8);
        assert_eq!(image.channels(), channels);
        assert_eq!(image.shape(), [1, 2, channels as u32]);
        assert!(!image.is_empty());
        assert_eq!(image.pixels(), &pixels[..]);
        image.pixels_mut()[0] = 7;
        assert_eq!(image.pixels()[0], 7);
        assert_eq!(image.size(), [2, 1]);
        assert_eq!(image.shape(), [1, 2, channels as u32]);
    }
}

#[test]
fn storage_zeroed_and_rejects() {
    for (format, channels) in [
        (PixelFormat::Mono, 1usize),
        (PixelFormat::Rgb, 3),
        (PixelFormat::Rgba, 4),
    ] {
        let image = PixelImage::zeroed(2, 2, format).unwrap();
        assert_eq!(image.size(), [2, 2]);
        assert_eq!(image.shape(), [2, 2, channels as u32]);
        assert_eq!(image.pixels(), &vec![0u8; 4 * channels][..]);
    }
    let empty = PixelImage::from_pixels(0, 3, PixelFormat::Rgb, vec![]).unwrap();
    assert!(empty.is_empty());
    assert_eq!(empty.shape(), [3, 0, 3]);
    let empty = PixelImage::zeroed(0, 0, PixelFormat::Mono).unwrap();
    assert!(empty.is_empty());
    assert_eq!(empty.shape(), [0, 0, 1]);

    assert!(PixelImage::from_pixels(2, 1, PixelFormat::Rgb, vec![0; 5]).is_err());
    assert!(PixelImage::from_pixels(4097, 4097, PixelFormat::Rgba, vec![]).is_err());
    assert!(PixelImage::zeroed(4097, 4097, PixelFormat::Rgba).is_err());
    assert!(PixelImage::from_pixels(u32::MAX, u32::MAX, PixelFormat::Mono, vec![]).is_err());
    assert!(PixelImage::zeroed(u32::MAX, 1, PixelFormat::Mono).is_err());
}

const CENTER_SEED_3X3: [u8; 36] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 200, 50, 25, 255, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

#[test]
fn bleed_center_seed_fills_in_one_pass() {
    let image =
        PixelImage::from_pixels(3, 3, PixelFormat::Rgba, CENTER_SEED_3X3.to_vec()).unwrap();
    let mut budget = WorkBudget::new(1_000_000);
    let out = image.bleed(1, &mut budget).unwrap();
    for index in 0..9 {
        assert_eq!(
            &out.pixels()[index * 4..index * 4 + 3],
            &[200, 50, 25],
            "pixel {index}"
        );
        assert_eq!(out.pixels()[index * 4 + 3], if index == 4 { 255 } else { 0 });
    }
    assert_eq!(image.pixels(), &CENTER_SEED_3X3[..]);
}

#[test]
fn bleed_propagates_one_ring_per_pass() {
    let mut pixels = vec![0u8; 5 * 4];
    pixels[..4].copy_from_slice(&[100, 20, 10, 128]);
    let image = PixelImage::from_pixels(5, 1, PixelFormat::Rgba, pixels.clone()).unwrap();

    let mut budget = WorkBudget::new(1_000_000);
    let one = image.bleed(1, &mut budget).unwrap();
    assert_eq!(&one.pixels()[4..8], &[100, 20, 10, 0]);
    assert_eq!(&one.pixels()[8..12], &[0, 0, 0, 0]);
    assert_eq!(&one.pixels()[16..20], &[0, 0, 0, 0]);
    assert_eq!(&one.pixels()[..4], &[100, 20, 10, 128]);

    let mut budget = WorkBudget::new(1_000_000);
    let four = image.bleed(4, &mut budget).unwrap();
    for x in 0..5 {
        assert_eq!(
            &four.pixels()[x * 4..x * 4 + 3],
            &[100, 20, 10],
            "pixel {x}"
        );
        assert_eq!(four.pixels()[x * 4 + 3], if x == 0 { 128 } else { 0 });
    }
    assert_eq!(image.pixels(), &pixels[..]);
}

#[test]
fn bleed_averages_multiple_known_neighbors() {
    let pixels = vec![200, 100, 50, 255, 0, 0, 0, 0, 100, 0, 0, 1];
    let image = PixelImage::from_pixels(3, 1, PixelFormat::Rgba, pixels.clone()).unwrap();
    let mut budget = WorkBudget::new(1_000_000);
    let out = image.bleed(1, &mut budget).unwrap();
    assert_eq!(&out.pixels()[..4], &[200, 100, 50, 255]);
    assert_eq!(&out.pixels()[4..8], &[150, 50, 25, 0]);
    assert_eq!(&out.pixels()[8..12], &[100, 0, 0, 1]);
    assert_eq!(image.pixels(), &pixels[..]);
}

#[test]
fn bleed_no_seed_and_no_change_cases() {
    let transparent = vec![9u8, 9, 9, 0, 7, 7, 7, 0];
    let image = PixelImage::from_pixels(2, 1, PixelFormat::Rgba, transparent.clone()).unwrap();
    let mut budget = WorkBudget::new(1_000_000);
    assert_eq!(image.bleed(32, &mut budget).unwrap().pixels(), &transparent[..]);

    let opaque = vec![1, 2, 3, 255, 4, 5, 6, 200];
    let image = PixelImage::from_pixels(2, 1, PixelFormat::Rgba, opaque.clone()).unwrap();
    let mut budget = WorkBudget::new(1_000_000);
    assert_eq!(image.bleed(32, &mut budget).unwrap().pixels(), &opaque[..]);

    let mut budget = WorkBudget::new(1_000_000);
    assert_eq!(image.bleed(0, &mut budget).unwrap().pixels(), &opaque[..]);
    let mut budget = WorkBudget::new(1_000_000);
    assert_eq!(image.bleed(-5, &mut budget).unwrap().pixels(), &opaque[..]);

    let empty = PixelImage::from_pixels(0, 2, PixelFormat::Rgba, vec![]).unwrap();
    let mut budget = WorkBudget::new(1_000_000);
    assert!(empty.bleed(32, &mut budget).unwrap().is_empty());
}

#[test]
fn bleed_rejects_non_rgba_and_honors_budget() {
    for (format, channels) in [(PixelFormat::Mono, 1usize), (PixelFormat::Rgb, 3)] {
        let image = PixelImage::zeroed(2, 2, format).unwrap();
        assert_eq!(image.pixels().len(), 4 * channels);
        let mut budget = WorkBudget::new(1_000_000);
        assert!(image.bleed(32, &mut budget).is_err());
        assert_eq!(image.pixels(), &vec![0u8; 4 * channels][..]);
    }
    let image =
        PixelImage::from_pixels(3, 3, PixelFormat::Rgba, CENTER_SEED_3X3.to_vec()).unwrap();
    let mut budget = WorkBudget::new(0);
    assert!(image.bleed(1, &mut budget).is_err());
    assert_eq!(image.pixels(), &CENTER_SEED_3X3[..]);
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

#[test]
fn inspect_returns_metadata_and_bytes() {
    let mut session = session();
    let before = session.snapshot();
    let response = request(
        &mut session,
        json!({"op": "image.process", "method": "inspect", "format": "Rgb",
            "size": [2, 1], "pixels": [1, 2, 3, 4, 5, 6]}),
    );
    let body = result(&response);
    assert_eq!(body["method"], "inspect");
    assert_eq!(body["size"], json!([2, 1]));
    assert_eq!(body["format"], "Rgb");
    assert_eq!(body["bitDepth"], 8);
    assert_eq!(body["channels"], 3);
    assert_eq!(body["shape"], json!([1, 2, 3]));
    assert_eq!(body["empty"], false);
    assert_eq!(body["pixels"], json!([1, 2, 3, 4, 5, 6]));
    assert_eq!(session.snapshot(), before);

    let response = request(
        &mut session,
        json!({"op": "image.process", "method": "inspect", "format": "Mono",
            "size": [0, 3], "pixels": []}),
    );
    let body = result(&response);
    assert_eq!(body["empty"], true);
    assert_eq!(body["shape"], json!([3, 0, 1]));
    assert_eq!(body["pixels"], json!([]));
    assert_eq!(session.snapshot(), before);
}

#[test]
fn process_bleed_literals_and_default_iterations() {
    let mut session = session();
    let response = request(
        &mut session,
        json!({"op": "image.process", "method": "bleed", "format": "Rgba",
            "size": [3, 3], "pixels": CENTER_SEED_3X3.to_vec(), "iterations": 1}),
    );
    let body = result(&response);
    let pixels = body["pixels"].as_array().unwrap();
    for index in 0..9 {
        assert_eq!(
            &pixels[index * 4..index * 4 + 3],
            &[json!(200), json!(50), json!(25)][..]
        );
        assert_eq!(pixels[index * 4 + 3], if index == 4 { 255 } else { 0 });
    }

    let mut seed = vec![0u8; 20];
    seed[..4].copy_from_slice(&[100, 20, 10, 128]);
    let response = request(
        &mut session,
        json!({"op": "image.process", "method": "bleed", "format": "Rgba",
            "size": [5, 1], "pixels": seed}),
    );
    let body = result(&response);
    assert_eq!(
        body["pixels"],
        json!([100, 20, 10, 128, 100, 20, 10, 0, 100, 20, 10, 0, 100, 20, 10, 0, 100, 20, 10, 0])
    );
}

#[test]
fn process_rejections_preserve_snapshot() {
    let mut session = session();
    let before = session.snapshot();
    let bad_requests = vec![
        json!({"op": "image.process"}),
        json!({"op": "image.process", "method": "warp", "format": "Rgba",
            "size": [1, 1], "pixels": [0, 0, 0, 0]}),
        json!({"op": "image.process", "method": "inspect", "format": "rgba",
            "size": [1, 1], "pixels": [0, 0, 0, 0]}),
        json!({"op": "image.process", "method": "inspect", "format": 4,
            "size": [1, 1], "pixels": [0, 0, 0, 0]}),
        json!({"op": "image.process", "method": "inspect", "format": "Rgba",
            "size": [1, 1], "pixels": [0, 0, 0]}),
        json!({"op": "image.process", "method": "inspect", "format": "Rgba",
            "size": [1, 1], "pixels": [0, 0, 0, 256]}),
        json!({"op": "image.process", "method": "inspect", "format": "Rgba",
            "size": [1, 1], "pixels": [0, 0, 0, -1]}),
        json!({"op": "image.process", "method": "inspect", "format": "Rgba",
            "size": [-1, 1], "pixels": [0, 0, 0, 0]}),
        json!({"op": "image.process", "method": "inspect", "format": "Rgba",
            "size": [65537, 1], "pixels": []}),
        json!({"op": "image.process", "method": "inspect", "format": "Mono",
            "size": [257, 256], "pixels": vec![0; 65792]}),
        json!({"op": "image.process", "method": "inspect", "format": "Rgba",
            "size": [1, 1], "pixels": [0, 0, 0, 0], "iterations": 257}),
        json!({"op": "image.process", "method": "inspect", "format": "Rgba",
            "size": [1, 1], "pixels": [0, 0, 0, 0], "iterations": 1.5}),
        json!({"op": "image.process", "method": "inspect", "format": "Rgba",
            "size": [1, 1], "pixels": [0, 0, 0, 0], "iterations": "many"}),
        json!({"op": "image.process", "method": "bleed", "format": "Rgb",
            "size": [1, 1], "pixels": [0, 0, 0]}),
    ];
    for bad in bad_requests {
        let response = request(&mut session, bad);
        assert_eq!(response["succeeded"], false, "{response}");
        assert_eq!(session.snapshot(), before);
    }
}

#[test]
fn process_iterations_bounds() {
    let mut session = session();
    let before = session.snapshot();
    let seed = vec![100, 20, 10, 128, 0, 0, 0, 0];
    for iterations in [-2147483649i64, -4294967295, i64::MIN] {
        for method in ["inspect", "bleed"] {
            let response = request(
                &mut session,
                json!({"op": "image.process", "method": method, "format": "Rgba",
                    "size": [2, 1], "pixels": seed, "iterations": iterations}),
            );
            assert_eq!(response["succeeded"], false, "{response}");
            assert_eq!(session.snapshot(), before);
        }
    }
    let response = request(
        &mut session,
        json!({"op": "image.process", "method": "bleed", "format": "Rgba",
            "size": [2, 1], "pixels": seed, "iterations": -2147483648i64}),
    );
    assert_eq!(
        result(&response)["pixels"],
        json!([100, 20, 10, 128, 0, 0, 0, 0])
    );
    assert_eq!(session.snapshot(), before);
}

#[test]
fn process_is_read_only_over_session_and_mcp() {
    let mut session = session();
    let before = session.snapshot();
    let response = request(
        &mut session,
        json!({"op": "image.process", "method": "bleed", "format": "Rgba",
            "size": [1, 1], "pixels": [10, 20, 30, 255]}),
    );
    assert_eq!(result(&response)["pixels"], json!([10, 20, 30, 255]));
    assert_eq!(session.snapshot(), before);

    let listed = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .unwrap();
    let tools = listed["result"]["tools"].as_array().unwrap();
    let tool = tools
        .iter()
        .find(|tool| tool["name"] == "image_process")
        .expect("image_process tool");
    assert_eq!(
        tool["inputSchema"]["required"],
        json!(["method", "format", "size", "pixels"])
    );
    assert_eq!(
        tool["inputSchema"]["properties"]["method"]["enum"],
        json!(["inspect", "bleed"])
    );
    assert_eq!(
        tool["inputSchema"]["properties"]["format"]["enum"],
        json!(["Mono", "Rgb", "Rgba"])
    );

    let called = session
        .handle_mcp(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "image_process",
                "arguments": {
                    "method": "inspect",
                    "format": "Mono",
                    "size": [1, 2],
                    "pixels": [7, 9],
                },
            },
        }))
        .unwrap();
    let structured = &called["result"]["structuredContent"];
    assert_eq!(structured["channels"], 1);
    assert_eq!(structured["shape"], json!([2, 1, 1]));
    assert_eq!(structured["pixels"], json!([7, 9]));
    assert_eq!(session.snapshot(), before);
}
