use serde_json::{json, Value};

use crate::Session;

pub const FILE_OP_BLOCKED_MESSAGE: &str = "Apply or discard the draft before file operations";
pub const UNSAVED_REPLACEMENT_MESSAGE: &str =
    "Save the current document before replacing it";
pub const RESELECT_BLOCKED_MESSAGE: &str =
    "Apply or discard the draft before selecting another edit";
pub const NODE_ACTION_BLOCKED_MESSAGE: &str =
    "Apply or discard the draft before editing nodes";
pub const NODE_SELECT_REQUIRED_MESSAGE: &str = "Select a node first";
pub const NODE_CONVERT_MIXED_MESSAGE: &str = "Select nodes of one class to convert";
pub const NODE_CREATE_CLASSES: &[&str] = crate::nodes::SUPPORTED_CREATE_TYPES;
pub const NODE_INSERT_CLASSES: &[&str] = &[
    "Node",
    "Composite",
    "MeshGroup",
    "DynamicComposite",
    "Camera",
    "PathDeformer",
    "GridDeformer",
];

pub use crate::nodes::{DropSite, ParameterUse};

pub fn node_convert_classes(current_type: &str) -> &'static [&'static str] {
    match current_type {
        "Node" => &["MeshGroup", "DynamicComposite", "GridDeformer"],
        "DynamicComposite" => &["MeshGroup", "Node", "Part", "Composite"],
        "MeshGroup" => &["DynamicComposite", "Node", "GridDeformer"],
        "Composite" => &["DynamicComposite", "Node"],
        "GridDeformer" => &["MeshGroup", "Node"],
        _ => &[],
    }
}

pub enum NodeAction<'a> {
    Create {
        class_name: &'a str,
        name: Option<&'a str>,
    },
    Copy,
    Cut,
    Paste,
    Delete,
    Reparent {
        parent: u64,
    },
    Insert {
        class_name: &'a str,
        suffix: Option<&'a str>,
    },
    Convert {
        class_name: &'a str,
    },
    ToggleVisibility,
    /// D "Add" with a suffix: a child under every selected node (the root when nothing is
    /// selected), named after its parent plus `suffix`.
    Add {
        class_name: &'a str,
        suffix: Option<&'a str>,
    },
    /// Hide or show `nodes` (the selection when empty) as one undo step. `enabled: None` toggles
    /// from the first node. `recursive` carries each node's subtree along.
    SetVisibility {
        nodes: &'a [u64],
        enabled: Option<bool>,
        recursive: bool,
    },
    /// Lock or unlock `nodes` as one undo step. `recursive` carries each node's subtree along.
    SetLock {
        nodes: &'a [u64],
        locked: bool,
        recursive: bool,
    },
    /// Move the selection to a drop site of the tree, in tree order, as one undo step.
    Drop {
        site: DropSite,
    },
}

/// Check draft and unsaved-document gates for new, open, recovery, import, save and INP export operations.
pub fn file_op_gate(dirty_draft: bool, modified: bool, request: &Value) -> Result<(), String> {
    match request.get("op").and_then(Value::as_str) {
        Some("new") | Some("open") | Some("import.psd") | Some("import.kra") | Some("import.inp")
        | Some("import.folder") | Some("editor.recovery.restore") | Some("save") | Some("export.inp") => {}
        _ => return Ok(()),
    }
    if dirty_draft {
        return Err(FILE_OP_BLOCKED_MESSAGE.to_owned());
    }
    if modified
        && matches!(
            request.get("op").and_then(Value::as_str),
            Some("new") | Some("open") | Some("import.psd") | Some("import.kra") | Some("import.inp") | Some("import.folder")
                | Some("editor.recovery.restore")
        )
    {
        return Err(UNSAVED_REPLACEMENT_MESSAGE.to_owned());
    }
    Ok(())
}

enum EditDraft {
    NodeName {
        id: u64,
        revision: u64,
        original: String,
        value: String,
    },
    Can3Anchor {
        scene_guid: String,
        track_guid: String,
        curve_id: String,
        part_opacity_map_key: Option<String>,
        frame: i64,
        revision: u64,
        original: f64,
        value: f64,
    },
}

impl EditDraft {
    fn revision(&self) -> u64 {
        match self {
            Self::NodeName { revision, .. } | Self::Can3Anchor { revision, .. } => *revision,
        }
    }
}

pub enum DraftView<'a> {
    NodeName {
        id: u64,
        original: &'a str,
        value: &'a str,
    },
    Can3Anchor {
        scene_guid: &'a str,
        track_guid: &'a str,
        curve_id: &'a str,
        part_opacity_map_key: Option<&'a str>,
        frame: i64,
        original: f64,
        value: f64,
    },
}

pub struct EditorController {
    session: Session,
    snapshot: Value,
    draft: Option<EditDraft>,
    status: String,
    playing: Option<PlaybackTarget>,
    selection: NodeSelection,
    /// The user answered "Don't save": the next document-replacing request may discard unsaved changes.
    discard_unsaved: bool,
    /// An MCP request replaced the document; the window takes this to drop what it held for the old one.
    document_replaced: bool,
    /// Unapplied drafts held by editor panels outside the controller; they block remote file operations too.
    pub panel_drafts: bool,
    /// Why the window refuses a document replacement now (a dialog waits for the user's answer); the window
    /// sets it before each MCP message.
    pub replacement_refusal: Option<String>,
    /// The last playback tick stepped INX physics itself; the viewport's per-frame step is then skipped.
    physics_stepped_in_tick: bool,
    /// The viewport's "Enable physics": whether a Motion3 tick steps INX physics.
    pub playback_physics: bool,
}

/// Longest dt one playback tick may advance a Motion3 clip by, so a stalled frame does not jump the clip
/// (D capped it at 0.1 s). A `motion3.advance {dt}` request outside the tick honours the dt it is given.
const MOTION3_MAX_TICK_DT: f64 = 0.1;

/// True when the node tree holds a SimplePhysics driver that is assigned to a parameter.
fn has_assigned_physics(node: &Value) -> bool {
    let assigned = node["type"] == "SimplePhysics"
        && node["param"].as_u64() != Some(u64::from(crate::physics::UNASSIGNED_PARAM));
    assigned || node["children"].as_array().is_some_and(|children| children.iter().any(has_assigned_physics))
}

/// Requests that replace the open document (`new`, `open` and the imports). The editor asks the user about
/// unsaved changes before sending one; `mcp_request` and a plain `core_request` still refuse them.
pub fn replaces_document(request: &Value) -> bool {
    matches!(
        request.get("op").and_then(Value::as_str),
        Some("new" | "open" | "import.psd" | "import.kra" | "import.inp" | "import.folder")
    )
}

/// The selected nodes in selection order; the first is the primary (D `selectedNodes[0]`).
#[derive(Default)]
struct NodeSelection {
    ids: Vec<u64>,
    /// Shift-click anchor (D `lastClick`).
    anchor: Option<u64>,
    /// Rows the last Shift-click added, taken back out by the next one (D `rangeSelectNodes`).
    range: Vec<u64>,
}

impl NodeSelection {
    fn set(&mut self, ids: Vec<u64>) {
        self.anchor = ids.first().copied();
        self.ids = ids;
        self.range.clear();
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

enum PlaybackTarget {
    Inx(String),
    NativeScene(String),
    Motion3(String),
}

impl EditorController {
    pub fn new() -> Self {
        let session = Session::new();
        let snapshot = session.snapshot();
        Self {
            session,
            snapshot,
            draft: None,
            status: String::new(),
            playing: None,
            selection: NodeSelection::default(),
            discard_unsaved: false,
            document_replaced: false,
            panel_drafts: false,
            replacement_refusal: None,
            physics_stepped_in_tick: false,
            playback_physics: true,
        }
    }

    /// Lets the next document-replacing `core_request` discard unsaved changes (the user chose "Don't save").
    /// Cleared by that request, or by `allow_replace_unsaved(false)`.
    pub fn allow_replace_unsaved(&mut self, allowed: bool) {
        self.discard_unsaved = allowed;
    }

    pub fn snapshot(&self) -> &Value {
        &self.snapshot
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn set_status(&mut self, status: String) {
        self.status = status;
    }

    pub fn playing(&self) -> bool {
        self.playing.is_some()
    }

    pub fn pause(&mut self) {
        self.playing = None;
    }

    pub fn play(&mut self, name: &str) -> Value {
        let active = &self.snapshot["animation"];
        let length = self.snapshot["model"]["animations"][name]["length"].as_f64();
        let frame = active["frame"]
            .as_f64()
            .filter(|frame| {
                active["name"].as_str() == Some(name)
                    && length.is_some_and(|length| frame.round() + 1.0 < length)
            })
            .unwrap_or(0.0);
        let response =
            self.core_request(&json!({"op":"animation.seek", "name":name, "frame":frame}));
        if response["succeeded"].as_bool() == Some(true) {
            self.playing = Some(PlaybackTarget::Inx(name.to_owned()));
        }
        response
    }

    /// True once after a tick that stepped INX physics, so the same frame does not step it a second time.
    pub fn take_physics_stepped(&mut self) -> bool {
        std::mem::take(&mut self.physics_stepped_in_tick)
    }

    pub fn tick(&mut self, dt: f64, looping: bool) -> Option<Value> {
        self.physics_stepped_in_tick = false;
        let (op, valid) = match self.playing.as_ref()? {
            PlaybackTarget::Inx(name) => (
                "animation.advance",
                self.snapshot["documentFormat"] == "inx"
                    && self.snapshot["animation"]["name"].as_str() == Some(name),
            ),
            PlaybackTarget::NativeScene(guid) => (
                "live2d.animation.advance",
                self.snapshot["nativePlayback"]["active"]["sceneGuid"].as_str() == Some(guid),
            ),
            PlaybackTarget::Motion3(name) => (
                "motion3.advance",
                matches!(self.snapshot["documentFormat"].as_str(), Some("inx" | "live2d.cmo3"))
                    && self.snapshot["motion3"]["name"].as_str() == Some(name),
            ),
        };
        if !valid {
            self.pause();
            return None;
        }
        let motion3 = matches!(self.playing, Some(PlaybackTarget::Motion3(_)));
        let dt = if motion3 && dt.is_finite() { dt.min(MOTION3_MAX_TICK_DT) } else { dt };
        // `motion3.advance` on INX drops the physics result, so assigned SimplePhysics drivers are stepped by the
        // same dt. A native document steps its attached physics inside `motion3.advance` already.
        let restep = motion3
            && self.playback_physics
            && self.snapshot["documentFormat"] == "inx"
            && has_assigned_physics(&self.snapshot["model"]["nodes"]);
        let mut response = self.session.handle_request(&json!({
            "op":op, "dt":dt, "loop":looping,
        }));
        if restep && response["succeeded"].as_bool() == Some(true) {
            let stepped = self.session.handle_request(&json!({"op":"physics.step", "dt":dt}));
            self.physics_stepped_in_tick = stepped["succeeded"].as_bool() == Some(true);
            if !self.physics_stepped_in_tick {
                response = stepped;
            }
        }
        self.refresh();
        let ended = match self.playing.as_ref() {
            Some(PlaybackTarget::Inx(name)) => {
                self.snapshot["animation"]["frame"].as_f64().unwrap_or(0.0).round() + 1.0
                    >= self.snapshot["model"]["animations"][name]["length"].as_f64().unwrap_or(0.0)
            }
            Some(PlaybackTarget::NativeScene(_)) => {
                let active = &self.snapshot["nativePlayback"]["active"];
                active["clockFrame"].as_f64().unwrap_or(0.0)
                    >= active["durationFrames"].as_f64().unwrap_or(0.0)
            }
            Some(PlaybackTarget::Motion3(_)) => {
                let active = &self.snapshot["motion3"];
                active["time"].as_f64().unwrap_or(0.0) >= active["clipEnd"].as_f64().unwrap_or(0.0)
            }
            None => true,
        };
        if response["succeeded"].as_bool() != Some(true) || (!looping && ended) {
            self.pause();
        }
        self.note(&response);
        Some(response)
    }

    pub fn play_native(&mut self, scene_guid: &str) -> Value {
        let active = &self.snapshot["nativePlayback"]["active"];
        let frame = active["clockFrame"]
            .as_f64()
            .filter(|frame| {
                active["sceneGuid"].as_str() == Some(scene_guid)
                    && *frame < active["durationFrames"].as_f64().unwrap_or(0.0)
            })
            .unwrap_or(0.0);
        let response = self.core_request(&json!({
            "op":"live2d.animation.seek", "sceneGuid":scene_guid, "frame":frame,
        }));
        if response["succeeded"].as_bool() == Some(true) {
            self.playing = Some(PlaybackTarget::NativeScene(scene_guid.to_owned()));
        }
        response
    }

    pub fn play_motion3(&mut self, name: &str) -> Value {
        let active = &self.snapshot["motion3"];
        let time = active["time"]
            .as_f64()
            .filter(|time| {
                active["name"].as_str() == Some(name)
                    && *time < active["clipEnd"].as_f64().unwrap_or(0.0)
            })
            .unwrap_or(0.0);
        let response = self.core_request(&json!({"op":"motion3.seek", "name":name, "time":time}));
        if response["succeeded"].as_bool() == Some(true) {
            self.playing = Some(PlaybackTarget::Motion3(name.to_owned()));
        }
        response
    }

    fn require_selectable(&self, id: u64) -> Result<(), String> {
        if self.snapshot["documentFormat"].as_str() != Some("inx") {
            return Err("node selection requires an INX document".into());
        }
        if find_node(&self.snapshot["model"]["nodes"], id).is_none() {
            return Err(format!("node {id} was not found"));
        }
        Ok(())
    }

    /// Plain click: the selection becomes exactly this node, and it anchors the next range.
    pub fn select_node_target(&mut self, id: u64) -> Result<(), String> {
        self.require_selectable(id)?;
        self.selection.set(vec![id]);
        Ok(())
    }

    /// The whole selection at once, in the order given (MCP `editor_selection_set`); none clears it.
    pub fn select_nodes(&mut self, ids: &[u64]) -> Result<(), String> {
        for id in ids {
            self.require_selectable(*id)?;
        }
        self.selection.set(ids.to_vec());
        Ok(())
    }

    /// Ctrl/Cmd-click: toggles one node in the selection. The node anchors the next range.
    pub fn toggle_node_selection(&mut self, id: u64) -> Result<(), String> {
        self.require_selectable(id)?;
        match self.selection.ids.iter().position(|selected| *selected == id) {
            Some(at) => {
                self.selection.ids.remove(at);
            }
            None => self.selection.ids.push(id),
        }
        self.selection.anchor = Some(id);
        self.selection.range.clear();
        Ok(())
    }

    /// Shift-click over the visible rows in display order (`rows`). The rows between the anchor and
    /// `to` join the selection; the rows the previous Shift-click added are taken back out first.
    /// Without a visible anchor this is a plain click.
    pub fn select_node_range(&mut self, to: u64, rows: &[u64]) -> Result<(), String> {
        self.require_selectable(to)?;
        let anchor = self.selection.anchor.filter(|anchor| rows.contains(anchor));
        let (Some(anchor), Some(end)) = (anchor, rows.iter().position(|row| *row == to)) else {
            return self.select_node_target(to);
        };
        let start = rows.iter().position(|row| *row == anchor).unwrap_or(end);
        let previous = std::mem::take(&mut self.selection.range);
        self.selection.ids.retain(|id| !previous.contains(id));
        for id in &rows[start.min(end)..=start.max(end)] {
            if !self.selection.ids.contains(id) {
                self.selection.ids.push(*id);
                if *id != anchor {
                    self.selection.range.push(*id);
                }
            }
        }
        Ok(())
    }

    /// Ctrl+A: every node under the root (D `incSelectAll`).
    pub fn select_all_nodes(&mut self) -> Result<(), String> {
        let root = self.snapshot["model"]["nodes"]["uuid"].as_u64().unwrap_or(0);
        self.require_selectable(root)?;
        let mut order = Vec::new();
        preorder_ids(&self.snapshot["model"]["nodes"], &mut order);
        order.retain(|id| *id != root);
        self.selection.set(order);
        self.selection.anchor = None;
        Ok(())
    }

    /// Click on empty space (D `incSelectNode(null)`).
    pub fn clear_node_selection(&mut self) {
        self.selection.clear();
    }

    /// The primary node: the first one selected. The inspector, viewport and mesh tools follow it.
    pub fn selected_node(&self) -> Option<u64> {
        self.selection.ids.first().copied()
    }

    /// Every selected node, in selection order.
    pub fn selected_nodes(&self) -> &[u64] {
        &self.selection.ids
    }

    /// Every selected node, in tree order: the order actions apply to a multi-selection.
    pub fn selected_nodes_in_tree_order(&self) -> Vec<u64> {
        let mut order = Vec::new();
        preorder_ids(&self.snapshot["model"]["nodes"], &mut order);
        order.retain(|id| self.selection.ids.contains(id));
        order
    }

    /// Class shared by all `ids`, or `None` when they differ or one is missing (D `ngGetCommonNodeType`).
    pub fn common_node_type(&self, ids: &[u64]) -> Option<String> {
        common_node_type(&self.snapshot["model"], ids)
    }

    /// Parameters with a binding on node `id`, from the current snapshot.
    pub fn parameter_usage(&self, id: u64) -> Vec<ParameterUse> {
        parameter_usage(&self.snapshot["model"], id)
    }

    /// Parent and index a drop of the selection at `site` would use; `Ok(None)` is a no-op drop.
    pub fn resolve_drop(&self, site: DropSite) -> Result<Option<(u64, usize)>, String> {
        resolve_node_drop(&self.snapshot["model"], &self.selected_nodes_in_tree_order(), site)
    }

    pub fn node_action(&mut self, action: NodeAction<'_>) -> Value {
        if self.snapshot["documentFormat"].as_str() != Some("inx") {
            return failure("node authoring requires an INX document");
        }
        if self.draft_dirty() && !matches!(action, NodeAction::Copy) {
            self.status = NODE_ACTION_BLOCKED_MESSAGE.to_owned();
            return failure(NODE_ACTION_BLOCKED_MESSAGE);
        }
        let explicit_visibility = matches!(&action, NodeAction::SetVisibility { nodes, .. } if !nodes.is_empty())
            || matches!(action, NodeAction::SetLock { .. });
        let needs_selection = !matches!(
            action,
            NodeAction::Create { .. } | NodeAction::Add { .. } | NodeAction::Paste
        ) && !explicit_visibility;
        if needs_selection && self.selection.ids.is_empty() {
            self.status = NODE_SELECT_REQUIRED_MESSAGE.to_owned();
            return failure(NODE_SELECT_REQUIRED_MESSAGE);
        }
        let root = self.snapshot["model"]["nodes"]["uuid"].as_u64();
        let target = self.selected_node().or(root);
        let Some(target) = target else {
            return failure("no target node is available");
        };
        // The selection in tree order, or the root when nothing is selected (creation targets).
        let selected = self.selected_nodes_in_tree_order();
        let parents = if selected.is_empty() { vec![target] } else { selected.clone() };
        // D skips the root when a selection holds it next to other nodes.
        let without_root = if selected.len() > 1 {
            selected.iter().copied().filter(|id| Some(*id) != root).collect()
        } else {
            selected.clone()
        };
        let request = match &action {
            NodeAction::Create { class_name, name } => {
                let mut request = json!({
                    "op": "node.create",
                    "className": class_name,
                    "parents": parents,
                });
                if let Some(name) = name.filter(|name| !name.trim().is_empty()) {
                    request["name"] = json!(name);
                }
                request
            }
            NodeAction::Add { class_name, suffix } => {
                let mut request = json!({
                    "op": "node.create",
                    "className": class_name,
                    "parents": parents,
                });
                if let Some(suffix) = suffix.filter(|value| !value.is_empty()) {
                    request["_suffix"] = json!(suffix);
                }
                request
            }
            NodeAction::Copy => json!({"op": "node.copy", "nodes": selected}),
            NodeAction::Cut => json!({"op": "node.cut", "nodes": without_root}),
            NodeAction::Paste => json!({"op": "node.paste", "parent": target}),
            NodeAction::Delete => json!({"op": "node.delete", "nodes": without_root}),
            NodeAction::Reparent { parent } => {
                json!({"op": "node.move", "nodes": without_root, "parent": parent})
            }
            NodeAction::Insert { class_name, suffix } => {
                let mut request = json!({
                    "op": "node.insert",
                    "className": class_name,
                    "nodes": without_root,
                });
                if let Some(suffix) = suffix.filter(|value| !value.is_empty()) {
                    request["_suffix"] = json!(suffix);
                }
                request
            }
            NodeAction::Convert { class_name } => {
                if self.common_node_type(&without_root).is_none() {
                    self.status = NODE_CONVERT_MIXED_MESSAGE.to_owned();
                    return failure(NODE_CONVERT_MIXED_MESSAGE);
                }
                json!({"op": "node.convert", "className": class_name, "nodes": without_root})
            }
            NodeAction::ToggleVisibility if selected.len() <= 1 => {
                json!({"op": "node.enabled.toggle", "id": target})
            }
            NodeAction::ToggleVisibility => {
                json!({"op": "node.visibility.set", "nodes": selected})
            }
            NodeAction::SetLock { nodes, locked, recursive } => {
                json!({"op": "node.lock.set", "nodes": nodes, "locked": locked, "recursive": recursive})
            }
            NodeAction::SetVisibility { nodes, enabled, recursive } => {
                let nodes: Vec<u64> = if nodes.is_empty() { selected.clone() } else { nodes.to_vec() };
                let mut request = json!({"op": "node.visibility.set", "nodes": nodes});
                if let Some(enabled) = enabled {
                    request["enabled"] = json!(enabled);
                }
                if *recursive {
                    request["recursive"] = json!(true);
                }
                request
            }
            NodeAction::Drop { site } => {
                match resolve_node_drop(&self.snapshot["model"], &selected, *site) {
                    Err(message) => {
                        self.status = message.clone();
                        return failure(&message);
                    }
                    Ok(None) => {
                        return json!({"succeeded": true, "status": "ok", "result": {"moved": []}});
                    }
                    Ok(Some((parent, index))) => {
                        json!({"op": "node.move", "nodes": selected, "parent": parent, "index": index})
                    }
                }
            }
        };
        let response = if request["op"] == "node.visibility.set" {
            self.visibility_request(&request)
        } else {
            self.core_request(&request)
        };
        if response["succeeded"].as_bool() == Some(true) {
            let uuids = |list: &Value| -> Vec<u64> {
                list.as_array().into_iter().flatten().filter_map(|node| node["uuid"].as_u64()).collect()
            };
            match action {
                NodeAction::Create { .. }
                | NodeAction::Add { .. }
                | NodeAction::Paste
                | NodeAction::Insert { .. } => {
                    self.selection.set(uuids(&response["result"]["created"]));
                }
                NodeAction::Convert { .. } => {
                    self.selection.set(uuids(&response["result"]["converted"]));
                }
                NodeAction::Cut | NodeAction::Delete => self.selection.clear(),
                NodeAction::Copy
                | NodeAction::Reparent { .. }
                | NodeAction::ToggleVisibility
                | NodeAction::SetVisibility { .. }
                | NodeAction::SetLock { .. }
                | NodeAction::Drop { .. } => {}
            }
        }
        response
    }

    fn visibility_request(&mut self, request: &Value) -> Value {
        let response = self.session.handle_node_visibility(request);
        self.reconcile_playback(request, response["succeeded"].as_bool() == Some(true));
        self.refresh();
        self.note(&response);
        response
    }

    pub fn select_node(&mut self, id: u64) -> Result<(), String> {
        if self.draft_dirty() {
            return Err(RESELECT_BLOCKED_MESSAGE.to_owned());
        }
        if self.snapshot["documentFormat"].as_str() != Some("inx") {
            return Err("node rename drafts require an INX document".into());
        }
        let name = find_node(&self.snapshot["model"]["nodes"], id)
            .and_then(|node| node["name"].as_str())
            .ok_or_else(|| format!("node {id} was not found"))?
            .to_owned();
        self.draft = Some(EditDraft::NodeName {
            id,
            revision: self.revision(),
            original: name.clone(),
            value: name,
        });
        Ok(())
    }

    pub fn select_can3_anchor(
        &mut self,
        scene: &str,
        track: &str,
        curve: &str,
        frame: i64,
        part_opacity_map_key: Option<&str>,
    ) -> Result<(), String> {
        if self.draft_dirty() {
            return Err(RESELECT_BLOCKED_MESSAGE.to_owned());
        }
        if self.snapshot["documentFormat"].as_str() != Some("live2d.can3") {
            return Err("anchor drafts require a CAN3 document".into());
        }
        let scenes = self.snapshot["model"]["scenes"]
            .as_array()
            .ok_or("CAN3 model lacks scenes")?;
        let scene_value = scenes
            .iter()
            .find(|s| s["guid"].as_str() == Some(scene))
            .ok_or_else(|| format!("animation scene {scene} was not found"))?;
        let track_value = scene_value["tracks"]
            .as_array()
            .and_then(|tracks| tracks.iter().find(|t| t["guid"].as_str() == Some(track)))
            .ok_or_else(|| format!("animation track {track} was not found"))?;
        let (field, index) = crate::animation_source::editable_curve_slot(track_value, curve, part_opacity_map_key)?;
        let curve_value = &track_value[field][index];
        let original = curve_value["points"]
            .as_array()
            .and_then(|points| {
                points
                    .iter()
                    .find(|p| p["anchor"]["frame"].as_i64() == Some(frame))
            })
            .and_then(|point| point["anchor"]["value"].as_f64())
            .ok_or_else(|| format!("animation anchor frame {frame} was not found"))?;
        self.draft = Some(EditDraft::Can3Anchor {
            scene_guid: scene.to_owned(),
            track_guid: track.to_owned(),
            curve_id: curve.to_owned(),
            part_opacity_map_key: part_opacity_map_key.map(str::to_owned),
            frame,
            revision: self.revision(),
            original,
            value: original,
        });
        Ok(())
    }

    pub fn set_draft_name(&mut self, value: String) {
        if let Some(EditDraft::NodeName { value: current, .. }) = &mut self.draft {
            *current = value;
        }
    }

    pub fn set_draft_value(&mut self, value: f64) {
        if let Some(EditDraft::Can3Anchor { value: current, .. }) = &mut self.draft {
            *current = value;
        }
    }

    pub fn draft_view(&self) -> Option<DraftView<'_>> {
        match &self.draft {
            Some(EditDraft::NodeName {
                id,
                original,
                value,
                ..
            }) => Some(DraftView::NodeName {
                id: *id,
                original,
                value,
            }),
            Some(EditDraft::Can3Anchor {
                scene_guid,
                track_guid,
                curve_id,
                part_opacity_map_key,
                frame,
                original,
                value,
                ..
            }) => Some(DraftView::Can3Anchor {
                scene_guid,
                track_guid,
                curve_id,
                part_opacity_map_key: part_opacity_map_key.as_deref(),
                frame: *frame,
                original: *original,
                value: *value,
            }),
            None => None,
        }
    }

    pub fn draft_dirty(&self) -> bool {
        match &self.draft {
            Some(EditDraft::NodeName {
                original, value, ..
            }) => original != value,
            Some(EditDraft::Can3Anchor {
                original, value, ..
            }) => original != value,
            None => false,
        }
    }

    pub fn draft_stale(&self) -> bool {
        match &self.draft {
            Some(draft) => draft.revision() != self.revision(),
            None => false,
        }
    }

    pub fn discard_draft(&mut self) {
        self.draft = None;
    }

    pub fn apply_draft(&mut self) -> Value {
        let Some(draft) = &self.draft else {
            return failure("no draft is selected");
        };
        if draft.revision() != self.revision() {
            self.status = "draft is stale; discard it and reselect".into();
            return failure("draft is stale; discard it and reselect");
        }
        let request = match draft {
            EditDraft::NodeName { id, value, .. } => {
                json!({"op":"node.rename", "id":id, "name":value})
            }
            EditDraft::Can3Anchor {
                scene_guid,
                track_guid,
                curve_id,
                part_opacity_map_key,
                frame,
                revision,
                value,
                ..
            } => {
                if !value.is_finite() {
                    self.status = "animation anchor value must be finite".into();
                    return failure("animation anchor value must be finite");
                }
                let mut request = json!({"op":"live2d.animation.key.set", "sceneGuid":scene_guid,
                    "trackGuid":track_guid, "curveId":curve_id, "anchorFrame":frame,
                    "value":value, "expectedRevision":revision});
                if let Some(key) = part_opacity_map_key {
                    request["partOpacityMapKey"] = json!(key);
                }
                request
            }
        };
        let response = self.session.handle_request(&request);
        if response["succeeded"].as_bool() == Some(true) {
            self.draft = None;
            self.refresh();
            self.pause();
            self.status = "applied".into();
        } else {
            self.status = response["message"]
                .as_str()
                .unwrap_or("apply failed")
                .to_owned();
        }
        response
    }

    pub fn core_request(&mut self, request: &Value) -> Value {
        let discard = replaces_document(request) && std::mem::take(&mut self.discard_unsaved);
        if let Err(message) = file_op_gate(self.draft_dirty(), self.modified() && !discard, request) {
            self.status = message.clone();
            return failure(&message);
        }
        let replaced = replaces_document(request);
        let response = self.session.handle_request(request);
        self.reconcile_playback(request, response["succeeded"].as_bool() == Some(true));
        if replaced && response["succeeded"].as_bool() == Some(true) {
            // The old target is gone; only clean drafts can reach this point.
            self.draft = None;
            self.selection.clear();
        }
        self.refresh();
        self.note(&response);
        response
    }

    pub fn mcp_request(&mut self, request: &Value) -> Option<Value> {
        self.mcp_request_guarded(request, &mut |_| Ok(()))
    }

    fn mcp_request_guarded(
        &mut self,
        request: &Value,
        guard: &mut dyn FnMut(&Value) -> Result<(), String>,
    ) -> Option<Value> {
        self.mcp_request_guarded_with_prepared(request, false, &mut |call, _| guard(call))
    }

    fn mcp_request_guarded_with_prepared(
        &mut self, request: &Value, prepare_exports: bool,
        guard: &mut dyn FnMut(&Value, Result<Option<crate::export::ExportConsent<'_>>, String>) -> Result<(), String>,
    ) -> Option<Value> {
        let dirty = self.draft_dirty() || self.panel_drafts;
        let modified = self.modified();
        let before_revision = self.revision();
        let mut replaced = false;
        let mut command = None;
        let refusal = self.replacement_refusal.clone();
        let response = crate::mcp::handle_mcp(&mut self.session, request, |session, core| {
            let prepared = if prepare_exports && (core["op"] == "export.gltf" || request.get("id").is_some_and(|id| !id.is_null())) {
                match core["op"].as_str() {
                    Some("export.video.start") => session.prepare_video_export(core).and_then(|mut video| {
                        video.resolve_program()?; Ok(Some(crate::export_ops::PreparedExport::Video(video)))
                    }),
                    Some("export.collect") => session.prepare_collection(core).map(|plan| Some(crate::export_ops::PreparedExport::Collection(plan))),
                    Some("export.gltf") => session.prepare_gltf_export(core).map(|plan| Some(crate::export_ops::PreparedExport::Gltf(plan))),
                    Some("depth.draw.export_png") => session.prepare_depth_png_export(core).map(|plan| Some(crate::export_ops::PreparedExport::DepthPng(plan))),
                    Some("automesh.preset.save" | "automesh.preset.delete" | "physics.preset.save" | "physics.preset.delete"
                        | "live2d.physics3.preset.save" | "live2d.physics3.preset.delete"
                        | "animation.template.save" | "animation.template.delete") => session.prepare_store_write(core).map(|plan| Some(crate::export_ops::PreparedExport::Store(plan))),
                    _ => Ok(None),
                }
            } else { Ok(None) };
            guard(core, prepared.as_ref().map(|plan| plan.as_ref().map(|plan| plan.consent())).map_err(Clone::clone))?;
            let prepared = prepared?;
            if replaces_document(core) {
                if let Some(refusal) = &refusal {
                    return Err(refusal.clone());
                }
                replaced = true;
            }
            file_op_gate(dirty, modified, core)?;
            command = Some(core.clone());
            Ok(prepared)
        });
        if let Some(command) = command {
            // Notifications have no result; state changes still invalidate play intent.
            let succeeded = response
                .as_ref()
                .is_some_and(|response| response["result"]["isError"].as_bool() == Some(false));
            self.reconcile_playback(&command, succeeded || response.is_none());
        }
        self.refresh();
        // Notifications answer None, so success is proven by the revision
        // advancing; a failed replacement leaves it unchanged and keeps the draft.
        if replaced && self.revision() != before_revision {
            self.draft = None;
            self.selection.clear();
            self.document_replaced = true;
        }
        if let Some(response) = &response {
            if let Some(result) = response.get("result") {
                if result["isError"].as_bool() == Some(true) {
                    self.status = result["content"][0]["text"]
                        .as_str()
                        .unwrap_or("tool failed")
                        .to_owned();
                }
            }
        }
        response
    }

    /// `mcp_request` for a host with a window. `window` answers the `editor.*` requests (the editor state
    /// resource and the editor tools) with their result: the state, read through `editor_state`, for those
    /// that answer with it. `mcp_request` alone answers them as a host without a window does.
    /// `guard` may refuse a normalized operation before dispatch, after envelope and argument validation.
    pub fn mcp_request_with(
        &mut self,
        request: &Value,
        guard: &mut dyn FnMut(&Value) -> Result<(), String>,
        window: &mut dyn FnMut(&mut Self, &Value) -> Result<Value, String>,
    ) -> Option<Value> {
        self.mcp_request_with_prepared(request, false, &mut |call, _| guard(call), window)
    }

    pub fn mcp_request_with_prepared(
        &mut self, request: &Value, prepare_exports: bool,
        guard: &mut dyn FnMut(&Value, Result<Option<crate::export::ExportConsent<'_>>, String>) -> Result<(), String>,
        window: &mut dyn FnMut(&mut Self, &Value) -> Result<Value, String>,
    ) -> Option<Value> {
        let Some(call) = crate::mcp::editor_request(request) else {
            return self.mcp_request_guarded_with_prepared(request, prepare_exports, guard);
        };
        let outcome = call.and_then(|call| guard(&call, Ok(None)).and_then(|()| window(self, &call)));
        if let Err(message) = &outcome {
            self.status = message.clone();
        }
        crate::mcp::editor_reply(request, outcome)
    }

    /// The editor state resource: the session's document facts plus `window`, what only the window holds.
    pub fn editor_state(&self, window: Value) -> Value {
        crate::mcp::editor_state(&self.session, Some(window))
    }

    /// A document replacement an editor tool asks for, under the gate `mcp_request` puts on `open`: an open
    /// draft or unsaved changes refuse it. The tool runs the window's post-replacement step itself, at the
    /// point its own order needs, so this does not raise `take_document_replaced`.
    pub fn replace_document(&mut self, request: &Value) -> Result<(), String> {
        file_op_gate(self.draft_dirty() || self.panel_drafts, self.modified(), request)?;
        let response = self.core_request(request);
        if response["succeeded"].as_bool() != Some(true) {
            return Err(response["message"].as_str().unwrap_or("request failed").to_owned());
        }
        Ok(())
    }

    /// Whether an MCP request to the core (`new`, `open`, an import) replaced the document since the last call.
    pub fn take_document_replaced(&mut self) -> bool {
        std::mem::take(&mut self.document_replaced)
    }

    /// Path the open document saves to; `None` for a new document.
    pub fn project_path(&self) -> Option<&std::path::Path> {
        self.session.file_path.as_deref()
    }

    /// Reads shared video host state without rebuilding the document snapshot.
    pub fn video_reads(&mut self) -> (Value, Value) {
        (self.session.op_export_video_probe_status(), self.session.video_command_preview().clone())
    }

    /// Runs a backend that drives the session directly (export, autosave) and refreshes the snapshot.
    /// Callers that move the animation preview should `pause` first.
    pub fn with_session<T>(&mut self, run: impl FnOnce(&mut Session) -> T) -> T {
        let output = run(&mut self.session);
        self.refresh();
        output
    }

    pub fn can_close(&self) -> bool {
        !self.draft_dirty() && !self.panel_drafts && self.snapshot["modified"].as_bool() != Some(true)
    }

    pub fn has_background_work(&self) -> bool {
        self.session.has_background_work()
    }

    fn revision(&self) -> u64 {
        self.snapshot["revision"].as_u64().unwrap_or(0)
    }

    fn modified(&self) -> bool {
        self.snapshot["modified"].as_bool() == Some(true)
    }

    fn refresh(&mut self) {
        self.snapshot = self.session.snapshot();
        if self.snapshot["documentFormat"].as_str() != Some("inx") {
            self.selection.clear();
        } else {
            let model = &self.snapshot["model"]["nodes"];
            self.selection.ids.retain(|id| find_node(model, *id).is_some());
            self.selection.range.retain(|id| find_node(model, *id).is_some());
            if self.selection.anchor.is_some_and(|id| find_node(model, id).is_none()) {
                self.selection.anchor = None;
            }
        }
    }

    fn reconcile_playback(&mut self, request: &Value, succeeded: bool) {
        if !self.playing() {
            return;
        }
        let current = self.session.snapshot();
        let changed = [
            "revision",
            "animation",
            "motion3",
            "nativePlayback",
            "values",
            "physics",
            "nativePose",
            "nativePhysics",
            "previewDeformation",
        ]
        .iter()
        .any(|key| current[key] != self.snapshot[key]);
        let runtime_preview = matches!(
            request["op"].as_str(),
            Some(
                "animation.seek"
                    | "animation.advance"
                    | "animation.reset"
                    | "value.set"
                    | "motion3.seek"
                    | "motion3.advance"
                    | "motion3.reset"
                    | "live2d.animation.seek"
                    | "live2d.animation.advance"
                    | "live2d.animation.reset"
                    | "live2d.parameter.set"
                    | "live2d.expression.apply"
                    | "live2d.expression.reset"
                    | "live2d.pose.attach" | "live2d.pose.reset" | "live2d.pose.step"
                    | "live2d.physics.attach" | "live2d.physics.reset" | "live2d.physics.step"
                    | "live2d.physics.options.set" | "live2d.physics.stabilize"
                    | "physics.step"
                    | "physics.reset"
                    | "rig.preview_deform"
            )
        );
        if changed || (succeeded && runtime_preview) {
            self.pause();
        }
    }

    fn note(&mut self, response: &Value) {
        self.status = if response["succeeded"].as_bool() == Some(true) {
            "ok".into()
        } else {
            response["message"]
                .as_str()
                .unwrap_or("operation failed")
                .to_owned()
        };
    }
}

fn preorder_ids(node: &Value, out: &mut Vec<u64>) {
    if let Some(id) = node["uuid"].as_u64() {
        out.push(id);
    }
    for child in node["children"].as_array().into_iter().flatten() {
        preorder_ids(child, out);
    }
}

/// Class shared by all `ids` of `model`, or `None` when they differ, one is missing or `ids` is empty.
pub fn common_node_type(model: &Value, ids: &[u64]) -> Option<String> {
    let mut common: Option<&str> = None;
    for id in ids {
        let kind = find_node(&model["nodes"], *id)?["type"].as_str().unwrap_or("Node");
        match common {
            None => common = Some(kind),
            Some(seen) if seen == kind => {}
            Some(_) => return None,
        }
    }
    common.map(str::to_owned)
}

/// Parameters of `model` with a binding on node `id`, in model order.
pub fn parameter_usage(model: &Value, id: u64) -> Vec<ParameterUse> {
    u32::try_from(id)
        .map(|id| crate::nodes::parameter_usage(model, id))
        .unwrap_or_default()
}

/// Parent and child index for dropping `moving` at `site` (see [`DropSite`]); `Ok(None)` when the
/// drop would leave the tree as it is, `Err` with the reason when it is not allowed.
pub fn resolve_node_drop(model: &Value, moving: &[u64], site: DropSite) -> Result<Option<(u64, usize)>, String> {
    let moving = moving
        .iter()
        .map(|id| u32::try_from(*id).map_err(|_| format!("node {id} was not found")))
        .collect::<Result<Vec<_>, _>>()?;
    crate::nodes::resolve_drop(model, &moving, site)
        .map(|drop| drop.map(|(parent, index)| (u64::from(parent), index)))
}

fn failure(message: &str) -> Value {
    json!({
        "succeeded": false,
        "status": "error",
        "message": message,
        "result": null,
    })
}

fn find_node(value: &Value, id: u64) -> Option<&Value> {
    if value.get("uuid").and_then(Value::as_u64) == Some(id) {
        return Some(value);
    }
    value
        .get("children")?
        .as_array()?
        .iter()
        .find_map(|child| find_node(child, id))
}
