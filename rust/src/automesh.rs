//! Automatic mesh generation from an alpha image (grid, contour, optimum and skeleton generators).
//!
//! Coordinate convention (identical to the D automeshers): the input is a row-major alpha image
//! (`Mono`, or the A channel of `Rgba`). A pixel `(x, y)` maps to the mesh position `(x, y) - c`
//! where `c = (width / 2, height / 2)` uses integer division and no half-pixel offset. The y axis
//! points down like the image rows. Mapping to a node's local space (the `textureOffset` of a
//! projectable Part, or the projected-bounds centre of a non-Part provider) is left to the caller.
//! The D generators never emit UVs: the part pipeline derives them from positions on export.
//! Every function returns `Ok(None)` where D leaves the incoming mesh unchanged (empty input,
//! nothing opaque, too few vertices to triangulate a Drawable).

use crate::cv::{
    ApproximationMethod, BinaryMask, DistanceTransform, Point, RetrievalMode, WorkBudget,
};
use crate::document::TextureBlob;
use crate::geometry;
use crate::pixels::{PixelFormat, PixelImage};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::io::Cursor;
use std::path::Path;

type V2 = [f32; 2];

/// Work budget callers can use when they have no better figure.
pub const DEFAULT_WORK_BUDGET: u64 = 400_000_000;

/// Largest scale-array length accepted; keeps grid vertex counts below the 16-bit index limit.
pub const MAX_AXIS_LEN: usize = 255;
const MAX_SCALES: usize = 64;

/// Whether the mesh is for a Drawable (needs triangles) or another Deformable (points only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeshTarget {
    Drawable,
    Deformable,
}

/// Grid axes: `rows` are y positions (D `axes[0]`), `columns` are x positions (D `axes[1]`).
#[derive(Clone, Debug, PartialEq)]
pub struct GridAxes {
    pub rows: Vec<f32>,
    pub columns: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MeshOutput {
    pub vertices: Vec<[f32; 2]>,
    /// Empty for non-Drawable targets. Grid meshes list vertices row-major (`row * columns + col`).
    pub triangles: Vec<[u32; 3]>,
    pub grid: Option<GridAxes>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GridOptions {
    pub mask_threshold: f32,
    pub x_segments: u32,
    pub y_segments: u32,
    pub margin: f32,
    /// Explicit axis scales; `None` derives them from segments and margin like the D defaults.
    pub scale_x: Option<Vec<f32>>,
    pub scale_y: Option<Vec<f32>>,
}

impl Default for GridOptions {
    fn default() -> Self {
        Self {
            mask_threshold: 15.0,
            x_segments: 2,
            y_segments: 2,
            margin: 0.1,
            scale_x: None,
            scale_y: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContourOptions {
    pub sampling_step: f32,
    pub mask_threshold: f32,
    pub min_distance: f32,
    /// Negative means `2 * sampling_step`.
    pub max_distance: f32,
    pub scales: Vec<f32>,
    pub mirror_horizontal: bool,
    /// Mirror axis x, relative to the image centre.
    pub mirror_axis: f32,
}

impl Default for ContourOptions {
    fn default() -> Self {
        Self {
            sampling_step: 32.0,
            mask_threshold: 15.0,
            min_distance: 16.0,
            max_distance: -1.0,
            scales: vec![1.0, 1.1, 0.9, 0.7, 0.4, 0.2, 0.1],
            mirror_horizontal: false,
            mirror_axis: 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OptimumOptions {
    pub large_threshold: f32,
    pub length_threshold: f32,
    pub ratio_threshold: f32,
    pub sharp_expand: f32,
    pub unsharp_expand: f32,
    pub unsharp_contract: f32,
    pub scales: Vec<f32>,
    pub min_distance: f32,
    pub mask_threshold: f32,
    pub div_per_part: f32,
    pub mirror_horizontal: bool,
    /// Mirror axis x, relative to the image centre.
    pub mirror_axis: f32,
}

impl Default for OptimumOptions {
    fn default() -> Self {
        Self {
            large_threshold: 400.0,
            length_threshold: 100.0,
            ratio_threshold: 0.2,
            sharp_expand: 0.01,
            unsharp_expand: 0.05,
            unsharp_contract: 0.05,
            scales: vec![0.5, 0.0],
            min_distance: 10.0,
            mask_threshold: 1.0,
            div_per_part: 12.0,
            mirror_horizontal: false,
            mirror_axis: 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SkeletonOptions {
    pub mask_threshold: f32,
    pub target_point_count: u32,
}

impl Default for SkeletonOptions {
    fn default() -> Self {
        Self {
            mask_threshold: 15.0,
            target_point_count: 10,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Algorithm {
    Grid(GridOptions),
    Contour(ContourOptions),
    Optimum(OptimumOptions),
    Skeleton(SkeletonOptions),
}

fn in_range(name: &str, value: f32, lo: f32, hi: f32) -> Result<(), String> {
    if value.is_finite() && value >= lo && value <= hi {
        Ok(())
    } else {
        Err(format!("{name} must be within {lo}..={hi}, got {value}"))
    }
}

fn check_scales(
    name: &str,
    scales: &[f32],
    lo: f32,
    hi: f32,
    min_len: usize,
    max_len: usize,
) -> Result<(), String> {
    if scales.len() < min_len || scales.len() > max_len {
        return Err(format!(
            "{name} must hold {min_len}..={max_len} values, got {}",
            scales.len()
        ));
    }
    scales.iter().try_for_each(|&s| in_range(name, s, lo, hi))
}

impl GridOptions {
    pub fn validate(&self) -> Result<(), String> {
        in_range("mask_threshold", self.mask_threshold, 1.0, 200.0)?;
        in_range("x_segments", self.x_segments as f32, 2.0, 20.0)?;
        in_range("y_segments", self.y_segments as f32, 2.0, 20.0)?;
        in_range("margin", self.margin, 0.0, 1.0)?;
        if let Some(scales) = &self.scale_x {
            check_scales("scale_x", scales, -1.0, 2.0, 2, MAX_AXIS_LEN)?;
        }
        if let Some(scales) = &self.scale_y {
            check_scales("scale_y", scales, -1.0, 2.0, 2, MAX_AXIS_LEN)?;
        }
        Ok(())
    }

    fn axis_scales(segments: u32, margin: f32) -> Vec<f32> {
        let mut scales = Vec::new();
        if margin != 0.0 {
            scales.push(-margin);
        }
        scales.extend((0..=segments).map(|i| i as f32 / segments as f32));
        if margin != 0.0 {
            scales.push(1.0 + margin);
        }
        scales
    }
}

impl ContourOptions {
    pub fn validate(&self) -> Result<(), String> {
        in_range("sampling_step", self.sampling_step, 1.0, 200.0)?;
        in_range("mask_threshold", self.mask_threshold, 1.0, 200.0)?;
        in_range("min_distance", self.min_distance, 1.0, 200.0)?;
        in_range("max_distance", self.max_distance, -1.0, 200.0)?;
        check_scales("scales", &self.scales, 0.0, 2.0, 0, MAX_SCALES)?;
        in_range("mirror_axis", self.mirror_axis, -1.0e6, 1.0e6)
    }

    /// The D presets: "Normal parts", "Detailed mesh", "Large parts", "Small parts",
    /// "Thin and minimum parts" and "Preserve edges".
    pub fn preset(name: &str) -> Option<Self> {
        let (step, mask, min, scales): (f32, f32, f32, &[f32]) = match name {
            "Normal parts" => (50.0, 15.0, 16.0, &[1.0, 1.1, 0.9, 0.7, 0.4, 0.2, 0.1, 0.0]),
            "Detailed mesh" => (32.0, 15.0, 16.0, &[1.0, 1.1, 0.9, 0.7, 0.4, 0.2, 0.1, 0.0]),
            "Large parts" => (80.0, 15.0, 24.0, &[1.0, 1.1, 0.9, 0.7, 0.4, 0.2, 0.1, 0.0]),
            "Small parts" => (24.0, 15.0, 12.0, &[1.0, 1.1, 0.6, 0.2]),
            "Thin and minimum parts" => (12.0, 1.0, 4.0, &[1.0]),
            "Preserve edges" => (24.0, 15.0, 8.0, &[1.0, 1.2, 0.8]),
            _ => return None,
        };
        Some(Self {
            sampling_step: step,
            mask_threshold: mask,
            min_distance: min,
            max_distance: step * 2.0,
            scales: scales.to_vec(),
            ..Self::default()
        })
    }
}

impl OptimumOptions {
    pub fn validate(&self) -> Result<(), String> {
        in_range("large_threshold", self.large_threshold, 50.0, 2000.0)?;
        in_range("length_threshold", self.length_threshold, 20.0, 2000.0)?;
        in_range("ratio_threshold", self.ratio_threshold, 0.01, 1.0)?;
        in_range("sharp_expand", self.sharp_expand, 0.0, 0.2)?;
        in_range("unsharp_expand", self.unsharp_expand, 0.0, 0.5)?;
        in_range("unsharp_contract", self.unsharp_contract, 0.0, 0.5)?;
        check_scales("scales", &self.scales, 0.0, 2.0, 0, MAX_SCALES)?;
        in_range("min_distance", self.min_distance, 1.0, 200.0)?;
        in_range("mask_threshold", self.mask_threshold, 1.0, 200.0)?;
        in_range("div_per_part", self.div_per_part, 4.0, 64.0)?;
        in_range("mirror_axis", self.mirror_axis, -1.0e6, 1.0e6)
    }

    /// The D presets; same names as [`ContourOptions::preset`].
    pub fn preset(name: &str) -> Option<Self> {
        // (div_per_part, min_distance, scales, large, length, ratio, sharp, expand, contract)
        type Row = (f32, f32, &'static [f32], f32, f32, f32, f32, f32, f32);
        const FULL: &[f32] = &[1.0, 1.1, 0.9, 0.7, 0.4, 0.2, 0.1, 0.0];
        let (mask, row): (f32, Row) = match name {
            "Normal parts" => (
                15.0,
                (12.0, 16.0, FULL, 400.0, 100.0, 0.20, 0.010, 0.050, 0.050),
            ),
            "Detailed mesh" => (
                15.0,
                (16.0, 12.0, FULL, 380.0, 90.0, 0.18, 0.008, 0.045, 0.045),
            ),
            "Large parts" => (
                15.0,
                (8.0, 24.0, FULL, 600.0, 200.0, 0.30, 0.015, 0.080, 0.080),
            ),
            "Small parts" => (
                15.0,
                (
                    18.0,
                    12.0,
                    &[1.0, 1.1, 0.6, 0.2],
                    300.0,
                    80.0,
                    0.15,
                    0.008,
                    0.040,
                    0.040,
                ),
            ),
            "Thin and minimum parts" => (
                1.0,
                (24.0, 4.0, &[1.0], 200.0, 60.0, 0.10, 0.006, 0.030, 0.030),
            ),
            "Preserve edges" => (
                15.0,
                (
                    20.0,
                    8.0,
                    &[1.0, 1.2, 0.8],
                    350.0,
                    80.0,
                    0.15,
                    0.010,
                    0.030,
                    0.030,
                ),
            ),
            _ => return None,
        };
        Some(Self {
            large_threshold: row.3,
            length_threshold: row.4,
            ratio_threshold: row.5,
            sharp_expand: row.6,
            unsharp_expand: row.7,
            unsharp_contract: row.8,
            scales: row.2.to_vec(),
            min_distance: row.1,
            mask_threshold: mask,
            div_per_part: row.0,
            ..Self::default()
        })
    }
}

impl SkeletonOptions {
    pub fn validate(&self) -> Result<(), String> {
        in_range("mask_threshold", self.mask_threshold, 1.0, 200.0)?;
        in_range(
            "target_point_count",
            self.target_point_count as f32,
            2.0,
            128.0,
        )
    }
}

/// Runs the selected generator. `target` is ignored by the grid generator.
pub fn generate(
    image: &PixelImage,
    algorithm: &Algorithm,
    target: MeshTarget,
    budget: &mut WorkBudget,
) -> Result<Option<MeshOutput>, String> {
    match algorithm {
        Algorithm::Grid(options) => grid(image, options, budget),
        Algorithm::Contour(options) => contour(image, options, target, budget),
        Algorithm::Optimum(options) => optimum(image, options, target, budget),
        Algorithm::Skeleton(options) => skeleton(image, options, target, budget),
    }
}

// ---------------------------------------------------------------------------------------------
// vector helpers (f32 like the D vec2)

fn sub(a: V2, b: V2) -> V2 {
    [a[0] - b[0], a[1] - b[1]]
}

fn add(a: V2, b: V2) -> V2 {
    [a[0] + b[0], a[1] + b[1]]
}

fn mul(a: V2, s: f32) -> V2 {
    [a[0] * s, a[1] * s]
}

fn length_squared(a: V2) -> f32 {
    a[0] * a[0] + a[1] * a[1]
}

fn length(a: V2) -> f32 {
    length_squared(a).sqrt()
}

fn sign(value: f32) -> f32 {
    if value > 0.0 {
        1.0
    } else if value < 0.0 {
        -1.0
    } else {
        value
    }
}

/// D `std.algorithm.min`: returns `a` unless `b < a` (so NaN in `a` wins).
fn dmin(a: f64, b: f64) -> f64 {
    if b < a {
        b
    } else {
        a
    }
}

fn centroid(points: &[V2]) -> V2 {
    let sum = points.iter().fold([0.0f32, 0.0], |acc, &p| add(acc, p));
    let n = points.len() as f32;
    [sum[0] / n, sum[1] / n]
}

fn to_v2(points: &[Point]) -> Vec<V2> {
    points.iter().map(|p| [p.x as f32, p.y as f32]).collect()
}

fn center(width: u32, height: u32) -> V2 {
    [(width / 2) as f32, (height / 2) as f32]
}

/// Binarises the alpha plane (`< threshold` is background, like the D `cast(ubyte)` cutoff).
fn binarize(
    image: &PixelImage,
    threshold: f32,
    budget: &mut WorkBudget,
) -> Result<Option<BinaryMask>, String> {
    let [width, height] = image.size();
    if width == 0 || height == 0 {
        return Ok(None);
    }
    let offset = match image.format() {
        PixelFormat::Mono => 0,
        PixelFormat::Rgba => 3,
        PixelFormat::Rgb => return Err("automesh needs an alpha (Mono) or RGBA image".to_owned()),
    };
    budget.charge(width as u64 * height as u64)?;
    let cutoff = threshold as u8;
    let pixels = image
        .pixels()
        .chunks_exact(image.channels())
        .map(|channels| if channels[offset] < cutoff { 0 } else { 255 })
        .collect();
    BinaryMask::new(width, height, pixels).map(Some)
}

// ---------------------------------------------------------------------------------------------
// Delaunay triangulation (port of core/math/triangle.d `triangulate`; the root may deduplicate it
// with the geometry port)

struct Triangulation {
    vtx: Vec<V2>,
    tris: Vec<[usize; 3]>,
    tri2edge: Vec<[usize; 3]>,
    edge2tri: Vec<[usize; 2]>,
}

impl Triangulation {
    fn contains(&self, tri: [usize; 3], pt: V2) -> bool {
        let side = |p1: V2, p2: V2, p3: V2| {
            (p1[0] - p3[0]) * (p2[1] - p3[1]) - (p2[0] - p3[0]) * (p1[1] - p3[1])
        };
        let v = |i: usize| self.vtx[i];
        let d = [
            side(pt, v(tri[0]), v(tri[1])),
            side(pt, v(tri[1]), v(tri[2])),
            side(pt, v(tri[2]), v(tri[0])),
        ];
        !(d.iter().any(|&x| x < 0.0) && d.iter().any(|&x| x > 0.0))
    }

    fn replace_e2t(&mut self, edge: usize, from: usize, to: usize) -> Result<(), String> {
        let e2t = &mut self.edge2tri[edge];
        if e2t[0] == from {
            e2t[0] = to;
            if e2t[1] == from {
                e2t[1] = to;
            }
        } else if e2t[1] == from {
            e2t[1] = to;
        } else {
            return Err("triangulation edge mismatch".to_owned());
        }
        Ok(())
    }

    fn orient_tri(&mut self, tri: usize, edge: usize) -> Result<(), String> {
        let t2e = self.tri2edge[tri];
        let pt = self.tris[tri];
        if t2e[0] == edge {
        } else if t2e[1] == edge {
            self.tri2edge[tri] = [t2e[1], t2e[2], t2e[0]];
            self.tris[tri] = [pt[1], pt[2], pt[0]];
        } else if t2e[2] == edge {
            self.tri2edge[tri] = [t2e[2], t2e[0], t2e[1]];
            self.tris[tri] = [pt[2], pt[0], pt[1]];
        } else {
            return Err("triangulation triangle does not own edge".to_owned());
        }
        Ok(())
    }

    fn split_edges(&mut self) -> Result<(), String> {
        let edge_count = self.edge2tri.len();
        for e in 0..edge_count {
            let tr = self.edge2tri[e];
            if tr[0] != tr[1] {
                continue;
            }
            self.orient_tri(tr[0], e)?;
            let t1 = tr[0];
            let t2 = self.tris.len();
            let [l, r, z] = self.tris[t1];
            let m = self.vtx.len();
            self.vtx.push([
                (self.vtx[l][0] + self.vtx[r][0]) / 2.0,
                (self.vtx[l][1] + self.vtx[r][1]) / 2.0,
            ]);
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
}

fn in_circle(pa: V2, pb: V2, pc: V2, pd: V2) -> bool {
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

/// Incremental Delaunay triangulation of `vertices` inside `bounds = [min_x, min_y, max_x, max_y]`.
/// Returns triangle indices into `vertices`.
fn triangulate(
    vertices: &[V2],
    bounds: [f32; 4],
    budget: &mut WorkBudget,
) -> Result<Vec<[u32; 3]>, String> {
    if bounds.iter().any(|b| !b.is_finite())
        || vertices
            .iter()
            .any(|v| !v[0].is_finite() || !v[1].is_finite())
    {
        return Err("triangulation needs finite coordinates".to_owned());
    }
    if vertices.len() >= u32::MAX as usize / 4 {
        return Err("too many vertices to triangulate".to_owned());
    }
    let mut min = [bounds[0], bounds[1]];
    let mut max = [bounds[2], bounds[3]];
    let range = [max[0] - min[0], max[1] - min[1]];
    // Fudge factors keep the super rectangle off exact vertex coordinates, as in D.
    min[0] -= range[0] + range[1] + 0.123;
    min[1] -= range[1] + range[0] + 0.125;
    max[0] += range[0] + range[1] + 0.127;
    max[1] += range[1] + range[0] + 0.129;
    let mut m = Triangulation {
        vtx: vec![
            [min[0], min[1]],
            [min[0], max[1]],
            [max[0], max[1]],
            [max[0], min[1]],
        ],
        tris: vec![[0, 1, 3], [1, 2, 3]],
        tri2edge: vec![[0, 1, 2], [3, 4, 1]],
        edge2tri: vec![[0, 0], [0, 1], [0, 0], [1, 1], [1, 1]],
    };
    for _ in 0..4 {
        m.split_edges()?;
    }
    let dropped = m.vtx.len();
    for (n, &pt) in vertices.iter().enumerate() {
        let i = n + dropped;
        m.vtx.push(pt);
        budget.charge(m.tris.len() as u64 + 1)?;
        let Some(a) = m.tris.iter().position(|&tri| m.contains(tri, pt)) else {
            return Err("vertex lies outside the triangulation bounds".to_owned());
        };
        let tri = m.tris[a];
        let (b, c) = (m.tris.len(), m.tris.len() + 1);
        m.tris[a] = [tri[0], tri[1], i];
        m.tris.push([tri[1], tri[2], i]);
        m.tris.push([tri[2], tri[0], i]);
        let (p, q, r) = (m.edge2tri.len(), m.edge2tri.len() + 1, m.edge2tri.len() + 2);
        let [x, y, z] = m.tri2edge[a];
        m.tri2edge[a] = [x, q, p];
        m.tri2edge.push([y, r, q]);
        m.tri2edge.push([z, p, r]);
        m.edge2tri.push([c, a]);
        m.edge2tri.push([a, b]);
        m.edge2tri.push([b, c]);
        m.replace_e2t(y, a, b)?;
        m.replace_e2t(z, a, c)?;
        let mut affected = vec![x, y, z, p, q, r];
        let mut checked = vec![false; m.edge2tri.len()];
        let mut j = 0;
        while j < affected.len() {
            budget.charge(1)?;
            let e = affected[j];
            j += 1;
            let t = m.edge2tri[e];
            if t[0] == t[1] {
                continue;
            }
            m.orient_tri(t[0], e)?;
            m.orient_tri(t[1], e)?;
            if m.tris[t[0]][0] != m.tris[t[1]][1] || m.tris[t[1]][0] != m.tris[t[0]][1] {
                return Err("triangulation triangles do not share an edge".to_owned());
            }
            let (a, c, d, b) = (
                m.tris[t[0]][0],
                m.tris[t[0]][1],
                m.tris[t[0]][2],
                m.tris[t[1]][2],
            );
            if !in_circle(m.vtx[b], m.vtx[a], m.vtx[c], m.vtx[d]) {
                continue;
            }
            let (r, s) = (m.tri2edge[t[0]][1], m.tri2edge[t[0]][2]);
            let (p, q) = (m.tri2edge[t[1]][1], m.tri2edge[t[1]][2]);
            m.tris[t[0]] = [d, b, c];
            m.tris[t[1]] = [b, d, a];
            m.tri2edge[t[0]] = [e, q, r];
            m.tri2edge[t[1]] = [e, s, p];
            m.replace_e2t(q, t[1], t[0])?;
            m.replace_e2t(s, t[0], t[1])?;
            checked[e] = true;
            for edge in [p, q, r, s] {
                if !checked[edge] {
                    affected.push(edge);
                }
            }
        }
    }
    Ok(m.tris
        .iter()
        .filter(|t| t.iter().all(|&v| v >= dropped))
        .map(|t| {
            [
                (t[0] - dropped) as u32,
                (t[1] - dropped) as u32,
                (t[2] - dropped) as u32,
            ]
        })
        .collect())
}

/// D `IncMesh.autoTriangulate`: bounds are the plain vertex min/max; fewer than 3 vertices keep the
/// original mesh (reported as `None`).
fn auto_triangulate(
    vertices: Vec<V2>,
    budget: &mut WorkBudget,
) -> Result<Option<MeshOutput>, String> {
    if vertices.len() < 3 {
        return Ok(None);
    }
    let mut bounds = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for v in &vertices {
        bounds[0] = bounds[0].min(v[0]);
        bounds[1] = bounds[1].min(v[1]);
        bounds[2] = bounds[2].max(v[0]);
        bounds[3] = bounds[3].max(v[1]);
    }
    let triangles = triangulate(&vertices, bounds, budget)?;
    Ok(Some(MeshOutput {
        vertices,
        triangles,
        grid: None,
    }))
}

/// Shared tail for the point generators: Drawables are triangulated, others keep points only.
fn finish_points(
    vertices: Vec<V2>,
    target: MeshTarget,
    budget: &mut WorkBudget,
) -> Result<Option<MeshOutput>, String> {
    match target {
        MeshTarget::Drawable => auto_triangulate(vertices, budget),
        MeshTarget::Deformable if vertices.is_empty() => Ok(None),
        MeshTarget::Deformable => Ok(Some(MeshOutput {
            vertices,
            triangles: Vec::new(),
            grid: None,
        })),
    }
}

// ---------------------------------------------------------------------------------------------
// grid

pub fn grid(
    image: &PixelImage,
    options: &GridOptions,
    budget: &mut WorkBudget,
) -> Result<Option<MeshOutput>, String> {
    options.validate()?;
    let Some(mask) = binarize(image, options.mask_threshold, budget)? else {
        return Ok(None);
    };
    let [width, height] = mask.size();
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (width as i64, height as i64, -1i64, -1i64);
    for (index, &pixel) in mask.pixels().iter().enumerate() {
        if pixel > 0 {
            let (x, y) = (
                (index % width as usize) as i64,
                (index / width as usize) as i64,
            );
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    // D builds collapsed zero-area triangles for a one-pixel-wide shape; report nothing instead.
    if max_x <= min_x || max_y <= min_y {
        return Ok(None);
    }
    let c = center(width, height);
    let (min_x, min_y, max_x, max_y) = (min_x as f32, min_y as f32, max_x as f32, max_y as f32);
    let sorted = |explicit: &Option<Vec<f32>>, segments: u32| {
        let mut scales = explicit
            .clone()
            .unwrap_or_else(|| GridOptions::axis_scales(segments, options.margin));
        scales.sort_by(|a, b| a.total_cmp(b));
        scales
    };
    let rows: Vec<f32> = sorted(&options.scale_y, options.y_segments)
        .iter()
        .map(|&y| (min_y * (1.0 - y) + max_y * y) - c[1])
        .collect();
    let columns: Vec<f32> = sorted(&options.scale_x, options.x_segments)
        .iter()
        .map(|&x| (min_x * (1.0 - x) + max_x * x) - c[0])
        .collect();
    let mut vertices = Vec::with_capacity(rows.len() * columns.len());
    for &y in &rows {
        for &x in &columns {
            vertices.push([x, y]);
        }
    }
    // D swaps width and height here (and so the centre); kept because it picks the diagonals.
    let span_y = rows[rows.len() - 1] - rows[0];
    let span_x = columns[columns.len() - 1] - columns[0];
    let centre = [columns[0] + span_y / 2.0, rows[0] + span_x / 2.0];
    let at = |col: usize, row: usize| (row * columns.len() + col) as u32;
    let mut triangles = Vec::with_capacity((rows.len() - 1) * (columns.len() - 1) * 2);
    for i in 0..rows.len() - 1 {
        for j in 0..columns.len() - 1 {
            let (x_value, y_value) = (columns[j], rows[i]);
            let (i0, i1, i2, i3) = (at(j, i), at(j, i + 1), at(j + 1, i), at(j + 1, i + 1));
            if (x_value < centre[0] && y_value < centre[1])
                || (x_value >= centre[0] && y_value >= centre[1])
            {
                triangles.extend([[i0, i2, i3], [i0, i3, i1]]);
            } else {
                triangles.extend([[i0, i1, i2], [i1, i2, i3]]);
            }
        }
    }
    Ok(Some(MeshOutput {
        vertices,
        triangles,
        grid: Some(GridAxes { rows, columns }),
    }))
}

// ---------------------------------------------------------------------------------------------
// contour

/// Contour sampling used by the contour generator (with optional left/right mirroring).
fn resample_mirrored(contour: &[V2], rate: f64, mirror: bool, axis: f32) -> Vec<V2> {
    let mut base = 0usize;
    if mirror {
        let mut min_distance = -1.0f32;
        for (i, v) in contour.iter().enumerate() {
            if min_distance < 0.0 || v[0] - axis < min_distance {
                base = i;
                min_distance = v[0] - axis;
            }
        }
    }
    let mut sampled = vec![contour[base]];
    let mut side = 0.0f32;
    for idx in 1..contour.len() {
        let prev = sampled[sampled.len() - 1];
        let c = contour[(idx + base) % contour.len()];
        if length_squared(sub(c, prev)) as f64 > rate * rate {
            if mirror {
                if side == 0.0 {
                    side = sign(c[0] - axis);
                } else if sign(c[0] - axis) != side {
                    continue;
                }
            }
            sampled.push(c);
        }
    }
    sampled
}

pub fn contour(
    image: &PixelImage,
    options: &ContourOptions,
    target: MeshTarget,
    budget: &mut WorkBudget,
) -> Result<Option<MeshOutput>, String> {
    options.validate()?;
    let Some(mask) = binarize(image, options.mask_threshold, budget)? else {
        return Ok(None);
    };
    let [width, height] = mask.size();
    let c = center(width, height);
    let max_distance = if options.max_distance < 0.0 {
        options.sampling_step * 2.0
    } else {
        options.max_distance
    };
    let found = mask.contours(RetrievalMode::External, ApproximationMethod::Simple, budget)?;
    let axis = c[0] + options.mirror_axis;
    let mut vertices: Vec<V2> = Vec::new();
    for points in &found.points {
        if points.is_empty() {
            continue;
        }
        let contour = to_v2(points);
        let moment = centroid(&contour);
        for &scale in &options.scales {
            let scale = scale as f64;
            let by_scale = if scale > 0.0 {
                options.sampling_step as f64 / (scale * scale)
            } else {
                1.0
            };
            let rate = dmin(max_distance as f64 / scale, by_scale);
            budget.charge(contour.len() as u64)?;
            let sampled = resample_mirrored(&contour, rate, options.mirror_horizontal, axis);
            for p in sampled {
                let scaled = add(mul(sub(p, moment), scale as f32), moment);
                let local = sub(scaled, c);
                if !vertices.is_empty() {
                    budget.charge(vertices.len() as u64)?;
                    let nearest = vertices
                        .iter()
                        .map(|&v| length(sub(local, v)))
                        .fold(f32::INFINITY, |a, b| if a < b { a } else { b });
                    if nearest <= options.min_distance {
                        continue;
                    }
                }
                vertices.push(local);
            }
        }
    }
    finish_points(vertices, target, budget)
}

// ---------------------------------------------------------------------------------------------
// skeleton

fn triangle_area(a: Point, b: Point, c: Point) -> f64 {
    let (ax, ay, bx, by, cx, cy) = (
        a.x as f64, a.y as f64, b.x as f64, b.y as f64, c.x as f64, c.y as f64,
    );
    (ax * (by - cy) + bx * (cy - ay) + cx * (ay - by)).abs() / 2.0
}

/// Visvalingam-Whyatt style reduction keeping both endpoints.
fn simplify_by_target_count(
    points: &[Point],
    target: usize,
    budget: &mut WorkBudget,
) -> Result<Vec<Point>, String> {
    let mut simplified = points.to_vec();
    while simplified.len() > target && simplified.len() >= 3 {
        budget.charge(simplified.len() as u64)?;
        let mut remove = 1;
        let mut min_area = triangle_area(simplified[0], simplified[1], simplified[2]);
        for i in 2..simplified.len() - 1 {
            let area = triangle_area(simplified[i - 1], simplified[i], simplified[i + 1]);
            if area < min_area {
                min_area = area;
                remove = i;
            }
        }
        simplified.remove(remove);
    }
    Ok(simplified)
}

pub fn skeleton(
    image: &PixelImage,
    options: &SkeletonOptions,
    target: MeshTarget,
    budget: &mut WorkBudget,
) -> Result<Option<MeshOutput>, String> {
    options.validate()?;
    let Some(mask) = binarize(image, options.mask_threshold, budget)? else {
        return Ok(None);
    };
    let [width, height] = mask.size();
    let path = mask.thin(budget)?.longest_path(budget)?;
    let control = simplify_by_target_count(&path, options.target_point_count as usize, budget)?;
    let c = center(width, height);
    let vertices = control
        .iter()
        .map(|p| sub([p.x as f32, p.y as f32], c))
        .collect();
    finish_points(vertices, target, budget)
}

// ---------------------------------------------------------------------------------------------
// optimum

/// Mean skeleton width statistics of one region: (path pixels with positive distance, sum of 2*dt).
fn region_width(
    mask: &BinaryMask,
    region: &[Point],
    budget: &mut WorkBudget,
) -> Result<(u64, f64), String> {
    let [width, _] = mask.size();
    let x0 = region.iter().map(|p| p.x).min().unwrap_or(0);
    let x1 = region.iter().map(|p| p.x).max().unwrap_or(0);
    let y0 = region.iter().map(|p| p.y).min().unwrap_or(0);
    let y1 = region.iter().map(|p| p.y).max().unwrap_or(0);
    if x1 >= width || y1 >= mask.size()[1] {
        return Err("contour lies outside the mask".to_owned());
    }
    let (rw, rh) = (x1 - x0 + 1, y1 - y0 + 1);
    let mut data = Vec::with_capacity(rw as usize * rh as usize);
    for y in y0..=y1 {
        let start = y as usize * width as usize + x0 as usize;
        data.extend_from_slice(&mask.pixels()[start..start + rw as usize]);
    }
    let region_mask = BinaryMask::new(rw, rh, data)?;
    let path = region_mask.thin(budget)?.longest_path(budget)?;
    let DistanceTransform::Field(field) = region_mask.distance_transform(budget)? else {
        return Ok((0, 0.0));
    };
    let (mut count, mut sum) = (0u64, 0.0f64);
    for p in path {
        if p.x < rw && p.y < rh {
            let distance = field.samples()[p.y as usize * rw as usize + p.x as usize].distance;
            if distance > 0.0 {
                count += 1;
                sum += (2.0 * distance) as f64;
            }
        }
    }
    Ok((count, sum))
}

/// Concatenates contours, always continuing with the unvisited contour whose first point is nearest.
fn chain_contours(contours: &[Vec<Point>], budget: &mut WorkBudget) -> Result<Vec<V2>, String> {
    let mut visited = vec![false; contours.len()];
    let mut result = Vec::new();
    let mut next = if contours.is_empty() {
        None
    } else {
        Some(0usize)
    };
    let mut done = 0usize;
    while let Some(index) = next {
        if done >= contours.len() {
            break;
        }
        visited[index] = true;
        done += 1;
        let contour = &contours[index];
        result.extend(to_v2(contour));
        budget.charge(contours.len() as u64)?;
        let Some(&last) = contour.last() else {
            break;
        };
        let mut best: Option<(usize, f32)> = None;
        for (i, other) in contours.iter().enumerate() {
            if visited[i] || other.is_empty() {
                continue;
            }
            let dy = last.y as f32 - other[0].y as f32;
            let dx = last.x as f32 - other[0].x as f32;
            let distance = length([dy, dx]);
            if best.map_or(distance < f32::INFINITY, |(_, d)| distance < d) {
                best = Some((i, distance));
            }
        }
        next = best.map(|(i, _)| i);
    }
    Ok(result)
}

fn resample(contour: &[V2], rate: f64) -> Vec<V2> {
    let mut sampled = vec![contour[0]];
    for &c in &contour[1..] {
        let prev = sampled[sampled.len() - 1];
        if length_squared(sub(c, prev)) as f64 > rate * rate {
            sampled.push(c);
        }
    }
    sampled
}

fn normal_vector(p1: V2, p2: V2) -> V2 {
    let (vx, vy) = (p2[1] - p1[1], p1[0] - p2[0]);
    let norm = (vx * vx + vy * vy).sqrt();
    if norm == 0.0 {
        [0.0, 0.0]
    } else {
        [vx / norm, vy / norm]
    }
}

/// Moves every point along the normal of its neighbours' chord by `distance` (negative contracts).
fn offset_along_normal(points: &[V2], distance: f32) -> Vec<V2> {
    let n = points.len();
    if n < 3 {
        return points.to_vec();
    }
    let mut moved: Vec<V2> = (0..n)
        .map(|i| {
            let normal = normal_vector(points[(i + n - 1) % n], points[(i + 1) % n]);
            [
                points[i][0] + normal[0] * distance,
                points[i][1] + normal[1] * distance,
            ]
        })
        .collect();
    moved.dedup();
    moved
}

fn scaled_about_centroid(points: &[V2], scale: f32) -> Vec<V2> {
    let c = centroid(points);
    let mut scaled: Vec<V2> = points
        .iter()
        .map(|p| [c[0] + scale * (p[0] - c[0]), c[1] + scale * (p[1] - c[1])])
        .collect();
    scaled.dedup();
    scaled
}

fn sampled_at_rate(points: &[V2], rate: f32) -> Vec<V2> {
    let mut flag = 0.0f32;
    let mut sampled = Vec::new();
    for &p in points {
        if flag <= 0.0 {
            sampled.push(p);
            flag += 1.0;
        }
        flag -= rate;
    }
    sampled
}

fn horizontal_mirrored(
    points: &[V2],
    axis: f32,
    budget: &mut WorkBudget,
) -> Result<Vec<V2>, String> {
    let mut side = 0.0f32;
    let mut mirrored: Vec<V2> = Vec::new();
    let mut used = HashSet::new();
    for &c in points {
        if side == 0.0 {
            side = sign(c[0] - axis);
            mirrored.push(c);
        } else if sign(c[0] - axis) != side {
            let flipped = [axis * 2.0 - c[0], c[1]];
            budget.charge(points.len() as u64)?;
            let mut index = 0;
            let mut best = f32::INFINITY;
            for (i, p) in points.iter().enumerate() {
                let d = length_squared(sub(*p, flipped));
                if d < best {
                    best = d;
                    index = i;
                }
            }
            if used.insert(index) {
                mirrored.push([axis * 2.0 - points[index][0], points[index][1]]);
            }
        } else {
            mirrored.push(c);
        }
    }
    mirrored.dedup();
    Ok(mirrored)
}

/// D `fillPoly` for one triangle: clears the mask pixels the triangle covers (clipped to the mask).
fn fill_poly(
    mask: &mut [u8],
    width: u32,
    height: u32,
    vertices: &[V2],
    tri: [usize; 3],
    budget: &mut WorkBudget,
) -> Result<(), String> {
    let Some(corners) = tri.map(|i| vertices.get(i)).into_iter().collect::<Option<Vec<_>>>() else {
        return Ok(());
    };
    let corners: Vec<geometry::Vec2> = corners
        .iter()
        .map(|v| [f64::from(v[0]), f64::from(v[1])])
        .collect();
    let span = |axis: usize, size: u32| {
        let lo = corners.iter().map(|c| c[axis]).fold(f64::MAX, f64::min).floor().max(0.0);
        let hi = corners.iter().map(|c| c[axis]).fold(f64::MIN, f64::max).ceil().min(f64::from(size) - 1.0);
        (hi - lo + 1.0).max(0.0) as u64
    };
    budget.charge(span(0, width) * span(1, height))?;
    geometry::fill_poly(mask, width as usize, height as usize, [0, 0], &corners, [0, 1, 2], 0u8)
}

/// One round of D `completeUncoveredArea`: adds the contour point whose neighbourhood has the most
/// opaque pixels no triangle covers yet, if that actually reduces the uncovered area.
fn complete_uncovered_area(
    uncovered: &mut [u8],
    size: [u32; 2],
    mesh: &mut (Vec<V2>, Vec<[u32; 3]>),
    contour: &[V2],
    min_distance: f32,
    budget: &mut WorkBudget,
) -> Result<bool, String> {
    let [width, height] = size;
    let (w, h) = (width as usize, height as usize);
    let bounds = [0.0, 0.0, width as f32, height as f32];
    let tri_index = |t: &[u32; 3]| [t[0] as usize, t[1] as usize, t[2] as usize];
    for tri in &mesh.1 {
        fill_poly(uncovered, width, height, &mesh.0, tri_index(tri), budget)?;
    }
    budget.charge(w as u64 * h as u64)?;
    let stride = w + 1;
    let mut sat = vec![0i32; (h + 1) * stride];
    for y in 0..h {
        let mut row_sum = 0;
        for x in 0..w {
            row_sum += i32::from(uncovered[y * w + x] != 0);
            sat[(y + 1) * stride + x + 1] = sat[y * stride + x + 1] + row_sum;
        }
    }
    let initial_remaining = sat[h * stride + w];
    if initial_remaining == 0 {
        return Ok(false);
    }
    let min_dist = min_distance * 0.5;
    let min_dist_sq = min_dist * min_dist;
    let mut grid_size = min_dist as i32;
    if (grid_size as f32) < min_dist {
        grid_size += 1;
    }
    let grid_size = grid_size.max(1);
    let (grid_w, grid_h) = (
        (width as i32 + grid_size - 1) / grid_size,
        (height as i32 + grid_size - 1) / grid_size,
    );
    let mut cells: Vec<Vec<V2>> = vec![Vec::new(); grid_w as usize * grid_h as usize];
    for v in &mesh.0 {
        let (gx, gy) = (
            (v[0] / grid_size as f32).floor() as i32,
            (v[1] / grid_size as f32).floor() as i32,
        );
        if gx >= 0 && gy >= 0 && gx < grid_w && gy < grid_h {
            cells[(gy * grid_w + gx) as usize].push(*v);
        }
    }
    let mut candidates: Vec<V2> = Vec::new();
    for &p in contour {
        let (x, y) = (p[0].round() as i32, p[1].round() as i32);
        if x < 0 || x >= width as i32 || y < 0 || y >= height as i32 {
            continue;
        }
        if uncovered[y as usize * w + x as usize] != 255 {
            continue;
        }
        let (gx, gy) = (
            (p[0] / grid_size as f32).floor() as i32,
            (p[1] / grid_size as f32).floor() as i32,
        );
        if gx < 0 || gy < 0 || gx >= grid_w || gy >= grid_h {
            continue;
        }
        let mut skip = false;
        'cells: for ny in (gy - 1).max(0)..=(gy + 1).min(grid_h - 1) {
            for nx in (gx - 1).max(0)..=(gx + 1).min(grid_w - 1) {
                let cell = &cells[(ny * grid_w + nx) as usize];
                budget.charge(cell.len() as u64 + 1)?;
                if cell.iter().any(|v| {
                    (v[0] - p[0]) * (v[0] - p[0]) + (v[1] - p[1]) * (v[1] - p[1]) < min_dist_sq
                }) {
                    skip = true;
                    break 'cells;
                }
            }
        }
        if !skip {
            candidates.push(p);
        }
    }
    if candidates.is_empty() {
        return Ok(false);
    }
    let window = min_distance as i32;
    let (mut best_score, mut best) = (-1i32, candidates[0]);
    for &p in &candidates {
        let (x, y) = (p[0].round() as i32, p[1].round() as i32);
        let (x1, y1) = ((x - window).max(0) as usize, (y - window).max(0) as usize);
        let (x2, y2) = (
            (x + window).min(width as i32 - 1) as usize + 1,
            (y + window).min(height as i32 - 1) as usize + 1,
        );
        let score = sat[y2 * stride + x2] - sat[y1 * stride + x2] - sat[y2 * stride + x1]
            + sat[y1 * stride + x1];
        if score > best_score {
            best_score = score;
            best = p;
        }
    }
    let mut vertices = mesh.0.clone();
    vertices.push(best);
    let triangles = triangulate(&vertices, bounds, budget)?;
    if triangles.is_empty() {
        return Ok(false);
    }
    for tri in &triangles {
        fill_poly(uncovered, width, height, &vertices, tri_index(tri), budget)?;
    }
    let remaining = uncovered.iter().filter(|&&v| v != 0).count() as i32;
    if remaining < initial_remaining {
        *mesh = (vertices, triangles);
        return Ok(true);
    }
    Ok(false)
}

pub fn optimum(
    image: &PixelImage,
    options: &OptimumOptions,
    target: MeshTarget,
    budget: &mut WorkBudget,
) -> Result<Option<MeshOutput>, String> {
    options.validate()?;
    let Some(mask) = binarize(image, options.mask_threshold, budget)? else {
        return Ok(None);
    };
    let [tex_w, tex_h] = mask.size();
    let axis = if options.mirror_horizontal {
        options.mirror_axis + (tex_w / 2) as f32
    } else {
        options.mirror_axis
    };
    let c = center(tex_w, tex_h);
    let size_avg = ((tex_w + tex_h) as f64 / 2.0) as f32;
    let min_distance = (tex_w.max(tex_h) as f32 / options.div_per_part).max(options.min_distance);

    let found = mask.contours(RetrievalMode::External, ApproximationMethod::Simple, budget)?;
    let (mut sum_width, mut path_len) = (0.0f64, 0.0f64);
    for region in &found.points {
        if region.is_empty() {
            continue;
        }
        let (count, sum) = region_width(&mask, region, budget)?;
        path_len += count as f64;
        sum_width += sum;
    }
    let contour_vec = chain_contours(&found.points, budget)?;
    if contour_vec.len() < 3 {
        return Ok(None);
    }
    let avg_width = if path_len > 0.0 {
        sum_width / path_len
    } else {
        0.0
    };
    let width_length_ratio = if path_len > 0.0 {
        avg_width / path_len
    } else {
        f64::INFINITY
    };
    let sharp = avg_width < options.large_threshold as f64
        && (path_len < options.length_threshold as f64
            || width_length_ratio < options.ratio_threshold as f64);

    let base = resample(&contour_vec, min_distance as f64);
    let mut vertices: Vec<V2>;
    if sharp {
        vertices = offset_along_normal(&base, size_avg * options.sharp_expand);
    } else {
        vertices = base.clone();
        vertices.extend(offset_along_normal(
            &base,
            size_avg * options.unsharp_expand,
        ));
        vertices.extend(offset_along_normal(
            &base,
            -(size_avg * options.unsharp_contract),
        ));
        for &scale in &options.scales {
            let mut ring = sampled_at_rate(&scaled_about_centroid(&base, scale), scale);
            if options.mirror_horizontal {
                ring = horizontal_mirrored(&ring, axis, budget)?;
            }
            vertices.extend(ring);
        }
        vertices.dedup();
    }

    let bounds = [0.0, 0.0, tex_w as f32, tex_h as f32];
    let triangles = triangulate(&vertices, bounds, budget)?;
    let mut mesh = (vertices, triangles);
    let mut uncovered: Vec<u8> = mask
        .pixels()
        .iter()
        .map(|&p| if p != 0 { 255 } else { 0 })
        .collect();
    for _ in 0..5 {
        if !complete_uncovered_area(
            &mut uncovered,
            [tex_w, tex_h],
            &mut mesh,
            &contour_vec,
            min_distance,
            budget,
        )? {
            break;
        }
    }
    let (vertices, triangles) = mesh;
    if vertices.is_empty() || (target == MeshTarget::Drawable && vertices.len() < 3) {
        return Ok(None);
    }
    let vertices = vertices.iter().map(|&v| sub(v, c)).collect();
    let triangles = if target == MeshTarget::Drawable {
        triangles
    } else {
        Vec::new()
    };
    Ok(Some(MeshOutput {
        vertices,
        triangles,
        grid: None,
    }))
}

// ---------------------------------------------------------------------------------------------
// Session operation `automesh.generate`: D `ApplyAutoMeshPT` for one Part.
//
// The command calls the processor with mirroring off, so the request has no mirror fields. D
// re-applies the edited mesh through `applyMeshToTarget`; deformation bindings, welds and depth
// payloads are transferred there, which is not ported: such Parts are rejected instead.

/// Outcome of [`generate_for_part`]. `candidate` is `None` when the generators produced no mesh.
pub(crate) struct PartMeshResult {
    pub candidate: Option<Value>,
    pub summary: Value,
}

const NO_TEXTURE: u64 = u32::MAX as u64;

fn number_field(request: &Value, key: &str, lo: f32, hi: f32) -> Result<Option<f32>, String> {
    let Some(value) = request.get(key).filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let number = value
        .as_f64()
        .filter(|n| n.is_finite())
        .ok_or_else(|| format!("{key} must be a finite number"))?;
    Ok(Some((number as f32).clamp(lo, hi)))
}

fn array_field(
    request: &Value,
    key: &str,
    lo: f32,
    hi: f32,
    max_len: usize,
) -> Result<Option<Vec<f32>>, String> {
    let Some(value) = request.get(key).filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let items = value
        .as_array()
        .ok_or_else(|| format!("{key} must be an array of numbers"))?;
    if items.len() > max_len {
        return Err(format!("{key} holds at most {max_len} values"));
    }
    items
        .iter()
        .map(|item| {
            let number = item
                .as_f64()
                .filter(|n| n.is_finite())
                .ok_or_else(|| format!("{key} must be an array of finite numbers"))?;
            Ok((number as f32).clamp(lo, hi))
        })
        .collect::<Result<Vec<_>, String>>()
        .map(Some)
}

/// Builds the algorithm from the request like D `applyPreset` followed by `writeValues`: the
/// preset is applied first, then every present field is clamped to its documented range.
fn algorithm_from_request(request: &Value) -> Result<Algorithm, String> {
    let name = request
        .get("algorithm")
        .and_then(Value::as_str)
        .ok_or("algorithm must be one of grid, contour, optimum, skeleton")?;
    let preset = match request.get("preset") {
        None | Some(Value::Null) => None,
        Some(Value::String(preset)) => Some(preset.as_str()),
        Some(_) => return Err("preset must be a string".to_owned()),
    };
    let unknown_preset = |preset: &str| format!("unknown {name} preset {preset:?}");
    let no_presets = || format!("the {name} algorithm has no presets");
    let n = |key: &str, lo: f32, hi: f32| number_field(request, key, lo, hi);
    let segments = |key: &str| n(key, 2.0, 20.0).map(|v| v.map(|v| v as u32));
    let algorithm = match name {
        "grid" => {
            if preset.is_some() {
                return Err(no_presets());
            }
            let mut o = GridOptions::default();
            o.mask_threshold = n("mask_threshold", 1.0, 200.0)?.unwrap_or(o.mask_threshold);
            o.x_segments = segments("x_segments")?.unwrap_or(o.x_segments);
            o.y_segments = segments("y_segments")?.unwrap_or(o.y_segments);
            o.margin = n("margin", 0.0, 1.0)?.unwrap_or(o.margin);
            o.scale_x = array_field(request, "scale_x", -1.0, 2.0, MAX_AXIS_LEN)?;
            o.scale_y = array_field(request, "scale_y", -1.0, 2.0, MAX_AXIS_LEN)?;
            Algorithm::Grid(o)
        }
        "contour" => {
            let mut o = match preset {
                Some(p) => ContourOptions::preset(p).ok_or_else(|| unknown_preset(p))?,
                None => ContourOptions::default(),
            };
            o.sampling_step = n("sampling_step", 1.0, 200.0)?.unwrap_or(o.sampling_step);
            o.mask_threshold = n("mask_threshold", 1.0, 200.0)?.unwrap_or(o.mask_threshold);
            o.min_distance = n("min_distance", 1.0, 200.0)?.unwrap_or(o.min_distance);
            o.max_distance = n("max_distance", -1.0, 200.0)?.unwrap_or(o.max_distance);
            if let Some(scales) = array_field(request, "scales", 0.0, 2.0, MAX_SCALES)? {
                o.scales = scales;
            }
            Algorithm::Contour(o)
        }
        "optimum" => {
            let mut o = match preset {
                Some(p) => OptimumOptions::preset(p).ok_or_else(|| unknown_preset(p))?,
                None => OptimumOptions::default(),
            };
            o.large_threshold = n("large_threshold", 50.0, 2000.0)?.unwrap_or(o.large_threshold);
            o.length_threshold = n("length_threshold", 20.0, 2000.0)?.unwrap_or(o.length_threshold);
            o.ratio_threshold = n("ratio_threshold", 0.01, 1.0)?.unwrap_or(o.ratio_threshold);
            o.sharp_expand = n("sharp_expand", 0.0, 0.2)?.unwrap_or(o.sharp_expand);
            o.unsharp_expand = n("unsharp_expand", 0.0, 0.5)?.unwrap_or(o.unsharp_expand);
            o.unsharp_contract = n("unsharp_contract", 0.0, 0.5)?.unwrap_or(o.unsharp_contract);
            o.min_distance = n("min_distance", 1.0, 200.0)?.unwrap_or(o.min_distance);
            o.mask_threshold = n("mask_threshold", 1.0, 200.0)?.unwrap_or(o.mask_threshold);
            o.div_per_part = n("div_per_part", 4.0, 64.0)?.unwrap_or(o.div_per_part);
            if let Some(scales) = array_field(request, "scales", 0.0, 2.0, MAX_SCALES)? {
                o.scales = scales;
            }
            Algorithm::Optimum(o)
        }
        "skeleton" => {
            if preset.is_some() {
                return Err(no_presets());
            }
            let mut o = SkeletonOptions::default();
            o.mask_threshold = n("mask_threshold", 1.0, 200.0)?.unwrap_or(o.mask_threshold);
            if let Some(count) = n("target_point_count", 2.0, 128.0)? {
                o.target_point_count = count as u32;
            }
            Algorithm::Skeleton(o)
        }
        other => {
            return Err(format!(
                "unknown algorithm {other:?}; expected grid, contour, optimum or skeleton"
            ))
        }
    };
    match &algorithm {
        Algorithm::Grid(o) => o.validate(),
        Algorithm::Contour(o) => o.validate(),
        Algorithm::Optimum(o) => o.validate(),
        Algorithm::Skeleton(o) => o.validate(),
    }?;
    Ok(algorithm)
}

/// The presets of the user (issue #157), in `automesh-presets.json` of the settings directory.
pub(crate) const PRESETS: crate::preset_store::Kind =
    crate::preset_store::Kind {
        file: "automesh-presets.json",
        noun: "preset",
        entry: preset_entry,
        builtin: is_builtin_preset,
    };

/// The option fields a user preset keeps, for the algorithms that have presets.
fn preset_fields(algorithm: &str) -> Option<&'static [&'static str]> {
    match algorithm {
        "contour" => Some(&["sampling_step", "mask_threshold", "min_distance", "max_distance", "scales"]),
        "optimum" => Some(&[
            "large_threshold", "length_threshold", "ratio_threshold", "sharp_expand", "unsharp_expand",
            "unsharp_contract", "min_distance", "mask_threshold", "div_per_part", "scales",
        ]),
        _ => None,
    }
}

fn is_builtin_preset(name: &str) -> bool {
    ContourOptions::preset(name).is_some() || OptimumOptions::preset(name).is_some()
}

/// What a preset keeps of `request`: the algorithm, the built-in preset it starts from and the option fields.
/// The fields are kept as they are given; `algorithm_from_request` clamps them when a mesh is generated.
fn preset_entry(request: &Value) -> Result<Value, String> {
    let algorithm = request.get("algorithm").and_then(Value::as_str).unwrap_or_default();
    let fields = preset_fields(algorithm).ok_or("a preset needs the algorithm contour or optimum")?;
    let entry: Map<String, Value> = ["algorithm", "preset"]
        .iter()
        .chain(fields)
        .filter_map(|key| Some(((*key).to_owned(), request.get(*key).filter(|value| !value.is_null())?.clone())))
        .collect();
    let entry = Value::Object(entry);
    algorithm_from_request(&entry)?;
    Ok(entry)
}

/// `request` with the fields of the user preset it names below its own fields, so a field of the request wins.
/// A request that names no preset, a built-in one or a preset of another algorithm is returned as it is, and
/// the file is read only for a name that is not built in.
pub(crate) fn with_user_preset(dir: Option<&Path>, request: &Value) -> Result<Value, String> {
    let text = |key: &str| request.get(key).and_then(Value::as_str);
    let (Some(dir), Some(name), Some(algorithm)) = (dir, text("preset"), text("algorithm")) else {
        return Ok(request.clone());
    };
    if preset_fields(algorithm).is_none() || is_builtin_preset(name) {
        return Ok(request.clone());
    }
    let presets = PRESETS.load(dir)?;
    let Some(entry) = presets.get(name).filter(|entry| entry["algorithm"] == algorithm) else {
        return Ok(request.clone());
    };
    let mut merged = entry.clone();
    for (key, value) in request.as_object().into_iter().flatten() {
        if key != "preset" && !value.is_null() {
            merged[key] = value.clone();
        }
    }
    Ok(merged)
}

/// D `getAlphaInput` for a Part: the RGBA texture in slot 0, of which only alpha is used.
fn part_texture_alpha(node: &Value, id: u32, textures: &[TextureBlob]) -> Result<PixelImage, String> {
    let slot = node
        .get("textures")
        .and_then(Value::as_array)
        .and_then(|slots| slots.first())
        .and_then(Value::as_u64)
        .filter(|&slot| slot != NO_TEXTURE)
        .ok_or_else(|| format!("Part {id} has no texture"))?;
    let blob = usize::try_from(slot)
        .ok()
        .and_then(|slot| textures.get(slot))
        .ok_or_else(|| format!("Part {id} texture slot {slot} is not in the file"))?;
    let format = match blob.tag {
        0 => image::ImageFormat::Png,
        1 => image::ImageFormat::Tga,
        tag => return Err(format!("texture slot {slot} has unsupported image tag {tag}")),
    };
    let mut reader = image::ImageReader::with_format(Cursor::new(blob.data.as_slice()), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let rgba = reader
        .decode()
        .map_err(|error| format!("could not decode texture slot {slot}: {error}"))?
        .into_rgba8();
    PixelImage::from_pixels(rgba.width(), rgba.height(), PixelFormat::Rgba, rgba.into_raw())
}

fn reject_unsupported_payload(model: &Value, node: &Value, id: u32) -> Result<(), String> {
    let nonempty = |key: &str| {
        node.get(key)
            .is_some_and(|v| v.as_array().map_or(!v.is_null(), |items| !items.is_empty()))
    };
    if ["depths", "depth-ops", "depth-op-base-depths"]
        .iter()
        .any(|key| node.get(*key).is_some_and(|v| !v.is_null()))
    {
        return Err(format!("Part {id} has a depth payload that automesh cannot resize"));
    }
    if nonempty("weldedLinks") {
        return Err(format!("Part {id} has welds that automesh cannot remap"));
    }
    // A new mesh has other vertices, also when it has as many.
    crate::bindings::reject_vertex_weight_resize(model, id, usize::MAX)
        .map_err(|_| format!("Part {id} has skinning weights that automesh cannot remap"))?;
    crate::deformers::reject_deform_bindings(model, id)
}

/// Flat `[x0, y0, x1, y1, ...]` list of f32 pairs, widened exactly to f64.
fn flat_pairs(pairs: &[V2]) -> Vec<f64> {
    pairs.iter().flat_map(|p| [f64::from(p[0]), f64::from(p[1])]).collect()
}

/// Rewrites the legacy top-level `vertices`/`uvs`/`indices` of a Part in the shape it already has.
fn rewrite_top_level(object: &mut Map<String, Value>, key: &str, flat: &[f64], pair: bool) {
    let Some(existing) = object.get(key) else {
        return;
    };
    let nested = existing
        .as_array()
        .and_then(|items| items.first())
        .is_some_and(Value::is_array);
    let value = if pair && nested {
        Value::Array(flat.chunks(2).map(|p| json!([p[0], p[1]])).collect())
    } else {
        json!(flat)
    };
    object.insert(key.to_owned(), value);
}

/// Runs the requested generator on the alpha of Part `id` and returns the model with the Part's
/// mesh replaced (`mesh.verts`, `uvs`, `indices`; `origin` is kept as D keeps `eOrigin`).
pub(crate) fn generate_for_part(
    model: &Value,
    textures: &[TextureBlob],
    request: &Value,
) -> Result<PartMeshResult, String> {
    let id = request
        .get("id")
        .and_then(Value::as_u64)
        .and_then(crate::valid_id)
        .ok_or("id must be a nonzero u32 Part UUID")?;
    let algorithm = algorithm_from_request(request)?;
    let node = crate::nodes::find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    if node.get("type").and_then(Value::as_str) != Some("Part") {
        return Err(format!("node {id} is not a Part"));
    }
    reject_unsupported_payload(model, node, id)?;
    let alpha = part_texture_alpha(node, id, textures)?;
    let [width, height] = alpha.size();
    let mut budget = WorkBudget::new(DEFAULT_WORK_BUDGET);
    let Some(output) = generate(&alpha, &algorithm, MeshTarget::Drawable, &mut budget)? else {
        return Ok(PartMeshResult {
            candidate: None,
            summary: json!({ "id": id, "changed": false, "message": "no mesh generated" }),
        });
    };
    if output.vertices.len() > usize::from(u16::MAX) {
        return Err("generated mesh has more vertices than 16-bit indices address".to_owned());
    }
    // Part rest positions are image-centred already (D adds `textureOffset` only for Projectables).
    let vertices = output.vertices;
    let mut indices: Vec<u32> = Vec::with_capacity(output.triangles.len() * 3);
    for [a, b, c] in output.triangles {
        let (va, vb, vc) = (vertices[a as usize], vertices[b as usize], vertices[c as usize]);
        // D `fixWinding`: counter-clockwise triangles only.
        let clockwise = (vb[0] - va[0]) * (vc[1] - va[1]) - (vb[1] - va[1]) * (vc[0] - va[0]) < 0.0;
        indices.extend(if clockwise { [a, c, b] } else { [a, b, c] });
    }
    // D `Part.normalizeUV` after `mExport` copied positions into the UVs.
    let (tex_w, tex_h) = (width as f32, height as f32);
    let uvs: Vec<V2> = vertices
        .iter()
        .map(|v| [v[0] / tex_w + 0.5, v[1] / tex_h + 0.5])
        .collect();

    let (flat_verts, flat_uvs) = (flat_pairs(&vertices), flat_pairs(&uvs));
    let mut candidate = model.clone();
    let node = crate::nodes::find_node_mut(&mut candidate, id)
        .ok_or_else(|| format!("node {id} was not found"))?;
    let object = node.as_object_mut().ok_or("node must be an object")?;
    let mesh = object
        .entry("mesh")
        .or_insert_with(|| json!({ "origin": [0.0, 0.0] }))
        .as_object_mut()
        .ok_or_else(|| format!("node {id} mesh must be an object"))?;
    mesh.insert("verts".into(), json!(flat_verts));
    mesh.insert("uvs".into(), json!(flat_uvs));
    mesh.insert("indices".into(), json!(indices));
    mesh.entry("origin").or_insert_with(|| json!([0.0, 0.0]));
    match output.grid {
        Some(grid) => {
            mesh.insert("gridAxes".into(), json!([grid.rows, grid.columns]));
        }
        None => {
            mesh.remove("gridAxes");
        }
    }
    rewrite_top_level(object, "vertices", &flat_verts, true);
    rewrite_top_level(object, "uvs", &flat_uvs, true);
    if let Some(top) = object.get_mut("indices") {
        *top = json!(indices);
    }
    crate::validate_model(&candidate)?;
    let summary = json!({
        "id": id,
        "changed": true,
        "vertices": vertices.len(),
        "triangles": indices.len() / 3,
    });
    Ok(PartMeshResult { candidate: Some(candidate), summary })
}
