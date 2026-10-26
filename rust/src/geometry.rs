//! Plain-data mesh geometry: hit tests, vertex connections, Delaunay triangulation,
//! triangle rasterization and mesh-based offset transfer.
//!
//! Ports `core/math/{vertex,mesh,triangle}.d`. Skeleton thinning and longest-path
//! extraction (`skeletonize.d`, `path.d`) already live in `cv.rs`.
//! Points are `[f64; 2]` like the other editor modules; every function rejects
//! non-finite coordinates instead of comparing against NaN.

pub type Vec2 = [f64; 2];

/// Hit radius in screen pixels (`selectRadius` in `vertex.d`).
pub const SELECT_RADIUS: f64 = 16.0;

fn check_point(point: Vec2, what: &str) -> Result<(), String> {
    if point[0].is_finite() && point[1].is_finite() {
        Ok(())
    } else {
        Err(format!("{what} must be finite"))
    }
}

fn check_points(points: &[Vec2], what: &str) -> Result<(), String> {
    for (index, point) in points.iter().enumerate() {
        check_point(*point, &format!("{what} {index}"))?;
    }
    Ok(())
}

fn check_zoom(zoom: f64) -> Result<(), String> {
    if zoom.is_finite() && zoom > 0.0 {
        Ok(())
    } else {
        Err("zoom must be finite and positive".to_owned())
    }
}

fn distance(a: Vec2, b: Vec2) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

// ---------------------------------------------------------------- mesh.d

/// `MeshVertex`: `connections` hold indices into the slice that owns the vertex.
#[derive(Clone, Debug, PartialEq)]
pub struct MeshVertex {
    pub position: Vec2,
    pub connections: Vec<usize>,
    pub group_id: u32,
}

impl MeshVertex {
    /// Group 1 is the D default.
    pub fn new(position: Vec2) -> Self {
        Self { position, connections: Vec::new(), group_id: 1 }
    }

    /// `groupId > 0 && !groupIdEquals` filter: group 0 selects every vertex.
    pub fn matches_group(&self, group_id: u32) -> bool {
        group_id == 0 || self.group_id == group_id
    }
}

fn check_pair(vertices: &[MeshVertex], a: usize, b: usize) -> Result<(), String> {
    if a >= vertices.len() || b >= vertices.len() {
        return Err(format!("vertex index {} is past the vertex list", a.max(b)));
    }
    if a == b {
        return Err("a vertex cannot connect to itself".to_owned());
    }
    Ok(())
}

pub fn is_connected_to(vertices: &[MeshVertex], a: usize, b: usize) -> Result<bool, String> {
    check_pair(vertices, a, b)?;
    Ok(vertices[b].connections.contains(&a))
}

/// Symmetric and idempotent.
pub fn connect(vertices: &mut [MeshVertex], a: usize, b: usize) -> Result<(), String> {
    if is_connected_to(vertices, a, b)? {
        return Ok(());
    }
    vertices[a].connections.push(b);
    vertices[b].connections.push(a);
    Ok(())
}

pub fn disconnect(vertices: &mut [MeshVertex], a: usize, b: usize) -> Result<(), String> {
    check_pair(vertices, a, b)?;
    for (from, to) in [(a, b), (b, a)] {
        if let Some(slot) = vertices[from].connections.iter().position(|&c| c == to) {
            vertices[from].connections.remove(slot);
        }
    }
    Ok(())
}

pub fn disconnect_all(vertices: &mut [MeshVertex], a: usize) -> Result<(), String> {
    if a >= vertices.len() {
        return Err(format!("vertex index {a} is past the vertex list"));
    }
    while let Some(&other) = vertices[a].connections.first() {
        disconnect(vertices, a, other)?;
    }
    Ok(())
}

// --------------------------------------------------------------- vertex.d

/// `isOverlapped`: strictly inside the zoom-scaled select radius.
pub fn is_overlapped(a: Vec2, b: Vec2, zoom: f64) -> Result<bool, String> {
    check_zoom(zoom)?;
    check_point(a, "point")?;
    check_point(b, "point")?;
    Ok(distance(a, b) < SELECT_RADIUS / zoom)
}

/// `getVertexFromPoint`, also the target of `removeVertexAt`: the first vertex in
/// list order under the cursor, not the nearest one.
pub fn get_vertex_from_point(points: &[Vec2], point: Vec2, zoom: f64) -> Result<Option<usize>, String> {
    check_zoom(zoom)?;
    check_point(point, "point")?;
    check_points(points, "vertex")?;
    let radius = SELECT_RADIUS / zoom;
    Ok(points.iter().position(|&vertex| distance(vertex, point) < radius))
}

pub fn is_point_over_vertex(points: &[Vec2], point: Vec2, zoom: f64) -> Result<bool, String> {
    Ok(get_vertex_from_point(points, point, zoom)?.is_some())
}

/// `findPoint`: the nearest vertex, accepted when within the radius (inclusive).
pub fn find_point(points: &[Vec2], point: Vec2, zoom: f64) -> Result<Option<usize>, String> {
    check_zoom(zoom)?;
    check_point(point, "point")?;
    check_points(points, "vertex")?;
    let mut best: Option<(usize, f64)> = None;
    for (index, vertex) in points.iter().enumerate() {
        let dist = distance(*vertex, point);
        if best.map_or(true, |(_, best_dist)| dist < best_dist) {
            best = Some((index, dist));
        }
    }
    Ok(best.filter(|&(_, dist)| dist <= SELECT_RADIUS / zoom).map(|(index, _)| index))
}

/// `getBounds(vertices, min, max)` of `vertex.d`: exact extent. Empty input is an error
/// because D returns infinite sentinels.
pub fn get_min_max(points: &[Vec2]) -> Result<(Vec2, Vec2), String> {
    if points.is_empty() {
        return Err("bounds need at least one point".to_owned());
    }
    check_points(points, "vertex")?;
    let mut min = [f64::INFINITY; 2];
    let mut max = [f64::NEG_INFINITY; 2];
    for point in points {
        for axis in 0..2 {
            min[axis] = min[axis].min(point[axis]);
            max[axis] = max[axis].max(point[axis]);
        }
    }
    Ok((min, max))
}

fn in_rect(points: impl Iterator<Item = Vec2>, a: Vec2, b: Vec2) -> Result<Vec<usize>, String> {
    check_point(a, "rectangle corner")?;
    check_point(b, "rectangle corner")?;
    let (lo, hi) = ([a[0].min(b[0]), a[1].min(b[1])], [a[0].max(b[0]), a[1].max(b[1])]);
    let mut hits = Vec::new();
    for (index, point) in points.enumerate() {
        check_point(point, "vertex")?;
        if point[0] >= lo[0] && point[1] >= lo[1] && point[0] <= hi[0] && point[1] <= hi[1] {
            hits.push(index);
        }
    }
    Ok(hits)
}

/// `getInRect` for plain positions (group ids are ignored, as for `vec2` in D).
/// Corners may be given in either order; edges are inclusive.
pub fn get_in_rect(points: &[Vec2], a: Vec2, b: Vec2) -> Result<Vec<usize>, String> {
    in_rect(points.iter().copied(), a, b)
}

/// `getInRect` for mesh vertices; `group_id` 0 matches every group.
pub fn get_mesh_vertices_in_rect(
    vertices: &[MeshVertex],
    a: Vec2,
    b: Vec2,
    group_id: u32,
) -> Result<Vec<usize>, String> {
    let hits = in_rect(vertices.iter().map(|v| v.position), a, b)?;
    Ok(hits.into_iter().filter(|&i| vertices[i].matches_group(group_id)).collect())
}

// ------------------------------------------------------------- triangle.d

fn sign(p1: Vec2, p2: Vec2, p3: Vec2) -> f64 {
    (p1[0] - p3[0]) * (p2[1] - p3[1]) - (p2[0] - p3[0]) * (p1[1] - p3[1])
}

// Inclusive of edges. A zero-area triangle reports collinear points as inside.
fn triangle_contains(a: Vec2, b: Vec2, c: Vec2, pt: Vec2) -> bool {
    let d = [sign(pt, a, b), sign(pt, b, c), sign(pt, c, a)];
    !(d.iter().any(|&v| v < 0.0) && d.iter().any(|&v| v > 0.0))
}

/// `isPointInTriangle`; either winding, edges and corners count as inside.
pub fn point_in_triangle(point: Vec2, triangle: [Vec2; 3]) -> Result<bool, String> {
    check_point(point, "point")?;
    check_points(&triangle, "triangle corner")?;
    Ok(triangle_contains(triangle[0], triangle[1], triangle[2], point))
}

/// `getBounds` of `triangle.d`: `[min_x, min_y, max_x, max_y]` floored/ceiled to whole pixels.
pub fn get_bounds(points: &[Vec2]) -> Result<[f64; 4], String> {
    let (min, max) = get_min_max(points)?;
    Ok([min[0].floor(), min[1].floor(), max[0].ceil(), max[1].ceil()])
}

/// `pointInPolygon` (Sunday's algorithm). `edges` holds endpoint pairs, so its length is even.
/// A ray through a shared vertex counts both edges, as in D. Group filtering is
/// `MeshVertex::matches_group`, applied by the caller.
pub fn point_in_polygon(point: Vec2, edges: &[Vec2]) -> Result<bool, String> {
    if edges.len() % 2 != 0 {
        return Err("polygon edges must be endpoint pairs".to_owned());
    }
    check_point(point, "point")?;
    check_points(edges, "polygon vertex")?;
    let mut crossings = 0i64;
    for pair in edges.chunks_exact(2) {
        let (p1, p2) = (pair[0], pair[1]);
        let (low, high) = if p1[1] > p2[1] { (p2, p1) } else { (p1, p2) };
        if !(low[1] <= point[1] && point[1] <= high[1] && low[1] != high[1]) {
            continue;
        }
        let cross_x = p1[0] + (point[1] - p1[1]) / (p2[1] - p1[1]) * (p2[0] - p1[0]);
        if point[0] < cross_x {
            crossings += if p1[1] < p2[1] { 1 } else { -1 };
        }
    }
    Ok(crossings != 0)
}

/// `fillPoly` for one triangle: writes `value` at every pixel whose integer coordinate lies in
/// the triangle. `origin` is the texture's pixel offset (`bounds.xy` in D); `texture` is
/// `width * height` row-major. Pixels outside the texture are skipped; D lets them wrap
/// into the neighbouring row.
pub fn fill_poly<T: Copy>(
    texture: &mut [T],
    width: usize,
    height: usize,
    origin: [i64; 2],
    vertices: &[Vec2],
    triangle: [u32; 3],
    value: T,
) -> Result<(), String> {
    let pixels = width.checked_mul(height).ok_or("texture size overflows")?;
    if texture.len() < pixels {
        return Err(format!("texture holds {} pixels, {width}x{height} needs {pixels}", texture.len()));
    }
    if vertices.len() < 3 {
        return Ok(());
    }
    let mut corners = [[0.0; 2]; 3];
    for (slot, &index) in corners.iter_mut().zip(&triangle) {
        *slot = *vertices
            .get(index as usize)
            .ok_or_else(|| format!("triangle index {index} is past the vertex list"))?;
    }
    check_points(&corners, "triangle corner")?;
    let span = |axis: usize, origin: i64, size: usize| -> Option<(i64, i64)> {
        let lo = corners.iter().map(|c| c[axis]).fold(f64::INFINITY, f64::min).floor();
        let hi = corners.iter().map(|c| c[axis]).fold(f64::NEG_INFINITY, f64::max).ceil();
        let lo = lo.max(origin as f64);
        let hi = hi.min(origin as f64 + size as f64 - 1.0);
        (lo <= hi).then_some((lo as i64, hi as i64))
    };
    let (Some((x0, x1)), Some((y0, y1))) = (span(0, origin[0], width), span(1, origin[1], height)) else {
        return Ok(());
    };
    for y in y0..=y1 {
        for x in x0..=x1 {
            if triangle_contains(corners[0], corners[1], corners[2], [x as f64, y as f64]) {
                texture[(y - origin[1]) as usize * width + (x - origin[0]) as usize] = value;
            }
        }
    }
    Ok(())
}

/// `deformByDeformationBinding` for a `Drawable` target: re-expresses `points` in the
/// barycentric frame of the mesh triangle that contains them (or, outside the mesh, the
/// triangle with the nearest corner), moves them with the deformed mesh and returns the
/// per-point offsets. `flip_horz` mirrors the lookup in x only, the mesh stays put.
/// Fewer than three points, offsets or mesh vertices yields all-zero offsets, as in D.
pub fn transfer_mesh_offsets(
    points: &[Vec2],
    mesh_vertices: &[Vec2],
    mesh_indices: &[u32],
    mesh_offsets: &[Vec2],
    flip_horz: bool,
) -> Result<Vec<Vec2>, String> {
    check_points(points, "point")?;
    check_points(mesh_vertices, "mesh vertex")?;
    check_points(mesh_offsets, "mesh offset")?;
    if mesh_offsets.len() < 3 || points.len() < 3 || mesh_vertices.len() < 3 {
        return Ok(vec![[0.0, 0.0]; points.len()]);
    }
    if mesh_offsets.len() != mesh_vertices.len() {
        return Err("mesh offsets must match the mesh vertices".to_owned());
    }
    if mesh_indices.is_empty() || mesh_indices.len() % 3 != 0 {
        return Err("mesh indices must be a non-empty list of triangles".to_owned());
    }
    if let Some(bad) = mesh_indices.iter().find(|&&i| i as usize >= mesh_vertices.len()) {
        return Err(format!("mesh index {bad} is past the mesh vertices"));
    }
    let deformed: Vec<Vec2> = mesh_vertices
        .iter()
        .zip(mesh_offsets)
        .map(|(v, o)| [v[0] + o[0], v[1] + o[1]])
        .collect();
    let pick = |list: &[Vec2], t: &[u32]| [list[t[0] as usize], list[t[1] as usize], list[t[2] as usize]];
    let corners = |t: &[u32]| pick(mesh_vertices, t);
    let mut out = Vec::with_capacity(points.len());
    for &original in points {
        let pt = if flip_horz { [-original[0], original[1]] } else { original };
        // ponytail: linear scan per point like D; add a grid index if meshes get large.
        let triangle = mesh_indices
            .chunks_exact(3)
            .find(|t| {
                let c = corners(t);
                triangle_contains(c[0], c[1], c[2], pt)
            })
            .or_else(|| nearest_triangle(mesh_vertices, mesh_indices, pt))
            .ok_or("mesh has no triangle")?;
        let [p1, p2, p3] = corners(triangle);
        let (u, v) = if pt == p1 {
            (0.0, 0.0)
        } else {
            let sub = |a: Vec2, b: Vec2| [a[0] - b[0], a[1] - b[1]];
            let (e1, e2, d) = (sub(p2, p1), sub(p3, p1), sub(pt, p1));
            let det = e1[0] * e2[1] - e1[1] * e2[0];
            let (u, v) = ((d[0] * e2[1] - d[1] * e2[0]) / det, (e1[0] * d[1] - e1[1] * d[0]) / det);
            if det == 0.0 || !u.is_finite() || !v.is_finite() {
                return Err("mesh triangle has no area".to_owned());
            }
            (u, v)
        };
        let [q1, q2, q3] = pick(&deformed, triangle);
        let mut moved = [0, 1].map(|k| q1[k] + (q2[k] - q1[k]) * u + (q3[k] - q1[k]) * v);
        if flip_horz {
            moved[0] = -moved[0];
        }
        out.push([moved[0] - original[0], moved[1] - original[1]]);
    }
    Ok(out)
}

// Triangle owning the vertex closest to `pt`; the first one wins ties.
fn nearest_triangle<'a>(vertices: &[Vec2], indices: &'a [u32], pt: Vec2) -> Option<&'a [u32]> {
    let mut best: Option<(&[u32], f64)> = None;
    for triangle in indices.chunks_exact(3) {
        let near = triangle
            .iter()
            .map(|&i| {
                let v = vertices[i as usize];
                (pt[0] - v[0]).powi(2) + (pt[1] - v[1]).powi(2)
            })
            .fold(f64::INFINITY, f64::min);
        if best.map_or(true, |(_, d)| near < d) {
            best = Some((triangle, near));
        }
    }
    best.map(|(t, _)| t)
}

// Bowyer-Watson style incremental Delaunay on a half-edge-less triangle/edge table,
// ported 1:1 from `triangulate` so winding and tie-breaking match D.
struct Mesh {
    vtx: Vec<Vec2>,
    tris: Vec<[usize; 3]>,
    tri2edge: Vec<[usize; 3]>,
    edge2tri: Vec<[usize; 2]>,
}

fn in_circle(pa: Vec2, pb: Vec2, pc: Vec2, pd: Vec2) -> bool {
    let (adx, ady) = (pa[0] - pd[0], pa[1] - pd[1]);
    let (bdx, bdy) = (pb[0] - pd[0], pb[1] - pd[1]);
    let oabd = adx * bdy - bdx * ady;
    if oabd <= 0.0 {
        return false;
    }
    let (cdx, cdy) = (pc[0] - pd[0], pc[1] - pd[1]);
    let ocad = cdx * ady - adx * cdy;
    if ocad <= 0.0 {
        return false;
    }
    let alift = adx * adx + ady * ady;
    let blift = bdx * bdx + bdy * bdy;
    let clift = cdx * cdx + cdy * cdy;
    alift * (bdx * cdy - cdx * bdy) + blift * ocad + clift * oabd > 0.0
}

impl Mesh {
    fn contains(&self, tri: [usize; 3], pt: Vec2) -> bool {
        triangle_contains(self.vtx[tri[0]], self.vtx[tri[1]], self.vtx[tri[2]], pt)
    }

    fn replace_e2t(&mut self, edge: usize, from: usize, to: usize) -> Result<(), String> {
        let e = &mut self.edge2tri[edge];
        if e[0] == from {
            e[0] = to;
            if e[1] == from {
                e[1] = to;
            }
        } else if e[1] == from {
            e[1] = to;
        } else {
            return Err("triangulation edge mismatch".to_owned());
        }
        Ok(())
    }

    // Rotate the triangle so `edge` is its first edge.
    fn orient_tri(&mut self, tri: usize, edge: usize) -> Result<(), String> {
        let (e, p) = (self.tri2edge[tri], self.tris[tri]);
        if e[0] == edge {
        } else if e[1] == edge {
            self.tri2edge[tri] = [e[1], e[2], e[0]];
            self.tris[tri] = [p[1], p[2], p[0]];
        } else if e[2] == edge {
            self.tri2edge[tri] = [e[2], e[0], e[1]];
            self.tris[tri] = [p[2], p[0], p[1]];
        } else {
            return Err("triangulation triangle does not own edge".to_owned());
        }
        Ok(())
    }

    fn split_edges(&mut self) -> Result<(), String> {
        for e in 0..self.edge2tri.len() {
            let tr = self.edge2tri[e];
            if tr[0] != tr[1] {
                continue;
            }
            self.orient_tri(tr[0], e)?;
            let t1 = tr[0];
            let t2 = self.tris.len();
            let [l, r, z] = self.tris[t1];
            let m = self.vtx.len();
            self.vtx.push([(self.vtx[l][0] + self.vtx[r][0]) / 2.0, (self.vtx[l][1] + self.vtx[r][1]) / 2.0]);
            let xe = self.edge2tri.len();
            let me = xe + 1;
            let re = self.tri2edge[t1][1];
            self.tris[t1][1] = m;
            self.tri2edge[t1][1] = me;
            self.tris.push([m, r, z]);
            self.tri2edge.push([xe, re, me]);
            self.edge2tri.push([t2, t2]);
            self.edge2tri.push([t1, t2]);
            self.replace_e2t(re, t1, t2)?;
        }
        Ok(())
    }

    fn insert(&mut self, index: usize, point: Vec2) -> Result<(), String> {
        self.vtx.push(point);
        let Some(a) = (0..self.tris.len()).find(|&a| self.contains(self.tris[a], point)) else {
            return Err(format!("point {index} lies outside the triangulation bounds"));
        };
        let tri = self.tris[a];
        let (b, c) = (self.tris.len(), self.tris.len() + 1);
        let i = self.vtx.len() - 1;
        self.tris[a] = [tri[0], tri[1], i];
        self.tris.push([tri[1], tri[2], i]);
        self.tris.push([tri[2], tri[0], i]);
        let p = self.edge2tri.len();
        let (q, r) = (p + 1, p + 2);
        let [x, y, z] = self.tri2edge[a];
        self.tri2edge[a] = [x, q, p];
        self.tri2edge.push([y, r, q]);
        self.tri2edge.push([z, p, r]);
        self.edge2tri.push([c, a]);
        self.edge2tri.push([a, b]);
        self.edge2tri.push([b, c]);
        self.replace_e2t(y, a, b)?;
        self.replace_e2t(z, a, c)?;

        let mut affected = vec![x, y, z, p, q, r];
        let mut checked = vec![false; self.edge2tri.len()];
        // ponytail: flips terminate in exact arithmetic; the cap only guards float noise.
        let limit = 100 * (self.edge2tri.len() + 16);
        let mut j = 0;
        while j < affected.len() {
            if affected.len() > limit {
                return Err("triangulation did not converge".to_owned());
            }
            let e = affected[j];
            j += 1;
            let t = self.edge2tri[e];
            if t[0] == t[1] {
                continue;
            }
            self.orient_tri(t[0], e)?;
            self.orient_tri(t[1], e)?;
            if self.tris[t[0]][0] != self.tris[t[1]][1] || self.tris[t[1]][0] != self.tris[t[0]][1] {
                return Err("triangulation triangles do not share edge".to_owned());
            }
            let (a, c, d) = (self.tris[t[0]][0], self.tris[t[0]][1], self.tris[t[0]][2]);
            let b = self.tris[t[1]][2];
            if !in_circle(self.vtx[b], self.vtx[a], self.vtx[c], self.vtx[d]) {
                continue;
            }
            let (r, s) = (self.tri2edge[t[0]][1], self.tri2edge[t[0]][2]);
            let (p, q) = (self.tri2edge[t[1]][1], self.tri2edge[t[1]][2]);
            self.tris[t[0]] = [d, b, c];
            self.tris[t[1]] = [b, d, a];
            self.tri2edge[t[0]] = [e, q, r];
            self.tri2edge[t[1]] = [e, s, p];
            self.replace_e2t(q, t[1], t[0])?;
            self.replace_e2t(s, t[0], t[1])?;
            checked[e] = true;
            affected.extend([p, q, r, s].into_iter().filter(|&edge| !checked[edge]));
        }
        Ok(())
    }
}

/// Delaunay triangulation of `points` inside `bounds` (`[min_x, min_y, max_x, max_y]`, for
/// example from `get_bounds`). Returns triangles as indices into `points`, wound as D
/// produces them. Errors on non-finite input, coincident points, or a point outside `bounds`
/// (D silently stops inserting there). Fewer than three non-collinear points give no triangles.
pub fn triangulate(points: &[Vec2], bounds: [f64; 4]) -> Result<Vec<[u32; 3]>, String> {
    check_points(points, "point")?;
    if bounds.iter().any(|v| !v.is_finite()) || bounds[0] > bounds[2] || bounds[1] > bounds[3] {
        return Err("triangulation bounds must be finite with min <= max".to_owned());
    }
    if points.len() > (u32::MAX / 4) as usize {
        return Err("too many points to triangulate".to_owned());
    }
    let mut order: Vec<usize> = (0..points.len()).collect();
    // `+ 0.0` folds -0.0 into 0.0 so equal coordinates sort together.
    let key = |i: usize| (points[i][0] + 0.0, points[i][1] + 0.0);
    order.sort_by(|&a, &b| key(a).0.total_cmp(&key(b).0).then(key(a).1.total_cmp(&key(b).1)));
    if let Some(pair) = order.windows(2).find(|w| key(w[0]) == key(w[1])) {
        return Err(format!("points {} and {} coincide", pair[0].min(pair[1]), pair[0].max(pair[1])));
    }
    if points.is_empty() {
        return Ok(Vec::new());
    }

    // Pad the super-rectangle; the fudge terms keep helper vertices off the input
    // (D works around `contains` instability the same way).
    let range = [bounds[2] - bounds[0], bounds[3] - bounds[1]];
    let min = [bounds[0] - (range[0] + range[1] + 0.123), bounds[1] - (range[1] + range[0] + 0.125)];
    let max = [bounds[2] + (range[0] + range[1] + 0.127), bounds[3] + (range[1] + range[0] + 0.129)];
    let mut mesh = Mesh {
        vtx: vec![[min[0], min[1]], [min[0], max[1]], [max[0], max[1]], [max[0], min[1]]],
        tris: vec![[0, 1, 3], [1, 2, 3]],
        tri2edge: vec![[0, 1, 2], [3, 4, 1]],
        edge2tri: vec![[0, 0], [0, 1], [0, 0], [1, 1], [1, 1]],
    };
    for _ in 0..4 {
        mesh.split_edges()?;
    }
    let drop = mesh.vtx.len();
    for (index, &point) in points.iter().enumerate() {
        mesh.insert(index, point)?;
    }
    Ok(mesh
        .tris
        .iter()
        .filter(|t| t.iter().all(|&i| i >= drop))
        .map(|t| [(t[0] - drop) as u32, (t[1] - drop) as u32, (t[2] - drop) as u32])
        .collect())
}
