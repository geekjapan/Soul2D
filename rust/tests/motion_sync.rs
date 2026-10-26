
mod common;

use serde_json::{json, Value};
use soul2d_core::model_import::inx_bytes;
use soul2d_core::Session;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const MOUTH: u32 = 10;
const JAW: u32 = 11;
const EYE: u32 = 12;
const OTHER: u32 = 13;

fn temp_dir(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "soul2d-motionsync-{}-{nanos}-{label}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst),
    ));
    fs::create_dir(&dir).unwrap();
    common::scratch(dir)
}

fn wav(dir: &Path, name: &str) -> String {
    let path = dir.join(name);
    let mut bytes = b"RIFF".to_vec();
    bytes.extend([36, 0, 0, 0]);
    bytes.extend(b"WAVEfmt ");
    bytes.extend([0; 32]);
    fs::write(&path, bytes).unwrap();
    fs::canonicalize(&path).unwrap().to_string_lossy().into_owned()
}

fn stub(dir: &Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\nfor last in \"$@\"; do :; done\necho $$ > '{}/pid-{name}.txt'\n{body}\n", dir.display())).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    path.to_string_lossy().into_owned()
}

const FIXTURE: &str = "cat <<JSON\n\
 {\"metadata\":{\"soundFile\":\"$last\",\"duration\":0.5},\"mouthCues\":[\
 {\"start\":0.0,\"end\":0.125,\"value\":\"X\"},\
 {\"start\":0.125,\"end\":0.25,\"value\":\"D\"},\
 {\"start\":0.25,\"end\":0.375,\"value\":\"F\"},\
 {\"start\":0.375,\"end\":0.5,\"value\":\"X\"}]}\nJSON";

fn model(wav: &str) -> Value {
    let clip = |length: i64, extra: Value| {
        let mut clip = json!({"length": length, "timestep": 0.125, "additive": false,
            "animationWeight": 1.0, "leadIn": -1, "leadOut": -1, "lanes": [],
            "audio": [{"path": wav, "frame": 2}]});
        for (key, value) in extra.as_object().unwrap() {
            clip[key] = value.clone();
        }
        clip
    };
    json!({
        "meta": {"name": "", "version": "1.0-alpha"},
        "live2d": {"eyeBlink": [EYE]},
        "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
            "transform": {"trans": [0.0,0.0,0.0], "rot": [0.0,0.0,0.0], "scale": [1.0,1.0]},
            "lockToRoot": false, "pinToMesh": false, "children": []},
        "param": [
            {"uuid": MOUTH, "name": "Mouth", "is_vec2": false, "min": [0.0,0.0], "max": [1.0,0.0],
             "defaults": [0.0,0.0], "axis_points": [[0.0,1.0],[0.0]], "merge_mode": "Additive",
             "bindings": []},
            {"uuid": JAW, "name": "Jaw", "is_vec2": true, "min": [-1.0,-1.0], "max": [1.0,1.0],
             "defaults": [0.0,0.0], "axis_points": [[0.0,1.0],[0.0,1.0]], "merge_mode": "Additive",
             "bindings": []},
            {"uuid": EYE, "name": "Eye", "is_vec2": false, "min": [0.0,0.0], "max": [1.0,0.0],
             "defaults": [1.0,0.0], "axis_points": [[0.0,1.0],[0.0]], "merge_mode": "Additive",
             "bindings": []},
            {"groupUUID": 40, "name": "Extras", "children": [
                {"uuid": OTHER, "name": "Other", "is_vec2": false, "min": [0.0,0.0], "max": [1.0,0.0],
                 "defaults": [0.0,0.0], "axis_points": [[0.0,1.0],[0.0]], "merge_mode": "Additive",
                 "bindings": []}]},
        ],
        "animations": {
            "Talk": clip(8, json!({})),
            "Short": clip(4, json!({})),
            "Muted": clip(8, json!({"audio": [{"path": wav, "frame": 2, "mute": true}]})),
            "HiddenTrack": clip(8, json!({
                "groups": [{"name": "H", "x": 0.0, "y": 0.0, "scale": 1.0, "opacity": 1.0, "hidden": true}],
                "audio": [{"path": wav, "frame": 2, "group": "H"}]})),
            "LockedTrack": clip(8, json!({
                "groups": [{"name": "L", "x": 0.0, "y": 0.0, "scale": 1.0, "opacity": 1.0, "locked": true}],
                "audio": [{"path": wav, "frame": 2, "group": "L"}]})),
            "LockedModel": clip(8, json!({
                "groups": [{"name": "M", "x": 0.0, "y": 0.0, "scale": 1.0, "opacity": 1.0, "locked": true}],
                "modelGroup": "M"})),
            "Blink": clip(8, json!({"eyeBlink": {"keyframes": [{"frame": 3, "value": 0.0}]}})),
            "Stepped": clip(8, json!({"lanes": [{"uuid": MOUTH, "target": 0, "interpolation": "Stepped",
                "merge_mode": "Forced", "keyframes": [{"frame": 0, "value": 0.0, "tension": 0.5}]}]})),
            "Weighted": clip(8, json!({"lanes": [{"uuid": MOUTH, "target": 0, "interpolation": "Linear",
                "merge_mode": "Weighted", "keyframes": [{"frame": 0, "value": 0.0, "tension": 0.5}]}]})),
            "Additive": clip(8, json!({"additive": true})),
            "Halved": clip(8, json!({"animationWeight": 0.5})),
            "Frozen": clip(8, json!({"freezeFrame": 2})),
            "Hold": clip(8, json!({"frameStep": {"keyframes": [{"frame": 0, "value": 4}]}})),
            "Broken": clip(8, json!({"lanes": [{"uuid": MOUTH, "target": 0, "interpolation": "Linear",
                "merge_mode": "Forced", "keyframes": {"oops": true}}]})),
            "Keep": clip(8, json!({"lanes": [
                {"uuid": MOUTH, "target": 0, "interpolation": "Linear", "merge_mode": "Forced",
                 "keyframes": [{"frame": 0, "value": 0.42, "tension": 0.7, "note": "keep"}]},
                {"uuid": JAW, "target": 0, "interpolation": "Linear", "merge_mode": "Forced",
                 "keyframes": [{"frame": 1, "value": -0.33, "tension": 0.9}]}]})),
            "Tiny": clip(8, json!({"timestep": 0.000000001})),
        },
    })
}

fn open_session(dir: &Path, wav: &str) -> (Session, PathBuf) {
    let file = dir.join("fixture.inx");
    fs::write(&file, inx_bytes(&model(wav), &[]).unwrap()).unwrap();
    let mut session = Session::new();
    ok(&mut session, json!({"op": "open", "path": file}));
    (session, file)
}

fn ok(session: &mut Session, request: Value) -> Value {
    let response = session.handle_request(&request);
    assert_eq!(response["succeeded"], true, "{request}: {response}");
    response["result"].clone()
}

fn fail(session: &mut Session, request: Value) -> String {
    let response = session.handle_request(&request);
    assert_eq!(response["succeeded"], false, "{request}: {response}");
    response["message"].as_str().unwrap().to_owned()
}

fn history_count(session: &mut Session) -> u64 {
    ok(session, json!({"op": "history.summary"}))["count"].as_u64().unwrap()
}

fn analyze(session: &mut Session, wav: &str) -> Value {
    let started = ok(session, json!({"op": "animation.motionsync.start", "path": wav}));
    assert_eq!(started["state"], "running");
    assert_eq!(started["backend"], "rhubarb");
    let job = started["job"].as_u64().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = ok(session, json!({"op": "animation.motionsync.status", "job": job}));
        if status["state"] != "running" {
            return status;
        }
        assert!(Instant::now() < deadline, "the analysis did not settle");
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn ready_job(session: &mut Session, wav: &str) -> u64 {
    let status = analyze(session, wav);
    assert_eq!(status["state"], "finished", "{status}");
    assert_eq!(status["duration"], 0.5);
    assert_eq!(
        status["cues"].as_array().unwrap().iter().map(|cue| cue["value"].as_str().unwrap()).collect::<Vec<_>>(),
        ["X", "D", "F", "X"]
    );
    status["job"].as_u64().unwrap()
}

fn mappings() -> Value {
    json!([
        {"parameter": MOUTH, "axis": 0,
         "shapes": {"A":0.0,"B":0.0,"C":0.0,"D":1.0,"E":0.0,"F":0.25,"G":0.0,"H":0.0,"X":0.0}},
        {"parameter": JAW, "axis": 1,
         "shapes": {"A":0.0,"B":0.0,"C":0.0,"D":0.5,"E":0.0,"F":-0.75,"G":0.0,"H":0.0,"X":0.0}},
    ])
}

fn bake(name: &str, job: u64) -> Value {
    json!({"op": "animation.motionsync.bake", "name": name, "job": job, "audio_index": 0,
           "mappings": mappings()})
}

fn lane(clip: &Value, id: u32, axis: u64) -> Option<Value> {
    clip["lanes"].as_array()?.iter().find(|lane| {
        lane["uuid"].as_u64() == Some(u64::from(id)) && lane["target"].as_u64() == Some(axis)
    }).cloned()
}

fn keys(lane: &Value) -> Vec<(i64, f64)> {
    lane["keyframes"].as_array().unwrap().iter()
        .map(|key| (key["frame"].as_i64().unwrap(), key["value"].as_f64().unwrap()))
        .collect()
}

#[test]
fn request_fields_are_validated_without_a_worker() {
    let mut session = Session::new();
    ok(&mut session, json!({"op": "new"}));
    let count = history_count(&mut session);
    for key in ["program", "args", "output", "timeout"] {
        let message = fail(&mut session, json!({"op": "animation.motionsync.start", "path": "x", key: "y"}));
        assert!(message.contains("host setting"), "{key}: {message}");
    }
    let message = fail(&mut session, json!({"op": "animation.motionsync.start", "path": "x", "recognizer": "gm"}));
    assert!(message.contains("phonetic or pocketSphinx"), "{message}");
    let message = fail(&mut session, json!({"op": "animation.motionsync.start", "path": "x"}));
    assert!(message.contains("was not found"), "{message}");
    let message = fail(&mut session, json!({"op": "animation.motionsync.status", "job": 9}));
    assert!(message.contains("not known"), "{message}");
    let message = fail(&mut session, json!({"op": "animation.motionsync.bake", "name": "Talk", "job": 9,
        "audio_index": 0, "mappings": mappings()}));
    assert!(message.contains("not known"), "{message}");
    assert_eq!(history_count(&mut session), count, "refusals write no history");
}

#[cfg(unix)]
mod analyzer {
    use super::*;
    use std::process::Command;

    fn stub_pid(dir: &Path, name: &str) -> Option<String> {
        fs::read_to_string(dir.join(format!("pid-{name}.txt"))).ok().map(|pid| pid.trim().to_owned())
    }

    fn wait_pid(dir: &Path, name: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(pid) = stub_pid(dir, name).filter(|pid| !pid.is_empty()) {
                return pid;
            }
            assert!(Instant::now() < deadline, "the stub did not record its pid");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn assert_gone(pid: &str) {
        let alive = Command::new("kill").args(["-0", pid]).stderr(std::process::Stdio::null()).status();
        assert!(!alive.unwrap().success(), "process {pid} is still around");
    }

    fn ready_session(dir: &Path, wav: &str, stub_name: &str) -> (Session, u64) {
        let (mut session, _) = open_session(dir, wav);
        session.set_motion_sync_analyzer(&stub(dir, stub_name, FIXTURE));
        let job = ready_job(&mut session, wav);
        (session, job)
    }

    #[test]
    fn bake_keys_the_fixture_cues_and_survives_save() {
        let dir = temp_dir("bake");
        let wav = wav(&dir, "voice.wav");
        let (mut session, job) = ready_session(&dir, &wav, "fake-rhubarb");
        let result = ok(&mut session, bake("Talk", job));
        assert_eq!((result["from"].as_i64().unwrap(), result["to"].as_i64().unwrap()), (2, 6));
        assert_eq!(result["keys"], 10);
        assert_eq!(result["frames"], 5);
        let clip = session.snapshot()["model"]["animations"]["Talk"].clone();
        for (id, axis, values) in [
            (MOUTH, 0, [0.0, 1.0, 0.25, 0.0, 0.0]),
            (JAW, 1, [0.0, 0.5, -0.75, 0.0, 0.0]),
        ] {
            let lane = lane(&clip, id, axis).expect("the mapped lane exists");
            assert_eq!(lane["interpolation"], "Linear");
            assert_eq!(lane["merge_mode"], "Forced");
            assert_eq!(keys(&lane).iter().map(|key| key.0).collect::<Vec<_>>(), [2, 3, 4, 5, 6]);
            assert_eq!(keys(&lane).iter().map(|key| key.1).collect::<Vec<_>>(), values);
        }
        assert!(lane(&clip, JAW, 0).is_none(), "the other axis keeps no lane");
        let baked = session.snapshot()["model"].clone();
        ok(&mut session, json!({"op": "undo"}));
        assert!(session.snapshot()["model"]["animations"]["Talk"]["lanes"].as_array().unwrap().is_empty());
        ok(&mut session, json!({"op": "redo"}));
        assert_eq!(session.snapshot()["model"], baked);
        let file = dir.join("fixture.inx");
        ok(&mut session, json!({"op": "save", "path": file}));
        let mut reopened = Session::new();
        ok(&mut reopened, json!({"op": "open", "path": file}));
        assert_eq!(reopened.snapshot()["model"]["animations"]["Talk"]["lanes"], clip["lanes"]);
        assert!(fail(&mut reopened, json!({"op": "animation.motionsync.status", "job": 1})).contains("not known"));
        let count = history_count(&mut session);
        let again = ok(&mut session, bake("Talk", job));
        assert_eq!(again["keys"], 10);
        assert_eq!(session.snapshot()["model"], baked);
        assert_eq!(history_count(&mut session), count);
    }

    #[test]
    fn a_partial_range_and_extend_behave_atomically() {
        let dir = temp_dir("range");
        let wav = wav(&dir, "voice.wav");
        let (mut session, _) = ready_session(&dir, &wav, "fake-rhubarb");
        let mut range = bake("Talk", 1);
        range["from"] = json!(3);
        range["to"] = json!(4);
        let result = ok(&mut session, range);
        assert_eq!(result["keys"], 4);
        let clip = session.snapshot()["model"]["animations"]["Talk"].clone();
        assert_eq!(keys(&lane(&clip, MOUTH, 0).unwrap()), [(3, 1.0), (4, 0.25)]);

        let mut short = bake("Short", 1);
        let before = session.snapshot();
        let message = fail(&mut session, short.clone());
        assert!(message.contains("extend"), "{message}");
        assert_eq!(session.snapshot(), before, "the refused bake changed nothing");
        short["extend"] = json!(true);
        let result = ok(&mut session, short);
        assert_eq!(result["to"], 6);
        let clip = session.snapshot()["model"]["animations"]["Short"].clone();
        assert_eq!(clip["length"], 7);
        assert_eq!(keys(&lane(&clip, MOUTH, 0).unwrap()).len(), 5);
        ok(&mut session, json!({"op": "undo"}));
        let clip = session.snapshot()["model"]["animations"]["Short"].clone();
        assert_eq!(clip["length"], 4, "undo also restores the clip length");
        assert!(clip["lanes"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_changed_source_or_track_refuses_the_bake() {
        let dir = temp_dir("stale");
        let wav = wav(&dir, "voice.wav");
        let (mut session, _) = ready_session(&dir, &wav, "fake-rhubarb");
        fs::write(&wav, b"RIFF\x24\0\0\0WAVEfmt different bytes here").unwrap();
        let before = session.snapshot();
        let count = history_count(&mut session);
        let message = fail(&mut session, bake("Talk", 1));
        assert!(message.contains("changed since the analysis"), "{message}");
        assert_eq!(session.snapshot(), before);
        assert_eq!(history_count(&mut session), count);
    }

    #[test]
    fn muted_hidden_and_locked_tracks_refuse() {
        let dir = temp_dir("tracks");
        let wav = wav(&dir, "voice.wav");
        let (mut session, _) = ready_session(&dir, &wav, "fake-rhubarb");
        for (clip, part) in [
            ("Muted", "muted"),
            ("HiddenTrack", "hidden group"),
            ("LockedTrack", "locked group"),
            ("LockedModel", "locked group"),
            ("Talk", ""), // control: unblocked
        ] {
            let before = session.snapshot();
            let response = session.handle_request(&bake(clip, 1));
            if part.is_empty() {
                assert_eq!(response["succeeded"], true, "{clip}: {response}");
            } else {
                assert_eq!(response["succeeded"], false, "{clip}: {response}");
                assert!(response["message"].as_str().unwrap().contains(part), "{clip}: {response}");
                assert_eq!(session.snapshot(), before, "{clip} changed the document");
            }
        }
        let mut bad_index = bake("Talk", 1);
        bad_index["audio_index"] = json!(7);
        let message = fail(&mut session, bad_index);
        assert!(message.contains("no audio track 7"), "{message}");
    }

    #[test]
    fn frame_ranges_and_extend_are_validated() {
        let dir = temp_dir("ranges");
        let wav = wav(&dir, "voice.wav");
        let (mut session, _) = ready_session(&dir, &wav, "fake-rhubarb");
        for (request, part) in [
            (bake("Tiny", 1), "too long to cover"),          // absurd default end, not clamped
            ({ let mut r = bake("Talk", 1); r["to"] = json!(1_000_000); r }, "supported animation length"),
            ({ let mut r = bake("Short", 1); r["extend"] = json!("yes"); r }, "extend must be a boolean"),
            ({ let mut r = bake("Talk", 1); r["from"] = json!(5); r["to"] = json!(3); r },
             "from must not be after to"),
        ] {
            let before = session.snapshot();
            let count = history_count(&mut session);
            let message = fail(&mut session, request.clone());
            assert!(message.contains(part), "{request}: {message}");
            assert_eq!(session.snapshot(), before, "{request} changed the document");
            assert_eq!(history_count(&mut session), count);
        }
    }

    #[test]
    fn baking_preserves_keys_outside_the_range_and_other_lanes() {
        let dir = temp_dir("keep");
        let wav = wav(&dir, "voice.wav");
        let (mut session, _) = ready_session(&dir, &wav, "fake-rhubarb");
        let result = ok(&mut session, bake("Keep", 1));
        assert_eq!(result["keys"], 10);
        let clip = session.snapshot()["model"]["animations"]["Keep"].clone();
        let mouth = lane(&clip, MOUTH, 0).unwrap();
        let keys = mouth["keyframes"].as_array().unwrap();
        assert_eq!(keys[0]["frame"], 0);
        assert_eq!(keys[0]["value"], 0.42);
        assert_eq!(keys[0]["tension"], 0.7);
        assert_eq!(keys[0]["note"], "keep");
        assert_eq!(keys.len(), 6, "the five baked keys were added after it");
        assert_eq!(
            lane(&clip, JAW, 0).unwrap(),
            json!({"uuid": JAW, "target": 0, "interpolation": "Linear", "merge_mode": "Forced",
                   "keyframes": [{"frame": 1, "value": -0.33, "tension": 0.9}]}));
    }

    #[test]
    fn jobs_survive_open_and_the_settled_cap_evicts_oldest() {
        let dir = temp_dir("jobs");
        let wav = wav(&dir, "voice.wav");
        let (mut session, file) = open_session(&dir, &wav);
        session.set_motion_sync_analyzer(&stub(&dir, "fake-rhubarb", FIXTURE));
        let first = ready_job(&mut session, &wav);
        ok(&mut session, json!({"op": "open", "path": file}));
        let kept = ok(&mut session, json!({"op": "animation.motionsync.status", "job": first}));
        assert_eq!(kept["state"], "finished");
        assert_eq!(kept["duration"], 0.5);
        assert_eq!(ok(&mut session, bake("Talk", first))["keys"], 10);
        for _ in 0..4 {
            ready_job(&mut session, &wav);
        }
        assert!(fail(&mut session, json!({"op": "animation.motionsync.status", "job": first}))
            .contains("not known"));
        let last = ok(&mut session, json!({"op": "animation.motionsync.status", "job": first + 4}));
        assert_eq!(last["state"], "finished");
    }

    #[test]
    fn the_advertised_schema_matches_the_bake_contract() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../doc/commands.manifest.json");
        let manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let command = |id: &str| {
            manifest["commands"].as_array().unwrap().iter()
                .find(|command| command["id"] == id)
                .unwrap_or_else(|| panic!("{id} is not in the manifest"))
                .clone()
        };
        for id in [
            "animation_motionsync_start",
            "animation_motionsync_status",
            "animation_motionsync_cancel",
            "animation_motionsync_bake",
        ] {
            command(id);
        }
        let bake = command("animation_motionsync_bake");
        let properties = &bake["inputSchema"]["properties"];
        assert_eq!(properties["audio_index"]["minimum"], 0);
        assert_eq!(properties["from"]["minimum"], 0);
        assert_eq!(properties["to"]["minimum"], 0);
        assert_eq!(properties["job"]["minimum"], 1);
        assert!(properties["job"].get("maximum").is_none());
        let shapes = &properties["mappings"]["items"]["properties"]["shapes"];
        assert_eq!(shapes["additionalProperties"], false, "the core rejects unknown shape keys");
        for shape in ["A", "B", "C", "D", "E", "F", "G", "H", "X"] {
            assert!(shapes["properties"].get(shape).is_some(), "missing shape {shape}");
            assert!(shapes["required"].as_array().unwrap().contains(&json!(shape)));
        }
        assert_eq!(command("animation_motionsync_status")["inputSchema"]["properties"]["job"]["minimum"], 1);
    }

    #[test]
    fn mappings_are_validated_before_any_write() {
        let dir = temp_dir("mappings");
        let wav = wav(&dir, "voice.wav");
        let (mut session, _) = ready_session(&dir, &wav, "fake-rhubarb");
        let all_x = || json!({"A":0.0,"B":0.0,"C":0.0,"D":0.0,"E":0.0,"F":0.0,"G":0.0,"H":0.0,"X":0.0});
        let cases = [
            json!([{"parameter": 9999, "axis": 0, "shapes": all_x()}]),               // missing parameter
            json!([{"parameter": 40, "axis": 0, "shapes": all_x()}]),                 // a parameter group
            json!([{"parameter": MOUTH, "axis": 1, "shapes": all_x()}]),              // axis the param lacks
            json!([{"parameter": JAW, "axis": 2, "shapes": all_x()}]),
            json!([{"parameter": MOUTH, "axis": 0, "shapes": all_x()},
                   {"parameter": MOUTH, "axis": 0, "shapes": all_x()}]),              // duplicate lane
            json!([{"parameter": MOUTH, "axis": 0, "shapes": {"D": 1.0}}]),           // missing shapes
            json!([{"parameter": MOUTH, "axis": 0, "shapes": {"A":0.0,"B":0.0,"C":0.0,"D":0.0,"E":0.0,
                   "F":0.0,"G":0.0,"H":0.0,"X":0.0,"Q":0.0}}]),                       // unknown shape
            json!([{"parameter": MOUTH, "axis": 0, "shapes": {"A":9.0,"B":0.0,"C":0.0,"D":0.0,"E":0.0,
                   "F":0.0,"G":0.0,"H":0.0,"X":0.0}}]),                               // out of range
            json!([{"parameter": MOUTH, "axis": 0, "shapes": {"A":"x","B":0.0,"C":0.0,"D":0.0,"E":0.0,
                   "F":0.0,"G":0.0,"H":0.0,"X":0.0}}]),                               // not a number
            json!([]),                                                                // empty
        ];
        for (index, mappings) in cases.iter().enumerate() {
            let before = session.snapshot();
            let count = history_count(&mut session);
            let message = fail(&mut session, json!({"op": "animation.motionsync.bake", "name": "Talk",
                "job": 1, "audio_index": 0, "mappings": mappings}));
            assert!(!message.is_empty());
            assert_eq!(session.snapshot(), before, "case {index} changed the document: {message}");
            assert_eq!(history_count(&mut session), count);
        }
        let (mut running, _) = open_session(&dir, &wav);
        running.set_motion_sync_analyzer(&stub(&dir, "slow", "exec sleep 30"));
        let started = ok(&mut running, json!({"op": "animation.motionsync.start", "path": wav}));
        let job = started["job"].as_u64().unwrap();
        assert!(fail(&mut running, json!({"op": "animation.motionsync.bake", "name": "Talk", "job": job,
            "audio_index": 0, "mappings": mappings()})).contains("still running"));
        ok(&mut running, json!({"op": "animation.motionsync.cancel", "job": job}));
        let deadline = Instant::now() + Duration::from_secs(10);
        while ok(&mut running, json!({"op": "animation.motionsync.status", "job": job}))["state"]
            == "running"
        {
            assert!(Instant::now() < deadline, "the cancelled job did not settle");
            std::thread::sleep(Duration::from_millis(15));
        }
        assert!(fail(&mut running, json!({"op": "animation.motionsync.bake", "name": "Talk", "job": job,
            "audio_index": 0, "mappings": mappings()})).contains("cancelled"));
    }

    #[test]
    fn incompatible_clips_and_lanes_refuse_cleanly() {
        let dir = temp_dir("conflicts");
        let wav = wav(&dir, "voice.wav");
        let (mut session, _) = ready_session(&dir, &wav, "fake-rhubarb");
        for (clip, part) in [
            ("Stepped", "Linear and Forced"),
            ("Weighted", "Linear and Forced"),
            ("Additive", "additive"),
            ("Halved", "animationWeight"),
            ("Frozen", "freeze"),
            ("Hold", "frame stepping"),
        ] {
            let before = session.snapshot();
            let count = history_count(&mut session);
            let message = fail(&mut session, bake(clip, 1));
            assert!(message.contains(part), "{clip}: {message}");
            assert_eq!(session.snapshot(), before, "{clip} changed the document");
            assert_eq!(history_count(&mut session), count);
        }
        let mut blinky = bake("Blink", 1);
        blinky["mappings"] = json!([{"parameter": EYE, "axis": 0, "shapes":
            {"A":1.0,"B":1.0,"C":1.0,"D":1.0,"E":1.0,"F":1.0,"G":1.0,"H":1.0,"X":1.0}}]);
        let message = fail(&mut session, blinky);
        assert!(message.contains("eye parameter"), "{message}");
    }

    fn previews_match_after_refusal(a: &mut Session, b: &mut Session) {
        ok(a, json!({"op": "animation.advance", "dt": 0.25}));
        ok(b, json!({"op": "animation.advance", "dt": 0.25}));
        assert_eq!(
            a.snapshot()["animation"], b.snapshot()["animation"],
            "the failed bake disturbed the running preview"
        );
    }

    #[test]
    fn a_refused_bake_leaves_the_live_preview_untouched() {
        let dir = temp_dir("preview");
        let wav = wav(&dir, "voice.wav");
        let (mut a, _) = ready_session(&dir, &wav, "fake-a");
        let (mut b, _) = ready_session(&dir, &wav, "fake-b");
        for session in [&mut a, &mut b] {
            ok(session, json!({"op": "animation.seek", "name": "Talk", "frame": 0}));
            ok(session, json!({"op": "animation.advance", "dt": 0.125}));
        }
        let count = history_count(&mut a);
        let before = a.snapshot();
        assert!(fail(&mut a, bake("Stepped", 1)).contains("Linear and Forced"));
        assert_eq!(a.snapshot(), before);
        assert_eq!(history_count(&mut a), count);
        previews_match_after_refusal(&mut a, &mut b);
    }

    #[test]
    fn a_staged_write_failure_leaves_the_live_preview_untouched() {
        let dir = temp_dir("staged");
        let wav = wav(&dir, "voice.wav");
        let (mut a, _) = ready_session(&dir, &wav, "fake-a");
        let (mut b, _) = ready_session(&dir, &wav, "fake-b");
        for session in [&mut a, &mut b] {
            ok(session, json!({"op": "animation.seek", "name": "Talk", "frame": 0}));
            ok(session, json!({"op": "animation.advance", "dt": 0.125}));
        }
        let count = history_count(&mut a);
        let before = a.snapshot();
        let message = fail(&mut a, bake("Broken", 1));
        assert!(message.contains("keyframe"), "{message}");
        assert_eq!(a.snapshot(), before);
        assert_eq!(history_count(&mut a), count);
        previews_match_after_refusal(&mut a, &mut b);
    }

    #[test]
    fn analyzer_failures_and_terminal_states_are_reported() {
        let dir = temp_dir("failing");
        let wav = wav(&dir, "voice.wav");
        let (mut session, _) = open_session(&dir, &wav);
        session.set_motion_sync_analyzer(&dir.join("not-here").to_string_lossy());
        let status = analyze(&mut session, &wav);
        assert_eq!(status["state"], "failed");
        assert!(status["error"].as_str().unwrap().contains("cannot start"), "{status}");
        session.set_motion_sync_analyzer(&stub(&dir, "bad-exit", "echo 'nope' >&2\nexit 3"));
        let status = analyze(&mut session, &wav);
        assert_eq!(status["state"], "failed");
        assert!(status["error"].as_str().unwrap().contains("nope"), "{status}");
        session.set_motion_sync_analyzer(&stub(&dir, "not-json", "echo 'not json'"));
        assert_eq!(analyze(&mut session, &wav)["state"], "failed");
        session.set_motion_sync_analyzer(&stub(&dir, "bad-shape",
            "cat <<JSON\n{\"metadata\":{\"duration\":0.5},\"mouthCues\":[{\"start\":0.0,\"end\":0.5,\"value\":\"Q\"}]}\nJSON"));
        let status = analyze(&mut session, &wav);
        assert!(status["error"].as_str().unwrap().contains("one of"), "{status}");
        session.set_motion_sync_analyzer(&stub(&dir, "gap",
            "cat <<JSON\n{\"metadata\":{\"duration\":0.5},\"mouthCues\":[{\"start\":0.0,\"end\":0.1,\"value\":\"X\"},{\"start\":0.2,\"end\":0.5,\"value\":\"X\"}]}\nJSON"));
        let status = analyze(&mut session, &wav);
        assert!(status["error"].as_str().unwrap().contains("gap"), "{status}");
        session.set_motion_sync_analyzer(&stub(&dir, "flat",
            "cat <<JSON\n{\"metadata\":{\"duration\":0.5},\"mouthCues\":[{\"start\":0.0,\"end\":0.0,\"value\":\"X\"},{\"start\":0.0,\"end\":0.5,\"value\":\"X\"}]}\nJSON"));
        let status = analyze(&mut session, &wav);
        assert!(status["error"].as_str().unwrap().contains("after start"), "{status}");
        session.set_motion_sync_analyzer(&stub(&dir, "typed",
            "cat <<JSON\n{\"metadata\":{\"soundFile\":7,\"duration\":0.5},\"mouthCues\":[{\"start\":0.0,\"end\":0.5,\"value\":\"X\"}]}\nJSON"));
        let status = analyze(&mut session, &wav);
        assert!(status["error"].as_str().unwrap().contains("soundFile must be a string"), "{status}");
        let again = ok(&mut session, json!({"op": "animation.motionsync.status", "job": status["job"]}));
        assert_eq!(again, status);
        session.set_motion_sync_analyzer(&stub(&dir, "loud", "head -c 9000000 /dev/zero | tr '\\0' 'x'"));
        let status = analyze(&mut session, &wav);
        assert!(status["error"].as_str().unwrap().contains("limit"), "{status}");
        session.set_motion_sync_analyzer(&stub(&dir, "noisy",
            "cat <<JSON\n{\"metadata\":{\"duration\":0.5},\"mouthCues\":[{\"start\":0.0,\"end\":0.5,\"value\":\"X\"}]}\nJSON\nhead -c 70000 /dev/zero | tr '\\0' 'e' >&2"));
        let status = analyze(&mut session, &wav);
        assert_eq!(status["state"], "failed");
        assert!(status["error"].as_str().unwrap().contains("limit"), "{status}");
    }

    #[test]
    fn the_private_input_directory_and_files_are_owner_only() {
        let dir = temp_dir("modes");
        let wav = wav(&dir, "voice.wav");
        let (mut session, _) = open_session(&dir, &wav);
        session.set_motion_sync_analyzer(&stub(&dir, "modes", &format!(
            "tmp=$(dirname \"$last\")\n\
             if [ \"$(uname -s)\" = Darwin ]; then S='-f%Lp'; else S='-c%a'; fi\n\
             stat $S \"$tmp\" > '{}/dir-mode.txt'\n\
             stat $S \"$last\" > '{}/file-mode.txt'\n{FIXTURE}", dir.display(), dir.display())));
        assert_eq!(analyze(&mut session, &wav)["state"], "finished");
        assert_eq!(fs::read_to_string(dir.join("dir-mode.txt")).unwrap().trim(), "700");
        assert_eq!(fs::read_to_string(dir.join("file-mode.txt")).unwrap().trim(), "600");
    }

    #[test]
    fn a_second_start_is_refused_and_cancel_reaps_the_child() {
        let dir = temp_dir("cancel");
        let wav = wav(&dir, "voice.wav");
        let (mut session, _) = open_session(&dir, &wav);
        session.set_motion_sync_analyzer(&stub(&dir, "slow", "exec sleep 30"));
        let started = ok(&mut session, json!({"op": "animation.motionsync.start", "path": wav}));
        let job = started["job"].as_u64().unwrap();
        let pid = wait_pid(&dir, "slow");
        let message = fail(&mut session, json!({"op": "animation.motionsync.start", "path": wav}));
        assert!(message.contains("already running"), "{message}");
        let reply = ok(&mut session, json!({"op": "animation.motionsync.cancel", "job": job}));
        assert!(["running", "cancelled"].contains(&reply["state"].as_str().unwrap()), "{reply}");
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            let status = ok(&mut session, json!({"op": "animation.motionsync.status", "job": job}));
            if status["state"] != "running" {
                break status;
            }
            assert!(Instant::now() < deadline, "the cancelled job did not settle");
            std::thread::sleep(Duration::from_millis(15));
        };
        assert_eq!(status["state"], "cancelled");
        assert_gone(&pid);
        assert_eq!(ok(&mut session, json!({"op": "animation.motionsync.cancel", "job": job})), status);
    }

    #[test]
    fn dropping_the_session_reaps_the_child() {
        let dir = temp_dir("drop");
        let wav = wav(&dir, "voice.wav");
        let pid;
        {
            let (mut session, _) = open_session(&dir, &wav);
            session.set_motion_sync_analyzer(&stub(&dir, "slow", "exec sleep 30"));
            ok(&mut session, json!({"op": "animation.motionsync.start", "path": wav}));
            pid = wait_pid(&dir, "slow");
        }
        assert_gone(&pid);
    }

    #[test]
    fn an_overlong_analysis_times_out() {
        let dir = temp_dir("timeout");
        let wav = wav(&dir, "voice.wav");
        let (mut session, _) = open_session(&dir, &wav);
        session.set_motion_sync_analyzer(&stub(&dir, "slow", "exec sleep 30"));
        session.set_motion_sync_deadline(Duration::from_millis(300));
        let status = analyze(&mut session, &wav);
        assert_eq!(status["state"], "failed");
        assert!(status["error"].as_str().unwrap().contains("did not finish"), "{status}");
        if let Some(pid) = stub_pid(&dir, "slow").filter(|pid| !pid.is_empty()) {
            assert_gone(&pid);
        }
    }
}
