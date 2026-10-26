//! Interactive model-edit viewport: camera, picking, overlays and the transform handle.
//!
//! Port of the model-mode parts of panels/viewport.d, viewport/base.d (camera, zoom, handle drags),
//! viewport/common/transformhandle.d and viewport/model/package.d (gizmos, part menu).
//!
//! World space is the render space of the session (y down, 1 unit = 1 pixel at zoom 1). The camera is
//! stored as the world point at the middle of the image (`centre`, D's `-camera.position`) and a zoom in
//! points per world unit. All math is in plain functions on `[f64; 2]` so it can be tested with literals.
//!
//! The viewport never mutates the document while the pointer is down. A handle drag is previewed with a
//! ghost outline and becomes exactly one `node.transform` request when the button is released.
//!
//! Geometry for picking and the Part overlay comes from `scene.geometry` (painter order, no textures);
//! node matrices and deformation offsets from `scene.evaluate`. "Mirror View" lives in [`Camera`], so the
//! picture, the overlays, the handles and the mesh tools all map the pointer the same way. The toolbar
//! row (`ViewportState::toolbar`) holds Mirror View, Enable physics, Reset physics, Reset parameters, Onion Slice and Trajectories. The view
//! tabs (`ViewportState::view_tabs`) keep one camera per view of `view.*`.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const ZOOM_MIN: f64 = 0.05;
pub const ZOOM_MAX: f64 = 12.0;
/// Edge of the square move, rotate and scale handles (`HandleSize` in base.d).
const HANDLE_SIZE: f64 = 24.0;
const MAX_RENDER_SIDE: f64 = 768.0;
const MIN_VIEW_SIDE: f64 = 16.0;
/// egui reports one wheel notch as this many points; D's `io.MouseWheel` counts notches.
const WHEEL_NOTCH_POINTS: f32 = 50.0;
const ORIENTATION_LENGTH: f64 = 32.0;
const MAX_PHYSICS_DT: f64 = 0.1;
/// Node types `node.transform` accepts (`ensure_transformable_node` in nodes.rs).
const TRANSFORMABLE: [&str; 13] = [
    "Node", "Root", "Part", "Composite", "MeshGroup", "DynamicComposite", "Camera", "Mask", "PathDeformer",
    "GridDeformer", "DepthBone", "DepthRigRoot", "SimplePhysics",
];

type V2 = [f64; 2];
type Mat2 = [[f64; 2]; 2];
pub type Mat4 = [[f64; 4]; 4];

// ------------------------------------------------------------------------------------------ camera

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// World point shown in the middle of the image.
    pub centre: V2,
    /// Points per world unit.
    pub zoom: f64,
    /// "Mirror View": the picture and every pointer mapping are flipped left to right. View-only: the
    /// rendered image is the same, so [`Camera::unmirrored`] is what decides whether to render again.
    pub mirror: bool,
}

impl Camera {
    /// `incViewportReset`: position (0, 0), zoom 1.
    pub const HOME: Camera = Camera { centre: [0.0, 0.0], zoom: 1.0, mirror: false };

    /// +1, or -1 when the view is mirrored: the factor of the x axis between world and screen.
    fn flip(&self) -> f64 {
        if self.mirror { -1.0 } else { 1.0 }
    }

    pub fn unmirrored(self) -> Camera {
        Camera { mirror: false, ..self }
    }

    /// World rectangle `[x0, y0, x1, y1]` shown by an image of `size` points.
    pub fn view_bounds(&self, size: V2) -> [f64; 4] {
        let (half_w, half_h) = (size[0] / (2.0 * self.zoom), size[1] / (2.0 * self.zoom));
        [self.centre[0] - half_w, self.centre[1] - half_h, self.centre[0] + half_w, self.centre[1] + half_h]
    }

    /// World to image-relative points (origin at the image's top-left corner).
    pub fn to_screen(&self, size: V2, world: V2) -> V2 {
        [
            (world[0] - self.centre[0]) * self.zoom * self.flip() + size[0] / 2.0,
            (world[1] - self.centre[1]) * self.zoom + size[1] / 2.0,
        ]
    }

    pub fn to_world(&self, size: V2, screen: V2) -> V2 {
        [
            (screen[0] - size[0] / 2.0) / (self.zoom * self.flip()) + self.centre[0],
            (screen[1] - size[1] / 2.0) / self.zoom + self.centre[1],
        ]
    }

    /// `incViewportZoomToMouse` / `incViewportZoomToCenter`: `zoom += notches * speed / 50 * zoom`,
    /// clamped to the D limits. With a cursor the world point under it stays under it.
    pub fn zoom_step(&mut self, size: V2, cursor: Option<V2>, notches: f64, speed: f64) {
        let next = (self.zoom + notches * speed / 50.0 * self.zoom).clamp(ZOOM_MIN, ZOOM_MAX);
        if !next.is_finite() {
            return;
        }
        let anchor = cursor.map(|cursor| (cursor, self.to_world(size, cursor)));
        self.zoom = next;
        if let Some((cursor, world)) = anchor {
            self.centre = [
                world[0] - (cursor[0] - size[0] / 2.0) / (next * self.flip()),
                world[1] - (cursor[1] - size[1] / 2.0) / next,
            ];
        }
    }

    /// Right-drag pan: the picture follows the pointer.
    pub fn pan(&mut self, screen_delta: V2) {
        self.centre[0] -= screen_delta[0] / (self.zoom * self.flip());
        self.centre[1] -= screen_delta[1] / self.zoom;
    }
}

/// `FitViewportToModelCommand`: centre on the bounds and zoom so they fill the image.
pub fn fit_camera(bounds: [f64; 4], size: V2) -> Option<Camera> {
    let (width, height) = (bounds[2] - bounds[0], bounds[3] - bounds[1]);
    if bounds.iter().any(|value| !value.is_finite()) || width <= 0.0 || height <= 0.0 {
        return None;
    }
    Some(Camera {
        centre: [(bounds[0] + bounds[2]) / 2.0, (bounds[1] + bounds[3]) / 2.0],
        zoom: (size[0] / width).min(size[1] / height).clamp(ZOOM_MIN, ZOOM_MAX),
        mirror: false,
    })
}

/// Grid lines closer together than this many points are not drawn.
pub const GRID_MIN_GAP: f64 = 4.0;
/// The grid spacing the controls and `editor_grid_set` accept, in world units.
pub const GRID_SPACING_MIN: f64 = 1.0;
pub const GRID_SPACING_MAX: f64 = 1e6;

/// The canvas grid (Cubism "Show Grid", issue #204): lines every `spacing` world units through the world
/// origin, over the picture. View-only, like the background colour, and not saved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub show: bool,
    pub spacing: f64,
    /// Unmultiplied RGBA.
    pub colour: [u8; 4],
}

impl Grid {
    pub const DEFAULT: Grid = Grid { show: false, spacing: 100.0, colour: [128, 128, 128, 128] };
}

/// The Solo view (Cubism "Solo", issue #202): the selected nodes are drawn in front of the rest of the
/// picture, which `colour` (unmultiplied RGBA, alpha the opacity) covers. View-only, and not saved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Solo {
    pub show: bool,
    pub colour: [u8; 4],
}

impl Solo {
    /// The vendor manual calls the inactive area greyed out and gives no value; this grey is a guess.
    pub const DEFAULT: Solo = Solo { show: false, colour: [128, 128, 128, 192] };
}

/// World x of the vertical and world y of the horizontal grid lines that an image of `size` points shows,
/// or none when the lines would be closer together than [`GRID_MIN_GAP`] points.
pub fn grid_lines(camera: &Camera, size: V2, spacing: f64) -> (Vec<f64>, Vec<f64>) {
    if !(spacing.is_finite() && spacing > 0.0) || spacing * camera.zoom < GRID_MIN_GAP {
        return (Vec::new(), Vec::new());
    }
    let bounds = camera.view_bounds(size);
    let axis = |low: f64, high: f64| -> Vec<f64> {
        ((low / spacing).ceil() as i64..=(high / spacing).floor() as i64).map(|k| k as f64 * spacing).collect()
    };
    (axis(bounds[0], bounds[2]), axis(bounds[1], bounds[3]))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuideAxis {
    /// A line along x at `position` on y.
    Horizontal,
    /// A line along y at `position` on x.
    Vertical,
}

impl GuideAxis {
    pub fn id(self) -> &'static str {
        match self {
            GuideAxis::Horizontal => "horizontal",
            GuideAxis::Vertical => "vertical",
        }
    }
}

/// One guide line of the modeling view (Cubism "Guide Line", issue #203), in world units.
#[derive(Clone, Debug, PartialEq)]
pub struct Guide {
    pub axis: GuideAxis,
    pub position: f64,
    /// Unmultiplied RGBA.
    pub colour: [u8; 4],
    pub memo: String,
}

/// The guide lines of the view. View-only, like the grid, and not saved.
#[derive(Clone, Debug, PartialEq)]
pub struct Guides {
    pub show: bool,
    /// Moving vertices with the Point tool adhere to a guide near them.
    pub snap: bool,
    /// The colour a new guide gets.
    pub colour: [u8; 4],
    pub lines: Vec<Guide>,
}

/// A guide a moving vertex adheres to is drawn in this colour.
const GUIDE_ATTACHED: Color32 = Color32::from_rgb(0, 230, 0);

/// `[r, g, b, a]` of a checked request.
fn rgba_of(value: &Value) -> Option<[u8; 4]> {
    let channels = value.as_array()?.iter().map(|c| c.as_u64().and_then(|c| u8::try_from(c).ok()));
    channels.collect::<Option<Vec<u8>>>()?.try_into().ok()
}

impl Guides {
    pub fn new() -> Self {
        Self { show: true, snap: false, colour: [0, 200, 255, 255], lines: Vec::new() }
    }

    /// The `guides` key of the editor state, which is also what `editor_guides_set` takes.
    pub fn state(&self) -> Value {
        let lines: Vec<Value> = self
            .lines
            .iter()
            .map(|guide| {
                json!({"axis": guide.axis.id(), "position": guide.position, "colour": guide.colour, "memo": guide.memo})
            })
            .collect();
        json!({"show": self.show, "snap": self.snap, "colour": self.colour, "lines": lines})
    }

    /// Applies an `editor.guides.set` call that `mcp::editor_request` has checked. `lines` replaces every guide;
    /// a guide without a colour gets the default colour, the new one when the call sets it.
    pub fn apply(&mut self, call: &Value) {
        if let Some(show) = call["show"].as_bool() {
            self.show = show;
        }
        if let Some(snap) = call["snap"].as_bool() {
            self.snap = snap;
        }
        if let Some(colour) = rgba_of(&call["colour"]) {
            self.colour = colour;
        }
        if let Some(lines) = call["lines"].as_array() {
            self.lines = lines
                .iter()
                .map(|line| Guide {
                    axis: if line["axis"] == "vertical" { GuideAxis::Vertical } else { GuideAxis::Horizontal },
                    position: line["position"].as_f64().unwrap_or(0.0),
                    colour: rgba_of(&line["colour"]).unwrap_or(self.colour),
                    memo: line["memo"].as_str().unwrap_or_default().to_owned(),
                })
                .collect();
        }
    }
}

/// The most guides a view holds, as `editor_guides_set` allows.
pub const MAX_GUIDES: usize = 256;

impl Guides {
    /// A guide of `axis` through the world point `at`, in the default colour. Nothing is added past [`MAX_GUIDES`].
    pub fn add(&mut self, axis: GuideAxis, at: V2) {
        if self.lines.len() < MAX_GUIDES {
            let position = match axis {
                GuideAxis::Horizontal => at[1],
                GuideAxis::Vertical => at[0],
            };
            self.lines.push(Guide { axis, position, colour: self.colour, memo: String::new() });
        }
    }

    /// The Guide Settings window (Cubism "Guide Line Settings"): both switches, the default colour, guides at
    /// the origin or at `points` (the selected vertices of the mesh tools, in world units), and each guide's
    /// type, position, colour and memo.
    pub fn window(&mut self, ctx: &egui::Context, open: &mut bool, points: &[V2]) {
        egui::Window::new("Guide Settings").open(open).show(ctx, |ui| self.settings(ui, points));
    }

    fn settings(&mut self, ui: &mut egui::Ui, points: &[V2]) {
        ui.checkbox(&mut self.show, "Show guide lines");
        ui.checkbox(&mut self.snap, "Adhere moving vertices to guide lines")
            .on_hover_text("Vertices the Point tool moves stick to a guide within 8 points");
        ui.horizontal(|ui| {
            let label = ui.label("Default colour");
            ui.color_edit_button_srgba_unmultiplied(&mut self.colour).labelled_by(label.id);
            if ui.button("Apply to every guide").clicked() {
                let colour = self.colour;
                self.lines.iter_mut().for_each(|guide| guide.colour = colour);
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Add at the origin").on_hover_text("A horizontal and a vertical guide through (0, 0)").clicked() {
                self.add(GuideAxis::Horizontal, [0.0, 0.0]);
                self.add(GuideAxis::Vertical, [0.0, 0.0]);
            }
            let button = ui.add_enabled(!points.is_empty(), egui::Button::new("Add at the selected vertices"));
            if button.on_hover_text("A horizontal and a vertical guide through each selected vertex").clicked() {
                for point in points {
                    self.add(GuideAxis::Horizontal, *point);
                    self.add(GuideAxis::Vertical, *point);
                }
            }
        });
        ui.separator();
        let mut removed = None;
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            for (index, guide) in self.lines.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    let row = ui.label(format!("Guide {}", index + 1));
                    ui.selectable_value(&mut guide.axis, GuideAxis::Horizontal, "Horizontal");
                    ui.selectable_value(&mut guide.axis, GuideAxis::Vertical, "Vertical");
                    ui.add(egui::DragValue::new(&mut guide.position).range(-1e9..=1e9).speed(1.0)).labelled_by(row.id);
                    ui.color_edit_button_srgba_unmultiplied(&mut guide.colour).labelled_by(row.id);
                    let memo = egui::TextEdit::singleline(&mut guide.memo).hint_text("Memo").char_limit(256);
                    ui.add(memo.desired_width(120.0)).labelled_by(row.id);
                    if ui.button("Delete").clicked() {
                        removed = Some(index);
                    }
                });
            }
        });
        if let Some(index) = removed {
            self.lines.remove(index);
        }
    }
}

impl Default for Guides {
    fn default() -> Self {
        Self::new()
    }
}

/// `incFocusCamera`: centre on the node bounds at 90 % of the largest fit, within 0.1..2.5.
pub fn focus_camera(bounds: [f64; 4], size: V2) -> Option<Camera> {
    if bounds.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let largest = (bounds[2] - bounds[0]).max(bounds[3] - bounds[1]);
    let zoom = if largest > 0.0 { (size[0].min(size[1]) / largest * 0.9).clamp(0.1, 2.5) } else { 2.5 };
    Some(Camera { centre: [(bounds[0] + bounds[2]) / 2.0, (bounds[1] + bounds[3]) / 2.0], zoom, mirror: false })
}

/// Pixel size of the preview image for a view of `size` points. The picture is stretched to the view,
/// so the cap only costs sharpness, never alignment.
pub fn render_size(size: V2) -> (u32, u32) {
    let scale = (MAX_RENDER_SIDE / size[0].max(size[1])).min(1.0);
    ((size[0] * scale).round().max(1.0) as u32, (size[1] * scale).round().max(1.0) as u32)
}

// --------------------------------------------------------------------------------------- scene data

/// One drawn Part in world space: evaluated vertices and triangles from `scene.geometry`.
#[derive(Clone, Debug, PartialEq)]
pub struct PartGeom {
    pub id: u32,
    pub verts: Vec<V2>,
    pub tris: Vec<[usize; 3]>,
    /// The node or one above it is locked: a click in the viewport does not pick this Part.
    pub locked: bool,
}

impl PartGeom {
    pub fn bounds(&self) -> Option<[f64; 4]> {
        union_bounds(self.verts.iter().map(|v| [v[0], v[1], v[0], v[1]]))
    }

    fn area(&self) -> f64 {
        self.bounds().map_or(0.0, |b| (b[2] - b[0]) * (b[3] - b[1]))
    }
}

fn union_bounds(boxes: impl Iterator<Item = [f64; 4]>) -> Option<[f64; 4]> {
    boxes.reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])])
}

/// Parts of a `scene.geometry` result in painter order (first drawn first). The session already lists a
/// composite's Parts at the composite's place, so there is nothing to walk.
pub fn parse_geometry(result: &Value) -> Result<Vec<PartGeom>, String> {
    result["parts"].as_array().ok_or("scene.geometry has no parts")?.iter().map(parse_part).collect()
}

fn parse_part(part: &Value) -> Result<PartGeom, String> {
    let id = part["id"].as_u64().and_then(|id| u32::try_from(id).ok()).ok_or("part id must be a u32")?;
    let verts = part["vertices"]
        .as_array()
        .ok_or("part vertices must be an array")?
        .iter()
        .map(|v| Some([v[0].as_f64()?, v[1].as_f64()?]))
        .collect::<Option<Vec<V2>>>()
        .ok_or("part vertex must be two numbers")?;
    let indices = part["indices"]
        .as_array()
        .ok_or("part indices must be an array")?
        .iter()
        .map(|i| i.as_u64().map(|i| i as usize).filter(|i| *i < verts.len()))
        .collect::<Option<Vec<usize>>>()
        .ok_or("part index is out of range")?;
    let tris = indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect();
    Ok(PartGeom { id, verts, tris, locked: part["locked"] == true })
}

/// Per-node deformation offsets of a `scene.evaluate` result. Nodes that no deformation touched are absent.
pub fn parse_local_offsets(result: &Value) -> Result<BTreeMap<u32, Vec<V2>>, String> {
    let Some(object) = result["localOffsets"].as_object() else { return Ok(BTreeMap::new()) };
    object
        .iter()
        .map(|(key, value)| {
            let id = key.parse::<u32>().map_err(|_| format!("bad node id {key}"))?;
            let offsets = value
                .as_array()
                .and_then(|list| list.iter().map(|p| Some([p[0].as_f64()?, p[1].as_f64()?])).collect::<Option<Vec<V2>>>())
                .ok_or_else(|| format!("bad offsets for node {id}"))?;
            Ok((id, offsets))
        })
        .collect()
}

pub fn parse_world_matrices(result: &Value) -> Result<BTreeMap<u32, Mat4>, String> {
    let object = result["worldMatrices"].as_object().ok_or("scene.evaluate has no worldMatrices")?;
    object
        .iter()
        .map(|(key, value)| {
            let id = key.parse::<u32>().map_err(|_| format!("bad node id {key}"))?;
            let mut matrix = [[0.0; 4]; 4];
            for (r, row) in matrix.iter_mut().enumerate() {
                for (c, cell) in row.iter_mut().enumerate() {
                    *cell = value[r][c].as_f64().ok_or_else(|| format!("bad matrix for node {id}"))?;
                }
            }
            Ok((id, matrix))
        })
        .collect()
}

// ---------------------------------------------------------------------------------------- picking

/// Topmost Part that is not locked and whose evaluated mesh covers `point`: a click goes through a locked Part
/// to the one below it.
pub fn hit_test(parts: &[PartGeom], point: V2) -> Option<u32> {
    parts.iter().rev().find(|part| !part.locked && covers(part, point)).map(|part| part.id)
}

/// In the Solo view the Parts under the inactive colour cannot be picked, as locked ones cannot.
fn lock_inactive(parts: &mut [PartGeom], solo: &Option<BTreeSet<u64>>, show: bool) {
    if let Some(drawn) = solo.as_ref().filter(|_| show) {
        for part in parts {
            part.locked |= !drawn.contains(&u64::from(part.id));
        }
    }
}

/// Sets `locked` of every Part from the tree: a node is locked when it or a node above it stores the lock. The
/// cached geometry can be older than the last lock, the tree is not.
fn mark_locked(parts: &mut [PartGeom], nodes: &Value) {
    fn walk(node: &Value, inherited: bool, out: &mut BTreeSet<u64>) {
        let locked = inherited || node["locked"] == true;
        if locked {
            out.extend(node["uuid"].as_u64());
        }
        for child in node["children"].as_array().into_iter().flatten() {
            walk(child, locked, out);
        }
    }
    let mut locked = BTreeSet::new();
    walk(nodes, false, &mut locked);
    for part in parts {
        part.locked = locked.contains(&u64::from(part.id));
    }
}

fn covers(part: &PartGeom, point: V2) -> bool {
    part.tris.iter().any(|t| {
        soul2d_core::geometry::point_in_triangle(point, [part.verts[t[0]], part.verts[t[1]], part.verts[t[2]]])
            .unwrap_or(false)
    })
}

/// The right-click list of `ModelViewport.menuOpening`: every unselected, unlocked Part whose bounds contain the
/// point, front to back (ascending z-sort) or smallest first.
pub fn parts_under(parts: &[PartGeom], point: V2, selected: Option<u32>, by_size: bool) -> Vec<u32> {
    let mut found: Vec<&PartGeom> = parts
        .iter()
        .rev()
        .filter(|part| Some(part.id) != selected && !part.locked)
        .filter(|part| {
            part.bounds().is_some_and(|b| point[0] >= b[0] && point[0] <= b[2] && point[1] >= b[1] && point[1] <= b[3])
        })
        .collect();
    if by_size {
        found.sort_by(|a, b| a.area().total_cmp(&b.area()));
    }
    found.into_iter().map(|part| part.id).collect()
}

// ----------------------------------------------------------------------------------- node snapshot

/// What the viewport needs from the model tree about the selected node.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeInfo {
    pub id: u32,
    pub kind: String,
    pub parent: Option<u32>,
    pub root: u32,
    pub lock_to_root: bool,
    pub trans: [f64; 3],
    pub rot: [f64; 3],
    pub scale: V2,
    /// The node and all its descendants.
    pub subtree: Vec<u32>,
}

pub fn node_info(nodes: &Value, id: u64) -> Option<NodeInfo> {
    let root = u32::try_from(nodes["uuid"].as_u64()?).ok()?;
    let (node, parent) = locate(nodes, id, None)?;
    let numbers = |value: &Value, count: usize| -> Option<Vec<f64>> {
        let list = value.as_array().filter(|list| list.len() == count)?;
        list.iter().map(Value::as_f64).collect()
    };
    let transform = &node["transform"];
    let trans = numbers(&transform["trans"], 3).unwrap_or_else(|| vec![0.0; 3]);
    let rot = numbers(&transform["rot"], 3).unwrap_or_else(|| vec![0.0; 3]);
    let scale = numbers(&transform["scale"], 2).unwrap_or_else(|| vec![1.0; 2]);
    let mut subtree = Vec::new();
    collect_ids(node, &mut subtree);
    Some(NodeInfo {
        id: u32::try_from(id).ok()?,
        kind: node["type"].as_str().unwrap_or("Node").to_owned(),
        parent: parent.and_then(|p| u32::try_from(p).ok()),
        root,
        lock_to_root: node["lockToRoot"].as_bool().unwrap_or(false),
        trans: [trans[0], trans[1], trans[2]],
        rot: [rot[0], rot[1], rot[2]],
        scale: [scale[0], scale[1]],
        subtree,
    })
}

fn locate(node: &Value, id: u64, parent: Option<u64>) -> Option<(&Value, Option<u64>)> {
    let uuid = node["uuid"].as_u64();
    if uuid == Some(id) {
        return Some((node, parent));
    }
    node["children"].as_array()?.iter().find_map(|child| locate(child, id, uuid))
}

fn collect_ids(node: &Value, out: &mut Vec<u32>) {
    if let Some(id) = node["uuid"].as_u64().and_then(|id| u32::try_from(id).ok()) {
        out.push(id);
    }
    for child in node["children"].as_array().into_iter().flatten() {
        collect_ids(child, out);
    }
}

fn node_name(nodes: &Value, id: u32) -> String {
    locate(nodes, u64::from(id), None)
        .and_then(|(node, _)| node["name"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "unnamed".to_owned())
}

// ------------------------------------------------------------------------------ transform handle

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Move,
    Rotate,
    Scale,
}

/// The three handle squares around a selection box given in image-relative points `[x0, y0, x1, y1]`:
/// move above-left, rotate above-right, scale below-left (all outside the box, as in D).
pub fn handle_rects(bounds: [f64; 4]) -> [(Tool, [f64; 4]); 3] {
    [
        (Tool::Move, [bounds[0] - HANDLE_SIZE, bounds[1] - HANDLE_SIZE, bounds[0], bounds[1]]),
        (Tool::Rotate, [bounds[2], bounds[1] - HANDLE_SIZE, bounds[2] + HANDLE_SIZE, bounds[1]]),
        (Tool::Scale, [bounds[0] - HANDLE_SIZE, bounds[3], bounds[0], bounds[3] + HANDLE_SIZE]),
    ]
}

pub fn handle_at(bounds: [f64; 4], point: V2) -> Option<Tool> {
    handle_rects(bounds)
        .into_iter()
        .find(|(_, r)| point[0] >= r[0] && point[0] <= r[2] && point[1] >= r[1] && point[1] <= r[3])
        .map(|(tool, _)| tool)
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeValues {
    pub trans: [f64; 3],
    pub rot: [f64; 3],
    pub scale: V2,
}

/// Locked axis of a shift-drag (`LockedOrientation`): the named axis keeps its start value.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Lock {
    None,
    KeepX,
    KeepY,
}

/// A world-space affine map used to draw the ghost: `pivot + m * (p - pivot) + t`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    m: Mat2,
    pivot: V2,
    t: V2,
}

impl Affine {
    const IDENTITY: Affine = Affine { m: [[1.0, 0.0], [0.0, 1.0]], pivot: [0.0, 0.0], t: [0.0, 0.0] };

    pub fn apply(&self, p: V2) -> V2 {
        let d = [p[0] - self.pivot[0], p[1] - self.pivot[1]];
        [
            self.pivot[0] + self.m[0][0] * d[0] + self.m[0][1] * d[1] + self.t[0],
            self.pivot[1] + self.m[1][0] * d[0] + self.m[1][1] * d[1] + self.t[1],
        ]
    }
}

/// The selected node as the handle sees it: where it is and how its parent maps local to world.
#[derive(Clone, Debug)]
struct SelView {
    info: NodeInfo,
    /// World position of the node origin.
    origin: V2,
    /// World bounds of the node and its descendants (a point at the origin when nothing is drawn).
    bounds: [f64; 4],
    /// Unit vectors of the node's own x and y axes in world space.
    axes: (V2, V2),
    parent_lin: Mat2,
}

fn linear(m: &Mat4) -> Mat2 {
    [[m[0][0], m[0][1]], [m[1][0], m[1][1]]]
}

fn invert(m: Mat2) -> Option<Mat2> {
    let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    (det.abs() > 1e-12 && det.is_finite())
        .then(|| [[m[1][1] / det, -m[0][1] / det], [-m[1][0] / det, m[0][0] / det]])
}

fn apply(m: Mat2, v: V2) -> V2 {
    [m[0][0] * v[0] + m[0][1] * v[1], m[1][0] * v[0] + m[1][1] * v[1]]
}

fn unit(v: V2, fallback: V2) -> V2 {
    let length = v[0].hypot(v[1]);
    if length > 1e-12 { [v[0] / length, v[1] / length] } else { fallback }
}

fn sel_view(
    info: NodeInfo,
    matrices: &BTreeMap<u32, Mat4>,
    parts: Option<&[PartGeom]>,
) -> Option<SelView> {
    let world = matrices.get(&info.id)?;
    let origin = [world[0][3], world[1][3]];
    let bounds = parts
        .and_then(|parts| {
            union_bounds(parts.iter().filter(|p| info.subtree.contains(&p.id)).filter_map(PartGeom::bounds))
        })
        .unwrap_or([origin[0], origin[1], origin[0], origin[1]]);
    // A node locked to the root is placed by the root's matrix, not its parent's.
    let parent = if info.lock_to_root && info.id != info.root { Some(info.root) } else { info.parent };
    let parent_lin = parent.and_then(|id| matrices.get(&id)).map_or([[1.0, 0.0], [0.0, 1.0]], linear);
    let axes = (unit([world[0][0], world[1][0]], [1.0, 0.0]), unit([world[0][1], world[1][1]], [0.0, 1.0]));
    Some(SelView { info, origin, bounds, axes, parent_lin })
}

/// One handle gesture. All reads and writes are local values of the node; world motion is mapped
/// through the parent's linear part so the node follows the pointer under any parent rotation/scale.
#[derive(Clone, Debug)]
pub struct HandleDrag {
    tool: Tool,
    id: u32,
    start: NodeValues,
    parent_lin: Mat2,
    inverse_parent: Mat2,
    start_pointer: V2,
    last_pointer: V2,
    /// Accumulated world rotation of the pointer around the origin (radians).
    angle: f64,
    origin: V2,
    centre: V2,
    axes: (V2, V2),
    lock: Lock,
    pub values: NodeValues,
    pub ghost: Affine,
}

fn begin_drag(tool: Tool, sel: &SelView, pointer: V2) -> Option<HandleDrag> {
    let start = NodeValues { trans: sel.info.trans, rot: sel.info.rot, scale: sel.info.scale };
    Some(HandleDrag {
        tool,
        id: sel.info.id,
        start,
        parent_lin: sel.parent_lin,
        inverse_parent: invert(sel.parent_lin)?,
        start_pointer: pointer,
        last_pointer: pointer,
        angle: 0.0,
        origin: sel.origin,
        centre: [(sel.bounds[0] + sel.bounds[2]) / 2.0, (sel.bounds[1] + sel.bounds[3]) / 2.0],
        axes: sel.axes,
        lock: Lock::None,
        values: start,
        ghost: Affine::IDENTITY,
    })
}

fn snap(value: f64, step: f64) -> f64 {
    (value / step).round() * step
}

/// Advance a gesture to a new pointer position (world space). Mirrors the move/rotate/scale blocks of
/// `incViewportTransformHandle`: Ctrl snaps, Shift locks an axis, Shift+Alt scales uniformly.
pub fn drag_update(drag: &mut HandleDrag, pointer: V2, mods: Mods) {
    match drag.tool {
        Tool::Move => {
            let world = [pointer[0] - drag.start_pointer[0], pointer[1] - drag.start_pointer[1]];
            let local = apply(drag.inverse_parent, world);
            if !mods.shift {
                drag.lock = Lock::None;
            } else if drag.lock == Lock::None {
                drag.lock = if local[0].abs() > local[1].abs() { Lock::KeepY } else { Lock::KeepX };
            }
            let mut x = drag.start.trans[0] + local[0];
            let mut y = drag.start.trans[1] + local[1];
            if mods.ctrl {
                x = snap(x, 5.0);
                y = snap(y, 5.0);
            }
            match drag.lock {
                Lock::KeepY => y = drag.start.trans[1],
                Lock::KeepX => x = drag.start.trans[0],
                Lock::None => {}
            }
            drag.values.trans = [x, y, drag.start.trans[2]];
            let moved = apply(drag.parent_lin, [x - drag.start.trans[0], y - drag.start.trans[1]]);
            drag.ghost = Affine { t: moved, ..Affine::IDENTITY };
        }
        Tool::Rotate => {
            let angle_of = |p: V2| (p[1] - drag.origin[1]).atan2(p[0] - drag.origin[0]);
            let mut step = angle_of(pointer) - angle_of(drag.last_pointer);
            while step > std::f64::consts::PI {
                step -= std::f64::consts::TAU;
            }
            while step < -std::f64::consts::PI {
                step += std::f64::consts::TAU;
            }
            drag.angle += step;
            drag.last_pointer = pointer;
            // A mirroring parent turns a world rotation into the opposite local one.
            let det = drag.parent_lin[0][0] * drag.parent_lin[1][1] - drag.parent_lin[0][1] * drag.parent_lin[1][0];
            let sign = if det < 0.0 { -1.0 } else { 1.0 };
            let mut z = drag.start.rot[2] + sign * drag.angle;
            if mods.ctrl {
                z = snap(z.to_degrees(), 5.0).to_radians();
            }
            drag.values.rot = [drag.start.rot[0], drag.start.rot[1], z];
            let world = sign * (z - drag.start.rot[2]);
            let (sin, cos) = world.sin_cos();
            drag.ghost = Affine { m: [[cos, -sin], [sin, cos]], pivot: drag.origin, t: [0.0, 0.0] };
        }
        Tool::Scale => {
            let (u, v) = drag.axes;
            let coords = |p: V2| {
                let d = [p[0] - drag.centre[0], p[1] - drag.centre[1]];
                [d[0] * u[0] + d[1] * u[1], d[0] * v[0] + d[1] * v[1]]
            };
            let (from, to) = (coords(drag.start_pointer), coords(pointer));
            let ratio = |a: f64, b: f64| if a == 0.0 { 1.0 } else { b / a };
            let (mut rx, mut ry) = (ratio(from[0], to[0]), ratio(from[1], to[1]));
            if mods.shift && mods.alt {
                let from_len = from[0].hypot(from[1]);
                let uniform = if from_len == 0.0 { 1.0 } else { to[0].hypot(to[1]) / from_len };
                (rx, ry) = (uniform, uniform);
            } else if mods.shift {
                if drag.lock == Lock::None {
                    drag.lock = if rx.abs() > ry.abs() { Lock::KeepY } else { Lock::KeepX };
                }
                match drag.lock {
                    Lock::KeepY => ry = 1.0,
                    Lock::KeepX => rx = 1.0,
                    Lock::None => {}
                }
            } else {
                drag.lock = Lock::None;
            }
            let mut sx = drag.start.scale[0] * rx;
            let mut sy = drag.start.scale[1] * ry;
            if mods.ctrl {
                sx = (sx * 10.0).floor() / 10.0;
                sy = (sy * 10.0).floor() / 10.0;
            }
            drag.values.scale = [sx, sy];
            let applied = |new: f64, old: f64| if old == 0.0 { 1.0 } else { new / old };
            let (ax, ay) = (applied(sx, drag.start.scale[0]), applied(sy, drag.start.scale[1]));
            drag.ghost = Affine {
                m: [
                    [ax * u[0] * u[0] + ay * v[0] * v[0], ax * u[0] * u[1] + ay * v[0] * v[1]],
                    [ax * u[0] * u[1] + ay * v[0] * v[1], ax * u[1] * u[1] + ay * v[1] * v[1]],
                ],
                pivot: drag.centre,
                t: [0.0, 0.0],
            };
        }
    }
}

/// Drops the matrix-inverse noise (10.000000000000002) from a value that is stored in the document.
fn tidy(value: f64) -> f64 {
    (value * 1e9).round() / 1e9
}

/// The single request that commits a gesture, or `None` when nothing changed.
pub fn drag_request(drag: &HandleDrag) -> Option<Value> {
    let finite = |values: &[f64]| values.iter().all(|value| value.is_finite());
    match drag.tool {
        Tool::Move if drag.values.trans != drag.start.trans && finite(&drag.values.trans) => {
            let t = drag.values.trans;
            Some(json!({"op": "node.transform", "id": drag.id, "translation": [tidy(t[0]), tidy(t[1]), t[2]]}))
        }
        Tool::Rotate if drag.values.rot != drag.start.rot && finite(&drag.values.rot) => {
            let r = drag.values.rot;
            Some(json!({"op": "node.transform", "id": drag.id, "rotation": [r[0], r[1], tidy(r[2])]}))
        }
        Tool::Scale if drag.values.scale != drag.start.scale && finite(&drag.values.scale) => {
            let s = drag.values.scale;
            Some(json!({"op": "node.transform", "id": drag.id, "scale": [tidy(s[0]), tidy(s[1])]}))
        }
        _ => None,
    }
}

// -------------------------------------------------------------------------------------------- curves

/// The curve class the engine builds for a PathDeformer from its `curve_type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveKind {
    Spline,
    Bezier,
}

impl CurveKind {
    pub fn of_node(node: &Value) -> Self {
        if node["curve_type"].as_str() == Some("Bezier") { CurveKind::Bezier } else { CurveKind::Spline }
    }
}

/// Point of the curve through `points` at `t` in 0..=1: a Catmull-Rom spline for `Spline`, one Bezier
/// curve of degree `len - 1` for `Bezier`. Port of `spline_sample` / `bezier_sample` in scene_deform.rs
/// (private there), with the segment index clamped so `t = 1` ends on the last point instead of D's
/// penultimate one. Fewer than two points have no curve: the single point, or the origin.
pub fn curve_point(kind: CurveKind, points: &[V2], t: f64) -> V2 {
    match points {
        [] => return [0.0, 0.0],
        [only] => return *only,
        _ => {}
    }
    let t = t.clamp(0.0, 1.0);
    match kind {
        CurveKind::Bezier => {
            let degree = points.len() - 1;
            let mut point = [0.0; 2];
            for (index, control) in points.iter().enumerate() {
                let coefficient = binomial(degree, index) * (1.0 - t).powi((degree - index) as i32) * t.powi(index as i32);
                point[0] += coefficient * control[0];
                point[1] += coefficient * control[1];
            }
            point
        }
        CurveKind::Spline if points.len() == 2 => {
            [points[0][0] * (1.0 - t) + points[1][0] * t, points[0][1] * (1.0 - t) + points[1][1] * t]
        }
        CurveKind::Spline => {
            let n = points.len();
            let segment = t * (n - 1) as f64;
            let p1 = (segment as usize).min(n - 2);
            let p0 = p1.saturating_sub(1);
            let p2 = (p1 + 1).min(n - 1);
            let p3 = (p2 + 1).min(n - 1);
            let local = segment - p1 as f64;
            let (local2, local3) = (local * local, local * local * local);
            let mut value = [0.0; 2];
            for axis in 0..2 {
                let a = 2.0 * points[p1][axis];
                let b = points[p2][axis] - points[p0][axis];
                let c = 2.0 * points[p0][axis] - 5.0 * points[p1][axis] + 4.0 * points[p2][axis] - points[p3][axis];
                let d = -points[p0][axis] + 3.0 * points[p1][axis] - 3.0 * points[p2][axis] + points[p3][axis];
                value[axis] = 0.5 * (a + b * local + c * local2 + d * local3);
            }
            value
        }
    }
}

fn binomial(n: usize, k: usize) -> f64 {
    let k = k.min(n - k);
    (0..k).fold(1.0, |result, index| result * (n - index) as f64 / (index + 1) as f64)
}

/// `t` of the curve point nearest to `query`. D runs a Brent local minimiser from the whole `[0, 1]`
/// bracket; this scans 64 samples per span and then narrows the best bracket by golden-section steps,
/// which finds the same point on any curve that does not fold back on itself.
pub fn curve_closest_t(kind: CurveKind, points: &[V2], query: V2) -> f64 {
    if points.len() < 2 {
        return 0.0;
    }
    let distance = |t: f64| {
        let p = curve_point(kind, points, t);
        (p[0] - query[0]).powi(2) + (p[1] - query[1]).powi(2)
    };
    let samples = 64 * (points.len() - 1);
    let step = 1.0 / samples as f64;
    let best = (0..=samples)
        .min_by(|a, b| distance(*a as f64 * step).total_cmp(&distance(*b as f64 * step)))
        .unwrap_or(0);
    let (mut low, mut high) = (((best as f64 - 1.0) * step).max(0.0), ((best as f64 + 1.0) * step).min(1.0));
    let ratio = (5.0_f64.sqrt() - 1.0) / 2.0;
    for _ in 0..40 {
        let (a, b) = (high - ratio * (high - low), low + ratio * (high - low));
        if distance(a) <= distance(b) {
            high = b;
        } else {
            low = a;
        }
    }
    let refined = (low + high) / 2.0;
    if distance(refined) <= distance(best as f64 * step) { refined } else { best as f64 * step }
}

/// The curve as `segments + 1` points.
pub fn curve_polyline(kind: CurveKind, points: &[V2], segments: usize) -> Vec<V2> {
    if points.len() < 2 {
        return Vec::new();
    }
    (0..=segments).map(|i| curve_point(kind, points, i as f64 / segments as f64)).collect()
}

// ------------------------------------------------------------------------------ model-mode overlays

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shade {
    /// The undeformed shape, behind the deformed one.
    Rest,
    Deformed,
    Lattice,
    Mesh,
    Anchor,
}

/// World-space shapes of the selected non-Part node (`ModelLayoutViewport.draw`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Overlay {
    pub lines: Vec<(V2, V2, Shade)>,
    pub points: Vec<V2>,
}

const CURVE_SEGMENTS: usize = 100;

/// What D draws for the selected node in layout mode: a PathDeformer's rest and deformed curve with the
/// deformed control points, a GridDeformer's lattice, a MeshGroup's mesh lines (`show_mesh`), and a
/// SimplePhysics driver's anchor with its rest-pose output line. Deformers are drawn at `rest + offsets`
/// (`offsets` are the node's `scene.evaluate` local offsets; none, or a different count, mean rest).
pub fn node_overlay(node: &Value, world: &Mat4, offsets: &[V2], show_mesh: bool) -> Overlay {
    let to_world = |p: V2| {
        [
            world[0][0] * p[0] + world[0][1] * p[1] + world[0][3],
            world[1][0] * p[0] + world[1][1] * p[1] + world[1][3],
        ]
    };
    use crate::editor_mesh_tools::{edges_of, read_geometry, NodeKind};
    let mut overlay = Overlay::default();
    if node["type"].as_str() == Some("SimplePhysics") {
        // `Driver.drawDebug` draws the anchor and the bob. Only the rest bob (anchor + (0, length)) can be
        // known here: the session does not report the simulated one.
        let local = &node["transform"]["trans"];
        let anchor = match (node["local_only"].as_bool(), local[0].as_f64(), local[1].as_f64()) {
            (Some(true), Some(x), Some(y)) => [x, y],
            _ => to_world([0.0, 0.0]),
        };
        let length = node["length"].as_f64().filter(|length| length.is_finite()).unwrap_or(100.0);
        overlay.lines.push((anchor, [anchor[0], anchor[1] + length], Shade::Anchor));
        overlay.points.push(anchor);
        return overlay;
    }
    let Some(geometry) = read_geometry(node) else { return overlay };
    let shown: Vec<V2> = if offsets.len() == geometry.points.len() {
        geometry.points.iter().zip(offsets).map(|(p, o)| [p[0] + o[0], p[1] + o[1]]).collect()
    } else {
        geometry.points.clone()
    };
    let polyline = |points: &[V2], shade: Shade| -> Vec<(V2, V2, Shade)> {
        points.windows(2).map(|pair| (to_world(pair[0]), to_world(pair[1]), shade)).collect()
    };
    match geometry.kind {
        NodeKind::PathDeformer => {
            let curve = CurveKind::of_node(node);
            if shown != geometry.points {
                overlay.lines.extend(polyline(&curve_polyline(curve, &geometry.points, CURVE_SEGMENTS), Shade::Rest));
            }
            overlay.lines.extend(polyline(&curve_polyline(curve, &shown, CURVE_SEGMENTS), Shade::Deformed));
            overlay.points = shown.iter().map(|p| to_world(*p)).collect();
        }
        NodeKind::GridDeformer | NodeKind::MeshGroup => {
            let (shade, columns) = match geometry.kind {
                NodeKind::GridDeformer => (Shade::Lattice, node["grid_axis_x"].as_array().map_or(1, Vec::len)),
                _ if show_mesh => (Shade::Mesh, 1),
                _ => return overlay,
            };
            for [a, b] in edges_of(&geometry, columns) {
                overlay.lines.push((to_world(shown[a]), to_world(shown[b]), shade));
            }
        }
        NodeKind::Part => {}
    }
    overlay
}

// ------------------------------------------------------------------------------------- depth bones

/// How a bone link is drawn: relative to the selected bone it is its own parent link, a child link, or neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoneLink {
    Plain,
    Parent,
    Child,
}

/// World-space lines and joints of one depth rig (`selected` marks the selected bone's joint).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BoneOverlay {
    pub lines: Vec<(V2, V2, BoneLink)>,
    pub joints: Vec<(V2, bool)>,
}

/// The DepthRigRoot a selected node belongs to (the node itself, or its nearest DepthRigRoot ancestor) and, when
/// the node is a DepthBone, that bone. This is the `root` and `selected` of `rig.overlay`.
pub fn rig_of(nodes: &Value, selected: u64) -> Option<(u32, Option<u32>)> {
    fn walk(node: &Value, id: u64, rig: Option<u32>) -> Option<(u32, Option<u32>)> {
        let this = node["uuid"].as_u64();
        let rig = if node["type"].as_str() == Some("DepthRigRoot") { this.and_then(|u| u32::try_from(u).ok()) } else { rig };
        if this == Some(id) {
            let bone = (node["type"].as_str() == Some("DepthBone")).then(|| u32::try_from(id).ok()).flatten();
            return rig.map(|root| (root, bone));
        }
        node["children"].as_array()?.iter().find_map(|child| walk(child, id, rig))
    }
    walk(nodes, selected, None)
}

/// D drew the posed bone joints and the links between a bone and its parent in the model view. `data` is the
/// answer of `rig.overlay` (points in the rig root's own space); `root_world` is that root's world matrix.
pub fn bone_overlay(data: &Value, root_world: &Mat4) -> BoneOverlay {
    let point = |value: &Value| -> Option<V2> {
        let list = value.as_array()?;
        let (x, y, z) = (list.first()?.as_f64()?, list.get(1)?.as_f64()?, list.get(2).and_then(Value::as_f64).unwrap_or(0.0));
        Some([
            root_world[0][0] * x + root_world[0][1] * y + root_world[0][2] * z + root_world[0][3],
            root_world[1][0] * x + root_world[1][1] * y + root_world[1][2] * z + root_world[1][3],
        ])
    };
    let mut overlay = BoneOverlay::default();
    for (key, link) in [("lines", BoneLink::Plain), ("parentLines", BoneLink::Parent), ("childLines", BoneLink::Child)] {
        for line in data[key].as_array().into_iter().flatten() {
            if let (Some(a), Some(b)) = (point(&line[0]), point(&line[1])) {
                overlay.lines.push((a, b, link));
            }
        }
    }
    for (key, selected) in [("points", false), ("selectedPoints", true)] {
        overlay.joints.extend(data[key].as_array().into_iter().flatten().filter_map(point).map(|p| (p, selected)));
    }
    overlay
}

// ---------------------------------------------------------------------------------- parameter reset

/// `ResetParametersCommand`: one `value.set` per leaf parameter whose current value is not its default.
/// `snapshot` is a session snapshot; groups (`groupUUID`) are walked through their `children`.
pub fn reset_parameter_requests(snapshot: &Value) -> Vec<Value> {
    fn pair(value: &Value) -> Option<[f64; 2]> {
        let list = value.as_array().filter(|list| list.len() == 2)?;
        Some([list[0].as_f64()?, list[1].as_f64()?])
    }
    fn walk(list: &Value, values: &Value, out: &mut Vec<Value>) {
        for item in list.as_array().into_iter().flatten() {
            if item.get("groupUUID").is_some() {
                walk(&item["children"], values, out);
                continue;
            }
            let (Some(id), Some(defaults)) = (item["uuid"].as_u64(), pair(&item["defaults"])) else { continue };
            if pair(&values[id.to_string()]) != Some(defaults) {
                out.push(json!({"op": "value.set", "id": id, "value": defaults}));
            }
        }
    }
    let mut requests = Vec::new();
    walk(&snapshot["model"]["param"], &snapshot["values"], &mut requests);
    requests
}

// ----------------------------------------------------------------------------------------- state

#[derive(Default)]
struct SceneCache {
    matrices: Option<BTreeMap<u32, Mat4>>,
    /// `localOffsets` of the same `scene.evaluate`: deformer points are drawn at `rest + offset`.
    offsets: BTreeMap<u32, Vec<V2>>,
    parts: Option<Vec<PartGeom>>,
    /// The depth rig of the selection: `(root, selected bone)` and its `rig.overlay` answer.
    rig: Option<((u32, Option<u32>), Value)>,
    /// `parts` was read after the last render (it may still be older geometry while playing).
    parts_fresh: bool,
    error: Option<String>,
    /// The `animation.trajectory` answer for `(animation, node, revision)`: one polyline per vertex.
    trajectory: Option<(TrajectoryKey, Vec<Vec<V2>>)>,
}

type TrajectoryKey = (String, u32, u64);

pub struct ViewportState {
    pub camera: Camera,
    pub show_vertices: bool,
    pub show_bounds: bool,
    pub show_orientation: bool,
    /// The mesh tools draw the selected Part's vertices themselves (`editor_mesh_tools`).
    pub suppress_part_overlay: bool,
    pub background: [u8; 3],
    pub grid: Grid,
    pub solo: Solo,
    /// The nodes the preview draws in front while [`Solo::show`] is on: a click selects no other Part then.
    pub solo_drawn: Option<BTreeSet<u64>>,
    pub guides: Guides,
    /// The guides a vertex the Point tool moves adheres to now, by index; drawn in green.
    pub guides_attached: Vec<usize>,
    /// The Guide Settings window is open.
    pub guides_open: bool,
    /// "Enable physics": every frame advances `physics.step` by the frame delta (off by default).
    pub physics: bool,
    /// "Onion Slice": the picture shows the frames around the previewed animation frame faintly.
    pub onion: bool,
    /// "Trajectories": the paths the selected Part's vertices take through the previewed animation.
    pub trajectories: bool,
    /// The parameter bookmark labels of the clip on the timeline, as stored index, label and canvas point.
    pub bookmarks: Vec<(usize, String, V2)>,
    /// The right-click menu offers parameter bookmarks (a clip is on the timeline).
    pub bookmark_menu: bool,
    /// The parameter controllers of the clip on the timeline, placed on the playhead's frame.
    pub controllers: Vec<ViewController>,
    /// The right-click menu can add a parameter controller: the playhead stands still on a whole frame.
    pub controller_add: bool,
    /// The controller whose handle is being dragged.
    controller_drag: Option<String>,
    /// The drawn vertices of the node whose form is edited, at canvas points; empty when no form is edited.
    pub form_vertices: Vec<V2>,
    /// The form vertex being dragged and the canvas point the drag began on.
    form_drag: Option<(usize, V2)>,
    /// The camera of each view of `view.*` that is not shown; the shown view's camera is `camera`.
    cameras: BTreeMap<u64, Camera>,
    shown_view: Option<u64>,
    size: V2,
    rendered: Option<(Camera, V2)>,
    scene: SceneCache,
    drag: Option<HandleDrag>,
    pending_click: Option<V2>,
    menu_point: Option<V2>,
    /// The right-click menu is open, so the Part geometry is kept fresh for it.
    menu_open: bool,
    pub(super) menu_by_size: bool,
}

/// Per-frame facts from the editor.
pub struct Input<'a> {
    pub nodes: &'a Value,
    pub selected: Option<u64>,
    /// Clicking a Part selects it (INX documents).
    pub selectable: bool,
    /// The transform handle is offered (INX document, no armed parameter).
    pub handles: bool,
    pub zoom_to_cursor: bool,
    pub zoom_speed: f64,
    /// The right-click menu opens. The editor keeps it shut while a mesh or depth tool is active, as it kept the
    /// Part list shut before the menu held guides.
    pub menu: bool,
}

#[derive(Debug, Default)]
pub struct Output {
    pub requests: Vec<Value>,
    pub editor_calls: Vec<Value>,
    pub select: Option<u32>,
    pub bookmark: Option<BookmarkPick>,
    pub controller: Option<ControllerPick>,
}

/// A parameter controller as the viewport draws it, at canvas points.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewController {
    pub name: String,
    pub start: V2,
    /// The end of a direction controller.
    pub end: Option<V2>,
}

/// What the user asked of a parameter controller in the viewport.
#[derive(Debug, PartialEq)]
pub enum ControllerPick {
    /// Add a single-point controller at this canvas point.
    Add(V2),
    /// Move the controller to this canvas point: a point controller goes there, a direction controller turns to it.
    Move(String, V2),
    Delete(String),
}

/// A form vertex dragged in the viewport: which vertex, and how far it went in canvas points.
#[derive(Debug, PartialEq)]
pub struct FormPick {
    pub vertex: usize,
    pub delta: V2,
}

/// What the user asked of a parameter bookmark in the viewport.
#[derive(Debug, PartialEq)]
pub enum BookmarkPick {
    /// The parameters of this Part, picked at this canvas point.
    Part(u32, V2),
    /// Show the label at this index.
    Label(usize),
    /// Delete the label at this index.
    Delete(usize),
}

impl Default for ViewportState {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewportState {
    pub fn new() -> Self {
        Self {
            camera: Camera::HOME,
            show_vertices: true,
            show_bounds: true,
            show_orientation: true,
            suppress_part_overlay: false,
            background: [0, 0, 0],
            grid: Grid::DEFAULT,
            solo: Solo::DEFAULT,
            solo_drawn: None,
            guides: Guides::new(),
            guides_attached: Vec::new(),
            guides_open: false,
            physics: false,
            onion: false,
            trajectories: false,
            bookmarks: Vec::new(),
            bookmark_menu: false,
            controllers: Vec::new(),
            controller_add: false,
            controller_drag: None,
            form_vertices: Vec::new(),
            form_drag: None,
            cameras: BTreeMap::new(),
            shown_view: None,
            size: [512.0, 512.0],
            rendered: None,
            scene: SceneCache::default(),
            drag: None,
            pending_click: None,
            menu_point: None,
            menu_open: false,
            menu_by_size: false,
        }
    }

    /// Size of the picture in points (the mesh tools map the pointer with it).
    pub fn size(&self) -> V2 {
        self.size
    }

    /// World matrix of a node from the last `scene.evaluate`, when it has been read.
    pub fn world_matrix(&self, id: u32) -> Option<Mat4> {
        self.scene.matrices.as_ref()?.get(&id).copied()
    }

    /// World rectangle to render for the current camera and image size.
    pub fn view_bounds(&self) -> [f64; 4] {
        self.camera.view_bounds(self.size)
    }

    pub fn render_size(&self) -> (u32, u32) {
        render_size(self.size)
    }

    /// Fit the camera to scene bounds from the `bounds` op.
    pub fn fit(&mut self, bounds: [f64; 4]) {
        if let Some(camera) = fit_camera(bounds, self.size) {
            self.set_view(camera);
        }
    }

    /// Moves the camera but keeps the mirror setting, which is the user's, not the view's.
    pub fn set_view(&mut self, camera: Camera) {
        self.camera = Camera { mirror: self.camera.mirror, ..camera };
    }

    /// "Focus Selected" for any nodes: centre on their combined bounds. The scene is read here rather than
    /// taken from the cache, because the request can follow an edit that no frame has rendered yet.
    pub fn focus_nodes(&mut self, core: &mut dyn FnMut(&Value) -> Value, nodes: &Value, ids: &[u64]) -> Result<(), String> {
        let mut read = |op: &str| {
            let response = core(&json!({"op": op}));
            if response["succeeded"] == true {
                Ok(response["result"].clone())
            } else {
                Err(response["message"].as_str().unwrap_or("the scene could not be read").to_owned())
            }
        };
        let matrices = parse_world_matrices(&read("scene.evaluate")?)?;
        let parts = parse_geometry(&read("scene.geometry")?)?;
        let boxes = ids.iter().filter_map(|id| Some(sel_view(node_info(nodes, *id)?, &matrices, Some(&parts))?.bounds));
        let camera = union_bounds(boxes).and_then(|bounds| focus_camera(bounds, self.size));
        self.set_view(camera.ok_or("the nodes have no place in the evaluated scene")?);
        Ok(())
    }

    /// The picture on screen no longer matches the camera or the image size. Mirroring flips the picture
    /// on screen and needs no new render.
    pub fn needs_render(&self) -> bool {
        self.rendered != Some((self.camera.unmirrored(), self.size))
    }

    pub fn mark_rendered(&mut self) {
        self.rendered = Some((self.camera.unmirrored(), self.size));
    }

    /// The `physics.step` request for this frame while "Enable physics" is on. D steps the drivers by the
    /// frame delta; a long stall (window drag, breakpoint) is cut to [`MAX_PHYSICS_DT`] so one frame
    /// cannot throw the simulation. Asks for a new picture, since the pose moves every frame.
    pub fn physics_request(&mut self, dt: f64) -> Option<Value> {
        if !self.physics || !dt.is_finite() {
            return None;
        }
        self.rendered = None;
        Some(json!({"op": "physics.step", "dt": dt.clamp(0.0, MAX_PHYSICS_DT)}))
    }

    /// Call after every render: the evaluated scene may have changed.
    pub fn invalidate_scene(&mut self) {
        self.scene.matrices = None;
        self.scene.parts_fresh = false;
        self.scene.rig = None;
    }

    /// Reserve the whole free area for the picture.
    pub fn allocate(&mut self, ui: &mut egui::Ui) -> (Rect, egui::Response) {
        let available = ui.available_size();
        let size = egui::vec2(available.x.max(MIN_VIEW_SIDE as f32), available.y.max(MIN_VIEW_SIDE as f32));
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        self.size = [f64::from(rect.width()), f64::from(rect.height())];
        (rect, response)
    }

    /// Turns Onion Slice on or off. The picture shows other frames now, so it asks for a new render.
    pub fn set_onion(&mut self, on: bool) {
        if self.onion != on {
            self.onion = on;
            self.rendered = None;
        }
    }

    /// Reads the paths of the selected Part's vertices through the previewed animation when "Trajectories" is on.
    /// The answer is kept until the animation, the Part or the document changes.
    pub fn sync_trajectory(&mut self, core: &mut dyn FnMut(&Value) -> Value, animation: Option<&str>, node: Option<u32>, revision: u64) {
        let key = animation.zip(node).filter(|_| self.trajectories).map(|(name, node)| (name.to_owned(), node, revision));
        let Some(key) = key else {
            self.scene.trajectory = None;
            return;
        };
        if self.scene.trajectory.as_ref().is_some_and(|(kept, _)| *kept == key) {
            return;
        }
        let response = core(&json!({"op": "animation.trajectory", "name": key.0, "node": key.1}));
        let paths = response["result"]["paths"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|path| {
                let points = path["points"].as_array().into_iter().flatten();
                points.filter_map(|point| Some([point[0].as_f64()?, point[1].as_f64()?])).collect()
            })
            .collect();
        // A refusal (a node that draws no mesh) keeps an empty answer, so it is not asked again every frame.
        self.scene.trajectory = Some((key, paths));
    }

    /// Read the evaluated scene the overlays and picking need. `selected` asks for the node matrices
    /// and Part geometry; geometry is not re-read while `playing` or while physics runs (it would cost a
    /// full evaluation per frame).
    pub fn sync_scene(&mut self, core: &mut dyn FnMut(&Value) -> Value, selected: bool, playing: bool) {
        let playing = playing || self.physics;
        let scene = &mut self.scene;
        if selected && scene.matrices.is_none() {
            let response = core(&json!({"op": "scene.evaluate"}));
            match response["succeeded"].as_bool() {
                Some(true) => match (parse_world_matrices(&response["result"]), parse_local_offsets(&response["result"])) {
                    (Ok(matrices), Ok(offsets)) => {
                        scene.matrices = Some(matrices);
                        scene.offsets = offsets;
                    }
                    (Err(error), _) | (_, Err(error)) => scene.error = Some(error),
                },
                _ => scene.error = response["message"].as_str().map(str::to_owned),
            }
            // A failed read is not retried until the next render.
            scene.matrices.get_or_insert_with(BTreeMap::new);
        }
        let want_parts = self.pending_click.is_some() || self.menu_open || (selected && !playing);
        if want_parts && !scene.parts_fresh {
            scene.parts_fresh = true;
            let response = core(&json!({"op": "scene.geometry"}));
            if response["succeeded"] == true {
                match parse_geometry(&response["result"]) {
                    Ok(parts) => {
                        scene.parts = Some(parts);
                        scene.error = None;
                    }
                    Err(error) => scene.error = Some(error),
                }
            } else {
                scene.error = response["message"].as_str().map(str::to_owned);
            }
        }
    }

    /// Read the bones of the depth rig the selection belongs to (`rig_of`), once per rig and selected bone until the
    /// next render. `rig.overlay` is the session's posed bone geometry.
    pub fn sync_rig(&mut self, core: &mut dyn FnMut(&Value) -> Value, rig: Option<(u32, Option<u32>)>) {
        let Some(key) = rig else {
            self.scene.rig = None;
            return;
        };
        if self.scene.rig.as_ref().is_some_and(|(cached, _)| *cached == key) {
            return;
        }
        let mut request = json!({"op": "rig.overlay", "root": key.0});
        if let Some(bone) = key.1 {
            request["selected"] = json!(bone);
        }
        let response = core(&request);
        // A failed read is not retried until the next render.
        let data = if response["succeeded"] == true { response["result"].clone() } else { Value::Null };
        self.scene.rig = Some((key, data));
    }

    fn selection(&self, input: &Input) -> Option<SelView> {
        let info = node_info(input.nodes, input.selected?)?;
        sel_view(info, self.scene.matrices.as_ref()?, self.scene.parts.as_deref())
    }

    fn rel(rect: Rect, pos: Pos2) -> V2 {
        [f64::from(pos.x - rect.min.x), f64::from(pos.y - rect.min.y)]
    }

    fn pos(&self, rect: Rect, world: V2) -> Pos2 {
        let s = self.camera.to_screen(self.size, world);
        Pos2::new(rect.min.x + s[0] as f32, rect.min.y + s[1] as f32)
    }

    fn screen_box(&self, bounds: [f64; 4]) -> [f64; 4] {
        let a = self.camera.to_screen(self.size, [bounds[0], bounds[1]]);
        let b = self.camera.to_screen(self.size, [bounds[2], bounds[3]]);
        [a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])]
    }

    // ------------------------------------------------------------------------------ controls row

    /// Zoom slider and readout, view reset, overlay toggles and background colour.
    pub fn controls(&mut self, ui: &mut egui::Ui, armed: bool) -> Option<Value> {
        let mut arguments = json!({});
        let mut zoom = self.camera.zoom;
        let slider = egui::Slider::new(&mut zoom, ZOOM_MIN..=ZOOM_MAX)
            .text("Zoom")
            .custom_formatter(|value, _| format!("{}%", (value * 100.0) as i64))
            .custom_parser(|text| text.trim().trim_end_matches('%').parse::<f64>().ok().map(|percent| percent / 100.0));
        if ui.add(slider).changed() {
            self.camera.zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        }
        // D shows the camera position, the negative of the view centre.
        ui.label(format!("x = {:.2} y = {:.2}", 0.0 - self.camera.centre[0], 0.0 - self.camera.centre[1]));
        if ui.button("Reset view").clicked() {
            self.set_view(Camera::HOME);
        }
        if !armed {
            for (key, mut value, label) in [
                ("vertices", self.show_vertices, "Show vertices"),
                ("bounds", self.show_bounds, "Show bounds"),
                ("orientation", self.show_orientation, "Show orientation gizmo"),
            ] {
                if ui.checkbox(&mut value, label).changed() {
                    arguments[key] = json!(value);
                }
            }
        }
        let mut background = self.background;
        let label = ui.label("Background colour");
        if ui.color_edit_button_srgb(&mut background).labelled_by(label.id).changed() {
            arguments["background"] = json!(background);
        }
        if ui.button("Reset background").clicked() {
            arguments["background"] = json!([0, 0, 0]);
        }
        let spacing = egui::DragValue::new(&mut self.grid.spacing).range(GRID_SPACING_MIN..=GRID_SPACING_MAX).speed(1.0);
        let label = ui.label("Grid spacing");
        ui.add(spacing)
            .labelled_by(label.id)
            .on_hover_text("World units between two grid lines. Lines closer than 4 points on screen are not drawn");
        let label = ui.label("Grid colour");
        ui.color_edit_button_srgba_unmultiplied(&mut self.grid.colour).labelled_by(label.id);
        let label = ui.label("Inactive colour");
        ui.color_edit_button_srgba_unmultiplied(&mut self.solo.colour)
            .labelled_by(label.id)
            .on_hover_text("What covers the nodes that are not selected in the Solo view, and how opaque it is");
        (!arguments.as_object().unwrap().is_empty())
            .then(|| crate::editor_windows::call_request("editor_viewport_set", arguments))
    }

    /// The part of D's "ModelControl" strip (panels/viewport.d) the session can serve: Mirror View, Enable
    /// physics, Reset physics and Reset parameters. Returns the requests to send. `session_ops` is false
    /// for documents whose session refuses `physics.*` and `value.set`. Onion Slice and Trajectories draw the
    /// previewed animation's neighbouring frames and vertex paths. Post processing is not drawn: the session has
    /// no post-process switch to drive.
    pub fn toolbar(&mut self, ui: &mut egui::Ui, snapshot: &Value, session_ops: bool) -> Vec<crate::editor_windows::Request> {
        use crate::editor_windows::{call_request, McpTarget, Request};
        let mut requests = Vec::new();
        ui.toggle_value(&mut self.camera.mirror, "Mirror View")
            .on_hover_text("Flip the view left to right. The document is not changed");
        ui.toggle_value(&mut self.grid.show, "Show Grid")
            .on_hover_text("Draw a grid over the canvas. Its spacing and colour are next to the zoom");
        let mut open = self.guides_open;
        if ui.toggle_value(&mut open, "Guide Settings")
            .on_hover_text("Add, move, colour and delete guide lines. Right-click the view to add one where you click").changed() {
            requests.push(Request::Mcp(McpTarget::Viewport, call_request("editor_guides_set", json!({"open":open}))));
        }
        ui.toggle_value(&mut self.solo.show, "Solo")
            .on_hover_text("Draw the selected nodes in front and cover the rest with the inactive colour");
        if session_ops {
            let mut enabled = self.physics;
            if ui.toggle_value(&mut enabled, "Enable physics").on_hover_text("Run the physics drivers every frame").changed() {
                requests.push(Request::Mcp(McpTarget::Viewport,
                    call_request("editor_physics_preview_set", json!({"enabled":enabled}))));
            }
            if ui.button("Reset physics").on_hover_text("Put the physics drivers back at rest").clicked() {
                requests.push(Request::Core(json!({"op": "physics.reset"})));
            }
            let reset = reset_parameter_requests(snapshot);
            let button = ui.add_enabled(!reset.is_empty(), egui::Button::new("Reset parameters"));
            if button.on_hover_text("Set every parameter back to its default").clicked() {
                requests.extend(reset.into_iter().map(Request::Core));
            }
            let mut onion = self.onion;
            ui.toggle_value(&mut onion, "Onion Slice")
                .on_hover_text("While an animation frame is previewed, draw the frames around it faintly: red before, blue after");
            self.set_onion(onion);
            ui.toggle_value(&mut self.trajectories, "Trajectories")
                .on_hover_text("While an animation frame is previewed, draw the path each vertex of the selected Part takes");
        }
        requests
    }

    /// A new document starts with its own views: the cameras of the last one's views do not carry over.
    pub fn forget_views(&mut self) {
        self.cameras.clear();
        self.shown_view = None;
        self.drag = None;
        self.controller_drag = None;
        self.form_drag = None;
        self.pending_click = None;
    }

    pub fn editing(&self) -> bool {
        self.drag.is_some() || self.controller_drag.is_some() || self.form_drag.is_some()
    }

    pub fn reconcile_views(&mut self, snapshot: &Value) {
        let list = &snapshot["views"];
        let ids: Vec<u64> =
            list["views"].as_array().into_iter().flatten().filter_map(|view| view["id"].as_u64()).collect();
        let active = list["active"].as_u64();
        if active != self.shown_view {
            if let Some(shown) = self.shown_view {
                self.cameras.insert(shown, self.camera);
            }
            if let Some(camera) = active.and_then(|id| self.cameras.get(&id).copied()) {
                self.set_view(camera);
            }
            self.shown_view = active;
        }
        // After the insert: a removed view's camera must not wait for a new view that takes its id.
        self.cameras.retain(|id, _| ids.contains(id));
    }

    pub fn sync_cameras(&mut self, snapshot: &Value) {
        self.reconcile_views(snapshot);
        for view in snapshot["views"]["views"].as_array().into_iter().flatten() {
            if let Some(id) = view["id"].as_u64() {
                self.cameras.insert(id, self.camera);
            }
        }
    }

    pub fn view_cameras(&self, snapshot: &Value) -> Value {
        if snapshot["documentFormat"] == "live2d.can3" {
            return Value::Null;
        }
        let list = &snapshot["views"];
        let views: Vec<Value> = list["views"].as_array().into_iter().flatten().map(|view| {
            let id = view["id"].as_u64();
            let camera = if id == self.shown_view { Some(self.camera) }
                else { id.and_then(|id| self.cameras.get(&id).copied()) };
            json!({"id": view["id"], "camera": camera.map(|camera| json!({"centre":camera.centre,"zoom":camera.zoom}))})
        }).collect();
        json!({"active":list["active"],"views":views})
    }

    pub fn view_tabs(&mut self, ui: &mut egui::Ui, snapshot: &Value) -> Vec<crate::editor_windows::Request> {
        use crate::editor_windows::{call_request, McpTarget, Request};
        self.reconcile_views(snapshot);
        let list = &snapshot["views"];
        let ids: Vec<u64> =
            list["views"].as_array().into_iter().flatten().filter_map(|view| view["id"].as_u64()).collect();
        let active = list["active"].as_u64();
        let mut requests = Vec::new();
        ui.horizontal_wrapped(|ui| {
            for id in &ids {
                if ui.selectable_label(Some(*id) == active, format!("View {id}")).clicked() && Some(*id) != active {
                    requests.push(Request::Core(json!({"op": "view.select", "id": id})));
                }
            }
            let add = ui.add_enabled(ids.len() < 16, egui::Button::new("Add view"));
            if add.on_hover_text("Add a view at the default forms").clicked() {
                requests.push(Request::Core(json!({"op": "view.add"})));
            }
            let remove = ui.add_enabled(ids.len() > 1, egui::Button::new("Remove view"));
            if remove.on_hover_text("Remove the active view").clicked() {
                requests.push(Request::Core(json!({"op": "view.remove", "id": active})));
            }
            let mut synchronized = list["synchronized"] == true;
            let sync = ui.checkbox(&mut synchronized, "Synchronize parameters");
            if sync.on_hover_text("Every view shows the same parameter values").changed() {
                requests.push(Request::Core(json!({"op": "view.sync.set", "synchronized": synchronized})));
            }
            let same = ui.add_enabled(ids.len() > 1, egui::Button::new("Same display position"));
            if same.on_hover_text("Give every view the position and zoom of this one").clicked() {
                requests.push(Request::Mcp(McpTarget::Viewport, call_request("editor_camera_sync", json!({}))));
            }
        });
        requests
    }

    // ------------------------------------------------------------------------------------- input

    /// Pan, zoom, pick and handle gestures over the picture. Call before rendering so the frame is
    /// rendered for the camera this frame ends with.
    pub fn interact(&mut self, ui: &mut egui::Ui, rect: Rect, response: &egui::Response, input: &Input) -> Output {
        let mut out = Output::default();
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Model viewport"));
        let size = self.size;

        if response.dragged_by(egui::PointerButton::Secondary) {
            let delta = response.drag_delta();
            self.camera.pan([f64::from(delta.x), f64::from(delta.y)]);
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll != 0.0 {
                let cursor = response.hover_pos().map(|pos| Self::rel(rect, pos));
                let notches = f64::from(scroll / WHEEL_NOTCH_POINTS);
                self.camera.zoom_step(size, cursor.filter(|_| input.zoom_to_cursor), notches, input.zoom_speed);
            }
        }

        let sel = if input.handles { self.selection(input) } else { None };
        let sel = sel.filter(|sel| TRANSFORMABLE.contains(&sel.info.kind.as_str()));
        let mods = ui.input(|i| Mods {
            ctrl: i.modifiers.ctrl || i.modifiers.command,
            shift: i.modifiers.shift,
            alt: i.modifiers.alt,
        });

        if self.drag.is_none() && response.drag_started_by(egui::PointerButton::Primary) {
            if let (Some(sel), Some(press)) = (&sel, ui.input(|i| i.pointer.press_origin())) {
                let press = Self::rel(rect, press);
                if let Some(tool) = handle_at(self.screen_box(sel.bounds), press) {
                    self.drag = begin_drag(tool, sel, self.camera.to_world(size, press));
                }
            }
        }
        if let Some(drag) = self.drag.as_mut() {
            let (down, latest, escape) =
                ui.input(|i| (i.pointer.primary_down(), i.pointer.latest_pos(), i.key_pressed(egui::Key::Escape)));
            if escape || sel.as_ref().map(|sel| sel.info.id) != Some(drag.id) {
                self.drag = None;
            } else {
                if let Some(latest) = latest {
                    drag_update(drag, self.camera.to_world(size, Self::rel(rect, latest)), mods);
                }
                if !down {
                    out.requests.extend(drag_request(drag));
                    self.drag = None;
                }
            }
        }

        if input.selectable && self.drag.is_none() && response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                let rel = Self::rel(rect, pos);
                let on_handle = sel.as_ref().is_some_and(|sel| handle_at(self.screen_box(sel.bounds), rel).is_some());
                if !on_handle {
                    self.pending_click = Some(self.camera.to_world(size, rel));
                }
            }
        }
        if let Some(point) = self.pending_click {
            if let Some(parts) = &mut self.scene.parts {
                self.pending_click = None;
                mark_locked(parts, input.nodes);
                lock_inactive(parts, &self.solo_drawn, self.solo.show);
                out.select = hit_test(parts, point);
            } else if self.scene.parts_fresh {
                self.pending_click = None;
            } else {
                ui.ctx().request_repaint();
            }
        }

        if input.menu {
            self.part_menu(rect, response, input, &mut out);
        } else {
            self.menu_open = false;
        }
        out
    }

    /// The right-click menu: guide lines, and the Part list (`ModelViewport.menu`) where a click can select.
    fn part_menu(&mut self, rect: Rect, response: &egui::Response, input: &Input, out: &mut Output) {
        if response.secondary_clicked() {
            self.menu_point =
                response.interact_pointer_pos().map(|pos| self.camera.to_world(self.size, Self::rel(rect, pos)));
        }
        let selected = input.selected.and_then(|id| u32::try_from(id).ok());
        let shown = response.context_menu(|ui| {
            ui.menu_button("Guide line", |ui| {
                if let Some(point) = self.menu_point {
                    let here = [
                        ("Horizontal guide here", &[GuideAxis::Horizontal][..]),
                        ("Vertical guide here", &[GuideAxis::Vertical][..]),
                        ("Both guides here", &[GuideAxis::Horizontal, GuideAxis::Vertical][..]),
                    ];
                    for (label, axes) in here {
                        if ui.button(label).clicked() {
                            axes.iter().for_each(|axis| self.guides.add(*axis, point));
                            ui.close();
                        }
                    }
                }
                if ui.button("Guide Settings").clicked() {
                    out.editor_calls.push(crate::editor_windows::call_request("editor_guides_set", json!({"open":true})));
                    ui.close();
                }
            });
            if !input.selectable {
                return;
            }
            if self.bookmark_menu {
                ui.menu_button("Parameter bookmark", |ui| {
                    // Front to back. Unlike a click, a bookmark also reaches a locked Part: it is still drawn.
                    if !self.scene.parts_fresh {
                        // The geometry is read for the open menu on the next frame.
                        ui.label("Reading the parts");
                        ui.ctx().request_repaint();
                        return;
                    }
                    let found: Vec<u32> = match (&self.scene.parts, self.menu_point) {
                        (Some(parts), Some(point)) => {
                            parts.iter().rev().filter(|part| covers(part, point)).map(|part| part.id).collect()
                        }
                        _ => Vec::new(),
                    };
                    if found.is_empty() {
                        ui.label("No parts here");
                    }
                    for id in found {
                        if ui.button(format!("{} (#{id})", node_name(input.nodes, id))).clicked() {
                            out.bookmark = self.menu_point.map(|point| BookmarkPick::Part(id, point));
                            ui.close();
                        }
                    }
                });
                let add = ui.add_enabled(self.controller_add, egui::Button::new("Add parameter controller"));
                let hint = "Stop playback on a whole frame of the clip; after a value change, seek to show it";
                if add.on_disabled_hover_text(hint).clicked() {
                    out.controller = self.menu_point.map(ControllerPick::Add);
                    ui.close();
                }
            }
            ui.separator();
            let label = if self.menu_by_size { "Size-Sort" } else { "Z-Sort" };
            if ui.button(label).clicked() {
                out.editor_calls.push(crate::editor_windows::call_request("editor_viewport_set",
                    json!({"partSort":if self.menu_by_size { "z_sort" } else { "size_sort" }})));
            }
            ui.separator();
            let root = input.nodes["uuid"].as_u64();
            if let Some(sel) = self.selection(input).filter(|sel| Some(u64::from(sel.info.id)) != root) {
                if ui.button("Focus Selected").clicked() {
                    if let Some(camera) = focus_camera(sel.bounds, self.size) {
                        self.set_view(camera);
                    }
                    ui.close();
                }
            }
            let found = match (&mut self.scene.parts, self.menu_point) {
                (Some(parts), Some(point)) => {
                    mark_locked(parts, input.nodes);
                    lock_inactive(parts, &self.solo_drawn, self.solo.show);
                    parts_under(parts, point, selected, self.menu_by_size)
                }
                _ => Vec::new(),
            };
            if found.is_empty() {
                ui.label("No parts found");
            }
            for id in found {
                if ui.selectable_label(false, format!("{} #{id}", node_name(input.nodes, id))).clicked() {
                    out.select = Some(id);
                    ui.close();
                }
            }
        });
        self.menu_open = shown.is_some();
    }

    /// The bookmark labels, drawn over the picture where they were placed. A click shows one, its menu deletes it.
    pub fn bookmark_labels(&self, ui: &mut egui::Ui, rect: Rect) -> Option<BookmarkPick> {
        let mut pick = None;
        for (index, label, point) in &self.bookmarks {
            let (index, at) = (*index, self.pos(rect, *point));
            if !rect.contains(at) {
                continue;
            }
            // In the viewport's own layer, so windows and menus stay above the labels.
            let place = egui::UiBuilder::new().max_rect(Rect::from_min_max(at, rect.max));
            let response = ui.scope_builder(place, |ui| ui.button(label)).inner;
            let response = response.on_hover_text("Show the parameters of this bookmark");
            if response.clicked() {
                pick = Some(BookmarkPick::Label(index));
            }
            response.context_menu(|ui| {
                if ui.button(format!("Delete label {label}")).clicked() {
                    pick = Some(BookmarkPick::Delete(index));
                    ui.close();
                }
            });
        }
        pick
    }

    /// The parameter controllers, drawn over the picture: a ring, and for a direction controller a line from its start
    /// to the ring at its end. Dragging the ring moves the controller where it is let go, Escape cancels, and its menu
    /// deletes it. Only the ring of a dragged controller moves until then: each move solves its parameters.
    pub fn controller_handles(&mut self, ui: &mut egui::Ui, rect: Rect) -> Option<ControllerPick> {
        let mut pick = None;
        let painter = ui.painter_at(rect);
        let colour = egui::Color32::from_rgb(255, 160, 0);
        let (escape, pointer, down) =
            ui.input(|i| (i.key_pressed(egui::Key::Escape), i.pointer.latest_pos(), i.pointer.primary_down()));
        if escape {
            self.controller_drag = None;
        }
        for controller in self.controllers.clone() {
            let start = self.pos(rect, controller.start);
            let at = controller.end.map_or(start, |end| self.pos(rect, end));
            if controller.end.is_some() {
                painter.line_segment([start, at], egui::Stroke::new(2.0, colour));
                painter.circle_filled(start, 3.0, colour);
            }
            painter.circle_stroke(at, 6.0, egui::Stroke::new(2.0, colour));
            let name = controller.name;
            let id = ui.id().with(("parameter controller", &name));
            // A ring dragged off the picture keeps its drag, so a pan begun on it goes on.
            if !rect.contains(at) && !ui.ctx().is_being_dragged(id) {
                continue;
            }
            let place = Rect::from_center_size(at, egui::vec2(14.0, 14.0));
            let response = ui.interact(place, id, egui::Sense::click_and_drag());
            let info = || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Controller {name}"));
            response.widget_info(info);
            let response = response.on_hover_text(format!("Drag to move controller {name}"));
            // The ring takes the drag from the picture under it, so it pans as the picture would.
            if response.dragged_by(egui::PointerButton::Secondary) {
                let delta = response.drag_delta();
                self.camera.pan([f64::from(delta.x), f64::from(delta.y)]);
            }
            if response.drag_started() {
                self.controller_drag = Some(name.clone());
            }
            let dragged = self.controller_drag.as_ref() == Some(&name);
            if let (true, Some(pointer)) = (dragged, pointer) {
                painter.circle_stroke(pointer, 6.0, egui::Stroke::new(1.0, colour.gamma_multiply(0.6)));
                if response.drag_stopped() {
                    self.controller_drag = None;
                    let point = self.camera.to_world(self.size, Self::rel(rect, pointer));
                    pick = Some(ControllerPick::Move(name.clone(), point));
                }
            }
            response.context_menu(|ui| {
                if ui.button(format!("Delete controller {name}")).clicked() {
                    pick = Some(ControllerPick::Delete(name.clone()));
                    ui.close();
                }
            });
        }
        // Only a primary drag moves a ring, since the secondary button pans the picture. A drag whose ring went away
        // (playback, a tool, an undo) ends with the button too, so no ghost ring is left.
        if !down {
            self.controller_drag = None;
        }
        pick
    }

    /// The vertices of the node whose form is edited, drawn over the picture. Dragging one keys the form where it is
    /// let go, and Escape cancels; until then only the dragged vertex follows the pointer.
    pub fn form_handles(&mut self, ui: &mut egui::Ui, rect: Rect) -> Option<FormPick> {
        let mut pick = None;
        let painter = ui.painter_at(rect);
        let colour = egui::Color32::from_rgb(0, 200, 255);
        let (escape, pointer, down) =
            ui.input(|i| (i.key_pressed(egui::Key::Escape), i.pointer.latest_pos(), i.pointer.primary_down()));
        // egui ends a drag on Escape by reporting it stopped, so the drag is dropped before that is read.
        if escape {
            self.form_drag = None;
        }
        for (index, vertex) in self.form_vertices.clone().into_iter().enumerate() {
            let at = self.pos(rect, vertex);
            painter.circle_filled(at, 3.0, colour);
            let id = ui.id().with(("form vertex", index));
            if !rect.contains(at) && !ui.ctx().is_being_dragged(id) {
                continue;
            }
            // ponytail: one hit area per vertex on every frame; hit-test the pointer against the list if dense meshes
            // make the viewport slow.
            let place = Rect::from_center_size(at, egui::vec2(10.0, 10.0));
            let response = ui.interact(place, id, egui::Sense::click_and_drag());
            let info = || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Form vertex {index}"));
            response.widget_info(info);
            // The vertex takes the drag from the picture under it, so it pans as the picture would.
            if response.dragged_by(egui::PointerButton::Secondary) {
                let delta = response.drag_delta();
                self.camera.pan([f64::from(delta.x), f64::from(delta.y)]);
            }
            if response.drag_started() {
                let from = ui.input(|i| i.pointer.press_origin()).unwrap_or(at);
                self.form_drag = Some((index, self.camera.to_world(self.size, Self::rel(rect, from))));
            }
            let Some((_, from)) = self.form_drag.filter(|(dragged, _)| *dragged == index) else {
                continue;
            };
            if let Some(pointer) = pointer {
                painter.circle_stroke(pointer, 4.0, egui::Stroke::new(1.5, colour));
                if response.drag_stopped() {
                    self.form_drag = None;
                    let to = self.camera.to_world(self.size, Self::rel(rect, pointer));
                    pick = Some(FormPick { vertex: index, delta: [to[0] - from[0], to[1] - from[1]] });
                }
            }
        }
        // Only a primary drag moves a vertex, and a drag whose vertices went away ends with the button.
        if !down {
            self.form_drag = None;
        }
        pick
    }

    // ------------------------------------------------------------------------------------ painting

    /// PathDeformer curve, GridDeformer lattice, MeshGroup lines and SimplePhysics anchor of the selection.
    fn paint_overlay(&self, painter: &egui::Painter, rect: Rect, input: &Input, sel: &SelView) {
        let (Some(node), Some(world)) = (locate(input.nodes, u64::from(sel.info.id), None), self.scene.matrices.as_ref())
        else {
            return;
        };
        let Some(world) = world.get(&sel.info.id) else { return };
        let offsets = self.scene.offsets.get(&sel.info.id).map_or(&[][..], Vec::as_slice);
        let overlay = node_overlay(node.0, world, offsets, self.show_vertices);
        for (a, b, shade) in &overlay.lines {
            let (width, color) = match shade {
                Shade::Rest => (1.0, Color32::from_gray(128)),
                Shade::Deformed => (1.0, Color32::from_rgb(128, 255, 128)),
                Shade::Lattice => (1.0, Color32::from_gray(128)),
                Shade::Mesh => (1.0, Color32::from_rgb(120, 200, 255)),
                Shade::Anchor => (3.0, Color32::from_rgb(255, 0, 255)),
            };
            painter.line_segment([self.pos(rect, *a), self.pos(rect, *b)], Stroke::new(width, color));
        }
        for point in &overlay.points {
            painter.circle_filled(self.pos(rect, *point), 3.0, Color32::from_rgb(128, 255, 128));
        }
    }

    /// Joints and links of the selected depth rig.
    fn paint_bones(&self, painter: &egui::Painter, rect: Rect) {
        let Some(((root, _), data)) = &self.scene.rig else { return };
        let Some(world) = self.scene.matrices.as_ref().and_then(|m| m.get(root)) else { return };
        let overlay = bone_overlay(data, world);
        for (a, b, link) in &overlay.lines {
            let color = match link {
                BoneLink::Plain => Color32::from_rgb(200, 200, 80),
                BoneLink::Parent => Color32::from_rgb(80, 200, 255),
                BoneLink::Child => Color32::from_rgb(255, 120, 220),
            };
            painter.line_segment([self.pos(rect, *a), self.pos(rect, *b)], Stroke::new(2.0, color));
        }
        for (joint, selected) in &overlay.joints {
            let at = self.pos(rect, *joint);
            if *selected {
                painter.circle_filled(at, 6.0, Color32::from_rgb(255, 140, 0));
                painter.circle_stroke(at, 8.0, Stroke::new(2.0, Color32::WHITE));
            } else {
                painter.circle_filled(at, 4.0, Color32::from_rgb(255, 230, 90));
            }
        }
    }

    /// Background, picture, selection overlays and handles.
    pub fn paint(&self, ui: &mut egui::Ui, rect: Rect, texture: Option<&egui::TextureHandle>, input: &Input) {
        let painter = ui.painter_at(rect);
        let [r, g, b] = self.background;
        painter.rect_filled(rect, egui::CornerRadius::ZERO, Color32::from_rgb(r, g, b));
        if let Some(texture) = texture {
            // A mirrored view reads the picture right to left.
            let (left, right) = if self.camera.mirror { (1.0, 0.0) } else { (0.0, 1.0) };
            let uv = Rect::from_min_max(Pos2::new(left, 0.0), Pos2::new(right, 1.0));
            painter.image(texture.id(), rect, uv, Color32::WHITE);
        }
        if self.grid.show {
            let (xs, ys) = grid_lines(&self.camera, self.size, self.grid.spacing);
            let [r, g, b, a] = self.grid.colour;
            let stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(r, g, b, a));
            let view = self.view_bounds();
            for x in xs {
                painter.line_segment([self.pos(rect, [x, view[1]]), self.pos(rect, [x, view[3]])], stroke);
            }
            for y in ys {
                painter.line_segment([self.pos(rect, [view[0], y]), self.pos(rect, [view[2], y])], stroke);
            }
        }
        if self.guides.show {
            let view = self.view_bounds();
            for (index, guide) in self.guides.lines.iter().enumerate() {
                let [r, g, b, a] = guide.colour;
                let colour = if self.guides_attached.contains(&index) {
                    GUIDE_ATTACHED
                } else {
                    Color32::from_rgba_unmultiplied(r, g, b, a)
                };
                let (from, to) = match guide.axis {
                    GuideAxis::Horizontal => ([view[0], guide.position], [view[2], guide.position]),
                    GuideAxis::Vertical => ([guide.position, view[1]], [guide.position, view[3]]),
                };
                painter.line_segment([self.pos(rect, from), self.pos(rect, to)], Stroke::new(1.0, colour));
            }
        }
        if let Some(error) = &self.scene.error {
            painter.text(
                rect.left_bottom() + egui::vec2(4.0, -4.0),
                egui::Align2::LEFT_BOTTOM,
                format!("Overlay data unavailable: {error}"),
                egui::FontId::proportional(12.0),
                Color32::YELLOW,
            );
        }
        if let Some((_, paths)) = self.scene.trajectory.as_ref().filter(|_| self.trajectories) {
            let stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 220, 80, 200));
            for path in paths {
                painter.add(egui::Shape::line(path.iter().map(|point| self.pos(rect, *point)).collect(), stroke));
            }
        }
        let Some(sel) = self.selection(input) else { return };
        let line = |a: V2, b: V2, stroke: Stroke| painter.line_segment([self.pos(rect, a), self.pos(rect, b)], stroke);

        if self.show_bounds {
            let b = sel.bounds;
            let corners = [[b[0], b[1]], [b[2], b[1]], [b[2], b[3]], [b[0], b[3]]];
            for i in 0..4 {
                line(corners[i], corners[(i + 1) % 4], Stroke::new(3.0, Color32::from_gray(128)));
            }
        }
        if !self.suppress_part_overlay {
            self.paint_overlay(&painter, rect, input, &sel);
        }
        if self.show_vertices && !self.suppress_part_overlay {
            if let Some(part) = self.scene.parts.as_ref().and_then(|parts| parts.iter().find(|p| p.id == sel.info.id)) {
                let stroke = Stroke::new(1.0, Color32::from_rgb(120, 200, 255));
                for t in &part.tris {
                    for k in 0..3 {
                        line(part.verts[t[k]], part.verts[t[(k + 1) % 3]], stroke);
                    }
                }
                for vertex in &part.verts {
                    painter.circle_filled(self.pos(rect, *vertex), 2.5, Color32::from_rgb(255, 200, 80));
                }
            }
        }
        if !self.suppress_part_overlay {
            self.paint_bones(&painter, rect);
        }
        if self.show_orientation {
            if let Some(world) = self.scene.matrices.as_ref().and_then(|m| m.get(&sel.info.id)) {
                let at = |p: V2| {
                    [
                        world[0][0] * p[0] + world[0][1] * p[1] + world[0][3],
                        world[1][0] * p[0] + world[1][1] * p[1] + world[1][3],
                    ]
                };
                let red = Color32::from_rgba_unmultiplied(255, 0, 0, 178);
                let green = Color32::from_rgba_unmultiplied(0, 255, 0, 178);
                line(at([0.0, 0.0]), at([ORIENTATION_LENGTH, 0.0]), Stroke::new(4.0, red));
                line(at([0.0, 0.0]), at([0.0, -ORIENTATION_LENGTH]), Stroke::new(4.0, green));
            }
        }
        painter.circle_stroke(self.pos(rect, sel.origin), 5.0, Stroke::new(2.0, Color32::WHITE));
        painter.circle_filled(self.pos(rect, sel.origin), 2.0, Color32::WHITE);

        if !input.handles || !TRANSFORMABLE.contains(&sel.info.kind.as_str()) {
            return;
        }
        for (tool, zone) in handle_rects(self.screen_box(sel.bounds)) {
            let zone_rect = Rect::from_min_max(
                Pos2::new(rect.min.x + zone[0] as f32, rect.min.y + zone[1] as f32),
                Pos2::new(rect.min.x + zone[2] as f32, rect.min.y + zone[3] as f32),
            );
            let (glyph, name) = match tool {
                Tool::Move => ("M", "Move handle"),
                Tool::Rotate => ("R", "Rotate handle"),
                Tool::Scale => ("S", "Scale handle"),
            };
            let fill = Color32::from_rgba_unmultiplied(40, 40, 40, 200);
            painter.rect_filled(zone_rect, egui::CornerRadius::same(3), fill);
            let font = egui::FontId::proportional(14.0);
            painter.text(zone_rect.center(), egui::Align2::CENTER_CENTER, glyph, font, Color32::WHITE);
            let id = ui.id().with(("viewport-handle", name));
            ui.interact(zone_rect.intersect(rect), id, egui::Sense::hover())
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
        }
        if let Some(drag) = &self.drag {
            let b = sel.bounds;
            let corners = [[b[0], b[1]], [b[2], b[1]], [b[2], b[3]], [b[0], b[3]]].map(|c| drag.ghost.apply(c));
            for i in 0..4 {
                line(corners[i], corners[(i + 1) % 4], Stroke::new(2.0, Color32::from_rgb(80, 220, 255)));
            }
            line(sel.origin, drag.ghost.apply(sel.origin), Stroke::new(1.0, Color32::from_rgb(80, 220, 255)));
        }
    }
}

// ------------------------------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use soul2d_core::Session;

    #[test]
    fn remote_quit_detects_pending_viewport_interactions_and_replacement_releases_them() {
        let mut view = ViewportState::default();
        assert!(!view.editing());
        view.controller_drag = Some("controller".into());
        assert!(view.editing());
        view.forget_views();
        assert!(!view.editing());
        view.form_drag = Some((0, [1.0, 2.0]));
        assert!(view.editing());
        view.forget_views();
        assert!(!view.editing());
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    fn close2(a: V2, b: V2) {
        close(a[0], b[0]);
        close(a[1], b[1]);
    }

    #[test]
    fn camera_maps_between_world_and_screen() {
        let camera = Camera { centre: [10.0, 20.0], zoom: 2.0, mirror: false };
        assert_eq!(camera.view_bounds([200.0, 100.0]), [-40.0, -5.0, 60.0, 45.0]);
        assert_eq!(camera.to_screen([200.0, 100.0], [10.0, 20.0]), [100.0, 50.0]);
        assert_eq!(camera.to_screen([200.0, 100.0], [-40.0, -5.0]), [0.0, 0.0]);
        assert_eq!(camera.to_world([200.0, 100.0], [200.0, 100.0]), [60.0, 45.0]);
    }

    #[test]
    fn wheel_zoom_follows_the_d_step_and_limits() {
        let size = [200.0, 100.0];
        let mut camera = Camera::HOME;
        // Speed 5, one notch: 1 + 5/50 = 1.1, around the view centre.
        camera.zoom_step(size, None, 1.0, 5.0);
        close(camera.zoom, 1.1);
        assert_eq!(camera.centre, [0.0, 0.0]);
        // One notch back out is a step of the current zoom, not the inverse of the previous one.
        camera.zoom_step(size, None, -1.0, 5.0);
        close(camera.zoom, 1.1 * 0.9);
        camera.zoom_step(size, None, 10_000.0, 5.0);
        assert_eq!(camera.zoom, ZOOM_MAX);
        camera.zoom_step(size, None, -10_000.0, 5.0);
        assert_eq!(camera.zoom, ZOOM_MIN);
    }

    #[test]
    fn zoom_to_cursor_keeps_the_world_point_under_the_cursor() {
        let size = [200.0, 100.0];
        let mut camera = Camera::HOME;
        let cursor = [150.0, 50.0];
        assert_eq!(camera.to_world(size, cursor), [50.0, 0.0]);
        camera.zoom_step(size, Some(cursor), 1.0, 5.0);
        close(camera.zoom, 1.1);
        close(camera.centre[0], 50.0 - 50.0 / 1.1);
        close(camera.centre[1], 0.0);
        close2(camera.to_world(size, cursor), [50.0, 0.0]);
    }

    #[test]
    fn pan_moves_the_picture_with_the_pointer() {
        let mut camera = Camera { centre: [0.0, 0.0], zoom: 2.0, mirror: false };
        camera.pan([10.0, -4.0]);
        assert_eq!(camera.centre, [-5.0, 2.0]);
    }

    #[test]
    fn fit_and_focus_follow_the_d_commands() {
        let fit = fit_camera([0.0, 0.0, 400.0, 200.0], [200.0, 200.0]).unwrap();
        assert_eq!(fit, Camera { centre: [200.0, 100.0], zoom: 0.5, mirror: false });
        assert_eq!(fit_camera([0.0, 0.0, 1.0, 1.0], [600.0, 600.0]).unwrap().zoom, ZOOM_MAX);
        assert_eq!(fit_camera([0.0, 0.0, 100_000.0, 1.0], [100.0, 100.0]).unwrap().zoom, ZOOM_MIN);
        assert_eq!(fit_camera([0.0, 0.0, 0.0, 5.0], [100.0, 100.0]), None);
        let focus = focus_camera([0.0, 0.0, 100.0, 50.0], [200.0, 400.0]).unwrap();
        assert_eq!(focus, Camera { centre: [50.0, 25.0], zoom: 1.8, mirror: false });
        assert_eq!(focus_camera([0.0, 0.0, 10.0, 10.0], [200.0, 400.0]).unwrap().zoom, 2.5);
        assert_eq!(render_size([1536.0, 768.0]), (768, 384));
        assert_eq!(render_size([300.0, 200.0]), (300, 200));
    }

    fn quad(id: u32, x: f64, y: f64, w: f64, h: f64) -> PartGeom {
        PartGeom {
            id,
            verts: vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]],
            tris: vec![[0, 1, 2], [0, 2, 3]],
            locked: false,
        }
    }

    #[test]
    fn a_locked_part_is_not_picked_and_the_click_reaches_the_part_below() {
        let mut parts = [quad(7, 0.0, 0.0, 10.0, 10.0), quad(8, 0.0, 0.0, 10.0, 10.0)];
        assert_eq!(hit_test(&parts, [5.0, 5.0]), Some(8));
        assert_eq!(parts_under(&parts, [5.0, 5.0], None, false), [8, 7]);
        parts[1].locked = true;
        assert_eq!(hit_test(&parts, [5.0, 5.0]), Some(7));
        assert_eq!(parts_under(&parts, [5.0, 5.0], None, false), [7]);
        parts[0].locked = true;
        assert_eq!(hit_test(&parts, [5.0, 5.0]), None);
        let read = json!({"parts": [{"id": 3, "vertices": [[0.0, 0.0]], "indices": [], "locked": true},
            {"id": 4, "vertices": [[0.0, 0.0]], "indices": []}]});
        let locked: Vec<bool> = parse_geometry(&read).unwrap().iter().map(|part| part.locked).collect();
        assert_eq!(locked, [true, false]);
    }

    #[test]
    fn picking_takes_the_topmost_triangle_and_ignores_gaps() {
        // Painter order: 7 is drawn first, 8 on top. 8 is a triangle, so its far corner is empty.
        let top =
            PartGeom { id: 8, verts: vec![[0.0, 0.0], [10.0, 0.0], [0.0, 10.0]], tris: vec![[0, 1, 2]], locked: false };
        let parts = [quad(7, 0.0, 0.0, 10.0, 10.0), top];
        assert_eq!(hit_test(&parts, [2.0, 2.0]), Some(8));
        assert_eq!(hit_test(&parts, [9.0, 9.0]), Some(7));
        assert_eq!(hit_test(&parts, [11.0, 5.0]), None);
        assert_eq!(hit_test(&[], [0.0, 0.0]), None);
    }

    #[test]
    fn the_part_list_is_front_first_or_smallest_first_and_skips_the_selection() {
        let parts = [quad(1, 0.0, 0.0, 100.0, 100.0), quad(2, 0.0, 0.0, 10.0, 10.0), quad(3, 0.0, 0.0, 50.0, 50.0)];
        assert_eq!(parts_under(&parts, [5.0, 5.0], None, false), [3, 2, 1]);
        assert_eq!(parts_under(&parts, [5.0, 5.0], None, true), [2, 3, 1]);
        assert_eq!(parts_under(&parts, [5.0, 5.0], Some(2), false), [3, 1]);
        assert_eq!(parts_under(&parts, [60.0, 60.0], None, false), [1]);
        let mut locked = parts.clone();
        locked[1].locked = true;
        assert_eq!(parts_under(&locked, [5.0, 5.0], None, true), [3, 1]);
        assert_eq!(hit_test(&parts, [5.0, 5.0]), Some(3));
    }

    #[test]
    fn handles_sit_outside_the_box_corners() {
        let b = [100.0, 100.0, 200.0, 180.0];
        assert_eq!(handle_at(b, [90.0, 90.0]), Some(Tool::Move));
        assert_eq!(handle_at(b, [210.0, 90.0]), Some(Tool::Rotate));
        assert_eq!(handle_at(b, [90.0, 190.0]), Some(Tool::Scale));
        assert_eq!(handle_at(b, [150.0, 140.0]), None, "inside the box");
        assert_eq!(handle_at(b, [210.0, 190.0]), None, "bottom-right has no handle");
        assert_eq!(handle_at(b, [70.0, 90.0]), None);
        assert_eq!(handle_rects(b)[0].1, [76.0, 76.0, 100.0, 100.0]);
    }

    fn selection(trans: [f64; 3], rot_z: f64, scale: V2, parent_lin: Mat2, bounds: [f64; 4]) -> SelView {
        let info = NodeInfo {
            id: 9,
            kind: "Node".into(),
            parent: Some(1),
            root: 1,
            lock_to_root: false,
            trans,
            rot: [0.0, 0.0, rot_z],
            scale,
            subtree: vec![9],
        };
        SelView { info, origin: [0.0, 0.0], bounds, axes: ([1.0, 0.0], [0.0, 1.0]), parent_lin }
    }

    #[test]
    fn a_move_drag_is_mapped_through_the_parent_matrix() {
        // The parent turns 90 degrees and doubles: local (x, y) lands at world (-2y, 2x).
        let sel = selection([10.0, 5.0, 3.0], 0.0, [1.0, 1.0], [[0.0, -2.0], [2.0, 0.0]], [0.0; 4]);
        let mut drag = begin_drag(Tool::Move, &sel, [100.0, 100.0]).unwrap();
        drag_update(&mut drag, [120.0, 100.0], Mods::default());
        assert_eq!(drag.values.trans, [10.0, -5.0, 3.0]);
        assert_eq!(drag.ghost.apply([0.0, 0.0]), [20.0, 0.0], "the ghost moves by the world delta");
        assert_eq!(
            drag_request(&drag),
            Some(json!({"op": "node.transform", "id": 9, "translation": [10.0, -5.0, 3.0]}))
        );
        // Ctrl snaps the local values to 5; Shift locks to the dominant axis.
        drag_update(&mut drag, [103.0, 100.0], Mods { ctrl: true, ..Mods::default() });
        assert_eq!(drag.values.trans, [10.0, 5.0, 3.0]);
        let id = [[1.0, 0.0], [0.0, 1.0]];
        let sel = selection([1.0, 1.0, 0.0], 0.0, [1.0, 1.0], id, [0.0; 4]);
        let mut drag = begin_drag(Tool::Move, &sel, [0.0, 0.0]).unwrap();
        drag_update(&mut drag, [30.0, 7.0], Mods { shift: true, ..Mods::default() });
        assert_eq!(drag.values.trans, [31.0, 1.0, 0.0], "vertical position is kept");
        drag_update(&mut drag, [4.0, 9.0], Mods { shift: true, ..Mods::default() });
        assert_eq!(drag.values.trans, [5.0, 1.0, 0.0], "the lock stays while Shift is held");
        drag_update(&mut drag, [4.0, 9.0], Mods::default());
        assert_eq!(drag.values.trans, [5.0, 10.0, 0.0]);
        // No movement, no request.
        let still = begin_drag(Tool::Move, &sel, [0.0, 0.0]).unwrap();
        assert_eq!(drag_request(&still), None);
    }

    #[test]
    fn a_rotation_drag_adds_the_swept_angle_and_flips_under_a_mirroring_parent() {
        let id = [[1.0, 0.0], [0.0, 1.0]];
        let sel = selection([0.0; 3], 0.25, [1.0, 1.0], id, [0.0; 4]);
        let mut drag = begin_drag(Tool::Rotate, &sel, [10.0, 0.0]).unwrap();
        drag_update(&mut drag, [0.0, 10.0], Mods::default());
        close(drag.values.rot[2], 0.25 + std::f64::consts::FRAC_PI_2);
        // The ghost turns the box by the same angle about the origin.
        close2(drag.ghost.apply([10.0, 0.0]), [0.0, 10.0]);
        // Sweeping past -pi stays continuous: from angle 170 degrees to -170 degrees is +20 degrees.
        let mut drag = begin_drag(Tool::Rotate, &sel, [-10.0 * 0.984807753, 10.0 * 0.173648178]).unwrap();
        drag_update(&mut drag, [-10.0 * 0.984807753, -10.0 * 0.173648178], Mods::default());
        close(drag.values.rot[2], 0.25 + 20.0_f64.to_radians());
        // A mirroring parent reverses the local direction.
        let mirrored = selection([0.0; 3], 0.0, [1.0, 1.0], [[-1.0, 0.0], [0.0, 1.0]], [0.0; 4]);
        let mut drag = begin_drag(Tool::Rotate, &mirrored, [10.0, 0.0]).unwrap();
        drag_update(&mut drag, [0.0, 10.0], Mods::default());
        close(drag.values.rot[2], -std::f64::consts::FRAC_PI_2);
        // Ctrl snaps the angle to 5 degrees.
        let mut drag = begin_drag(Tool::Rotate, &sel, [10.0, 0.0]).unwrap();
        let near = 12.0_f64.to_radians();
        drag_update(&mut drag, [10.0 * near.cos(), 10.0 * near.sin()], Mods { ctrl: true, ..Mods::default() });
        // 0.25 rad is 14.32 degrees; plus the 12 degree sweep is 26.32, which snaps to 25.
        close(drag.values.rot[2].to_degrees(), 25.0);
        assert!(drag_request(&drag).is_some());
    }

    #[test]
    fn a_scale_drag_scales_by_the_pointer_ratio_about_the_box_centre() {
        let id = [[1.0, 0.0], [0.0, 1.0]];
        let sel = selection([0.0; 3], 0.0, [2.0, 3.0], id, [-10.0, -10.0, 10.0, 10.0]);
        // From (20, 10) to (40, 30) relative to the centre: x doubles, y triples.
        let mut drag = begin_drag(Tool::Scale, &sel, [20.0, 10.0]).unwrap();
        drag_update(&mut drag, [40.0, 30.0], Mods::default());
        assert_eq!(drag.values.scale, [4.0, 9.0]);
        assert_eq!(drag.ghost.apply([10.0, 10.0]), [20.0, 30.0], "the ghost scales about the centre");
        assert_eq!(drag_request(&drag), Some(json!({"op": "node.transform", "id": 9, "scale": [4.0, 9.0]})));
        // Shift locks the weaker axis; Shift+Alt scales both by the distance ratio; Ctrl floors to 0.1.
        drag_update(&mut drag, [40.0, 30.0], Mods { shift: true, ..Mods::default() });
        assert_eq!(drag.values.scale, [2.0, 9.0]);
        let mut drag = begin_drag(Tool::Scale, &sel, [30.0, 40.0]).unwrap();
        drag_update(&mut drag, [60.0, 80.0], Mods { shift: true, alt: true, ..Mods::default() });
        assert_eq!(drag.values.scale, [4.0, 6.0]);
        let mut drag = begin_drag(Tool::Scale, &sel, [20.0, 20.0]).unwrap();
        drag_update(&mut drag, [30.9, 20.0], Mods { ctrl: true, ..Mods::default() });
        assert_eq!(drag.values.scale, [3.0, 3.0]);
        // A node turned 90 degrees reads the pointer in its own axes: world +y is its x.
        let mut turned = selection([0.0; 3], 0.0, [1.0, 1.0], id, [-10.0, -10.0, 10.0, 10.0]);
        turned.axes = ([0.0, 1.0], [-1.0, 0.0]);
        let mut drag = begin_drag(Tool::Scale, &turned, [0.0, 20.0]).unwrap();
        drag_update(&mut drag, [0.0, 60.0], Mods::default());
        assert_eq!(drag.values.scale[0], 3.0);
    }

    #[test]
    fn a_singular_parent_offers_no_drag() {
        let sel = selection([0.0; 3], 0.0, [1.0, 1.0], [[0.0, 0.0], [0.0, 0.0]], [0.0; 4]);
        assert!(begin_drag(Tool::Move, &sel, [0.0, 0.0]).is_none());
    }

    // ------------------------------------------------------------ against a real session

    fn node(uuid: u64, name: &str, kind: &str, trans: [f64; 3], rot_z: f64, scale: [f64; 2], children: Value) -> Value {
        json!({
            "uuid": uuid, "name": name, "type": kind, "enabled": true, "zsort": 0.0,
            "transform": {"trans": trans, "rot": [0.0, 0.0, rot_z], "scale": scale},
            "lockToRoot": false, "pinToMesh": false, "children": children,
        })
    }

    fn part(uuid: u64, name: &str, zsort: f64, trans: [f64; 3], verts: Value) -> Value {
        let mut part = node(uuid, name, "Part", trans, 0.0, [1.0, 1.0], json!([]));
        part["zsort"] = json!(zsort);
        part["textures"] = json!([u32::MAX, u32::MAX, u32::MAX]);
        part["mesh"] = json!({"verts": verts, "indices": [0, 1, 2], "origin": [0.0, 0.0]});
        part
    }

    fn session() -> Session {
        // Group (2) is turned 90 degrees and doubled; Child (3) sits at (10, 5) inside it.
        let group = node(2, "Group", "Node", [100.0, 50.0, 0.0], std::f64::consts::FRAC_PI_2, [2.0, 2.0], json!([
            node(3, "Child", "Node", [10.0, 5.0, 0.0], 0.0, [1.0, 1.0], json!([])),
        ]));
        // Back (4) is a triangle at z 1 (behind); Front (5) overlaps it at z 0.
        let back = part(4, "Back", 1.0, [0.0, 0.0, 0.0], json!([0.0, 0.0, 40.0, 0.0, 0.0, 40.0]));
        let front = part(5, "Front", 0.0, [0.0, 0.0, 0.0], json!([0.0, 0.0, 20.0, 0.0, 0.0, 20.0]));
        let model = json!({
            "meta": {"name": "viewport", "version": "1.0-alpha"},
            "nodes": node(1, "Root", "Node", [0.0; 3], 0.0, [1.0, 1.0], json!([group, back, front])),
            "param": [], "animations": {},
        });
        let mut session = Session::new();
        let response = session.handle_request(&json!({"op": "new", "model": model}));
        assert_eq!(response["succeeded"], true, "{response}");
        session
    }

    fn core(session: &mut Session) -> impl FnMut(&Value) -> Value + '_ {
        move |request| session.handle_request(request)
    }

    #[test]
    fn the_rig_of_a_selection_is_its_nearest_depth_rig_root_and_its_bone() {
        let tree = node(1, "Root", "Node", [0.0; 3], 0.0, [1.0, 1.0], json!([
            node(2, "Rig", "DepthRigRoot", [0.0; 3], 0.0, [1.0, 1.0], json!([
                node(3, "Hip", "DepthBone", [0.0; 3], 0.0, [1.0, 1.0], json!([
                    node(4, "Leg", "DepthBone", [0.0; 3], 0.0, [1.0, 1.0], json!([
                        node(6, "Boot", "Node", [0.0; 3], 0.0, [1.0, 1.0], json!([])),
                    ])),
                ])),
            ])),
            node(5, "Other", "Node", [0.0; 3], 0.0, [1.0, 1.0], json!([])),
        ]));
        assert_eq!(rig_of(&tree, 2), Some((2, None)), "the rig root itself");
        assert_eq!(rig_of(&tree, 3), Some((2, Some(3))));
        assert_eq!(rig_of(&tree, 4), Some((2, Some(4))));
        assert_eq!(rig_of(&tree, 6), Some((2, None)), "a non-bone node under a bone");
        assert_eq!(rig_of(&tree, 5), None);
        assert_eq!(rig_of(&tree, 1), None);
        assert_eq!(rig_of(&tree, 99), None);
    }

    #[test]
    fn bone_overlay_maps_rig_space_points_through_the_root_matrix() {
        // Scale 2, then move by (10, 20); a z term of 0.5 shifts x by 0.5 * z.
        let world: Mat4 = [[2.0, 0.0, 0.5, 10.0], [0.0, 2.0, 0.0, 20.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
        let data = json!({
            "points": [[0.0, 10.0, 0.0]],
            "selectedPoints": [[5.0, 0.0, 2.0]],
            "lines": [[[0.0, 0.0, 0.0], [0.0, 10.0, 0.0]]],
            "parentLines": [[[0.0, 10.0, 0.0], [5.0, 0.0, 2.0]]],
            "childLines": [[[5.0, 0.0, 2.0], [5.0, -4.0, 2.0]]],
            "effectivePivots": [],
        });
        assert_eq!(
            bone_overlay(&data, &world),
            BoneOverlay {
                lines: vec![
                    ([10.0, 20.0], [10.0, 40.0], BoneLink::Plain),
                    ([10.0, 40.0], [21.0, 20.0], BoneLink::Parent),
                    ([21.0, 20.0], [21.0, 12.0], BoneLink::Child),
                ],
                joints: vec![([10.0, 40.0], false), ([21.0, 20.0], true)],
            }
        );
        assert_eq!(bone_overlay(&Value::Null, &world), BoneOverlay::default());
        assert_eq!(bone_overlay(&json!({"points": [["x"]], "lines": [[[0.0]]]}), &world), BoneOverlay::default());
    }

    #[test]
    fn the_session_bones_become_joints_and_links_in_world_space() {
        let model = json!({
            "meta": {"name": "rig", "version": "1.0-alpha"},
            "nodes": node(1, "Root", "Node", [0.0; 3], 0.0, [1.0, 1.0], json!([
                node(2, "Rig", "DepthRigRoot", [100.0, 50.0, 0.0], 0.0, [1.0, 1.0], json!([])),
            ])),
            "param": [], "animations": {},
        });
        let mut session = Session::new();
        assert_eq!(session.handle_request(&json!({"op": "new", "model": model}))["succeeded"], true);
        let mut bone = |parent: u64, name: &str, head: [f64; 3], tail: [f64; 3]| {
            let made = session.handle_request(&json!({
                "op": "node.create", "className": "DepthBone", "name": name, "parents": [parent],
                "restHead": head, "restTail": tail,
            }));
            let id = made["result"]["created"][0]["uuid"].as_u64().unwrap();
            let moved = session.handle_request(&json!({"op": "node.transform", "id": id, "translation": head}));
            assert_eq!(moved["succeeded"], true, "{moved}");
            id
        };
        let hip = bone(2, "Hip", [0.0, 0.0, 0.0], [0.0, 100.0, 0.0]);
        let leg = bone(hip, "Leg", [0.0, 100.0, 0.0], [0.0, 200.0, 0.0]);
        let mut viewport = ViewportState::new();
        {
            let mut core = core(&mut session);
            viewport.sync_scene(&mut core, true, false);
            let nodes = core(&json!({"op": "snapshot"}))["result"]["model"]["nodes"].clone();
            viewport.sync_rig(&mut core, rig_of(&nodes, leg));
        }
        let ((root, bone_id), data) = viewport.scene.rig.clone().expect("the rig was read");
        assert_eq!((root, bone_id), (2, Some(leg as u32)));
        let world = viewport.world_matrix(2).unwrap();
        let overlay = bone_overlay(&data, &world);
        // The rig sits at (100, 50): the hip joint is there, the leg joint 100 further along the bone.
        assert_eq!(overlay.joints.len(), 2);
        assert!(overlay.joints.iter().any(|(p, selected)| !*selected && (p[0] - 100.0).abs() < 1e-3 && (p[1] - 50.0).abs() < 1e-3), "{overlay:?}");
        assert!(overlay.joints.iter().any(|(p, selected)| *selected && (p[0] - 100.0).abs() < 1e-3 && (p[1] - 150.0).abs() < 1e-3), "{overlay:?}");
        assert_eq!(overlay.lines.len(), 1);
        assert_eq!(overlay.lines[0].2, BoneLink::Parent, "the selected leg's link to its parent");
        // Selecting a node outside any rig clears it; the same rig is not read twice.
        let mut asked = 0;
        let mut counting = |request: &Value| {
            asked += 1;
            session.handle_request(request)
        };
        viewport.sync_rig(&mut counting, Some((2, Some(leg as u32))));
        viewport.sync_rig(&mut counting, None);
        assert_eq!(asked, 0);
        assert!(viewport.scene.rig.is_none());
    }

    #[test]
    fn the_packet_gives_parts_in_painter_order_and_picking_agrees() {
        let mut session = session();
        let mut state = ViewportState::new();
        state.sync_scene(&mut core(&mut session), true, false);
        let parts = state.scene.parts.clone().unwrap();
        // zsort 1 draws before zsort 0, so Front (5) is last, i.e. on top.
        assert_eq!(parts.iter().map(|p| p.id).collect::<Vec<_>>(), [4, 5]);
        assert_eq!(parts[1].verts, vec![[0.0, 0.0], [20.0, 0.0], [0.0, 20.0]]);
        assert_eq!(hit_test(&parts, [2.0, 2.0]), Some(5));
        assert_eq!(hit_test(&parts, [30.0, 5.0]), Some(4));
        assert_eq!(hit_test(&parts, [30.0, 30.0]), None);
        let matrices = state.scene.matrices.clone().unwrap();
        close2([matrices[&3][0][3], matrices[&3][1][3]], [100.0 - 10.0, 50.0 + 20.0]);
    }

    #[test]
    fn a_screen_drag_moves_the_node_under_the_cursor_and_commits_once() {
        let mut session = session();
        let mut state = ViewportState::new();
        state.camera = Camera { centre: [0.0, 0.0], zoom: 2.0, mirror: false };
        state.sync_scene(&mut core(&mut session), true, false);
        let nodes = session.snapshot()["model"]["nodes"].clone();
        let info = node_info(&nodes, 3).unwrap();
        assert_eq!((info.parent, info.root, info.subtree.clone()), (Some(2), 1, vec![3]));
        let sel = sel_view(info, state.scene.matrices.as_ref().unwrap(), state.scene.parts.as_deref()).unwrap();
        close2(sel.origin, [90.0, 70.0]);

        // Drag 40 screen points to the right at zoom 2: 20 world units.
        let size = [400.0, 400.0];
        let press = state.camera.to_screen(size, sel.origin);
        let mut drag = begin_drag(Tool::Move, &sel, state.camera.to_world(size, press)).unwrap();
        drag_update(&mut drag, state.camera.to_world(size, [press[0] + 40.0, press[1]]), Mods::default());
        let request = drag_request(&drag).unwrap();
        assert_eq!(request, json!({"op": "node.transform", "id": 3, "translation": [10.0, -5.0, 0.0]}));
        let before = session.snapshot()["revision"].as_u64().unwrap();
        let response = session.handle_request(&request);
        assert_eq!(response["succeeded"], true, "{response}");
        assert_eq!(session.snapshot()["revision"].as_u64().unwrap(), before + 1, "one revision per gesture");
        let stored = &session.snapshot()["model"]["nodes"]["children"][0]["children"][0]["transform"]["trans"];
        assert_eq!(*stored, json!([10.0, -5.0, 0.0]));
        // The world origin moved by exactly the 20 units the pointer travelled.
        state.invalidate_scene();
        state.sync_scene(&mut core(&mut session), true, false);
        let after = &state.scene.matrices.as_ref().unwrap()[&3];
        close2([after[0][3], after[1][3]], [110.0, 70.0]);
    }

    // ------------------------------------------------------------ headless egui frames

    use egui::accesskit::Role;

    struct Harness {
        ctx: egui::Context,
        state: ViewportState,
        session: Session,
        selected: Option<u64>,
        selectable: bool,
        menu: bool,
        requests: Vec<Value>,
        selection: Vec<u32>,
        bookmarks: Vec<BookmarkPick>,
        controllers: Vec<ControllerPick>,
        forms: Vec<FormPick>,
    }

    impl Harness {
        fn new() -> Self {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut state = ViewportState::new();
            state.camera = Camera { centre: [100.0, 60.0], zoom: 2.0, mirror: false };
            let mut harness = Self {
                ctx,
                state,
                session: session(),
                selected: Some(3),
                selectable: true,
                menu: true,
                requests: Vec::new(),
                selection: Vec::new(),
                bookmarks: Vec::new(),
                controllers: Vec::new(),
                forms: Vec::new(),
            };
            // Two frames: layout, then stable positions.
            harness.frame(Vec::new());
            harness.frame(Vec::new());
            harness
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(400.0, 400.0))),
                events,
                ..Default::default()
            };
            let (state, session, selected) = (&mut self.state, &mut self.session, self.selected);
            let (selectable, menu) = (self.selectable, self.menu);
            let (mut requests, mut picked, mut bookmarks) = (Vec::new(), Vec::new(), Vec::new());
            let mut controllers = Vec::new();
            let mut forms = Vec::new();
            let output = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
                    let (rect, response) = state.allocate(ui);
                    state.sync_scene(&mut core(session), selected.is_some(), false);
                    let nodes = session.snapshot()["model"]["nodes"].clone();
                    let input = Input {
                        nodes: &nodes,
                        selected,
                        selectable,
                        handles: true,
                        zoom_to_cursor: true,
                        zoom_speed: 5.0,
                        menu,
                    };
                    let out = state.interact(ui, rect, &response, &input);
                    requests.extend(out.requests);
                    requests.extend(out.editor_calls);
                    picked.extend(out.select);
                    bookmarks.extend(out.bookmark);
                    controllers.extend(out.controller);
                    state.paint(ui, rect, None, &input);
                    bookmarks.extend(state.bookmark_labels(ui, rect));
                    controllers.extend(state.controller_handles(ui, rect));
                    forms.extend(state.form_handles(ui, rect));
                });
            });
            self.requests.extend(requests);
            self.selection.extend(picked);
            self.bookmarks.extend(bookmarks);
            self.controllers.extend(controllers);
            self.forms.extend(forms);
            output
        }
    }

    fn button(pos: Pos2, pressed: bool) -> egui::Event {
        let (button, modifiers) = (egui::PointerButton::Primary, egui::Modifiers::NONE);
        egui::Event::PointerButton { pos, button, pressed, modifiers }
    }

    fn accesskit_nodes(output: &egui::FullOutput) -> Vec<egui::accesskit::Node> {
        let update = output.platform_output.accesskit_update.clone().expect("accesskit is enabled");
        update.nodes.into_iter().map(|(_, node)| node).collect()
    }

    #[test]
    fn dragging_the_move_handle_sends_one_request_on_release() {
        let mut harness = Harness::new();
        let size = harness.state.size;
        assert_eq!(size, [400.0, 400.0]);
        // Node 3 sits at world (90, 70); the bounds are that point, so the move square is up-left of it.
        let origin = harness.state.camera.to_screen(size, [90.0, 70.0]);
        let handle = Pos2::new(origin[0] as f32 - 12.0, origin[1] as f32 - 12.0);
        let moved = handle + egui::vec2(40.0, 0.0);
        harness.frame(vec![egui::Event::PointerMoved(handle), button(handle, true)]);
        harness.frame(vec![egui::Event::PointerMoved(handle + egui::vec2(20.0, 0.0))]);
        harness.frame(vec![egui::Event::PointerMoved(moved)]);
        assert!(harness.requests.is_empty(), "nothing is sent while the button is down");
        harness.frame(vec![button(moved, false)]);
        assert_eq!(
            harness.requests,
            [json!({"op": "node.transform", "id": 3, "translation": [10.0, -5.0, 0.0]})]
        );
        assert!(harness.state.drag.is_none());
        let response = harness.session.handle_request(&harness.requests[0]);
        assert_eq!(response["succeeded"], true, "{response}");
    }

    #[test]
    fn clicking_a_part_selects_the_topmost_one() {
        let mut harness = Harness::new();
        harness.selected = None;
        harness.frame(Vec::new());
        let (camera, size) = (harness.state.camera, harness.state.size);
        let at = |world: V2| {
            let s = camera.to_screen(size, world);
            Pos2::new(s[0] as f32, s[1] as f32)
        };
        let front = at([4.0, 4.0]);
        harness.frame(vec![egui::Event::PointerMoved(front), button(front, true), button(front, false)]);
        // The first frame fetches the geometry; the click resolves on the next one.
        harness.frame(Vec::new());
        assert_eq!(harness.selection, [5]);
        let back = at([30.0, 5.0]);
        harness.frame(vec![egui::Event::PointerMoved(back), button(back, true), button(back, false)]);
        harness.frame(Vec::new());
        assert_eq!(harness.selection, [5, 4]);
        let empty = at([30.0, 30.0]);
        harness.frame(vec![egui::Event::PointerMoved(empty), button(empty, true), button(empty, false)]);
        harness.frame(Vec::new());
        assert_eq!(harness.selection, [5, 4], "a click on nothing selects nothing");

        // A lock set after the geometry was read holds at once: the click goes through the front Part, and a
        // lock on the root leaves nothing to pick.
        let lock = |harness: &mut Harness, id: u64, locked: bool| {
            let response = harness.session.handle_request(&json!({"op": "node.lock.set", "nodes": [id], "locked": locked}));
            assert_eq!(response["succeeded"], true, "{response}");
        };
        lock(&mut harness, 5, true);
        let both = at([2.0, 2.0]);
        harness.frame(vec![egui::Event::PointerMoved(both), button(both, true), button(both, false)]);
        harness.frame(Vec::new());
        assert_eq!(harness.selection, [5, 4, 4]);
        let root = harness.session.snapshot()["model"]["nodes"]["uuid"].as_u64().unwrap();
        lock(&mut harness, root, true);
        harness.frame(vec![egui::Event::PointerMoved(front), button(front, true), button(front, false)]);
        harness.frame(Vec::new());
        assert_eq!(harness.selection, [5, 4, 4]);
        lock(&mut harness, root, false);
        lock(&mut harness, 5, false);
        harness.frame(vec![egui::Event::PointerMoved(front), button(front, true), button(front, false)]);
        harness.frame(Vec::new());
        assert_eq!(harness.selection, [5, 4, 4, 5]);
    }

    #[test]
    fn viewport_controls_have_accessibility_names() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut state = ViewportState::new();
        let run = |state: &mut ViewportState, armed: bool| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(900.0, 200.0))),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| ui.horizontal_wrapped(|ui| state.controls(ui, armed)));
                },
            )
        };
        let output = run(&mut state, false);
        let mut texts = Vec::new();
        let mut controls = 0;
        for node in accesskit_nodes(&output) {
            let role = node.role();
            if matches!(role, Role::Button | Role::CheckBox | Role::Slider | Role::SpinButton | Role::TextInput) {
                controls += 1;
                let named = node.label().is_some_and(|label| !label.is_empty()) || !node.labelled_by().is_empty();
                assert!(named, "unlabelled {:?} control {:?}", node.role(), node.value());
            }
            texts.extend([node.label(), node.value()].into_iter().flatten().map(str::to_owned));
        }
        assert!(controls >= 6, "only {controls} controls were drawn");
        for expected in [
            "Zoom", "100%", "x = 0.00 y = 0.00", "Reset view", "Show vertices", "Show bounds", "Show orientation gizmo",
            "Background colour", "Reset background", "Grid spacing", "Grid colour", "Inactive colour",
        ] {
            assert!(texts.iter().any(|text| text == expected), "missing {expected:?} in {texts:?}");
        }
        // With an armed parameter D hides the gizmo toggles.
        let armed = run(&mut state, true);
        let texts: Vec<String> = accesskit_nodes(&armed).iter().filter_map(|n| n.label().map(str::to_owned)).collect();
        assert!(!texts.iter().any(|text| text == "Show vertices"), "{texts:?}");
        assert!(texts.iter().any(|text| text == "Reset view"));
    }

    #[test]
    fn handle_squares_are_named_for_assistive_tools() {
        let mut harness = Harness::new();
        let output = harness.frame(Vec::new());
        let nodes = accesskit_nodes(&output);
        let labels: Vec<String> = nodes.iter().filter_map(|n| n.label().map(str::to_owned)).collect();
        for expected in ["Move handle", "Rotate handle", "Scale handle", "Model viewport"] {
            assert!(labels.iter().any(|l| l == expected), "missing {expected:?} in {labels:?}");
        }
    }

    // ----------------------------------------------------------- scene.geometry, mirror, toolbar

    /// Records the ops a `sync_scene` sends and answers them from a real session.
    fn recording(session: &mut Session, ops: &mut Vec<String>, request: &Value) -> Value {
        ops.push(request["op"].as_str().unwrap_or("").to_owned());
        session.handle_request(request)
    }

    #[test]
    fn the_geometry_read_is_one_scene_geometry_request_and_never_a_packet() {
        let mut session = session();
        let mut state = ViewportState::new();
        let mut ops = Vec::new();
        state.sync_scene(&mut |request| recording(&mut session, &mut ops, request), true, false);
        assert_eq!(ops, ["scene.evaluate", "scene.geometry"]);
        let parts = state.scene.parts.clone().unwrap();
        assert_eq!(parts.iter().map(|p| p.id).collect::<Vec<_>>(), [4, 5]);
        assert_eq!(parts[0].tris, vec![[0, 1, 2]]);
        // While physics runs the geometry is not read again, but the matrices are.
        state.invalidate_scene();
        state.physics = true;
        ops.clear();
        state.sync_scene(&mut |request| recording(&mut session, &mut ops, request), true, false);
        assert_eq!(ops, ["scene.evaluate"]);
    }

    #[test]
    fn scene_geometry_results_are_validated() {
        let ok = json!({"parts": [{"id": 3, "vertices": [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]], "indices": [0, 1, 2], "zsort": 0.5}]});
        let parts = parse_geometry(&ok).unwrap();
        let expected =
            PartGeom { id: 3, verts: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]], tris: vec![[0, 1, 2]], locked: false };
        assert_eq!(parts, [expected]);
        assert_eq!(parse_geometry(&json!({"parts": []})).unwrap(), Vec::<PartGeom>::new());
        assert!(parse_geometry(&json!({})).is_err());
        let bad_index = json!({"parts": [{"id": 3, "vertices": [[0.0, 0.0]], "indices": [0, 0, 5]}]});
        assert_eq!(parse_geometry(&bad_index).unwrap_err(), "part index is out of range");
        let bad_id = json!({"parts": [{"id": -1, "vertices": [], "indices": []}]});
        assert_eq!(parse_geometry(&bad_id).unwrap_err(), "part id must be a u32");
    }

    #[test]
    fn a_mirrored_camera_maps_the_pointer_from_the_other_side() {
        let size = [400.0, 400.0];
        let plain = Camera { centre: [100.0, 60.0], zoom: 2.0, mirror: false };
        let mirrored = Camera { mirror: true, ..plain };
        assert_eq!(plain.to_world(size, [8.0, 88.0]), [4.0, 4.0]);
        assert_eq!(mirrored.to_world(size, [392.0, 88.0]), [4.0, 4.0]);
        assert_eq!(mirrored.to_world(size, [8.0, 88.0]), [196.0, 4.0]);
        assert_eq!(mirrored.to_screen(size, [4.0, 4.0]), [392.0, 88.0]);
        assert_eq!(mirrored.to_screen(size, [100.0, 60.0]), [200.0, 200.0], "the centre stays put");
        // The view bounds are those of the unmirrored picture, which is what gets rendered.
        assert_eq!(mirrored.view_bounds(size), plain.view_bounds(size));
        // Right-drag pan still moves the picture with the pointer: world (4, 4) goes 10 points right.
        let mut panned = mirrored;
        panned.pan([10.0, 0.0]);
        assert_eq!(panned.centre, [105.0, 60.0]);
        assert_eq!(panned.to_screen(size, [4.0, 4.0]), [402.0, 88.0]);
        // Zoom to cursor keeps the world point under the cursor.
        let mut zoomed = mirrored;
        let cursor = [300.0, 100.0];
        let under = zoomed.to_world(size, cursor);
        zoomed.zoom_step(size, Some(cursor), 1.0, 5.0);
        close(zoomed.zoom, 2.2);
        close2(zoomed.to_world(size, cursor), under);
        assert!(zoomed.mirror);
    }

    #[test]
    fn mirroring_is_view_only_and_survives_fit_and_reset() {
        let mut state = ViewportState::new();
        state.mark_rendered();
        assert!(!state.needs_render());
        state.camera.mirror = true;
        assert!(!state.needs_render(), "a mirrored picture is the same render");
        state.camera.zoom = 2.0;
        assert!(state.needs_render());
        state.mark_rendered();
        state.fit([0.0, 0.0, 400.0, 200.0]);
        assert!(state.camera.mirror);
        assert_eq!((state.camera.centre, state.camera.zoom), ([200.0, 100.0], 512.0 / 400.0));
        state.set_view(Camera::HOME);
        assert_eq!(state.camera, Camera { mirror: true, ..Camera::HOME });
    }

    #[test]
    fn clicking_in_a_mirrored_view_picks_the_part_at_the_mirrored_place() {
        let mut harness = Harness::new();
        harness.selected = None;
        harness.state.camera.mirror = true;
        harness.frame(Vec::new());
        // Front is the triangle (0,0)-(20,0)-(0,20); world (4, 4) is at screen (8, 88) unmirrored.
        let click = |harness: &mut Harness, at: Pos2| {
            harness.frame(vec![egui::Event::PointerMoved(at), button(at, true), button(at, false)]);
            harness.frame(Vec::new());
        };
        click(&mut harness, Pos2::new(392.0, 88.0));
        assert_eq!(harness.selection, [5], "the mirrored place selects the part");
        click(&mut harness, Pos2::new(8.0, 88.0));
        assert_eq!(harness.selection, [5], "the unmirrored place is empty now, so nothing new is selected");
    }

    #[test]
    fn dragging_the_move_handle_in_a_mirrored_view_follows_the_pointer_on_screen() {
        let mut harness = Harness::new();
        harness.state.camera.mirror = true;
        harness.frame(Vec::new());
        harness.frame(Vec::new());
        let size = harness.state.size;
        // Node 3 sits at world (90, 70): 20 points right of the middle when mirrored, 20 points below it.
        let origin = harness.state.camera.to_screen(size, [90.0, 70.0]);
        assert_eq!(origin, [220.0, 220.0]);
        let handle = Pos2::new(origin[0] as f32 - 12.0, origin[1] as f32 - 12.0);
        let moved = handle + egui::vec2(40.0, 0.0);
        harness.frame(vec![egui::Event::PointerMoved(handle), button(handle, true)]);
        harness.frame(vec![egui::Event::PointerMoved(moved)]);
        harness.frame(vec![button(moved, false)]);
        // 40 points to the right on screen is 20 world units to the left in a mirrored view. The unmirrored
        // drag of the same distance gives local y = -5; the group is turned 90 degrees and doubled, so the
        // opposite world direction gives local y = 15.
        assert_eq!(harness.requests, [json!({"op": "node.transform", "id": 3, "translation": [10.0, 15.0, 0.0]})]);
    }

    #[test]
    fn trajectories_are_read_once_for_an_animation_part_and_revision() {
        let calls = std::cell::RefCell::new(Vec::<Value>::new());
        let answer = json!({"succeeded": true, "result": {"paths": [{"vertex": 0, "points": [[0.0, 0.0], [1.0, 2.0]]}]}});
        let mut core = |request: &Value| {
            calls.borrow_mut().push(request.clone());
            answer.clone()
        };
        let mut state = ViewportState::new();
        state.sync_trajectory(&mut core, Some("Idle"), Some(2), 1);
        assert!(calls.borrow().is_empty() && state.scene.trajectory.is_none(), "off by default");

        state.trajectories = true;
        state.sync_trajectory(&mut core, Some("Idle"), Some(2), 1);
        state.sync_trajectory(&mut core, Some("Idle"), Some(2), 1);
        assert_eq!(*calls.borrow(), [json!({"op": "animation.trajectory", "name": "Idle", "node": 2})], "asked once");
        assert_eq!(state.scene.trajectory.as_ref().unwrap().1, vec![vec![[0.0, 0.0], [1.0, 2.0]]]);

        state.sync_trajectory(&mut core, Some("Idle"), Some(2), 2);
        state.sync_trajectory(&mut core, Some("Walk"), Some(2), 2);
        state.sync_trajectory(&mut core, Some("Walk"), Some(3), 2);
        assert_eq!(calls.borrow().len(), 4, "a new revision, animation or Part asks again");
        state.sync_trajectory(&mut core, None, Some(3), 2);
        assert!(state.scene.trajectory.is_none(), "no previewed animation");
        state.sync_trajectory(&mut core, Some("Walk"), None, 2);
        assert!(state.scene.trajectory.is_none(), "no selection");
    }

    #[test]
    fn a_refused_trajectory_is_kept_empty_and_not_asked_again() {
        let calls = std::cell::Cell::new(0);
        let mut core = |_: &Value| {
            calls.set(calls.get() + 1);
            json!({"succeeded": false, "message": "node 1 draws no mesh"})
        };
        let mut state = ViewportState::new();
        state.trajectories = true;
        for _ in 0..3 {
            state.sync_trajectory(&mut core, Some("Idle"), Some(1), 1);
        }
        assert_eq!(calls.get(), 1);
        assert!(state.scene.trajectory.as_ref().unwrap().1.is_empty());
    }

    #[test]
    fn the_physics_request_follows_the_toggle_and_the_frame_delta() {
        let mut state = ViewportState::new();
        state.mark_rendered();
        assert_eq!(state.physics_request(1.0 / 60.0), None, "off by default");
        assert!(!state.needs_render());
        state.physics = true;
        assert_eq!(state.physics_request(0.25), Some(json!({"op": "physics.step", "dt": 0.1})), "a stall is cut");
        assert!(state.needs_render(), "the pose changes every frame");
        assert_eq!(state.physics_request(0.0), Some(json!({"op": "physics.step", "dt": 0.0})));
        assert_eq!(state.physics_request(f64::NAN), None);
        assert_eq!(state.physics_request(0.04), Some(json!({"op": "physics.step", "dt": 0.04})));
    }

    #[test]
    fn reset_parameters_sends_one_value_set_per_changed_parameter_including_grouped_ones() {
        let model = json!({
            "meta": {"name": "params", "version": "1.0-alpha"},
            "nodes": node(1, "Root", "Node", [0.0; 3], 0.0, [1.0, 1.0], json!([])),
            "param": [
                {"uuid": 17, "name": "A", "is_vec2": true, "min": [-1.0, -1.0], "max": [1.0, 1.0],
                 "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0, 1.0]], "bindings": []},
                {"groupUUID": 20, "name": "Group", "children": [
                    {"uuid": 18, "name": "B", "is_vec2": false, "min": [0.0, 0.0], "max": [1.0, 0.0],
                     "defaults": [0.5, 0.0], "axis_points": [[0.0, 1.0], [0.0]], "bindings": []},
                    {"uuid": 19, "name": "C", "is_vec2": false, "min": [0.0, 0.0], "max": [1.0, 0.0],
                     "defaults": [1.0, 0.0], "axis_points": [[0.0, 1.0], [0.0]], "bindings": []},
                ]},
            ],
            "animations": {},
        });
        let mut session = Session::new();
        assert_eq!(session.handle_request(&json!({"op": "new", "model": model}))["succeeded"], true);
        assert_eq!(reset_parameter_requests(&session.snapshot()), Vec::<Value>::new(), "defaults need nothing");
        for (id, value) in [(17, [0.5, -0.5]), (18, [0.25, 0.0])] {
            let reply = session.handle_request(&json!({"op": "value.set", "id": id, "value": value}));
            assert_eq!(reply["succeeded"], true, "{reply}");
        }
        let requests = reset_parameter_requests(&session.snapshot());
        assert_eq!(
            requests,
            [
                json!({"op": "value.set", "id": 17, "value": [0.0, 0.0]}),
                json!({"op": "value.set", "id": 18, "value": [0.5, 0.0]}),
            ]
        );
        for request in &requests {
            assert_eq!(session.handle_request(request)["succeeded"], true);
        }
        let values = session.snapshot()["values"].clone();
        assert_eq!((values["17"].clone(), values["18"].clone(), values["19"].clone()), (json!([0.0, 0.0]), json!([0.5, 0.0]), json!([1.0, 0.0])));
        assert!(reset_parameter_requests(&session.snapshot()).is_empty());
    }

    #[test]
    fn physics_step_and_reset_are_accepted_by_the_session() {
        let mut session = session();
        let mut state = ViewportState::new();
        state.physics = true;
        let step = state.physics_request(1.0 / 60.0).unwrap();
        assert_eq!(session.handle_request(&step)["succeeded"], true);
        assert_eq!(session.snapshot()["physics"]["active"], true);
        assert_eq!(session.handle_request(&json!({"op": "physics.reset"}))["succeeded"], true);
        assert_eq!(session.snapshot()["physics"]["active"], false);
    }

    /// One frame of the toolbar row; returns its requests and the accessibility tree.
    fn toolbar_frame(
        ctx: &egui::Context,
        state: &mut ViewportState,
        snapshot: &Value,
        session_ops: bool,
        events: Vec<egui::Event>,
    ) -> (Vec<Value>, egui::FullOutput) {
        let mut requests = Vec::new();
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(900.0, 120.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| requests.extend(state.toolbar(ui, snapshot, session_ops).into_iter().map(|request| {
                        match request {
                            crate::editor_windows::Request::Core(value) | crate::editor_windows::Request::Mcp(_, value) => value,
                            _ => panic!("unexpected toolbar request"),
                        }
                    })));
                });
            },
        );
        (requests, output)
    }

    fn centre_of(output: &egui::FullOutput, label: &str) -> Pos2 {
        let node = accesskit_nodes(output)
            .into_iter()
            .find(|node| node.label() == Some(label))
            .unwrap_or_else(|| panic!("no control named {label:?}"));
        let bounds = node.bounds().expect("the control has a place");
        Pos2::new(((bounds.x0 + bounds.x1) / 2.0) as f32, ((bounds.y0 + bounds.y1) / 2.0) as f32)
    }

    fn click_events(at: Pos2) -> Vec<egui::Event> {
        vec![egui::Event::PointerMoved(at), button(at, true), button(at, false)]
    }

    fn tabs_frame(ctx: &egui::Context, state: &mut ViewportState, snapshot: &Value, events: Vec<egui::Event>)
        -> (Vec<Value>, egui::FullOutput) {
        let mut answer = Vec::new();
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(900.0, 120.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    answer = state.view_tabs(ui, snapshot).into_iter().map(|request| match request {
                        crate::editor_windows::Request::Core(value) | crate::editor_windows::Request::Mcp(_, value) => value,
                        _ => panic!("unexpected view tab request"),
                    }).collect();
                });
            },
        );
        (answer, output)
    }

    #[test]
    fn each_view_tab_keeps_its_camera_and_sends_the_view_requests() {
        let mut session = session();
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut state = ViewportState::new();
        let at = |x: f64| Camera { centre: [x, 0.0], zoom: 2.0, mirror: false };
        // Click a control by its name, send what it asks for, and lay out the next frame.
        let click = |state: &mut ViewportState, session: &mut Session, label: &str| {
            let snapshot = session.snapshot();
            let (_, laid_out) = tabs_frame(&ctx, state, &snapshot, Vec::new());
            let (requests, _) = tabs_frame(&ctx, state, &snapshot, click_events(centre_of(&laid_out, label)));
            for request in &requests {
                if request["method"] == "tools/call" {
                    assert_eq!(request, &crate::editor_windows::call_request("editor_camera_sync", json!({})));
                    state.sync_cameras(&session.snapshot());
                } else {
                    assert_eq!(session.handle_request(request)["succeeded"], true, "{request}");
                }
            }
            let (_, output) = tabs_frame(&ctx, state, &session.snapshot(), Vec::new());
            (requests, output)
        };

        let (_, output) = tabs_frame(&ctx, &mut state, &session.snapshot(), Vec::new());
        let labels: Vec<String> =
            accesskit_nodes(&output).iter().filter_map(|n| n.label().map(str::to_owned)).collect();
        for expected in ["View 1", "Add view", "Remove view", "Synchronize parameters", "Same display position"] {
            assert!(labels.iter().any(|l| l == expected), "missing {expected:?} in {labels:?}");
        }
        state.camera = at(1.0);
        let (requests, _) = click(&mut state, &mut session, "Add view");
        assert_eq!(requests, [json!({"op": "view.add"})]);
        // A new view starts where the last one was, and moves on its own.
        assert_eq!(state.camera, at(1.0));
        state.camera = at(2.0);
        let (requests, _) = click(&mut state, &mut session, "View 1");
        assert_eq!(requests, [json!({"op": "view.select", "id": 1})]);
        assert_eq!(state.camera, at(1.0));
        // The active tab sends nothing.
        state.camera = at(3.0);
        let (requests, _) = click(&mut state, &mut session, "View 1");
        assert_eq!((requests.len(), state.camera), (0, at(3.0)));
        // A switch made by a tool brings the camera of that view back too.
        session.handle_request(&json!({"op": "view.select", "id": 2}));
        tabs_frame(&ctx, &mut state, &session.snapshot(), Vec::new());
        assert_eq!(state.camera, at(2.0));

        // Same display position gives every view this camera.
        let (requests, _) = click(&mut state, &mut session, "Same display position");
        assert_eq!(requests, [crate::editor_windows::call_request("editor_camera_sync", json!({}))]);
        state.camera = at(4.0);
        click(&mut state, &mut session, "View 1");
        assert_eq!(state.camera, at(2.0));

        let (requests, _) = click(&mut state, &mut session, "Synchronize parameters");
        assert_eq!(requests, [json!({"op": "view.sync.set", "synchronized": true})]);
        assert_eq!(session.snapshot()["views"]["synchronized"], true);
        let (requests, output) = click(&mut state, &mut session, "Remove view");
        assert_eq!(requests, [json!({"op": "view.remove", "id": 1})]);
        assert_eq!(session.snapshot()["views"]["active"], 2);
        // One view left: it cannot be removed.
        let remove = click_events(centre_of(&output, "Remove view"));
        let (requests, _) = tabs_frame(&ctx, &mut state, &session.snapshot(), remove);
        assert!(requests.is_empty());

        // A removed view's camera goes with it: a new view that gets its id starts where the shown one is.
        state.camera = at(7.0);
        click(&mut state, &mut session, "Add view");
        state.camera = at(8.0);
        click(&mut state, &mut session, "Remove view");
        assert_eq!(state.camera, at(7.0));
        click(&mut state, &mut session, "Add view");
        assert_eq!((session.snapshot()["views"]["active"].clone(), state.camera), (json!(3), at(7.0)));
        // Also when the new view comes in the frame right after the removal.
        state.camera = at(6.0);
        session.handle_request(&json!({"op": "view.remove", "id": 3}));
        tabs_frame(&ctx, &mut state, &session.snapshot(), Vec::new());
        assert_eq!(state.camera, at(7.0));
        session.handle_request(&json!({"op": "view.add"}));
        tabs_frame(&ctx, &mut state, &session.snapshot(), Vec::new());
        assert_eq!((session.snapshot()["views"]["active"].clone(), state.camera), (json!(3), at(7.0)));

        // Another document keeps the shown camera, not the one view 1 had in the last document.
        let mut state = ViewportState::new();
        let mut other = self::session();
        state.camera = at(1.0);
        click(&mut state, &mut other, "Add view");
        state.camera = at(2.0);
        state.forget_views();
        tabs_frame(&ctx, &mut state, &self::session().snapshot(), Vec::new());
        assert_eq!(state.camera, at(2.0));
        let unseen = json!({"documentFormat":"inx","views":{"active":1,"views":[{"id":1},{"id":2}]}});
        state.reconcile_views(&unseen);
        assert_eq!(state.view_cameras(&unseen)["views"][1]["camera"], Value::Null);
    }

    #[test]
    fn the_toolbar_buttons_are_named_and_send_their_requests() {
        let mut session = session();
        session.handle_request(&json!({"op": "new", "model": {
            "meta": {"name": "p", "version": "1.0-alpha"},
            "nodes": node(1, "Root", "Node", [0.0; 3], 0.0, [1.0, 1.0], json!([])),
            "param": [{"uuid": 17, "name": "A", "is_vec2": true, "min": [-1.0, -1.0], "max": [1.0, 1.0],
                       "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0, 1.0]], "bindings": []}],
            "animations": {}}}));
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut state = ViewportState::new();
        let snapshot = session.snapshot();
        let (_, first) = toolbar_frame(&ctx, &mut state, &snapshot, true, Vec::new());
        let (_, laid_out) = toolbar_frame(&ctx, &mut state, &snapshot, true, Vec::new());
        let labels: Vec<String> = accesskit_nodes(&laid_out).iter().filter_map(|n| n.label().map(str::to_owned)).collect();
        let toggles =
            ["Mirror View", "Show Grid", "Guide Settings", "Solo", "Enable physics", "Reset physics", "Reset parameters", "Onion Slice", "Trajectories"];
        for expected in toggles {
            assert!(labels.iter().any(|l| l == expected), "missing {expected:?} in {labels:?}");
        }
        for node in accesskit_nodes(&first) {
            if matches!(node.role(), Role::Button | Role::CheckBox | Role::Switch) {
                assert!(node.label().is_some_and(|label| !label.is_empty()), "unnamed {:?}", node.role());
            }
        }

        // Show Grid turns the grid of the view on and off, and sends nothing.
        let at = centre_of(&laid_out, "Show Grid");
        let (requests, _) = toolbar_frame(&ctx, &mut state, &snapshot, true, click_events(at));
        assert!(requests.is_empty());
        assert_eq!((state.grid.show, state.camera.mirror), (true, false));
        toolbar_frame(&ctx, &mut state, &snapshot, true, click_events(at));
        assert!(!state.grid.show);
        let at = centre_of(&laid_out, "Guide Settings");
        let (requests, _) = toolbar_frame(&ctx, &mut state, &snapshot, true, click_events(at));
        assert!(!state.guides_open, "the toolbar emits the shared call before changing state");
        assert_eq!(requests, [crate::editor_windows::call_request("editor_guides_set", json!({"open":true}))]);

        // Reset physics is one physics.reset request.
        let at = centre_of(&laid_out, "Reset physics");
        let (requests, _) = toolbar_frame(&ctx, &mut state, &snapshot, true, click_events(at));
        assert_eq!(requests, [json!({"op": "physics.reset"})]);
        // Reset parameters is disabled while every parameter is at its default.
        let at = centre_of(&laid_out, "Reset parameters");
        let (requests, _) = toolbar_frame(&ctx, &mut state, &snapshot, true, click_events(at));
        assert!(requests.is_empty());
        session.handle_request(&json!({"op": "value.set", "id": 17, "value": [0.5, 0.5]}));
        let moved = session.snapshot();
        let (_, laid_out) = toolbar_frame(&ctx, &mut state, &moved, true, Vec::new());
        let at = centre_of(&laid_out, "Reset parameters");
        let (requests, _) = toolbar_frame(&ctx, &mut state, &moved, true, click_events(at));
        assert_eq!(requests, [json!({"op": "value.set", "id": 17, "value": [0.0, 0.0]})]);
        let at = centre_of(&laid_out, "Mirror View");
        let (requests, _) = toolbar_frame(&ctx, &mut state, &moved, true, click_events(at));
        assert!(requests.is_empty() && state.camera.mirror);
        let at = centre_of(&laid_out, "Enable physics");
        let (requests, _) = toolbar_frame(&ctx, &mut state, &moved, true, click_events(at));
        assert!(!state.physics, "the toolbar emits the shared call before changing state");
        assert_eq!(requests, [crate::editor_windows::call_request("editor_physics_preview_set", json!({"enabled":true}))]);
        // Onion Slice asks for a new picture, since the picture shows other frames now. Trajectories need none.
        state.mark_rendered();
        let at = centre_of(&laid_out, "Trajectories");
        let (requests, _) = toolbar_frame(&ctx, &mut state, &moved, true, click_events(at));
        assert!(requests.is_empty() && state.trajectories && !state.needs_render());
        let at = centre_of(&laid_out, "Onion Slice");
        let (requests, _) = toolbar_frame(&ctx, &mut state, &moved, true, click_events(at));
        assert!(requests.is_empty() && state.onion && state.needs_render());
    }

    #[test]
    fn documents_without_session_physics_get_only_the_view_toggle() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut state = ViewportState::new();
        let snapshot = Session::new().snapshot();
        toolbar_frame(&ctx, &mut state, &snapshot, false, Vec::new());
        let (_, output) = toolbar_frame(&ctx, &mut state, &snapshot, false, Vec::new());
        let labels: Vec<String> = accesskit_nodes(&output).iter().filter_map(|n| n.label().map(str::to_owned)).collect();
        assert!(labels.iter().any(|l| l == "Mirror View"));
        assert!(!labels.iter().any(|l| l == "Reset physics" || l == "Enable physics"), "{labels:?}");
    }

    // -------------------------------------------------------------------------- curves and overlays

    #[test]
    fn curves_pass_through_their_control_points_with_the_engine_formulas() {
        let line = [[0.0, 0.0], [100.0, 0.0]];
        assert_eq!(curve_point(CurveKind::Spline, &line, 0.25), [25.0, 0.0]);
        let bump = [[0.0, 0.0], [100.0, 100.0], [200.0, 0.0]];
        // The spline goes through its middle point; one quadratic Bezier only leans towards it.
        assert_eq!(curve_point(CurveKind::Spline, &bump, 0.5), [100.0, 100.0]);
        assert_eq!(curve_point(CurveKind::Bezier, &bump, 0.5), [100.0, 50.0]);
        for kind in [CurveKind::Spline, CurveKind::Bezier] {
            assert_eq!(curve_point(kind, &bump, 0.0), [0.0, 0.0]);
            assert_eq!(curve_point(kind, &bump, 1.0), [200.0, 0.0], "the last point ends the curve ({kind:?})");
        }
        assert_eq!(curve_point(CurveKind::Spline, &[[7.0, 8.0]], 0.5), [7.0, 8.0]);
        assert_eq!(curve_point(CurveKind::Spline, &[], 0.5), [0.0, 0.0]);
        assert_eq!(curve_polyline(CurveKind::Spline, &bump, 4).len(), 5);
        assert!(curve_polyline(CurveKind::Spline, &[[1.0, 1.0]], 4).is_empty());
        assert_eq!(CurveKind::of_node(&json!({"curve_type": "Bezier"})), CurveKind::Bezier);
        assert_eq!(CurveKind::of_node(&json!({})), CurveKind::Spline);
    }

    #[test]
    fn the_closest_curve_parameter_finds_the_foot_of_the_perpendicular() {
        let line = [[0.0, 0.0], [100.0, 0.0]];
        assert!((curve_closest_t(CurveKind::Spline, &line, [25.0, 40.0]) - 0.25).abs() < 1e-6);
        assert_eq!(curve_closest_t(CurveKind::Spline, &line, [-30.0, 5.0]), 0.0);
        assert_eq!(curve_closest_t(CurveKind::Spline, &line, [300.0, 5.0]), 1.0);
        let bump = [[0.0, 0.0], [100.0, 100.0], [200.0, 0.0]];
        // Straight above the top of the spline.
        assert!((curve_closest_t(CurveKind::Spline, &bump, [100.0, 300.0]) - 0.5).abs() < 1e-6);
        assert!((curve_closest_t(CurveKind::Bezier, &bump, [100.0, 300.0]) - 0.5).abs() < 1e-6);
        assert_eq!(curve_closest_t(CurveKind::Spline, &[[1.0, 1.0]], [5.0, 5.0]), 0.0);
    }

    fn translated(x: f64, y: f64) -> Mat4 {
        [[1.0, 0.0, 0.0, x], [0.0, 1.0, 0.0, y], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]]
    }

    fn count(overlay: &Overlay, shade: Shade) -> usize {
        overlay.lines.iter().filter(|line| line.2 == shade).count()
    }

    #[test]
    fn a_path_deformer_is_drawn_as_its_rest_and_deformed_curves_with_control_points() {
        let path = json!({"uuid": 6, "type": "PathDeformer", "curve_type": "Spline", "vertices": [0.0, 0.0, 100.0, 0.0, 200.0, 0.0]});
        let rest = node_overlay(&path, &translated(10.0, 20.0), &[], true);
        assert_eq!(count(&rest, Shade::Deformed), 100);
        assert_eq!(count(&rest, Shade::Rest), 0, "nothing deformed, nothing behind it");
        assert_eq!(rest.points, vec![[10.0, 20.0], [110.0, 20.0], [210.0, 20.0]]);
        assert_eq!(rest.lines[0].0, [10.0, 20.0]);
        assert_eq!(rest.lines[99].1, [210.0, 20.0]);
        assert_eq!(rest.lines[49].1, [110.0, 20.0], "t = 0.5 is the middle point");
        let moved = node_overlay(&path, &translated(10.0, 20.0), &[[0.0, 10.0], [0.0, 20.0], [0.0, 30.0]], true);
        assert_eq!(moved.points, vec![[10.0, 30.0], [110.0, 40.0], [210.0, 50.0]]);
        assert_eq!(count(&moved, Shade::Rest), 100);
        assert_eq!(moved.lines.iter().find(|l| l.2 == Shade::Rest).unwrap().0, [10.0, 20.0]);
        assert_eq!(moved.lines.iter().rfind(|l| l.2 == Shade::Deformed).unwrap().1, [210.0, 50.0]);
        // A wrong number of offsets is ignored; a path of one point has no curve.
        assert_eq!(node_overlay(&path, &translated(0.0, 0.0), &[[1.0, 1.0]], true).points[0], [0.0, 0.0]);
        let single = json!({"uuid": 6, "type": "PathDeformer", "vertices": [5.0, 5.0]});
        let overlay = node_overlay(&single, &translated(0.0, 0.0), &[], true);
        assert!(overlay.lines.is_empty() && overlay.points == vec![[5.0, 5.0]]);
        // The Bezier type bends differently from the spline.
        let bezier = json!({"uuid": 6, "type": "PathDeformer", "curve_type": "Bezier", "vertices": [0.0, 0.0, 100.0, 100.0, 200.0, 0.0]});
        assert_eq!(node_overlay(&bezier, &translated(0.0, 0.0), &[], true).lines[49].1, [100.0, 50.0]);
    }

    #[test]
    fn a_grid_deformer_is_drawn_as_its_lattice_and_a_mesh_group_as_its_mesh_when_vertices_show() {
        let grid = json!({"uuid": 7, "type": "GridDeformer", "grid_axis_x": [0.0, 10.0, 20.0], "grid_axis_y": [0.0, 5.0]});
        let overlay = node_overlay(&grid, &translated(100.0, 200.0), &[], false);
        assert_eq!(overlay.lines.len(), 7, "4 horizontal and 3 vertical lines");
        assert!(overlay.lines.iter().all(|line| line.2 == Shade::Lattice));
        assert_eq!(overlay.lines[0], ([100.0, 200.0], [110.0, 200.0], Shade::Lattice));
        let mut offsets = vec![[0.0, 0.0]; 6];
        offsets[0] = [1.0, 2.0];
        let pulled = node_overlay(&grid, &translated(100.0, 200.0), &offsets, false);
        assert_eq!(pulled.lines[0], ([101.0, 202.0], [110.0, 200.0], Shade::Lattice));

        let group = json!({"uuid": 8, "type": "MeshGroup", "mesh": {"verts": [0.0, 0.0, 4.0, 0.0, 0.0, 4.0], "indices": [0, 1, 2], "origin": [0.0, 0.0]}});
        let shown = node_overlay(&group, &translated(1.0, 1.0), &[], true);
        assert_eq!(shown.lines.len(), 3);
        assert_eq!(shown.lines[0], ([1.0, 1.0], [5.0, 1.0], Shade::Mesh));
        assert!(node_overlay(&group, &translated(1.0, 1.0), &[], false).lines.is_empty(), "hidden with the vertices");
        assert_eq!(node_overlay(&json!({"uuid": 2, "type": "Node"}), &translated(0.0, 0.0), &[], true), Overlay::default());
    }

    #[test]
    fn a_simple_physics_driver_shows_its_anchor_and_rest_output_line() {
        let driver = json!({"uuid": 9, "type": "SimplePhysics", "length": 40.0, "local_only": false,
            "transform": {"trans": [3.0, 4.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]}});
        let overlay = node_overlay(&driver, &translated(30.0, 40.0), &[], true);
        assert_eq!(overlay.lines, vec![([30.0, 40.0], [30.0, 80.0], Shade::Anchor)]);
        assert_eq!(overlay.points, vec![[30.0, 40.0]]);
        // `local_only` drivers anchor at their own translation, whatever their parents do.
        let local = json!({"uuid": 9, "type": "SimplePhysics", "local_only": true,
            "transform": {"trans": [3.0, 4.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]}});
        let overlay = node_overlay(&local, &translated(30.0, 40.0), &[], true);
        assert_eq!(overlay.lines, vec![([3.0, 4.0], [3.0, 104.0], Shade::Anchor)], "length defaults to 100");
    }

    fn deformer_session() -> Session {
        let path = json!({"uuid": 6, "name": "Path", "type": "PathDeformer", "enabled": true, "zsort": 0.0,
            "transform": {"trans": [10.0, 20.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
            "lockToRoot": false, "pinToMesh": false, "children": [], "curve_type": "Spline",
            "dynamic_deformation": false, "vertices": [0.0, 0.0, 100.0, 0.0, 200.0, 0.0]});
        let model = json!({
            "meta": {"name": "deformer", "version": "1.0-alpha"},
            "nodes": node(1, "Root", "Node", [0.0; 3], 0.0, [1.0, 1.0], json!([path])),
            "param": [{"uuid": 17, "name": "Bend", "is_vec2": false, "min": [0.0, 0.0], "max": [1.0, 0.0],
                "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0]], "bindings": [{
                    "node": 6, "param_name": "deform", "isSet": [[true], [true]],
                    "values": [[[[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]], [[[0.0, 10.0], [0.0, 20.0], [0.0, 30.0]]]]
                }]}],
            "animations": {},
        });
        let mut session = Session::new();
        let reply = session.handle_request(&json!({"op": "new", "model": model}));
        assert_eq!(reply["succeeded"], true, "{reply}");
        session
    }

    #[test]
    fn the_overlay_follows_the_offsets_the_session_evaluates() {
        let mut session = deformer_session();
        let mut state = ViewportState::new();
        state.sync_scene(&mut core(&mut session), true, false);
        let nodes = session.snapshot()["model"]["nodes"].clone();
        let world = state.scene.matrices.clone().unwrap()[&6];
        let node = locate(&nodes, 6, None).unwrap().0;
        let rest = node_overlay(node, &world, state.scene.offsets.get(&6).map_or(&[][..], Vec::as_slice), true);
        assert_eq!(rest.points, vec![[10.0, 20.0], [110.0, 20.0], [210.0, 20.0]]);
        let reply = session.handle_request(&json!({"op": "value.set", "id": 17, "value": [1.0, 0.0]}));
        assert_eq!(reply["succeeded"], true, "{reply}");
        state.invalidate_scene();
        state.sync_scene(&mut core(&mut session), true, false);
        let offsets = state.scene.offsets.get(&6).cloned().unwrap();
        assert_eq!(offsets.len(), 3);
        close2(offsets[2], [0.0, 30.0]);
        let moved = node_overlay(node, &world, &offsets, true);
        assert_eq!(moved.points.len(), 3);
        close2(moved.points[0], [10.0, 30.0]);
        close2(moved.points[2], [210.0, 50.0]);
    }

    #[test]
    fn grid_lines_lie_on_multiples_of_the_spacing_inside_the_view() {
        let home = Camera::HOME;
        let lines = (vec![-200.0, -100.0, 0.0, 100.0, 200.0], vec![-100.0, 0.0, 100.0]);
        assert_eq!(grid_lines(&home, [400.0, 300.0], 100.0), lines);
        // The view is 830 to 1230 by -190 to 110.
        let moved = Camera { centre: [1030.0, -40.0], zoom: 1.0, mirror: true };
        assert_eq!(grid_lines(&moved, [400.0, 300.0], 100.0), (vec![900.0, 1000.0, 1100.0, 1200.0], vec![-100.0, 0.0, 100.0]));
        // Lines four points apart are drawn, closer ones are not.
        let half = Camera { zoom: 0.5, ..home };
        assert_eq!(grid_lines(&half, [16.0, 16.0], 8.0), (vec![-16.0, -8.0, 0.0, 8.0, 16.0], vec![-16.0, -8.0, 0.0, 8.0, 16.0]));
        assert_eq!(grid_lines(&half, [16.0, 16.0], 7.5), (vec![], vec![]));
        for spacing in [0.0, -100.0, f64::NAN, f64::INFINITY] {
            assert_eq!(grid_lines(&home, [400.0, 300.0], spacing), (vec![], vec![]), "{spacing}");
        }
    }

    #[test]
    fn the_grid_is_drawn_over_the_picture_without_a_selection_and_follows_the_mirror() {
        let colour = Color32::from_rgba_unmultiplied(1, 2, 3, 200);
        let segments = |output: &egui::FullOutput| -> Vec<[Pos2; 2]> {
            output
                .shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::LineSegment { points, stroke } if stroke.color == colour => Some(*points),
                    _ => None,
                })
                .collect()
        };
        let mut harness = Harness::new();
        harness.selected = None;
        harness.state.grid = Grid { show: true, spacing: 50.0, colour: [1, 2, 3, 200] };
        harness.frame(Vec::new());
        let drawn = segments(&harness.frame(Vec::new()));
        let (xs, ys) = grid_lines(&harness.state.camera, harness.state.size(), 50.0);
        assert!(!xs.is_empty() && !ys.is_empty());
        assert_eq!(drawn.len(), xs.len() + ys.len());
        // Each line crosses the whole picture.
        let size = harness.state.size();
        for [a, b] in &drawn[..xs.len()] {
            assert_eq!((a.x, a.y, b.y), (b.x, 0.0, size[1] as f32));
        }
        for [a, b] in &drawn[xs.len()..] {
            assert_eq!((a.y, a.x, b.x), (b.y, 0.0, size[0] as f32));
        }
        let vertical: Vec<f32> = drawn[..xs.len()].iter().map(|[a, _]| a.x).collect();
        let expected = |camera: &Camera| -> Vec<f32> {
            xs.iter().map(|x| camera.to_screen(size, [*x, 0.0])[0] as f32).collect()
        };
        let horizontal: Vec<f32> = drawn[xs.len()..].iter().map(|[a, _]| a.y).collect();
        let camera = harness.state.camera;
        assert_eq!(horizontal, ys.iter().map(|y| camera.to_screen(size, [0.0, *y])[1] as f32).collect::<Vec<f32>>());
        assert_eq!(vertical, expected(&harness.state.camera));
        harness.state.camera.mirror = true;
        harness.frame(Vec::new());
        let mirrored = segments(&harness.frame(Vec::new()));
        let vertical: Vec<f32> = mirrored[..xs.len()].iter().map(|[a, _]| a.x).collect();
        assert_eq!(vertical, expected(&harness.state.camera));
        assert_ne!(expected(&harness.state.camera), expected(&Camera { mirror: false, ..harness.state.camera }));
        harness.state.grid.show = false;
        assert!(segments(&harness.frame(Vec::new())).is_empty());
    }

    #[test]
    fn guides_are_drawn_across_the_picture_and_an_attached_one_in_green() {
        let colour = Color32::from_rgba_unmultiplied(4, 5, 6, 200);
        let segments = |output: &egui::FullOutput, colour: Color32| -> Vec<[Pos2; 2]> {
            output
                .shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::LineSegment { points, stroke } if stroke.color == colour => Some(*points),
                    _ => None,
                })
                .collect()
        };
        let mut harness = Harness::new();
        harness.selected = None;
        let guide = |axis, position| Guide { axis, position, colour: [4, 5, 6, 200], memo: String::new() };
        harness.state.guides.lines =
            vec![guide(GuideAxis::Horizontal, 50.0), guide(GuideAxis::Vertical, 80.0), guide(GuideAxis::Vertical, 120.0)];
        harness.frame(Vec::new());
        let drawn = segments(&harness.frame(Vec::new()), colour);
        let (camera, size) = (harness.state.camera, harness.state.size());
        let at = |world: V2| camera.to_screen(size, world).map(|value| value as f32);
        assert_eq!(drawn.len(), 3);
        assert_eq!(drawn[0], [Pos2::new(0.0, at([0.0, 50.0])[1]), Pos2::new(size[0] as f32, at([0.0, 50.0])[1])]);
        assert_eq!(drawn[1], [Pos2::new(at([80.0, 0.0])[0], 0.0), Pos2::new(at([80.0, 0.0])[0], size[1] as f32)]);
        assert_eq!(drawn[2][0].x, at([120.0, 0.0])[0]);
        harness.state.guides_attached = vec![1];
        let output = harness.frame(Vec::new());
        assert_eq!(segments(&output, colour).len(), 2);
        assert_eq!(segments(&output, GUIDE_ATTACHED), [drawn[1]]);
        harness.state.guides.show = false;
        let output = harness.frame(Vec::new());
        assert!(segments(&output, colour).is_empty() && segments(&output, GUIDE_ATTACHED).is_empty());
    }

    #[test]
    fn a_guide_is_added_through_a_point_in_the_default_colour_up_to_the_most() {
        let mut guides = Guides::new();
        guides.colour = [9, 8, 7, 6];
        guides.add(GuideAxis::Horizontal, [3.0, -4.5]);
        guides.add(GuideAxis::Vertical, [3.0, -4.5]);
        assert_eq!(guides.state()["lines"], json!([
            {"axis": "horizontal", "position": -4.5, "colour": [9, 8, 7, 6], "memo": ""},
            {"axis": "vertical", "position": 3.0, "colour": [9, 8, 7, 6], "memo": ""},
        ]));
        for index in 2..300 {
            guides.add(GuideAxis::Vertical, [f64::from(index), 0.0]);
        }
        assert_eq!(guides.lines.len(), MAX_GUIDES);
        assert_eq!(guides.lines.last().unwrap().position, 255.0);
    }

    #[test]
    fn part_menu_sort_emits_the_shared_viewport_operation() {
        let mut harness = Harness::new();
        harness.selected = None;
        let at = Pos2::new(140.0, 90.0);
        let right_click = |harness: &mut Harness| {
            harness.frame(vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton { pos:at, button:egui::PointerButton::Secondary, pressed:true, modifiers:egui::Modifiers::NONE },
                egui::Event::PointerButton { pos:at, button:egui::PointerButton::Secondary, pressed:false, modifiers:egui::Modifiers::NONE },
            ]);
            harness.frame(Vec::new())
        };
        let output = right_click(&mut harness);
        harness.frame(click_events(centre_of(&output, "Z-Sort")));
        harness.frame(Vec::new());
        assert!(!harness.state.menu_by_size);
        assert_eq!(harness.requests.last(), Some(&crate::editor_windows::call_request(
            "editor_viewport_set", json!({"partSort":"size_sort"}))));
        harness.state.menu_by_size = true;
        let output = right_click(&mut harness);
        harness.frame(click_events(centre_of(&output, "Size-Sort")));
        harness.frame(Vec::new());
        assert!(harness.state.menu_by_size);
        assert_eq!(harness.requests.last(), Some(&crate::editor_windows::call_request(
            "editor_viewport_set", json!({"partSort":"z_sort"}))));
    }

    #[test]
    fn a_right_click_on_empty_canvas_adds_a_guide_where_it_was_made() {
        let mut harness = Harness::new();
        harness.selected = None;
        let at = Pos2::new(140.0, 90.0);
        let right_click = |harness: &mut Harness| {
            let (button, modifiers) = (egui::PointerButton::Secondary, egui::Modifiers::NONE);
            harness.frame(vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton { pos: at, button, pressed: true, modifiers },
                egui::Event::PointerButton { pos: at, button, pressed: false, modifiers },
            ]);
            harness.frame(Vec::new())
        };
        // egui names a submenu button with its arrow.
        let choose = |harness: &mut Harness, item: &str| {
            let mut output = right_click(harness);
            for label in ["Guide line ⏵", item] {
                harness.frame(click_events(centre_of(&output, label)));
                output = harness.frame(Vec::new());
            }
        };
        let world = harness.state.camera.to_world(harness.state.size(), [140.0, 90.0]);
        choose(&mut harness, "Vertical guide here");
        choose(&mut harness, "Both guides here");
        let placed: Vec<(GuideAxis, f64)> = harness.state.guides.lines.iter().map(|g| (g.axis, g.position)).collect();
        use GuideAxis::{Horizontal, Vertical};
        assert_eq!(placed, [(Vertical, world[0]), (Horizontal, world[1]), (Vertical, world[0])]);
        assert!(!harness.state.guides_open);
        choose(&mut harness, "Guide Settings");
        assert!(!harness.state.guides_open, "the submenu emits a call for the host");
        assert_eq!(harness.requests.last(), Some(&crate::editor_windows::call_request("editor_guides_set", json!({"open":true}))));
        let labels = |output: &egui::FullOutput| -> Vec<String> {
            accesskit_nodes(output).into_iter().filter_map(|node| node.label().map(str::to_owned)).collect()
        };
        // Where a click cannot select, the menu offers the guides and no Part list.
        harness.selectable = false;
        let shown = labels(&right_click(&mut harness));
        assert!(shown.iter().any(|label| label == "Guide line ⏵"), "{shown:?}");
        assert!(!shown.iter().any(|label| label == "Z-Sort"), "{shown:?}");
        harness.frame(vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        // With `menu` off, as while a mesh or depth tool is active, no menu opens.
        (harness.selectable, harness.menu) = (true, false);
        let shown = labels(&right_click(&mut harness));
        assert!(!shown.iter().any(|label| label == "Guide line ⏵" || label == "Z-Sort"), "{shown:?}");
    }

    #[test]
    fn a_form_vertex_sits_on_its_point_and_gives_how_far_it_was_dragged() {
        let mut harness = Harness::new();
        harness.selected = None;
        let (camera, size) = (harness.state.camera, harness.state.size);
        let screen = |world: V2| {
            let at = camera.to_screen(size, world);
            Pos2::new(at[0] as f32, at[1] as f32)
        };
        let world = |at: Pos2| camera.to_world(size, [f64::from(at.x), f64::from(at.y)]);
        let near = |a: Pos2, b: Pos2| (a - b).length() < 0.01;
        harness.state.form_vertices = vec![[10.0, 10.0], [30.0, 10.0], [5000.0, 5000.0]];
        harness.frame(Vec::new());
        let output = harness.frame(Vec::new());
        assert!(near(centre_of(&output, "Form vertex 0"), screen([10.0, 10.0])));
        assert!(near(centre_of(&output, "Form vertex 1"), screen([30.0, 10.0])));
        let far = accesskit_nodes(&output).into_iter().any(|node| node.label() == Some("Form vertex 2"));
        assert!(!far, "a vertex off the picture takes no drag");

        let (primary, none) = (egui::PointerButton::Primary, egui::Modifiers::NONE);
        let press = |at: Pos2, pressed| egui::Event::PointerButton { pos: at, button: primary, pressed, modifiers: none };
        let from = screen([30.0, 10.0]) + egui::vec2(2.0, 1.0);
        let to = from + egui::vec2(30.0, 20.0);
        harness.frame(vec![egui::Event::PointerMoved(from), press(from, true)]);
        harness.frame(vec![egui::Event::PointerMoved(from + egui::vec2(10.0, 5.0))]);
        harness.frame(vec![egui::Event::PointerMoved(to)]);
        assert!(harness.forms.is_empty(), "nothing is keyed while the vertex is dragged");
        harness.frame(vec![press(to, false)]);
        let (a, b) = (world(from), world(to));
        assert_eq!(harness.forms, [FormPick { vertex: 1, delta: [b[0] - a[0], b[1] - a[1]] }], "measured from the press");

        let secondary = egui::PointerButton::Secondary;
        let right = |at: Pos2, pressed| egui::Event::PointerButton { pos: at, button: secondary, pressed, modifiers: none };
        harness.forms.clear();
        harness.frame(vec![egui::Event::PointerMoved(from), right(from, true)]);
        harness.frame(vec![egui::Event::PointerMoved(to)]);
        harness.frame(vec![right(to, false)]);
        harness.frame(Vec::new());
        assert!(harness.forms.is_empty(), "a secondary drag does not key a form");
        assert_eq!(harness.state.camera.centre, [100.0 - 15.0, 60.0 - 10.0], "it pans the picture");
        harness.state.camera = camera;
        harness.frame(Vec::new());

        let escape = egui::Event::Key {
            key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: none,
        };
        harness.frame(vec![egui::Event::PointerMoved(from), press(from, true)]);
        harness.frame(vec![egui::Event::PointerMoved(to)]);
        assert!(harness.state.form_drag.is_some(), "the vertex is dragged");
        harness.frame(vec![escape]);
        harness.frame(vec![press(to, false)]);
        assert!(harness.forms.is_empty(), "Escape cancels the drag");
        let vertices = harness.state.form_vertices.clone();
        harness.frame(vec![egui::Event::PointerMoved(from), press(from, true)]);
        harness.frame(vec![egui::Event::PointerMoved(to)]);
        assert!(harness.state.form_drag.is_some(), "the vertex is dragged");
        harness.state.form_vertices.clear();
        harness.frame(vec![press(to, false)]);
        harness.state.form_vertices = vertices;
        harness.frame(Vec::new());
        assert_eq!(harness.state.form_drag, None, "a drag whose vertices went away leaves nothing behind");
    }

    #[test]
    fn a_controller_ring_sits_on_its_point_moves_where_it_is_let_go_and_is_added_from_the_menu() {
        let mut harness = Harness::new();
        harness.selected = None;
        let (camera, size) = (harness.state.camera, harness.state.size);
        let screen = |world: V2| {
            let at = camera.to_screen(size, world);
            Pos2::new(at[0] as f32, at[1] as f32)
        };
        let world = |at: Pos2| camera.to_world(size, [f64::from(at.x), f64::from(at.y)]);
        let near = |a: Pos2, b: Pos2| (a - b).length() < 0.01;
        harness.state.controllers = vec![
            ViewController { name: "Grip".into(), start: [10.0, 10.0], end: None },
            ViewController { name: "Aim".into(), start: [30.0, 10.0], end: Some([30.0, 20.0]) },
            ViewController { name: "Far".into(), start: [5000.0, 5000.0], end: None },
        ];
        harness.frame(Vec::new());
        let output = harness.frame(Vec::new());
        assert!(near(centre_of(&output, "Controller Grip"), screen([10.0, 10.0])));
        assert!(near(centre_of(&output, "Controller Aim"), screen([30.0, 20.0])), "a direction is dragged by its end");
        let far = accesskit_nodes(&output).into_iter().any(|node| node.label() == Some("Controller Far"));
        assert!(!far, "a ring off the picture takes no clicks");

        let (primary, none) = (egui::PointerButton::Primary, egui::Modifiers::NONE);
        let press = |at: Pos2, pressed| egui::Event::PointerButton { pos: at, button: primary, pressed, modifiers: none };
        let from = screen([10.0, 10.0]);
        let to = from + egui::vec2(30.0, 20.0);
        harness.frame(vec![egui::Event::PointerMoved(from), press(from, true)]);
        harness.frame(vec![egui::Event::PointerMoved(from + egui::vec2(10.0, 5.0))]);
        harness.frame(vec![egui::Event::PointerMoved(to)]);
        assert!(harness.controllers.is_empty(), "nothing is solved while the ring is dragged");
        harness.frame(vec![press(to, false)]);
        assert_eq!(harness.controllers, [ControllerPick::Move("Grip".into(), world(to))]);

        let secondary = egui::PointerButton::Secondary;
        let right = |at: Pos2, pressed| egui::Event::PointerButton { pos: at, button: secondary, pressed, modifiers: none };
        harness.controllers.clear();
        harness.frame(vec![egui::Event::PointerMoved(from), right(from, true)]);
        harness.frame(vec![egui::Event::PointerMoved(to)]);
        harness.frame(vec![right(to, false)]);
        harness.frame(Vec::new());
        assert!(harness.controllers.is_empty(), "a secondary drag does not move a ring");
        assert_eq!(harness.state.camera.centre, [100.0 - 15.0, 60.0 - 10.0], "it pans the picture");
        harness.state.camera = camera;
        harness.frame(Vec::new());
        let rings = harness.state.controllers.clone();
        harness.frame(vec![egui::Event::PointerMoved(from), press(from, true)]);
        harness.frame(vec![egui::Event::PointerMoved(to)]);
        assert!(harness.state.controller_drag.is_some(), "the ring is dragged");
        harness.state.controllers.clear();
        harness.frame(vec![press(to, false)]);
        harness.state.controllers = rings;
        harness.frame(Vec::new());
        assert_eq!(harness.state.controller_drag, None, "a drag whose ring went away leaves no ghost ring");

        let escape = egui::Event::Key {
            key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: none,
        };
        harness.controllers.clear();
        harness.frame(vec![egui::Event::PointerMoved(from), press(from, true)]);
        harness.frame(vec![egui::Event::PointerMoved(to)]);
        assert!(harness.state.controller_drag.is_some(), "the ring is dragged");
        harness.frame(vec![escape.clone()]);
        harness.frame(vec![press(to, false)]);
        assert!(harness.controllers.is_empty(), "Escape cancels the move");

        let point = screen([2.0, 2.0]);
        let right_click = |harness: &mut Harness| {
            harness.frame(vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton { pos: point, button: secondary, pressed: true, modifiers: none },
                egui::Event::PointerButton { pos: point, button: secondary, pressed: false, modifiers: none },
            ]);
            harness.frame(Vec::new())
        };
        let add = |output: &egui::FullOutput| {
            accesskit_nodes(output).into_iter().find(|node| node.label() == Some("Add parameter controller"))
        };
        assert!(add(&right_click(&mut harness)).is_none(), "no clip on the timeline, no controller");
        harness.frame(vec![escape.clone()]);
        harness.frame(Vec::new());
        harness.state.bookmark_menu = true;
        let output = right_click(&mut harness);
        assert!(add(&output).expect("offered with a clip").is_disabled(), "off a whole frame or while playing");
        harness.frame(vec![escape]);
        harness.frame(Vec::new());
        harness.state.controller_add = true;
        let output = right_click(&mut harness);
        harness.frame(click_events(centre_of(&output, "Add parameter controller")));
        assert_eq!(harness.controllers, [ControllerPick::Add(world(point))]);
    }

    #[test]
    fn the_bookmark_menu_lists_every_part_drawn_under_the_point_and_labels_show_and_delete() {
        let mut harness = Harness::new();
        harness.selected = None;
        let (camera, size) = (harness.state.camera, harness.state.size);
        let screen = |world: V2| {
            let at = camera.to_screen(size, world);
            Pos2::new(at[0] as f32, at[1] as f32)
        };
        let (secondary, none) = (egui::PointerButton::Secondary, egui::Modifiers::NONE);
        let right_click = |harness: &mut Harness, at: Pos2| {
            harness.frame(vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton { pos: at, button: secondary, pressed: true, modifiers: none },
                egui::Event::PointerButton { pos: at, button: secondary, pressed: false, modifiers: none },
            ]);
            harness.frame(Vec::new())
        };
        let escape = egui::Event::Key {
            key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: none,
        };
        let labels = |output: &egui::FullOutput| -> Vec<String> {
            accesskit_nodes(output).into_iter().filter_map(|node| node.label().map(str::to_owned)).collect()
        };
        let shown = labels(&right_click(&mut harness, screen([2.0, 2.0])));
        assert!(!shown.iter().any(|label| label == "Parameter bookmark ⏵"), "no clip, no bookmark menu: {shown:?}");
        harness.frame(vec![escape.clone()]);
        harness.frame(Vec::new());

        harness.state.bookmark_menu = true;
        // Front (5) is locked: a click passes it, a bookmark does not.
        let response = harness.session.handle_request(&json!({"op": "node.lock.set", "nodes": [5], "locked": true}));
        assert_eq!(response["succeeded"], true, "{response}");
        let point = screen([2.0, 2.0]);
        let world = camera.to_world(size, [f64::from(point.x), f64::from(point.y)]);
        let output = right_click(&mut harness, point);
        harness.frame(click_events(centre_of(&output, "Parameter bookmark ⏵")));
        let output = harness.frame(Vec::new());
        let listed: Vec<String> = labels(&output).into_iter().filter(|label| label.contains(" (#")).collect();
        assert_eq!(listed, ["Front (#5)", "Back (#4)"], "front to back");
        harness.frame(click_events(centre_of(&output, "Back (#4)")));
        assert_eq!(harness.bookmarks, [BookmarkPick::Part(4, world)]);
        harness.frame(Vec::new());
        let output = right_click(&mut harness, screen([30.0, 30.0]));
        harness.frame(click_events(centre_of(&output, "Parameter bookmark ⏵")));
        let texts: Vec<String> = accesskit_nodes(&harness.frame(Vec::new())).into_iter()
            .flat_map(|node| [node.label().map(str::to_owned), node.value().map(str::to_owned)]).flatten().collect();
        assert!(texts.iter().any(|text| text == "No parts here"), "{texts:?}");
        harness.frame(vec![escape.clone()]);
        harness.frame(Vec::new());

        // With nothing selected the geometry is not kept fresh; an open menu reads it again.
        let response = harness.session.handle_request(&json!({"op": "node.delete", "nodes": [5]}));
        assert_eq!(response["succeeded"], true, "{response}");
        harness.state.invalidate_scene();
        let output = right_click(&mut harness, point);
        harness.frame(click_events(centre_of(&output, "Parameter bookmark ⏵")));
        let output = harness.frame(Vec::new());
        let listed: Vec<String> = labels(&output).into_iter().filter(|label| label.contains(" (#")).collect();
        assert_eq!(listed, ["Back (#4)"], "the deleted Part is gone from the menu");
        assert!(harness.state.menu_open);
        harness.menu = false;
        harness.frame(Vec::new());
        assert!(!harness.state.menu_open, "a tool that takes the viewport ends the fresh reads of the menu");
        harness.menu = true;
        harness.frame(vec![escape]);
        harness.frame(Vec::new());

        harness.state.bookmarks = vec![(3, "Arm".into(), [10.0, 10.0]), (4, "Far".into(), [5000.0, 5000.0])];
        // An area takes its place on the frame after it first shows.
        harness.frame(Vec::new());
        let output = harness.frame(Vec::new());
        assert!(!labels(&output).iter().any(|label| label == "Far"), "a label off the picture is not drawn");
        let arm = centre_of(&output, "Arm");
        let corner = accesskit_nodes(&output).into_iter().find(|node| node.label() == Some("Arm")).unwrap().bounds().unwrap();
        assert_eq!(Pos2::new(corner.x0 as f32, corner.y0 as f32), screen([10.0, 10.0]), "placed at its canvas point");
        harness.bookmarks.clear();
        harness.frame(click_events(arm));
        assert_eq!(harness.bookmarks, [BookmarkPick::Label(3)], "the index stored in the clip");
        let output = right_click(&mut harness, arm);
        harness.frame(click_events(centre_of(&output, "Delete label Arm")));
        assert_eq!(harness.bookmarks.last(), Some(&BookmarkPick::Delete(3)));
    }

    #[test]
    fn solo_lets_a_click_pick_only_the_parts_drawn_in_front() {
        let mut harness = Harness::new();
        harness.state.solo.show = true;
        // A click picks only a Part the Solo view draws: Back (4) through Front (5), which is on top.
        harness.selected = None;
        harness.state.solo_drawn = Some(BTreeSet::from([4]));
        let (camera, size) = (harness.state.camera, harness.state.size);
        let front = camera.to_screen(size, [4.0, 4.0]);
        let front = Pos2::new(front[0] as f32, front[1] as f32);
        let click = |harness: &mut Harness| {
            harness.frame(vec![egui::Event::PointerMoved(front), button(front, true), button(front, false)]);
            harness.frame(Vec::new());
        };
        click(&mut harness);
        assert_eq!(harness.selection, [4]);
        // The right-click Part list offers only the Parts drawn in front, as well.
        let (secondary, none) = (egui::PointerButton::Secondary, egui::Modifiers::NONE);
        harness.frame(vec![
            egui::Event::PointerMoved(front),
            egui::Event::PointerButton { pos: front, button: secondary, pressed: true, modifiers: none },
            egui::Event::PointerButton { pos: front, button: secondary, pressed: false, modifiers: none },
        ]);
        let listed: Vec<String> = accesskit_nodes(&harness.frame(Vec::new()))
            .into_iter()
            .filter_map(|node| node.label().map(str::to_owned))
            .filter(|label| label.contains(" #"))
            .collect();
        assert_eq!(listed, ["Back #4"]);
        harness.state.solo.show = false;
        // The first click closes the menu.
        click(&mut harness);
        click(&mut harness);
        assert_eq!(harness.selection.last(), Some(&5), "with Solo off the front Part is picked again");
    }

    #[test]
    fn the_guide_settings_add_edit_and_delete_guides_with_named_controls() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut guides = Guides::new();
        let run = |guides: &mut Guides, points: &[V2], events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(900.0, 600.0))),
                events,
                ..Default::default()
            };
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| guides.settings(ui, points));
            })
        };
        run(&mut guides, &[], Vec::new());
        let output = run(&mut guides, &[], Vec::new());
        let nodes = accesskit_nodes(&output);
        let disabled = nodes.into_iter().find(|n| n.label() == Some("Add at the selected vertices")).unwrap();
        assert!(disabled.is_disabled(), "no selected vertex, nothing to add at");
        run(&mut guides, &[], click_events(centre_of(&output, "Add at the origin")));
        assert_eq!(guides.lines.len(), 2);
        let output = run(&mut guides, &[[5.0, 6.0]], Vec::new());
        run(&mut guides, &[[5.0, 6.0]], click_events(centre_of(&output, "Add at the selected vertices")));
        let positions: Vec<(GuideAxis, f64)> = guides.lines.iter().map(|guide| (guide.axis, guide.position)).collect();
        use GuideAxis::{Horizontal, Vertical};
        assert_eq!(positions, [(Horizontal, 0.0), (Vertical, 0.0), (Horizontal, 6.0), (Vertical, 5.0)]);
        let output = run(&mut guides, &[], Vec::new());
        for node in accesskit_nodes(&output) {
            if matches!(node.role(), Role::Button | Role::CheckBox | Role::SpinButton | Role::TextInput | Role::ColorWell) {
                let named = node.label().is_some_and(|label| !label.is_empty()) || !node.labelled_by().is_empty();
                assert!(named, "unlabelled {:?} control {:?}", node.role(), node.value());
            }
        }
        // The topmost Delete removes the first guide; Apply to every guide gives each the default colour.
        let first = accesskit_nodes(&output)
            .into_iter()
            .filter(|node| node.label() == Some("Delete"))
            .filter_map(|node| node.bounds())
            .min_by(|a, b| a.y0.total_cmp(&b.y0))
            .unwrap();
        let at = Pos2::new(((first.x0 + first.x1) / 2.0) as f32, ((first.y0 + first.y1) / 2.0) as f32);
        run(&mut guides, &[], click_events(at));
        assert_eq!(guides.lines.len(), 3);
        assert_eq!(guides.lines[0].axis, Vertical);
        guides.lines[0].colour = [1, 1, 1, 1];
        let output = run(&mut guides, &[], Vec::new());
        run(&mut guides, &[], click_events(centre_of(&output, "Apply to every guide")));
        assert!(guides.lines.iter().all(|guide| guide.colour == guides.colour));
        // The Vertical button of the second row makes that guide vertical.
        assert_eq!(guides.lines[1].axis, Horizontal);
        let output = run(&mut guides, &[], Vec::new());
        let mut rows: Vec<_> = accesskit_nodes(&output)
            .into_iter()
            .filter(|node| node.label() == Some("Vertical"))
            .filter_map(|node| node.bounds())
            .collect();
        rows.sort_by(|a, b| a.y0.total_cmp(&b.y0));
        let second = rows[1];
        let at = Pos2::new(((second.x0 + second.x1) / 2.0) as f32, ((second.y0 + second.y1) / 2.0) as f32);
        run(&mut guides, &[], click_events(at));
        assert_eq!(guides.lines.iter().map(|guide| guide.axis).collect::<Vec<_>>(), [Vertical, Vertical, Vertical]);
        let output = run(&mut guides, &[], Vec::new());
        run(&mut guides, &[], click_events(centre_of(&output, "Show guide lines")));
        assert!(!guides.show);
    }

    #[test]
    fn local_offsets_are_parsed_and_absent_means_none() {
        assert_eq!(parse_local_offsets(&json!({"worldMatrices": {}})).unwrap(), BTreeMap::new());
        let parsed = parse_local_offsets(&json!({"localOffsets": {"4": [[1.0, 2.0], [3.0, 4.0]]}})).unwrap();
        assert_eq!(parsed[&4], vec![[1.0, 2.0], [3.0, 4.0]]);
        assert!(parse_local_offsets(&json!({"localOffsets": {"x": []}})).is_err());
        assert!(parse_local_offsets(&json!({"localOffsets": {"4": [[1.0]]}})).is_err());
    }

    #[test]
    fn the_overlay_is_drawn_in_a_headless_frame_for_a_deformer_and_for_the_root() {
        let mut harness = Harness::new();
        harness.session = deformer_session();
        for selected in [6u64, 1] {
            harness.selected = Some(selected);
            harness.state.invalidate_scene();
            harness.frame(Vec::new());
            let output = harness.frame(Vec::new());
            assert!(!output.shapes.is_empty());
        }
        assert_eq!(harness.state.scene.error, None);
    }
}
