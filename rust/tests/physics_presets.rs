mod common;

use serde_json::{json, Value};
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const PRESET_FILE: &str = "physics-presets.json";

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "soul2d-physics-presets-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    common::scratch(path)
}

fn call(session: &mut Session, op: &str, mut body: Value) -> Value {
    body["op"] = json!(op);
    session.handle_request(&body)
}

fn ok(session: &mut Session, op: &str, body: Value) -> Value {
    let reply = call(session, op, body);
    assert_eq!(reply["succeeded"], true, "{op}: {reply}");
    reply["result"].clone()
}

fn message(session: &mut Session, op: &str, body: Value) -> String {
    let reply = call(session, op, body);
    assert_eq!(reply["succeeded"], false, "{op}: {reply}");
    reply["message"].as_str().unwrap().to_owned()
}

/// A document with the SimplePhysics nodes 2 and 3 and the plain node 4, and a settings directory of its own
/// that does not exist yet.
fn session() -> (Session, PathBuf) {
    let dir = temp_dir().join("settings");
    let mut session = Session::new();
    session.set_config_dir(dir.clone());
    for (class, name, id) in [("SimplePhysics", "Hair", 2), ("SimplePhysics", "Skirt", 3), ("Node", "Plain", 4)] {
        let created = ok(&mut session, "node.create", json!({"className": class, "name": name}));
        assert_eq!(created["created"][0]["uuid"], id);
    }
    (session, dir)
}

/// The settings of a SimplePhysics node, without what every node has.
fn settings(session: &mut Session, id: u64) -> Value {
    let node = ok(session, "node.get", json!({"id": id}));
    let keys = [
        "param", "model_type", "map_mode", "gravity", "length", "frequency", "angle_damping", "length_damping",
        "output_scale", "local_only",
    ];
    Value::Object(keys.iter().map(|key| ((*key).to_owned(), node[*key].clone())).collect())
}

fn defaults() -> Value {
    json!({
        "param": u32::MAX, "model_type": "Pendulum", "map_mode": "AngleLength", "gravity": 1.0, "length": 100.0,
        "frequency": 1.0, "angle_damping": 0.5, "length_damping": 0.5, "output_scale": [1.0, 1.0],
        "local_only": false,
    })
}

fn with(mut base: Value, changes: Value) -> Value {
    for (key, value) in changes.as_object().unwrap() {
        base[key] = value.clone();
    }
    base
}

fn presets(session: &mut Session) -> Value {
    ok(session, "physics.preset.list", json!({}))["presets"].clone()
}

#[test]
fn a_saved_preset_is_listed_and_sets_the_nodes_it_is_applied_to() {
    let (mut s, dir) = session();
    assert_eq!(presets(&mut s), json!({}));
    assert!(!dir.exists(), "a list creates nothing");
    let before = s.snapshot();

    // The settings are kept. The parameter and the node of the call are not.
    let hair = json!({"model_type": "SpringPendulum", "gravity": 2.5, "length": 80.0, "output_scale": [0.5, 2.0]});
    let saved = ok(&mut s, "physics.preset.save", json!({
        "name": "Hair", "model_type": "SpringPendulum", "gravity": 2.5, "length": 80.0, "output_scale": [0.5, 2.0],
        "param": 7, "frequency": null,
    }));
    assert_eq!(saved, json!({"name": "Hair", "preset": hair, "replaced": false}));
    assert_eq!(s.snapshot(), before, "a preset is no change of the document");
    assert_eq!(presets(&mut s), json!({"Hair": hair}));
    let stored: Value = serde_json::from_slice(&fs::read(dir.join(PRESET_FILE)).unwrap()).unwrap();
    assert_eq!(stored, json!({"presets": {"Hair": hair}}));
    let read = s
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "resources/read", "params": {
            "uri": "resource://soul2d/physics/presets",
        }}))
        .unwrap();
    let text = read["result"]["contents"][0]["text"].as_str().unwrap_or_else(|| panic!("{read}"));
    assert_eq!(serde_json::from_str::<Value>(text).unwrap(), json!({"presets": {"Hair": hair}}));

    // One call sets every node it names, in one undo step. What the preset does not hold stays.
    let applied = with(defaults(), hair.clone());
    assert_eq!(ok(&mut s, "physics.set", json!({"nodes": [2, 3], "preset": "Hair"})), json!({"nodes": [2, 3]}));
    assert_eq!(settings(&mut s, 2), applied);
    assert_eq!(settings(&mut s, 3), applied);
    ok(&mut s, "undo", json!({}));
    assert_eq!(settings(&mut s, 2), defaults());
    assert_eq!(settings(&mut s, 3), defaults());
    assert_eq!(s.snapshot()["canUndo"], true, "the three nodes of the fixture are still there");

    // A setting of the call wins, and one axis of the output scale replaces that axis of the preset.
    ok(&mut s, "physics.set", json!({"node": 2, "preset": "Hair", "gravity": 9.0, "output_scale_x": 3.0}));
    assert_eq!(settings(&mut s, 2), with(applied.clone(), json!({"gravity": 9.0, "output_scale": [3.0, 2.0]})));
    assert_eq!(settings(&mut s, 3), defaults());
    ok(&mut s, "undo", json!({}));
    assert_eq!(settings(&mut s, 2), defaults(), "the preset and the settings of the call are one undo step");
    // A preset that is null is a preset that is not there.
    ok(&mut s, "physics.set", json!({"node": 3, "preset": null, "length": 60.0}));
    assert_eq!(settings(&mut s, 3), with(defaults(), json!({"length": 60.0})));

    // A preset can be what a node holds now, with the settings of the call above it.
    ok(&mut s, "physics.set", json!({"node": 2, "preset": "Hair", "local_only": true, "map_mode": "XY"}));
    let held = json!({
        "model_type": "SpringPendulum", "map_mode": "XY", "gravity": 2.5, "length": 80.0, "frequency": 1.0,
        "angle_damping": 0.5, "length_damping": 0.5, "output_scale": [0.5, 2.0], "local_only": true,
    });
    let saved = ok(&mut s, "physics.preset.save", json!({"name": "Held", "node": 2}));
    assert_eq!(saved, json!({"name": "Held", "preset": held, "replaced": false}));
    let saved = ok(&mut s, "physics.preset.save", json!({"name": "Faster", "node": 2, "frequency": 3.0}));
    assert_eq!(saved["preset"], with(held.clone(), json!({"frequency": 3.0})));
    ok(&mut s, "physics.set", json!({"node": 3, "preset": "Faster"}));
    assert_eq!(settings(&mut s, 3), with(defaults(), with(held.clone(), json!({"frequency": 3.0}))));

    // Another host session reads the presets, and they outlive the document.
    let mut other = Session::new();
    other.set_config_dir(dir.clone());
    assert_eq!(presets(&mut other).as_object().unwrap().keys().collect::<Vec<_>>(), ["Faster", "Hair", "Held"]);
    ok(&mut s, "new", json!({}));
    assert_eq!(presets(&mut s)["Hair"], hair);

    // A saved name is replaced with overwrite only.
    let refused = message(&mut s, "physics.preset.save", json!({"name": "Hair", "gravity": 4.0}));
    assert_eq!(refused, "a preset named \"Hair\" is saved; pass overwrite to replace it");
    assert_eq!(presets(&mut other)["Hair"], hair);
    let saved = ok(&mut s, "physics.preset.save", json!({"name": "Hair", "gravity": 4.0, "overwrite": true}));
    assert_eq!(saved, json!({"name": "Hair", "preset": {"gravity": 4.0}, "replaced": true}));

    assert_eq!(ok(&mut s, "physics.preset.delete", json!({"name": "Hair"})), json!({"name": "Hair"}));
    assert_eq!(presets(&mut other).as_object().unwrap().keys().collect::<Vec<_>>(), ["Faster", "Held"]);
    assert_eq!(message(&mut s, "physics.preset.delete", json!({"name": "Hair"})), "no saved preset is named \"Hair\"");
}

#[test]
fn a_preset_that_cannot_be_used_or_saved_is_refused_and_nothing_changes() {
    let (mut s, dir) = session();
    ok(&mut s, "physics.preset.save", json!({"name": "Base", "gravity": 2.0}));
    let file = fs::read(dir.join(PRESET_FILE)).unwrap();
    let before = s.snapshot();
    let long = "x".repeat(65);
    let saves = [
        (json!({"name": "A"}), "a preset needs at least one setting"),
        // The parameter is no setting of a preset. What a preset cannot keep is refused rather than dropped.
        (json!({"name": "A", "param": 7}), "a preset needs at least one setting"),
        (json!({"name": "A", "gravity": 2.0, "output_scale_x": 2.0}), "a preset cannot keep output_scale_x"),
        (json!({"name": "A", "node": 2, "output_scale_y": 2.0}), "a preset cannot keep output_scale_y"),
        (json!({"name": "A", "gravity": 2.0, "nodes": [2]}), "a preset cannot keep nodes"),
        (json!({"name": "A", "gravity": 2.0, "preset": "Base"}), "a preset cannot keep preset"),
        (json!({"name": "A", "gravity": "strong"}), "gravity must contain finite numbers"),
        (json!({"name": "A", "model_type": "Spring"}), "unknown model_type: Spring"),
        (json!({"name": "A", "map_mode": 3}), "unknown map_mode"),
        (json!({"name": "A", "output_scale": [1.0]}), "output_scale must be a finite [x, y]"),
        (json!({"name": "A", "local_only": 1}), "local_only must be a boolean"),
        (json!({"name": "A", "node": 99}), "node 99 was not found"),
        (json!({"name": "A", "node": 4}), "node 4 is not SimplePhysics"),
        (json!({"name": "A", "node": "Hair"}), "a SimplePhysics node UUID is required"),
        (json!({"gravity": 2.0}), "name must be a string"),
        (json!({"name": "", "gravity": 2.0}), "name must be 1 to 64 characters"),
        (json!({"name": long, "gravity": 2.0}), "name must be 1 to 64 characters"),
        (json!({"name": " a", "gravity": 2.0}), "name must be 1 to 64 characters"),
        (json!({"name": "a\tb", "gravity": 2.0}), "name must be 1 to 64 characters"),
        (json!({"name": "Base", "gravity": 3.0}), "a preset named \"Base\" is saved; pass overwrite"),
        (json!({"name": "Base", "gravity": 3.0, "overwrite": "yes"}), "overwrite must be a boolean"),
    ];
    for (request, expected) in saves {
        let text = message(&mut s, "physics.preset.save", request.clone());
        assert!(text.starts_with(expected), "{request}: {text}");
        assert_eq!(fs::read(dir.join(PRESET_FILE)).unwrap(), file, "{request}");
    }
    let sets = [
        (json!({"node": 2, "preset": "Nope"}), "unknown physics preset \"Nope\""),
        (json!({"node": 2, "preset": 3}), "preset must be a string"),
        (json!({"node": 4, "preset": "Base"}), "node 4 is not SimplePhysics"),
        (json!({"nodes": [2, 99], "preset": "Base"}), "node 99 was not found"),
        (json!({"preset": "Base"}), "a SimplePhysics node UUID is required"),
        (json!({"node": 2, "nodes": [3], "preset": "Base"}), "physics.set accepts node or nodes, not both"),
        // The preset is good and the setting of the call is not: neither is written.
        (json!({"node": 2, "preset": "Base", "length": "long"}), "length must contain finite numbers"),
        (json!({"node": 2, "preset": "Base", "output_scale": [2.0, 2.0], "output_scale_x": 1.0}),
            "output_scale cannot be combined with one axis"),
    ];
    for (request, expected) in sets {
        assert_eq!(message(&mut s, "physics.set", request.clone()), expected, "{request}");
        assert_eq!(s.snapshot(), before, "{request}");
    }
    // An unknown name is shown as far as a name can be long.
    let long = message(&mut s, "physics.set", json!({"node": 2, "preset": "x".repeat(1000)}));
    assert_eq!(long, format!("unknown physics preset {:?}", "x".repeat(64)));

    // 64 presets are the most.
    for index in 1..64 {
        ok(&mut s, "physics.preset.save", json!({"name": format!("P{index}"), "length": index}));
    }
    assert_eq!(message(&mut s, "physics.preset.save", json!({"name": "P64", "length": 1})), "at most 64 presets can be saved");
    assert_eq!(presets(&mut s).as_object().unwrap().len(), 64);

    // A session without a settings directory has no preset.
    let mut bare = Session::new();
    ok(&mut bare, "node.create", json!({"className": "SimplePhysics", "name": "Hair"}));
    assert_eq!(presets(&mut bare), json!({}));
    for (op, request) in [("physics.preset.save", json!({"name": "A", "gravity": 2.0})), ("physics.preset.delete", json!({"name": "A"}))] {
        assert_eq!(message(&mut bare, op, request), "this host has no settings directory to keep presets in");
    }
    assert_eq!(message(&mut bare, "physics.set", json!({"node": 2, "preset": "Base"})), "unknown physics preset \"Base\"");
}

#[test]
fn a_preset_file_that_is_not_valid_is_refused_and_not_written_over() {
    let (mut s, dir) = session();
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(PRESET_FILE);
    let before = s.snapshot();
    let files: [(&[u8], &str); 5] = [
        (b"{", "EOF while parsing"),
        (br#"{"presets": []}"#, "presets must be an object"),
        (br#"{"presets": {"A": {}}}"#, "preset \"A\": a preset needs at least one setting"),
        (br#"{"presets": {"A": {"gravity": "x"}}}"#, "preset \"A\": gravity must contain finite numbers"),
        (br#"{"presets": {" a": {"gravity": 2}}}"#, "name must be 1 to 64 characters"),
    ];
    for (bytes, reason) in files {
        fs::write(&path, bytes).unwrap();
        let expected = format!("physics-presets.json of the settings directory is not valid: {reason}");
        let texts = [
            message(&mut s, "physics.preset.list", json!({})),
            message(&mut s, "physics.preset.save", json!({"name": "B", "gravity": 2.0})),
            message(&mut s, "physics.preset.delete", json!({"name": "B"})),
            message(&mut s, "physics.set", json!({"node": 2, "preset": "B"})),
        ];
        for text in texts {
            assert!(text.starts_with(&expected), "{reason}: {text}");
            assert!(!text.contains(dir.to_str().unwrap()), "the answer names no path: {text}");
        }
        assert_eq!(fs::read(&path).unwrap(), bytes, "{reason}: the file is kept");
        assert_eq!(s.snapshot(), before, "{reason}");
    }
    // A call without a preset does not read the file.
    ok(&mut s, "physics.set", json!({"node": 2, "gravity": 3.0}));
    assert_eq!(settings(&mut s, 2), with(defaults(), json!({"gravity": 3.0})));

    // A file written by hand is read; what is no setting of a preset is left out.
    fs::write(&path, br#"{"presets": {"Hand": {"gravity": 2, "param": 7, "note": "x"}}}"#).unwrap();
    assert_eq!(presets(&mut s), json!({"Hand": {"gravity": 2}}));
    ok(&mut s, "physics.set", json!({"node": 3, "preset": "Hand"}));
    assert_eq!(settings(&mut s, 3), with(defaults(), json!({"gravity": 2.0})));
}

#[test]
fn a_call_with_a_preset_and_one_setting_of_its_own_writes_both() {
    let (mut s, _) = session();
    let parameter = ok(&mut s, "parameter.create", json!({"name": "Sway", "dimensions": 1}))["uuid"].as_u64().unwrap();
    let hair = json!({"gravity": 2.5, "output_scale": [0.5, 2.0]});
    ok(&mut s, "physics.preset.save", json!({"name": "Hair", "gravity": 2.5, "output_scale": [0.5, 2.0]}));
    // The parameter is a setting of the call only: a preset never holds one.
    ok(&mut s, "physics.set", json!({"node": 2, "preset": "Hair", "param": parameter}));
    assert_eq!(settings(&mut s, 2), with(with(defaults(), hair.clone()), json!({"param": parameter})));
    ok(&mut s, "physics.set", json!({"node": 3, "preset": "Hair", "output_scale_y": 5.0}));
    assert_eq!(settings(&mut s, 3), with(with(defaults(), hair), json!({"output_scale": [0.5, 5.0]})));
    ok(&mut s, "physics.set", json!({"node": 3, "preset": "Hair", "output_scale_x": 7.0}));
    assert_eq!(settings(&mut s, 3)["output_scale"], json!([7.0, 2.0]));
}

#[test]
fn a_null_in_a_save_is_a_field_that_was_not_given() {
    let (mut s, _) = session();
    ok(&mut s, "physics.set", json!({"node": 2, "gravity": 3.5, "length": 40.0}));
    // A null setting next to a node is taken from the node, and a null node is no node.
    let saved = ok(&mut s, "physics.preset.save", json!({"name": "A", "node": 2, "gravity": null, "length": 9.0}));
    assert_eq!(saved["preset"]["gravity"], 3.5);
    assert_eq!(saved["preset"]["length"], 9.0);
    let saved = ok(&mut s, "physics.preset.save", json!({"name": "B", "node": null, "nodes": null, "gravity": 1.5}));
    assert_eq!(saved["preset"], json!({"gravity": 1.5}));
}

#[test]
fn the_presets_fill_the_file_to_its_last_byte_and_no_further() {
    let (mut s, dir) = session();
    let save = |s: &mut Session, zeros: usize| {
        let text = format!(r#"{{"name": "Long", "gravity": 1.{}, "overwrite": true}}"#, "0".repeat(zeros));
        call(s, "physics.preset.save", serde_json::from_str(&text).unwrap())
    };
    assert_eq!(save(&mut s, 1)["succeeded"], true);
    let zeros = 1 + (1 << 20) - fs::read(dir.join(PRESET_FILE)).unwrap().len();
    assert_eq!(save(&mut s, zeros)["succeeded"], true);
    let file = fs::read(dir.join(PRESET_FILE)).unwrap();
    assert_eq!(file.len(), 1 << 20);
    assert_eq!(presets(&mut s)["Long"]["gravity"], 1.0, "the largest file that is written is read again");
    assert_eq!(save(&mut s, zeros + 1)["message"], "the presets would be larger than 1048576 bytes");
    assert_eq!(fs::read(dir.join(PRESET_FILE)).unwrap(), file);
}
