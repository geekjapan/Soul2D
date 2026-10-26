mod common;

use serde_json::{json, Value};
use soul2d_core::autosave::{
    autosave_dir, discard, list_backups, restore, save_via_session, unclean_exit, Autosave, DocState, Now, Settings,
};
use soul2d_core::Session;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static TEMP_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

#[test]
fn consecutive_depthdraw_edits_get_distinct_backups_without_changing_document_history() {
    let mut f = fixture();
    ok(f.session.handle_request(&json!({"op": "depth.draw.manifest.set", "manifest": {
        "layers": [{"id": "a", "width": 1, "height": 1, "zOffset": 0.0}]
    }})));
    let settings = Settings::new(true, 1, 5);
    let mut autosave = autosave_for(&f);
    let first = autosave.tick(&settings, doc(&f.session), at(1, 1001),
        |path| save_via_session(&mut f.session, path)).unwrap().unwrap();
    let before = f.session.snapshot();
    ok(f.session.handle_request(&json!({"op": "depth.draw.layer.update", "layer": "a", "zOffset": 1.0})));
    let after = f.session.snapshot();
    assert_eq!(before["revision"], after["revision"]);
    assert_eq!(before["history"], after["history"]);
    assert_ne!(before["depthDraw"]["revision"], after["depthDraw"]["revision"]);
    let second = autosave.tick(&settings, doc(&f.session), at(2, 1002),
        |path| save_via_session(&mut f.session, path)).unwrap().unwrap();
    for (path, offset) in [(first, 0.0), (second, 1.0)] {
        let mut reopened = Session::new();
        ok(reopened.handle_request(&json!({"op": "open", "path": path})));
        let manifest = ok(reopened.handle_request(&json!({"op": "depth.draw.manifest.get"})));
        assert_eq!(manifest["manifest"]["layers"][0]["zOffset"], offset);
    }
    assert_eq!(fs::read(&f.project).unwrap(), f.original);
    assert!(doc(&f.session).modified);
    assert!(autosave.tick(&settings, doc(&f.session), at(3, 1003), |_| panic!("unchanged backup")).unwrap().is_none());
    rename(&mut f.session, "Another edit");
    assert!(autosave.tick(&settings, doc(&f.session), at(4, 1004),
        |path| save_via_session(&mut f.session, path)).unwrap().is_some());
    ok(f.session.handle_request(&json!({"op": "save", "path": f.project})));
    let mut reopened = Session::new();
    ok(reopened.handle_request(&json!({"op": "open", "path": f.project})));
    let manifest = ok(reopened.handle_request(&json!({"op": "depth.draw.manifest.get"})));
    assert_eq!(manifest["manifest"]["layers"][0]["zOffset"], 1.0);
}

#[test]
fn autosave_revision_components_do_not_collide_and_require_valid_snapshots() {
    let mut f = fixture();
    let mut autosave = autosave_for(&f);
    let settings = Settings::new(true, 1, 3);
    let first = DocState { revision: 1, depth_revision: 2, modified: true };
    autosave.tick(&settings, first, at(1, 1001), |path| save_via_session(&mut f.session, path)).unwrap().unwrap();
    assert!(autosave.is_due(&settings, DocState { revision: 2, depth_revision: 1, modified: true }, at(2, 1002)));
    assert!(DocState::from_snapshot(&json!({"revision": 1, "modified": true})).is_err());
    assert!(DocState::from_snapshot(&json!({"revision": 1, "modified": true, "depthDraw": {"revision": -1}})).is_err());
}

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "soul2d-autosave-test-{}-{}",
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
        "uuid": 1, "name": "Fixture", "type": "Node", "enabled": true, "zsort": 0.0,
        "transform": { "trans": [0.0,0.0,0.0], "rot": [0.0,0.0,0.0], "scale": [1.0,1.0] },
        "lockToRoot": false, "pinToMesh": false,
        "children": [{
            "uuid": 9, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
            "transform": { "trans": [0.0,0.0,0.0], "rot": [0.0,0.0,0.0], "scale": [1.0,1.0] },
            "lockToRoot": false, "pinToMesh": false
        }]
    },
    "param": [],
    "animations": {}
}"#;

fn ok(response: Value) -> Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    response["result"].clone()
}

fn rename(session: &mut Session, name: &str) {
    ok(session.handle_request(&json!({"op": "node.rename", "id": 9, "name": name})));
}

fn root_name(snapshot: &Value) -> String {
    snapshot["model"]["nodes"]["children"][0]["name"].as_str().unwrap().to_owned()
}

fn doc(session: &Session) -> DocState {
    DocState::from_snapshot(&session.snapshot()).unwrap()
}

fn at(minutes: u64, unix: u64) -> Now {
    Now { mono_secs: minutes * 60, unix_secs: unix }
}

/// A session saved to `<dir>/proj.inx` (named "Saved"), then renamed to "Edited" so it is modified.
struct Fixture {
    config: PathBuf,
    project: PathBuf,
    original: Vec<u8>,
    session: Session,
}

fn fixture() -> Fixture {
    let dir = temp_dir();
    let project = dir.join("proj.inx");
    let mut session = Session::new();
    let model: Value = serde_json::from_str(MODEL).unwrap();
    ok(session.handle_request(&json!({"op": "new", "model": model})));
    rename(&mut session, "Saved");
    ok(session.handle_request(&json!({"op": "save", "path": project.to_str().unwrap()})));
    let original = fs::read(&project).unwrap();
    rename(&mut session, "Edited");
    Fixture { config: dir.join("config"), project, original, session }
}

fn autosave_for(f: &Fixture) -> Autosave {
    let mut autosave = Autosave::new(&f.config, at(0, 1000));
    autosave.set_project(Some(f.project.clone()), at(0, 1000));
    autosave
}

fn stamps(config: &Path, project: &Path) -> Vec<u64> {
    list_backups(config, project).unwrap().iter().map(|b| b.stamp).collect()
}

#[test]
fn settings_are_clamped_like_d() {
    assert_eq!(Settings::default(), Settings { enabled: true, interval_minutes: 5, max_backups: 3 });
    assert_eq!(Settings::new(true, 0, 0), Settings { enabled: true, interval_minutes: 1, max_backups: 1 });
    assert_eq!(
        Settings::new(false, 99_999, 99_999),
        Settings { enabled: false, interval_minutes: 1440, max_backups: 1000 }
    );
}

#[test]
fn due_only_after_the_interval_and_only_when_modified_enabled_and_new_revision() {
    let f = fixture();
    let autosave = autosave_for(&f);
    let settings = Settings::new(true, 5, 3);
    let edited = DocState { revision: 4, depth_revision: 0, modified: true };
    assert!(!autosave.is_due(&settings, edited, Now { mono_secs: 299, unix_secs: 1299 }));
    assert!(autosave.is_due(&settings, edited, Now { mono_secs: 300, unix_secs: 1300 }));
    assert!(!autosave.is_due(&settings, DocState { revision: 4, depth_revision: 0, modified: false }, at(60, 5000)));
    assert!(!autosave.is_due(&Settings::new(false, 5, 3), edited, at(60, 5000)));
    // No project path (new unsaved document): D returns early.
    let anonymous = Autosave::new(&f.config, at(0, 1000));
    assert!(!anonymous.is_due(&settings, edited, at(60, 5000)));
}

#[test]
fn tick_writes_a_backup_without_touching_the_document_or_the_original_file() {
    let mut f = fixture();
    let mut autosave = autosave_for(&f);
    let settings = Settings::new(true, 5, 3);
    let before = f.session.snapshot();
    assert_eq!(before["modified"], true);

    let early = autosave.tick(&settings, doc(&f.session), at(4, 1240), |_| unreachable!("not due")).unwrap();
    assert_eq!(early, None);

    let written = autosave
        .tick(&settings, doc(&f.session), at(5, 1300), |path| save_via_session(&mut f.session, path))
        .unwrap()
        .expect("due");
    let expected = autosave_dir(&f.config, &f.project).unwrap().join("proj_1300.inx");
    assert_eq!(written.file_name(), expected.file_name());
    assert_eq!(written.parent().unwrap().canonicalize().unwrap(), expected.parent().unwrap().canonicalize().unwrap());
    assert!(unclean_exit(&f.config, &f.project));
    assert_eq!(fs::read(&f.project).unwrap(), f.original);

    // Document state is exactly as before.
    let after = f.session.snapshot();
    assert_eq!(after["modified"], true);
    assert_eq!(after["revision"], before["revision"]);
    assert_eq!(after["history"], before["history"]);

    // The backup holds the unsaved edit.
    let mut probe = Session::new();
    let opened = ok(probe.handle_request(&json!({"op": "open", "path": written.to_str().unwrap()})));
    assert_eq!(root_name(&opened), "Edited");

    // File path is still the project: a manual save over it still works and clears modified.
    ok(f.session.handle_request(&json!({"op": "save", "path": f.project.to_str().unwrap()})));
    assert_eq!(f.session.snapshot()["modified"], false);
    assert_ne!(fs::read(&f.project).unwrap(), f.original);
}

#[test]
fn not_due_again_until_the_revision_changes() {
    let mut f = fixture();
    let mut autosave = autosave_for(&f);
    let settings = Settings::new(true, 5, 3);
    let saved = |autosave: &mut Autosave, session: &mut Session, now| {
        let state = doc(session);
        autosave.tick(&settings, state, now, |path| save_via_session(session, path)).unwrap()
    };
    assert!(saved(&mut autosave, &mut f.session, at(5, 2000)).is_some());
    assert!(saved(&mut autosave, &mut f.session, at(11, 2360)).is_none(), "same revision, still modified");
    rename(&mut f.session, "Edited again");
    assert!(saved(&mut autosave, &mut f.session, at(9, 2240)).is_none(), "timer restarted at minute 5");
    assert!(saved(&mut autosave, &mut f.session, at(10, 2300)).is_some());
    assert_eq!(stamps(&f.config, &f.project), vec![2300, 2000]);
}

#[test]
fn rotation_keeps_at_most_the_cap_deleting_the_oldest_first() {
    let f = fixture();
    let mut autosave = autosave_for(&f);
    let settings = Settings::new(true, 1, 2);
    for unix in [100, 200, 300, 400] {
        autosave.autosave_now(&settings, at(0, unix), |path| fs::write(path, b"x").map_err(|e| e.to_string())).unwrap();
    }
    assert_eq!(stamps(&f.config, &f.project), vec![400, 300]);
    // Cap 1 trims an existing pair down before writing the new one.
    let one = Settings::new(true, 1, 1);
    autosave.autosave_now(&one, at(0, 500), |path| fs::write(path, b"x").map_err(|e| e.to_string())).unwrap();
    assert_eq!(stamps(&f.config, &f.project), vec![500]);
}

#[test]
fn same_second_and_backwards_clock_still_order_and_never_overwrite() {
    let f = fixture();
    let mut autosave = autosave_for(&f);
    let settings = Settings::new(true, 1, 5);
    let write = |path: &Path| fs::write(path, path.to_str().unwrap()).map_err(|e| e.to_string());
    for unix in [100, 100, 50] {
        autosave.autosave_now(&settings, at(0, unix), write).unwrap();
    }
    assert_eq!(stamps(&f.config, &f.project), vec![102, 101, 100]);
}

#[test]
fn failed_write_leaves_no_marker_and_waits_a_full_interval_before_retrying() {
    let f = fixture();
    let mut autosave = autosave_for(&f);
    let settings = Settings::new(true, 5, 3);
    let state = DocState { revision: 1, depth_revision: 0, modified: true };
    let error = autosave.tick(&settings, state, at(5, 1300), |_| Err("disk full".into())).unwrap_err();
    assert_eq!(error, "disk full");
    assert!(!unclean_exit(&f.config, &f.project));
    assert_eq!(autosave.tick(&settings, state, at(9, 1540), |_| unreachable!("retry too soon")).unwrap(), None);
    assert!(autosave.tick(&settings, state, at(10, 1600), |_| Ok(())).unwrap().is_some());
}

#[test]
fn unclean_exit_is_the_marker_and_clean_release_or_new_project_clears_it() {
    let mut f = fixture();
    let mut autosave = autosave_for(&f);
    assert!(!unclean_exit(&f.config, &f.project), "no marker before the first autosave");
    let settings = Settings::new(true, 1, 3);
    autosave.autosave_now(&settings, at(0, 10), |p| save_via_session(&mut f.session, p)).unwrap();
    assert!(unclean_exit(&f.config, &f.project));
    // A fresh process sees the marker without any in-memory state.
    assert!(unclean_exit(&f.config, &f.project));

    autosave.release_lock(); // clean exit or manual save
    assert!(!unclean_exit(&f.config, &f.project));
    assert_eq!(stamps(&f.config, &f.project), vec![10], "backups outlive the marker, like D");

    autosave.autosave_now(&settings, at(0, 20), |p| save_via_session(&mut f.session, p)).unwrap();
    assert!(unclean_exit(&f.config, &f.project));
    autosave.set_project(None, at(1, 30)); // File > New releases the old project's marker
    assert!(!unclean_exit(&f.config, &f.project));
    assert_eq!(fs::read(&f.project).unwrap(), f.original);
}

#[test]
fn restore_opens_the_newest_backup_and_never_writes_the_original() {
    let mut f = fixture();
    let mut autosave = autosave_for(&f);
    let settings = Settings::new(true, 1, 3);
    autosave.autosave_now(&settings, at(0, 10), |p| save_via_session(&mut f.session, p)).unwrap();
    rename(&mut f.session, "Newest edit");
    autosave.autosave_now(&settings, at(0, 20), |p| save_via_session(&mut f.session, p)).unwrap();

    let mut recovered = Session::new();
    let backup = restore(&mut recovered, &f.config, &f.project).unwrap();
    assert_eq!(backup.stamp, 20);
    assert_eq!(root_name(&recovered.snapshot()), "Newest edit");
    assert_eq!(recovered.snapshot()["modified"], true, "a restored document is unsaved work");
    assert_eq!(fs::read(&f.project).unwrap(), f.original);
    assert_eq!(stamps(&f.config, &f.project), vec![20, 10], "restore keeps the backups");
}

#[test]
fn restore_without_backups_or_with_a_corrupt_one_is_an_error_and_leaves_the_session_alone() {
    let mut f = fixture();
    let error = restore(&mut f.session, &f.config, &f.project).unwrap_err();
    assert_eq!(error, "no autosave backup to restore");

    let dir = autosave_dir(&f.config, &f.project).unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("proj_5.inx"), b"not an inx file").unwrap();
    // The session's own reason is reported, not the generic fallback.
    let error = restore(&mut f.session, &f.config, &f.project).unwrap_err();
    assert_ne!(error, "could not open backup");
    assert_eq!(root_name(&f.session.snapshot()), "Edited");
    assert_eq!(f.session.snapshot()["modified"], true);
    assert_eq!(fs::read(&f.project).unwrap(), f.original);
}

#[test]
fn discard_deletes_backups_and_marker_but_not_the_original_or_other_projects() {
    let mut f = fixture();
    let mut autosave = autosave_for(&f);
    let settings = Settings::new(true, 1, 3);
    for unix in [10, 20] {
        autosave.autosave_now(&settings, at(0, unix), |p| save_via_session(&mut f.session, p)).unwrap();
    }
    let other = f.config.join("autosaves").join("other");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("other_7.inx"), b"keep").unwrap();

    assert_eq!(discard(&f.config, &f.project).unwrap(), 2);
    assert!(!unclean_exit(&f.config, &f.project));
    assert!(stamps(&f.config, &f.project).is_empty());
    assert_eq!(fs::read(&f.project).unwrap(), f.original);
    assert_eq!(fs::read(other.join("other_7.inx")).unwrap(), b"keep");
    assert_eq!(discard(&f.config, &f.project).unwrap(), 0, "idempotent");
}

#[test]
fn native_extension_follows_the_project_and_unknown_ones_fall_back_to_inx() {
    let dir = temp_dir();
    let write = |p: &Path| fs::write(p, b"x").map_err(|e| e.to_string());
    let settings = Settings::new(true, 1, 3);
    for (name, expected) in [("rig.cmo3", "rig_7.cmo3"), ("anim.can3", "anim_7.can3"), ("odd.txt", "odd_7.inx")] {
        let project = dir.join(name);
        let mut autosave = Autosave::new(dir.join("config"), at(0, 7));
        autosave.set_project(Some(project.clone()), at(0, 7));
        let path = autosave.autosave_now(&settings, at(0, 7), write).unwrap();
        assert_eq!(path.file_name().unwrap().to_str().unwrap(), expected);
        assert_eq!(list_backups(&dir.join("config"), &project).unwrap().len(), 1);
    }
}

#[test]
fn unrelated_files_in_the_autosave_directory_are_ignored_by_listing_and_rotation() {
    let f = fixture();
    let dir = autosave_dir(&f.config, &f.project).unwrap();
    fs::create_dir_all(&dir).unwrap();
    for name in ["notes.txt", "proj_abc.inx", "proj.inx", "proj_9.inx.swp", ".proj_3.inx.tmp"] {
        fs::write(dir.join(name), b"stray").unwrap();
    }
    let mut autosave = autosave_for(&f);
    let settings = Settings::new(true, 1, 1);
    autosave.autosave_now(&settings, at(0, 5), |p| fs::write(p, b"b").map_err(|e| e.to_string())).unwrap();
    assert_eq!(stamps(&f.config, &f.project), vec![5]);
    for name in ["notes.txt", "proj_abc.inx", "proj.inx", "proj_9.inx.swp", ".proj_3.inx.tmp"] {
        assert!(dir.join(name).exists(), "{name} must survive rotation");
    }
}

#[test]
fn restored_document_saves_over_the_project_path_with_the_restored_content() {
    let mut f = fixture();
    let mut autosave = autosave_for(&f);
    let settings = Settings::new(true, 1, 3);
    autosave.autosave_now(&settings, at(0, 10), |p| save_via_session(&mut f.session, p)).unwrap();

    let mut recovered = Session::new();
    restore(&mut recovered, &f.config, &f.project).unwrap();
    ok(recovered.handle_request(&json!({"op": "save", "path": f.project.to_str().unwrap()})));
    assert_eq!(recovered.snapshot()["modified"], false);
    assert_ne!(fs::read(&f.project).unwrap(), f.original);
    let mut reopened = Session::new();
    let opened = ok(reopened.handle_request(&json!({"op": "open", "path": f.project.to_str().unwrap()})));
    assert_eq!(root_name(&opened), "Edited");
}

#[test]
fn restore_when_the_project_file_is_missing_saves_a_new_project_file() {
    let mut f = fixture();
    let mut autosave = autosave_for(&f);
    let settings = Settings::new(true, 1, 3);
    autosave.autosave_now(&settings, at(0, 10), |p| save_via_session(&mut f.session, p)).unwrap();
    fs::remove_file(&f.project).unwrap();

    let mut recovered = Session::new();
    restore(&mut recovered, &f.config, &f.project).unwrap();
    assert_eq!(recovered.snapshot()["modified"], true);
    ok(recovered.handle_request(&json!({"op": "save", "path": f.project.to_str().unwrap()})));
    let mut reopened = Session::new();
    let opened = ok(reopened.handle_request(&json!({"op": "open", "path": f.project.to_str().unwrap()})));
    assert_eq!(root_name(&opened), "Edited");
}

#[test]
fn open_without_as_is_unchanged_and_a_non_string_as_is_rejected() {
    let f = fixture();
    let path = f.project.to_str().unwrap();
    let mut plain = Session::new();
    ok(plain.handle_request(&json!({"op": "open", "path": path})));
    assert_eq!(plain.snapshot()["modified"], false);
    let rejected = plain.handle_request(&json!({"op": "open", "path": path, "as": 7}));
    assert_eq!(rejected["succeeded"], false);
    assert_eq!(rejected["message"], "as must be a string");
    assert_eq!(plain.snapshot()["modified"], false);
}

#[cfg(unix)]
#[test]
fn a_moved_or_renamed_project_takes_its_backups_along_and_other_files_do_not() {
    let dir = temp_dir();
    let config = dir.join("config");
    let write = |p: &Path| fs::write(p, b"x").map_err(|e| e.to_string());
    let settings = Settings::new(true, 1, 3);
    let (old, new) = (dir.join("a/proj.inx"), dir.join("b/renamed.inx"));
    for path in [&old, &new] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
    }
    fs::write(&old, b"project").unwrap();
    let mut autosave = Autosave::new(config.clone(), at(0, 7));
    autosave.set_project(Some(old.clone()), at(0, 7));
    autosave.autosave_now(&settings, at(0, 7), write).unwrap();

    // A copy and a same-named file elsewhere are other files: nothing is shared while the owner exists,
    // and nothing is shared after it is gone either.
    let (copy, namesake) = (dir.join("b/copy.inx"), dir.join("c/proj.inx"));
    fs::create_dir_all(namesake.parent().unwrap()).unwrap();
    fs::copy(&old, &copy).unwrap();
    fs::write(&namesake, b"project").unwrap();
    for other in [&copy, &namesake] {
        assert!(list_backups(&config, other).unwrap().is_empty());
        assert!(!unclean_exit(&config, other));
    }

    // Moved and renamed in one step: the backups and the crash marker follow the file.
    fs::rename(&old, &new).unwrap();
    for other in [&copy, &namesake] {
        assert!(list_backups(&config, other).unwrap().is_empty());
    }
    assert!(unclean_exit(&config, &new));
    let backups = list_backups(&config, &new).unwrap();
    assert_eq!(backups.len(), 1);
    assert!(backups[0].path.starts_with(autosave_dir(&config, &new).unwrap()));

    // The adopted directory is now this project's own: a clean exit clears the marker, discard empties it.
    let mut moved = Autosave::new(config.clone(), at(0, 9));
    moved.set_project(Some(new.clone()), at(0, 9));
    moved.release_lock();
    assert!(!unclean_exit(&config, &new));
    assert_eq!(discard(&config, &new).unwrap(), 1);
}
