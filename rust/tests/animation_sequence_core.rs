use image::{Rgba, RgbaImage};
use serde_json::{json, Value};
use soul2d_core::Session;
use std::fs;
use std::path::{Path, PathBuf};

fn response(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(session: &mut Session, value: Value) -> Value {
    let response = response(session, value.clone());
    assert_eq!(response["succeeded"], true, "{value}: {response}");
    response["result"].clone()
}

fn write_inx(model: &Value, path: &Path) {
    let bytes = soul2d_core::model_import::inx_bytes(model, &[]).unwrap();
    fs::write(path, bytes).unwrap();
}

fn empty_session_model() -> Value {
    let mut session = Session::new();
    result(&mut session, json!({"op":"new"}))["model"].clone()
}

fn sequence_session(label: &str) -> (Session, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "soul2d-sequence-core-{label}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let dir = fs::canonicalize(dir).unwrap();
    for number in [10, 11, 13] {
        RgbaImage::from_pixel(2, 2, Rgba([number as u8, 0, 0, 255]))
            .save(dir.join(format!("frame{number}.png")))
            .unwrap();
    }
    let mut model = empty_session_model();
    model["animations"]["Idle"] = json!({
        "length": 4,
        "timestep": 0.25,
        "additive": false,
        "animationWeight": 1.0,
        "leadIn": -1,
        "leadOut": -1,
        "lanes": [],
    });
    let path = dir.join("fixture.inx");
    write_inx(&model, &path);
    let mut session = Session::new();
    result(&mut session, json!({"op":"open","path":path}));
    result(
        &mut session,
        json!({"op":"animation.image.sequence","name":"Idle","directory":dir,"prefix":"frame"}),
    );
    (session, dir)
}

#[test]
fn sequence_read_floors_raw_frames_and_applies_exact_remap_keys_and_gaps() {
    let (mut session, dir) = sequence_session("remap");
    let read = |session: &mut Session, frame| {
        result(session, json!({"op":"animation.image.read","name":"Idle","frame":frame}))
            ["images"][0]
            .clone()
    };

    let before_boundary = read(&mut session, 1.999_999);
    assert_eq!(before_boundary["sourceFrame"], 11.0);
    assert!(before_boundary["path"].as_str().unwrap().ends_with("frame11.png"));
    let missing = read(&mut session, 2.0);
    assert_eq!(missing["sourceFrame"], 12.0);
    assert_eq!(missing["gap"], true);
    assert_eq!(missing["path"], Value::Null);

    result(
        &mut session,
        json!({"op":"animation.image.time","name":"Idle","index":0,"interpolation":"Linear",
            "keyframes":[{"frame":0,"value":0.0},{"frame":2,"value":3.0}]}),
    );
    assert_eq!(read(&mut session, 0.6)["sourceFrame"], 10.0);
    assert_eq!(read(&mut session, 1.0)["sourceFrame"], 11.0);

    result(
        &mut session,
        json!({"op":"animation.image.time","name":"Idle","index":0,"interpolation":"Stepped",
            "keyframes":[{"frame":0,"value":3.0},{"frame":2,"value":0.0}]}),
    );
    assert_eq!(read(&mut session, 1.999_999)["sourceFrame"], 13.0);
    assert_eq!(read(&mut session, 2.0)["sourceFrame"], 10.0);
    assert_eq!(read(&mut session, 2.000_001)["sourceFrame"], 10.0);
    result(&mut session, json!({"op":"animation.image.time","name":"Idle","index":0,
        "interpolation":"Linear","keyframes":[{"frame":0,"value":0.6},
            {"frame":1,"value":0.6},{"frame":2,"value":1000000.0}]}));
    assert_eq!(read(&mut session, 0.5)["sourceFrame"], 10.0,
        "a distant large key must not round a deliberate fractional offset");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn nested_group_exposure_uses_nearest_override_and_deleting_group_prunes_its_curve() {
    let mut model = empty_session_model();
    model["param"] = json!([{
        "groupUUID":10,"name":"Outer","children":[
            {"uuid":18,"name":"Outer parameter","is_vec2":false,"defaults":[0.0,0.0],"merge_mode":"Forced"},
            {"groupUUID":20,"name":"Inner","children":[
                {"uuid":17,"name":"Inner parameter","is_vec2":false,"defaults":[0.0,0.0],"merge_mode":"Forced"}
            ]}
        ]
    }]);
    model["animations"]["Idle"] = json!({
        "length":12,"timestep":0.1,"additive":false,"animationWeight":1.0,"leadIn":-1,"leadOut":-1,
        "lanes":[
            {"uuid":17,"target":0,"merge_mode":"Forced","interpolation":"Linear",
                "keyframes":[{"frame":0,"value":0.0},{"frame":11,"value":11.0}]},
            {"uuid":18,"target":0,"merge_mode":"Forced","interpolation":"Linear",
                "keyframes":[{"frame":0,"value":0.0},{"frame":11,"value":11.0}]}
        ],
        "frameStep":{"keyframes":[{"frame":0,"value":5}]},
        "frameStepGroups":{
            "10":{"keyframes":[{"frame":0,"value":3}]},
            "20":{"keyframes":[{"frame":0,"value":2}]}
        }
    });
    let dir = std::env::temp_dir().join(format!("soul2d-step-groups-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fixture.inx");
    write_inx(&model, &path);
    let mut session = Session::new();
    result(&mut session, json!({"op":"open","path":path}));

    let sampled = result(&mut session, json!({"op":"animation.sample","name":"Idle","frame":5.5}));
    assert!((sampled["values"]["18"][0].as_f64().unwrap() - 3.0).abs() < 1e-5);
    assert!((sampled["values"]["17"][0].as_f64().unwrap() - 4.0).abs() < 1e-5);
    result(&mut session, json!({"op":"animation.freeze.set","name":"Idle","mark":"all","frame":5}));
    let frozen = session.snapshot();
    for request in [
        json!({"op":"parameter.move","id":17,"group":null}),
        json!({"op":"parameter.group.delete","id":20}),
    ] {
        let rejected = response(&mut session, request.clone());
        assert_eq!(rejected["succeeded"], false, "{request}: {rejected}");
        assert_eq!(session.snapshot(), frozen, "a refused inheritance change must be atomic");
    }
    result(&mut session, json!({"op":"animation.freeze.set","name":"Idle","frame":null}));
    result(&mut session, json!({"op":"parameter.move","id":17,"group":null}));
    result(&mut session, json!({"op":"undo"}));
    result(&mut session, json!({"op":"parameter.group.delete","id":20}));
    let clip = &session.snapshot()["model"]["animations"]["Idle"];
    assert_eq!(clip["frameStepGroups"].as_object().unwrap().len(), 1);
    assert!(clip["frameStepGroups"].get("20").is_none());
    let sampled = result(&mut session, json!({"op":"animation.sample","name":"Idle","frame":5.5}));
    assert!((sampled["values"]["17"][0].as_f64().unwrap() - 3.0).abs() < 1e-5);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn locked_sequence_remap_refusal_preserves_the_session_snapshot() {
    let (mut session, dir) = sequence_session("locked");
    result(
        &mut session,
        json!({"op":"animation.image.set","name":"Idle","index":0,"locked":true}),
    );
    let before = session.snapshot();
    let rejected = response(
        &mut session,
        json!({"op":"animation.image.time","name":"Idle","index":0,
            "keyframes":[{"frame":0,"value":1.0}]}),
    );
    assert_eq!(rejected["succeeded"], false);
    assert_eq!(session.snapshot(), before);
    fs::remove_dir_all(dir).unwrap();
}
