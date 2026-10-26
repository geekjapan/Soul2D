//! Audio tracks of an INX animation (ANI03): `audio: [{path, frame, mute?}]` in the clip, WAV files that start at
//! their frame in a video export.

use serde_json::{json, Value};
use soul2d_core::export::{clip_audio, plan_frames, AnimationTiming, AudioInput, FramePlan};
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
    let dir = std::env::temp_dir().join(format!("soul2d-audio-{label}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(dir).unwrap()
}

/// A file that starts as a WAV does (RIFF, size, WAVE); only that header is read.
fn wav(dir: &Path, name: &str, magic: &[u8; 4]) -> String {
    let path = dir.join(name);
    let mut bytes = magic.to_vec();
    bytes.extend([36, 0, 0, 0]);
    bytes.extend(b"WAVEfmt ");
    fs::write(&path, bytes).unwrap();
    path.to_string_lossy().into_owned()
}

/// An empty document with the 60-frame clip "Idle" at 1/60 s a frame.
fn session() -> Session {
    let mut session = Session::new();
    result(&request(&mut session, json!({"op": "new"})));
    result(&request(&mut session, json!({"op": "animation.create", "name": "Idle"})));
    session
}

fn tracks(session: &Session) -> Value {
    session.snapshot()["model"]["animations"]["Idle"].get("audio").cloned().unwrap_or(Value::Null)
}

fn history_count(session: &mut Session) -> u64 {
    result(&request(session, json!({"op": "history.summary"})))["count"].as_u64().unwrap()
}

/// A succeeding request is one undo step that undo reverts and redo restores.
fn one_undo_step(session: &mut Session, value: Value) {
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
fn rejected(session: &mut Session, value: Value) -> String {
    let before = session.snapshot();
    let count = history_count(session);
    let response = request(session, value);
    assert_eq!(response["succeeded"], false, "expected failure: {response}");
    assert_eq!(session.snapshot(), before);
    assert_eq!(history_count(session), count);
    response["message"].as_str().unwrap_or("").to_owned()
}

#[test]
fn tracks_are_added_moved_muted_and_deleted_in_one_undo_step_each() {
    let dir = temp_dir("edit");
    let voice = wav(&dir, "voice.wav", b"RIFF");
    let music = wav(&dir, "music.wav", b"RIFF");
    let mut s = session();
    // A relative or roundabout name is kept resolved.
    let roundabout = dir.join("..").join(dir.file_name().unwrap()).join("voice.wav");
    one_undo_step(&mut s, json!({"op": "animation.audio.add", "name": "Idle", "path": roundabout}));
    one_undo_step(&mut s, json!({"op": "animation.audio.add", "name": "Idle", "path": music, "frame": 30}));
    assert_eq!(tracks(&s), json!([{"path": voice, "frame": 0}, {"path": music, "frame": 30}]));

    one_undo_step(&mut s, json!({"op": "animation.audio.set", "name": "Idle", "index": 0, "frame": 12, "mute": true}));
    assert_eq!(tracks(&s)[0], json!({"path": voice, "frame": 12, "mute": true}));
    one_undo_step(&mut s, json!({"op": "animation.audio.set", "name": "Idle", "index": 0, "mute": false}));
    assert_eq!(tracks(&s)[0], json!({"path": voice, "frame": 12}));

    // The clip cannot end at or before a track's start.
    result(&request(&mut s, json!({"op": "animation.set", "name": "Idle", "length": 31})));
    assert!(rejected(&mut s, json!({"op": "animation.set", "name": "Idle", "length": 30})).contains("audio[1].frame"));

    one_undo_step(&mut s, json!({"op": "animation.audio.delete", "name": "Idle", "index": 0}));
    assert_eq!(tracks(&s), json!([{"path": music, "frame": 30}]));
    one_undo_step(&mut s, json!({"op": "animation.audio.delete", "name": "Idle", "index": 0}));
    assert_eq!(tracks(&s), Value::Null, "a clip without tracks has no audio key");
}

#[test]
fn bad_files_and_requests_are_refused_and_change_nothing() {
    let dir = temp_dir("refuse");
    let voice = wav(&dir, "voice.wav", b"RIFF");
    let rf64 = wav(&dir, "long.wav", b"RF64");
    let png = dir.join("image.wav");
    fs::write(&png, b"\x89PNG\r\n\x1a\n0000").unwrap();
    let avi = dir.join("movie.wav");
    fs::write(&avi, b"RIFF\x24\0\0\0AVI LIST").unwrap();
    let wave = dir.join("wave.wav");
    fs::write(&wave, b"FORM\x24\0\0\0WAVEfmt ").unwrap();
    let short = dir.join("short.wav");
    fs::write(&short, b"RIFF").unwrap();
    let missing = dir.join("missing.wav").to_string_lossy().into_owned();
    let mut s = session();
    let add = |path: &str| json!({"op": "animation.audio.add", "name": "Idle", "path": path});
    assert_eq!(rejected(&mut s, add(&missing)), format!("audio file {missing} was not found"));
    assert!(rejected(&mut s, add(dir.to_str().unwrap())).contains("was not found"), "a folder is not a file");
    assert!(rejected(&mut s, add(&rf64)).contains("RF64"));
    assert!(rejected(&mut s, add(png.to_str().unwrap())).contains("is not a WAV file"));
    assert!(rejected(&mut s, add(short.to_str().unwrap())).contains("is not a WAV file"));
    assert!(rejected(&mut s, add(avi.to_str().unwrap())).contains("is not a WAV file"), "RIFF, but not WAVE");
    assert!(rejected(&mut s, add(wave.to_str().unwrap())).contains("is not a WAV file"), "WAVE, but not RIFF");
    let late = json!({"op": "animation.audio.add", "name": "Idle", "path": voice, "frame": 60});
    assert_eq!(rejected(&mut s, late), "frame must be between 0 and 59");
    let elsewhere = json!({"op": "animation.audio.add", "name": "Walk", "path": voice});
    assert!(rejected(&mut s, elsewhere).contains("\"Walk\" was not found"));

    result(&request(&mut s, add(&voice)));
    let set = |fields: Value| {
        let mut request = json!({"op": "animation.audio.set", "name": "Idle"});
        request.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        request
    };
    assert_eq!(rejected(&mut s, set(json!({"index": 0}))), "animation.audio.set requires frame, mute or group");
    assert_eq!(rejected(&mut s, set(json!({"index": 0, "group": 3}))), "group must be a string or null");
    assert_eq!(rejected(&mut s, set(json!({"index": 0, "mute": "yes"}))), "mute must be a boolean");
    assert_eq!(rejected(&mut s, set(json!({"index": 0, "frame": -1}))), "frame must be between 0 and 59");
    let no_track = "index must name one of the 1 audio tracks of \"Idle\"";
    assert_eq!(rejected(&mut s, set(json!({"index": 1, "mute": true}))), no_track);
    assert_eq!(rejected(&mut s, set(json!({"mute": true}))), no_track);
    let delete = json!({"op": "animation.audio.delete", "name": "Idle", "index": 1});
    assert_eq!(rejected(&mut s, delete), no_track);
}

/// The frames the export renders for `timing`, played `loops` times, at `framerate` or the clip's own rate.
fn plan(length: i32, timestep: f32, lead: (i32, i32), loops: u32, framerate: Option<u32>) -> FramePlan {
    plan_frames(AnimationTiming { length, timestep, lead_in: lead.0, lead_out: lead.1 }, loops, framerate).unwrap()
}

fn delays(model: &Value, plan: &FramePlan) -> Vec<u64> {
    let (inputs, left_out) = clip_audio(model, "Idle", plan, "auto", "/x/clip.mp4");
    assert_eq!(left_out, Vec::<String>::new());
    inputs.iter().map(|input| input.delay_ms).collect()
}

#[test]
fn a_video_takes_the_tracks_that_play_where_their_frames_first_show() {
    let dir = temp_dir("export");
    let voice = wav(&dir, "voice.wav", b"RIFF");
    let music = wav(&dir, "music.wav", b"RIFF");
    let mut s = session();
    result(&request(&mut s, json!({"op": "animation.set", "name": "Idle", "length": 1000})));
    result(&request(&mut s, json!({"op": "animation.audio.add", "name": "Idle", "path": voice, "frame": 600})));
    result(&request(&mut s, json!({"op": "animation.audio.add", "name": "Idle", "path": music, "frame": 6})));
    result(&request(&mut s, json!({"op": "animation.audio.add", "name": "Idle", "path": music})));
    result(&request(&mut s, json!({"op": "animation.audio.set", "name": "Idle", "index": 2, "mute": true})));
    let model = s.snapshot()["model"].clone();
    let (inputs, _) = clip_audio(&model, "Idle", &plan(1000, 1.0 / 60.0, (-1, -1), 1, None), "auto", "/x/clip.mp4");
    assert_eq!(
        inputs,
        [AudioInput { path: voice.clone(), delay_ms: 10_000 }, AudioInput { path: music.clone(), delay_ms: 100 }],
        "frame f of a 60 fps clip is f / 60 s in; the muted track is left out"
    );
    // 0.016 s a frame is exported at the rounded 62 fps, image k showing frame k: frame 600 is 9.677 s in, not 9.6.
    assert_eq!(delays(&model, &plan(1000, 0.016, (-1, -1), 1, None)), [9677, 97]);
    // At another framerate the clip keeps its own speed: frame 600 at 0.016 s a frame is 9.6 s in.
    assert_eq!(delays(&model, &plan(1000, 0.016, (-1, -1), 1, Some(30))), [9600, 96]);
    // A lead-in of frame 5 with two loops: frame 6 shows first in the first pass, frame 600 too.
    assert_eq!(delays(&model, &plan(1000, 1.0 / 60.0, (5, 900), 2, None)), [10_000, 100]);
    // Two loops up to a lead-out at frame 300 end the video there, so frame 600 never shows.
    let lead_out = plan(1000, 1.0 / 60.0, (-1, 300), 2, None);
    let (inputs, left_out) = clip_audio(&model, "Idle", &lead_out, "auto", "/x/a.mp4");
    assert_eq!(inputs, [AudioInput { path: music.clone(), delay_ms: 100 }]);
    assert_eq!(left_out, [format!("audio file {voice} never shows in the video")]);

    // A container without sound takes no tracks.
    let silent = [
        ("png", "/x/a.png"), ("tga", "/x/a.tga"), ("auto", "/x/a.gif"), ("gif", "/x/a.mkv"), ("auto", "/x/a.APNG"),
        ("auto", "/x/a.webp"), ("libwebp_anim", "/x/a.mkv"), ("auto", "/x/a.jpg"), ("auto", "/x/a.y4m"),
        ("jpeg", "/x/f"),
    ];
    for (codec, file) in silent {
        let one = plan(1000, 1.0 / 60.0, (-1, -1), 1, None);
        assert_eq!(clip_audio(&model, "Idle", &one, codec, file), (vec![], vec![]), "{codec} {file}");
    }

    // A document can name any file, so the export hands ffmpeg its resolved name, reads each header again, and
    // leaves out one that fails.
    let mut roundabout = model.clone();
    let dotted = voice.replace("/voice.wav", "/./voice.wav");
    roundabout["animations"]["Idle"]["audio"] = json!([{"path": dotted, "frame": 0}]);
    let one = plan(1000, 1.0 / 60.0, (-1, -1), 1, None);
    let (inputs, _) = clip_audio(&roundabout, "Idle", &one, "auto", "/x/a.mp4");
    assert_eq!(inputs, [AudioInput { path: voice.clone(), delay_ms: 0 }]);
    // A relative name would resolve against this process's folder, not the document's.
    roundabout["animations"]["Idle"]["audio"] = json!([{"path": "voice.wav", "frame": 0}]);
    let (inputs, left_out) = clip_audio(&roundabout, "Idle", &one, "auto", "/x/a.mp4");
    assert_eq!((inputs, left_out), (vec![], vec!["audio file voice.wav is not an absolute path".to_owned()]));
    fs::write(&music, b"RF64\0\0\0\0WAVE").unwrap();
    let (inputs, left_out) = clip_audio(&model, "Idle", &one, "auto", "/x/clip.mp4");
    assert_eq!(inputs.len(), 1);
    assert_eq!(left_out, [format!("audio file {music} is an RF64 WAV, which is not supported")]);
    fs::remove_file(&music).unwrap();
    let (_, left_out) = clip_audio(&model, "Idle", &one, "auto", "/x/clip.mp4");
    assert_eq!(left_out, [format!("audio file {music} was not found")]);
}

#[test]
fn a_name_with_a_percent_sign_and_tracks_past_the_cap_are_refused_or_left_out() {
    let dir = temp_dir("cap");
    let odd = wav(&dir, "take-%d.wav", b"RIFF");
    let mut s = session();
    let add =
        |path: &str, frame: u32| json!({"op": "animation.audio.add", "name": "Idle", "path": path, "frame": frame});
    assert!(rejected(&mut s, add(&odd, 0)).contains("must not contain '%'"));
    let voice = wav(&dir, "voice.wav", b"RIFF");
    for frame in 0..34 {
        result(&request(&mut s, add(&voice, frame)));
    }
    let model = s.snapshot()["model"].clone();
    let (inputs, left_out) = clip_audio(&model, "Idle", &plan(60, 1.0 / 60.0, (-1, -1), 1, None), "auto", "/x/a.mp4");
    assert_eq!(inputs.len(), 32);
    assert_eq!(left_out.len(), 2);
    assert!(left_out[0].ends_with("is past the first 32 tracks"), "{left_out:?}");
}

#[test]
fn a_bad_track_from_a_file_can_be_changed_or_deleted() {
    let dir = temp_dir("repair");
    let voice = wav(&dir, "voice.wav", b"RIFF");
    let mut s = session();
    let mut model = s.snapshot()["model"].clone();
    model["animations"]["Idle"]["audio"] = json!([{"path": voice, "frame": 99}, {"path": voice, "frame": 2}]);
    result(&request(&mut s, json!({"op": "new", "model": model.clone()})));
    // An edit that leaves the bad track is refused; one that changes or deletes it is not.
    let other = json!({"op": "animation.audio.set", "name": "Idle", "index": 1, "mute": true});
    assert_eq!(rejected(&mut s, other), "animation.audio[0].frame is outside animation length");
    result(&request(&mut s, json!({"op": "animation.audio.set", "name": "Idle", "index": 0, "frame": 1})));
    assert_eq!(tracks(&s), json!([{"path": voice, "frame": 1}, {"path": voice, "frame": 2}]));
    result(&request(&mut s, json!({"op": "new", "model": model.clone()})));
    result(&request(&mut s, json!({"op": "animation.audio.delete", "name": "Idle", "index": 0})));
    assert_eq!(tracks(&s), json!([{"path": voice, "frame": 2}]));
    for (track, message) in [
        (json!({"path": 7, "frame": 0}), "animation.audio[0].path must be a string"),
        (json!({"path": voice, "frame": 0, "mute": 1}), "animation.audio[0].mute must be a boolean"),
    ] {
        model["animations"]["Idle"]["audio"] = json!([track, {"path": voice, "frame": 2}]);
        result(&request(&mut s, json!({"op": "new", "model": model.clone()})));
        let other = json!({"op": "animation.audio.set", "name": "Idle", "index": 1, "mute": true});
        assert_eq!(rejected(&mut s, other), message);
    }
    // `audio: null` reads as no tracks, so the clip can still be edited and given one.
    model["animations"]["Idle"]["audio"] = Value::Null;
    result(&request(&mut s, json!({"op": "new", "model": model.clone()})));
    result(&request(&mut s, json!({"op": "animation.set", "name": "Idle", "length": 30})));
    result(&request(&mut s, json!({"op": "animation.audio.add", "name": "Idle", "path": voice, "frame": 2})));
    assert_eq!(tracks(&s), json!([{"path": voice, "frame": 2}]));
}
