mod common;

use serde_json::{json, Value};
use soul2d_core::editor_controller::EditorController;
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "soul2d-history-test-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    common::scratch(path)
}

const MODEL: &str = r#"{
    "meta": { "name": "fixture", "version": "1.0-alpha" },
    "nodes": {
        "uuid": 1, "name": "Fixture", "type": "Node", "enabled": true,
        "zsort": 0.0,
        "transform": { "trans": [0.0,0.0,0.0], "rot": [0.0,0.0,0.0], "scale": [1.0,1.0] },
        "lockToRoot": false, "pinToMesh": false,
        "children": [{
            "uuid": 9, "name": "Root", "type": "Node", "enabled": true,
            "zsort": 0.0,
            "transform": { "trans": [0.0,0.0,0.0], "rot": [0.0,0.0,0.0], "scale": [1.0,1.0] },
            "lockToRoot": false, "pinToMesh": false
        }]
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

fn rename(session: &mut Session, name: &str) {
    let response = request(
        session,
        json!({"op": "node.rename", "id": 9, "name": name}),
    );
    assert_eq!(response["succeeded"], true, "{response}");
}

fn root_name(snapshot: &Value) -> &str {
    snapshot["model"]["nodes"]["children"][0]["name"].as_str().unwrap()
}

fn goto(session: &mut Session, index: u64, expected: u64) -> Value {
    request(
        session,
        json!({"op": "history.goto", "index": index, "expectedRevision": expected}),
    )
}

#[test]
fn initial_summary_and_three_renames() {
    let mut session = session();
    let history = &session.snapshot()["history"];
    assert_eq!(history["position"], 0);
    assert_eq!(history["count"], 0);
    assert_eq!(history["limit"], 100);
    assert_eq!(
        history["states"],
        json!([{ "index": 0, "label": "", "current": true, "applied": true }])
    );

    rename(&mut session, "First");
    rename(&mut session, "Second");
    rename(&mut session, "Third");
    let history = &session.snapshot()["history"];
    assert_eq!(history["position"], 3);
    assert_eq!(history["count"], 3);
    assert_eq!(history["states"].as_array().unwrap().len(), 4);
    assert_eq!(history["states"][3], json!({"index":3,"label":"node.rename","current":true,"applied":true}));
}

#[test]
fn goto_restores_once_and_truncates_redo_on_edit() {
    let mut session = session();
    rename(&mut session, "First");
    rename(&mut session, "Second");
    rename(&mut session, "Third");
    let before = session.snapshot();
    let revision = before["revision"].as_u64().unwrap();

    let snapshot = goto(&mut session, 0, revision);
    assert_eq!(snapshot["succeeded"], true, "{snapshot}");
    let snapshot = snapshot["result"].clone();
    assert_eq!(root_name(&snapshot), "Root");
    assert_eq!(snapshot["canRedo"], true);
    assert_eq!(snapshot["revision"], revision + 1);
    assert_eq!(snapshot["history"]["position"], 0);

    let revision = snapshot["revision"].as_u64().unwrap();
    let snapshot = result(&goto(&mut session, 1, revision)).clone();
    assert_eq!(root_name(&snapshot), "First");

    rename(&mut session, "Fourth");
    let history = session.snapshot()["history"].clone();
    assert_eq!(history["position"], 2);
    assert_eq!(history["count"], 2);
    assert_eq!(history["states"].as_array().unwrap().len(), 3);
    assert_eq!(session.snapshot()["canRedo"], false);
    assert_eq!(root_name(&session.snapshot()), "Fourth");
}

#[test]
fn goto_guards_leave_snapshot_unchanged() {
    let mut session = session();
    rename(&mut session, "First");
    let before = session.snapshot();
    let revision = before["revision"].as_u64().unwrap();

    let same = goto(&mut session, 1, revision);
    assert_eq!(same["succeeded"], true);
    assert_eq!(same["result"], before);

    let bad_requests = vec![
        json!({"op": "history.goto"}),
        json!({"op": "history.goto", "index": 0}),
        json!({"op": "history.goto", "index": 0, "expectedRevision": revision + 9}),
        json!({"op": "history.goto", "index": 5, "expectedRevision": revision}),
        json!({"op": "history.goto", "index": -1, "expectedRevision": revision}),
        json!({"op": "history.goto", "index": 0.5, "expectedRevision": revision}),
        json!({"op": "history.goto", "index": "0", "expectedRevision": revision}),
        json!({"op": "history.goto", "index": 0, "expectedRevision": "x"}),
        json!({"op": "history.goto", "index": 18446744073709551615u64,
            "expectedRevision": revision}),
    ];
    for bad in bad_requests {
        let response = request(&mut session, bad);
        assert_eq!(response["succeeded"], false, "{response}");
        assert_eq!(session.snapshot(), before);
    }
}

#[test]
fn eviction_retains_hundred_newest_states() {
    let mut session = session();
    for index in 1..=105 {
        rename(&mut session, &format!("State{index}"));
    }
    let history = &session.snapshot()["history"];
    assert_eq!(history["count"], 99);
    assert_eq!(history["position"], 99);
    assert_eq!(history["states"].as_array().unwrap().len(), 100);

    let revision = session.snapshot()["revision"].as_u64().unwrap();
    let snapshot = result(&goto(&mut session, 0, revision)).clone();
    assert_eq!(root_name(&snapshot), "State6");
}

#[test]
fn goto_preserves_clipboard_and_modified_tracks_saved_state() {
    let mut session = session();
    let directory = temp_dir();
    let saved = directory.join("history.inx");
    rename(&mut session, "First");
    result(&request(
        &mut session,
        json!({"op": "save", "path": saved.to_str().unwrap()}),
    ));
    rename(&mut session, "Second");
    assert_eq!(session.snapshot()["modified"], true);

    let revision = session.snapshot()["revision"].as_u64().unwrap();
    let jumped = result(&goto(&mut session, 1, revision)).clone();
    assert_eq!(root_name(&jumped), "First");
    assert_eq!(jumped["modified"], false);

    let revision = jumped["revision"].as_u64().unwrap();
    let jumped = result(&goto(&mut session, 0, revision)).clone();
    assert_eq!(root_name(&jumped), "Root");
    assert_eq!(jumped["modified"], true);

    result(&request(
        &mut session,
        json!({"op": "node.copy", "nodes": [9]}),
    ));
    let clipboard = session.snapshot()["clipboard"].clone();
    assert_ne!(clipboard, json!([]));
    let revision = jumped["revision"].as_u64().unwrap();
    let back = result(&goto(&mut session, 1, revision)).clone();
    assert_eq!(back["clipboard"], clipboard);

    let reopened = result(&request(
        &mut session,
        json!({"op": "open", "path": saved.to_str().unwrap()}),
    ))
    .clone();
    assert_eq!(reopened["history"]["position"], 0);
    assert_eq!(reopened["history"]["count"], 0);
    assert_eq!(reopened["history"]["states"].as_array().unwrap().len(), 1);
}

#[test]
fn history_ops_over_mcp() {
    let mut session = session();
    rename(&mut session, "First");
    rename(&mut session, "Second");
    let before = session.snapshot();
    let revision = before["revision"].as_u64().unwrap();

    let listed = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .unwrap();
    let tools = listed["result"]["tools"].as_array().unwrap();
    let summary_tool = tools
        .iter()
        .find(|tool| tool["name"] == "history_summary")
        .expect("history_summary tool");
    assert_eq!(summary_tool["inputSchema"]["required"], json!([]));
    let goto_tool = tools
        .iter()
        .find(|tool| tool["name"] == "history_goto")
        .expect("history_goto tool");
    assert_eq!(
        goto_tool["inputSchema"]["required"],
        json!(["index", "expectedRevision"])
    );

    let summary = session
        .handle_mcp(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "history_summary", "arguments": {}},
        }))
        .unwrap();
    let summary = &summary["result"]["structuredContent"];
    assert_eq!(summary["position"], 2);
    assert_eq!(summary["count"], 2);
    assert_eq!(summary["limit"], 100);
    assert_eq!(session.snapshot(), before);

    let jumped = session
        .handle_mcp(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "history_goto",
                "arguments": {"index": 0, "expectedRevision": revision},
            },
        }))
        .unwrap();
    let snapshot = &jumped["result"]["structuredContent"];
    assert_eq!(root_name(snapshot), "Root");
    assert_eq!(snapshot["revision"], revision + 1);

    let stale = session
        .handle_mcp(&json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "history_goto",
                "arguments": {"index": 2, "expectedRevision": revision},
            },
        }))
        .unwrap();
    assert_eq!(stale["result"]["isError"], true);
}

#[test]
fn controller_goto_pauses_playback_and_stales_drafts() {
    let mut controller = EditorController::new();
    let mut model: Value = serde_json::from_str(MODEL).unwrap();
    model["animations"] = json!({
        "Idle": {"length": 12, "timestep": 0.1, "additive": false,
            "animationWeight": 1.0, "leadIn": -1, "leadOut": -1, "lanes": []},
    });
    let response = controller.core_request(&json!({"op": "new", "model": model}));
    assert_eq!(response["succeeded"], true, "{response}");
    let response = controller.core_request(
        &json!({"op": "node.rename", "id": 9, "name": "First"}),
    );
    assert_eq!(response["succeeded"], true, "{response}");
    assert_eq!(controller.snapshot()["history"]["position"], 1);

    let response = controller.play("Idle");
    assert_eq!(response["succeeded"], true, "{response}");
    assert!(controller.playing());
    controller.select_node(9).unwrap();
    controller.set_draft_name("Draft".to_owned());
    assert!(controller.draft_dirty());
    assert!(!controller.draft_stale());

    let before = controller.snapshot().clone();
    let revision = before["revision"].as_u64().unwrap();

    let summary = controller.core_request(&json!({"op": "history.summary"}));
    assert_eq!(summary["succeeded"], true, "{summary}");
    assert_eq!(summary["result"], before["history"]);
    assert!(controller.playing());
    assert_eq!(controller.snapshot(), &before);

    let noop = controller.core_request(&json!({
        "op": "history.goto", "index": 1, "expectedRevision": revision,
    }));
    assert_eq!(noop["succeeded"], true, "{noop}");
    assert_eq!(noop["result"], before);
    assert!(controller.playing());
    assert!(!controller.draft_stale());

    let jumped = controller.core_request(&json!({
        "op": "history.goto", "index": 0, "expectedRevision": revision,
    }));
    assert_eq!(jumped["succeeded"], true, "{jumped}");
    assert_eq!(root_name(&jumped["result"]), "Root");
    assert_eq!(jumped["result"]["revision"], revision + 1);
    assert_eq!(controller.snapshot(), &jumped["result"]);
    assert!(!controller.playing());
    assert!(controller.draft_stale());

    let applied = controller.apply_draft();
    assert_eq!(applied["succeeded"], false, "{applied}");
    assert_eq!(controller.snapshot(), &jumped["result"]);
    assert!(controller.draft_dirty());
    assert!(controller.draft_stale());
}

#[test]
fn states_carry_their_operation_and_clear_keeps_only_the_current_document() {
    let mut session = session();
    rename(&mut session, "A");
    rename(&mut session, "B");
    result(&request(&mut session, json!({"op": "undo"})));
    let before = result(&request(&mut session, json!({"op": "snapshot"}))).clone();
    let labels: Vec<&str> = before["history"]["states"]
        .as_array()
        .unwrap()
        .iter()
        .map(|state| state["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["", "node.rename", "node.rename"]);
    let revision = before["revision"].as_u64().unwrap();

    let cleared = result(&request(&mut session, json!({"op": "history.clear"}))).clone();

    assert_eq!(root_name(&cleared), "A");
    assert_eq!(cleared["modified"], before["modified"]);
    assert_eq!(
        cleared["history"]["states"],
        json!([{ "index": 0, "label": "", "current": true, "applied": true }])
    );
    assert_eq!((cleared["canUndo"].clone(), cleared["canRedo"].clone()), (json!(false), json!(false)));
    assert_eq!(cleared["revision"], revision + 1);
    assert_eq!(goto(&mut session, 1, revision)["succeeded"], false);
    assert_eq!(request(&mut session, json!({"op": "undo"}))["message"], "nothing to undo");

    let again = result(&request(&mut session, json!({"op": "history.clear"}))).clone();
    assert_eq!(again["revision"], revision + 1);
    rename(&mut session, "C");
    let after = result(&request(&mut session, json!({"op": "history.summary"}))).clone();
    assert_eq!(after["states"][1]["label"], "node.rename");
    assert_eq!(after["position"], 1);
}

#[test]
fn the_history_limit_is_settable_trims_oldest_first_and_never_drops_the_current_state() {
    let mut session = session();
    for name in ["A", "B", "C", "D", "E"] {
        rename(&mut session, name);
    }
    let snapshot = result(&request(&mut session, json!({"op": "snapshot"}))).clone();
    assert_eq!((snapshot["history"]["limit"].clone(), snapshot["history"]["count"].clone()), (json!(100), json!(5)));
    for bad in [json!(1), json!(1001), json!(-3), json!("5"), json!(2.5), Value::Null] {
        let refused = request(&mut session, json!({"op": "history.limit.set", "limit": bad}));
        assert_eq!(refused["succeeded"], false, "{bad}");
        assert!(refused["message"].as_str().unwrap().contains("from 2 to 1000"), "{refused}");
    }
    assert_eq!(request(&mut session, json!({"op": "history.limit.set"}))["succeeded"], false);

    // Shrinking drops the oldest states and keeps the current one (the newest here).
    let shrunk = result(&request(&mut session, json!({"op": "history.limit.set", "limit": 3}))).clone();
    assert_eq!(shrunk["history"]["limit"], 3);
    assert_eq!((shrunk["history"]["count"].clone(), shrunk["history"]["position"].clone()), (json!(2), json!(2)));
    assert_eq!(root_name(&shrunk), "E");
    assert_eq!(shrunk["revision"].as_u64().unwrap(), snapshot["revision"].as_u64().unwrap() + 1);
    result(&request(&mut session, json!({"op": "undo"})));
    let undone = result(&request(&mut session, json!({"op": "undo"}))).clone();
    assert_eq!(root_name(&undone), "C", "two undo steps survive a limit of three states");
    assert_eq!(request(&mut session, json!({"op": "undo"}))["message"], "nothing to undo");

    // The current state is not dropped even when it is the oldest and the redo tail is longer than the limit.
    let tail = result(&request(&mut session, json!({"op": "history.limit.set", "limit": 2}))).clone();
    assert_eq!(root_name(&tail), "C");
    assert_eq!((tail["history"]["position"].clone(), tail["canUndo"].clone(), tail["canRedo"].clone()), (json!(0), json!(false), json!(true)));
    assert_eq!(tail["history"]["states"].as_array().unwrap().len(), 2);

    // New commits honour the limit, and growing it again makes room without restoring anything.
    rename(&mut session, "F");
    rename(&mut session, "G");
    let capped = result(&request(&mut session, json!({"op": "history.summary"}))).clone();
    assert_eq!(capped["states"].as_array().unwrap().len(), 2);
    let grown = result(&request(&mut session, json!({"op": "history.limit.set", "limit": 10}))).clone();
    assert_eq!((grown["history"]["limit"].clone(), grown["history"]["count"].clone()), (json!(10), json!(1)));
    let fresh = result(&request(&mut session, json!({"op": "new"}))).clone();
    assert_eq!(fresh["history"]["limit"], 10, "a new document keeps the session's history limit");
}
