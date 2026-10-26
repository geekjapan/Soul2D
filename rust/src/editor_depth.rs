//! Depth mode of the egui viewport: the D depth editing viewport (`viewport/depth`).
//!
//! The four D depth tools work on one GridDeformer of the selected node:
//!
//! * **Direct Depth** selects vertices (click, Shift toggles, Ctrl removes, a drag on empty space draws a
//!   rectangle) and a drag on a selected vertex raises or lowers every selected vertex together.
//! * **Ring** drags out a ring operation between two vertices (a click makes one of length `2 * radius_x`
//!   along the angle); its Edit mode (Tab) grabs an existing ring by an endpoint or its body.
//! * **Attached Point** drags a vertex up or down; the distance is the operation's amount.
//! * **Plane** picks a centre vertex and drags out the radii of a flattening ellipse.
//!
//! Edits stay local until Apply, as in D (`DepthMeshEditor.applyToTargets`): the pending operation list and
//! the working depths are shown over the mesh as a colour ramp, and Apply sends them to the session.
//! Cancel drops them. The depths of an operation list are previewed by running the very session ops on a
//! scratch copy of the document (`depth.ops.set` then `depth.ops.apply`), so the preview and the committed
//! depths cannot disagree. A direct depth drag is previewed by [`depth_draw::direct_depth_drag`].
//!
//! The viewport shows the model from above: D's right-drag orbit of the depth camera and the projection of
//! depth into the view are not offered (`depth_draw::DepthCamera` and the projection functions are ported
//! and tested; only the UI is missing). Gestures work on pointer positions in world space, mapped into the
//! grid's own coordinates through the inverse of the node's world matrix, like the mesh tools.

use soul2d_core::depth_draw as dd;
use crate::editor_depth_draw::DrawPanel;
use crate::editor_mesh_tools::{Frame, Mods, Picture, V2};
use eframe::egui::{self, Color32, Pos2, Stroke};
use serde_json::{json, Value};
use soul2d_core::Session;

type P = [f32; 2];

/// Pixels the pointer may travel before a press on a selected vertex becomes a depth drag.
const DRAG_PX: f64 = 4.0;
/// Pick radius of D (`14 / viewport zoom`), in screen points.
const HIT_PX: f64 = 14.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    DirectDepth,
    Ring,
    AttachedPoint,
    Plane,
}

impl Tool {
    pub const ALL: [Tool; 4] = [Tool::DirectDepth, Tool::Ring, Tool::AttachedPoint, Tool::Plane];

    pub fn label(self) -> &'static str {
        match self {
            Tool::DirectDepth => "Direct Depth",
            Tool::Ring => "Ring",
            Tool::AttachedPoint => "Attached Point",
            Tool::Plane => "Plane",
        }
    }

    fn hover(self) -> &'static str {
        match self {
            Tool::DirectDepth => "Adjust Vertex Depth: select vertices, then drag up or down to change their depth",
            Tool::Ring => "Edit Depth Ring: drag between two vertices; Tab switches between Create and Edit",
            Tool::AttachedPoint => "Edit Attached Depth Point: drag a vertex up or down",
            Tool::Plane => "Flatten Depth Plane: pick a centre vertex and drag out the radii",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingMode {
    Create,
    Edit,
}

/// What the viewport reports to the tools. `at` is a WORLD position, `sy` the screen y in points (the
/// amount of an attached point and the direct depth follow the vertical pointer travel on screen).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ev {
    Press { at: V2, sy: f32, mods: Mods },
    Move { at: V2, sy: f32, mods: Mods },
    Release { at: V2, sy: f32, mods: Mods },
    Key(Key),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Tab,
    Delete,
    Escape,
}

/// Facts from the editor for one frame.
pub struct Ctx<'a> {
    /// The document model (`snapshot["model"]`).
    pub model: &'a Value,
    pub selected: Option<u64>,
    /// A parameter is armed: the deform tools own the viewport.
    pub armed: bool,
    /// Selected node's local-to-world map; `None` while the scene has not been evaluated.
    pub frame: Option<Frame>,
    pub zoom: f64,
    /// `(snapshot depthDraw.revision, snapshot revision)`: the Depth Draw section reads the session again when it changes.
    pub draw_key: (u64, u64),
}

#[derive(Clone, Debug)]
enum Gesture {
    Idle,
    /// Rectangle selection (`add`: Shift, `remove`: Ctrl).
    Box { origin: P, now: P, add: bool, remove: bool },
    /// Pressed on a selected vertex: a drag past [`DRAG_PX`] starts the depth drag, a release selects it alone.
    Pending { vertex: usize, press_world: V2, origin_sy: f32, maybe_alone: Option<usize> },
    Drag { start: Vec<f32>, dragged: Vec<usize>, origin_sy: f32 },
    RingCreate { start: P, now: P },
    RingEdit { index: usize, handle: dd::RingHandle, start_p0: P, start_p1: P, start_local: P },
    Attached { op: usize, origin_sy: f32 },
    Plane { center: P, rx: f32, ry: f32 },
}

#[derive(Clone, Debug)]
struct Snapshot {
    ops: Vec<Value>,
    working: Vec<f32>,
    dirty: bool,
    selected_op: Option<usize>,
}

/// The grid being edited, as read from the document.
#[derive(Clone, Debug)]
struct Active {
    id: u32,
    frame: Frame,
    zoom: f64,
    points: Vec<P>,
    columns: usize,
    stored_ops: Vec<Value>,
    stored_depths: Vec<f32>,
    stored_base: Option<Vec<f32>>,
    /// `working` before any operation: the saved base when operations exist, else the saved depths.
    base: Vec<f32>,
    /// The document pruned to the grid (with `base` as its depths) for the preview session.
    scratch: Value,
}

pub struct DepthTools {
    pub brush: dd::BrushSettings,
    pub tool: Tool,
    pub ring_mode: RingMode,
    candidate: Option<u32>,
    entered_for: Option<u32>,
    active: Option<Active>,
    ops: Vec<Value>,
    working: Vec<f32>,
    dirty: bool,
    selection: Vec<usize>,
    selected_op: Option<usize>,
    gesture: Gesture,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    cursor: Option<P>,
    down: bool,
    /// The "Depth Draw" section starts open (it starts closed in the editor).
    pub depth_draw_open: bool,
    /// The "Depth Draw" button of the strip asked for the section to open.
    show_draw_section: bool,
    /// The layer-composition panel (`depth.draw.*` requests).
    pub draw: DrawPanel,
    pub notice: String,
}

impl Default for DepthTools {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------------- document reading

fn find_node(node: &Value, id: u64) -> Option<&Value> {
    if node["uuid"].as_u64() == Some(id) {
        return Some(node);
    }
    node["children"].as_array()?.iter().find_map(|child| find_node(child, id))
}

fn numbers(value: &Value) -> Option<Vec<f32>> {
    value.as_array()?.iter().map(|v| v.as_f64().filter(|f| f.is_finite()).map(|f| f as f32)).collect()
}

/// The vertices of a GridDeformer row by row, and the number of columns; `None` for any other node.
fn read_grid(node: &Value) -> Option<(Vec<P>, usize)> {
    if node["type"].as_str()? != "GridDeformer" {
        return None;
    }
    let xs = numbers(node.get("grid_axis_x")?)?;
    let ys = numbers(node.get("grid_axis_y")?)?;
    if xs.len() < 2 || ys.len() < 2 {
        return None;
    }
    Some((ys.iter().flat_map(|&y| xs.iter().map(move |&x| [x, y])).collect(), xs.len()))
}

/// The document pruned to the grid, with `base` as the grid's depths and no saved operations: what the
/// preview session starts from, so `depth.ops.set` snapshots exactly D's base depths.
fn scratch_model(model: &Value, id: u32, base: &[f32]) -> Option<Value> {
    let mut grid = find_node(&model["nodes"], u64::from(id))?.clone();
    let object = grid.as_object_mut()?;
    object.insert("children".into(), json!([]));
    object.remove("depth-ops");
    object.remove("depth-op-base-depths");
    object.insert("depths".into(), json!(base.iter().map(|d| f64::from(*d)).collect::<Vec<_>>()));
    let mut pruned = model.clone();
    let mut root = model["nodes"].clone();
    root.as_object_mut()?.insert("children".into(), json!([grid]));
    pruned["nodes"] = root;
    pruned["param"] = json!([]);
    pruned["animations"] = json!({});
    let mut probe = Session::new();
    if probe.handle_request(&json!({"op": "new", "model": pruned}))["succeeded"] == true {
        return Some(pruned);
    }
    // The pruned copy is not a valid document: keep the whole document, only the grid reset.
    full_scratch(model, id, base)
}

/// The whole document with the grid's depths reset to `base` and its saved operations dropped.
fn full_scratch(model: &Value, id: u32, base: &[f32]) -> Option<Value> {
    let mut grid = find_node(&model["nodes"], u64::from(id))?.clone();
    let object = grid.as_object_mut()?;
    object.remove("depth-ops");
    object.remove("depth-op-base-depths");
    object.insert("depths".into(), json!(base.iter().map(|d| f64::from(*d)).collect::<Vec<_>>()));
    let mut full = model.clone();
    set_node(&mut full["nodes"], u64::from(id), &grid).then_some(full)
}

fn set_node(node: &mut Value, id: u64, replacement: &Value) -> bool {
    if node["uuid"].as_u64() == Some(id) {
        *node = replacement.clone();
        return true;
    }
    match node["children"].as_array_mut() {
        Some(children) => children.iter_mut().any(|child| set_node(child, id, replacement)),
        None => false,
    }
}

/// The depths the saved operations `ops` produce from the scratch document's base (D `recompute`).
// ponytail: a new Session (and a validated document) per recompute; fine for one grid per gesture, slow for
// very large documents. Upgrade: keep one scratch Session in `Active` and only replay set + apply.
fn preview_depths(scratch: &Value, id: u32, ops: &[Value]) -> Result<Vec<f32>, String> {
    let mut session = Session::new();
    let steps = [
        json!({"op": "new", "model": scratch}),
        json!({"op": "depth.ops.set", "target": id, "operations": ops}),
        json!({"op": "depth.ops.apply", "target": id}),
    ];
    let mut reply = Value::Null;
    for step in &steps {
        reply = session.handle_request(step);
        if reply["succeeded"] != true {
            return Err(reply["message"].as_str().unwrap_or("depth preview failed").to_owned());
        }
    }
    numbers(&reply["result"]["depths"]).ok_or_else(|| "depth preview returned no depths".to_owned())
}

fn pair(value: &Value) -> Option<P> {
    let list = value.as_array().filter(|l| l.len() == 2)?;
    Some([list[0].as_f64()? as f32, list[1].as_f64()? as f32])
}

fn op_f32(op: &Value, key: &str, fallback: f32) -> f32 {
    op.get(key).and_then(Value::as_f64).map_or(fallback, |v| v as f32)
}

fn set_pair(op: &mut Value, key: &str, p: P) {
    op[key] = json!([f64::from(p[0]), f64::from(p[1])]);
}

fn dist(a: P, b: P) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

impl DepthTools {
    pub fn new() -> Self {
        Self {
            brush: dd::BrushSettings::default(),
            tool: Tool::DirectDepth,
            ring_mode: RingMode::Create,
            candidate: None,
            entered_for: None,
            active: None,
            ops: Vec::new(),
            working: Vec::new(),
            dirty: false,
            selection: Vec::new(),
            selected_op: None,
            gesture: Gesture::Idle,
            undo: Vec::new(),
            redo: Vec::new(),
            cursor: None,
            down: false,
            depth_draw_open: false,
            show_draw_section: false,
            draw: DrawPanel::default(),
            notice: String::new(),
        }
    }

    /// The depth tools take the pointer (the viewport must not select Parts or drag the transform handle).
    pub fn captures(&self) -> bool {
        self.active.is_some()
    }

    pub fn editing(&self) -> bool {
        self.active.is_some() || self.entered_for.is_some()
    }

    /// Start depth mode for the selected node (what the "Edit Depth" button does).
    pub fn enter(&mut self, id: u32) {
        self.entered_for = Some(id);
        self.notice.clear();
    }

    #[cfg(test)]
    pub fn candidate(&self) -> Option<u32> {
        self.candidate
    }

    #[cfg(test)]
    pub fn pending_ops(&self) -> &[Value] {
        &self.ops
    }

    #[cfg(test)]
    pub fn working_depths(&self) -> &[f32] {
        &self.working
    }

    #[cfg(test)]
    pub fn selection(&self) -> &[usize] {
        &self.selection
    }

    #[cfg(test)]
    pub fn direct_edits(&self) -> bool {
        self.dirty
    }

    fn reset_edit_state(&mut self) {
        self.ops.clear();
        self.working.clear();
        self.dirty = false;
        self.selection.clear();
        self.selected_op = None;
        self.gesture = Gesture::Idle;
        self.undo.clear();
        self.redo.clear();
        self.cursor = None;
        self.down = false;
    }

    fn leave(&mut self) {
        self.entered_for = None;
        self.active = None;
        self.reset_edit_state();
    }

    /// Reads the document for this frame. Changing the node, or the document changing under the tools (undo,
    /// an edit elsewhere), reloads the saved operations and depths and drops the pending edits.
    pub fn sync(&mut self, ctx: &Ctx) {
        self.draw.observe(ctx.draw_key);
        let node = ctx.selected.and_then(|id| find_node(&ctx.model["nodes"], id));
        self.candidate = node
            .filter(|n| !ctx.armed && read_grid(n).is_some())
            .and_then(|n| n["uuid"].as_u64())
            .and_then(|id| u32::try_from(id).ok());
        if ctx.armed || self.entered_for != self.candidate {
            if self.active.is_some() || self.entered_for.is_some() {
                self.leave();
            }
            return;
        }
        let (Some(id), Some(node), Some(frame)) = (self.entered_for, node, ctx.frame) else {
            if self.active.take().is_some() {
                self.reset_edit_state();
            }
            return;
        };
        let Some((points, columns)) = read_grid(node) else { return };
        let count = points.len();
        let mut stored_depths = node.get("depths").and_then(numbers).unwrap_or_default();
        stored_depths.resize(count, 0.0);
        let stored_ops = node.get("depth-ops").and_then(Value::as_array).cloned().unwrap_or_default();
        let stored_base = node.get("depth-op-base-depths").and_then(numbers);
        if let Some(active) = &mut self.active {
            let same = active.id == id
                && active.points == points
                && active.stored_ops == stored_ops
                && active.stored_depths == stored_depths
                && active.stored_base == stored_base;
            if same {
                active.frame = frame;
                active.zoom = ctx.zoom;
                return;
            }
        }
        // D `loadOperationsFromTarget`: a saved operation list brings its own base (zeros for an old file).
        let base = if stored_ops.is_empty() {
            stored_depths.clone()
        } else {
            match &stored_base {
                Some(base) if base.len() == count => base.clone(),
                _ => vec![0.0; count],
            }
        };
        let Some(scratch) = scratch_model(ctx.model, id, &base) else {
            self.notice = "The selected grid cannot be previewed".into();
            self.leave();
            return;
        };
        self.reset_edit_state();
        self.ops = stored_ops.clone();
        self.working = base.clone();
        self.active = Some(Active {
            id,
            frame,
            zoom: ctx.zoom,
            points,
            columns,
            stored_ops,
            stored_depths,
            stored_base,
            base,
            scratch,
        });
        self.recompute();
    }

    // ------------------------------------------------------------------------------ pending edits

    fn checkpoint(&mut self) {
        self.undo.push(Snapshot {
            ops: self.ops.clone(),
            working: self.working.clone(),
            dirty: self.dirty,
            selected_op: self.selected_op,
        });
        self.redo.clear();
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.ops = snapshot.ops;
        self.working = snapshot.working;
        self.dirty = snapshot.dirty;
        self.selected_op = snapshot.selected_op;
        self.gesture = Gesture::Idle;
    }

    fn current_snapshot(&self) -> Snapshot {
        Snapshot { ops: self.ops.clone(), working: self.working.clone(), dirty: self.dirty, selected_op: self.selected_op }
    }

    pub fn can_undo_edit(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo_edit(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_edit(&mut self) {
        if let Some(previous) = self.undo.pop() {
            let current = self.current_snapshot();
            self.redo.push(current);
            self.restore(previous);
        }
    }

    pub fn redo_edit(&mut self) {
        if let Some(next) = self.redo.pop() {
            let current = self.current_snapshot();
            self.undo.push(current);
            self.restore(next);
        }
    }

    /// D `recompute`: working depths are the base with the operation list applied.
    fn recompute(&mut self) {
        let Some(active) = &self.active else { return };
        if self.ops.is_empty() {
            self.working = active.base.clone();
            return;
        }
        match preview_depths(&active.scratch, active.id, &self.ops) {
            Ok(depths) => self.working = depths,
            Err(error) => self.notice = error,
        }
    }

    fn delete_selected_op(&mut self) {
        if let Some(index) = self.selected_op.filter(|i| *i < self.ops.len()) {
            self.checkpoint();
            self.ops.remove(index);
            self.selected_op = self.ops.len().checked_sub(1);
            self.recompute();
        }
    }

    fn duplicate_selected_op(&mut self) {
        if let Some(op) = self.selected_op.and_then(|i| self.ops.get(i)).cloned() {
            self.checkpoint();
            self.ops.push(op);
            self.selected_op = Some(self.ops.len() - 1);
            self.recompute();
        }
    }

    /// Edit one number of the selected operation (the "Selected Deformer" fields of D).
    fn edit_selected_op(&mut self, key: &str, value: f32) {
        if let Some(index) = self.selected_op.filter(|i| *i < self.ops.len()) {
            self.checkpoint();
            self.ops[index][key] = json!(f64::from(value));
            self.recompute();
        }
    }

    /// The request that commits the pending edits (D `applyToTargets`) and leaves depth mode. D wraps the steps
    /// in one action group; `depth.edit.apply` is that group, so Apply is one undo step. Nothing pending sends
    /// nothing.
    pub fn apply(&mut self) -> Vec<Value> {
        let Some(active) = &self.active else { return Vec::new() };
        let id = active.id;
        let plan = dd::apply_plan(!active.stored_ops.is_empty(), self.ops != active.stored_ops, self.dirty);
        let depths = json!(self.working.iter().map(|d| f64::from(*d)).collect::<Vec<_>>());
        let requests = match plan {
            dd::ApplyPlan::Nothing => Vec::new(),
            _ => vec![self.combined_request(id, depths)],
        };
        self.leave();
        requests
    }

    /// The `depth.edit.apply` request; the session chooses the steps with `depth_draw::apply_plan`.
    fn combined_request(&self, id: u32, depths: Value) -> Value {
        json!({"op": "depth.edit.apply", "target": id, "operations": self.ops, "depths": depths, "directEdits": self.dirty})
    }

    /// Cancel: drop every pending edit and leave depth mode.
    pub fn cancel(&mut self) {
        self.leave();
    }

    // ------------------------------------------------------------------------------------ gestures

    fn local(&self, at: V2) -> P {
        let local = self.active.as_ref().map_or(at, |a| a.frame.to_local(at));
        [local[0] as f32, local[1] as f32]
    }

    /// Units of the grid's own space per screen point.
    fn local_per_px(&self) -> f32 {
        self.active.as_ref().map_or(1.0, |a| (1.0 / (a.zoom * a.frame.scale()).max(1e-9)) as f32)
    }

    fn points(&self) -> &[P] {
        self.active.as_ref().map_or(&[], |a| a.points.as_slice())
    }

    fn snap(&self, p: P) -> P {
        if self.brush.snap_to_grid {
            dd::snap_to_vertex(self.points(), p)
        } else {
            p
        }
    }

    /// Turns one pointer event into a change of the pending edits.
    pub fn event(&mut self, event: Ev) {
        if self.active.is_none() {
            return;
        }
        match event {
            Ev::Key(key) => self.key(key),
            Ev::Press { at, sy, mods } => {
                self.down = true;
                self.press(at, sy, mods);
            }
            Ev::Move { at, sy, .. } => {
                self.cursor = Some(self.local(at));
                if self.down {
                    self.drag(at, sy);
                }
            }
            Ev::Release { at, sy, .. } => {
                if self.down {
                    self.drag(at, sy);
                    self.down = false;
                    self.release();
                }
            }
        }
    }

    fn key(&mut self, key: Key) {
        match key {
            Key::Tab if self.tool == Tool::Ring => self.set_ring_mode(match self.ring_mode {
                RingMode::Create => RingMode::Edit,
                RingMode::Edit => RingMode::Create,
            }),
            Key::Tab => {}
            Key::Delete => self.delete_selected_op(),
            Key::Escape => {
                self.gesture = Gesture::Idle;
                self.down = false;
            }
        }
    }

    pub fn set_ring_mode(&mut self, mode: RingMode) {
        if self.ring_mode != mode {
            self.ring_mode = mode;
            self.gesture = Gesture::Idle;
        }
    }

    pub fn set_tool(&mut self, tool: Tool) {
        if self.tool != tool {
            self.tool = tool;
            self.gesture = Gesture::Idle;
            self.down = false;
        }
    }

    fn press(&mut self, at: V2, sy: f32, mods: Mods) {
        let p = self.local(at);
        let radius = HIT_PX as f32 * self.local_per_px();
        match self.tool {
            Tool::DirectDepth => match dd::nearest_vertex(self.points(), p, radius) {
                None => {
                    self.gesture = Gesture::Box { origin: p, now: p, add: mods.shift, remove: mods.ctrl && !mods.shift }
                }
                Some(vertex) => {
                    let maybe = dd::click_selection(&mut self.selection, vertex, mods.ctrl, mods.shift);
                    if self.selection.contains(&vertex) {
                        self.gesture = Gesture::Pending { vertex, press_world: at, origin_sy: sy, maybe_alone: maybe };
                    }
                }
            },
            Tool::Ring if self.ring_mode == RingMode::Create => {
                if let Some(index) = dd::nearest_vertex(self.points(), p, f32::MAX) {
                    let start = if self.brush.snap_to_grid { self.points()[index] } else { p };
                    self.gesture = Gesture::RingCreate { start, now: start };
                }
            }
            Tool::Ring => {
                let hit = self
                    .ops
                    .iter()
                    .enumerate()
                    .filter(|(_, op)| op["type"] == "ring")
                    .filter_map(|(i, op)| {
                        let (p0, p1) = (pair(&op["p0"])?, pair(&op["p1"])?);
                        dd::ring_hit(p0, p1, p, radius).map(|(handle, d)| (i, handle, d, p0, p1))
                    })
                    .min_by(|a, b| a.2.total_cmp(&b.2));
                if let Some((index, handle, _, start_p0, start_p1)) = hit {
                    self.checkpoint();
                    self.selected_op = Some(index);
                    self.gesture = Gesture::RingEdit { index, handle, start_p0, start_p1, start_local: p };
                }
            }
            Tool::AttachedPoint => {
                if let Some(vertex) = dd::nearest_vertex(self.points(), p, f32::MAX) {
                    self.checkpoint();
                    self.ops.push(dd::attached_point_op(vertex, 0.0));
                    self.selected_op = Some(self.ops.len() - 1);
                    self.selection = vec![vertex];
                    self.gesture = Gesture::Attached { op: self.ops.len() - 1, origin_sy: sy };
                }
            }
            Tool::Plane => {
                if let Some(vertex) = dd::nearest_vertex(self.points(), p, f32::MAX) {
                    let center = if self.brush.snap_to_grid { self.points()[vertex] } else { p };
                    self.gesture = Gesture::Plane { center, rx: self.brush.radius_x, ry: self.brush.radius_y };
                }
            }
        }
    }

    fn drag(&mut self, at: V2, sy: f32) {
        let p = self.local(at);
        let target = self.snap(p);
        match self.gesture.clone() {
            Gesture::Idle => {}
            Gesture::Box { origin, add, remove, .. } => self.gesture = Gesture::Box { origin, now: p, add, remove },
            Gesture::Pending { vertex, press_world, origin_sy, .. } => {
                let zoom = self.active.as_ref().map_or(1.0, |a| a.zoom);
                let travelled = ((at[0] - press_world[0]).hypot(at[1] - press_world[1]) * zoom).max(f64::from((sy - origin_sy).abs()));
                if travelled > DRAG_PX {
                    let dragged = if self.selection.is_empty() { vec![vertex] } else { self.selection.clone() };
                    self.checkpoint();
                    self.dirty = true;
                    self.gesture = Gesture::Drag { start: self.working.clone(), dragged, origin_sy };
                    self.drag(at, sy);
                }
            }
            Gesture::Drag { start, dragged, origin_sy } => {
                let delta = dd::visible_depth_delta([0.0, sy], [0.0, origin_sy], [0.0, 0.0]);
                self.working = dd::direct_depth_drag(&start, &dragged, delta);
            }
            Gesture::RingCreate { start, .. } => self.gesture = Gesture::RingCreate { start, now: target },
            Gesture::RingEdit { index, handle, start_p0, start_p1, start_local } => {
                let snap_points = self.brush.snap_to_grid.then(|| self.points().to_vec());
                let (p0, p1) = dd::ring_drag(handle, start_p0, start_p1, start_local, p, snap_points.as_deref());
                if let Some(op) = self.ops.get_mut(index) {
                    set_pair(op, "p0", p0);
                    set_pair(op, "p1", p1);
                }
            }
            Gesture::Attached { op, origin_sy } => {
                let amount = dd::attached_point_amount(origin_sy, sy);
                if let Some(entry) = self.ops.get_mut(op) {
                    entry["amount"] = json!(f64::from(amount));
                }
            }
            Gesture::Plane { center, .. } => {
                let (rx, ry) = dd::plane_radii(center, target);
                self.gesture = Gesture::Plane { center, rx, ry };
            }
        }
    }

    fn release(&mut self) {
        match std::mem::replace(&mut self.gesture, Gesture::Idle) {
            Gesture::Idle | Gesture::Drag { .. } => {}
            Gesture::Box { origin, now, add, remove } => {
                let hits = dd::vertices_in_rect(self.points(), origin, now);
                if remove {
                    self.selection.retain(|i| !hits.contains(i));
                } else if add {
                    for i in hits {
                        if !self.selection.contains(&i) {
                            self.selection.push(i);
                        }
                    }
                } else {
                    self.selection = hits;
                }
            }
            Gesture::Pending { maybe_alone, .. } => {
                if let Some(vertex) = maybe_alone {
                    self.selection = vec![vertex];
                }
            }
            Gesture::RingCreate { start, now } => {
                let (p0, p1) = dd::ring_endpoints(start, now, &self.brush);
                self.checkpoint();
                self.ops.push(dd::ring_op(p0, p1, &self.brush));
                self.selected_op = Some(self.ops.len() - 1);
                self.recompute();
            }
            Gesture::RingEdit { .. } => {
                // A click that moved nothing leaves no edit behind.
                if self.undo.last().is_some_and(|s| s.ops == self.ops) {
                    self.undo.pop();
                } else {
                    self.recompute();
                }
            }
            Gesture::Attached { .. } => self.recompute(),
            Gesture::Plane { center, rx, ry } => {
                self.checkpoint();
                self.ops.push(dd::plane_op(center, rx, ry, &self.brush));
                self.selected_op = Some(self.ops.len() - 1);
                self.recompute();
            }
        }
    }
}

// ------------------------------------------------------------------------------------------ strip

fn drag_value(ui: &mut egui::Ui, label: &str, value: &mut f32, range: std::ops::RangeInclusive<f32>, speed: f64, hover: &str) -> bool {
    let name = ui.label(label);
    ui.add(egui::DragValue::new(value).range(range).speed(speed).fixed_decimals(2))
        .labelled_by(name.id)
        .on_hover_text(hover)
        .changed()
}


impl DepthTools {
    /// The tool strip and tool settings of the D depth toolbar, confirm bar and Depth dropdown. Returns the
    /// requests to send (Apply).
    pub fn strip(&mut self, ui: &mut egui::Ui) -> Vec<Value> {
        let mut requests = Vec::new();
        if self.active.is_none() {
            if let Some(id) = self.candidate {
                if ui
                    .button("Edit Depth")
                    .on_hover_text("Edit the depth map of the selected GridDeformer with the depth tools")
                    .clicked()
                {
                    self.enter(id);
                }
            }
            return requests;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Depth:");
            for tool in Tool::ALL {
                if ui.selectable_label(self.tool == tool, tool.label()).on_hover_text(tool.hover()).clicked() {
                    self.set_tool(tool);
                }
            }
            if ui
                .button("Depth Draw")
                .on_hover_text("Compose depth layers (PNG files) onto the grid; D has no painting brush")
                .clicked()
            {
                self.show_draw_section = true;
            }
            ui.separator();
            ui.checkbox(&mut self.brush.snap_to_grid, "Snap to grid vertices")
                .on_hover_text("Snap tool points to grid vertices (off: free tool points)");
            if self.tool == Tool::Ring {
                ui.separator();
                if ui.selectable_label(self.ring_mode == RingMode::Create, "Create").on_hover_text("Create depth rings").clicked() {
                    self.set_ring_mode(RingMode::Create);
                }
                if ui.selectable_label(self.ring_mode == RingMode::Edit, "Edit").on_hover_text("Edit existing depth rings").clicked() {
                    self.set_ring_mode(RingMode::Edit);
                }
            }
            ui.separator();
            if ui.add_enabled(self.can_undo_edit(), egui::Button::new("Undo edit")).on_hover_text("Drop the last edit").clicked() {
                self.undo_edit();
            }
            if ui.add_enabled(self.can_redo_edit(), egui::Button::new("Redo edit")).on_hover_text("Bring the dropped edit back").clicked() {
                self.redo_edit();
            }
            if ui.button("Apply").on_hover_text("Apply the depth edits to the model").clicked() {
                requests = self.apply();
            }
            if ui.button("Cancel").on_hover_text("Discard the depth edits").clicked() {
                self.cancel();
            }
        });
        if self.active.is_none() {
            return requests;
        }
        self.settings_ui(ui);
        self.operations_ui(ui);
        requests.extend(self.depth_draw_ui(ui));
        if !self.notice.is_empty() {
            ui.colored_label(Color32::LIGHT_RED, self.notice.clone());
        }
        requests
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Depth Tool Settings").default_open(true).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let b = &mut self.brush;
                match self.tool {
                    Tool::DirectDepth => {
                        ui.label("Drag a selected vertex up or down; Shift adds and Ctrl removes from the selection.");
                    }
                    Tool::AttachedPoint => {
                        ui.label("Drag a vertex up or down to set the amount.");
                    }
                    Tool::Ring => {
                        drag_value(ui, "Amount", &mut b.amount, -2.0..=2.0, 0.01, "Depth added at the ring's middle");
                        drag_value(ui, "Length", &mut b.radius_x, 1.0..=1000.0, 1.0, "Half length of a ring made by a click");
                        drag_value(ui, "Width", &mut b.radius_y, 1.0..=400.0, 1.0, "Ring width");
                        drag_value(ui, "Angle", &mut b.angle, -180.0..=180.0, 1.0, "Direction of a ring made by a click");
                        drag_value(ui, "Falloff", &mut b.hardness, 0.1..=8.0, 0.05, "How sharply the ring fades to the sides");
                    }
                    Tool::Plane => {
                        drag_value(ui, "Target", &mut b.amount, -2.0..=2.0, 0.01, "Depth the plane flattens to");
                        drag_value(ui, "Radius X", &mut b.radius_x, 1.0..=1000.0, 1.0, "Radius along the angle");
                        drag_value(ui, "Radius Y", &mut b.radius_y, 1.0..=1000.0, 1.0, "Radius across the angle");
                        drag_value(ui, "Angle", &mut b.angle, -180.0..=180.0, 1.0, "Rotation of the plane");
                        drag_value(ui, "Flatten", &mut b.flatten_strength, 0.0..=1.0, 0.01, "How fully the plane flattens");
                    }
                }
            });
        });
    }

    /// D "Depth Deformers" list with the "Selected Deformer" editor.
    fn operations_ui(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Depth Deformers").default_open(true).show(ui, |ui| {
            if self.ops.is_empty() {
                ui.label("No depth deformers.");
            }
            for index in 0..self.ops.len() {
                let label = format!("{}  {}", dd::op_label(&self.ops[index]), dd::op_value_label(&self.ops[index]));
                if ui.selectable_label(self.selected_op == Some(index), label).clicked() {
                    self.selected_op = Some(index);
                }
            }
            let Some(index) = self.selected_op.filter(|i| *i < self.ops.len()) else { return };
            ui.separator();
            ui.label("Selected Deformer");
            let fields: &[(&str, &str, f32, std::ops::RangeInclusive<f32>, f64)] = match self.ops[index]["type"].as_str() {
                Some("attached-point") => &[("Amount", "amount", 0.0, -2.0..=2.0, 0.01)],
                Some("ring") => &[
                    ("Amount", "amount", 0.0, -2.0..=2.0, 0.01),
                    ("P0 Angle", "p0Angle", 180.0, -360.0..=360.0, 1.0),
                    ("P1 Angle", "p1Angle", 0.0, -360.0..=360.0, 1.0),
                    ("Width", "width", 1.0, 1.0..=400.0, 1.0),
                    ("Falloff", "hardness", 1.0, 0.1..=8.0, 0.05),
                ],
                Some("plane") => &[
                    ("Target", "targetDepth", 0.0, -2.0..=2.0, 0.01),
                    ("Radius X", "radiusX", 1.0, 1.0..=1000.0, 1.0),
                    ("Radius Y", "radiusY", 1.0, 1.0..=1000.0, 1.0),
                    ("Angle", "angle", 0.0, -180.0..=180.0, 1.0),
                    ("Flatten", "flattenStrength", 1.0, 0.0..=1.0, 0.01),
                ],
                _ => &[],
            };
            ui.horizontal_wrapped(|ui| {
                for (label, key, fallback, range, speed) in fields {
                    let mut value = op_f32(&self.ops[index], key, *fallback);
                    if drag_value(ui, &format!("Operation {label}"), &mut value, range.clone(), *speed, label) {
                        self.edit_selected_op(key, value);
                    }
                }
            });
            ui.horizontal(|ui| {
                if ui.button("Duplicate").on_hover_text("Add a copy of the selected deformer").clicked() {
                    self.duplicate_selected_op();
                }
                if ui.button("Delete").on_hover_text("Remove the selected deformer (Delete key)").clicked() {
                    self.delete_selected_op();
                }
            });
        });
    }

    /// The layer-composition half of D's depth viewport, for the grid being edited.
    fn depth_draw_ui(&mut self, ui: &mut egui::Ui) -> Vec<Value> {
        let Some(id) = self.active.as_ref().map(|a| a.id) else { return Vec::new() };
        let mut header = egui::CollapsingHeader::new("Depth Draw").default_open(self.depth_draw_open);
        if std::mem::take(&mut self.show_draw_section) {
            header = header.open(Some(true));
        }
        header.show(ui, |ui| self.draw.show(ui, id)).body_returned.unwrap_or_default()
    }

    /// Takes the session's answer to a request [`Self::strip`] returned.
    pub fn deliver(&mut self, request: &Value, response: &Value) {
        self.draw.deliver(request, response);
        if response["succeeded"] != true && !request["op"].as_str().unwrap_or("").starts_with("depth.draw.") {
            self.notice = response["message"].as_str().unwrap_or("the request failed").to_owned();
        }
    }
}

// --------------------------------------------------------------------------------------- pointer feed

fn mods_of(m: egui::Modifiers) -> Mods {
    Mods { ctrl: m.ctrl || m.command, shift: m.shift, alt: m.alt }
}

fn world_of(picture: &Picture, pos: Pos2) -> V2 {
    picture.camera.to_world(picture.size, [f64::from(pos.x - picture.rect.min.x), f64::from(pos.y - picture.rect.min.y)])
}

fn screen_of(picture: &Picture, world: V2) -> Pos2 {
    let s = picture.camera.to_screen(picture.size, world);
    Pos2::new(picture.rect.min.x + s[0] as f32, picture.rect.min.y + s[1] as f32)
}

impl DepthTools {
    /// Turns this frame's egui events over the picture into tool events. Call before the viewport renders.
    pub fn pointer(&mut self, ui: &egui::Ui, picture: &Picture, response: &egui::Response) {
        if !self.captures() {
            return;
        }
        let over = response.hovered() || response.contains_pointer();
        let events = ui.input(|i| i.events.clone());
        let wants_keys = ui.ctx().wants_keyboard_input();
        let mut mods = ui.input(|i| mods_of(i.modifiers));
        for event in events {
            match event {
                egui::Event::PointerMoved(pos) if (self.down || picture.rect.contains(pos)) && (self.down || over) => {
                    self.event(Ev::Move { at: world_of(picture, pos), sy: pos.y, mods });
                }
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers } => {
                    mods = mods_of(modifiers);
                    if pressed && !self.down && picture.rect.contains(pos) && over {
                        self.event(Ev::Press { at: world_of(picture, pos), sy: pos.y, mods });
                    } else if !pressed && self.down {
                        self.event(Ev::Release { at: world_of(picture, pos), sy: pos.y, mods });
                    }
                }
                egui::Event::Key { key, pressed: true, .. } if over && !wants_keys => {
                    let key = match key {
                        egui::Key::Tab => Key::Tab,
                        egui::Key::Delete | egui::Key::Backspace => Key::Delete,
                        egui::Key::Escape => Key::Escape,
                        _ => continue,
                    };
                    self.event(Ev::Key(key));
                }
                _ => {}
            }
        }
    }
}

// ------------------------------------------------------------------------------------------ painting

const EDGE: Color32 = Color32::from_gray(128);
const SELECTED: Color32 = Color32::from_rgb(255, 0, 0);

fn color_of(c: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

fn ellipse_points(center: P, rx: f32, ry: f32, angle_deg: f32) -> Vec<P> {
    let a = angle_deg.to_radians();
    let (ux, uy) = ([a.cos(), a.sin()], [-a.sin(), a.cos()]);
    (0..=48)
        .map(|i| {
            let t = i as f32 / 48.0 * std::f32::consts::TAU;
            let (x, y) = (t.cos() * rx, t.sin() * ry);
            [center[0] + ux[0] * x + uy[0] * y, center[1] + ux[1] * x + uy[1] * y]
        })
        .collect()
}

impl DepthTools {
    /// The depth ramp over the mesh, the vertices, the pending operations and the running gesture.
    pub fn paint(&self, ui: &egui::Ui, picture: &Picture) {
        let Some(active) = &self.active else { return };
        let painter = ui.painter_at(picture.rect);
        let at = |local: P| screen_of(picture, active.frame.to_world([f64::from(local[0]), f64::from(local[1])]));
        // Compose Preview shows what the layers would write; otherwise the pending edit is shown.
        let display = self.draw.display();
        let composed = self
            .draw
            .composed(active.id)
            .filter(|(depths, _)| display.show_composite && depths.len() == active.points.len());
        let shown: &[f32] = composed.as_ref().map_or(&self.working, |(depths, _)| depths);
        let finite = shown.iter().copied().filter(|d| d.is_finite());
        let (min, max) = finite.fold((f32::MAX, f32::MIN), |(lo, hi), d| (lo.min(d), hi.max(d)));
        let color = |i: usize| {
            let [r, g, b, _] = dd::depth_ramp_color(shown.get(i).copied().unwrap_or(0.0), min, max);
            Color32::from_rgba_unmultiplied(r, g, b, 170)
        };
        let columns = active.columns.max(2);
        let mut mesh = egui::Mesh::default();
        for (i, p) in active.points.iter().enumerate() {
            mesh.colored_vertex(at(*p), color(i));
        }
        for i in 0..active.points.len() {
            if (i + 1) % columns != 0 && i + columns + 1 < active.points.len() {
                let (a, b, c, d) = (i as u32, i as u32 + 1, (i + columns + 1) as u32, (i + columns) as u32);
                mesh.add_triangle(a, b, c);
                mesh.add_triangle(a, c, d);
            }
        }
        painter.add(egui::Shape::mesh(mesh));
        for (a, b) in lattice(active.points.len(), columns) {
            painter.line_segment([at(active.points[a]), at(active.points[b])], Stroke::new(1.0, EDGE));
        }
        if display.show_layer_planes {
            for (_, corners, selected) in self.draw.planes() {
                let stroke = Stroke::new(if selected { 2.0 } else { 1.0 }, Color32::from_rgb(255, 200, 80));
                for i in 0..4 {
                    painter.line_segment([screen_of(picture, corners[i]), screen_of(picture, corners[(i + 1) % 4])], stroke);
                }
            }
        }
        if let Some((_, winners)) = composed.as_ref().filter(|(_, winners)| winners.len() == active.points.len()) {
            for (point, winner) in active.points.iter().zip(winners) {
                if winner.is_empty() {
                    if display.show_missing_vertices {
                        painter.circle_stroke(at(*point), 9.0, Stroke::new(2.0, Color32::from_rgb(255, 60, 60)));
                    }
                } else if display.show_winning_layer {
                    painter.circle_filled(at(*point), 7.0, color_of(dd::winning_layer_color(winner).map(|c| (c * 255.0).round() as u8)));
                }
            }
        }
        let hot = self
            .cursor
            .filter(|_| matches!(self.gesture, Gesture::Idle) && self.tool == Tool::DirectDepth)
            .and_then(|c| dd::nearest_vertex(&active.points, c, HIT_PX as f32 * self.local_per_px()));
        for (i, p) in active.points.iter().enumerate() {
            let pos = at(*p);
            painter.circle_filled(pos, 5.0, Color32::BLACK);
            painter.circle_filled(pos, 3.0, if self.selection.contains(&i) { SELECTED } else { Color32::WHITE });
            if hot == Some(i) {
                painter.circle_filled(pos, 7.5, Color32::from_white_alpha(76));
            }
        }
        for (index, op) in self.ops.iter().enumerate() {
            self.paint_op(&painter, &at, index, op, active);
        }
        self.paint_gesture(&painter, &at);
    }

    fn paint_op(&self, painter: &egui::Painter, at: &dyn Fn(P) -> Pos2, index: usize, op: &Value, active: &Active) {
        let selected = self.selected_op == Some(index);
        let amount_key = if op["type"] == "plane" { "targetDepth" } else { "amount" };
        let color = color_of(dd::operation_color(op_f32(op, amount_key, 0.0), selected));
        let stroke = Stroke::new(if selected { 3.0 } else { 2.0 }, color);
        let polyline = |points: &[P], stroke: Stroke| {
            for w in points.windows(2) {
                painter.line_segment([at(w[0]), at(w[1])], stroke);
            }
        };
        match op["type"].as_str() {
            Some("attached-point") => {
                let vertex = op["index"].as_u64().and_then(|i| active.points.get(i as usize));
                if let Some(v) = vertex {
                    painter.circle_stroke(at(*v), if selected { 11.0 } else { 9.0 }, stroke);
                    painter.text(
                        at(*v) + egui::vec2(10.0, -10.0),
                        egui::Align2::LEFT_BOTTOM,
                        dd::op_value_label(op),
                        egui::FontId::proportional(11.0),
                        color,
                    );
                }
            }
            Some("ring") => {
                if let (Some(p0), Some(p1)) = (pair(&op["p0"]), pair(&op["p1"])) {
                    painter.line_segment([at(p0), at(p1)], Stroke::new(1.0, Color32::from_gray(140)));
                    let centre = [(p0[0] + p1[0]) / 2.0, (p0[1] + p1[1]) / 2.0];
                    let half = dist(p0, p1) / 2.0;
                    let angle = (p1[1] - p0[1]).atan2(p1[0] - p0[0]).to_degrees();
                    polyline(&ellipse_points(centre, half.max(1.0), op_f32(op, "width", 1.0), angle), stroke);
                    for p in [p0, p1] {
                        painter.circle_filled(at(p), if selected { 5.0 } else { 4.0 }, Color32::from_rgb(25, 166, 255));
                    }
                }
            }
            Some("plane") => {
                if let Some(c) = pair(&op["center"]) {
                    polyline(&ellipse_points(c, op_f32(op, "radiusX", 1.0), op_f32(op, "radiusY", 1.0), op_f32(op, "angle", 0.0)), stroke);
                    painter.circle_filled(at(c), 4.0, color);
                }
            }
            _ => {}
        }
    }

    fn paint_gesture(&self, painter: &egui::Painter, at: &dyn Fn(P) -> Pos2) {
        let preview = Stroke::new(2.0, color_of(dd::operation_color(self.brush.amount, true)));
        match &self.gesture {
            Gesture::Box { origin, now, add, remove } => {
                let color = match (add, remove) {
                    (_, true) => Color32::from_rgba_unmultiplied(0, 255, 255, 204),
                    (true, _) => Color32::from_rgba_unmultiplied(0, 255, 0, 204),
                    _ => SELECTED,
                };
                let corners = [*origin, [now[0], origin[1]], *now, [origin[0], now[1]]];
                for i in 0..4 {
                    painter.line_segment([at(corners[i]), at(corners[(i + 1) % 4])], Stroke::new(1.0, color));
                }
            }
            Gesture::RingCreate { start, now } => {
                let (a, b) = dd::ring_preview_line(*start, *now, &self.brush);
                painter.line_segment([at(a), at(b)], preview);
            }
            Gesture::Plane { center, rx, ry } => {
                for w in ellipse_points(*center, *rx, *ry, self.brush.angle).windows(2) {
                    painter.line_segment([at(w[0]), at(w[1])], preview);
                }
            }
            _ => {}
        }
    }
}

/// Lines of a lattice of `n` points stored row by row, `columns` points per row.
fn lattice(n: usize, columns: usize) -> Vec<(usize, usize)> {
    let mut edges = Vec::new();
    for i in 0..n {
        if (i + 1) % columns != 0 && i + 1 < n {
            edges.push((i, i + 1));
        }
        if i + columns < n {
            edges.push((i, i + columns));
        }
    }
    edges
}

// ------------------------------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor_viewport::{Camera, ViewportState};
    use egui::accesskit::Role;

    const NONE: Mods = Mods { ctrl: false, shift: false, alt: false };
    const CTRL: Mods = Mods { ctrl: true, shift: false, alt: false };
    const SHIFT: Mods = Mods { ctrl: false, shift: true, alt: false };

    fn transform() -> Value {
        json!({"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]})
    }

    fn grid(extra: Value) -> Value {
        let mut node = json!({
            "uuid": 5, "name": "Grid", "type": "GridDeformer", "dynamic": false, "enabled": true, "zsort": 0.0,
            "transform": transform(), "children": [], "lockToRoot": false, "pinToMesh": false,
            "grid_axis_x": [0.0, 50.0, 100.0], "grid_axis_y": [0.0, 50.0, 100.0],
        });
        for (key, value) in extra.as_object().unwrap() {
            node[key] = value.clone();
        }
        node
    }

    fn session_with(node: Value) -> Session {
        let model = json!({
            "meta": {"name": "fixture", "version": "1.0-alpha"},
            "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
                "transform": transform(), "lockToRoot": false, "pinToMesh": false, "children": [node]},
            "param": [],
            "animations": {},
        });
        let mut session = Session::new();
        let reply = session.handle_request(&json!({"op": "new", "model": model}));
        assert_eq!(reply["succeeded"], true, "{reply}");
        session
    }

    /// A 3 x 3 grid: vertices (0,0) (50,0) (100,0) / (0,50) (50,50) (100,50) / (0,100) (50,100) (100,100).
    fn plain() -> Session {
        session_with(grid(json!({"depths": vec![0.0; 9]})))
    }

    fn sync(tools: &mut DepthTools, session: &Session) {
        let snapshot = session.snapshot();
        let ctx = Ctx { model: &snapshot["model"], selected: Some(5), armed: false, frame: Some(Frame::IDENTITY), zoom: 1.0, draw_key: (0, 0) };
        tools.sync(&ctx);
    }

    fn started(session: &Session) -> DepthTools {
        let mut tools = DepthTools::new();
        sync(&mut tools, session);
        assert_eq!(tools.candidate(), Some(5));
        tools.enter(5);
        sync(&mut tools, session);
        assert!(tools.captures());
        tools
    }

    fn press(tools: &mut DepthTools, at: V2, sy: f32, mods: Mods) {
        tools.event(Ev::Press { at, sy, mods });
    }

    fn drag_to(tools: &mut DepthTools, at: V2, sy: f32) {
        tools.event(Ev::Move { at, sy, mods: NONE });
    }

    fn release(tools: &mut DepthTools, at: V2, sy: f32) {
        tools.event(Ev::Release { at, sy, mods: NONE });
    }

    fn send(session: &mut Session, requests: &[Value]) {
        for request in requests {
            let reply = session.handle_request(request);
            assert_eq!(reply["succeeded"], true, "{request} -> {reply}");
        }
    }

    fn node_of(session: &Session) -> Value {
        find_node(&session.snapshot()["model"]["nodes"], 5).unwrap().clone()
    }

    fn depths_of(session: &Session) -> Vec<f32> {
        numbers(&node_of(session)["depths"]).unwrap()
    }

    fn near(a: &[f32], b: &[f32]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
    }

    // ---------------------------------------------------------------- gesture to pending edit

    #[test]
    fn direct_depth_selects_then_drags_the_selection() {
        let session = plain();
        let mut tools = started(&session);
        // A click on the centre vertex selects it; a rectangle around the bottom row adds with Shift.
        press(&mut tools, [50.0, 50.0], 300.0, NONE);
        release(&mut tools, [50.0, 50.0], 300.0);
        assert_eq!(tools.selection(), [4]);
        press(&mut tools, [-20.0, 80.0], 0.0, SHIFT);
        drag_to(&mut tools, [120.0, 120.0], 0.0);
        release(&mut tools, [120.0, 120.0], 0.0);
        assert_eq!(tools.selection(), [4, 6, 7, 8]);
        // Ctrl+click removes a vertex.
        press(&mut tools, [50.0, 100.0], 0.0, CTRL);
        release(&mut tools, [50.0, 100.0], 0.0);
        assert_eq!(tools.selection(), [4, 6, 8]);
        // Press on a selected vertex, drag up 100 px: every selected vertex gains 0.6, the rest stay.
        press(&mut tools, [50.0, 50.0], 300.0, NONE);
        drag_to(&mut tools, [50.0, 50.0], 200.0);
        release(&mut tools, [50.0, 50.0], 200.0);
        assert!(tools.direct_edits());
        assert!(near(tools.working_depths(), &[0.0, 0.0, 0.0, 0.0, 0.6, 0.0, 0.6, 0.0, 0.6]), "{:?}", tools.working_depths());
        assert_eq!(tools.selection(), [4, 6, 8], "a drag keeps the group");
        assert!(tools.pending_ops().is_empty());
    }

    #[test]
    fn a_click_on_a_selected_vertex_keeps_the_group_until_release() {
        let session = plain();
        let mut tools = started(&session);
        for (x, y) in [(0.0, 0.0), (100.0, 0.0)] {
            press(&mut tools, [x, y], 0.0, SHIFT);
            release(&mut tools, [x, y], 0.0);
        }
        assert_eq!(tools.selection(), [0, 2]);
        press(&mut tools, [0.0, 0.0], 0.0, NONE);
        assert_eq!(tools.selection(), [0, 2]);
        release(&mut tools, [0.0, 0.0], 0.0);
        assert_eq!(tools.selection(), [0]);
        assert!(!tools.direct_edits());
        // A press on empty space with no modifier clears the selection (an empty rectangle).
        press(&mut tools, [-500.0, -500.0], 0.0, NONE);
        release(&mut tools, [-500.0, -500.0], 0.0);
        assert!(tools.selection().is_empty());
    }

    #[test]
    fn ring_gestures_create_and_edit_operations() {
        let session = plain();
        let mut tools = started(&session);
        tools.set_tool(Tool::Ring);
        tools.brush = dd::BrushSettings { amount: 0.5, radius_y: 20.0, hardness: 2.0, ..dd::BrushSettings::default() };
        // Drag from the vertex at (0,0) to the one at (100,0), snapped to vertices.
        press(&mut tools, [3.0, 2.0], 0.0, NONE);
        drag_to(&mut tools, [97.0, 4.0], 0.0);
        release(&mut tools, [97.0, 4.0], 0.0);
        let op = &tools.pending_ops()[0];
        assert_eq!(op["type"], "ring");
        assert_eq!(pair(&op["p0"]), Some([0.0, 0.0]));
        assert_eq!(pair(&op["p1"]), Some([100.0, 0.0]));
        assert_eq!((op_f32(op, "amount", 0.0), op_f32(op, "width", 0.0), op_f32(op, "hardness", 0.0)), (0.5, 20.0, 2.0));
        // A click makes a ring of length 2 * radius_x along the angle, centred on the snapped vertex.
        tools.brush.radius_x = 30.0;
        press(&mut tools, [50.0, 52.0], 0.0, NONE);
        release(&mut tools, [50.0, 52.0], 0.0);
        let click = &tools.pending_ops()[1];
        assert_eq!((pair(&click["p0"]), pair(&click["p1"])), (Some([20.0, 50.0]), Some([80.0, 50.0])));
        // Tab switches to Edit; grabbing p1 of the first ring and dropping it on (50, 50) snaps there.
        tools.event(Ev::Key(Key::Tab));
        assert_eq!(tools.ring_mode, RingMode::Edit);
        press(&mut tools, [98.0, 1.0], 0.0, NONE);
        drag_to(&mut tools, [52.0, 49.0], 0.0);
        release(&mut tools, [52.0, 49.0], 0.0);
        assert_eq!(pair(&tools.pending_ops()[0]["p1"]), Some([50.0, 50.0]));
        assert_eq!(pair(&tools.pending_ops()[0]["p0"]), Some([0.0, 0.0]));
        // Grabbing the body moves both endpoints by the travel (unsnapped).
        tools.brush.snap_to_grid = false;
        press(&mut tools, [25.0, 25.0], 0.0, NONE);
        drag_to(&mut tools, [35.0, 30.0], 0.0);
        release(&mut tools, [35.0, 30.0], 0.0);
        assert_eq!(pair(&tools.pending_ops()[0]["p0"]), Some([10.0, 5.0]));
        // A click that moves nothing leaves no undo step behind.
        let steps = tools.undo.len();
        press(&mut tools, [10.0, 5.0], 0.0, NONE);
        release(&mut tools, [10.0, 5.0], 0.0);
        assert_eq!(tools.undo.len(), steps);
    }

    #[test]
    fn attached_point_amount_follows_the_vertical_drag() {
        let session = plain();
        let mut tools = started(&session);
        tools.set_tool(Tool::AttachedPoint);
        press(&mut tools, [52.0, 49.0], 400.0, NONE);
        drag_to(&mut tools, [52.0, 49.0], 300.0);
        release(&mut tools, [52.0, 49.0], 300.0);
        let op = &tools.pending_ops()[0];
        assert_eq!((op["type"].as_str(), op["index"].as_u64()), (Some("attached-point"), Some(4)));
        assert!((op_f32(op, "amount", 0.0) - 0.6).abs() < 1e-5, "100 px up is +0.6: {op}");
        assert!((tools.working_depths()[4] - 0.6).abs() < 1e-4, "the preview applied the operation");
        assert_eq!(tools.working_depths()[0], 0.0);
        // Dragging down gives a negative amount.
        press(&mut tools, [0.0, 0.0], 100.0, NONE);
        release(&mut tools, [0.0, 0.0], 150.0);
        assert!((op_f32(&tools.pending_ops()[1], "amount", 0.0) + 0.3).abs() < 1e-5);
    }

    #[test]
    fn plane_radii_follow_the_pointer_with_a_four_unit_minimum() {
        let session = plain();
        let mut tools = started(&session);
        tools.set_tool(Tool::Plane);
        tools.brush = dd::BrushSettings { amount: -0.4, angle: 15.0, flatten_strength: 0.5, ..dd::BrushSettings::default() };
        press(&mut tools, [49.0, 51.0], 0.0, NONE);
        drag_to(&mut tools, [100.0, 100.0], 0.0);
        release(&mut tools, [100.0, 100.0], 0.0);
        let op = &tools.pending_ops()[0];
        assert_eq!(op["type"], "plane");
        assert_eq!(pair(&op["center"]), Some([50.0, 50.0]));
        assert_eq!((op_f32(op, "radiusX", 0.0), op_f32(op, "radiusY", 0.0)), (50.0, 50.0));
        assert_eq!((op_f32(op, "targetDepth", 0.0), op_f32(op, "angle", 0.0), op_f32(op, "flattenStrength", 0.0)), (-0.4, 15.0, 0.5));
        // A release on the centre keeps the minimum radius of 4.
        press(&mut tools, [50.0, 50.0], 0.0, NONE);
        release(&mut tools, [50.0, 50.0], 0.0);
        let op = &tools.pending_ops()[1];
        assert_eq!((op_f32(op, "radiusX", 0.0), op_f32(op, "radiusY", 0.0)), (4.0, 4.0));
        assert!(tools.working_depths().iter().any(|d| d.abs() > 0.01), "the plane flattened some vertices");
    }

    #[test]
    fn pointer_travel_is_measured_in_the_grids_own_space() {
        let session = plain();
        let mut tools = DepthTools::new();
        // The node is scaled 2x and moved by (1000, 0): world (1100, 100) is local (50, 50).
        let world = [[2.0, 0.0, 0.0, 1000.0], [0.0, 2.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
        let frame = Frame::from_world_matrix(&world).unwrap();
        let snapshot = session.snapshot();
        let ctx = Ctx { model: &snapshot["model"], selected: Some(5), armed: false, frame: Some(frame), zoom: 1.0, draw_key: (0, 0) };
        tools.sync(&ctx);
        tools.enter(5);
        tools.sync(&ctx);
        press(&mut tools, [1100.0, 100.0], 0.0, NONE);
        release(&mut tools, [1100.0, 100.0], 0.0);
        assert_eq!(tools.selection(), [4]);
        tools.set_tool(Tool::Plane);
        tools.brush.snap_to_grid = false;
        press(&mut tools, [1100.0, 100.0], 0.0, NONE);
        drag_to(&mut tools, [1160.0, 100.0], 0.0);
        release(&mut tools, [1160.0, 100.0], 0.0);
        assert_eq!(pair(&tools.pending_ops()[0]["center"]), Some([50.0, 50.0]));
        assert_eq!(op_f32(&tools.pending_ops()[0], "radiusX", 0.0), 30.0, "60 world units are 30 local units");
    }

    // ------------------------------------------------------------------ pending list and history

    #[test]
    fn undo_redo_delete_and_duplicate_of_pending_operations() {
        let session = plain();
        let mut tools = started(&session);
        tools.set_tool(Tool::AttachedPoint);
        for (x, y0, y1) in [(0.0, 200.0, 100.0), (100.0, 200.0, 150.0)] {
            press(&mut tools, [x, 0.0], y0, NONE);
            release(&mut tools, [x, 0.0], y1);
        }
        assert_eq!(tools.pending_ops().len(), 2);
        assert!(tools.can_undo_edit() && !tools.can_redo_edit());
        tools.undo_edit();
        assert_eq!(tools.pending_ops().len(), 1);
        assert!((tools.working_depths()[0] - 0.6).abs() < 1e-4 && tools.working_depths()[2] == 0.0 || tools.working_depths()[2].abs() < 1e-4);
        tools.redo_edit();
        assert_eq!(tools.pending_ops().len(), 2);
        assert!((tools.working_depths()[2] - 0.3).abs() < 1e-4);
        tools.duplicate_selected_op();
        assert_eq!(tools.pending_ops().len(), 3);
        assert!((tools.working_depths()[2] - 0.6).abs() < 1e-4, "the duplicate adds its amount again");
        tools.event(Ev::Key(Key::Delete));
        assert_eq!(tools.pending_ops().len(), 2);
        tools.edit_selected_op("amount", 1.0);
        assert!((tools.working_depths()[2] - 1.0).abs() < 1e-4);
        tools.cancel();
        assert!(!tools.captures());
        assert!(tools.pending_ops().is_empty());
    }

    #[test]
    fn the_whole_document_works_as_a_preview_base_too() {
        let session = session_with(grid(json!({
            "depths": vec![0.0; 9],
            "children": [{"uuid": 9, "name": "Child", "type": "Node", "enabled": true, "zsort": 0.0,
                "transform": transform(), "lockToRoot": false, "pinToMesh": false, "children": []}],
        })));
        let model = session.snapshot()["model"].clone();
        let ops = [dd::attached_point_op(4, 0.25)];
        let pruned = scratch_model(&model, 5, &[0.0; 9]).unwrap();
        assert!(find_node(&pruned["nodes"], 9).is_none(), "the pruned copy drops the grid's children");
        let full = full_scratch(&model, 5, &[0.0; 9]).unwrap();
        assert!(find_node(&full["nodes"], 9).is_some());
        let (a, b) = (preview_depths(&pruned, 5, &ops).unwrap(), preview_depths(&full, 5, &ops).unwrap());
        assert_eq!(a, b);
        assert!((a[4] - 0.25).abs() < 1e-6);
        assert!(preview_depths(&pruned, 5, &[json!({"type": "spiral"})]).is_err());
    }

    #[test]
    fn leaving_the_node_or_arming_a_parameter_drops_the_mode() {
        let session = plain();
        let mut tools = started(&session);
        let snapshot = session.snapshot();
        let armed = Ctx { model: &snapshot["model"], selected: Some(5), armed: true, frame: Some(Frame::IDENTITY), zoom: 1.0, draw_key: (0, 0) };
        tools.sync(&armed);
        assert!(!tools.captures());
        assert_eq!(tools.candidate(), None);
        let mut tools = started(&session);
        let other = Ctx { model: &snapshot["model"], selected: Some(1), armed: false, frame: Some(Frame::IDENTITY), zoom: 1.0, draw_key: (0, 0) };
        tools.sync(&other);
        assert!(!tools.captures());
        assert_eq!(tools.candidate(), None, "the root is not a GridDeformer");
    }

    // ----------------------------------------------------------- round trips through a real Session

    #[test]
    fn direct_depth_apply_is_one_edit_apply_and_one_undo() {
        let mut session = plain();
        let before = depths_of(&session);
        let mut tools = started(&session);
        press(&mut tools, [50.0, 50.0], 300.0, NONE);
        drag_to(&mut tools, [50.0, 50.0], 200.0);
        release(&mut tools, [50.0, 50.0], 200.0);
        let requests = tools.apply();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["op"], "depth.edit.apply");
        assert_eq!((requests[0]["target"].as_u64(), requests[0]["directEdits"].as_bool()), (Some(5), Some(true)));
        send(&mut session, &requests);
        assert!(near(&depths_of(&session), &[0.0, 0.0, 0.0, 0.0, 0.6, 0.0, 0.0, 0.0, 0.0]));
        assert!(!tools.captures(), "Apply leaves depth mode, as D leaves to model mode");
        session.handle_request(&json!({"op": "undo"}));
        assert_eq!(depths_of(&session), before, "one undo restores the depths");
        assert!(node_of(&session).get("depth-ops").is_none());
    }

    #[test]
    fn operation_apply_commits_the_previewed_depths() {
        for tool in [Tool::Ring, Tool::AttachedPoint, Tool::Plane] {
            let mut session = plain();
            let before = depths_of(&session);
            let mut tools = started(&session);
            tools.set_tool(tool);
            match tool {
                Tool::Ring => {
                    press(&mut tools, [0.0, 50.0], 0.0, NONE);
                    drag_to(&mut tools, [100.0, 50.0], 0.0);
                    release(&mut tools, [100.0, 50.0], 0.0);
                }
                Tool::AttachedPoint => {
                    press(&mut tools, [50.0, 50.0], 200.0, NONE);
                    release(&mut tools, [50.0, 50.0], 100.0);
                }
                _ => {
                    press(&mut tools, [50.0, 50.0], 0.0, NONE);
                    drag_to(&mut tools, [100.0, 100.0], 0.0);
                    release(&mut tools, [100.0, 100.0], 0.0);
                }
            }
            let preview = tools.working_depths().to_vec();
            assert!(preview.iter().any(|d| d.abs() > 0.01), "{tool:?} changed the preview");
            let ops = tools.pending_ops().to_vec();
            let requests = tools.apply();
            assert_eq!(requests.iter().map(|r| r["op"].as_str().unwrap()).collect::<Vec<_>>(), ["depth.edit.apply"]);
            let steps = session.snapshot()["history"]["count"].as_u64().unwrap();
            send(&mut session, &requests);
            assert_eq!(session.snapshot()["history"]["count"].as_u64().unwrap(), steps + 1, "{tool:?}: one undo step");
            assert!(near(&depths_of(&session), &preview), "{tool:?}: committed {:?} vs preview {preview:?}", depths_of(&session));
            assert_eq!(node_of(&session)["depth-ops"].as_array().unwrap().len(), ops.len());
            session.handle_request(&json!({"op": "undo"}));
            assert_eq!(depths_of(&session), before, "{tool:?}: one undo restores the depths");
            assert!(node_of(&session).get("depth-ops").is_none(), "{tool:?}: and removes the operations");
        }
    }

    #[test]
    fn the_apply_request_carries_the_pending_operations_and_depths() {
        let session = plain();
        let mut tools = started(&session);
        tools.set_tool(Tool::AttachedPoint);
        press(&mut tools, [50.0, 50.0], 200.0, NONE);
        release(&mut tools, [50.0, 50.0], 100.0);
        let request = tools.combined_request(5, json!([0.0]));
        assert_eq!(request["op"], "depth.edit.apply");
        assert_eq!((request["target"].as_u64(), request["directEdits"].as_bool()), (Some(5), Some(false)));
        assert_eq!(request["operations"].as_array().unwrap().len(), 1);
        assert_eq!(request["depths"], json!([0.0]));
    }

    #[test]
    fn saved_operations_reload_and_direct_edits_clear_them() {
        let mut session = plain();
        send(&mut session, &[
            json!({"op": "depth.ops.set", "target": 5, "operations": [{"type": "attached-point", "index": 4, "amount": 0.5}]}),
            json!({"op": "depth.ops.apply", "target": 5}),
        ]);
        assert!((depths_of(&session)[4] - 0.5).abs() < 1e-4);
        let mut tools = started(&session);
        assert_eq!(tools.pending_ops().len(), 1, "D loads the saved operations");
        assert!((tools.working_depths()[4] - 0.5).abs() < 1e-4);
        // Nothing changed: Apply sends nothing.
        assert!(started(&session).apply().is_empty());
        // A direct drag bakes the preview: the saved operations are cleared, then the depths set.
        press(&mut tools, [0.0, 0.0], 200.0, NONE);
        release(&mut tools, [0.0, 0.0], 200.0);
        press(&mut tools, [0.0, 0.0], 200.0, NONE);
        drag_to(&mut tools, [0.0, 0.0], 100.0);
        release(&mut tools, [0.0, 0.0], 100.0);
        let requests = tools.apply();
        assert_eq!(requests.iter().map(|r| r["op"].as_str().unwrap()).collect::<Vec<_>>(), ["depth.edit.apply"]);
        assert_eq!(requests[0]["directEdits"], true);
        let steps = session.snapshot()["history"]["count"].as_u64().unwrap();
        send(&mut session, &requests);
        assert_eq!(session.snapshot()["history"]["count"].as_u64().unwrap(), steps + 1, "clearing and writing is one undo step");
        assert!(node_of(&session).get("depth-ops").is_none());
        let depths = depths_of(&session);
        assert!((depths[0] - 0.6).abs() < 1e-4 && (depths[4] - 0.5).abs() < 1e-4, "{depths:?}");
    }

    #[test]
    fn a_saved_list_without_a_base_starts_from_zeros_and_undo_in_the_document_reloads() {
        let mut session = plain();
        send(&mut session, &[json!({"op": "depth.set", "target": 5, "depths": [0.1, 0.1, 0.1, 0.1, 0.1, 0.1, 0.1, 0.1, 0.1]})]);
        let mut tools = started(&session);
        press(&mut tools, [0.0, 0.0], 200.0, NONE);
        release(&mut tools, [0.0, 0.0], 100.0);
        send(&mut session, &tools.apply());
        // The document changed: a fresh session of the tools sees the new depths.
        let reopened = started(&session);
        assert!(reopened.working_depths().iter().all(|d| d.is_finite()));
        // Undo in the document while the tools are open reloads them from the document.
        let mut open = started(&session);
        press(&mut open, [100.0, 100.0], 0.0, NONE);
        release(&mut open, [100.0, 100.0], 0.0);
        assert_eq!(open.selection(), [8]);
        session.handle_request(&json!({"op": "undo"}));
        sync(&mut open, &session);
        assert!(open.selection().is_empty(), "the pending state restarts from the document");
        assert!(open.captures());
    }

    // ------------------------------------------------------------------ headless egui frames

    struct Harness {
        ctx: egui::Context,
        viewport: ViewportState,
        tools: DepthTools,
        session: Session,
        requests: Vec<Value>,
        rect: egui::Rect,
    }

    impl Harness {
        fn new(session: Session) -> Self {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut viewport = ViewportState::new();
            viewport.camera = Camera { centre: [50.0, 50.0], zoom: 2.0, mirror: false };
            let mut harness = Self { ctx, viewport, tools: DepthTools::new(), session, requests: Vec::new(), rect: egui::Rect::NOTHING };
            for _ in 0..3 {
                harness.frame(Vec::new());
            }
            harness
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(700.0, 500.0))),
                events,
                ..Default::default()
            };
            let (viewport, tools, session) = (&mut self.viewport, &mut self.tools, &mut self.session);
            let (mut sent, mut rect_out) = (Vec::new(), egui::Rect::NOTHING);
            let output = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
                    sent.extend(tools.strip(ui));
                    let (rect, response) = viewport.allocate(ui);
                    rect_out = rect;
                    viewport.sync_scene(&mut |request| session.handle_request(request), true, false);
                    let snapshot = session.snapshot();
                    let frame = viewport.world_matrix(5).and_then(|m| Frame::from_world_matrix(&m));
                    let draw_key = (
                        snapshot["depthDraw"]["revision"].as_u64().unwrap_or(0),
                        snapshot["revision"].as_u64().unwrap_or(0),
                    );
                    let ctx = Ctx {
                        model: &snapshot["model"], selected: Some(5), armed: false, frame, zoom: viewport.camera.zoom, draw_key,
                    };
                    tools.sync(&ctx);
                    let picture = Picture { rect, camera: viewport.camera, size: viewport.size() };
                    tools.pointer(ui, &picture, &response);
                    tools.paint(ui, &picture);
                });
            });
            self.rect = rect_out;
            for request in &sent {
                let reply = self.session.handle_request(request);
                assert_eq!(reply["succeeded"], true, "{request} -> {reply}");
                self.tools.deliver(request, &reply);
            }
            if !sent.is_empty() {
                self.viewport.invalidate_scene();
            }
            self.requests.extend(sent);
            output
        }

        fn screen(&self, world: V2) -> Pos2 {
            let s = self.viewport.camera.to_screen(self.viewport.size(), world);
            self.rect.min + egui::vec2(s[0] as f32, s[1] as f32)
        }
    }

    fn button(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
    }

    fn labels(output: &egui::FullOutput) -> Vec<String> {
        let update = output.platform_output.accesskit_update.clone().expect("accesskit is enabled");
        update.nodes.into_iter().flat_map(|(_, node)| [node.label().map(str::to_owned), node.value().map(str::to_owned)]).flatten().collect()
    }

    fn assert_named_controls(output: &egui::FullOutput) {
        let update = output.platform_output.accesskit_update.clone().unwrap();
        for (_, node) in update.nodes {
            if matches!(node.role(), Role::Button | Role::CheckBox | Role::Slider | Role::SpinButton | Role::ComboBox) {
                let named = node.label().is_some_and(|l| !l.is_empty()) || !node.labelled_by().is_empty();
                assert!(named, "unlabelled {:?} control {:?}", node.role(), node.value());
            }
        }
    }

    fn expect_labels(output: &egui::FullOutput, expected: &[&str]) {
        let found = labels(output);
        for want in expected {
            assert!(found.iter().any(|l| l == want), "missing {want:?} in {found:?}");
        }
    }

    #[test]
    fn the_depth_strip_and_settings_are_named() {
        let mut harness = Harness::new(plain());
        let output = harness.frame(Vec::new());
        expect_labels(&output, &["Edit Depth"]);
        assert_named_controls(&output);
        harness.tools.enter(5);
        harness.frame(Vec::new());
        let output = harness.frame(Vec::new());
        expect_labels(
            &output,
            &["Depth:", "Direct Depth", "Ring", "Attached Point", "Plane", "Depth Draw", "Snap to grid vertices", "Undo edit",
                "Redo edit", "Apply", "Cancel", "Depth Tool Settings", "Depth Deformers", "No depth deformers."],
        );
        assert_named_controls(&output);
        harness.tools.set_tool(Tool::Ring);
        harness.frame(Vec::new());
        let output = harness.frame(Vec::new());
        expect_labels(&output, &["Create", "Edit", "Amount", "Length", "Width", "Angle", "Falloff"]);
        assert_named_controls(&output);
        harness.tools.set_tool(Tool::Plane);
        harness.frame(Vec::new());
        let output = harness.frame(Vec::new());
        expect_labels(&output, &["Target", "Radius X", "Radius Y", "Flatten"]);
    }

    fn nodes(output: &egui::FullOutput) -> Vec<egui::accesskit::Node> {
        output.platform_output.accesskit_update.clone().unwrap().nodes.into_iter().map(|(_, node)| node).collect()
    }

    fn is_disabled(output: &egui::FullOutput, role: Role, label: &str) -> bool {
        let found: Vec<_> = nodes(output).into_iter().filter(|n| n.role() == role && n.label() == Some(label)).collect();
        assert_eq!(found.len(), 1, "exactly one {role:?} named {label:?}");
        found[0].is_disabled()
    }

    /// Clicks the control `label` by its accessibility bounds and returns the requests that click sent.
    fn click(harness: &mut Harness, role: Role, label: &str) -> Vec<Value> {
        let output = harness.frame(Vec::new());
        let node = nodes(&output)
            .into_iter()
            .find(|n| n.role() == role && n.label() == Some(label))
            .unwrap_or_else(|| panic!("no {role:?} named {label:?}"));
        let bounds = node.bounds().expect("the control has bounds");
        let pos = Pos2::new(((bounds.x0 + bounds.x1) / 2.0) as f32, ((bounds.y0 + bounds.y1) / 2.0) as f32);
        let before = harness.requests.len();
        harness.frame(vec![egui::Event::PointerMoved(pos)]);
        harness.frame(vec![button(pos, true)]);
        harness.frame(vec![button(pos, false)]);
        harness.requests[before..].iter().filter(|r| !is_refresh(r)).cloned().collect()
    }

    fn settle(harness: &mut Harness) -> egui::FullOutput {
        for _ in 0..4 {
            harness.frame(Vec::new());
        }
        harness.frame(Vec::new())
    }

    fn is_refresh(request: &Value) -> bool {
        matches!(request["op"].as_str(), Some("depth.draw.manifest.get" | "depth.draw.diagnostics"))
    }

    fn png(name: &str) -> String {
        let path = crate::scratch::scratch(
            std::env::temp_dir().join(format!("soul2d-editor-depth-draw-{}-{name}", std::process::id())),
        );
        image::RgbaImage::from_fn(4, 4, |x, _| image::Rgba([x as u8 * 60, 0, 0, 255])).save(&path).unwrap();
        path.to_str().unwrap().to_owned()
    }

    #[test]
    fn the_depth_draw_controls_are_enabled_and_send_the_session_requests() {
        let mut harness = Harness::new(plain());
        harness.tools.depth_draw_open = true;
        harness.tools.enter(5);
        harness.frame(Vec::new());
        harness.frame(Vec::new());
        let output = harness.frame(Vec::new());
        expect_labels(
            &output,
            &["Depth Draw", "PNG path", "Layer id", "Load PNG", "Auto Bind Layers", "Compose Preview", "Apply DepthDraw",
                "Fill Alpha-Depth Gaps", "Contour thickness", "Repair Contour", "Fit gap", "Fit margin", "Fit Z to Gap",
                "Export directory", "Overwrite existing files", "Export Depth PNGs", "Composite", "Layer Planes",
                "Depth Ranges", "Missing Vertices", "Winning Layer", "Normal Image", "Raw Depth", "Coverage", "GPU Preview",
                "No layers. Load a PNG to start."],
        );
        assert_named_controls(&output);
        for always in ["Load PNG", "Auto Bind Layers", "Compose Preview", "Apply DepthDraw", "Export Depth PNGs"] {
            assert!(!is_disabled(&output, Role::Button, always), "{always} is enabled");
        }
        for display in ["Composite", "Layer Planes", "Depth Ranges", "Missing Vertices", "Winning Layer", "Normal Image", "Raw Depth", "Coverage", "GPU Preview"] {
            assert!(!is_disabled(&output, Role::CheckBox, display), "{display} is enabled");
        }
        for needs_layer in ["Fill Alpha-Depth Gaps", "Repair Contour", "Fit Z to Gap"] {
            assert!(is_disabled(&output, Role::Button, needs_layer), "{needs_layer} waits for a selected layer with pixels");
        }

        // An empty path is refused before anything is sent.
        assert!(click(&mut harness, Role::Button, "Load PNG").is_empty());
        expect_labels(&harness.frame(Vec::new()), &["Enter the path of a PNG file"]);

        let path = png("layer.png");
        harness.tools.draw.png_path = path.clone();
        harness.tools.draw.png_id = "layer".into();
        let sent = click(&mut harness, Role::Button, "Load PNG");
        assert_eq!(sent, [json!({"op": "depth.draw.load_png", "path": path, "id": "layer"})]);
        // The key changes, the panel notices, asks for the manifest and shows the answer: a few frames.
        let output = settle(&mut harness);
        expect_labels(&output, &["Loaded layer layer", "Visible layer", "Enabled layer", "layer settings", "Z Scale", "Z Offset",
            "Back Depth", "Front Depth", "Invert", "Offset X", "Offset Y", "Scale X", "Scale Y", "Channel", "Convolution",
            "Custom Radius", "Alpha Threshold", "Bound to this grid"]);
        assert_named_controls(&output);
        assert!(is_disabled(&output, Role::Button, "Repair Contour"), "no layer is selected yet");

        assert_eq!(click(&mut harness, Role::Button, "layer"), [json!({"op": "depth.draw.select", "layer": "layer"})]);
        let output = settle(&mut harness);
        for enabled in ["Fill Alpha-Depth Gaps", "Repair Contour", "Fit Z to Gap"] {
            assert!(!is_disabled(&output, Role::Button, enabled), "{enabled} is enabled for the selected layer");
        }
        assert_eq!(
            click(&mut harness, Role::Button, "Fill Alpha-Depth Gaps"),
            [json!({"op": "depth.draw.cleanup", "layer": "layer", "kind": "gap_fill"})]
        );
        assert_eq!(
            click(&mut harness, Role::Button, "Repair Contour"),
            [json!({"op": "depth.draw.cleanup", "layer": "layer", "kind": "contour_repair", "thickness": 2})]
        );
        assert_eq!(click(&mut harness, Role::Button, "Auto Bind Layers"), [json!({"op": "depth.draw.auto_bind"})]);
        assert_eq!(
            click(&mut harness, Role::CheckBox, "Bound to this grid"),
            [json!({"op": "depth.draw.binding.update", "layer": "layer", "target": 5, "enabled": true})]
        );
        settle(&mut harness);
        assert_eq!(
            click(&mut harness, Role::Button, "Fit Z to Gap"),
            [json!({"op": "depth.draw.fit_z", "layer": "layer", "target": 5, "gap": "adjacent", "margin": 0.0})]
        );
        expect_labels(&settle(&mut harness), &["Fit Z failed: adjacent layers are required"]);
        expect_labels(&settle(&mut harness), &["Order", "Merge", "Use normal-layer alpha", "Coverage Threshold"]);
        assert_eq!(
            click(&mut harness, Role::CheckBox, "Invert"),
            [json!({"op": "depth.draw.layer.update", "layer": "layer", "invert": true})]
        );
        assert_eq!(click(&mut harness, Role::Button, "Compose Preview"), [json!({"op": "depth.draw.compose", "target": 5})]);
        assert!(harness.tools.draw.composed(5).is_some(), "the answer is kept for the viewport");
        // The composed grid is drawn: vertices no layer covers get a red ring, the layer outline is drawn.
        let output = harness.frame(Vec::new());
        let red_rings = output
            .shapes
            .iter()
            .filter(|s| matches!(&s.shape, egui::epaint::Shape::Circle(c) if c.stroke.color == Color32::from_rgb(255, 60, 60)))
            .count();
        assert!(red_rings >= 1, "missing vertices are marked after Compose Preview");
        assert_eq!(
            click(&mut harness, Role::CheckBox, "Raw Depth"),
            [json!({"op": "depth.draw.display", "showRawDepth": true})]
        );
        harness.tools.draw.export_dir = crate::scratch::scratch(
            std::env::temp_dir().join(format!("soul2d-editor-depth-draw-out-{}", std::process::id())),
        )
        .to_str()
        .unwrap()
        .to_owned();
        let sent = click(&mut harness, Role::Button, "Export Depth PNGs");
        assert_eq!(sent[0]["op"], "depth.draw.export_png");
        assert_eq!(sent[0]["overwrite"], false);
        assert_eq!(click(&mut harness, Role::Button, "Apply DepthDraw"), [json!({"op": "depth.draw.apply", "target": 5})]);
        // The diagnostics table reads the session after each change.
        let output = settle(&mut harness);
        expect_labels(&output, &["Layer", "Target", "Merge", "Sampled", "Missing", "Won", "Warnings"]);
        assert_named_controls(&output);
    }

    #[test]
    fn pointer_events_over_the_picture_drive_a_depth_drag_and_apply() {
        let mut harness = Harness::new(plain());
        harness.tools.enter(5);
        harness.frame(Vec::new());
        harness.frame(Vec::new());
        let centre = harness.screen([50.0, 50.0]);
        harness.frame(vec![egui::Event::PointerMoved(centre)]);
        harness.frame(vec![button(centre, true)]);
        harness.frame(vec![egui::Event::PointerMoved(centre + egui::vec2(0.0, -40.0))]);
        harness.frame(vec![button(centre + egui::vec2(0.0, -100.0), false)]);
        assert!(harness.tools.direct_edits(), "the drag past the threshold edited the working depths");
        assert!((harness.tools.working_depths()[4] - 0.6).abs() < 1e-3, "{:?}", harness.tools.working_depths());
        assert!(harness.requests.is_empty(), "nothing is sent before Apply");
        let requests = harness.tools.apply();
        for request in &requests {
            assert_eq!(harness.session.handle_request(request)["succeeded"], true);
        }
        assert!((depths_of(&harness.session)[4] - 0.6).abs() < 1e-3);
        let output = harness.frame(Vec::new());
        expect_labels(&output, &["Edit Depth"]);
    }

    #[test]
    fn the_overlay_paints_a_mesh_and_dots_over_the_picture() {
        let mut harness = Harness::new(plain());
        harness.tools.enter(5);
        harness.frame(Vec::new());
        let output = harness.frame(Vec::new());
        let meshes = output
            .shapes
            .iter()
            .filter(|s| matches!(s.shape, egui::epaint::Shape::Mesh(_)))
            .count();
        assert!(meshes >= 1, "the depth ramp is drawn as a coloured mesh");
        let circles = output.shapes.iter().filter(|s| matches!(s.shape, egui::epaint::Shape::Circle(_))).count();
        assert!(circles >= 18, "a black and a white dot for each of the nine vertices, got {circles}");
    }
}
