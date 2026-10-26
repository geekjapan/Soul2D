mod common;

use serde_json::{json, Value};
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "soul2d-part-split-applied-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    common::scratch(dir)
}

fn write_png(dir: &PathBuf, name: &str, pixel: impl Fn(u32, u32) -> [u8; 4]) -> String {
    let mut image = image::RgbaImage::new(8, 8);
    for (x, y, px) in image.enumerate_pixels_mut() {
        *px = image::Rgba(pixel(x, y));
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    let path = dir.join(name);
    fs::write(&path, bytes.into_inner()).unwrap();
    path.to_str().unwrap().to_owned()
}

fn ok(session: &mut Session, op: &str, request: Value) -> Value {
    let mut call = json!({"op": op});
    call.as_object_mut().unwrap().extend(request.as_object().unwrap().clone());
    let response = session.handle_request(&call);
    assert_eq!(response["succeeded"], json!(true), "{op} failed: {response}");
    response["result"].clone()
}

fn fails(session: &mut Session, op: &str, request: Value) -> Value {
    let mut call = json!({"op": op});
    call.as_object_mut().unwrap().extend(request.as_object().unwrap().clone());
    let response = session.handle_request(&call);
    assert_eq!(response["succeeded"], json!(false), "{op} should refuse: {response}");
    response
}

fn split_session(dir: &PathBuf) -> (Session, String) {
    let path = write_png(dir, "source.png", |x, _| if x < 4 { [255, 0, 0, 255] } else { [0, 0, 255, 255] });
    let mut session = Session::new();
    ok(&mut session, "split.open", json!({"path": path}));
    (session, path)
}

fn left_right(session: &mut Session) {
    ok(session, "split.part.set", json!({"name": "Left", "steps": [{"op": "box", "rect": [0, 0, 4, 8]}]}));
    ok(session, "split.part.set", json!({"name": "Right", "steps": [{"op": "box", "rect": [4, 0, 4, 8]}]}));
}

fn applied(session: &Session) -> bool {
    let snapshot = session.snapshot();
    let value = snapshot["partSplit"].get("applied");
    assert!(value.is_some(), "partSplit.applied is missing");
    value.unwrap().as_bool().unwrap()
}

#[test]
fn split_apply_marks_matching_plan_as_applied() {
    let directory = temp_dir();
    let (mut session, _) = split_session(&directory);
    left_right(&mut session);
    assert_eq!(applied(&session), false, "draft before apply");
    let steps = session.snapshot()["history"]["count"].as_u64().unwrap();
    ok(&mut session, "split.apply", json!({}));
    assert_eq!(applied(&session), true, "after apply");
    assert_eq!(session.snapshot()["history"]["count"].as_u64().unwrap(), steps + 1);
    let _ = ok(&mut session, "undo", json!({}));
    assert_eq!(applied(&session), false, "undo unmarks");
    let _ = ok(&mut session, "redo", json!({}));
    assert_eq!(applied(&session), true, "redo marks again");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn applied_survives_rename_and_unrelated_edits() {
    let directory = temp_dir();
    let (mut session, _) = split_session(&directory);
    left_right(&mut session);
    ok(&mut session, "split.apply", json!({}));
    assert!(applied(&session));
    let left = session.snapshot()["model"]["nodes"]["children"].as_array().unwrap()
        .iter().find(|n| n["name"] == "Left").unwrap()["uuid"].as_u64().unwrap();
    ok(&mut session, "node.rename", json!({"id": left, "name": "Renamed"}));
    assert!(applied(&session), "rename keeps applied");
    ok(&mut session, "parameter.create", json!({"name": "Extra", "dimensions": 1}));
    assert!(applied(&session), "unrelated edit keeps applied");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn foreign_apply_reusing_ids_does_not_mark_a_different_draft() {
    let directory = temp_dir();
    let (mut session, _) = split_session(&directory);
    left_right(&mut session);
    let plan = ok(&mut session, "split.plan", json!({}))["plan"].clone();
    ok(&mut session, "split.apply", json!({}));
    assert!(applied(&session));
    let _ = ok(&mut session, "undo", json!({}));
    let mut foreign = plan.clone();
    foreign["parts"] = json!([{"name": "Only", "steps": [{"op": "box", "rect": [0, 0, 8, 8]}]}]);
    ok(&mut session, "split.apply", json!({"plan": foreign}));
    left_right(&mut session);
    assert!(!applied(&session), "the reused Part ids belong to a different plan");
    ok(&mut session, "split.part.set", json!({"name": "Only", "steps": [{"op": "box", "rect": [0, 0, 8, 8]}]}));
    ok(&mut session, "split.part.remove", json!({"name": "Left"}));
    ok(&mut session, "split.part.remove", json!({"name": "Right"}));
    assert!(applied(&session), "the surviving foreign plan does mark its own draft");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn partial_deletion_stays_applied_and_full_deletion_unmarks() {
    let directory = temp_dir();
    let (mut session, _) = split_session(&directory);
    left_right(&mut session);
    ok(&mut session, "split.apply", json!({}));
    let ids: Vec<u64> = session.snapshot()["model"]["nodes"]["children"].as_array().unwrap()
        .iter().map(|n| n["uuid"].as_u64().unwrap()).collect();
    assert_eq!(ids.len(), 2);
    ok(&mut session, "node.delete", json!({"id": ids[0]}));
    assert!(applied(&session), "one surviving Part keeps the record");
    ok(&mut session, "node.delete", json!({"id": ids[1]}));
    assert!(!applied(&session), "all gone unmarks");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn node_copy_paste_does_not_revive_origins() {
    let directory = temp_dir();
    let (mut session, _) = split_session(&directory);
    left_right(&mut session);
    ok(&mut session, "split.apply", json!({}));
    let ids: Vec<u64> = session.snapshot()["model"]["nodes"]["children"].as_array().unwrap()
        .iter().map(|n| n["uuid"].as_u64().unwrap()).collect();
    let parent = session.snapshot()["model"]["nodes"]["uuid"].as_u64().unwrap();
    ok(&mut session, "node.copy", json!({"nodes": [ids[0]]}));
    let copy = ok(&mut session, "node.paste", json!({"parent": parent, "index": 0}))["created"][0]["uuid"].clone();
    assert!(applied(&session), "copy+paste keeps the original live");
    ok(&mut session, "node.delete", json!({"id": ids[0]}));
    assert!(applied(&session), "the pasted copy is a new origin: deleting the original still leaves the second part");
    ok(&mut session, "node.delete", json!({"id": ids[1]}));
    assert!(!applied(&session));
    ok(&mut session, "node.copy", json!({"nodes": [copy]}));
    let pasted = ok(&mut session, "node.paste", json!({"parent": parent, "index": 0}));
    assert_eq!(pasted["created"][0]["uuid"], ids[0], "paste reuses the deleted original id");
    assert!(!applied(&session), "reused ids cannot revive the deleted application");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn replay_while_closed_then_identical_draft_marks_true() {
    let directory = temp_dir();
    let (mut session, path) = split_session(&directory);
    left_right(&mut session);
    let plan = ok(&mut session, "split.plan", json!({}))["plan"].clone();
    ok(&mut session, "split.close", json!({}));
    let mut fresh = Session::new();
    ok(&mut fresh, "split.apply", json!({"plan": plan}));
    assert!(!applied(&fresh), "no open draft is never applied");
    ok(&mut fresh, "split.open", json!({"path": path}));
    left_right(&mut fresh);
    assert!(applied(&fresh), "an identical draft matches the replayed plan");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn changed_draft_details_unmark_and_restore() {
    let directory = temp_dir();
    let (mut session, _) = split_session(&directory);
    left_right(&mut session);
    ok(&mut session, "split.apply", json!({}));
    ok(&mut session, "split.part.order", json!({"names": ["Right", "Left"]}));
    assert!(!applied(&session), "reordered draft is a different plan");
    ok(&mut session, "split.part.order", json!({"names": ["Left", "Right"]}));
    assert!(applied(&session), "restored order matches again");
    ok(&mut session, "split.part.set", json!({"name": "Left", "steps": [{"op": "box", "rect": [0, 0, 2, 8]}]}));
    assert!(!applied(&session), "changed steps unmark");
    ok(&mut session, "split.part.set", json!({"name": "Left", "steps": [{"op": "box", "rect": [0, 0, 4, 8]}]}));
    assert!(applied(&session));
    ok(&mut session, "split.part.set", json!({"name": "Left", "steps": [{"op": "box", "rect": [0, 0, 4, 8]}], "padding": 3}));
    assert!(!applied(&session), "different padding is a different plan");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn thirty_three_applications_do_not_cap_and_older_plans_stay_known() {
    let directory = temp_dir();
    let (mut session, path) = split_session(&directory);
    let _ = path;
    let mut plans = Vec::new();
    for i in 0..33u32 {
        let mut fresh = Session::new();
        ok(&mut fresh, "split.open", json!({"path": write_png(&directory, &format!("s{i}.png"), |x, _| if x < 4 { [255, 0, 0, 255] } else { [0, 0, 255, 255] })}));
        ok(&mut fresh, "split.part.set", json!({"name": format!("P{i}"), "steps": [{"op": "box", "rect": [0, 0, 8, 8]}]}));
        let plan = ok(&mut fresh, "split.plan", json!({}))["plan"].clone();
        ok(&mut session, "split.apply", json!({"plan": plan}));
        plans.push(plan);
    }
    let first = &plans[0];
    ok(&mut session, "split.open", json!({"path": &first["source"]["path"]}));
    ok(&mut session, "split.part.set", json!({"name": "P0", "steps": [{"op": "box", "rect": [0, 0, 8, 8]}]}));
    assert!(applied(&session), "the first of 33 applications still marks");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn failed_apply_changes_nothing() {
    let directory = temp_dir();
    let (mut session, _) = split_session(&directory);
    left_right(&mut session);
    let before = session.snapshot();
    fails(&mut session, "split.apply", json!({"plan": {"version": 99}}));
    assert_eq!(session.snapshot(), before);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn history_limit_and_clear_preserve_live_origins() {
    let directory = temp_dir();
    let (mut session, _) = split_session(&directory);
    left_right(&mut session);
    ok(&mut session, "history.limit.set", json!({"limit": 2}));
    ok(&mut session, "split.apply", json!({}));
    assert!(applied(&session));
    ok(&mut session, "parameter.create", json!({"name": "A", "dimensions": 1}));
    ok(&mut session, "parameter.create", json!({"name": "B", "dimensions": 1}));
    assert!(applied(&session), "limit pruning keeps the live record");
    ok(&mut session, "history.clear", json!({}));
    assert!(applied(&session), "history clear keeps the live record");
    fails(&mut session, "undo", json!({}));
    assert!(applied(&session), "cleared history cannot drop the live origins");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn save_keeps_applied_and_open_resets() {
    let directory = temp_dir();
    let (mut session, source) = split_session(&directory);
    left_right(&mut session);
    ok(&mut session, "split.apply", json!({}));
    assert!(applied(&session));
    let saved = directory.join("saved.inx");
    ok(&mut session, "save", json!({"path": saved.to_str().unwrap()}));
    assert!(applied(&session), "saving keeps the in-session record");
    ok(&mut session, "open", json!({"path": saved.to_str().unwrap()}));
    assert!(!applied(&session), "a reopened document has no in-memory origins");
    ok(&mut session, "split.open", json!({"path": source}));
    left_right(&mut session);
    assert!(!applied(&session), "reopening the same draft does not restore saved origins");
    ok(&mut session, "split.apply", json!({}));
    assert!(applied(&session));
    ok(&mut session, "import.inp", json!({"path": saved}));
    ok(&mut session, "split.open", json!({"path": source}));
    left_right(&mut session);
    assert!(!applied(&session), "import resets origins even with the same draft and ids");
    ok(&mut session, "new", json!({}));
    assert!(!applied(&session), "new resets");
    let mut other = Session::new();
    ok(&mut other, "open", json!({"path": saved.to_str().unwrap()}));
    assert!(!applied(&other), "imported/opened documents never carry origins");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn plan_tool_and_snapshot_agree_on_applied() {
    let directory = temp_dir();
    let (mut session, _) = split_session(&directory);
    left_right(&mut session);
    let before = session.snapshot();
    let plan = ok(&mut session, "split.plan", json!({}));
    assert_eq!(plan["applied"], json!(false), "plan tool agrees before apply");
    assert_eq!(session.snapshot(), before, "plan read is read-only");
    ok(&mut session, "split.apply", json!({}));
    let plan = ok(&mut session, "split.plan", json!({}));
    assert_eq!(plan["applied"], json!(true));
    assert_eq!(applied(&session), plan["applied"].as_bool().unwrap());
    let before = session.snapshot();
    let response = session.handle_mcp(&json!({"jsonrpc":"2.0", "id":1, "method":"resources/read",
        "params":{"uri":"resource://soul2d/part-split/plan"}})).unwrap();
    let resource: Value = serde_json::from_str(response["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(resource["applied"], true);
    assert_eq!(resource["plan"], plan["plan"]);
    assert_eq!(session.snapshot(), before);
    fs::remove_dir_all(directory).unwrap();
}
