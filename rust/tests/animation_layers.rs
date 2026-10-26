//! Image and group tracks of an INX animation (ANI04): `images: [{path, x, y, scale, opacity, front?, hidden?,
//! locked?, group?}]`, `groups: [{name, x, y, scale, opacity, hidden?, locked?}]` and `modelGroup` in the clip.

use serde_json::{json, Value};
use soul2d_core::export::{clip_audio, clip_scene, plan_frames, AnimationTiming, Placement};
use soul2d_core::Session;
use std::fs;
use std::path::{Path, PathBuf};

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("soul2d-layers-{label}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(dir).unwrap()
}

/// A 2x2 PNG of one colour.
fn png(dir: &Path, name: &str) -> String {
    let path = dir.join(name);
    image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255])).save(&path).unwrap();
    path.to_string_lossy().into_owned()
}

/// An empty document with the 60-frame clip "Idle".
fn session() -> Session {
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new"})));
    result(&request(&mut session, json!({"op": "animation.create", "name": "Idle"})));
    session
}

fn clip(session: &Session, key: &str) -> Value {
    session.snapshot()["model"]["animations"]["Idle"].get(key).cloned().unwrap_or(Value::Null)
}

fn history_count(session: &mut Session) -> u64 {
    result(&request(session, json!({"op": "history.summary"})))["count"].as_u64().unwrap()
}

/// A succeeding request is one undo step that undo reverts and redo restores.
fn one_undo_step(session: &mut Session, mut value: Value) {
    value["name"] = json!("Idle");
    let before = session.snapshot()["model"].clone();
    let count = history_count(session);
    result(&request(session, value));
    let after = session.snapshot()["model"].clone();
    assert_ne!(after, before, "request changed nothing");
    assert_eq!(history_count(session), count + 1);
    result(&request(session, json!({"op": "undo"})));
    assert_eq!(session.snapshot()["model"], before);
    result(&request(session, json!({"op": "redo"})));
    assert_eq!(session.snapshot()["model"], after);
}

/// A failing request leaves snapshot and history untouched.
fn rejected(session: &mut Session, mut value: Value) -> String {
    value["name"] = json!("Idle");
    let before = session.snapshot();
    let count = history_count(session);
    let response = request(session, value);
    assert_eq!(response["succeeded"], false, "expected failure: {response}");
    assert_eq!(session.snapshot(), before);
    assert_eq!(history_count(session), count);
    response["message"].as_str().unwrap_or("").to_owned()
}

fn image(index: u64, fields: Value) -> Value {
    let mut request = json!({"op": "animation.image.set", "index": index});
    request.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
    request
}

fn group(op: &str, name: &str, fields: Value) -> Value {
    let mut request = json!({"op": format!("animation.group.{op}"), "group": name});
    request.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
    request
}

#[test]
fn image_tracks_are_added_placed_reordered_and_deleted_in_one_undo_step_each() {
    let dir = temp_dir("images");
    let (sky, hill) = (png(&dir, "sky.png"), png(&dir, "hill.png"));
    let mut s = session();
    let dotted = sky.replace("/sky.png", "/./sky.png");
    one_undo_step(&mut s, json!({"op": "animation.image.add", "path": dotted}));
    one_undo_step(&mut s, json!({"op": "animation.image.add", "path": hill, "x": 3, "y": -2, "scale": 0.5, "front": true}));
    assert_eq!(
        clip(&s, "images"),
        json!([
            {"path": sky, "x": 0.0, "y": 0.0, "scale": 1.0, "opacity": 1.0},
            {"path": hill, "x": 3.0, "y": -2.0, "scale": 0.5, "opacity": 1.0, "front": true},
        ]),
        "the path is stored resolved; a new track goes to the bottom"
    );
    one_undo_step(&mut s, image(1, json!({"opacity": 0.25, "front": false, "hidden": true})));
    one_undo_step(&mut s, image(1, json!({"to": 0})));
    assert_eq!(
        clip(&s, "images"),
        json!([
            {"path": hill, "x": 3.0, "y": -2.0, "scale": 0.5, "opacity": 0.25, "hidden": true},
            {"path": sky, "x": 0.0, "y": 0.0, "scale": 1.0, "opacity": 1.0},
        ])
    );
    one_undo_step(&mut s, json!({"op": "animation.image.delete", "index": 0}));
    one_undo_step(&mut s, json!({"op": "animation.image.delete", "index": 0}));
    assert_eq!(clip(&s, "images"), Value::Null, "an empty list leaves no trace");

    let add = |path: &str| json!({"op": "animation.image.add", "path": path});
    for _ in 0..8 {
        result(&request(&mut s, json!({"op": "animation.image.add", "name": "Idle", "path": sky})));
    }
    assert_eq!(rejected(&mut s, add(&sky)), "animation \"Idle\" already has the most image tracks, 8");
}

#[test]
fn bad_files_and_requests_are_refused_and_change_nothing() {
    let dir = temp_dir("bad");
    let sky = png(&dir, "sky.png");
    fs::write(dir.join("fake.png"), b"not a png").unwrap();
    let mut s = session();
    let add = |path: &str, extra: Value| {
        let mut request = json!({"op": "animation.image.add", "path": path});
        request.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        request
    };
    let missing = dir.join("gone.png").to_string_lossy().into_owned();
    assert_eq!(rejected(&mut s, add(&missing, json!({}))), format!("image file {missing} was not found"));
    assert!(rejected(&mut s, add(dir.to_str().unwrap(), json!({}))).ends_with("was not found"));
    assert!(rejected(&mut s, add(&dir.join("fake.png").to_string_lossy(), json!({}))).starts_with("Could not load fake.png"));
    assert_eq!(rejected(&mut s, add(&sky, json!({"scale": 0}))), "animation.images[0].scale must be above 0");
    assert_eq!(rejected(&mut s, add(&sky, json!({"opacity": 2}))), "animation.images[0].opacity must be between 0 and 1");
    assert_eq!(rejected(&mut s, add(&sky, json!({"x": "left"}))), "x must be a finite number");
    assert_eq!(rejected(&mut s, add(&sky, json!({"front": 1}))), "front must be a boolean");
    let no_group = "animation.images[0].group must name one of the clip's groups";
    assert_eq!(rejected(&mut s, add(&sky, json!({"group": "Nope"}))), no_group);
    result(&request(&mut s, json!({"op": "animation.image.add", "name": "Idle", "path": sky})));
    let no_track = "index must name one of the 1 image tracks of \"Idle\"";
    assert_eq!(rejected(&mut s, image(1, json!({"x": 1}))), no_track);
    assert_eq!(rejected(&mut s, json!({"op": "animation.image.delete", "index": 1})), no_track);
    assert_eq!(rejected(&mut s, image(0, json!({"to": 1}))), "to must name one of the 1 image tracks of \"Idle\"");
    assert_eq!(rejected(&mut s, image(0, json!({}))), "animation.image.set requires a field to change");
    assert_eq!(rejected(&mut s, image(0, json!({"group": 3}))), "group must be a string or null");
    assert_eq!(rejected(&mut s, group("add", "", json!({}))), "group must be a nonempty string");
    result(&request(&mut s, json!({"op": "animation.group.add", "name": "Idle", "group": "Set"})));
    assert_eq!(rejected(&mut s, group("add", "Set", json!({}))), "animation \"Idle\" already has a group \"Set\"");
    assert_eq!(rejected(&mut s, group("set", "Nope", json!({"x": 1}))), "animation \"Idle\" has no group \"Nope\"");
    assert_eq!(rejected(&mut s, group("set", "Set", json!({"scale": -1}))), "animation.groups[0].scale must be above 0");
    assert_eq!(rejected(&mut s, group("set", "Set", json!({"model": "yes"}))), "model must be a boolean");
    assert_eq!(rejected(&mut s, group("set", "Set", json!({}))), "animation.group.set requires a field to change");
}

#[test]
fn groups_hold_tracks_and_the_model_lock_them_and_leave_them_when_deleted() {
    let dir = temp_dir("groups");
    let sky = png(&dir, "sky.png");
    let voice = dir.join("voice.wav");
    fs::write(&voice, b"RIFF\x24\0\0\0WAVEfmt ").unwrap();
    let voice = voice.to_string_lossy().into_owned();
    let mut s = session();
    one_undo_step(&mut s, group("add", "Set", json!({"x": 2, "opacity": 0.5})));
    one_undo_step(&mut s, json!({"op": "animation.image.add", "path": sky, "group": "Set"}));
    result(&request(&mut s, json!({"op": "animation.audio.add", "name": "Idle", "path": voice})));
    one_undo_step(&mut s, json!({"op": "animation.audio.set", "index": 0, "group": "Set"}));
    one_undo_step(&mut s, group("set", "Set", json!({"model": true, "hidden": true})));
    assert_eq!(clip(&s, "groups"), json!([{"name": "Set", "x": 2.0, "y": 0.0, "scale": 1.0, "opacity": 0.5, "hidden": true}]));
    assert_eq!(clip(&s, "modelGroup"), json!("Set"));
    assert_eq!(clip(&s, "audio"), json!([{"path": voice, "frame": 0, "group": "Set"}]));

    // A locked group only changes in a request that unlocks it, and its members cannot change either.
    one_undo_step(&mut s, group("set", "Set", json!({"locked": true})));
    assert_eq!(rejected(&mut s, group("set", "Set", json!({"x": 1}))), "the group \"Set\" is locked");
    assert_eq!(rejected(&mut s, group("delete", "Set", json!({}))), "the group \"Set\" is locked");
    assert_eq!(rejected(&mut s, image(0, json!({"x": 1}))), "the group \"Set\" is locked");
    assert_eq!(rejected(&mut s, json!({"op": "animation.image.delete", "index": 0})), "the group \"Set\" is locked");
    let join = json!({"op": "animation.image.add", "path": sky, "group": "Set"});
    assert_eq!(rejected(&mut s, join), "the group \"Set\" is locked");
    let audio = |fields: Value| {
        let mut request = json!({"op": "animation.audio.set", "index": 0});
        request.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        request
    };
    assert_eq!(rejected(&mut s, audio(json!({"mute": true}))), "the group \"Set\" is locked");
    assert_eq!(rejected(&mut s, audio(json!({"group": null}))), "the group \"Set\" is locked");
    assert_eq!(rejected(&mut s, json!({"op": "animation.audio.delete", "index": 0})), "the group \"Set\" is locked");
    one_undo_step(&mut s, group("add", "Other", json!({})));
    assert_eq!(rejected(&mut s, group("set", "Other", json!({"model": true}))), "the group \"Set\" is locked");
    one_undo_step(&mut s, group("set", "Set", json!({"locked": false, "x": 1})));
    one_undo_step(&mut s, audio(json!({"group": null})));
    one_undo_step(&mut s, group("set", "Set", json!({"locked": true})));
    assert_eq!(rejected(&mut s, audio(json!({"group": "Set"}))), "the group \"Set\" is locked");
    one_undo_step(&mut s, group("set", "Set", json!({"locked": false})));
    one_undo_step(&mut s, audio(json!({"group": "Set"})));
    one_undo_step(&mut s, group("delete", "Other", json!({})));

    // A locked track likewise.
    one_undo_step(&mut s, image(0, json!({"locked": true})));
    assert_eq!(rejected(&mut s, image(0, json!({"x": 1}))), "image track 0 of \"Idle\" is locked");
    let locked_member = "image track 0 of \"Idle\" in the group \"Set\" is locked";
    assert_eq!(rejected(&mut s, group("delete", "Set", json!({}))), locked_member);
    assert_eq!(rejected(&mut s, json!({"op": "animation.image.delete", "index": 0})), "image track 0 of \"Idle\" is locked");
    one_undo_step(&mut s, image(0, json!({"locked": false, "group": null})));
    assert_eq!(clip(&s, "images"), json!([{"path": sky, "x": 0.0, "y": 0.0, "scale": 1.0, "opacity": 1.0}]));

    one_undo_step(&mut s, group("set", "Set", json!({"model": false})));
    assert_eq!(clip(&s, "modelGroup"), Value::Null);
    one_undo_step(&mut s, image(0, json!({"group": "Set"})));
    one_undo_step(&mut s, group("set", "Set", json!({"model": true})));
    one_undo_step(&mut s, group("delete", "Set", json!({})));
    assert_eq!(clip(&s, "groups"), Value::Null);
    assert_eq!(clip(&s, "modelGroup"), Value::Null, "the members stay, out of the group");
    assert_eq!(clip(&s, "images"), json!([{"path": sky, "x": 0.0, "y": 0.0, "scale": 1.0, "opacity": 1.0}]));
    assert_eq!(clip(&s, "audio"), json!([{"path": voice, "frame": 0}]));

    one_undo_step(&mut s, group("add", "Shut", json!({"hidden": true, "locked": true})));
    assert_eq!(clip(&s, "groups")[0]["locked"], true);
    for index in 1..32 {
        result(&request(&mut s, json!({"op": "animation.group.add", "name": "Idle", "group": format!("G{index}")})));
    }
    assert_eq!(rejected(&mut s, group("add", "One more", json!({}))), "animation \"Idle\" already has the most groups, 32");
}

#[test]
fn a_bad_track_from_a_file_can_be_repaired_and_null_lists_read_as_none() {
    let dir = temp_dir("repair");
    let sky = png(&dir, "sky.png");
    let mut s = session();
    let mut model = s.snapshot()["model"].clone();
    let good = json!({"path": sky, "x": 0, "y": 0, "scale": 1, "opacity": 1});
    model["animations"]["Idle"]["images"] = json!([{"path": sky, "x": 0, "y": 0, "scale": 0, "opacity": 1}, good]);
    result(&request(&mut s, json!({"op": "new", "model": model.clone()})));
    // An edit that leaves the bad track is refused; one that changes or deletes it is not.
    assert_eq!(rejected(&mut s, image(1, json!({"x": 1}))), "animation.images[0].scale must be above 0");
    let length = json!({"op": "animation.set", "length": 30});
    assert_eq!(rejected(&mut s, length), "animation.images[0].scale must be above 0");
    result(&request(&mut s, json!({"op": "animation.image.set", "name": "Idle", "index": 0, "scale": 2})));
    result(&request(&mut s, json!({"op": "new", "model": model.clone()})));
    result(&request(&mut s, json!({"op": "animation.image.delete", "name": "Idle", "index": 0})));
    assert_eq!(clip(&s, "images"), json!([good]));
    model["animations"]["Idle"]["modelGroup"] = json!("Gone");
    model["animations"]["Idle"]["images"] = json!([good]);
    result(&request(&mut s, json!({"op": "new", "model": model.clone()})));
    assert_eq!(rejected(&mut s, image(0, json!({"x": 1}))), "animation.modelGroup must name one of the clip's groups");

    let set = json!({"name": "Set", "x": 0, "y": 0, "scale": 1, "opacity": 1});
    model["animations"]["Idle"]["modelGroup"] = Value::Null;
    model["animations"]["Idle"]["groups"] = json!([set, set]);
    result(&request(&mut s, json!({"op": "new", "model": model.clone()})));
    assert_eq!(rejected(&mut s, image(0, json!({"x": 1}))), "animation.groups[1].name \"Set\" is used twice");

    model["animations"]["Idle"]["modelGroup"] = Value::Null;
    model["animations"]["Idle"]["images"] = Value::Null;
    model["animations"]["Idle"]["groups"] = Value::Null;
    result(&request(&mut s, json!({"op": "new", "model": model.clone()})));
    result(&request(&mut s, json!({"op": "animation.set", "name": "Idle", "length": 30})));
    result(&request(&mut s, json!({"op": "animation.group.add", "name": "Idle", "group": "Set"})));
    result(&request(&mut s, json!({"op": "animation.image.add", "name": "Idle", "path": sky})));
    assert_eq!(clip(&s, "images").as_array().map(Vec::len), Some(1));
}

#[test]
fn an_export_takes_the_tracks_it_can_draw_placed_by_their_groups() {
    let dir = temp_dir("scene");
    let sky = png(&dir, "sky.png");
    let voice = dir.join("voice.wav");
    fs::write(&voice, b"RIFF\x24\0\0\0WAVEfmt ").unwrap();
    let track = |extra: Value| {
        let mut track = json!({"path": sky, "x": 1, "y": 2, "scale": 0.5, "opacity": 0.5});
        track.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        track
    };
    let mut clip = json!({
        "length": 60, "timestep": 0.1, "leadIn": -1, "leadOut": -1, "lanes": [],
        "groups": [{"name": "Set", "x": 10, "y": 20, "scale": 2, "opacity": 0.5}, {"name": "Off", "x": 0, "y": 0,
            "scale": 1, "opacity": 1, "hidden": true}],
        "modelGroup": "Set",
        "images": [track(json!({"front": true, "group": "Set"})), track(json!({"hidden": true})),
            track(json!({"group": "Off"})), track(json!({"path": "sky.png"}))],
        "audio": [{"path": voice, "frame": 0, "group": "Off"}, {"path": voice, "frame": 0}],
    });
    for _ in 0..9 {
        clip["images"].as_array_mut().unwrap().push(track(json!({})));
    }
    let model = json!({"animations": {"Idle": clip}});
    let (scene, left_out) = clip_scene(&model, "Idle", true);
    let set = Placement { x: 10.0, y: 20.0, scale: 2.0, opacity: 0.5 };
    assert_eq!(scene.model, Some(set), "the model is placed by its group");
    let placements: Vec<Placement> = scene.front.iter().chain(&scene.back).map(|layer| layer.placement).collect();
    let own = Placement { x: 1.0, y: 2.0, scale: 0.5, opacity: 0.5 };
    // The grouped track lies at 10 + 1 * 2, 20 + 2 * 2, its scale and opacity multiplied; the hidden track and the
    // track in the hidden group are not drawn; only the first 8 tracks shown are read, the relative one among them,
    // so six of the nine plain tracks are drawn.
    let grouped = Placement { x: 12.0, y: 24.0, scale: 1.0, opacity: 0.25 };
    assert_eq!(placements, [vec![grouped], vec![own; 6]].concat());
    assert_eq!(scene.front.len(), 1);
    assert_eq!(scene.front[0].image.dimensions(), (2, 2));
    assert_eq!(left_out, ["image file sky.png is not an absolute path", "3 more image tracks are past the first 8"]);

    let timing = AnimationTiming { length: 60, timestep: 0.1, lead_in: -1, lead_out: -1 };
    let (inputs, _) = clip_audio(&model, "Idle", &plan_frames(timing, 1, None).unwrap(), "auto", "/x/a.mp4");
    assert_eq!(inputs.len(), 1, "the track in the hidden group is not exported");

    let mut hidden = model.clone();
    hidden["animations"]["Idle"]["groups"][0]["hidden"] = json!(true);
    let (checked, checked_left_out) = clip_scene(&model, "Idle", false);
    assert_eq!((checked.back.len() + checked.front.len(), checked_left_out), (0, left_out), "a check keeps no picture");
    assert_eq!(clip_scene(&hidden, "Idle", true).0.model, None, "a hidden group hides the model");
    let plain = json!({"animations": {"Idle": {"length": 60, "timestep": 0.1}}});
    assert_eq!(clip_scene(&plain, "Idle", true), (Default::default(), vec![]), "a clip without tracks draws as before");

    // Values a file brought in: a group it does not hold places nothing, a scale at or below 0 reads as 1 and an
    // opacity is brought between 0 and 1.
    let odd = json!({"animations": {"Idle": {"modelGroup": "Gone", "images": [
        {"path": sky, "x": 1, "y": 2, "scale": -3, "opacity": 4, "group": "Gone"}, {"path": "//host/share/a.png"},
    ], "audio": [{"path": voice, "frame": 0, "group": "Gone"}, {"path": "//host/share/a.wav", "frame": 0}]}}});
    let (scene, left_out) = clip_scene(&odd, "Idle", true);
    let network = "image file //host/share/a.png is a network or device path";
    assert_eq!((scene.model, left_out), (Some(Placement::IDENTITY), vec![network.to_owned()]));
    assert_eq!(scene.back[0].placement, Placement { x: 1.0, y: 2.0, scale: 1.0, opacity: 1.0 });
    let (inputs, left_out) = clip_audio(&odd, "Idle", &plan_frames(timing, 1, None).unwrap(), "auto", "/x/a.mp4");
    assert_eq!((inputs.len(), left_out), (1, vec!["audio file //host/share/a.wav is a network or device path".to_owned()]));
}
