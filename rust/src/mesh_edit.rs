//! Document effect of the mesh-editor tools.
//!
//! Ports `viewport/common/mesh.d` (`IncMesh`), `core/math/mesh.d` (`applyMeshToTarget`), the tools in
//! `viewport/common/mesheditor/tools/*.d`, `viewport/vertex/mesheditor/tools/edgecutter.d`,
//! `viewport/common/spline.d` and the brushes. D tools react to mouse frames; here each tool is the
//! pure function of the final gesture: a list of vertex-definition edits (`mesh.edit`) or a deform
//! gesture over the armed keypoint (`deform.edit`).
//!
//! Vertex-definition mode edits an `IncMesh`: vertices with connections. Triangles are re-derived from
//! the connection graph on every apply, exactly like `IncMesh.mExport`, then wound counter-clockwise
//! and given UVs by `Part.normalizeUV`. Deform mode edits `rest + keypoint offset` and writes
//! `position - rest` back through the same `set_deform` path as `binding.set_deform`.

use crate::deformers::{self, PartMesh};
use crate::document::TextureBlob;
use crate::geometry::{self, MeshVertex, Vec2};
use crate::nodes::{find_node, find_node_mut};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashSet};
use std::io::Cursor;

/// D stores triangle indices as `ushort`.
pub const MAX_VERTICES: usize = u16::MAX as usize;
/// `meshEditAOE`: snap distance of a new vertex to a mirror axis.
const MIRROR_AOE: f64 = 4.0;
/// Same output ceiling as `binding.set_deform`.
const MAX_BINDING_VALUES: usize = 1_000_000;
const NO_TEXTURE: u64 = u32::MAX as u64;

fn check(point: Vec2, what: &str) -> Result<(), String> {
    if point[0].is_finite() && point[1].is_finite() {
        Ok(())
    } else {
        Err(format!("{what} must be finite"))
    }
}

fn add(a: Vec2, b: Vec2) -> Vec2 {
    [a[0] + b[0], a[1] + b[1]]
}

fn sub(a: Vec2, b: Vec2) -> Vec2 {
    [a[0] - b[0], a[1] - b[1]]
}

fn scale(a: Vec2, s: f64) -> Vec2 {
    [a[0] * s, a[1] * s]
}

// ------------------------------------------------------------------ IncMesh

/// `IncMesh`: vertices with symmetric connections, plus the `eOrigin` carried through export.
#[derive(Clone, Debug, PartialEq)]
pub struct EditMesh {
    pub verts: Vec<MeshVertex>,
    pub origin: Vec2,
}

impl EditMesh {
    /// `mImport`: every triangle connects (i0,i1), (i1,i2), (i2,i0) in that order, skipping existing
    /// edges. The connection order decides the triangle order of the export.
    pub fn from_triangles(positions: &[Vec2], indices: &[u32], origin: Vec2) -> Result<Self, String> {
        if indices.len() % 3 != 0 {
            return Err("indices must be a list of triangles".to_owned());
        }
        let mut verts: Vec<MeshVertex> = positions.iter().map(|&p| MeshVertex::new(p)).collect();
        for tri in indices.chunks_exact(3) {
            let [a, b, c] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
            if a >= verts.len() || b >= verts.len() || c >= verts.len() {
                return Err(format!("triangle index {} is past the vertex list", a.max(b).max(c)));
            }
            if a == b || b == c || c == a {
                return Err("mesh has a degenerate triangle".to_owned());
            }
            geometry::connect(&mut verts, a, b)?;
            geometry::connect(&mut verts, b, c)?;
            geometry::connect(&mut verts, c, a)?;
        }
        Ok(Self { verts, origin })
    }

    pub fn positions(&self) -> Vec<Vec2> {
        self.verts.iter().map(|v| v.position).collect()
    }
}

/// Triangles of the connection graph as `IncMesh.mExport` extracts them: a depth-first walk in vertex
/// order; every directed edge (v, conn) takes the first neighbour of `conn` that connects back to `v`
/// unless that triangle was already emitted. D's intersection filter (`isIntersectingWithTris`) reads
/// the first corner of each stored triangle three times, so it never rejects anything and is not ported:
/// overlapping triangles of a dense graph are kept, as in D.
pub fn extract_triangles(verts: &[MeshVertex]) -> Vec<[usize; 3]> {
    let mut visited = vec![false; verts.len()];
    let mut seen: HashSet<[usize; 3]> = HashSet::new();
    let mut triangles = Vec::new();
    let mut emit = |v: usize, triangles: &mut Vec<[usize; 3]>| {
        for &conn in &verts[v].connections {
            if conn == v {
                continue;
            }
            for &conn2 in &verts[conn].connections {
                if !verts[conn2].connections.contains(&v) {
                    continue;
                }
                let mut key = [v, conn, conn2];
                key.sort_unstable();
                if !seen.insert(key) {
                    continue;
                }
                triangles.push([v, conn, conn2]);
                break;
            }
        }
    };
    for root in 0..verts.len() {
        if visited[root] {
            continue;
        }
        visited[root] = true;
        emit(root, &mut triangles);
        let mut stack = vec![(root, 0usize)];
        while let Some((v, next)) = stack.last().copied() {
            let Some(&conn) = verts[v].connections.get(next) else {
                stack.pop();
                continue;
            };
            if let Some(top) = stack.last_mut() {
                top.1 += 1;
            }
            if !visited[conn] {
                visited[conn] = true;
                emit(conn, &mut triangles);
                stack.push((conn, 0));
            }
        }
    }
    triangles
}

/// `MeshData.fixWinding`: clockwise triangles swap their last two corners.
fn fix_winding(positions: &[Vec2], triangles: &[[usize; 3]]) -> Vec<u32> {
    let mut indices = Vec::with_capacity(triangles.len() * 3);
    for &[a, b, c] in triangles {
        let (va, vb, vc) = (positions[a], positions[b], positions[c]);
        let cross = (vb[0] - va[0]) * (vc[1] - va[1]) - (vb[1] - va[1]) * (vc[0] - va[0]);
        let (b, c) = if cross < 0.0 { (c, b) } else { (b, c) };
        indices.extend([a as u32, b as u32, c as u32]);
    }
    indices
}

/// `Part.normalizeUV` applied to the positions D copies into the UVs: `p / texture + 0.5`. Without a
/// usable texture `Drawable.normalizeUV` rescales the bounding box to 0..1 (a flat extent gives 0.5).
pub fn normalize_uvs(positions: &[Vec2], texture: Option<[u32; 2]>) -> Vec<Vec2> {
    if let Some([width, height]) = texture.filter(|t| t[0] != 0 && t[1] != 0) {
        return positions
            .iter()
            .map(|p| [p[0] / f64::from(width) + 0.5, p[1] / f64::from(height) + 0.5])
            .collect();
    }
    if positions.is_empty() {
        return Vec::new();
    }
    let (min, max) = positions
        .iter()
        .fold(([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]), |(min, max), p| {
            (
                [min[0].min(p[0]), min[1].min(p[1])],
                [max[0].max(p[0]), max[1].max(p[1])],
            )
        });
    let (width, height) = (max[0] - min[0], max[1] - min[1]);
    if width == 0.0 || height == 0.0 {
        return vec![[0.5, 0.5]; positions.len()];
    }
    let center = [(min[0] + max[0]) / 2.0 / width, (min[1] + max[1]) / 2.0 / height];
    positions
        .iter()
        .map(|p| [p[0] / width + 0.5 - center[0], p[1] / height + 0.5 - center[1]])
        .collect()
}

/// `isGrid`: every distinct y holds as many vertices as there are distinct x and the reverse. Returns
/// `[ys, xs]` sorted ascending (`gridAxes[0]` is Y). Needs at least four vertices.
pub fn grid_axes(points: &[Vec2]) -> Option<[Vec<f64>; 2]> {
    if points.len() < 4 {
        return None;
    }
    let count = |axis: usize| {
        let mut counts: Vec<(f64, usize)> = Vec::new();
        let mut sorted: Vec<f64> = points.iter().map(|p| p[axis] + 0.0).collect();
        sorted.sort_by(f64::total_cmp);
        for value in sorted {
            match counts.last_mut() {
                Some(last) if last.0 == value => last.1 += 1,
                _ => counts.push((value, 1)),
            }
        }
        counts
    };
    let (ys, xs) = (count(1), count(0));
    if ys.iter().any(|&(_, n)| n != xs.len()) || xs.iter().any(|&(_, n)| n != ys.len()) {
        return None;
    }
    Some([ys.iter().map(|c| c.0).collect(), xs.iter().map(|c| c.0).collect()])
}

/// What `applyToTarget` leaves on the Part.
#[derive(Clone, Debug, PartialEq)]
pub struct BuiltMesh {
    pub vertices: Vec<Vec2>,
    pub uvs: Vec<Vec2>,
    pub indices: Vec<u32>,
    pub origin: Vec2,
    pub grid_axes: Option<[Vec<f64>; 2]>,
    /// The grid sort moved vertices, so old vertex indices no longer name the same vertices.
    pub permuted: bool,
}

/// `IncMeshEditorOneFor!(Drawable, VertexEdit).applyToTarget` plus `applyMeshToTarget`'s export: a grid
/// is sorted by (y, x) first (connections follow their vertices), then triangles, winding and UVs are
/// rebuilt from the connection graph. Fails when the vertices do not fit 16-bit indices.
pub fn build_mesh(mesh: &EditMesh, texture: Option<[u32; 2]>) -> Result<BuiltMesh, String> {
    if mesh.verts.len() > MAX_VERTICES {
        return Err(format!("a Part mesh holds at most {MAX_VERTICES} vertices"));
    }
    for vertex in &mesh.verts {
        check(vertex.position, "vertex position")?;
    }
    check(mesh.origin, "mesh origin")?;
    let axes = grid_axes(&mesh.positions());
    let mut verts = mesh.verts.clone();
    let mut permuted = false;
    if axes.is_some() {
        let mut order: Vec<usize> = (0..verts.len()).collect();
        order.sort_by(|&a, &b| {
            let (pa, pb) = (verts[a].position, verts[b].position);
            pa[1].total_cmp(&pb[1]).then(pa[0].total_cmp(&pb[0]))
        });
        let mut new_index = vec![0usize; order.len()];
        for (new, &old) in order.iter().enumerate() {
            new_index[old] = new;
        }
        permuted = order.iter().enumerate().any(|(new, &old)| new != old);
        verts = order
            .iter()
            .map(|&old| {
                let mut vertex = mesh.verts[old].clone();
                vertex.connections = vertex.connections.iter().map(|&c| new_index[c]).collect();
                vertex
            })
            .collect();
    }
    let positions: Vec<Vec2> = verts.iter().map(|v| v.position).collect();
    let triangles = extract_triangles(&verts);
    Ok(BuiltMesh {
        indices: fix_winding(&positions, &triangles),
        uvs: normalize_uvs(&positions, texture),
        vertices: positions,
        origin: mesh.origin,
        grid_axes: axes,
        permuted,
    })
}

// -------------------------------------------------------------- mirror mode

/// The editor's mirror mode (`mirrorHoriz`, `mirrorVert`, `mirrorOrigin`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mirror {
    pub horizontal: bool,
    pub vertical: bool,
    pub origin: Vec2,
}

impl Mirror {
    /// `foreachMirror` order: horizontal, vertical, both, then the identity axis 0 last.
    fn axes(&self) -> Vec<u8> {
        let mut axes = Vec::new();
        if self.horizontal {
            axes.push(1);
        }
        if self.vertical {
            axes.push(2);
        }
        if self.horizontal && self.vertical {
            axes.push(3);
        }
        axes.push(0);
        axes
    }

    /// `mirrorH/V/HV`; like D, `mirrorV` takes the Y term from `origin.x`.
    fn point(&self, axis: u8, p: Vec2) -> Vec2 {
        match axis {
            1 => [2.0 * self.origin[0] - p[0], p[1]],
            2 => [p[0], 2.0 * self.origin[0] - p[1]],
            3 => [2.0 * self.origin[0] - p[0], 2.0 * self.origin[1] - p[1]],
            _ => p,
        }
    }

    fn delta(&self, axis: u8, d: Vec2) -> Vec2 {
        match axis {
            1 => [-d[0], d[1]],
            2 => [d[0], -d[1]],
            3 => [-d[0], -d[1]],
            _ => d,
        }
    }

    fn near_axis(&self, v: f64) -> bool {
        v > -MIRROR_AOE && v < MIRROR_AOE
    }

    fn on_mirror(&self, p: Vec2) -> bool {
        (self.vertical && self.near_axis(p[1])) || (self.horizontal && self.near_axis(p[0]))
    }

    fn on_center(&self, p: Vec2) -> bool {
        (self.vertical && self.near_axis(p[1])) && (self.horizontal && self.near_axis(p[0]))
    }

    /// `mirrorVertex`: the vertex under the mirrored position, `None` when it is the vertex itself.
    fn vertex(&self, positions: &[Vec2], axis: u8, index: usize, zoom: f64) -> Result<Option<usize>, String> {
        if axis == 0 {
            return Ok(Some(index));
        }
        let at = self.point(axis, positions[index]);
        Ok(geometry::get_vertex_from_point(positions, at, zoom)?.filter(|&found| found != index))
    }

    /// `updateMirrorSelected`: a selection never lists both a vertex and its mirror partner.
    fn normalize_selection(&self, positions: &[Vec2], selected: &[usize], zoom: f64) -> Result<Vec<usize>, String> {
        if !self.horizontal && !self.vertical {
            return Ok(selected.to_vec());
        }
        let (mut kept, mut partners): (Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new());
        for &v in selected {
            if partners.contains(&v) {
                continue;
            }
            kept.push(v);
            for axis in self.axes() {
                let Some(v2) = self.vertex(positions, axis, v, zoom)? else {
                    continue;
                };
                if axis != 0 && !kept.contains(&v2) && !partners.contains(&v2) {
                    partners.push(v2);
                }
            }
        }
        Ok(kept)
    }
}

// --------------------------------------------------------------------- edits

/// One vertex-definition edit of `mesh.edit`.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// Point tool, Ctrl+click on empty space. `connect` links the new vertex to an existing one (the
    /// Point tool's auto-connect link to the previously added vertex).
    Add {
        pos: Vec2,
        connect: Option<usize>,
    },
    /// Point tool, Delete key: the listed vertices and their mirror partners.
    Remove {
        indices: Vec<usize>,
    },
    /// Point tool, double click: the vertex under `pos` (and under its mirror positions).
    RemoveAt {
        pos: Vec2,
    },
    /// Point tool drag or arrow keys: translate vertices.
    Move {
        indices: Vec<usize>,
        delta: Vec2,
    },
    Connect {
        a: usize,
        b: usize,
    },
    Disconnect {
        a: usize,
        b: usize,
    },
    /// Edge tool click: connect two vertices, or disconnect them when already connected.
    Toggle {
        a: usize,
        b: usize,
    },
    /// Edge cutter drag: disconnect every edge crossed by the segment.
    Cut {
        from: Vec2,
        to: Vec2,
    },
    /// Grid tool drag-create: `bounds = [x0, y0, x1, y1]` in any corner order.
    GridCreate {
        bounds: [f64; 4],
        cuts: usize,
    },
    /// Grid from explicit axes (`gridAxes`: Y list, X list).
    GridAxes {
        ys: Vec<f64>,
        xs: Vec<f64>,
    },
    /// Grid tool double click on empty space.
    GridAddAxis {
        pos: Vec2,
    },
    /// Grid tool double click on a vertex.
    GridRemoveAxis {
        vertex: usize,
    },
    /// Grid tool drag of a vertex.
    GridMove {
        vertex: usize,
        to: Vec2,
    },
    /// `autoTriangulate` + `applyPreview`: replace the connections by a Delaunay triangulation.
    Triangulate,
    /// `IncMesh.flipHorz/flipVert`.
    Flip {
        vertical: bool,
    },
    /// Cubism "Reset mesh": the quad an imported layer gets, over the Part's texture, with the origin at (0, 0).
    Reset,
    /// Merge vertices no farther apart than `distance` (0: only coincident ones) into the lowest-numbered one,
    /// which takes over their edges.
    Merge {
        distance: f64,
    },
    /// Path-deform tool applied to vertex positions.
    PathDeform {
        path: Vec<Vec2>,
        target: Vec<Vec2>,
        indices: Vec<usize>,
        resolution: usize,
    },
}

/// Per-request editor settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditOptions {
    pub mirror: Mirror,
    /// Viewport zoom: the select radius is 16 / zoom.
    pub zoom: f64,
    /// Point tool auto-connect: every `Add` without `connect` links to the previously added vertex.
    pub auto_connect: bool,
    /// Pixel size of the Part's texture, for `Reset`.
    pub texture: Option<[u32; 2]>,
}

impl Default for EditOptions {
    fn default() -> Self {
        Self {
            mirror: Mirror::default(),
            zoom: 1.0,
            auto_connect: false,
            texture: None,
        }
    }
}

fn need_vertex(mesh: &EditMesh, index: usize) -> Result<(), String> {
    if index < mesh.verts.len() {
        Ok(())
    } else {
        Err(format!("vertex index {index} is past the vertex list"))
    }
}

fn need_indices(len: usize, indices: &[usize]) -> Result<(), String> {
    match indices.iter().find(|&&i| i >= len) {
        Some(bad) => Err(format!("vertex index {bad} is past the vertex list")),
        None => Ok(()),
    }
}

/// Deletes vertices and remaps the connections of the survivors, as repeated `IncMesh.remove`.
fn remove_vertices(verts: &mut Vec<MeshVertex>, doomed: &BTreeSet<usize>) {
    let mut new_index = vec![None; verts.len()];
    let mut kept = 0;
    for (old, slot) in new_index.iter_mut().enumerate() {
        if !doomed.contains(&old) {
            *slot = Some(kept);
            kept += 1;
        }
    }
    let old = std::mem::take(verts);
    for (index, mut vertex) in old.into_iter().enumerate() {
        if new_index[index].is_none() {
            continue;
        }
        vertex.connections = vertex.connections.iter().filter_map(|&c| new_index[c]).collect();
        verts.push(vertex);
    }
}

/// `areLineSegmentsIntersecting` (`soul2d_live/math`): crossings strictly inside both segments.
pub fn segments_intersect(p1: Vec2, p2: Vec2, p3: Vec2, p4: Vec2) -> bool {
    const EPSILON: f64 = 0.00001;
    let denominator = (p4[1] - p3[1]) * (p2[0] - p1[0]) - (p4[0] - p3[0]) * (p2[1] - p1[1]);
    if denominator == 0.0 {
        return false;
    }
    let ua = ((p4[0] - p3[0]) * (p1[1] - p3[1]) - (p4[1] - p3[1]) * (p1[0] - p3[0])) / denominator;
    let ub = ((p2[0] - p1[0]) * (p1[1] - p3[1]) - (p2[1] - p1[1]) * (p1[0] - p3[0])) / denominator;
    ua > EPSILON && ua < 1.0 - EPSILON && ub > EPSILON && ub < 1.0 - EPSILON
}

/// `MeshData.regenerateGrid` + `IncMesh.copyFromMeshData`: vertices row by row (Y outer), the cells split
/// in D's X pattern, connections taken from those triangles. The origin resets to (0, 0) because the
/// fresh `MeshData` carries none. D's width/height swap only touches the UVs, which export recomputes.
fn regenerate_grid(ys: &[f64], xs: &[f64]) -> Result<EditMesh, String> {
    if ys.len() < 2 || xs.len() < 2 {
        return Err("a grid needs at least two values on each axis".to_owned());
    }
    for (axis, name) in [(ys, "Y"), (xs, "X")] {
        if axis.iter().any(|v| !v.is_finite()) {
            return Err(format!("grid {name} axis must be finite"));
        }
        if axis.windows(2).any(|w| w[0] >= w[1]) {
            return Err(format!("grid {name} axis must be strictly increasing"));
        }
    }
    let cols = xs.len();
    if ys.len().checked_mul(cols).map_or(true, |n| n > MAX_VERTICES) {
        return Err(format!("a grid may hold at most {MAX_VERTICES} vertices"));
    }
    let positions: Vec<Vec2> = ys.iter().flat_map(|&y| xs.iter().map(move |&x| [x, y])).collect();
    let (min_x, min_y) = (xs[0], ys[0]);
    let width = ys[ys.len() - 1] - min_y;
    let height = xs[xs.len() - 1] - min_x;
    let center = [min_x + width / 2.0, min_y + height / 2.0];
    let m = |x: usize, y: usize| (y * cols + x) as u32;
    let mut indices = Vec::new();
    for (i, &y) in ys.iter().enumerate().take(ys.len() - 1) {
        for (j, &x) in xs.iter().enumerate().take(cols - 1) {
            let (i0, i1, i2, i3) = (m(j, i), m(j, i + 1), m(j + 1, i), m(j + 1, i + 1));
            if (x < center[0] && y < center[1]) || (x >= center[0] && y >= center[1]) {
                indices.extend([i0, i2, i3, i0, i3, i1]);
            } else {
                indices.extend([i0, i1, i2, i1, i2, i3]);
            }
        }
    }
    EditMesh::from_triangles(&positions, &indices, [0.0, 0.0])
}

fn position_of(axis: &[f64], value: f64) -> Option<usize> {
    axis.iter().position(|&v| v == value)
}

/// Mutable editing session over one `EditMesh`.
struct Editor<'a> {
    mesh: &'a mut EditMesh,
    options: EditOptions,
    /// Position of the vertex added by the previous `Add` (D finds it again by position).
    last_added: Option<Vec2>,
    /// Cleared by edits that renumber existing vertices (removal, grid rebuild).
    stable: bool,
}

impl Editor<'_> {
    fn positions(&self) -> Vec<Vec2> {
        self.mesh.positions()
    }

    fn partner(&self, axis: u8, index: usize) -> Result<Option<usize>, String> {
        if axis == 0 {
            return Ok(Some(index));
        }
        self.options
            .mirror
            .vertex(&self.positions(), axis, index, self.options.zoom)
    }

    fn selection(&self, indices: &[usize]) -> Result<Vec<usize>, String> {
        need_indices(self.mesh.verts.len(), indices)?;
        let mirror = self.options.mirror;
        if !mirror.horizontal && !mirror.vertical {
            return Ok(indices.to_vec());
        }
        mirror.normalize_selection(&self.positions(), indices, self.options.zoom)
    }

    fn apply(&mut self, edit: &Edit) -> Result<(), String> {
        if matches!(
            edit,
            Edit::Remove { .. }
                | Edit::RemoveAt { .. }
                | Edit::GridCreate { .. }
                | Edit::GridAxes { .. }
                | Edit::GridAddAxis { .. }
                | Edit::GridRemoveAxis { .. }
                | Edit::GridMove { .. }
                | Edit::Reset
        ) {
            self.stable = false;
        }
        match edit {
            Edit::Add { pos, connect } => self.add(*pos, *connect),
            Edit::Remove { indices } => {
                let selected = self.selection(indices)?;
                let mut doomed = BTreeSet::new();
                for axis in self.options.mirror.axes() {
                    for &v in &selected {
                        if let Some(found) = self.partner(axis, v)? {
                            doomed.insert(found);
                        }
                    }
                }
                remove_vertices(&mut self.mesh.verts, &doomed);
                Ok(())
            }
            Edit::RemoveAt { pos } => {
                check(*pos, "position")?;
                let positions = self.positions();
                if geometry::get_vertex_from_point(&positions, *pos, self.options.zoom)?.is_none() {
                    return Err("no vertex under the point".to_owned());
                }
                let mut doomed = BTreeSet::new();
                for axis in self.options.mirror.axes() {
                    let at = self.options.mirror.point(axis, *pos);
                    if let Some(found) = geometry::get_vertex_from_point(&positions, at, self.options.zoom)? {
                        doomed.insert(found);
                    }
                }
                remove_vertices(&mut self.mesh.verts, &doomed);
                Ok(())
            }
            Edit::Move { indices, delta } => {
                check(*delta, "delta")?;
                let selected = self.selection(indices)?;
                for select in selected {
                    for axis in self.options.mirror.axes() {
                        if let Some(found) = self.partner(axis, select)? {
                            let moved = add(self.mesh.verts[found].position, self.options.mirror.delta(axis, *delta));
                            self.mesh.verts[found].position = moved;
                        }
                    }
                }
                Ok(())
            }
            Edit::Connect { a, b } => self.link(*a, *b, true),
            Edit::Disconnect { a, b } => self.link(*a, *b, false),
            Edit::Toggle { a, b } => {
                need_vertex(self.mesh, *a)?;
                need_vertex(self.mesh, *b)?;
                let connected = geometry::is_connected_to(&self.mesh.verts, *a, *b)?;
                self.link(*a, *b, !connected)
            }
            Edit::Cut { from, to } => {
                check(*from, "cut start")?;
                check(*to, "cut end")?;
                if from == to {
                    return Err("the cut needs two different points".to_owned());
                }
                let mut edges = Vec::new();
                for (v, vertex) in self.mesh.verts.iter().enumerate() {
                    for &conn in &vertex.connections {
                        if conn > v && segments_intersect(*from, *to, vertex.position, self.mesh.verts[conn].position) {
                            edges.push((v, conn));
                        }
                    }
                }
                for (a, b) in edges {
                    geometry::disconnect(&mut self.mesh.verts, a, b)?;
                }
                Ok(())
            }
            Edit::GridCreate { bounds, cuts } => {
                if bounds.iter().any(|v| !v.is_finite()) {
                    return Err("grid bounds must be finite".to_owned());
                }
                if *cuts < 2 {
                    return Err("a grid needs at least two cuts".to_owned());
                }
                let (x0, x1) = (bounds[0].min(bounds[2]), bounds[0].max(bounds[2]));
                let (y0, y1) = (bounds[1].min(bounds[3]), bounds[1].max(bounds[3]));
                let steps = (*cuts - 1) as f64;
                let ys: Vec<f64> = (0..*cuts).map(|i| y0 + (y1 - y0) * i as f64 / steps).collect();
                let xs: Vec<f64> = (0..*cuts).map(|i| x0 + (x1 - x0) * i as f64 / steps).collect();
                *self.mesh = regenerate_grid(&ys, &xs)?;
                Ok(())
            }
            Edit::GridAxes { ys, xs } => {
                *self.mesh = regenerate_grid(ys, xs)?;
                Ok(())
            }
            Edit::GridAddAxis { pos } => {
                check(*pos, "position")?;
                let [mut ys, mut xs] = self.current_axes()?;
                let threshold = geometry::SELECT_RADIUS / self.options.zoom;
                let near = |axis: &[f64], v: f64| axis.iter().any(|&a| (v - a).abs() < threshold);
                let within = |axis: &[f64], v: f64| v >= axis[0] - threshold && v <= axis[axis.len() - 1] + threshold;
                let found_y = within(&xs, pos[0]) && near(&ys, pos[1]);
                let found_x = within(&ys, pos[1]) && near(&xs, pos[0]);
                // D inserts before the first larger value, so a value past the last one is dropped.
                if !found_y {
                    if let Some(at) = ys.iter().position(|&v| pos[1] < v) {
                        ys.insert(at, pos[1]);
                    }
                }
                if !found_x {
                    if let Some(at) = xs.iter().position(|&v| pos[0] < v) {
                        xs.insert(at, pos[0]);
                    }
                }
                *self.mesh = regenerate_grid(&ys, &xs)?;
                Ok(())
            }
            Edit::GridRemoveAxis { vertex } => {
                need_vertex(self.mesh, *vertex)?;
                let [mut ys, mut xs] = self.current_axes()?;
                let at = self.mesh.verts[*vertex].position;
                let (Some(yi), Some(xi)) = (position_of(&ys, at[1]), position_of(&xs, at[0])) else {
                    return Err("the vertex is not on the grid".to_owned());
                };
                let (x_edge, y_edge) = (xi == 0 || xi == xs.len() - 1, yi == 0 || yi == ys.len() - 1);
                if x_edge && y_edge {
                    return Ok(());
                } else if x_edge {
                    ys.remove(yi);
                } else if y_edge {
                    xs.remove(xi);
                } else {
                    ys.remove(yi);
                    xs.remove(xi);
                }
                *self.mesh = regenerate_grid(&ys, &xs)?;
                Ok(())
            }
            Edit::GridMove { vertex, to } => {
                need_vertex(self.mesh, *vertex)?;
                check(*to, "target position")?;
                let [mut ys, mut xs] = self.current_axes()?;
                let origin = self.mesh.verts[*vertex].position;
                let threshold = geometry::SELECT_RADIUS / self.options.zoom;
                // `isOnEdge`: the vertex sits on the first or last line of the other axis.
                let on_edge = |along: f64, lines: &[f64], across: f64, span: &[f64]| {
                    across >= span[0] - threshold
                        && across <= span[span.len() - 1] + threshold
                        && ((along - lines[0]).abs() < threshold || (along - lines[lines.len() - 1]).abs() < threshold)
                };
                let found_y = on_edge(origin[1], &ys, origin[0], &xs);
                let found_x = on_edge(origin[0], &xs, origin[1], &ys);
                let xi = position_of(&xs, origin[0]);
                let yi = position_of(&ys, origin[1]);
                let (move_x, move_y) = match (found_y, found_x) {
                    (true, false) => (true, false),
                    (false, true) => (false, true),
                    _ => (true, true),
                };
                if move_y {
                    ys[yi.ok_or("the vertex is not on the grid")?] = to[1];
                    ys.sort_by(f64::total_cmp);
                }
                if move_x {
                    xs[xi.ok_or("the vertex is not on the grid")?] = to[0];
                    xs.sort_by(f64::total_cmp);
                }
                *self.mesh = regenerate_grid(&ys, &xs)?;
                Ok(())
            }
            Edit::Triangulate => {
                // A triangulation cannot hold two points at one place; Cubism merges overlapping vertices.
                let positions = self.positions();
                let mut seen = HashSet::new();
                // `+ 0.0` makes -0.0 and 0.0 one key, as they are one point.
                if !positions.iter().all(|p| seen.insert([(p[0] + 0.0).to_bits(), (p[1] + 0.0).to_bits()])) {
                    self.apply(&Edit::Merge { distance: 0.0 })?;
                }
                let positions = self.positions();
                if positions.len() < 3 {
                    return Ok(());
                }
                let (min, max) = geometry::get_min_max(&positions)?;
                let triangles = geometry::triangulate(&positions, [min[0], min[1], max[0], max[1]])?;
                let flat: Vec<u32> = triangles.iter().flatten().copied().collect();
                self.mesh.verts = EditMesh::from_triangles(&positions, &flat, self.mesh.origin)?.verts;
                Ok(())
            }
            Edit::Reset => {
                let [width, height] = self.options.texture.ok_or("the Part has no texture to reset its mesh to")?;
                // The import's integer halves (`quad_half` without `exact`).
                let [hw, hh] = [f64::from(width / 2), f64::from(height / 2)];
                let quad = [[-hw, -hh], [-hw, hh], [hw, -hh], [hw, hh]];
                *self.mesh = EditMesh::from_triangles(&quad, &[0, 1, 2, 2, 1, 3], [0.0, 0.0])?;
                Ok(())
            }
            Edit::Merge { distance } => {
                // ponytail: O(n^2) pair scan, bounded by MAX_VERTICES; a spatial grid if it gets slow.
                let positions = self.positions();
                let mut keeper: Vec<usize> = (0..positions.len()).collect();
                for i in 0..positions.len() {
                    if keeper[i] != i {
                        continue;
                    }
                    for j in i + 1..positions.len() {
                        let [dx, dy] = sub(positions[j], positions[i]);
                        if keeper[j] == j && dx.hypot(dy) <= *distance {
                            keeper[j] = i;
                        }
                    }
                }
                let mut edges: Vec<Vec<usize>> = vec![Vec::new(); positions.len()];
                for (v, vertex) in self.mesh.verts.iter().enumerate() {
                    for &c in &vertex.connections {
                        let (from, to) = (keeper[v], keeper[c]);
                        if from != to && !edges[from].contains(&to) {
                            edges[from].push(to);
                        }
                    }
                }
                for (vertex, edges) in self.mesh.verts.iter_mut().zip(edges) {
                    vertex.connections = edges;
                }
                let doomed: BTreeSet<usize> = (0..positions.len()).filter(|&v| keeper[v] != v).collect();
                // Nothing merged keeps vertex i as vertex i.
                self.stable &= doomed.is_empty();
                remove_vertices(&mut self.mesh.verts, &doomed);
                Ok(())
            }
            Edit::Flip { vertical } => {
                let axis = usize::from(*vertical);
                for vertex in &mut self.mesh.verts {
                    vertex.position[axis] *= -1.0;
                }
                Ok(())
            }
            Edit::PathDeform {
                path,
                target,
                indices,
                resolution,
            } => {
                need_indices(self.mesh.verts.len(), indices)?;
                let mut positions = self.positions();
                path_deform(&mut positions, indices, path, target, *resolution)?;
                for (vertex, position) in self.mesh.verts.iter_mut().zip(positions) {
                    vertex.position = position;
                }
                Ok(())
            }
        }
    }

    fn current_axes(&self) -> Result<[Vec<f64>; 2], String> {
        grid_axes(&self.positions()).ok_or_else(|| "the mesh is not a grid".to_owned())
    }

    /// Connect or disconnect `a`-`b` and every mirrored pair.
    fn link(&mut self, a: usize, b: usize, connect: bool) -> Result<(), String> {
        need_vertex(self.mesh, a)?;
        need_vertex(self.mesh, b)?;
        if a == b {
            return Err("a vertex cannot connect to itself".to_owned());
        }
        for axis in self.options.mirror.axes() {
            let (Some(ma), Some(mb)) = (self.partner(axis, a)?, self.partner(axis, b)?) else {
                continue;
            };
            if ma == mb {
                continue;
            }
            if connect {
                geometry::connect(&mut self.mesh.verts, ma, mb)?;
            } else {
                geometry::disconnect(&mut self.mesh.verts, ma, mb)?;
            }
        }
        Ok(())
    }

    fn add(&mut self, pos: Vec2, connect: Option<usize>) -> Result<(), String> {
        check(pos, "position")?;
        if let Some(index) = connect {
            need_vertex(self.mesh, index)?;
        }
        let mirror = self.options.mirror;
        if mirror.on_mirror(pos) {
            // `placeOnMirror` adds one vertex on the axis and records no auto-connect partner.
            let mut placed = pos;
            if mirror.horizontal && mirror.vertical && mirror.on_center(pos) {
                placed = [0.0, 0.0];
            } else if mirror.vertical {
                placed[1] = 0.0;
            } else if mirror.horizontal {
                placed[0] = 0.0;
            }
            self.mesh.verts.push(MeshVertex::new(placed));
            return Ok(());
        }
        let prev = match connect {
            Some(index) => Some(index),
            None if self.options.auto_connect => match self.last_added {
                Some(at) => geometry::get_vertex_from_point(&self.positions(), at, self.options.zoom)?,
                None => None,
            },
            None => None,
        };
        // Mirrored copies first, the vertex under the cursor last; the last one is the new vertex.
        for axis in mirror.axes() {
            self.mesh.verts.push(MeshVertex::new(mirror.point(axis, pos)));
        }
        let created = self.mesh.verts.len() - 1;
        if let Some(prev) = prev {
            self.link(prev, created, true)?;
        }
        self.last_added = Some(pos);
        Ok(())
    }
}

/// Applies `edits` in order to `mesh`; the caller owns a copy, so an error leaves nothing half done.
/// Returns whether every vertex that existed before still has its index (no removal or grid rebuild).
pub fn apply_edits(mesh: &mut EditMesh, edits: &[Edit], options: &EditOptions) -> Result<bool, String> {
    if !options.zoom.is_finite() || options.zoom <= 0.0 {
        return Err("zoom must be finite and positive".to_owned());
    }
    let mut editor = Editor {
        mesh,
        options: *options,
        last_added: None,
        stable: true,
    };
    for (index, edit) in edits.iter().enumerate() {
        editor.apply(edit).map_err(|error| format!("edit {index}: {error}"))?;
    }
    Ok(editor.stable)
}

// ---------------------------------------------------------------- selection

/// How a new selection combines with the current one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectMode {
    Replace,
    Add,
    Remove,
    Toggle,
}

/// Rectangle select (`getInRect`): corners in either order, edges inclusive.
pub fn select_rect(positions: &[Vec2], a: Vec2, b: Vec2) -> Result<Vec<usize>, String> {
    geometry::get_in_rect(positions, a, b)
}

/// Lasso select. `points` are the clicked lasso points; like `LassoTool.doSelectionTrigger` the last one
/// is replaced by the first to close the polygon, and each segment becomes an endpoint pair for
/// `pointInPolygon`. Needs three points.
pub fn select_lasso(positions: &[Vec2], points: &[Vec2]) -> Result<Vec<usize>, String> {
    if points.len() < 3 {
        return Err("a lasso needs at least three points".to_owned());
    }
    let mut ring = points.to_vec();
    let last = ring.len() - 1;
    ring[last] = ring[0];
    let edges: Vec<Vec2> = ring.windows(2).flat_map(|pair| [pair[0], pair[1]]).collect();
    let mut hits = Vec::new();
    for (index, &position) in positions.iter().enumerate() {
        if geometry::point_in_polygon(position, &edges)? {
            hits.push(index);
        }
    }
    Ok(hits)
}

/// Combines hits with the current selection (`select`, `deselect`, `toggleSelect`, or replace).
pub fn combine_selection(current: &[usize], hits: &[usize], mode: SelectMode) -> Vec<usize> {
    let mut selected = if mode == SelectMode::Replace {
        Vec::new()
    } else {
        current.to_vec()
    };
    for &hit in hits {
        let at = selected.iter().position(|&s| s == hit);
        match (mode, at) {
            (SelectMode::Remove, Some(at)) | (SelectMode::Toggle, Some(at)) => {
                selected.remove(at);
            }
            (SelectMode::Remove, None) => {}
            (_, None) => selected.push(hit),
            (_, Some(_)) => {}
        }
    }
    selected
}

// ------------------------------------------------------------------ brushes

/// Deform brush footprints in node-local coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Brush {
    Line {
        radius: f64,
        angle: f64,
    },
    Circle {
        radius: f64,
    },
    DoubleThreshold {
        radius: f64,
        inner: f64,
    },
    /// `width` and `height` are the full extents D falls off over (not half extents); the weight
    /// reaches zero at `width` / `height` from the centre.
    Rectangle {
        width: f64,
        height: f64,
    },
}

impl Brush {
    pub fn line(radius: f64, angle: f64) -> Result<Self, String> {
        finite_positive(radius, "brush radius")?;
        if !angle.is_finite() {
            return Err("brush angle must be finite".to_owned());
        }
        Ok(Self::Line { radius, angle })
    }

    pub fn circle(radius: f64) -> Result<Self, String> {
        finite_positive(radius, "brush radius")?;
        Ok(Self::Circle { radius })
    }

    /// The constructor clamps like D: `radius = max(1, radius)`, `inner = min(inner, radius)`.
    pub fn double_threshold(radius: f64, inner: f64) -> Result<Self, String> {
        if !radius.is_finite() || !inner.is_finite() {
            return Err("brush radius and innerRadius must be finite".to_owned());
        }
        let radius = radius.max(1.0);
        Ok(Self::DoubleThreshold {
            radius,
            inner: inner.min(radius),
        })
    }

    pub fn rectangle(width: f64, height: f64) -> Result<Self, String> {
        if !width.is_finite() || !height.is_finite() {
            return Err("brush width and height must be finite".to_owned());
        }
        Ok(Self::Rectangle {
            width: width.max(1.0),
            height: height.max(1.0),
        })
    }

    /// `weightAt(center, pos)`.
    pub fn weight_at(&self, center: Vec2, pos: Vec2) -> f64 {
        let clamp = |v: f64| v.max(0.0).min(1.0);
        let distance = (pos[0] - center[0]).hypot(pos[1] - center[1]);
        match *self {
            Self::Line { radius, angle } => {
                let (sin, cos) = angle.sin_cos();
                let distance = (-(pos[0] - center[0]) * sin + (pos[1] - center[1]) * cos).abs();
                clamp(1.0 - distance / radius)
            }
            Self::Circle { radius } => clamp(1.0 - distance / radius),
            Self::DoubleThreshold { radius, inner } => {
                if distance <= inner {
                    1.0
                } else if radius > inner {
                    clamp(1.0 - (distance - inner) / (radius - inner))
                } else {
                    0.0
                }
            }
            Self::Rectangle { width, height } => {
                let xratio = clamp(1.0 - (pos[0] - center[0]).abs() / width);
                let yratio = clamp(1.0 - (pos[1] - center[1]).abs() / height);
                xratio.min(yratio)
            }
        }
    }

    pub fn weights_at(&self, center: Vec2, positions: &[Vec2]) -> Vec<f64> {
        positions.iter().map(|&p| self.weight_at(center, p)).collect()
    }
}

fn finite_positive(value: f64, what: &str) -> Result<(), String> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(format!("{what} must be finite and positive"))
    }
}

/// Brush stroke (`BrushTool`): `stroke` are the mouse positions of one drag, the first being the press.
/// Each later position moves every selected vertex (all vertices when none is selected) by the mouse
/// step times its brush weight. Weights come from the press position unless `flow`, which recomputes
/// them at every step on the current positions. `snap_zoom` is the Shift axis lock: after the drag
/// leaves a 32 px box (`32 / zoom` in document units) the stroke sticks to its dominant axis.
pub fn brush_stroke(
    positions: &mut [Vec2],
    selected: &[usize],
    brush: &Brush,
    flow: bool,
    stroke: &[Vec2],
    snap_zoom: Option<f64>,
) -> Result<(), String> {
    if stroke.len() < 2 {
        return Err("a brush stroke needs at least two mouse positions".to_owned());
    }
    for point in stroke {
        check(*point, "stroke position")?;
    }
    if let Some(zoom) = snap_zoom {
        finite_positive(zoom, "zoom")?;
    }
    need_indices(positions.len(), selected)?;
    let targets: Vec<usize> = if selected.is_empty() {
        (0..positions.len()).collect()
    } else {
        selected.to_vec()
    };
    let init = stroke[0];
    let mut last = init;
    let mut weights = brush.weights_at(init, positions);
    let mut axis_direction = 0;
    for &raw in &stroke[1..] {
        let mut mouse = raw;
        if let Some(zoom) = snap_zoom {
            if axis_direction == 0 {
                let diff = sub(mouse, init);
                if diff[0].abs() / zoom > 32.0 {
                    axis_direction = 1;
                } else if diff[1].abs() / zoom > 32.0 {
                    axis_direction = 2;
                }
            }
            match axis_direction {
                1 => mouse[1] = init[1],
                2 => mouse[0] = init[0],
                _ => {}
            }
        }
        if flow {
            weights = brush.weights_at(mouse, positions);
        }
        let diff = sub(mouse, last);
        for &index in &targets {
            if weights[index] > 0.0 {
                positions[index] = add(positions[index], scale(diff, weights[index]));
            }
        }
        last = mouse;
    }
    Ok(())
}

// ------------------------------------------------------------ path deform

/// `CatmullSpline`: control points evaluated as Hermite ends and Catmull-Rom middles.
struct Spline {
    points: Vec<Vec2>,
    resolution: usize,
    interpolated: Vec<Vec2>,
}

impl Spline {
    fn new(points: Vec<Vec2>, resolution: usize) -> Self {
        let mut spline = Self {
            points,
            resolution,
            interpolated: Vec::new(),
        };
        if !spline.points.is_empty() {
            let samples = resolution * (spline.points.len() - 1) + 1;
            spline.interpolated = (0..samples)
                .map(|i| spline.eval(i as f64 / resolution as f64))
                .collect();
        }
        spline
    }

    fn eval(&self, off: f64) -> Vec2 {
        let n = self.points.len();
        if off <= 0.0 {
            return self.points[0];
        }
        if off >= (n - 1) as f64 {
            return self.points[n - 1];
        }
        let ioff = off as usize;
        let t = off - ioff as f64;
        let s = 1.0 - t;
        let (p1, p2) = (self.points[ioff], self.points[ioff + 1]);
        if n == 2 {
            return add(scale(p1, s), scale(p2, t));
        }
        let edge = |p0: Vec2, p1: Vec2, p2: Vec2, t: f64| {
            let (t2, t3) = (t * t, t * t * t);
            let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
            let h01 = -2.0 * t3 + 3.0 * t2;
            let h11 = t3 - t2;
            let slope = scale(sub(p2, p0), 0.5);
            add(add(scale(slope, h11), scale(p1, h01)), scale(p0, h00))
        };
        if ioff == 0 {
            return edge(p1, p2, self.points[ioff + 2], t);
        }
        if ioff == n - 2 {
            return edge(p2, p1, self.points[ioff - 1], s);
        }
        // Catmull-Rom as a Bezier segment, split at t (`BezierSegment.eval`).
        let (p0, p3) = (self.points[ioff - 1], self.points[ioff + 2]);
        let b = [
            p1,
            add(p1, scale(sub(p2, p0), 1.0 / 6.0)),
            sub(p2, scale(sub(p3, p1), 1.0 / 6.0)),
            p2,
        ];
        let mc = add(scale(b[1], s), scale(b[2], t));
        let a1 = add(scale(b[0], s), scale(b[1], t));
        let a2 = add(scale(a1, s), scale(mc, t));
        let b2 = add(scale(b[2], s), scale(b[3], t));
        let b1 = add(scale(b2, t), scale(mc, s));
        add(scale(a2, s), scale(b1, t))
    }

    /// `findClosestPointOffset`. D computes `bestIdx - 1` in `uint`, so a point whose nearest sample is
    /// the first one gets a left bound near 1.07e8 and an offset far past the path end; callers treat
    /// that as "off the path" and leave the vertex alone. The wrapping subtraction keeps that behaviour.
    fn closest_offset(&self, point: Vec2) -> f64 {
        let n = self.points.len();
        if n < 2 {
            return 0.0;
        }
        let res = self.resolution as f64;
        let mut best = (0usize, f64::INFINITY);
        for (i, sample) in self.interpolated.iter().enumerate() {
            let dist = (sample[0] - point[0]).hypot(sample[1] - point[1]);
            if dist < best.1 {
                best = (i, dist);
            }
        }
        let best_idx = best.0 as u32;
        let dist = |p: Vec2| (point[0] - p[0]).hypot(point[1] - p[1]);
        let mut left = 0.0f64.max(f64::from(best_idx.wrapping_sub(1)) / res);
        let mut right = ((n - 1) as f64).min(f64::from(best_idx + 1) / res);
        let mut left_point = self.eval(left);
        let mut right_point = self.eval(right);
        const EPSILON: f64 = 0.0001;
        while right - left > EPSILON * 2.0 {
            let mid = (left + right) / 2.0;
            if dist(self.eval(mid - EPSILON)) < dist(self.eval(mid + EPSILON)) {
                right = mid;
                right_point = self.eval(right);
            } else {
                left = mid;
                left_point = self.eval(left);
            }
        }
        if left == 0.0 {
            return 0.0;
        }
        if right == (n - 1) as f64 {
            return right;
        }
        let (dx, dy) = (right_point[0] - left_point[0], right_point[1] - left_point[1]);
        let t = ((point[0] - left_point[0]) * dx + (point[1] - left_point[1]) * dy) / (dx * dx + dy * dy);
        left * (1.0 - t) + right * t
    }
}

/// Path-deform tool (`CatmullSpline.createTarget` + `updateTarget`): every selected vertex (all when
/// `selected` is empty) is expressed along `path` (offset along the curve, distance along and across
/// the tangent) and rebuilt on `target`, the same control points after the user moved some of them.
/// Vertices whose nearest path offset is an end of the path stay where they are.
pub fn path_deform(
    positions: &mut [Vec2],
    selected: &[usize],
    path: &[Vec2],
    target: &[Vec2],
    resolution: usize,
) -> Result<(), String> {
    if path.len() < 2 {
        return Err("a path needs at least two points".to_owned());
    }
    if target.len() != path.len() {
        return Err("target must have as many points as path".to_owned());
    }
    if resolution == 0 || resolution > 1000 {
        return Err("resolution must be between 1 and 1000".to_owned());
    }
    if path.len() > 1024 {
        return Err("a path holds at most 1024 points".to_owned());
    }
    for point in path.iter().chain(target) {
        check(*point, "path point")?;
    }
    need_indices(positions.len(), selected)?;
    let (from, to) = (
        Spline::new(path.to_vec(), resolution),
        Spline::new(target.to_vec(), resolution),
    );
    const EPSILON: f64 = 0.0001;
    let last_offset = (path.len() - 1) as f64;
    let mut moved = Vec::new();
    for (index, &vertex) in positions.iter().enumerate() {
        if !selected.is_empty() && !selected.contains(&index) {
            continue;
        }
        let off = from.closest_offset(vertex);
        if !off.is_finite() || off <= 0.0 || off >= last_offset {
            continue;
        }
        let pt = from.eval(off);
        let tangent = sub(from.eval(off + EPSILON), pt);
        let length = tangent[0].hypot(tangent[1]);
        let tangent = [tangent[0] / length, tangent[1] / length];
        let rel = sub(vertex, pt);
        let along = rel[0] * tangent[0] + rel[1] * tangent[1];
        let across = rel[1] * tangent[0] - rel[0] * tangent[1];
        let pt = to.eval(off);
        let tangent = sub(to.eval(off + EPSILON), pt);
        let length = tangent[0].hypot(tangent[1]);
        let tangent = [tangent[0] / length, tangent[1] / length];
        let rebuilt = [
            pt[0] + along * tangent[0] - across * tangent[1],
            pt[1] + across * tangent[0] + along * tangent[1],
        ];
        check(rebuilt, "deformed vertex")?;
        moved.push((index, rebuilt));
    }
    for (index, position) in moved {
        positions[index] = position;
    }
    Ok(())
}

// -------------------------------------------------------------- deform tools

pub const MAX_INFLATE_SAMPLES: usize = 4096;
/// Most cells along one side of a warp lattice.
pub const MAX_WARP_CELLS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InflateSample {
    pub center: Vec2,
    pub amount: f64,
}

pub fn inflate_stroke(positions: &mut [Vec2], selected: &[usize], brush: &Brush, samples: &[InflateSample]) -> Result<(), String> {
    if samples.is_empty() || samples.len() > MAX_INFLATE_SAMPLES {
        return Err(format!("inflate needs 1 to {MAX_INFLATE_SAMPLES} samples"));
    }
    need_indices(positions.len(), selected)?;
    for sample in samples {
        check(sample.center, "inflate center")?;
        if !sample.amount.is_finite() || !(-1.0..=1.0).contains(&sample.amount) {
            return Err("inflate amount must be finite and between -1 and 1".to_owned());
        }
    }
    let targets: Vec<usize> = if selected.is_empty() { (0..positions.len()).collect() } else { selected.to_vec() };
    for sample in samples {
        for &index in &targets {
            let point = positions[index];
            let amount = sample.amount * brush.weight_at(sample.center, point);
            if amount != 0.0 {
                positions[index] = add(point, scale(sub(point, sample.center), amount));
                check(positions[index], "inflated position")?;
            }
        }
    }
    Ok(())
}

/// A deform-mode gesture over `rest + keypoint offset` positions.
#[derive(Clone, Debug, PartialEq)]
pub enum DeformTool {
    /// Select tool / Point tool in deform mode: move the selection.
    Translate { selected: Vec<usize>, delta: Vec2 },
    /// `resetSelection`: put the selected vertices back on their rest positions.
    Reset { selected: Vec<usize> },
    Brush {
        selected: Vec<usize>,
        brush: Brush,
        flow: bool,
        stroke: Vec<Vec2>,
        snap: bool,
    },
    Inflate {
        selected: Vec<usize>,
        brush: Brush,
        samples: Vec<InflateSample>,
    },
    PathDeform {
        selected: Vec<usize>,
        path: Vec<Vec2>,
        target: Vec<Vec2>,
        resolution: usize,
    },
    /// Temporary deform tool, warp (issue #171): a lattice of `(columns + 1) x (rows + 1)` points, row-major from the
    /// top left, laid over the selection's bounding box and moved to `points`; each cell maps bilinearly. A one-cell
    /// lattice also zooms, rotates, transforms freely or distorts.
    Warp { selected: Vec<usize>, columns: usize, rows: usize, points: Vec<Vec2> },
    /// Temporary deform tool, perspective: the corners of the selection's bounding box (top left, top right, bottom
    /// left, bottom right) move to `corners` through a projective map.
    Perspective { selected: Vec<usize>, corners: [Vec2; 4] },
    /// Bezier-deform tool on PathDeformer control points: move one point.
    BezierTranslate { index: usize, delta: Vec2 },
    /// Bezier-deform tool with a locked point: rotate the points beyond `index`'s side of `locked`
    /// about `locked`, by the angle the mouse swept from `from` to `to`.
    BezierRotate {
        locked: usize,
        index: usize,
        from: Vec2,
        to: Vec2,
    },
}

/// Top left and size of the bounding box of the `selected` positions, refused when it has no area.
fn selection_box(positions: &[Vec2], selected: &[usize]) -> Result<(Vec2, Vec2), String> {
    need_indices(positions.len(), selected)?;
    let (mut low, mut high) = ([f64::MAX; 2], [f64::MIN; 2]);
    for &index in selected {
        for axis in 0..2 {
            low[axis] = low[axis].min(positions[index][axis]);
            high[axis] = high[axis].max(positions[index][axis]);
        }
    }
    let size = [high[0] - low[0], high[1] - low[1]];
    if !(size[0] > 0.0 && size[1] > 0.0) {
        return Err("the selection's bounding box has no area".to_owned());
    }
    Ok((low, size))
}

/// The projective map from the unit square onto `corners` (top left, top right, bottom left, bottom right), after
/// Heckbert's square-to-quad. It answers `None` for a point the map sends through infinity.
fn square_to_quad(corners: &[Vec2; 4]) -> Result<impl Fn(f64, f64) -> Option<Vec2>, String> {
    let [p0, p1, p3, p2] = *corners;
    // Around the quad (TL, TR, BR, BL) every turn has one sign: a convex quad, so no corner triple is on one line
    // and the map stays finite inside the box.
    let ring = [p0, p1, p2, p3];
    let turns: Vec<f64> = (0..4).map(|at| {
        let [a, b, c] = [ring[at], ring[(at + 1) % 4], ring[(at + 2) % 4]];
        (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0])
    }).collect();
    let scale = ring.iter().flatten().fold(1.0f64, |scale, value| scale.max(value.abs()));
    if !(turns.iter().all(|turn| *turn > 1e-9 * scale * scale) || turns.iter().all(|turn| *turn < -1e-9 * scale * scale)) {
        return Err("the corners must make a convex quad".to_owned());
    }
    let (dx1, dx2, dx3) = (p1[0] - p2[0], p3[0] - p2[0], p0[0] - p1[0] + p2[0] - p3[0]);
    let (dy1, dy2, dy3) = (p1[1] - p2[1], p3[1] - p2[1], p0[1] - p1[1] + p2[1] - p3[1]);
    let denominator = dx1 * dy2 - dx2 * dy1;
    if denominator.abs() < 1e-12 {
        return Err("the corners must not lie on one line".to_owned());
    }
    let g = (dx3 * dy2 - dx2 * dy3) / denominator;
    let h = (dx1 * dy3 - dx3 * dy1) / denominator;
    let (a, b, d, e) = (p1[0] - p0[0] + g * p1[0], p3[0] - p0[0] + h * p3[0], p1[1] - p0[1] + g * p1[1],
        p3[1] - p0[1] + h * p3[1]);
    Ok(move |u: f64, v: f64| {
        let w = g * u + h * v + 1.0;
        (w > 1e-12).then(|| [(a * u + b * v + p0[0]) / w, (d * u + e * v + p0[1]) / w])
    })
}

/// Runs a deform tool on `positions`; `rest` is needed by `Reset`.
pub fn deform_positions(
    positions: &mut [Vec2],
    rest: &[Vec2],
    tool: &DeformTool,
    options: &EditOptions,
) -> Result<(), String> {
    if !options.zoom.is_finite() || options.zoom <= 0.0 {
        return Err("zoom must be finite and positive".to_owned());
    }
    let mirror = options.mirror;
    match tool {
        DeformTool::Translate { selected, delta } => {
            check(*delta, "delta")?;
            need_indices(positions.len(), selected)?;
            let selected = mirror.normalize_selection(positions, selected, options.zoom)?;
            for select in selected {
                for axis in mirror.axes() {
                    if let Some(found) = mirror.vertex(positions, axis, select, options.zoom)? {
                        positions[found] = add(positions[found], mirror.delta(axis, *delta));
                    }
                }
            }
        }
        DeformTool::Reset { selected } => {
            need_indices(positions.len(), selected)?;
            for &index in selected {
                positions[index] = rest[index];
            }
        }
        DeformTool::Brush {
            selected,
            brush,
            flow,
            stroke,
            snap,
        } => {
            brush_stroke(positions, selected, brush, *flow, stroke, snap.then_some(options.zoom))?;
        }
        DeformTool::Inflate { selected, brush, samples } => inflate_stroke(positions, selected, brush, samples)?,
        DeformTool::PathDeform {
            selected,
            path,
            target,
            resolution,
        } => {
            path_deform(positions, selected, path, target, *resolution)?;
        }
        DeformTool::Warp { selected, columns, rows, points } => {
            let ([left, top], [width, height]) = selection_box(positions, selected)?;
            for &index in selected {
                let [x, y] = positions[index];
                let (u, v) = ((x - left) / width * *columns as f64, (y - top) / height * *rows as f64);
                let (column, row) = ((u.floor() as usize).min(columns - 1), (v.floor() as usize).min(rows - 1));
                let (s, t) = (u - column as f64, v - row as f64);
                let at = |c: usize, r: usize| points[r * (columns + 1) + c];
                let (a, b, c, d) = (at(column, row), at(column + 1, row), at(column, row + 1), at(column + 1, row + 1));
                positions[index] = std::array::from_fn(|axis| {
                    (a[axis] * (1.0 - s) + b[axis] * s) * (1.0 - t) + (c[axis] * (1.0 - s) + d[axis] * s) * t
                });
            }
        }
        DeformTool::Perspective { selected, corners } => {
            let ([left, top], [width, height]) = selection_box(positions, selected)?;
            let map = square_to_quad(corners)?;
            for &index in selected {
                let [x, y] = positions[index];
                positions[index] = map((x - left) / width, (y - top) / height)
                    .ok_or("the corners fold the selection behind the view")?;
            }
        }
        DeformTool::BezierTranslate { index, delta } => {
            check(*delta, "delta")?;
            need_indices(positions.len(), &[*index])?;
            positions[*index] = add(positions[*index], *delta);
        }
        DeformTool::BezierRotate {
            locked,
            index,
            from,
            to,
        } => {
            need_indices(positions.len(), &[*locked, *index])?;
            check(*from, "from")?;
            check(*to, "to")?;
            if locked == index {
                return Err("the dragged point cannot be the locked point".to_owned());
            }
            let pivot = positions[*locked];
            let before = sub(*from, pivot);
            let after = sub(*to, pivot);
            let angle = after[1].atan2(after[0]) - before[1].atan2(before[0]);
            let (sin, cos) = angle.sin_cos();
            let step: isize = if index > locked { 1 } else { -1 };
            let mut i = *locked as isize + step;
            while i >= 0 && (i as usize) < positions.len() {
                let rel = sub(positions[i as usize], pivot);
                positions[i as usize] = [
                    pivot[0] + rel[0] * cos - rel[1] * sin,
                    pivot[1] + rel[0] * sin + rel[1] * cos,
                ];
                i += step;
            }
        }
    }
    for position in positions.iter() {
        check(*position, "deformed vertex")?;
    }
    Ok(())
}

// ------------------------------------------------------------- JSON boundary

fn object<'a>(value: &'a Value, what: &str, allowed: &[&str]) -> Result<&'a Map<String, Value>, String> {
    let map = value.as_object().ok_or_else(|| format!("{what} must be an object"))?;
    match map.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => Err(format!("{what} has unknown field {key:?}")),
        None => Ok(map),
    }
}

fn number(value: &Value, what: &str) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|n| n.is_finite())
        .ok_or_else(|| format!("{what} must be a finite number"))
}

fn pair(value: &Value, what: &str) -> Result<Vec2, String> {
    match value.as_array().map(Vec::as_slice) {
        Some([x, y]) => Ok([number(x, what)?, number(y, what)?]),
        _ => Err(format!("{what} must be an [x, y] pair")),
    }
}

fn pairs(value: &Value, what: &str) -> Result<Vec<Vec2>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{what} must be an array of [x, y] pairs"))?
        .iter()
        .map(|item| pair(item, what))
        .collect()
}

fn index(value: &Value, what: &str) -> Result<usize, String> {
    value
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| format!("{what} must be a non-negative integer"))
}

fn indices(value: &Value, what: &str) -> Result<Vec<usize>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{what} must be an array of vertex indices"))?
        .iter()
        .map(|item| index(item, what))
        .collect()
}

fn field<'a>(map: &'a Map<String, Value>, key: &str, op: &str) -> Result<&'a Value, String> {
    map.get(key).ok_or_else(|| format!("{op} needs {key}"))
}

fn numbers(value: &Value, what: &str) -> Result<Vec<f64>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{what} must be an array of numbers"))?
        .iter()
        .map(|item| number(item, what))
        .collect()
}

fn resolution(map: &Map<String, Value>) -> Result<usize, String> {
    map.get("resolution").map_or(Ok(40), |value| index(value, "resolution"))
}

/// Parses one `mesh.edit` entry, e.g. `{"op": "add", "pos": [0, 0], "connect": 2}`.
pub fn parse_edit(value: &Value) -> Result<Edit, String> {
    let op = value
        .get("op")
        .and_then(Value::as_str)
        .ok_or("each edit needs an op string")?;
    let pair_op = |map: &Map<String, Value>| -> Result<(usize, usize), String> {
        Ok((index(field(map, "a", op)?, "a")?, index(field(map, "b", op)?, "b")?))
    };
    Ok(match op {
        "add" => {
            let map = object(value, "add", &["op", "pos", "connect"])?;
            let connect = map
                .get("connect")
                .filter(|v| !v.is_null())
                .map(|v| index(v, "connect"))
                .transpose()?;
            Edit::Add {
                pos: pair(field(map, "pos", op)?, "pos")?,
                connect,
            }
        }
        "remove" => {
            let map = object(value, "remove", &["op", "indices", "at"])?;
            match (map.get("indices"), map.get("at")) {
                (Some(list), None) => Edit::Remove {
                    indices: indices(list, "indices")?,
                },
                (None, Some(at)) => Edit::RemoveAt { pos: pair(at, "at")? },
                _ => return Err("remove needs exactly one of indices or at".to_owned()),
            }
        }
        "move" => {
            let map = object(value, "move", &["op", "indices", "delta"])?;
            Edit::Move {
                indices: indices(field(map, "indices", op)?, "indices")?,
                delta: pair(field(map, "delta", op)?, "delta")?,
            }
        }
        "connect" | "disconnect" | "toggle" => {
            let map = object(value, op, &["op", "a", "b"])?;
            let (a, b) = pair_op(map)?;
            match op {
                "connect" => Edit::Connect { a, b },
                "disconnect" => Edit::Disconnect { a, b },
                _ => Edit::Toggle { a, b },
            }
        }
        "cut" => {
            let map = object(value, "cut", &["op", "from", "to"])?;
            Edit::Cut {
                from: pair(field(map, "from", op)?, "from")?,
                to: pair(field(map, "to", op)?, "to")?,
            }
        }
        "grid_create" => {
            let map = object(value, "grid_create", &["op", "bounds", "cuts"])?;
            let bounds = numbers(field(map, "bounds", op)?, "bounds")?;
            let bounds: [f64; 4] = bounds
                .try_into()
                .map_err(|_| "bounds must be [x0, y0, x1, y1]".to_owned())?;
            let cuts = map.get("cuts").map_or(Ok(3), |v| index(v, "cuts"))?;
            Edit::GridCreate { bounds, cuts }
        }
        "grid_axes" => {
            let map = object(value, "grid_axes", &["op", "ys", "xs"])?;
            Edit::GridAxes {
                ys: numbers(field(map, "ys", op)?, "ys")?,
                xs: numbers(field(map, "xs", op)?, "xs")?,
            }
        }
        "grid_add_axis" => {
            let map = object(value, "grid_add_axis", &["op", "pos"])?;
            Edit::GridAddAxis {
                pos: pair(field(map, "pos", op)?, "pos")?,
            }
        }
        "grid_remove_axis" => {
            let map = object(value, "grid_remove_axis", &["op", "vertex"])?;
            Edit::GridRemoveAxis {
                vertex: index(field(map, "vertex", op)?, "vertex")?,
            }
        }
        "grid_move" => {
            let map = object(value, "grid_move", &["op", "vertex", "to"])?;
            Edit::GridMove {
                vertex: index(field(map, "vertex", op)?, "vertex")?,
                to: pair(field(map, "to", op)?, "to")?,
            }
        }
        "triangulate" => {
            object(value, "triangulate", &["op"])?;
            Edit::Triangulate
        }
        "reset" => {
            object(value, "reset", &["op"])?;
            Edit::Reset
        }
        "merge" => {
            let map = object(value, "merge", &["op", "distance"])?;
            let distance = map.get("distance").map(|d| number(d, "distance")).transpose()?.unwrap_or(0.0);
            if !(distance.is_finite() && distance >= 0.0) {
                return Err("merge distance must be a finite number of at least 0".to_owned());
            }
            Edit::Merge { distance }
        }
        "flip" => {
            let map = object(value, "flip", &["op", "axis"])?;
            let vertical = match field(map, "axis", op)?.as_str() {
                Some("h" | "horizontal") => false,
                Some("v" | "vertical") => true,
                _ => return Err("flip axis must be h or v".to_owned()),
            };
            Edit::Flip { vertical }
        }
        "path_deform" => {
            let map = object(value, "path_deform", &["op", "path", "target", "indices", "resolution"])?;
            Edit::PathDeform {
                path: pairs(field(map, "path", op)?, "path")?,
                target: pairs(field(map, "target", op)?, "target")?,
                indices: map.get("indices").map_or(Ok(Vec::new()), |v| indices(v, "indices"))?,
                resolution: resolution(map)?,
            }
        }
        other => return Err(format!("unknown mesh edit {other:?}")),
    })
}

/// Parses the editor settings shared by `mesh.edit` and `deform.edit`.
pub fn parse_options(request: &Value) -> Result<EditOptions, String> {
    let mut options = EditOptions::default();
    if let Some(zoom) = request.get("zoom").filter(|v| !v.is_null()) {
        options.zoom = number(zoom, "zoom")?;
        finite_positive(options.zoom, "zoom")?;
    }
    if let Some(auto) = request.get("autoConnect").filter(|v| !v.is_null()) {
        options.auto_connect = auto.as_bool().ok_or("autoConnect must be a boolean")?;
    }
    if let Some(mirror) = request.get("mirror").filter(|v| !v.is_null()) {
        let map = object(mirror, "mirror", &["horizontal", "vertical", "origin"])?;
        let flag = |key: &str| match map.get(key) {
            None | Some(Value::Null) => Ok(false),
            Some(value) => value.as_bool().ok_or_else(|| format!("mirror.{key} must be a boolean")),
        };
        options.mirror = Mirror {
            horizontal: flag("horizontal")?,
            vertical: flag("vertical")?,
            origin: map.get("origin").map_or(Ok([0.0, 0.0]), |v| pair(v, "mirror.origin"))?,
        };
    }
    Ok(options)
}

fn parse_brush(value: &Value) -> Result<Brush, String> {
    let map = object(value, "brush", &["kind", "radius", "innerRadius", "width", "height", "angle"])?;
    let get = |key: &str| number(field(map, key, "brush")?, key);
    match map.get("kind").and_then(Value::as_str) {
        Some("line") => Brush::line(get("radius")?, get("angle")?),
        Some("circle") => Brush::circle(get("radius")?),
        Some("double_threshold") => Brush::double_threshold(get("radius")?, get("innerRadius")?),
        Some("rectangle") => Brush::rectangle(get("width")?, get("height")?),
        _ => Err("brush kind must be circle, double_threshold, rectangle or line".to_owned()),
    }
}

/// Which vertices a deform gesture works on, resolved against the current positions.
fn every_if_empty(selected: Vec<usize>, positions: &[Vec2]) -> Vec<usize> {
    if selected.is_empty() {
        (0..positions.len()).collect()
    } else {
        selected
    }
}

fn parse_selection(request: &Value, positions: &[Vec2]) -> Result<Vec<usize>, String> {
    let given = ["selected", "rect", "lasso"]
        .iter()
        .filter(|key| request.get(**key).is_some_and(|v| !v.is_null()))
        .count();
    if given > 1 {
        return Err("give only one of selected, rect and lasso".to_owned());
    }
    let hits = if let Some(list) = request.get("selected").filter(|v| !v.is_null()) {
        indices(list, "selected")?
    } else if let Some(rect) = request.get("rect").filter(|v| !v.is_null()) {
        let corners = numbers(rect, "rect")?;
        let [x0, y0, x1, y1]: [f64; 4] = corners
            .try_into()
            .map_err(|_| "rect must be [x0, y0, x1, y1]".to_owned())?;
        select_rect(positions, [x0, y0], [x1, y1])?
    } else if let Some(points) = request.get("lasso").filter(|v| !v.is_null()) {
        select_lasso(positions, &pairs(points, "lasso")?)?
    } else {
        Vec::new()
    };
    need_indices(positions.len(), &hits)?;
    let mut seen = HashSet::new();
    Ok(hits.into_iter().filter(|i| seen.insert(*i)).collect())
}

fn parse_deform_tool(request: &Value, positions: &[Vec2]) -> Result<DeformTool, String> {
    let name = request
        .get("tool")
        .and_then(Value::as_str)
        .ok_or("tool must be a string")?;
    let selected = parse_selection(request, positions)?;
    let need_selection = |selected: Vec<usize>| {
        if selected.is_empty() {
            Err(format!("tool {name} needs a non-empty selection"))
        } else {
            Ok(selected)
        }
    };
    Ok(match name {
        "translate" => DeformTool::Translate {
            selected: need_selection(selected)?,
            delta: pair(request.get("delta").ok_or("translate needs delta")?, "delta")?,
        },
        // With no selection, rect or lasso, reset returns the whole keyform to the rest shape (issue #166).
        "reset" if ["selected", "rect", "lasso"].iter().all(|key| request.get(key).is_none()) => DeformTool::Reset {
            selected: (0..positions.len()).collect(),
        },
        "reset" => DeformTool::Reset {
            selected: need_selection(selected)?,
        },
        "brush" => DeformTool::Brush {
            selected,
            brush: parse_brush(request.get("brush").ok_or("brush needs a brush")?)?,
            flow: request
                .get("flow")
                .map_or(Ok(false), |v| v.as_bool().ok_or("flow must be a boolean"))?,
            stroke: pairs(request.get("stroke").ok_or("brush needs stroke")?, "stroke")?,
            snap: request
                .get("snapAxis")
                .map_or(Ok(false), |v| v.as_bool().ok_or("snapAxis must be a boolean"))?,
        },
        "inflate" => {
            let samples = request.get("samples").and_then(Value::as_array).ok_or("inflate needs a samples array")?;
            if samples.is_empty() || samples.len() > MAX_INFLATE_SAMPLES {
                return Err(format!("inflate needs 1 to {MAX_INFLATE_SAMPLES} samples"));
            }
            let samples = samples.iter().map(|sample| {
                let sample = object(sample, "inflate sample", &["center", "amount"])?;
                Ok(InflateSample {
                    center: pair(field(sample, "center", "inflate sample")?, "inflate center")?,
                    amount: number(field(sample, "amount", "inflate sample")?, "inflate amount")?,
                })
            }).collect::<Result<Vec<_>, String>>()?;
            DeformTool::Inflate { selected, brush: parse_brush(request.get("brush").ok_or("inflate needs a brush")?)?, samples }
        }
        "path_deform" => DeformTool::PathDeform {
            selected,
            path: pairs(request.get("path").ok_or("path_deform needs path")?, "path")?,
            target: pairs(request.get("target").ok_or("path_deform needs target")?, "target")?,
            resolution: resolution(request.as_object().ok_or("request must be an object")?)?,
        },
        "warp" => {
            let grid = object(request.get("grid").ok_or("warp needs grid")?, "warp grid", &["columns", "rows", "points"])?;
            let [columns, rows] = ["columns", "rows"].map(|key| grid.get(key).map(|value| index(value, key)));
            let (columns, rows) = (columns.ok_or("warp grid needs columns")??, rows.ok_or("warp grid needs rows")??);
            if !(1..=MAX_WARP_CELLS).contains(&columns) || !(1..=MAX_WARP_CELLS).contains(&rows) {
                return Err(format!("warp grid columns and rows must be 1 to {MAX_WARP_CELLS}"));
            }
            let points = pairs(grid.get("points").ok_or("warp grid needs points")?, "warp point")?;
            if points.len() != (columns + 1) * (rows + 1) {
                return Err(format!("warp grid needs {} points", (columns + 1) * (rows + 1)));
            }
            DeformTool::Warp { selected: every_if_empty(selected, positions), columns, rows, points }
        }
        "perspective" => {
            let corners = pairs(request.get("corners").ok_or("perspective needs corners")?, "corner")?;
            DeformTool::Perspective {
                selected: every_if_empty(selected, positions),
                corners: corners.try_into().map_err(|_| "perspective needs 4 corners".to_owned())?,
            }
        }
        "bezier_translate" => DeformTool::BezierTranslate {
            index: index(request.get("index").ok_or("bezier_translate needs index")?, "index")?,
            delta: pair(request.get("delta").ok_or("bezier_translate needs delta")?, "delta")?,
        },
        "bezier_rotate" => DeformTool::BezierRotate {
            locked: index(request.get("locked").ok_or("bezier_rotate needs locked")?, "locked")?,
            index: index(request.get("index").ok_or("bezier_rotate needs index")?, "index")?,
            from: pair(request.get("from").ok_or("bezier_rotate needs from")?, "from")?,
            to: pair(request.get("to").ok_or("bezier_rotate needs to")?, "to")?,
        },
        other => return Err(format!("unknown deform tool {other:?}")),
    })
}

// ---------------------------------------------------------- Part document I/O

/// Welds that name this Part (its own links, or links from any other node to it).
fn has_welds(model: &Value, id: u32) -> bool {
    fn links_to(node: &Value, id: u32) -> bool {
        let own = node.get("uuid").and_then(Value::as_u64) == Some(u64::from(id));
        let linked = node.get("weldedLinks").and_then(Value::as_array).is_some_and(|links| {
            !links.is_empty()
                && (own
                    || links
                        .iter()
                        .any(|l| l.get("targetUUID").and_then(Value::as_u64) == Some(u64::from(id))))
        });
        linked
            || node
                .get("children")
                .and_then(Value::as_array)
                .is_some_and(|children| children.iter().any(|child| links_to(child, id)))
    }
    model.get("nodes").is_some_and(|root| links_to(root, id))
}

fn has_depth_payload(node: &Value) -> bool {
    ["depths", "depth-ops", "depth-op-base-depths"]
        .iter()
        .any(|key| node.get(*key).is_some_and(|v| !v.is_null()))
}

/// Pixel size of texture slot 0 without decoding the image. `None` (D: `textures[0] is null`) selects the
/// bounding-box UV fallback.
pub(crate) fn texture_size(node: &Value, textures: &[TextureBlob]) -> Result<Option<[u32; 2]>, String> {
    let Some(slot) = node
        .get("textures")
        .and_then(Value::as_array)
        .and_then(|slots| slots.first())
        .and_then(Value::as_u64)
        .filter(|&slot| slot != NO_TEXTURE)
    else {
        return Ok(None);
    };
    let Some(blob) = usize::try_from(slot).ok().and_then(|slot| textures.get(slot)) else {
        return Ok(None);
    };
    let format = match blob.tag {
        0 => image::ImageFormat::Png,
        1 => image::ImageFormat::Tga,
        tag => return Err(format!("texture slot {slot} has unsupported image tag {tag}")),
    };
    let (width, height) = image::ImageReader::with_format(Cursor::new(blob.data.as_slice()), format)
        .into_dimensions()
        .map_err(|error| format!("could not read texture slot {slot}: {error}"))?;
    Ok(Some([width, height]))
}

fn flat(points: &[Vec2]) -> Vec<f64> {
    points.iter().flat_map(|p| [p[0], p[1]]).collect()
}

/// Rewrites the legacy top-level array in the shape it already has (nested pairs or flat).
fn rewrite_top_level(object: &mut Map<String, Value>, key: &str, values: &[f64], paired: bool) {
    let Some(existing) = object.get(key) else {
        return;
    };
    let nested = existing
        .as_array()
        .and_then(|items| items.first())
        .is_some_and(Value::is_array);
    let value = if paired && nested {
        Value::Array(values.chunks(2).map(|p| json!([p[0], p[1]])).collect())
    } else {
        json!(values)
    };
    object.insert(key.to_owned(), value);
}

fn write_part(node: &mut Value, built: &BuiltMesh) -> Result<(), String> {
    let (verts, uvs) = (flat(&built.vertices), flat(&built.uvs));
    let object = node.as_object_mut().ok_or("node must be an object")?;
    let mesh = object
        .entry("mesh")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("mesh must be an object")?;
    mesh.insert("verts".into(), json!(verts));
    mesh.insert("uvs".into(), json!(uvs));
    mesh.insert("indices".into(), json!(built.indices));
    mesh.insert("origin".into(), json!(built.origin));
    match &built.grid_axes {
        Some(axes) => {
            mesh.insert("gridAxes".into(), json!([axes[0], axes[1]]));
        }
        None => {
            mesh.remove("gridAxes");
        }
    }
    rewrite_top_level(object, "vertices", &verts, true);
    rewrite_top_level(object, "uvs", &uvs, true);
    if let Some(top) = object.get_mut("indices") {
        *top = json!(built.indices);
    }
    Ok(())
}

fn request_id(request: &Value) -> Result<u32, String> {
    request
        .get("id")
        .and_then(Value::as_u64)
        .and_then(crate::valid_id)
        .ok_or_else(|| "id must be a nonzero u32 node UUID".to_owned())
}

fn part_mesh_of(model: &Value, id: u32) -> Result<(PartMesh, Vec<u32>), String> {
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    if node.get("type").and_then(Value::as_str) != Some("Part") {
        return Err(format!(
            "node {id} is not a Part; Path, Grid and MeshGroup vertices use deformer.define_vertices and \
             deformer.define_grid"
        ));
    }
    let mesh = deformers::parse_part_mesh(node)?;
    let indices = mesh
        .indices
        .iter()
        .map(|&i| u32::try_from(i).map_err(|_| "mesh index is too large".to_owned()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((mesh, indices))
}

/// `mesh.edit`: applies the request's edits to the Part and returns `(candidate model, summary)`.
pub(crate) fn edit_part(model: &Value, textures: &[TextureBlob], request: &Value) -> Result<(Value, Value), String> {
    object(
        request,
        "request",
        &["op", "id", "edits", "mirror", "zoom", "autoConnect"],
    )?;
    let id = request_id(request)?;
    let edits = request
        .get("edits")
        .and_then(Value::as_array)
        .ok_or("edits must be an array")?
        .iter()
        .map(parse_edit)
        .collect::<Result<Vec<_>, _>>()?;
    if edits.is_empty() {
        return Err("edits must not be empty".to_owned());
    }
    let mut options = parse_options(request)?;
    let (old, old_indices) = part_mesh_of(model, id)?;
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    let mut mesh = EditMesh::from_triangles(&old.vertices, &old_indices, old.origin)?;
    options.texture = texture_size(node, textures)?;
    let stable = apply_edits(&mut mesh, &edits, &options)?;
    let built = build_mesh(&mesh, options.texture)?;

    let moved = built.vertices != old.vertices;
    // Per-vertex data (depths, skinning weights) stays valid only while vertex i is still vertex i.
    let preserved = stable && !built.permuted && built.vertices.len() == old.vertices.len();
    if moved && has_welds(model, id) {
        return Err(format!("Part {id} has welds that mesh.edit cannot remap"));
    }
    if !preserved {
        if has_depth_payload(node) {
            return Err(format!("Part {id} has a depth payload that mesh.edit cannot resize"));
        }
        crate::bindings::reject_vertex_weight_resize(model, id, usize::MAX)
            .map_err(|_| format!("Part {id} has skinning weights that mesh.edit cannot remap"))?;
    }

    let mut candidate = model.clone();
    if moved {
        transfer_bindings(&mut candidate, id, &old, &old_indices, &built.vertices)?;
    }
    let target = find_node_mut(&mut candidate, id).ok_or_else(|| format!("node {id} was not found"))?;
    write_part(target, &built)?;
    crate::validate_model(&candidate)?;
    let summary = json!({
        "id": id,
        "changed": candidate != *model,
        "vertices": built.vertices.len(),
        "triangles": built.indices.len() / 3,
        "grid": built.grid_axes.is_some(),
    });
    Ok((candidate, summary))
}

/// A Part's rest mesh, snapshotted at copy time. Paste does not read the source again.
#[derive(Clone, Debug)]
pub(crate) struct MeshClipboard {
    pub(crate) source: u32,
    pub(crate) vertices: Vec<Vec2>,
    pub(crate) indices: Vec<u32>,
    pub(crate) origin: Vec2,
    pub(crate) world: crate::bone_deform::Mat4,
}

/// `mesh.copy`. The source must be a Part whose rest frame is an affine XY transform.
pub(crate) fn copy_part_mesh(model: &Value, id: u32) -> Result<MeshClipboard, String> {
    reject_nonlinear_rest(model, id)?;
    let (mesh, indices) = part_mesh_of(model, id)?;
    Ok(MeshClipboard {
        source: id,
        vertices: mesh.vertices,
        indices,
        origin: mesh.origin,
        world: crate::bone_deform::world_matrix(model, id)?,
    })
}

/// `mesh.paste`. Appends the copied triangles onto `target` in the target's rest space.
/// `reflect` is `none`, `horizontal`, `vertical`, or `both`, across the target mesh origin.
pub(crate) fn paste_part_mesh(
    model: &Value,
    textures: &[TextureBlob],
    clipboard: &MeshClipboard,
    target: u32,
    reflect: &str,
) -> Result<(Value, Value), String> {
    if clipboard.vertices.is_empty() || clipboard.indices.is_empty() {
        return Err("mesh clipboard has no triangles".to_owned());
    }
    let (flip_x, flip_y) = match reflect {
        "none" => (false, false),
        "horizontal" => (true, false),
        "vertical" => (false, true),
        "both" => (true, true),
        _ => return Err("reflect must be none, horizontal, vertical, or both".to_owned()),
    };
    reject_nonlinear_rest(model, target)?;
    let (old, old_indices) = part_mesh_of(model, target)?;
    let inverse = crate::bone_deform::invert(crate::bone_deform::world_matrix(model, target)?)?;
    let mut placed = Vec::with_capacity(clipboard.vertices.len());
    for vertex in &clipboard.vertices {
        let canvas = crate::bone_deform::transform_point(
            clipboard.world,
            [vertex[0] - clipboard.origin[0], vertex[1] - clipboard.origin[1], 0.0],
        )?;
        let local = crate::bone_deform::transform_point(inverse, canvas)?;
        let mut point = [local[0] + old.origin[0], local[1] + old.origin[1]];
        if flip_x {
            point[0] = 2.0 * old.origin[0] - point[0];
        }
        if flip_y {
            point[1] = 2.0 * old.origin[1] - point[1];
        }
        placed.push(point);
    }
    let mut vertices = old.vertices.clone();
    let mut remap = Vec::with_capacity(placed.len());
    for point in placed {
        let existing = vertices.iter().position(|kept| {
            (kept[0] - point[0]).abs() <= 1e-6 && (kept[1] - point[1]).abs() <= 1e-6
        });
        remap.push(match existing {
            Some(index) => u32::try_from(index).map_err(|_| "mesh index is too large".to_owned())?,
            None => {
                let index = u32::try_from(vertices.len()).map_err(|_| "mesh index is too large".to_owned())?;
                vertices.push(point);
                index
            }
        });
    }
    let mut indices = old_indices;
    let reverse = flip_x ^ flip_y;
    for triangle in clipboard.indices.chunks(3) {
        if triangle.len() != 3 {
            return Err("copied mesh indices are not triangles".to_owned());
        }
        let mut mapped = [0; 3];
        for (slot, index) in triangle.iter().enumerate() {
            mapped[slot] = *remap
                .get(*index as usize)
                .ok_or_else(|| format!("copied index {index} is outside the copied mesh"))?;
        }
        if reverse {
            mapped.swap(1, 2);
        }
        if mapped[0] == mapped[1] || mapped[1] == mapped[2] || mapped[0] == mapped[2] {
            continue;
        }
        indices.extend(mapped);
    }
    let node = find_node(model, target).ok_or_else(|| format!("node {target} was not found"))?;
    if vertices.len() != old.vertices.len() {
        if has_welds(model, target) {
            return Err(format!("Part {target} has welds that mesh paste cannot remap"));
        }
        if has_depth_payload(node) {
            return Err(format!("Part {target} has a depth payload that mesh paste cannot resize"));
        }
        crate::bindings::reject_vertex_weight_resize(model, target, usize::MAX)
            .map_err(|_| format!("Part {target} has skinning weights that mesh paste cannot remap"))?;
    }
    let mesh = EditMesh::from_triangles(&vertices, &indices, old.origin)?;
    let texture = texture_size(node, textures)?;
    let built = build_mesh(&mesh, texture)?;
    let mut candidate = model.clone();
    let written = find_node_mut(&mut candidate, target).ok_or_else(|| format!("node {target} was not found"))?;
    write_part(written, &built)?;
    crate::validate_model(&candidate)?;
    let summary = json!({
        "id": target,
        "source": clipboard.source,
        "vertices": built.vertices.len(),
        "appended": built.vertices.len().saturating_sub(old.vertices.len()),
        "triangles": built.indices.len() / 3,
    });
    Ok((candidate, summary))
}

fn reject_nonlinear_rest(model: &Value, id: u32) -> Result<(), String> {
    let mut current = Some(id);
    while let Some(node_id) = current {
        let node = find_node(model, node_id).ok_or_else(|| format!("node {node_id} was not found"))?;
        let kind = node.get("type").and_then(Value::as_str).unwrap_or("");
        if node_id != id && matches!(kind, "GridDeformer" | "PathDeformer" | "MeshGroup") {
            return Err(format!("node {id} is under a {kind}, which mesh copy cannot convert"));
        }
        if node.get("pinToMesh").and_then(Value::as_bool) == Some(true) {
            return Err(format!("node {node_id} is pinned to a mesh"));
        }
        if node.get("dynamic_deformation").and_then(Value::as_bool) == Some(true) {
            return Err(format!("node {node_id} has dynamic deformation"));
        }
        if let Some(rotation) = node.get("transform").and_then(|transform| transform.get("rot")).and_then(Value::as_array) {
            for (axis, name) in [(0, "X"), (1, "Y")] {
                let value = rotation.get(axis).and_then(Value::as_f64).unwrap_or(0.0);
                if value.abs() > 1e-8 {
                    return Err(format!("node {node_id} has {name} rotation, which mesh copy cannot convert"));
                }
            }
        }
        current = crate::bone_deform::parent_of(model, node_id)?;
    }
    Ok(())
}

type Cells = Vec<Vec<Vec<Vec2>>>;

fn parse_cell(cell: &Value) -> Result<Vec<Vec2>, String> {
    match cell {
        Value::Null => Ok(Vec::new()),
        Value::Array(points) => points.iter().map(|p| pair(p, "deform offset")).collect(),
        _ => Err("deform binding values must be arrays".to_owned()),
    }
}

/// `applyMeshToTarget`'s binding pass for every deform binding of the Part: set keypoints are
/// re-expressed on the new vertices through the old mesh (`deformByDeformationBinding`), the others
/// become zeros of the new length, then unset keypoints are re-interpolated (`reInterpolate`).
fn transfer_bindings(
    model: &mut Value,
    id: u32,
    old: &PartMesh,
    old_indices: &[u32],
    new_vertices: &[Vec2],
) -> Result<(), String> {
    fn walk(
        list: &mut [Value],
        id: u32,
        visit: &mut impl FnMut(&[f64], &[f64], bool, &mut Value) -> Result<(), String>,
    ) -> Result<(), String> {
        for parameter in list.iter_mut() {
            let has_binding = parameter
                .get("bindings")
                .and_then(Value::as_array)
                .is_some_and(|bindings| {
                    bindings.iter().any(|b| {
                        b.get("param_name").and_then(Value::as_str) == Some("deform")
                            && b.get("node").and_then(Value::as_u64) == Some(u64::from(id))
                    })
                });
            if has_binding {
                let (x_axis, y_axis, _) = deformers::deform_key_axes(parameter)?;
                let repeat = crate::evaluation::parameter_repeats(parameter);
                let bindings = parameter
                    .get_mut("bindings")
                    .and_then(Value::as_array_mut)
                    .ok_or("parameter.bindings must be an array")?;
                for binding in bindings.iter_mut().filter(|b| {
                    b.get("param_name").and_then(Value::as_str) == Some("deform")
                        && b.get("node").and_then(Value::as_u64) == Some(u64::from(id))
                }) {
                    visit(&x_axis, &y_axis, repeat, binding)?;
                }
            }
            if let Some(children) = parameter.get_mut("children").and_then(Value::as_array_mut) {
                walk(children, id, visit)?;
            }
        }
        Ok(())
    }

    let Some(parameters) = model.get_mut("param").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    let new_len = new_vertices.len();
    walk(parameters, id, &mut |x_axis, y_axis, repeat, binding| {
        let malformed = || format!("node {id} deform binding is malformed");
        let values = binding.get("values").and_then(Value::as_array).ok_or_else(malformed)?;
        let flags = binding.get("isSet").and_then(Value::as_array).ok_or_else(malformed)?;
        if values.len() != x_axis.len() || flags.len() != x_axis.len() {
            return Err(malformed());
        }
        let cells_total = x_axis.len().saturating_mul(y_axis.len());
        if cells_total.saturating_mul(new_len).saturating_mul(2) > MAX_BINDING_VALUES {
            return Err(format!(
                "node {id} deform bindings exceed the output size limit after the mesh edit"
            ));
        }
        let mut offsets: Cells = Vec::new();
        let mut is_set: Vec<Vec<bool>> = Vec::new();
        for (column, flag_column) in values.iter().zip(flags) {
            let (column, flag_column) = (
                column.as_array().ok_or_else(malformed)?,
                flag_column.as_array().ok_or_else(malformed)?,
            );
            if column.len() != y_axis.len() || flag_column.len() != y_axis.len() {
                return Err(malformed());
            }
            let mut new_column = Vec::new();
            let mut set_column = Vec::new();
            for (cell, flag) in column.iter().zip(flag_column) {
                let set = match flag {
                    Value::Bool(set) => *set,
                    Value::Null => false,
                    _ => return Err(malformed()),
                };
                new_column.push(if set {
                    let cell = parse_cell(cell)?;
                    if cell.len() != old.vertices.len() {
                        return Err(format!("node {id} deform binding does not match the Part mesh"));
                    }
                    geometry::transfer_mesh_offsets(new_vertices, &old.vertices, old_indices, &cell, false)?
                } else {
                    vec![[0.0, 0.0]; new_len]
                });
                set_column.push(set);
            }
            offsets.push(new_column);
            is_set.push(set_column);
        }
        if is_set.iter().flatten().any(|set| *set) {
            deformers::fill_unset_offsets(&mut offsets, &is_set, x_axis, y_axis, repeat)?;
        }
        let rows: Vec<Value> = offsets
            .iter()
            .map(|column| {
                Value::Array(
                    column
                        .iter()
                        .map(|cell| Value::Array(cell.iter().map(|p| json!([p[0], p[1]])).collect()))
                        .collect(),
                )
            })
            .collect();
        binding
            .as_object_mut()
            .ok_or_else(malformed)?
            .insert("values".into(), Value::Array(rows));
        Ok(())
    })
}

// ------------------------------------------------------------------ deform.edit

/// Positions a deform gesture starts from: rest points plus the offsets stored at `key` (zeros without
/// a binding), the state `DeformationViewport.syncEditor` shows.
fn key_positions(model: &Value, id: u32, parameter_id: u32, key: [usize; 2]) -> Result<(Vec<Vec2>, Vec<Vec2>), String> {
    let node = find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    let rest = deformers::node_geometry_points(node);
    if rest.is_empty() {
        return Err(format!("node {id} has no deform geometry"));
    }
    fn find<'a>(list: &'a [Value], id: u32) -> Option<&'a Value> {
        list.iter().find_map(|p| {
            if p.get("groupUUID").is_some() {
                find(p.get("children")?.as_array()?, id)
            } else if p.get("uuid").and_then(Value::as_u64) == Some(u64::from(id)) {
                Some(p)
            } else {
                None
            }
        })
    }
    let parameter = model
        .get("param")
        .and_then(Value::as_array)
        .and_then(|list| find(list, parameter_id))
        .ok_or_else(|| format!("request refers to unknown leaf parameter {parameter_id}"))?;
    let (x_axis, y_axis, _) = deformers::deform_key_axes(parameter)?;
    if key[0] >= x_axis.len() || key[1] >= y_axis.len() {
        return Err("key must identify an existing parameter keypoint index".to_owned());
    }
    let mut offsets = vec![[0.0, 0.0]; rest.len()];
    let binding = parameter
        .get("bindings")
        .and_then(Value::as_array)
        .and_then(|bindings| {
            bindings.iter().find(|b| {
                b.get("param_name").and_then(Value::as_str) == Some("deform")
                    && b.get("node").and_then(Value::as_u64) == Some(u64::from(id))
            })
        });
    if let Some(cell) = binding
        .and_then(|b| b.get("values"))
        .and_then(|v| v.get(key[0]))
        .and_then(|v| v.get(key[1]))
    {
        let cell = parse_cell(cell)?;
        if !cell.is_empty() {
            if cell.len() != rest.len() {
                return Err(format!("node {id} deform binding does not match its geometry"));
            }
            offsets = cell;
        }
    }
    let positions = rest.iter().zip(&offsets).map(|(r, o)| add(*r, *o)).collect();
    Ok((rest, positions))
}

fn key_of(request: &Value) -> Result<[usize; 2], String> {
    let pair = request
        .get("key")
        .and_then(Value::as_array)
        .filter(|pair| pair.len() == 2)
        .ok_or("key must be a two-element array of axis indexes")?;
    Ok([index(&pair[0], "key")?, index(&pair[1], "key")?])
}

fn parameter_of(request: &Value) -> Result<u32, String> {
    request
        .get("parameter")
        .and_then(Value::as_u64)
        .and_then(|id| u32::try_from(id).ok())
        .ok_or_else(|| "parameter must be an unsigned 32-bit id".to_owned())
}

/// `deform.edit`: runs one deform tool on the node's keypoint and stores the result through
/// `binding.set_deform`'s code path. Returns `(candidate model, summary)`.
pub(crate) fn deform_edit(model: &Value, request: &Value) -> Result<(Value, Value), String> {
    let id = request_id(request)?;
    let parameter = parameter_of(request)?;
    let key = key_of(request)?;
    let options = parse_options(request)?;
    let (rest, mut positions) = key_positions(model, id, parameter, key)?;
    let tool = parse_deform_tool(request, &positions)?;
    let before = matches!(&tool, DeformTool::Inflate { .. }).then(|| positions.clone());
    deform_positions(&mut positions, &rest, &tool, &options)?;
    let summary = json!({ "id": id, "parameter": parameter, "key": key, "vertices": rest.len() });
    if before.as_ref() == Some(&positions) {
        return Ok((model.clone(), summary));
    }
    let values: Vec<f64> = positions
        .iter()
        .zip(&rest)
        .flat_map(|(p, r)| [p[0] - r[0], p[1] - r[1]])
        .collect();
    let mut candidate = model.clone();
    deformers::set_deform(
        &mut candidate,
        &json!({ "parameter": parameter, "nodes": [id], "key": key, "values": values }),
    )?;
    Ok((candidate, summary))
}

/// `mesh.select`: rectangle or lasso hits (read only). Positions are the Part's mesh vertices, or
/// `rest + offset` at `key` when `parameter` and `key` are given.
pub(crate) fn select_vertices(model: &Value, request: &Value) -> Result<Value, String> {
    let id = request_id(request)?;
    let positions = if request.get("parameter").is_some() {
        key_positions(model, id, parameter_of(request)?, key_of(request)?)?.1
    } else {
        part_mesh_of(model, id)?.0.vertices
    };
    let hits = match (request.get("rect"), request.get("lasso")) {
        (Some(rect), None) => {
            let corners = numbers(rect, "rect")?;
            let [x0, y0, x1, y1]: [f64; 4] = corners
                .try_into()
                .map_err(|_| "rect must be [x0, y0, x1, y1]".to_owned())?;
            select_rect(&positions, [x0, y0], [x1, y1])?
        }
        (None, Some(points)) => select_lasso(&positions, &pairs(points, "lasso")?)?,
        _ => return Err("give exactly one of rect and lasso".to_owned()),
    };
    let mode = match request.get("mode").and_then(Value::as_str) {
        None | Some("replace") => SelectMode::Replace,
        Some("add") => SelectMode::Add,
        Some("remove") => SelectMode::Remove,
        Some("toggle") => SelectMode::Toggle,
        Some(_) => return Err("mode must be replace, add, remove or toggle".to_owned()),
    };
    let current = request
        .get("current")
        .map_or(Ok(Vec::new()), |v| indices(v, "current"))?;
    need_indices(positions.len(), &current)?;
    Ok(json!({ "selected": combine_selection(&current, &hits, mode) }))
}
