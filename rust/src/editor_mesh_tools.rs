//! Mesh editor and deform editor tools of the egui viewport.
//!
//! Port of the interaction and drawing half of viewport/common/mesheditor (point, connect, lasso, brush,
//! grid, path-deform and bezier-deform tools), viewport/vertex (the "Edit Mesh" mode with its Apply and
//! Cancel bar), viewport/model/deform.d (deform mode of an armed parameter) and panels/toolsettings.d.
//!
//! The document half already lives in `soul2d_core::mesh_edit`; this module only turns pointer events into
//! requests for it.
//!
//! * Vertex-definition mode edits a working copy, as D edits an `IncMesh` until Apply. The copy is the
//!   Part's mesh plus a list of pending `mesh.edit` entries; the preview is `apply_edits` over that list,
//!   the very function the session runs, so the preview and the committed mesh cannot disagree. Apply
//!   sends the whole list as ONE `mesh.edit` (one undo step); Cancel drops it.
//! * "Edit Mesh" also starts for a MeshGroup, a PathDeformer and a GridDeformer, with the tools D's
//!   `canUse` gives them. The preview is the same replay of the pending list. Apply sends the finished
//!   geometry as ONE `deformer.define_mesh` (MeshGroup), `deformer.define_vertices` (PathDeformer) or
//!   `deformer.define_grid` (GridDeformer), so it is one undo step as well. A path replays its own list
//!   (`replay_path`) because `apply_edits` cannot insert a control point between two others, and
//!   `build_mesh` would sort control points that happen to form a rectangle.
//! * Deform mode edits `rest + keypoint offset` of the armed parameter. Every finished gesture is one
//!   `deform.edit`; while the button is down the preview is `deform_positions` over the same tool value.
//!
//! Gestures work on pointer positions in world space. They are mapped into the edited node's own
//! coordinates (where the mesh vertices live) through the inverse of the node's world matrix, and the
//! select radius is scaled by that map, so tools keep a constant on-screen size under any parent transform.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke};
use serde_json::{json, Value};
use crate::editor_viewport::{curve_closest_t, curve_point, curve_polyline, CurveKind, GuideAxis};
use soul2d_core::geometry::{self, SELECT_RADIUS};
use soul2d_core::mesh_edit::{
    apply_edits, combine_selection, deform_positions, inflate_stroke, parse_edit, select_lasso, select_rect, Brush,
    DeformTool, EditMesh, EditOptions, InflateSample, Mirror, SelectMode, MAX_INFLATE_SAMPLES,
};

pub type V2 = [f64; 2];
type Mat4 = [[f64; 4]; 4];

// ------------------------------------------------------------------------------------------ input

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Delete,
    Escape,
    Left,
    Right,
    Up,
    Down,
}

/// What the viewport reports to the active tool. Positions are WORLD coordinates; the left button state
/// is tracked by the tool state (`Press` .. `Release`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pointer {
    Press { at: V2, mods: Mods },
    Move { at: V2, mods: Mods },
    Release { at: V2, mods: Mods },
    /// Sent after the `Release` that completed a double click.
    DoubleClick { at: V2, mods: Mods },
    /// Right click (the lasso tool drops its last point).
    Secondary { at: V2, mods: Mods },
    Key { key: Key, mods: Mods },
}

// ----------------------------------------------------------------------------------------- frames

/// Edited node's local coordinates to world: `world = m * local + t`, with the inverse kept at hand.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    m: [[f64; 2]; 2],
    inv: [[f64; 2]; 2],
    t: V2,
}

impl Frame {
    #[cfg(test)]
    pub const IDENTITY: Frame = Frame { m: [[1.0, 0.0], [0.0, 1.0]], inv: [[1.0, 0.0], [0.0, 1.0]], t: [0.0, 0.0] };

    /// `None` for a singular map (the node is flattened away), where no tool can place a pointer.
    pub fn from_world_matrix(world: &Mat4) -> Option<Frame> {
        let m = [[world[0][0], world[0][1]], [world[1][0], world[1][1]]];
        let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
        if !det.is_finite() || det.abs() < 1e-12 {
            return None;
        }
        let inv = [[m[1][1] / det, -m[0][1] / det], [-m[1][0] / det, m[0][0] / det]];
        Some(Frame { m, inv, t: [world[0][3], world[1][3]] })
    }

    pub fn to_world(&self, p: V2) -> V2 {
        [self.m[0][0] * p[0] + self.m[0][1] * p[1] + self.t[0], self.m[1][0] * p[0] + self.m[1][1] * p[1] + self.t[1]]
    }

    pub fn to_local(&self, p: V2) -> V2 {
        let d = [p[0] - self.t[0], p[1] - self.t[1]];
        [self.inv[0][0] * d[0] + self.inv[0][1] * d[1], self.inv[1][0] * d[0] + self.inv[1][1] * d[1]]
    }

    /// Local units per world unit stretch: the select radius shrinks by this under a magnifying parent.
    pub fn scale(&self) -> f64 {
        (self.m[0][0] * self.m[1][1] - self.m[0][1] * self.m[1][0]).abs().sqrt()
    }
}

// -------------------------------------------------------------------------------------- node data

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Part,
    MeshGroup,
    PathDeformer,
    GridDeformer,
}

/// Rest geometry of the edited node in its own coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    pub id: u32,
    pub kind: NodeKind,
    pub points: Vec<V2>,
    /// Triangle list of the mesh (empty for deformers).
    pub indices: Vec<u32>,
    pub origin: V2,
    /// The PathDeformer's curve class (`Spline` for every other node).
    pub curve: CurveKind,
}

fn pairs_of_flat(value: &Value) -> Option<Vec<V2>> {
    let items = value.as_array().filter(|items| items.len() % 2 == 0)?;
    items
        .chunks(2)
        .map(|c| Some([c[0].as_f64().filter(|v| v.is_finite())?, c[1].as_f64().filter(|v| v.is_finite())?]))
        .collect()
}

fn numbers_of(value: &Value) -> Option<Vec<f64>> {
    value.as_array()?.iter().map(|v| v.as_f64().filter(|v| v.is_finite())).collect()
}

/// The node's rest points as `deform.edit` sees them (`node_geometry_points`). `None` when the node is
/// not editable by these tools or its data is malformed. A node without points (a new PathDeformer or
/// MeshGroup) is returned too: vertex definition starts from it; deform mode has nothing to edit there.
pub fn read_geometry(node: &Value) -> Option<Geometry> {
    let id = u32::try_from(node["uuid"].as_u64()?).ok()?;
    let mesh_geometry = |kind| {
        let mesh = node.get("mesh");
        // `parse_part_mesh` reads the top-level arrays when a node carries them (nested pairs), else `mesh`.
        let top_level = ["vertices", "uvs", "indices"].iter().any(|key| node.get(*key).is_some());
        let (points, indices) = if top_level {
            let points = node
                .get("vertices")?
                .as_array()?
                .iter()
                .map(|p| Some([p.get(0)?.as_f64().filter(|v| v.is_finite())?, p.get(1)?.as_f64().filter(|v| v.is_finite())?]))
                .collect::<Option<Vec<V2>>>()?;
            (points, node.get("indices")?)
        } else {
            let mesh = mesh?;
            (pairs_of_flat(mesh.get("verts")?)?, mesh.get("indices").unwrap_or(&Value::Null))
        };
        let indices: Vec<u32> = if indices.is_null() {
            Vec::new()
        } else {
            indices.as_array()?.iter().map(|v| u32::try_from(v.as_u64()?).ok()).collect::<Option<_>>()?
        };
        if indices.len() % 3 != 0 || indices.iter().any(|&i| i as usize >= points.len()) {
            return None;
        }
        let origin = match mesh.and_then(|m| m.get("origin")) {
            None => [0.0, 0.0],
            Some(o) => {
                let o = numbers_of(o)?;
                [*o.first()?, *o.get(1)?]
            }
        };
        Some(Geometry { id, kind, points, indices, origin, curve: CurveKind::Spline })
    };
    let geometry = match node["type"].as_str()? {
        "Part" => mesh_geometry(NodeKind::Part)?,
        "MeshGroup" => mesh_geometry(NodeKind::MeshGroup)?,
        "PathDeformer" => Geometry {
            id,
            kind: NodeKind::PathDeformer,
            points: pairs_of_flat(node.get("vertices")?)?,
            indices: Vec::new(),
            origin: [0.0, 0.0],
            curve: CurveKind::of_node(node),
        },
        "GridDeformer" => {
            let xs = numbers_of(node.get("grid_axis_x")?)?;
            let ys = numbers_of(node.get("grid_axis_y")?)?;
            if xs.len() < 2 || ys.len() < 2 {
                return None;
            }
            let points = ys.iter().flat_map(|&y| xs.iter().map(move |&x| [x, y])).collect();
            Geometry {
                id,
                kind: NodeKind::GridDeformer,
                points,
                indices: Vec::new(),
                origin: [0.0, 0.0],
                curve: CurveKind::Spline,
            }
        }
        _ => return None,
    };
    Some(geometry)
}

/// Line segments to draw for the geometry: triangle edges of a mesh, the control polyline of a path and
/// the lines of a grid (`columns` points per row).
pub fn edges_of(geometry: &Geometry, columns: usize) -> Vec<[usize; 2]> {
    let n = geometry.points.len();
    match geometry.kind {
        NodeKind::Part | NodeKind::MeshGroup => {
            let mut edges: Vec<[usize; 2]> = Vec::new();
            for t in geometry.indices.chunks_exact(3) {
                for k in 0..3 {
                    let (a, b) = (t[k] as usize, t[(k + 1) % 3] as usize);
                    let edge = [a.min(b), a.max(b)];
                    if !edges.contains(&edge) {
                        edges.push(edge);
                    }
                }
            }
            edges
        }
        NodeKind::PathDeformer => (1..n).map(|i| [i - 1, i]).collect(),
        NodeKind::GridDeformer => lattice_edges(n, columns),
    }
}

/// Lines of a lattice of `n` points stored row by row, `columns` points per row.
fn lattice_edges(n: usize, columns: usize) -> Vec<[usize; 2]> {
    let columns = columns.max(1);
    let mut edges = Vec::new();
    for i in 0..n {
        if (i + 1) % columns != 0 && i + 1 < n {
            edges.push([i, i + 1]);
        }
        if i + columns < n {
            edges.push([i, i + columns]);
        }
    }
    edges
}

/// `rest + offsets` at the parameter keypoint: the state `DeformationViewport.syncEditor` shows. A missing
/// binding, an unset keypoint or a null cell mean zero offsets.
pub fn key_offsets(model: &Value, parameter: u32, node: u32, key: [usize; 2], count: usize) -> Vec<V2> {
    fn find(list: &Value, id: u32) -> Option<&Value> {
        list.as_array()?.iter().find_map(|p| {
            if p.get("groupUUID").is_some() {
                find(&p["children"], id)
            } else if p["uuid"].as_u64() == Some(u64::from(id)) {
                Some(p)
            } else {
                None
            }
        })
    }
    let cell = find(&model["param"], parameter).and_then(|p| {
        let binding = p["bindings"].as_array()?.iter().find(|b| {
            b["param_name"].as_str() == Some("deform") && b["node"].as_u64() == Some(u64::from(node))
        })?;
        binding["values"].get(key[0])?.get(key[1])?.as_array().map(|cell| {
            cell.iter().map(|point| Some([point.get(0)?.as_f64()?, point.get(1)?.as_f64()?])).collect::<Option<Vec<V2>>>()
        })
    });
    match cell {
        Some(Some(cell)) if cell.len() == count => cell,
        _ => vec![[0.0, 0.0]; count],
    }
}

// -------------------------------------------------------------------------------------- settings

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrushKind {
    Circle,
    DoubleThreshold,
    Rectangle,
    Line,
}

impl BrushKind {
    pub const ALL: [BrushKind; 4] = [BrushKind::Circle, BrushKind::DoubleThreshold, BrushKind::Rectangle, BrushKind::Line];

    pub fn label(self) -> &'static str {
        match self {
            BrushKind::Circle => "Circle Brush",
            BrushKind::DoubleThreshold => "Double Threshold Circle",
            BrushKind::Rectangle => "Rectangle Brush",
            BrushKind::Line => "Line Brush",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrushMode {
    Drag,
    Flow,
    Inflate,
}

/// The values D keeps as statics and fields on the tools and the mesh editor.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub brush: BrushKind,
    pub radius: f64,
    pub inner_radius: f64,
    pub width: f64,
    pub height: f64,
    pub line_angle_degrees: f64,
    /// Pull with fixed weights, pull with moving weights, or inflate and contract while held.
    pub brush_mode: BrushMode,
    pub inflate_rate: f64,
    pub mirror_horizontal: bool,
    pub mirror_vertical: bool,
    pub mirror_origin: V2,
    /// Point tool: link every new vertex to the previously added one.
    pub auto_connect: bool,
    /// `GridTool.numCut`.
    pub grid_cuts: usize,
    /// Lasso tool: click points and close on the first one, instead of dragging a free line.
    pub poly_lasso: bool,
    /// Samples per control-point span of the path tools.
    pub path_resolution: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            brush: BrushKind::Circle,
            radius: 300.0,
            inner_radius: 100.0,
            width: 300.0,
            height: 300.0,
            line_angle_degrees: 0.0,
            brush_mode: BrushMode::Drag,
            inflate_rate: 0.5,
            mirror_horizontal: false,
            mirror_vertical: false,
            mirror_origin: [0.0, 0.0],
            auto_connect: false,
            grid_cuts: 3,
            poly_lasso: false,
            path_resolution: 40,
        }
    }
}

impl Settings {
    fn mirror(&self) -> Mirror {
        Mirror { horizontal: self.mirror_horizontal, vertical: self.mirror_vertical, origin: self.mirror_origin }
    }

    pub fn options(&self, zoom: f64) -> EditOptions {
        EditOptions { mirror: self.mirror(), zoom, auto_connect: self.auto_connect, texture: None }
    }

    fn brush_value(&self) -> Result<Brush, String> {
        match self.brush {
            BrushKind::Circle => Brush::circle(self.radius),
            BrushKind::DoubleThreshold => Brush::double_threshold(self.radius, self.inner_radius),
            BrushKind::Rectangle => Brush::rectangle(self.width, self.height),
            BrushKind::Line => Brush::line(self.radius, self.line_angle_degrees.to_radians()),
        }
    }
}

fn mirror_json(mirror: &Mirror) -> Value {
    json!({"horizontal": mirror.horizontal, "vertical": mirror.vertical, "origin": mirror.origin})
}

// ------------------------------------------------------------------------------------------ tools

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// "Edit Mesh" of a Part: a working copy that Apply commits.
    Vertex,
    /// Armed parameter: edits of the keypoint's offsets.
    Deform,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// "Vertex Tool" (PointTool, also the select tool).
    Point,
    /// "Edge Tool" (ConnectTool).
    Connect,
    /// "Grid Vertex Tool".
    Grid,
    /// "Lasso Selection".
    Lasso,
    /// "Edge Cutter".
    EdgeCutter,
    /// "Brush Tool".
    Brush,
    /// "Path deformation" (OneTimeDeform over a path).
    PathDeform,
    /// "Path Deform Tool" on a PathDeformer's control points.
    Bezier,
}

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            Tool::Point => "Vertex Tool",
            Tool::Connect => "Edge Tool",
            Tool::Grid => "Grid Vertex Tool",
            Tool::Lasso => "Lasso Selection",
            Tool::EdgeCutter => "Edge Cutter",
            Tool::Brush => "Brush Tool",
            Tool::PathDeform => "Path deformation",
            Tool::Bezier => "Path Deform Tool",
        }
    }

    /// The tools D offers in `mode` for a node of `kind`, in strip order (`incGetToolInfo`).
    pub fn offered(mode: Mode, kind: NodeKind) -> Vec<Tool> {
        match (mode, kind) {
            (Mode::Vertex, NodeKind::Part | NodeKind::MeshGroup) => {
                vec![Tool::Point, Tool::Connect, Tool::Grid, Tool::Lasso, Tool::EdgeCutter]
            }
            // Deformable but not Drawable (`canUse`): no edges to connect or cut, and the Bezier tool is
            // for PathDeformers only.
            (Mode::Vertex, NodeKind::PathDeformer) => vec![Tool::Point, Tool::Lasso, Tool::Bezier],
            // The lattice stays a lattice: only the Grid tool edits it (adding or dragging single points
            // would break it, which `deformer.define_grid` cannot store).
            (Mode::Vertex, NodeKind::GridDeformer) => vec![Tool::Grid],
            (Mode::Deform, NodeKind::PathDeformer) => {
                vec![Tool::Point, Tool::Brush, Tool::Lasso, Tool::Bezier, Tool::PathDeform]
            }
            (Mode::Deform, _) => vec![Tool::Point, Tool::Brush, Tool::Lasso, Tool::PathDeform],
        }
    }
}

// -------------------------------------------------------------------------------------- constants

/// Screen distance before a press becomes a drag (ImGui's default drag threshold is about this).
const DRAG_PX: f64 = 4.0;
/// A moving vertex adheres to a guide this close, in points. The vendor manual gives no distance; this is a guess.
const ADHERE_PX: f64 = 8.0;

/// `point` (world units) pulled onto the nearest guide of each axis within `threshold` points at `zoom` points
/// per world unit, and the guides it adheres to, by index into `guides`.
pub fn adhere(point: V2, guides: &[(GuideAxis, f64)], zoom: f64, threshold: f64) -> (V2, Vec<usize>) {
    let mut out = point;
    let mut attached = Vec::new();
    for (axis, coordinate) in [(GuideAxis::Horizontal, 1), (GuideAxis::Vertical, 0)] {
        let nearest = guides
            .iter()
            .enumerate()
            .filter(|(_, (guide, _))| *guide == axis)
            .map(|(index, (_, position))| (index, (position - point[coordinate]).abs()))
            .filter(|(_, distance)| distance * zoom <= threshold)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((index, _)) = nearest {
            out[coordinate] = guides[index].1;
            attached.push(index);
        }
    }
    (out, attached)
}
/// `LassoTool.isCloses`: a point within this many screen pixels closes a polygon and extends a free line.
const LASSO_PX: f64 = 14.0;
/// `NodeSelect.draw` cursor radius in deform mode.
const CURSOR_RADIUS_PX: f32 = 5.0;
/// Samples per span when a path is drawn or followed.
const SPLINE_SAMPLES: usize = 40;
/// Arrow-key nudge (`shiftSelection`): 10 units, Alt 1, Shift 100.
const NUDGE: f64 = 10.0;

fn sub(a: V2, b: V2) -> V2 {
    [a[0] - b[0], a[1] - b[1]]
}

fn add(a: V2, b: V2) -> V2 {
    [a[0] + b[0], a[1] + b[1]]
}

fn dist(a: V2, b: V2) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

// ----------------------------------------------------------------------------------- path helper

/// Catmull-Rom/Hermite evaluation of `CatmullSpline`, used to draw a path and to slide a control point
/// along it. The document-side deformation always goes through `mesh_edit::path_deform`.
fn spline_eval(points: &[V2], off: f64) -> V2 {
    let n = points.len();
    if off <= 0.0 {
        return points[0];
    }
    if off >= (n - 1) as f64 {
        return points[n - 1];
    }
    let ioff = off as usize;
    let t = off - ioff as f64;
    let s = 1.0 - t;
    let (p1, p2) = (points[ioff], points[ioff + 1]);
    let mix = |a: V2, b: V2, wa: f64, wb: f64| [a[0] * wa + b[0] * wb, a[1] * wa + b[1] * wb];
    if n == 2 {
        return mix(p1, p2, s, t);
    }
    let edge = |p0: V2, p1: V2, p2: V2, t: f64| {
        let (t2, t3) = (t * t, t * t * t);
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        let slope = [(p2[0] - p0[0]) * 0.5, (p2[1] - p0[1]) * 0.5];
        [
            slope[0] * h11 + p1[0] * h01 + p0[0] * h00,
            slope[1] * h11 + p1[1] * h01 + p0[1] * h00,
        ]
    };
    if ioff == 0 {
        return edge(p1, p2, points[ioff + 2], t);
    }
    if ioff == n - 2 {
        return edge(p2, p1, points[ioff - 1], s);
    }
    let (p0, p3) = (points[ioff - 1], points[ioff + 2]);
    let b1 = add(p1, [(p2[0] - p0[0]) / 6.0, (p2[1] - p0[1]) / 6.0]);
    let b2 = sub(p2, [(p3[0] - p1[0]) / 6.0, (p3[1] - p1[1]) / 6.0]);
    let mc = mix(b1, b2, s, t);
    let a1 = mix(p1, b1, s, t);
    let a2 = mix(a1, mc, s, t);
    let b2n = mix(b2, p2, s, t);
    let b1n = mix(mc, b2n, s, t);
    mix(a2, b1n, s, t)
}

/// The curve as a polyline, `SPLINE_SAMPLES` points per span.
fn spline_polyline(points: &[V2]) -> Vec<V2> {
    if points.len() < 2 {
        return points.to_vec();
    }
    let samples = SPLINE_SAMPLES * (points.len() - 1) + 1;
    (0..samples).map(|i| spline_eval(points, i as f64 / SPLINE_SAMPLES as f64)).collect()
}

/// Offset on the curve nearest to `point`, by sampling (`findClosestPointOffset`).
fn closest_offset(points: &[V2], point: V2) -> f64 {
    let line = spline_polyline(points);
    let best = line
        .iter()
        .enumerate()
        .min_by(|a, b| dist(*a.1, point).total_cmp(&dist(*b.1, point)))
        .map_or(0, |(i, _)| i);
    best as f64 / SPLINE_SAMPLES as f64
}

/// `CatmullSpline.findPoint`: the nearest control point inside the select radius.
fn nearest_point(points: &[V2], at: V2, zoom: f64) -> Option<usize> {
    let (best, d) = points
        .iter()
        .enumerate()
        .map(|(i, p)| (i, dist(*p, at)))
        .min_by(|a, b| a.1.total_cmp(&b.1))?;
    (d <= SELECT_RADIUS / zoom).then_some(best)
}

/// `CatmullSpline.addPoint`: append or prepend beyond the ends, split the curve when the click is on it.
fn path_add_point(points: &mut Vec<V2>, at: V2, zoom: f64) {
    if points.len() < 2 {
        points.push(at);
        return;
    }
    let off = closest_offset(points, at);
    let last = (points.len() - 1) as f64;
    if off <= 0.0 {
        points.insert(0, at);
    } else if off >= last {
        points.push(at);
    } else if dist(at, spline_eval(points, off)) < SELECT_RADIUS / zoom {
        let on_curve = spline_eval(points, off);
        points.insert(off.floor() as usize + 1, on_curve);
    }
}

/// Where a double click adds a control point to a PathDeformer (`BezierDeformTool.updateVertexEdit`): on
/// the curve, between the neighbours by curve position, when the click is within the select radius of
/// it; before the first point when it is off the curve in the first half; otherwise after the last one.
/// Returns the index to insert at and the position. Points at the click itself when off the curve.
pub fn bezier_insertion(curve: CurveKind, points: &[V2], click: V2, zoom: f64) -> (usize, V2) {
    if points.len() < 2 {
        return (points.len(), click);
    }
    let t = curve_closest_t(curve, points, click);
    let on_curve = curve_point(curve, points, t);
    if dist(on_curve, click) < SELECT_RADIUS / zoom {
        if let Some(before) = points.iter().position(|p| t <= curve_closest_t(curve, points, *p)) {
            return (before, on_curve);
        }
    } else if t < 0.5 {
        return (0, click);
    }
    (points.len(), click)
}

// ----------------------------------------------------------------------------------------- state

/// What is being edited and at which keypoint.
#[derive(Clone, Debug, PartialEq)]
struct Target {
    id: u32,
    mode: Mode,
    parameter: u32,
    key: [usize; 2],
}

/// Per-frame facts the tools need; built by [`MeshTools::sync`].
struct Active {
    revision: u64,
    target: Target,
    geometry: Geometry,
    /// Vertex mode: the Part's vertices. Deform mode: `rest + offsets` at the key.
    base: Vec<V2>,
    base_mesh: Option<EditMesh>,
    columns: usize,
    frame: Frame,
    /// Camera zoom in points per world unit.
    zoom: f64,
    /// Why the session would refuse to store new geometry for this node (see [`storage_blocker`]).
    blocked: Option<String>,
}

/// Facts from the editor for one frame.
pub struct Ctx<'a> {
    pub revision: u64,
    /// The document model (`snapshot["model"]`).
    pub model: &'a Value,
    pub selected: Option<u64>,
    /// Armed parameter and the key `closest_keypoint` gives for its current value.
    pub armed: Option<(u32, [usize; 2])>,
    /// Selected node's local-to-world map; `None` while the scene has not been evaluated.
    pub frame: Option<Frame>,
    pub zoom: f64,
}

#[derive(Clone, Debug)]
struct Press {
    world: V2,
    local: V2,
    hit: Option<usize>,
    /// The press landed on an already selected vertex: a release without a drag selects just that one.
    maybe: bool,
    mods: Mods,
}

/// A gesture that is under way. Positions are in the node's local space.
#[derive(Clone, Debug)]
enum Gesture {
    Idle,
    /// Left button down; the tool decides what the press becomes when the pointer travels.
    Pending(Press),
    /// Rectangle selection.
    Box { origin: V2, now: V2, mods: Mods },
    /// Dragging the selection (Point tool). `anchor`: the grabbed vertex, which adheres to guides, when it is selected.
    Move { from: V2, now: V2, anchor: Option<usize> },
    Brush { stroke: Vec<V2>, shift: bool },
    Inflate {
        revision: u64,
        brush: Brush,
        selected: Vec<usize>,
        samples: Vec<InflateSample>,
        preview: Vec<V2>,
        center: V2,
        rate: f64,
        elapsed: f64,
        options: EditOptions,
    },
    /// Lasso button held (the polyline lives in `MeshTools::lasso`).
    LassoDown,
    Cut { from: V2, now: V2 },
    GridCreate { from: V2, now: V2 },
    GridMove { vertex: usize, now: V2 },
    /// A path control point is being dragged: its index, the pointer where the drag started and now.
    PathDrag { index: usize, from: V2, now: V2, shift: bool },
    /// `shift`: slide the point along the path instead of translating it (Shift, or "Move points along the
    /// path").
    BezierDrag { index: usize, from: V2, now: V2, shift: bool },
}

/// The edit a gesture would make right now.
#[derive(Clone, Debug)]
enum Draft {
    Edit(Value),
    Deform(DeformTool),
}

/// Where the selection goes after a vertex-mode edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum After {
    Keep,
    Clear,
    /// The vertex that was added last.
    Last,
    /// The first vertex added by the edit (`count` is the vertex count before).
    From(usize),
}

pub struct MeshTools {
    pub settings: Settings,
    pub tool: Tool,
    /// "Edit Mesh" was asked for this node (a Part, MeshGroup, PathDeformer or GridDeformer).
    vertex_for: Option<u32>,
    edit_mesh_for: Option<u32>,
    active: Option<Active>,
    selected: Vec<usize>,
    gesture: Gesture,
    /// Vertex mode: the edits not applied yet, the options they run under and the mesh they produce.
    pending: Vec<Value>,
    undone: Vec<Value>,
    pending_options: Option<EditOptions>,
    mesh: Option<EditMesh>,
    /// Path tools.
    path: Vec<V2>,
    path_target: Vec<V2>,
    path_transform: bool,
    locked: Option<usize>,
    rotate_mode: bool,
    shift_mode: bool,
    /// Pointer in local space, for the cursor and hover.
    cursor: Option<V2>,
    lasso: Vec<V2>,
    lasso_checkpoints: Vec<usize>,
    pub notice: String,
    /// The left button is down inside the picture.
    down: bool,
    /// The guides a vertex moved with the Point tool adheres to (world units); empty when adhesion is off.
    pub guides: Vec<(GuideAxis, f64)>,
    /// The guides the grabbed vertex adheres to now, by index into `guides`.
    attached: Vec<usize>,
}

impl Default for MeshTools {
    fn default() -> Self {
        Self::new()
    }
}

impl MeshTools {
    pub fn new() -> Self {
        Self {
            settings: Settings::default(),
            tool: Tool::Point,
            vertex_for: None,
            edit_mesh_for: None,
            active: None,
            selected: Vec::new(),
            gesture: Gesture::Idle,
            pending: Vec::new(),
            undone: Vec::new(),
            pending_options: None,
            mesh: None,
            path: Vec::new(),
            path_target: Vec::new(),
            path_transform: false,
            locked: None,
            rotate_mode: false,
            shift_mode: false,
            cursor: None,
            lasso: Vec::new(),
            lasso_checkpoints: Vec::new(),
            notice: String::new(),
            down: false,
            guides: Vec::new(),
            attached: Vec::new(),
        }
    }

    pub fn mode(&self) -> Option<Mode> {
        self.active.as_ref().map(|a| a.target.mode)
    }

    /// The tools take the pointer (the viewport must not select Parts or drag the transform handle).
    pub fn captures(&self) -> bool {
        self.active.is_some()
    }

    /// Includes a requested mode before the next viewport sync creates its working geometry.
    pub fn editing(&self) -> bool {
        self.active.is_some() || self.vertex_for.is_some()
    }

    /// The guides the vertex being moved adheres to, by index; none when no vertex is moving.
    pub fn guides_attached(&self) -> Vec<usize> {
        match self.gesture {
            Gesture::Move { .. } => self.attached.clone(),
            _ => Vec::new(),
        }
    }

    /// The pointer of a Point drag, in local space, moved so that the grabbed vertex adheres to the guides near
    /// it. The frame is affine, so a world offset of the vertex is the same world offset of the pointer.
    fn adhere_move(&mut self, from: V2, anchor: Option<usize>, world: V2, local: V2) -> V2 {
        self.attached.clear();
        let Some((active, anchor)) = self.active.as_ref().zip(anchor) else { return local };
        if self.guides.is_empty() {
            return local;
        }
        // The working positions: in Edit Mesh they hold the edits not yet applied, and new vertices.
        let Some(&start) = self.points().get(anchor) else { return local };
        let vertex = active.frame.to_world(add(start, sub(local, from)));
        let (adhered, attached) = adhere(vertex, &self.guides, active.zoom, ADHERE_PX);
        self.attached = attached;
        active.frame.to_local(add(world, sub(adhered, vertex)))
    }

    /// The selected vertices in world units, for guides through them.
    pub fn selected_world_points(&self) -> Vec<V2> {
        let Some(active) = &self.active else { return Vec::new() };
        let points = self.points();
        self.selected.iter().filter_map(|index| points.get(*index)).map(|point| active.frame.to_world(*point)).collect()
    }

    /// The node the active edit targets, `None` outside an edit mode.
    pub fn target_node(&self) -> Option<u32> {
        self.active.as_ref().map(|active| active.target.id)
    }

    /// `indices` as a selection of the active edit's working points: each once, in the order given. Refuses an
    /// index at or past the point count, or when no edit is active. Changes nothing.
    pub fn checked_selection(&self, indices: &[usize]) -> Result<Vec<usize>, String> {
        if self.active.is_none() {
            return Err("no mesh edit is active; select a mesh node (and arm a parameter for deform mode) first".into());
        }
        let count = self.points().len();
        if let Some(bad) = indices.iter().find(|index| **index >= count) {
            return Err(format!("vertex {bad} is out of range: the mesh has {count} points"));
        }
        let mut selected = Vec::with_capacity(indices.len());
        for index in indices {
            if !selected.contains(index) {
                selected.push(*index);
            }
        }
        Ok(selected)
    }

    /// Replace the vertex selection with `indices` (see `checked_selection`).
    pub fn select(&mut self, indices: &[usize]) -> Result<(), String> {
        self.selected = self.checked_selection(indices)?;
        Ok(())
    }

    pub fn selection(&self) -> &[usize] {
        &self.selected
    }

    #[cfg(test)]
    pub fn pending_edits(&self) -> &[Value] {
        &self.pending
    }

    pub fn tool_offered(&self) -> Vec<Tool> {
        self.active
            .as_ref()
            .map_or_else(Vec::new, |a| Tool::offered(a.target.mode, a.geometry.kind))
    }

    #[cfg(test)]
    /// The node "Edit Mesh" can start for (selected, no armed parameter), as of the last `sync`.
    pub fn edit_mesh_target(&self) -> Option<u32> {
        self.edit_mesh_for
    }

    pub fn start_edit_mesh(&mut self, id: u32) {
        self.vertex_for = Some(id);
        self.tool = Tool::Point;
        self.reset_edit_state();
        self.notice.clear();
    }

    #[cfg(test)]
    pub fn edit_mesh_started(&self) -> bool {
        self.vertex_for.is_some()
    }

    fn reset_edit_state(&mut self) {
        if matches!(self.gesture, Gesture::Inflate { .. }) {
            self.down = false;
        }
        self.selected.clear();
        self.gesture = Gesture::Idle;
        self.pending.clear();
        self.undone.clear();
        self.pending_options = None;
        self.mesh = None;
        self.path.clear();
        self.path_target.clear();
        self.path_transform = false;
        self.locked = None;
        self.lasso.clear();
        self.lasso_checkpoints.clear();
    }

    /// Reads the document for this frame and moves the state to the mode the context asks for. Changing
    /// the node, parameter or key drops the selection and the pending edits (D rebuilds its editors).
    pub fn sync(&mut self, ctx: &Ctx) {
        if ctx.armed.is_some() {
            self.vertex_for = None;
        }
        self.edit_mesh_for = ctx
            .selected
            .filter(|_| ctx.armed.is_none())
            .and_then(|id| find_node(&ctx.model["nodes"], id))
            .and_then(read_geometry)
            .map(|g| g.id);
        let desired = self.desired(ctx);
        let Some((target, geometry, base, frame)) = desired else {
            if self.active.take().is_some() {
                self.reset_edit_state();
            }
            if ctx.armed.is_some() || ctx.selected.map(|s| u32::try_from(s).ok()) != Some(self.vertex_for) {
                self.vertex_for = None;
            }
            return;
        };
        let changed_target = self.active.as_ref().map(|a| &a.target) != Some(&target);
        let changed_base = self.active.as_ref().is_some_and(|a| a.base != base);
        let changed_origin = geometry.kind == NodeKind::Part
            && self.active.as_ref().is_some_and(|a| a.geometry.origin != geometry.origin);
        let changed_document = self.active.as_ref().is_some_and(|active| {
            active.revision != ctx.revision || active.frame != frame || active.geometry != geometry
        });
        if matches!(self.gesture, Gesture::Inflate { .. }) && (changed_target || changed_base || changed_document) {
            self.gesture = Gesture::Idle;
            self.down = false;
            self.notice = "Inflation cancelled because the document or edited shape changed".to_owned();
        }
        if changed_target {
            self.reset_edit_state();
            let offered = Tool::offered(target.mode, geometry.kind);
            if !offered.contains(&self.tool) {
                self.tool = offered.first().copied().unwrap_or(Tool::Point);
            }
        } else if changed_origin && target.mode == Mode::Deform {
            self.reset_edit_state();
            self.down = false;
            self.cursor = None;
        } else if changed_base && target.mode == Mode::Vertex {
            // The document changed under the tools (undo, an edit elsewhere): start from what it holds.
            // Deform mode keeps its selection, since every finished gesture changes the base.
            self.reset_edit_state();
        }
        let columns = grid_columns(ctx.model, target.id);
        let base_mesh = (target.mode == Mode::Vertex)
            .then(|| EditMesh::from_triangles(&geometry.points, &geometry.indices, geometry.origin).ok())
            .flatten();
        if target.mode == Mode::Vertex && self.mesh.is_none() {
            self.mesh = base_mesh.clone();
        }
        let blocked = (target.mode == Mode::Vertex && geometry.kind != NodeKind::Part)
            .then(|| find_node(&ctx.model["nodes"], u64::from(target.id)).and_then(|n| storage_blocker(ctx.model, n, geometry.kind)))
            .flatten();
        self.active = Some(Active {
            revision: ctx.revision,
            target,
            geometry,
            base,
            base_mesh,
            columns,
            frame,
            zoom: ctx.zoom,
            blocked,
        });
        if !Tool::offered(self.mode().unwrap_or(Mode::Deform), self.active.as_ref().map_or(NodeKind::Part, |a| a.geometry.kind))
            .contains(&self.tool)
        {
            self.tool = Tool::Point;
        }
        let len = self.points().len();
        self.selected.retain(|&i| i < len);
    }

    fn desired(&self, ctx: &Ctx) -> Option<(Target, Geometry, Vec<V2>, Frame)> {
        let frame = ctx.frame?;
        let id = ctx.selected?;
        let node = find_node(&ctx.model["nodes"], id)?;
        let geometry = read_geometry(node)?;
        // Stored points stay raw. A Part is drawn as W * (vertex - origin), so deform and vertex tools share that frame.
        let frame = if geometry.kind == NodeKind::Part {
            Frame { t: frame.to_world([-geometry.origin[0], -geometry.origin[1]]), ..frame }
        } else {
            frame
        };
        if let Some((parameter, key)) = ctx.armed {
            if geometry.points.is_empty() {
                return None;
            }
            let offsets = key_offsets(ctx.model, parameter, geometry.id, key, geometry.points.len());
            let base = geometry.points.iter().zip(&offsets).map(|(r, o)| add(*r, *o)).collect();
            let target = Target { id: geometry.id, mode: Mode::Deform, parameter, key };
            return Some((target, geometry, base, frame));
        }
        if self.vertex_for == Some(geometry.id) {
            let base = geometry.points.clone();
            let target = Target { id: geometry.id, mode: Mode::Vertex, parameter: 0, key: [0, 0] };
            return Some((target, geometry, base, frame));
        }
        None
    }

    // ------------------------------------------------------------------------------ working data

    /// The working vertex positions: pending edits applied in vertex mode, the keypoint positions in
    /// deform mode.
    pub fn points(&self) -> Vec<V2> {
        match (&self.active, &self.mesh) {
            (Some(a), Some(mesh)) if a.target.mode == Mode::Vertex => mesh.positions(),
            (Some(a), _) => a.base.clone(),
            _ => Vec::new(),
        }
    }

    /// Positions and edges to draw, with the running gesture's draft applied.
    pub fn preview(&self) -> (Vec<V2>, Vec<[usize; 2]>) {
        let Some(active) = &self.active else { return (Vec::new(), Vec::new()) };
        match active.target.mode {
            Mode::Vertex => {
                let mut mesh = self.mesh.clone().unwrap_or_else(|| EditMesh { verts: Vec::new(), origin: [0.0; 2] });
                if let Some(Draft::Edit(value)) = self.draft() {
                    if let Ok(edit) = parse_edit(&value) {
                        let options = self.options();
                        let _ = apply_edits(&mut mesh, &[edit], &options);
                    }
                }
                let positions = mesh.positions();
                let edges = match active.geometry.kind {
                    NodeKind::Part | NodeKind::MeshGroup => mesh_edges(&mesh),
                    NodeKind::PathDeformer => (1..positions.len()).map(|i| [i - 1, i]).collect(),
                    NodeKind::GridDeformer => {
                        let columns = soul2d_core::mesh_edit::grid_axes(&positions).map_or(1, |[_, xs]| xs.len());
                        lattice_edges(positions.len(), columns)
                    }
                };
                (positions, edges)
            }
            Mode::Deform => {
                if let Gesture::Inflate { preview, .. } = &self.gesture {
                    return (preview.clone(), edges_of(&active.geometry, active.columns));
                }
                let mut points = active.base.clone();
                if let Some(Draft::Deform(tool)) = self.draft() {
                    let mut moved = points.clone();
                    if deform_positions(&mut moved, &active.geometry.points, &tool, &self.options()).is_ok() {
                        points = moved;
                    }
                }
                (points, edges_of(&active.geometry, active.columns))
            }
        }
    }

    /// Select radius scale: points per local unit.
    fn local_zoom(&self) -> f64 {
        self.active.as_ref().map_or(1.0, |a| (a.zoom * a.frame.scale()).max(1e-9))
    }

    fn options(&self) -> EditOptions {
        self.pending_options.unwrap_or_else(|| self.fresh_options())
    }

    /// The options a gesture runs under now. Auto-connect links mesh edges, which only a Part or a
    /// MeshGroup has.
    fn fresh_options(&self) -> EditOptions {
        let mut options = self.settings.options(self.local_zoom());
        if !self.active.as_ref().is_some_and(|a| matches!(a.geometry.kind, NodeKind::Part | NodeKind::MeshGroup)) {
            options.auto_connect = false;
        }
        options
    }

    /// The options a deform gesture runs under now.
    fn live_options(&self) -> EditOptions {
        self.fresh_options()
    }

    fn vertex_at(&self, local: V2) -> Option<usize> {
        geometry::get_vertex_from_point(&self.points(), local, self.local_zoom()).ok().flatten()
    }

    fn to_local(&self, world: V2) -> V2 {
        self.active.as_ref().map_or(world, |a| a.frame.to_local(world))
    }

}

fn find_node(node: &Value, id: u64) -> Option<&Value> {
    if node["uuid"].as_u64() == Some(id) {
        return Some(node);
    }
    node["children"].as_array()?.iter().find_map(|child| find_node(child, id))
}

fn grid_columns(model: &Value, id: u32) -> usize {
    find_node(&model["nodes"], u64::from(id)).and_then(|n| n["grid_axis_x"].as_array()).map_or(1, Vec::len)
}

fn mesh_edges(mesh: &EditMesh) -> Vec<[usize; 2]> {
    let mut edges = Vec::new();
    for (a, vertex) in mesh.verts.iter().enumerate() {
        for &b in &vertex.connections {
            if b > a {
                edges.push([a, b]);
            }
        }
    }
    edges
}

// ---------------------------------------------------------------------------------- JSON requests

fn brush_json(brush: &Brush) -> Value {
    match *brush {
        Brush::Circle { radius } => json!({"kind": "circle", "radius": radius}),
        Brush::DoubleThreshold { radius, inner } => {
            json!({"kind": "double_threshold", "radius": radius, "innerRadius": inner})
        }
        Brush::Rectangle { width, height } => json!({"kind": "rectangle", "width": width, "height": height}),
        Brush::Line { radius, angle } => json!({"kind": "line", "radius": radius, "angle": angle}),
    }
}

/// The fields of a `deform.edit` request that name the tool (`parse_deform_tool` reads them back).
fn deform_fields(tool: &DeformTool) -> Value {
    match tool {
        DeformTool::Translate { selected, delta } => json!({"tool": "translate", "selected": selected, "delta": delta}),
        DeformTool::Reset { selected } => json!({"tool": "reset", "selected": selected}),
        DeformTool::Brush { selected, brush, flow, stroke, snap } => json!({
            "tool": "brush", "selected": selected, "brush": brush_json(brush), "flow": flow, "stroke": stroke,
            "snapAxis": snap,
        }),
        DeformTool::Inflate { selected, brush, samples } => json!({
            "tool": "inflate",
            "selected": selected,
            "brush": brush_json(brush),
            "samples": samples.iter().map(|sample| json!({"center": sample.center, "amount": sample.amount})).collect::<Vec<_>>(),
        }),
        DeformTool::PathDeform { selected, path, target, resolution } => json!({
            "tool": "path_deform", "selected": selected, "path": path, "target": target, "resolution": resolution,
        }),
        DeformTool::Warp { selected, columns, rows, points } => json!({
            "tool": "warp", "selected": selected, "grid": {"columns": columns, "rows": rows, "points": points},
        }),
        DeformTool::Perspective { selected, corners } => {
            json!({"tool": "perspective", "selected": selected, "corners": corners})
        }
        DeformTool::BezierTranslate { index, delta } => {
            json!({"tool": "bezier_translate", "index": index, "delta": delta})
        }
        DeformTool::BezierRotate { locked, index, from, to } => {
            json!({"tool": "bezier_rotate", "locked": locked, "index": index, "from": from, "to": to})
        }
    }
}

/// `deform.edit` for one finished gesture.
pub fn deform_request(
    node: u32,
    parameter: u32,
    key: [usize; 2],
    tool: &DeformTool,
    options: &EditOptions,
) -> Value {
    let mut request = json!({
        "op": "deform.edit", "id": node, "parameter": parameter, "key": key,
        "mirror": mirror_json(&options.mirror), "zoom": options.zoom,
    });
    if let (Some(map), Some(fields)) = (request.as_object_mut(), deform_fields(tool).as_object()) {
        map.extend(fields.clone());
    }
    request
}

/// `mesh.edit` for the pending list: ONE request, so Apply is one undo step.
pub fn mesh_request(node: u32, edits: &[Value], options: &EditOptions) -> Value {
    json!({
        "op": "mesh.edit", "id": node, "edits": edits, "mirror": mirror_json(&options.mirror),
        "zoom": options.zoom, "autoConnect": options.auto_connect,
    })
}

/// The reasons `deformer.define_mesh` / `define_vertices` / `define_grid` refuse a node, as far as they can
/// be read from the document (`reject_locked_geometry` in deformers.rs, which is private): payloads the
/// new geometry would leave stale, and, for a MeshGroup only, a deform parameter binding (D remaps those
/// on remesh; `define_mesh` does not, while `define_vertices` and `define_grid` resize them).
pub fn storage_blocker(model: &Value, node: &Value, kind: NodeKind) -> Option<String> {
    fn bound(list: &Value, id: u64) -> bool {
        list.as_array().into_iter().flatten().any(|parameter| {
            parameter["bindings"].as_array().into_iter().flatten().any(|binding| {
                binding["param_name"].as_str() == Some("deform") && binding["node"].as_u64() == Some(id)
            }) || bound(&parameter["children"], id)
        })
    }
    let id = node["uuid"].as_u64()?;
    let filled = |key: &str| node.get(key).and_then(Value::as_array).is_some_and(|list| !list.is_empty());
    let depth_mapped = matches!(kind, NodeKind::PathDeformer | NodeKind::GridDeformer);
    let depth_keys = ["depths", "depth-ops", "depth-op-base-depths"];
    let payload = filled("textures")
        || node.get("deformation").is_some()
        || (!depth_mapped && depth_keys.iter().any(|key| node.get(*key).is_some()))
        || (kind == NodeKind::PathDeformer && ["depth-ops", "depth-op-base-depths"].iter().any(|key| node.get(*key).is_some()))
        || filled("weldedLinks")
        || filled("masks")
        || filled("masked_by");
    if payload {
        return Some(format!("node {id} deform geometry is outside Rust Core mode"));
    }
    (kind == NodeKind::MeshGroup && bound(&model["param"], id))
        .then(|| format!("node {id} has a deform parameter binding and is outside Rust Core mode"))
}

fn flat(points: &[V2]) -> Vec<f64> {
    points.iter().flat_map(|p| [p[0], p[1]]).collect()
}

/// `deformer.define_mesh` for a MeshGroup's finished mesh: vertices, triangles, and the bounding-box UVs
/// `Drawable.normalizeUV` leaves (the session refuses a vertex count that its old UVs no longer match).
pub fn mesh_group_request(node: u32, built: &soul2d_core::mesh_edit::BuiltMesh) -> Result<Value, String> {
    if built.indices.is_empty() {
        return Err("A MeshGroup needs at least one triangle: connect its vertices or triangulate them".to_owned());
    }
    Ok(json!({
        "op": "deformer.define_mesh", "nodes": [node], "vertices": flat(&built.vertices),
        "indices": built.indices, "uvs": flat(&built.uvs), "origin": built.origin,
    }))
}

/// `deformer.define_vertices` for a PathDeformer's control points.
pub fn path_request(node: u32, points: &[V2]) -> Result<Value, String> {
    if points.is_empty() {
        return Err("A path needs at least one control point".to_owned());
    }
    Ok(json!({"op": "deformer.define_vertices", "nodes": [node], "vertices": flat(points)}))
}

/// `deformer.define_grid` for a GridDeformer's lattice. `grid_axes` lists Y first.
pub fn grid_request(node: u32, points: &[V2]) -> Result<Value, String> {
    let [ys, xs] = soul2d_core::mesh_edit::grid_axes(points).ok_or("The points no longer form a grid")?;
    Ok(json!({"op": "deformer.define_grid", "nodes": [node], "axisX": xs, "axisY": ys}))
}

// ----------------------------------------------------------------------------------- gestures

fn dir_of(key: Key) -> Option<V2> {
    match key {
        Key::Left => Some([-1.0, 0.0]),
        Key::Right => Some([1.0, 0.0]),
        Key::Up => Some([0.0, -1.0]),
        Key::Down => Some([0.0, 1.0]),
        _ => None,
    }
}

impl MeshTools {
    /// Feeds one pointer event to the active tool. Returns the requests to send now (deform mode:
    /// `deform.edit` per finished gesture); vertex-mode edits collect in the pending list instead.
    pub fn event(&mut self, ev: Pointer) -> Vec<Value> {
        if self.active.is_none() {
            return Vec::new();
        }
        match ev {
            Pointer::Press { at, mods } => {
                self.press(at, mods);
                Vec::new()
            }
            Pointer::Move { at, mods } => {
                self.moved(at, mods);
                Vec::new()
            }
            Pointer::Release { at, mods } => self.release(at, mods),
            Pointer::DoubleClick { at, mods } => {
                self.double_click(at, mods);
                Vec::new()
            }
            Pointer::Secondary { .. } => {
                if self.tool == Tool::Lasso {
                    self.lasso_rollback();
                }
                Vec::new()
            }
            Pointer::Key { key, mods } => self.key(key, mods),
        }
    }

    fn press_at(&self, world: V2, mods: Mods) -> Press {
        let local = self.to_local(world);
        let hit = match self.tool {
            Tool::PathDeform if !self.path_transform => nearest_point(&self.path, local, self.local_zoom()),
            Tool::PathDeform => nearest_point(&self.path_target, local, self.local_zoom()),
            Tool::Bezier => nearest_point(&self.points(), local, self.local_zoom()),
            _ => self.vertex_at(local),
        };
        let maybe = hit.is_some_and(|v| self.selected.contains(&v)) && !mods.shift;
        Press { world, local, hit, maybe, mods }
    }

    fn press(&mut self, world: V2, mods: Mods) {
        self.gesture = Gesture::Idle;
        let press = self.press_at(world, mods);
        self.cursor = Some(press.local);
        match self.tool {
            Tool::Point => self.point_press(press),
            Tool::Brush if mods.alt => self.point_press(press),
            Tool::Brush if self.settings.brush_mode == BrushMode::Inflate => match self.settings.brush_value() {
                Ok(brush) if self.settings.inflate_rate.is_finite() && (0.0..=2.0).contains(&self.settings.inflate_rate) => {
                    self.notice.clear();
                    let revision = self.active.as_ref().map_or(0, |active| active.revision);
                    self.gesture = Gesture::Inflate {
                        revision,
                        brush,
                        selected: self.selected.clone(),
                        samples: Vec::new(),
                        preview: self.points(),
                        center: press.local,
                        rate: self.settings.inflate_rate,
                        elapsed: 0.0,
                        options: self.live_options(),
                    };
                }
                _ => self.notice = "Choose a valid brush and inflation rate between 0 and 2".to_owned(),
            },
            Tool::Brush => self.gesture = Gesture::Pending(press),
            Tool::Connect => self.connect_press(&press),
            Tool::Grid => self.gesture = Gesture::Pending(press),
            Tool::EdgeCutter => self.gesture = Gesture::Cut { from: press.local, now: press.local },
            Tool::Lasso => self.lasso_press(&press),
            Tool::PathDeform => self.path_press(press),
            Tool::Bezier if self.mode() == Some(Mode::Vertex) => self.point_press(press),
            Tool::Bezier => self.bezier_press(press),
        }
    }

    /// Selection part of a press, shared by the tools that select like the Point tool.
    fn point_press(&mut self, press: Press) {
        let vertex_mode = self.mode() == Some(Mode::Vertex);
        if vertex_mode && press.mods.ctrl && !press.mods.shift && self.tool == Tool::Point {
            match press.hit {
                Some(_) => self.push_edit(json!({"op": "remove", "at": press.local}), After::Clear),
                None => self.add_vertex(press.local, true),
            }
            return;
        }
        match press.hit {
            Some(v) => {
                if press.mods.shift {
                    self.toggle(v);
                } else if !self.selected.contains(&v) {
                    self.selected = vec![v];
                }
                self.gesture = Gesture::Pending(press);
            }
            None => {
                self.gesture = Gesture::Box { origin: press.local, now: press.local, mods: press.mods };
            }
        }
    }

    fn toggle(&mut self, v: usize) {
        match self.selected.iter().position(|&s| s == v) {
            Some(at) => {
                self.selected.remove(at);
            }
            None => self.selected.push(v),
        }
    }

    fn add_vertex(&mut self, local: V2, ctrl: bool) {
        let before = self.points().len();
        let after = if ctrl { After::Last } else { After::From(before) };
        self.push_edit(json!({"op": "add", "pos": local}), after);
    }

    /// `ConnectTool`: click two vertices to toggle the edge between them; Shift keeps the second one as the
    /// next anchor.
    fn connect_press(&mut self, press: &Press) {
        let Some(v) = press.hit else {
            self.selected.clear();
            return;
        };
        match self.selected.last().copied() {
            Some(anchor) if anchor != v => {
                self.selected = vec![v];
                self.push_edit(json!({"op": "toggle", "a": anchor, "b": v}), After::Keep);
                if !press.mods.shift {
                    self.selected.clear();
                }
            }
            Some(_) => self.selected.clear(),
            None => self.selected = vec![v],
        }
    }

    fn lasso_press(&mut self, press: &Press) {
        self.lasso_checkpoints.push(self.lasso.len());
        self.lasso.push(press.local);
        self.gesture = Gesture::LassoDown;
        if self.lasso.len() > 1 && dist(self.lasso[0], press.local) < LASSO_PX / self.local_zoom() {
            self.lasso_select(press.mods);
        }
    }

    fn lasso_select(&mut self, mods: Mods) {
        if self.lasso.len() < 3 {
            return;
        }
        let points = self.points();
        if let Ok(hits) = select_lasso(&points, &self.lasso) {
            let mode = match (mods.shift, mods.ctrl) {
                (true, true) => SelectMode::Toggle,
                (true, false) => SelectMode::Add,
                (false, true) => SelectMode::Remove,
                (false, false) => SelectMode::Replace,
            };
            self.selected = combine_selection(&self.selected, &hits, mode);
        }
        self.lasso.clear();
        self.lasso_checkpoints.clear();
    }

    fn lasso_rollback(&mut self) {
        if let Some(len) = self.lasso_checkpoints.pop() {
            self.lasso.truncate(len);
        }
    }

    fn path_press(&mut self, press: Press) {
        if press.mods.alt {
            // Alt picks the vertices the path moves.
            let hit = self.vertex_at(press.local);
            self.point_press(Press { hit, maybe: hit.is_some_and(|v| self.selected.contains(&v)), ..press });
            return;
        }
        if self.path_transform && press.hit.is_some() && (press.mods.ctrl || self.rotate_mode) {
            self.locked = if self.locked == press.hit { None } else { press.hit };
            self.rotate_mode = false;
            return;
        }
        self.gesture = Gesture::Pending(press);
    }

    fn bezier_press(&mut self, press: Press) {
        match press.hit {
            Some(v) if press.mods.ctrl || self.rotate_mode => {
                self.locked = if self.locked == Some(v) { None } else { Some(v) };
                self.selected.clear();
                self.rotate_mode = false;
            }
            Some(v) => {
                if !self.selected.contains(&v) {
                    self.selected = vec![v];
                }
                self.gesture = Gesture::Pending(press);
            }
            None => {
                self.gesture = Gesture::Box { origin: press.local, now: press.local, mods: press.mods };
            }
        }
    }

    fn moved(&mut self, world: V2, mods: Mods) {
        let local = self.to_local(world);
        self.cursor = Some(local);
        if let Gesture::Inflate { center, .. } = &mut self.gesture {
            *center = local;
            return;
        }
        match self.gesture.clone() {
            Gesture::Pending(press) => {
                if dist(world, press.world) * self.active.as_ref().map_or(1.0, |a| a.zoom) > DRAG_PX {
                    self.begin_drag(&press, local, mods);
                }
            }
            Gesture::Box { origin, .. } => self.gesture = Gesture::Box { origin, now: local, mods },
            Gesture::Move { from, anchor, .. } => {
                let now = self.adhere_move(from, anchor, world, local);
                self.gesture = Gesture::Move { from, now, anchor };
            }
            Gesture::Brush { mut stroke, .. } => {
                stroke.push(local);
                self.gesture = Gesture::Brush { stroke, shift: mods.shift };
            }
            Gesture::Inflate { .. } => {}
            Gesture::Cut { from, .. } => self.gesture = Gesture::Cut { from, now: local },
            Gesture::GridCreate { from, .. } => self.gesture = Gesture::GridCreate { from, now: local },
            Gesture::GridMove { vertex, .. } => self.gesture = Gesture::GridMove { vertex, now: local },
            Gesture::PathDrag { index, from, .. } => {
                let shift = mods.shift || self.shift_mode;
                self.gesture = Gesture::PathDrag { index, from, now: local, shift };
            }
            Gesture::BezierDrag { index, from, .. } => {
                let shift = mods.shift || self.shift_mode;
                self.gesture = Gesture::BezierDrag { index, from, now: local, shift };
            }
            Gesture::LassoDown => {
                let far = self.lasso.last().is_some_and(|last| dist(*last, local) > LASSO_PX / self.local_zoom());
                if far {
                    self.lasso.push(local);
                    if dist(self.lasso[0], local) < LASSO_PX / self.local_zoom() {
                        self.lasso_select(mods);
                        self.gesture = Gesture::Idle;
                    }
                }
            }
            Gesture::Idle => {}
        }
    }

    fn begin_drag(&mut self, press: &Press, now: V2, mods: Mods) {
        self.attached.clear();
        let vertex_mode = self.mode() == Some(Mode::Vertex);
        self.gesture = match self.tool {
            Tool::Point | Tool::Brush if press.mods.alt || self.tool == Tool::Point => {
                if press.hit.is_some() {
                    // A Shift press can deselect the vertex it lands on; the vertices that move are the selected ones.
                    Gesture::Move { from: now, now, anchor: press.hit.filter(|v| self.selected.contains(v)) }
                } else {
                    Gesture::Idle
                }
            }
            Tool::Brush if !vertex_mode => Gesture::Brush { stroke: vec![now], shift: mods.shift },
            Tool::Grid => {
                let is_grid = geometry_grid(&self.points()).is_some();
                match press.hit {
                    Some(vertex) if is_grid => Gesture::GridMove { vertex, now },
                    None if !is_grid => Gesture::GridCreate { from: now, now },
                    _ => Gesture::Idle,
                }
            }
            Tool::PathDeform => match press.hit {
                Some(index) if self.path_transform && self.locked != Some(index) => {
                    Gesture::PathDrag { index, from: now, now, shift: press.mods.shift || self.shift_mode }
                }
                Some(index) if !self.path_transform => {
                    // Define mode drags the control point itself.
                    self.path[index] = now;
                    Gesture::Idle
                }
                _ => Gesture::Idle,
            },
            // Vertex definition drags the selected control points, like the Point tool.
            Tool::Bezier if vertex_mode => {
                if press.hit.is_some() { Gesture::Move { from: now, now, anchor: None } } else { Gesture::Idle }
            }
            Tool::Bezier => match (self.selected.as_slice(), press.hit) {
                ([index], Some(_)) if self.locked != Some(*index) => Gesture::BezierDrag {
                    index: *index,
                    from: now,
                    now,
                    shift: press.mods.shift || mods.shift || self.shift_mode,
                },
                _ => Gesture::Idle,
            },
            _ => Gesture::Idle,
        };
    }

    fn tick_inflate(&mut self, dt: f64, shift: bool, finish: bool) {
        if !matches!(self.gesture, Gesture::Inflate { .. }) {
            return;
        }
        if !dt.is_finite() || !(0.0..=0.25).contains(&dt) {
            self.gesture = Gesture::Idle;
            self.down = false;
            self.notice = "Inflation cancelled after a timing interruption".to_owned();
            return;
        }
        let mut error = None;
        let mut limit = false;
        if let Gesture::Inflate { brush, selected, samples, preview, center, rate, elapsed, .. } = &mut self.gesture {
            *elapsed += dt;
            let mut next = Vec::new();
            let step = 1.0 / 60.0;
            let sign = if shift { -1.0 } else { 1.0 };
            while *elapsed + 1e-12 >= step && samples.len() + next.len() < MAX_INFLATE_SAMPLES {
                next.push(InflateSample { center: *center, amount: *rate * step * sign });
                *elapsed = (*elapsed - step).max(0.0);
            }
            if finish && *elapsed > 1e-12 && samples.len() + next.len() < MAX_INFLATE_SAMPLES {
                next.push(InflateSample { center: *center, amount: *rate * *elapsed * sign });
                *elapsed = 0.0;
            }
            if samples.len() + next.len() == MAX_INFLATE_SAMPLES {
                limit = true;
                *elapsed = 0.0;
            }
            if !next.is_empty() {
                let mut moved = preview.clone();
                match inflate_stroke(&mut moved, selected, brush, &next) {
                    Ok(()) => {
                        *preview = moved;
                        samples.extend(next);
                    }
                    Err(message) => error = Some(message),
                }
            }
        }
        if limit {
            self.notice = "Stroke limit reached. Release to apply this stroke.".to_owned();
        }
        if let Some(message) = error {
            self.notice = message;
            self.gesture = Gesture::Idle;
            self.down = false;
        }
    }

    fn release(&mut self, world: V2, mods: Mods) -> Vec<Value> {
        self.moved(world, mods);
        let gesture = std::mem::replace(&mut self.gesture, Gesture::Idle);
        let mut requests = Vec::new();
        match gesture {
            Gesture::Pending(press) => {
                if press.maybe {
                    if let Some(v) = press.hit {
                        self.selected = vec![v];
                    }
                }
            }
            Gesture::Box { origin, now, mods } => {
                let points = self.points();
                if let Ok(hits) = select_rect(&points, origin, now) {
                    let mode = match (mods.shift, mods.ctrl) {
                        (true, true) => SelectMode::Remove,
                        (true, false) => SelectMode::Add,
                        _ => SelectMode::Replace,
                    };
                    self.selected = combine_selection(&self.selected, &hits, mode);
                }
            }
            Gesture::LassoDown => {
                // Poly lasso closes only on its first point; the regular one also closes on release.
                if !self.settings.poly_lasso {
                    self.lasso_select(mods);
                }
            }
            Gesture::Idle => {}
            Gesture::Inflate { revision, selected, brush, samples, preview, options, .. } => {
                self.notice.clear();
                if let Some(active) = &self.active {
                    if !samples.is_empty() && preview != active.base {
                        let tool = DeformTool::Inflate { selected, brush, samples };
                        let mut request = deform_request(active.target.id, active.target.parameter, active.target.key, &tool, &options);
                        request["expectedRevision"] = json!(revision);
                        requests.push(request);
                    }
                }
            }
            other => {
                self.gesture = other;
                let draft = self.draft();
                let after = self.after_of_gesture();
                self.gesture = Gesture::Idle;
                match draft {
                    Some(Draft::Edit(value)) => self.push_edit(value, after),
                    Some(Draft::Deform(tool)) => requests.extend(self.deform_commit(&tool)),
                    None => {}
                }
            }
        }
        requests
    }

    fn after_of_gesture(&self) -> After {
        match self.gesture {
            Gesture::GridCreate { .. } | Gesture::GridMove { .. } => After::Clear,
            _ => After::Keep,
        }
    }

    fn deform_commit(&mut self, tool: &DeformTool) -> Option<Value> {
        let active = self.active.as_ref()?;
        let request = deform_request(active.target.id, active.target.parameter, active.target.key, tool, &self.live_options());
        if let DeformTool::PathDeform { target, .. } = tool {
            // The document now follows `target`; the next drag starts from that shape.
            self.path = target.clone();
            self.path_target = target.clone();
        }
        Some(request)
    }

    fn double_click(&mut self, world: V2, mods: Mods) {
        let press = self.press_at(world, mods);
        match self.tool {
            Tool::Point if self.mode() == Some(Mode::Vertex) && !mods.shift && !mods.ctrl => match press.hit {
                Some(v) if self.selected.contains(&v) => {
                    self.push_edit(json!({"op": "remove", "at": press.local}), After::Clear)
                }
                Some(_) => {}
                None => self.add_vertex(press.local, false),
            },
            Tool::Grid => {
                if geometry_grid(&self.points()).is_none() {
                    return;
                }
                match press.hit {
                    Some(vertex) => self.push_edit(json!({"op": "grid_remove_axis", "vertex": vertex}), After::Clear),
                    None => self.push_edit(json!({"op": "grid_add_axis", "pos": press.local}), After::Clear),
                }
            }
            Tool::Bezier if self.mode() == Some(Mode::Vertex) => self.bezier_define(&press),
            Tool::PathDeform if !self.path_transform => {
                let zoom = self.local_zoom();
                match nearest_point(&self.path, press.local, zoom) {
                    Some(i) => {
                        self.path.remove(i);
                    }
                    None => path_add_point(&mut self.path, press.local, zoom),
                }
                self.locked = None;
            }
            _ => {}
        }
    }

    /// Bezier tool double click in vertex mode: remove the control point under the pointer, or add one at
    /// the clicked place of the curve.
    fn bezier_define(&mut self, press: &Press) {
        match press.hit {
            Some(index) => self.push_edit(json!({"op": "remove", "indices": [index]}), After::Clear),
            None => {
                let curve = self.active.as_ref().map_or(CurveKind::Spline, |a| a.geometry.curve);
                let (index, pos) = bezier_insertion(curve, &self.points(), press.local, self.local_zoom());
                self.push_edit(json!({"op": "insert", "index": index, "pos": pos}), After::Clear);
            }
        }
        self.locked = None;
    }

    fn key(&mut self, key: Key, mods: Mods) -> Vec<Value> {
        let vertex_mode = self.mode() == Some(Mode::Vertex);
        match key {
            Key::Escape => {
                if matches!(self.gesture, Gesture::Inflate { .. }) {
                    self.down = false;
                }
                self.gesture = Gesture::Idle;
                self.lasso.clear();
                self.lasso_checkpoints.clear();
                Vec::new()
            }
            Key::Delete if vertex_mode && self.tool == Tool::Point && !self.selected.is_empty() => {
                let indices = self.selected.clone();
                self.push_edit(json!({"op": "remove", "indices": indices}), After::Clear);
                Vec::new()
            }
            Key::Delete => Vec::new(),
            _ if self.tool == Tool::Point && !self.selected.is_empty() => {
                let Some(dir) = dir_of(key) else { return Vec::new() };
                let magnitude = if mods.alt {
                    1.0
                } else if mods.shift {
                    100.0
                } else {
                    NUDGE
                };
                let delta = [dir[0] * magnitude, dir[1] * magnitude];
                let selected = self.selected.clone();
                if vertex_mode {
                    self.push_edit(json!({"op": "move", "indices": selected, "delta": delta}), After::Keep);
                    Vec::new()
                } else {
                    self.deform_commit(&DeformTool::Translate { selected, delta }).into_iter().collect()
                }
            }
            _ => Vec::new(),
        }
    }

    // ---------------------------------------------------------------------------------- drafts

    fn draft(&self) -> Option<Draft> {
        let vertex_mode = self.mode()? == Mode::Vertex;
        match &self.gesture {
            Gesture::Move { from, now, .. } if !self.selected.is_empty() => {
                let delta = sub(*now, *from);
                Some(if vertex_mode {
                    Draft::Edit(json!({"op": "move", "indices": self.selected, "delta": delta}))
                } else {
                    Draft::Deform(DeformTool::Translate { selected: self.selected.clone(), delta })
                })
            }
            Gesture::Brush { stroke, shift } if stroke.len() >= 2 => {
                let brush = self.settings.brush_value().ok()?;
                Some(Draft::Deform(DeformTool::Brush {
                    selected: self.selected.clone(),
                    brush,
                    flow: self.settings.brush_mode == BrushMode::Flow,
                    stroke: stroke.clone(),
                    snap: *shift,
                }))
            }
            Gesture::Cut { from, now } if from != now => Some(Draft::Edit(json!({"op": "cut", "from": from, "to": now}))),
            Gesture::GridCreate { from, now } if from != now => Some(Draft::Edit(
                json!({"op": "grid_create", "bounds": [from[0], from[1], now[0], now[1]], "cuts": self.settings.grid_cuts}),
            )),
            Gesture::GridMove { vertex, now } => Some(Draft::Edit(json!({"op": "grid_move", "vertex": vertex, "to": now}))),
            Gesture::PathDrag { index, from, now, shift } if self.path.len() >= 2 => {
                let target = self.path_drag_target(*index, *from, *now, *shift);
                Some(Draft::Deform(DeformTool::PathDeform {
                    selected: self.selected.clone(),
                    path: self.path.clone(),
                    target,
                    resolution: self.settings.path_resolution,
                }))
            }
            Gesture::BezierDrag { index, from, now, shift } => Some(Draft::Deform(match self.locked {
                Some(locked) => DeformTool::BezierRotate { locked, index: *index, from: *from, to: *now },
                None if *shift => DeformTool::BezierTranslate { index: *index, delta: self.bezier_shift(*index, *now) },
                None => DeformTool::BezierTranslate { index: *index, delta: sub(*now, *from) },
            })),
            _ => None,
        }
    }

    /// The move that puts control point `index` on the curve through the points as they are now, at the
    /// place nearest to the pointer (`BezierDeformActionID.Shift`). As a translation it commits like a
    /// plain drag, so the session and the preview share `BezierTranslate`.
    fn bezier_shift(&self, index: usize, now: V2) -> V2 {
        let points = self.points();
        let curve = self.active.as_ref().map_or(CurveKind::Spline, |a| a.geometry.curve);
        let Some(current) = points.get(index) else { return [0.0, 0.0] };
        sub(curve_point(curve, &points, curve_closest_t(curve, &points, now)), *current)
    }

    /// The path's moved shape for a control point drag (`PathDeformTool.update`): translate the point,
    /// slide it along the path, or turn the points beyond the locked one about it.
    fn path_drag_target(&self, index: usize, from: V2, now: V2, shift: bool) -> Vec<V2> {
        let mut target = self.path_target.clone();
        if let Some(locked) = self.locked.filter(|&l| l < target.len()) {
            let pivot = target[locked];
            let angle = (now[1] - pivot[1]).atan2(now[0] - pivot[0]) - (from[1] - pivot[1]).atan2(from[0] - pivot[0]);
            let (sin, cos) = angle.sin_cos();
            let step: isize = if index > locked { 1 } else { -1 };
            let mut i = locked as isize + step;
            while i >= 0 && (i as usize) < target.len() {
                let rel = sub(target[i as usize], pivot);
                target[i as usize] = [pivot[0] + rel[0] * cos - rel[1] * sin, pivot[1] + rel[0] * sin + rel[1] * cos];
                i += step;
            }
        } else if shift {
            target[index] = spline_eval(&self.path, closest_offset(&self.path, now));
        } else {
            target[index] = add(target[index], sub(now, from));
        }
        target
    }
}

fn geometry_grid(points: &[V2]) -> Option<[Vec<f64>; 2]> {
    soul2d_core::mesh_edit::grid_axes(points)
}

// ------------------------------------------------------------------------------ pending edits

impl MeshTools {
    /// Appends a vertex-definition edit to the pending list. The new list is replayed on the Part's mesh
    /// through `apply_edits`; an edit the session would refuse is dropped here with a notice, so Apply
    /// cannot fail on something the user already saw accepted.
    fn push_edit(&mut self, edit: Value, after: After) {
        let Some(active) = &self.active else { return };
        if active.target.mode != Mode::Vertex {
            return;
        }
        let mut edits = self.pending.clone();
        edits.push(edit);
        let options = self.pending_options.unwrap_or_else(|| self.fresh_options());
        let count = self.points().len();
        match replay(active, &edits, &options) {
            Ok(mesh) => {
                self.pending = edits;
                self.undone.clear();
                self.pending_options = Some(options);
                let len = mesh.verts.len();
                self.selected = match after {
                    // A triangulation that merged coincident points renumbers the vertices after them.
                    After::Keep if count != len => Vec::new(),
                    After::Keep => self.selected.iter().copied().filter(|&i| i < len).collect(),
                    After::Clear => Vec::new(),
                    After::Last => len.checked_sub(1).into_iter().collect(),
                    After::From(first) => (first < len).then_some(first).into_iter().collect(),
                };
                self.mesh = Some(mesh);
                self.notice.clear();
            }
            Err(error) => self.notice = error,
        }
    }

    fn rebuild(&mut self) {
        let Some(active) = &self.active else { return };
        let options = self.pending_options.unwrap_or_else(|| self.fresh_options());
        match replay(active, &self.pending, &options) {
            Ok(mesh) => self.mesh = Some(mesh),
            Err(error) => {
                self.notice = error;
                self.pending.clear();
                self.mesh = active.base_mesh.clone();
            }
        }
        if self.pending.is_empty() {
            self.pending_options = None;
        }
        self.selected.clear();
    }

    /// Drops the last pending edit (D's undo inside the mesh editor).
    pub fn undo_edit(&mut self) {
        if let Some(edit) = self.pending.pop() {
            self.undone.push(edit);
            self.rebuild();
        }
    }

    pub fn redo_edit(&mut self) {
        if let Some(edit) = self.undone.pop() {
            let undone = std::mem::take(&mut self.undone);
            self.push_edit(edit, After::Clear);
            self.undone = undone;
        }
    }

    pub fn can_undo_edit(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn can_redo_edit(&self) -> bool {
        !self.undone.is_empty()
    }

    /// Triangulate button: replaces the connections by a Delaunay triangulation, shown as the preview.
    pub fn triangulate(&mut self) {
        self.push_edit(json!({"op": "triangulate"}), After::Keep);
    }

    pub fn flip(&mut self, vertical: bool) {
        let axis = if vertical { "v" } else { "h" };
        self.push_edit(json!({"op": "flip", "axis": axis}), After::Keep);
    }

    /// Apply, then leave the mode. A Part sends the whole pending list as ONE `mesh.edit`; a MeshGroup, a
    /// PathDeformer and a GridDeformer send the finished geometry of the preview as ONE
    /// `deformer.define_mesh` / `define_vertices` / `define_grid`. Either way it is one undo step. `None`
    /// without edits, and `None` with a notice (the mode stays open) when the geometry cannot be stored.
    pub fn apply(&mut self) -> Option<Value> {
        let active = self.active.as_ref().filter(|a| a.target.mode == Mode::Vertex)?;
        let (id, kind) = (active.target.id, active.geometry.kind);
        if self.pending.is_empty() {
            self.cancel();
            return None;
        }
        if let Some(reason) = active.blocked.clone() {
            // Keep the edits: the user can still Cancel, but must not lose them to a refusal.
            self.notice = reason;
            return None;
        }
        let request = match (kind, &self.mesh) {
            (NodeKind::Part, _) => Ok(mesh_request(id, &self.pending, &self.options())),
            (_, None) => Err("the working copy is missing".to_owned()),
            (NodeKind::MeshGroup, Some(mesh)) => {
                soul2d_core::mesh_edit::build_mesh(mesh, None).and_then(|built| mesh_group_request(id, &built))
            }
            (NodeKind::PathDeformer, Some(mesh)) => path_request(id, &mesh.positions()),
            (NodeKind::GridDeformer, Some(mesh)) => grid_request(id, &mesh.positions()),
        };
        match request {
            Ok(request) => {
                self.cancel();
                Some(request)
            }
            Err(error) => {
                self.notice = error;
                None
            }
        }
    }

    /// Cancel: drop the working copy; the document is untouched.
    pub fn cancel(&mut self) {
        self.vertex_for = None;
        self.active = None;
        self.reset_edit_state();
    }

    /// Deform mode "Reset selected": the selected vertices back to their rest positions.
    pub fn reset_selection(&mut self) -> Option<Value> {
        if self.selected.is_empty() || self.mode() != Some(Mode::Deform) {
            return None;
        }
        self.deform_commit(&DeformTool::Reset { selected: self.selected.clone() })
    }

    pub fn can_reset_selection(&self) -> bool {
        !self.selected.is_empty() && self.mode() == Some(Mode::Deform)
    }

    pub fn set_tool(&mut self, tool: Tool) {
        if self.tool_offered().contains(&tool) && self.tool != tool {
            if matches!(self.gesture, Gesture::Inflate { .. }) {
                self.down = false;
            }
            self.tool = tool;
            self.gesture = Gesture::Idle;
            self.lasso.clear();
            self.lasso_checkpoints.clear();
            self.locked = None;
            self.rotate_mode = false;
            self.shift_mode = false;
        }
    }

    /// "Define paths" / "Transform path". Transform needs two control points.
    pub fn set_path_transform(&mut self, transform: bool) {
        if transform && !self.path_transform {
            if self.path.len() < 2 {
                self.notice = "Define a path with at least two points first (double click)".to_owned();
                return;
            }
            self.path_target = self.path.clone();
        }
        if self.path_transform != transform {
            self.locked = None;
        }
        self.path_transform = transform;
    }

    #[cfg(test)]
    pub fn path_transform(&self) -> bool {
        self.path_transform
    }

    /// Mirror, auto-connect and zoom of pending edits are fixed when the first edit is made.
    pub fn options_locked(&self) -> bool {
        !self.pending.is_empty() || matches!(self.gesture, Gesture::Inflate { .. })
    }

    pub fn settings_lock_note(&self) -> &'static str {
        if matches!(self.gesture, Gesture::Inflate { .. }) {
            "Locked during inflation: release to apply or press Escape to cancel"
        } else {
            "Locked while edits are pending: Apply or Cancel first"
        }
    }
}

fn replay(active: &Active, edits: &[Value], options: &EditOptions) -> Result<EditMesh, String> {
    let mut mesh = active.base_mesh.clone().ok_or("the node's points could not be read")?;
    if active.geometry.kind == NodeKind::PathDeformer {
        // `build_mesh` would sort a path whose control points happen to form a rectangle.
        replay_path(&mut mesh, edits, options)?;
        return Ok(mesh);
    }
    let parsed = edits.iter().map(parse_edit).collect::<Result<Vec<_>, _>>()?;
    apply_edits(&mut mesh, &parsed, options)?;
    if active.geometry.kind == NodeKind::GridDeformer {
        soul2d_core::mesh_edit::grid_axes(&mesh.positions()).ok_or("the lattice is no longer a grid")?;
    } else {
        soul2d_core::mesh_edit::build_mesh(&mesh, None)?;
    }
    Ok(mesh)
}

/// The PathDeformer's pending list: the crate's edits (add, remove, move) plus the Bezier tool's
/// `{"op": "insert", "index", "pos"}`, which puts a control point between two others. `apply_edits` has no
/// such edit, so this is replayed here, one edit at a time, for the preview and for Apply alike.
fn replay_path(mesh: &mut EditMesh, edits: &[Value], options: &EditOptions) -> Result<(), String> {
    for (number, edit) in edits.iter().enumerate() {
        if edit["op"].as_str() == Some("insert") {
            let index = edit["index"].as_u64().ok_or("insert needs an index")? as usize;
            let pos = pair_of(&edit["pos"]).ok_or("insert needs a position")?;
            if index > mesh.verts.len() || !pos.iter().all(|v| v.is_finite()) {
                return Err(format!("edit {number}: cannot insert a control point at {index}"));
            }
            mesh.verts.insert(index, soul2d_core::geometry::MeshVertex::new(pos));
        } else {
            apply_edits(mesh, &[parse_edit(edit)?], options).map_err(|error| format!("edit {number}: {error}"))?;
        }
    }
    Ok(())
}

fn pair_of(value: &Value) -> Option<V2> {
    let list = value.as_array().filter(|list| list.len() == 2)?;
    Some([list[0].as_f64()?, list[1].as_f64()?])
}

// ------------------------------------------------------------------------------------------- UI

fn drag_value(ui: &mut egui::Ui, label: &str, value: &mut f64, range: std::ops::RangeInclusive<f64>, hover: &str) -> bool {
    drag_value_if(ui, true, label, value, range, hover)
}

fn drag_value_if(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    hover: &str,
) -> bool {
    let name = ui.label(label);
    ui.add_enabled(enabled, egui::DragValue::new(value).range(range).speed(1.0).fixed_decimals(2))
        .labelled_by(name.id)
        .on_hover_text(hover)
        .changed()
}

impl MeshTools {
    /// The tool strip and the tool settings of the D toolbar and Tool Settings panel. Returns the requests
    /// to send (Apply, Reset selected).
    pub fn strip(&mut self, ui: &mut egui::Ui) -> Vec<Value> {
        let mut requests = Vec::new();
        let Some(mode) = self.mode() else {
            if let Some(id) = self.edit_mesh_for {
                if ui.button("Edit Mesh").on_hover_text("Edit the vertices and edges of the selected Part").clicked() {
                    self.start_edit_mesh(id);
                }
            }
            return requests;
        };
        let locked = self.options_locked();
        ui.horizontal_wrapped(|ui| {
            ui.label(match mode {
                Mode::Vertex => "Edit Mesh:",
                Mode::Deform => "Deform:",
            });
            for tool in self.tool_offered() {
                let hover = match tool {
                    Tool::Point => "Select and move vertices. Ctrl+click adds or removes a vertex",
                    Tool::Connect => "Click two vertices to connect or disconnect them",
                    Tool::Grid => "Drag to define a 2x2 grid, drag its points, double click to add or remove lines",
                    Tool::Lasso => "Select vertices inside a drawn outline",
                    Tool::EdgeCutter => "Drag a line to cut the edges it crosses",
                    Tool::Brush => "Drag to pull vertices with a soft brush",
                    Tool::PathDeform => "Define a path by double clicking, then transform it to deform the vertices",
                    Tool::Bezier => "Move the control points of the path; in Edit Mesh double click to add or remove one",
                };
                if ui.selectable_label(self.tool == tool, tool.label()).on_hover_text(hover).clicked() {
                    self.set_tool(tool);
                }
            }
            ui.separator();
            let lock_hover = if locked { self.settings_lock_note() } else { "" };
            let toggle = |ui: &mut egui::Ui, value: &mut bool, label: &str, hover: &str| {
                ui.add_enabled(!locked, egui::Checkbox::new(value, label))
                    .on_hover_text(hover)
                    .on_disabled_hover_text(lock_hover);
            };
            toggle(ui, &mut self.settings.mirror_horizontal, "Mirror Horizontally", "Edit mirrored copies across the vertical axis");
            toggle(ui, &mut self.settings.mirror_vertical, "Mirror Vertically", "Edit mirrored copies across the horizontal axis");
            if mode == Mode::Vertex {
                let meshy = self.active.as_ref().is_some_and(|a| matches!(a.geometry.kind, NodeKind::Part | NodeKind::MeshGroup));
                if self.tool == Tool::Point && meshy {
                    toggle(ui, &mut self.settings.auto_connect, "Auto connect vertices", "Link each new vertex to the previously added one");
                }
                if meshy {
                    ui.separator();
                    if ui.button("Flip Horizontally").on_hover_text("Flip Horizontally").clicked() {
                        self.flip(false);
                    }
                    if ui.button("Flip Vertically").on_hover_text("Flip Vertically").clicked() {
                        self.flip(true);
                    }
                    if ui.button("Triangulate vertices").on_hover_text("Rebuild the edges as a Delaunay triangulation").clicked() {
                        self.triangulate();
                    }
                }
                ui.separator();
                if ui.add_enabled(self.can_undo_edit(), egui::Button::new("Undo edit")).on_hover_text("Drop the last edit").clicked() {
                    self.undo_edit();
                }
                if ui.add_enabled(self.can_redo_edit(), egui::Button::new("Redo edit")).on_hover_text("Bring the dropped edit back").clicked() {
                    self.redo_edit();
                }
                if ui.button("Apply").on_hover_text("Apply all edits as one undo step").clicked() {
                    requests.extend(self.apply());
                }
                if ui.button("Cancel").on_hover_text("Discard all edits").clicked() {
                    self.cancel();
                }
            } else {
                ui.separator();
                if ui
                    .add_enabled(self.can_reset_selection(), egui::Button::new("Reset selected"))
                    .on_hover_text("Put the selected vertices back on their rest positions")
                    .clicked()
                {
                    requests.extend(self.reset_selection());
                }
            }
        });
        if self.mode().is_some() {
            self.settings_ui(ui);
        }
        if !self.notice.is_empty() {
            ui.colored_label(Color32::LIGHT_RED, self.notice.clone());
        }
        requests
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let locked = self.options_locked();
        egui::CollapsingHeader::new("Tool Settings").default_open(true).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                match self.tool {
                    Tool::Brush => {
                        ui.add_enabled_ui(!locked, |ui| {
                        egui::ComboBox::from_label("Brush")
                            .selected_text(self.settings.brush.label())
                            .show_ui(ui, |ui| {
                                for kind in BrushKind::ALL {
                                    ui.selectable_value(&mut self.settings.brush, kind, kind.label());
                                }
                            });
                        ui.selectable_value(&mut self.settings.brush_mode, BrushMode::Drag, "Drag mode")
                            .on_hover_text("Weights are fixed where the stroke starts");
                        ui.selectable_value(&mut self.settings.brush_mode, BrushMode::Flow, "Flow mode")
                            .on_hover_text("Weights follow the pointer");
                        ui.selectable_value(&mut self.settings.brush_mode, BrushMode::Inflate, "Inflate / Contract")
                            .on_hover_text("Hold to inflate; hold Shift to contract");
                        if self.settings.brush_mode == BrushMode::Inflate {
                            drag_value(ui, "Inflation rate", &mut self.settings.inflate_rate, 0.0..=2.0, "Relative change per second");
                        }
                        match self.settings.brush {
                            BrushKind::Circle => {
                                drag_value(ui, "Brush radius", &mut self.settings.radius, 1.0..=2000.0, "Radius in units");
                            }
                            BrushKind::Line => {
                                drag_value(ui, "Brush radius", &mut self.settings.radius, 1.0..=2000.0, "Half-width in local units");
                                drag_value(ui, "Line angle (degrees)", &mut self.settings.line_angle_degrees, -180.0..=180.0, "Zero is horizontal in local coordinates");
                            }
                            BrushKind::DoubleThreshold => {
                                drag_value(ui, "Brush radius", &mut self.settings.radius, 1.0..=2000.0, "Outer radius");
                                drag_value(ui, "Brush inner radius", &mut self.settings.inner_radius, 1.0..=2000.0, "Full-strength radius");
                            }
                            BrushKind::Rectangle => {
                                drag_value(ui, "Brush width", &mut self.settings.width, 1.0..=2000.0, "Width in units");
                                drag_value(ui, "Brush height", &mut self.settings.height, 1.0..=2000.0, "Height in units");
                            }
                        }
                        });
                    }
                    Tool::Lasso => {
                        ui.selectable_value(&mut self.settings.poly_lasso, true, "Poly Lasso Selection Mode")
                            .on_hover_text("Click points and close the outline on the first point");
                        ui.selectable_value(&mut self.settings.poly_lasso, false, "Regular Lasso Selection Mode")
                            .on_hover_text("Drag a free outline; releasing closes it");
                    }
                    Tool::Grid => {
                        let mut cuts = self.settings.grid_cuts as f64;
                        if drag_value(ui, "Grid cuts", &mut cuts, 2.0..=64.0, "Lines per axis of a new grid") {
                            self.settings.grid_cuts = cuts.round().max(2.0) as usize;
                        }
                    }
                    Tool::PathDeform => {
                        if ui
                            .selectable_label(!self.path_transform, "Define paths")
                            .on_hover_text("Double click to add or remove path points")
                            .clicked()
                        {
                            self.set_path_transform(false);
                        }
                        if ui
                            .selectable_label(self.path_transform, "Transform path")
                            .on_hover_text("Drag path points to deform the vertices")
                            .clicked()
                        {
                            self.set_path_transform(true);
                        }
                        ui.checkbox(&mut self.rotate_mode, "Set rotation center")
                            .on_hover_text("The next click on a path point sets the rotation center (or Ctrl+click)");
                        ui.checkbox(&mut self.shift_mode, "Move points along the path")
                            .on_hover_text("Dragged points slide along the path (or hold Shift)");
                    }
                    Tool::Bezier if self.mode() == Some(Mode::Vertex) => {
                        ui.label("Double click adds a point on the path, or removes the one under the pointer.");
                    }
                    Tool::Bezier => {
                        ui.checkbox(&mut self.rotate_mode, "Set rotation center")
                            .on_hover_text("The next click on a point sets the rotation center (or Ctrl+click)");
                        ui.checkbox(&mut self.shift_mode, "Move points along the path")
                            .on_hover_text("Dragged points slide along the path (or hold Shift)");
                    }
                    Tool::Point | Tool::Connect | Tool::EdgeCutter => {
                        ui.label("No settings for this tool.");
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                let range = -100000.0..=100000.0;
                let hover = "Mirror axis position";
                drag_value_if(ui, !locked, "Mirror origin X", &mut self.settings.mirror_origin[0], range.clone(), hover);
                drag_value_if(ui, !locked, "Mirror origin Y", &mut self.settings.mirror_origin[1], range, hover);
            });
        });
    }
}

// ---------------------------------------------------------------------------------- pointer feed

fn mods_of(m: egui::Modifiers) -> Mods {
    Mods { ctrl: m.ctrl || m.command, shift: m.shift, alt: m.alt }
}

fn key_of(key: egui::Key) -> Option<Key> {
    Some(match key {
        egui::Key::Delete | egui::Key::Backspace => Key::Delete,
        egui::Key::Escape => Key::Escape,
        egui::Key::ArrowLeft => Key::Left,
        egui::Key::ArrowRight => Key::Right,
        egui::Key::ArrowUp => Key::Up,
        egui::Key::ArrowDown => Key::Down,
        _ => return None,
    })
}

/// The view of the picture the tools map the pointer with.
pub struct Picture {
    pub rect: Rect,
    pub camera: crate::editor_viewport::Camera,
    pub size: V2,
}

impl Picture {
    fn world(&self, pos: Pos2) -> V2 {
        self.camera.to_world(self.size, [f64::from(pos.x - self.rect.min.x), f64::from(pos.y - self.rect.min.y)])
    }

    fn pos(&self, world: V2) -> Pos2 {
        let s = self.camera.to_screen(self.size, world);
        Pos2::new(self.rect.min.x + s[0] as f32, self.rect.min.y + s[1] as f32)
    }
}

impl MeshTools {
    /// Turns this frame's egui events over the picture into pointer events for the active tool. Returns the
    /// requests to send; call before the viewport renders.
    pub fn pointer(&mut self, ui: &egui::Ui, picture: &Picture, response: &egui::Response) -> Vec<Value> {
        let mut requests = Vec::new();
        if !self.captures() {
            return requests;
        }
        let over = response.hovered() || response.contains_pointer();
        let (events, double, wants_keys, dt, focused, modifiers) = ui.input(|input| {
            (
                input.events.clone(),
                input.pointer.button_double_clicked(egui::PointerButton::Primary),
                false,
                f64::from(input.unstable_dt),
                input.focused,
                input.modifiers,
            )
        });
        if !focused && matches!(self.gesture, Gesture::Inflate { .. }) {
            self.gesture = Gesture::Idle;
            self.down = false;
            self.notice = "Inflation cancelled because the window lost focus".to_owned();
            return requests;
        }
        let wants_keys = wants_keys || ui.ctx().wants_keyboard_input();
        let mut last = mods_of(modifiers);
        let mut started = false;
        for event in events {
            match event {
                egui::Event::PointerMoved(pos) if (self.down || picture.rect.contains(pos)) && (self.down || over) => {
                    requests.extend(self.event(Pointer::Move { at: picture.world(pos), mods: last }));
                }
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers } => {
                    last = mods_of(modifiers);
                    let at = picture.world(pos);
                    if pressed && !self.down && picture.rect.contains(pos) && over {
                        self.down = true;
                        requests.extend(self.event(Pointer::Press { at, mods: last }));
                        started = matches!(self.gesture, Gesture::Inflate { .. });
                    } else if !pressed && self.down {
                        if matches!(self.gesture, Gesture::Inflate { .. }) {
                            self.moved(at, last);
                            self.tick_inflate(if started { 0.0 } else { dt }, last.shift, true);
                        }
                        self.down = false;
                        requests.extend(self.event(Pointer::Release { at, mods: last }));
                        if double {
                            requests.extend(self.event(Pointer::DoubleClick { at, mods: last }));
                        }
                    }
                }
                egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed: true, modifiers }
                    if picture.rect.contains(pos) && over =>
                {
                    requests.extend(self.event(Pointer::Secondary { at: picture.world(pos), mods: mods_of(modifiers) }));
                }
                egui::Event::Key { key, pressed: true, modifiers, .. }
                    if (over && !wants_keys) || (key == egui::Key::Escape && matches!(self.gesture, Gesture::Inflate { .. })) =>
                {
                    let holding = matches!(self.gesture, Gesture::Inflate { .. });
                    if let Some(key) = key_of(key) {
                        requests.extend(self.event(Pointer::Key { key, mods: mods_of(modifiers) }));
                        if key == Key::Escape && holding {
                            self.down = false;
                        }
                    }
                }
                _ => {}
            }
        }
        if matches!(self.gesture, Gesture::Inflate { .. }) {
            if !started {
                self.tick_inflate(dt, last.shift, false);
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(1.0 / 60.0));
        }
        requests
    }
}

// --------------------------------------------------------------------------------------- painting

const EDGE: Color32 = Color32::from_gray(128);
const SELECTED: Color32 = Color32::from_rgb(255, 0, 0);
const MIRROR: Color32 = Color32::from_rgb(204, 0, 204);

impl MeshTools {
    /// Vertices, edges, selection, the running gesture and the tool cursor, over the picture.
    pub fn paint(&self, ui: &egui::Ui, picture: &Picture) {
        let Some(active) = &self.active else { return };
        let painter = ui.painter_at(picture.rect);
        let frame = active.frame;
        let at = |local: V2| picture.pos(frame.to_world(local));
        let (points, edges) = self.preview();
        // A path being defined shows its curve; the polyline between control points is not the path.
        let defining_path = active.target.mode == Mode::Vertex && active.geometry.kind == NodeKind::PathDeformer;
        if defining_path {
            for pair in curve_polyline(active.geometry.curve, &points, 100).windows(2) {
                painter.line_segment([at(pair[0]), at(pair[1])], Stroke::new(1.0, EDGE));
            }
        } else {
            for [a, b] in &edges {
                if let (Some(pa), Some(pb)) = (points.get(*a), points.get(*b)) {
                    painter.line_segment([at(*pa), at(*pb)], Stroke::new(1.0, EDGE));
                }
            }
        }
        if active.target.mode == Mode::Deform && !points.is_empty() {
            // The node origin, as D marks it in deform mode.
            let origin = if active.geometry.kind == NodeKind::Part { active.geometry.origin } else { [0.0, 0.0] };
            painter.circle_filled(at(origin), 3.0, Color32::from_rgb(128, 255, 128));
        }
        self.paint_mirror(&painter, picture, &frame);
        let hover = self
            .cursor
            .filter(|_| !matches!(self.gesture, Gesture::Box { .. }))
            .and_then(|c| self.vertex_at(c));
        for (i, p) in points.iter().enumerate() {
            let pos = at(*p);
            let color = if self.selected.contains(&i) { SELECTED } else { Color32::WHITE };
            painter.circle_filled(pos, 5.0, Color32::BLACK);
            painter.circle_filled(pos, 3.0, color);
            if hover == Some(i) {
                painter.circle_filled(pos, 7.5, Color32::from_white_alpha(76));
            }
        }
        self.paint_gesture(&painter, picture, &frame);
    }

    fn paint_mirror(&self, painter: &egui::Painter, picture: &Picture, frame: &Frame) {
        let s = &self.settings;
        let far = 1.0e5;
        let stroke = Stroke::new(1.0, MIRROR);
        let at = |local: V2| picture.pos(frame.to_world(local));
        if s.mirror_horizontal {
            painter.line_segment([at([s.mirror_origin[0], -far]), at([s.mirror_origin[0], far])], stroke);
        }
        if s.mirror_vertical {
            // `mirrorV` of the session reflects about y = origin.x, so the axis is drawn there.
            painter.line_segment([at([-far, s.mirror_origin[0]]), at([far, s.mirror_origin[0]])], stroke);
        }
    }

    fn paint_gesture(&self, painter: &egui::Painter, picture: &Picture, frame: &Frame) {
        let at = |local: V2| picture.pos(frame.to_world(local));
        let line = |a: V2, b: V2, stroke: Stroke| painter.line_segment([at(a), at(b)], stroke);
        let polyline = |pts: &[V2], stroke: Stroke| {
            for w in pts.windows(2) {
                line(w[0], w[1], stroke);
            }
        };
        match &self.gesture {
            Gesture::Box { origin, now, mods } => {
                let color = match (mods.shift, mods.ctrl) {
                    (true, true) => Color32::from_rgba_unmultiplied(0, 255, 255, 204),
                    (true, false) => Color32::from_rgba_unmultiplied(0, 255, 0, 204),
                    _ => SELECTED,
                };
                let corners = [*origin, [now[0], origin[1]], *now, [origin[0], now[1]]];
                for i in 0..4 {
                    line(corners[i], corners[(i + 1) % 4], Stroke::new(1.0, color));
                }
            }
            Gesture::Cut { from, now } => {
                line(*from, *now, Stroke::new(3.0, Color32::BLACK));
                line(*from, *now, Stroke::new(1.0, SELECTED));
            }
            Gesture::GridCreate { from, now } => {
                let cuts = self.settings.grid_cuts.max(2);
                let (x0, x1) = (from[0].min(now[0]), from[0].max(now[0]));
                let (y0, y1) = (from[1].min(now[1]), from[1].max(now[1]));
                for i in 0..cuts {
                    let f = i as f64 / (cuts - 1) as f64;
                    line([x0, y0 + (y1 - y0) * f], [x1, y0 + (y1 - y0) * f], Stroke::new(1.0, EDGE));
                    line([x0 + (x1 - x0) * f, y0], [x0 + (x1 - x0) * f, y1], Stroke::new(1.0, EDGE));
                }
            }
            _ => {}
        }
        if self.tool == Tool::Lasso && !self.lasso.is_empty() {
            let mut pts = self.lasso.clone();
            if let Some(c) = self.cursor {
                pts.push(c);
            }
            polyline(&pts, Stroke::new(3.0, Color32::BLACK));
            polyline(&pts, Stroke::new(1.0, Color32::WHITE));
            let near = self.cursor.is_some_and(|c| dist(self.lasso[0], c) < LASSO_PX / self.local_zoom());
            if near {
                painter.circle_filled(at(self.lasso[0]), 5.0, SELECTED);
            } else if self.settings.poly_lasso {
                painter.circle_filled(at(self.lasso[0]), 3.5, Color32::from_rgba_unmultiplied(153, 153, 153, 153));
            }
        }
        if matches!(self.tool, Tool::PathDeform) {
            let teal = Color32::from_rgb(0, 255, 255);
            let green = Color32::from_rgb(0, 255, 0);
            let (path_color, target_color) = if self.path_transform {
                (Color32::from_rgb(0, 153, 153), green)
            } else {
                (teal, Color32::from_rgb(0, 153, 0))
            };
            let moved = match &self.gesture {
                Gesture::PathDrag { index, from, now, shift } => Some(self.path_drag_target(*index, *from, *now, *shift)),
                _ => None,
            };
            let target = moved.as_ref().unwrap_or(&self.path_target);
            if self.path_transform && target.len() >= 2 {
                polyline(&spline_polyline(target), Stroke::new(1.0, target_color));
            }
            if self.path.len() >= 2 {
                polyline(&spline_polyline(&self.path), Stroke::new(1.0, path_color));
            }
            let shown = if self.path_transform { target } else { &self.path };
            for (i, p) in shown.iter().enumerate() {
                painter.circle_filled(at(*p), 5.0, Color32::BLACK);
                let color = if self.locked == Some(i) { SELECTED } else if self.path_transform { green } else { teal };
                painter.circle_filled(at(*p), 3.0, color);
            }
        }
        if self.tool == Tool::Bezier {
            if let Some(locked) = self.locked {
                if let Some(p) = self.points().get(locked) {
                    painter.circle_filled(at(*p), 4.0, Color32::from_rgb(0, 255, 0));
                }
            }
        }
        if let (Some(c), Some(Mode::Deform)) = (self.cursor, self.mode()) {
            if self.tool == Tool::Brush {
                self.paint_brush(painter, picture, frame, c);
            } else if self.tool == Tool::Point {
                painter.circle_stroke(at(c), CURSOR_RADIUS_PX, Stroke::new(1.0, Color32::from_rgb(0, 255, 0)));
            }
        }
    }

    /// The brush outline at the pointer (`Brush.draw`).
    fn paint_brush(&self, painter: &egui::Painter, picture: &Picture, frame: &Frame, center: V2) {
        let px = |units: f64| (units * frame.scale() * picture.camera.zoom) as f32;
        let at = picture.pos(frame.to_world(center));
        let brush = match &self.gesture {
            Gesture::Inflate { brush, .. } => *brush,
            _ => match self.settings.brush_value() {
                Ok(brush) => brush,
                Err(_) => return,
            },
        };
        for (width, color) in [(3.0, Color32::BLACK), (1.0, SELECTED)] {
            let stroke = Stroke::new(width, color);
            match brush {
                Brush::Circle { radius } => {
                    painter.circle_stroke(at, px(radius), stroke);
                }
                Brush::DoubleThreshold { radius, inner } => {
                    painter.circle_stroke(at, px(radius), stroke);
                    painter.circle_stroke(at, px(inner.min(radius)), stroke);
                }
                Brush::Rectangle { width, height } => {
                    let half = egui::vec2(px(width), px(height));
                    painter.rect_stroke(Rect::from_center_size(at, half * 2.0), 0.0, stroke, egui::StrokeKind::Middle);
                }
                Brush::Line { radius, angle } => {
                    let (sin, cos) = angle.sin_cos();
                    let corners = [picture.rect.left_top(), picture.rect.right_top(), picture.rect.left_bottom(), picture.rect.right_bottom()];
                    let along = corners.map(|p| {
                        let p = frame.to_local(picture.world(p));
                        (p[0] - center[0]) * cos + (p[1] - center[1]) * sin
                    });
                    let low = along.iter().copied().fold(f64::INFINITY, f64::min);
                    let high = along.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    let painter = painter.with_clip_rect(picture.rect.intersect(painter.clip_rect()));
                    for side in [-radius, radius] {
                        let point = |t: f64| picture.pos(frame.to_world([
                            center[0] + t * cos - side * sin,
                            center[1] + t * sin + side * cos,
                        ]));
                        painter.line_segment([point(low), point(high)], stroke);
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use soul2d_core::Session;

    const NONE: Mods = Mods { ctrl: false, shift: false, alt: false };
    const CTRL: Mods = Mods { ctrl: true, shift: false, alt: false };
    const SHIFT: Mods = Mods { ctrl: false, shift: true, alt: false };
    const NO_TEXTURE: u32 = u32::MAX;

    fn transform() -> Value {
        json!({"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]})
    }

    fn part(verts: Value, indices: Value) -> Value {
        let count = verts.as_array().unwrap().len();
        json!({
            "uuid": 5, "name": "Part5", "type": "Part", "enabled": true, "zsort": 0.0,
            "transform": transform(), "children": [], "lockToRoot": false, "pinToMesh": false,
            "mesh": {"verts": verts, "uvs": vec![0.0; count], "indices": indices, "origin": [0.0, 0.0]},
            "textures": [NO_TEXTURE], "blend_mode": "Normal", "tint": [1, 1, 1], "screenTint": [0, 0, 0],
            "emissionStrength": 1, "opacity": 1,
        })
    }

    fn session_with(part: Value) -> Session {
        let model = json!({
            "meta": {"name": "fixture", "version": "1.0-alpha"},
            "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
                "transform": transform(), "lockToRoot": false, "pinToMesh": false, "children": [part]},
            "param": [{
                "uuid": 17, "name": "Blink", "is_vec2": true, "min": [-1.0, -1.0], "max": [1.0, 1.0],
                "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0, 1.0]], "merge_mode": "Additive",
                "bindings": [],
            }],
            "animations": {},
        });
        let mut session = Session::new();
        let reply = session.handle_request(&json!({"op": "new", "model": model}));
        assert_eq!(reply["succeeded"], true, "{reply}");
        session
    }

    /// Four vertices that are not a grid: (0,0), (100,0), (50,80), (0,100); two triangles.
    fn kite() -> Session {
        session_with(part(json!([0.0, 0.0, 100.0, 0.0, 50.0, 80.0, 0.0, 100.0]), json!([0, 1, 2, 0, 2, 3])))
    }

    /// A 2 x 2 grid.
    fn square() -> Session {
        session_with(part(json!([0.0, 0.0, 100.0, 0.0, 100.0, 100.0, 0.0, 100.0]), json!([0, 1, 2, 0, 2, 3])))
    }

    const KEY: [usize; 2] = [1, 1];

    fn sync(tools: &mut MeshTools, session: &Session, armed: Option<(u32, [usize; 2])>) {
        let snapshot = session.snapshot();
        let ctx = Ctx {
            revision: snapshot["revision"].as_u64().unwrap(),
            model: &snapshot["model"],
            selected: Some(5),
            armed,
            frame: Some(Frame::IDENTITY),
            zoom: 1.0,
        };
        tools.sync(&ctx);
    }

    fn vertex_tools(session: &Session) -> MeshTools {
        let mut tools = MeshTools::new();
        sync(&mut tools, session, None);
        assert_eq!(tools.edit_mesh_target(), Some(5));
        tools.start_edit_mesh(5);
        sync(&mut tools, session, None);
        assert_eq!(tools.mode(), Some(Mode::Vertex));
        tools
    }

    fn vertex_framed(session: &Session, frame: Frame) -> MeshTools {
        let mut tools = MeshTools::new();
        let apply = |tools: &mut MeshTools| {
            let snapshot = session.snapshot();
            tools.sync(&Ctx {
                revision: snapshot["revision"].as_u64().unwrap(),
                model: &snapshot["model"],
                selected: Some(5),
                armed: None,
                frame: Some(frame),
                zoom: 1.0,
            });
        };
        apply(&mut tools);
        assert_eq!(tools.edit_mesh_target(), Some(5));
        tools.start_edit_mesh(5);
        apply(&mut tools);
        assert_eq!(tools.mode(), Some(Mode::Vertex));
        tools
    }

    fn deform_tools(session: &Session) -> MeshTools {
        let mut tools = MeshTools::new();
        sync(&mut tools, session, Some((17, KEY)));
        assert_eq!(tools.mode(), Some(Mode::Deform));
        tools
    }

    fn press(tools: &mut MeshTools, at: V2, mods: Mods) -> Vec<Value> {
        tools.event(Pointer::Press { at, mods })
    }

    fn drag_to(tools: &mut MeshTools, at: V2) -> Vec<Value> {
        tools.event(Pointer::Move { at, mods: NONE })
    }

    fn release(tools: &mut MeshTools, at: V2) -> Vec<Value> {
        tools.event(Pointer::Release { at, mods: NONE })
    }

    fn click(tools: &mut MeshTools, at: V2, mods: Mods) {
        press(tools, at, mods);
        tools.event(Pointer::Release { at, mods });
    }

    fn send(session: &mut Session, requests: &[Value]) {
        for request in requests {
            let reply = session.handle_request(request);
            assert_eq!(reply["succeeded"], true, "{request} -> {reply}");
        }
    }

    fn part_verts(session: &Session) -> Vec<f64> {
        let model = session.snapshot()["model"].clone();
        let node = find_node(&model["nodes"], 5).unwrap().clone();
        node["mesh"]["verts"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect()
    }

    fn assert_close(a: &[V2], b: &[V2]) {
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b) {
            assert!((x[0] - y[0]).abs() < 1e-4 && (x[1] - y[1]).abs() < 1e-4, "{a:?} != {b:?}");
        }
    }

    fn flat(points: &[V2]) -> Vec<f64> {
        points.iter().flat_map(|p| [p[0], p[1]]).collect()
    }

    // ---------------------------------------------------------------- vertex tools: gesture to edit

    #[test]
    fn ctrl_click_on_empty_space_adds_a_vertex_and_selects_it() {
        let mut tools = vertex_tools(&kite());
        click(&mut tools, [30.0, 30.0], CTRL);
        assert_eq!(tools.pending_edits(), [json!({"op": "add", "pos": [30.0, 30.0]})]);
        assert_eq!(tools.selection(), [4]);
        assert_eq!(tools.points()[4], [30.0, 30.0]);
    }

    #[test]
    fn ctrl_click_on_a_vertex_removes_it_and_double_click_removes_a_selected_one() {
        let mut tools = vertex_tools(&kite());
        click(&mut tools, [100.0, 0.0], CTRL);
        assert_eq!(tools.pending_edits(), [json!({"op": "remove", "at": [100.0, 0.0]})]);
        assert_eq!(tools.points().len(), 3);
        assert_eq!(tools.selection(), Vec::<usize>::new());

        // A double click first selects the vertex (first click), then removes it.
        let mut tools = vertex_tools(&kite());
        click(&mut tools, [50.0, 80.0], NONE);
        assert_eq!(tools.selection(), [2]);
        tools.event(Pointer::DoubleClick { at: [50.0, 80.0], mods: NONE });
        assert_eq!(tools.pending_edits(), [json!({"op": "remove", "at": [50.0, 80.0]})]);
        // On empty space a double click adds, selecting the first appended vertex.
        tools.event(Pointer::DoubleClick { at: [200.0, 200.0], mods: NONE });
        assert_eq!(tools.pending_edits()[1], json!({"op": "add", "pos": [200.0, 200.0]}));
        assert_eq!(tools.selection(), [3]);
    }

    #[test]
    fn dragging_a_selected_vertex_is_one_move_edit_from_where_the_drag_started() {
        let mut tools = vertex_tools(&kite());
        press(&mut tools, [100.0, 0.0], NONE);
        assert_eq!(tools.selection(), [1]);
        // Below the 4 px drag threshold nothing moves yet.
        drag_to(&mut tools, [102.0, 0.0]);
        assert!(tools.pending_edits().is_empty());
        drag_to(&mut tools, [110.0, 0.0]);
        // The drag starts at 110, so the 10 units travelled before it are not part of it.
        drag_to(&mut tools, [130.0, 20.0]);
        assert!(tools.pending_edits().is_empty(), "nothing is committed while the button is down");
        assert_eq!(tools.preview().0[1], [120.0, 20.0], "the preview shows the move");
        release(&mut tools, [130.0, 20.0]);
        assert_eq!(tools.pending_edits(), [json!({"op": "move", "indices": [1], "delta": [20.0, 20.0]})]);
        assert_eq!(tools.points()[1], [120.0, 20.0]);
    }

    #[test]
    fn a_point_adheres_to_the_nearest_guide_of_each_axis_within_the_distance() {
        use GuideAxis::{Horizontal, Vertical};
        let guides = [(Vertical, 10.0), (Horizontal, 20.0), (Vertical, 13.0), (Horizontal, 50.0)];
        // 8 points at zoom 2 is 4 world units: x 12 is 2 from 10 and 1 from 13, y 24 is 4 from 20.
        assert_eq!(adhere([12.0, 24.0], &guides, 2.0, 8.0), ([13.0, 20.0], vec![1, 2]));
        // Just past 4 units, and the guides of the other axis do not count.
        assert_eq!(adhere([17.5, 24.5], &guides, 2.0, 8.0), ([17.5, 24.5], vec![]));
        assert_eq!(adhere([5.0, 54.0], &guides, 1.0, 8.0), ([10.0, 50.0], vec![3, 0]));
        assert_eq!(adhere([5.0, 54.0], &[], 1.0, 8.0), ([5.0, 54.0], vec![]));
    }

    #[test]
    fn a_dragged_vertex_adheres_to_a_guide_and_shows_it_attached_until_release() {
        let mut tools = vertex_tools(&kite());
        tools.guides = vec![(GuideAxis::Horizontal, 70.0), (GuideAxis::Vertical, 125.0)];
        press(&mut tools, [100.0, 0.0], NONE);
        drag_to(&mut tools, [110.0, 0.0]);
        // The vertex would be at [120, 20]: 5 from the vertical guide, 50 from the horizontal one.
        drag_to(&mut tools, [130.0, 20.0]);
        assert_eq!(tools.preview().0[1], [125.0, 20.0]);
        assert_eq!(tools.guides_attached(), [1]);
        release(&mut tools, [130.0, 20.0]);
        assert_eq!(tools.pending_edits(), [json!({"op": "move", "indices": [1], "delta": [25.0, 20.0]})]);
        assert!(tools.guides_attached().is_empty());
        // The next drag starts where the unapplied move left the vertex: 3 units off the guide, so it adheres.
        press(&mut tools, [125.0, 20.0], NONE);
        drag_to(&mut tools, [135.0, 20.0]);
        drag_to(&mut tools, [138.0, 44.0]);
        assert_eq!(tools.preview().0[1], [125.0, 44.0]);
        assert_eq!(tools.guides_attached(), [1]);
        release(&mut tools, [138.0, 44.0]);
        assert_eq!(tools.points()[1], [125.0, 44.0]);
        assert_eq!(tools.selected_world_points(), [[125.0, 44.0]], "guides go through the vertex where it is now");
        // Away from every guide the vertex follows the pointer, and nothing is attached from its first frame.
        press(&mut tools, [0.0, 0.0], NONE);
        drag_to(&mut tools, [10.0, 0.0]);
        assert!(tools.guides_attached().is_empty());
        drag_to(&mut tools, [20.0, 30.0]);
        assert_eq!(tools.preview().0[0], [10.0, 30.0]);
        assert!(tools.guides_attached().is_empty());
    }

    #[test]
    fn a_vertex_a_shift_press_deselects_does_not_adhere_for_the_ones_that_move() {
        let mut tools = vertex_tools(&kite());
        tools.guides = vec![(GuideAxis::Vertical, 125.0)];
        click(&mut tools, [100.0, 0.0], NONE);
        click(&mut tools, [0.0, 0.0], SHIFT);
        assert_eq!(tools.selection(), [1, 0]);
        // The Shift press takes vertex 1 out of the selection, and the drag moves vertex 0 only.
        press(&mut tools, [100.0, 0.0], SHIFT);
        drag_to(&mut tools, [110.0, 0.0]);
        drag_to(&mut tools, [130.0, 20.0]);
        assert_eq!(tools.selection(), [0]);
        assert!(tools.guides_attached().is_empty());
        assert_eq!(tools.preview().0[0], [20.0, 20.0]);
    }

    #[test]
    fn box_select_replaces_or_adds_with_shift_and_removes_with_shift_ctrl() {
        let mut tools = vertex_tools(&kite());
        press(&mut tools, [-30.0, -30.0], NONE);
        drag_to(&mut tools, [60.0, 50.0]);
        release(&mut tools, [60.0, 50.0]);
        assert_eq!(tools.selection(), [0]);
        press(&mut tools, [115.0, -30.0], SHIFT);
        drag_to(&mut tools, [90.0, 10.0]);
        tools.event(Pointer::Release { at: [90.0, 10.0], mods: SHIFT });
        assert_eq!(tools.selection(), [0, 1]);
        let both = Mods { ctrl: true, shift: true, alt: false };
        press(&mut tools, [-30.0, -30.0], both);
        drag_to(&mut tools, [10.0, 10.0]);
        tools.event(Pointer::Release { at: [10.0, 10.0], mods: both });
        assert_eq!(tools.selection(), [1]);
        // A click on empty space with an empty box replaces the selection by nothing.
        click(&mut tools, [300.0, 300.0], NONE);
        assert_eq!(tools.selection(), Vec::<usize>::new());
        assert!(tools.pending_edits().is_empty(), "selection never edits the mesh");
    }

    #[test]
    fn delete_and_arrow_keys_edit_the_selection() {
        let mut tools = vertex_tools(&kite());
        click(&mut tools, [0.0, 100.0], NONE);
        tools.event(Pointer::Key { key: Key::Right, mods: NONE });
        tools.event(Pointer::Key { key: Key::Up, mods: SHIFT });
        tools.event(Pointer::Key { key: Key::Left, mods: Mods { alt: true, ..NONE } });
        assert_eq!(
            tools.pending_edits(),
            [
                json!({"op": "move", "indices": [3], "delta": [10.0, 0.0]}),
                json!({"op": "move", "indices": [3], "delta": [0.0, -100.0]}),
                json!({"op": "move", "indices": [3], "delta": [-1.0, 0.0]}),
            ]
        );
        tools.event(Pointer::Key { key: Key::Delete, mods: NONE });
        assert_eq!(tools.pending_edits()[3], json!({"op": "remove", "indices": [3]}));
        assert_eq!(tools.points().len(), 3);
    }

    #[test]
    fn the_edge_tool_toggles_an_edge_between_two_clicked_vertices() {
        let mut tools = vertex_tools(&kite());
        tools.set_tool(Tool::Connect);
        click(&mut tools, [0.0, 0.0], NONE);
        assert_eq!(tools.selection(), [0]);
        click(&mut tools, [50.0, 80.0], NONE);
        assert_eq!(tools.pending_edits(), [json!({"op": "toggle", "a": 0, "b": 2})]);
        assert_eq!(tools.selection(), Vec::<usize>::new());
        // Shift keeps the second vertex as the next anchor ("Connect Multiple").
        click(&mut tools, [100.0, 0.0], NONE);
        click(&mut tools, [0.0, 100.0], SHIFT);
        assert_eq!(tools.selection(), [3]);
        // Clicking the same vertex twice unselects it; empty space clears.
        click(&mut tools, [0.0, 100.0], NONE);
        assert_eq!(tools.selection(), Vec::<usize>::new());
    }

    #[test]
    fn the_edge_cutter_sends_the_dragged_line() {
        let mut tools = vertex_tools(&kite());
        tools.set_tool(Tool::EdgeCutter);
        press(&mut tools, [25.0, -20.0], NONE);
        drag_to(&mut tools, [25.0, 40.0]);
        release(&mut tools, [25.0, 120.0]);
        assert_eq!(tools.pending_edits(), [json!({"op": "cut", "from": [25.0, -20.0], "to": [25.0, 120.0]})]);
        // The edge 0-2 and the edge 2-3 are gone from the preview.
        let (_, edges) = tools.preview();
        assert!(!edges.contains(&[0, 2]) && !edges.contains(&[2, 3]), "{edges:?}");
    }

    #[test]
    fn the_poly_lasso_selects_on_the_closing_click_and_right_click_drops_a_point() {
        let mut tools = vertex_tools(&kite());
        tools.set_tool(Tool::Lasso);
        tools.settings.poly_lasso = true;
        for p in [[-10.0, -10.0], [120.0, -10.0], [120.0, 60.0], [-10.0, 60.0]] {
            click(&mut tools, p, NONE);
        }
        assert!(tools.selection().is_empty());
        tools.event(Pointer::Secondary { at: [0.0, 0.0], mods: NONE });
        click(&mut tools, [-10.0, 60.0], NONE);
        click(&mut tools, [-8.0, -8.0], NONE);
        assert_eq!(tools.selection(), [0, 1]);
        // Escape clears a half-drawn outline.
        click(&mut tools, [0.0, 0.0], NONE);
        tools.event(Pointer::Key { key: Key::Escape, mods: NONE });
        assert!(tools.lasso.is_empty());
    }

    #[test]
    fn the_regular_lasso_follows_the_drag_and_selects_on_release() {
        let mut tools = vertex_tools(&kite());
        tools.set_tool(Tool::Lasso);
        press(&mut tools, [-10.0, -10.0], NONE);
        for p in [[120.0, -10.0], [120.0, 60.0], [-10.0, 60.0]] {
            drag_to(&mut tools, p);
        }
        release(&mut tools, [-10.0, 60.0]);
        // The last point is replaced by the first, so the outline is the triangle (-10,-10), (120,-10),
        // (120,60): vertex 1 only.
        assert_eq!(tools.selection(), [1]);
    }

    #[test]
    fn the_grid_tool_creates_moves_and_edits_axes() {
        let mut tools = vertex_tools(&kite());
        tools.set_tool(Tool::Grid);
        press(&mut tools, [300.0, 300.0], NONE);
        drag_to(&mut tools, [310.0, 310.0]);
        drag_to(&mut tools, [400.0, 380.0]);
        release(&mut tools, [400.0, 380.0]);
        assert_eq!(
            tools.pending_edits(),
            [json!({"op": "grid_create", "bounds": [310.0, 310.0, 400.0, 380.0], "cuts": 3})]
        );
        assert_eq!(tools.points().len(), 9);
        // Drag the centre vertex of the new grid, then add and remove axis lines by double click.
        press(&mut tools, [355.0, 345.0], NONE);
        drag_to(&mut tools, [365.0, 345.0]);
        drag_to(&mut tools, [370.0, 350.0]);
        release(&mut tools, [370.0, 350.0]);
        assert_eq!(tools.pending_edits()[1], json!({"op": "grid_move", "vertex": 4, "to": [370.0, 350.0]}));
        tools.event(Pointer::DoubleClick { at: [340.0, 380.0], mods: NONE });
        assert_eq!(tools.pending_edits()[2], json!({"op": "grid_add_axis", "pos": [340.0, 380.0]}));
        assert_eq!(tools.points().len(), 12, "380 is a line already, so only an x line was inserted");
        tools.event(Pointer::DoubleClick { at: [370.0, 350.0], mods: NONE });
        assert_eq!(tools.pending_edits().last().unwrap()["op"], "grid_remove_axis");
    }

    #[test]
    fn a_triangulation_that_merges_points_clears_the_selection() {
        let session = kite();
        let mut tools = vertex_tools(&session);
        let first = tools.points()[0];
        tools.push_edit(json!({"op": "add", "pos": first}), After::Last);
        for pos in [[200.0, 200.0], [210.0, 230.0]] {
            tools.push_edit(json!({"op": "add", "pos": pos}), After::Last);
        }
        let count = tools.points().len();
        // The selected point comes after the coincident one, so the merge renumbers it.
        let selected = count - 2;
        tools.selected = vec![selected];
        tools.flip(false);
        assert_eq!(tools.selected, [selected], "an edit that keeps the count keeps the selection");
        tools.triangulate();
        assert_eq!(tools.points().len(), count - 1, "the coincident point was merged");
        assert!(tools.selected.is_empty(), "index {selected} now names another point");
    }

    // ------------------------------------------------------- pending list = session after Apply

    #[test]
    fn the_preview_of_the_pending_edits_is_what_the_session_makes_of_the_one_apply_request() {
        let mut session = kite();
        let mut tools = vertex_tools(&session);
        click(&mut tools, [30.0, 30.0], CTRL);
        click(&mut tools, [70.0, 20.0], CTRL);
        press(&mut tools, [100.0, 0.0], NONE);
        drag_to(&mut tools, [110.0, 0.0]);
        release(&mut tools, [120.0, 10.0]);
        tools.triangulate();
        assert_eq!(tools.pending_edits().len(), 4);
        let (preview, _) = tools.preview();
        assert_eq!(preview.len(), 6);
        let before = session.snapshot()["revision"].as_u64().unwrap();
        assert_eq!(part_verts(&session).len(), 8, "nothing reaches the document before Apply");

        let request = tools.apply().expect("edits are pending");
        assert_eq!(request["op"], "mesh.edit");
        assert_eq!(request["edits"].as_array().unwrap().len(), 4);
        send(&mut session, &[request]);
        assert_eq!(session.snapshot()["revision"].as_u64().unwrap(), before + 1, "one undo step");
        assert_eq!(part_verts(&session), flat(&preview));
        assert!(!tools.captures() && tools.mode().is_none(), "Apply leaves the mode");
        let undo = session.handle_request(&json!({"op": "undo"}));
        assert_eq!(undo["succeeded"], true);
        assert_eq!(part_verts(&session).len(), 8);
    }

    #[test]
    fn mirror_and_auto_connect_travel_with_the_apply_request_and_lock_while_edits_are_pending() {
        let mut session = kite();
        let mut tools = vertex_tools(&session);
        tools.settings.mirror_horizontal = true;
        tools.settings.auto_connect = true;
        click(&mut tools, [30.0, 30.0], CTRL);
        click(&mut tools, [60.0, 30.0], CTRL);
        assert!(tools.options_locked());
        // Changing the toggles now must not change what the preview shows.
        let shown = tools.points();
        tools.settings.mirror_horizontal = false;
        tools.rebuild();
        assert_eq!(tools.points(), shown);
        let request = tools.apply().unwrap();
        assert_eq!(request["mirror"]["horizontal"], true);
        assert_eq!(request["autoConnect"], true);
        send(&mut session, &[request]);
        assert_eq!(part_verts(&session), flat(&shown));
    }

    #[test]
    fn cancel_drops_the_pending_edits_and_leaves_the_session_alone() {
        let session = kite();
        let before = session.snapshot();
        let mut tools = vertex_tools(&session);
        click(&mut tools, [30.0, 30.0], CTRL);
        tools.flip(false);
        assert_eq!(tools.pending_edits().len(), 2);
        tools.cancel();
        assert!(tools.pending_edits().is_empty() && tools.mode().is_none());
        assert_eq!(tools.apply(), None, "nothing left to apply");
        assert_eq!(session.snapshot(), before);
        // Apply without edits just leaves the mode and sends nothing.
        let mut tools = vertex_tools(&session);
        assert_eq!(tools.apply(), None);
        assert!(tools.mode().is_none());
    }

    #[test]
    fn undo_and_redo_edit_walk_the_pending_list() {
        let mut tools = vertex_tools(&kite());
        click(&mut tools, [30.0, 30.0], CTRL);
        click(&mut tools, [60.0, 30.0], CTRL);
        assert_eq!(tools.points().len(), 6);
        tools.undo_edit();
        assert_eq!(tools.points().len(), 5);
        assert!(tools.can_redo_edit());
        tools.redo_edit();
        assert_eq!(tools.points().len(), 6);
        assert_eq!(tools.pending_edits().len(), 2);
    }

    #[test]
    fn an_edit_the_session_would_refuse_is_dropped_with_a_notice() {
        let mut tools = vertex_tools(&square());
        // A square is a grid; double click on empty space with the point tool adds a vertex, which is fine,
        // but an axis edit on a non-grid mesh is refused.
        click(&mut tools, [30.0, 30.0], CTRL);
        assert_eq!(tools.points().len(), 5);
        tools.set_tool(Tool::Grid);
        tools.push_edit(json!({"op": "grid_remove_axis", "vertex": 0}), After::Clear);
        assert_eq!(tools.pending_edits().len(), 1);
        assert!(tools.notice.contains("not a grid"), "{}", tools.notice);
    }

    #[test]
    fn a_document_change_under_the_tools_drops_the_pending_edits() {
        let mut session = kite();
        let mut tools = vertex_tools(&session);
        click(&mut tools, [30.0, 30.0], CTRL);
        send(&mut session, &[json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "flip", "axis": "h"}]})]);
        sync(&mut tools, &session, None);
        assert!(tools.pending_edits().is_empty());
        assert_eq!(tools.points().len(), 4);
    }

    // --------------------------------------------------------------------------------- deform mode

    fn offsets(session: &Session) -> Value {
        session.snapshot()["model"]["param"][0]["bindings"][0]["values"][KEY[0]][KEY[1]].clone()
    }

    #[test]
    fn a_point_drag_in_deform_mode_is_one_translate_request_that_matches_the_preview() {
        let mut session = kite();
        let mut tools = deform_tools(&session);
        press(&mut tools, [50.0, 80.0], NONE);
        assert_eq!(tools.selection(), [2]);
        drag_to(&mut tools, [60.0, 80.0]);
        assert!(drag_to(&mut tools, [80.0, 100.0]).is_empty());
        let preview = tools.preview().0;
        let requests = release(&mut tools, [80.0, 100.0]);
        assert_eq!(
            requests,
            [json!({
                "op": "deform.edit", "id": 5, "parameter": 17, "key": [1, 1],
                "mirror": {"horizontal": false, "vertical": false, "origin": [0.0, 0.0]}, "zoom": 1.0,
                "tool": "translate", "selected": [2], "delta": [20.0, 20.0],
            })]
        );
        send(&mut session, &requests);
        assert_eq!(offsets(&session), json!([[0.0, 0.0], [0.0, 0.0], [20.0, 20.0], [0.0, 0.0]]));
        sync(&mut tools, &session, Some((17, KEY)));
        assert_close(&tools.points(), &preview);
        // Arrow keys nudge from there.
        let requests = tools.event(Pointer::Key { key: Key::Down, mods: NONE });
        send(&mut session, &requests);
        assert_eq!(offsets(&session)[2], json!([20.0, 30.0]));
    }

    #[test]
    fn a_brush_stroke_sends_its_mouse_positions_and_the_settings() {
        let mut session = kite();
        let mut tools = deform_tools(&session);
        tools.set_tool(Tool::Brush);
        tools.settings.brush = BrushKind::Circle;
        tools.settings.radius = 300.0;
        press(&mut tools, [0.0, 0.0], NONE);
        drag_to(&mut tools, [10.0, 0.0]);
        drag_to(&mut tools, [20.0, 0.0]);
        let preview = tools.preview().0;
        let requests = release(&mut tools, [30.0, 0.0]);
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request["tool"], "brush");
        assert_eq!(request["stroke"], json!([[10.0, 0.0], [20.0, 0.0], [30.0, 0.0]]));
        assert_eq!(request["brush"], json!({"kind": "circle", "radius": 300.0}));
        assert_eq!(request["flow"], false);
        assert_eq!(request["selected"], json!([]));
        assert_ne!(tools.preview().0, preview, "the release extends the stroke by its last position");
        send(&mut session, &requests);
        sync(&mut tools, &session, Some((17, KEY)));
        let stroke_end = tools.points();
        // The vertex at the stroke start moved the full 20 units; the far ones less.
        // Weights come from the stroke start (10, 0): 1 - 10 / 300 for vertex 0, 20 units of travel.
        assert!((stroke_end[0][0] - 20.0 * (1.0 - 10.0 / 300.0)).abs() < 1e-4, "{stroke_end:?}");
        assert!(stroke_end[1][0] > 100.0 && stroke_end[1][0] < 120.0, "{stroke_end:?}");
        assert!(preview[0][0] > 0.0 && preview[0][0] < stroke_end[0][0], "the preview stops at the last move");
    }

    #[test]
    fn brush_kinds_and_flow_reach_the_request() {
        let session = kite();
        let mut tools = deform_tools(&session);
        tools.set_tool(Tool::Brush);
        tools.settings.brush = BrushKind::DoubleThreshold;
        tools.settings.radius = 80.0;
        tools.settings.inner_radius = 20.0;
        tools.settings.brush_mode = BrushMode::Flow;
        press(&mut tools, [0.0, 0.0], NONE);
        drag_to(&mut tools, [10.0, 0.0]);
        let requests = release(&mut tools, [12.0, 0.0]);
        assert_eq!(requests[0]["brush"], json!({"kind": "double_threshold", "radius": 80.0, "innerRadius": 20.0}));
        assert_eq!(requests[0]["flow"], true);
        tools.settings.brush = BrushKind::Rectangle;
        tools.settings.width = 50.0;
        tools.settings.height = 60.0;
        tools.selected = vec![0, 1];
        press(&mut tools, [0.0, 0.0], Mods { shift: true, ..NONE });
        tools.event(Pointer::Release { at: [0.0, 0.0], mods: SHIFT });
        press(&mut tools, [0.0, 0.0], NONE);
        drag_to(&mut tools, [10.0, 0.0]);
        let requests = release(&mut tools, [12.0, 0.0]);
        assert_eq!(requests[0]["brush"], json!({"kind": "rectangle", "width": 50.0, "height": 60.0}));
    }

    #[test]
    fn reset_selected_puts_the_selection_back_on_its_rest_positions() {
        let mut session = kite();
        let mut tools = deform_tools(&session);
        click(&mut tools, [50.0, 80.0], NONE);
        press(&mut tools, [50.0, 80.0], NONE);
        drag_to(&mut tools, [60.0, 80.0]);
        let requests = release(&mut tools, [70.0, 80.0]);
        send(&mut session, &requests);
        sync(&mut tools, &session, Some((17, KEY)));
        assert_eq!(tools.points()[2], [60.0, 80.0]);
        let reset = tools.reset_selection().expect("a vertex is selected");
        assert_eq!(reset["tool"], "reset");
        send(&mut session, &[reset]);
        sync(&mut tools, &session, Some((17, KEY)));
        assert_eq!(tools.points()[2], [50.0, 80.0]);
    }

    #[test]
    fn line_brush_preview_matches_the_committed_drag_and_flow() {
        for flow in [false, true] {
            let mut session = kite();
            let mut tools = deform_tools(&session);
            tools.set_tool(Tool::Brush);
            tools.settings.brush = BrushKind::Line;
            tools.settings.radius = 100.0;
            tools.settings.line_angle_degrees = 45.0;
            tools.settings.brush_mode = if flow { BrushMode::Flow } else { BrushMode::Drag };
            press(&mut tools, [0.0, 0.0], NONE);
            drag_to(&mut tools, [10.0, 0.0]);
            drag_to(&mut tools, [20.0, 10.0]);
            let preview = tools.preview().0;
            let requests = release(&mut tools, [20.0, 10.0]);
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0]["brush"], json!({"kind":"line","radius":100.0,"angle":std::f64::consts::FRAC_PI_4}));
            assert_eq!(requests[0]["flow"], flow);
            send(&mut session, &requests);
            sync(&mut tools, &session, Some((17, KEY)));
            for (actual, expected) in tools.points().iter().zip(&preview) {
                assert!((actual[0] - expected[0]).abs() < 1e-4);
                assert!((actual[1] - expected[1]).abs() < 1e-4);
            }
            assert_ne!(preview[0], [0.0, 0.0]);
        }
    }

    #[test]
    fn part_origin_aligns_deform_tools_with_rendered_vertices() {
        let mut item = part(json!([13, 24, 63, 24, 13, 74]), json!([0, 1, 2]));
        item["mesh"]["origin"] = json!([10, 20]);
        let part_session = session_with(item);
        let mut group = group_session(json!([13, 24, 63, 24, 13, 74]), json!([0, 0, 1, 0, 0, 1]), json!([0, 1, 2]));
        let mut model = group.snapshot()["model"].clone();
        model["nodes"]["children"][0]["mesh"]["origin"] = json!([10, 20]);
        assert_eq!(group.handle_request(&json!({"op": "new", "model": model}))["succeeded"], true);
        let world = [[2.0, 0.0, 0.0, 100.0], [0.0, 3.0, 0.0, 200.0],
            [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
        for (mut session, expected) in [(part_session, [106.0, 212.0]), (group, [126.0, 272.0])] {
            let mut tools = deform_tools(&session);
            let snapshot = session.snapshot();
            tools.sync(&Ctx {
                revision: snapshot["revision"].as_u64().unwrap(),
                model: &snapshot["model"],
                selected: Some(5),
                armed: Some((17, KEY)),
                frame: Frame::from_world_matrix(&world),
                zoom: 1.0,
            });
            let frame = tools.active.as_ref().unwrap().frame;
            assert_eq!(frame.to_world([13.0, 24.0]), expected);
            assert_eq!(frame.to_local(expected), [13.0, 24.0]);
            click(&mut tools, expected, NONE);
            assert_eq!(tools.selection(), [0]);
            assert_eq!(tools.selected_world_points(), [expected]);
            tools.set_tool(Tool::Brush);
            tools.settings.radius = 100.0;
            press(&mut tools, [expected[0] - 20.0, expected[1]], NONE);
            drag_to(&mut tools, expected);
            drag_to(&mut tools, [expected[0] + 20.0, expected[1]]);
            let preview = tools.preview().0;
            let requests = release(&mut tools, [expected[0] + 20.0, expected[1]]);
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0]["stroke"][0], json!([13.0, 24.0]));
            assert_eq!(preview[0], [23.0, 24.0]);
            send(&mut session, &requests);
            sync(&mut tools, &session, Some((17, KEY)));
            assert_close(&tools.points(), &preview);
            assert_eq!(session.handle_request(&json!({"op": "undo"}))["succeeded"], true);
        }
    }

    #[test]
    fn part_origin_aligns_vertex_and_grid_tools_with_rendered_vertices() {
        let world = [[2.0, 0.0, 0.0, 100.0], [0.0, 3.0, 0.0, 200.0],
            [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
        let raw = Frame::from_world_matrix(&world).unwrap();
        let rendered = |p: V2| [100.0 + 2.0 * (p[0] - 10.0), 200.0 + 3.0 * (p[1] - 20.0)];
        let plain = |p: V2| [100.0 + 2.0 * p[0], 200.0 + 3.0 * p[1]];
        let mut item = part(json!([13.0, 24.0, 63.0, 24.0, 13.0, 74.0]), json!([0, 1, 2]));
        item["mesh"]["origin"] = json!([10.0, 20.0]);
        let mut session = session_with(item);
        let mut tools = vertex_framed(&session, raw);
        let frame = tools.active.as_ref().unwrap().frame;
        assert_eq!(frame.to_world([13.0, 24.0]), rendered([13.0, 24.0]));
        assert_eq!(frame.to_local(rendered([13.0, 24.0])), [13.0, 24.0]);
        click(&mut tools, plain([13.0, 24.0]), NONE);
        assert!(tools.selection().is_empty(), "an unshifted world point is not the rendered Part vertex");
        click(&mut tools, rendered([13.0, 24.0]), NONE);
        assert_eq!(tools.selection(), [0]);

        tools.set_tool(Tool::Grid);
        press(&mut tools, rendered([20.0, 30.0]), NONE);
        drag_to(&mut tools, rendered([30.0, 40.0]));
        drag_to(&mut tools, rendered([90.0, 80.0]));
        let (creating, _) = tools.preview();
        assert!(creating.iter().any(|p| *p == [30.0, 40.0]), "preview stores raw local corners: {creating:?}");
        release(&mut tools, rendered([90.0, 80.0]));
        assert_eq!(
            tools.pending_edits()[0],
            json!({"op": "grid_create", "bounds": [30.0, 40.0, 90.0, 80.0], "cuts": 3})
        );
        press(&mut tools, rendered([60.0, 60.0]), NONE);
        drag_to(&mut tools, rendered([75.0, 65.0]));
        release(&mut tools, rendered([75.0, 65.0]));
        assert_eq!(tools.pending_edits()[1], json!({"op": "grid_move", "vertex": 4, "to": [75.0, 65.0]}));
        doubled(&mut tools, rendered([45.0, 55.0]));
        assert_eq!(tools.pending_edits()[2], json!({"op": "grid_add_axis", "pos": [45.0, 55.0]}));
        let victim = tools.points()[0];
        doubled(&mut tools, rendered(victim));
        assert_eq!(tools.pending_edits().last().unwrap()["op"], "grid_remove_axis");
        let (preview, _) = tools.preview();
        for point in &preview {
            assert_eq!(frame.to_world(*point), rendered(*point));
        }
        tools.undo_edit();
        assert_ne!(tools.pending_edits().last().unwrap()["op"], "grid_remove_axis");
        tools.redo_edit();
        assert_eq!(tools.pending_edits().last().unwrap()["op"], "grid_remove_axis");

        let shown = tools.points();
        let request = tools.apply().expect("grid edits apply as one mesh edit");
        assert_eq!(request["op"], "mesh.edit");
        assert_eq!(request["edits"][0]["bounds"], json!([30.0, 40.0, 90.0, 80.0]));
        send(&mut session, &[request]);
        assert_eq!(part_verts(&session), flat(&shown));
        assert_eq!(node_of(&session)["mesh"]["origin"], json!([0.0, 0.0]), "grid regeneration builds a fresh mesh");
        assert_eq!(session.handle_request(&json!({"op": "undo"}))["succeeded"], true);
        assert_eq!(part_verts(&session), vec![13.0, 24.0, 63.0, 24.0, 13.0, 74.0]);
        assert_eq!(node_of(&session)["mesh"]["origin"], json!([10.0, 20.0]));

        let mut group = group_session(
            json!([13.0, 24.0, 63.0, 24.0, 13.0, 74.0]),
            json!([0.0, 0.0, 1.0, 0.0, 0.0, 1.0]),
            json!([0, 1, 2]),
        );
        let mut model = group.snapshot()["model"].clone();
        model["nodes"]["children"][0]["mesh"]["origin"] = json!([10.0, 20.0]);
        assert_eq!(group.handle_request(&json!({"op": "new", "model": model}))["succeeded"], true);
        let path = path_session(json!([13.0, 24.0, 63.0, 24.0]), "Spline");
        let grid = grid_session(json!([13.0, 63.0]), json!([24.0, 74.0]));
        for (session, kind) in [(&group, "MeshGroup"), (&path, "Path"), (&grid, "GridDeformer")] {
            let tools = vertex_framed(session, raw);
            assert_eq!(tools.active.as_ref().unwrap().frame.to_world([13.0, 24.0]), plain([13.0, 24.0]), "{kind}");
        }
        let mut tools = vertex_framed(&path, raw);
        click(&mut tools, plain([13.0, 24.0]), NONE);
        assert_eq!(tools.selection(), [0]);
        click(&mut tools, rendered([13.0, 24.0]), NONE);
        assert!(tools.selection().is_empty(), "a Path ignores the Part origin shift");
        let mut tools = vertex_framed(&group, raw);
        click(&mut tools, plain([13.0, 24.0]), NONE);
        assert_eq!(tools.selection(), [0]);
        click(&mut tools, rendered([13.0, 24.0]), NONE);
        assert!(tools.selection().is_empty(), "a MeshGroup ignores the Part origin shift");
        let mut tools = vertex_framed(&grid, raw);
        assert_eq!(tools.points()[0], [13.0, 24.0]);
        press(&mut tools, plain([13.0, 24.0]), NONE);
        drag_to(&mut tools, plain([23.0, 24.0]));
        release(&mut tools, plain([23.0, 24.0]));
        assert_eq!(tools.pending_edits()[0]["to"], json!([23.0, 24.0]));
    }

    #[test]
    fn a_source_origin_change_cancels_a_held_deform_brush() {
        let mut session = kite();
        let mut tools = deform_tools(&session);
        tools.set_tool(Tool::Brush);
        tools.down = true;
        press(&mut tools, [0.0, 0.0], NONE);
        drag_to(&mut tools, [10.0, 0.0]);
        drag_to(&mut tools, [20.0, 0.0]);
        assert!(matches!(tools.gesture, Gesture::Brush { .. }));
        assert_ne!(tools.preview().0, tools.points());
        let mut model = session.snapshot()["model"].clone();
        model["nodes"]["children"][0]["mesh"]["origin"] = json!([30, 40]);
        assert_eq!(session.handle_request(&json!({"op": "new", "model": model}))["succeeded"], true);
        sync(&mut tools, &session, Some((17, KEY)));
        assert!(!tools.down);
        assert!(tools.cursor.is_none());
        assert!(matches!(tools.gesture, Gesture::Idle));
        assert_eq!(tools.preview().0, tools.points());
        assert!(drag_to(&mut tools, [20.0, 0.0]).is_empty());
        assert!(release(&mut tools, [20.0, 0.0]).is_empty());
        assert!(session.snapshot()["model"]["param"][0]["bindings"].as_array().unwrap().is_empty());
    }

    #[test]
    fn inflate_ticks_are_time_based_and_release_commits_the_frozen_preview_once() {
        let mut reference: Option<Vec<V2>> = None;
        for hz in [30, 60, 120] {
            let mut session = kite();
            let before = session.snapshot();
            let mut tools = deform_tools(&session);
            tools.set_tool(Tool::Brush);
            tools.settings.brush_mode = BrushMode::Inflate;
            press(&mut tools, [0.0, 0.0], NONE);
            for _ in 0..hz {
                tools.tick_inflate(1.0 / f64::from(hz), false, false);
            }
            tools.tick_inflate(0.0, false, true);
            let preview = tools.preview().0;
            assert_eq!(session.snapshot(), before, "holding only changes the preview");
            assert!(preview[1][0] > 100.0);
            if let Some(points) = &reference {
                for (got, want) in preview.iter().zip(points) {
                    assert!((got[0] - want[0]).abs() < 1e-9 && (got[1] - want[1]).abs() < 1e-9);
                }
            }
            reference = Some(preview.clone());
            tools.settings.brush = BrushKind::Line;
            tools.settings.radius = 1.0;
            tools.settings.inflate_rate = 2.0;
            tools.settings.mirror_horizontal = true;
            let requests = release(&mut tools, [0.0, 0.0]);
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0]["tool"], "inflate");
            assert_eq!(requests[0]["samples"].as_array().unwrap().len(), 60);
            assert_eq!(requests[0]["brush"], json!({"kind": "circle", "radius": 300.0}));
            assert_eq!(requests[0]["mirror"]["horizontal"], false);
            send(&mut session, &requests);
            sync(&mut tools, &session, Some((17, KEY)));
            for (got, want) in tools.points().iter().zip(&preview) {
                assert!((got[0] - want[0]).abs() < 1e-4 && (got[1] - want[1]).abs() < 1e-4);
            }
            assert!(release(&mut tools, [0.0, 0.0]).is_empty());
            send(&mut session, &[json!({"op": "undo"})]);
            assert_eq!(session.snapshot()["model"], before["model"]);
        }
    }

    #[test]
    fn inflation_timing_interruptions_cancel_and_the_limit_preserves_the_stroke() {
        let session = kite();
        let mut tools = deform_tools(&session);
        tools.set_tool(Tool::Brush);
        tools.settings.brush_mode = BrushMode::Inflate;
        press(&mut tools, [0.0, 0.0], NONE);
        tools.tick_inflate(0.1, false, false);
        tools.tick_inflate(0.26, false, false);
        assert!(release(&mut tools, [0.0, 0.0]).is_empty());
        assert!(tools.notice.contains("timing interruption"));
        press(&mut tools, [0.0, 0.0], NONE);
        for _ in 0..300 {
            tools.tick_inflate(0.25, true, false);
        }
        assert!(tools.notice.contains("Stroke limit"));
        let requests = release(&mut tools, [0.0, 0.0]);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["samples"].as_array().unwrap().len(), MAX_INFLATE_SAMPLES);
        assert!(tools.notice.is_empty());
    }

    #[test]
    fn inflate_center_uses_the_part_local_frame() {
        let mut item = part(json!([13, 24, 63, 24, 13, 74]), json!([0, 1, 2]));
        item["mesh"]["origin"] = json!([10, 20]);
        let part_session = session_with(item);
        let mut group = group_session(json!([13, 24, 63, 24, 13, 74]), json!([0, 0, 1, 0, 0, 1]), json!([0, 1, 2]));
        let mut model = group.snapshot()["model"].clone();
        model["nodes"]["children"][0]["mesh"]["origin"] = json!([10, 20]);
        assert_eq!(group.handle_request(&json!({"op": "new", "model": model}))["succeeded"], true);
        let world = [[2.0, 0.0, 0.0, 100.0], [0.0, 3.0, 0.0, 200.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
        for (session, expected) in [(part_session, [106.0, 212.0]), (group, [126.0, 272.0])] {
            let mut tools = deform_tools(&session);
            let snapshot = session.snapshot();
            tools.sync(&Ctx {
                revision: snapshot["revision"].as_u64().unwrap(),
                model: &snapshot["model"],
                selected: Some(5),
                armed: Some((17, KEY)),
                frame: Frame::from_world_matrix(&world),
                zoom: 1.0,
            });
            tools.set_tool(Tool::Brush);
            tools.settings.brush_mode = BrushMode::Inflate;
            press(&mut tools, expected, NONE);
            tools.tick_inflate(0.1, false, true);
            assert_eq!(tools.preview().0[0], [13.0, 24.0]);
            let requests = release(&mut tools, expected);
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0]["samples"][0]["center"], json!([13.0, 24.0]));
        }
    }

    #[test]
    fn line_brush_outline_preserves_local_boundaries_under_affine_frames() {
        for m in [[[0.0, -3.0], [2.0, 0.0]], [[-2.0, 0.5], [0.0, 3.0]]] {
            let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
            let frame = Frame { m, inv: [[m[1][1]/det, -m[0][1]/det], [-m[1][0]/det, m[0][0]/det]], t: [7.0, -11.0] };
            let mut tools = MeshTools::new();
            tools.settings.brush = BrushKind::Line;
            tools.settings.radius = 25.0;
            tools.settings.line_angle_degrees = 45.0;
            let picture = Picture {
                rect: Rect::from_min_max(Pos2::new(20.0, 30.0), Pos2::new(620.0, 430.0)),
                size: [600.0, 400.0],
                camera: crate::editor_viewport::Camera { centre: [10.0, -20.0], zoom: 1.5, mirror: true },
            };
            let center = [13.0, -4.0];
            let context = egui::Context::default();
            let output = context.run(egui::RawInput { screen_rect: Some(picture.rect), ..Default::default() }, |ctx| {
                let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("line-outline")));
                tools.paint_brush(&painter, &picture, &frame, center);
            });
            let mut count = 0;
            for shape in output.shapes {
                if let egui::Shape::LineSegment { points, .. } = shape.shape {
                    assert_eq!(shape.clip_rect, picture.rect);
                    for p in points {
                        let local = frame.to_local(picture.world(p));
                        let normal = (-(local[0]-center[0]) + local[1]-center[1]) / 2.0_f64.sqrt();
                        assert!((normal.abs()-25.0).abs() < 1e-4, "{local:?}: {normal}");
                    }
                    count += 1;
                }
            }
            assert_eq!(count, 4, "both boundaries are painted with an outline and foreground");
        }
    }

    #[test]
    fn mirror_in_deform_mode_moves_the_partner_too() {
        let mut session = session_with(part(json!([-50.0, 0.0, 50.0, 0.0, 0.0, 80.0]), json!([0, 1, 2])));
        let mut tools = deform_tools(&session);
        tools.settings.mirror_horizontal = true;
        click(&mut tools, [50.0, 0.0], NONE);
        press(&mut tools, [50.0, 0.0], NONE);
        drag_to(&mut tools, [60.0, 0.0]);
        drag_to(&mut tools, [70.0, 0.0]);
        let preview = tools.preview().0;
        assert_eq!(preview[0], [-60.0, 0.0], "the mirror partner follows, reflected");
        let requests = release(&mut tools, [70.0, 0.0]);
        send(&mut session, &requests);
        sync(&mut tools, &session, Some((17, KEY)));
        assert_close(&tools.points()[0..2], &[[-60.0, 0.0], [60.0, 0.0]]);
    }

    #[test]
    fn the_path_tool_defines_a_path_then_deforms_along_it() {
        let mut session = session_with(part(json!([10.0, 10.0, 50.0, 20.0, 90.0, 10.0]), json!([0, 1, 2])));
        let mut tools = deform_tools(&session);
        tools.set_tool(Tool::PathDeform);
        for p in [[0.0, 0.0], [100.0, 0.0]] {
            tools.event(Pointer::DoubleClick { at: p, mods: NONE });
        }
        tools.event(Pointer::DoubleClick { at: [50.0, 2.0], mods: NONE });
        assert_eq!(tools.path, vec![[0.0, 0.0], [50.0, 0.0], [100.0, 0.0]]);
        tools.set_path_transform(true);
        press(&mut tools, [50.0, 0.0], NONE);
        drag_to(&mut tools, [50.0, 10.0]);
        drag_to(&mut tools, [50.0, 30.0]);
        let preview = tools.preview().0;
        assert_ne!(preview[1], [50.0, 20.0], "the middle vertex follows the path");
        let requests = release(&mut tools, [50.0, 30.0]);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["tool"], "path_deform");
        assert_eq!(requests[0]["path"], json!([[0.0, 0.0], [50.0, 0.0], [100.0, 0.0]]));
        assert_eq!(requests[0]["target"], json!([[0.0, 0.0], [50.0, 20.0], [100.0, 0.0]]));
        send(&mut session, &requests);
        sync(&mut tools, &session, Some((17, KEY)));
        assert_close(&tools.points(), &preview);
        // The next drag starts from the new shape.
        assert_eq!(tools.path, vec![[0.0, 0.0], [50.0, 20.0], [100.0, 0.0]]);
        // Transform needs a path.
        let mut empty = deform_tools(&session);
        empty.set_tool(Tool::PathDeform);
        empty.set_path_transform(true);
        assert!(!empty.path_transform() && empty.notice.contains("path"));
    }

    // ------------------------------------------------------------------------- frame mapping

    #[test]
    fn pointer_positions_are_mapped_into_the_node_and_the_radius_follows_the_scale() {
        // The Part sits at world (200, 100) and is drawn twice as big.
        let world = [[2.0, 0.0, 0.0, 200.0], [0.0, 2.0, 0.0, 100.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
        let frame = Frame::from_world_matrix(&world).unwrap();
        assert_eq!(frame.to_world([10.0, 5.0]), [220.0, 110.0]);
        assert_eq!(frame.to_local([220.0, 110.0]), [10.0, 5.0]);
        assert_eq!(frame.scale(), 2.0);
        let session = kite();
        let snapshot = session.snapshot();
        let mut tools = MeshTools::new();
        tools.start_edit_mesh(5);
        let ctx = Ctx {
            revision: snapshot["revision"].as_u64().unwrap(),
            model: &snapshot["model"],
            selected: Some(5),
            armed: None,
            frame: Some(frame),
            zoom: 1.0,
        };
        tools.sync(&ctx);
        // Local vertex (50, 80) is at world (300, 260).
        click(&mut tools, [300.0, 260.0], NONE);
        assert_eq!(tools.selection(), [2]);
        // 16 points at zoom 1 are 8 local units: 7 away hits, 9 away does not.
        click(&mut tools, [300.0 + 14.0, 260.0], NONE);
        assert_eq!(tools.selection(), [2]);
        click(&mut tools, [300.0 + 18.0, 260.0], NONE);
        assert!(tools.selection().is_empty());
        click(&mut tools, [300.0, 260.0], CTRL);
        click(&mut tools, [240.0, 160.0], CTRL);
        assert_eq!(tools.pending_edits()[1], json!({"op": "add", "pos": [20.0, 30.0]}));
        let singular = [[0.0; 4], [0.0; 4], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
        assert_eq!(Frame::from_world_matrix(&singular), None);
    }

    #[test]
    fn geometry_is_read_for_the_four_node_kinds_and_rejects_the_rest() {
        let part = part(json!([0.0, 0.0, 4.0, 0.0, 0.0, 4.0]), json!([0, 1, 2]));
        let g = read_geometry(&part).unwrap();
        assert_eq!((g.kind, g.points.clone(), g.indices.clone()), (NodeKind::Part, vec![[0.0, 0.0], [4.0, 0.0], [0.0, 4.0]], vec![0, 1, 2]));
        assert_eq!(edges_of(&g, 1), vec![[0, 1], [1, 2], [0, 2]]);
        let path = json!({"uuid": 6, "type": "PathDeformer", "vertices": [0.0, 0.0, 5.0, 5.0, 9.0, 0.0]});
        let g = read_geometry(&path).unwrap();
        assert_eq!(edges_of(&g, 1), vec![[0, 1], [1, 2]]);
        let grid = json!({"uuid": 7, "type": "GridDeformer", "grid_axis_x": [0.0, 1.0, 2.0], "grid_axis_y": [0.0, 4.0]});
        let g = read_geometry(&grid).unwrap();
        assert_eq!(g.points, vec![[0.0, 0.0], [1.0, 0.0], [2.0, 0.0], [0.0, 4.0], [1.0, 4.0], [2.0, 4.0]]);
        assert_eq!(edges_of(&g, 3).len(), 7);
        assert_eq!(read_geometry(&json!({"uuid": 8, "type": "Node"})), None);
        assert_eq!(read_geometry(&json!({"uuid": 8, "type": "Part", "mesh": {"verts": [1.0], "indices": []}})), None);
        // Top-level arrays win over `mesh`, as in the session's own parser.
        let both = json!({"uuid": 9, "type": "Part", "vertices": [[1.0, 2.0]], "indices": [], "mesh": {"verts": [0.0, 0.0]}});
        assert_eq!(read_geometry(&both).unwrap().points, vec![[1.0, 2.0]]);
    }

    #[test]
    fn the_bezier_tool_translates_and_rotates_path_deformer_points() {
        let mut model_part = part(json!([0.0, 0.0]), json!([]));
        model_part["type"] = json!("PathDeformer");
        model_part.as_object_mut().unwrap().remove("mesh");
        model_part["vertices"] = json!([0.0, 0.0, 100.0, 0.0, 200.0, 0.0]);
        let mut session = session_with(model_part);
        let mut tools = deform_tools(&session);
        assert_eq!(tools.tool_offered(), [Tool::Point, Tool::Brush, Tool::Lasso, Tool::Bezier, Tool::PathDeform]);
        tools.set_tool(Tool::Bezier);
        press(&mut tools, [100.0, 0.0], NONE);
        assert_eq!(tools.selection(), [1]);
        drag_to(&mut tools, [100.0, 10.0]);
        drag_to(&mut tools, [100.0, 40.0]);
        let requests = release(&mut tools, [100.0, 40.0]);
        assert_eq!(requests[0]["tool"], "bezier_translate");
        assert_eq!(requests[0]["delta"], json!([0.0, 30.0]));
        send(&mut session, &requests);
        sync(&mut tools, &session, Some((17, KEY)));
        assert_eq!(tools.points()[1], [100.0, 30.0]);
        // Ctrl+click on point 0 locks it; dragging point 2 then rotates about it.
        click(&mut tools, [0.0, 0.0], CTRL);
        click(&mut tools, [200.0, 0.0], NONE);
        press(&mut tools, [200.0, 0.0], NONE);
        drag_to(&mut tools, [200.0, 10.0]);
        let requests = release(&mut tools, [200.0, 100.0]);
        assert_eq!(requests[0]["tool"], "bezier_rotate");
        assert_eq!(requests[0]["locked"], 0);
        assert_eq!(requests[0]["index"], 2);
        send(&mut session, &requests);
    }

    #[test]
    fn tools_offered_follow_the_d_strip() {
        use NodeKind::*;
        assert_eq!(
            Tool::offered(Mode::Vertex, Part),
            [Tool::Point, Tool::Connect, Tool::Grid, Tool::Lasso, Tool::EdgeCutter]
        );
        assert_eq!(Tool::offered(Mode::Deform, Part), [Tool::Point, Tool::Brush, Tool::Lasso, Tool::PathDeform]);
        assert_eq!(Tool::offered(Mode::Deform, GridDeformer), Tool::offered(Mode::Deform, MeshGroup));
        assert_eq!(Tool::offered(Mode::Vertex, MeshGroup), Tool::offered(Mode::Vertex, Part));
        assert_eq!(Tool::offered(Mode::Vertex, PathDeformer), [Tool::Point, Tool::Lasso, Tool::Bezier]);
        assert_eq!(Tool::offered(Mode::Vertex, GridDeformer), [Tool::Grid]);
    }

    #[test]
    fn nothing_is_captured_without_a_part_or_when_the_armed_node_has_no_geometry() {
        let session = kite();
        let snapshot = session.snapshot();
        let mut tools = MeshTools::new();
        let none = Ctx {
            revision: snapshot["revision"].as_u64().unwrap(),
            model: &snapshot["model"],
            selected: None,
            armed: None,
            frame: Some(Frame::IDENTITY),
            zoom: 1.0,
        };
        tools.sync(&none);
        assert!(!tools.captures() && tools.edit_mesh_target().is_none());
        let root = Ctx { selected: Some(1), ..none };
        tools.sync(&root);
        assert!(!tools.captures());
        assert!(tools.event(Pointer::Key { key: Key::Delete, mods: NONE }).is_empty());
    }

    #[test]
    fn selecting_another_node_leaves_the_edit_mesh_mode() {
        let session = kite();
        let mut tools = vertex_tools(&session);
        click(&mut tools, [30.0, 30.0], CTRL);
        let snapshot = session.snapshot();
        let ctx = Ctx {
            revision: snapshot["revision"].as_u64().unwrap(),
            model: &snapshot["model"],
            selected: Some(1),
            armed: None,
            frame: Some(Frame::IDENTITY),
            zoom: 1.0,
        };
        tools.sync(&ctx);
        assert!(!tools.captures() && !tools.edit_mesh_started());
        assert!(tools.pending_edits().is_empty(), "the pending edits are dropped like a Cancel");
        // Arming a parameter switches to deform mode and drops the vertex-mode request.
        let mut tools = vertex_tools(&session);
        sync(&mut tools, &session, Some((17, KEY)));
        assert_eq!(tools.mode(), Some(Mode::Deform));
        assert!(!tools.edit_mesh_started());
    }

    // ------------------------------------------------ vertex definition of MeshGroup, PathDeformer, GridDeformer

    fn deformer_node(kind: &str, fields: Value) -> Value {
        let mut node = json!({
            "uuid": 5, "name": "Node5", "type": kind, "enabled": true, "zsort": 0.0, "transform": transform(),
            "children": [], "lockToRoot": false, "pinToMesh": false,
        });
        node.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        node
    }

    fn path_session(vertices: Value, curve: &str) -> Session {
        let fields = json!({"curve_type": curve, "dynamic_deformation": false, "vertices": vertices});
        session_with(deformer_node("PathDeformer", fields))
    }

    fn grid_session(xs: Value, ys: Value) -> Session {
        let fields = json!({"dynamic": false, "formation": "Bilinear", "grid_axis_x": xs, "grid_axis_y": ys});
        session_with(deformer_node("GridDeformer", fields))
    }

    fn group_session(verts: Value, uvs: Value, indices: Value) -> Session {
        let fields = json!({
            "mesh": {"verts": verts, "uvs": uvs, "indices": indices, "origin": [0.0, 0.0]},
            "blend_mode": "Normal", "dynamic_deformation": false, "opacity": 1.0, "screenTint": [0.0, 0.0, 0.0],
            "tint": [1.0, 1.0, 1.0], "translate_children": true,
        });
        session_with(deformer_node("MeshGroup", fields))
    }

    fn node_of(session: &Session) -> Value {
        find_node(&session.snapshot()["model"]["nodes"], 5).unwrap().clone()
    }

    fn revision(session: &Session) -> u64 {
        session.snapshot()["revision"].as_u64().unwrap()
    }

    fn doubled(tools: &mut MeshTools, at: V2) {
        tools.event(Pointer::DoubleClick { at, mods: NONE });
    }

    fn close_to(actual: V2, expected: V2) {
        assert!((actual[0] - expected[0]).abs() < 1e-3 && (actual[1] - expected[1]).abs() < 1e-3, "{actual:?} != {expected:?}");
    }

    fn pair(value: &Value) -> V2 {
        pair_of(value).unwrap()
    }

    #[test]
    fn empty_deformers_can_be_edited_but_have_nothing_to_deform() {
        let session = path_session(json!([]), "Spline");
        let snapshot = session.snapshot();
        let g = read_geometry(find_node(&snapshot["model"]["nodes"], 5).unwrap()).unwrap();
        assert_eq!((g.kind, g.points.len(), g.curve), (NodeKind::PathDeformer, 0, CurveKind::Spline));
        let tools = vertex_tools(&session);
        assert_eq!(tools.mode(), Some(Mode::Vertex));
        let mut armed = MeshTools::new();
        sync(&mut armed, &session, Some((17, KEY)));
        assert!(!armed.captures(), "an armed parameter cannot deform a node without points");
        let bezier = path_session(json!([0.0, 0.0, 1.0, 1.0]), "Bezier");
        let snapshot = bezier.snapshot();
        assert_eq!(read_geometry(find_node(&snapshot["model"]["nodes"], 5).unwrap()).unwrap().curve, CurveKind::Bezier);
    }

    #[test]
    fn a_path_deformer_is_edited_with_the_point_tool_and_applied_as_one_define_vertices() {
        let mut session = path_session(json!([0.0, 0.0, 100.0, 0.0, 200.0, 0.0]), "Spline");
        let mut tools = vertex_tools(&session);
        assert_eq!(tools.tool_offered(), [Tool::Point, Tool::Lasso, Tool::Bezier]);
        click(&mut tools, [300.0, 50.0], CTRL);
        press(&mut tools, [100.0, 0.0], NONE);
        drag_to(&mut tools, [110.0, 0.0]);
        drag_to(&mut tools, [130.0, 20.0]);
        release(&mut tools, [130.0, 20.0]);
        assert_eq!(
            tools.pending_edits(),
            [json!({"op": "add", "pos": [300.0, 50.0]}), json!({"op": "move", "indices": [1], "delta": [20.0, 20.0]})]
        );
        assert_eq!(tools.points(), [[0.0, 0.0], [120.0, 20.0], [200.0, 0.0], [300.0, 50.0]]);
        assert_eq!(tools.preview().1, [[0, 1], [1, 2], [2, 3]]);
        let before = revision(&session);
        assert_eq!(node_of(&session)["vertices"], json!([0.0, 0.0, 100.0, 0.0, 200.0, 0.0]), "nothing before Apply");

        let request = tools.apply().expect("edits are pending");
        assert_eq!(
            request,
            json!({"op": "deformer.define_vertices", "nodes": [5],
                   "vertices": [0.0, 0.0, 120.0, 20.0, 200.0, 0.0, 300.0, 50.0]})
        );
        assert!(tools.mode().is_none(), "Apply leaves the mode");
        send(&mut session, &[request]);
        assert_eq!(revision(&session), before + 1, "one undo step");
        assert_eq!(node_of(&session)["vertices"], json!([0.0, 0.0, 120.0, 20.0, 200.0, 0.0, 300.0, 50.0]));
        assert_eq!(session.handle_request(&json!({"op": "undo"}))["succeeded"], true);
        assert_eq!(node_of(&session)["vertices"], json!([0.0, 0.0, 100.0, 0.0, 200.0, 0.0]));
    }

    #[test]
    fn a_path_of_rectangle_corners_keeps_its_order() {
        // Four points that form a grid must not be sorted the way a mesh is when it is built.
        let mut session = path_session(json!([]), "Spline");
        let mut tools = vertex_tools(&session);
        for p in [[100.0, 100.0], [0.0, 100.0], [0.0, 0.0], [100.0, 0.0]] {
            click(&mut tools, p, CTRL);
        }
        assert_eq!(tools.points(), [[100.0, 100.0], [0.0, 100.0], [0.0, 0.0], [100.0, 0.0]]);
        send(&mut session, &[tools.apply().unwrap()]);
        assert_eq!(node_of(&session)["vertices"], json!([100.0, 100.0, 0.0, 100.0, 0.0, 0.0, 100.0, 0.0]));
    }

    #[test]
    fn pure_insertion_follows_the_bezier_tool_rules() {
        let line = [[0.0, 0.0], [100.0, 0.0], [200.0, 0.0]];
        // On the curve: between the neighbours by curve position, at the curve point.
        let (index, pos) = bezier_insertion(CurveKind::Spline, &line, [150.0, 2.0], 1.0);
        assert_eq!(index, 2);
        close_to(pos, [150.0, 0.0]);
        let (index, pos) = bezier_insertion(CurveKind::Spline, &line, [40.0, -3.0], 1.0);
        assert_eq!(index, 1);
        close_to(pos, [40.0, 0.0]);
        // Off the curve: the first half goes in front, the rest at the end, both where the pointer is.
        assert_eq!(bezier_insertion(CurveKind::Spline, &line, [-60.0, 90.0], 1.0), (0, [-60.0, 90.0]));
        assert_eq!(bezier_insertion(CurveKind::Spline, &line, [260.0, 90.0], 1.0), (3, [260.0, 90.0]));
        // The select radius shrinks with the zoom: 2 units off the curve is off it at zoom 16 (radius 1).
        assert_eq!(bezier_insertion(CurveKind::Spline, &line, [150.0, 2.0], 16.0), (3, [150.0, 2.0]));
        // One quadratic Bezier leans towards its middle point: (50, 37.5) is its t = 0.25 point.
        let arch = [[0.0, 0.0], [100.0, 100.0], [200.0, 0.0]];
        let (index, pos) = bezier_insertion(CurveKind::Bezier, &arch, [50.0, 37.5], 1.0);
        assert_eq!(index, 1);
        close_to(pos, [50.0, 37.5]);
        // Too few points have no curve to land on.
        assert_eq!(bezier_insertion(CurveKind::Spline, &[], [5.0, 6.0], 1.0), (0, [5.0, 6.0]));
        assert_eq!(bezier_insertion(CurveKind::Spline, &[[1.0, 1.0]], [5.0, 6.0], 1.0), (1, [5.0, 6.0]));
    }

    #[test]
    fn the_bezier_tool_double_click_inserts_on_the_curve_or_removes_a_point_and_apply_keeps_the_order() {
        let mut session = path_session(json!([0.0, 0.0, 100.0, 0.0, 200.0, 0.0]), "Spline");
        let mut tools = vertex_tools(&session);
        tools.set_tool(Tool::Bezier);
        doubled(&mut tools, [150.0, 2.0]);
        let edit = tools.pending_edits()[0].clone();
        assert_eq!((edit["op"].clone(), edit["index"].clone()), (json!("insert"), json!(2)));
        close_to(pair(&edit["pos"]), [150.0, 0.0]);
        assert_eq!(tools.points().len(), 4);
        assert_eq!(tools.points()[2], pair(&edit["pos"]), "the preview shows the point in place");
        doubled(&mut tools, [-60.0, 90.0]);
        assert_eq!(tools.pending_edits()[1], json!({"op": "insert", "index": 0, "pos": [-60.0, 90.0]}));
        // A double click on a control point removes it: (100, 0) is index 2 now.
        doubled(&mut tools, [100.0, 0.0]);
        assert_eq!(tools.pending_edits()[2], json!({"op": "remove", "indices": [2]}));
        let shown = tools.points();
        assert_eq!(shown.len(), 4);
        assert_eq!(shown[0], [-60.0, 90.0]);
        assert_eq!(&shown[1..2], &[[0.0, 0.0]]);
        let before = revision(&session);
        let request = tools.apply().unwrap();
        assert_eq!(request["op"], "deformer.define_vertices");
        send(&mut session, &[request]);
        assert_eq!(revision(&session), before + 1);
        let stored = node_of(&session)["vertices"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
        assert_eq!(stored, flat(&shown));
        assert_eq!(session.handle_request(&json!({"op": "undo"}))["succeeded"], true);
        assert_eq!(node_of(&session)["vertices"], json!([0.0, 0.0, 100.0, 0.0, 200.0, 0.0]));
    }

    #[test]
    fn dragging_with_the_bezier_tool_in_vertex_mode_moves_the_selected_points() {
        let mut tools = vertex_tools(&path_session(json!([0.0, 0.0, 100.0, 0.0, 200.0, 0.0]), "Spline"));
        tools.set_tool(Tool::Bezier);
        press(&mut tools, [100.0, 0.0], NONE);
        drag_to(&mut tools, [110.0, 0.0]);
        drag_to(&mut tools, [120.0, 30.0]);
        release(&mut tools, [120.0, 30.0]);
        assert_eq!(tools.pending_edits(), [json!({"op": "move", "indices": [1], "delta": [10.0, 30.0]})]);
        // Ctrl+click does not lock a rotation centre here, and does not add a vertex either.
        click(&mut tools, [300.0, 300.0], CTRL);
        assert_eq!(tools.pending_edits().len(), 1);
    }

    #[test]
    fn a_grid_deformer_is_edited_with_the_grid_tool_and_applied_as_one_define_grid() {
        let mut session = grid_session(json!([0.0, 100.0, 200.0]), json!([0.0, 50.0]));
        let mut tools = vertex_tools(&session);
        assert_eq!(tools.tool_offered(), [Tool::Grid]);
        assert_eq!(tools.tool, Tool::Grid);
        assert_eq!(tools.preview().1.len(), 7);
        // Drag the middle vertex of the bottom row: that moves the x line of the grid.
        press(&mut tools, [100.0, 50.0], NONE);
        drag_to(&mut tools, [110.0, 50.0]);
        drag_to(&mut tools, [130.0, 50.0]);
        release(&mut tools, [130.0, 50.0]);
        assert_eq!(tools.pending_edits(), [json!({"op": "grid_move", "vertex": 4, "to": [130.0, 50.0]})]);
        assert_eq!(
            tools.points(),
            [[0.0, 0.0], [130.0, 0.0], [200.0, 0.0], [0.0, 50.0], [130.0, 50.0], [200.0, 50.0]]
        );
        // A double click on empty ground adds a line on each axis.
        doubled(&mut tools, [60.0, 25.0]);
        assert_eq!(tools.pending_edits()[1], json!({"op": "grid_add_axis", "pos": [60.0, 25.0]}));
        assert_eq!(tools.points().len(), 12, "4 columns, 3 rows");
        assert_eq!(tools.preview().1.len(), 3 * 3 + 4 * 2, "the lattice lines follow the new axes");
        let before = revision(&session);
        let request = tools.apply().expect("edits are pending");
        assert_eq!(
            request,
            json!({"op": "deformer.define_grid", "nodes": [5], "axisX": [0.0, 60.0, 130.0, 200.0], "axisY": [0.0, 25.0, 50.0]})
        );
        send(&mut session, &[request]);
        assert_eq!(revision(&session), before + 1, "one undo step");
        let node = node_of(&session);
        assert_eq!((node["grid_axis_x"].clone(), node["grid_axis_y"].clone()), (json!([0.0, 60.0, 130.0, 200.0]), json!([0.0, 25.0, 50.0])));
        assert_eq!(session.handle_request(&json!({"op": "undo"}))["succeeded"], true);
        assert_eq!(node_of(&session)["grid_axis_x"], json!([0.0, 100.0, 200.0]));
    }

    #[test]
    fn a_mesh_group_is_edited_like_a_part_and_applied_as_one_define_mesh() {
        let mut session = group_session(
            json!([0.0, 0.0, 100.0, 0.0, 0.0, 100.0]),
            json!([0.0, 0.0, 1.0, 0.0, 0.0, 1.0]),
            json!([0, 1, 2]),
        );
        let mut tools = vertex_tools(&session);
        assert_eq!(tools.tool_offered(), [Tool::Point, Tool::Connect, Tool::Grid, Tool::Lasso, Tool::EdgeCutter]);
        click(&mut tools, [100.0, 100.0], CTRL);
        tools.set_tool(Tool::Connect);
        click(&mut tools, [100.0, 0.0], NONE);
        click(&mut tools, [100.0, 100.0], NONE);
        click(&mut tools, [0.0, 100.0], NONE);
        click(&mut tools, [100.0, 100.0], NONE);
        assert_eq!(tools.pending_edits().len(), 3);
        assert_eq!(tools.points().len(), 4);
        let before = revision(&session);
        assert_eq!(node_of(&session)["mesh"]["verts"].as_array().unwrap().len(), 6, "nothing before Apply");

        let request = tools.apply().expect("edits are pending");
        assert_eq!(request["op"], "deformer.define_mesh");
        assert_eq!(request["nodes"], json!([5]));
        assert_eq!(request["vertices"], json!([0.0, 0.0, 100.0, 0.0, 0.0, 100.0, 100.0, 100.0]));
        assert_eq!(request["uvs"], json!([0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0]), "the bounding box becomes the unit square");
        assert_eq!(request["indices"].as_array().unwrap().len(), 6, "two triangles");
        assert_eq!(request["origin"], json!([0.0, 0.0]));
        send(&mut session, &[request.clone()]);
        assert_eq!(revision(&session), before + 1, "one undo step");
        assert_eq!(node_of(&session)["mesh"]["indices"], request["indices"]);
        assert_eq!(node_of(&session)["mesh"]["uvs"], request["uvs"]);
        assert_eq!(session.handle_request(&json!({"op": "undo"}))["succeeded"], true);
        assert_eq!(node_of(&session)["mesh"]["verts"].as_array().unwrap().len(), 6);
    }

    #[test]
    fn a_new_mesh_group_needs_a_triangle_before_apply_can_store_it() {
        let mut session = group_session(json!([]), json!([]), json!([]));
        let mut tools = vertex_tools(&session);
        for p in [[0.0, 0.0], [100.0, 0.0], [0.0, 100.0]] {
            click(&mut tools, p, CTRL);
        }
        assert_eq!(tools.apply(), None, "three unconnected points are no mesh");
        assert!(tools.notice.contains("triangle"), "{}", tools.notice);
        assert_eq!(tools.mode(), Some(Mode::Vertex), "the edits are still there to fix");
        assert_eq!(tools.pending_edits().len(), 3);
        tools.triangulate();
        let request = tools.apply().expect("one triangle now");
        assert_eq!(request["vertices"], json!([0.0, 0.0, 100.0, 0.0, 0.0, 100.0]));
        assert_eq!(request["indices"].as_array().unwrap().len(), 3);
        send(&mut session, &[request]);
        assert_eq!(node_of(&session)["mesh"]["verts"].as_array().unwrap().len(), 6);
    }

    #[test]
    fn a_mesh_group_with_a_deform_binding_keeps_its_edits_when_the_session_would_refuse_them() {
        let mut session = group_session(json!([0.0, 0.0, 100.0, 0.0, 0.0, 100.0]), json!([]), json!([0, 1, 2]));
        let model = {
            let mut model = session.snapshot()["model"].clone();
            model["param"][0]["bindings"] = json!([{
                "node": 5, "param_name": "deform", "isSet": [[true], [true]],
                "values": [[[[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]], [[[1.0, 0.0], [1.0, 0.0], [1.0, 0.0]]]],
            }]);
            model["param"][0]["axis_points"] = json!([[0.0, 1.0], [0.0]]);
            model["param"][0]["is_vec2"] = json!(false);
            model
        };
        assert_eq!(session.handle_request(&json!({"op": "new", "model": model}))["succeeded"], true);
        let mut tools = vertex_tools(&session);
        click(&mut tools, [100.0, 100.0], CTRL);
        assert_eq!(tools.pending_edits().len(), 1);
        assert_eq!(tools.apply(), None);
        assert!(tools.notice.contains("deform parameter binding"), "{}", tools.notice);
        assert_eq!(tools.mode(), Some(Mode::Vertex), "the edit mode stays open");
        assert_eq!(tools.pending_edits().len(), 1, "the pending edit is still there");
        // The session really does refuse this request, which is why it is not sent.
        let refused = session.handle_request(&json!({
            "op": "deformer.define_mesh", "nodes": [5], "vertices": [0.0, 0.0, 100.0, 0.0, 0.0, 100.0, 100.0, 100.0],
            "indices": [0, 1, 2, 1, 2, 3],
        }));
        assert_eq!(refused["succeeded"], false);
        assert!(refused["message"].as_str().unwrap().contains(&tools.notice), "{refused}");
        tools.cancel();
        assert!(tools.mode().is_none());
    }

    #[test]
    fn the_storage_blocker_reads_the_same_reasons_as_the_session() {
        let model = json!({"param": [{"uuid": 17, "bindings": [], "children": []},
            {"groupUUID": 20, "children": [{"uuid": 18, "bindings": [{"param_name": "deform", "node": 7}]}]}]});
        let plain = json!({"uuid": 5, "type": "MeshGroup"});
        assert_eq!(storage_blocker(&model, &plain, NodeKind::MeshGroup), None);
        let bound = json!({"uuid": 7, "type": "MeshGroup"});
        assert_eq!(
            storage_blocker(&model, &bound, NodeKind::MeshGroup),
            Some("node 7 has a deform parameter binding and is outside Rust Core mode".to_owned())
        );
        // A path or grid is resized with its bindings, so a binding alone is fine; welded links are not.
        let path = json!({"uuid": 7, "type": "PathDeformer"});
        assert_eq!(storage_blocker(&model, &path, NodeKind::PathDeformer), None);
        let welded = json!({"uuid": 5, "type": "PathDeformer", "weldedLinks": [{"target": 2}]});
        assert_eq!(
            storage_blocker(&model, &welded, NodeKind::PathDeformer),
            Some("node 5 deform geometry is outside Rust Core mode".to_owned())
        );
        // Depths are carried along for paths and grids, but block a MeshGroup.
        let depths = json!({"uuid": 5, "depths": [0.0]});
        assert_eq!(storage_blocker(&model, &depths, NodeKind::GridDeformer), None);
        assert!(storage_blocker(&model, &depths, NodeKind::MeshGroup).is_some());
        let ops = json!({"uuid": 5, "depth-ops": []});
        assert!(storage_blocker(&model, &ops, NodeKind::PathDeformer).is_some());
        assert_eq!(storage_blocker(&model, &json!({"masks": []}), NodeKind::MeshGroup), None, "no id, no verdict");
    }

    #[test]
    fn requests_are_refused_before_they_are_sent_when_the_geometry_cannot_be_stored() {
        assert_eq!(path_request(5, &[]), Err("A path needs at least one control point".to_owned()));
        assert_eq!(
            path_request(5, &[[1.0, 2.0]]),
            Ok(json!({"op": "deformer.define_vertices", "nodes": [5], "vertices": [1.0, 2.0]}))
        );
        assert_eq!(grid_request(5, &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]), Err("The points no longer form a grid".to_owned()));
        // `grid_axes` lists Y first; the request names X first.
        assert_eq!(
            grid_request(5, &[[0.0, 0.0], [10.0, 0.0], [0.0, 5.0], [10.0, 5.0]]),
            Ok(json!({"op": "deformer.define_grid", "nodes": [5], "axisX": [0.0, 10.0], "axisY": [0.0, 5.0]}))
        );
    }

    #[test]
    fn auto_connect_is_not_applied_to_a_path() {
        let mut tools = vertex_tools(&path_session(json!([]), "Spline"));
        tools.settings.auto_connect = true;
        click(&mut tools, [0.0, 0.0], CTRL);
        click(&mut tools, [50.0, 0.0], CTRL);
        assert_eq!(tools.preview().1, [[0, 1]], "the polyline of the path, not a mesh edge");
        assert!(!tools.options().auto_connect);
    }

    fn bezier_deform_tools() -> (Session, MeshTools) {
        let session = path_session(json!([0.0, 0.0, 100.0, 0.0, 200.0, 0.0]), "Spline");
        let mut tools = deform_tools(&session);
        tools.set_tool(Tool::Bezier);
        (session, tools)
    }

    #[test]
    fn shift_slides_a_dragged_path_point_along_the_curve_and_commits_one_translate() {
        let (mut session, mut tools) = bezier_deform_tools();
        press(&mut tools, [100.0, 0.0], NONE);
        tools.event(Pointer::Move { at: [110.0, 0.0], mods: SHIFT });
        tools.event(Pointer::Move { at: [150.0, 30.0], mods: SHIFT });
        // The curve is the line y = 0: the point goes to the foot of the pointer, not to the pointer.
        let preview = tools.preview().0;
        close_to(preview[1], [150.0, 0.0]);
        let requests = tools.event(Pointer::Release { at: [150.0, 30.0], mods: SHIFT });
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["tool"], "bezier_translate");
        assert_eq!(requests[0]["index"], 1);
        close_to(pair(&requests[0]["delta"]), [50.0, 0.0]);
        send(&mut session, &requests);
        sync(&mut tools, &session, Some((17, KEY)));
        close_to(tools.points()[1], [150.0, 0.0]);

        // Without Shift the same drag is a plain translation.
        let (_, mut plain) = bezier_deform_tools();
        press(&mut plain, [100.0, 0.0], NONE);
        drag_to(&mut plain, [110.0, 0.0]);
        drag_to(&mut plain, [150.0, 30.0]);
        let requests = release(&mut plain, [150.0, 30.0]);
        assert_eq!(requests[0]["delta"], json!([40.0, 30.0]), "the drag started at 110");
    }

    #[test]
    fn move_points_along_the_path_makes_every_drag_slide() {
        let (mut session, mut tools) = bezier_deform_tools();
        tools.shift_mode = true;
        press(&mut tools, [100.0, 0.0], NONE);
        drag_to(&mut tools, [110.0, 0.0]);
        drag_to(&mut tools, [60.0, 25.0]);
        let requests = release(&mut tools, [60.0, 25.0]);
        close_to(pair(&requests[0]["delta"]), [-40.0, 0.0]);
        send(&mut session, &requests);
        // A locked rotation centre still rotates, as in D.
        let (_, mut locked) = bezier_deform_tools();
        locked.shift_mode = true;
        click(&mut locked, [0.0, 0.0], CTRL);
        click(&mut locked, [200.0, 0.0], NONE);
        press(&mut locked, [200.0, 0.0], NONE);
        drag_to(&mut locked, [200.0, 10.0]);
        let requests = release(&mut locked, [200.0, 100.0]);
        assert_eq!(requests[0]["tool"], "bezier_rotate");
    }

    // ------------------------------------------------------------------ headless egui frames

    use crate::editor_viewport::{Camera, ViewportState};
    use egui::accesskit::Role;

    struct Harness {
        ctx: egui::Context,
        viewport: ViewportState,
        tools: MeshTools,
        session: Session,
        armed: Option<(u32, [usize; 2])>,
        requests: Vec<Value>,
        rect: Rect,
    }

    impl Harness {
        fn new(session: Session, armed: Option<(u32, [usize; 2])>) -> Self {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut viewport = ViewportState::new();
            viewport.camera = Camera { centre: [50.0, 50.0], zoom: 2.0, mirror: false };
            let mut harness = Self {
                ctx,
                viewport,
                tools: MeshTools::new(),
                session,
                armed,
                requests: Vec::new(),
                rect: Rect::NOTHING,
            };
            for _ in 0..3 {
                harness.frame(Vec::new());
            }
            harness
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            self.frame_input(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(700.0, 500.0))),
                events,
                ..Default::default()
            })
        }

        fn timed_frame(&mut self, events: Vec<egui::Event>, time: f64, shift: bool, focused: bool) -> egui::FullOutput {
            self.frame_input(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(700.0, 500.0))),
                events,
                time: Some(time),
                modifiers: egui::Modifiers { shift, ..Default::default() },
                focused,
                ..Default::default()
            })
        }

        fn frame_input(&mut self, input: egui::RawInput) -> egui::FullOutput {
            let (viewport, tools, session, armed) = (&mut self.viewport, &mut self.tools, &mut self.session, self.armed);
            let (mut sent, mut rect_out) = (Vec::new(), Rect::NOTHING);
            let output = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
                    sent.extend(tools.strip(ui));
                    let (rect, response) = viewport.allocate(ui);
                    rect_out = rect;
                    viewport.sync_scene(&mut |request| session.handle_request(request), true, false);
                    let snapshot = session.snapshot();
                    let frame = viewport.world_matrix(5).and_then(|m| Frame::from_world_matrix(&m));
                    let tool_ctx = Ctx {
                        revision: snapshot["revision"].as_u64().unwrap(),
                        model: &snapshot["model"],
                        selected: Some(5),
                        armed,
                        frame,
                        zoom: viewport.camera.zoom,
                    };
                    tools.sync(&tool_ctx);
                    let picture = Picture { rect, camera: viewport.camera, size: viewport.size() };
                    sent.extend(tools.pointer(ui, &picture, &response));
                    tools.paint(ui, &picture);
                });
            });
            self.rect = rect_out;
            for request in &sent {
                let reply = self.session.handle_request(request);
                assert_eq!(reply["succeeded"], true, "{request} -> {reply}");
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

    #[test]
    fn stationary_inflate_pointer_frames_and_shift_work_on_part_path_and_grid() {
        for session in [kite(), path_session(json!([-50, 0, 0, 0, 50, 0]), "Spline"), grid_session(json!([-50, 0, 50]), json!([-50, 0, 50]))] {
            let mut harness = Harness::new(session, Some((17, KEY)));
            harness.tools.set_tool(Tool::Brush);
            harness.tools.settings.brush_mode = BrushMode::Inflate;
            harness.timed_frame(Vec::new(), 0.9, false, true);
            let at = harness.screen([0.0, 0.0]);
            let before = harness.session.snapshot();
            harness.timed_frame(vec![egui::Event::PointerMoved(at), button(at, true)], 1.0, false, true);
            harness.timed_frame(Vec::new(), 1.1, false, true);
            let grown = harness.tools.preview().0;
            assert!(harness.requests.is_empty());
            assert_eq!(harness.session.snapshot(), before);
            harness.timed_frame(Vec::new(), 1.2, true, true);
            let shrunk = harness.tools.preview().0;
            let extent = |points: &[V2]| points.iter().map(|point| point[0].hypot(point[1])).sum::<f64>();
            assert!(extent(&shrunk) < extent(&grown));
            harness.timed_frame(
                vec![egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers { shift: true, ..Default::default() },
                }],
                1.2,
                true,
                true,
            );
            assert_eq!(harness.requests.len(), 1);
            assert_eq!(harness.requests[0]["tool"], "inflate");
            harness.timed_frame(Vec::new(), 1.21, false, true);
            for (got, want) in harness.tools.points().iter().zip(&shrunk) {
                assert!((got[0] - want[0]).abs() < 1e-4 && (got[1] - want[1]).abs() < 1e-4);
            }
            assert_eq!(harness.requests.len(), 1);
        }
    }

    #[test]
    fn held_inflation_is_discarded_on_focus_escape_or_base_change() {
        for cancel in ["focus", "escape", "base", "target", "tool", "revision"] {
            let mut harness = Harness::new(kite(), Some((17, KEY)));
            harness.tools.set_tool(Tool::Brush);
            harness.tools.settings.brush_mode = BrushMode::Inflate;
            harness.timed_frame(Vec::new(), 0.9, false, true);
            let at = harness.screen([0.0, 0.0]);
            harness.timed_frame(vec![egui::Event::PointerMoved(at), button(at, true)], 1.0, false, true);
            harness.timed_frame(Vec::new(), 1.1, false, true);
            let mut expected = harness.session.snapshot()["model"].clone();
            match cancel {
                "focus" => {
                    harness.timed_frame(Vec::new(), 1.2, false, false);
                }
                "escape" => {
                    harness.timed_frame(
                        vec![
                            egui::Event::PointerMoved(Pos2::new(-100.0, -100.0)),
                            egui::Event::Key {
                                key: egui::Key::Escape,
                                physical_key: None,
                                pressed: true,
                                repeat: false,
                                modifiers: Default::default(),
                            },
                        ],
                        1.2,
                        false,
                        true,
                    );
                }
                "target" => {
                    harness.armed = None;
                    harness.timed_frame(Vec::new(), 1.2, false, true);
                    harness.armed = Some((17, KEY));
                }
                "tool" => {
                    harness.tools.set_tool(Tool::Point);
                    harness.timed_frame(Vec::new(), 1.2, false, true);
                }
                "revision" => {
                    let mut model = expected.clone();
                    model["meta"]["name"] = json!("Replacement");
                    let reply = harness.session.handle_request(&json!({"op": "new", "model": model}));
                    assert_eq!(reply["succeeded"], true);
                    expected = harness.session.snapshot()["model"].clone();
                    harness.timed_frame(Vec::new(), 1.2, false, true);
                }
                _ => {
                    let reply = harness.session.handle_request(&json!({
                        "op": "deform.edit",
                        "id": 5,
                        "parameter": 17,
                        "key": KEY,
                        "tool": "translate",
                        "selected": [0],
                        "delta": [1, 0]
                    }));
                    assert_eq!(reply["succeeded"], true);
                    expected = harness.session.snapshot()["model"].clone();
                    harness.timed_frame(Vec::new(), 1.2, false, true);
                }
            }
            harness.timed_frame(vec![button(at, false)], 1.3, false, true);
            assert!(harness.requests.is_empty(), "{cancel}");
            assert_eq!(harness.session.snapshot()["model"], expected, "{cancel}");
            if cancel == "target" {
                // Re-arming rebuilds the deform strip on the following frame. egui hit-tests with the
                // previous pass, so the restored picture accepts a press only after that layout is shown.
                harness.timed_frame(Vec::new(), 1.35, false, true);
                harness.timed_frame(vec![egui::Event::PointerMoved(at), button(at, true)], 1.4, false, true);
                harness.timed_frame(Vec::new(), 1.5, false, true);
                harness.timed_frame(vec![button(at, false)], 1.6, false, true);
                assert_eq!(harness.requests.len(), 1);
                assert_eq!(harness.requests[0]["tool"], "inflate");
            }
        }
    }

    fn labels(output: &egui::FullOutput) -> Vec<String> {
        let update = output.platform_output.accesskit_update.clone().expect("accesskit is enabled");
        update
            .nodes
            .into_iter()
            .flat_map(|(_, node)| [node.label().map(str::to_owned), node.value().map(str::to_owned)])
            .flatten()
            .collect()
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
    fn the_edit_mesh_strip_and_settings_are_named() {
        let mut harness = Harness::new(kite(), None);
        let output = harness.frame(Vec::new());
        expect_labels(&output, &["Edit Mesh"]);
        harness.tools.start_edit_mesh(5);
        harness.frame(Vec::new());
        let output = harness.frame(Vec::new());
        expect_labels(
            &output,
            &[
                "Edit Mesh:", "Vertex Tool", "Edge Tool", "Grid Vertex Tool", "Lasso Selection", "Edge Cutter",
                "Mirror Horizontally", "Mirror Vertically", "Auto connect vertices", "Flip Horizontally",
                "Flip Vertically", "Triangulate vertices", "Undo edit", "Redo edit", "Apply", "Cancel",
                "Tool Settings", "No settings for this tool.", "Mirror origin X", "Mirror origin Y",
            ],
        );
        assert_named_controls(&output);
        harness.tools.set_tool(Tool::Grid);
        harness.frame(Vec::new());
        let output = harness.frame(Vec::new());
        expect_labels(&output, &["Grid cuts"]);
        assert_named_controls(&output);
        harness.tools.set_tool(Tool::Lasso);
        harness.frame(Vec::new());
        let output = harness.frame(Vec::new());
        expect_labels(&output, &["Poly Lasso Selection Mode", "Regular Lasso Selection Mode"]);
    }

    #[test]
    fn the_deform_strip_names_the_brush_path_and_reset_controls() {
        let mut harness = Harness::new(kite(), Some((17, KEY)));
        let output = harness.frame(Vec::new());
        expect_labels(
            &output,
            &["Deform:", "Vertex Tool", "Brush Tool", "Lasso Selection", "Path deformation", "Reset selected"],
        );
        assert_named_controls(&output);
        harness.tools.set_tool(Tool::Brush);
        harness.frame(Vec::new());
        let output = harness.frame(Vec::new());
        expect_labels(&output, &["Drag mode", "Flow mode", "Brush radius", "Circle Brush"]);
        assert_named_controls(&output);
        harness.tools.settings.brush = BrushKind::Rectangle;
        harness.frame(Vec::new());
        let output = harness.frame(Vec::new());
        expect_labels(&output, &["Brush width", "Brush height"]);
        harness.tools.settings.brush = BrushKind::Line;
        harness.frame(Vec::new());
        let output = harness.frame(Vec::new());
        expect_labels(&output, &["Line Brush", "Brush radius", "Line angle (degrees)"]);
        assert_named_controls(&output);
        harness.tools.set_tool(Tool::PathDeform);
        harness.frame(Vec::new());
        let output = harness.frame(Vec::new());
        expect_labels(
            &output,
            &["Define paths", "Transform path", "Set rotation center", "Move points along the path"],
        );
        assert_named_controls(&output);
    }

    #[test]
    fn a_deform_drag_sends_exactly_one_request_when_the_button_is_released() {
        let mut harness = Harness::new(kite(), Some((17, KEY)));
        let start = harness.screen([50.0, 80.0]);
        harness.frame(vec![egui::Event::PointerMoved(start), button(start, true)]);
        // 20 screen points at zoom 2 are 10 world units: the drag starts there.
        harness.frame(vec![egui::Event::PointerMoved(start + egui::vec2(20.0, 0.0))]);
        harness.frame(vec![egui::Event::PointerMoved(start + egui::vec2(40.0, 0.0))]);
        assert!(harness.requests.is_empty(), "nothing is sent while the button is down");
        harness.frame(Vec::new());
        assert!(harness.requests.is_empty());
        harness.frame(vec![button(start + egui::vec2(40.0, 0.0), false)]);
        assert_eq!(
            harness.requests,
            [json!({
                "op": "deform.edit", "id": 5, "parameter": 17, "key": [1, 1],
                "mirror": {"horizontal": false, "vertical": false, "origin": [0.0, 0.0]}, "zoom": 2.0,
                "tool": "translate", "selected": [2], "delta": [10.0, 0.0],
            })]
        );
        assert_eq!(offsets(&harness.session)[2], json!([10.0, 0.0]));
        harness.frame(Vec::new());
        harness.frame(vec![egui::Event::PointerMoved(start)]);
        assert_eq!(harness.requests.len(), 1, "moving the pointer without the button sends nothing");
    }

    #[test]
    fn an_edit_mesh_drag_waits_for_apply_which_sends_one_mesh_edit() {
        let mut harness = Harness::new(kite(), None);
        harness.tools.start_edit_mesh(5);
        // The strip grows by a row: let the layout settle before aiming at the picture.
        for _ in 0..3 {
            harness.frame(Vec::new());
        }
        let start = harness.screen([100.0, 0.0]);
        harness.frame(vec![egui::Event::PointerMoved(start), button(start, true)]);
        harness.frame(vec![egui::Event::PointerMoved(start + egui::vec2(20.0, 0.0))]);
        harness.frame(vec![egui::Event::PointerMoved(start + egui::vec2(40.0, 20.0))]);
        harness.frame(vec![button(start + egui::vec2(40.0, 20.0), false)]);
        assert!(harness.requests.is_empty(), "edits stay in the working copy");
        assert_eq!(
            harness.tools.pending_edits(),
            [json!({"op": "move", "indices": [1], "delta": [10.0, 10.0]})]
        );
        assert_eq!(part_verts(&harness.session)[2], 100.0);

        let output = harness.frame(Vec::new());
        let update = output.platform_output.accesskit_update.clone().unwrap();
        let apply = update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Apply") && node.role() == Role::Button)
            .map(|(_, node)| node.bounds().unwrap())
            .expect("an Apply button");
        let center = Pos2::new(((apply.x0 + apply.x1) / 2.0) as f32, ((apply.y0 + apply.y1) / 2.0) as f32);
        harness.frame(vec![egui::Event::PointerMoved(center), button(center, true), button(center, false)]);
        assert_eq!(harness.requests.len(), 1, "{:?}", harness.requests);
        assert_eq!(harness.requests[0]["op"], "mesh.edit");
        assert_eq!(harness.requests[0]["edits"], json!([{"op": "move", "indices": [1], "delta": [10.0, 10.0]}]));
        assert_eq!(part_verts(&harness.session)[2..4], [110.0, 10.0]);
        assert!(harness.tools.mode().is_none());
    }

    #[test]
    fn cancel_in_the_strip_leaves_the_document_untouched() {
        let mut harness = Harness::new(kite(), None);
        harness.tools.start_edit_mesh(5);
        let before = harness.session.snapshot();
        for _ in 0..3 {
            harness.frame(Vec::new());
        }
        let at = harness.screen([30.0, 30.0]);
        let ctrl = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
        harness.frame(vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: ctrl },
            egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: ctrl },
        ]);
        assert_eq!(harness.tools.pending_edits().len(), 1);
        let output = harness.frame(Vec::new());
        let update = output.platform_output.accesskit_update.clone().unwrap();
        let cancel = update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Cancel") && node.role() == Role::Button)
            .map(|(_, node)| node.bounds().unwrap())
            .unwrap();
        let center = Pos2::new(((cancel.x0 + cancel.x1) / 2.0) as f32, ((cancel.y0 + cancel.y1) / 2.0) as f32);
        harness.frame(vec![egui::Event::PointerMoved(center), button(center, true), button(center, false)]);
        assert!(harness.requests.is_empty());
        assert!(harness.tools.mode().is_none());
        assert_eq!(harness.session.snapshot(), before);
    }

    // ------------------------------------------------------- deformer strips in headless egui frames

    fn settle(harness: &mut Harness) -> egui::FullOutput {
        for _ in 0..3 {
            harness.frame(Vec::new());
        }
        harness.frame(Vec::new())
    }

    fn lacks_labels(output: &egui::FullOutput, unwanted: &[&str]) {
        let found = labels(output);
        for label in unwanted {
            assert!(!found.iter().any(|l| l == label), "unexpected {label:?} in {found:?}");
        }
    }

    #[test]
    fn the_path_deformer_strip_offers_point_lasso_and_bezier_without_the_mesh_only_buttons() {
        let mut harness = Harness::new(path_session(json!([0.0, 0.0, 100.0, 0.0, 200.0, 0.0]), "Spline"), None);
        let output = harness.frame(Vec::new());
        expect_labels(&output, &["Edit Mesh"]);
        harness.tools.start_edit_mesh(5);
        let output = settle(&mut harness);
        expect_labels(
            &output,
            &[
                "Edit Mesh:", "Vertex Tool", "Lasso Selection", "Path Deform Tool", "Mirror Horizontally",
                "Mirror Vertically", "Undo edit", "Redo edit", "Apply", "Cancel", "Tool Settings",
            ],
        );
        lacks_labels(
            &output,
            &["Edge Tool", "Grid Vertex Tool", "Edge Cutter", "Triangulate vertices", "Flip Horizontally", "Auto connect vertices"],
        );
        assert_named_controls(&output);
        harness.tools.set_tool(Tool::Bezier);
        let output = settle(&mut harness);
        expect_labels(&output, &["Double click adds a point on the path, or removes the one under the pointer."]);
        lacks_labels(&output, &["Move points along the path", "Set rotation center"]);
    }

    #[test]
    fn the_grid_and_mesh_group_strips_are_named() {
        let mut harness = Harness::new(grid_session(json!([0.0, 100.0]), json!([0.0, 100.0])), None);
        harness.frame(Vec::new());
        harness.tools.start_edit_mesh(5);
        let output = settle(&mut harness);
        expect_labels(&output, &["Edit Mesh:", "Grid Vertex Tool", "Undo edit", "Apply", "Cancel"]);
        lacks_labels(&output, &["Vertex Tool", "Lasso Selection", "Triangulate vertices"]);
        assert_named_controls(&output);

        let group = group_session(json!([0.0, 0.0, 100.0, 0.0, 0.0, 100.0]), json!([]), json!([0, 1, 2]));
        let mut harness = Harness::new(group, None);
        harness.frame(Vec::new());
        harness.tools.start_edit_mesh(5);
        let output = settle(&mut harness);
        expect_labels(
            &output,
            &[
                "Edit Mesh:", "Vertex Tool", "Edge Tool", "Grid Vertex Tool", "Lasso Selection", "Edge Cutter",
                "Auto connect vertices", "Flip Horizontally", "Triangulate vertices", "Apply", "Cancel",
            ],
        );
        assert_named_controls(&output);
    }

    #[test]
    fn the_bezier_deform_settings_name_the_shift_checkbox() {
        let session = path_session(json!([0.0, 0.0, 100.0, 0.0, 200.0, 0.0]), "Spline");
        let mut harness = Harness::new(session, Some((17, KEY)));
        let output = settle(&mut harness);
        expect_labels(&output, &["Deform:", "Vertex Tool", "Path Deform Tool"]);
        harness.tools.set_tool(Tool::Bezier);
        let output = settle(&mut harness);
        expect_labels(&output, &["Set rotation center", "Move points along the path"]);
        assert_named_controls(&output);
    }

    fn click_label(harness: &mut Harness, label: &str) {
        let output = harness.frame(Vec::new());
        let update = output.platform_output.accesskit_update.clone().unwrap();
        let bounds = update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some(label) && node.role() == Role::Button)
            .map(|(_, node)| node.bounds().unwrap())
            .unwrap_or_else(|| panic!("no button named {label:?}"));
        let center = Pos2::new(((bounds.x0 + bounds.x1) / 2.0) as f32, ((bounds.y0 + bounds.y1) / 2.0) as f32);
        harness.frame(vec![egui::Event::PointerMoved(center), button(center, true), button(center, false)]);
    }

    #[test]
    fn a_path_edited_with_the_mouse_is_applied_by_the_apply_button_as_one_undo_step() {
        let mut harness = Harness::new(path_session(json!([0.0, 0.0, 100.0, 0.0, 200.0, 0.0]), "Spline"), None);
        harness.tools.start_edit_mesh(5);
        settle(&mut harness);
        harness.tools.set_tool(Tool::Bezier);
        settle(&mut harness);
        // Two quick clicks on the curve make a double click.
        let at = harness.screen([150.0, 2.0]);
        harness.frame(vec![egui::Event::PointerMoved(at), button(at, true), button(at, false)]);
        harness.frame(vec![button(at, true), button(at, false)]);
        assert_eq!(harness.tools.pending_edits().len(), 1, "{:?}", harness.tools.pending_edits());
        assert_eq!(harness.tools.pending_edits()[0]["op"], "insert");
        let before = revision(&harness.session);
        click_label(&mut harness, "Apply");
        assert_eq!(harness.requests.len(), 1, "{:?}", harness.requests);
        assert_eq!(harness.requests[0]["op"], "deformer.define_vertices");
        assert_eq!(revision(&harness.session), before + 1);
        assert_eq!(node_of(&harness.session)["vertices"].as_array().unwrap().len(), 8);
        assert!(harness.tools.mode().is_none());
    }
}
