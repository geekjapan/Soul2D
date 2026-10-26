use serde_json::{json, Value};
use soul2d_core::editor_controller::{
    DropSite, EditorController, NodeAction, NODE_CONVERT_MIXED_MESSAGE, NODE_INSERT_CLASSES,
};

fn node(uuid: u64, name: &str, kind: &str, children: Value) -> Value {
    json!({
        "uuid": uuid, "name": name, "type": kind, "enabled": true, "zsort": 0.0,
        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
        "lockToRoot": false, "pinToMesh": false, "children": children,
    })
}

/// 1 Root: 2 A (5 A1, 6 A2), 3 B Composite (7 B1), 8 D. Row order is 1 2 5 6 3 7 8.
fn fixture() -> Value {
    json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": node(1, "Root", "Node", json!([
            node(2, "A", "Node", json!([node(5, "A1", "Node", json!([])), node(6, "A2", "Node", json!([]))])),
            node(3, "B", "Composite", json!([node(7, "B1", "Node", json!([]))])),
            node(8, "D", "Node", json!([])),
        ])),
        "param": [
            {"uuid": 20, "name": "Eyes", "is_vec2": false, "min": [-1.0, 0.0], "max": [1.0, 1.0],
             "defaults": [0.0, 0.0], "axis_points": [[0.0, 0.5, 1.0], [0.0]], "merge_mode": "Additive",
             "bindings": [{"node": 5, "param_name": "transform.t.x", "interpolate_mode": "Linear",
                           "values": [[1.0], [2.0], [3.0]], "isSet": [[true], [true], [true]]}]},
        ],
        "animations": {},
    })
}

const ROWS: [u64; 7] = [1, 2, 5, 6, 3, 7, 8];

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

fn find_node(node: &Value, id: u64) -> Option<&Value> {
    if node["uuid"].as_u64() == Some(id) {
        return Some(node);
    }
    node["children"].as_array()?.iter().find_map(|child| find_node(child, id))
}

fn model(controller: &EditorController) -> Value {
    controller.snapshot()["model"].clone()
}

fn children(controller: &EditorController, id: u64) -> Vec<u64> {
    find_node(&controller.snapshot()["model"]["nodes"], id).unwrap()["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|child| child["uuid"].as_u64().unwrap())
        .collect()
}

fn names(controller: &EditorController, id: u64) -> Vec<String> {
    find_node(&controller.snapshot()["model"]["nodes"], id).unwrap()["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|child| child["name"].as_str().unwrap().to_owned())
        .collect()
}

fn enabled(controller: &EditorController, id: u64) -> bool {
    find_node(&controller.snapshot()["model"]["nodes"], id).unwrap()["enabled"]
        .as_bool()
        .unwrap_or(true)
}

fn history_count(controller: &EditorController) -> u64 {
    controller.snapshot()["history"]["count"].as_u64().unwrap()
}

fn undo(controller: &mut EditorController) {
    ok(&controller.core_request(&json!({"op": "undo"})));
}

fn select(controller: &mut EditorController, ids: &[u64]) {
    controller.select_node_target(ids[0]).unwrap();
    for id in &ids[1..] {
        controller.toggle_node_selection(*id).unwrap();
    }
}

/// Runs a failing action and asserts the document, the history and the selection did not change.
fn rejected(controller: &mut EditorController, action: NodeAction<'_>) -> String {
    let before = controller.snapshot().clone();
    let selection = controller.selected_nodes().to_vec();
    let response = controller.node_action(action);
    assert_eq!(response["succeeded"], false, "expected failure: {response}");
    assert_eq!(controller.snapshot(), &before);
    assert_eq!(controller.selected_nodes(), selection);
    response["message"].as_str().unwrap().to_owned()
}

#[test]
fn ctrl_click_toggles_and_the_first_selected_node_stays_primary() {
    let mut controller = controller();
    controller.select_node_target(5).unwrap();
    controller.toggle_node_selection(6).unwrap();
    assert_eq!(controller.selected_nodes(), [5, 6]);
    assert_eq!(controller.selected_node(), Some(5));
    controller.toggle_node_selection(5).unwrap();
    assert_eq!(controller.selected_nodes(), [6]);
    assert_eq!(controller.selected_node(), Some(6), "the next selected node becomes primary");
    controller.toggle_node_selection(6).unwrap();
    assert_eq!(controller.selected_nodes(), [] as [u64; 0]);
    assert_eq!(controller.selected_node(), None);
    // A plain click replaces the whole set.
    select(&mut controller, &[5, 6, 7]);
    controller.select_node_target(8).unwrap();
    assert_eq!(controller.selected_nodes(), [8]);
    // Unknown ids change nothing.
    assert!(controller.toggle_node_selection(99).is_err());
    assert!(controller.select_node_target(99).is_err());
    assert_eq!(controller.selected_nodes(), [8]);
}

#[test]
fn shift_click_selects_the_visible_rows_between_the_anchor_and_the_target() {
    let mut controller = controller();
    controller.select_node_target(5).unwrap();
    controller.select_node_range(3, &ROWS).unwrap();
    assert_eq!(controller.selected_nodes(), [5, 6, 3]);
    // The next Shift-click takes the previous range back out before adding its own.
    controller.select_node_range(8, &ROWS).unwrap();
    assert_eq!(controller.selected_nodes(), [5, 6, 3, 7, 8]);
    controller.select_node_range(6, &ROWS).unwrap();
    assert_eq!(controller.selected_nodes(), [5, 6]);
    // Shift-clicking above the anchor works backwards too.
    controller.select_node_target(7).unwrap();
    controller.select_node_range(2, &ROWS).unwrap();
    assert_eq!(controller.selected_nodes(), [7, 2, 5, 6, 3]);
    assert_eq!(controller.selected_node(), Some(7));
}

#[test]
fn shift_click_keeps_ctrl_selected_rows_outside_the_range() {
    let mut controller = controller();
    controller.select_node_target(2).unwrap();
    controller.toggle_node_selection(8).unwrap();
    // Ctrl-click moved the anchor to 8; Shift-click on 3 spans 3..=8.
    controller.select_node_range(3, &ROWS).unwrap();
    assert_eq!(controller.selected_nodes(), [2, 8, 3, 7]);
    controller.select_node_range(7, &ROWS).unwrap();
    assert_eq!(controller.selected_nodes(), [2, 8, 7]);
}

#[test]
fn shift_click_without_a_visible_anchor_is_a_plain_click() {
    let mut controller = controller();
    controller.select_node_target(5).unwrap();
    // The filter hides the anchor: only rows 1, 3, 7 are visible.
    controller.select_node_range(7, &[1, 3, 7]).unwrap();
    assert_eq!(controller.selected_nodes(), [7]);
    // And with no anchor at all.
    controller.clear_node_selection();
    controller.select_node_range(3, &ROWS).unwrap();
    assert_eq!(controller.selected_nodes(), [3]);
}

#[test]
fn select_all_takes_every_node_but_the_root_in_tree_order() {
    let mut controller = controller();
    controller.select_all_nodes().unwrap();
    assert_eq!(controller.selected_nodes(), [2, 5, 6, 3, 7, 8]);
    assert_eq!(controller.selected_node(), Some(2));
    controller.clear_node_selection();
    assert_eq!(controller.selected_node(), None);
}

#[test]
fn selection_drops_nodes_that_disappear() {
    let mut controller = controller();
    select(&mut controller, &[5, 7, 8]);
    ok(&controller.core_request(&json!({"op": "node.delete", "nodes": [7]})));
    assert_eq!(controller.selected_nodes(), [5, 8]);
    ok(&controller.core_request(&json!({"op": "undo"})));
    assert_eq!(controller.selected_nodes(), [5, 8], "undo does not bring a selection back");
    ok(&controller.core_request(&json!({"op": "new", "model": fixture()})));
    assert_eq!(controller.selected_nodes(), [] as [u64; 0]);
}

#[test]
fn multi_delete_is_one_undo_step_and_clears_the_selection() {
    let mut controller = controller();
    let before = model(&controller);
    let count = history_count(&controller);
    // Selection order is not tree order; the root is skipped like D does.
    select(&mut controller, &[8, 5, 1]);
    ok(&controller.node_action(NodeAction::Delete));
    assert_eq!(children(&controller, 1), [2, 3]);
    assert_eq!(children(&controller, 2), [6]);
    assert_eq!(history_count(&controller), count + 1);
    assert_eq!(controller.selected_nodes(), [] as [u64; 0]);
    // The parameter binding on the deleted node went with it.
    assert_eq!(controller.snapshot()["model"]["param"][0]["bindings"], json!([]));
    undo(&mut controller);
    assert_eq!(model(&controller), before);
}

#[test]
fn deleting_only_the_root_is_still_refused() {
    let mut controller = controller();
    controller.select_node_target(1).unwrap();
    let message = rejected(&mut controller, NodeAction::Delete);
    assert_eq!(message, "the root node cannot be deleted in Rust Core mode");
}

#[test]
fn multi_copy_cut_and_paste_carry_the_whole_selection() {
    let mut controller = controller();
    select(&mut controller, &[7, 5]);
    let copied = ok(&controller.node_action(NodeAction::Copy)).clone();
    assert_eq!(copied["count"], 2);
    assert_eq!(controller.snapshot()["clipboard"][0]["name"], "A1'", "copied in tree order");
    assert_eq!(controller.snapshot()["clipboard"][1]["name"], "B1'");
    controller.select_node_target(8).unwrap();
    let count = history_count(&controller);
    let pasted = ok(&controller.node_action(NodeAction::Paste)).clone();
    assert_eq!(pasted["created"].as_array().unwrap().len(), 2);
    assert_eq!(history_count(&controller), count + 1);
    assert_eq!(controller.selected_nodes().len(), 2, "the pasted nodes are selected");
    let mut pasted_names = names(&controller, 8);
    pasted_names.sort();
    assert_eq!(pasted_names, ["A1'", "B1'"]);

    // Cut two nodes at once, one undo step.
    let before = model(&controller);
    let count = history_count(&controller);
    select(&mut controller, &[6, 2]);
    ok(&controller.node_action(NodeAction::Cut));
    assert_eq!(children(&controller, 1), [3, 8]);
    assert_eq!(history_count(&controller), count + 1);
    assert_eq!(controller.snapshot()["clipboard"].as_array().unwrap().len(), 1, "6 sits inside 2");
    undo(&mut controller);
    assert_eq!(model(&controller), before);
}

#[test]
fn multi_reparent_keeps_tree_order_in_one_undo_step() {
    let mut controller = controller();
    let before = model(&controller);
    let count = history_count(&controller);
    // Selection order 7, 5; tree order 5, 7.
    select(&mut controller, &[7, 5]);
    ok(&controller.node_action(NodeAction::Reparent { parent: 8 }));
    assert_eq!(children(&controller, 8), [5, 7]);
    assert_eq!(children(&controller, 2), [6]);
    assert_eq!(history_count(&controller), count + 1);
    assert_eq!(controller.selected_nodes(), [7, 5], "the selection survives a move");
    undo(&mut controller);
    assert_eq!(model(&controller), before);
}

#[test]
fn reparenting_a_selection_into_itself_is_rejected_unchanged() {
    let mut controller = controller();
    select(&mut controller, &[2, 8]);
    let message = rejected(&mut controller, NodeAction::Reparent { parent: 5 });
    assert_eq!(message, "node 2 cannot be reparented into one of its descendants");
}

#[test]
fn multi_insert_wraps_each_node_with_its_own_suffixed_name() {
    let mut controller = controller();
    let before = model(&controller);
    let count = history_count(&controller);
    select(&mut controller, &[5, 6]);
    let created = ok(&controller.node_action(NodeAction::Insert {
        class_name: "MeshGroup",
        suffix: Some("_grp"),
    }))
    .clone();
    let created = created["created"].as_array().unwrap();
    assert_eq!(created.len(), 2);
    assert_eq!(names(&controller, 2), ["A2_grp", "A1_grp"], "each wrapper lands first in its parent");
    assert_eq!(history_count(&controller), count + 1);
    assert_eq!(controller.selected_nodes().len(), 2);
    undo(&mut controller);
    assert_eq!(model(&controller), before);
}

#[test]
fn insert_without_a_suffix_uses_numbered_default_names() {
    let mut controller = controller();
    select(&mut controller, &[5, 6]);
    ok(&controller.node_action(NodeAction::Insert { class_name: "Node", suffix: None }));
    assert_eq!(names(&controller, 2), ["Unnamed Node 1", "Unnamed Node"]);
}

#[test]
fn insert_classes_stay_the_seven_the_core_wraps() {
    assert_eq!(
        NODE_INSERT_CLASSES,
        ["Node", "Composite", "MeshGroup", "DynamicComposite", "Camera", "PathDeformer", "GridDeformer"]
    );
    let mut controller = controller();
    controller.select_node_target(5).unwrap();
    for class_name in ["Mask", "SimplePhysics", "DepthRigRoot", "DepthBone"] {
        let message = rejected(&mut controller, NodeAction::Insert { class_name, suffix: None });
        if class_name == "Mask" {
            // A Mask cannot hold the node it would wrap.
            assert_eq!(message, "parent node 4 cannot accept children in Rust Core mode");
        } else {
            assert_eq!(message, format!("unsupported node class for Rust Core mode: {class_name}"));
        }
    }
}

#[test]
fn add_creates_a_suffixed_child_under_every_selected_node() {
    let mut controller = controller();
    let before = model(&controller);
    let count = history_count(&controller);
    select(&mut controller, &[2, 3]);
    ok(&controller.node_action(NodeAction::Add { class_name: "Node", suffix: Some("_x") }));
    assert_eq!(names(&controller, 2), ["A1", "A2", "A_x"]);
    assert_eq!(names(&controller, 3), ["B1", "B_x"]);
    assert_eq!(history_count(&controller), count + 1);
    assert_eq!(controller.selected_nodes().len(), 2);
    // The same suffix again collides with the existing names and changes nothing.
    select(&mut controller, &[2, 3]);
    let message = rejected(&mut controller, NodeAction::Add { class_name: "Node", suffix: Some("_x") });
    assert_eq!(message, "Name already taken");
    undo(&mut controller);
    assert_eq!(model(&controller), before);
}

#[test]
fn add_without_a_suffix_or_selection_goes_under_the_root() {
    let mut controller = controller();
    ok(&controller.node_action(NodeAction::Add { class_name: "Composite", suffix: None }));
    assert_eq!(names(&controller, 1), ["A", "B", "D", "Unnamed Composite"]);
}

#[test]
fn multi_convert_needs_one_shared_class() {
    let mut controller = controller();
    select(&mut controller, &[2, 8]);
    let count = history_count(&controller);
    ok(&controller.node_action(NodeAction::Convert { class_name: "MeshGroup" }));
    for id in [2, 8] {
        assert_eq!(find_node(&controller.snapshot()["model"]["nodes"], id).unwrap()["type"], "MeshGroup");
    }
    assert_eq!(history_count(&controller), count + 1);
    assert_eq!(controller.selected_nodes(), [2, 8]);

    select(&mut controller, &[2, 3]);
    let message = rejected(&mut controller, NodeAction::Convert { class_name: "Node" });
    assert_eq!(message, NODE_CONVERT_MIXED_MESSAGE);
    assert_eq!(controller.common_node_type(&[2, 8]).as_deref(), Some("MeshGroup"));
    assert_eq!(controller.common_node_type(&[2, 3]), None);
    assert_eq!(controller.common_node_type(&[2, 99]), None);
    assert_eq!(controller.common_node_type(&[]), None);
}

#[test]
fn visibility_of_a_selection_is_one_undo_step() {
    let mut controller = controller();
    let before = model(&controller);
    let count = history_count(&controller);
    select(&mut controller, &[2, 8]);
    ok(&controller.node_action(NodeAction::ToggleVisibility));
    assert!(!enabled(&controller, 2) && !enabled(&controller, 8));
    assert!(enabled(&controller, 5), "no recursion by default");
    assert_eq!(history_count(&controller), count + 1);
    assert_eq!(controller.selected_nodes(), [2, 8]);
    // Toggling again reads the first node: it is hidden, so both are shown.
    ok(&controller.node_action(NodeAction::ToggleVisibility));
    assert!(enabled(&controller, 2) && enabled(&controller, 8));
    undo(&mut controller);
    undo(&mut controller);
    assert_eq!(model(&controller), before);
}

#[test]
fn recursive_visibility_hides_and_shows_a_whole_subtree() {
    let mut controller = controller();
    let before = model(&controller);
    let count = history_count(&controller);
    // An explicit node list needs no selection, as for a click on a row's visibility button.
    ok(&controller.node_action(NodeAction::SetVisibility { nodes: &[2], enabled: None, recursive: true }));
    for id in [2, 5, 6] {
        assert!(!enabled(&controller, id), "node {id}");
    }
    assert!(enabled(&controller, 3));
    assert_eq!(history_count(&controller), count + 1);
    assert_eq!(controller.selected_nodes(), [] as [u64; 0]);
    ok(&controller.node_action(NodeAction::SetVisibility {
        nodes: &[2],
        enabled: Some(true),
        recursive: true,
    }));
    for id in [2, 5, 6] {
        assert!(enabled(&controller, id), "node {id}");
    }
    undo(&mut controller);
    undo(&mut controller);
    assert_eq!(model(&controller), before);
}

#[test]
fn visibility_of_the_selection_when_no_nodes_are_listed() {
    let mut controller = controller();
    select(&mut controller, &[5, 7]);
    ok(&controller.node_action(NodeAction::SetVisibility { nodes: &[], enabled: Some(false), recursive: false }));
    assert!(!enabled(&controller, 5) && !enabled(&controller, 7));
    controller.clear_node_selection();
    let message = rejected(
        &mut controller,
        NodeAction::SetVisibility { nodes: &[], enabled: Some(true), recursive: false },
    );
    assert_eq!(message, "Select a node first");
    let message = rejected(
        &mut controller,
        NodeAction::SetVisibility { nodes: &[99], enabled: Some(true), recursive: false },
    );
    assert_eq!(message, "node 99 was not found");
}

#[test]
fn a_single_node_toggle_keeps_using_the_core_visibility_op() {
    let mut controller = controller();
    controller.select_node_target(5).unwrap();
    ok(&controller.node_action(NodeAction::ToggleVisibility));
    assert!(!enabled(&controller, 5));
    let history = &controller.snapshot()["history"];
    let label = &history["states"][history["position"].as_u64().unwrap() as usize]["label"];
    assert_eq!(label, "node.enabled.toggle");
}

#[test]
fn dropping_a_selection_moves_it_in_tree_order_in_one_undo_step() {
    let mut controller = controller();
    let before = model(&controller);
    let count = history_count(&controller);
    // 7 sits in B and 6 in A; tree order is 6, 7.
    select(&mut controller, &[7, 6]);
    ok(&controller.node_action(NodeAction::Drop { site: DropSite::Before(5) }));
    assert_eq!(children(&controller, 2), [6, 7, 5]);
    assert_eq!(children(&controller, 3), [] as [u64; 0]);
    assert_eq!(history_count(&controller), count + 1);
    assert_eq!(controller.selected_nodes(), [7, 6]);
    undo(&mut controller);
    assert_eq!(model(&controller), before);

    controller.select_node_target(5).unwrap();
    ok(&controller.node_action(NodeAction::Drop { site: DropSite::After(8) }));
    assert_eq!(children(&controller, 1), [2, 3, 8, 5]);
    ok(&controller.node_action(NodeAction::Drop { site: DropSite::Onto(3) }));
    assert_eq!(children(&controller, 3), [5, 7]);
}

#[test]
fn illegal_and_no_op_drops_leave_the_document_unchanged() {
    let mut controller = controller();
    controller.select_node_target(2).unwrap();
    assert_eq!(
        rejected(&mut controller, NodeAction::Drop { site: DropSite::Onto(5) }),
        "node 2 cannot be dropped into itself or its descendants"
    );
    assert_eq!(
        rejected(&mut controller, NodeAction::Drop { site: DropSite::Before(1) }),
        "nothing can be dropped beside the root"
    );
    controller.select_node_target(1).unwrap();
    assert_eq!(
        rejected(&mut controller, NodeAction::Drop { site: DropSite::Onto(8) }),
        "the root node cannot be moved"
    );
    controller.select_node_target(5).unwrap();
    let before = controller.snapshot().clone();
    let response = controller.node_action(NodeAction::Drop { site: DropSite::After(5) });
    assert_eq!(response["succeeded"], true);
    assert_eq!(response["result"]["moved"], json!([]));
    assert_eq!(controller.snapshot(), &before, "a drop that changes nothing adds no history");
    assert_eq!(controller.resolve_drop(DropSite::After(5)), Ok(None));
    assert_eq!(controller.resolve_drop(DropSite::Onto(8)), Ok(Some((8, 0))));
    assert_eq!(controller.resolve_drop(DropSite::Before(6)), Ok(None));
}

#[test]
fn drops_into_a_mask_are_refused() {
    let mut controller = controller();
    let created = ok(&controller.core_request(&json!({
        "op": "node.create", "className": "Mask", "parents": [1], "name": "Cover"
    })))
    .clone();
    let mask = created["created"][0]["uuid"].as_u64().unwrap();
    controller.select_node_target(5).unwrap();
    let message = rejected(&mut controller, NodeAction::Drop { site: DropSite::Onto(mask) });
    assert_eq!(message, "Mask nodes cannot accept children");
}

#[test]
fn parameter_usage_lists_the_parameters_bound_to_a_node() {
    let controller = controller();
    let usage = controller.parameter_usage(5);
    assert_eq!(usage.len(), 1);
    assert_eq!((usage[0].uuid, usage[0].name.as_str()), (20, "Eyes"));
    assert!(controller.parameter_usage(6).is_empty());
    assert!(controller.parameter_usage(u64::MAX).is_empty());
}

#[test]
fn a_dirty_rename_draft_blocks_the_new_actions_too() {
    let mut controller = controller();
    controller.select_node(5).unwrap();
    controller.set_draft_name("Changed".into());
    select(&mut controller, &[5, 6]);
    for action in [
        NodeAction::Add { class_name: "Node", suffix: None },
        NodeAction::SetVisibility { nodes: &[5], enabled: Some(false), recursive: false },
        NodeAction::Drop { site: DropSite::Onto(8) },
    ] {
        let message = rejected(&mut controller, action);
        assert_eq!(message, "Apply or discard the draft before editing nodes");
    }
}

#[test]
fn multi_node_actions_run_through_mcp_and_the_gui_controller_alike() {
    // The MCP tools take the same id lists the panel sends.
    let mut controller = controller();
    let response = controller
        .mcp_request(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "node_move", "arguments": {"nodes": [7, 5], "parent": 8}},
        }))
        .unwrap();
    assert_eq!(response["result"]["isError"], false, "{response}");
    assert_eq!(children(&controller, 8), [5, 7]);
}
