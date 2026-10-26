mod common;

use serde_json::{json, Value};
use soul2d_core::export::{
    camera_frame, export_frame_sequence, export_image, ffmpeg_codecs_command, ffmpeg_command, frame_path,
    parse_encoders, plan_frames, plan_gif, plan_video, png_dimensions, png_file_dimensions, render_video_frames,
    AnimationTiming, AudioInput, EncoderSettings, ImageExportOptions, StillFormat, VideoCodec, VideoExportOptions,
};
use soul2d_core::export::{
    encode_jpeg, export_video, probe_encoders, Encoder, VideoOutput, VideoPreset, EXPORT_CANCELLED,
};
use soul2d_core::Session;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "soul2d-export-test-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    // Exports report the resolved destination.
    common::scratch(path).canonicalize().unwrap()
}

#[test]
fn shared_video_command_preview_is_nonexecuting_and_cleared_by_new() {
    let dir = temp_dir();
    let mut session = open_fixture(true);
    let program = dir.join("uninstalled-encoder");
    session.set_video_encoder(program.to_str().unwrap());
    session.set_video_ffmpeg_options("-crf 19");
    let output = dir.join("existing.mp4");
    fs::write(&output, b"sentinel").unwrap();
    let before = session.snapshot();
    let history = session.handle_request(&json!({"op":"history.summary"}));
    for overwrite in [false, true] {
        let args = json!({"path":output,"animation":"Idle","overwrite":overwrite,"preset":"mp4"});
        let reply = session.handle_mcp(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"export_video_command","arguments":args}})).unwrap();
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        let preview = session.video_command_preview().clone();
        assert_eq!(preview["program"], program.to_str().unwrap());
        assert_eq!(preview["args"][0], if overwrite { "-y" } else { "-n" });
        assert!(preview["args"].as_array().unwrap().windows(2).any(|p| p == [json!("-crf"), json!("19")]));
        assert_eq!(preview["frames"], 4);
        let resource = session.handle_mcp(&json!({"jsonrpc":"2.0","id":2,"method":"resources/read",
            "params":{"uri":"resource://soul2d/export/video-command"}})).unwrap();
        assert_eq!(serde_json::from_str::<Value>(resource["result"]["contents"][0]["text"].as_str().unwrap()).unwrap(), preview);
        assert_eq!(session.snapshot(), before);
        assert_eq!(session.handle_request(&json!({"op":"history.summary"})), history);
        assert_eq!(fs::read(&output).unwrap(), b"sentinel");
        assert!(!program.exists());
    }
    let last = session.video_command_preview().clone();
    let invalid = session.handle_request(&json!({"op":"export.video.command","path":output,"codec":"invalid codec"}));
    assert_eq!(invalid["succeeded"], false);
    assert_eq!(*session.video_command_preview(), last);
    let tools = session.handle_mcp(&json!({"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}})).unwrap();
    for name in ["export_video_probe", "export_video_probe_status", "export_video_command"] {
        let schema = &tools["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == name).unwrap()["inputSchema"];
        assert_eq!(schema["additionalProperties"], false);
        for key in ["program", "ffmpeg_options"] {
            let arguments = if name == "export_video_command" { json!({"path":output,(key):"forbidden"}) } else { json!({(key):"forbidden"}) };
            let reply = session.handle_mcp(&json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
                "params":{"name":name,"arguments":arguments}})).unwrap();
            assert_eq!(reply["result"]["isError"], true, "{reply}");
        }
    }
    let project = dir.join("preview-source.inx");
    op_ok(&mut session, json!({"op":"save","path":project}));
    op_ok(&mut session, json!({"op":"open","path":project}));
    assert_eq!(*session.video_command_preview(), Value::Null);
    op_ok(&mut session, json!({"op":"export.video.command","path":output}));
    assert!(!session.video_command_preview().is_null());
    assert_eq!(session.handle_request(&json!({"op":"new"}))["succeeded"], true);
    assert_eq!(*session.video_command_preview(), Value::Null);
    fs::remove_dir_all(dir).unwrap();
}

fn transform(trans: [f64; 3]) -> Value {
    json!({"trans": trans, "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]})
}

fn camera(uuid: u32, name: &str, trans: [f64; 3], viewport: [f64; 2]) -> Value {
    json!({"uuid": uuid, "name": name, "type": "Camera", "enabled": true, "zsort": 0.0,
        "transform": transform(trans), "children": [], "lockToRoot": false, "pinToMesh": false,
        "viewport": viewport})
}

/// A 2x2 texel quad covering world (2,2)..(6,6) whose x translation follows parameter 17, three
/// cameras ("Main" 8x8 at (4,4), "Close" 4x4 at (3,3), "Huge" over the pixel cap) and animation
/// "Idle" (4 frames, 0.25 s, parameter 0..3 on frames 0..3).
fn open_fixture(with_animation: bool) -> Session {
    let texel = [200u8, 100, 80, 255];
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(2, 2, image::Rgba(texel)))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let texture = png.into_inner();
    let part = json!({
        "uuid": 5, "name": "Quad", "type": "Part", "enabled": true, "zsort": 0.0,
        "transform": transform([0.0, 0.0, 0.0]), "children": [], "lockToRoot": false, "pinToMesh": false,
        "mesh": {"verts": [2.0, 2.0, 6.0, 2.0, 6.0, 6.0, 2.0, 6.0],
                 "uvs": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
                 "indices": [0, 1, 2, 0, 2, 3], "origin": [0.0, 0.0]},
        "vertices": [[2.0, 2.0], [6.0, 2.0], [6.0, 6.0], [2.0, 6.0]],
        "uvs": [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        "indices": [0, 1, 2, 0, 2, 3],
        "textures": [0], "blend_mode": "Normal", "tint": [1, 1, 1], "screenTint": [0, 0, 0],
        "emissionStrength": 1, "opacity": 1,
    });
    let animations = if with_animation {
        json!({"Idle": {
            "length": 4, "timestep": 0.25, "additive": false, "animationWeight": 1.0,
            "leadIn": -1, "leadOut": -1,
            "lanes": [{"uuid": 17, "target": 0, "interpolation": "Linear", "merge_mode": "Forced",
                "keyframes": [{"frame": 0, "value": 0.0, "tension": 0.5},
                              {"frame": 3, "value": 3.0, "tension": 0.5}]}]
        }})
    } else {
        json!({})
    };
    let model = json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
            "transform": transform([0.0, 0.0, 0.0]), "lockToRoot": false, "pinToMesh": false,
            "children": [
                part,
                camera(6, "Main", [4.0, 4.0, 0.0], [8.0, 8.0]),
                camera(7, "Close", [3.0, 3.0, 0.0], [4.0, 4.0]),
                camera(8, "Huge", [0.0, 0.0, 0.0], [16384.0, 16384.0]),
                camera(9, "Big", [0.0, 0.0, 0.0], [8192.0, 8192.0]),
            ]},
        "param": [{
            "uuid": 17, "name": "Slide", "is_vec2": true,
            "min": [0.0, -1.0], "max": [6.0, 1.0], "defaults": [0.0, 0.0],
            "axis_points": [[0.0, 1.0], [0.0, 1.0]], "merge_mode": "Additive",
            "bindings": [{"node": 5, "param_name": "transform.t.x", "interpolate_mode": "Linear",
                          "values": [[0, 0], [6, 6]], "isSet": [[true, true], [true, true]]}],
        }],
        "animations": animations,
    });
    let raw = serde_json::to_vec(&model).unwrap();
    let mut file = b"TRNSRTS\0".to_vec();
    file.extend_from_slice(&(raw.len() as u32).to_be_bytes());
    file.extend_from_slice(&raw);
    file.extend_from_slice(b"TEX_SECT");
    file.extend_from_slice(&1u32.to_be_bytes());
    file.extend_from_slice(&(texture.len() as u32).to_be_bytes());
    file.push(0);
    file.extend_from_slice(&texture);
    let path = temp_dir().join("fixture.inx");
    fs::write(&path, file).unwrap();
    let mut session = Session::new();
    let opened = session.handle_request(&json!({"op": "open", "path": path.to_str().unwrap()}));
    assert_eq!(opened["succeeded"], true, "{opened}");
    session
}

fn pixel(path: &Path, x: u32, y: u32) -> [u8; 4] {
    image::open(path).unwrap().into_rgba8().get_pixel(x, y).0
}

/// First column of row 4 whose alpha is nonzero: tells where the quad is, one pixel per world unit.
fn quad_left(path: &Path) -> u32 {
    let image = image::open(path).unwrap().into_rgba8();
    (0..image.width()).find(|x| image.get_pixel(*x, 4)[3] != 0).expect("quad visible in row 4")
}

fn assert_session_untouched(before: &Value, after: &Value) {
    for key in ["history", "canUndo", "canRedo", "modified", "revision", "model", "values"] {
        assert_eq!(after[key], before[key], "{key} must not change");
    }
}

fn real_png(width: u32, height: u32) -> Vec<u8> {
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::new(width, height))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    png.into_inner()
}

// ------------------------------------------------------------------ PNG header

#[test]
fn png_header_reports_dimensions_without_decoding() {
    let bytes = real_png(3, 5);
    assert_eq!(png_dimensions(&bytes), Ok((3, 5)));
    // Only the first 24 bytes are inspected; the rest may be garbage.
    assert_eq!(png_dimensions(&bytes[..24]), Ok((3, 5)));
    let path = temp_dir().join("header.png");
    fs::write(&path, &bytes).unwrap();
    assert_eq!(png_file_dimensions(&path), Ok((3, 5)));
    assert!(png_file_dimensions(&path.with_file_name("missing.png")).unwrap_err().starts_with("cannot open"));
}

#[test]
fn png_header_rejects_truncated_and_malformed_input() {
    let bytes = real_png(3, 5);
    let too_short = "PNG source is too short to contain an IHDR header";
    assert_eq!(png_dimensions(&[]), Err(too_short.into()));
    assert_eq!(png_dimensions(&bytes[..23]), Err(too_short.into()));

    let mut bad_signature = bytes.clone();
    bad_signature[1] = b'X';
    assert_eq!(png_dimensions(&bad_signature), Err("PNG source has an invalid signature".into()));

    let no_ihdr = "PNG source does not begin with an IHDR chunk";
    let mut bad_length = bytes.clone();
    bad_length[11] = 12;
    assert_eq!(png_dimensions(&bad_length), Err(no_ihdr.into()));
    let mut bad_tag = bytes.clone();
    bad_tag[12..16].copy_from_slice(b"IDAT");
    assert_eq!(png_dimensions(&bad_tag), Err(no_ihdr.into()));

    let invalid = "PNG source dimensions are invalid";
    let mut zero_width = bytes.clone();
    zero_width[16..20].copy_from_slice(&0u32.to_be_bytes());
    assert_eq!(png_dimensions(&zero_width), Err(invalid.into()));
    let mut zero_height = bytes.clone();
    zero_height[20..24].copy_from_slice(&0u32.to_be_bytes());
    assert_eq!(png_dimensions(&zero_height), Err(invalid.into()));
    let mut huge = bytes.clone();
    huge[16..20].copy_from_slice(&0x8000_0000u32.to_be_bytes());
    assert_eq!(png_dimensions(&huge), Err(invalid.into()));
    let mut largest = bytes;
    largest[16..20].copy_from_slice(&0x7FFF_FFFFu32.to_be_bytes());
    assert_eq!(png_dimensions(&largest), Ok((0x7FFF_FFFF, 5)));
}

// ------------------------------------------------------------------ still export

#[test]
fn png_export_uses_the_first_camera_size_framing_and_transparency() {
    let mut session = open_fixture(false);
    let before = session.snapshot();
    let directory = temp_dir();
    let options = ImageExportOptions { transparency: true, ..Default::default() };
    let result = export_image(&mut session, &options, &directory.join("shot.png")).unwrap();
    assert_eq!((result.path.as_path(), result.width, result.height), (directory.join("shot.png").as_path(), 8, 8));
    assert_eq!(png_file_dimensions(&result.path), Ok((8, 8)));
    assert_eq!(pixel(&result.path, 0, 0), [0, 0, 0, 0]);
    assert_eq!(pixel(&result.path, 1, 2), [0, 0, 0, 0]);
    // The renderer leaves the outermost covered pixel row/column partially transparent, so probe the interior.
    assert_eq!(pixel(&result.path, 3, 4), [200, 100, 80, 255]);
    assert_eq!(pixel(&result.path, 4, 4), [200, 100, 80, 255]);
    assert_eq!(pixel(&result.path, 6, 2), [0, 0, 0, 0]);
    assert_eq!(pixel(&result.path, 2, 6), [0, 0, 0, 0]);
    assert_session_untouched(&before, &session.snapshot());
}

#[test]
fn named_camera_frames_the_world_without_flipping() {
    let mut session = open_fixture(false);
    let frame = camera_frame(&mut session, Some("Close")).unwrap();
    assert_eq!((frame.name.as_str(), frame.width, frame.height, frame.bounds), ("Close", 4, 4, [1.0, 1.0, 5.0, 5.0]));
    let directory = temp_dir();
    let options = ImageExportOptions { camera: Some("Close".into()), transparency: true, ..Default::default() };
    let result = export_image(&mut session, &options, &directory.join("close")).unwrap();
    assert_eq!(result.path, directory.join("close.png"), "the extension follows the format");
    assert_eq!((result.width, result.height), (4, 4));
    assert_eq!(pixel(&result.path, 0, 0), [0, 0, 0, 0]);
    assert_eq!(pixel(&result.path, 0, 3), [0, 0, 0, 0]);
    assert_eq!(pixel(&result.path, 3, 0), [0, 0, 0, 0]);
    assert_eq!(pixel(&result.path, 2, 2), [200, 100, 80, 255]);
    assert_eq!(pixel(&result.path, 3, 3), [200, 100, 80, 255]);
}

#[test]
fn a_typed_jpg_name_is_the_file_written_for_jpeg() {
    let mut session = open_fixture(false);
    let directory = temp_dir();
    let jpeg = ImageExportOptions { format: StillFormat::Jpeg, ..Default::default() };
    for (typed, written) in [("a.jpg", "a.jpg"), ("b.JPG", "b.JPG"), ("c.png", "c.jpeg"), ("d", "d.jpeg")] {
        let result = export_image(&mut session, &jpeg, &directory.join(typed)).unwrap();
        assert_eq!(result.path, directory.join(written), "{typed}");
        assert_eq!(&std::fs::read(&result.path).unwrap()[..2], &[0xff, 0xd8], "{typed}");
    }
}

#[test]
fn opaque_export_composites_over_the_background_and_tga_is_supported() {
    let mut session = open_fixture(false);
    let directory = temp_dir();
    let options = ImageExportOptions { background: [10, 20, 30], ..Default::default() };
    let result = export_image(&mut session, &options, &directory.join("opaque.png")).unwrap();
    assert_eq!(pixel(&result.path, 0, 0), [10, 20, 30, 255]);
    assert_eq!(pixel(&result.path, 3, 3), [200, 100, 80, 255]);

    let tga = ImageExportOptions { format: StillFormat::Tga, transparency: true, ..Default::default() };
    let result = export_image(&mut session, &tga, &directory.join("shot.png")).unwrap();
    assert_eq!(result.path, directory.join("shot.tga"));
    let bytes = fs::read(&result.path).unwrap();
    assert_eq!(bytes[1], 0);
    assert!(matches!(bytes[2], 2 | 10));
    assert_eq!(bytes[16], 32);
    if bytes[2] == 10 {
        let mut offset = 18 + bytes[0] as usize;
        let mut pixels = 0;
        while pixels < 64 {
            let packet = bytes[offset];
            offset += 1;
            let count = (packet & 0x7f) as usize + 1;
            if packet & 0x80 != 0 {
                assert!(count <= 8 - pixels % 8, "TGA readers must be able to decode one scanline at a time");
                offset += 4;
            } else {
                offset += count * 4;
            }
            pixels += count;
            assert!(offset <= bytes.len() && pixels <= 64);
        }
    }
    let decoded = image::open(&result.path).unwrap().into_rgba8();
    assert_eq!(decoded.dimensions(), (8, 8));
    assert_eq!(decoded.get_pixel(0, 0).0, [0, 0, 0, 0]);
    assert_eq!(decoded.get_pixel(3, 3).0, [200, 100, 80, 255]);
}

#[test]
fn image_export_validates_options_and_never_overwrites_by_default() {
    let mut session = open_fixture(false);
    let directory = temp_dir();
    let post = ImageExportOptions { postprocessing: true, ..Default::default() };
    assert_eq!(
        export_image(&mut session, &post, &directory.join("a.png")),
        Err("post processing is not available in this build".into())
    );
    let missing = ImageExportOptions { camera: Some("Nope".into()), ..Default::default() };
    assert_eq!(export_image(&mut session, &missing, &directory.join("a.png")), Err("Camera not found: Nope".into()));
    let huge = ImageExportOptions { camera: Some("Huge".into()), ..Default::default() };
    assert_eq!(
        export_image(&mut session, &huge, &directory.join("a.png")),
        Err("export dimensions exceed the render pixel limit".into())
    );
    assert_eq!(export_image(&mut session, &Default::default(), Path::new("")), Err("File path is required".into()));
    assert_eq!(
        export_image(&mut Session::new(), &Default::default(), &directory.join("a.png")),
        Err("No cameras available in the active puppet".into())
    );
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0, "failed exports write nothing");

    let target = directory.join("keep.png");
    fs::write(&target, b"original").unwrap();
    let error = export_image(&mut session, &Default::default(), &target).unwrap_err();
    assert_eq!(error, format!("{} already exists", target.display()));
    assert_eq!(fs::read(&target).unwrap(), b"original");
    let replace = ImageExportOptions { overwrite: true, ..Default::default() };
    export_image(&mut session, &replace, &target).unwrap();
    assert_eq!(png_file_dimensions(&target), Ok((8, 8)));
}

// ------------------------------------------------------------------ frame plan

fn timing(length: i32, timestep: f32, lead_in: i32, lead_out: i32) -> AnimationTiming {
    AnimationTiming { length, timestep, lead_in, lead_out }
}

fn some(values: &[f32]) -> Vec<Option<f32>> {
    values.iter().map(|v| Some(*v)).collect()
}

#[test]
fn nominal_frame_rates_survive_the_f32_timestep() {
    for rate in [24, 30, 60] {
        assert_eq!(plan_frames(timing(4, 1.0 / rate as f32, -1, -1), 1, None).unwrap().fps, rate);
    }
    assert!(plan_frames(timing(4, 3.0, -1, -1), 1, None).is_err());
}

#[test]
fn plan_plays_the_animation_once_per_loop() {
    let once = plan_frames(timing(4, 0.25, -1, -1), 1, None).unwrap();
    assert_eq!((once.fps, once.poses), (4, some(&[0.0, 1.0, 2.0, 3.0])));
    // loops == 0 is clamped to one loop, as in D.
    assert_eq!(plan_frames(timing(4, 0.25, -1, -1), 0, None).unwrap().poses, some(&[0.0, 1.0, 2.0, 3.0]));
    let twice = plan_frames(timing(4, 0.25, -1, -1), 2, None).unwrap();
    assert_eq!(twice.poses, some(&[0.0, 1.0, 2.0, 3.0, 0.0, 1.0, 2.0, 3.0]));
}

#[test]
fn plan_honours_lead_in_and_lead_out_markers() {
    // 2 lead-in + 2 * 4 loop + 4 lead-out frames; the player stops at the second wrap, so the
    // lead-out is cut short exactly as D's frame budget cuts it.
    let plan = plan_frames(timing(10, 0.125, 2, 6), 2, None).unwrap();
    assert_eq!(plan.fps, 8);
    assert_eq!(
        plan.poses,
        some(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 2.0, 3.0, 4.0, 5.0, 2.0, 3.0, 4.0, 5.0])
    );
}

#[test]
fn plan_rescales_to_a_custom_framerate() {
    // 4 fps animation exported at 8 fps: 8 frames, half-frame steps, clamped at the last frame. The
    // first wrap reaches the single loop and stops the player, so the final image is the rest pose.
    let plan = plan_frames(timing(4, 0.25, -1, -1), 1, Some(8)).unwrap();
    assert_eq!(plan.fps, 8);
    assert_eq!(plan.poses, vec![Some(0.0), Some(0.5), Some(1.0), Some(1.5), Some(2.0), Some(3.0), Some(0.0), None]);
    // The same rate as the animation changes nothing.
    assert_eq!(plan_frames(timing(4, 0.25, -1, -1), 1, Some(4)).unwrap().poses, some(&[0.0, 1.0, 2.0, 3.0]));
}

#[test]
fn plan_rejects_degenerate_animations_and_runaway_frame_counts() {
    assert_eq!(
        plan_frames(timing(0, 0.25, -1, -1), 1, None),
        Err("animation length must be at least 1 frame".into())
    );
    assert_eq!(
        plan_frames(timing(4, 0.0, -1, -1), 1, None),
        Err("animation timestep must be positive".into())
    );
    assert_eq!(
        plan_frames(timing(4, f32::NAN, -1, -1), 1, None),
        Err("animation timestep must be positive".into())
    );
    assert_eq!(
        plan_frames(timing(1000, 0.25, -1, -1), u32::MAX, None),
        Err("video would need 2147483647000 frames; the limit is 100000".into())
    );
}

// ------------------------------------------------------------------ frame sequence

fn file_names(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn frame_sequence_writes_numbered_pngs_following_the_animation() {
    let mut session = open_fixture(true);
    let before = session.snapshot();
    assert_eq!(before["animation"], Value::Null);
    let directory = temp_dir();
    let options = VideoExportOptions { transparency: true, ..Default::default() };
    let plan = plan_video(&mut session, &options).unwrap();
    assert_eq!((plan.animation.as_str(), plan.frames(), plan.plan.fps), ("Idle", 4, 4));
    let paths = export_frame_sequence(&mut session, &options, &directory.join("clip.png")).unwrap();
    let expected: Vec<PathBuf> = (1..=4).map(|n| directory.join(format!("clip-{n}.png"))).collect();
    assert_eq!(paths, expected);
    assert_eq!(file_names(&directory), ["clip-1.png", "clip-2.png", "clip-3.png", "clip-4.png"]);
    for path in &paths {
        assert_eq!(png_file_dimensions(path), Ok((8, 8)));
    }
    // The quad slides one world unit (one pixel) per frame.
    let lefts: Vec<u32> = paths.iter().map(|path| quad_left(path)).collect();
    assert_eq!(lefts, [2, 3, 4, 5]);
    assert_eq!(pixel(&paths[3], 6, 4), [200, 100, 80, 255]);
    assert_eq!(pixel(&paths[3], 4, 4), [0, 0, 0, 0]);

    let after = session.snapshot();
    assert_session_untouched(&before, &after);
    assert_eq!(after["animation"], Value::Null, "no preview before means no preview after");
}

/// Image tracks (ANI04) behind and in front of the model, and the model placed by its group, in exported frames.
#[test]
fn image_tracks_and_groups_compose_with_the_model_in_exported_frames() {
    let mut session = open_fixture(true);
    let dir = temp_dir();
    let png = |name: &str, rgba: [u8; 4], size: u32| {
        let path = dir.join(name);
        image::RgbaImage::from_pixel(size, size, image::Rgba(rgba)).save(&path).unwrap();
        path.to_string_lossy().into_owned()
    };
    let frame = |session: &mut Session, transparency: bool| {
        let path = dir.join("f.png");
        let export = json!({"op": "export.frames", "path": path, "transparency": transparency, "overwrite": true});
        (op_ok(session, export), dir.join("f-1.png"))
    };
    let near = |actual: [u8; 4], expected: [f64; 4]| {
        let close = actual.iter().zip(expected).all(|(a, e)| (f64::from(*a) - e).abs() <= 1.0);
        assert!(close, "{actual:?} is not {expected:?}");
    };
    let (_, plain) = frame(&mut session, true);
    let edge = pixel(&plain, 2, 4);
    assert!(edge[3] > 0 && edge[3] < 255, "the quad's left column is partly covered: {edge:?}");

    // The camera shows world 0..8 at one pixel a unit. A green 8x8 track fills it, the red 2x2 track above it covers
    // pixels 0 and 1, and a half-transparent blue 2x2 track in front of the model covers pixels 3 and 4.
    let green = png("green.png", [0, 255, 0, 255], 8);
    let red = png("red.png", [255, 0, 0, 255], 2);
    let blue = png("blue.png", [0, 0, 255, 255], 2);
    let tracks = [(&red, 1, json!({})), (&green, 4, json!({})), (&blue, 4, json!({"front": true, "opacity": 0.5}))];
    for (path, at, extra) in tracks {
        let mut add = json!({"op": "animation.image.add", "name": "Idle", "path": path, "x": at, "y": at});
        add.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        op_ok(&mut session, add);
    }
    let (_, composed) = frame(&mut session, true);
    assert_eq!(pixel(&composed, 0, 0), [255, 0, 0, 255], "the top track is drawn over the lower one");
    assert_eq!(pixel(&composed, 7, 7), [0, 255, 0, 255]);
    near(pixel(&composed, 4, 4), [100.0, 50.0, 167.5, 255.0]);
    let alpha = f64::from(edge[3]) / 255.0;
    let over_green = |channel: usize, green: f64| f64::from(edge[channel]) * alpha + green * (1.0 - alpha);
    near(pixel(&composed, 2, 4), [over_green(0, 0.0), over_green(1, 255.0), over_green(2, 0.0), 255.0]);
    let set = |index: u64, fields: Value| {
        let mut set = json!({"op": "animation.image.set", "name": "Idle", "index": index});
        set.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        set
    };
    op_ok(&mut session, set(1, json!({"to": 0})));
    assert_eq!(pixel(&frame(&mut session, true).1, 0, 0), [0, 255, 0, 255], "moved up, green covers red");
    // Without transparency the tracks lie over the background colour; a hidden track is not drawn.
    op_ok(&mut session, set(0, json!({"hidden": true})));
    let (_, opaque) = frame(&mut session, false);
    assert_eq!((pixel(&opaque, 0, 0), pixel(&opaque, 7, 7)), ([255, 0, 0, 255], [0, 0, 0, 255]));

    // The model in a group one unit right at half opacity.
    for _ in 0..3 {
        op_ok(&mut session, json!({"op": "animation.image.delete", "name": "Idle", "index": 0}));
    }
    op_ok(&mut session, json!({"op": "animation.group.add", "name": "Idle", "group": "Set", "x": 1, "opacity": 0.5}));
    op_ok(&mut session, json!({"op": "animation.group.set", "name": "Idle", "group": "Set", "model": true}));
    let (_, moved) = frame(&mut session, true);
    assert_eq!(quad_left(&moved), 3);
    near(pixel(&moved, 5, 4), [200.0, 100.0, 80.0, 127.5]);
    // At scale 2 from -2, -2 the quad's world 2..6 lies at 2..10, so pixel 6, 6 (world 4 in the model) shows it and
    // pixel 1, 1 (world 1.5) does not. A 2x2 track at 6, 2 with half a unit a pixel covers world 5.5..6.5 by
    // 1.5..2.5: pixel 5, 1 alone.
    let scaled = json!({"op": "animation.group.set", "name": "Idle", "group": "Set", "x": -2, "y": -2, "scale": 2});
    op_ok(&mut session, scaled);
    let small = json!({"op": "animation.image.add", "name": "Idle", "path": red, "x": 6, "y": 2, "scale": 0.5});
    op_ok(&mut session, small);
    let (_, scaled) = frame(&mut session, true);
    near(pixel(&scaled, 6, 6), [200.0, 100.0, 80.0, 127.5]);
    assert_eq!(pixel(&scaled, 1, 1), [0, 0, 0, 0]);
    assert_eq!(pixel(&scaled, 5, 1), [255, 0, 0, 255]);
    assert_eq!((pixel(&scaled, 6, 1), pixel(&scaled, 5, 0), pixel(&scaled, 4, 1)), ([0; 4], [0; 4], [0; 4]));
    op_ok(&mut session, json!({"op": "animation.image.delete", "name": "Idle", "index": 0}));
    // A hidden group's members are not exported, the model included.
    let red_in_group = json!({"op": "animation.image.add", "name": "Idle", "path": red, "x": 1, "y": 1, "group": "Set"});
    op_ok(&mut session, red_in_group);
    op_ok(&mut session, json!({"op": "animation.group.set", "name": "Idle", "group": "Set", "hidden": true}));
    let (_, hidden) = frame(&mut session, true);
    assert!(image::open(&hidden).unwrap().into_rgba8().pixels().all(|pixel| pixel.0 == [0, 0, 0, 0]));
    // A track whose file is gone is left out and named in the reply.
    op_ok(&mut session, json!({"op": "animation.group.set", "name": "Idle", "group": "Set", "hidden": false}));
    fs::remove_file(&red).unwrap();
    let (reply, _) = frame(&mut session, true);
    assert_eq!(reply["imagesLeftOut"], json!([format!("image file {red} was not found")]));
    // A file that is there but is no picture is left out too, and a GIF names both.
    fs::write(&red, b"not a png").unwrap();
    let broken = json!({"op": "animation.image.add", "name": "Idle", "path": blue});
    op_ok(&mut session, broken);
    fs::write(&blue, b"not a png").unwrap();
    let gif = op_ok(&mut session, json!({"op": "export.gif", "path": dir.join("a.gif"), "overwrite": true}));
    let left_out: Vec<&str> = gif["imagesLeftOut"].as_array().unwrap().iter().map(|e| e.as_str().unwrap()).collect();
    let unreadable = |name: &str| format!("Could not load {name}: Format error decoding Png: Invalid PNG signature.");
    assert_eq!(left_out, [unreadable("red.png"), unreadable("blue.png")]);
}

#[test]
fn frame_sequence_restores_an_existing_preview_and_supports_loops_framerate_and_snapping() {
    let mut session = open_fixture(true);
    let seek = session.handle_request(&json!({"op": "animation.seek", "name": "Idle", "frame": 2}));
    assert_eq!(seek["succeeded"], true, "{seek}");
    let before = session.snapshot();
    assert_eq!(before["animation"], json!({"name": "Idle", "frame": 2.0}));

    let directory = temp_dir();
    let options = VideoExportOptions {
        transparency: true,
        framerate: Some(8),
        snap_to_framerate: true,
        ..Default::default()
    };
    let paths = export_frame_sequence(&mut session, &options, &directory.join("slow.tga")).unwrap();
    assert_eq!(paths.len(), 8);
    assert_eq!(paths[7], directory.join("slow-8.tga"));
    // Poses 0, .5, 1, 1.5, 2, 3, 0, idle (rest pose) with the fractional ones floored.
    let lefts: Vec<u32> = paths
        .iter()
        .map(|path| {
            let image = image::open(path).unwrap().into_rgba8();
            (0..8).find(|x| image.get_pixel(*x, 4)[3] != 0).unwrap()
        })
        .collect();
    assert_eq!(lefts, [2, 2, 3, 3, 4, 5, 2, 2]);

    let after = session.snapshot();
    assert_session_untouched(&before, &after);
    assert_eq!(after["animation"], before["animation"]);
    assert_eq!(after["values"], before["values"]);

    let looped = VideoExportOptions { loops: 2, ..Default::default() };
    let mut seen = Vec::new();
    let plan = render_video_frames(&mut session, &looped, &mut |index, image| {
        assert_eq!(image.dimensions(), (8, 8));
        seen.push(index);
        Ok(())
    })
    .unwrap();
    assert_eq!((plan.frames(), seen), (8, (0..8).collect::<Vec<_>>()));
}

#[test]
fn frame_sequence_refuses_to_overwrite_and_validates_before_writing() {
    let mut session = open_fixture(true);
    let directory = temp_dir();
    let existing = directory.join("clip-3.png");
    fs::write(&existing, b"original").unwrap();
    let options = VideoExportOptions::default();
    let error = export_frame_sequence(&mut session, &options, &directory.join("clip.png")).unwrap_err();
    assert_eq!(error, format!("{} already exists", existing.display()));
    assert_eq!(file_names(&directory), ["clip-3.png"], "nothing else was written");
    assert_eq!(fs::read(&existing).unwrap(), b"original");

    let replace = VideoExportOptions { overwrite: true, ..Default::default() };
    export_frame_sequence(&mut session, &replace, &directory.join("clip.png")).unwrap();
    assert_eq!(png_file_dimensions(&existing), Ok((8, 8)));

    let clean = temp_dir();
    let cases: [(VideoExportOptions, &str, &str); 6] = [
        (VideoExportOptions { framerate: Some(0), ..Default::default() }, "a.png",
            "framerate must be between 1 and 240"),
        (VideoExportOptions { framerate: Some(241), ..Default::default() }, "a.png",
            "framerate must be between 1 and 240"),
        (VideoExportOptions { postprocessing: true, ..Default::default() }, "a.png",
            "post processing is not available in this build"),
        (VideoExportOptions { animation: Some("Nope".into()), ..Default::default() }, "a.png",
            "Animation not found: Nope"),
        (VideoExportOptions { camera: Some("Huge".into()), ..Default::default() }, "a.png",
            "export dimensions exceed the render pixel limit"),
        (VideoExportOptions::default(), "a.mp4", "frame sequence destination must end in .png or .tga"),
    ];
    for (options, name, message) in cases {
        assert_eq!(export_frame_sequence(&mut session, &options, &clean.join(name)), Err(message.into()));
    }
    assert!(file_names(&clean).is_empty(), "rejected exports write nothing");
    assert_eq!(
        export_frame_sequence(&mut open_fixture(false), &VideoExportOptions::default(), &clean.join("a.png")),
        Err("No animations are defined for this model".into())
    );
    assert_eq!(frame_path(Path::new("/x/out.png"), 12), PathBuf::from("/x/out-12.png"));
}

#[test]
fn a_failing_sink_stops_the_export_and_leaves_the_preview_alone() {
    let mut session = open_fixture(true);
    let before = session.snapshot();
    let mut calls = 0;
    let result = render_video_frames(&mut session, &VideoExportOptions::default(), &mut |_, _| {
        calls += 1;
        if calls == 2 { Err("disk full".into()) } else { Ok(()) }
    });
    assert_eq!(result, Err("disk full".into()));
    assert_eq!(calls, 2);
    assert_session_untouched(&before, &session.snapshot());
    assert_eq!(session.snapshot()["animation"], Value::Null);
}

// ------------------------------------------------------------------ ffmpeg command

fn settings(codec: &str, file: &str) -> EncoderSettings {
    EncoderSettings {
        codec: codec.into(),
        framerate: 30,
        file: file.into(),
        frames: 90,
        width: 1920,
        height: 1080,
        transparency: false,
        ffmpeg_options: String::new(),
        audio: Vec::new(),
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

#[test]
fn ffmpeg_command_matches_the_d_argument_vector() {
    let auto = ffmpeg_command(&settings("auto", "out.mp4")).unwrap();
    assert_eq!(auto.program, "ffmpeg");
    assert_eq!(
        auto.args,
        strings(&[
            "-y", "-hide_banner", "-loglevel", "error", "-f", "rawvideo", "-vcodec", "rawvideo", "-pix_fmt", "rgba", "-s", "1920x1080", "-r", "30",
            "-i", "-", "-vframes", "90", "-an", "-pix_fmt", "yuv420p", "out.mp4",
        ])
    );

    let mut h264 = settings("libx264", "/tmp/my clip.mp4");
    h264.transparency = true;
    h264.ffmpeg_options = "  -crf 18   -preset slow ".into();
    assert_eq!(
        ffmpeg_command(&h264).unwrap().args,
        strings(&[
            "-y", "-hide_banner", "-loglevel", "error", "-f", "rawvideo", "-vcodec", "rawvideo", "-pix_fmt", "rgba", "-s", "1920x1080", "-r", "30",
            "-i", "-", "-vframes", "90", "-an", "-pix_fmt", "yuva420p", "-vcodec", "libx264",
            "-crf", "18", "-preset", "slow", "/tmp/my clip.mp4",
        ])
    );
}

#[test]
fn ffmpeg_command_numbers_image_sequences_and_rejects_unsafe_input() {
    for (codec, encoder, file, expected) in [
        ("png", "png", "out/clip.png", "out/clip-%d.png"),
        ("jpeg", "mjpeg", "clip.v2.jpg", "clip.v2-%d.jpg"),
        ("jpg", "mjpeg", "clip.jpg", "clip-%d.jpg"),
        ("tga", "targa", "dir.d/clip", "dir.d/clip-%d"),
    ] {
        let command = ffmpeg_command(&settings(codec, file)).unwrap();
        assert_eq!(command.args.last().unwrap(), expected);
        assert_eq!(&command.args[command.args.len() - 3..command.args.len() - 1], &strings(&["-vcodec", encoder]));
    }
    assert_eq!(ffmpeg_codecs_command().args, strings(&["-codecs"]));

    let bad_codec = "codec must be \"auto\" or an ffmpeg encoder tag";
    assert_eq!(ffmpeg_command(&settings("", "a.mp4")), Err(bad_codec.into()));
    assert_eq!(ffmpeg_command(&settings("-f", "a.mp4")), Err(bad_codec.into()));
    assert_eq!(ffmpeg_command(&settings("h264 -x", "a.mp4")), Err(bad_codec.into()));
    assert!(ffmpeg_command(&settings("libvpx-vp9", "a.webm")).is_ok());

    // An audio file is a second input, made as long as the video.
    let tone = |path: &str, delay_ms: u64| AudioInput { path: path.into(), delay_ms };
    let with_audio = EncoderSettings { audio: vec![tone("/s/tone.wav", 0)], ..settings("auto", "a.mp4") };
    let args = ffmpeg_command(&with_audio).unwrap().args;
    let at = args.iter().position(|arg| arg == "/s/tone.wav").unwrap();
    assert_eq!(
        args[at - 5..at + 11],
        strings(&[
            "-i", "-", "-protocol_whitelist", "file", "-i", "/s/tone.wav", "-map", "0:v", "-map", "1:a:0", "-af", "apad",
            "-t", "3", "-vframes", "90",
        ])
    );
    assert!(!args.contains(&"-an".to_owned()));
    assert!(ffmpeg_command(&settings("auto", "a.mp4")).unwrap().args.contains(&"-an".to_owned()));
    let thirds = EncoderSettings { framerate: 3, frames: 4, ..with_audio.clone() };
    assert!(ffmpeg_command(&thirds).unwrap().args.windows(2).any(|pair| pair == ["-t", "1.3333333333333333"]));
    let image = EncoderSettings { audio: vec![tone("/s/tone.wav", 0)], ..settings("png", "a.png") };
    assert_eq!(ffmpeg_command(&image), Err("an image sequence has no audio track".into()));
    let option = EncoderSettings { audio: vec![tone("/s/a.wav", 0), tone("-i", 0)], ..settings("auto", "a.mp4") };
    assert_eq!(ffmpeg_command(&option), Err("audio file must be nonempty and must not start with '-'".into()));

    // A sound that starts later is delayed in its own `-af` chain.
    let late = EncoderSettings { audio: vec![tone("/s/tone.wav", 500)], ..settings("auto", "a.mp4") };
    let args = ffmpeg_command(&late).unwrap().args;
    assert!(args.windows(2).any(|pair| pair == ["-af", "adelay=500:all=1,apad"]), "{args:?}");
    // Two or more are each delayed and mixed at full volume in a filter graph.
    let mixed =
        EncoderSettings { audio: vec![tone("/s/a.wav", 0), tone("/s/b.wav", 1250)], ..settings("auto", "a.mp4") };
    let args = ffmpeg_command(&mixed).unwrap().args;
    let at = args.iter().position(|arg| arg == "/s/a.wav").unwrap();
    assert_eq!(
        args[at - 3..at + 11],
        strings(&[
            "-protocol_whitelist", "file", "-i", "/s/a.wav", "-protocol_whitelist", "file", "-i", "/s/b.wav",
            "-map", "0:v", "-filter_complex",
            "[1:a:0]anull[a0];[2:a:0]adelay=1250:all=1,anull[a1];[a0][a1]\
             amix=inputs=2:normalize=0,asetpts=N/SR/TB,apad=whole_dur=3,atrim=end=3[audio]",
            "-map", "[audio]",
        ])
    );
    assert!(!args.contains(&"-af".to_owned()) && !args.contains(&"-an".to_owned()));
    let bad_file = "output file must be nonempty and must not start with '-'";
    assert_eq!(ffmpeg_command(&settings("auto", "")), Err(bad_file.into()));
    assert_eq!(ffmpeg_command(&settings("auto", "-a.mp4")), Err(bad_file.into()));
    let mut zero = settings("auto", "a.mp4");
    zero.frames = 0;
    assert_eq!(ffmpeg_command(&zero), Err("framerate, frames, width and height must be positive".into()));
}

#[test]
fn plan_and_command_agree_on_frames_and_size() {
    let mut session = open_fixture(true);
    let plan = plan_video(&mut session, &VideoExportOptions { loops: 2, ..Default::default() }).unwrap();
    let command = ffmpeg_command(&EncoderSettings {
        codec: "auto".into(),
        framerate: plan.plan.fps as u32,
        file: "out.mp4".into(),
        frames: plan.frames() as u32,
        width: plan.camera.width,
        height: plan.camera.height,
        transparency: false,
        ffmpeg_options: String::new(),
        audio: Vec::new(),
    })
    .unwrap();
    assert_eq!(command.args[11], "8x8");
    assert_eq!(command.args[13], "4");
    assert_eq!(command.args[17], "8");
}

#[test]
fn encoder_list_is_parsed_sorted_and_prefixed_with_auto() {
    let output = "Codecs:\n D..... = Decoding supported\n .E.... = Encoding supported\n -------\n \
        DEV.LS h264                 H.264 / AVC / MPEG-4 AVC\n \
        DEA.L. aac                  AAC (Advanced Audio Coding)\n \
        D.V.L. gif                  GIF (Graphics Interchange Format)\n \
        .EV.L. mpeg4                MPEG-4 part 2\n \
        .EV.L. broken\n \
        .E\n";
    let auto = VideoCodec { tag: "auto".into(), name: "Automatic".into() };
    let h264 = VideoCodec { tag: "h264".into(), name: "H.264 / AVC / MPEG-4 AVC".into() };
    let mpeg4 = VideoCodec { tag: "mpeg4".into(), name: "MPEG-4 part 2".into() };
    assert_eq!(parse_encoders(output), Ok(vec![auto, h264, mpeg4]));
    assert_eq!(parse_encoders("ffmpeg: command not found"), Err("ffmpeg codec list was not recognised".into()));
}

// ------------------------------------------------------------------ JPEG

/// Minimal baseline JFIF decoder (8-bit, 3 components, 1x1 sampling), written independently of the
/// encoder: it reads every table from the stream and reports `(width, height, rgb pixels)`.
fn decode_jpeg(bytes: &[u8]) -> (usize, usize, Vec<[u8; 3]>) {
    struct Huffman {
        first_code: [i32; 17],
        count: [i32; 17],
        offset: [usize; 17],
        values: Vec<u8>,
    }
    assert_eq!(&bytes[..2], &[0xff, 0xd8]);
    let mut quant: [[i32; 64]; 4] = [[1; 64]; 4];
    let mut huffman: Vec<(u8, Huffman)> = Vec::new();
    let (mut width, mut height) = (0usize, 0usize);
    let mut components: Vec<(u8, usize)> = Vec::new(); // (id, quant table)
    let mut scan_tables: Vec<(usize, usize)> = Vec::new();
    let mut at = 2;
    let scan_start = loop {
        assert_eq!(bytes[at], 0xff, "marker expected at {at}");
        let marker = bytes[at + 1];
        let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        let payload = &bytes[at + 4..at + 2 + length];
        match marker {
            0xdb => {
                assert_eq!(payload[0] >> 4, 0, "8-bit quantisation tables only");
                let zigzag = zigzag_order();
                for (k, value) in payload[1..65].iter().enumerate() {
                    quant[usize::from(payload[0] & 15)][zigzag[k]] = i32::from(*value);
                }
            }
            0xc0 => {
                assert_eq!(payload[0], 8);
                height = usize::from(u16::from_be_bytes([payload[1], payload[2]]));
                width = usize::from(u16::from_be_bytes([payload[3], payload[4]]));
                assert_eq!(payload[5], 3);
                for c in payload[6..15].chunks(3) {
                    assert_eq!(c[1], 0x11, "only 4:4:4 is supported by this decoder");
                    components.push((c[0], usize::from(c[2])));
                }
            }
            0xc4 => {
                let mut rest = payload;
                while !rest.is_empty() {
                    let counts: Vec<i32> = rest[1..17].iter().map(|c| i32::from(*c)).collect();
                    let total: usize = counts.iter().sum::<i32>() as usize;
                    let mut table = Huffman {
                        first_code: [0; 17],
                        count: [0; 17],
                        offset: [0; 17],
                        values: rest[17..17 + total].to_vec(),
                    };
                    let (mut code, mut offset) = (0i32, 0usize);
                    for length in 1..=16 {
                        table.first_code[length] = code;
                        table.count[length] = counts[length - 1];
                        table.offset[length] = offset;
                        code = (code + counts[length - 1]) << 1;
                        offset += counts[length - 1] as usize;
                    }
                    huffman.push((rest[0], table));
                    rest = &rest[17 + total..];
                }
            }
            0xda => {
                assert_eq!(payload[0], 3);
                for c in payload[1..7].chunks(2) {
                    scan_tables.push((usize::from(c[1] >> 4), usize::from(c[1] & 15)));
                }
                assert_eq!(&payload[7..], &[0, 63, 0]);
                break at + 2 + length;
            }
            0xe0 | 0xe1 => {}
            other => panic!("unexpected marker {other:#x}"),
        }
        at += 2 + length;
    };
    assert_eq!(&bytes[bytes.len() - 2..], &[0xff, 0xd9]);
    let mut scan = Vec::new();
    let mut i = scan_start;
    while i < bytes.len() - 2 {
        scan.push(bytes[i]);
        if bytes[i] == 0xff {
            assert_eq!(bytes[i + 1], 0, "unstuffed 0xff inside the scan");
            i += 1;
        }
        i += 1;
    }

    let mut bit_position = 0usize;
    let mut next_bit = || -> i32 {
        let byte = *scan.get(bit_position / 8).expect("scan data ended early");
        let bit = (byte >> (7 - bit_position % 8)) & 1;
        bit_position += 1;
        i32::from(bit)
    };
    let table = |class: u8, id: usize| &huffman.iter().find(|(key, _)| *key == (class << 4) | id as u8).unwrap().1;
    let zigzag = zigzag_order();
    let basis: Vec<Vec<f64>> = (0..8)
        .map(|u| {
            let scale = if u == 0 { (1.0f64 / 8.0).sqrt() } else { 0.5 };
            (0..8).map(|x| scale * ((2 * x + 1) as f64 * u as f64 * std::f64::consts::PI / 16.0).cos()).collect()
        })
        .collect();

    let mut planes = vec![vec![0f64; width.next_multiple_of(8) * height.next_multiple_of(8)]; 3];
    let stride = width.next_multiple_of(8);
    let mut previous_dc = [0i32; 3];
    for block_y in 0..height.div_ceil(8) {
        for block_x in 0..width.div_ceil(8) {
            for (component, (_, quant_id)) in components.iter().enumerate() {
                let (dc_id, ac_id) = scan_tables[component];
                let decode = |table: &Huffman, next_bit: &mut dyn FnMut() -> i32| -> u8 {
                    let mut code = 0;
                    for length in 1..=16usize {
                        code = (code << 1) | next_bit();
                        let index = code - table.first_code[length];
                        if index >= 0 && index < table.count[length] {
                            return table.values[table.offset[length] + index as usize];
                        }
                    }
                    panic!("invalid huffman code");
                };
                let receive = |size: u8, next_bit: &mut dyn FnMut() -> i32| -> i32 {
                    let mut value = 0;
                    for _ in 0..size {
                        value = (value << 1) | next_bit();
                    }
                    if size > 0 && value < (1 << (size - 1)) { value - (1 << size) + 1 } else { value }
                };
                let mut coefficients = [0i32; 64];
                let size = decode(table(0, dc_id), &mut next_bit);
                previous_dc[component] += receive(size, &mut next_bit);
                coefficients[0] = previous_dc[component] * quant[*quant_id][0];
                let mut k = 1;
                while k < 64 {
                    let symbol = decode(table(1, ac_id), &mut next_bit);
                    if symbol == 0 {
                        break;
                    }
                    k += usize::from(symbol >> 4);
                    if symbol == 0xf0 {
                        k += 1;
                        continue;
                    }
                    let natural = zigzag[k];
                    coefficients[natural] = receive(symbol & 15, &mut next_bit) * quant[*quant_id][natural];
                    k += 1;
                }
                for y in 0..8 {
                    for x in 0..8 {
                        let mut sum = 0.0;
                        for v in 0..8 {
                            for u in 0..8 {
                                sum += f64::from(coefficients[v * 8 + u]) * basis[u][x] * basis[v][y];
                            }
                        }
                        planes[component][(block_y * 8 + y) * stride + block_x * 8 + x] = sum;
                    }
                }
            }
        }
    }
    let clamp = |value: f64| value.round().clamp(0.0, 255.0) as u8;
    let mut pixels = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let (luma, blue, red) = (planes[0][y * stride + x] + 128.0, planes[1][y * stride + x], planes[2][y * stride + x]);
            pixels.push([
                clamp(luma + 1.402 * red),
                clamp(luma - 0.344_136 * blue - 0.714_136 * red),
                clamp(luma + 1.772 * blue),
            ]);
        }
    }
    (width, height, pixels)
}

fn zigzag_order() -> [usize; 64] {
    // Generated by walking the anti-diagonals, not copied from the encoder.
    let mut order = [0usize; 64];
    let (mut x, mut y, mut up) = (0i32, 0i32, true);
    for slot in order.iter_mut() {
        *slot = (y * 8 + x) as usize;
        if up {
            if x == 7 { y += 1; up = false } else if y == 0 { x += 1; up = false } else { x += 1; y -= 1 }
        } else if y == 7 {
            x += 1;
            up = true;
        } else if x == 0 {
            y += 1;
            up = true;
        } else {
            x -= 1;
            y += 1;
        }
    }
    order
}

fn solid(width: u32, height: u32, rgb: [u8; 3]) -> image::RgbaImage {
    image::RgbaImage::from_pixel(width, height, image::Rgba([rgb[0], rgb[1], rgb[2], 255]))
}

fn max_error(decoded: &[[u8; 3]], expected: impl Fn(usize) -> [u8; 3]) -> i32 {
    decoded
        .iter()
        .enumerate()
        .flat_map(|(i, pixel)| {
            let want = expected(i);
            (0..3).map(move |c| (i32::from(pixel[c]) - i32::from(want[c])).abs())
        })
        .max()
        .unwrap()
}

#[test]
fn zigzag_helper_matches_the_standard_scan_prefix() {
    assert_eq!(&zigzag_order()[..10], &[0, 1, 8, 16, 9, 2, 3, 10, 17, 24]);
    assert_eq!(zigzag_order()[63], 63);
}

#[test]
fn jpeg_stream_has_the_baseline_jfif_structure() {
    let bytes = encode_jpeg(&solid(20, 9, [10, 200, 90])).unwrap();
    assert_eq!(&bytes[..2], &[0xff, 0xd8], "SOI");
    // APP0 JFIF 1.01, no density units, 1:1 aspect, no thumbnail.
    assert_eq!(
        &bytes[2..20],
        &[0xff, 0xe0, 0, 16, b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0]
    );
    // Walk the segments up to the scan.
    let mut segments: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut at = 2;
    loop {
        assert_eq!(bytes[at], 0xff);
        let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        segments.push((bytes[at + 1], bytes[at + 4..at + 2 + length].to_vec()));
        at += 2 + length;
        if segments.last().unwrap().0 == 0xda {
            break;
        }
    }
    let markers: Vec<u8> = segments.iter().map(|(m, _)| *m).collect();
    assert_eq!(markers, [0xe0, 0xdb, 0xdb, 0xc0, 0xc4, 0xc4, 0xc4, 0xc4, 0xda]);
    assert_eq!(&bytes[bytes.len() - 2..], &[0xff, 0xd9], "EOI");

    // Quality 90: scale 20, so Annex K luma 16,11,12 / chroma 17,18,18 become 3,2,2 / 3,4,4 in zigzag order.
    assert_eq!((segments[1].1[0], &segments[1].1[1..4], segments[1].1[64]), (0, &[3u8, 2, 2][..], 20));
    assert_eq!((segments[2].1[0], &segments[2].1[1..4], segments[2].1[64]), (1, &[3u8, 4, 4][..], 20));
    assert_eq!(segments[1].1.len(), 65);
    // SOF0: 8-bit, 9 rows, 20 columns, three components with 1x1 sampling, quant tables 0/1/1.
    assert_eq!(segments[3].1, [8, 0, 9, 0, 20, 3, 1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1]);
    // DHT: DC luma, AC luma, DC chroma, AC chroma with the Annex K symbol counts.
    let dht: Vec<(u8, usize)> = segments[4..8].iter().map(|(_, p)| (p[0], p.len() - 17)).collect();
    assert_eq!(dht, [(0x00, 12), (0x10, 162), (0x01, 12), (0x11, 162)]);
    assert_eq!(&segments[4].1[1..17], &[0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(&segments[5].1[1..17], &[0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d]);
    assert_eq!(&segments[7].1[1..17], &[0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77]);
    for ac in [&segments[5].1, &segments[7].1] {
        let mut symbols: Vec<u8> = ac[17..].to_vec();
        symbols.sort_unstable();
        let mut expected: Vec<u8> = (0..16u8).flat_map(|run| (1..=10u8).map(move |size| run << 4 | size)).collect();
        expected.extend([0x00, 0xf0]);
        expected.sort_unstable();
        assert_eq!(symbols, expected, "every run/size symbol exactly once");
    }
    // SOS: components 1,2,3 with tables 0/0, 1/1, 1/1, spectral selection 0..63.
    assert_eq!(segments[8].1, [3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0]);
    // Entropy data: every 0xff is stuffed with 0x00.
    let scan = &bytes[at..bytes.len() - 2];
    assert!(!scan.is_empty());
    assert!(scan.windows(2).all(|w| w[0] != 0xff || w[1] == 0x00));
    assert_ne!(scan[scan.len() - 1], 0xff, "the final byte is padded with one-bits, then stuffed");
}

#[test]
fn jpeg_flat_blocks_decode_to_the_input_colour() {
    for rgb in [[200u8, 100, 80], [0, 0, 0], [255, 255, 255], [10, 200, 90]] {
        let (w, h, pixels) = decode_jpeg(&encode_jpeg(&solid(16, 16, rgb)).unwrap());
        assert_eq!((w, h), (16, 16));
        assert!(max_error(&pixels, |_| rgb) <= 3, "{rgb:?} decoded to {:?}", pixels[0]);
    }
}

#[test]
fn jpeg_gradients_and_hard_edges_stay_close_to_the_input() {
    let mut gradient = image::RgbaImage::new(32, 24);
    for (x, y, pixel) in gradient.enumerate_pixels_mut() {
        *pixel = image::Rgba([(x * 8) as u8, (y * 10) as u8, 128, 255]);
    }
    let (w, h, pixels) = decode_jpeg(&encode_jpeg(&gradient).unwrap());
    assert_eq!((w, h), (32, 24));
    let error = max_error(&pixels, |i| {
        let (x, y) = ((i % 32) as u32, (i / 32) as u32);
        [(x * 8) as u8, (y * 10) as u8, 128]
    });
    assert!(error <= 8, "gradient error {error}");

    // A black/white split down the middle of a block: ringing is allowed, the plateaus must hold.
    let mut split = image::RgbaImage::new(16, 8);
    for (x, _, pixel) in split.enumerate_pixels_mut() {
        let v = if x < 8 { 0 } else { 255 };
        *pixel = image::Rgba([v, v, v, 255]);
    }
    let (_, _, pixels) = decode_jpeg(&encode_jpeg(&split).unwrap());
    assert!(max_error(&pixels, |i| if i % 16 < 8 { [0, 0, 0] } else { [255, 255, 255] }) <= 3);
}

#[test]
fn jpeg_handles_partial_blocks_and_a_single_pixel() {
    let mut odd = image::RgbaImage::new(13, 7);
    for (x, y, pixel) in odd.enumerate_pixels_mut() {
        *pixel = image::Rgba([(x * 15) as u8, (y * 30) as u8, 200, 255]);
    }
    let bytes = encode_jpeg(&odd).unwrap();
    let (w, h, pixels) = decode_jpeg(&bytes);
    assert_eq!((w, h, pixels.len()), (13, 7, 91));
    let error = max_error(&pixels, |i| [((i % 13) * 15) as u8, ((i / 13) * 30) as u8, 200]);
    assert!(error <= 12, "odd-size error {error}");

    let one = encode_jpeg(&solid(1, 1, [200, 100, 80])).unwrap();
    let (w, h, pixels) = decode_jpeg(&one);
    assert_eq!((w, h), (1, 1));
    assert!(max_error(&pixels, |_| [200, 100, 80]) <= 3, "{:?}", pixels[0]);
    let sof = one.windows(2).position(|w| w == [0xff, 0xc0]).unwrap();
    assert_eq!(&one[sof + 5..sof + 9], &[0, 1, 0, 1], "SOF0 height and width");
}

#[test]
fn jpeg_rejects_unrepresentable_sizes() {
    let message = Err("JPEG dimensions must be between 1 and 65535".to_string());
    assert_eq!(encode_jpeg(&image::RgbaImage::new(0, 4)), message);
    assert_eq!(encode_jpeg(&image::RgbaImage::new(65536, 1)), message);
    assert!(encode_jpeg(&image::RgbaImage::new(65535, 1)).is_ok());
}

#[test]
fn jpeg_export_composites_over_the_background_and_ignores_transparency() {
    let mut session = open_fixture(false);
    let directory = temp_dir();
    let options = ImageExportOptions {
        format: StillFormat::Jpeg,
        transparency: true,
        background: [255, 255, 255],
        ..Default::default()
    };
    let result = export_image(&mut session, &options, &directory.join("shot.png")).unwrap();
    assert_eq!(result.path, directory.join("shot.jpeg"));
    assert_eq!((result.width, result.height), (8, 8));
    let (w, h, pixels) = decode_jpeg(&fs::read(&result.path).unwrap());
    assert_eq!((w, h), (8, 8));
    let at = |x: usize, y: usize| pixels[y * 8 + x];
    for channel in 0..3 {
        assert!(i32::from(at(0, 0)[channel]) >= 235, "background corner {:?}", at(0, 0));
    }
    let want = [200i32, 100, 80];
    for channel in 0..3 {
        assert!((i32::from(at(4, 4)[channel]) - want[channel]).abs() <= 30, "quad centre {:?}", at(4, 4));
    }
    // Existing output is kept unless overwrite is set, as for the other formats.
    let again = export_image(&mut session, &options, &directory.join("shot.jpeg"));
    assert_eq!(again, Err(format!("{} already exists", directory.join("shot.jpeg").display())));
}

// ------------------------------------------------------------------ session operations

fn op(session: &mut Session, request: Value) -> Value {
    session.handle_request(&request)
}

fn op_ok(session: &mut Session, request: Value) -> Value {
    let response = op(session, request);
    assert_eq!(response["succeeded"], true, "{response}");
    response["result"].clone()
}

/// Asserts the request fails with `needle` in its message and leaves the session and `watched` unchanged.
fn op_refused(session: &mut Session, request: Value, needle: &str, watched: &Path) {
    let (before, files) = (session.snapshot(), file_names(watched));
    let response = op(session, request.clone());
    assert_eq!(response["succeeded"], false, "{request} should be refused: {response}");
    let message = response["message"].as_str().unwrap();
    assert!(message.contains(needle), "{request}: {message:?} lacks {needle:?}");
    assert_eq!(session.snapshot(), before, "{request} changed the session");
    assert_eq!(file_names(watched), files, "{request} changed {}", watched.display());
}

#[test]
fn export_image_op_writes_the_png_the_editor_dialog_would() {
    let mut session = open_fixture(true);
    let before = session.snapshot();
    let directory = temp_dir();
    let target = directory.join("shot.png");
    let result = op_ok(&mut session, json!({"op": "export.image", "path": target.to_str().unwrap()}));
    assert_eq!(result, json!({"path": target.to_str().unwrap(), "width": 8, "height": 8}));
    assert_eq!(file_names(&directory), ["shot.png"], "no temporary file is left behind");
    assert_eq!(png_file_dimensions(&target), Ok((8, 8)));
    assert_eq!(pixel(&target, 4, 4), [200, 100, 80, 255], "the textured quad is drawn");
    assert_eq!(pixel(&target, 0, 0), [0, 0, 0, 255], "opaque black background by default");
    assert_session_untouched(&before, &session.snapshot());

    // The extension follows the format; transparency and background are honoured.
    let tga = directory.join("shot2.png");
    let result = op_ok(&mut session, json!({
        "op": "export.image", "path": tga.to_str().unwrap(), "format": "tga", "transparency": true,
    }));
    assert_eq!(result["path"], directory.join("shot2.tga").to_str().unwrap());
    assert_eq!(pixel(&directory.join("shot2.tga"), 0, 0)[3], 0);
    let white = directory.join("white");
    op_ok(&mut session, json!({"op": "export.image", "path": white.to_str().unwrap(), "background": [255, 255, 255]}));
    assert_eq!(pixel(&directory.join("white.png"), 0, 0), [255, 255, 255, 255]);
    op_ok(&mut session, json!({"op": "export.image", "path": directory.join("j").to_str().unwrap(),
        "format": "jpeg", "camera": "Close"}));
    let jpeg = fs::read(directory.join("j.jpeg")).unwrap();
    assert_eq!((&jpeg[..2], &jpeg[jpeg.len() - 2..]), (&[0xff, 0xd8][..], &[0xff, 0xd9][..]), "a JFIF stream");
    assert_session_untouched(&before, &session.snapshot());
}

#[test]
#[cfg(unix)]
fn image_export_writes_the_resolved_file_without_renaming_it_into_the_open_document() {
    for (format, extension) in [("png", "png"), ("tga", "tga"), ("jpeg", "jpeg")] {
        let mut session = open_fixture(true);
        let directory = temp_dir();
        let source = directory.join(format!("canonical.{extension}"));
        op_ok(&mut session, json!({"op":"save","path":source}));
        let original = fs::read(&source).unwrap();
        let resolved = directory.join("canonical.inx");
        fs::write(&resolved, b"replace only this owned output").unwrap();
        let alias = directory.join(format!("requested.{extension}"));
        std::os::unix::fs::symlink(&resolved, &alias).unwrap();
        let before = session.snapshot();
        let response = session.handle_request(&json!({"op":"export.image","path":alias,"format":format,"overwrite":true}));
        assert_eq!(fs::read(&source).unwrap(), original, "{format}: resolving an output link must not rename it into the open document");
        assert_eq!(response["succeeded"], true, "{response}");
        assert_eq!(response["result"]["path"], json!(resolved));
        let bytes = fs::read(&resolved).unwrap();
        let image = if format == "jpeg" { soul2d_core::jpeg::decode(&bytes, 8, 64).unwrap() }
            else { image::load_from_memory_with_format(&bytes, if format == "tga" { image::ImageFormat::Tga } else { image::ImageFormat::Png }).unwrap().to_rgba8() };
        assert_eq!((image.width(), image.height()), (8, 8));
        assert_session_untouched(&before, &session.snapshot());
        assert!(fs::symlink_metadata(alias).unwrap().file_type().is_symlink());
    }
}

#[test]
#[cfg(unix)]
fn single_file_exports_resolve_the_effective_name_before_protecting_the_open_document() {
    let mut session = open_fixture(true);
    let directory = temp_dir();
    let source = directory.join("project.inx");
    op_ok(&mut session, json!({"op":"save","path":source}));
    let bytes = fs::read(&source).unwrap();
    let before = session.snapshot();
    for (op, extension) in [("export.inp", "inp"), ("export.image", "png"), ("export.gif", "gif"),
        ("view.screenshot", "png"), ("object.export_csv", "csv")] {
        let path = directory.join(format!("alias-{op}.{extension}"));
        std::os::unix::fs::symlink(&source, &path).unwrap();
        let raw = if op == "object.export_csv" { path.clone() } else { path.with_extension("ignored") };
        let response = session.handle_request(&json!({"op":op,"path":raw,"overwrite":true,"size":16}));
        assert_eq!(response["succeeded"], false, "{op}: {response}");
        assert!(response["message"].as_str().unwrap().contains("would overwrite the open document"), "{op}: {response}");
        assert_eq!(fs::read(&source).unwrap(), bytes);
        assert_session_untouched(&before, &session.snapshot());
    }
}

#[test]
fn export_image_op_refuses_bad_requests_and_leaves_everything_unchanged() {
    let mut session = open_fixture(true);
    let directory = temp_dir();
    let target = directory.join("shot.png");
    let path = target.to_str().unwrap();
    for (request, needle) in [
        (json!({"op": "export.image"}), "path must be a string"),
        (json!({"op": "export.image", "path": 7}), "path must be a string"),
        (json!({"op": "export.image", "path": ""}), "File path is required"),
        (json!({"op": "export.image", "path": format!("{}/", directory.display())}), "path must name a file"),
        (json!({"op": "export.image", "path": path, "format": "bmp"}), "format must be png, tga or jpeg"),
        (json!({"op": "export.image", "path": path, "format": 3}), "format must be a string"),
        (json!({"op": "export.image", "path": path, "transparency": "yes"}), "transparency must be a boolean"),
        (json!({"op": "export.image", "path": path, "background": [1, 2]}), "background must be [r, g, b]"),
        (json!({"op": "export.image", "path": path, "background": [0, 0, 256]}), "background channels"),
        (json!({"op": "export.image", "path": path, "postprocessing": true}), "post processing"),
        (json!({"op": "export.image", "path": path, "camera": "Nowhere"}), "Camera not found: Nowhere"),
        (json!({"op": "export.image", "path": path, "camera": "Huge"}), "pixel limit"),
        (json!({"op": "export.image", "path": directory.join("no/such/shot.png").to_str().unwrap()}),
            "could not resolve parent directory"),
    ] {
        op_refused(&mut session, request, needle, &directory);
    }

    // An existing file is kept without overwrite and replaced with it.
    fs::write(&target, b"keep me").unwrap();
    op_refused(&mut session, json!({"op": "export.image", "path": path}), "already exists", &directory);
    assert_eq!(fs::read(&target).unwrap(), b"keep me");
    op_ok(&mut session, json!({"op": "export.image", "path": path, "overwrite": true}));
    assert_eq!(png_file_dimensions(&target), Ok((8, 8)));

    // The open project file is never overwritten, even when it is named like an image.
    let project = directory.join("project.png");
    op_ok(&mut session, json!({"op": "save", "path": project.to_str().unwrap()}));
    let saved = fs::read(&project).unwrap();
    op_refused(
        &mut session,
        json!({"op": "export.image", "path": project.to_str().unwrap(), "overwrite": true}),
        "would overwrite the open document",
        &directory,
    );
    assert_eq!(fs::read(&project).unwrap(), saved);
}

#[cfg(unix)]
#[test]
fn export_image_op_reports_an_unwritable_directory_without_naming_more_than_the_caller_did() {
    use std::os::unix::fs::PermissionsExt;
    let mut session = open_fixture(true);
    let directory = temp_dir();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o555)).unwrap();
    let writable_anyway = fs::File::create(directory.join("probe")).is_ok();
    if !writable_anyway {
        let target = directory.join("shot.png");
        let response = op(&mut session, json!({"op": "export.image", "path": target.to_str().unwrap()}));
        assert_eq!(response["succeeded"], false);
        let message = response["message"].as_str().unwrap();
        assert!(message.starts_with("could not create temporary"), "{message}");
        assert!(!message.contains(directory.to_str().unwrap()), "{message}");
        let frames = op(&mut session, json!({"op": "export.frames", "path": target.to_str().unwrap()}));
        assert_eq!(frames["succeeded"], false, "{frames}");
        assert!(!directory.join("shot-1.png").exists());
    }
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn export_frames_op_writes_the_numbered_files_of_the_editor_action() {
    let mut session = open_fixture(true);
    let before = session.snapshot();
    let directory = temp_dir();
    let target = directory.join("clip.png");
    let result = op_ok(&mut session, json!({
        "op": "export.frames", "path": target.to_str().unwrap(), "transparency": true,
    }));
    assert_eq!(result, json!({
        "frames": 4,
        "first": directory.join("clip-1.png").to_str().unwrap(),
        "last": directory.join("clip-4.png").to_str().unwrap(),
    }));
    assert_eq!(file_names(&directory), ["clip-1.png", "clip-2.png", "clip-3.png", "clip-4.png"]);
    let lefts: Vec<u32> = (1..=4).map(|n| quad_left(&directory.join(format!("clip-{n}.png")))).collect();
    assert_eq!(lefts, [2, 3, 4, 5], "the quad slides one pixel per frame");
    assert_session_untouched(&before, &session.snapshot());

    // Options: tga, a doubled rate over two loops.
    let other = temp_dir();
    let result = op_ok(&mut session, json!({
        "op": "export.frames", "path": other.join("t.tga").to_str().unwrap(), "loops": 2, "framerate": 8,
        "snap_to_framerate": true, "transparency": true,
    }));
    assert_eq!(result["frames"], 16);
    assert_eq!(file_names(&other).len(), 16);
    assert!(other.join("t-16.tga").exists());
    assert_session_untouched(&before, &session.snapshot());
}

#[cfg(unix)]
#[test]
fn export_frames_op_writes_where_it_checked_when_the_name_is_a_symlink() {
    let mut session = open_fixture(true);
    let directory = temp_dir();
    // The open project sits where the frames of the link's own name would land.
    let project = directory.join("clip-1.png");
    op_ok(&mut session, json!({"op": "save", "path": project.to_str().unwrap()}));
    let saved = std::fs::read(&project).unwrap();
    std::fs::write(directory.join("other.png"), b"x").unwrap();
    std::os::unix::fs::symlink(directory.join("other.png"), directory.join("clip.png")).unwrap();
    let response = session.handle_request(&json!({
        "op": "export.frames", "path": directory.join("clip.png").to_str().unwrap(), "overwrite": true,
    }));
    assert_eq!(std::fs::read(&project).unwrap(), saved, "{response}");
    assert_eq!(response["succeeded"], true, "{response}");
    assert!(directory.join("other-1.png").exists(), "the frames go where the link points");
}

#[test]
fn export_frames_op_refuses_bad_requests_and_leaves_everything_unchanged() {
    let mut session = open_fixture(true);
    let directory = temp_dir();
    let path = directory.join("clip.png");
    let path = path.to_str().unwrap();
    for (request, needle) in [
        (json!({"op": "export.frames"}), "path must be a string"),
        (json!({"op": "export.frames", "path": directory.join("clip.jpg").to_str().unwrap()}), ".png or .tga"),
        (json!({"op": "export.frames", "path": directory.join("clip").to_str().unwrap()}), ".png or .tga"),
        (json!({"op": "export.frames", "path": path, "framerate": 0}), "framerate must be between 1 and 240"),
        (json!({"op": "export.frames", "path": path, "framerate": 241}), "framerate must be between 1 and 240"),
        (json!({"op": "export.frames", "path": path, "framerate": -1}), "framerate must be a non-negative integer"),
        (json!({"op": "export.frames", "path": path, "loops": "two"}), "loops must be a non-negative integer"),
        (json!({"op": "export.frames", "path": path, "loops": 1_000_000}), "the limit is 100000"),
        (json!({"op": "export.frames", "path": path, "animation": "Nope"}), "Animation not found: Nope"),
        (json!({"op": "export.frames", "path": path, "camera": "Nowhere"}), "Camera not found: Nowhere"),
        (json!({"op": "export.frames", "path": directory.join("no/clip.png").to_str().unwrap()}),
            "could not resolve parent directory"),
    ] {
        op_refused(&mut session, request, needle, &directory);
    }

    // One existing frame stops the whole run before anything is written.
    fs::write(directory.join("clip-3.png"), b"keep me").unwrap();
    op_refused(&mut session, json!({"op": "export.frames", "path": path}), "already exists", &directory);
    assert_eq!(fs::read(directory.join("clip-3.png")).unwrap(), b"keep me");
    op_ok(&mut session, json!({"op": "export.frames", "path": path, "overwrite": true}));
    assert_eq!(png_file_dimensions(&directory.join("clip-3.png")), Ok((8, 8)));

    // A frame name that is the open project file is refused up front.
    let project = directory.join("movie-2.png");
    op_ok(&mut session, json!({"op": "save", "path": project.to_str().unwrap()}));
    op_refused(
        &mut session,
        json!({"op": "export.frames", "path": directory.join("movie.png").to_str().unwrap(), "overwrite": true}),
        "would overwrite the open document",
        &directory,
    );
}

#[test]
fn export_frames_op_caps_frames_times_pixels_and_points_to_the_video_op() {
    let mut session = open_fixture(true);
    let directory = temp_dir();
    let path = directory.join("big.png");
    // 16 frames of 8192 x 8192 are 1.07 G pixels; one 8 x 8 frame is far below the cap.
    let request = json!({"op": "export.frames", "path": path.to_str().unwrap(), "camera": "Big", "loops": 4});
    op_refused(&mut session, request, "limited to 1000000000 pixels", &directory);
    op_refused(
        &mut session,
        json!({"op": "export.frames", "path": path.to_str().unwrap(), "camera": "Big", "loops": 4}),
        "export.video.start with an image codec",
        &directory,
    );
    let small = json!({"op": "export.frames", "path": directory.join("ok.png").to_str().unwrap(), "loops": 4});
    assert_eq!(op_ok(&mut session, small)["frames"], 16);
}

// ------------------------------------------------------------------ GIF

struct DecodedFrame {
    delay: u16,
    /// The graphic control's disposal method: 1 keeps the frame, 2 clears it before the next one.
    disposal: u8,
    /// RGBA, transparent texels `[0, 0, 0, 0]`.
    pixels: Vec<[u8; 4]>,
}

struct DecodedGif {
    width: usize,
    height: usize,
    /// The NETSCAPE loop count, `None` without the extension.
    repeat: Option<u16>,
    frames: Vec<DecodedFrame>,
}

/// A GIF reader written from the format description, so the tests do not trust the encoder's own view.
fn decode_gif(bytes: &[u8]) -> DecodedGif {
    assert_eq!(&bytes[..6], b"GIF89a");
    let word = |at: usize| usize::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]));
    let (width, height) = (word(6), word(8));
    let flags = bytes[10];
    let mut at = 13;
    let global: Vec<[u8; 3]> = if flags & 0x80 != 0 {
        let size = 2usize << (flags & 7);
        let table = (0..size).map(|i| [bytes[at + 3 * i], bytes[at + 3 * i + 1], bytes[at + 3 * i + 2]]).collect();
        at += 3 * size;
        table
    } else {
        Vec::new()
    };
    let (mut repeat, mut frames) = (None, Vec::new());
    let (mut delay, mut disposal, mut transparent) = (0u16, 0u8, None);
    let blocks = |at: &mut usize| -> Vec<u8> {
        let mut data = Vec::new();
        while bytes[*at] != 0 {
            let len = usize::from(bytes[*at]);
            data.extend(&bytes[*at + 1..*at + 1 + len]);
            *at += 1 + len;
        }
        *at += 1;
        data
    };
    loop {
        match bytes[at] {
            0x3B => break,
            0x21 => {
                let label = bytes[at + 1];
                at += 2;
                let data = blocks(&mut at);
                if label == 0xF9 {
                    delay = u16::from_le_bytes([data[1], data[2]]);
                    disposal = (data[0] >> 2) & 7;
                    transparent = (data[0] & 1 != 0).then_some(data[3]);
                } else if label == 0xFF && data.starts_with(b"NETSCAPE2.0") {
                    assert_eq!(data[11], 1);
                    repeat = Some(u16::from_le_bytes([data[12], data[13]]));
                }
            }
            0x2C => {
                assert_eq!((word(at + 1), word(at + 3), word(at + 5), word(at + 7)), (0, 0, width, height));
                let local_flags = bytes[at + 9];
                at += 10;
                let table = if local_flags & 0x80 != 0 {
                    let size = 2usize << (local_flags & 7);
                    let table: Vec<[u8; 3]> =
                        (0..size).map(|i| [bytes[at + 3 * i], bytes[at + 3 * i + 1], bytes[at + 3 * i + 2]]).collect();
                    at += 3 * size;
                    table
                } else {
                    global.clone()
                };
                let min_bits = u32::from(bytes[at]);
                at += 1;
                let data = blocks(&mut at);
                let indices = lzw_decode(&data, min_bits);
                assert_eq!(indices.len(), width * height, "one index per pixel");
                let pixels = indices
                    .iter()
                    .map(|index| {
                        if Some(*index) == transparent {
                            [0, 0, 0, 0]
                        } else {
                            let [r, g, b] = table[usize::from(*index)];
                            [r, g, b, 255]
                        }
                    })
                    .collect();
                frames.push(DecodedFrame { delay, disposal, pixels });
            }
            other => panic!("unexpected GIF block {other:#x}"),
        }
    }
    DecodedGif { width, height, repeat, frames }
}

fn lzw_decode(data: &[u8], min_bits: u32) -> Vec<u8> {
    let (clear, end) = (1usize << min_bits, (1usize << min_bits) + 1);
    let fresh = || -> Vec<Vec<u8>> { (0..=end).map(|code| vec![code as u8]).collect() };
    let (mut table, mut width, mut previous): (Vec<Vec<u8>>, u32, Option<Vec<u8>>) =
        (fresh(), min_bits + 1, None);
    let (mut out, mut position) = (Vec::new(), 0usize);
    while position + width as usize <= data.len() * 8 {
        let code = (0..width as usize)
            .map(|bit| usize::from(data[(position + bit) / 8] >> ((position + bit) % 8) & 1) << bit)
            .sum::<usize>();
        position += width as usize;
        if code == clear {
            (table, width, previous) = (fresh(), min_bits + 1, None);
            continue;
        }
        if code == end {
            return out;
        }
        let entry = if code < table.len() {
            table[code].clone()
        } else {
            let mut guess = previous.clone().expect("a code beyond the table follows another code");
            guess.push(guess[0]);
            guess
        };
        out.extend(&entry);
        if let Some(previous) = previous {
            let mut added = previous;
            added.push(entry[0]);
            if table.len() < 4096 {
                table.push(added);
            }
        }
        previous = Some(entry);
        if table.len() == 1 << width && width < 12 {
            width += 1;
        }
    }
    panic!("the LZW data ended without an end code");
}

/// First column of row 4 that is drawn (`pixels` is a `width` wide RGBA grid).
fn gif_quad_left(frame: &DecodedFrame, width: usize) -> usize {
    (0..width).find(|x| frame.pixels[4 * width + x][3] != 0).expect("quad visible in row 4")
}

fn noise_image(width: u32, height: u32, colors: &[[u8; 3]]) -> image::RgbaImage {
    let mut state = 0x2545F491u32;
    image::RgbaImage::from_fn(width, height, |_, _| {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        let [r, g, b] = colors[(state >> 16) as usize % colors.len()];
        image::Rgba([r, g, b, 255])
    })
}

#[test]
fn the_gif_writer_round_trips_noise_that_fills_and_resets_the_lzw_table() {
    // Sixteen colours that are far apart in 5 bit bins, so the palette loses nothing and the
    // decoded picture must equal the input. 200 x 200 random pixels run the table full many times.
    let colors: Vec<[u8; 3]> = (0..16u8).map(|i| [i * 16, 255 - i * 16, i.wrapping_mul(37) ^ 0x80]).collect();
    let image = noise_image(200, 200, &colors);
    let mut writer = soul2d_core::gif::GifWriter::new(200, 200, 7, 256, false, Some(3)).unwrap();
    writer.add_frame(&image).unwrap();
    writer.add_frame(&image).unwrap();
    let gif = decode_gif(&writer.finish());
    assert_eq!((gif.width, gif.height, gif.repeat, gif.frames.len()), (200, 200, Some(3), 2));
    for frame in &gif.frames {
        assert_eq!(frame.delay, 7);
        let expected: Vec<[u8; 4]> = image.pixels().map(|p| p.0).collect();
        assert_eq!(frame.pixels, expected);
    }
}

#[test]
fn the_gif_writer_keeps_to_the_colour_limit_and_the_transparent_slot() {
    // A smooth ramp has far more than 4 colours; `colors` caps the palette, and a transparent texel
    // costs one of the slots without costing the picture its other pixels.
    let ramp = image::RgbaImage::from_fn(64, 8, |x, y| {
        let alpha = if x == 0 && y == 0 { 0 } else { 255 };
        image::Rgba([(x * 4) as u8, (255 - x * 4) as u8, 40, alpha])
    });
    for (colors, transparent) in [(4usize, false), (4, true), (2, false), (256, true)] {
        let mut writer = soul2d_core::gif::GifWriter::new(64, 8, 5, colors, transparent, None).unwrap();
        writer.add_frame(&ramp).unwrap();
        let gif = decode_gif(&writer.finish());
        let frame = &gif.frames[0];
        let distinct: std::collections::BTreeSet<[u8; 4]> = frame.pixels.iter().copied().collect();
        let drawn = distinct.iter().filter(|p| p[3] != 0).count();
        assert!(drawn <= colors - usize::from(transparent), "{colors} colours, transparent {transparent}: {drawn}");
        assert_eq!(frame.pixels[0][3] == 0, transparent, "only a kept-transparent export has a hole");
        assert!(frame.pixels[1..].iter().all(|p| p[3] == 255));
        assert_eq!(gif.repeat, Some(0), "absent repeat loops forever");
    }
    // The widest palette keeps the ramp within one 5 bit bin of the input.
    let mut writer = soul2d_core::gif::GifWriter::new(64, 8, 5, 256, false, None).unwrap();
    writer.add_frame(&ramp).unwrap();
    let frame = &decode_gif(&writer.finish()).frames[0];
    for (got, want) in frame.pixels.iter().zip(ramp.pixels()).skip(1) {
        assert!((0..3).all(|c| got[c].abs_diff(want[c]) <= 8), "{got:?} vs {want:?}");
    }
}

fn round_trip(image: &image::RgbaImage, colors: usize, transparent: bool) -> Vec<[u8; 4]> {
    let mut writer =
        soul2d_core::gif::GifWriter::new(image.width(), image.height(), 5, colors, transparent, None).unwrap();
    writer.add_frame(image).unwrap();
    decode_gif(&writer.finish()).frames.remove(0).pixels
}

#[test]
fn the_gif_writer_keeps_256_colours_exactly() {
    // 256 colours in 256 different 5 bit bins: the widest palette holds every one of them.
    let image = image::RgbaImage::from_fn(16, 16, |x, y| image::Rgba([(x * 16) as u8, (y * 16) as u8, 99, 255]));
    let expected: Vec<[u8; 4]> = image.pixels().map(|p| p.0).collect();
    assert_eq!(round_trip(&image, 256, false), expected);
}

#[test]
fn the_gif_writer_keeps_255_colours_and_the_transparent_slot_at_256() {
    // 256 colours in different bins, one texel of them hidden: 255 drawn colours fill the 255 opaque slots
    // exactly, and no drawn texel lands on the transparent index.
    let image = image::RgbaImage::from_fn(16, 16, |x, y| {
        let alpha = if (x, y) == (15, 15) { 0 } else { 255 };
        image::Rgba([(x * 16) as u8, (y * 16) as u8, 99, alpha])
    });
    let expected: Vec<[u8; 4]> = image.pixels().map(|p| if p[3] == 0 { [0, 0, 0, 0] } else { p.0 }).collect();
    assert_eq!(round_trip(&image, 256, true), expected);
}

#[test]
fn a_gif_frame_with_nothing_drawn_is_all_transparent() {
    let image = image::RgbaImage::from_pixel(5, 3, image::Rgba([200, 30, 30, 0]));
    assert_eq!(round_trip(&image, 256, true), vec![[0, 0, 0, 0]; 15]);
}

#[test]
fn gif_transparent_texels_take_no_palette_slot_from_the_drawn_ones() {
    // Two drawn colours and two slots for them (three with the transparent one). The hidden texels'
    // colours must not win a slot over a drawn colour.
    let image = image::RgbaImage::from_fn(12, 4, |x, _| match x % 3 {
        0 => image::Rgba([250, 10, 10, 255]),
        1 => image::Rgba([10, 10, 250, 255]),
        _ => image::Rgba([10, 250, 10, 0]),
    });
    let expected: Vec<[u8; 4]> = image.pixels().map(|p| if p[3] == 0 { [0, 0, 0, 0] } else { p.0 }).collect();
    assert_eq!(round_trip(&image, 3, true), expected);
}

#[test]
fn the_gif_end_code_follows_a_code_width_that_grew_with_the_last_code() {
    // One colour: the LZW codes cover runs of 1, 2, 3, ... pixels. After 10 runs (55 pixels) the table's next
    // entry is 16, so the reader widens its codes to 5 bits after the last data code, and the end code must be
    // written 5 bits wide. From 56 to 66 pixels the data then fills exactly 6 bytes, so a 4 bit end code would
    // leave the reader short of bits. The sizes on either side of that range must decode too.
    for pixels in [55u32, 56, 60, 66, 67] {
        let image = image::RgbaImage::from_pixel(pixels, 1, image::Rgba([40, 80, 120, 255]));
        assert_eq!(round_trip(&image, 256, false), vec![[40, 80, 120, 255]; pixels as usize], "{pixels} pixels");
    }
}

#[test]
fn the_gif_writer_refuses_a_frame_of_another_size_and_an_oversize_picture() {
    let mut writer = soul2d_core::gif::GifWriter::new(4, 4, 5, 16, false, None).unwrap();
    assert_eq!(writer.add_frame(&image::RgbaImage::new(5, 4)), Err("a GIF frame changed size".into()));
    assert!(soul2d_core::gif::GifWriter::new(65_536, 4, 5, 16, false, None).is_err());
    assert!(soul2d_core::gif::GifWriter::new(0, 4, 5, 16, false, None).is_err());
}

#[test]
fn export_gif_op_writes_one_animated_gif_of_the_editor_clip() {
    let mut session = open_fixture(true);
    let before = session.snapshot();
    let directory = temp_dir();
    // A typed extension is replaced, as `export.image` does.
    let result = op_ok(&mut session, json!({
        "op": "export.gif", "path": directory.join("clip.png").to_str().unwrap(), "transparency": true,
    }));
    let target = directory.join("clip.gif");
    assert_eq!(result, json!({
        "path": target.to_str().unwrap(), "frames": 4, "width": 8, "height": 8, "delay": 25,
    }));
    assert_eq!(file_names(&directory), ["clip.gif"]);
    let gif = decode_gif(&fs::read(&target).unwrap());
    assert_eq!((gif.width, gif.height, gif.frames.len()), (8, 8, 4));
    assert_eq!(gif.repeat, Some(0), "loops forever unless asked");
    assert!(gif.frames.iter().all(|frame| frame.delay == 25));
    assert!(gif.frames.iter().all(|frame| frame.disposal == 2), "a transparent frame is cleared before the next");
    let lefts: Vec<usize> = gif.frames.iter().map(|frame| gif_quad_left(frame, 8)).collect();
    assert_eq!(lefts, [2, 3, 4, 5], "the quad slides one pixel per frame");
    // The same frame as `export.frames` draws it, to one 5 bit bin.
    op_ok(&mut session, json!({
        "op": "export.frames", "path": directory.join("ref.png").to_str().unwrap(), "transparency": true,
    }));
    for n in 1..=4usize {
        let reference = image::open(directory.join(format!("ref-{n}.png"))).unwrap().into_rgba8();
        for (index, got) in gif.frames[n - 1].pixels.iter().enumerate() {
            let want = reference.get_pixel((index % 8) as u32, (index / 8) as u32).0;
            if want[3] == 255 {
                assert!((0..3).all(|c| got[c].abs_diff(want[c]) <= 8) && got[3] == 255, "frame {n} texel {index}");
            } else if want[3] == 0 {
                assert_eq!(got[3], 0, "frame {n} texel {index}");
            }
        }
    }
    assert_session_untouched(&before, &session.snapshot());

    // Options: no transparency draws over the background, a rate of 10 is 10 hundredths, repeat plays 3 times.
    let other = temp_dir();
    let result = op_ok(&mut session, json!({
        "op": "export.gif", "path": other.join("b.gif").to_str().unwrap(), "background": [9, 8, 7],
        "framerate": 10, "colors": 4, "repeat": 2, "loops": 2,
    }));
    assert_eq!((result["frames"].as_u64(), result["delay"].as_u64()), (Some(20), Some(10)));
    let gif = decode_gif(&fs::read(other.join("b.gif")).unwrap());
    assert_eq!((gif.repeat, gif.frames.len()), (Some(2), 20));
    assert!(gif.frames.iter().all(|frame| frame.pixels[0] == [9, 8, 7, 255]), "the background shows");
    assert!(gif.frames.iter().all(|frame| frame.disposal == 1), "an opaque frame is kept under the next");
    assert_session_untouched(&before, &session.snapshot());

    // repeat 0 plays once: no loop extension at all. A rate over 50 a second is rendered at 50, two hundredths
    // a frame, the shortest that browsers play as written, so the one second clip stays one second long.
    op_ok(&mut session, json!({"op": "export.gif", "path": other.join("once.gif").to_str().unwrap(), "repeat": 0}));
    assert_eq!(decode_gif(&fs::read(other.join("once.gif")).unwrap()).repeat, None);
    let fast = op_ok(&mut session, json!({"op": "export.gif", "path": other.join("fast.gif").to_str().unwrap(), "framerate": 240}));
    assert_eq!((fast["delay"].as_u64(), fast["frames"].as_u64()), (Some(2), Some(50)));
    assert_eq!(decode_gif(&fs::read(other.join("fast.gif")).unwrap()).frames.len(), 50);
    let seventy = op_ok(&mut session, json!({"op": "export.gif", "path": other.join("seventy.gif").to_str().unwrap(), "framerate": 70}));
    assert_eq!((seventy["delay"].as_u64(), seventy["frames"].as_u64()), (Some(2), Some(50)));
    let fifty = op_ok(&mut session, json!({"op": "export.gif", "path": other.join("fifty.gif").to_str().unwrap(), "framerate": 50}));
    assert_eq!((fifty["delay"].as_u64(), fifty["frames"].as_u64()), (Some(2), Some(50)));
    // 8 a second is 12.5 hundredths, rounded to 13.
    let eight = op_ok(&mut session, json!({"op": "export.gif", "path": other.join("eight.gif").to_str().unwrap(), "framerate": 8}));
    assert_eq!(eight["delay"], 13);
}

#[test]
fn a_gif_checks_the_frame_limit_at_50_fps_not_at_the_rate_asked_for() {
    // 1000 loops of the 1 s clip: 240000 frames at 240 fps is over the limit, 50000 at 50 fps is not.
    let mut session = open_fixture(true);
    let options = VideoExportOptions { loops: 1000, framerate: Some(240), ..Default::default() };
    let (options, plan) = plan_gif(&mut session, &options).unwrap();
    assert_eq!((options.framerate, plan.plan.fps, plan.frames()), (Some(50), 50, 50_000));
}

#[test]
fn an_animation_faster_than_50_fps_is_written_at_50_fps() {
    // The clip's own rate is 100 fps (4 frames, 0.04 s) and no framerate is asked for: 2 frames of 2
    // hundredths keep its length.
    let mut session = open_fixture(true);
    op_ok(&mut session, json!({"op": "animation.set", "name": "Idle", "timestep": 0.01}));
    let directory = temp_dir();
    let result = op_ok(&mut session, json!({"op": "export.gif", "path": directory.join("fast.gif").to_str().unwrap()}));
    assert_eq!((result["frames"].as_u64(), result["delay"].as_u64()), (Some(2), Some(2)));
    let gif = decode_gif(&fs::read(directory.join("fast.gif")).unwrap());
    assert_eq!(gif.frames.iter().map(|frame| frame.delay).collect::<Vec<_>>(), [2, 2]);
}

#[test]
fn export_gif_op_refuses_bad_requests_and_leaves_everything_unchanged() {
    let mut session = open_fixture(true);
    let directory = temp_dir();
    let path = directory.join("clip.gif");
    let path = path.to_str().unwrap();
    for (request, needle) in [
        (json!({"op": "export.gif"}), "path must be a string"),
        (json!({"op": "export.gif", "path": path, "colors": 1}), "colors must be an integer from 2 to 256"),
        (json!({"op": "export.gif", "path": path, "colors": 257}), "colors must be an integer from 2 to 256"),
        (json!({"op": "export.gif", "path": path, "colors": "all"}), "colors must be an integer from 2 to 256"),
        (json!({"op": "export.gif", "path": path, "repeat": 65_536}), "repeat must be an integer from 0 to 65535"),
        (json!({"op": "export.gif", "path": path, "repeat": -1}), "repeat must be an integer from 0 to 65535"),
        (json!({"op": "export.gif", "path": path, "framerate": 0}), "framerate must be between 1 and 240"),
        (json!({"op": "export.gif", "path": path, "animation": "Nope"}), "Animation not found: Nope"),
        (json!({"op": "export.gif", "path": path, "camera": "Nowhere"}), "Camera not found: Nowhere"),
        (json!({"op": "export.gif", "path": path, "camera": "Big", "loops": 4}), "limited to 1000000000 pixels"),
        (json!({"op": "export.gif", "path": path, "camera": "Huge"}), "render pixel limit"),
        (json!({"op": "export.gif", "path": format!("{path}/")}), "path must name a file"),
        (json!({"op": "export.gif", "path": directory.join("no/clip.gif").to_str().unwrap()}),
            "could not resolve parent directory"),
    ] {
        op_refused(&mut session, request, needle, &directory);
    }

    // An existing file is kept without overwrite, and replaced with it.
    fs::write(&path, b"keep me").unwrap();
    op_refused(&mut session, json!({"op": "export.gif", "path": path}), "already exists", &directory);
    assert_eq!(fs::read(path).unwrap(), b"keep me");
    op_ok(&mut session, json!({"op": "export.gif", "path": path, "overwrite": true}));
    assert_eq!(decode_gif(&fs::read(path).unwrap()).frames.len(), 4);

    // The open project cannot be the destination.
    let project = directory.join("movie.gif");
    op_ok(&mut session, json!({"op": "save", "path": project.to_str().unwrap()}));
    op_refused(
        &mut session,
        json!({"op": "export.gif", "path": project.to_str().unwrap(), "overwrite": true}),
        "would overwrite the open document",
        &directory,
    );
}

#[test]
fn export_tools_are_listed_and_work_through_mcp() {
    let mut session = open_fixture(true);
    let listed = session.handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    let names: Vec<&str> =
        listed["result"]["tools"].as_array().unwrap().iter().map(|tool| tool["name"].as_str().unwrap()).collect();
    for tool in ["export_image", "export_frames", "export_gif", "export_video_start", "export_video_status", "export_video_cancel"] {
        assert!(names.contains(&tool), "{tool} is listed");
    }
    let directory = temp_dir();
    let target = directory.join("mcp.png");
    let called = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "export_image", "arguments": {"path": target.to_str().unwrap()}}}))
        .unwrap();
    assert_eq!(called["result"]["isError"], false, "{called}");
    assert_eq!(png_file_dimensions(&target), Ok((8, 8)));
    let failed = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "export_image", "arguments": {"path": target.to_str().unwrap()}}}))
        .unwrap();
    assert_eq!(failed["result"]["isError"], true, "an existing file is refused: {failed}");
}

#[test]
fn export_tools_work_through_the_stdio_mcp_host() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    let mut source = open_fixture(false);
    let directory = temp_dir();
    let project = directory.join("project.inx");
    op_ok(&mut source, json!({"op": "save", "path": project.to_str().unwrap()}));
    let target = directory.join("stdio.png");
    let mut host = Command::new(env!("CARGO_BIN_EXE_soul2d-core"))
        .arg("--mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let call = |id: u32, name: &str, arguments: Value| {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": arguments}})
    };
    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        call(2, "open", json!({"path": project.to_str().unwrap()})),
        call(3, "export_image", json!({"path": target.to_str().unwrap()})),
    ];
    let mut stdin = host.stdin.take().unwrap();
    for request in &requests {
        writeln!(stdin, "{request}").unwrap();
    }
    drop(stdin);
    let replies: Vec<Value> = BufReader::new(host.stdout.take().unwrap())
        .lines()
        .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
        .collect();
    host.wait().unwrap();
    assert_eq!(replies.len(), 3);
    let listed: Vec<&str> =
        replies[0]["result"]["tools"].as_array().unwrap().iter().map(|tool| tool["name"].as_str().unwrap()).collect();
    assert!(listed.contains(&"export_video_start") && listed.contains(&"export_frames"));
    assert_eq!(replies[2]["result"]["isError"], false, "{}", replies[2]);
    assert_eq!(png_file_dimensions(&target), Ok((8, 8)));
}

// ------------------------------------------------------------------ running the encoder

#[cfg(unix)]
mod encoder_process {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    /// Writing an executable and exec'ing it while another test forks can hit ETXTBSY; serialise.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A `#!/bin/sh` stand-in for ffmpeg. `$DIR` is the temp directory; `body` runs after the
    /// argument vector (one argument per line) is saved to `$DIR/args.txt`.
    fn stub(dir: &Path, body: &str) -> String {
        let path = dir.join("fake-ffmpeg");
        let script = format!(
            "#!/bin/sh\nDIR='{}'\nprintf '%s\\n' \"$@\" > \"$DIR/args.txt\"\necho $$ > \"$DIR/pid.txt\"\n{body}\n",
            dir.display()
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    const COUNT_STDIN: &str = "wc -c | tr -d ' ' > \"$DIR/bytes.txt\"";

    fn lines(path: PathBuf) -> Vec<String> {
        fs::read_to_string(path).unwrap().lines().map(str::to_owned).collect()
    }

    /// Waits until the stub shell has recorded its pid. Frames can sit in the pipe before the shell runs, so a kill
    /// right after a write can come first, and then `assert_gone` has no pid to look for.
    fn wait_for_stub(dir: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        // The redirection creates the file before `echo` fills it, so wait for the number.
        while fs::read_to_string(dir.join("pid.txt")).map_or(true, |pid| pid.trim().parse::<u32>().is_err()) {
            assert!(Instant::now() < deadline, "the stub did not record its pid within five seconds");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn assert_gone(dir: &Path) {
        let pid = fs::read_to_string(dir.join("pid.txt")).unwrap().trim().to_owned();
        // `kill -0` succeeds for zombies too, so a failure proves the child was killed and reaped.
        let alive = std::process::Command::new("kill").args(["-0", &pid]).stderr(std::process::Stdio::null()).status();
        assert!(!alive.unwrap().success(), "process {pid} is still around");
    }

    fn small(dir: &Path, codec: &str, file: &str, frames: u32) -> EncoderSettings {
        EncoderSettings {
            codec: codec.into(),
            framerate: 4,
            file: dir.join(file).to_string_lossy().into_owned(),
            frames,
            width: 8,
            height: 8,
            transparency: false,
            ffmpeg_options: String::new(),
            audio: Vec::new(),
        }
    }

    fn no_cancel() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[test]
    fn shared_codec_probe_is_nonblocking_captures_program_and_survives_new() {
        let _serial = serial();
        let dir = temp_dir();
        let program = stub(&dir, "while [ ! -f \"$DIR/release\" ]; do sleep 0.01; done\nprintf 'Codecs:\\n -------\\n DEV.LS h264                 H.264 / AVC\\n'");
        let mut session = open_fixture(true);
        session.set_video_encoder(&program);
        let before = session.snapshot();
        let history = session.handle_request(&json!({"op":"history.summary"}));
        let started = Instant::now();
        let reply = op_ok(&mut session, json!({"op":"export.video.probe"}));
        assert_eq!(reply["state"], "running");
        assert!(started.elapsed() < Duration::from_secs(2));
        wait_for_stub(&dir);
        assert_eq!(lines(dir.join("args.txt")), ["-codecs"]);
        assert_eq!(session.handle_request(&json!({"op":"export.video.probe"}))["succeeded"], false);
        assert_eq!(session.snapshot(), before);
        assert_eq!(session.handle_request(&json!({"op":"history.summary"})), history);
        session.set_video_encoder(dir.join("missing").to_str().unwrap());
        op_ok(&mut session, json!({"op":"new"}));
        let after_new = session.snapshot();
        fs::write(dir.join("release"), b"release").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            let response = session.handle_mcp(&json!({"jsonrpc":"2.0","id":1,"method":"resources/read",
                "params":{"uri":"resource://soul2d/export/video-codecs"}})).unwrap();
            let status: Value = serde_json::from_str(response["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
            if status["state"] != "running" { break status; }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status["state"], "finished", "{status}");
        assert_eq!(status["program"], program);
        assert_eq!(status["codecs"][1]["tag"], "h264");
        assert_eq!(op_ok(&mut session, json!({"op":"export.video.probe.status"})), status);
        assert_eq!(session.snapshot(), after_new);
        assert_gone(&dir);
        op_ok(&mut session, json!({"op":"export.video.probe"}));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let failed = op_ok(&mut session, json!({"op":"export.video.probe.status"}));
            if failed["state"] != "running" {
                assert_eq!(failed["state"], "failed");
                assert_eq!(failed["program"], dir.join("missing").to_str().unwrap());
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn export_video_streams_every_frame_and_reports_progress() {
        let _serial = serial();
        let mut session = open_fixture(true);
        let dir = temp_dir();
        let output = VideoOutput {
            program: stub(&dir, COUNT_STDIN),
            codec: "mpeg4".into(),
            file: dir.join("clip.mp4").to_string_lossy().into_owned(),
            ffmpeg_options: "-b:v 1M".into(),
            audio: Vec::new(),
        };
        let options = VideoExportOptions { overwrite: true, ..Default::default() };
        let mut progress = Vec::new();
        let plan = export_video(&mut session, &options, &output, no_cancel(), &mut |done, total| {
            progress.push((done, total))
        })
        .unwrap();
        assert_eq!(plan.frames(), 4);
        assert_eq!(progress, [(1, 4), (2, 4), (3, 4), (4, 4)]);
        assert_eq!(lines(dir.join("bytes.txt")), ["1024"], "8*8*4 bytes times four frames");
        let file = output.file.clone();
        assert_eq!(
            lines(dir.join("args.txt")),
            [
                "-y", "-hide_banner", "-loglevel", "error", "-f", "rawvideo", "-vcodec", "rawvideo", "-pix_fmt", "rgba", "-s", "8x8", "-r", "4", "-i", "-",
                "-vframes", "4", "-an", "-pix_fmt", "yuv420p", "-vcodec", "mpeg4", "-b:v", "1M", file.as_str(),
            ]
        );
        // The preview is restored and the document untouched.
        assert_eq!(session.snapshot()["animation"], Value::Null);
    }

    #[test]
    fn export_video_refuses_existing_output_without_overwrite() {
        let _serial = serial();
        let mut session = open_fixture(true);
        let dir = temp_dir();
        let existing = dir.join("clip.mp4");
        fs::write(&existing, b"keep me").unwrap();
        let output = VideoOutput {
            program: stub(&dir, COUNT_STDIN),
            file: existing.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let error = export_video(&mut session, &Default::default(), &output, no_cancel(), &mut |_, _| {});
        assert_eq!(error, Err(format!("{} already exists", existing.display())));
        assert!(!dir.join("args.txt").exists(), "the encoder must not even start");
        assert_eq!(fs::read(&existing).unwrap(), b"keep me");

        // Image codecs write numbered files; any existing frame file counts.
        let frame = dir.join("clip-3.png");
        fs::write(&frame, b"keep me too").unwrap();
        let sequence = VideoOutput { codec: "png".into(), file: dir.join("clip.png").to_string_lossy().into_owned(), ..output };
        let error = export_video(&mut session, &Default::default(), &sequence, no_cancel(), &mut |_, _| {});
        assert_eq!(error, Err(format!("{} already exists", frame.display())));
        assert!(!dir.join("args.txt").exists());
    }

    #[test]
    fn without_overwrite_the_encoder_gets_dash_n_instead_of_dash_y() {
        let _serial = serial();
        let dir = temp_dir();
        let settings = small(&dir, "auto", "fresh.mp4", 1);
        let program = stub(&dir, COUNT_STDIN);
        let mut encoder = Encoder::spawn(&program, &settings, false, no_cancel()).unwrap();
        encoder.write_frame(&[0u8; 256]).unwrap();
        encoder.finish().unwrap();
        assert_eq!(lines(dir.join("args.txt"))[0], "-n");
        assert_eq!(lines(dir.join("bytes.txt")), ["256"]);
    }

    #[test]
    fn write_frame_rejects_wrong_sizes_and_missing_programs_fail_to_start() {
        let _serial = serial();
        let dir = temp_dir();
        let settings = small(&dir, "auto", "a.mp4", 1);
        let program = stub(&dir, COUNT_STDIN);
        let mut encoder = Encoder::spawn(&program, &settings, true, no_cancel()).unwrap();
        assert_eq!(encoder.write_frame(&[0u8; 255]), Err("frame has 255 bytes, expected 256".into()));
        encoder.write_frame(&[0u8; 256]).unwrap();
        encoder.finish().unwrap();
        assert_eq!(lines(dir.join("bytes.txt")), ["256"]);

        let missing = dir.join("does-not-exist").to_string_lossy().into_owned();
        let error = Encoder::spawn(&missing, &settings, true, no_cancel()).err().unwrap();
        assert!(error.starts_with(&format!("cannot start {missing}: ")), "{error}");
        let mut bad = settings.clone();
        bad.codec = "x y".into();
        assert!(Encoder::spawn(&program, &bad, true, no_cancel()).is_err());
    }

    #[test]
    fn nonzero_exit_returns_the_stderr_tail() {
        let _serial = serial();
        let dir = temp_dir();
        let settings = small(&dir, "auto", "a.mp4", 2);
        let program = stub(&dir, "cat > /dev/null\necho 'Unknown encoder x264' >&2\necho 'conversion failed' >&2\nexit 3");
        let mut encoder = Encoder::spawn(&program, &settings, true, no_cancel()).unwrap();
        encoder.write_frame(&[0u8; 256]).unwrap();
        encoder.write_frame(&[0u8; 256]).unwrap();
        assert_eq!(
            encoder.finish(),
            Err("ffmpeg exited with exit status: 3: Unknown encoder x264\nconversion failed".into())
        );
        assert_gone(&dir);
    }

    #[test]
    fn stderr_tail_is_bounded_to_the_last_4096_bytes() {
        let _serial = serial();
        let dir = temp_dir();
        let settings = small(&dir, "auto", "a.mp4", 1);
        let program = stub(
            &dir,
            "cat > /dev/null\nyes AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA | head -n 1000 >&2\necho final-line >&2\nexit 1",
        );
        let mut encoder = Encoder::spawn(&program, &settings, true, no_cancel()).unwrap();
        encoder.write_frame(&[0u8; 256]).unwrap();
        let error = encoder.finish().unwrap_err();
        let tail = error.strip_prefix("ffmpeg exited with exit status: 1: ").unwrap();
        assert!(tail.len() <= 4096, "tail is {} bytes", tail.len());
        assert!(tail.ends_with("A\nfinal-line"), "{tail}");
        assert!(tail.len() > 4000);
    }

    #[test]
    fn an_encoder_that_dies_early_fails_the_export_with_its_message() {
        let _serial = serial();
        let mut session = open_fixture(true);
        let dir = temp_dir();
        // Frames are 1 KiB in total, so whether the write sees EPIPE or the exit is noticed afterwards
        // depends on timing; both must carry the stderr text.
        let output = VideoOutput {
            program: stub(&dir, "echo 'Invalid pixel format' >&2\nexit 2"),
            file: dir.join("clip.mp4").to_string_lossy().into_owned(),
            ..Default::default()
        };
        let error = export_video(&mut session, &Default::default(), &output, no_cancel(), &mut |_, _| {}).unwrap_err();
        assert!(error.ends_with(": Invalid pixel format"), "{error}");
        assert_gone(&dir);
        assert_eq!(session.snapshot()["animation"], Value::Null, "preview restored after the failure");
    }

    #[test]
    fn cancelling_a_polite_encoder_lets_it_finalise_without_a_kill() {
        let _serial = serial();
        let mut session = open_fixture(true);
        let dir = temp_dir();
        let output = VideoOutput {
            program: stub(&dir, COUNT_STDIN),
            file: dir.join("clip.mp4").to_string_lossy().into_owned(),
            ..Default::default()
        };
        let cancel = no_cancel();
        let raise = Arc::clone(&cancel);
        let options = VideoExportOptions { overwrite: true, ..Default::default() };
        let started = Instant::now();
        let mut progress = Vec::new();
        let result = export_video(&mut session, &options, &output, cancel, &mut |done, total| {
            progress.push((done, total));
            if done == 2 {
                raise.store(true, Ordering::Relaxed);
            }
        });
        assert_eq!(result, Err(EXPORT_CANCELLED.to_string()));
        assert_eq!(progress, [(1, 4), (2, 4)], "no frame is rendered after the flag is seen");
        assert!(started.elapsed() < Duration::from_millis(450), "took {:?}", started.elapsed());
        assert_eq!(lines(dir.join("bytes.txt")), ["512"], "the two queued frames were delivered, then EOF");
        assert_gone(&dir);
        assert_eq!(session.snapshot()["animation"], Value::Null);
    }

    #[test]
    fn cancelling_a_stuck_encoder_kills_and_reaps_it_promptly() {
        let _serial = serial();
        let dir = temp_dir();
        // 512x512 frames (1 MiB) fill the pipe and the queue, so write_frame must block on the child.
        let mut settings = small(&dir, "auto", "stuck.mp4", 50);
        settings.width = 512;
        settings.height = 512;
        let program = stub(&dir, "exec sleep 30");
        let cancel = no_cancel();
        let mut encoder = Encoder::spawn(&program, &settings, true, Arc::clone(&cancel)).unwrap();
        wait_for_stub(&dir);
        let raise = Arc::clone(&cancel);
        let flipper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            raise.store(true, Ordering::Relaxed);
        });
        let frame = vec![7u8; 512 * 512 * 4];
        let started = Instant::now();
        let outcome = (0..50).map(|_| encoder.write_frame(&frame)).find(Result::is_err);
        assert_eq!(outcome, Some(Err(EXPORT_CANCELLED.to_string())));
        assert!(started.elapsed() < Duration::from_secs(2), "blocked for {:?}", started.elapsed());
        let message = encoder.abort(EXPORT_CANCELLED.into());
        assert_eq!(message, EXPORT_CANCELLED);
        assert!(started.elapsed() < Duration::from_secs(3), "cancel took {:?}", started.elapsed());
        flipper.join().unwrap();
        assert_gone(&dir);
    }

    #[test]
    fn dropping_an_encoder_kills_and_reaps_the_child() {
        let _serial = serial();
        let dir = temp_dir();
        let program = stub(&dir, "exec sleep 30");
        let encoder = Encoder::spawn(&program, &small(&dir, "auto", "d.mp4", 1), true, no_cancel()).unwrap();
        wait_for_stub(&dir);
        drop(encoder);
        assert_gone(&dir);
    }

    #[test]
    fn probe_runs_dash_codecs_and_parses_the_encoder_list() {
        let _serial = serial();
        let dir = temp_dir();
        let listing = " -------\n DEV.LS h264                 H.264 / AVC\n .EV.L. mpeg4                MPEG-4 part 2\n D.V.L. gif                  GIF";
        let program = stub(&dir, &format!("cat <<'EOF'\nCodecs:\n{listing}\nEOF"));
        let codecs = probe_encoders(&program, Duration::from_secs(5)).unwrap();
        assert_eq!(lines(dir.join("args.txt")), ["-codecs"]);
        let tags: Vec<&str> = codecs.iter().map(|c| c.tag.as_str()).collect();
        assert_eq!(tags, ["auto", "h264", "mpeg4"]);
        assert_eq!(codecs[1], VideoCodec { tag: "h264".into(), name: "H.264 / AVC".into() });
    }

    #[test]
    fn probe_reports_failures_and_unrecognised_output() {
        let _serial = serial();
        let dir = temp_dir();
        let failing = stub(&dir, "exit 4");
        assert_eq!(
            probe_encoders(&failing, Duration::from_secs(5)),
            Err(format!("{failing} -codecs exited with exit status: 4"))
        );
        let garbage = stub(&dir, "echo not an ffmpeg");
        assert_eq!(
            probe_encoders(&garbage, Duration::from_secs(5)),
            Err("ffmpeg codec list was not recognised".into())
        );
        let missing = dir.join("nope").to_string_lossy().into_owned();
        assert!(probe_encoders(&missing, Duration::from_secs(5)).unwrap_err().starts_with("cannot start"));
    }

    #[test]
    fn probe_times_out_instead_of_blocking_forever() {
        let _serial = serial();
        let dir = temp_dir();
        let program = stub(&dir, "exec sleep 30");
        let started = Instant::now();
        // Long enough for the stub shell to record its pid even when the machine is busy.
        let error = probe_encoders(&program, Duration::from_millis(1500)).unwrap_err();
        assert_eq!(error, format!("{program} -codecs did not finish within 1500 ms"));
        assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
        assert_gone(&dir);
    }

    #[test]
    fn probe_survives_output_larger_than_a_pipe_buffer() {
        let _serial = serial();
        let dir = temp_dir();
        let program = stub(&dir, "echo ' -------'\nyes ' .EV.L. filler               Filler codec' | head -n 5000");
        let codecs = probe_encoders(&program, Duration::from_secs(10)).unwrap();
        assert_eq!(codecs.len(), 5001);
    }

    /// One real encode: `cargo test --test export -- --ignored real_ffmpeg`. `SOUL2D_FFMPEG` and
    /// `SOUL2D_FFPROBE` override the program names.
    #[test]
    #[ignore = "needs real ffmpeg and ffprobe"]
    fn real_ffmpeg_encodes_a_video_that_ffprobe_can_read() {
        let _serial = serial();
        let mut session = open_fixture(true);
        let dir = temp_dir();
        let output = VideoOutput {
            program: std::env::var("SOUL2D_FFMPEG").unwrap_or_else(|_| "ffmpeg".into()),
            codec: "auto".into(),
            file: dir.join("clip.mp4").to_string_lossy().into_owned(),
            ffmpeg_options: String::new(),
            audio: Vec::new(),
        };
        let options = VideoExportOptions { overwrite: true, ..Default::default() };
        let plan = export_video(&mut session, &options, &output, no_cancel(), &mut |_, _| {}).unwrap();
        assert_eq!(plan.frames(), 4);
        let probe = std::env::var("SOUL2D_FFPROBE").unwrap_or_else(|_| "ffprobe".into());
        let probed = std::process::Command::new(probe)
            .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries"])
            .args(["stream=width,height,nb_read_frames", "-of", "csv=p=0", &output.file])
            .output()
            .unwrap();
        assert!(probed.status.success(), "{}", String::from_utf8_lossy(&probed.stderr));
        assert_eq!(String::from_utf8_lossy(&probed.stdout).trim(), "8,8,4");
    }

    /// A sound muxed into a real encode: `cargo test --test export -- --ignored real_ffmpeg`.
    #[test]
    #[ignore = "needs real ffmpeg and ffprobe"]
    fn real_ffmpeg_muxes_audio_to_the_length_of_the_video() {
        let _serial = serial();
        let ffmpeg = std::env::var("SOUL2D_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
        let probe = std::env::var("SOUL2D_FFPROBE").unwrap_or_else(|_| "ffprobe".into());
        let dir = temp_dir();
        let mut session = open_fixture(true);
        // The 1 s clip with a sound shorter (padded) and longer (cut) than it, and with a host `-af` that drops the
        // padding.
        // The sound's own length is read where the container records it (MP4).
        let cases = [
            ("0.5", VideoPreset::Mp4, "", "h264,4", "aac,", Some(1.0)),
            ("3", VideoPreset::Webm, "", "vp9,4", "opus,", None),
            ("0.5", VideoPreset::Mp4, " -af volume=0.5", "h264,4", "aac,", Some(0.5)),
        ];
        for (seconds, preset, host, video, audio, sound) in cases {
            let tone = dir.join(format!("tone-{seconds}.wav"));
            let made = std::process::Command::new(&ffmpeg)
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("sine=duration={seconds}")])
                .arg(&tone)
                .status()
                .unwrap();
            assert!(made.success());
            let (codec, options) = preset.encoder(false);
            let file = dir.join(format!("voiced.{}", preset.extension())).to_string_lossy().into_owned();
            let output = VideoOutput {
                program: ffmpeg.clone(),
                codec: codec.into(),
                file: file.clone(),
                ffmpeg_options: format!("{options}{host}"),
                audio: vec![AudioInput { path: tone.to_string_lossy().into_owned(), delay_ms: 0 }],
            };
            let options = VideoExportOptions { overwrite: true, ..Default::default() };
            export_video(&mut session, &options, &output, no_cancel(), &mut |_, _| {}).unwrap();
            let read = |entries: &str| {
                let out = std::process::Command::new(&probe)
                    .args(["-v", "error", "-count_frames", "-show_entries", entries, "-of", "csv=p=0", &file])
                    .output()
                    .unwrap();
                String::from_utf8_lossy(&out.stdout).trim().to_owned()
            };
            let streams = read("stream=codec_name,nb_read_frames");
            let streams: Vec<&str> = streams.lines().collect();
            assert_eq!(streams.len(), 2, "{file}: {streams:?}");
            assert_eq!(streams[0], video, "{file}: every frame of the video is kept");
            assert!(streams[1].starts_with(audio), "{file}: {streams:?}");
            let duration: f64 = read("format=duration").parse().unwrap();
            assert!((0.95..1.05).contains(&duration), "{file}: {duration} s");
            if let Some(sound) = sound {
                let length: f64 = read("stream=duration").lines().nth(1).unwrap().parse().unwrap();
                assert!((length - sound).abs() < 0.05, "{file}: the sound lasts {length} s, not {sound} s");
            }
        }
    }

    /// A clip's audio tracks in a real encode, decoded again: `cargo test --test export -- --ignored real_ffmpeg`.
    #[test]
    #[ignore = "needs real ffmpeg"]
    fn real_ffmpeg_starts_each_audio_track_at_its_frame_at_full_volume() {
        let _serial = serial();
        let ffmpeg = std::env::var("SOUL2D_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
        let dir = temp_dir();
        let mut session = open_fixture(true);
        let beep = dir.join("beep.wav");
        let made = std::process::Command::new(&ffmpeg)
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=1000:duration=0.2"])
            .arg(&beep)
            .status()
            .unwrap();
        assert!(made.success());
        let beep = beep.to_string_lossy().into_owned();
        // The 1 s clip steps 0.25 s a frame: a beep at frame 0, one at frame 2 (0.5 s) and a muted one at frame 1.
        for (frame, mute) in [(0, false), (2, false), (1, true)] {
            let add = json!({"op": "animation.audio.add", "name": "Idle", "path": beep, "frame": frame});
            let added = session.handle_request(&add);
            assert_eq!(added["succeeded"], true, "{added}");
            if mute {
                let mute = json!({"op": "animation.audio.set", "name": "Idle", "index": 2, "mute": true});
                let muted = session.handle_request(&mute);
                assert_eq!(muted["succeeded"], true, "{muted}");
            }
        }
        let file = dir.join("beeps.mp4").to_string_lossy().into_owned();
        let options = VideoExportOptions { overwrite: true, ..Default::default() };
        let plan = plan_video(&mut session, &options).unwrap();
        let (audio, left_out) = soul2d_core::export::session_audio(&session, &plan, "auto", &file);
        assert!(left_out.is_empty(), "{left_out:?}");
        assert_eq!(audio.iter().map(|input| input.delay_ms).collect::<Vec<_>>(), [0, 500]);
        let output = VideoOutput {
            program: ffmpeg.clone(), codec: "auto".into(), file: file.clone(), ffmpeg_options: String::new(), audio,
        };
        export_video(&mut session, &options, &output, no_cancel(), &mut |_, _| {}).unwrap();
        let decoded = std::process::Command::new(&ffmpeg)
            .args(["-v", "error", "-i", &file, "-f", "s16le", "-ac", "1", "-ar", "8000", "-"])
            .output()
            .unwrap();
        assert!(decoded.status.success(), "{}", String::from_utf8_lossy(&decoded.stderr));
        let samples: Vec<i32> =
            decoded.stdout.chunks_exact(2).map(|pair| i32::from(i16::from_le_bytes([pair[0], pair[1]]))).collect();
        let peak = |from: f64, to: f64| {
            samples[(from * 8000.0) as usize..(to * 8000.0) as usize].iter().map(|s| s.abs()).max().unwrap()
        };
        let alone = peak(0.05, 0.15);
        // A sine of amplitude 1/8 peaks near 4096; amix would halve two inputs without normalize=0.
        assert!((3000..5000).contains(&alone), "the first beep peaks at {alone}");
        assert!(peak(0.3, 0.45) < alone / 10, "silence between the beeps, where the muted one would play");
        let second = peak(0.55, 0.65);
        assert!((3000..5000).contains(&second), "the second beep peaks at {second}");
        let onset = samples.iter().skip(4000 - 400).position(|s| s.abs() > alone / 4).unwrap() as f64 / 8000.0 + 0.45;
        assert!((onset - 0.5).abs() < 0.05, "the second beep starts {onset} s in, not 0.5 s");
    }

    /// Each preset through a real encode, decoded again: `cargo test --test export -- --ignored real_ffmpeg`.
    #[test]
    #[ignore = "needs real ffmpeg and ffprobe"]
    fn real_ffmpeg_decodes_every_video_preset() {
        let _serial = serial();
        let ffmpeg = std::env::var("SOUL2D_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
        let probe = std::env::var("SOUL2D_FFPROBE").unwrap_or_else(|_| "ffprobe".into());
        let dir = temp_dir();
        for (preset, transparency, probed) in [
            (VideoPreset::Mp4, false, "h264,8,8,4"),
            (VideoPreset::Webm, true, "vp9,8,8,4,1"),
            (VideoPreset::Mov, true, "prores,8,8,4"),
            (VideoPreset::Mov, false, "prores,8,8,4"),
        ] {
            let mut session = open_fixture(true);
            let (codec, options) = preset.encoder(transparency);
            let file = dir.join(format!("clip-{transparency}.{}", preset.extension())).to_string_lossy().into_owned();
            let output = VideoOutput {
                program: ffmpeg.clone(),
                codec: codec.into(),
                file: file.clone(),
                ffmpeg_options: options.into(),
                audio: Vec::new(),
            };
            let options = VideoExportOptions { overwrite: true, transparency, ..Default::default() };
            export_video(&mut session, &options, &output, no_cancel(), &mut |_, _| {}).unwrap();
            let read = std::process::Command::new(&probe)
                .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries"])
                .args(["stream=codec_name,width,height,nb_read_frames:stream_tags=alpha_mode", "-of", "csv=p=0", &file])
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&read.stdout).trim(), probed, "{file}");
            // The first frame decoded to RGBA: the fixture leaves the corners empty.
            let decoder: &[&str] = if preset == VideoPreset::Webm { &["-c:v", "libvpx-vp9"] } else { &[] };
            let frame = std::process::Command::new(&ffmpeg)
                .args(["-v", "error"])
                .args(decoder)
                .args(["-i", &file, "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
                .output()
                .unwrap();
            assert_eq!(frame.stdout.len(), 8 * 8 * 4, "{file}");
            let alpha = frame.stdout.iter().skip(3).step_by(4).copied();
            let least = if transparency { 0 } else { 255 };
            assert_eq!((alpha.clone().min(), alpha.max()), (Some(least), Some(255)), "{file}");
        }
    }

    // -------------------------------------------------------------- video jobs through the session

    /// Polls `export.video.status` until the job leaves `running` (the encoders here are stubs).
    fn settle(session: &mut Session, job: u64) -> Value {
        let started = Instant::now();
        loop {
            let status = op_ok(session, json!({"op": "export.video.status", "job": job}));
            if status["state"] != "running" {
                return status;
            }
            assert!(started.elapsed() < Duration::from_secs(20), "job {job} is still running: {status}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn shared_video_command_matches_launch_and_remains_available_during_a_job() {
        let _serial = serial();
        let dir = temp_dir();
        let program = stub(&dir, "cat > /dev/null\nwhile [ ! -f \"$DIR/release\" ]; do sleep 0.01; done");
        let mut session = open_fixture(true);
        session.set_video_encoder(&program);
        session.set_video_ffmpeg_options("-crf 19");
        let before = session.snapshot();
        let mut request = json!({"op":"export.video.command","path":dir.join("clip.mp4"),"preset":"mp4"});
        let preview = op_ok(&mut session, request.clone());
        assert!(!dir.join("pid.txt").exists());
        request["op"] = json!("export.video.start");
        let started = op_ok(&mut session, request.clone());
        wait_for_stub(&dir);
        let args: Vec<String> = serde_json::from_value(preview["args"].clone()).unwrap();
        assert_eq!(lines(dir.join("args.txt")), args);
        assert_eq!(session.handle_request(&request)["succeeded"], false);
        request["op"] = json!("export.video.command");
        assert_eq!(op_ok(&mut session, request), preview);
        assert_eq!(session.snapshot(), before);
        fs::write(dir.join("release"), b"release").unwrap();
        assert_eq!(settle(&mut session, started["job"].as_u64().unwrap())["state"], "finished");
    }

    #[test]
    fn a_video_job_runs_to_the_end_and_reports_its_progress() {
        let _serial = serial();
        let mut session = open_fixture(true);
        let before = session.snapshot();
        let dir = temp_dir();
        session.set_video_encoder(&stub(&dir, COUNT_STDIN));
        session.set_video_ffmpeg_options("  -b:v 1M -vf scale=iw/2:ih/2 ");
        let output = dir.join("clip.mp4");
        let started = op_ok(&mut session, json!({
            "op": "export.video.start", "path": output.to_str().unwrap(), "codec": "mpeg4", "overwrite": true,
        }));
        assert_eq!(started["frames"], 4);
        assert_eq!((started["width"].as_u64(), started["height"].as_u64(), started["fps"].as_u64()), (Some(8), Some(8), Some(4)));
        let job = started["job"].as_u64().unwrap();
        let done = settle(&mut session, job);
        assert_eq!(
            done,
            json!({"job": job, "state": "finished", "path": output.to_str().unwrap(), "framesDone": 4, "framesTotal": 4, "frames": 4})
        );
        assert_eq!(op_ok(&mut session, json!({"op": "export.video.status", "job": job})), done, "final states are stable");
        assert_eq!(lines(dir.join("bytes.txt")), ["1024"], "8*8*4 bytes times four frames");
        let args = lines(dir.join("args.txt"));
        assert_eq!(args[0], "-y");
        // ffmpeg gets the canonical absolute target, and the host's options, never anything from the request.
        assert_eq!(args.last().unwrap().as_str(), fs::canonicalize(&dir).unwrap().join("clip.mp4").to_str().unwrap());
        let position = args.iter().position(|arg| arg == "-b:v").unwrap();
        assert_eq!(args[position..position + 4], ["-b:v", "1M", "-vf", "scale=iw/2:ih/2"]);
        assert_session_untouched(&before, &session.snapshot());
        assert_eq!(session.snapshot()["animation"], Value::Null, "the preview is the session's own, untouched");
        // A finished job does not block the next one, and ids are not reused.
        let next = op_ok(&mut session, json!({"op": "export.video.start", "path": output.to_str().unwrap(), "overwrite": true}));
        assert_ne!(next["job"].as_u64().unwrap(), job);
        let next = next["job"].as_u64().unwrap();
        settle(&mut session, next);
    }

    #[test]
    fn a_video_job_mixes_the_clip_audio_tracks_unless_the_request_names_a_sound() {
        let _serial = serial();
        let mut session = open_fixture(true);
        let dir = temp_dir();
        session.set_video_encoder(&stub(&dir, COUNT_STDIN));
        let wav = |name: &str| {
            let path = dir.join(name);
            fs::write(&path, b"RIFF\x24\0\0\0WAVEfmt ").unwrap();
            fs::canonicalize(path).unwrap().to_string_lossy().into_owned()
        };
        let (voice, music, quiet) = (wav("voice.wav"), wav("music.wav"), wav("quiet.wav"));
        // The clip steps 0.25 s a frame: music at frame 2 starts 500 ms in.
        for (path, frame) in [(&voice, 0), (&music, 2), (&quiet, 1)] {
            op_ok(&mut session, json!({"op": "animation.audio.add", "name": "Idle", "path": path, "frame": frame}));
        }
        op_ok(&mut session, json!({"op": "animation.audio.set", "name": "Idle", "index": 2, "mute": true}));
        let output = dir.join("clip.mp4");
        let run = |session: &mut Session, extra: Value| {
            let mut request = json!({"op": "export.video.start", "path": output.to_str().unwrap(), "overwrite": true});
            request.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            let job = op_ok(session, request)["job"].as_u64().unwrap();
            assert_eq!(settle(session, job)["state"], "finished");
            lines(dir.join("args.txt"))
        };
        let args = run(&mut session, json!({"codec": "mpeg4"}));
        let inputs: Vec<&String> = args.windows(2).filter(|pair| pair[0] == "-i").map(|pair| &pair[1]).collect();
        assert_eq!(inputs, ["-", &voice, &music], "the muted track is left out");
        let graph = &args[args.iter().position(|arg| arg == "-filter_complex").unwrap() + 1];
        assert!(graph.starts_with("[1:a:0]anull[a0];[2:a:0]adelay=500:all=1,anull[a1];"), "{graph}");

        // A sound in the request replaces the tracks.
        let other = wav("other.wav");
        let args = run(&mut session, json!({"codec": "mpeg4", "audio": other}));
        let inputs: Vec<&String> = args.windows(2).filter(|pair| pair[0] == "-i").map(|pair| &pair[1]).collect();
        assert_eq!(inputs, ["-", &other]);
        assert!(args.windows(2).any(|pair| pair == ["-af", "apad"]));
        let odd = wav("take-%d.wav");
        let start = json!({"op": "export.video.start", "path": output.to_str().unwrap(), "overwrite": true, "audio": odd});
        assert_eq!(op(&mut session, start)["message"], format!("audio file {odd} must not contain '%'"));

        // An image sequence has no sound, and the tracks do not stop it.
        let args = run(&mut session, json!({"codec": "png"}));
        assert!(args.contains(&"-an".to_owned()) && !args.contains(&voice), "{args:?}");

        // A track whose file is gone is left out and named in the reply; the rest still play. So is an image track.
        fs::remove_file(&music).unwrap();
        let picture = dir.join("bg.png");
        image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255])).save(&picture).unwrap();
        op_ok(&mut session, json!({"op": "animation.image.add", "name": "Idle", "path": picture}));
        fs::remove_file(&picture).unwrap();
        let start = json!({"op": "export.video.start", "path": output.to_str().unwrap(), "overwrite": true});
        let started = op_ok(&mut session, start);
        assert_eq!(started["audioLeftOut"], json!([format!("audio file {music} was not found")]));
        let picture = fs::canonicalize(&dir).unwrap().join("bg.png");
        assert_eq!(started["imagesLeftOut"], json!([format!("image file {} was not found", picture.display())]));
        assert_eq!(settle(&mut session, started["job"].as_u64().unwrap())["state"], "finished");
        let args = lines(dir.join("args.txt"));
        let inputs: Vec<&String> = args.windows(2).filter(|pair| pair[0] == "-i").map(|pair| &pair[1]).collect();
        assert_eq!(inputs, ["-", &voice]);
    }

    #[test]
    fn a_running_job_blocks_a_second_start_and_can_be_cancelled_across_new() {
        let _serial = serial();
        let mut session = open_fixture(true);
        let dir = temp_dir();
        session.set_video_encoder(&stub(&dir, "cat > /dev/null\nexec sleep 30"));
        let output = dir.join("clip.mp4");
        let start = json!({"op": "export.video.start", "path": output.to_str().unwrap(), "overwrite": true});
        let job = op_ok(&mut session, start.clone())["job"].as_u64().unwrap();
        let second = op(&mut session, start);
        assert_eq!(second["succeeded"], false);
        assert_eq!(second["message"], "a video export is already running");
        // Replacing the document keeps the job reachable; it works on its own copy.
        op_ok(&mut session, json!({"op": "new"}));
        let status = loop {
            let status = op_ok(&mut session, json!({"op": "export.video.status", "job": job}));
            if status["framesDone"] == 4 {
                break status;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status["state"], "running");
        wait_for_stub(&dir);
        let asked = Instant::now();
        op_ok(&mut session, json!({"op": "export.video.cancel", "job": job}));
        let done = settle(&mut session, job);
        assert_eq!((done["state"].as_str(), done["partialFileKept"].as_bool()), (Some("cancelled"), Some(true)));
        assert!(asked.elapsed() < Duration::from_secs(5), "took {:?}", asked.elapsed());
        assert_gone(&dir);
        // Cancelling a finished job is harmless.
        assert_eq!(op_ok(&mut session, json!({"op": "export.video.cancel", "job": job}))["state"], "cancelled");
    }

    #[test]
    fn a_failing_or_missing_encoder_fails_the_job_with_a_clean_message() {
        let _serial = serial();
        let mut session = open_fixture(true);
        let dir = temp_dir();
        let output = dir.join("clip.mp4");
        let start = json!({"op": "export.video.start", "path": output.to_str().unwrap(), "overwrite": true});

        session.set_video_encoder(&stub(&dir, "cat > /dev/null\necho 'Unknown encoder boom' >&2\nexit 3"));
        let job = op_ok(&mut session, start.clone())["job"].as_u64().unwrap();
        let failed = settle(&mut session, job);
        assert_eq!(failed["state"], "failed");
        assert_eq!(failed["error"], "ffmpeg exited with exit status: 3: Unknown encoder boom");

        let missing = dir.join("no-such-ffmpeg");
        session.set_video_encoder(missing.to_str().unwrap());
        let job = op_ok(&mut session, start)["job"].as_u64().unwrap();
        let failed = settle(&mut session, job);
        let error = failed["error"].as_str().unwrap();
        assert_eq!(failed["state"], "failed");
        assert!(error.starts_with(&format!("cannot start {}: ", missing.display())), "{error}");
        assert!(!output.exists());
    }

    #[test]
    fn video_start_refuses_bad_requests_before_anything_runs() {
        let _serial = serial();
        let mut session = open_fixture(true);
        let dir = temp_dir();
        session.set_video_encoder(&stub(&dir, COUNT_STDIN));
        let output = dir.join("clip.mp4");
        let path = output.to_str().unwrap();
        let tone = dir.join("tone.wav");
        fs::write(&tone, b"RIFF").unwrap();
        for (request, needle) in [
            (json!({"op": "export.video.start"}), "path must be a string"),
            (json!({"op": "export.video.start", "path": format!("{}/", dir.display())}), "path must name a file"),
            (json!({"op": "export.video.start", "path": path, "codec": "x y"}), "codec must be"),
            (json!({"op": "export.video.start", "path": path, "codec": 3}), "codec must be a string"),
            (json!({"op": "export.video.start", "path": path, "preset": "avi"}), "preset must be mp4, webm or mov"),
            (json!({"op": "export.video.start", "path": path, "preset": 3}), "preset must be a string"),
            (json!({"op": "export.video.start", "path": path, "preset": "mp4", "codec": "mpeg4"}), "codec or preset"),
            (json!({"op": "export.video.start", "path": path, "preset": "mp4", "transparency": true}), "no alpha"),
            (json!({"op": "export.video.start", "path": path, "audio": 3}), "audio must be a string"),
            (json!({"op": "export.video.start", "path": path, "audio": dir.join("no.wav").to_str().unwrap()}), "not found"),
            (json!({"op": "export.video.start", "path": path, "audio": dir.to_str().unwrap()}), "not found"),
            (json!({"op": "export.video.start", "path": path, "audio": tone.to_str().unwrap(), "codec": "png"}), "no audio"),
            (json!({"op": "export.video.start", "path": path, "ffmpeg_options": "-crf 20"}), "ffmpeg_options cannot be set"),
            (json!({"op": "export.video.start", "path": path, "ffmpeg_options": "-f matroska x.inx"}), "ffmpeg_options cannot"),
            (json!({"op": "export.video.start", "path": path, "ffmpeg_options": ""}), "ffmpeg_options cannot"),
            (json!({"op": "export.video.start", "path": dir.join("shot-%d.jpg").to_str().unwrap()}), "must not contain '%'"),
            (json!({"op": "export.video.start", "path": path, "framerate": 500}), "framerate must be between"),
            (json!({"op": "export.video.start", "path": path, "animation": "Nope"}), "Animation not found: Nope"),
            (json!({"op": "export.video.start", "path": dir.join("no/clip.mp4").to_str().unwrap()}), "could not resolve parent"),
            (json!({"op": "export.video.status", "job": 99}), "unknown video export job"),
            (json!({"op": "export.video.status"}), "job must be a job id"),
            (json!({"op": "export.video.cancel", "job": "x"}), "job must be a job id"),
        ] {
            op_refused(&mut session, request, needle, &dir);
        }
        let project = dir.join("project.mp4");
        op_ok(&mut session, json!({"op": "save", "path": project.to_str().unwrap()}));
        op_refused(
            &mut session,
            json!({"op": "export.video.start", "path": project.to_str().unwrap(), "overwrite": true}),
            "would overwrite the open document",
            &dir,
        );
        assert!(!dir.join("args.txt").exists(), "the encoder never started");

        // Without overwrite an existing output fails the job and stays as it was.
        fs::write(&output, b"keep me").unwrap();
        let job = op_ok(&mut session, json!({"op": "export.video.start", "path": path}))["job"].as_u64().unwrap();
        let failed = settle(&mut session, job);
        assert_eq!(failed["error"], format!("{} already exists", fs::canonicalize(&output).unwrap().display()));
        assert_eq!(fs::read(&output).unwrap(), b"keep me");
    }

    #[test]
    fn a_video_preset_picks_the_encoder_and_the_extension() {
        let _serial = serial();
        let mut session = open_fixture(true);
        let dir = temp_dir();
        session.set_video_encoder(&stub(&dir, COUNT_STDIN));
        session.set_video_ffmpeg_options("-threads 2");
        let pad = "pad=ceil(iw/2)*2:ceil(ih/2)*2";
        for (preset, transparency, file, expected) in [
            ("mp4", false, "clip.mp4", vec!["libx264", "-crf", "18", "-movflags", "+faststart", "-vf", pad]),
            ("webm", false, "clip.webm", vec!["libvpx-vp9", "-b:v", "0", "-crf", "30"]),
            ("webm", true, "clip.webm", vec!["libvpx-vp9", "-b:v", "0", "-crf", "30"]),
            ("mov", false, "clip.mov", vec!["prores_ks", "-profile:v", "3", "-pix_fmt", "yuv422p10le", "-vf", pad]),
            ("mov", true, "clip.mov", vec![
                "prores_ks", "-profile:v", "4444", "-pix_fmt", "yuva444p10le",
                "-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2:color=black@0",
            ]),
        ] {
            let request = json!({
                "op": "export.video.start", "path": dir.join("clip.avi").to_str().unwrap(), "preset": preset,
                "transparency": transparency, "overwrite": true,
            });
            let started = op_ok(&mut session, request);
            assert_eq!(started["path"], dir.join(file).to_str().unwrap(), "{preset}");
            let done = settle(&mut session, started["job"].as_u64().unwrap());
            assert_eq!(done["path"], started["path"], "{preset}");
            let args = lines(dir.join("args.txt"));
            let at = args.iter().rposition(|arg| arg == "-vcodec").unwrap() + 1;
            // The preset's options, then the host's, then the output file.
            let mut tail = expected.clone();
            tail.extend(["-threads", "2"]);
            assert_eq!(args[at..args.len() - 1], tail, "{preset} {transparency}");
            assert!(args.last().unwrap().ends_with(file), "{preset}");
        }
        // The MCP tool takes the preset too.
        let called = session
            .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "export_video_start",
                "arguments": {"path": dir.join("tool").to_str().unwrap(), "preset": "webm", "overwrite": true}}}))
            .unwrap();
        assert_eq!(called["result"]["structuredContent"]["path"], dir.join("tool.webm").to_str().unwrap(), "{called}");
        settle(&mut session, called["result"]["structuredContent"]["job"].as_u64().unwrap());
    }

    #[test]
    fn video_start_hands_ffmpeg_the_resolved_absolute_target() {
        let _serial = serial();
        let mut session = open_fixture(true);
        let dir = temp_dir();
        session.set_video_encoder(&stub(&dir, COUNT_STDIN));
        // `file:` and `tcp:` prefixes are protocols to ffmpeg but plain relative file names to the session.
        for name in ["file:clip.mp4", "tcp:127.0.0.1:22?x=.mkv"] {
            let started = op_ok(&mut session, json!({"op": "export.video.start", "path": name, "overwrite": true}));
            settle(&mut session, started["job"].as_u64().unwrap());
            let last = lines(dir.join("args.txt")).pop().unwrap();
            let expected = fs::canonicalize(".").unwrap().join(name);
            assert_eq!(Path::new(&last), expected, "ffmpeg must not see {name:?} as written");
            assert!(Path::new(&last).is_absolute());
        }
        // So does the audio, named here through a `file:` prefix that ffmpeg would read as a protocol.
        let tone = dir.join("file:tone.wav");
        fs::write(&tone, b"RIFF").unwrap();
        fs::create_dir_all(dir.join("sub")).unwrap();
        let started = op_ok(&mut session, json!({
            "op": "export.video.start", "path": dir.join("a.mp4").to_str().unwrap(),
            "audio": dir.join("sub/../file:tone.wav").to_str().unwrap(), "overwrite": true,
        }));
        settle(&mut session, started["job"].as_u64().unwrap());
        let args = lines(dir.join("args.txt"));
        let at = args.iter().position(|arg| arg == "1:a:0").unwrap();
        assert_eq!(args[at - 4], fs::canonicalize(&tone).unwrap().to_str().unwrap());
        assert!(!args.contains(&"-an".to_owned()));
    }

    #[test]
    fn video_start_refuses_a_percent_in_the_path() {
        let _serial = serial();
        let mut session = open_fixture(true);
        let dir = temp_dir();
        session.set_video_encoder(&stub(&dir, COUNT_STDIN));
        let folder = dir.join("100%");
        fs::create_dir_all(&folder).unwrap();
        for target in [dir.join("shot-%d.jpg"), dir.join("a%d.mp4"), folder.join("clip.mp4")] {
            let path = target.to_str().unwrap();
            op_refused(&mut session, json!({"op": "export.video.start", "path": path, "codec": "png"}), "'%'", &dir);
        }
        assert!(!dir.join("args.txt").exists(), "the encoder never started");
    }

    #[test]
    fn a_failed_job_reports_at_most_the_bounded_stderr_tail() {
        let _serial = serial();
        let mut session = open_fixture(true);
        let dir = temp_dir();
        session.set_video_encoder(&stub(&dir, "cat > /dev/null\nyes BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB | head -n 1000 >&2\nexit 1"));
        let output = dir.join("clip.mp4");
        let job = op_ok(&mut session, json!({"op": "export.video.start", "path": output.to_str().unwrap(), "overwrite": true}));
        let failed = settle(&mut session, job["job"].as_u64().unwrap());
        let error = failed["error"].as_str().unwrap();
        assert!(error.len() <= "ffmpeg exited with exit status: 1: ".len() + 4096, "{} bytes", error.len());
        assert!(args_quiet(&dir), "banner and info lines are switched off");
    }

    fn args_quiet(dir: &Path) -> bool {
        let args = lines(dir.join("args.txt"));
        args[..4] == ["-y", "-hide_banner", "-loglevel", "error"]
    }

    #[test]
    fn finish_kills_an_encoder_that_ignores_end_of_input() {
        let _serial = serial();
        let dir = temp_dir();
        let settings = small(&dir, "auto", "a.mp4", 1);
        let program = stub(&dir, "exec sleep 30");
        let mut encoder = Encoder::spawn(&program, &settings, true, no_cancel()).unwrap();
        encoder.write_frame(&[0u8; 256]).unwrap();
        let started = Instant::now();
        let error = encoder.finish_within(Duration::from_millis(700)).unwrap_err();
        assert_eq!(error, "ffmpeg did not exit within 700ms after the last frame");
        assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
        assert_gone(&dir);
    }
}
