//! Part split session operations (`split.*`): turn one flattened RGBA illustration into several Parts.
//!
//! Deterministic pixel work lives here; an agent only supplies the prompts (points, boxes, thresholds). The draft
//! is a [`State`] held by the editing `Session` outside the document and its undo history, like DepthDraw.
//! A part is a list of [`MaskStep`]s evaluated into one coverage byte per pixel, so masks are derived, never
//! stored: the plan (`split.plan`) is just the steps, and `split.apply {plan}` replays it without a draft.
//! `split.apply` is the only operation that edits the document and it is ONE undo step.
//!
//! "Claimed" means coverage of at least [`CLAIMED`]; every statistic and the morphology steps use it.

use crate::cv::{BinaryMask, DistanceTransform, WorkBudget};
use crate::model_import::{self, ImageFile, Texture, TEXTURE_PNG};
use crate::psd;
use crate::render::{MAX_TEXTURE_PIXELS, RASTER_WORK_REFUSAL, RENDER_MEMORY_REFUSAL};
use crate::{automesh, Session};
use base64::Engine;
use image::{ImageDecoder, ImageEncoder};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::borrow::Cow;
use std::io::{Cursor, Read};
use std::path::Path;

const PLAN_VERSION: u64 = 1;
const MAX_PARTS: usize = 32;
const MAX_STEPS: usize = 64;
const MAX_POLYGON_POINTS: usize = 1024;
const MAX_RADIUS: u32 = 32;
const MAX_PADDING: u32 = 64;
const MAX_NAME_BYTES: usize = 128;
const MAX_TEXTURE_SIDE: u32 = 8192;
/// Most strips a PSD is flattened in. Each strip is one render under the renderer's own limits. A try that fails is
/// drawn again from its first row with twice the strips, so a flattening costs fewer than twice this many renders.
// ponytail: starts at one strip and doubles; estimate the count from the layer boxes if the retries show up.
const MAX_FLATTEN_STRIPS: u32 = 64;
/// Masks of all parts are alive at once (overlap statistics), so bound their total size.
const MAX_MASK_BYTES: u64 = 256 * 1024 * 1024;
const MAX_RASTER_BYTES: u64 = 32 * 1024 * 1024;
const WORK_PER_PIXEL: u64 = 512;
/// Sum of the texture areas of all parts of a plan. Parts may overlap (under-layers, padding), so this is twice the
/// source limit; the textures, their PNG copies and the merge then stay near 0.4 GiB, see doc/part-split.md.
const MAX_TOTAL_TEXTURE_PIXELS: u64 = 2 * MAX_TEXTURE_PIXELS;
/// Largest area a grow or shrink may process (the mask bounding box grown by the radius). The distance transform
/// holds about 37 bytes per pixel, so this bounds that transient at roughly 155 MB.
const MAX_MORPH_PIXELS: u64 = 4 * 1024 * 1024;
/// Coverage at or above this counts as claimed.
const CLAIMED: u8 = 128;
const DEFAULT_FLOOD_TOLERANCE: u8 = 16;
const DEFAULT_LINE_THRESHOLD: u8 = 80;
const OVERLAY_TINT: [[u8; 3]; 8] = [
    [230, 25, 75], [60, 180, 75], [0, 130, 200], [245, 130, 48], [145, 30, 180], [70, 240, 240], [240, 50, 230],
    [210, 245, 60],
];

// ---------------------------------------------------------------------------------------------- plan types

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    #[default]
    Add,
    Subtract,
    Intersect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Keep {
    Left,
    Right,
}

fn default_tolerance() -> u8 {
    DEFAULT_FLOOD_TOLERANCE
}

fn default_line_threshold() -> u8 {
    DEFAULT_LINE_THRESHOLD
}

/// One selection or morphology step of a part. Selection steps combine into the running mask by `mode`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum MaskStep {
    /// Every pixel whose alpha is above `threshold`.
    Alpha {
        #[serde(default)]
        threshold: u8,
        #[serde(default)]
        mode: Mode,
    },
    /// The 4-connected component of pixels with alpha above `threshold` that contains `point`.
    Component {
        point: [u32; 2],
        #[serde(default)]
        threshold: u8,
        #[serde(default)]
        mode: Mode,
    },
    /// The 4-connected pixels within `tolerance` (largest channel difference, alpha included) of the seed colour.
    Flood {
        point: [u32; 2],
        #[serde(default = "default_tolerance")]
        tolerance: u8,
        #[serde(default)]
        mode: Mode,
    },
    /// Like `flood`, but stops at transparent pixels and at line art: pixels whose luminance is below
    /// `lineThreshold`. The seed must not be a line pixel.
    BoundedFill {
        point: [u32; 2],
        #[serde(default = "default_line_threshold")]
        line_threshold: u8,
        #[serde(default)]
        mode: Mode,
    },
    /// Even-odd polygon; vertices are pixel corners, so `[0, 0]` to `[width, height]` spans the image.
    Polygon {
        points: Vec<[u32; 2]>,
        #[serde(default)]
        mode: Mode,
    },
    /// `[x, y, width, height]` in pixels.
    #[serde(rename = "box")]
    Rect {
        rect: [u32; 4],
        #[serde(default)]
        mode: Mode,
    },
    /// The mask of another part of the plan.
    Part {
        name: String,
        #[serde(default)]
        mode: Mode,
    },
    Raster {
        path: String,
        crc32: u32,
        #[serde(default)]
        mode: Mode,
    },
    /// Euclidean grow of the claimed pixels by `radius`; the result is binary.
    Grow { radius: u32 },
    /// Euclidean shrink of the claimed pixels by `radius`; the result is binary.
    Shrink { radius: u32 },
    /// Box blur of the coverage, which makes soft edges.
    Feather { radius: u32 },
    /// Keeps the pixels left of (`x < axisX`) or right of (`x >= axisX`) a vertical line: a symmetry split.
    Side { axis_x: u32, keep: Keep },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PartSpec {
    pub name: String,
    pub steps: Vec<MaskStep>,
    /// Transparent pixels around the visible box of the texture (room for automesh and filtering).
    #[serde(default)]
    pub padding: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlanSource {
    path: String,
    width: u32,
    height: u32,
    /// CRC-32 of the decoded straight-alpha RGBA bytes: catches a different file at the same path (not tamper proof).
    crc32: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Plan {
    version: u64,
    source: PlanSource,
    parts: Vec<PartSpec>,
}

impl Plan {
    fn of_draft(source: &Source, parts: &[PartSpec]) -> Self {
        Plan {
            version: PLAN_VERSION,
            source: PlanSource {
                path: source.path.clone(),
                width: source.width,
                height: source.height,
                crc32: source.crc32,
            },
            parts: parts.to_vec(),
        }
    }

    fn matches(&self, source: &Source, parts: &[PartSpec]) -> bool {
        self.source.path == source.path
            && self.source.width == source.width
            && self.source.height == source.height
            && self.source.crc32 == source.crc32
            && self.parts == parts
    }
}

/// One applied split: the accepted plan and the live Part ids it created.
struct Application {
    plan: std::sync::Arc<Plan>,
    live: Vec<u32>,
}

/// Applied splits remembered in memory so `applied` survives in-session edits.
#[derive(Default)]
pub(crate) struct Applications(Vec<Application>);

impl Applications {
    pub(crate) fn carried_into(&self, model: &Value) -> Self {
        if self.0.is_empty() {
            return Self::default();
        }
        let mut live_ids = std::collections::HashSet::new();
        let mut stack = vec![&model["nodes"]];
        while let Some(node) = stack.pop() {
            if node["type"] == "Part" {
                if let Some(id) = node.get("uuid").and_then(Value::as_u64).and_then(crate::valid_id) {
                    live_ids.insert(id);
                }
            }
            stack.extend(node.get("children").and_then(Value::as_array).into_iter().flatten());
        }
        Applications(
            self.0
                .iter()
                .filter_map(|application| {
                    let live: Vec<u32> = application
                        .live
                        .iter()
                        .copied()
                        .filter(|id| live_ids.contains(id))
                        .collect();
                    (!live.is_empty()).then_some(Application { plan: application.plan.clone(), live })
                })
                .collect(),
        )
    }

    fn record(&mut self, plan: Plan, created: &[u32]) {
        if created.is_empty() {
            return;
        }
        self.0.push(Application { plan: std::sync::Arc::new(plan), live: created.to_vec() });
    }

    fn contains_draft(&self, source: &Source, parts: &[PartSpec]) -> bool {
        self.0.iter().any(|application| application.plan.matches(source, parts))
    }
}

// ------------------------------------------------------------------------------------------------- state

/// The opened illustration, straight alpha.
pub(crate) struct Source {
    path: String,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    crc32: u32,
}

pub(crate) enum SplitImage<'a> {
    Source,
    Mask(&'a str),
}

/// What an editing `Session` keeps for the part split draft.
#[derive(Default)]
pub(crate) struct State {
    source: Option<Source>,
    /// Draw order, back to front.
    parts: Vec<PartSpec>,
    /// Counts every change of the draft.
    revision: u64,
}

impl State {
    pub(crate) fn summary(&self, applied: bool) -> Value {
        json!({
            "revision": self.revision,
            "open": self.source.is_some(),
            "width": self.source.as_ref().map_or(0, |source| source.width),
            "height": self.source.as_ref().map_or(0, |source| source.height),
            "parts": self.parts.len(),
            "applied": applied,
        })
    }

    /// Whether the CURRENT draft's canonical plan has live Parts in the model.
    pub(crate) fn applied(&self, applications: &Applications) -> bool {
        match (&self.source, self.parts.is_empty()) {
            (Some(source), false) => applications.contains_draft(source, &self.parts),
            _ => false,
        }
    }

    fn touch(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    fn source(&self) -> Result<&Source, String> {
        self.source.as_ref().ok_or_else(|| "no source is open (call split.open first)".to_owned())
    }
}

impl Source {
    fn len(&self) -> usize {
        self.width as usize * self.height as usize
    }

    fn alpha(&self, index: usize) -> u8 {
        self.rgba[index * 4 + 3]
    }
}

// -------------------------------------------------------------------------------------------- validation

fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_NAME_BYTES || name.chars().any(char::is_control) {
        return Err(format!("name must be 1 to {MAX_NAME_BYTES} bytes without control characters"));
    }
    Ok(())
}

fn check_point(point: [u32; 2], width: u32, height: u32, what: &str) -> Result<(), String> {
    if point[0] >= width || point[1] >= height {
        return Err(format!("{what} {point:?} is outside the {width}x{height} image"));
    }
    Ok(())
}

fn check_radius(radius: u32) -> Result<(), String> {
    if radius == 0 || radius > MAX_RADIUS {
        return Err(format!("radius must be 1 to {MAX_RADIUS}"));
    }
    Ok(())
}

fn check_step(step: &MaskStep, width: u32, height: u32) -> Result<(), String> {
    match step {
        MaskStep::Alpha { .. } | MaskStep::Part { .. } => Ok(()),
        MaskStep::Raster { path, .. } => {
            if path.is_empty() { Err("raster path must not be empty".into()) } else { Ok(()) }
        }
        MaskStep::Component { point, .. } | MaskStep::Flood { point, .. } | MaskStep::BoundedFill { point, .. } => {
            check_point(*point, width, height, "point")
        }
        MaskStep::Polygon { points, .. } => {
            if !(3..=MAX_POLYGON_POINTS).contains(&points.len()) {
                return Err(format!("polygon needs 3 to {MAX_POLYGON_POINTS} points"));
            }
            match points.iter().find(|point| point[0] > width || point[1] > height) {
                Some(point) => Err(format!("polygon point {point:?} is outside the {width}x{height} image")),
                None => Ok(()),
            }
        }
        MaskStep::Rect { rect: [x, y, w, h], .. } => {
            let (right, bottom) = (u64::from(*x) + u64::from(*w), u64::from(*y) + u64::from(*h));
            if *w == 0 || *h == 0 || right > u64::from(width) || bottom > u64::from(height) {
                return Err(format!("rect {:?} is empty or outside the {width}x{height} image", [x, y, w, h]));
            }
            Ok(())
        }
        MaskStep::Grow { radius } | MaskStep::Shrink { radius } | MaskStep::Feather { radius } => check_radius(*radius),
        MaskStep::Side { axis_x, .. } => {
            if *axis_x == 0 || *axis_x >= width {
                return Err(format!("axisX must be 1 to {} for a {width}-pixel-wide image", width.saturating_sub(1)));
            }
            Ok(())
        }
    }
}

fn step_refs(spec: &PartSpec) -> impl Iterator<Item = &str> {
    spec.steps.iter().filter_map(|step| match step {
        MaskStep::Part { name, .. } => Some(name.as_str()),
        _ => None,
    })
}

/// Part indexes so that every part comes after the parts it refers to.
fn topological_order(parts: &[PartSpec]) -> Result<Vec<usize>, String> {
    fn visit(index: usize, parts: &[PartSpec], state: &mut [u8], out: &mut Vec<usize>) -> Result<(), String> {
        match state[index] {
            2 => return Ok(()),
            1 => return Err(format!("part {:?} refers to itself through a cycle", parts[index].name)),
            _ => {}
        }
        state[index] = 1;
        for name in step_refs(&parts[index]) {
            let target = parts
                .iter()
                .position(|part| part.name == name)
                .ok_or_else(|| format!("part {:?} refers to the unknown part {name:?}", parts[index].name))?;
            visit(target, parts, state, out)?;
        }
        state[index] = 2;
        out.push(index);
        Ok(())
    }
    let mut state = vec![0u8; parts.len()];
    let mut out = Vec::with_capacity(parts.len());
    for index in 0..parts.len() {
        visit(index, parts, &mut state, &mut out)?;
    }
    Ok(out)
}

/// Everything the boundary must refuse before a plan is evaluated.
fn validate(parts: &[PartSpec], width: u32, height: u32) -> Result<(), String> {
    if parts.len() > MAX_PARTS {
        return Err(format!("a plan holds at most {MAX_PARTS} parts"));
    }
    for (index, part) in parts.iter().enumerate() {
        check_name(&part.name)?;
        if parts[..index].iter().any(|other| other.name == part.name) {
            return Err(format!("part name {:?} is used twice", part.name));
        }
        let fail = |message: String| format!("part {:?}: {message}", part.name);
        if part.steps.is_empty() || part.steps.len() > MAX_STEPS {
            return Err(fail(format!("steps must hold 1 to {MAX_STEPS} entries")));
        }
        if part.padding > MAX_PADDING {
            return Err(fail(format!("padding must be 0 to {MAX_PADDING}")));
        }
        for step in &part.steps {
            check_step(step, width, height).map_err(fail)?;
        }
    }
    topological_order(parts).map(drop)
}

// ---------------------------------------------------------------------------------------------- the masks

fn charge(budget: &mut WorkBudget, work: u64) -> Result<(), String> {
    budget.charge(work).map_err(|_| {
        format!("split work limit reached: evaluation allows {WORK_PER_PIXEL} units per source pixel; use fewer or cheaper steps")
    })
}

/// 255 where `keep(index)` holds.
fn select(count: usize, keep: impl Fn(usize) -> bool) -> Vec<u8> {
    (0..count).map(|index| if keep(index) { 255 } else { 0 }).collect()
}

/// 4-connected region around `seed` of the pixels `inside` accepts; empty when the seed itself is refused.
fn flood(
    width: usize,
    height: usize,
    seed: [u32; 2],
    inside: impl Fn(usize) -> bool,
    budget: &mut WorkBudget,
) -> Result<Vec<u8>, String> {
    let mut out = vec![0u8; width * height];
    flood_into(
        width,
        seed,
        inside,
        &mut out,
        &mut Vec::new(),
        budget,
        |_| {},
    )?;
    Ok(out)
}

fn flood_into(
    width: usize,
    seed: [u32; 2],
    inside: impl Fn(usize) -> bool,
    visited: &mut [u8],
    stack: &mut Vec<u32>,
    budget: &mut WorkBudget,
    mut visit: impl FnMut(usize),
) -> Result<(), String> {
    let height = visited.len() / width;
    stack.clear();
    let start = seed[1] as usize * width + seed[0] as usize;
    if visited[start] != 0 || !inside(start) {
        return Ok(());
    }
    visited[start] = 255;
    stack.push(start as u32);
    while let Some(index) = stack.pop() {
        charge(budget, 4)?;
        let index = index as usize;
        visit(index);
        let (x, y) = (index % width, index / width);
        let mut neighbours = [None; 4];
        neighbours[0] = (x > 0).then(|| index - 1);
        neighbours[1] = (x + 1 < width).then(|| index + 1);
        neighbours[2] = (y > 0).then(|| index - width);
        neighbours[3] = (y + 1 < height).then(|| index + width);
        for next in neighbours.into_iter().flatten() {
            if visited[next] == 0 && inside(next) {
                visited[next] = 255;
                stack.push(next as u32);
            }
        }
    }
    Ok(())
}

fn components(
    source: &Source,
    threshold: u8,
    offset: u32,
    budget: &mut WorkBudget,
) -> Result<Value, String> {
    charge(budget, source.len() as u64)?;
    let width = source.width as usize;
    let mut visited = vec![0; source.len()];
    let mut stack = Vec::new();
    let mut page = Vec::new();
    let mut total = 0u32;
    for index in 0..source.len() {
        if visited[index] != 0 || source.alpha(index) <= threshold {
            continue;
        }
        let seed = [(index % width) as u32, (index / width) as u32];
        let keep = total >= offset && page.len() < MAX_PARTS;
        let [mut x0, mut y0] = seed;
        let (mut x1, mut y1, mut pixels) = (x0, y0, 0u32);
        flood_into(
            width,
            seed,
            |i| source.alpha(i) > threshold,
            &mut visited,
            &mut stack,
            budget,
            |i| {
                if keep {
                    let (x, y) = ((i % width) as u32, (i / width) as u32);
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                    pixels += 1;
                }
            },
        )?;
        if keep {
            page.push(json!({"seed":seed, "pixels":pixels, "bbox":[x0,y0,x1-x0+1,y1-y0+1]}));
        }
        total += 1;
    }
    let next = offset
        .checked_add(page.len() as u32)
        .filter(|next| *next < total);
    Ok(json!({"components":page, "totalComponents":total, "nextOffset":next}))
}

/// Even-odd scanline fill: per pixel row the edge crossings are sorted and every second span is inside.
fn polygon(width: usize, height: usize, points: &[[u32; 2]], budget: &mut WorkBudget) -> Result<Vec<u8>, String> {
    let mut out = vec![0u8; width * height];
    let (min_x, max_x) = points.iter().fold((u32::MAX, 0), |(lo, hi), p| (lo.min(p[0]), hi.max(p[0])));
    let (min_y, max_y) = points.iter().fold((u32::MAX, 0), |(lo, hi), p| (lo.min(p[1]), hi.max(p[1])));
    let max_y = max_y.min(height as u32);
    charge(budget, u64::from(max_y.saturating_sub(min_y)) * points.len() as u64)?;
    let mut crossings: Vec<f64> = Vec::with_capacity(points.len());
    for y in min_y..max_y {
        let py = f64::from(y) + 0.5;
        crossings.clear();
        let mut previous = points[points.len() - 1];
        for &current in points {
            let (x0, y0) = (f64::from(previous[0]), f64::from(previous[1]));
            let (x1, y1) = (f64::from(current[0]), f64::from(current[1]));
            if (y0 > py) != (y1 > py) {
                crossings.push((x1 - x0) * (py - y0) / (y1 - y0) + x0);
            }
            previous = current;
        }
        crossings.sort_by(f64::total_cmp);
        let row = y as usize * width;
        for span in crossings.chunks_exact(2) {
            // Pixel x is inside when its centre x + 0.5 lies in [span[0], span[1]).
            let from = (span[0] - 0.5).ceil().max(f64::from(min_x)) as usize;
            let to = (span[1] - 0.5).ceil().min(f64::from(max_x)).min(width as f64) as usize;
            if from < to {
                out[row + from..row + to].fill(255);
            }
        }
    }
    Ok(out)
}

fn combine(mask: &mut [u8], selection: &[u8], mode: Mode) {
    for (value, &chosen) in mask.iter_mut().zip(selection) {
        *value = match mode {
            Mode::Add => (*value).max(chosen),
            Mode::Subtract => ((u32::from(*value) * u32::from(255 - chosen) + 127) / 255) as u8,
            Mode::Intersect => (*value).min(chosen),
        };
    }
}

/// Euclidean grow or shrink of the claimed pixels, through the existing distance transform. Only the bounding box of
/// the claimed pixels, grown by `radius` (by 1 for a shrink, which needs one unclaimed pixel beside the edge), is
/// transformed: nothing outside it can change. Pixels beyond the image edge are not background, as before.
fn morph(
    mask: &[u8],
    width: usize,
    height: usize,
    radius: u32,
    grow: bool,
    budget: &mut WorkBudget,
) -> Result<Vec<u8>, String> {
    charge(budget, 3 * mask.len() as u64)?;
    let mut found: Option<[usize; 4]> = None;
    for (y, row) in mask.chunks_exact(width).enumerate() {
        let (Some(first), Some(last)) = (
            row.iter().position(|&value| value >= CLAIMED),
            row.iter().rposition(|&value| value >= CLAIMED),
        ) else {
            continue;
        };
        found = Some(match found {
            None => [first, y, last, y],
            Some([x0, y0, x1, _]) => [x0.min(first), y0, x1.max(last), y],
        });
    }
    // Nothing is claimed: both grow and shrink give an empty mask.
    let Some([x0, y0, x1, y1]) = found else { return Ok(vec![0; mask.len()]) };
    let margin = if grow { radius as usize } else { 1 };
    let (left, top) = (x0.saturating_sub(margin), y0.saturating_sub(margin));
    let (right, bottom) = ((x1 + margin + 1).min(width), (y1 + margin + 1).min(height));
    let (crop_w, crop_h) = (right - left, bottom - top);
    let area = (crop_w * crop_h) as u64;
    if area > MAX_MORPH_PIXELS {
        return Err(format!(
            "grow or shrink would process a {crop_w}x{crop_h} area, more than {MAX_MORPH_PIXELS} pixels; clip the mask with a box first"
        ));
    }
    // The transform visits each pixel about 5 times.
    charge(budget, 5 * area)?;
    let mut probe = Vec::with_capacity(area as usize);
    for row in mask.chunks_exact(width).skip(top).take(crop_h) {
        // Shrink measures claimed pixels to the nearest unclaimed one; grow measures unclaimed pixels to the nearest claimed.
        probe.extend(row[left..right].iter().map(|&value| if (value >= CLAIMED) == !grow { 255 } else { 0 }));
    }
    let transform = BinaryMask::new(crop_w as u32, crop_h as u32, probe)?
        .distance_transform(&mut WorkBudget::new(5 * area + 1024))?;
    let DistanceTransform::Field(field) = transform else {
        // Only a shrink can get here: the box holds no unclaimed pixel, which means it is the whole image.
        return Ok(select(mask.len(), |index| mask[index] >= CLAIMED));
    };
    let (radius, samples) = (radius as f32, field.samples());
    let mut out = vec![0u8; mask.len()];
    for y in 0..crop_h {
        for x in 0..crop_w {
            let index = (top + y) * width + left + x;
            let distance = samples[y * crop_w + x].distance;
            let inside = if grow { distance <= radius } else { mask[index] >= CLAIMED && distance > radius };
            if inside {
                out[index] = 255;
            }
        }
    }
    Ok(out)
}

/// Box blur of the coverage with clamped edges: two running-sum passes, so the cost does not depend on the radius.
fn feather(mask: &[u8], width: usize, height: usize, radius: u32, budget: &mut WorkBudget) -> Result<Vec<u8>, String> {
    charge(budget, 4 * mask.len() as u64)?;
    let reach = radius as isize;
    let window = 2 * radius + 1;
    let average = |sum: u32| ((sum + window / 2) / window) as u8;
    let mut across = vec![0u8; mask.len()];
    for (row, out) in mask.chunks_exact(width).zip(across.chunks_exact_mut(width)) {
        let at = |x: isize| u32::from(row[x.clamp(0, width as isize - 1) as usize]);
        let mut sum: u32 = (-reach..=reach).map(at).sum();
        for (x, value) in out.iter_mut().enumerate() {
            *value = average(sum);
            sum = sum + at(x as isize + 1 + reach) - at(x as isize - reach);
        }
    }
    let row_of = |y: isize| y.clamp(0, height as isize - 1) as usize * width;
    let mut sums: Vec<u32> =
        (0..width).map(|x| (-reach..=reach).map(|dy| u32::from(across[row_of(dy) + x])).sum()).collect();
    let mut out = vec![0u8; mask.len()];
    for y in 0..height {
        for (value, &sum) in out[y * width..(y + 1) * width].iter_mut().zip(&sums) {
            *value = average(sum);
        }
        let (added, removed) = (row_of(y as isize + 1 + reach), row_of(y as isize - reach));
        for (x, sum) in sums.iter_mut().enumerate() {
            *sum = *sum + u32::from(across[added + x]) - u32::from(across[removed + x]);
        }
    }
    Ok(out)
}

fn luminance(rgba: &[u8], index: usize) -> u32 {
    let pixel = &rgba[index * 4..index * 4 + 3];
    (299 * u32::from(pixel[0]) + 587 * u32::from(pixel[1]) + 114 * u32::from(pixel[2])) / 1000
}

/// Evaluates the steps of one part; `referenced` gives the mask of a part it refers to.
fn evaluate_part<'a>(
    source: &Source,
    spec: &PartSpec,
    referenced: impl Fn(&str) -> &'a [u8],
    budget: &mut WorkBudget,
) -> Result<Vec<u8>, String> {
    let (width, height) = (source.width as usize, source.height as usize);
    let count = source.len();
    let rgba = &source.rgba;
    let mut mask = vec![0u8; count];
    for step in &spec.steps {
        // Every selection step builds a selection and combines it into the mask: two visits per pixel.
        let selecting = !matches!(
            step,
            MaskStep::Grow { .. } | MaskStep::Shrink { .. } | MaskStep::Feather { .. } | MaskStep::Side { .. }
        );
        if selecting {
            charge(budget, 2 * count as u64)?;
        }
        let (selection, mode): (Cow<[u8]>, Mode) = match step {
            MaskStep::Alpha { threshold, mode } => (select(count, |i| source.alpha(i) > *threshold).into(), *mode),
            MaskStep::Component { point, threshold, mode } => {
                (flood(width, height, *point, |i| source.alpha(i) > *threshold, budget)?.into(), *mode)
            }
            MaskStep::Flood { point, tolerance, mode } => {
                let seed = (point[1] as usize * width + point[0] as usize) * 4;
                let seed = &rgba[seed..seed + 4];
                let near = |i: usize| (0..4).all(|c| rgba[i * 4 + c].abs_diff(seed[c]) <= *tolerance);
                (flood(width, height, *point, near, budget)?.into(), *mode)
            }
            MaskStep::BoundedFill { point, line_threshold, mode } => {
                let open = |i: usize| source.alpha(i) > 0 && luminance(rgba, i) >= u32::from(*line_threshold);
                (flood(width, height, *point, open, budget)?.into(), *mode)
            }
            MaskStep::Polygon { points, mode } => (polygon(width, height, points, budget)?.into(), *mode),
            MaskStep::Rect { rect: [x, y, w, h], mode } => {
                let (x, y, w, h) = (*x as usize, *y as usize, *w as usize, *h as usize);
                let inside = select(count, |i| (x..x + w).contains(&(i % width)) && (y..y + h).contains(&(i / width)));
                (inside.into(), *mode)
            }
            MaskStep::Part { name, mode } => (Cow::Borrowed(referenced(name)), *mode),
            MaskStep::Raster { path, crc32, mode } => {
                let (pixels, actual) = load_raster(source, path, budget)?;
                if actual != *crc32 {
                    return Err(format!("raster {path:?} pixels differ from the plan (checksum mismatch)"));
                }
                (pixels.into(), *mode)
            }
            MaskStep::Grow { radius } => {
                mask = morph(&mask, width, height, *radius, true, budget)?;
                continue;
            }
            MaskStep::Shrink { radius } => {
                mask = morph(&mask, width, height, *radius, false, budget)?;
                continue;
            }
            MaskStep::Feather { radius } => {
                mask = feather(&mask, width, height, *radius, budget)?;
                continue;
            }
            MaskStep::Side { axis_x, keep } => {
                charge(budget, count as u64)?;
                for (i, value) in mask.iter_mut().enumerate() {
                    let left = ((i % width) as u32) < *axis_x;
                    if left != (*keep == Keep::Left) {
                        *value = 0;
                    }
                }
                continue;
            }
        };
        combine(&mut mask, &selection, mode);
    }
    Ok(mask)
}

/// The work one request may spend: masks, statistics, texture checks and overlay all draw from it.
fn new_budget(source: &Source) -> WorkBudget {
    WorkBudget::new(source.len() as u64 * WORK_PER_PIXEL)
}

/// The mask of every part, in plan order. `parts` must have passed [`validate`].
fn evaluate_all(source: &Source, parts: &[PartSpec], budget: &mut WorkBudget) -> Result<Vec<Vec<u8>>, String> {
    if parts.len() as u64 * source.len() as u64 > MAX_MASK_BYTES {
        return Err("too many parts for an image this large".into());
    }
    let mut masks: Vec<Vec<u8>> = vec![Vec::new(); parts.len()];
    for index in topological_order(parts)? {
        let mask = evaluate_part(
            source,
            &parts[index],
            |name| parts.iter().position(|part| part.name == name).map_or(&[][..], |at| &masks[at]),
            budget,
        )?;
        masks[index] = mask;
    }
    Ok(masks)
}

// ------------------------------------------------------------------------------------------- statistics

/// Source alpha times coverage: the alpha a part's texture gets.
fn visible(alpha: u8, coverage: u8) -> u8 {
    ((u32::from(alpha) * u32::from(coverage) + 127) / 255) as u8
}

/// `[x, y, width, height]` of the pixels where a part's texture is not transparent.
fn texture_box(source: &Source, mask: &[u8]) -> Option<[u32; 4]> {
    let width = source.width as usize;
    let mut found: Option<[usize; 4]> = None;
    for (index, &coverage) in mask.iter().enumerate() {
        if visible(source.alpha(index), coverage) == 0 {
            continue;
        }
        let (x, y) = (index % width, index / width);
        found = Some(match found {
            None => [x, y, x, y],
            Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
        });
    }
    found.map(|[x0, y0, x1, y1]| [x0 as u32, y0 as u32, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32])
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Per-part statistics plus the plan-wide counts: opaque (alpha above 0) pixels that no part claims and pixels that
/// more than one part claims.
fn statistics(source: &Source, parts: &[PartSpec], masks: &[Vec<u8>], budget: &mut WorkBudget) -> Result<Value, String> {
    let (count, n) = (source.len(), masks.len());
    // Claim counting, the unclaimed count and the two per-part passes below each visit every pixel of every mask.
    charge(budget, 4 * n as u64 * count as u64)?;
    let mut claimers = vec![0u8; count];
    for mask in masks {
        for (count, &coverage) in claimers.iter_mut().zip(mask) {
            *count += u8::from(coverage >= CLAIMED);
        }
    }
    // A shared pixel looks at every mask once and counts each pair of its holders.
    let pair_work: u64 = claimers.iter().filter(|&&c| c > 1).map(|&c| n as u64 + u64::from(c) * u64::from(c - 1) / 2).sum();
    charge(budget, pair_work)?;
    let unclaimed = (0..count).filter(|&i| claimers[i] == 0 && source.alpha(i) > 0).count();
    let mut shared = 0usize;
    // pairs[a * n + b] with a < b: pixels claimed by both. `n` is at most MAX_PARTS (see `validate`).
    let mut pairs = vec![0u64; n * n];
    for index in (0..count).filter(|&i| claimers[i] > 1) {
        shared += 1;
        let mut holders = [0usize; MAX_PARTS];
        let mut held = 0;
        for p in (0..n).filter(|&p| masks[p][index] >= CLAIMED) {
            holders[held] = p;
            held += 1;
        }
        for (at, &a) in holders[..held].iter().enumerate() {
            for &b in &holders[at + 1..held] {
                pairs[a * n + b] += 1;
            }
        }
    }
    let rows: Vec<Value> = parts
        .iter()
        .zip(masks)
        .enumerate()
        .map(|(at, (part, mask))| {
            let pixels = mask.iter().filter(|&&coverage| coverage >= CLAIMED).count();
            let (mut touched, mut coverage_sum, mut alpha_sum) = (0u64, 0u64, 0u64);
            for (index, &coverage) in mask.iter().enumerate() {
                if coverage > 0 {
                    touched += 1;
                    coverage_sum += u64::from(coverage);
                    alpha_sum += u64::from(source.alpha(index));
                }
            }
            let mean = |sum: u64| if touched == 0 { 0.0 } else { round4(sum as f64 / touched as f64 / 255.0) };
            let overlaps: Vec<Value> = (0..n)
                .filter_map(|other| {
                    let pixels = pairs[at.min(other) * n + at.max(other)];
                    (other != at && pixels > 0).then(|| json!({"part": parts[other].name, "pixels": pixels}))
                })
                .collect();
            json!({
                "name": part.name,
                "index": at,
                "pixels": pixels,
                "bbox": texture_box(source, mask),
                "meanCoverage": mean(coverage_sum),
                "meanAlpha": mean(alpha_sum),
                "overlaps": overlaps,
            })
        })
        .collect();
    Ok(json!({"parts": rows, "unclaimedPixels": unclaimed, "multiplyClaimedPixels": shared}))
}

// ------------------------------------------------------------------------------------------------ textures

/// Size of the texture for a visible box: padded on every side and rounded up to an even size (the Part quad is
/// `size / 2` wide on each side, so an odd texture would lose a pixel).
fn texture_size(name: &str, visible: [u32; 4], padding: u32) -> Result<(i64, i64), String> {
    let pad = i64::from(padding);
    let even = |side: u32| (i64::from(side) + 2 * pad + 1) / 2 * 2;
    let (width, height) = (even(visible[2]), even(visible[3]));
    if width > i64::from(MAX_TEXTURE_SIDE) || height > i64::from(MAX_TEXTURE_SIDE) || (width * height) as u64 > MAX_TEXTURE_PIXELS {
        return Err(format!(
            "part {name:?} needs a {width}x{height} texture ({}x{} visible plus padding {padding}); the limit is {MAX_TEXTURE_SIDE} pixels a side and {MAX_TEXTURE_PIXELS} in total",
            visible[2], visible[3]
        ));
    }
    Ok((width, height))
}

/// Source RGBA times the mask, cropped to the visible box (padded by `padding`) and placed so the Part sits where the
/// pixels were in the source. Textures are premultiplied, like every imported image.
///
/// The padding stays fully zero: a premultiplied texel with alpha 0 and colour would be added to the light behind it
/// by the renderer, so colour bleed (as `tool.rebleed_textures` writes it) would show as a halo around the Part.
fn part_image(source: &Source, spec: &PartSpec, mask: &[u8]) -> Result<ImageFile, String> {
    let visible_box = texture_box(source, mask).ok_or_else(|| format!("part {:?} selects no visible pixels", spec.name))?;
    let [x0, y0, w, h] = visible_box;
    let (tex_w, tex_h) = texture_size(&spec.name, visible_box, spec.padding)?;
    let pad = i64::from(spec.padding);
    let (left, top) = (i64::from(x0) - pad, i64::from(y0) - pad);
    let mut pixels = vec![0u8; (tex_w * tex_h) as usize * 4];
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            let index = y as usize * source.width as usize + x as usize;
            let alpha = visible(source.alpha(index), mask[index]);
            let out = (((i64::from(y) - top) * tex_w + (i64::from(x) - left)) * 4) as usize;
            for c in 0..3 {
                pixels[out + c] = (u32::from(source.rgba[index * 4 + c]) * u32::from(alpha) / 255) as u8;
            }
            pixels[out + 3] = alpha;
        }
    }
    let raw = image::RgbaImage::from_raw(tex_w as u32, tex_h as u32, pixels).ok_or("texture size mismatch")?;
    let mut png = Cursor::new(Vec::new());
    raw.write_to(&mut png, image::ImageFormat::Png).map_err(|error| error.to_string())?;
    // Same centring as layer imports: integer halves of the texture and of the document.
    let trans = [
        left + tex_w / 2 - i64::from(source.width / 2),
        top + tex_h / 2 - i64::from(source.height / 2),
    ];
    Ok(ImageFile {
        name: spec.name.clone(),
        stem: spec.name.clone(),
        width: tex_w as u32,
        height: tex_h as u32,
        texture: Texture { tag: TEXTURE_PNG, data: png.into_inner() },
        trans: [trans[0] as f64, trans[1] as f64, 0.0],
    })
}

/// Refuses a plan whose textures apply could not create: each one within the size limits, all together within
/// [`MAX_TOTAL_TEXTURE_PIXELS`]. Every part is checked, because a part that refers to another changes with it.
fn check_textures(source: &Source, parts: &[PartSpec], masks: &[Vec<u8>], budget: &mut WorkBudget) -> Result<(), String> {
    charge(budget, parts.len() as u64 * source.len() as u64)?;
    let mut total = 0u64;
    for (spec, mask) in parts.iter().zip(masks) {
        if let Some(visible_box) = texture_box(source, mask) {
            let (width, height) = texture_size(&spec.name, visible_box, spec.padding)?;
            total += (width * height) as u64;
        }
    }
    if total > MAX_TOTAL_TEXTURE_PIXELS {
        return Err(format!(
            "the textures of all parts would hold {total} pixels; the limit is {MAX_TOTAL_TEXTURE_PIXELS} in total"
        ));
    }
    Ok(())
}

fn render_parts(source: &Source, parts: &[PartSpec]) -> Result<Vec<ImageFile>, String> {
    if parts.is_empty() {
        return Err("the plan has no parts".into());
    }
    validate(parts, source.width, source.height)?;
    let mut budget = new_budget(source);
    let masks = evaluate_all(source, parts, &mut budget)?;
    check_textures(source, parts, &masks, &mut budget)?;
    parts.iter().zip(&masks).map(|(spec, mask)| part_image(source, spec, mask)).collect()
}

fn overlay_png(source: &Source, masks: &[Vec<u8>], only: Option<usize>, budget: &mut WorkBudget) -> Result<Vec<u8>, String> {
    let (width, height) = (source.width as usize, source.height as usize);
    // The grey base costs 1 visit per pixel; each drawn mask looks at four neighbours per pixel and blends: 5.
    let drawn = only.map_or(masks.len(), |_| 1) as u64;
    charge(budget, source.len() as u64 * (1 + 5 * drawn))?;
    let mut out = vec![0u8; source.len() * 4];
    for index in 0..source.len() {
        // Straight-alpha source over mid grey so transparent areas stay visible.
        let alpha = u32::from(source.alpha(index));
        for c in 0..3 {
            out[index * 4 + c] = ((u32::from(source.rgba[index * 4 + c]) * alpha + 128 * (255 - alpha)) / 255) as u8;
        }
        out[index * 4 + 3] = 255;
    }
    for (at, mask) in masks.iter().enumerate().filter(|(at, _)| only.is_none_or(|only| only == *at)) {
        let tint = OVERLAY_TINT[at % OVERLAY_TINT.len()];
        for (index, &coverage) in mask.iter().enumerate() {
            let (x, y) = (index % width, index / width);
            let around = [(x > 0).then(|| index - 1), (x + 1 < width).then(|| index + 1)]
                .into_iter()
                .chain([(y > 0).then(|| index - width), (y + 1 < height).then(|| index + width)]);
            let edge = coverage >= CLAIMED && around.flatten().any(|next| mask[next] < CLAIMED);
            let weight = if edge { 255 } else { u32::from(coverage) * 45 / 100 };
            for c in 0..3 {
                let base = u32::from(out[index * 4 + c]);
                out[index * 4 + c] = ((base * (255 - weight) + u32::from(tint[c]) * weight) / 255) as u8;
            }
        }
    }
    let raw = image::RgbaImage::from_raw(source.width, source.height, out).ok_or("overlay size mismatch")?;
    let mut png = Cursor::new(Vec::new());
    raw.write_to(&mut png, image::ImageFormat::Png).map_err(|error| error.to_string())?;
    Ok(png.into_inner())
}

// -------------------------------------------------------------------------------------------- request fields

fn req_str<'a>(request: &'a Value, key: &str) -> Result<&'a str, String> {
    match request.get(key) {
        Some(Value::String(text)) => Ok(text),
        _ => Err(format!("{key} is required and must be a string")),
    }
}

fn opt_u32(request: &Value, key: &str) -> Result<Option<u32>, String> {
    match request.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|number| u32::try_from(number).ok())
            .map(Some)
            .ok_or_else(|| format!("{key} must be a nonnegative integer")),
    }
}

fn load_source(path: &str) -> Result<Source, String> {
    if path.is_empty() {
        return Err("path is required".into());
    }
    let path_ref = Path::new(path);
    let rgba = if path_ref.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("psd")) {
        flatten_psd(path_ref)?
    } else {
        model_import::read_rgba(path_ref)?.1
    };
    let (width, height) = rgba.dimensions();
    let rgba = rgba.into_raw();
    Ok(Source { path: path.to_owned(), width, height, crc32: crc32fast::hash(&rgba), rgba })
}

fn load_raster(
    source: &Source,
    path: &str,
    budget: &mut WorkBudget,
) -> Result<(Vec<u8>, u32), String> {
    charge(budget, 2 * source.len() as u64)?;
    let fail = |message: String| format!("raster {path:?}: {message}");
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|error| fail(error.to_string()))?;
    let metadata = file.metadata().map_err(|error| fail(error.to_string()))?;
    if !metadata.is_file() {
        return Err(fail("source must be a regular file".into()));
    }
    let len = metadata.len();
    if len > MAX_RASTER_BYTES {
        return Err(fail("PNG exceeds the 32 MiB encoded size limit".into()));
    }
    charge(budget, len)?;
    let mut bytes = Vec::new();
    file.take(len + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| fail(error.to_string()))?;
    if bytes.len() as u64 != len {
        return Err(fail("file changed size while reading".into()));
    }
    if bytes.len() < 33
        || &bytes[..8] != b"\x89PNG\r\n\x1a\n"
        || bytes[8..12] != [0, 0, 0, 13]
        || &bytes[12..16] != b"IHDR"
        || bytes[24] != 8
        || bytes[25] != 0
    {
        return Err(fail(
            "mask must be an 8-bit grayscale PNG without alpha".into(),
        ));
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    if (width, height) != (source.width, source.height) {
        return Err(fail(format!(
            "mask is {width}x{height}, expected {}x{}",
            source.width, source.height
        )));
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(source.width);
    limits.max_image_height = Some(source.height);
    limits.max_alloc = Some(64 * 1024 * 1024);
    let decoder = image::codecs::png::PngDecoder::with_limits(Cursor::new(bytes), limits)
        .map_err(|error| fail(error.to_string()))?;
    if decoder.color_type() != image::ColorType::L8 {
        return Err(fail(
            "mask must decode to grayscale coverage without alpha".into(),
        ));
    }
    let mut pixels = vec![0; source.len()];
    decoder
        .read_image(&mut pixels)
        .map_err(|error| fail(error.to_string()))?;
    let crc32 = crc32fast::hash(&pixels);
    Ok((pixels, crc32))
}

/// The flattened picture of a PSD: its visible layers composited by the importer and the CPU renderer, so groups,
/// opacity, clipping and the blend modes the importer maps all count. The merged image stored in the file is never
/// read, because it is absent or stale when Photoshop saved without "Maximize Compatibility" and it carries no alpha.
fn flatten_psd(path: &Path) -> Result<image::RgbaImage, String> {
    let name = path.file_name().and_then(|name| name.to_str()).ok_or("file name is not valid UTF-8")?;
    let fail = |message: String| format!("Could not load {name}: {message}");
    let bytes = crate::read_bounded_file(path.to_str().ok_or("path is not valid UTF-8")?, crate::LayerFile::Psd.max_bytes(), "PSD")
        .map_err(&fail)?;
    let limits = psd::Limits::default();
    let document = psd::parse(&bytes, &limits).map_err(&fail)?;
    // Before the import allocates a texture per layer.
    model_import::check_picture_size(name, document.width, document.height)?;
    if !document.layers.iter().any(|layer| layer.has_pixel_data()) {
        return Err(fail("the PSD has no layer with pixels (a PSD saved without layers is not supported)".into()));
    }
    let options = model_import::ImportOptions { exact_quads: true, ..model_import::ImportOptions::default() };
    let imported = model_import::import_psd_for_split(&bytes, &options, &limits).map_err(&fail)?;
    let mut parsed = crate::document::Document::parse(&model_import::inx_bytes(&imported.model, &imported.textures)?)?;
    group_psd_clipping_stacks(&mut parsed.model)?;
    // A clipping stack is one more Composite. Where the groups already nest as deep as the renderer allows, the
    // picture is drawn as the import leaves it instead, with stencil clips that ignore fractional base alpha.
    let stacks = composite_depth(&parsed.model["nodes"]) <= crate::render::MAX_COMPOSITE_DEPTH;
    if !stacks {
        let imported = model_import::import_psd(&bytes, &options, &limits).map_err(&fail)?;
        parsed = crate::document::Document::parse(&model_import::inx_bytes(&imported.model, &imported.textures)?)?;
    }
    crate::prepare_model(&mut parsed.model)?;
    let scratch = Session::from_document(parsed, 1, None, None);
    let (evaluated, geometry) = scratch.evaluate_scene(&json!({}))?;
    let (width, height) = (document.width, document.height);
    // The layer import places the document centre at integer halves.
    let (left, top) = (-f64::from(width / 2), -f64::from(height / 2));
    let rgba = render_in_strips(height, |first, rows| {
        let bounds = [left, top + f64::from(first), left + f64::from(width), top + f64::from(first + rows)];
        let textures = scratch.document.textures();
        let png = match stacks {
            true => crate::render::render_png_psd(&evaluated, &geometry, textures, width, rows, bounds)?,
            false => crate::render::render_png_lit(&evaluated, &geometry, textures, width, rows, bounds, [1.0; 3])?,
        };
        let strip = image::load_from_memory_with_format(&png, image::ImageFormat::Png).map_err(|error| error.to_string())?;
        Ok(strip.to_rgba8().into_raw())
    })
    .map_err(&fail)?;
    image::RgbaImage::from_raw(width, height, rgba).ok_or_else(|| fail("the render has another size".into()))
}

fn composite_depth(node: &Value) -> usize {
    let below = node["children"].as_array().into_iter().flatten().map(composite_depth).max().unwrap_or(0);
    below + usize::from(matches!(node["type"].as_str(), Some("Composite" | "DynamicComposite")))
}

fn group_psd_clipping_stacks(model: &mut Value) -> Result<(), String> {
    fn sources(node: &Value) -> Vec<u64> {
        if matches!(node["type"].as_str(), Some("Part" | "Composite" | "DynamicComposite")) {
            return node["uuid"].as_u64().into_iter().collect();
        }
        node["children"].as_array().into_iter().flatten()
            .filter(|child| child["enabled"] != false).flat_map(sources).collect()
    }
    fn masks(node: &Value) -> Vec<u64> {
        let mut ids: Vec<_> = node["masks"].as_array().into_iter().flatten()
            .filter_map(|mask| mask["source"].as_u64()).collect();
        ids.sort_unstable();
        ids
    }
    fn group(node: &mut Value, next_id: &mut u32) -> Result<(), String> {
        let Some(children) = node["children"].as_array_mut() else { return Ok(()); };
        let original_sources: std::collections::BTreeMap<_, _> = children.iter().map(|child| {
            let mut ids = sources(child);
            ids.sort_unstable();
            (child["uuid"].as_u64().unwrap_or(0), ids)
        }).collect();
        for child in children.iter_mut() {
            group(child, next_id)?;
        }
        // Imported siblings are bottom-to-top; consecutive clipped layers share the preceding unclipped base.
        let mut start = 0;
        while start < children.len() {
            if !masks(&children[start]).is_empty() { start += 1; continue; }
            let base_sources = &original_sources[&children[start]["uuid"].as_u64().unwrap_or(0)];
            let mut end = start + 1;
            while end < children.len() && !base_sources.is_empty() && masks(&children[end]) == *base_sources {
                end += 1;
            }
            if end == start + 1 { start += 1; continue; }
            let mut stack: Vec<_> = children.drain(start..end).collect();
            let base = stack.first_mut().expect("clipping stack has a base");
            let blend = base.get("blend_mode").cloned().unwrap_or_else(|| json!("Normal"));
            let opacity = base.get("opacity").cloned().unwrap_or_else(|| json!(1.0));
            let (enabled, zsort) = (base["enabled"].clone(), base["zsort"].clone());
            base["blend_mode"] = json!("Normal");
            base["opacity"] = json!(1.0);
            let id = *next_id;
            *next_id = next_id.checked_add(1).filter(|id| *id < u32::MAX).ok_or("clipping stack UUID limit")?;
            children.insert(start, json!({
                "uuid": id, "name": "Clipping stack", "type": "Composite", "enabled": enabled,
                "zsort": zsort, "opacity": opacity, "blend_mode": blend,
                "transform": {"trans": [0, 0, 0], "rot": [0, 0, 0], "scale": [1, 1]},
                "children": stack,
            }));
            start += 1;
        }
        Ok(())
    }
    let mut next_id = crate::fresh_resource_id(model)?;
    group(&mut model["nodes"], &mut next_id)
}

/// The rows of a picture `height` tall, drawn by `render(first row, rows)` in as few equal strips as the renderer's
/// limits allow. Its work limit counts pixel visits, and a stack of layers visits every pixel once per layer and more;
/// its memory limit counts target-sized layers, one for the picture and one for every nested group being drawn, and
/// also the decoded layers, which no strip shrinks. So the first memory refusal draws one row: if that is refused too,
/// what a shorter strip cannot shrink (the decoded layers and one row of each buffer) is over the limit, and no number
/// of strips can help.
fn render_in_strips(height: u32, mut render: impl FnMut(u32, u32) -> Result<Vec<u8>, String>) -> Result<Vec<u8>, String> {
    let (mut strips, mut probed) = (1, false);
    loop {
        let rows = height.div_ceil(strips).max(1);
        let drawn = (0..height).step_by(rows as usize).try_fold(Vec::new(), |mut picture, first| {
            picture.extend(render(first, rows.min(height - first))?);
            Ok::<_, String>(picture)
        });
        match drawn {
            Err(message) if message.contains(RASTER_WORK_REFUSAL) || message.contains(RENDER_MEMORY_REFUSAL) => {
                let memory = message.contains(RENDER_MEMORY_REFUSAL);
                let mut hopeless = false;
                if memory && !probed && rows > 1 {
                    probed = true;
                    hopeless = matches!(render(0, 1), Err(refusal) if refusal.contains(RENDER_MEMORY_REFUSAL));
                }
                if hopeless || strips >= MAX_FLATTEN_STRIPS || rows == 1 {
                    return Err(match memory {
                        true => "the layers need more working memory than the renderer's 640-megabyte limit allows".to_owned(),
                        false => format!(
                            "the layers need more render work than {MAX_FLATTEN_STRIPS} strips of the renderer's limit allow"
                        ),
                    });
                }
                strips *= 2;
            }
            drawn => return drawn,
        }
    }
}

fn plan_json(source: &Source, parts: &[PartSpec]) -> Value {
    json!(Plan::of_draft(source, parts))
}

// ------------------------------------------------------------------------------------------------- the ops

impl Session {
    pub(crate) fn split_mask_names(&self) -> impl Iterator<Item = &str> {
        self.part_split.parts.iter().map(|part| part.name.as_str())
    }

    pub(crate) fn split_image_png(&self, image: SplitImage<'_>) -> Result<Vec<u8>, String> {
        self.require_inx("split image resources")?;
        let source = self.part_split.source()?;
        let mut budget = new_budget(source);
        let (pixels, color): (Cow<'_, [u8]>, image::ExtendedColorType) = match image {
            SplitImage::Source => (Cow::Borrowed(&source.rgba), image::ExtendedColorType::Rgba8),
            SplitImage::Mask(name) => {
                let index = self.part_split.parts.iter().position(|part| part.name == name)
                    .ok_or_else(|| format!("part {name:?} was not found"))?;
                let mut masks = evaluate_all(source, &self.part_split.parts, &mut budget)?;
                (Cow::Owned(masks.swap_remove(index)), image::ExtendedColorType::L8)
            }
        };
        charge(&mut budget, source.len() as u64)?;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&pixels, source.width, source.height, color)
            .map_err(|error| error.to_string())?;
        Ok(png)
    }

    /// Whether the current draft's canonical plan has live Parts in the model (read-only, in-memory only).
    pub(crate) fn split_applied(&self) -> bool {
        if !self.document.is_inx() {
            return false;
        }
        let Some(entry) = self.history.get(self.history_position) else { return false };
        self.part_split.applied(&entry.split_applications)
    }

    /// `split.open`: the image at `path` (PNG, TGA, JPEG or a flattened PSD) becomes the draft source; parts are cleared.
    pub(crate) fn op_split_open(&mut self, request: &Value) -> Result<Value, String> {
        let source = load_source(req_str(request, "path")?)?;
        let result = json!({"width": source.width, "height": source.height});
        self.part_split.source = Some(source);
        self.part_split.parts.clear();
        self.part_split.touch();
        let mut result = result;
        result["revision"] = json!(self.part_split.revision);
        Ok(result)
    }

    /// `split.part.set`: add or replace a part by name. `index` is its place in the draw order (0 is the back).
    pub(crate) fn op_split_part_set(&mut self, request: &Value) -> Result<Value, String> {
        let state = &self.part_split;
        let source = state.source()?;
        let name = req_str(request, "name")?.to_owned();
        let steps: Vec<MaskStep> = serde_json::from_value(request.get("steps").cloned().ok_or("steps is required")?)
            .map_err(|error| format!("steps: {error}"))?;
        let padding = opt_u32(request, "padding")?.unwrap_or(0);
        let index = opt_u32(request, "index")?.map(|index| index as usize);
        let mut parts = state.parts.clone();
        let spec = PartSpec { name, steps, padding };
        let existing = parts.iter().position(|part| part.name == spec.name);
        let at = match (existing, index) {
            (Some(at), None) => {
                parts[at] = spec;
                at
            }
            (existing, index) => {
                if let Some(at) = existing {
                    parts.remove(at);
                }
                let at = index.unwrap_or(parts.len());
                if at > parts.len() {
                    return Err(format!("index must be at most {}", parts.len()));
                }
                parts.insert(at, spec);
                at
            }
        };
        validate(&parts, source.width, source.height)?;
        let mut budget = new_budget(source);
        let masks = evaluate_all(source, &parts, &mut budget)?;
        // Refuse a draft that apply could not render, while it is still being shaped.
        check_textures(source, &parts, &masks, &mut budget)?;
        let mut stats = statistics(source, &parts, &masks, &mut budget)?;
        let result = json!({
            "part": stats["parts"][at].clone(),
            "index": at,
            "parts": parts.len(),
            "unclaimedPixels": stats["unclaimedPixels"].take(),
            "multiplyClaimedPixels": stats["multiplyClaimedPixels"].take(),
        });
        self.part_split.parts = parts;
        self.part_split.touch();
        let mut result = result;
        result["revision"] = json!(self.part_split.revision);
        Ok(result)
    }

    /// `split.part.remove`: drop a part that no other part refers to.
    pub(crate) fn op_split_part_remove(&mut self, request: &Value) -> Result<Value, String> {
        self.part_split.source()?;
        let name = req_str(request, "name")?;
        let parts = &mut self.part_split.parts;
        let at = parts.iter().position(|part| part.name == name).ok_or_else(|| format!("part {name:?} was not found"))?;
        if let Some(user) = parts.iter().find(|part| step_refs(part).any(|reference| reference == name)) {
            return Err(format!("part {name:?} is referenced by part {:?}", user.name));
        }
        parts.remove(at);
        self.part_split.touch();
        Ok(json!({"removed": name, "parts": self.part_split.parts.len(), "revision": self.part_split.revision}))
    }

    /// `split.part.order`: the draw order, back to front, as a permutation of the part names.
    pub(crate) fn op_split_part_order(&mut self, request: &Value) -> Result<Value, String> {
        self.part_split.source()?;
        let names: Vec<&str> = request
            .get("names")
            .and_then(Value::as_array)
            .ok_or("names must be an array of part names")?
            .iter()
            .map(|name| name.as_str().ok_or("names must hold strings"))
            .collect::<Result<_, _>>()?;
        let parts = &self.part_split.parts;
        let mut ordered = Vec::with_capacity(parts.len());
        for name in &names {
            let part = parts.iter().find(|part| part.name == *name).ok_or_else(|| format!("part {name:?} was not found"))?;
            if ordered.iter().any(|seen: &PartSpec| seen.name == part.name) {
                return Err(format!("part {name:?} is listed twice"));
            }
            ordered.push(part.clone());
        }
        if ordered.len() != parts.len() {
            return Err(format!("names must list all {} parts", parts.len()));
        }
        self.part_split.parts = ordered;
        self.part_split.touch();
        Ok(json!({"order": names, "revision": self.part_split.revision}))
    }

    /// `split.plan`: the plan JSON and the statistics of every part (read-only).
    pub(crate) fn op_split_plan(&self, _request: &Value) -> Result<Value, String> {
        let source = self.part_split.source()?;
        let parts = &self.part_split.parts;
        let mut budget = new_budget(source);
        let masks = evaluate_all(source, parts, &mut budget)?;
        let mut result = statistics(source, parts, &masks, &mut budget)?;
        result["plan"] = plan_json(source, parts);
        result["applied"] = json!(self.split_applied());
        Ok(result)
    }

    pub(crate) fn op_split_components(&self, request: &Value) -> Result<Value, String> {
        let source = self.part_split.source()?;
        let threshold = u8::try_from(opt_u32(request, "threshold")?.unwrap_or(0))
            .map_err(|_| "threshold must be an integer from 0 to 255")?;
        let offset = opt_u32(request, "offset")?.unwrap_or(0);
        components(source, threshold, offset, &mut new_budget(source))
    }

    pub(crate) fn op_split_mask_inspect(&self, request: &Value) -> Result<Value, String> {
        let source = self.part_split.source()?;
        let path = req_str(request, "path")?;
        let (_, crc32) = load_raster(source, path, &mut new_budget(source))?;
        let canonical = Path::new(path)
            .canonicalize()
            .map_err(|error| format!("raster {path:?}: {error}"))?;
        let canonical = canonical
            .to_str()
            .ok_or_else(|| format!("raster {path:?}: resolved path is not valid UTF-8"))?;
        let step = MaskStep::Raster {
            path: canonical.to_owned(),
            crc32,
            mode: Mode::Add,
        };
        Ok(json!({"step":step, "width":source.width, "height":source.height}))
    }

    /// `split.overlay`: the source with each part tinted and outlined, as a PNG (read-only).
    pub(crate) fn op_split_overlay(&self, request: &Value) -> Result<Value, String> {
        let source = self.part_split.source()?;
        let parts = &self.part_split.parts;
        let only = match request.get("part") {
            None | Some(Value::Null) => None,
            Some(Value::String(name)) => {
                Some(parts.iter().position(|part| &part.name == name).ok_or_else(|| format!("part {name:?} was not found"))?)
            }
            Some(_) => return Err("part must be a string".into()),
        };
        let mut budget = new_budget(source);
        let masks = evaluate_all(source, parts, &mut budget)?;
        let png = overlay_png(source, &masks, only, &mut budget)?;
        Ok(json!({
            "png": base64::engine::general_purpose::STANDARD.encode(png),
            "width": source.width,
            "height": source.height,
        }))
    }

    /// `split.close`: drop the draft.
    pub(crate) fn op_split_close(&mut self) -> Result<Value, String> {
        let was_open = self.part_split.source.take().is_some();
        self.part_split.parts.clear();
        self.part_split.touch();
        Ok(json!({"closed": was_open, "revision": self.part_split.revision}))
    }

    /// `split.apply`: one Part per part of the draft (or of `plan`, replayed without a draft) under `parent`, in
    /// ONE undo step. `automesh` (`true`, or an `automesh.generate` request) meshes each new Part in the same step.
    pub(crate) fn op_split_apply(&mut self, request: &Value) -> Result<Value, String> {
        self.require_inx("split.apply")?;
        let automesh = match request.get("automesh") {
            None | Some(Value::Null) | Some(Value::Bool(false)) => None,
            Some(Value::Bool(true)) => Some(json!({"algorithm": "contour"})),
            Some(options @ Value::Object(_)) => {
                let mut options = options.clone();
                if options.get("algorithm").is_none() {
                    options["algorithm"] = json!("contour");
                }
                Some(automesh::with_user_preset(self.config_dir.as_deref(), &options)?)
            }
            Some(_) => return Err("automesh must be a boolean or an automesh.generate request".into()),
        };
        let (images, plan) = match request.get("plan") {
            None | Some(Value::Null) => {
                let state = &self.part_split;
                let source = state.source()?;
                (render_parts(source, &state.parts)?, Plan::of_draft(source, &state.parts))
            }
            Some(plan) => {
                let plan: Plan = serde_json::from_value(plan.clone()).map_err(|error| format!("plan: {error}"))?;
                if plan.version != PLAN_VERSION {
                    return Err(format!("plan version must be {PLAN_VERSION}"));
                }
                let source = load_source(&plan.source.path)?;
                if (source.width, source.height) != (plan.source.width, plan.source.height) {
                    return Err(format!(
                        "the source is {}x{} but the plan recorded {}x{}",
                        source.width, source.height, plan.source.width, plan.source.height
                    ));
                }
                if source.crc32 != plan.source.crc32 {
                    return Err("the source pixels differ from those the plan recorded (checksum mismatch)".into());
                }
                (render_parts(&source, &plan.parts)?, plan)
            }
        };
        let root = self.document.model["nodes"]["uuid"].as_u64().and_then(crate::valid_id).ok_or("root has no uuid")?;
        let parent = match request.get("parent") {
            None | Some(Value::Null) => root,
            Some(value) => value.as_u64().and_then(crate::valid_id).ok_or("parent must be a node UUID")?,
        };
        let mut merged = model_import::add_image_parts(&self.document.model, &self.model_textures(), parent, &images)?;
        let (mut meshed, mut skipped) = (0, Vec::new());
        if let Some(options) = automesh {
            let blobs = crate::app_shell::blobs(&merged.textures);
            for id in merged.created.clone() {
                let mut mesh_request = options.clone();
                mesh_request["id"] = json!(id);
                let result = automesh::generate_for_part(&merged.model, &blobs, &mesh_request)
                    .map_err(|error| format!("automesh failed for part {id}: {error}"))?;
                match result.candidate {
                    Some(candidate) => {
                        merged.model = candidate;
                        meshed += 1;
                    }
                    None => skipped.push(id),
                }
            }
        }
        let revision = self.revision;
        self.commit_merged(&merged)?;
        debug_assert_ne!(self.revision, revision, "split.apply must commit one step");
        if let Some(entry) = self.history.get_mut(self.history_position) {
            entry.split_applications.record(plan, &merged.created);
        }
        let created: Vec<Value> = merged
            .created
            .iter()
            .zip(&images)
            .map(|(id, image)| json!({"id": id, "name": image.name, "size": [image.width, image.height], "trans": [image.trans[0], image.trans[1]]}))
            .collect();
        Ok(json!({"created": created, "parent": parent, "automesh": {"meshed": meshed, "skipped": skipped}}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raster_reads_share_the_request_budget_and_charge_before_data_io() {
        let source = picture();
        let missing = "/soul2d-raster-budget-must-not-open/missing.png";
        assert!(load_raster(&source, missing, &mut WorkBudget::new(0))
            .unwrap_err()
            .contains("split work limit"));
        let dir = std::env::temp_dir().join(format!(
            "soul2d-raster-budget-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let dir = crate::test_scratch::scratch(dir);
        let path = dir.join("mask.png");
        image::GrayImage::from_pixel(source.width, source.height, image::Luma([128]))
            .save(&path)
            .unwrap();
        let encoded = std::fs::metadata(&path).unwrap().len();
        let cost = 2 * source.len() as u64 + encoded;
        let mut budget = WorkBudget::new(cost);
        assert_eq!(
            load_raster(&source, path.to_str().unwrap(), &mut budget).unwrap(),
            (vec![128; 96], crc32fast::hash(&[128; 96]))
        );
        assert_eq!(budget.remaining(), 0);
        assert!(load_raster(&source, missing, &mut budget)
            .unwrap_err()
            .contains("split work limit"));
        std::fs::write(&path, [0; 32]).unwrap();
        let mut short = WorkBudget::new(2 * source.len() as u64 + 31);
        assert!(load_raster(&source, path.to_str().unwrap(), &mut short)
            .unwrap_err()
            .contains("split work limit"));
    }

    #[test]
    fn components_follow_winding_regions_and_refuse_budget_exhaustion() {
        let alpha = [255, 0, 0, 255, 255, 255, 0, 255, 0, 255, 255, 255];
        let source = Source {
            path: String::new(),
            width: 4,
            height: 3,
            rgba: alpha
                .into_iter()
                .flat_map(|alpha| [1, 2, 3, alpha])
                .collect(),
            crc32: 0,
        };
        assert!(components(&source, 0, 0, &mut WorkBudget::new(12))
            .unwrap_err()
            .contains("split work limit"));
        assert_eq!(
            components(&source, 0, 0, &mut WorkBudget::new(44)).unwrap(),
            json!({"components":[{"seed":[0,0], "pixels":8, "bbox":[0,0,4,3]}],
                "totalComponents":1, "nextOffset":null})
        );
        assert_eq!(
            components(&source, 255, 0, &mut WorkBudget::new(12)).unwrap(),
            json!({"components":[], "totalComponents":0, "nextOffset":null})
        );
        let one = Source {
            path: String::new(),
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 1],
            crc32: 0,
        };
        assert_eq!(
            components(&one, 0, 0, &mut WorkBudget::new(5)).unwrap(),
            json!({"components":[{"seed":[0,0], "pixels":1, "bbox":[0,0,1,1]}],
                "totalComponents":1, "nextOffset":null})
        );
    }

    #[test]
    fn strips_double_until_each_render_fits_and_join_in_order() {
        // A render that refuses more than three rows: ten rows take strips of three, three, three and one.
        let mut calls = Vec::new();
        let picture = render_in_strips(10, |first, rows| {
            if rows > 3 {
                return Err(format!("node 7: {RASTER_WORK_REFUSAL}"));
            }
            calls.push((first, rows));
            Ok((first..first + rows).map(|row| row as u8).collect())
        });
        assert_eq!(picture.unwrap(), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(calls, [(0, 3), (3, 3), (6, 3), (9, 1)]);
        // The memory limit shrinks with the strip as well: more than five rows do not fit, five do.
        let rows = render_in_strips(10, |_, rows| match rows > 5 {
            true => Err(RENDER_MEMORY_REFUSAL.to_owned()),
            false => Ok(vec![rows as u8]),
        });
        assert_eq!(rows.unwrap(), [5, 5]);
    }

    #[test]
    fn strips_stop_at_the_cap_and_pass_any_other_refusal_through() {
        let mut calls = 0;
        let work = render_in_strips(4096, |_, _| {
            calls += 1;
            Err(RASTER_WORK_REFUSAL.to_owned())
        });
        assert_eq!(work.unwrap_err(), "the layers need more render work than 64 strips of the renderer's limit allow");
        assert_eq!(calls, 7, "one, two, four and so on up to sixty-four strips, each stopped at its first");
        let mut calls = 0;
        let other = render_in_strips(4096, |_, _| {
            calls += 1;
            Err("no".to_owned())
        });
        assert_eq!((other.unwrap_err().as_str(), calls), ("no", 1));
        // Two rows cannot be cut into more than two strips.
        let rows = render_in_strips(2, |_, _| Err(RASTER_WORK_REFUSAL.to_owned()));
        assert_eq!(rows.unwrap_err(), "the layers need more render work than 64 strips of the renderer's limit allow");
    }

    #[test]
    fn a_memory_refusal_no_strip_can_clear_is_named_and_not_retried() {
        // Decoded layers over the limit: even one row is refused, so the whole picture and the one-row probe are all.
        let mut calls = Vec::new();
        let textures = render_in_strips(4096, |first, rows| {
            calls.push((first, rows));
            Err(format!("node 3: {RENDER_MEMORY_REFUSAL}"))
        });
        assert_eq!(textures.unwrap_err(), "the layers need more working memory than the renderer's 640-megabyte limit allows");
        assert_eq!(calls, [(0, 4096), (0, 1)]);
        // Strip-sized layers still over the limit at sixty-four strips, though one row fits: the same limit is named.
        let mut calls = 0;
        let layers = render_in_strips(4096, |_, rows| {
            calls += 1;
            match rows > 1 {
                true => Err(RENDER_MEMORY_REFUSAL.to_owned()),
                false => Ok(vec![0]),
            }
        });
        assert_eq!(layers.unwrap_err(), "the layers need more working memory than the renderer's 640-megabyte limit allows");
        assert_eq!(calls, 7 + 1, "seven attempts, each stopped at its first strip, and one probe of a single row");
    }

    /// A 12x8 picture: a red 4x4 block (x 1..5, y 2..6) with a black line down its middle column x=3, a blue 3x4
    /// block (x 8..11, y 2..6), everything else transparent.
    fn picture() -> Source {
        let (width, height) = (12u32, 8u32);
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let colour = if (1..5).contains(&x) && (2..6).contains(&y) {
                    Some(if x == 3 { [0, 0, 0, 255] } else { [200, 30, 30, 255] })
                } else if (8..11).contains(&x) && (2..6).contains(&y) {
                    Some([20, 30, 200, 255])
                } else {
                    None
                };
                if let Some(colour) = colour {
                    let at = ((y * width + x) * 4) as usize;
                    rgba[at..at + 4].copy_from_slice(&colour);
                }
            }
        }
        Source { path: "picture.png".into(), width, height, crc32: crc32fast::hash(&rgba), rgba }
    }

    fn spec(name: &str, steps: Value) -> PartSpec {
        PartSpec { name: name.into(), steps: serde_json::from_value(steps).unwrap(), padding: 0 }
    }

    fn eval(source: &Source, spec: &PartSpec) -> Vec<u8> {
        evaluate_part(source, spec, |_| &[], &mut WorkBudget::new(source.len() as u64 * WORK_PER_PIXEL)).unwrap()
    }

    fn claimed(source: &Source, steps: Value) -> Vec<(usize, usize)> {
        let mask = eval(source, &spec("probe", steps));
        (0..source.len()).filter(|&i| mask[i] >= CLAIMED).map(|i| (i % source.width as usize, i / source.width as usize)).collect()
    }

    #[test]
    fn steps_parse_from_json_with_defaults_and_refuse_unknown_fields() {
        let step: MaskStep = serde_json::from_value(json!({"op": "alpha"})).unwrap();
        assert_eq!(step, MaskStep::Alpha { threshold: 0, mode: Mode::Add });
        let step: MaskStep = serde_json::from_value(json!({"op": "boundedFill", "point": [1, 2], "lineThreshold": 9, "mode": "subtract"})).unwrap();
        assert_eq!(step, MaskStep::BoundedFill { point: [1, 2], line_threshold: 9, mode: Mode::Subtract });
        let step: MaskStep = serde_json::from_value(json!({"op": "side", "axisX": 4, "keep": "right"})).unwrap();
        assert_eq!(step, MaskStep::Side { axis_x: 4, keep: Keep::Right });
        assert!(serde_json::from_value::<MaskStep>(json!({"op": "alpha", "treshold": 1})).is_err());
        assert!(serde_json::from_value::<MaskStep>(json!({"op": "wand"})).is_err());
        let again = serde_json::to_value(MaskStep::Rect { rect: [1, 2, 3, 4], mode: Mode::Intersect }).unwrap();
        assert_eq!(again, json!({"op": "box", "rect": [1, 2, 3, 4], "mode": "intersect"}));
    }

    #[test]
    fn alpha_and_component_select_by_alpha_and_connectivity() {
        let source = picture();
        assert_eq!(claimed(&source, json!([{"op": "alpha"}])).len(), 16 + 12);
        let blue = claimed(&source, json!([{"op": "component", "point": [9, 3]}]));
        assert_eq!(blue.len(), 12);
        assert!(blue.iter().all(|&(x, _)| (8..11).contains(&x)));
        assert!(claimed(&source, json!([{"op": "component", "point": [6, 6]}])).is_empty(), "a transparent seed selects nothing");
    }

    #[test]
    fn flood_follows_colour_and_bounded_fill_stops_at_line_art() {
        let source = picture();
        // The black line splits the red block in two columns on the left (x=1,2) and one on the right (x=4).
        assert_eq!(claimed(&source, json!([{"op": "flood", "point": [1, 3], "tolerance": 0}])).len(), 8);
        assert_eq!(claimed(&source, json!([{"op": "boundedFill", "point": [4, 3]}])).len(), 4);
        // A loose tolerance crosses the line but not into transparency (alpha counts as a channel).
        assert_eq!(claimed(&source, json!([{"op": "flood", "point": [1, 3], "tolerance": 200}])).len(), 16);
        assert!(claimed(&source, json!([{"op": "boundedFill", "point": [3, 3]}])).is_empty(), "a line seed selects nothing");
    }

    #[test]
    fn modes_combine_selections() {
        let source = picture();
        let both = json!([{"op": "alpha"}, {"op": "box", "rect": [0, 0, 6, 8], "mode": "intersect"}]);
        assert_eq!(claimed(&source, both).len(), 16);
        let minus = json!([{"op": "alpha"}, {"op": "box", "rect": [0, 0, 6, 8], "mode": "subtract"}]);
        assert_eq!(claimed(&source, minus).len(), 12);
        let sum = json!([{"op": "box", "rect": [0, 0, 2, 2]}, {"op": "box", "rect": [1, 1, 2, 2]}]);
        assert_eq!(claimed(&source, sum).len(), 7);
    }

    #[test]
    fn polygon_box_and_side_cut_regions() {
        let source = picture();
        let triangle = json!([{"op": "polygon", "points": [[0, 0], [8, 0], [0, 8]]}]);
        // Pixel centres below the hypotenuse: x + y < 7, which is 7 + 6 + ... + 1 pixels.
        assert_eq!(claimed(&source, triangle).len(), 28);
        let left = claimed(&source, json!([{"op": "alpha"}, {"op": "side", "axisX": 3, "keep": "left"}]));
        assert!(left.iter().all(|&(x, _)| x < 3));
        assert_eq!(left.len(), 8);
        let right = claimed(&source, json!([{"op": "alpha"}, {"op": "side", "axisX": 3, "keep": "right"}]));
        assert_eq!(right.len(), 16 + 12 - 8);
    }

    #[test]
    fn grow_shrink_and_feather_change_the_edge() {
        let source = picture();
        let block = json!([{"op": "box", "rect": [4, 2, 4, 4]}]);
        assert_eq!(claimed(&source, block.clone()).len(), 16);
        let mut grown = block.as_array().unwrap().clone();
        grown.push(json!({"op": "grow", "radius": 1}));
        // A 4x4 block grown by one pixel (4-neighbours) gains 4 sides of 4 pixels.
        assert_eq!(claimed(&source, json!(grown)).len(), 16 + 16);
        let mut shrunk = block.as_array().unwrap().clone();
        shrunk.push(json!({"op": "shrink", "radius": 1}));
        assert_eq!(claimed(&source, json!(shrunk)).len(), 4);
        let mut soft = block.as_array().unwrap().clone();
        soft.push(json!({"op": "feather", "radius": 1}));
        let mask = eval(&source, &spec("soft", json!(soft)));
        assert!(mask.iter().any(|&value| value > 0 && value < 255), "feather makes grey edge pixels");
    }

    #[test]
    fn feather_is_a_clamped_box_blur_with_literal_values() {
        let budget = &mut WorkBudget::new(1 << 20);
        // One lit pixel in a row: each neighbour in the window gets (255 + 1) / 3 = 85, the edges stay clamped.
        assert_eq!(feather(&[0, 0, 255, 0, 0], 5, 1, 1, budget).unwrap(), [0, 85, 85, 85, 0]);
        // Radius 2 spreads over 5 pixels: (255 + 2) / 5 = 51.
        assert_eq!(feather(&[0, 0, 0, 255, 0, 0, 0], 7, 1, 2, budget).unwrap(), [0, 51, 51, 51, 51, 51, 0]);
        // Both axes: 255 -> 85 across, then (85 + 1) / 3 = 28 down.
        assert_eq!(feather(&[0, 0, 0, 0, 255, 0, 0, 0, 0], 3, 3, 1, budget).unwrap(), [28; 9]);
        // A fully covered mask stays fully covered, whatever the clamping.
        assert_eq!(feather(&[255; 12], 4, 3, 5, budget).unwrap(), [255; 12]);
    }

    #[test]
    fn modes_round_to_literal_coverage() {
        let mut mask = [255, 200, 10, 0];
        combine(&mut mask, &[128, 128, 255, 77], Mode::Subtract);
        // (value * (255 - chosen) + 127) / 255
        assert_eq!(mask, [127, 100, 0, 0]);
        let mut mask = [255, 200, 10];
        combine(&mut mask, &[128, 255, 0], Mode::Intersect);
        assert_eq!(mask, [128, 200, 0]);
        combine(&mut mask, &[0, 201, 7], Mode::Add);
        assert_eq!(mask, [128, 201, 7]);
    }

    #[test]
    fn partial_coverage_gives_literal_premultiplied_bytes() {
        // A 2x2 source of [200, 100, 50] at alpha 128 under the coverage 255, 128, 0, 255.
        let rgba = [200, 100, 50, 128].repeat(4);
        let source = Source { path: "half.png".into(), width: 2, height: 2, crc32: 0, rgba };
        let image = part_image(&source, &spec("half", json!([{"op": "alpha"}])), &[255, 128, 0, 255]).unwrap();
        let decoded = image::load_from_memory(&image.texture.data).unwrap().to_rgba8();
        // Texture alpha is (128 * coverage + 127) / 255: 128, 64, 0, 128. Colour is floor(channel * alpha / 255).
        assert_eq!(decoded.as_raw(), &[100, 50, 25, 128, 50, 25, 12, 64, 0, 0, 0, 0, 100, 50, 25, 128]);
    }

    #[test]
    fn morphology_works_at_the_image_edge_and_on_a_full_mask() {
        let source = picture();
        let corner = json!([{"op": "box", "rect": [0, 0, 3, 3]}, {"op": "grow", "radius": 1}]);
        // The image edge is not background: the block grows right and down only, 9 + 3 + 3 pixels.
        assert_eq!(claimed(&source, corner).len(), 15);
        let whole = json!([{"op": "box", "rect": [0, 0, 12, 8]}, {"op": "shrink", "radius": 3}]);
        assert_eq!(claimed(&source, whole).len(), 96, "nothing unclaimed to shrink from");
        let empty = json!([{"op": "box", "rect": [0, 0, 3, 3]}, {"op": "shrink", "radius": 3}, {"op": "grow", "radius": 2}]);
        assert!(claimed(&source, empty).is_empty(), "the corner pixel is 3 from the nearest unclaimed one");
    }

    #[test]
    fn scanline_polygon_matches_a_per_pixel_even_odd_test() {
        let (width, height) = (30usize, 24usize);
        let mut seed = 99u32;
        let mut next = |limit: u32| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 16) % (limit + 1)
        };
        for _ in 0..20 {
            let points: Vec<[u32; 2]> = (0..9).map(|_| [next(width as u32), next(height as u32)]).collect();
            let out = polygon(width, height, &points, &mut WorkBudget::new(u64::MAX)).unwrap();
            for index in 0..width * height {
                let (px, py) = ((index % width) as f64 + 0.5, (index / width) as f64 + 0.5);
                let mut inside = false;
                let mut previous = points[points.len() - 1];
                for &current in &points {
                    let (x0, y0) = (f64::from(previous[0]), f64::from(previous[1]));
                    let (x1, y1) = (f64::from(current[0]), f64::from(current[1]));
                    if (y0 > py) != (y1 > py) && px < (x1 - x0) * (py - y0) / (y1 - y0) + x0 {
                        inside = !inside;
                    }
                    previous = current;
                }
                assert_eq!(out[index] != 0, inside, "{points:?} pixel {index}");
            }
        }
    }

    #[test]
    fn cropped_morphology_matches_a_brute_force_distance() {
        let (width, height) = (23usize, 17usize);
        let mut seed = 12345u32;
        for density in [2u32, 6, 9] {
            for radius in [1u32, 2, 5] {
                let mask: Vec<u8> = (0..width * height)
                    .map(|_| {
                        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        if (seed >> 24) % 10 < density { 255 } else { 0 }
                    })
                    .collect();
                let nearest = |index: usize, target: bool| {
                    (0..mask.len())
                        .filter(|&other| (mask[other] >= CLAIMED) == target)
                        .map(|other| {
                            let (dx, dy) = ((index % width).abs_diff(other % width), (index / width).abs_diff(other / width));
                            (dx * dx + dy * dy) as u32
                        })
                        .min()
                        .unwrap_or(u32::MAX)
                };
                let budget = &mut WorkBudget::new(u64::MAX);
                let grown = morph(&mask, width, height, radius, true, budget).unwrap();
                let shrunk = morph(&mask, width, height, radius, false, budget).unwrap();
                for index in 0..mask.len() {
                    let claimed = mask[index] >= CLAIMED;
                    assert_eq!(grown[index] != 0, nearest(index, true) <= radius * radius, "grow {index} r{radius}");
                    assert_eq!(shrunk[index] != 0, claimed && nearest(index, false) > radius * radius, "shrink {index} r{radius}");
                }
            }
        }
    }

    #[test]
    fn grow_and_shrink_are_exact_on_a_row_wider_than_4096_pixels() {
        // Squares above 2^24 are not exact in f32, which used to push the nearest-pixel choice off by one.
        let (width, height) = (6000u32, 110u32);
        let source = Source { path: "wide.png".into(), width, height, crc32: 0, rgba: vec![0; (width * height * 4) as usize] };
        let band = [json!({"op": "box", "rect": [0, 0, width, 100]})];
        let rows = |steps: Vec<Value>| {
            let mut counts = vec![0usize; height as usize];
            for (_, y) in claimed(&source, json!(steps)) {
                counts[y] += 1;
            }
            counts
        };
        let grown = rows(vec![band[0].clone(), json!({"op": "grow", "radius": 1})]);
        assert!(grown[..=100].iter().all(|&n| n == width as usize), "row 100 must be fully grown: {:?}", &grown[98..103]);
        assert!(grown[101..].iter().all(|&n| n == 0));
        let shrunk = rows(vec![band[0].clone(), json!({"op": "shrink", "radius": 1})]);
        assert!(shrunk[..99].iter().all(|&n| n == width as usize), "rows 0..99 must survive: {:?}", &shrunk[96..101]);
        assert!(shrunk[99..].iter().all(|&n| n == 0));
    }

    #[test]
    fn a_morphology_area_over_the_ceiling_is_refused_before_the_transform() {
        let side = 2100u32; // 4.41 MP, over MAX_MORPH_PIXELS
        let source = Source { path: "big.png".into(), width: side, height: side, crc32: 0, rgba: vec![0; (side * side * 4) as usize] };
        let steps = json!([{"op": "box", "rect": [0, 0, side, side]}, {"op": "grow", "radius": 1}]);
        let budget = &mut WorkBudget::new(u64::MAX);
        let error = evaluate_part(&source, &spec("big", steps), |_| &[], budget).unwrap_err();
        assert!(error.contains("clip the mask with a box"), "{error}");
    }

    #[test]
    fn the_work_budget_covers_every_step_of_every_part() {
        let source = picture();
        // 5 parts x 64 selection steps x 2 visits x 96 pixels is 61440, over 96 x 512 = 49152.
        let steps: Vec<Value> = (0..64).map(|_| json!({"op": "alpha"})).collect();
        let parts: Vec<PartSpec> = (0..5).map(|n| spec(&format!("p{n}"), json!(steps))).collect();
        validate(&parts, source.width, source.height).unwrap();
        let error = evaluate_all(&source, &parts, &mut new_budget(&source)).unwrap_err();
        assert!(error.contains("split work limit"), "{error}");
        assert!(evaluate_all(&source, &parts[..4], &mut new_budget(&source)).is_ok());
    }

    #[test]
    fn part_references_evaluate_in_dependency_order_and_cycles_are_refused() {
        let source = picture();
        let parts = vec![
            spec("rest", json!([{"op": "alpha"}, {"op": "part", "name": "red", "mode": "subtract"}])),
            spec("red", json!([{"op": "component", "point": [1, 2]}])),
        ];
        validate(&parts, source.width, source.height).unwrap();
        let masks = evaluate_all(&source, &parts, &mut new_budget(&source)).unwrap();
        assert_eq!(masks[0].iter().filter(|&&v| v >= CLAIMED).count(), 12);
        assert_eq!(masks[1].iter().filter(|&&v| v >= CLAIMED).count(), 16);

        let cyclic = vec![
            spec("a", json!([{"op": "part", "name": "b"}])),
            spec("b", json!([{"op": "part", "name": "a"}])),
        ];
        assert!(validate(&cyclic, 12, 8).unwrap_err().contains("cycle"));
        let own = vec![spec("a", json!([{"op": "part", "name": "a"}]))];
        assert!(validate(&own, 12, 8).unwrap_err().contains("cycle"));
        let unknown = vec![spec("a", json!([{"op": "part", "name": "nobody"}]))];
        assert!(validate(&unknown, 12, 8).unwrap_err().contains("unknown part"));
    }

    #[test]
    fn validation_refuses_out_of_range_input() {
        let ok = |steps: Value| validate(&[spec("a", steps)], 12, 8);
        assert!(ok(json!([{"op": "component", "point": [12, 0]}])).unwrap_err().contains("outside"));
        assert!(ok(json!([{"op": "flood", "point": [0, 8]}])).is_err());
        assert!(ok(json!([{"op": "box", "rect": [10, 0, 3, 1]}])).is_err());
        assert!(ok(json!([{"op": "box", "rect": [0, 0, 0, 1]}])).is_err());
        assert!(ok(json!([{"op": "polygon", "points": [[0, 0], [1, 1]]}])).is_err());
        assert!(ok(json!([{"op": "polygon", "points": [[0, 0], [13, 1], [1, 1]]}])).is_err());
        assert!(ok(json!([{"op": "grow", "radius": 0}])).is_err());
        assert!(ok(json!([{"op": "feather", "radius": 33}])).is_err());
        assert!(ok(json!([{"op": "side", "axisX": 12, "keep": "left"}])).is_err());
        assert!(ok(json!([])).is_err());
        assert!(validate(&[spec("", json!([{"op": "alpha"}]))], 12, 8).is_err());
        let twice = [spec("a", json!([{"op": "alpha"}])), spec("a", json!([{"op": "alpha"}]))];
        assert!(validate(&twice, 12, 8).unwrap_err().contains("used twice"));
        let mut padded = spec("a", json!([{"op": "alpha"}]));
        padded.padding = 65;
        assert!(validate(&[padded], 12, 8).is_err());
    }

    #[test]
    fn textures_are_cropped_premultiplied_even_and_centred_where_they_were() {
        let source = picture();
        let blue = spec("blue", json!([{"op": "component", "point": [9, 3]}]));
        let mask = eval(&source, &blue);
        let image = part_image(&source, &blue, &mask).unwrap();
        // 3x4 visible pixels at (8, 2): the width is padded to 4.
        assert_eq!((image.width, image.height), (4, 4));
        // Left edge 8, top edge 2; the centre of the 4x4 texture is (10, 4); the document centre is (6, 4).
        assert_eq!(image.trans, [4.0, 0.0, 0.0]);
        let decoded = image::load_from_memory(&image.texture.data).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(0, 0).0, [20, 30, 200, 255]);
        assert_eq!(decoded.get_pixel(3, 0).0, [0, 0, 0, 0], "the padding column is transparent");

        let mut padded = blue.clone();
        padded.padding = 2;
        let image = part_image(&source, &padded, &mask).unwrap();
        // 3x4 plus 2 on every side is 7x8, rounded up to even width 8.
        assert_eq!((image.width, image.height), (8, 8));
        let decoded = image::load_from_memory(&image.texture.data).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(1, 4).0, [0, 0, 0, 0], "padding is fully zero, so it adds no light when rendered");
        assert_eq!(decoded.get_pixel(2, 2).0, [20, 30, 200, 255]);

        let none = spec("none", json!([{"op": "box", "rect": [6, 0, 2, 2]}]));
        let mask = eval(&source, &none);
        assert!(part_image(&source, &none, &mask).unwrap_err().contains("no visible pixels"));
    }

    #[test]
    fn statistics_count_claims_overlaps_and_unclaimed_pixels() {
        let source = picture();
        let parts = vec![
            spec("red", json!([{"op": "component", "point": [1, 2]}])),
            spec("wide", json!([{"op": "box", "rect": [3, 2, 6, 4]}])),
        ];
        let masks = evaluate_all(&source, &parts, &mut new_budget(&source)).unwrap();
        let stats = statistics(&source, &parts, &masks, &mut new_budget(&source)).unwrap();
        assert_eq!(stats["parts"][0]["pixels"], 16);
        assert_eq!(stats["parts"][0]["bbox"], json!([1, 2, 4, 4]));
        assert_eq!(stats["parts"][0]["overlaps"], json!([{"part": "wide", "pixels": 8}]));
        // The box covers red columns 3, 4 (8 pixels) and the blue column x=8 (4 pixels, the rest of blue is outside).
        assert_eq!(stats["multiplyClaimedPixels"], 8);
        // Blue has 12 pixels: x=8 is claimed by the box, x=9, 10 (8 pixels) are not.
        assert_eq!(stats["unclaimedPixels"], 8);
        assert_eq!(stats["parts"][1]["meanAlpha"], 0.5);
    }

    #[test]
    fn overlaps_are_counted_per_pair_with_literal_numbers() {
        let source = picture();
        // Columns: a 0..5, b 3..8, c 4..9 over all 8 rows. a-b share 3 columns, a-c 2, b-c 4 (x 4..7).
        let parts = vec![
            spec("a", json!([{"op": "box", "rect": [0, 0, 6, 8]}])),
            spec("b", json!([{"op": "box", "rect": [3, 0, 6, 8]}])),
            spec("c", json!([{"op": "box", "rect": [4, 0, 6, 8]}])),
        ];
        let masks = evaluate_all(&source, &parts, &mut new_budget(&source)).unwrap();
        let stats = statistics(&source, &parts, &masks, &mut new_budget(&source)).unwrap();
        assert_eq!(stats["parts"][0]["overlaps"], json!([{"part": "b", "pixels": 24}, {"part": "c", "pixels": 16}]));
        assert_eq!(stats["parts"][1]["overlaps"], json!([{"part": "a", "pixels": 24}, {"part": "c", "pixels": 40}]));
        assert_eq!(stats["parts"][2]["overlaps"], json!([{"part": "a", "pixels": 16}, {"part": "b", "pixels": 40}]));
        // x 3..8 is claimed by at least two parts.
        assert_eq!(stats["multiplyClaimedPixels"], 48);
    }

    #[test]
    fn overlap_statistics_are_charged_to_the_work_budget() {
        let source = picture();
        let parts: Vec<PartSpec> =
            (0..MAX_PARTS).map(|n| spec(&format!("p{n}"), json!([{"op": "box", "rect": [0, 0, 12, 8]}]))).collect();
        validate(&parts, source.width, source.height).unwrap();
        // Evaluating the masks is cheap (2 visits per pixel and part); counting 496 pairs on every pixel is not.
        let masks = evaluate_all(&source, &parts, &mut new_budget(&source)).unwrap();
        let error = statistics(&source, &parts, &masks, &mut new_budget(&source)).unwrap_err();
        assert!(error.contains("split work limit"), "{error}");
        assert!(statistics(&source, &parts[..20], &masks[..20], &mut new_budget(&source)).is_ok());
    }

    #[test]
    fn overlay_blends_literal_pixels_and_is_charged_to_the_budget() {
        let rgba = [[200, 100, 50], [0, 0, 0], [255, 255, 255], [10, 20, 30]].iter().flat_map(|c| [c[0], c[1], c[2], 255]).collect();
        let source = Source { path: "tiny.png".into(), width: 2, height: 2, crc32: 0, rgba };
        // Pixel 0 and 3 touch an unclaimed neighbour: outline in the tint [230, 25, 75]. Pixel 1 is untouched.
        // Pixel 2 has coverage 128 and no unclaimed neighbour: weight 128 * 45 / 100 = 57 of the tint.
        let masks = [vec![255, 0, 128, 255]];
        let png = overlay_png(&source, &masks, None, &mut new_budget(&source)).unwrap();
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.as_raw(), &[230, 25, 75, 255, 0, 0, 0, 255, 249, 203, 214, 255, 230, 25, 75, 255]);
        let error = overlay_png(&source, &masks, None, &mut WorkBudget::new(5)).unwrap_err();
        assert!(error.contains("split work limit"), "{error}");
    }

    #[test]
    fn a_polygon_touching_the_right_and_bottom_edges_fills_literal_pixels() {
        let source = picture();
        // The triangle (12,0), (12,8), (4,8) has its right angle in the bottom right corner; the edge from (12,0)
        // to (4,8) is x + y = 12, so a pixel centre is inside when x + y + 1 >= 12.
        let triangle = claimed(&source, json!([{"op": "polygon", "points": [[12, 0], [12, 8], [4, 8]]}]));
        let expected: Vec<(usize, usize)> =
            (0..8).flat_map(|y| (0..12).filter(move |&x| x + y >= 11).map(move |x| (x, y))).collect();
        assert_eq!(triangle, expected);
        assert_eq!(expected.len(), 36);
        assert!(triangle.contains(&(11, 0)) && triangle.contains(&(4, 7)) && triangle.contains(&(11, 7)));
    }

    #[test]
    fn the_textures_of_all_parts_together_are_capped() {
        // Three full-size parts of a 16 MP source: two textures fill the cap exactly, the third crosses it.
        let side = 4096u32;
        let source = Source { path: "big.png".into(), width: side, height: side, crc32: 0, rgba: vec![255; (side * side * 4) as usize] };
        let parts: Vec<PartSpec> = (0..3).map(|n| spec(&format!("p{n}"), json!([{"op": "alpha"}]))).collect();
        let masks = vec![vec![255u8; source.len()]; 3];
        let budget = &mut new_budget(&source);
        check_textures(&source, &parts[..2], &masks[..2], budget).unwrap();
        let error = check_textures(&source, &parts, &masks, budget).unwrap_err();
        assert!(error.contains("in total"), "{error}");
    }
}
