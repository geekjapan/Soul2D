mod common;

use serde_json::{json, Value};
use soul2d_core::editor_controller::{
    node_convert_classes, DraftView, EditorController, NodeAction,
};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "soul2d-nodeauth-test-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    common::scratch(path)
}

fn fixture() -> Value {
    json!({
        "meta": { "name": "fixture", "version": "1.0-alpha" },
        "nodes": {
            "uuid": 1, "name": "Fixture", "type": "Node", "enabled": true,
            "zsort": 0.0,
            "transform": { "trans": [0.0,0.0,0.0], "rot": [0.0,0.0,0.0], "scale": [1.0,1.0] },
            "lockToRoot": false, "pinToMesh": false,
            "children": [{
                "uuid": 2, "name": "A", "type": "Node", "enabled": true,
                "zsort": 0.0,
                "transform": { "trans": [10.0,0.0,0.0], "rot": [0.0,0.0,0.0], "scale": [1.0,1.0] },
                "lockToRoot": false, "pinToMesh": false
            }],
        },
        "param": [],
        "animations": {},
    })
}

fn controller() -> EditorController {
    let mut controller = EditorController::new();
    let response = controller.core_request(&json!({"op": "new", "model": fixture()}));
    assert_eq!(response["succeeded"], true, "{response}");
    controller
}

fn ok(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn child_names(snapshot: &Value) -> Vec<String> {
    snapshot["model"]["nodes"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["name"].as_str().unwrap().to_owned())
        .collect()
}

fn find_node(node: &Value, id: u64) -> Option<&Value> {
    if node["uuid"].as_u64() == Some(id) {
        return Some(node);
    }
    node["children"]
        .as_array()?
        .iter()
        .find_map(|child| find_node(child, id))
}

fn mcp_call(controller: &mut EditorController, id: u64, name: &str, arguments: Value) -> Value {
    controller
        .mcp_request(&json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }))
        .expect("tools/call did not reply")
}

#[test]
fn selection_leaves_snapshot_unchanged_and_preserves_draft() {
    let mut controller = controller();
    controller.select_node(2).unwrap();
    controller.set_draft_name("Draft".to_owned());
    let before = controller.snapshot().clone();

    controller.select_node_target(2).unwrap();
    assert_eq!(controller.selected_node(), Some(2));
    assert_eq!(controller.snapshot(), &before);
    assert!(controller.draft_dirty());

    assert_eq!(
        controller.select_node_target(99),
        Err("node 99 was not found".to_owned())
    );
    assert_eq!(controller.selected_node(), Some(2));
    assert_eq!(controller.snapshot(), &before);
}

#[test]
fn create_copy_paste_assigns_distinct_live_uuids() {
    let mut controller = controller();
    let created = ok(&controller.node_action(NodeAction::Create {
        class_name: "Composite",
        name: Some("ParentA"),
    }))
    .clone();
    let a = created["created"][0]["uuid"].as_u64().unwrap();
    assert_eq!(controller.selected_node(), Some(a));
    assert_eq!(created["created"][0]["type"], "Composite");

    let created = ok(&controller.node_action(NodeAction::Create {
        class_name: "Node",
        name: Some("B"),
    }))
    .clone();
    let b = created["created"][0]["uuid"].as_u64().unwrap();
    assert_eq!(controller.selected_node(), Some(b));
    let parent = find_node(&controller.snapshot()["model"]["nodes"], b)
        .and_then(|_| find_node(&controller.snapshot()["model"]["nodes"], a));
    assert_eq!(
        parent.unwrap()["children"][0]["uuid"].as_u64().unwrap(),
        b
    );

    ok(&controller.node_action(NodeAction::Copy));
    assert_eq!(controller.selected_node(), Some(b));
    controller.select_node_target(1).unwrap();
    let pasted = ok(&controller.node_action(NodeAction::Paste)).clone();
    let pasted_id = pasted["created"][0]["uuid"].as_u64().unwrap();
    assert_ne!(pasted_id, b);
    assert_eq!(controller.selected_node(), Some(pasted_id));
    assert_eq!(controller.snapshot()["clipboard"], json!([]));
    assert_eq!(pasted["created"][0]["name"], "B'");
}

#[test]
fn reparent_undo_delete_cut_clear_selection() {
    let mut controller = controller();
    ok(&controller.node_action(NodeAction::Create {
        class_name: "Node",
        name: Some("B"),
    }));
    let b = controller.selected_node().unwrap();
    ok(&controller.node_action(NodeAction::Reparent { parent: 2 }));
    assert_eq!(
        find_node(&controller.snapshot()["model"]["nodes"], 2).unwrap()["children"][0]["uuid"],
        b
    );

    let response = controller.core_request(&json!({"op": "undo"}));
    assert_eq!(response["succeeded"], true);
    assert!(find_node(&controller.snapshot()["model"]["nodes"], 2)
        .unwrap()["children"]
        .as_array()
        .map(|children| children.is_empty())
        .unwrap_or(true));

    ok(&controller.node_action(NodeAction::Cut));
    assert_eq!(controller.selected_node(), None);
    assert!(find_node(&controller.snapshot()["model"]["nodes"], b).is_none());
    let response = controller.core_request(&json!({"op": "undo"}));
    assert_eq!(response["succeeded"], true);
    assert!(find_node(&controller.snapshot()["model"]["nodes"], b).is_some());
    assert_eq!(controller.selected_node(), None);

    controller.select_node_target(b).unwrap();
    ok(&controller.node_action(NodeAction::Delete));
    assert_eq!(controller.selected_node(), None);
    assert!(find_node(&controller.snapshot()["model"]["nodes"], b).is_none());
    let response = controller.core_request(&json!({"op": "undo"}));
    assert_eq!(response["succeeded"], true);
    assert!(find_node(&controller.snapshot()["model"]["nodes"], b).is_some());
    assert_eq!(controller.selected_node(), None);
}

#[test]
fn dirty_draft_gates_mutating_actions_but_not_copy() {
    let mut controller = controller();
    controller.select_node_target(2).unwrap();
    controller.select_node(2).unwrap();
    controller.set_draft_name("Draft".to_owned());
    assert!(controller.draft_dirty());
    let before = controller.snapshot().clone();
    let revision = before["revision"].as_u64().unwrap();

    for action in [
        NodeAction::Create { class_name: "Node", name: Some("New") },
        NodeAction::Cut,
        NodeAction::Paste,
        NodeAction::Delete,
        NodeAction::Reparent { parent: 1 },
        NodeAction::ToggleVisibility,
    ] {
        let response = controller.node_action(action);
        assert_eq!(response["succeeded"], false, "{response}");
        assert_eq!(response["message"], "Apply or discard the draft before editing nodes");
        assert_eq!(controller.snapshot(), &before);
        assert!(controller.draft_dirty());
    }

    let response = controller.node_action(NodeAction::Copy);
    assert_eq!(response["succeeded"], true, "{response}");
    assert_eq!(controller.snapshot()["revision"], revision);
    assert!(controller.draft_dirty());
    assert!(!controller.draft_stale());
    assert_eq!(controller.selected_node(), Some(2));
    match controller.draft_view() {
        Some(DraftView::NodeName { value, .. }) => assert_eq!(value, "Draft"),
        _ => panic!("expected a node name draft"),
    }
}

#[test]
fn toggle_visibility_requires_selection_and_preserves_it() {
    let mut controller = controller();
    let before = controller.snapshot().clone();
    let response = controller.node_action(NodeAction::ToggleVisibility);
    assert_eq!(response["succeeded"], false, "{response}");
    assert_eq!(response["message"], "Select a node first");
    assert_eq!(controller.snapshot(), &before);

    controller.select_node_target(2).unwrap();
    let response = ok(&controller.node_action(NodeAction::ToggleVisibility)).clone();
    assert_eq!(response, json!({"uuid": 2, "enabled": false}));
    assert_eq!(controller.selected_node(), Some(2));
    assert_eq!(find_node(&controller.snapshot()["model"]["nodes"], 2).unwrap()["enabled"], false);

    ok(&controller.core_request(&json!({"op": "undo"})));
    assert_eq!(find_node(&controller.snapshot()["model"]["nodes"], 2).unwrap()["enabled"], true);
    assert_eq!(controller.selected_node(), Some(2));
    ok(&controller.core_request(&json!({"op": "redo"})));
    assert_eq!(find_node(&controller.snapshot()["model"]["nodes"], 2).unwrap()["enabled"], false);
    assert_eq!(controller.selected_node(), Some(2));

    let response = ok(&controller.node_action(NodeAction::ToggleVisibility)).clone();
    assert_eq!(response, json!({"uuid": 2, "enabled": true}));
    assert_eq!(controller.selected_node(), Some(2));
}

#[test]
fn toggle_visibility_allows_explicit_root_selection() {
    let mut controller = controller();
    controller.select_node_target(1).unwrap();
    let response = ok(&controller.node_action(NodeAction::ToggleVisibility)).clone();
    assert_eq!(response, json!({"uuid": 1, "enabled": false}));
    assert_eq!(controller.selected_node(), Some(1));
    assert_eq!(controller.snapshot()["model"]["nodes"]["enabled"], false);

    ok(&controller.core_request(&json!({"op": "undo"})));
    assert_eq!(controller.snapshot()["model"]["nodes"]["enabled"], true);
    assert_eq!(controller.selected_node(), Some(1));
    ok(&controller.core_request(&json!({"op": "redo"})));
    assert_eq!(controller.snapshot()["model"]["nodes"]["enabled"], false);
    assert_eq!(controller.selected_node(), Some(1));
}

#[test]
fn toggle_visibility_stales_clean_draft_and_refuses_apply() {
    let mut controller = controller();
    controller.select_node_target(2).unwrap();
    controller.select_node(2).unwrap();
    assert!(!controller.draft_dirty());
    assert!(!controller.draft_stale());

    let response = ok(&controller.node_action(NodeAction::ToggleVisibility)).clone();
    assert_eq!(response, json!({"uuid": 2, "enabled": false}));
    assert_eq!(controller.selected_node(), Some(2));
    assert!(controller.draft_stale(), "changed toggle must stale the open draft");
    match controller.draft_view() {
        Some(DraftView::NodeName { value, .. }) => assert_eq!(value, "A"),
        _ => panic!("draft input lost"),
    }

    let applied = controller.apply_draft();
    assert_eq!(applied["succeeded"], false, "{applied}");
    assert_eq!(applied["message"], "draft is stale; discard it and reselect");
    match controller.draft_view() {
        Some(DraftView::NodeName { value, .. }) => assert_eq!(value, "A"),
        _ => panic!("refused apply must keep the draft input"),
    }
    assert_eq!(
        find_node(&controller.snapshot()["model"]["nodes"], 2).unwrap()["name"],
        "A",
        "refused apply must not rename the node"
    );
}

#[test]
fn direct_mcp_write_stales_draft_without_input_loss() {
    let mut controller = controller();
    controller.select_node(2).unwrap();
    controller.set_draft_name("Draft text".to_owned());
    assert!(controller.draft_dirty());

    let response = mcp_call(
        &mut controller,
        1,
        "node_create",
        json!({"className": "Node", "name": "External"}),
    );
    assert_eq!(response["result"]["isError"], false, "{response}");
    assert!(controller.draft_stale());
    match controller.draft_view() {
        Some(DraftView::NodeName { value, .. }) => assert_eq!(value, "Draft text"),
        _ => panic!("draft lost"),
    }
    assert_eq!(child_names(controller.snapshot()), vec!["A", "External"]);
}

#[test]
fn mcp_delete_clears_selection_and_reused_uuid_stays_unselected() {
    let mut controller = controller();
    controller.select_node_target(2).unwrap();
    let response = mcp_call(&mut controller, 1, "node_delete", json!({"nodes": [2]}));
    assert_eq!(response["result"]["isError"], false, "{response}");
    assert_eq!(controller.selected_node(), None);
    assert!(find_node(&controller.snapshot()["model"]["nodes"], 2).is_none());

    let response = mcp_call(
        &mut controller,
        2,
        "node_create",
        json!({"className": "Node", "name": "Reused"}),
    );
    assert_eq!(response["result"]["isError"], false, "{response}");
    let created = response["result"]["structuredContent"]["created"][0]["uuid"]
        .as_u64()
        .unwrap();
    assert_eq!(created, 2);
    assert_eq!(controller.selected_node(), None);
}

#[test]
fn replacement_clears_selection_failure_preserves() {
    let mut controller = controller();
    controller.select_node_target(2).unwrap();

    let directory = temp_dir();
    let missing = directory.join("missing.inx");
    let failed = controller.core_request(&json!({"op": "open", "path": missing}));
    assert_eq!(failed["succeeded"], false, "{failed}");
    assert_eq!(controller.selected_node(), Some(2));

    let saved = directory.join("saved.inx");
    let response = controller.core_request(&json!({"op": "save", "path": saved}));
    assert_eq!(response["succeeded"], true, "{response}");
    controller.select_node_target(2).unwrap();

    let response = controller.core_request(&json!({"op": "open", "path": saved}));
    assert_eq!(response["succeeded"], true, "{response}");
    assert_eq!(controller.selected_node(), None);

    controller.select_node_target(2).unwrap();
    let response = mcp_call(
        &mut controller,
        1,
        "open",
        json!({"path": saved.to_str().unwrap()}),
    );
    assert_eq!(response["result"]["isError"], false, "{response}");
    assert_eq!(controller.selected_node(), None);

    controller.select_node_target(2).unwrap();
    let response = mcp_call(
        &mut controller,
        2,
        "open",
        json!({"path": missing.to_str().unwrap()}),
    );
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert_eq!(controller.selected_node(), Some(2));
}

#[test]
fn new_replacement_clears_selection_failure_preserves() {
    let mut controller = controller();
    controller.select_node_target(2).unwrap();
    controller.select_node(2).unwrap();
    controller.set_draft_name("Draft".to_owned());
    let before = controller.snapshot().clone();
    let failed = controller.core_request(&json!({"op": "new", "model": {"param": []}}));
    assert_eq!(failed["succeeded"], false, "{failed}");
    assert_eq!(controller.snapshot(), &before);
    assert_eq!(controller.selected_node(), Some(2));
    assert!(controller.draft_dirty());
    match controller.draft_view() {
        Some(DraftView::NodeName { value, .. }) => assert_eq!(value, "Draft"),
        _ => panic!("draft lost"),
    }

    controller.discard_draft();
    let failed = controller.core_request(&json!({"op": "new", "model": {"param": []}}));
    assert_eq!(failed["succeeded"], false, "{failed}");
    assert_eq!(controller.snapshot(), &before);
    assert_eq!(controller.selected_node(), Some(2));

    let response = controller.core_request(&json!({"op": "new", "model": fixture()}));
    assert_eq!(response["succeeded"], true, "{response}");
    assert!(find_node(&controller.snapshot()["model"]["nodes"], 2).is_some());
    assert_eq!(controller.selected_node(), None);

    let renamed = controller.core_request(&json!({"op": "node.rename", "id": 2, "name": "Renamed"}));
    assert_eq!(renamed["succeeded"], true, "{renamed}");
    assert_eq!(controller.snapshot()["modified"], true);
    controller.select_node_target(2).unwrap();
    let before = controller.snapshot().clone();
    let failed = mcp_call(&mut controller, 1, "new", json!({"model": {"param": []}}));
    assert_eq!(failed["result"]["isError"], true, "{failed}");
    assert_eq!(controller.snapshot(), &before);
    assert_eq!(controller.selected_node(), Some(2));

    let directory = temp_dir();
    let saved = directory.join("newtest.inx");
    let response = controller.core_request(&json!({"op": "save", "path": saved}));
    assert_eq!(response["succeeded"], true, "{response}");
    let before = controller.snapshot().clone();
    let failed = mcp_call(&mut controller, 2, "new", json!({"model": {"param": []}}));
    assert_eq!(failed["result"]["isError"], true, "{failed}");
    assert_eq!(controller.snapshot(), &before);
    assert_eq!(controller.selected_node(), Some(2));

    let response = mcp_call(&mut controller, 3, "new", json!({"model": fixture()}));
    assert_eq!(response["result"]["isError"], false, "{response}");
    assert!(find_node(&controller.snapshot()["model"]["nodes"], 2).is_some());
    assert_eq!(controller.selected_node(), None);
}

#[test]
fn invalid_parents_reject_without_model_change() {
    let mut controller = controller();
    controller.select_node_target(2).unwrap();
    let before = controller.snapshot().clone();
    let response = controller.node_action(NodeAction::Create {
        class_name: "DepthBone",
        name: Some("Bone"),
    });
    assert_eq!(response["succeeded"], false, "{response}");
    assert_eq!(controller.snapshot(), &before);

    let created = ok(&controller.node_action(NodeAction::Create {
        class_name: "Mask",
        name: Some("Cover"),
    }))
    .clone();
    let mask = created["created"][0]["uuid"].as_u64().unwrap();
    let before = controller.snapshot().clone();
    let response = controller.node_action(NodeAction::Create {
        class_name: "Node",
        name: Some("Inner"),
    });
    assert_eq!(response["succeeded"], false, "{response}");
    assert_eq!(controller.snapshot(), &before);

    controller.select_node_target(1).unwrap();
    let rig = ok(&controller.node_action(NodeAction::Create {
        class_name: "DepthRigRoot",
        name: Some("Rig"),
    }))
    .clone();
    let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
    assert_ne!(rig_id, mask);
    let bone = ok(&controller.node_action(NodeAction::Create {
        class_name: "DepthBone",
        name: Some("Bone"),
    }))
    .clone();
    let bone_node = &bone["created"][0];
    assert_eq!(bone_node["type"], "DepthBone");
    assert_eq!(bone_node["restHead"], json!([0.0, 0.0, 0.0]));
    assert_eq!(bone_node["restTail"], json!([0.0, 100.0, 0.0]));
    let bone_id = bone_node["uuid"].as_u64().unwrap();
    assert!(find_node(&controller.snapshot()["model"]["nodes"], rig_id)
        .unwrap()["children"]
        .as_array()
        .unwrap()
        .iter()
        .any(|child| child["uuid"].as_u64() == Some(bone_id)));
}

#[test]
fn all_eleven_classes_create_literal_types() {
    let mut controller = controller();
    let rig_created = ok(&controller.node_action(NodeAction::Create {
        class_name: "DepthRigRoot",
        name: Some("RigParent"),
    }))
    .clone();
    assert_eq!(rig_created["created"][0]["type"], "DepthRigRoot");
    let rig = rig_created["created"][0]["uuid"].as_u64().unwrap();
    for class_name in [
        "Node",
        "Composite",
        "MeshGroup",
        "DynamicComposite",
        "Camera",
        "Mask",
        "PathDeformer",
        "GridDeformer",
        "SimplePhysics",
    ] {
        controller.select_node_target(1).unwrap();
        let created = ok(&controller.node_action(NodeAction::Create {
            class_name,
            name: Some(class_name),
        }))
        .clone();
        assert_eq!(created["created"][0]["type"], class_name);
    }
    controller.select_node_target(rig).unwrap();
    let bone = ok(&controller.node_action(NodeAction::Create {
        class_name: "DepthBone",
        name: Some("DepthBone"),
    }))
    .clone();
    assert_eq!(bone["created"][0]["type"], "DepthBone");
}

#[test]
fn root_mutations_fail_with_core_messages() {
    let mut controller = controller();
    controller.select_node_target(1).unwrap();
    let before = controller.snapshot().clone();
    for (action, message) in [
        (NodeAction::Copy, "the root node cannot be copied in Rust Core mode"),
        (NodeAction::Delete, "the root node cannot be deleted in Rust Core mode"),
        (NodeAction::Cut, "the root node cannot be copied in Rust Core mode"),
    ] {
        let response = controller.node_action(action);
        assert_eq!(response["succeeded"], false, "{response}");
        assert_eq!(response["message"], message, "{response}");
        assert_eq!(controller.snapshot(), &before);
    }
    let response = controller.node_action(NodeAction::Reparent { parent: 2 });
    assert_eq!(response["succeeded"], false, "{response}");
    assert_eq!(
        response["message"],
        "the root node cannot be moved in Rust Core mode"
    );
    assert_eq!(controller.snapshot(), &before);
}

#[test]
fn insert_wraps_with_suffix_and_selects_wrapper() {
    let mut controller = controller();
    controller.select_node_target(2).unwrap();
    let created = ok(&controller.node_action(NodeAction::Insert {
        class_name: "MeshGroup",
        suffix: Some(" Group"),
    }))
    .clone();
    let wrapper = &created["created"][0];
    assert_eq!(wrapper["type"], "MeshGroup");
    assert_eq!(wrapper["name"], "A Group");
    assert_eq!(wrapper["transform"]["trans"], json!([10.0, 0.0, 0.0]));
    let wrapper_id = wrapper["uuid"].as_u64().unwrap();
    assert_eq!(controller.selected_node(), Some(wrapper_id));
    let child = find_node(wrapper, 2).unwrap();
    assert_eq!(child["transform"]["trans"], json!([0.0, 0.0, 0.0]));
    assert_eq!(child["name"], "A");
    assert_eq!(child_names(controller.snapshot()), vec!["A Group"]);
}

#[test]
fn insert_default_and_whitespace_suffix_names() {
    for suffix in [None, Some("")] {
        let mut controller = controller();
        controller.select_node_target(2).unwrap();
        let created = ok(&controller.node_action(NodeAction::Insert {
            class_name: "MeshGroup",
            suffix,
        }))
        .clone();
        assert_eq!(created["created"][0]["name"], "Unnamed MeshGroup");
        assert_eq!(created["created"][0]["type"], "MeshGroup");
    }
    let mut controller = controller();
    controller.select_node_target(2).unwrap();
    let created = ok(&controller.node_action(NodeAction::Insert {
        class_name: "MeshGroup",
        suffix: Some("  "),
    }))
    .clone();
    assert_eq!(created["created"][0]["name"], "A  ");
}

#[test]
fn insert_failures_preserve_snapshot_mask_target_wraps() {
    let mut controller = controller();
    controller.select_node_target(1).unwrap();
    let before = controller.snapshot().clone();
    let response = controller.node_action(NodeAction::Insert {
        class_name: "MeshGroup",
        suffix: None,
    });
    assert_eq!(response["succeeded"], false, "{response}");
    assert_eq!(
        response["message"],
        "the root node cannot be wrapped in Rust Core mode"
    );
    assert_eq!(controller.snapshot(), &before);
    assert_eq!(controller.selected_node(), Some(1));

    controller.select_node_target(2).unwrap();
    for class_name in ["DepthBone", "Mask"] {
        let before = controller.snapshot().clone();
        let response = controller.node_action(NodeAction::Insert {
            class_name,
            suffix: None,
        });
        assert_eq!(response["succeeded"], false, "{response}");
        assert_eq!(controller.snapshot(), &before);
        assert_eq!(controller.selected_node(), Some(2));
    }

    let created = ok(&controller.node_action(NodeAction::Create {
        class_name: "Mask",
        name: Some("Cover"),
    }))
    .clone();
    let mask_id = created["created"][0]["uuid"].as_u64().unwrap();
    controller.select_node_target(mask_id).unwrap();
    let wrapped = ok(&controller.node_action(NodeAction::Insert {
        class_name: "MeshGroup",
        suffix: None,
    }))
    .clone();
    let wrapper = &wrapped["created"][0];
    assert_eq!(wrapper["type"], "MeshGroup");
    assert!(find_node(wrapper, mask_id).is_some());
    assert_eq!(controller.selected_node(), wrapper["uuid"].as_u64());
}

#[test]
fn insert_undo_redo_restores_model_and_clears_selection() {
    let mut controller = controller();
    controller.select_node_target(2).unwrap();
    let before = controller.snapshot().clone();
    let created = ok(&controller.node_action(NodeAction::Insert {
        class_name: "MeshGroup",
        suffix: Some(" Group"),
    }))
    .clone();
    let wrapper_id = created["created"][0]["uuid"].as_u64().unwrap();
    assert_eq!(controller.selected_node(), Some(wrapper_id));

    let response = controller.core_request(&json!({"op": "undo"}));
    assert_eq!(response["succeeded"], true, "{response}");
    assert_eq!(controller.snapshot()["model"], before["model"]);
    assert_eq!(controller.selected_node(), None);

    let response = controller.core_request(&json!({"op": "redo"}));
    assert_eq!(response["succeeded"], true, "{response}");
    assert_eq!(controller.selected_node(), None);
    let wrapper = find_node(&controller.snapshot()["model"]["nodes"], wrapper_id)
        .expect("redo must recreate the wrapper");
    assert_eq!(wrapper["type"], "MeshGroup");
    assert_eq!(wrapper["name"], "A Group");
    assert!(find_node(wrapper, 2).is_some());
}

#[test]
fn convert_retains_uuid_and_undo_redo_restores_types() {
    let mut controller = controller();
    controller.select_node_target(2).unwrap();
    let converted = ok(&controller.node_action(NodeAction::Convert {
        class_name: "MeshGroup",
    }))
    .clone();
    let node = &converted["converted"][0];
    assert_eq!(node["uuid"], 2);
    assert_eq!(node["type"], "MeshGroup");
    assert_eq!(node["name"], "A");
    assert_eq!(node["transform"]["trans"], json!([10.0, 0.0, 0.0]));
    assert_eq!(controller.selected_node(), Some(2));

    let before = controller.snapshot().clone();
    let response = controller.node_action(NodeAction::Convert {
        class_name: "MeshGroup",
    });
    assert_eq!(response["succeeded"], false, "{response}");
    assert_eq!(response["message"], "No nodes converted");
    assert_eq!(controller.snapshot(), &before);
    assert_eq!(controller.selected_node(), Some(2));

    let response = controller.core_request(&json!({"op": "undo"}));
    assert_eq!(response["succeeded"], true, "{response}");
    assert_eq!(
        find_node(&controller.snapshot()["model"]["nodes"], 2).unwrap()["type"],
        "Node"
    );
    let response = controller.core_request(&json!({"op": "redo"}));
    assert_eq!(response["succeeded"], true, "{response}");
    assert_eq!(
        find_node(&controller.snapshot()["model"]["nodes"], 2).unwrap()["type"],
        "MeshGroup"
    );
}

#[test]
fn insert_and_convert_require_selection() {
    let mut controller = controller();
    let before = controller.snapshot().clone();
    for action in [
        NodeAction::Insert { class_name: "MeshGroup", suffix: None },
        NodeAction::Convert { class_name: "MeshGroup" },
    ] {
        let response = controller.node_action(action);
        assert_eq!(response["succeeded"], false, "{response}");
        assert_eq!(response["message"], "Select a node first");
        assert_eq!(controller.snapshot(), &before);
    }
}

#[test]
fn dirty_draft_blocks_insert_and_convert() {
    let mut controller = controller();
    controller.select_node_target(2).unwrap();
    controller.select_node(2).unwrap();
    controller.set_draft_name("Draft".to_owned());
    let before = controller.snapshot().clone();
    for action in [
        NodeAction::Insert { class_name: "MeshGroup", suffix: Some(" Group") },
        NodeAction::Convert { class_name: "MeshGroup" },
    ] {
        let response = controller.node_action(action);
        assert_eq!(response["succeeded"], false, "{response}");
        assert_eq!(
            response["message"],
            "Apply or discard the draft before editing nodes"
        );
        assert_eq!(controller.snapshot(), &before);
    }
    let response = controller.node_action(NodeAction::Copy);
    assert_eq!(response["succeeded"], true, "{response}");
    assert_eq!(controller.snapshot()["clipboard"][0]["source"], 2);
    match controller.draft_view() {
        Some(DraftView::NodeName { value, .. }) => assert_eq!(value, "Draft"),
        _ => panic!("draft lost"),
    }
}

#[test]
fn convert_matrix_matches_literal_choices() {
    assert_eq!(
        node_convert_classes("Node"),
        ["MeshGroup", "DynamicComposite", "GridDeformer"].as_slice()
    );
    assert_eq!(
        node_convert_classes("DynamicComposite"),
        ["MeshGroup", "Node", "Part", "Composite"].as_slice()
    );
    assert_eq!(
        node_convert_classes("MeshGroup"),
        ["DynamicComposite", "Node", "GridDeformer"].as_slice()
    );
    assert_eq!(
        node_convert_classes("Composite"),
        ["DynamicComposite", "Node"].as_slice()
    );
    assert_eq!(
        node_convert_classes("GridDeformer"),
        ["MeshGroup", "Node"].as_slice()
    );
    for current_type in [
        "Part",
        "Camera",
        "Mask",
        "PathDeformer",
        "SimplePhysics",
        "DepthRigRoot",
        "DepthBone",
        "Bogus",
    ] {
        assert!(node_convert_classes(current_type).is_empty(), "{current_type}");
    }
}
