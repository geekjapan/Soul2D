use image::{Rgba, RgbaImage};
use serde_json::{json, Value};
use soul2d_core::{export, Session};
use std::{fs, path::PathBuf};

fn request(session: &mut Session, request: Value) -> Value {
    let response = session.handle_request(&request);
    assert_eq!(response["succeeded"], true, "{request}: {response}");
    response["result"].clone()
}

fn fixture(label: &str) -> (Session, PathBuf) {
    let dir = std::env::temp_dir().join(format!("soul2d-sequence-{label}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let dir = fs::canonicalize(dir).unwrap();
    for (number, color) in [
        (10, [255, 0, 0, 255]),
        (11, [0, 0, 255, 255]),
        (13, [0, 255, 0, 255]),
    ] {
        RgbaImage::from_pixel(4, 4, Rgba(color))
            .save(dir.join(format!("frame{number}.png")))
            .unwrap();
    }
    let mut session = Session::new();
    request(&mut session, json!({"op":"new"}));
    request(
        &mut session,
        json!({"op":"animation.create","name":"Idle","length":4,"timestep":0.25}),
    );
    let mut model = session.snapshot()["model"].clone();
    model["animations"]["Idle"]["length"] = json!(4);
    model["animations"]["Idle"]["timestep"] = json!(0.25);
    model["nodes"]["children"] = json!([{"uuid":99,"name":"Camera","type":"Camera","enabled":true,
        "transform":{"trans":[0,0,0],"rot":[0,0,0],"scale":[1,1]},"viewport":[4,4],"children":[]}]);
    let bytes = soul2d_core::model_import::inx_bytes(&model, &[]).unwrap();
    let path = dir.join("fixture.inx");
    fs::write(&path, bytes).unwrap();
    let mut session = Session::new();
    request(&mut session, json!({"op":"open","path":path}));
    request(
        &mut session,
        json!({"op":"animation.image.sequence","name":"Idle","directory":dir,"prefix":"frame"}),
    );
    (session, dir)
}

#[test]
fn sequence_pixels_change_hold_and_leave_gaps_while_guides_are_preview_only() {
    let (mut session, _) = fixture("pixels");
    let options = export::VideoExportOptions {
        animation: Some("Idle".into()),
        transparency: true,
        ..Default::default()
    };
    let mut colors = Vec::new();
    export::render_video_frames(&mut session, &options, &mut |_, image| {
        colors.push(image.get_pixel(1, 1).0);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        colors,
        [
            [255, 0, 0, 255],
            [0, 0, 255, 255],
            [0, 0, 0, 0],
            [0, 255, 0, 255]
        ]
    );
    request(
        &mut session,
        json!({"op":"animation.frame_step","name":"Idle","keyframes":[{"frame":0,"value":2}]}),
    );
    colors.clear();
    export::render_video_frames(&mut session, &options, &mut |_, image| {
        colors.push(image.get_pixel(1, 1).0);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        colors,
        [
            [255, 0, 0, 255],
            [255, 0, 0, 255],
            [0, 0, 0, 0],
            [0, 0, 0, 0]
        ]
    );
    request(
        &mut session,
        json!({"op":"animation.image.set","name":"Idle","index":0,"guide":true}),
    );
    let before = session.snapshot();
    let camera = export::CameraFrame {
        name: "Camera".into(),
        width: 4,
        height: 4,
        bounds: [-2., -2., 2., 2.],
    };
    let (preview, warnings) =
        export::render_clip_frame(&mut session, &camera, "Idle", 0., true).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(preview.get_pixel(1, 1).0, [255, 0, 0, 255]);
    assert_eq!(session.snapshot(), before);
    export::render_video_frames(&mut session, &options, &mut |_, image| {
        assert_eq!(image.get_pixel(1, 1).0, [0, 0, 0, 0]);
        Ok(())
    })
    .unwrap();
}

#[test]
fn collect_keeps_sequences_portable_and_reload_keeps_source_origin() {
    let (mut session, dir) = fixture("collect");
    let target = dir.join("collected");
    let before = session.snapshot();
    let collected = request(&mut session, json!({"op":"export.collect","dir":target}));
    assert_eq!(collected["files"].as_array().unwrap().len(), 3);
    assert_eq!(session.snapshot(), before);
    let moved = dir.join("moved");
    fs::rename(target, &moved).unwrap();
    let document = moved.join("fixture.inx");
    let mut reopened = Session::new();
    request(&mut reopened, json!({"op":"open","path":document}));
    let selected = request(
        &mut reopened,
        json!({"op":"animation.image.read","name":"Idle","frame":1}),
    );
    assert!(selected["images"][0]["path"]
        .as_str()
        .unwrap()
        .starts_with(moved.to_str().unwrap()));
    let directory = reopened.snapshot()["model"]["animations"]["Idle"]["images"][0]["sequence"]
        ["directory"]
        .as_str()
        .unwrap()
        .to_owned();
    fs::remove_file(PathBuf::from(&directory).join("frame10.png")).unwrap();
    request(
        &mut reopened,
        json!({"op":"animation.image.sequence","name":"Idle","index":0}),
    );
    assert_eq!(
        reopened.snapshot()["model"]["animations"]["Idle"]["images"][0]["sequence"]["firstNumber"],
        10
    );
    let selected = request(
        &mut reopened,
        json!({"op":"animation.image.read","name":"Idle","frame":0}),
    );
    assert_eq!(selected["images"][0]["gap"], true);
}

#[test]
fn missing_static_and_sequence_sources_remain_export_warnings() {
    let (mut session, dir) = fixture("warnings");
    let still = dir.join("still.png");
    fs::copy(dir.join("frame10.png"), &still).unwrap();
    request(&mut session, json!({"op":"animation.image.add","name":"Idle","path":still}));
    fs::remove_file(still).unwrap();
    fs::remove_file(dir.join("frame11.png")).unwrap();
    fs::write(dir.join("frame13.png"), b"not a PNG").unwrap();
    let result = request(&mut session, json!({"op":"export.frames","animation":"Idle",
        "path":dir.join("out.png"),"transparency":true}));
    let warnings = result["imagesLeftOut"].to_string();
    for file in ["still.png", "frame11.png", "frame13.png"] {
        assert!(warnings.contains(file), "missing warning for {file}: {warnings}");
    }
}

#[test]
fn sequence_scan_refuses_ambiguous_corrupt_and_external_symlink_members() {
    let (_, dir) = fixture("scan");
    fs::copy(dir.join("frame10.png"), dir.join("frame010.PNG")).unwrap();
    assert!(export::image_sequence(dir.to_str().unwrap(), "frame", None)
        .unwrap_err()
        .contains("duplicate"));
    fs::remove_file(dir.join("frame010.PNG")).unwrap();
    fs::write(dir.join("frame12.png"), b"broken").unwrap();
    assert!(export::image_sequence(dir.to_str().unwrap(), "frame", None).is_err());
    fs::remove_file(dir.join("frame12.png")).unwrap();
    #[cfg(unix)]
    {
        let external = dir.with_extension("png");
        fs::copy(dir.join("frame10.png"), &external).unwrap();
        std::os::unix::fs::symlink(&external, dir.join("frame12.png")).unwrap();
        assert!(export::image_sequence(dir.to_str().unwrap(), "frame", None)
            .unwrap_err()
            .contains("symbolic links"));
        fs::remove_file(external).unwrap();
    }
}
