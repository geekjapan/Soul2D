mod common;
#[path = "common/visual_scalar.rs"]
mod visual_scalar;

use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use soul2d_core::editor_controller::{
    DraftView, EditorController, Can3KeyTarget, Can3ScalarKind, Can3HandleSide, Can3HandlePoint, Can3CurveType,
    FILE_OP_BLOCKED_MESSAGE, RESELECT_BLOCKED_MESSAGE,
    UNSAVED_REPLACEMENT_MESSAGE,
};

static TEMP_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "soul2d-editor-test-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    common::scratch(path)
}

fn first_node_id(snapshot: &Value) -> u64 {
    snapshot["model"]["nodes"]["children"]
        .as_array()
        .and_then(|children| children.first())
        .and_then(|node| node["uuid"].as_u64())
        .expect("no top-level node")
}

#[test]
fn image_anchor_draft_applies_undoes_and_reopens() {
    let directory = temp_dir();
    let source = directory.join("image.can3");
    let xml = visual_scalar::xml(false, "opacity", "attrOpacity");
    fs::write(&source, authoring_bytes(&xml)).unwrap();
    let mut controller = EditorController::new();
    assert_eq!(controller.core_request(&json!({"op":"open","path":source}))["succeeded"], true);
    let revision = controller.snapshot()["revision"].clone();
    let added = controller.core_request(&json!({"op":"live2d.animation.key.add","sceneGuid":"scene-guid",
        "trackGuid":"image-track-guid","curveId":"opacity","anchorFrame":0,"value":1.0,
        "expectedRevision":revision}));
    assert_eq!(added["succeeded"], true, "{added}");
    let before = controller.snapshot()["model"].clone();
    controller.select_can3_anchor("scene-guid", "image-track-guid", "opacity", 0, None).unwrap();
    controller.set_draft_value(0.4);
    assert_eq!(controller.apply_draft()["succeeded"], true);
    let mut expected = before.clone();
    expected["scenes"][0]["imageTracks"][0]["visualCurves"][0]["points"][0]["anchor"]["value"] = json!(0.4);
    assert_eq!(controller.snapshot()["model"], expected);
    assert_eq!(controller.core_request(&json!({"op":"undo"}))["succeeded"], true);
    assert_eq!(controller.snapshot()["model"], before);
    assert_eq!(controller.core_request(&json!({"op":"redo"}))["succeeded"], true);
    assert_eq!(controller.snapshot()["model"], expected);
    let saved = directory.join("saved.can3");
    assert_eq!(controller.core_request(&json!({"op":"save","path":saved}))["succeeded"], true);
    assert_eq!(controller.core_request(&json!({"op":"open","path":saved}))["succeeded"], true);
    assert_eq!(controller.snapshot()["model"], expected);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn visual_scalar_drafts_cover_six_image_and_guide_channels_with_shared_history() {
    assert!(Can3CurveType::parse("unknown").is_none());
    for guide in [false, true] {
        for (id, named) in [("scalex", "attrScaleX"), ("scaley", "attrScaleY"), ("rotate", "attrRotate"),
            ("shear", "attrShear"), ("opacity", "attrOpacity"), ("artPathWidth", "attrArtPathWidth")]
        {
            let directory = temp_dir();
            let source = directory.join("visual.can3");
            fs::write(&source, authoring_bytes(&visual_scalar::xml(guide, id, named))).unwrap();
            let original = fs::read(&source).unwrap();
            let mut controller = EditorController::new();
            assert_eq!(controller.core_request(&json!({"op":"open","path":source}))["succeeded"], true);
            let revision = controller.snapshot()["revision"].clone();
            let added = controller.core_request(&json!({"op":"live2d.animation.key.add","sceneGuid":"scene-guid",
                "trackGuid":"image-track-guid","curveId":id,"anchorFrame":0,"value":1.0,"expectedRevision":revision}));
            assert_eq!(added["succeeded"], true, "{guide}/{id}: {added}");
            let target = Can3KeyTarget { scene_guid: "scene-guid".into(), track_guid: "image-track-guid".into(),
                curve_id: id.into(), part_opacity_map_key: None, frame: 0 };
            let family = if guide { "guideTracks" } else { "imageTracks" };
            let before = controller.snapshot()["model"].clone();
            let curve_index = before["scenes"][0][family][0]["visualCurves"].as_array().unwrap().iter()
                .position(|curve| curve["id"] == id).unwrap();
            let mut expected = before.clone();
            for kind in [Can3ScalarKind::Anchor, Can3ScalarKind::Handle(Can3HandleSide::Previous),
                Can3ScalarKind::Handle(Can3HandleSide::Next), Can3ScalarKind::CurveType]
            {
                controller.select_can3_scalar(target.clone(), kind).unwrap();
                assert!(!controller.draft_dirty());
                let curve = &mut expected["scenes"][0][family][0]["visualCurves"][curve_index];
                match kind {
                    Can3ScalarKind::Anchor => {
                        controller.set_draft_value(0.4);
                        curve["points"][0]["anchor"]["value"] = json!(0.4);
                    }
                    Can3ScalarKind::Handle(side) => {
                        let frame = if side == Can3HandleSide::Previous { -2.5 } else { 2.5 };
                        controller.set_draft_handle(Can3HandlePoint { frame, value: 3.5 });
                        curve["points"][0][side.as_str()]["frame"] = json!(frame);
                        curve["points"][0][side.as_str()]["value"] = json!(3.5);
                    }
                    Can3ScalarKind::CurveType => {
                        controller.set_draft_curve_type(Can3CurveType::parse("STEP").unwrap());
                        curve["curveTypes"][0] = json!("STEP");
                    }
                }
                let revision = controller.snapshot()["revision"].as_u64().unwrap();
                assert!(controller.draft_dirty());
                let response = controller.apply_draft();
                assert_eq!(response["succeeded"], true, "{guide}/{id}/{kind:?}: {response}");
                assert_eq!(controller.snapshot()["revision"], revision + 1);
                assert_eq!(controller.snapshot()["model"], expected);
                assert!(controller.draft_view().is_none());
            }
            for _ in 0..4 { assert_eq!(controller.core_request(&json!({"op":"undo"}))["succeeded"], true); }
            assert_eq!(controller.snapshot()["model"], before);
            for _ in 0..4 { assert_eq!(controller.core_request(&json!({"op":"redo"}))["succeeded"], true); }
            assert_eq!(controller.snapshot()["model"], expected);
            controller.select_can3_scalar(target.clone(), Can3ScalarKind::CurveType).unwrap();
            let snapshot = controller.snapshot().clone();
            assert_eq!(controller.apply_draft()["succeeded"], true);
            assert_eq!(controller.snapshot(), &snapshot);
            let saved = directory.join("saved.can3");
            assert_eq!(controller.core_request(&json!({"op":"save","path":saved}))["succeeded"], true);
            assert_eq!(controller.core_request(&json!({"op":"open","path":saved}))["succeeded"], true);
            assert_eq!(controller.snapshot()["model"], expected);
            assert_eq!(fs::read(source).unwrap(), original);

            controller.select_can3_scalar(target.clone(), Can3ScalarKind::Handle(Can3HandleSide::Next)).unwrap();
            controller.set_draft_handle(Can3HandlePoint { frame: f64::NAN, value: 7.0 });
            let snapshot = controller.snapshot().clone();
            assert_eq!(controller.core_request(&json!({"op":"save","path":directory.join("blocked.can3")}))["message"], FILE_OP_BLOCKED_MESSAGE);
            assert_eq!(controller.core_request(&json!({"op":"new"}))["message"], FILE_OP_BLOCKED_MESSAGE);
            assert_eq!(controller.select_can3_scalar(target.clone(), Can3ScalarKind::CurveType).unwrap_err(), RESELECT_BLOCKED_MESSAGE);
            assert_eq!(controller.apply_draft()["succeeded"], false);
            assert_eq!(controller.snapshot(), &snapshot);
            assert!(matches!(controller.draft_view(), Some(DraftView::Can3Handle { value, .. }) if value.frame.is_nan()));
            controller.set_draft_handle(Can3HandlePoint { frame: f64::MAX, value: 7.0 });
            assert_eq!(controller.apply_draft()["succeeded"], false);
            assert_eq!(controller.snapshot(), &snapshot);
            assert!(controller.draft_dirty());
            controller.discard_draft();
            controller.select_can3_scalar(target, Can3ScalarKind::CurveType).unwrap();
            controller.set_draft_curve_type(Can3CurveType::parse("BEZIER").unwrap());
            let revision = controller.snapshot()["revision"].clone();
            assert_eq!(controller.core_request(&json!({"op":"live2d.animation.scene.set","sceneGuid":"scene-guid",
                "name":"Concurrent", "expectedRevision":revision}))["succeeded"], true);
            let snapshot = controller.snapshot().clone();
            assert!(controller.draft_stale());
            assert_eq!(controller.apply_draft()["succeeded"], false);
            assert_eq!(controller.snapshot(), &snapshot);
            assert!(matches!(controller.draft_view(), Some(DraftView::Can3CurveType { value, .. }) if value.as_str() == "BEZIER"));
            fs::remove_dir_all(directory).unwrap();
        }
    }
}

#[test]
fn visual_scalar_draft_targets_an_unsaved_copy_without_changing_the_donor() {
    for guide in [false, true] {
        let directory = temp_dir(); let source = directory.join("copies.can3");
        fs::write(&source, authoring_bytes(&visual_scalar::xml(guide, "opacity", "attrOpacity"))).unwrap();
        let mut controller = EditorController::new();
        assert_eq!(controller.core_request(&json!({"op":"open", "path":source}))["succeeded"], true);
        let revision = controller.snapshot()["revision"].clone();
        assert_eq!(controller.core_request(&json!({"op":"live2d.animation.key.add", "sceneGuid":"scene-guid",
            "trackGuid":"image-track-guid", "curveId":"opacity", "anchorFrame":0, "value":1.0, "expectedRevision":revision}))["succeeded"], true);
        let revision = controller.snapshot()["revision"].clone();
        let copied = controller.core_request(&json!({"op":"live2d.animation.track.duplicate", "sceneGuid":"scene-guid",
            "trackGuid":"image-track-guid", "name":"Copy", "expectedRevision":revision}));
        assert_eq!(copied["succeeded"], true, "{copied}");
        let guid = copied["result"]["trackGuid"].as_str().unwrap().to_owned();
        let target = Can3KeyTarget { scene_guid:"scene-guid".into(), track_guid:guid.clone(),
            curve_id:"opacity".into(), part_opacity_map_key:None, frame:0 };
        let before = controller.snapshot()["model"].clone();
        let mut expected = before.clone();
        let family = if guide { "guideTracks" } else { "imageTracks" };
        let track = expected["scenes"][0][family].as_array_mut().unwrap().iter_mut().find(|track| track["guid"] == guid).unwrap();
        let curve = track["visualCurves"].as_array_mut().unwrap().iter_mut().find(|curve| curve["id"] == "opacity").unwrap();
        curve["points"][0]["next"]["frame"] = json!(1.5);
        curve["points"][0]["next"]["value"] = json!(2.5);
        controller.select_can3_scalar(target, Can3ScalarKind::Handle(Can3HandleSide::Next)).unwrap();
        controller.set_draft_handle(Can3HandlePoint { frame:1.5, value:2.5 });
        assert_eq!(controller.apply_draft()["succeeded"], true);
        assert_eq!(controller.snapshot()["model"], expected);
        assert_eq!(controller.core_request(&json!({"op":"undo"}))["succeeded"], true);
        assert_eq!(controller.snapshot()["model"], before);
        assert_eq!(controller.core_request(&json!({"op":"redo"}))["succeeded"], true);
        let saved = directory.join("saved.can3");
        assert_eq!(controller.core_request(&json!({"op":"save", "path":saved}))["succeeded"], true);
        assert_eq!(controller.core_request(&json!({"op":"open", "path":saved}))["succeeded"], true);
        assert_eq!(controller.snapshot()["model"], expected);
        fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn underscore_anchor_draft_keeps_the_exact_map_selection() {
    let directory = temp_dir();
    let source = directory.join("underscore.can3");
    fs::write(&source, authoring_bytes(include_str!("fixtures/can3-underscore-opacity.xml"))).unwrap();
    let mut controller = EditorController::new();
    assert_eq!(controller.core_request(&json!({"op":"open","path":source}))["succeeded"], true);
    let before = controller.snapshot()["model"].clone();
    let id = "live2DPartsOpacity_PartArmA";
    assert!(controller.select_can3_anchor("scene-guid", "track-guid", id, 0, None).is_err());
    let map_key = "live2DPartsOpacity_PartArmB";
    controller.select_can3_anchor("scene-guid", "track-guid", id, 0, Some(map_key)).unwrap();
    assert!(matches!(controller.draft_view(), Some(DraftView::Can3Anchor {
        part_opacity_map_key: Some("live2DPartsOpacity_PartArmB"), ..
    })));
    controller.set_draft_value(0.4);
    assert_eq!(controller.apply_draft()["succeeded"], true);
    let after = controller.snapshot()["model"].clone();
    let mut expected = before.clone();
    expected["scenes"][0]["tracks"][0]["underscorePartOpacityCurves"][1]["points"][0]["anchor"]["value"] = json!(0.4);
    assert_eq!(after, expected);
    assert_eq!(controller.core_request(&json!({"op":"undo"}))["succeeded"], true);
    assert_eq!(controller.snapshot()["model"], before);
    assert_eq!(controller.core_request(&json!({"op":"redo"}))["succeeded"], true);
    assert_eq!(controller.snapshot()["model"], after);
    let saved = directory.join("saved.can3");
    assert_eq!(controller.core_request(&json!({"op":"save","path":saved}))["succeeded"], true);
    assert_eq!(controller.core_request(&json!({"op":"open","path":saved}))["succeeded"], true);
    assert_eq!(controller.snapshot()["model"], after);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn part_opacity_anchor_draft_applies_undoes_and_reopens() {
    for (curve_id, field, index) in [
        ("live2DPartsOpacity_PartArmB", "partOpacityCurves", 1),
        ("live2DPartsOpacity:legacy-attribute", "legacyPartOpacityCurves", 0),
    ] {
        let directory = temp_dir();
        let source = directory.join("part-opacity.can3");
        fs::write(&source, authoring_bytes(include_str!("fixtures/can3-part-opacity.xml"))).unwrap();
        let mut controller = EditorController::new();
        assert_eq!(controller.core_request(&json!({"op":"open","path":source}))["succeeded"], true);
        let before = controller.snapshot()["model"].clone();
        controller.select_can3_anchor("scene-guid", "track-guid", curve_id, 0, None).unwrap();
        controller.set_draft_value(0.4);
        assert!(controller.draft_dirty());
        assert_eq!(controller.apply_draft()["succeeded"], true);
        let after = controller.snapshot()["model"].clone();
        assert_eq!(after["scenes"][0]["tracks"][0][field][index]["points"][0]["anchor"]["value"], 0.4);
        assert_eq!(controller.core_request(&json!({"op":"undo"}))["succeeded"], true);
        assert_eq!(controller.snapshot()["model"], before);
        assert_eq!(controller.core_request(&json!({"op":"redo"}))["succeeded"], true);
        assert_eq!(controller.snapshot()["model"], after);
        let saved = directory.join("edited.can3");
        assert_eq!(controller.core_request(&json!({"op":"save","path":saved}))["succeeded"], true);
        assert_eq!(controller.core_request(&json!({"op":"open","path":saved}))["succeeded"], true);
        assert_eq!(controller.snapshot()["model"], after);
        assert!(controller.select_can3_anchor("scene-guid", "track-guid", "live2DPartsOpacity:missing-legacy-attribute", 0, None).is_err());
    }
}

fn mcp_call(controller: &mut EditorController, id: u64, name: &str, arguments: Value) -> Value {
    controller
        .mcp_request(&json!({
            "jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":name,"arguments":arguments}
        }))
        .expect("notification did not reply")
}

fn mcp_snapshot_name(controller: &mut EditorController, id: u64) -> (String, Value) {
    let read = controller
        .mcp_request(&json!({
            "jsonrpc":"2.0","id":id,"method":"resources/read",
            "params":{"uri":"resource://vsoul/snapshot"}
        }))
        .unwrap();
    let snapshot: Value =
        serde_json::from_str(read["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    (
        snapshot["model"]["nodes"]["children"][0]["name"]
            .as_str()
            .unwrap()
            .to_owned(),
        snapshot,
    )
}

fn playback_controller() -> EditorController {
    let mut controller = EditorController::new();
    for request in [
        json!({"op":"parameter.create", "name":"Preview", "dimensions":2,
            "min":[-1,-1], "max":[1,1], "defaults":[0,0]}),
        json!({"op":"animation.create", "name":"Clock"}),
        json!({"op":"animation.set", "name":"Clock", "length":10,
            "timestep":0.125, "leadIn":2, "leadOut":6}),
    ] {
        let response = controller.core_request(&request);
        assert_eq!(response["succeeded"], true, "{response}");
    }
    controller
}

#[test]
fn inx_play_intent_uses_session_clock_and_preserves_history() {
    let mut controller = playback_controller();
    let parameter = controller.snapshot()["model"]["param"][0]["uuid"]
        .as_u64()
        .unwrap();
    assert_eq!(
        controller.core_request(&json!({"op":"animation.key.set", "name":"Clock",
        "parameter":parameter, "axis":0, "frame":0, "value":0.75}))["succeeded"],
        true
    );
    assert_eq!(
        controller.core_request(&json!({"op":"value.set", "id":parameter,
        "value":[0.25,-0.5]}))["succeeded"],
        true
    );
    let before = controller.snapshot().clone();
    assert_eq!(controller.play("Clock")["succeeded"], true);
    assert!(controller.playing());
    assert_eq!(
        controller.snapshot()["values"][parameter.to_string()],
        json!([0.75, -0.5])
    );
    controller.tick(0.15625, false).unwrap();
    assert_eq!(controller.snapshot()["animation"]["frame"], 1.25);
    controller.pause();
    let paused = controller.snapshot().clone();
    assert!(controller.tick(1.0, false).is_none());
    assert_eq!(controller.snapshot(), &paused);
    controller.play("Clock");
    controller.tick(0.59375, true).unwrap();
    assert_eq!(controller.snapshot()["animation"]["frame"], 2.0);
    controller.tick(3.0, false).unwrap();
    assert_eq!(controller.snapshot()["animation"]["frame"], 9.0);
    assert!(!controller.playing());
    controller.play("Clock");
    assert_eq!(controller.snapshot()["animation"]["frame"], 0.0);
    let active = controller.snapshot().clone();
    assert_eq!(controller.tick(-1.0, false).unwrap()["succeeded"], false);
    assert!(!controller.playing());
    assert_eq!(controller.snapshot(), &active);
    for key in ["model", "revision", "modified", "canUndo", "canRedo"] {
        assert_eq!(controller.snapshot()[key], before[key]);
    }
    assert_eq!(
        controller.core_request(&json!({"op":"animation.reset"}))["succeeded"],
        true
    );
    assert_eq!(controller.snapshot(), &before);
    assert_eq!(
        controller.snapshot()["values"][parameter.to_string()],
        json!([0.25, -0.5])
    );
}

#[test]
fn inx_external_preview_commands_cancel_play_but_reads_and_failures_do_not() {
    let mut controller = playback_controller();
    controller.play("Clock");
    let before = controller.snapshot().clone();
    let read = controller
        .mcp_request(&json!({"jsonrpc":"2.0","id":1,
        "method":"resources/read","params":{"uri":"resource://vsoul/snapshot"}}))
        .unwrap();
    let observed: Value =
        serde_json::from_str(read["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(observed, before);
    assert!(controller.playing());
    assert_eq!(
        mcp_call(
            &mut controller,
            2,
            "animation_seek",
            json!({"name":"Missing","frame":0})
        )["result"]["isError"],
        true
    );
    assert!(controller.playing());
    assert_eq!(controller.snapshot(), &before);
    assert_eq!(
        mcp_call(
            &mut controller,
            3,
            "animation_seek",
            json!({"name":"Clock","frame":4})
        )["result"]["isError"],
        false
    );
    assert!(!controller.playing());
    assert_eq!(controller.snapshot()["animation"]["frame"], 4.0);
    controller.play("Clock");
    let id = controller.snapshot()["model"]["param"][0]["uuid"].clone();
    assert_eq!(
        mcp_call(
            &mut controller,
            4,
            "value_set",
            json!({"id":id,"value":[0.5,-0.5]})
        )["result"]["isError"],
        false
    );
    assert!(!controller.playing());
    assert_eq!(
        controller.snapshot()["values"][id.to_string()],
        json!([0.5, -0.5])
    );
    assert!(controller.snapshot()["animation"].is_null());
    controller.play("Clock");
    assert!(controller
        .mcp_request(&json!({"jsonrpc":"2.0","method":"tools/call",
        "params":{"name":"animation_seek","arguments":{"name":"Clock","frame":0}}}))
        .is_none());
    assert!(!controller.playing());
    controller.play("Clock");
    let before = controller.snapshot().clone();
    assert!(controller
        .mcp_request(&json!({"jsonrpc":"2.0","method":"tools/call",
        "params":{"name":"physics_step","arguments":{"dt":-1}}}))
        .is_none());
    assert!(!controller.playing());
    assert_eq!(controller.snapshot(), &before);
    controller.play("Clock");
    let before = controller.snapshot().clone();
    assert!(controller
        .mcp_request(&json!({"jsonrpc":"2.0","method":"tools/call",
        "params":{"name":"rig_preview_deform","arguments":{"root":18446744073709551615u64}}}))
        .is_none());
    assert!(!controller.playing());
    assert_eq!(controller.snapshot(), &before);
    controller.play("Clock");
    assert_eq!(
        mcp_call(&mut controller, 5, "undo", json!({}))["result"]["isError"],
        false
    );
    assert!(!controller.playing());
    controller.play("Clock");
    assert_eq!(
        mcp_call(&mut controller, 6, "random_pose_seek", json!({"time":1.0,"pattern":"A"}))["result"]["isError"],
        false
    );
    assert!(!controller.playing());
    assert_eq!(controller.snapshot()["randomPose"]["time"], 1.0);
    assert_eq!(controller.snapshot()["values"], before["values"]);
}

#[test]
fn inx_runtime_preview_does_not_persist_and_reopen_stops_play() {
    let mut controller = playback_controller();
    let path = temp_dir().join("preview.inx");
    assert_eq!(
        controller.core_request(&json!({"op":"save","path":path}))["succeeded"],
        true
    );
    let saved = fs::read(&path).unwrap();
    let id = controller.snapshot()["model"]["param"][0]["uuid"].clone();
    assert_eq!(
        controller.core_request(&json!({"op":"value.set","id":id,"value":[0.25,0.5]}))["succeeded"],
        true
    );
    controller.play("Clock");
    controller.tick(0.5, false);
    assert_eq!(
        controller.core_request(&json!({"op":"save","path":path}))["succeeded"],
        true
    );
    assert!(controller.playing());
    assert_eq!(fs::read(&path).unwrap(), saved);
    assert_eq!(
        controller.core_request(&json!({"op":"open","path":path}))["succeeded"],
        true
    );
    assert!(!controller.playing());
    assert!(controller.snapshot()["animation"].is_null());
    assert_eq!(
        controller.snapshot()["values"][id.to_string()],
        json!([0.0, 0.0])
    );
    assert!(!controller.snapshot()["modified"].as_bool().unwrap());
    fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn drafts_gate_file_ops_on_both_transports_and_apply_through_one_session() {
    let mut controller = EditorController::new();
    let before = controller.snapshot().clone();

    let initialized = controller
        .mcp_request(&json!({"jsonrpc":"2.0","id":1,"method":"initialize"}))
        .unwrap();
    assert_eq!(initialized["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(controller.snapshot(), &before);

    let created = mcp_call(
        &mut controller,
        2,
        "node_create",
        json!({"className":"Node", "name":"From MCP"}),
    );
    assert_eq!(created["result"]["isError"], false, "{created}");
    let id = first_node_id(controller.snapshot());
    assert_eq!(
        controller.snapshot()["model"]["nodes"]["children"][0]["name"],
        "From MCP"
    );

    controller.select_node(id).unwrap();
    controller.set_draft_name("From GUI".into());
    assert!(controller.draft_dirty());
    assert!(!controller.draft_stale());
    let editing = controller.snapshot().clone();

    let directory = temp_dir();
    let scratch = directory.join("draft.inx");
    for op in ["new", "open", "save"] {
        let mut request = json!({"op":op});
        if op != "new" {
            request["path"] = json!(scratch.to_str().unwrap());
        }
        let response = controller.core_request(&request);
        assert_eq!(response["succeeded"], false, "{op}: {response}");
        assert_eq!(response["message"], FILE_OP_BLOCKED_MESSAGE);
        assert_eq!(response["result"], Value::Null);
        assert_eq!(controller.snapshot(), &editing, "{op} changed state");
    }
    assert!(!scratch.exists());

    let rejected = mcp_call(
        &mut controller,
        3,
        "save",
        json!({"path": scratch.to_str().unwrap()}),
    );
    assert_eq!(rejected["result"]["isError"], true, "{rejected}");
    assert_eq!(
        rejected["result"]["content"][0]["text"],
        FILE_OP_BLOCKED_MESSAGE
    );
    assert!(!scratch.exists());
    assert_eq!(controller.snapshot(), &editing);
    assert!(controller.draft_dirty());

    let (name, _) = mcp_snapshot_name(&mut controller, 4);
    assert_eq!(name, "From MCP");

    let applied = controller.apply_draft();
    assert_eq!(applied["succeeded"], true, "{applied}");
    let (name, snapshot) = mcp_snapshot_name(&mut controller, 5);
    assert_eq!(name, "From GUI");
    assert_eq!(&snapshot, controller.snapshot());

    controller.core_request(&json!({"op":"undo"}));
    let (name, _) = mcp_snapshot_name(&mut controller, 6);
    assert_eq!(name, "From MCP");
    controller.core_request(&json!({"op":"redo"}));
    let (name, _) = mcp_snapshot_name(&mut controller, 7);
    assert_eq!(name, "From GUI");
}

#[test]
fn a_stale_draft_blocks_apply_until_discarded() {
    let mut controller = EditorController::new();
    controller.core_request(&json!({"op":"node.create","className":"Node","name":"First"}));
    let id = first_node_id(controller.snapshot());
    controller.select_node(id).unwrap();
    controller.set_draft_name("Draft".into());

    let renamed = mcp_call(&mut controller, 1, "node_rename", json!({"id":id,"name":"Current"}));
    assert_eq!(renamed["result"]["isError"], false, "{renamed}");
    assert!(controller.draft_stale());
    assert!(controller.draft_dirty());
    let history = controller.snapshot().clone();

    let failed = controller.apply_draft();
    assert_eq!(failed["succeeded"], false);
    match controller.draft_view() {
        Some(DraftView::NodeName { value, .. }) => assert_eq!(value, "Draft"),
        _ => panic!("draft was lost"),
    }
    assert_eq!(controller.snapshot(), &history);
    assert_eq!(
        controller.snapshot()["model"]["nodes"]["children"][0]["name"],
        "Current"
    );

    controller.discard_draft();
    controller.select_node(id).unwrap();
    controller.set_draft_name("Draft".into());
    assert_eq!(controller.apply_draft()["succeeded"], true);
    assert_eq!(
        controller.snapshot()["model"]["nodes"]["children"][0]["name"],
        "Draft"
    );
}

#[test]
fn a_dirty_draft_blocks_reselection_without_losing_input() {
    let mut controller = EditorController::new();
    controller.core_request(&json!({"op":"node.create","className":"Node","name":"One"}));
    controller.core_request(&json!({"op":"node.create","className":"Node","name":"Two"}));
    let children = controller.snapshot()["model"]["nodes"]["children"]
        .as_array()
        .unwrap();
    let one = children[0]["uuid"].as_u64().unwrap();
    let two = children[1]["uuid"].as_u64().unwrap();

    controller.select_node(one).unwrap();
    controller.set_draft_name("Drafted".into());
    let snapshot = controller.snapshot().clone();
    for target in [two, one, u64::MAX] {
        assert_eq!(
            controller.select_node(target).unwrap_err(),
            RESELECT_BLOCKED_MESSAGE
        );
        assert_eq!(controller.snapshot(), &snapshot);
        match controller.draft_view() {
            Some(DraftView::NodeName { value, .. }) => assert_eq!(value, "Drafted"),
            _ => panic!("draft was lost"),
        }
    }
    controller.discard_draft();
    controller.select_node(two).unwrap();

    let mut controller = EditorController::new();
    let directory = temp_dir();
    let source = directory.join("scene.can3");
    fs::write(&source, authoring_bytes(EDIT_CAN3_XML)).unwrap();
    assert_eq!(
        controller.core_request(&json!({"op":"open","path":source.to_str().unwrap()}))["succeeded"],
        true
    );
    controller
        .select_can3_anchor("scene-guid", "track-guid", "live2dParam_ParamTest", -352, None)
        .unwrap();
    controller.set_draft_value(7.5);
    let snapshot = controller.snapshot().clone();
    for frame in [-300, -352, 999] {
        assert_eq!(
            controller
                .select_can3_anchor("scene-guid", "track-guid", "live2dParam_ParamTest", frame, None)
                .unwrap_err(),
            RESELECT_BLOCKED_MESSAGE
        );
        assert_eq!(controller.snapshot(), &snapshot);
        match controller.draft_view() {
            Some(DraftView::Can3Anchor { value, .. }) => assert_eq!(value, 7.5),
            _ => panic!("draft was lost"),
        }
    }
    controller.discard_draft();
    controller
        .select_can3_anchor("scene-guid", "track-guid", "live2dParam_ParamTest", -300, None)
        .unwrap();
}

#[test]
fn unsaved_changes_block_document_replacement_until_saved() {
    let directory = temp_dir();
    let mut controller = EditorController::new();
    controller.core_request(&json!({"op":"node.create","className":"Node","name":"Keep"}));
    assert_eq!(controller.snapshot()["modified"], true);
    let snapshot = controller.snapshot().clone();
    let bogus = directory.join("never-created.inx");

    for request in [
        json!({"op":"new"}),
        json!({"op":"open","path":bogus.to_str().unwrap()}),
    ] {
        let response = controller.core_request(&request);
        assert_eq!(response["succeeded"], false, "{response}");
        assert_eq!(response["message"], UNSAVED_REPLACEMENT_MESSAGE);
        assert_eq!(controller.snapshot(), &snapshot);
    }
    for (id, name, arguments) in [
        (1, "new", json!({})),
        (2, "open", json!({"path":bogus.to_str().unwrap()})),
    ] {
        let rejected = mcp_call(&mut controller, id, name, arguments);
        assert_eq!(rejected["result"]["isError"], true, "{rejected}");
        assert_eq!(
            rejected["result"]["content"][0]["text"],
            UNSAVED_REPLACEMENT_MESSAGE
        );
    }
    assert!(!bogus.exists());
    assert_eq!(controller.snapshot(), &snapshot);

    let id = first_node_id(controller.snapshot());
    controller.select_node(id).unwrap();
    controller.set_draft_name("Dirty".into());
    for op in ["new", "open", "save"] {
        let mut request = json!({"op":op});
        if op != "new" {
            request["path"] = json!(bogus.to_str().unwrap());
        }
        let response = controller.core_request(&request);
        assert_eq!(response["message"], FILE_OP_BLOCKED_MESSAGE, "{op}");
    }
    controller.discard_draft();

    let saved = directory.join("saved.inx");
    assert_eq!(
        controller.core_request(&json!({"op":"save","path":saved.to_str().unwrap()}))["succeeded"],
        true
    );
    controller.select_node(id).unwrap();
    assert!(!controller.draft_dirty());
    assert_eq!(
        controller.core_request(&json!({"op":"open","path":saved.to_str().unwrap()}))["succeeded"],
        true
    );
    assert!(controller.draft_view().is_none());
    assert_eq!(first_node_id(controller.snapshot()), id);
    assert_eq!(
        controller.snapshot()["model"]["nodes"]["children"][0]["name"],
        "Keep"
    );
    assert_eq!(
        controller.core_request(&json!({"op":"new"}))["succeeded"],
        true
    );
}

#[test]
fn file_replacement_notifications_still_clear_a_clean_draft() {
    let directory = temp_dir();
    let mut controller = EditorController::new();
    controller.core_request(&json!({"op":"node.create","className":"Node","name":"Old"}));
    let saved = directory.join("old.inx");
    assert_eq!(
        controller.core_request(&json!({"op":"save","path":saved.to_str().unwrap()}))["succeeded"],
        true
    );

    // An id-less new notification executes on the same session, answers None,
    // and still clears the clean draft whose target no longer exists.
    let id = first_node_id(controller.snapshot());
    controller.select_node(id).unwrap();
    assert!(!controller.draft_dirty());
    let before = controller.snapshot()["revision"].as_u64().unwrap();
    let response = controller.mcp_request(&json!({
        "jsonrpc":"2.0","method":"tools/call",
        "params":{"name":"new","arguments":{}}
    }));
    assert!(response.is_none(), "{response:?}");
    assert_ne!(controller.snapshot()["revision"].as_u64().unwrap(), before);
    assert_eq!(
        controller.snapshot()["model"]["nodes"]["children"]
            .as_array()
            .map_or(0, |children| children.len()),
        0
    );
    assert!(controller.draft_view().is_none());

    // An id-less open notification restores the saved document the same way.
    let root = controller.snapshot()["model"]["nodes"]["uuid"]
        .as_u64()
        .unwrap();
    controller.select_node(root).unwrap();
    let before = controller.snapshot()["revision"].as_u64().unwrap();
    let response = controller.mcp_request(&json!({
        "jsonrpc":"2.0","method":"tools/call",
        "params":{"name":"open","arguments":{"path":saved.to_str().unwrap()}}
    }));
    assert!(response.is_none(), "{response:?}");
    assert_ne!(controller.snapshot()["revision"].as_u64().unwrap(), before);
    assert_eq!(
        controller.snapshot()["model"]["nodes"]["children"][0]["name"],
        "Old"
    );
    assert!(controller.draft_view().is_none());

    // A failed open notification keeps the clean draft and everything else.
    let restored = first_node_id(controller.snapshot());
    controller.select_node(restored).unwrap();
    assert!(!controller.draft_dirty());
    let missing = directory.join("missing.inx");
    let snapshot = controller.snapshot().clone();
    let response = controller.mcp_request(&json!({
        "jsonrpc":"2.0","method":"tools/call",
        "params":{"name":"open","arguments":{"path":missing.to_str().unwrap()}}
    }));
    assert!(response.is_none(), "{response:?}");
    assert_eq!(controller.snapshot(), &snapshot);
    match controller.draft_view() {
        Some(DraftView::NodeName { original, value, .. }) => {
            assert_eq!(original, "Old");
            assert_eq!(value, "Old");
        }
        _ => panic!("clean draft was lost on a failed open"),
    }
    assert!(!missing.exists());
}

#[test]
fn close_policy_requires_a_clean_backend_and_no_dirty_draft() {
    let directory = temp_dir();
    let mut controller = EditorController::new();
    assert!(controller.can_close());
    controller.core_request(&json!({"op":"node.create","className":"Node","name":"N"}));
    let id = first_node_id(controller.snapshot());
    controller.select_node(id).unwrap();
    controller.set_draft_name("Dirty".into());
    assert!(!controller.can_close());
    assert_eq!(controller.apply_draft()["succeeded"], true);
    assert!(!controller.can_close());
    let scratch = directory.join("saved.inx");
    let saved = controller.core_request(&json!({"op":"save","path":scratch.to_str().unwrap()}));
    assert_eq!(saved["succeeded"], true, "{saved}");
    assert!(scratch.exists());
    assert!(controller.can_close());
    controller.panel_drafts = true;
    assert!(!controller.can_close());
    controller.panel_drafts = false;
    assert!(controller.can_close());
}

const EDIT_CAN3_XML: &str = r#"<root fileFormatVersion="503030001"><main><CAnimation xs.ref="animation"/></main>
<shared>
<CAnimation xs.id="animation"><s xs.n="name">EditFixture</s>
  <ResourceManager xs.n="resourceManager"><carray_list xs.n="_resourceRefList" count="1">
    <ResourceEntry><CResource_Linked_Model xs.n="resourceRef" xs.ref="resource"/></ResourceEntry>
  </carray_list></ResourceManager>
  <carray_list xs.n="_scenes" count="1"><CSceneSource xs.ref="scene"/></carray_list>
</CAnimation>
<CResource_Linked_Model xs.id="resource"><CGuid xs.n="guid" uuid="res-guid"/>
  <s xs.n="name">Model</s><s xs.n="srcFile">C:\models\fixture.cmo3</s></CResource_Linked_Model>
<CSceneSource xs.id="scene"><CGuid xs.n="guid" uuid="scene-guid"/>
  <CAnimation xs.n="_animation" xs.ref="animation"/><s xs.n="sceneName">Scene</s>
  <CMovieInfo xs.n="movieInfo"><f xs.n="fps">60</f><i xs.n="duration">120</i></CMovieInfo>
  <CTrackSourceSet xs.n="trackSourceSet"><carray_list xs.n="_sources" count="1">
    <CMvTrack_Live2DModel_Source xs.ref="track"/></carray_list></CTrackSourceSet>
</CSceneSource>
<CMvTrack_Live2DModel_Source xs.id="track"><CGuid xs.n="guid" uuid="track-guid"/>
  <s xs.n="name">Track</s>
  <CSceneSource xs.n="_sceneSource" xs.ref="scene"/>
  <CModelGuid xs.n="_resourceGuid" uuid="res-guid"/>
  <CMvKeyParamEffect xs.n="keyParamEffect"><CMvTrack_Live2DModel_Source xs.n="track" xs.ref="track"/>
    <carray_list xs.n="attrList" count="1"><CMvAttrF xs.ref="attr"/></carray_list></CMvKeyParamEffect>
  <i xs.n="start">0</i><i xs.n="internalOffset">0</i><i xs.n="duration">120</i>
</CMvTrack_Live2DModel_Source>
<CMvAttrF xs.id="attr"><CGuid xs.n="guid" uuid="parameter-guid"/>
  <CAttrId xs.n="id" idstr="live2dParam_ParamTest"/>
  <CMvTrack_Live2DModel_Source xs.n="track" xs.ref="track"/>
  <CMutableSequence xs.n="valueData"><CMvAttrF xs.n="attr" xs.ref="attr"/>
    <carray_list xs.n="points" count="2"><CBezierPt>
      <CSeqPt xs.n="anchor"><i xs.n="pos">-352</i><d xs.n="doubleValue">1</d><b xs.n="isCorner">false</b></CSeqPt>
      <CBezierCtrlPt xs.n="prev"><i xs.n="pos">-352</i><d xs.n="posF">-352</d><d xs.n="doubleValue">1</d><b xs.n="isPosOptimized">true</b></CBezierCtrlPt>
      <CBezierCtrlPt xs.n="next"><i xs.n="pos">-352</i><d xs.n="posF">-352</d><d xs.n="doubleValue">1</d><b xs.n="isPosOptimized">true</b></CBezierCtrlPt>
    </CBezierPt><CBezierPt>
      <CSeqPt xs.n="anchor"><i xs.n="pos">-300</i><d xs.n="doubleValue">2</d><b xs.n="isCorner">false</b></CSeqPt>
      <CBezierCtrlPt xs.n="prev"><i xs.n="pos">-300</i><d xs.n="posF">-300</d><d xs.n="doubleValue">2</d><b xs.n="isPosOptimized">true</b></CBezierCtrlPt>
      <CBezierCtrlPt xs.n="next"><i xs.n="pos">-300</i><d xs.n="posF">-300</d><d xs.n="doubleValue">2</d><b xs.n="isPosOptimized">true</b></CBezierCtrlPt>
    </CBezierPt></carray_list>
    <carray_list xs.n="curveTypes" count="2"><CCurveType v="0"/><CCurveType v="0"/></carray_list>
    <d xs.n="curMin">1</d><d xs.n="curMax">2</d>
  </CMutableSequence>
  <COptionParam xs.n="optionParam"><CParameterId xs.n="KEY_PARAM_ID" idstr="ParamTest"/></COptionParam>
  <f xs.n="rangeMin">-1</f><f xs.n="rangeMax">1</f>
</CMvAttrF>
</shared></root>"#;

const PLAYBACK_CMO3_XML: &str = r#"<root fileFormatVersion="401000004"><CModelSource>
<s xs.n="name">Playback fixture</s>
<CImageCanvas xs.n="canvas"><i xs.n="pixelWidth">100</i><i xs.n="pixelHeight">100</i></CImageCanvas>
<CParameterSourceSet xs.n="parameterSourceSet"><carray_list xs.n="_sources" count="1">
<CParameterSource><CParameterGuid xs.n="guid" uuid="parameter-guid"/>
<CParameterId xs.n="id" idstr="ParamTest"/><f xs.n="minValue">-10</f><f xs.n="maxValue">10</f>
<f xs.n="defaultValue">0</f></CParameterSource></carray_list></CParameterSourceSet>
<CDrawableSourceSet xs.n="drawableSourceSet"><carray_list xs.n="_sources" count="0"/></CDrawableSourceSet>
<CDeformerSourceSet xs.n="deformerSourceSet"><carray_list xs.n="_sources" count="0"/></CDeformerSourceSet>
<CPartSourceSet xs.n="partSourceSet"><carray_list xs.n="_sources" count="1">
<CPartSource xs.ref="part"/></carray_list></CPartSourceSet><CPartSource xs.n="rootPart" xs.ref="part"/>
</CModelSource><CPartSource xs.id="part"><CPartGuid xs.n="guid" uuid="part-guid"/>
<CPartId xs.n="id" idstr="root"/><carray_list xs.n="keyforms" count="0"/></CPartSource></root>"#;

#[test]
fn native_playback_controller_shares_can3_clock_and_external_commands() {
    let directory = temp_dir();
    let animation = directory.join("animation.can3");
    let model = directory.join("model.cmo3");
    let xml = EDIT_CAN3_XML.replace("-352", "0").replace("-300", "60")
        .replace("CCurveType v=\"0\"", "CCurveType v=\"SMOOTH\"");
    fs::write(&animation, authoring_bytes(&xml)).unwrap();
    fs::write(&model, authoring_bytes(PLAYBACK_CMO3_XML)).unwrap();
    for animation_first in [false, true] {
        let mut controller = EditorController::new();
        let (open, attach, op) = if animation_first {
            (&animation, &model, "live2d.model.attach")
        } else {
            (&model, &animation, "live2d.animation.attach")
        };
        for request in [json!({"op":"open", "path":open}), json!({"op":op, "path":attach})] {
            let response = controller.core_request(&request);
            assert_eq!(response["succeeded"], true, "{response}");
        }
        let before = controller.snapshot().clone();
        assert_eq!(controller.play_native("scene-guid")["succeeded"], true);
        assert!(controller.playing());
        assert_eq!(controller.tick(0.5, false).unwrap()["succeeded"], true);
        assert_eq!(controller.snapshot()["nativePlayback"]["active"]["frame"], 30.0);
        assert!((controller.snapshot()["values"]["ParamTest"].as_f64().unwrap() - 1.5).abs() < 1e-6);
        controller.pause();
        assert!(controller.tick(1.0, true).is_none());
        controller.play_native("scene-guid");
        assert_eq!(controller.snapshot()["nativePlayback"]["active"]["frame"], 30.0);
        assert_eq!(controller.core_request(&json!({"op":"snapshot"}))["succeeded"], true);
        assert!(controller.playing());
        assert_eq!(mcp_call(&mut controller, 1, "live2d_animation_seek",
            json!({"sceneGuid":"scene-guid", "frame":0}))["result"]["isError"], false);
        assert!(!controller.playing());
        controller.play_native("scene-guid");
        controller.tick(10.0, false).unwrap();
        assert!(!controller.playing());
        assert_eq!(controller.snapshot()["nativePlayback"]["active"]["frame"], 119.0);
        assert_eq!(controller.snapshot()["nativePlayback"]["active"]["clockFrame"], 120.0);
        assert_eq!(controller.play_native("scene-guid")["succeeded"], true);
        controller.pause();
        let seek = controller.core_request(&json!({
            "op":"live2d.animation.seek", "sceneGuid":"scene-guid", "frame":119.1
        }));
        assert_eq!(seek["succeeded"], true);
        controller.play_native("scene-guid");
        assert!(controller.playing());
        controller.tick(0.01, false).unwrap();
        let paused_clock = controller.snapshot()["nativePlayback"]["active"]["clockFrame"]
            .as_f64().unwrap();
        assert!(paused_clock > 119.1 && paused_clock < 120.0);
        controller.pause();
        controller.play_native("scene-guid");
        assert!((controller.snapshot()["nativePlayback"]["active"]["clockFrame"]
            .as_f64().unwrap() - paused_clock).abs() < 1e-9);
        controller.tick(1.0, false).unwrap();
        assert!(!controller.playing());
        assert_eq!(controller.snapshot()["nativePlayback"]["active"]["clockFrame"], 120.0);
        controller.play_native("scene-guid");
        assert_eq!(controller.snapshot()["nativePlayback"]["active"]["frame"], 0.0);
        controller.tick(2.5, true).unwrap();
        assert!(controller.playing());
        assert_eq!(controller.snapshot()["nativePlayback"]["active"]["frame"], 30.0);
        let snapshot = controller.snapshot().clone();
        assert_eq!(controller.tick(-1.0, false).unwrap()["succeeded"], false);
        assert!(!controller.playing());
        assert_eq!(controller.snapshot(), &snapshot);
        for key in ["model", "revision", "modified", "canUndo", "canRedo"] {
            assert_eq!(controller.snapshot()[key], before[key]);
        }
    }
    fs::remove_dir_all(directory).unwrap();
}

fn authoring_bytes(xml: &str) -> Vec<u8> {
    use flate2::write::DeflateEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(xml.as_bytes()).unwrap();
    let compressed = encoder.finish().unwrap();
    let mut zip = Vec::new();
    zip.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
    zip.extend_from_slice(&20u16.to_le_bytes());
    zip.extend_from_slice(&0x0808u16.to_le_bytes());
    zip.extend_from_slice(&8u16.to_le_bytes());
    zip.extend_from_slice(&[0; 4]);
    zip.extend_from_slice(&[0; 12]);
    zip.extend_from_slice(&8u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(b"contents");
    zip.extend_from_slice(&compressed);
    zip.extend_from_slice(b"PK\x07\x08");
    zip.extend_from_slice(&crc32fast::hash(xml.as_bytes()).to_le_bytes());
    zip.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
    zip.extend_from_slice(&(xml.len() as u32).to_le_bytes());

    let seed = 0x562e_099f_u32;
    let mask = seed as u8;
    let entries = [
        ("main.xml", "main_xml", [0x01, 0x21], zip),
        ("mesh.png", "", [0x01, 0x10], vec![0x89, b'P', b'N', b'G']),
        ("opaque.bin", "", [0x01, 0x10], vec![0, 1, 2, 0xff]),
    ];
    let mut bytes = vec![0; 58];
    bytes[..4].copy_from_slice(b"CAFF");
    bytes[4..8].copy_from_slice(&45u32.to_be_bytes());
    bytes[14..18].copy_from_slice(&seed.to_be_bytes());
    bytes[54..58].copy_from_slice(&((entries.len() as u32) ^ seed).to_be_bytes());
    let mut fields = Vec::new();
    for (name, alias, flags, _) in &entries {
        bytes.push(name.len() as u8 ^ mask);
        bytes.extend(name.as_bytes().iter().map(|byte| byte ^ mask));
        bytes.push(alias.len() as u8 ^ mask);
        bytes.extend(alias.as_bytes().iter().map(|byte| byte ^ mask));
        bytes.extend_from_slice(&(0x1234_5678u32 ^ seed).to_be_bytes());
        let offset_field = bytes.len();
        bytes.extend_from_slice(&seed.to_be_bytes());
        let size_field = bytes.len();
        bytes.extend_from_slice(&seed.to_be_bytes());
        bytes.extend(flags.iter().map(|byte| byte ^ mask));
        bytes.extend([0; 8].iter().map(|byte| byte ^ mask));
        fields.push((offset_field, size_field));
    }
    for (index, (_, _, _, payload)) in entries.iter().enumerate() {
        let offset = bytes.len();
        bytes[fields[index].0..fields[index].0 + 4]
            .copy_from_slice(&((offset as u32 ^ seed).to_be_bytes()));
        bytes[fields[index].1..fields[index].1 + 4]
            .copy_from_slice(&((payload.len() as u32 ^ seed).to_be_bytes()));
        bytes.extend(payload.iter().map(|byte| byte ^ mask));
    }
    bytes.extend_from_slice(b"bc");
    bytes
}

fn anchor_value(controller: &EditorController) -> f64 {
    controller.snapshot()["model"]["scenes"][0]["tracks"][0]["parameterCurves"][0]["points"][0]
        ["anchor"]["value"]
        .as_f64()
        .unwrap()
}

#[test]
fn can3_anchor_draft_edits_a_negative_frame_through_shared_history() {
    let directory = temp_dir();
    let source = directory.join("scene.can3");
    fs::write(&source, authoring_bytes(EDIT_CAN3_XML)).unwrap();
    let mut controller = EditorController::new();
    let opened = controller.core_request(&json!({"op":"open","path":source.to_str().unwrap()}));
    assert_eq!(opened["succeeded"], true, "{opened}");
    assert_eq!(controller.snapshot()["documentFormat"], "live2d.can3");
    let revision = controller.snapshot()["revision"].as_u64().unwrap();

    controller
        .select_can3_anchor("scene-guid", "track-guid", "live2dParam_ParamTest", -352, None)
        .unwrap();
    controller.set_draft_value(2.125);
    assert!(controller.draft_dirty());
    assert_eq!(
        controller.apply_draft()["succeeded"],
        true,
        "apply failed"
    );
    assert_eq!(anchor_value(&controller), 2.125);
    assert_eq!(
        controller.snapshot()["revision"].as_u64().unwrap(),
        revision + 1
    );
    assert_eq!(controller.snapshot()["canUndo"], true);

    controller.core_request(&json!({"op":"undo"}));
    assert_eq!(anchor_value(&controller), 1.0);
    controller.core_request(&json!({"op":"redo"}));
    assert_eq!(anchor_value(&controller), 2.125);

    let saved = directory.join("saved.can3");
    assert_eq!(
        controller.core_request(&json!({"op":"save","path":saved}))["succeeded"],
        true
    );
    assert!(saved.exists());

    controller
        .select_can3_anchor("scene-guid", "track-guid", "live2dParam_ParamTest", -352, None)
        .unwrap();
    controller.set_draft_value(f64::NAN);
    assert_eq!(controller.apply_draft()["succeeded"], false);
    match controller.draft_view() {
        Some(DraftView::Can3Anchor { value, .. }) => assert!(value.is_nan()),
        _ => panic!("draft was lost"),
    }

    controller.set_draft_value(9.0);
    let current = controller.snapshot()["revision"].as_u64().unwrap();
    let external = mcp_call(
        &mut controller,
        9,
        "live2d_animation_key_set",
        json!({"sceneGuid":"scene-guid","trackGuid":"track-guid",
            "curveId":"live2dParam_ParamTest","anchorFrame":-352,
            "value":3.5,"expectedRevision":current}),
    );
    assert_eq!(external["result"]["isError"], false, "{external}");
    assert!(controller.draft_stale());
    assert_eq!(controller.apply_draft()["succeeded"], false);
    match controller.draft_view() {
        Some(DraftView::Can3Anchor { value, .. }) => assert_eq!(value, 9.0),
        _ => panic!("draft was lost"),
    }
    assert_eq!(anchor_value(&controller), 3.5);

    controller.discard_draft();
    controller
        .select_can3_anchor("scene-guid", "track-guid", "live2dParam_ParamTest", -352, None)
        .unwrap();
    controller.set_draft_value(4.5);
    assert_eq!(controller.apply_draft()["succeeded"], true);
    assert_eq!(anchor_value(&controller), 4.5);
}

fn find_named(root: &PathBuf, name: &str) -> Option<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
                found.push(path);
            }
        }
    }
    found.sort();
    found.into_iter().next()
}

fn anchor_value_at(
    controller: &EditorController,
    ids: &(String, String, String, i64, f64),
) -> f64 {
    let (scene, track, curve, frame, _) = ids;
    controller.snapshot()["model"]["scenes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["guid"].as_str() == Some(scene.as_str()))
        .and_then(|s| {
            s["tracks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["guid"].as_str() == Some(track.as_str()))
        })
        .and_then(|t| {
            t["parameterCurves"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"].as_str() == Some(curve.as_str()))
        })
        .and_then(|c| {
            c["points"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["anchor"]["frame"].as_i64() == Some(*frame))
        })
        .and_then(|p| p["anchor"]["value"].as_f64())
        .expect("edited anchor not found")
}

#[test]
#[ignore = "requires VSOUL_LIVE2D_INPUTS; run explicitly"]
fn can3_anchor_draft_reads_real_sources_when_configured() {
    let root = PathBuf::from(
        std::env::var_os("VSOUL_LIVE2D_INPUTS").expect("VSOUL_LIVE2D_INPUTS must be set"),
    );
    let directory = temp_dir();
    for name in [
        "hiyori_free_t03.can3",
        "hiyori_pro_t04.can3",
        "ren_t01.can3",
        "zundamon_t01.can3",
    ] {
        let source = find_named(&root, name)
            .unwrap_or_else(|| panic!("{name} not found under {}", root.display()));
        eprintln!("real CAN3 case: {name} ({})", source.display());
        let original = fs::read(&source).unwrap();
        let mut controller = EditorController::new();
        assert_eq!(
            controller.core_request(&json!({"op":"open","path":source.to_str().unwrap()}))
                ["succeeded"],
            true,
            "{name} failed to open"
        );
        assert_eq!(controller.snapshot()["documentFormat"], "live2d.can3");
        let ids = {
            let model = &controller.snapshot()["model"];
            let mut first = None;
            let mut negative = None;
            'outer: for scene in model["scenes"].as_array().unwrap() {
                for track in scene["tracks"].as_array().unwrap() {
                    for curve in track["parameterCurves"].as_array().unwrap() {
                        for point in curve["points"].as_array().unwrap() {
                            let frame = point["anchor"]["frame"].as_i64().unwrap();
                            let entry = (
                                scene["guid"].as_str().unwrap().to_owned(),
                                track["guid"].as_str().unwrap().to_owned(),
                                curve["id"].as_str().unwrap().to_owned(),
                                frame,
                                point["anchor"]["value"].as_f64().unwrap(),
                            );
                            if frame < 0 {
                                negative = Some(entry);
                                break 'outer;
                            }
                            if first.is_none() {
                                first = Some(entry);
                            }
                        }
                    }
                }
            }
            negative.or(first).expect("no CAN3 anchor")
        };
        controller
            .select_can3_anchor(&ids.0, &ids.1, &ids.2, ids.3, None)
            .unwrap();
        let edited = ids.4 + 0.25;
        controller.set_draft_value(edited);
        let revision = controller.snapshot()["revision"].as_u64().unwrap();
        assert_eq!(controller.apply_draft()["succeeded"], true, "{name} apply failed");
        assert_eq!(
            controller.snapshot()["revision"].as_u64().unwrap(),
            revision + 1
        );
        assert_eq!(anchor_value_at(&controller, &ids), edited);
        controller.core_request(&json!({"op":"undo"}));
        assert_eq!(anchor_value_at(&controller, &ids), ids.4);
        controller.core_request(&json!({"op":"redo"}));
        assert_eq!(anchor_value_at(&controller, &ids), edited);

        // Modified documents must be saved before replacement; then reopen.
        let scratch = directory.join(format!("{name}-saved.can3"));
        assert_eq!(
            controller.core_request(&json!({"op":"save","path":scratch.to_str().unwrap()}))
                ["succeeded"],
            true,
            "{name} save failed"
        );
        assert_eq!(
            controller.core_request(&json!({"op":"open","path":scratch.to_str().unwrap()}))
                ["succeeded"],
            true,
            "{name} reopen failed"
        );
        assert_eq!(anchor_value_at(&controller, &ids), edited);
        assert_eq!(fs::read(&source).unwrap(), original, "{name} source mutated");
    }
}
